//! Granular pitch shifter with two crossfaded read heads.
//!
//! Audio is written into a preallocated circular buffer. Two fractional read
//! heads are offset by half a 50 ms grain; a raised-sine window fades each head
//! in and out around its wrap point, so the summed output stays continuous.
//! The declared latency is 50 ms: at unity ratio the fixed read is exactly
//! `0.05 * sample_rate` samples behind the write pointer.

use std::f32::consts::PI;

use crate::dsp_util::OnePole;
use crate::{BuiltinDsp, ParamCurve, ParamSet, ParamSpec, ParamStyle};

pub static SPECS: &[ParamSpec] = &[
    ParamSpec {
        key: "semitones",
        name: "Semitones",
        unit: "st",
        min: -12.0,
        max: 12.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Pitch",
        visible_when: None,
    },
    ParamSpec {
        key: "cents",
        name: "Cents",
        unit: "ct",
        min: -100.0,
        max: 100.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Pitch",
        visible_when: None,
    },
    ParamSpec {
        key: "mix",
        name: "Mix",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 100.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Output",
        visible_when: None,
    },
];

const GRAIN_SECONDS: f32 = 0.05;
const BUFFER_SECONDS: f32 = 0.25;

fn sanitize_rate(sample_rate: f64) -> f64 {
    if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        48_000.0
    }
}

/// Circular buffer with f64-indexed fractional reads.
///
/// Kept local because granular delays land on arbitrary fractional positions;
/// the index is clamped, so a rounding edge can never read out of bounds.
struct GranularBuffer {
    buffer: Vec<f32>,
    write: usize,
}

impl GranularBuffer {
    fn new(max_seconds: f32, sample_rate: f64) -> Self {
        let samples = (f64::from(max_seconds) * sample_rate).ceil() as usize + 4;
        Self {
            buffer: vec![0.0; samples.max(8)],
            write: 0,
        }
    }

    #[inline]
    fn push(&mut self, sample: f32) {
        self.buffer[self.write] = sample;
        self.write += 1;
        if self.write == self.buffer.len() {
            self.write = 0;
        }
    }

    /// `delay_samples` follows `DelayLine` semantics: 1.0 is the current
    /// sample, so a delay of `n` returns the sample written `n - 1` frames ago.
    #[inline]
    fn read(&self, delay_samples: f32) -> f32 {
        let length = self.buffer.len();
        let max_delay = (length - 3) as f64;
        let delay = if delay_samples.is_finite() {
            f64::from(delay_samples).clamp(1.0, max_delay)
        } else {
            1.0
        };
        let mut position = (self.write as f64 - delay).rem_euclid(length as f64);
        if position >= length as f64 {
            position = 0.0;
        }
        let index = (position.floor() as usize).min(length - 1);
        let next = if index + 1 == length { 0 } else { index + 1 };
        let fraction = (position - index as f64) as f32;
        self.buffer[index] + (self.buffer[next] - self.buffer[index]) * fraction
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
    }
}

pub struct Dsp {
    left: GranularBuffer,
    right: GranularBuffer,
    grain: f32,
    base_delay: f32,
    phase: f32,
    ratio: OnePole,
    blend: OnePole,
    mix: OnePole,
}

impl Dsp {
    pub fn new(sample_rate: f64) -> Self {
        let sample_rate = sanitize_rate(sample_rate);
        let grain = ((sample_rate as f32) * GRAIN_SECONDS).max(2.0);

        let mut ratio = OnePole::from_time(20.0, sample_rate);
        ratio.set_value(1.0);
        let blend = OnePole::from_time(30.0, sample_rate);
        let mut mix = OnePole::from_time(20.0, sample_rate);
        mix.set_value(1.0);

        Self {
            left: GranularBuffer::new(BUFFER_SECONDS, sample_rate),
            right: GranularBuffer::new(BUFFER_SECONDS, sample_rate),
            grain,
            // DelayLine counts the current write slot as 1.0, so +1 makes the
            // fixed read exactly `grain` samples old.
            base_delay: grain + 1.0,
            phase: 0.0,
            ratio,
            blend,
            mix,
        }
    }
}

impl BuiltinDsp for Dsp {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let frames = left.len().min(right.len());
        if frames == 0 {
            return;
        }

        let semitones = params.get(0) + params.get(1) / 100.0;
        let target_ratio = 2.0_f32.powf(semitones / 12.0);
        // A single fixed-delay read is exact at unity, so fade granular output
        // out entirely there instead of summing two unrelated delays.
        let target_blend = if (target_ratio - 1.0).abs() <= 1.0e-6 {
            0.0
        } else {
            1.0
        };
        let target_mix = (params.get(2) / 100.0).clamp(0.0, 1.0);

        for index in 0..frames {
            let dry_l = left[index];
            let dry_r = right[index];

            self.left.push(dry_l);
            self.right.push(dry_r);

            let ratio = self.ratio.process(target_ratio);
            let blend = self.blend.process(target_blend);
            let mix = self.mix.process(target_mix);

            if blend < 1.0e-4 {
                // Head phase is irrelevant while the fixed path plays; start
                // the next granular run from a clean wrap point.
                self.phase = 0.0;
            }

            let phase0 = self.phase.rem_euclid(1.0);
            let phase1 = (self.phase + 0.5).rem_euclid(1.0);
            let delay0 = self.base_delay + phase0 * self.grain;
            let delay1 = self.base_delay + phase1 * self.grain;

            // Raised-sine windows, offset by half a grain and summing to unity.
            let window = (PI * phase0).sin();
            let weight0 = window * window;
            let weight1 = 1.0 - weight0;

            let granular_l = self.left.read(delay0) * weight0 + self.left.read(delay1) * weight1;
            let granular_r = self.right.read(delay0) * weight0 + self.right.read(delay1) * weight1;
            let fixed_l = self.left.read(self.base_delay);
            let fixed_r = self.right.read(self.base_delay);

            let wet_l = fixed_l + (granular_l - fixed_l) * blend;
            let wet_r = fixed_r + (granular_r - fixed_r) * blend;

            let out_l = if mix >= 1.0 {
                wet_l
            } else if mix <= 0.0 {
                dry_l
            } else {
                dry_l + (wet_l - dry_l) * mix
            };
            let out_r = if mix >= 1.0 {
                wet_r
            } else if mix <= 0.0 {
                dry_r
            } else {
                dry_r + (wet_r - dry_r) * mix
            };

            left[index] = out_l;
            right[index] = out_r;

            // Phase is a fraction of one grain; the delay behind the write
            // pointer changes by `1 - ratio` samples per output sample.
            self.phase = (self.phase + (1.0 - ratio) / self.grain).rem_euclid(1.0);
        }
    }

    fn reset(&mut self) {
        self.left.clear();
        self.right.clear();
        self.phase = 0.0;
        self.ratio.set_value(1.0);
        self.blend.set_value(0.0);
        self.mix.set_value(1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BuiltinKind;

    const SAMPLE_RATE: f32 = 48_000.0;

    fn sine(freq_hz: f32, len: usize) -> Vec<f32> {
        let scale = 2.0 * PI * freq_hz / SAMPLE_RATE;
        (0..len)
            .map(|index| 0.25 * (scale * index as f32).sin())
            .collect()
    }

    fn process_blocks(dsp: &mut Dsp, params: &ParamSet, input: &[f32]) -> Vec<f32> {
        let mut left = input.to_vec();
        let mut right = input.to_vec();
        let mut offset = 0;
        while offset < input.len() {
            let len = (input.len() - offset).min(512);
            dsp.process(
                &mut left[offset..offset + len],
                &mut right[offset..offset + len],
                params,
            );
            offset += len;
        }
        left
    }

    fn positive_crossings(samples: &[f32]) -> usize {
        let mut count = 0;
        let mut previous = samples.first().copied().unwrap_or(0.0);
        for sample in &samples[1..] {
            if previous <= 0.0 && *sample > 0.0 {
                count += 1;
            }
            previous = *sample;
        }
        count
    }

    #[test]
    fn zero_semitones_is_a_delayed_copy() {
        let params = ParamSet::new(BuiltinKind::PitchShift);
        let mut dsp = Dsp::new(f64::from(SAMPLE_RATE));
        let input = sine(440.0, 12_000);
        let output = process_blocks(&mut dsp, &params, &input);

        let latency = (SAMPLE_RATE * 0.05) as usize;
        for index in 3_000..input.len() {
            assert!(
                (output[index] - input[index - latency]).abs() < 1.0e-6,
                "index {index}: {} vs {}",
                output[index],
                input[index - latency]
            );
        }
    }

    #[test]
    fn octave_up_raises_the_zero_crossing_rate() {
        let params = ParamSet::new(BuiltinKind::PitchShift);
        params.set(0, 12.0);
        let mut dsp = Dsp::new(f64::from(SAMPLE_RATE));
        let input = sine(440.0, 48_000);
        let output = process_blocks(&mut dsp, &params, &input);

        let start = 12_000;
        let input_rate = positive_crossings(&input[start..]) as f32;
        let output_rate = positive_crossings(&output[start..]) as f32;
        let ratio = output_rate / input_rate;
        assert!((1.5..2.5).contains(&ratio), "crossing ratio {ratio}");
    }
}
