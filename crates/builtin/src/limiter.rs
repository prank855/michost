//! Lookahead limiter with true-peak-ish stereo linking.
//!
//! Both channels are delayed by 10 ms. A monotonic sliding maximum over that
//! lookahead window predicts the loudest sample about to emerge, so gain
//! reduction is already in place when the peak arrives (instant attack) and
//! only release is smoothed. Per-channel gain reduction is blended with the
//! linked stereo reduction using the `link` control. Wet output is bounded by
//! the ceiling by construction; a final clamp covers a wet/dry mix where the
//! dry signal is itself above the ceiling.

use std::collections::VecDeque;

use crate::compressor::{OVERSAMPLE_FACTOR, Oversampler4x};
use crate::dsp_util::{DelayLine, OnePole, db_to_gain, smoothing_coefficient};
use crate::{BuiltinDsp, ParamCurve, ParamSet, ParamSpec, ParamStyle};

pub static SPECS: &[ParamSpec] = &[
    ParamSpec {
        key: "input_db",
        name: "Input",
        unit: "dB",
        min: -12.0,
        max: 24.0,
        default: 0.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Limiter",
        visible_when: None,
    },
    ParamSpec {
        key: "ceiling_db",
        name: "Ceiling",
        unit: "dB",
        min: -12.0,
        max: 0.0,
        default: -1.0,
        curve: ParamCurve::Linear,
        decimals: 1,
        style: ParamStyle::Knob,
        section: "Limiter",
        visible_when: None,
    },
    ParamSpec {
        key: "release_ms",
        name: "Release",
        unit: "ms",
        min: 10.0,
        max: 500.0,
        default: 80.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Limiter",
        visible_when: None,
    },
    ParamSpec {
        key: "link",
        name: "Link",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 100.0,
        curve: ParamCurve::Linear,
        decimals: 0,
        style: ParamStyle::Knob,
        section: "Limiter",
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
const LOOKAHEAD_SECONDS: f64 = 0.010;

/// Index of the appended `oversample` toggle in [`SPECS`].
const OVERSAMPLE_PARAM: usize = 5;

#[inline]
fn ceiling_gain(params: &ParamSet) -> f32 {
    db_to_gain(params.get(1)).max(1.0e-6)
}

fn valid_rate(sample_rate: f64) -> f64 {
    if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        48_000.0
    }
}

/// Monotonic sliding maximum with amortized O(1) pushes.
///
/// Capacity is fixed in [`SlidingMax::new`]; pushes and pops never allocate.
struct SlidingMax {
    values: VecDeque<f32>,
    indices: VecDeque<u64>,
    position: u64,
    window: u64,
}

impl SlidingMax {
    fn new(window: usize) -> Self {
        let window = window.max(1);
        Self {
            values: VecDeque::with_capacity(window + 2),
            indices: VecDeque::with_capacity(window + 2),
            position: 0,
            window: window as u64,
        }
    }

    /// Pushes one sample and returns the maximum over the current window.
    #[inline]
    fn push(&mut self, value: f32) -> f32 {
        let value = if value.is_finite() { value } else { 0.0 };
        let index = self.position;
        self.position = self.position.wrapping_add(1);

        while let Some(&back) = self.values.back() {
            if back <= value {
                self.values.pop_back();
                self.indices.pop_back();
            } else {
                break;
            }
        }
        self.values.push_back(value);
        self.indices.push_back(index);

        while let Some(&front) = self.indices.front() {
            if index.wrapping_sub(front) >= self.window {
                self.values.pop_front();
                self.indices.pop_front();
            } else {
                break;
            }
        }

        self.values.front().copied().unwrap_or(0.0)
    }

    fn reset(&mut self) {
        self.values.clear();
        self.indices.clear();
        self.position = 0;
    }
}

struct Core {
    sample_rate: f64,
    delay_samples: f32,
    delay_l: DelayLine,
    delay_r: DelayLine,
    window_l: SlidingMax,
    window_r: SlidingMax,
    gain_link: f32,
    gain_l: f32,
    gain_r: f32,
    input_gain: OnePole,
    link: OnePole,
    mix: OnePole,
    primed: bool,
}

impl Core {
    fn new(sample_rate: f64) -> Self {
        let rate = valid_rate(sample_rate);
        let lookahead = ((rate * LOOKAHEAD_SECONDS).round().max(1.0)) as usize;
        let max_delay_seconds = ((lookahead as f64 + 2.0) / rate) as f32;
        Self {
            sample_rate: rate,
            delay_samples: (lookahead + 1) as f32,
            delay_l: DelayLine::new(max_delay_seconds, rate),
            delay_r: DelayLine::new(max_delay_seconds, rate),
            window_l: SlidingMax::new(lookahead + 1),
            window_r: SlidingMax::new(lookahead + 1),
            gain_link: 1.0,
            gain_l: 1.0,
            gain_r: 1.0,
            input_gain: OnePole::from_time(SMOOTHING_MS, rate),
            link: OnePole::from_time(SMOOTHING_MS, rate),
            mix: OnePole::from_time(SMOOTHING_MS, rate),
            primed: false,
        }
    }

    #[inline]
    fn attack(target: f32, current: f32, release: f32) -> f32 {
        if target < current {
            target
        } else {
            current + release * (target - current)
        }
    }
}

impl Core {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let input_db = params.get(0);
        let ceiling = ceiling_gain(params);
        let release_ms = params.get(2);
        let link_target = (params.get(3) * 0.01).clamp(0.0, 1.0);
        let mix_target = (params.get(4) * 0.01).clamp(0.0, 1.0);
        let release = smoothing_coefficient(release_ms, self.sample_rate);

        if !self.primed {
            self.input_gain.set_value(input_db);
            self.link.set_value(link_target);
            self.mix.set_value(mix_target);
            self.primed = true;
        }

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let input_gain = db_to_gain(self.input_gain.process(input_db));
            let input_l = *l * input_gain;
            let input_r = *r * input_gain;

            self.delay_l.push(input_l);
            self.delay_r.push(input_r);

            let peak_l = self.window_l.push(input_l.abs());
            let peak_r = self.window_r.push(input_r.abs());
            let peak_link = peak_l.max(peak_r);

            let target_l = if peak_l > ceiling {
                ceiling / peak_l
            } else {
                1.0
            };
            let target_r = if peak_r > ceiling {
                ceiling / peak_r
            } else {
                1.0
            };
            let target_link = if peak_link > ceiling {
                ceiling / peak_link
            } else {
                1.0
            };

            self.gain_l = Self::attack(target_l, self.gain_l, release);
            self.gain_r = Self::attack(target_r, self.gain_r, release);
            self.gain_link = Self::attack(target_link, self.gain_link, release);

            let link = self.link.process(link_target);
            let gain_l = self.gain_link * link + self.gain_l * (1.0 - link);
            let gain_r = self.gain_link * link + self.gain_r * (1.0 - link);

            let delayed_l = self.delay_l.read(self.delay_samples);
            let delayed_r = self.delay_r.read(self.delay_samples);
            let mix = self.mix.process(mix_target);

            let wet_l = delayed_l * gain_l;
            let wet_r = delayed_r * gain_r;
            let out_l = delayed_l + (wet_l - delayed_l) * mix;
            let out_r = delayed_r + (wet_r - delayed_r) * mix;

            *l = out_l.clamp(-ceiling, ceiling);
            *r = out_r.clamp(-ceiling, ceiling);
        }
    }

    fn reset(&mut self) {
        self.delay_l.clear();
        self.delay_r.clear();
        self.window_l.reset();
        self.window_r.reset();
        self.gain_link = 1.0;
        self.gain_l = 1.0;
        self.gain_r = 1.0;
        self.input_gain.reset();
        self.link.reset();
        self.mix.reset();
        self.primed = false;
    }
}

/// Limiter with an optional 4x oversampled processing core.
///
/// Both cores are preallocated; the host-rate core is used while `oversample`
/// is off (bit-identical to the original implementation) and the 4x core while
/// it is on. The 10 ms lookahead is kept in host time, i.e. four times as many
/// 4x samples. The resampler history is cleared on every toggle.
pub struct Dsp {
    host: Core,
    oversampled: Core,
    oversampler: Oversampler4x,
    oversample: bool,
}

impl Dsp {
    pub fn new(sample_rate: f64) -> Self {
        let rate = valid_rate(sample_rate);
        Self {
            host: Core::new(rate),
            oversampled: Core::new(rate * OVERSAMPLE_FACTOR as f64),
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

                // The 4x core bounds its own output, but the decimation FIR
                // can ring past the ceiling. Clamp once more at host rate so
                // the guarantee holds for the actual output samples.
                let ceiling = ceiling_gain(params);
                for (l, r) in left_block.iter_mut().zip(right_block.iter_mut()) {
                    *l = l.clamp(-ceiling, ceiling);
                    *r = r.clamp(-ceiling, ceiling);
                }
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

    fn expected_latency() -> usize {
        (SR * LOOKAHEAD_SECONDS).round() as usize
    }

    #[test]
    fn defaults_hold_peaks_below_ceiling() {
        let params = ParamSet::new(crate::BuiltinKind::Limiter);
        let ceiling = db_to_gain(params.get(1));
        let mut dsp = Dsp::new(SR);
        let mut left = vec![2.0; 4_800];
        let mut right = vec![2.0; 4_800];
        dsp.process(&mut left, &mut right, &params);
        let peak = left.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()));
        assert!(peak <= ceiling + 1e-5, "{peak} > {ceiling}");
        assert!((left[4_799] - ceiling).abs() < 1e-3, "{}", left[4_799]);
    }

    #[test]
    fn latency_matches_declared_lookahead() {
        let params = ParamSet::new(crate::BuiltinKind::Limiter);
        params.set(1, 0.0);
        let mut dsp = Dsp::new(SR);
        let latency = expected_latency();
        let mut left = vec![0.0; latency * 2 + 8];
        let mut right = vec![0.0; latency * 2 + 8];
        left[0] = 1.0;
        right[0] = 1.0;
        dsp.process(&mut left, &mut right, &params);
        assert!(left[..latency].iter().all(|sample| sample.abs() < 1e-6));
        assert!((left[latency] - 1.0).abs() < 1e-4, "{}", left[latency]);
    }

    #[test]
    fn link_control_blends_channel_reduction() {
        let params = ParamSet::new(crate::BuiltinKind::Limiter);
        params.set(1, -1.0);

        params.set(3, 0.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![2.0; 4_800];
        let mut right = vec![0.2; 4_800];
        dsp.process(&mut left, &mut right, &params);
        let unlinked_right = right[4_799];
        assert!((unlinked_right - 0.2).abs() < 1e-3, "{unlinked_right}");

        params.set(3, 100.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![2.0; 4_800];
        let mut right = vec![0.2; 4_800];
        dsp.process(&mut left, &mut right, &params);
        assert!(right[4_799] < 0.12, "{}", right[4_799]);
    }

    #[test]
    fn input_gain_drives_limiting() {
        let params = ParamSet::new(crate::BuiltinKind::Limiter);
        params.set(0, 6.0);
        params.set(1, -6.0);
        let ceiling = db_to_gain(-6.0);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![1.0; 4_800];
        let mut right = vec![1.0; 4_800];
        dsp.process(&mut left, &mut right, &params);
        assert!(left[4_799] <= ceiling + 1e-5, "{}", left[4_799]);
    }

    #[test]
    fn reset_clears_lookahead_state() {
        let params = ParamSet::new(crate::BuiltinKind::Limiter);
        let mut dsp = Dsp::new(SR);
        let mut left = vec![2.0; 1_024];
        let mut right = vec![2.0; 1_024];
        dsp.process(&mut left, &mut right, &params);
        dsp.reset();
        let mut left = vec![0.0; 64];
        let mut right = vec![0.0; 64];
        dsp.process(&mut left, &mut right, &params);
        assert!(left.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn oversample_toggle_defaults_off() {
        let params = ParamSet::new(crate::BuiltinKind::Limiter);
        assert_eq!(params.index_of("oversample"), Some(OVERSAMPLE_PARAM));
        assert_eq!(params.get(OVERSAMPLE_PARAM), 0.0);
        let spec = &params.specs()[OVERSAMPLE_PARAM];
        assert_eq!(spec.section, "Quality");
        assert!(matches!(spec.style, ParamStyle::Toggle));
    }

    #[test]
    fn oversampling_holds_ceiling_and_stays_finite() {
        let params = ParamSet::new(crate::BuiltinKind::Limiter);
        params.set(OVERSAMPLE_PARAM, 1.0);
        let ceiling = db_to_gain(params.get(1));
        let mut dsp = Dsp::new(SR);
        let mut left = vec![2.0; 9_600];
        let mut right = vec![2.0; 9_600];
        dsp.process(&mut left, &mut right, &params);
        assert!(
            left.iter()
                .chain(right.iter())
                .all(|sample| sample.is_finite())
        );
        let peak = left
            .iter()
            .chain(right.iter())
            .fold(0.0_f32, |acc, sample| acc.max(sample.abs()));
        assert!(peak <= ceiling + 1e-5, "{peak} > {ceiling}");
        assert!(left[9_599] > ceiling * 0.95, "{}", left[9_599]);
    }

    #[test]
    fn oversampling_toggle_mid_stream_is_finite() {
        let params = ParamSet::new(crate::BuiltinKind::Limiter);
        let mut dsp = Dsp::new(SR);
        for oversample in [0.0, 1.0, 0.0, 1.0] {
            params.set(OVERSAMPLE_PARAM, oversample);
            let mut left = vec![1.5; 2_048];
            let mut right = vec![1.5; 2_048];
            dsp.process(&mut left, &mut right, &params);
            assert!(
                left.iter()
                    .chain(right.iter())
                    .all(|sample| sample.is_finite())
            );
        }
    }
}
