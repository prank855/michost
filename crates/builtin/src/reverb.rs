//! Stereo Freeverb-style reverb.
//!
//! Eight damped comb filters feed four allpass filters per channel. Comb
//! feedback is derived from the requested decay time (RT60) and clamped below
//! unity, so the tail always decays even at extreme settings. The input passes
//! a pre-delay line and a low-cut highpass before entering the tanks; the two
//! tank outputs are mixed with a width control and blended with the dry signal.

use std::array::from_fn;

use crate::dsp_util::{Biquad, OnePole, smoothing_coefficient};
use crate::{BuiltinDsp, ParamCurve, ParamSet, ParamSpec, ParamStyle};

pub static SPECS: &[ParamSpec] = &[
    ParamSpec {
        key: "size",
        name: "Size",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 45.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Reverb",
        visible_when: None,
    },
    ParamSpec {
        key: "decay_s",
        name: "Decay",
        unit: "s",
        min: 0.2,
        max: 10.0,
        default: 2.0,
        curve: ParamCurve::Log,
        decimals: 2,
        style: ParamStyle::Knob,
        section: "Reverb",
        visible_when: None,
    },
    ParamSpec {
        key: "damping",
        name: "Damping",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 50.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Reverb",
        visible_when: None,
    },
    ParamSpec {
        key: "pre_delay_ms",
        name: "Pre-delay",
        unit: "ms",
        min: 0.0,
        max: 200.0,
        default: 20.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Reverb",
        visible_when: None,
    },
    ParamSpec {
        key: "width",
        name: "Width",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 100.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Reverb",
        visible_when: None,
    },
    ParamSpec {
        key: "low_cut_hz",
        name: "Low Cut",
        unit: "Hz",
        min: 20.0,
        max: 500.0,
        default: 100.0,
        curve: ParamCurve::Log,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Tone",
        visible_when: None,
    },
    ParamSpec {
        key: "mix",
        name: "Mix",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 25.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Output",
        visible_when: None,
    },
];

/// Classic Freeverb tunings at 44.1 kHz, scaled to the session rate.
const COMB_TUNING: [usize; NUM_COMBS] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_TUNING: [usize; NUM_ALLPASS] = [556, 441, 341, 225];
const NUM_COMBS: usize = 8;
const NUM_ALLPASS: usize = 4;
const STEREO_SPREAD: usize = 23;
const ALLPASS_FEEDBACK: f32 = 0.5;
const INPUT_GAIN: f32 = 0.025;
/// Hard ceiling for comb feedback, kept strictly below unity so the tail
/// always decays.
const MAX_FEEDBACK: f32 = 0.979;
const PRE_DELAY_MAX_SECONDS: f32 = 0.205;

#[inline]
fn finite(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

fn clean_rate(sample_rate: f64) -> f64 {
    if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        48_000.0
    }
}

fn scaled_delay(tuning: usize, spread: usize, sample_rate: f64) -> f32 {
    ((tuning + spread) as f64 * sample_rate / 44_100.0).max(1.0) as f32
}

fn feedback_for(delay_samples: f32, sample_rate: f64, decay_s: f32) -> f32 {
    let decay = f64::from(decay_s.max(0.05));
    let passes = f64::from(delay_samples) / sample_rate / decay;
    let feedback = 0.001_f64.powf(passes) as f32;
    feedback.clamp(0.0, MAX_FEEDBACK)
}

/// Circular line with a float-safe fractional read.
///
/// `dsp_util::DelayLine` computes its read position in `f32`, where a tiny
/// negative remainder can round up to the buffer length. This variant uses
/// `f64` and wraps an out-of-range position back to zero.
struct Line {
    buffer: Vec<f32>,
    write: usize,
}

impl Line {
    fn new(max_seconds: f32, sample_rate: f64) -> Self {
        let rate = clean_rate(sample_rate);
        let seconds = max_seconds.max(0.001);
        let samples = (f64::from(seconds) * rate).ceil() as usize + 2;
        Self {
            buffer: vec![0.0; samples.max(4)],
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

    #[inline]
    fn read(&self, delay: f32) -> f32 {
        let length = self.buffer.len();
        let max = (length - 2) as f32;
        let delay = if delay.is_finite() {
            delay.clamp(1.0, max)
        } else {
            1.0
        };
        let mut position = (self.write as f64 - f64::from(delay)).rem_euclid(length as f64);
        if position >= length as f64 {
            position = 0.0;
        }
        let index = (position.floor() as usize).min(length - 1);
        let fraction = (position - index as f64) as f32;
        let next = if index + 1 == length { 0 } else { index + 1 };
        self.buffer[index] + (self.buffer[next] - self.buffer[index]) * fraction
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
    }
}

struct Comb {
    line: Line,
    delay: f32,
    feedback: f32,
    damp_state: f32,
}

impl Comb {
    fn new(sample_rate: f64, tuning: usize, spread: usize) -> Self {
        let delay = scaled_delay(tuning, spread, sample_rate);
        let max_seconds = ((f64::from(delay) + 8.0) / sample_rate) as f32;
        Self {
            line: Line::new(max_seconds, sample_rate),
            delay,
            feedback: 0.0,
            damp_state: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32, target_feedback: f32, damp: f32, smooth: f32) -> f32 {
        self.feedback += smooth * (target_feedback - self.feedback);
        let delayed = self.line.read(self.delay);
        self.damp_state += (1.0 - damp) * (delayed - self.damp_state);
        let write = finite(input + self.damp_state * self.feedback);
        self.line.push(write);
        delayed
    }

    fn reset(&mut self) {
        self.line.clear();
        self.feedback = 0.0;
        self.damp_state = 0.0;
    }
}

struct Allpass {
    line: Line,
    delay: f32,
}

impl Allpass {
    fn new(sample_rate: f64, tuning: usize, spread: usize) -> Self {
        let delay = scaled_delay(tuning, spread, sample_rate);
        let max_seconds = ((f64::from(delay) + 8.0) / sample_rate) as f32;
        Self {
            line: Line::new(max_seconds, sample_rate),
            delay,
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.line.read(self.delay);
        let write = finite(input + delayed * ALLPASS_FEEDBACK);
        self.line.push(write);
        delayed - input
    }

    fn reset(&mut self) {
        self.line.clear();
    }
}

struct Tank {
    combs: [Comb; NUM_COMBS],
    allpasses: [Allpass; NUM_ALLPASS],
    pre_delay: Line,
    low_cut: Biquad,
}

impl Tank {
    fn new(sample_rate: f64, right: bool) -> Self {
        let spread = if right { STEREO_SPREAD } else { 0 };
        let mut low_cut = Biquad::new();
        low_cut.highpass(sample_rate, 100.0, 0.707);
        Self {
            combs: from_fn(|index| Comb::new(sample_rate, COMB_TUNING[index], spread)),
            allpasses: from_fn(|index| Allpass::new(sample_rate, ALLPASS_TUNING[index], spread)),
            pre_delay: Line::new(PRE_DELAY_MAX_SECONDS, sample_rate),
            low_cut,
        }
    }

    fn set_low_cut(&mut self, sample_rate: f64, cutoff_hz: f32) {
        self.low_cut.highpass(sample_rate, cutoff_hz, 0.707);
    }

    #[inline]
    fn process(
        &mut self,
        input: f32,
        pre_delay: f32,
        feedback: &[f32; NUM_COMBS],
        feedback_smooth: f32,
        damp: f32,
    ) -> f32 {
        let filtered = self.low_cut.process(finite(input));
        self.pre_delay.push(filtered);
        let delayed = self.pre_delay.read(pre_delay);

        let mut sum = 0.0;
        for (comb, target) in self.combs.iter_mut().zip(feedback) {
            sum += comb.process(delayed * INPUT_GAIN, *target, damp, feedback_smooth);
        }
        for allpass in &mut self.allpasses {
            sum = allpass.process(sum);
        }
        finite(sum)
    }

    fn reset(&mut self) {
        for comb in &mut self.combs {
            comb.reset();
        }
        for allpass in &mut self.allpasses {
            allpass.reset();
        }
        self.pre_delay.clear();
        self.low_cut.reset();
    }
}

pub struct Dsp {
    sample_rate: f64,
    left: Tank,
    right: Tank,
    mix: OnePole,
    width: OnePole,
    pre_delay: OnePole,
    feedback_smooth: f32,
    initialized: bool,
}

impl Dsp {
    pub fn new(sample_rate: f64) -> Self {
        let sample_rate = clean_rate(sample_rate);
        Self {
            sample_rate,
            left: Tank::new(sample_rate, false),
            right: Tank::new(sample_rate, true),
            mix: OnePole::new(smoothing_coefficient(20.0, sample_rate)),
            width: OnePole::new(smoothing_coefficient(20.0, sample_rate)),
            pre_delay: OnePole::new(smoothing_coefficient(30.0, sample_rate)),
            feedback_smooth: smoothing_coefficient(40.0, sample_rate),
            initialized: false,
        }
    }
}

impl BuiltinDsp for Dsp {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let size = params.get(0).clamp(0.0, 100.0) * 0.01;
        let decay = params.get(1).clamp(0.2, 10.0);
        let damping = params.get(2).clamp(0.0, 100.0) * 0.01;
        let pre_delay_ms = params.get(3).clamp(0.0, 200.0);
        let width = params.get(4).clamp(0.0, 100.0) * 0.01;
        let low_cut_hz = params.get(5).clamp(20.0, 500.0);
        let mix = params.get(6).clamp(0.0, 100.0) * 0.01;

        let damp = (damping * 0.9).clamp(0.0, 0.9);
        let effective_decay = decay * (0.35 + 1.25 * size);
        let mut left_feedback = [0.0_f32; NUM_COMBS];
        let mut right_feedback = [0.0_f32; NUM_COMBS];
        for (comb, target) in self.left.combs.iter().zip(left_feedback.iter_mut()) {
            *target = feedback_for(comb.delay, self.sample_rate, effective_decay);
        }
        for (comb, target) in self.right.combs.iter().zip(right_feedback.iter_mut()) {
            *target = feedback_for(comb.delay, self.sample_rate, effective_decay);
        }

        let max_pre_delay = (PRE_DELAY_MAX_SECONDS as f64 * self.sample_rate) as f32;
        let pre_delay_target =
            (pre_delay_ms * 0.001 * self.sample_rate as f32).clamp(1.0, max_pre_delay.max(1.0));
        let mix_target = mix.clamp(0.0, 1.0);
        let width_target = width.clamp(0.0, 1.0);

        self.left.set_low_cut(self.sample_rate, low_cut_hz);
        self.right.set_low_cut(self.sample_rate, low_cut_hz);

        if !self.initialized {
            self.mix.set_value(mix_target);
            self.width.set_value(width_target);
            self.pre_delay.set_value(pre_delay_target);
            for (comb, target) in self.left.combs.iter_mut().zip(&left_feedback) {
                comb.feedback = *target;
            }
            for (comb, target) in self.right.combs.iter_mut().zip(&right_feedback) {
                comb.feedback = *target;
            }
            self.initialized = true;
        }

        let feedback_smooth = self.feedback_smooth;

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mix_now = self.mix.process(mix_target);
            let width_now = self.width.process(width_target);
            let pre_delay_now = self.pre_delay.process(pre_delay_target);

            let wet_l = self
                .left
                .process(*l, pre_delay_now, &left_feedback, feedback_smooth, damp);
            let wet_r =
                self.right
                    .process(*r, pre_delay_now, &right_feedback, feedback_smooth, damp);

            let wet1 = width_now * 0.5 + 0.5;
            let wet2 = (1.0 - width_now) * 0.5;
            let out_l = wet_l * wet1 + wet_r * wet2;
            let out_r = wet_r * wet1 + wet_l * wet2;

            let dry_l = finite(*l);
            let dry_r = finite(*r);
            *l = dry_l * (1.0 - mix_now) + out_l * mix_now;
            *r = dry_r * (1.0 - mix_now) + out_r * mix_now;
        }
    }

    fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
        self.mix.reset();
        self.width.reset();
        self.pre_delay.reset();
        self.initialized = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BuiltinKind;

    #[test]
    fn impulse_produces_decaying_tail_without_nan() {
        let sample_rate = 48_000.0;
        let mut dsp = Dsp::new(sample_rate);
        let params = ParamSet::new(BuiltinKind::Reverb);
        params.set(0, 60.0);
        params.set(1, 1.5);
        params.set(6, 100.0);

        let block = 512;
        let total = sample_rate as usize * 2;
        let mut left = vec![0.0_f32; block];
        let mut right = vec![0.0_f32; block];
        let mut early_energy = 0.0_f64;
        let mut late_energy = 0.0_f64;
        let mut done = 0;

        while done < total {
            let len = (total - done).min(block);
            left[..len].fill(0.0);
            right[..len].fill(0.0);
            if done == 0 {
                left[0] = 1.0;
                right[0] = 1.0;
            }
            dsp.process(&mut left[..len], &mut right[..len], &params);

            for (offset, (l, r)) in left[..len].iter().zip(&right[..len]).enumerate() {
                assert!(l.is_finite() && r.is_finite());
                let time = (done + offset) as f64 / sample_rate;
                let energy = f64::from(l * l + r * r);
                if (0.2..0.5).contains(&time) {
                    early_energy += energy;
                }
                if (1.5..1.8).contains(&time) {
                    late_energy += energy;
                }
            }
            done += len;
        }

        assert!(early_energy > 0.0, "reverb produced no tail");
        assert!(
            late_energy < early_energy * 0.5,
            "tail did not decay: early {early_energy}, late {late_energy}"
        );
    }
}
