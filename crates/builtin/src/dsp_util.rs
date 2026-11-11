//! Allocation-free DSP building blocks shared by the built-in effects.
//!
//! All helpers take/return linear gain unless the name says `db`, sample rates
//! are in Hz as `f64`, and times are in milliseconds unless stated otherwise.
//! None of these types allocate after construction.

use std::f64::consts::PI;

/// Converts dBFS to linear gain (`0 dB -> 1.0`).
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Converts linear gain to dBFS. Zero and negative gains return `-inf`.
#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    if gain > 0.0 && gain.is_finite() {
        20.0 * gain.log10()
    } else {
        f32::NEG_INFINITY
    }
}

/// Base-2 logarithm without the libm `log2`/`log10` call.
///
/// The mantissa is reduced with `t = (m - 1) / (m + 1)`; the atanh series
/// through `t^11` is accurate to about 1.5e-8 in `[1, 2)`, which is well below
/// f32 resolution. Used inside per-sample control loops where the exact
/// libm call is too expensive; parameter changes still use [`gain_to_db`].
#[inline]
pub fn fast_log2(x: f32) -> f32 {
    if x <= 0.0 || x.is_nan() {
        return f32::NEG_INFINITY;
    }
    if x == f32::INFINITY {
        return f32::INFINITY;
    }
    let mut bits = x.to_bits();
    let mut exponent = ((bits >> 23) & 0xff) as i32 - 127;
    if exponent == -127 {
        // Subnormal: scale into the normal range and account for the shift.
        let scaled = x * 8_388_608.0;
        if scaled == 0.0 {
            return f32::NEG_INFINITY;
        }
        bits = scaled.to_bits();
        exponent = (((bits >> 23) & 0xff) as i32 - 127) - 23;
    }
    let mantissa = f32::from_bits((bits & 0x007f_ffff) | 0x3f80_0000);
    let t = (mantissa - 1.0) / (mantissa + 1.0);
    let t2 = t * t;
    let poly =
        1.0 + t2 * (1.0 / 3.0 + t2 * (1.0 / 5.0 + t2 * (1.0 / 7.0 + t2 * (1.0 / 9.0 + t2 / 11.0))));
    exponent as f32 + 2.0 * std::f32::consts::LOG2_E * t * poly
}

/// Base-2 exponential without the libm `exp2`/`powf` call.
///
/// The fractional part goes through an 8th-order series and the integer part
/// is applied by exponent bits. Relative error is below 1e-7 for the gain
/// ranges used by the built-ins.
#[inline]
pub fn fast_exp2(x: f32) -> f32 {
    if x.is_nan() {
        return x;
    }
    if x > 127.0 {
        return f32::INFINITY;
    }
    if x < -126.0 {
        return 0.0;
    }
    let whole = x.floor();
    let fraction = x - whole;
    let z = fraction * std::f32::consts::LN_2;
    let mut poly = 1.0 / 40_320.0;
    poly = 1.0 / 5_040.0 + z * poly;
    poly = 1.0 / 720.0 + z * poly;
    poly = 1.0 / 120.0 + z * poly;
    poly = 1.0 / 24.0 + z * poly;
    poly = 1.0 / 6.0 + z * poly;
    poly = 0.5 + z * poly;
    poly = 1.0 + z * poly;
    poly = 1.0 + z * poly;
    let scale = f32::from_bits((((whole as i32) + 127) as u32) << 23);
    poly * scale
}

/// [`db_to_gain`] for per-sample control loops (no libm `powf`).
#[inline]
pub fn fast_db_to_gain(db: f32) -> f32 {
    if db.is_nan() {
        return f32::NAN;
    }
    fast_exp2(db / (20.0 * std::f32::consts::LOG10_2))
}

/// [`gain_to_db`] for per-sample control loops (no libm `log10`).
#[inline]
pub fn fast_gain_to_db(gain: f32) -> f32 {
    if gain.is_nan() {
        return f32::NAN;
    }
    (20.0 * std::f32::consts::LOG10_2) * fast_log2(gain)
}

/// Clamps `value` to `min..=max`. NaN clamps to `min`.
#[inline]
pub fn clamp(value: f32, min: f32, max: f32) -> f32 {
    value.max(min).min(max)
}

/// One-pole smoothing coefficient for a time constant.
///
/// `ms <= 0`, non-finite input or a non-positive sample rate returns `1.0`
/// (instant response).
pub fn smoothing_coefficient(ms: f32, sample_rate: f64) -> f32 {
    if !ms.is_finite() || ms <= 0.0 || !sample_rate.is_finite() || sample_rate <= 0.0 {
        return 1.0;
    }
    let samples = f64::from(ms) * 0.001 * sample_rate;
    (1.0 - (-1.0 / samples).exp()) as f32
}

/// One-pole lowpass smoother: `state += coefficient * (target - state)`.
#[derive(Clone, Copy, Debug)]
pub struct OnePole {
    coefficient: f32,
    state: f32,
}

impl OnePole {
    pub fn new(coefficient: f32) -> Self {
        Self {
            coefficient: if coefficient.is_finite() {
                coefficient.clamp(0.0, 1.0)
            } else {
                1.0
            },
            state: 0.0,
        }
    }

    /// Builds a smoother for a time constant in milliseconds.
    pub fn from_time(ms: f32, sample_rate: f64) -> Self {
        Self::new(smoothing_coefficient(ms, sample_rate))
    }

    pub fn set_coefficient(&mut self, coefficient: f32) {
        if coefficient.is_finite() {
            self.coefficient = coefficient.clamp(0.0, 1.0);
        }
    }

    pub fn coefficient(&self) -> f32 {
        self.coefficient
    }

    /// Current smoothed value.
    pub fn value(&self) -> f32 {
        self.state
    }

    /// Jumps the state to `value` without smoothing.
    pub fn set_value(&mut self, value: f32) {
        self.state = value;
    }

    pub fn reset(&mut self) {
        self.state = 0.0;
    }

    #[inline]
    pub fn process(&mut self, target: f32) -> f32 {
        self.state += self.coefficient * (target - self.state);
        self.state
    }

    /// Processes in place.
    pub fn process_slice(&mut self, samples: &mut [f32]) {
        for sample in samples {
            *sample = self.process(*sample);
        }
    }
}

/// DC blocker (one-pole highpass, default 10 Hz corner).
#[derive(Clone, Copy, Debug)]
pub struct DcBlocker {
    r: f32,
    x1: f32,
    y1: f32,
}

impl DcBlocker {
    pub fn new(sample_rate: f64) -> Self {
        Self::with_cutoff(sample_rate, 10.0)
    }

    pub fn with_cutoff(sample_rate: f64, cutoff_hz: f32) -> Self {
        let r = if sample_rate.is_finite() && sample_rate > 0.0 && cutoff_hz.is_finite() {
            let r = 1.0 - 2.0 * PI as f32 * cutoff_hz / sample_rate as f32;
            r.clamp(0.0, 0.9999)
        } else {
            0.9999
        };
        Self {
            r,
            x1: 0.0,
            y1: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }

    pub fn process_slice(&mut self, samples: &mut [f32]) {
        for sample in samples {
            *sample = self.process(*sample);
        }
    }
}

/// Absolute-value envelope follower with independent attack and release.
///
/// Times are milliseconds to reach ~63 % of a step (`1 - 1/e`).
#[derive(Clone, Copy, Debug)]
pub struct EnvelopeFollower {
    attack: f32,
    release: f32,
    envelope: f32,
}

impl EnvelopeFollower {
    pub fn new(attack_ms: f32, release_ms: f32, sample_rate: f64) -> Self {
        let mut follower = Self {
            attack: 1.0,
            release: 1.0,
            envelope: 0.0,
        };
        follower.set_times(attack_ms, release_ms, sample_rate);
        follower
    }

    pub fn set_times(&mut self, attack_ms: f32, release_ms: f32, sample_rate: f64) {
        self.attack = smoothing_coefficient(attack_ms, sample_rate);
        self.release = smoothing_coefficient(release_ms, sample_rate);
    }

    pub fn value(&self) -> f32 {
        self.envelope
    }

    /// Primes the follower without smoothing, for startup and resets.
    pub fn set_value(&mut self, value: f32) {
        self.envelope = if value.is_finite() { value.abs() } else { 0.0 };
    }

    pub fn reset(&mut self) {
        self.envelope = 0.0;
    }

    /// Feeds one sample and returns the updated absolute-value envelope.
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        let level = input.abs();
        let coefficient = if level > self.envelope {
            self.attack
        } else {
            self.release
        };
        self.envelope += coefficient * (level - self.envelope);
        self.envelope
    }
}

/// Second-order RBJ biquad in transposed direct form II.
///
/// Coefficients are normalized by `a0` inside every setter. Invalid frequency
/// or rate input leaves the previous coefficients untouched. `new()` is a
/// bypass (pass-through) filter.
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    pub fn new() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Sets normalized coefficients (`a0` normalization is applied here).
    pub fn set_coefficients(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        if !a0.is_finite() || a0 == 0.0 {
            return;
        }
        let inv = 1.0 / a0;
        self.b0 = b0 * inv;
        self.b1 = b1 * inv;
        self.b2 = b2 * inv;
        self.a1 = a1 * inv;
        self.a2 = a2 * inv;
    }

    /// RBJ lowpass.
    pub fn lowpass(&mut self, sample_rate: f64, freq_hz: f32, q: f32) {
        let Some((sin_w0, cos_w0)) = rbj_terms(sample_rate, freq_hz) else {
            return;
        };
        let alpha = sin_w0 / (2.0 * sanitize_q(q));
        let b0 = (1.0 - cos_w0) * 0.5;
        let b1 = 1.0 - cos_w0;
        let b2 = b0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        self.set_coefficients(
            b0 as f32, b1 as f32, b2 as f32, a0 as f32, a1 as f32, a2 as f32,
        );
    }

    /// RBJ highpass.
    pub fn highpass(&mut self, sample_rate: f64, freq_hz: f32, q: f32) {
        let Some((sin_w0, cos_w0)) = rbj_terms(sample_rate, freq_hz) else {
            return;
        };
        let alpha = sin_w0 / (2.0 * sanitize_q(q));
        let b0 = (1.0 + cos_w0) * 0.5;
        let b1 = -(1.0 + cos_w0);
        let b2 = b0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        self.set_coefficients(
            b0 as f32, b1 as f32, b2 as f32, a0 as f32, a1 as f32, a2 as f32,
        );
    }

    /// RBJ peaking EQ; `gain_db` is the boost/cut at `freq_hz`.
    pub fn peaking(&mut self, sample_rate: f64, freq_hz: f32, q: f32, gain_db: f32) {
        let Some((sin_w0, cos_w0)) = rbj_terms(sample_rate, freq_hz) else {
            return;
        };
        let amp = 10.0_f64.powf(f64::from(gain_db) / 40.0);
        let alpha = sin_w0 / (2.0 * sanitize_q(q));
        let b0 = 1.0 + alpha * amp;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * amp;
        let a0 = 1.0 + alpha / amp;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / amp;
        self.set_coefficients(
            b0 as f32, b1 as f32, b2 as f32, a0 as f32, a1 as f32, a2 as f32,
        );
    }

    /// RBJ low shelf; `slope` (S) is clamped to `0.1..=1.0`.
    pub fn low_shelf(&mut self, sample_rate: f64, freq_hz: f32, gain_db: f32, slope: f32) {
        let Some((sin_w0, cos_w0)) = rbj_terms(sample_rate, freq_hz) else {
            return;
        };
        let amp = 10.0_f64.powf(f64::from(gain_db) / 40.0);
        let s = f64::from(if slope.is_finite() {
            slope.clamp(0.1, 1.0)
        } else {
            1.0
        });
        let alpha = sin_w0 * 0.5 * ((amp + 1.0 / amp) * (1.0 / s - 1.0) + 2.0).sqrt();
        let sqrt_amp = amp.sqrt();
        let b0 = amp * ((amp + 1.0) - (amp - 1.0) * cos_w0 + 2.0 * sqrt_amp * alpha);
        let b1 = 2.0 * amp * ((amp - 1.0) - (amp + 1.0) * cos_w0);
        let b2 = amp * ((amp + 1.0) - (amp - 1.0) * cos_w0 - 2.0 * sqrt_amp * alpha);
        let a0 = (amp + 1.0) + (amp - 1.0) * cos_w0 + 2.0 * sqrt_amp * alpha;
        let a1 = -2.0 * ((amp - 1.0) + (amp + 1.0) * cos_w0);
        let a2 = (amp + 1.0) + (amp - 1.0) * cos_w0 - 2.0 * sqrt_amp * alpha;
        self.set_coefficients(
            b0 as f32, b1 as f32, b2 as f32, a0 as f32, a1 as f32, a2 as f32,
        );
    }

    /// RBJ high shelf; `slope` (S) is clamped to `0.1..=1.0`.
    pub fn high_shelf(&mut self, sample_rate: f64, freq_hz: f32, gain_db: f32, slope: f32) {
        let Some((sin_w0, cos_w0)) = rbj_terms(sample_rate, freq_hz) else {
            return;
        };
        let amp = 10.0_f64.powf(f64::from(gain_db) / 40.0);
        let s = f64::from(if slope.is_finite() {
            slope.clamp(0.1, 1.0)
        } else {
            1.0
        });
        let alpha = sin_w0 * 0.5 * ((amp + 1.0 / amp) * (1.0 / s - 1.0) + 2.0).sqrt();
        let sqrt_amp = amp.sqrt();
        let b0 = amp * ((amp + 1.0) + (amp - 1.0) * cos_w0 + 2.0 * sqrt_amp * alpha);
        let b1 = -2.0 * amp * ((amp - 1.0) + (amp + 1.0) * cos_w0);
        let b2 = amp * ((amp + 1.0) + (amp - 1.0) * cos_w0 - 2.0 * sqrt_amp * alpha);
        let a0 = (amp + 1.0) - (amp - 1.0) * cos_w0 + 2.0 * sqrt_amp * alpha;
        let a1 = 2.0 * ((amp - 1.0) - (amp + 1.0) * cos_w0);
        let a2 = (amp + 1.0) - (amp - 1.0) * cos_w0 - 2.0 * sqrt_amp * alpha;
        self.set_coefficients(
            b0 as f32, b1 as f32, b2 as f32, a0 as f32, a1 as f32, a2 as f32,
        );
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn process_slice(&mut self, samples: &mut [f32]) {
        for sample in samples {
            *sample = self.process(*sample);
        }
    }
}

impl Default for Biquad {
    fn default() -> Self {
        Self::new()
    }
}

fn sanitize_q(q: f32) -> f64 {
    f64::from(if q.is_finite() { q.max(1.0e-4) } else { 0.707 })
}

fn rbj_terms(sample_rate: f64, freq_hz: f32) -> Option<(f64, f64)> {
    if !sample_rate.is_finite() || sample_rate <= 0.0 || !freq_hz.is_finite() || freq_hz <= 0.0 {
        return None;
    }
    let nyquist = sample_rate * 0.5;
    let freq = f64::from(freq_hz).min(nyquist * 0.99);
    let w0 = 2.0 * PI * freq / sample_rate;
    Some(w0.sin_cos())
}

/// Fractionally interpolated read from a circular buffer.
///
/// `delay` follows [`DelayLine::read`] semantics (1.0 is the previous sample)
/// and is clamped to `1.0..=len - 2`. The integer part of the delay is wrapped
/// with `usize` arithmetic instead of an `f64` `rem_euclid`/`floor` per sample.
#[inline]
pub fn circular_read(buffer: &[f32], write: usize, delay: f32) -> f32 {
    let length = buffer.len();
    if length < 3 || write >= length {
        return 0.0;
    }
    let max = (length - 2) as f32;
    let delay = if delay.is_finite() {
        delay.clamp(1.0, max)
    } else {
        1.0
    };
    let whole = delay as usize;
    let fraction = delay - whole as f32;
    let (index, fraction) = if fraction > 0.0 {
        (wrap_index(write, whole + 1, length), 1.0 - fraction)
    } else {
        (wrap_index(write, whole, length), 0.0)
    };
    let next = if index + 1 == length { 0 } else { index + 1 };
    let current = buffer[index];
    current + (buffer[next] - current) * fraction
}

#[inline]
fn wrap_index(write: usize, amount: usize, length: usize) -> usize {
    let sum = write + length - amount;
    if sum >= length { sum - length } else { sum }
}

/// Preallocated circular delay line with linear-interpolated fractional reads.
#[derive(Clone, Debug)]
pub struct DelayLine {
    buffer: Vec<f32>,
    write: usize,
}

impl DelayLine {
    /// Allocates room for `max_seconds` at `sample_rate` (plus interpolation
    /// guard samples). This is the only allocation in the type.
    pub fn new(max_seconds: f32, sample_rate: f64) -> Self {
        let rate = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            48_000.0
        };
        let seconds = if max_seconds.is_finite() {
            max_seconds.max(0.001)
        } else {
            0.001
        };
        let samples = (f64::from(seconds) * rate).ceil() as usize + 2;
        Self {
            buffer: vec![0.0; samples.max(4)],
            write: 0,
        }
    }

    /// Largest readable delay in samples.
    pub fn max_delay_samples(&self) -> f32 {
        (self.buffer.len() - 2) as f32
    }

    /// Pushes one sample. Call once per frame per channel.
    pub fn push(&mut self, sample: f32) {
        self.buffer[self.write] = sample;
        self.write += 1;
        if self.write == self.buffer.len() {
            self.write = 0;
        }
    }

    /// Reads `delay_samples` back (1.0 = previous sample), interpolating
    /// linearly. Values are clamped to `1.0..=max_delay_samples`.
    pub fn read(&self, delay_samples: f32) -> f32 {
        circular_read(&self.buffer, self.write, delay_samples)
    }

    pub fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
    }
}

/// Cubic soft clipper with unity slope at 0 and a hard limit at +/-1.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    let x = x.clamp(-1.5, 1.5);
    x - (4.0 / 27.0) * x * x * x
}

#[cfg(test)]
mod tests {
    use super::{
        DelayLine, circular_read, db_to_gain, fast_db_to_gain, fast_gain_to_db, fast_log2,
    };

    /// Reference implementation kept in `f64` with `rem_euclid`/`floor`; the
    /// integer path must produce the same interpolation.
    fn reference_read(buffer: &[f32], write: usize, delay: f32) -> f32 {
        let length = buffer.len();
        let max = (length - 2) as f32;
        let delay = if delay.is_finite() {
            delay.clamp(1.0, max)
        } else {
            1.0
        };
        let position = (write as f64 - f64::from(delay)).rem_euclid(length as f64);
        let index = (position.floor() as usize).min(length - 1);
        let fraction = (position - index as f64) as f32;
        let next = if index + 1 == length { 0 } else { index + 1 };
        buffer[index] + (buffer[next] - buffer[index]) * fraction
    }

    #[test]
    fn fractional_reads_at_the_wrap_boundary_stay_in_bounds() {
        let mut line = DelayLine::new(0.1, 48_000.0);
        line.push(1.0);
        for delay in [1.0, 1.000_000_1, 1.000_01, line.max_delay_samples()] {
            let value = line.read(delay);
            assert!(value.is_finite());
        }
    }

    #[test]
    fn circular_read_matches_the_f64_reference() {
        let buffer: Vec<f32> = (0..97).map(|index| index as f32 * 0.25 - 3.0).collect();
        for write in [0usize, 1, 37, 96] {
            for delay in [1.0f32, 1.5, 2.0, 17.25, 42.75, 95.0, 500.0, f32::NAN] {
                let measured = circular_read(&buffer, write, delay);
                let expected = reference_read(&buffer, write, delay);
                assert!(
                    (measured - expected).abs() < 1.0e-6,
                    "write {write} delay {delay}: {measured} != {expected}"
                );
            }
        }
    }

    #[test]
    fn circular_read_handles_degenerate_buffers() {
        assert_eq!(circular_read(&[], 0, 1.0), 0.0);
        assert_eq!(circular_read(&[1.0], 0, 1.0), 0.0);
        assert!(circular_read(&[1.0, 2.0], 0, 1.0).is_finite());
    }

    #[test]
    fn fast_log_and_exp_are_accurate() {
        for db in [-80.0f32, -40.0, -6.0, -0.5, 0.0, 3.0, 12.0] {
            let exact = db_to_gain(db);
            let fast = fast_db_to_gain(db);
            assert!(
                (fast - exact).abs() <= exact.abs() * 2.0e-6 + 1.0e-12,
                "db {db}: {fast} != {exact}"
            );
            let back = fast_gain_to_db(exact);
            assert!((back - db).abs() < 1.0e-5, "db {db}: round trip {back}");
        }
        let round_trip = fast_log2(std::f32::consts::PI);
        assert!((round_trip - std::f32::consts::PI.log2()).abs() < 1.0e-6);
        assert_eq!(fast_gain_to_db(0.0), f32::NEG_INFINITY);
    }
}
