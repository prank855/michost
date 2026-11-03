//! Three-band tone equalizer with low/high cuts.
//!
//! Bass, mid and treble use fixed frequencies (low shelf at 100 Hz, peak at
//! 1 kHz, high shelf at 8 kHz). The cuts are 12 dB/octave Butterworth
//! highpass/lowpass filters. A cut sitting at the edge of its range is
//! bypassed, so the default state is bit-exact flat. Coefficients are cached
//! per stage and only rebuilt when a control changes; the stage cascade runs
//! low cut -> bass -> mid -> treble -> high cut, then the smoothed output gain.

use crate::dsp_util::{Biquad, OnePole, db_to_gain};
use crate::{BuiltinDsp, ParamCurve, ParamSet, ParamSpec, ParamStyle};

pub const LOW_CUT_MIN_HZ: f32 = 20.0;
pub const HIGH_CUT_MAX_HZ: f32 = 20_000.0;
pub const BASS_HZ: f32 = 100.0;
pub const MID_HZ: f32 = 1_000.0;
pub const TREBLE_HZ: f32 = 8_000.0;
pub const SHELF_SLOPE: f32 = 0.7;
pub const MID_Q: f32 = 0.9;
pub const CUT_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

pub static SPECS: &[ParamSpec] = &[
    ParamSpec {
        key: "low_cut_hz",
        name: "Low Cut",
        unit: "Hz",
        min: LOW_CUT_MIN_HZ,
        max: 400.0,
        default: LOW_CUT_MIN_HZ,
        curve: ParamCurve::Log,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Tone",
        visible_when: None,
    },
    ParamSpec {
        key: "high_cut_hz",
        name: "High Cut",
        unit: "Hz",
        min: 2_000.0,
        max: HIGH_CUT_MAX_HZ,
        default: HIGH_CUT_MAX_HZ,
        curve: ParamCurve::Log,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Tone",
        visible_when: None,
    },
    ParamSpec {
        key: "bass_db",
        name: "Bass",
        unit: "dB",
        min: -12.0,
        max: 12.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Tone",
        visible_when: None,
    },
    ParamSpec {
        key: "mid_db",
        name: "Mid",
        unit: "dB",
        min: -12.0,
        max: 12.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Tone",
        visible_when: None,
    },
    ParamSpec {
        key: "treble_db",
        name: "Treble",
        unit: "dB",
        min: -12.0,
        max: 12.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Tone",
        visible_when: None,
    },
    ParamSpec {
        key: "output_db",
        name: "Output",
        unit: "dB",
        min: -12.0,
        max: 12.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Tone",
        visible_when: None,
    },
];

const LOW_CUT_INDEX: usize = 0;
const HIGH_CUT_INDEX: usize = 1;
const BASS_INDEX: usize = 2;
const MID_INDEX: usize = 3;
const TREBLE_INDEX: usize = 4;
const OUTPUT_GAIN_INDEX: usize = 5;
const STAGES: usize = 5;

pub(crate) const LEGACY_VALUE_COUNT: usize = 16;

/// Maps the old five-band state (`low gain, low freq, low Q, mid 1..3,
/// high, output`) onto the simplified layout. The middle band's gain is kept
/// and the cut controls stay at their bypass defaults.
pub(crate) fn migrate_legacy(values: &[f32]) -> Vec<f32> {
    let value = |index: usize| {
        let value = values.get(index).copied().unwrap_or(0.0);
        if value.is_finite() { value } else { 0.0 }
    };
    vec![
        LOW_CUT_MIN_HZ,
        HIGH_CUT_MAX_HZ,
        value(0),
        value(6),
        value(12),
        value(15),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StageKind {
    HighPass,
    LowShelf,
    Peak,
    HighShelf,
    LowPass,
}

struct Stage {
    kind: StageKind,
    frequency: f32,
    gain_db: f32,
    q: f32,
    enabled: bool,
    left: Biquad,
    right: Biquad,
}

impl Stage {
    fn new(kind: StageKind) -> Self {
        Self {
            kind,
            // NaN never compares equal, so the first block always configures.
            frequency: f32::NAN,
            gain_db: f32::NAN,
            q: f32::NAN,
            enabled: false,
            left: Biquad::new(),
            right: Biquad::new(),
        }
    }

    fn update(
        &mut self,
        sample_rate: f64,
        kind: StageKind,
        frequency: f32,
        gain_db: f32,
        q: f32,
        enabled: bool,
    ) {
        if self.kind == kind
            && self.frequency == frequency
            && self.gain_db == gain_db
            && self.q == q
            && self.enabled == enabled
        {
            return;
        }
        self.kind = kind;
        self.frequency = frequency;
        self.gain_db = gain_db;
        self.q = q;
        self.enabled = enabled;
        configure(
            &mut self.left,
            kind,
            sample_rate,
            frequency,
            gain_db,
            q,
            enabled,
        );
        configure(
            &mut self.right,
            kind,
            sample_rate,
            frequency,
            gain_db,
            q,
            enabled,
        );
    }

    fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
    }
}

fn sanitize_rate(sample_rate: f64) -> f64 {
    if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        48_000.0
    }
}

fn configure(
    filter: &mut Biquad,
    kind: StageKind,
    sample_rate: f64,
    frequency: f32,
    gain_db: f32,
    q: f32,
    enabled: bool,
) {
    if !enabled
        || (matches!(
            kind,
            StageKind::LowShelf | StageKind::Peak | StageKind::HighShelf
        ) && gain_db.abs() < 1.0e-6)
    {
        // Unity RBJ coefficients are mathematically flat, but the transposed
        // direct form still has a decaying transient. Use an exact bypass.
        *filter = Biquad::new();
        return;
    }
    match kind {
        StageKind::HighPass => filter.highpass(sample_rate, frequency, q),
        StageKind::LowShelf => filter.low_shelf(sample_rate, frequency, gain_db, q),
        StageKind::Peak => filter.peaking(sample_rate, frequency, q, gain_db),
        StageKind::HighShelf => filter.high_shelf(sample_rate, frequency, gain_db, q),
        StageKind::LowPass => filter.lowpass(sample_rate, frequency, q),
    }
}

pub struct Dsp {
    sample_rate: f64,
    stages: [Stage; STAGES],
    output_gain: OnePole,
}

impl Dsp {
    pub fn new(sample_rate: f64) -> Self {
        let sample_rate = sanitize_rate(sample_rate);
        let mut output_gain = OnePole::from_time(10.0, sample_rate);
        output_gain.set_value(1.0);
        Self {
            sample_rate,
            stages: [
                Stage::new(StageKind::HighPass),
                Stage::new(StageKind::LowShelf),
                Stage::new(StageKind::Peak),
                Stage::new(StageKind::HighShelf),
                Stage::new(StageKind::LowPass),
            ],
            output_gain,
        }
    }
}

impl BuiltinDsp for Dsp {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let frames = left.len().min(right.len());
        if frames == 0 {
            return;
        }

        let low_cut = params.get(LOW_CUT_INDEX);
        let high_cut = params.get(HIGH_CUT_INDEX);
        let bass = params.get(BASS_INDEX);
        let mid = params.get(MID_INDEX);
        let treble = params.get(TREBLE_INDEX);

        self.stages[0].update(
            self.sample_rate,
            StageKind::HighPass,
            low_cut,
            0.0,
            CUT_Q,
            low_cut > LOW_CUT_MIN_HZ + 0.01,
        );
        self.stages[1].update(
            self.sample_rate,
            StageKind::LowShelf,
            BASS_HZ,
            bass,
            SHELF_SLOPE,
            true,
        );
        self.stages[2].update(self.sample_rate, StageKind::Peak, MID_HZ, mid, MID_Q, true);
        self.stages[3].update(
            self.sample_rate,
            StageKind::HighShelf,
            TREBLE_HZ,
            treble,
            SHELF_SLOPE,
            true,
        );
        self.stages[4].update(
            self.sample_rate,
            StageKind::LowPass,
            high_cut,
            0.0,
            CUT_Q,
            high_cut < HIGH_CUT_MAX_HZ - 0.01,
        );

        let output_target = db_to_gain(params.get(OUTPUT_GAIN_INDEX));

        for (sample_l, sample_r) in left[..frames].iter_mut().zip(right[..frames].iter_mut()) {
            let mut mono_l = *sample_l;
            let mut mono_r = *sample_r;
            for stage in &mut self.stages {
                mono_l = stage.left.process(mono_l);
                mono_r = stage.right.process(mono_r);
            }
            let gain = self.output_gain.process(output_target);
            *sample_l = mono_l * gain;
            *sample_r = mono_r * gain;
        }
    }

    fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.reset();
        }
        self.output_gain.set_value(1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BuiltinKind;

    const SAMPLE_RATE: f32 = 48_000.0;

    fn sine(freq_hz: f32, len: usize) -> Vec<f32> {
        let scale = 2.0 * std::f32::consts::PI * freq_hz / SAMPLE_RATE;
        (0..len)
            .map(|index| 0.25 * (scale * index as f32).sin())
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn run(params: &ParamSet, input: &[f32]) -> Vec<f32> {
        let mut dsp = Dsp::new(f64::from(SAMPLE_RATE));
        let mut left = input.to_vec();
        let mut right = input.to_vec();
        dsp.process(&mut left, &mut right, params);
        left
    }

    fn set(params: &ParamSet, key: &str, value: f32) {
        let index = params
            .index_of(key)
            .unwrap_or_else(|| panic!("missing {key}"));
        params.set(index, value);
    }

    fn response(params: &ParamSet, signal_hz: f32) -> f32 {
        let input = sine(signal_hz, 9_600);
        let output = run(params, &input);
        rms(&output[4_800..]) / rms(&input[4_800..])
    }

    #[test]
    fn flat_settings_pass_through() {
        let params = ParamSet::new(BuiltinKind::Eq);
        let input = sine(1_000.0, 2_048);
        assert_eq!(run(&params, &input), input);
    }

    #[test]
    fn tone_knobs_move_the_expected_range() {
        let params = ParamSet::new(BuiltinKind::Eq);

        set(&params, "bass_db", 12.0);
        let bass_boost = response(&params, 50.0);
        set(&params, "bass_db", -12.0);
        let bass_cut = response(&params, 50.0);
        assert!(bass_boost > 2.0, "bass boost {bass_boost}");
        assert!(bass_cut < 0.5, "bass cut {bass_cut}");

        set(&params, "bass_db", 0.0);
        set(&params, "mid_db", 12.0);
        let mid_boost = response(&params, MID_HZ);
        set(&params, "mid_db", -12.0);
        let mid_cut = response(&params, MID_HZ);
        assert!(mid_boost > 2.0, "mid boost {mid_boost}");
        assert!(mid_cut < 0.5, "mid cut {mid_cut}");

        set(&params, "mid_db", 0.0);
        set(&params, "treble_db", 12.0);
        let treble_boost = response(&params, 16_000.0);
        set(&params, "treble_db", -12.0);
        let treble_cut = response(&params, 16_000.0);
        assert!(treble_boost > 2.0, "treble boost {treble_boost}");
        assert!(treble_cut < 0.5, "treble cut {treble_cut}");
    }

    #[test]
    fn cuts_remove_the_selected_end_of_the_spectrum() {
        let params = ParamSet::new(BuiltinKind::Eq);

        set(&params, "low_cut_hz", 200.0);
        let low_cut = response(&params, 50.0);
        set(&params, "low_cut_hz", LOW_CUT_MIN_HZ);
        let low_open = response(&params, 50.0);
        assert!(low_cut < 0.5, "low cut {low_cut}");
        assert!(low_open > 0.95, "low open {low_open}");

        set(&params, "high_cut_hz", 2_000.0);
        let high_cut = response(&params, 10_000.0);
        set(&params, "high_cut_hz", HIGH_CUT_MAX_HZ);
        let high_open = response(&params, 10_000.0);
        assert!(high_cut < 0.5, "high cut {high_cut}");
        assert!(high_open > 0.95, "high open {high_open}");
    }

    #[test]
    fn legacy_five_band_state_maps_to_the_simple_controls() {
        let legacy: [f32; 16] = [
            -9.0, 80.0, 0.7, 1.0, 250.0, 1.0, 2.5, 1_000.0, 1.0, -3.0, 4_000.0, 1.0, 4.0, 12_000.0,
            0.7, 1.5,
        ];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MHB1");
        bytes.extend_from_slice(&(legacy.len() as u32).to_le_bytes());
        bytes.push(BuiltinKind::Eq.index());
        bytes.extend_from_slice(&[0, 0, 0]);
        for value in legacy {
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        let params = ParamSet::new(BuiltinKind::Eq);
        assert!(params.decode(&bytes));
        assert_eq!(params.get_key("low_cut_hz"), LOW_CUT_MIN_HZ);
        assert_eq!(params.get_key("high_cut_hz"), HIGH_CUT_MAX_HZ);
        assert_eq!(params.get_key("bass_db"), -9.0);
        assert_eq!(params.get_key("mid_db"), 2.5);
        assert_eq!(params.get_key("treble_db"), 4.0);
        assert_eq!(params.get_key("output_db"), 1.5);
    }
}
