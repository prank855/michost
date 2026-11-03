//! Dynamic noise gate with automatic noise-floor tracking.
//!
//! A minimum-statistics tracker keeps the quietest 50 ms window from the last
//! three seconds. The gate threshold is that floor plus `Offset`, so a stable
//! background is learned without a manual threshold. The audio path uses the
//! same ratio/range expander as the Noise Gate. The detected floor is published
//! through the analysis tap for the editor.

use std::sync::Arc;

use crate::dsp_util::{EnvelopeFollower, OnePole, db_to_gain, gain_to_db, smoothing_coefficient};
use crate::{AudioAnalysis, BuiltinDsp, ParamCurve, ParamSet, ParamSpec, ParamStyle};

pub static SPECS: &[ParamSpec] = &[
    ParamSpec {
        key: "offset_db",
        name: "Offset",
        unit: "dB",
        min: 0.0,
        max: 30.0,
        default: 6.0,
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

const SMOOTHING_MS: f32 = 15.0;
const FLOOR_MIN_DB: f32 = -90.0;
const FLOOR_MAX_DB: f32 = 0.0;
const FLOOR_WINDOW_MS: f32 = 50.0;
const FLOOR_HISTORY: usize = 60;
const FLOOR_DETECTOR_ATTACK_MS: f32 = 2.0;
const FLOOR_DETECTOR_RELEASE_MS: f32 = 50.0;

/// Minimum-statistics floor estimate: the quietest 50 ms window in the last
/// three seconds. Window minima are recomputed only once per window.
struct NoiseFloor {
    windows: [f32; FLOOR_HISTORY],
    write: usize,
    window_min: f32,
    window_samples: usize,
    window_limit: usize,
    floor_db: f32,
}

impl NoiseFloor {
    fn new(sample_rate: f64) -> Self {
        let limit = (sample_rate * 0.001 * f64::from(FLOOR_WINDOW_MS))
            .round()
            .max(1.0) as usize;
        Self {
            windows: [f32::INFINITY; FLOOR_HISTORY],
            write: 0,
            window_min: f32::INFINITY,
            window_samples: 0,
            window_limit: limit,
            floor_db: FLOOR_MIN_DB,
        }
    }

    fn update(&mut self, level_db: f32) {
        if level_db < self.window_min {
            self.window_min = level_db;
        }
        self.window_samples += 1;
        if self.window_samples < self.window_limit {
            return;
        }

        let value = if self.window_min.is_finite() {
            self.window_min.clamp(FLOOR_MIN_DB, FLOOR_MAX_DB)
        } else {
            FLOOR_MIN_DB
        };
        self.windows[self.write] = value;
        self.write = (self.write + 1) % FLOOR_HISTORY;
        self.window_min = f32::INFINITY;
        self.window_samples = 0;
        self.floor_db = self
            .windows
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min)
            .clamp(FLOOR_MIN_DB, FLOOR_MAX_DB);
    }

    fn value_db(&self) -> f32 {
        self.floor_db
    }

    fn reset(&mut self) {
        self.windows = [f32::INFINITY; FLOOR_HISTORY];
        self.write = 0;
        self.window_min = f32::INFINITY;
        self.window_samples = 0;
        self.floor_db = FLOOR_MIN_DB;
    }
}

pub struct Dsp {
    sample_rate: f64,
    detector: EnvelopeFollower,
    floor_detector: EnvelopeFollower,
    floor: NoiseFloor,
    mix: OnePole,
    gain: f32,
    target_db: f32,
    target_gain: f32,
    open: bool,
    hold: u32,
    primed: bool,
    floor_primed: bool,
    analysis: Option<Arc<AudioAnalysis>>,
}

impl Dsp {
    pub fn new(sample_rate: f64, analysis: Option<Arc<AudioAnalysis>>) -> Self {
        let sample_rate = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            48_000.0
        };
        Self {
            sample_rate,
            detector: EnvelopeFollower::new(1.0, 150.0, sample_rate),
            floor_detector: EnvelopeFollower::new(
                FLOOR_DETECTOR_ATTACK_MS,
                FLOOR_DETECTOR_RELEASE_MS,
                sample_rate,
            ),
            floor: NoiseFloor::new(sample_rate),
            mix: OnePole::from_time(SMOOTHING_MS, sample_rate),
            gain: 1.0,
            target_db: 0.0,
            target_gain: 1.0,
            open: false,
            hold: 0,
            primed: false,
            floor_primed: false,
            analysis,
        }
    }
}

impl BuiltinDsp for Dsp {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let offset = params.get(0);
        let range = params.get(1).max(0.0);
        let ratio = params.get(2).max(1.0);
        let attack_ms = params.get(3);
        let hold_ms = params.get(4).max(0.0);
        let release_ms = params.get(5);
        let hysteresis = params.get(6).max(0.0);
        let mix_target = (params.get(7) * 0.01).clamp(0.0, 1.0);
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
            self.floor_detector.reset();
            self.floor.reset();
            self.primed = true;
            self.floor_primed = false;
        }

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let linked = l.abs().max(r.abs());
            let level = self.detector.process(linked);
            let level_db = gain_to_db(level);
            if !self.floor_primed {
                self.floor_detector.set_value(linked);
                self.floor_primed = true;
            }
            self.floor
                .update(gain_to_db(self.floor_detector.process(linked)));
            let threshold = (self.floor.value_db() + offset).clamp(FLOOR_MIN_DB, FLOOR_MAX_DB);
            let close_threshold = threshold - hysteresis;

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

        if let Some(analysis) = &self.analysis {
            analysis.set_noise_floor_db(self.floor.value_db());
        }
    }

    fn reset(&mut self) {
        self.detector.reset();
        self.floor_detector.reset();
        self.floor.reset();
        self.mix.reset();
        self.gain = 1.0;
        self.target_db = 0.0;
        self.target_gain = 1.0;
        self.open = false;
        self.hold = 0;
        self.primed = false;
        self.floor_primed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BuiltinKind;

    const SR: f64 = 48_000.0;

    #[test]
    fn floor_tracker_keeps_the_quietest_recent_window() {
        let mut floor = NoiseFloor::new(1_000.0);
        for _ in 0..100 {
            floor.update(-60.0);
        }
        assert!(
            (floor.value_db() + 60.0).abs() < 1.0,
            "{}",
            floor.value_db()
        );
    }

    #[test]
    fn floor_tracker_follows_a_rising_floor_after_the_history_expires() {
        let mut floor = NoiseFloor::new(1_000.0);
        for _ in 0..(FLOOR_HISTORY * 50) {
            floor.update(-60.0);
        }
        for _ in 0..(FLOOR_HISTORY * 50 + 50) {
            floor.update(-40.0);
        }
        assert!(floor.value_db() > -41.0, "{}", floor.value_db());
    }

    #[test]
    fn gate_learns_the_noise_floor_and_opens_for_speech() {
        let analysis = AudioAnalysis::new(SR as f32);
        let params = ParamSet::new(BuiltinKind::DynamicGate);
        let mut dsp = Dsp::new(SR, Some(analysis.clone()));

        let mut noise_left = vec![0.001; 48_000];
        let mut noise_right = noise_left.clone();
        dsp.process(&mut noise_left, &mut noise_right, &params);
        let mut scope_left = [0.0; 16];
        let mut scope_right = [0.0; 16];
        let snapshot = analysis.snapshot(&mut scope_left, &mut scope_right);
        assert!(
            (snapshot.noise_floor_db + 60.0).abs() < 2.0,
            "floor {}",
            snapshot.noise_floor_db
        );

        let mut speech_left = vec![0.2; 4_800];
        let mut speech_right = speech_left.clone();
        dsp.process(&mut speech_left, &mut speech_right, &params);
        assert!(speech_left[4_799] > 0.15, "{}", speech_left[4_799]);
    }

    #[test]
    fn gate_attenuates_noise_below_the_learned_threshold() {
        let params = ParamSet::new(BuiltinKind::DynamicGate);
        let mut dsp = Dsp::new(SR, None);
        let mut left = vec![0.001; 48_000];
        let mut right = left.clone();
        dsp.process(&mut left, &mut right, &params);
        assert!(left[47_999].abs() < 1e-4, "{}", left[47_999]);
    }
}
