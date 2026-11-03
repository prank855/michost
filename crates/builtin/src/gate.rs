//! Stereo-linked noise gate.
//!
//! A peak detector tracks `max(|L|, |R|)` with independent attack/release
//! times. The gate opens above the threshold and closes below
//! `threshold - hysteresis`, after holding for the configured time. The applied
//! gain glides to `-range` dB while closed so the floor never jumps.

use crate::dsp_util::{EnvelopeFollower, OnePole, db_to_gain, gain_to_db, smoothing_coefficient};
use crate::{BuiltinDsp, ParamCurve, ParamSet, ParamSpec, ParamStyle};

pub static SPECS: &[ParamSpec] = &[
    ParamSpec {
        key: "threshold_db",
        name: "Threshold",
        unit: "dB",
        min: -80.0,
        max: 0.0,
        default: -45.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Gate",
        visible_when: None,
    },
    ParamSpec {
        key: "range_db",
        name: "Range",
        unit: "dB",
        min: 0.0,
        max: 80.0,
        default: 40.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Gate",
        visible_when: None,
    },
    ParamSpec {
        key: "ratio",
        name: "Ratio",
        unit: ":1",
        min: 1.0,
        max: 100.0,
        default: 10.0,
        curve: ParamCurve::Log,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Gate",
        visible_when: None,
    },
    ParamSpec {
        key: "attack_ms",
        name: "Attack",
        unit: "ms",
        min: 0.1,
        max: 50.0,
        default: 1.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Gate",
        visible_when: None,
    },
    ParamSpec {
        key: "hold_ms",
        name: "Hold",
        unit: "ms",
        min: 0.0,
        max: 500.0,
        default: 50.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Gate",
        visible_when: None,
    },
    ParamSpec {
        key: "release_ms",
        name: "Release",
        unit: "ms",
        min: 5.0,
        max: 2_000.0,
        default: 150.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Gate",
        visible_when: None,
    },
    ParamSpec {
        key: "hysteresis_db",
        name: "Hysteresis",
        unit: "dB",
        min: 0.0,
        max: 20.0,
        default: 3.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Gate",
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
        section: "Gate",
        visible_when: None,
    },
];

pub(crate) const LEGACY_VALUE_COUNT: usize = 7;

/// Inserts the ratio control after Range. Older projects stored
/// threshold, range, attack, hold, release, hysteresis and mix contiguously.
pub(crate) fn migrate_legacy(values: &[f32]) -> Vec<f32> {
    let value = |index: usize| values.get(index).copied().unwrap_or(0.0);
    vec![
        value(0),
        value(1),
        10.0,
        value(2),
        value(3),
        value(4),
        value(5),
        value(6),
    ]
}

const SMOOTHING_MS: f32 = 15.0;

pub struct Dsp {
    sample_rate: f64,
    detector: EnvelopeFollower,
    mix: OnePole,
    gain: f32,
    target_db: f32,
    target_gain: f32,
    open: bool,
    hold: u32,
    primed: bool,
}

impl Dsp {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            sample_rate,
            detector: EnvelopeFollower::new(1.0, 150.0, sample_rate),
            mix: OnePole::from_time(SMOOTHING_MS, sample_rate),
            gain: 1.0,
            target_db: 0.0,
            target_gain: 1.0,
            open: false,
            hold: 0,
            primed: false,
        }
    }
}

impl BuiltinDsp for Dsp {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let threshold = params.get(0);
        let range = params.get(1).max(0.0);
        let ratio = params.get(2).max(1.0);
        let attack_ms = params.get(3);
        let hold_ms = params.get(4).max(0.0);
        let release_ms = params.get(5);
        let hysteresis = params.get(6).max(0.0);
        let mix_target = (params.get(7) * 0.01).clamp(0.0, 1.0);
        let close_threshold = threshold - hysteresis;
        let attack = smoothing_coefficient(attack_ms, self.sample_rate);
        let release = smoothing_coefficient(release_ms, self.sample_rate);
        let closed_gain = db_to_gain(-range);
        let slope = (ratio - 1.0).max(0.0);
        let hold_samples = (f64::from(hold_ms) * 0.001 * self.sample_rate).round() as u32;

        self.detector
            .set_times(attack_ms, release_ms, self.sample_rate);

        if !self.primed {
            self.mix.set_value(mix_target);
            self.gain = closed_gain;
            self.target_db = -range;
            self.target_gain = closed_gain;
            self.open = false;
            self.hold = 0;
            self.detector.reset();
            self.primed = true;
        }

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let linked = l.abs().max(r.abs());
            let level = self.detector.process(linked);
            let level_db = gain_to_db(level);

            if !self.open && level_db > threshold {
                self.open = true;
                self.hold = hold_samples;
            }

            if self.open {
                if level_db < close_threshold {
                    if self.hold > 0 {
                        self.hold -= 1;
                    } else {
                        self.open = false;
                    }
                } else {
                    self.hold = hold_samples;
                }
            }

            let target = if self.open {
                1.0
            } else {
                let depth = (threshold - level_db).max(0.0);
                let target_db = -(depth * slope).min(range);
                if (target_db - self.target_db).abs() > 0.05 {
                    self.target_db = target_db;
                    self.target_gain = db_to_gain(target_db);
                }
                self.target_gain
            };
            let coefficient = if target > self.gain { attack } else { release };
            self.gain += coefficient * (target - self.gain);

            let dry_l = *l;
            let dry_r = *r;
            let mix = self.mix.process(mix_target);
            *l = dry_l + (dry_l * self.gain - dry_l) * mix;
            *r = dry_r + (dry_r * self.gain - dry_r) * mix;
        }
    }

    fn reset(&mut self) {
        self.detector.reset();
        self.mix.reset();
        self.gain = 1.0;
        self.target_db = 0.0;
        self.target_gain = 1.0;
        self.open = false;
        self.hold = 0;
        self.primed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn configure(params: &ParamSet, threshold: f32, range: f32, hold: f32, release: f32) {
        params.set(0, threshold);
        params.set(1, range);
        params.set(2, 10.0);
        params.set(3, 0.1);
        params.set(4, hold);
        params.set(5, release);
        params.set(6, 0.0);
        params.set(7, 100.0);
    }

    #[test]
    fn attenuates_signal_below_threshold() {
        let params = ParamSet::new(crate::BuiltinKind::NoiseGate);
        configure(&params, -20.0, 60.0, 0.0, 5.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![0.001; 9_600];
        let mut right = vec![0.001; 9_600];
        dsp.process(&mut left, &mut right, &params);
        let tail = &left[8_000..];
        assert!(
            tail.iter().all(|sample| sample.abs() < 1e-4),
            "peak {}",
            tail.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()))
        );
    }

    #[test]
    fn passes_signal_above_threshold() {
        let params = ParamSet::new(crate::BuiltinKind::NoiseGate);
        configure(&params, -20.0, 60.0, 0.0, 5.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![0.5; 9_600];
        let mut right = vec![0.5; 9_600];
        dsp.process(&mut left, &mut right, &params);
        assert!((left[9_599] - 0.5).abs() < 1e-3, "{}", left[9_599]);
        assert!((right[9_599] - 0.5).abs() < 1e-3, "{}", right[9_599]);
    }

    #[test]
    fn hold_keeps_gate_open_after_signal_stops() {
        let params = ParamSet::new(crate::BuiltinKind::NoiseGate);
        configure(&params, -20.0, 80.0, 200.0, 5.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![0.001; 14_400];
        let mut right = vec![0.001; 14_400];
        left[..2_400].fill(0.5);
        right[..2_400].fill(0.5);
        dsp.process(&mut left, &mut right, &params);
        let during_hold = &left[3_400..6_000];
        let peak = during_hold.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()));
        assert!(peak > 5e-4, "gate closed during hold: {peak}");
        assert!(
            left[14_399].abs() < 1e-4,
            "gate stayed open: {}",
            left[14_399]
        );
    }

    #[test]
    fn lower_ratio_keeps_more_of_the_low_level_signal() {
        let params = ParamSet::new(crate::BuiltinKind::NoiseGate);
        configure(&params, -20.0, 60.0, 0.0, 5.0);
        let mut soft = vec![0.001; 9_600];
        let mut soft_right = soft.clone();
        params.set(2, 2.0);
        Dsp::new(SR).process(&mut soft, &mut soft_right, &params);

        let mut hard = vec![0.001; 9_600];
        let mut hard_right = hard.clone();
        params.set(2, 100.0);
        Dsp::new(SR).process(&mut hard, &mut hard_right, &params);

        let peak = |samples: &[f32]| {
            samples[8_000..]
                .iter()
                .fold(0.0_f32, |acc, sample| acc.max(sample.abs()))
        };
        assert!(
            peak(&soft) > peak(&hard) * 3.0,
            "{} vs {}",
            peak(&soft),
            peak(&hard)
        );
    }

    #[test]
    fn legacy_seven_value_state_gets_the_default_ratio() {
        let legacy: [f32; 7] = [-30.0, 40.0, 1.0, 50.0, 150.0, 3.0, 100.0];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MHB1");
        bytes.extend_from_slice(&(legacy.len() as u32).to_le_bytes());
        bytes.push(crate::BuiltinKind::NoiseGate.index());
        bytes.extend_from_slice(&[0, 0, 0]);
        for value in legacy {
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        let params = ParamSet::new(crate::BuiltinKind::NoiseGate);
        assert!(params.decode(&bytes));
        assert_eq!(params.get_key("threshold_db"), -30.0);
        assert_eq!(params.get_key("range_db"), 40.0);
        assert_eq!(params.get_key("ratio"), 10.0);
        assert_eq!(params.get_key("attack_ms"), 1.0);
        assert_eq!(params.get_key("mix"), 100.0);
    }

    #[test]
    fn reset_returns_to_closed_state() {
        let params = ParamSet::new(crate::BuiltinKind::NoiseGate);
        configure(&params, -20.0, 60.0, 0.0, 5.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![0.5; 2_400];
        let mut right = vec![0.5; 2_400];
        dsp.process(&mut left, &mut right, &params);
        dsp.reset();
        let mut left = vec![0.001; 64];
        let mut right = vec![0.001; 64];
        dsp.process(&mut left, &mut right, &params);
        assert!(left[0].abs() < 1e-5, "{}", left[0]);
    }
}
