//! Multi-tap echo.
//!
//! Up to four taps are spread between `time_ms` and `time_ms * (1 + spread)`.
//! Later taps are quieter, and the last tap feeds back through a one-pole
//! lowpass so repeats darken as they decay. The right channel uses a small
//! extra offset for width. Circular lines are preallocated with headroom for
//! the maximum spread, and all tap parameters are smoothed.

use std::f64::consts::PI;

use crate::dsp_util::{OnePole, smoothing_coefficient};
use crate::{BuiltinDsp, ParamCurve, ParamSet, ParamSpec, ParamStyle};

pub static SPECS: &[ParamSpec] = &[
    ParamSpec {
        key: "time_ms",
        name: "Time",
        unit: "ms",
        min: 20.0,
        max: 1500.0,
        default: 350.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Echo",
        visible_when: None,
    },
    ParamSpec {
        key: "taps",
        name: "Taps",
        unit: "",
        min: 1.0,
        max: 4.0,
        default: 3.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Echo",
        visible_when: None,
    },
    ParamSpec {
        key: "spread",
        name: "Spread",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 50.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Echo",
        visible_when: None,
    },
    ParamSpec {
        key: "feedback",
        name: "Feedback",
        unit: "%",
        min: 0.0,
        max: 90.0,
        default: 25.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Echo",
        visible_when: None,
    },
    ParamSpec {
        key: "tone_hz",
        name: "Tone",
        unit: "Hz",
        min: 200.0,
        max: 12_000.0,
        default: 5_000.0,
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
        default: 30.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Output",
        visible_when: None,
    },
];

const MAX_TAPS: usize = 4;
/// 1500 ms maximum tap spacing plus room for the widest spread and offsets.
const MAX_ECHO_SECONDS: f32 = 3.2;
const TAP_GAIN: f32 = 0.65;
const TAP_DECAY: f32 = 0.7;
const MAX_FEEDBACK: f32 = 0.9;
/// Stereo tap offset at full spread, in milliseconds.
const MAX_STEREO_OFFSET_MS: f32 = 2.0;

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

fn tone_coefficient(tone_hz: f32, sample_rate: f64) -> f32 {
    let nyquist = sample_rate * 0.5;
    let cutoff = f64::from(tone_hz.clamp(1.0, (nyquist * 0.98) as f32));
    (1.0 - (-2.0 * PI * cutoff / sample_rate).exp()) as f32
}

/// Circular line with a float-safe fractional read.
///
/// The shared helper computes its read position in `f32`, where a tiny
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

    fn max_delay_samples(&self) -> f32 {
        (self.buffer.len() - 2) as f32
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

pub struct Dsp {
    sample_rate: f64,
    line_l: Line,
    line_r: Line,
    tap_delay: [OnePole; MAX_TAPS],
    tap_gain: [OnePole; MAX_TAPS],
    width_offset: OnePole,
    feedback: OnePole,
    mix: OnePole,
    tone_l: OnePole,
    tone_r: OnePole,
    initialized: bool,
}

impl Dsp {
    pub fn new(sample_rate: f64) -> Self {
        let sample_rate = clean_rate(sample_rate);
        let tap_delay = [OnePole::new(smoothing_coefficient(30.0, sample_rate)); MAX_TAPS];
        let tap_gain = [OnePole::new(smoothing_coefficient(15.0, sample_rate)); MAX_TAPS];
        Self {
            sample_rate,
            line_l: Line::new(MAX_ECHO_SECONDS, sample_rate),
            line_r: Line::new(MAX_ECHO_SECONDS, sample_rate),
            tap_delay,
            tap_gain,
            width_offset: OnePole::new(smoothing_coefficient(20.0, sample_rate)),
            feedback: OnePole::new(smoothing_coefficient(20.0, sample_rate)),
            mix: OnePole::new(smoothing_coefficient(20.0, sample_rate)),
            tone_l: OnePole::new(1.0),
            tone_r: OnePole::new(1.0),
            initialized: false,
        }
    }
}

impl BuiltinDsp for Dsp {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let time_ms = params.get(0).clamp(20.0, 1500.0);
        let taps = params.get(1).round().clamp(1.0, 4.0) as usize;
        let spread = params.get(2).clamp(0.0, 100.0) * 0.01;
        let feedback = (params.get(3).clamp(0.0, 90.0) * 0.01).min(MAX_FEEDBACK);
        let tone_hz = params.get(4).clamp(200.0, 12_000.0);
        let mix = params.get(5).clamp(0.0, 100.0) * 0.01;

        let sample_rate = self.sample_rate;
        let max_delay = self
            .line_l
            .max_delay_samples()
            .min(self.line_r.max_delay_samples());
        let mut delay_targets = [0.0_f32; MAX_TAPS];
        let mut gain_targets = [0.0_f32; MAX_TAPS];
        for (index, (delay, gain)) in delay_targets
            .iter_mut()
            .zip(gain_targets.iter_mut())
            .enumerate()
        {
            let position = if taps > 1 {
                index.min(taps - 1) as f32 / (taps - 1) as f32
            } else {
                0.0
            };
            let tap_ms = time_ms * (1.0 + spread * position);
            *delay = (tap_ms * 0.001 * sample_rate as f32).clamp(1.0, max_delay.max(1.0));
            *gain = if index < taps {
                TAP_GAIN * TAP_DECAY.powi(index as i32)
            } else {
                0.0
            };
        }
        let offset_target = (spread * MAX_STEREO_OFFSET_MS * 0.001 * sample_rate as f32)
            .clamp(0.0, max_delay.max(0.0));

        let coefficient = tone_coefficient(tone_hz, sample_rate).clamp(0.0, 1.0);
        self.tone_l.set_coefficient(coefficient);
        self.tone_r.set_coefficient(coefficient);

        if !self.initialized {
            for index in 0..MAX_TAPS {
                self.tap_delay[index].set_value(delay_targets[index]);
                self.tap_gain[index].set_value(gain_targets[index]);
            }
            self.width_offset.set_value(offset_target);
            self.feedback.set_value(feedback);
            self.mix.set_value(mix);
            self.initialized = true;
        }

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let feedback_now = self.feedback.process(feedback);
            let mix_now = self.mix.process(mix);
            let offset_now = self.width_offset.process(offset_target);

            let mut wet_l = 0.0;
            let mut wet_r = 0.0;
            let mut last_l = 0.0;
            let mut last_r = 0.0;

            for index in 0..MAX_TAPS {
                let delay = self.tap_delay[index].process(delay_targets[index]);
                let gain = self.tap_gain[index].process(gain_targets[index]);
                let right_delay = (delay + offset_now).min(max_delay.max(1.0));
                let tap_l = self.line_l.read(delay);
                let tap_r = self.line_r.read(right_delay);
                wet_l += tap_l * gain;
                wet_r += tap_r * gain;
                if index + 1 == taps {
                    last_l = tap_l;
                    last_r = tap_r;
                }
            }

            let input_l = finite(*l);
            let input_r = finite(*r);
            let tone_l = self.tone_l.process(last_l);
            let tone_r = self.tone_r.process(last_r);

            self.line_l.push(finite(input_l + feedback_now * tone_l));
            self.line_r.push(finite(input_r + feedback_now * tone_r));

            *l = input_l * (1.0 - mix_now) + wet_l * mix_now;
            *r = input_r * (1.0 - mix_now) + wet_r * mix_now;
        }
    }

    fn reset(&mut self) {
        self.line_l.clear();
        self.line_r.clear();
        for delay in &mut self.tap_delay {
            delay.reset();
        }
        for gain in &mut self.tap_gain {
            gain.reset();
        }
        self.width_offset.reset();
        self.feedback.reset();
        self.mix.reset();
        self.tone_l.reset();
        self.tone_r.reset();
        self.initialized = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BuiltinKind;

    fn run_impulse(dsp: &mut Dsp, params: &ParamSet, total: usize) -> Vec<f32> {
        let block = 512;
        let mut output = Vec::with_capacity(total);
        let mut left = vec![0.0_f32; block];
        let mut right = vec![0.0_f32; block];
        let mut done = 0;
        while done < total {
            let len = (total - done).min(block);
            left[..len].fill(0.0);
            right[..len].fill(0.0);
            if done == 0 {
                left[0] = 1.0;
                right[0] = 1.0;
            }
            dsp.process(&mut left[..len], &mut right[..len], params);
            output.extend_from_slice(&left[..len]);
            done += len;
        }
        output
    }

    fn peak_between(samples: &[f32], start: usize, end: usize) -> (usize, f32) {
        let mut best = (start, 0.0_f32);
        for (index, sample) in samples
            .iter()
            .enumerate()
            .take(end.min(samples.len()))
            .skip(start)
        {
            if sample.abs() > best.1 {
                best = (index, sample.abs());
            }
        }
        best
    }

    #[test]
    fn three_taps_are_spaced_and_decrease() {
        let params = ParamSet::new(BuiltinKind::Echo);
        params.set(0, 100.0);
        params.set(1, 3.0);
        params.set(2, 100.0);
        params.set(3, 0.0);
        params.set(5, 100.0);

        let mut dsp = Dsp::new(48_000.0);
        let output = run_impulse(&mut dsp, &params, 12_000);

        let (first_index, first) = peak_between(&output, 4600, 5000);
        let (second_index, second) = peak_between(&output, 7000, 7400);
        let (third_index, third) = peak_between(&output, 9400, 9800);

        assert!(
            (first_index as i32 - 4800).abs() <= 2,
            "first tap at {first_index}"
        );
        assert!(
            (second_index as i32 - 7200).abs() <= 2,
            "second tap at {second_index}"
        );
        assert!(
            (third_index as i32 - 9600).abs() <= 2,
            "third tap at {third_index}"
        );
        assert!(first > second && second > third, "{first} {second} {third}");
        assert!(third > 0.1, "third tap too quiet: {third}");
    }

    #[test]
    fn one_tap_produces_a_single_echo() {
        let params = ParamSet::new(BuiltinKind::Echo);
        params.set(0, 100.0);
        params.set(1, 1.0);
        params.set(2, 100.0);
        params.set(3, 0.0);
        params.set(5, 100.0);

        let mut dsp = Dsp::new(48_000.0);
        let output = run_impulse(&mut dsp, &params, 12_000);

        let (index, amplitude) = peak_between(&output, 4600, 5000);
        assert!((index as i32 - 4800).abs() <= 2, "tap at {index}");
        assert!(amplitude > 0.5, "tap too quiet: {amplitude}");

        let (_, later) = peak_between(&output, 6000, output.len());
        assert!(later < 1.0e-5, "unexpected extra tap: {later}");
    }
}
