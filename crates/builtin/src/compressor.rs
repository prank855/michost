//! Compressor: single-band or three-band (LR4) with stereo-linked detection.
//!
//! Detection uses a peak envelope of `max(|L|, |R|)`. Gain is computed from a
//! soft-knee static curve, so the knee, ratio and makeup controls behave like
//! a conventional feed-forward compressor. In multiband mode the signal is
//! split with Linkwitz-Riley 4th-order crossovers, each band is compressed with
//! the same attack/release/knee/makeup settings, and the bands are summed.

use std::f32::consts::FRAC_1_SQRT_2;
use std::f64::consts::PI;

use crate::dsp_util::{Biquad, EnvelopeFollower, OnePole, db_to_gain, gain_to_db};
use crate::{BuiltinDsp, ParamCurve, ParamSet, ParamSpec, ParamStyle};

pub static SPECS: &[ParamSpec] = &[
    ParamSpec {
        key: "threshold_db",
        name: "Threshold",
        unit: "dB",
        min: -60.0,
        max: 0.0,
        default: -20.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Compressor",
        visible_when: None,
    },
    ParamSpec {
        key: "ratio",
        name: "Ratio",
        unit: ":1",
        min: 1.0,
        max: 20.0,
        default: 3.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Compressor",
        visible_when: None,
    },
    ParamSpec {
        key: "attack_ms",
        name: "Attack",
        unit: "ms",
        min: 0.1,
        max: 100.0,
        default: 10.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Compressor",
        visible_when: None,
    },
    ParamSpec {
        key: "release_ms",
        name: "Release",
        unit: "ms",
        min: 10.0,
        max: 1_000.0,
        default: 120.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Compressor",
        visible_when: None,
    },
    ParamSpec {
        key: "knee_db",
        name: "Knee",
        unit: "dB",
        min: 0.0,
        max: 24.0,
        default: 6.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Compressor",
        visible_when: None,
    },
    ParamSpec {
        key: "makeup_db",
        name: "Makeup",
        unit: "dB",
        min: -12.0,
        max: 24.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Output",
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
        key: "multiband",
        name: "Multiband",
        unit: "",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Toggle,
        section: "Multiband",
        visible_when: None,
    },
    ParamSpec {
        key: "low_xover",
        name: "Low X-over",
        unit: "Hz",
        min: 50.0,
        max: 500.0,
        default: 180.0,
        curve: ParamCurve::Log,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Multiband",
        visible_when: Some(("multiband", 1.0)),
    },
    ParamSpec {
        key: "high_xover",
        name: "High X-over",
        unit: "Hz",
        min: 1_000.0,
        max: 12_000.0,
        default: 4_000.0,
        curve: ParamCurve::Log,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Multiband",
        visible_when: Some(("multiband", 1.0)),
    },
    ParamSpec {
        key: "low_threshold_db",
        name: "Low Threshold",
        unit: "dB",
        min: -60.0,
        max: 0.0,
        default: -20.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Multiband",
        visible_when: Some(("multiband", 1.0)),
    },
    ParamSpec {
        key: "low_ratio",
        name: "Low Ratio",
        unit: ":1",
        min: 1.0,
        max: 20.0,
        default: 3.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Multiband",
        visible_when: Some(("multiband", 1.0)),
    },
    ParamSpec {
        key: "mid_threshold_db",
        name: "Mid Threshold",
        unit: "dB",
        min: -60.0,
        max: 0.0,
        default: -20.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Multiband",
        visible_when: Some(("multiband", 1.0)),
    },
    ParamSpec {
        key: "mid_ratio",
        name: "Mid Ratio",
        unit: ":1",
        min: 1.0,
        max: 20.0,
        default: 3.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Multiband",
        visible_when: Some(("multiband", 1.0)),
    },
    ParamSpec {
        key: "high_threshold_db",
        name: "High Threshold",
        unit: "dB",
        min: -60.0,
        max: 0.0,
        default: -20.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Multiband",
        visible_when: Some(("multiband", 1.0)),
    },
    ParamSpec {
        key: "high_ratio",
        name: "High Ratio",
        unit: ":1",
        min: 1.0,
        max: 20.0,
        default: 3.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Multiband",
        visible_when: Some(("multiband", 1.0)),
    },
    ParamSpec {
        key: "oversample",
        name: "Oversample",
        unit: "",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Toggle,
        section: "Quality",
        visible_when: None,
    },
];

const SMOOTHING_MS: f32 = 15.0;

/// Index of the appended `oversample` toggle in [`SPECS`].
const OVERSAMPLE_PARAM: usize = 16;

/// Oversampling factor of the shared anti-alias path.
pub(crate) const OVERSAMPLE_FACTOR: usize = 4;

const OVERSAMPLE_TAPS: usize = 64;
const OVERSAMPLE_HISTORY: usize = OVERSAMPLE_TAPS / OVERSAMPLE_FACTOR;
/// Cutoff in cycles per 4x sample, i.e. the original (host) Nyquist.
const OVERSAMPLE_CUTOFF: f64 = 0.125;

/// Stereo 4x up/down resampler shared by the dynamics effects.
///
/// The prototype is a 64-tap Hamming-windowed sinc lowpass with its cutoff at
/// the original Nyquist frequency (0.125 cycles per 4x sample), normalized to
/// unity DC gain. Upsampling zero-stuffs and filters, then compensates the 1/4
/// duty cycle with a gain of four. Downsampling filters and keeps every fourth
/// 4x sample (phase 3).
///
/// Measured impulse response: the round-trip peak lands 15 host samples after
/// the input (15.75 samples of linear-phase group delay minus the 0.75-sample
/// decimation phase). That is about 0.31 ms at 48 kHz and is independent of the
/// host rate. `BuiltinKind::latency_samples` keeps declaring 0 for the
/// compressor and 10 ms for the limiter, so this small extra path delay is not
/// reported.
///
/// All buffers are allocated in [`Oversampler4x::new`]; `upsample`,
/// `downsample_in_place` and `reset` never allocate, lock or panic.
pub(crate) struct Oversampler4x {
    taps: [f32; OVERSAMPLE_TAPS],
    up_history: [[f32; OVERSAMPLE_HISTORY]; 2],
    down_history: [[f32; OVERSAMPLE_TAPS]; 2],
    scratch_left: Vec<f32>,
    scratch_right: Vec<f32>,
}

impl Oversampler4x {
    pub(crate) fn new() -> Self {
        Self {
            taps: oversampling_taps(),
            up_history: [[0.0; OVERSAMPLE_HISTORY]; 2],
            down_history: [[0.0; OVERSAMPLE_TAPS]; 2],
            scratch_left: vec![0.0; crate::MAX_BLOCK * OVERSAMPLE_FACTOR],
            scratch_right: vec![0.0; crate::MAX_BLOCK * OVERSAMPLE_FACTOR],
        }
    }

    /// Clears the FIR history so the next block starts from silence.
    pub(crate) fn reset(&mut self) {
        self.up_history = [[0.0; OVERSAMPLE_HISTORY]; 2];
        self.down_history = [[0.0; OVERSAMPLE_TAPS]; 2];
        self.scratch_left.fill(0.0);
        self.scratch_right.fill(0.0);
    }

    /// Upsamples `left`/`right` into internal 4x scratch and returns the stereo
    /// slices (four samples per input frame). Blocks above [`crate::MAX_BLOCK`]
    /// are truncated so the scratch stays in bounds.
    pub(crate) fn upsample(&mut self, left: &[f32], right: &[f32]) -> (&mut [f32], &mut [f32]) {
        let frames = left.len().min(right.len()).min(crate::MAX_BLOCK);
        let samples = frames * OVERSAMPLE_FACTOR;
        let Self {
            taps,
            up_history,
            scratch_left,
            scratch_right,
            ..
        } = self;
        upsample_channel(
            taps,
            &mut up_history[0],
            &left[..frames],
            &mut scratch_left[..samples],
        );
        upsample_channel(
            taps,
            &mut up_history[1],
            &right[..frames],
            &mut scratch_right[..samples],
        );
        (&mut scratch_left[..samples], &mut scratch_right[..samples])
    }

    /// Downsamples the internal 4x scratch back into `left`/`right`.
    ///
    /// `frames` is the host-rate block length previously passed to
    /// [`Oversampler4x::upsample`]; it is clamped to the output lengths.
    pub(crate) fn downsample_in_place(
        &mut self,
        frames: usize,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let frames = frames
            .min(left.len())
            .min(right.len())
            .min(crate::MAX_BLOCK);
        let samples = frames * OVERSAMPLE_FACTOR;
        let Self {
            taps,
            down_history,
            scratch_left,
            scratch_right,
            ..
        } = self;
        downsample_channel(
            taps,
            &mut down_history[0],
            &scratch_left[..samples],
            &mut left[..frames],
        );
        downsample_channel(
            taps,
            &mut down_history[1],
            &scratch_right[..samples],
            &mut right[..frames],
        );
    }
}

impl Default for Oversampler4x {
    fn default() -> Self {
        Self::new()
    }
}

fn oversampling_taps() -> [f32; OVERSAMPLE_TAPS] {
    let mut taps = [0.0f32; OVERSAMPLE_TAPS];
    let center = (OVERSAMPLE_TAPS - 1) as f64 * 0.5;
    let two_cutoff = 2.0 * OVERSAMPLE_CUTOFF;
    let mut sum = 0.0f64;
    for (index, tap) in taps.iter_mut().enumerate() {
        let offset = index as f64 - center;
        let sinc = if offset.abs() < 1.0e-12 {
            1.0
        } else {
            let angle = PI * two_cutoff * offset;
            angle.sin() / angle
        };
        let window = 0.54 - 0.46 * (2.0 * PI * index as f64 / (OVERSAMPLE_TAPS - 1) as f64).cos();
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
    taps: &[f32; OVERSAMPLE_TAPS],
    history: &mut [f32; OVERSAMPLE_HISTORY],
    input: &[f32],
    output: &mut [f32],
) {
    for (out, &sample) in output.chunks_exact_mut(OVERSAMPLE_FACTOR).zip(input.iter()) {
        history.copy_within(0..OVERSAMPLE_HISTORY - 1, 1);
        history[0] = sample;
        for (phase, slot) in out.iter_mut().enumerate() {
            let mut sum = 0.0f32;
            for (&coefficient, &delayed) in taps[phase..]
                .iter()
                .step_by(OVERSAMPLE_FACTOR)
                .zip(history.iter())
            {
                sum += coefficient * delayed;
            }
            *slot = sum * OVERSAMPLE_FACTOR as f32;
        }
    }
}

fn downsample_channel(
    taps: &[f32; OVERSAMPLE_TAPS],
    history: &mut [f32; OVERSAMPLE_TAPS],
    input: &[f32],
    output: &mut [f32],
) {
    for (out, chunk) in output.iter_mut().zip(input.chunks_exact(OVERSAMPLE_FACTOR)) {
        for &sample in chunk {
            history.copy_within(0..OVERSAMPLE_TAPS - 1, 1);
            history[0] = sample;
        }
        let mut sum = 0.0f32;
        for (&coefficient, &delayed) in taps.iter().zip(history.iter()) {
            sum += coefficient * delayed;
        }
        *out = sum;
    }
}

/// Gain change in dB for one detector level. Always `<= 0`.
#[inline]
fn static_gain_db(level_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
    if level_db.is_nan() {
        return 0.0;
    }
    let ratio = if ratio.is_finite() {
        ratio.max(1.0)
    } else {
        1.0
    };
    let knee = if knee_db.is_finite() {
        knee_db.max(0.0)
    } else {
        0.0
    };
    let slope = 1.0 - 1.0 / ratio;
    let over = level_db - threshold_db;
    if knee > 0.0 {
        let half = knee * 0.5;
        if over <= -half {
            0.0
        } else if over >= half {
            -(over * slope)
        } else {
            let t = over + half;
            -(slope * t * t / (2.0 * knee))
        }
    } else if over > 0.0 {
        -(over * slope)
    } else {
        0.0
    }
}

/// Linear gain reduction for one linked peak level.
#[inline]
fn compressed_gain(level: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
    let gain_db = static_gain_db(gain_to_db(level), threshold_db, ratio, knee_db);
    if gain_db <= 0.0 {
        db_to_gain(gain_db)
    } else {
        1.0
    }
}

/// Two cascaded biquads form a 4th-order Linkwitz-Riley section.
#[derive(Clone, Copy)]
struct Cascade {
    first: Biquad,
    second: Biquad,
}

impl Cascade {
    fn new() -> Self {
        Self {
            first: Biquad::new(),
            second: Biquad::new(),
        }
    }

    fn lowpass(&mut self, sample_rate: f64, frequency: f32) {
        self.first.lowpass(sample_rate, frequency, FRAC_1_SQRT_2);
        self.second.lowpass(sample_rate, frequency, FRAC_1_SQRT_2);
    }

    fn highpass(&mut self, sample_rate: f64, frequency: f32) {
        self.first.highpass(sample_rate, frequency, FRAC_1_SQRT_2);
        self.second.highpass(sample_rate, frequency, FRAC_1_SQRT_2);
    }

    fn reset(&mut self) {
        self.first.reset();
        self.second.reset();
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        self.second.process(self.first.process(input))
    }
}

#[derive(Clone, Copy)]
struct ChannelFilters {
    low_lp: Cascade,
    low_hp: Cascade,
    high_lp: Cascade,
    high_hp: Cascade,
}

impl ChannelFilters {
    fn new() -> Self {
        Self {
            low_lp: Cascade::new(),
            low_hp: Cascade::new(),
            high_lp: Cascade::new(),
            high_hp: Cascade::new(),
        }
    }

    fn update(&mut self, sample_rate: f64, low_xover: f32, high_xover: f32) {
        self.low_lp.lowpass(sample_rate, low_xover);
        self.low_hp.highpass(sample_rate, low_xover);
        self.high_lp.lowpass(sample_rate, high_xover);
        self.high_hp.highpass(sample_rate, high_xover);
    }

    fn reset(&mut self) {
        self.low_lp.reset();
        self.low_hp.reset();
        self.high_lp.reset();
        self.high_hp.reset();
    }

    #[inline]
    fn split(&mut self, input: f32) -> (f32, f32, f32) {
        let low = self.low_lp.process(input);
        let mid_low = self.low_hp.process(input);
        let mid = self.high_lp.process(mid_low);
        let high = self.high_hp.process(input);
        (low, mid, high)
    }
}

struct Core {
    sample_rate: f64,
    single: EnvelopeFollower,
    bands: [EnvelopeFollower; 3],
    filters_l: ChannelFilters,
    filters_r: ChannelFilters,
    makeup: OnePole,
    mix: OnePole,
    last_low_xover: f32,
    last_high_xover: f32,
    multiband: bool,
    primed: bool,
}

impl Core {
    fn new(sample_rate: f64) -> Self {
        let envelope = EnvelopeFollower::new(10.0, 120.0, sample_rate);
        Self {
            sample_rate,
            single: envelope,
            bands: [envelope; 3],
            filters_l: ChannelFilters::new(),
            filters_r: ChannelFilters::new(),
            makeup: OnePole::from_time(SMOOTHING_MS, sample_rate),
            mix: OnePole::from_time(SMOOTHING_MS, sample_rate),
            last_low_xover: f32::NAN,
            last_high_xover: f32::NAN,
            multiband: false,
            primed: false,
        }
    }

    fn crossover_frequencies(&self, low: f32, high: f32) -> (f32, f32) {
        let rate = if self.sample_rate.is_finite() && self.sample_rate > 0.0 {
            self.sample_rate
        } else {
            48_000.0
        };
        let ceiling = (rate * 0.49).max(100.0) as f32;
        let low_min = (rate * 0.001).max(10.0) as f32;
        let low = if low.is_finite() {
            low.clamp(low_min, ceiling * 0.9)
        } else {
            low_min
        };
        let high_floor = (low * 1.5).min(ceiling);
        let mut high = if high.is_finite() {
            high.clamp(high_floor, ceiling)
        } else {
            high_floor
        };
        if high <= low {
            high = (low * 1.5).min(ceiling).max(low);
        }
        (low, high)
    }

    fn update_crossovers(&mut self, low_xover: f32, high_xover: f32) {
        let (low, high) = self.crossover_frequencies(low_xover, high_xover);
        let changed = self.last_low_xover.is_nan()
            || self.last_high_xover.is_nan()
            || (low - self.last_low_xover).abs() > 1.0e-3
            || (high - self.last_high_xover).abs() > 1.0e-3;
        if changed {
            self.filters_l.update(self.sample_rate, low, high);
            self.filters_r.update(self.sample_rate, low, high);
            self.last_low_xover = low;
            self.last_high_xover = high;
        }
    }
}

impl Core {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let threshold_db = params.get(0);
        let ratio = params.get(1);
        let attack_ms = params.get(2);
        let release_ms = params.get(3);
        let knee_db = params.get(4);
        let makeup_target = db_to_gain(params.get(5));
        let mix_target = (params.get(6) * 0.01).clamp(0.0, 1.0);
        let multiband = params.get(7) >= 0.5;
        let low_threshold_db = params.get(10);
        let low_ratio = params.get(11);
        let mid_threshold_db = params.get(12);
        let mid_ratio = params.get(13);
        let high_threshold_db = params.get(14);
        let high_ratio = params.get(15);

        self.single
            .set_times(attack_ms, release_ms, self.sample_rate);
        for band in &mut self.bands {
            band.set_times(attack_ms, release_ms, self.sample_rate);
        }

        if multiband {
            self.update_crossovers(params.get(8), params.get(9));
        }

        if multiband != self.multiband {
            self.multiband = multiband;
            if multiband {
                self.filters_l.reset();
                self.filters_r.reset();
                for band in &mut self.bands {
                    band.reset();
                }
            } else {
                self.single.reset();
            }
        }

        if !self.primed {
            self.makeup.set_value(makeup_target);
            self.mix.set_value(mix_target);
            self.primed = true;
        }

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let dry_l = *l;
            let dry_r = *r;
            let makeup = self.makeup.process(makeup_target);
            let mix = self.mix.process(mix_target);

            let (wet_l, wet_r) = if multiband {
                let (low_l, mid_l, high_l) = self.filters_l.split(dry_l);
                let (low_r, mid_r, high_r) = self.filters_r.split(dry_r);

                let low_gain = compressed_gain(
                    self.bands[0].process(low_l.abs().max(low_r.abs())),
                    low_threshold_db,
                    low_ratio,
                    knee_db,
                );
                let mid_gain = compressed_gain(
                    self.bands[1].process(mid_l.abs().max(mid_r.abs())),
                    mid_threshold_db,
                    mid_ratio,
                    knee_db,
                );
                let high_gain = compressed_gain(
                    self.bands[2].process(high_l.abs().max(high_r.abs())),
                    high_threshold_db,
                    high_ratio,
                    knee_db,
                );

                (
                    (low_l * low_gain + mid_l * mid_gain + high_l * high_gain) * makeup,
                    (low_r * low_gain + mid_r * mid_gain + high_r * high_gain) * makeup,
                )
            } else {
                let gain = compressed_gain(
                    self.single.process(dry_l.abs().max(dry_r.abs())),
                    threshold_db,
                    ratio,
                    knee_db,
                );
                (dry_l * gain * makeup, dry_r * gain * makeup)
            };

            *l = dry_l + (wet_l - dry_l) * mix;
            *r = dry_r + (wet_r - dry_r) * mix;
        }
    }

    fn reset(&mut self) {
        self.single.reset();
        for band in &mut self.bands {
            band.reset();
        }
        self.filters_l.reset();
        self.filters_r.reset();
        self.makeup.reset();
        self.mix.reset();
        self.last_low_xover = f32::NAN;
        self.last_high_xover = f32::NAN;
        self.primed = false;
    }
}

/// Compressor with an optional 4x oversampled processing core.
///
/// Both cores are preallocated; the host-rate core is used while `oversample`
/// is off (bit-identical to the original implementation) and the 4x core while
/// it is on. The resampler history is cleared on every toggle.
pub struct Dsp {
    host: Core,
    oversampled: Core,
    oversampler: Oversampler4x,
    oversample: bool,
}

impl Dsp {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            host: Core::new(sample_rate),
            oversampled: Core::new(sample_rate * OVERSAMPLE_FACTOR as f64),
            oversampler: Oversampler4x::new(),
            oversample: false,
        }
    }
}

impl BuiltinDsp for Dsp {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let oversample = params.get(OVERSAMPLE_PARAM) >= 0.5;
        if oversample != self.oversample {
            self.oversample = oversample;
            if oversample {
                self.oversampled.reset();
                self.oversampler.reset();
            } else {
                self.host.reset();
            }
        }

        let frames = left.len().min(right.len());
        let mut offset = 0;
        while offset < frames {
            let length = (frames - offset).min(crate::MAX_BLOCK);
            let left_block = &mut left[offset..offset + length];
            let right_block = &mut right[offset..offset + length];
            if oversample {
                let Self {
                    oversampled,
                    oversampler,
                    ..
                } = self;
                let (left4x, right4x) = oversampler.upsample(left_block, right_block);
                oversampled.process(left4x, right4x, params);
                oversampler.downsample_in_place(length, left_block, right_block);
            } else {
                self.host.process(left_block, right_block, params);
            }
            offset += length;
        }
    }

    fn reset(&mut self) {
        self.host.reset();
        self.oversampled.reset();
        self.oversampler.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn configure_single(params: &ParamSet) {
        params.set(0, -20.0);
        params.set(1, 4.0);
        params.set(2, 0.1);
        params.set(3, 100.0);
        params.set(4, 0.0);
        params.set(5, 0.0);
        params.set(6, 100.0);
        params.set(7, 0.0);
    }

    fn configure_multiband(params: &ParamSet) {
        configure_single(params);
        params.set(7, 1.0);
        params.set(8, 180.0);
        params.set(9, 4_000.0);
        params.set(10, -20.0);
        params.set(11, 4.0);
        params.set(12, -20.0);
        params.set(13, 4.0);
        params.set(14, -20.0);
        params.set(15, 4.0);
    }

    #[test]
    fn quiet_signal_passes_unchanged() {
        let params = ParamSet::new(crate::BuiltinKind::Compressor);
        configure_single(&params);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![0.001; 4_800];
        let mut right = vec![0.001; 4_800];
        dsp.process(&mut left, &mut right, &params);
        assert!((left[4_799] - 0.001).abs() < 1e-5, "{}", left[4_799]);
    }

    #[test]
    fn loud_signal_is_compressed_by_ratio() {
        let params = ParamSet::new(crate::BuiltinKind::Compressor);
        configure_single(&params);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![1.0; 4_800];
        let mut right = vec![1.0; 4_800];
        dsp.process(&mut left, &mut right, &params);
        let expected = db_to_gain(-15.0);
        assert!(
            (left[4_799] - expected).abs() < 0.02,
            "{} != {expected}",
            left[4_799]
        );
    }

    #[test]
    fn soft_knee_starts_below_threshold_and_matches_hard_at_edge() {
        let hard_at_threshold = static_gain_db(-20.0, -20.0, 4.0, 0.0);
        let soft_at_threshold = static_gain_db(-20.0, -20.0, 4.0, 6.0);
        assert_eq!(hard_at_threshold, 0.0);
        assert!(soft_at_threshold < 0.0, "{soft_at_threshold}");

        let hard_at_edge = static_gain_db(-17.0, -20.0, 4.0, 0.0);
        let soft_at_edge = static_gain_db(-17.0, -20.0, 4.0, 6.0);
        assert!((hard_at_edge - soft_at_edge).abs() < 1e-5);
    }

    #[test]
    fn makeup_adds_gain_after_compression() {
        let params = ParamSet::new(crate::BuiltinKind::Compressor);
        configure_single(&params);
        params.set(5, 6.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![1.0; 4_800];
        let mut right = vec![1.0; 4_800];
        dsp.process(&mut left, &mut right, &params);
        let expected = db_to_gain(-15.0 + 6.0);
        assert!(
            (left[4_799] - expected).abs() < 0.02,
            "{} != {expected}",
            left[4_799]
        );
    }

    #[test]
    fn multiband_compresses_low_band() {
        let params = ParamSet::new(crate::BuiltinKind::Compressor);
        configure_multiband(&params);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![1.0; 9_600];
        let mut right = vec![1.0; 9_600];
        dsp.process(&mut left, &mut right, &params);
        assert!(left[9_599] < 0.4, "{}", left[9_599]);
        assert!(left.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn multiband_handles_reversed_crossovers() {
        let params = ParamSet::new(crate::BuiltinKind::Compressor);
        configure_multiband(&params);
        params.set(8, 500.0);
        params.set(9, 1_000.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![0.05; 4_800];
        let mut right = vec![0.05; 4_800];
        dsp.process(&mut left, &mut right, &params);
        assert!(left.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn oversample_toggle_defaults_off() {
        let params = ParamSet::new(crate::BuiltinKind::Compressor);
        assert_eq!(params.index_of("oversample"), Some(OVERSAMPLE_PARAM));
        assert_eq!(params.get(OVERSAMPLE_PARAM), 0.0);
        let spec = &params.specs()[OVERSAMPLE_PARAM];
        assert_eq!(spec.section, "Quality");
        assert!(matches!(spec.style, ParamStyle::Toggle));
    }

    #[test]
    fn oversampling_compresses_and_stays_finite() {
        let params = ParamSet::new(crate::BuiltinKind::Compressor);
        configure_single(&params);
        params.set(OVERSAMPLE_PARAM, 1.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![1.0; 4_800];
        let mut right = vec![1.0; 4_800];
        dsp.process(&mut left, &mut right, &params);
        assert!(
            left.iter()
                .chain(right.iter())
                .all(|sample| sample.is_finite())
        );
        let expected = db_to_gain(-15.0);
        assert!(
            (left[4_799] - expected).abs() < 0.02,
            "{} != {expected}",
            left[4_799]
        );
    }

    #[test]
    fn oversampling_toggle_mid_stream_is_finite() {
        let params = ParamSet::new(crate::BuiltinKind::Compressor);
        configure_multiband(&params);
        let mut dsp = Dsp::new(SR);
        for oversample in [0.0, 1.0, 0.0, 1.0] {
            params.set(OVERSAMPLE_PARAM, oversample);
            let mut left = vec![0.5; 2_048];
            let mut right = vec![0.5; 2_048];
            dsp.process(&mut left, &mut right, &params);
            assert!(
                left.iter()
                    .chain(right.iter())
                    .all(|sample| sample.is_finite())
            );
        }
    }

    #[test]
    fn oversampler_round_trip_is_unity_and_aligned() {
        let mut oversampler = Oversampler4x::new();
        let mut left = vec![0.25f32; 256];
        let mut right = vec![0.0f32; 256];
        {
            let (left4x, right4x) = oversampler.upsample(&left, &right);
            assert_eq!(left4x.len(), left.len() * OVERSAMPLE_FACTOR);
            assert_eq!(right4x.len(), right.len() * OVERSAMPLE_FACTOR);
        }
        oversampler.downsample_in_place(256, &mut left, &mut right);
        assert!((left[255] - 0.25).abs() < 0.005, "{}", left[255]);

        let mut oversampler = Oversampler4x::new();
        let mut left = vec![0.0f32; 64];
        let mut right = vec![0.0f32; 64];
        left[0] = 1.0;
        {
            let _ = oversampler.upsample(&left, &right);
        }
        oversampler.downsample_in_place(64, &mut left, &mut right);
        let (peak, index) = left.iter().enumerate().fold(
            (0.0f32, 0usize),
            |(peak, peak_index), (index, &sample)| {
                if sample.abs() > peak.abs() {
                    (sample, index)
                } else {
                    (peak, peak_index)
                }
            },
        );
        assert!(peak > 0.9, "{peak}");
        assert_eq!(index, 15, "round-trip delay");
    }
}
