//! Voice-oriented spectral noise suppressor.
//!
//! A 256-point causal STFT (128-sample hop, sqrt-Hann WOLA) with a
//! speech-presence-aware per-bin noise estimate and decision-directed gain.
//! The dry path is delayed by 256 samples to match the STFT, so the mode has a
//! fixed 256-sample latency. A low-cut high-pass removes rumble before the
//! analysis and the dry/wet mix is smoothed.

use std::f32::consts::FRAC_1_SQRT_2;

use crate::dsp_util::{Biquad, DelayLine, OnePole};
use crate::{BuiltinDsp, MAX_BLOCK, ParamCurve, ParamSet, ParamSpec, ParamStyle};

mod spectral;

pub static SPECS: &[ParamSpec] = &[
    ParamSpec {
        key: "strength",
        name: "Strength",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 50.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Noise Removal",
        visible_when: None,
    },
    ParamSpec {
        key: "reduction_db",
        name: "Reduction",
        unit: "dB",
        min: 0.0,
        max: 60.0,
        default: 20.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Noise Removal",
        visible_when: None,
    },
    ParamSpec {
        key: "smoothing",
        name: "Smoothing",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 50.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Noise Removal",
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
    ParamSpec {
        key: "low_cut_hz",
        name: "Low Cut",
        unit: "Hz",
        min: 20.0,
        max: 300.0,
        default: 80.0,
        curve: ParamCurve::Log,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Noise Removal",
        visible_when: None,
    },
    ParamSpec {
        key: "voice",
        name: "Voice",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 60.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Noise Removal",
        visible_when: None,
    },
    ParamSpec {
        key: "gate",
        name: "Gate",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 30.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Noise Removal",
        visible_when: None,
    },
    ParamSpec {
        key: "gate_threshold_db",
        name: "Gate Thr",
        unit: "dB",
        min: -60.0,
        max: -20.0,
        default: -40.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Noise Removal",
        visible_when: None,
    },
];

const LATENCY_SAMPLES: usize = 256;
/// `DelayLine::read` counts the sample just pushed as `1.0`.
const LATENCY_READ: f32 = LATENCY_SAMPLES as f32 + 1.0;

#[inline]
fn sanitize(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

fn sanitize_rate(sample_rate: f64) -> f64 {
    if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        48_000.0
    }
}

pub struct Dsp {
    sample_rate: f64,
    low_cut: f32,
    highpass_left: Biquad,
    highpass_right: Biquad,
    spectral: spectral::Spectral,
    hp_left: Vec<f32>,
    hp_right: Vec<f32>,
    dry_left: Vec<f32>,
    dry_right: Vec<f32>,
    dry_delay_left: DelayLine,
    dry_delay_right: DelayLine,
    mix: OnePole,
    gate_gain: f32,
    gate_hold: u32,
}

impl Dsp {
    pub fn new(sample_rate: f64) -> Self {
        let sample_rate = sanitize_rate(sample_rate);
        let mut highpass_left = Biquad::new();
        let mut highpass_right = Biquad::new();
        highpass_left.highpass(sample_rate, 80.0, FRAC_1_SQRT_2);
        highpass_right.highpass(sample_rate, 80.0, FRAC_1_SQRT_2);
        let mut mix = OnePole::from_time(20.0, sample_rate);
        mix.set_value(1.0);

        Self {
            sample_rate,
            low_cut: 80.0,
            highpass_left,
            highpass_right,
            spectral: spectral::Spectral::new(sample_rate),
            hp_left: vec![0.0; MAX_BLOCK],
            hp_right: vec![0.0; MAX_BLOCK],
            dry_left: vec![0.0; MAX_BLOCK],
            dry_right: vec![0.0; MAX_BLOCK],
            dry_delay_left: DelayLine::new(0.1, sample_rate),
            dry_delay_right: DelayLine::new(0.1, sample_rate),
            mix,
            gate_gain: 1.0,
            gate_hold: 0,
        }
    }

    fn retune_low_cut(&mut self, low_cut: f32) {
        if (low_cut - self.low_cut).abs() <= 1.0e-6 {
            return;
        }
        self.low_cut = low_cut;
        self.highpass_left
            .highpass(self.sample_rate, low_cut, FRAC_1_SQRT_2);
        self.highpass_right
            .highpass(self.sample_rate, low_cut, FRAC_1_SQRT_2);
    }
}

impl BuiltinDsp for Dsp {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let frames = left.len().min(right.len()).min(MAX_BLOCK);
        if frames == 0 {
            return;
        }

        let mix_target = (params.get(3) / 100.0).clamp(0.0, 1.0);
        let low_cut = params.get(4).clamp(20.0, 300.0);
        self.retune_low_cut(low_cut);

        for index in 0..frames {
            let dry_l = sanitize(left[index]);
            let dry_r = sanitize(right[index]);
            left[index] = dry_l;
            right[index] = dry_r;
            let high_l = self.highpass_left.process(dry_l);
            let high_r = self.highpass_right.process(dry_r);
            self.hp_left[index] = high_l;
            self.hp_right[index] = high_r;
            // Keep a copy for the dry path; spectral processing overwrites
            // hp_left/hp_right in place. The low cut applies to dry too.
            self.dry_left[index] = high_l;
            self.dry_right[index] = high_r;
        }

        let level = {
            let mut sum = 0.0f32;
            for index in 0..frames {
                sum += self.dry_left[index] * self.dry_left[index]
                    + self.dry_right[index] * self.dry_right[index];
            }
            let rms = (sum / (frames as f32 * 2.0)).sqrt();
            if rms > 1.0e-6 {
                20.0 * rms.log10()
            } else {
                -120.0
            }
        };
        let gate_threshold = params.get_key("gate_threshold_db").clamp(-60.0, -20.0);

        self.spectral.process(
            &mut self.hp_left[..frames],
            &mut self.hp_right[..frames],
            params,
        );

        // Mix 0 is a dry monitor path; do not let the gate modulate it.
        let gate_amount = if mix_target <= 0.001 {
            0.0
        } else {
            (params.get_key("gate") / 100.0).clamp(0.0, 1.0)
        };
        let activity = self.spectral.voice_activity();
        let gate_open = activity > 0.55 && level > gate_threshold;
        let hold_samples = (0.18 * self.sample_rate) as u32;
        if gate_open {
            self.gate_hold = hold_samples;
        } else {
            self.gate_hold = self.gate_hold.saturating_sub(frames as u32);
        }
        let gate_target = if gate_amount <= 0.001 || gate_open || self.gate_hold > 0 {
            1.0
        } else {
            10.0_f32.powf(-60.0 * gate_amount / 20.0)
        };
        let gate_attack = crate::dsp_util::smoothing_coefficient(4.0, self.sample_rate);
        let gate_release = crate::dsp_util::smoothing_coefficient(200.0, self.sample_rate);

        for index in 0..frames {
            // Push and read once per frame; reading the same ring position for
            // a whole block would hold one sample and garble the dry path.
            self.dry_delay_left.push(self.dry_left[index]);
            self.dry_delay_right.push(self.dry_right[index]);
            let delayed_l = self.dry_delay_left.read(LATENCY_READ);
            let delayed_r = self.dry_delay_right.read(LATENCY_READ);
            let wet_l = self.hp_left[index];
            let wet_r = self.hp_right[index];
            let mix = if mix_target <= 0.001 {
                0.0
            } else {
                self.mix.process(mix_target)
            };
            let out_l = if mix >= 1.0 {
                wet_l
            } else if mix <= 0.0 {
                delayed_l
            } else {
                delayed_l + (wet_l - delayed_l) * mix
            };
            let out_r = if mix >= 1.0 {
                wet_r
            } else if mix <= 0.0 {
                delayed_r
            } else {
                delayed_r + (wet_r - delayed_r) * mix
            };
            let coefficient = if gate_target > self.gate_gain {
                gate_attack
            } else {
                gate_release
            };
            self.gate_gain += coefficient * (gate_target - self.gate_gain);
            self.gate_gain = self.gate_gain.clamp(0.0, 1.0);
            left[index] = sanitize(out_l * self.gate_gain);
            right[index] = sanitize(out_r * self.gate_gain);
        }
    }

    fn reset(&mut self) {
        self.low_cut = 80.0;
        self.highpass_left
            .highpass(self.sample_rate, 80.0, FRAC_1_SQRT_2);
        self.highpass_right
            .highpass(self.sample_rate, 80.0, FRAC_1_SQRT_2);
        self.spectral.reset();
        self.hp_left.fill(0.0);
        self.hp_right.fill(0.0);
        self.dry_left.fill(0.0);
        self.dry_right.fill(0.0);
        self.dry_delay_left.clear();
        self.dry_delay_right.clear();
        self.mix.set_value(1.0);
        self.gate_gain = 1.0;
        self.gate_hold = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BuiltinKind;

    const RATE: f32 = 48_000.0;

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn noise(len: usize) -> Vec<f32> {
        let mut state = 0x1234_5678_u32;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let unit = (state >> 8) as f32 / (1_u32 << 24) as f32;
                (unit * 2.0 - 1.0) * 0.05
            })
            .collect()
    }

    fn tone(len: usize, frequency: f32, amplitude: f32) -> Vec<f32> {
        (0..len)
            .map(|index| {
                amplitude * (2.0 * std::f32::consts::PI * frequency * index as f32 / RATE).sin()
            })
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

    fn params() -> std::sync::Arc<ParamSet> {
        ParamSet::new(BuiltinKind::NoiseRemoval)
    }

    #[test]
    fn silence_stays_silent() {
        let params = params();
        let mut dsp = Dsp::new(f64::from(RATE));
        let mut left = vec![0.0; 512];
        let mut right = vec![0.0; 512];
        for _ in 0..8 {
            dsp.process(&mut left, &mut right, &params);
        }
        assert!(left.iter().all(|sample| *sample == 0.0));
        assert!(right.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn impulse_latency_is_256_samples() {
        let params = params();
        let mut dsp = Dsp::new(f64::from(RATE));
        let mut input = vec![0.0f32; 2_048];
        input[0] = 1.0;
        let output = process_blocks(&mut dsp, &params, &input);
        let first = output
            .iter()
            .position(|sample| sample.abs() > 1.0e-6)
            .expect("no output");
        assert_eq!(first, LATENCY_SAMPLES);
    }

    #[test]
    fn high_frequency_noise_is_reduced() {
        use crate::dsp_util::Biquad;

        let len = 4 * RATE as usize;
        let input = noise(len);
        let params = params();
        params.set(0, 100.0);
        params.set(1, 30.0);
        params.set(2, 20.0);
        let mut dsp = Dsp::new(f64::from(RATE));
        let output = process_blocks(&mut dsp, &params, &input);

        let high_band = |samples: &[f32]| {
            let mut filter = Biquad::new();
            filter.highpass(f64::from(RATE), 6_000.0, FRAC_1_SQRT_2);
            let tail: Vec<f32> = samples[len - 8_000..]
                .iter()
                .map(|&sample| filter.process(sample))
                .collect();
            rms(&tail)
        };
        let ratio = high_band(&output) / high_band(&input);
        assert!(ratio < 0.35, "high-band ratio {ratio}");
    }

    #[test]
    fn voice_band_tone_is_not_deleted() {
        let len = 4 * RATE as usize;
        let noise = noise(len);
        let input: Vec<f32> = tone(len, 1_000.0, 0.2)
            .iter()
            .zip(&noise)
            .map(|(tone, noise)| tone + noise)
            .collect();
        let params = params();
        params.set(0, 100.0);
        params.set(1, 20.0);
        params.set(2, 50.0);
        let mut dsp = Dsp::new(f64::from(RATE));
        let output = process_blocks(&mut dsp, &params, &input);
        let start = len - 8_000;
        let tone_rms = rms(&input[start..]);
        let output_rms = rms(&output[start..]);
        assert!(
            output_rms > tone_rms * 0.25,
            "voice-band content was over-suppressed: output {output_rms} input {tone_rms}"
        );
    }

    #[test]
    fn voice_gate_closes_on_noise_only() {
        let len = 4 * RATE as usize;
        let input = noise(len);
        let measure = |gate: f32| {
            let params = params();
            params.set(0, 50.0);
            params.set(1, 20.0);
            params.set(params.index_of("gate").expect("gate"), gate);
            let mut dsp = Dsp::new(f64::from(RATE));
            let output = process_blocks(&mut dsp, &params, &input);
            rms(&output[len - 8_000..])
        };
        let open = measure(0.0);
        let closed = measure(100.0);
        let params = params();
        params.set(0, 50.0);
        params.set(1, 20.0);
        params.set(params.index_of("gate").expect("gate"), 100.0);
        let mut dsp = Dsp::new(f64::from(RATE));
        let _ = process_blocks(&mut dsp, &params, &input);
        assert!(
            closed < open * 0.6,
            "voice gate did not close on noise: {open} -> {closed}"
        );
    }

    #[test]
    fn voice_gate_does_not_duck_a_voice_burst() {
        let preroll = RATE as usize;
        let burst = RATE as usize;
        let len = preroll + burst;
        let mut input = noise(len);
        for (index, sample) in tone(burst, 1_000.0, 0.25).iter().enumerate() {
            input[preroll + index] += sample;
        }
        let measure = |gate: f32| {
            let params = params();
            params.set(0, 50.0);
            params.set(1, 20.0);
            params.set(params.index_of("gate").expect("gate"), gate);
            let mut dsp = Dsp::new(f64::from(RATE));
            let output = process_blocks(&mut dsp, &params, &input);
            rms(&output[preroll + 4_000..preroll + burst])
        };
        let open = measure(0.0);
        let gated = measure(100.0);
        assert!(
            gated > open * 0.7,
            "voice gate ducked a voice burst: {open} -> {gated}"
        );
    }

    #[test]
    fn mix_zero_is_delayed_dry() {
        let params = params();
        params.set(0, 0.0);
        params.set(3, 0.0);
        params.set(4, 20.0);
        params.set(params.index_of("voice").expect("voice"), 0.0);
        params.set(params.index_of("gate").expect("gate"), 0.0);
        let mut dsp = Dsp::new(f64::from(RATE));
        use crate::dsp_util::Biquad;
        let input = tone(8_192, 1_000.0, 0.2);
        let mut highpass = Biquad::new();
        highpass.highpass(f64::from(RATE), 20.0, FRAC_1_SQRT_2);
        let expected: Vec<f32> = input
            .iter()
            .map(|&sample| highpass.process(sample))
            .collect();
        let output = process_blocks(&mut dsp, &params, &input);
        for index in (LATENCY_SAMPLES + 4_096)..input.len() {
            let expected = expected[index - LATENCY_SAMPLES];
            assert!(
                (output[index] - expected).abs() < 2.0e-3,
                "mix 0 not dry at {index}: {} != {}",
                output[index],
                expected
            );
        }
    }

    #[test]
    fn reduction_is_not_masked_by_default_gate_and_voice() {
        let len = 4 * RATE as usize;
        let input = noise(len);
        let mut levels = Vec::new();
        for reduction in [20.0_f32, 40.0, 60.0] {
            let params = params();
            params.set(0, 100.0);
            params.set(1, reduction);
            // defaults: Voice 60, Gate 30
            let mut dsp = Dsp::new(f64::from(RATE));
            let output = process_blocks(&mut dsp, &params, &input);
            levels.push(rms(&output[len - 8_000..]));
        }
        assert!(
            levels[0] > levels[1] && levels[1] > levels[2],
            "default gate/voice masked reduction: {levels:?}"
        );
    }

    #[test]
    fn mix_zero_impulse_latency_is_256() {
        let params = params();
        params.set(0, 0.0);
        params.set(3, 0.0);
        params.set(4, 20.0);
        params.set(params.index_of("voice").expect("voice"), 0.0);
        params.set(params.index_of("gate").expect("gate"), 0.0);
        let mut input = vec![0.0f32; 8_192];
        input[0] = 1.0;
        let mut dsp = Dsp::new(f64::from(RATE));
        let output = process_blocks(&mut dsp, &params, &input);
        let first = output
            .iter()
            .position(|sample| sample.abs() > 1.0e-6)
            .expect("no output");
        assert_eq!(first, LATENCY_SAMPLES);
    }

    #[test]
    fn reduction_keeps_increasing_past_20_db() {
        let len = 4 * RATE as usize;
        let input = noise(len);
        let mut levels = Vec::new();
        for reduction in [20.0_f32, 40.0, 60.0] {
            let params = params();
            params.set(0, 100.0);
            params.set(1, reduction);
            params.set(params.index_of("voice").expect("voice"), 0.0);
            params.set(params.index_of("gate").expect("gate"), 0.0);
            let mut dsp = Dsp::new(f64::from(RATE));
            let output = process_blocks(&mut dsp, &params, &input);
            levels.push(rms(&output[len - 8_000..]));
        }
        assert!(
            levels[0] > levels[1] && levels[1] > levels[2],
            "reduction stalled: {levels:?}"
        );
    }

    #[test]
    fn parameter_extremes_stay_finite() {
        let input: Vec<f32> = tone(2_048, 220.0, 0.4)
            .iter()
            .zip(&noise(2_048))
            .map(|(tone, noise)| tone + noise)
            .collect();
        for extreme in [false, true] {
            let params = params();
            params.set(0, if extreme { 100.0 } else { 0.0 });
            params.set(1, if extreme { 60.0 } else { 0.0 });
            params.set(2, if extreme { 100.0 } else { 0.0 });
            params.set(3, if extreme { 100.0 } else { 0.0 });
            params.set(4, if extreme { 300.0 } else { 20.0 });
            let mut dsp = Dsp::new(f64::from(RATE));
            let output = process_blocks(&mut dsp, &params, &input);
            assert!(output.iter().all(|sample| sample.is_finite()));
        }
    }

    #[test]
    fn nan_input_does_not_poison_later_output() {
        let params = params();
        let mut dsp = Dsp::new(f64::from(RATE));
        let mut left = vec![f32::NAN; 512];
        let mut right = vec![f32::NAN; 512];
        left[1] = f32::INFINITY;
        right[2] = f32::NEG_INFINITY;
        dsp.process(&mut left, &mut right, &params);
        assert!(
            left.iter()
                .chain(right.iter())
                .all(|sample| sample.is_finite())
        );

        let input = tone(2_048, 1_000.0, 0.3);
        let output = process_blocks(&mut dsp, &params, &input);
        assert!(output.iter().all(|sample| sample.is_finite()));
        let peak = output.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()));
        assert!(peak > 0.01, "later output stayed silent: peak {peak}");
    }
}
