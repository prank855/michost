//! Stereo delay with feedback tone shaping and optional ping-pong routing.
//!
//! Both channels use preallocated two-second circular lines. Delay time,
//! feedback and mix are smoothed to avoid clicks when a knob moves; reads are
//! fractionally interpolated. The feedback path runs through a one-pole
//! lowpass and is hard-clamped below unity so the line cannot self-oscillate.

use std::f64::consts::PI;

use crate::dsp_util::{OnePole, smoothing_coefficient};
use crate::{BuiltinDsp, ParamCurve, ParamSet, ParamSpec, ParamStyle};

pub static SPECS: &[ParamSpec] = &[
    ParamSpec {
        key: "time_ms",
        name: "Time",
        unit: "ms",
        min: 1.0,
        max: 2000.0,
        default: 250.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Delay",
        visible_when: None,
    },
    ParamSpec {
        key: "feedback",
        name: "Feedback",
        unit: "%",
        min: 0.0,
        max: 95.0,
        default: 35.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Delay",
        visible_when: None,
    },
    ParamSpec {
        key: "tone_hz",
        name: "Tone",
        unit: "Hz",
        min: 200.0,
        max: 12_000.0,
        default: 6_000.0,
        curve: ParamCurve::Log,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Tone",
        visible_when: None,
    },
    ParamSpec {
        key: "ping_pong",
        name: "Ping-Pong",
        unit: "",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Toggle,
        section: "Delay",
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

const MAX_DELAY_SECONDS: f32 = 2.0;
/// Feedback is capped here even if a stale state value bypasses the spec.
const MAX_FEEDBACK: f32 = 0.95;

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
    delay: OnePole,
    feedback: OnePole,
    mix: OnePole,
    tone_l: OnePole,
    tone_r: OnePole,
    initialized: bool,
}

impl Dsp {
    pub fn new(sample_rate: f64) -> Self {
        let sample_rate = clean_rate(sample_rate);
        Self {
            sample_rate,
            line_l: Line::new(MAX_DELAY_SECONDS, sample_rate),
            line_r: Line::new(MAX_DELAY_SECONDS, sample_rate),
            delay: OnePole::new(smoothing_coefficient(30.0, sample_rate)),
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
        let time_ms = params.get(0).clamp(1.0, 2000.0);
        let feedback = (params.get(1).clamp(0.0, 95.0) * 0.01).min(MAX_FEEDBACK);
        let tone_hz = params.get(2).clamp(200.0, 12_000.0);
        let ping_pong = params.get(3) >= 0.5;
        let mix = params.get(4).clamp(0.0, 100.0) * 0.01;

        let max_delay = self.line_l.max_delay_samples();
        let delay_target =
            (time_ms * 0.001 * self.sample_rate as f32).clamp(1.0, max_delay.max(1.0));
        let coefficient = tone_coefficient(tone_hz, self.sample_rate).clamp(0.0, 1.0);
        self.tone_l.set_coefficient(coefficient);
        self.tone_r.set_coefficient(coefficient);

        if !self.initialized {
            self.delay.set_value(delay_target);
            self.feedback.set_value(feedback);
            self.mix.set_value(mix);
            self.initialized = true;
        }

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let delay_now = self.delay.process(delay_target);
            let feedback_now = self.feedback.process(feedback);
            let mix_now = self.mix.process(mix);

            let delayed_l = self.line_l.read(delay_now);
            let delayed_r = self.line_r.read(delay_now);
            let tone_l = self.tone_l.process(delayed_l);
            let tone_r = self.tone_r.process(delayed_r);

            let input_l = finite(*l);
            let input_r = finite(*r);

            if ping_pong {
                let mono = (input_l + input_r) * 0.5;
                self.line_l.push(finite(mono + feedback_now * tone_r));
                self.line_r.push(finite(feedback_now * tone_l));
            } else {
                self.line_l.push(finite(input_l + feedback_now * tone_l));
                self.line_r.push(finite(input_r + feedback_now * tone_r));
            }

            *l = input_l * (1.0 - mix_now) + delayed_l * mix_now;
            *r = input_r * (1.0 - mix_now) + delayed_r * mix_now;
        }
    }

    fn reset(&mut self) {
        self.line_l.clear();
        self.line_r.clear();
        self.delay.reset();
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
    fn impulse_arrives_at_the_requested_time() {
        let params = ParamSet::new(BuiltinKind::Delay);
        params.set(0, 100.0);
        params.set(1, 0.0);
        params.set(2, 12_000.0);
        params.set(4, 100.0);

        let mut dsp = Dsp::new(48_000.0);
        let output = run_impulse(&mut dsp, &params, 6000);
        let (index, amplitude) = peak_between(&output, 0, output.len());

        assert!(
            (index as i32 - 4800).abs() <= 2,
            "expected echo at 4800, found {index}"
        );
        assert!(amplitude > 0.9, "echo too quiet: {amplitude}");
    }

    #[test]
    fn feedback_repeats_decay() {
        let params = ParamSet::new(BuiltinKind::Delay);
        params.set(0, 50.0);
        params.set(1, 50.0);
        params.set(2, 12_000.0);
        params.set(4, 100.0);

        let mut dsp = Dsp::new(48_000.0);
        let output = run_impulse(&mut dsp, &params, 9000);

        let (first_index, first) = peak_between(&output, 2200, 2700);
        let (second_index, second) = peak_between(&output, 4600, 5100);
        let (third_index, third) = peak_between(&output, 7000, 7500);

        assert!(
            (first_index as i32 - 2400).abs() <= 2
                && (second_index as i32 - 4800).abs() <= 2
                && (third_index as i32 - 7200).abs() <= 2,
            "unexpected repeat positions {first_index}, {second_index}, {third_index}"
        );
        assert!(first > second && second > third, "{first} {second} {third}");
        assert!(third > 1.0e-4, "repeats decayed too fast: {third}");
    }
}
