//! Gain: smoothed gain, constant-power pan and phase invert.
//!
//! Gain and pan are one-pole smoothed so knob moves never click. The pan law
//! is an equal-power balance: the attenuated side follows a cosine curve while
//! the opposite side stays at unity, so a centred stereo signal is unchanged.

use std::f32::consts::FRAC_PI_2;

use crate::dsp_util::{OnePole, db_to_gain};
use crate::{BuiltinDsp, ParamCurve, ParamSet, ParamSpec, ParamStyle};

pub static SPECS: &[ParamSpec] = &[
    ParamSpec {
        key: "gain_db",
        name: "Gain",
        unit: "dB",
        min: -24.0,
        max: 24.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Gain",
        visible_when: None,
    },
    ParamSpec {
        key: "pan",
        name: "Pan",
        unit: "%",
        min: -100.0,
        max: 100.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Gain",
        visible_when: None,
    },
    ParamSpec {
        key: "phase",
        name: "Invert",
        unit: "",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Toggle,
        section: "Gain",
        visible_when: None,
    },
];

const SMOOTHING_MS: f32 = 15.0;

/// Equal-power balance gains for a pan position in `-1..=1`.
///
/// The attenuated channel follows `cos(pi/2 * |pan|)`; the other channel is
/// left at unity, so `pan == 0` is exactly unity on both sides.
#[inline]
fn pan_gains(pan: f32) -> (f32, f32) {
    let pan = if pan.is_finite() {
        pan.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let left = if pan <= 0.0 {
        1.0
    } else {
        (pan * FRAC_PI_2).cos()
    };
    let right = if pan >= 0.0 {
        1.0
    } else {
        (-pan * FRAC_PI_2).cos()
    };
    (left, right)
}

pub struct Dsp {
    gain_db: OnePole,
    pan: OnePole,
    phase: OnePole,
    primed: bool,
}

impl Dsp {
    pub fn new(sample_rate: f64) -> Self {
        let mut dsp = Self {
            gain_db: OnePole::from_time(SMOOTHING_MS, sample_rate),
            pan: OnePole::from_time(SMOOTHING_MS, sample_rate),
            phase: OnePole::from_time(SMOOTHING_MS, sample_rate),
            primed: false,
        };
        dsp.snap_to_defaults();
        dsp
    }

    fn snap_to_defaults(&mut self) {
        self.gain_db.set_value(0.0);
        self.pan.set_value(0.0);
        self.phase.set_value(1.0);
    }
}

impl BuiltinDsp for Dsp {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let gain_db = params.get(0);
        let pan = params.get(1) * 0.01;
        let phase_target = if params.get(2) >= 0.5 { -1.0 } else { 1.0 };

        if !self.primed {
            self.gain_db.set_value(gain_db);
            self.pan.set_value(pan);
            self.phase.set_value(phase_target);
            self.primed = true;
        }

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let gain = db_to_gain(self.gain_db.process(gain_db));
            let (pan_l, pan_r) = pan_gains(self.pan.process(pan));
            let phase = self.phase.process(phase_target);

            *l = *l * gain * pan_l * phase;
            *r = *r * gain * pan_r * phase;
        }
    }

    fn reset(&mut self) {
        self.gain_db.reset();
        self.pan.reset();
        self.phase.reset();
        self.snap_to_defaults();
        self.primed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn run(dsp: &mut Dsp, params: &ParamSet, left: &mut [f32], right: &mut [f32]) {
        dsp.process(left, right, params);
    }

    #[test]
    fn defaults_are_unity() {
        let params = ParamSet::new(crate::BuiltinKind::Gain);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![0.25; 512];
        let mut right = vec![-0.5; 512];
        run(&mut dsp, &params, &mut left, &mut right);
        for (l, r) in left.iter().zip(&right) {
            assert!((l - 0.25).abs() < 1e-6, "{l}");
            assert!((r + 0.5).abs() < 1e-6, "{r}");
        }
    }

    #[test]
    fn gain_db_changes_level() {
        let params = ParamSet::new(crate::BuiltinKind::Gain);
        params.set(0, 12.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![1.0; 4_800];
        let mut right = vec![1.0; 4_800];
        run(&mut dsp, &params, &mut left, &mut right);
        let expected = db_to_gain(12.0);
        assert!((left[4_799] - expected).abs() < 1e-4, "{}", left[4_799]);
    }

    #[test]
    fn pan_attenuates_one_side() {
        let params = ParamSet::new(crate::BuiltinKind::Gain);
        params.set(1, -100.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![1.0; 512];
        let mut right = vec![1.0; 512];
        run(&mut dsp, &params, &mut left, &mut right);
        assert!((left[511] - 1.0).abs() < 1e-6);
        assert!(right[511].abs() < 1e-6, "{}", right[511]);
    }

    #[test]
    fn phase_invert_flips_polarity() {
        let params = ParamSet::new(crate::BuiltinKind::Gain);
        params.set(2, 1.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![0.5; 256];
        let mut right = vec![0.5; 256];
        run(&mut dsp, &params, &mut left, &mut right);
        assert!((left[255] + 0.5).abs() < 1e-6);
        assert!((right[255] + 0.5).abs() < 1e-6);
    }

    #[test]
    fn legacy_four_value_state_ignores_the_removed_mix() {
        let legacy: [f32; 4] = [6.0, 0.0, 1.0, 50.0];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MHB1");
        bytes.extend_from_slice(&(legacy.len() as u32).to_le_bytes());
        bytes.push(crate::BuiltinKind::Gain.index());
        bytes.extend_from_slice(&[0, 0, 0]);
        for value in legacy {
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        let params = ParamSet::new(crate::BuiltinKind::Gain);
        assert!(params.decode(&bytes));
        assert_eq!(params.get_key("gain_db"), 6.0);
        assert_eq!(params.get_key("pan"), 0.0);
        assert_eq!(params.get_key("phase"), 1.0);
    }
}
