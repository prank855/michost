//! Per-plugin 2x/4x oversampling.
//!
//! One upsample stage before the graph and one downsample stage after it keep
//! the whole chain at N× between plugins. The FIR is a 64-tap Hamming-windowed
//! sinc with its cutoff at the original Nyquist frequency, the same prototype
//! the built-in dynamics use. All state is allocated in `new`; processing never
//! allocates.

use michost_graph::PluginProcessor;

const TAPS: usize = 64;

pub struct Oversampler {
    factor: usize,
    taps: [f32; TAPS],
    up_history: Vec<Vec<f32>>,
    down_history: Vec<Vec<f32>>,
}

impl Oversampler {
    pub fn new(factor: usize, channels: usize, _max_frames: usize) -> Self {
        debug_assert!(factor == 2 || factor == 4);
        let history = TAPS / factor;
        Self {
            factor,
            taps: design_taps(factor),
            up_history: vec![vec![0.0; history]; channels],
            down_history: vec![vec![0.0; TAPS]; channels],
        }
    }

    /// Round-trip delay of the up+down pair, in engine-rate samples.
    pub fn latency_samples(factor: usize) -> u32 {
        if factor <= 1 {
            0
        } else {
            ((TAPS - 1) / factor) as u32
        }
    }

    pub fn upsample(&mut self, input: &[&[f32]], frames: usize, output: &mut [Vec<f32>]) {
        let channels = self.up_history.len().min(input.len()).min(output.len());
        for channel in 0..channels {
            let source = &input[channel][..frames.min(input[channel].len())];
            let destination = &mut output[channel][..frames * self.factor];
            upsample_channel(
                &self.taps,
                self.factor,
                &mut self.up_history[channel],
                source,
                destination,
            );
        }
    }

    pub fn downsample(&mut self, input: &[Vec<f32>], frames: usize, output: &mut [&mut [f32]]) {
        let channels = self.down_history.len().min(input.len()).min(output.len());
        for channel in 0..channels {
            let source = &input[channel][..frames * self.factor];
            let length = frames.min(output[channel].len());
            let destination = &mut output[channel][..length];
            downsample_channel(
                &self.taps,
                self.factor,
                &mut self.down_history[channel],
                source,
                destination,
            );
        }
    }
}

fn design_taps(factor: usize) -> [f32; TAPS] {
    let mut taps = [0.0f32; TAPS];
    let cutoff = 0.5 / factor as f64;
    let two_cutoff = 2.0 * cutoff;
    let center = (TAPS - 1) as f64 * 0.5;
    let mut sum = 0.0f64;
    for (index, tap) in taps.iter_mut().enumerate() {
        let offset = index as f64 - center;
        let sinc = if offset.abs() < 1.0e-12 {
            1.0
        } else {
            let angle = std::f64::consts::PI * two_cutoff * offset;
            angle.sin() / angle
        };
        let window =
            0.54 - 0.46 * (2.0 * std::f64::consts::PI * index as f64 / (TAPS - 1) as f64).cos();
        let value = two_cutoff * sinc * window;
        *tap = value as f32;
        sum += value;
    }
    if sum.abs() > 1.0e-12 {
        for tap in &mut taps {
            *tap = (f64::from(*tap) / sum) as f32;
        }
    }
    taps
}

fn upsample_channel(
    taps: &[f32; TAPS],
    factor: usize,
    history: &mut [f32],
    input: &[f32],
    output: &mut [f32],
) {
    let history_len = TAPS / factor;
    for (out, &sample) in output.chunks_exact_mut(factor).zip(input.iter()) {
        history.copy_within(0..history_len - 1, 1);
        history[0] = sample;
        for (phase, slot) in out.iter_mut().enumerate() {
            let mut sum = 0.0f32;
            for (&coefficient, &delayed) in taps[phase..].iter().step_by(factor).zip(history.iter())
            {
                sum += coefficient * delayed;
            }
            *slot = sum * factor as f32;
        }
    }
}

fn downsample_channel(
    taps: &[f32; TAPS],
    factor: usize,
    history: &mut [f32],
    input: &[f32],
    output: &mut [f32],
) {
    for (out, chunk) in output.iter_mut().zip(input.chunks_exact(factor)) {
        for &sample in chunk {
            history.copy_within(0..TAPS - 1, 1);
            history[0] = sample;
        }
        let mut sum = 0.0f32;
        for (&coefficient, &delayed) in taps.iter().zip(history.iter()) {
            sum += coefficient * delayed;
        }
        *out = sum;
    }
}

/// Wraps one graph processor with a private up/down pair so that plugin runs
/// at N× while the graph itself stays at the engine rate.
pub struct OversampledProcessor {
    inner: Box<dyn PluginProcessor>,
    factor: usize,
    channels: usize,
    max_frames: usize,
    up: Oversampler,
    down: Oversampler,
    scratch: Vec<Vec<f32>>,
}

impl OversampledProcessor {
    pub fn new(
        inner: Box<dyn PluginProcessor>,
        factor: u8,
        channels: usize,
        max_frames: usize,
    ) -> Self {
        let factor = match factor {
            2 => 2usize,
            4 => 4usize,
            _ => 1,
        };
        let channels = channels.max(1);
        let max_frames = max_frames.max(1);
        Self {
            inner,
            factor,
            channels,
            max_frames,
            up: Oversampler::new(factor, channels, max_frames),
            down: Oversampler::new(factor, channels, max_frames),
            scratch: vec![vec![0.0f32; max_frames * factor]; channels],
        }
    }
}

impl PluginProcessor for OversampledProcessor {
    fn process(&mut self, channels: &mut [&mut [f32]], frames: usize) {
        let frames = frames.min(self.max_frames);
        let active = channels.len().min(self.channels);
        if frames == 0 || active == 0 || self.factor <= 1 {
            if self.factor <= 1 {
                self.inner.process(channels, frames);
            }
            return;
        }
        let graph_frames = frames * self.factor;
        {
            let input: Vec<&[f32]> = channels[..active]
                .iter()
                .map(|channel| &channel[..frames])
                .collect();
            self.up.upsample(&input, frames, &mut self.scratch);
        }
        {
            let mut buffers: Vec<&mut [f32]> = self.scratch[..active]
                .iter_mut()
                .map(|buffer| &mut buffer[..graph_frames])
                .collect();
            self.inner.process(&mut buffers, graph_frames);
        }
        self.down
            .downsample(&self.scratch[..active], frames, &mut channels[..active]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impulse_latency(factor: usize) -> usize {
        let mut up = Oversampler::new(factor, 1, 512);
        let mut down = Oversampler::new(factor, 1, 512);
        let frames = 256;
        let input = vec![0.0f32; frames];
        let mut input = input;
        input[0] = 1.0;
        let mut up_buffer = vec![vec![0.0f32; frames * factor]];
        up.upsample(&[&input], frames, &mut up_buffer);
        let mut output = [vec![0.0f32; frames]];
        down.downsample(&up_buffer, frames, &mut [&mut output[0]]);
        output[0]
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.abs().total_cmp(&right.1.abs()))
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    #[test]
    fn round_trip_latency_matches_report() {
        for factor in [2usize, 4] {
            let measured = impulse_latency(factor);
            let reported = Oversampler::latency_samples(factor) as usize;
            assert!(
                measured.abs_diff(reported) <= 1,
                "factor {factor}: measured {measured}, reported {reported}"
            );
        }
    }

    #[test]
    fn dc_gain_is_preserved() {
        for factor in [2usize, 4] {
            let mut up = Oversampler::new(factor, 1, 512);
            let mut down = Oversampler::new(factor, 1, 512);
            let frames = 256;
            let input = vec![0.25f32; frames];
            let mut up_buffer = [vec![0.0f32; frames * factor]];
            up.upsample(&[&input], frames, &mut up_buffer);
            let mut output = [vec![0.0f32; frames]];
            down.downsample(&up_buffer, frames, &mut [&mut output[0]]);
            let settled = output[0][40..].iter().sum::<f32>() / (frames - 40) as f32;
            assert!(
                (settled - 0.25).abs() < 1.0e-3,
                "factor {factor}: settled {settled}"
            );
        }
    }

    #[test]
    fn block_boundaries_are_continuous() {
        for factor in [2usize, 4] {
            for frequency in [100.0f32, 997.0, 5_000.0, 10_000.0, 17_000.0] {
                let mut up = Oversampler::new(factor, 1, 64);
                let mut down = Oversampler::new(factor, 1, 64);
                let frames = 64usize;
                let blocks = 32usize;
                let mut input = vec![0.0f32; frames * blocks];
                for (index, sample) in input.iter_mut().enumerate() {
                    let phase = 2.0 * std::f32::consts::PI * frequency * index as f32 / 48_000.0;
                    *sample = phase.sin() * 0.5;
                }
                let mut output = Vec::with_capacity(input.len());
                let mut up_buffer = vec![vec![0.0f32; frames * factor]];
                let mut block_out = [vec![0.0f32; frames]];
                for block in input.chunks_exact(frames) {
                    up.upsample(&[block], frames, &mut up_buffer);
                    down.downsample(&up_buffer, frames, &mut [&mut block_out[0]]);
                    output.extend_from_slice(&block_out[0]);
                }
                let latency = Oversampler::latency_samples(factor) as usize;
                let mut max_error = 0.0f32;
                for index in (latency + 200)..(input.len() - 200) {
                    let expected = input[index - latency];
                    max_error = max_error.max((output[index] - expected).abs());
                }
                assert!(
                    max_error < 0.03,
                    "factor {factor}, {frequency} Hz: max error {max_error}"
                );
            }
        }
    }

    #[test]
    fn latency_for_one_is_zero() {
        assert_eq!(Oversampler::latency_samples(1), 0);
    }
}
