//! Real-time voice-oriented spectral noise suppressor.
//!
//! A causal STFT with `N = 256`, `H = 128`, sqrt-Hann analysis and synthesis
//! windows, and 50 % overlap-add. A stereo power average drives one per-bin
//! SPP-MMSE noise PSD and one real gain curve that is applied to both channels.
//! Reading the overlap-add ring with a further 256 sample delay makes the first
//! output for an impulse at sample 0 land exactly at sample 256.
//!
//! The gain follows an OM-LSA rule: decision-directed a-priori SNR, an MMSE-LSA
//! gain with a speech-presence-controlled noise-only floor, log-domain mixing,
//! frequency smoothing, temporal attack/release, and a median filter that is
//! only active in noise-only frames. Consonants and word endings are protected
//! by an onset detector and a short speech hold.
//!
//! Everything is allocated in [`Spectral::new`]; [`Spectral::process`] only
//! touches fixed arrays and never allocates, locks, logs or blocks.

use std::f64::consts::PI;

use crate::ParamSet;
use crate::dsp_util::smoothing_coefficient;

const FFT_SIZE: usize = 256;
const HOP: usize = FFT_SIZE / 2;
const BINS: usize = FFT_SIZE / 2 + 1;
const OUT_LEN: usize = 1024;
const OUT_MASK: usize = OUT_LEN - 1;
const INPUT_MASK: usize = FFT_SIZE - 1;
const EPS: f32 = 1.0e-12;

/// Hops used to establish the initial noise PSD from the input power.
const WARMUP_HOPS: u32 = 10;
/// Speech-presence probability constants: `q = 0.5`, `xi_H = 15 dB`.
const SPP_Q: f32 = 0.5;
const SPP_XI: f32 = 31.6;
/// Unbiased noise-tracker rate: `lambda += (1 - mu) * (1 - p) * (P - lambda)`.
const NOISE_MU: f32 = 0.85;
/// Noise PSD growth limit per hop (3.5 dB) to stop babble creep.
const NOISE_MAX_RISE: f32 = 1.5;
/// `10^(-18/10)`, the a-priori SNR floor.
const XI_MIN: f32 = 0.015_848_932;
/// Latched onset release.
const ONSET_HOPS: u32 = 3;

/// Iterative radix-2 FFT with precomputed bit-reversal and twiddle tables.
struct Fft {
    reverse: [u8; FFT_SIZE],
    cos: [f32; FFT_SIZE / 2],
    sin: [f32; FFT_SIZE / 2],
}

impl Fft {
    fn new() -> Self {
        let bits = FFT_SIZE.trailing_zeros() as usize;
        let mut reverse = [0_u8; FFT_SIZE];
        for (index, slot) in reverse.iter_mut().enumerate() {
            let mut value = index;
            let mut reversed = 0_usize;
            for _ in 0..bits {
                reversed = (reversed << 1) | (value & 1);
                value >>= 1;
            }
            *slot = reversed as u8;
        }

        let mut cos = [0.0_f32; FFT_SIZE / 2];
        let mut sin = [0.0_f32; FFT_SIZE / 2];
        for index in 0..FFT_SIZE / 2 {
            let angle = 2.0 * PI * index as f64 / FFT_SIZE as f64;
            cos[index] = angle.cos() as f32;
            sin[index] = angle.sin() as f32;
        }

        Self { reverse, cos, sin }
    }

    fn run(&self, re: &mut [f32; FFT_SIZE], im: &mut [f32; FFT_SIZE], inverse: bool) {
        for index in 0..FFT_SIZE {
            let target = self.reverse[index] as usize;
            if target > index {
                re.swap(index, target);
                im.swap(index, target);
            }
        }

        let mut size = 2;
        while size <= FFT_SIZE {
            let half = size / 2;
            let step = FFT_SIZE / size;
            for start in (0..FFT_SIZE).step_by(size) {
                for offset in 0..half {
                    let table = offset * step;
                    let (wr, wi) = if inverse {
                        (self.cos[table], self.sin[table])
                    } else {
                        (self.cos[table], -self.sin[table])
                    };
                    let a = start + offset;
                    let b = a + half;
                    let br = re[b];
                    let bi = im[b];
                    let tr = br * wr - bi * wi;
                    let ti = br * wi + bi * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            size *= 2;
        }

        if inverse {
            let scale = 1.0 / FFT_SIZE as f32;
            for value in re.iter_mut() {
                *value *= scale;
            }
            for value in im.iter_mut() {
                *value *= scale;
            }
        }
    }
}

/// Coefficients derived from the parameters once per [`Spectral::process`].
struct Settings {
    bypass: bool,
    amount: f32,
    voice: f32,
    alpha_os: f32,
    reduction_db: f32,
    alpha_dd: f32,
    attack: f32,
    release: f32,
    fast: f32,
    slow: f32,
}

pub(crate) struct Spectral {
    sample_rate: f64,
    frames_per_second: f64,
    fft: Fft,
    window: [f32; FFT_SIZE],

    history_left: [f32; FFT_SIZE],
    history_right: [f32; FFT_SIZE],
    history_count: usize,

    output_left: [f32; OUT_LEN],
    output_right: [f32; OUT_LEN],
    output_position: usize,

    re_left: [f32; FFT_SIZE],
    im_left: [f32; FFT_SIZE],
    re_right: [f32; FFT_SIZE],
    im_right: [f32; FFT_SIZE],

    power_bins: [f32; BINS],
    noise_psd: [f32; BINS],
    speech_prob_state: [f32; BINS],
    speech_prob_bins: [f32; BINS],
    hops: u32,
    speech_hold: u32,
    onset_hold: u32,
    activity: f32,

    fast_energy: f32,
    slow_energy: f32,

    prev_lsa_gain: [f32; BINS],
    prev_gamma: [f32; BINS],
    log_spectrum: [f32; BINS],
    smoothed_log: [f32; BINS],
    smoothed_gain: [f32; BINS],
    applied_gain: [f32; BINS],
    median_history1: [f32; BINS],
    median_history2: [f32; BINS],
}

impl Spectral {
    pub fn new(sample_rate: f64) -> Self {
        let sample_rate = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            48_000.0
        };

        let mut window = [0.0_f32; FFT_SIZE];
        for (index, slot) in window.iter_mut().enumerate() {
            let hann = 0.5 - 0.5 * (2.0 * PI * index as f64 / FFT_SIZE as f64).cos();
            *slot = hann.max(0.0).sqrt() as f32;
        }

        Self {
            sample_rate,
            frames_per_second: sample_rate / HOP as f64,
            fft: Fft::new(),
            window,
            history_left: [0.0; FFT_SIZE],
            history_right: [0.0; FFT_SIZE],
            history_count: 0,
            output_left: [0.0; OUT_LEN],
            output_right: [0.0; OUT_LEN],
            output_position: 0,
            re_left: [0.0; FFT_SIZE],
            im_left: [0.0; FFT_SIZE],
            re_right: [0.0; FFT_SIZE],
            im_right: [0.0; FFT_SIZE],
            power_bins: [0.0; BINS],
            noise_psd: [0.0; BINS],
            speech_prob_state: [0.0; BINS],
            speech_prob_bins: [0.0; BINS],
            hops: 0,
            speech_hold: 0,
            onset_hold: 0,
            activity: 0.0,
            fast_energy: 0.0,
            slow_energy: 0.0,
            prev_lsa_gain: [1.0; BINS],
            prev_gamma: [0.0; BINS],
            log_spectrum: [0.0; BINS],
            smoothed_log: [0.0; BINS],
            smoothed_gain: [1.0; BINS],
            applied_gain: [1.0; BINS],
            median_history1: [1.0; BINS],
            median_history2: [1.0; BINS],
        }
    }

    pub fn reset(&mut self) {
        self.history_left.fill(0.0);
        self.history_right.fill(0.0);
        self.history_count = 0;
        self.output_left.fill(0.0);
        self.output_right.fill(0.0);
        self.output_position = 0;
        self.re_left.fill(0.0);
        self.im_left.fill(0.0);
        self.re_right.fill(0.0);
        self.im_right.fill(0.0);
        self.power_bins.fill(0.0);
        self.noise_psd.fill(0.0);
        self.speech_prob_state.fill(0.0);
        self.speech_prob_bins.fill(0.0);
        self.hops = 0;
        self.speech_hold = 0;
        self.onset_hold = 0;
        self.activity = 0.0;
        self.fast_energy = 0.0;
        self.slow_energy = 0.0;
        self.prev_lsa_gain.fill(1.0);
        self.prev_gamma.fill(0.0);
        self.log_spectrum.fill(0.0);
        self.smoothed_log.fill(0.0);
        self.smoothed_gain.fill(1.0);
        self.applied_gain.fill(1.0);
        self.median_history1.fill(1.0);
        self.median_history2.fill(1.0);
    }

    /// Smoothed speech activity (0..1) for the optional post-mix vocal gate.
    pub fn voice_activity(&self) -> f32 {
        self.activity
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet) {
        let frames = left.len().min(right.len());
        if frames == 0 {
            return;
        }

        let amount = read(params, "strength", 50.0).clamp(0.0, 100.0) / 100.0;
        let reduction_db = read(params, "reduction_db", 20.0).clamp(0.0, 60.0);
        let smoothing = read(params, "smoothing", 50.0).clamp(0.0, 100.0) / 100.0;
        let voice = read(params, "voice", 60.0).clamp(0.0, 100.0) / 100.0;

        // Rate-aware decision-directed smoothing.
        let tau_dd_ms = f64::from(40.0 + 60.0 * smoothing) * 0.001;
        let hop_seconds = HOP as f64 / self.sample_rate;
        let alpha_dd = (-hop_seconds / tau_dd_ms).exp() as f32;

        let attack_ms = 3.0;
        let release_ms = 30.0 + 250.0 * smoothing;

        let settings = Settings {
            bypass: amount <= 1.0e-6 || reduction_db <= 1.0e-6,
            amount,
            voice,
            // Fixed over-subtraction; Strength is the continuous amount of
            // attenuation, not an on/off threshold in the exponent.
            alpha_os: 4.0,
            reduction_db,
            alpha_dd,
            attack: smoothing_coefficient(attack_ms, self.frames_per_second),
            release: smoothing_coefficient(release_ms, self.frames_per_second),
            fast: smoothing_coefficient(4.0, self.frames_per_second),
            slow: smoothing_coefficient(90.0, self.frames_per_second),
        };

        for index in 0..frames {
            let sample_left = sanitize(left[index]);
            let sample_right = sanitize(right[index]);

            let slot = self.history_count & INPUT_MASK;
            self.history_left[slot] = sample_left;
            self.history_right[slot] = sample_right;
            self.history_count = self.history_count.wrapping_add(1);

            if self.history_count & (HOP - 1) == 0 {
                self.process_hop(&settings);
            }

            let output_slot = self.output_position & OUT_MASK;
            let wet_left = self.output_left[output_slot];
            let wet_right = self.output_right[output_slot];
            self.output_left[output_slot] = 0.0;
            self.output_right[output_slot] = 0.0;
            left[index] = sanitize(wet_left);
            right[index] = sanitize(wet_right);
            self.output_position = self.output_position.wrapping_add(1);
        }
    }

    fn process_hop(&mut self, settings: &Settings) {
        self.build_frame();
        self.fft.run(&mut self.re_left, &mut self.im_left, false);
        self.fft.run(&mut self.re_right, &mut self.im_right, false);

        let warming = self.hops < WARMUP_HOPS;
        self.hops = self.hops.saturating_add(1);

        let mut total_power = 0.0_f32;
        let mut log_power = 0.0_f32;
        for bin in 0..BINS {
            let left_re = self.re_left[bin];
            let left_im = self.im_left[bin];
            let right_re = self.re_right[bin];
            let right_im = self.im_right[bin];
            let left_power = left_re * left_re + left_im * left_im;
            let right_power = right_re * right_re + right_im * right_im;
            // Average the channel powers. Averaging the complex signals first
            // would cancel out-of-phase stereo content.
            let power = 0.5 * (left_power + right_power);
            let power = if power.is_finite() {
                power.max(0.0)
            } else {
                0.0
            };
            self.power_bins[bin] = power;
            total_power += power;
            log_power += (power + EPS).ln();
        }
        if !total_power.is_finite() {
            total_power = 0.0;
        }
        let mean_power = total_power / BINS as f32;
        let flatness = (log_power / BINS as f32).exp() / (mean_power + EPS);
        // A peaky frame is voice/tonal even if an immediately preceding voice
        // already raised the slow noise estimate. This keeps voice protection
        // working when speech starts at the very first sample.
        let speech_like = !warming && flatness < 0.45 && total_power > 1.0e-9;

        // Speech-presence probability, temporally smoothed per bin and then
        // smoothed across three bins.
        for bin in 0..BINS {
            let power = self.power_bins[bin];
            let lambda = self.noise_psd[bin].max(EPS);
            let gamma = power / lambda;
            let exponent = -(gamma * SPP_XI) / (1.0 + SPP_XI);
            let denominator = 1.0 + (SPP_Q / (1.0 - SPP_Q)) * (1.0 + SPP_XI) * exponent.exp();
            let instant = if denominator.is_finite() {
                (1.0 / denominator).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let state = &mut self.speech_prob_state[bin];
            *state = 0.7 * *state + 0.3 * instant;
            if !state.is_finite() {
                *state = instant;
            }
        }
        for bin in 0..BINS {
            let previous = self.speech_prob_state[bin.saturating_sub(1)];
            let next = self.speech_prob_state[(bin + 1).min(BINS - 1)];
            let center = self.speech_prob_state[bin];
            self.speech_prob_bins[bin] = 0.25 * previous + 0.5 * center + 0.25 * next;
        }

        let speech_mean = self.speech_prob_bins.iter().sum::<f32>() / BINS as f32;

        // Fast/slow broadband envelope for onsets (4 ms / 90 ms).
        self.fast_energy += settings.fast * (total_power - self.fast_energy);
        self.slow_energy += settings.slow * (total_power - self.slow_energy);
        if !self.fast_energy.is_finite() {
            self.fast_energy = 0.0;
        }
        if !self.slow_energy.is_finite() {
            self.slow_energy = 0.0;
        }
        let onset = !warming && self.slow_energy > EPS && self.fast_energy > 1.6 * self.slow_energy;

        let voiced = speech_mean > 0.30;
        let hold_frames = ((0.05 * self.sample_rate / HOP as f64).ceil() as u32).max(1);

        if onset {
            self.onset_hold = ONSET_HOPS;
        } else {
            self.onset_hold = self.onset_hold.saturating_sub(1);
        }
        if voiced || onset {
            self.speech_hold = hold_frames;
        } else {
            self.speech_hold = self.speech_hold.saturating_sub(1);
        }

        let speech_active =
            !warming && (voiced || speech_like || self.speech_hold > 0 || self.onset_hold > 0);
        let activity_target = if speech_active || speech_like {
            1.0
        } else {
            0.0
        };
        let activity_coefficient = if activity_target > self.activity {
            0.45
        } else {
            0.10
        };
        self.activity += activity_coefficient * (activity_target - self.activity);
        self.activity = self.activity.clamp(0.0, 1.0);

        // Unbiased SPP-MMSE noise PSD update. Bins with a high speech
        // probability freeze the estimate; the first hops bootstrap it from
        // the observed power.
        for bin in 0..BINS {
            let power = self.power_bins[bin];
            if warming {
                let lambda = &mut self.noise_psd[bin];
                *lambda += 0.5 * (power - *lambda);
                if !lambda.is_finite() || *lambda < 0.0 {
                    *lambda = 0.0;
                }
            } else if !speech_active {
                let probability = self.speech_prob_bins[bin];
                let lambda = self.noise_psd[bin].max(EPS);
                let mut next = lambda + (1.0 - NOISE_MU) * (1.0 - probability) * (power - lambda);
                let ceiling = lambda * NOISE_MAX_RISE + EPS;
                if next > ceiling {
                    next = ceiling;
                }
                if !next.is_finite() || next < 0.0 {
                    next = 0.0;
                }
                self.noise_psd[bin] = next;
            }
        }

        // Decision-directed a-priori SNR and MMSE-LSA gain.
        let (alpha_os, reduction_db, alpha_dd) =
            (settings.alpha_os, settings.reduction_db, settings.alpha_dd);
        for bin in 0..BINS {
            let power = self.power_bins[bin];
            let lambda = self.noise_psd[bin].max(EPS);
            let gamma = power / (alpha_os * lambda);
            let previous_gain = self.prev_lsa_gain[bin];
            let previous_gamma = self.prev_gamma[bin];
            let instant = (gamma - 1.0).max(0.0);
            let xi = (alpha_dd * previous_gain * previous_gain * previous_gamma
                + (1.0 - alpha_dd) * instant)
                .max(XI_MIN);
            self.prev_gamma[bin] = gamma;

            let v = (xi * gamma / (1.0 + xi)).max(1.0e-8);
            let g_lsa = (xi / (1.0 + xi) * (0.5 * e1_approx(v)).exp()).clamp(0.0, 1.0);
            self.prev_lsa_gain[bin] = g_lsa;

            let frequency = bin as f32 * self.sample_rate as f32 / FFT_SIZE as f32;
            let weight = floor_weight(frequency, self.sample_rate as f32 * 0.5);
            let floor = 10.0_f32.powf(-reduction_db * weight / 20.0).clamp(EPS, 1.0);

            // OM-LSA log-domain mix. During a speech hold, keep some LSA
            // weight so consonants survive the noise-only floor.
            let probability = if speech_active {
                self.speech_prob_bins[bin].max(0.5)
            } else {
                self.speech_prob_bins[bin]
            };
            let log_gain =
                probability * g_lsa.max(1.0e-8).ln() + (1.0 - probability) * floor.max(1.0e-8).ln();
            self.log_spectrum[bin] = if log_gain.is_finite() { log_gain } else { 0.0 };
        }

        // Frequency smoothing in the log domain: +/-1 bin below 1 kHz,
        // +/-2 bins above 2 kHz.
        for bin in 0..BINS {
            let frequency = bin as f32 * self.sample_rate as f32 / FFT_SIZE as f32;
            let radius = if frequency > 2_000.0 { 2 } else { 1 };
            let low = bin.saturating_sub(radius);
            let high = (bin + radius).min(BINS - 1);
            let mut sum = 0.0;
            let mut count = 0.0;
            for index in low..=high {
                sum += self.log_spectrum[index];
                count += 1.0;
            }
            self.smoothed_log[bin] = sum / count;
        }

        // Temporal attack/release in linear gain, then the median filter in
        // noise-only frames.
        let noise_only = !speech_active && speech_mean < 0.30;
        let fast_release = self.onset_hold > 0;
        for bin in 0..BINS {
            let frequency = bin as f32 * self.sample_rate as f32 / FFT_SIZE as f32;
            let weight = floor_weight(frequency, self.sample_rate as f32 * 0.5);
            let floor = 10.0_f32
                .powf(-settings.reduction_db * weight / 20.0)
                .clamp(EPS, 1.0);
            // Strength scales the attenuation continuously in the gain
            // domain (0 = bypass, 1 = full target). This keeps 1-99% useful
            // instead of snapping between "off" and "on".
            let mut target = self.smoothed_log[bin]
                .exp()
                .powf(settings.amount)
                .clamp(floor, 1.0);
            // Keep a speech floor in the voice band even when the OM-LSA
            // probability is high. This is what prevents a 20-60 dB reduction
            // from hollowing out formants; the Voice knob controls how much.
            if settings.voice > 0.001 {
                let frequency = bin as f32 * self.sample_rate as f32 / FFT_SIZE as f32;
                let weight = voice_floor_weight(frequency);
                let probability = if speech_active {
                    self.speech_prob_bins[bin].max(0.5)
                } else {
                    self.speech_prob_bins[bin]
                };
                // Only preserve when speech is actually likely; a low SPP bin
                // is noise and must receive the full reduction. The 0.25-1.0
                // ramp avoids a hard speech/noise switch.
                let speech_gate = ((probability - 0.25) / 0.75).clamp(0.0, 1.0);
                let frame_gate = if speech_like { 0.7 } else { 0.0 };
                let speech_gate = speech_gate.max(frame_gate);
                // Voice does not just set a floor below the computed gain; it
                // pulls the voice-band gain toward unity. This is the audible
                // "preserve detail" control at high Reduction values.
                let preserve = (settings.voice * weight * speech_gate).clamp(0.0, 0.85);
                target += (1.0 - target) * preserve;
                target = target.clamp(floor, 1.0);
            }
            let current = &mut self.smoothed_gain[bin];
            let coefficient = if fast_release || target > *current {
                settings.attack
            } else {
                settings.release
            };
            *current += coefficient * (target - *current);
            if !current.is_finite() {
                *current = floor;
            }
            let candidate = current.clamp(floor, 1.0);

            let mut output = candidate;
            if noise_only {
                let mut values = [
                    candidate,
                    self.median_history1[bin].clamp(floor, 1.0),
                    self.median_history2[bin].clamp(floor, 1.0),
                ];
                values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                output = values[1];
            }
            self.median_history2[bin] = self.median_history1[bin];
            self.median_history1[bin] = candidate;
            self.applied_gain[bin] = if output.is_finite() {
                output.clamp(floor, 1.0)
            } else {
                floor
            };
        }

        if settings.bypass {
            self.applied_gain.fill(1.0);
        }
        self.finish_hop();
    }

    fn finish_hop(&mut self) {
        self.apply_gains();
        self.fft.run(&mut self.re_left, &mut self.im_left, true);
        self.fft.run(&mut self.re_right, &mut self.im_right, true);
        self.overlap_add();
    }

    fn build_frame(&mut self) {
        for index in 0..FFT_SIZE {
            let slot = self.history_count.wrapping_add(index) & INPUT_MASK;
            let window = self.window[index];
            self.re_left[index] = self.history_left[slot] * window;
            self.im_left[index] = 0.0;
            self.re_right[index] = self.history_right[slot] * window;
            self.im_right[index] = 0.0;
        }
    }

    fn apply_gains(&mut self) {
        for bin in 0..BINS {
            let gain = self.applied_gain[bin];
            self.re_left[bin] *= gain;
            self.im_left[bin] *= gain;
            self.re_right[bin] *= gain;
            self.im_right[bin] *= gain;

            if bin > 0 && bin < FFT_SIZE / 2 {
                let mirror = FFT_SIZE - bin;
                self.re_left[mirror] *= gain;
                self.im_left[mirror] *= gain;
                self.re_right[mirror] *= gain;
                self.im_right[mirror] *= gain;
            }
        }
    }

    fn overlap_add(&mut self) {
        for index in 0..FFT_SIZE {
            // The first frame starts one hop before the stream. Its negative
            // timeline would become output before sample 0, so skip it and
            // keep the first 256 output samples exactly silent.
            if self.history_count.wrapping_add(index) < FFT_SIZE {
                continue;
            }
            let slot = self.history_count.wrapping_add(index) & OUT_MASK;
            let window = self.window[index];
            self.output_left[slot] += self.re_left[index] * window;
            self.output_right[slot] += self.re_right[index] * window;
        }
    }
}

/// Speech-preservation floor weight. It is highest in the voice core,
/// extends into the consonant region and fades out below rumble and above
/// sibilance where full reduction is safe.
fn voice_floor_weight(frequency: f32) -> f32 {
    if !frequency.is_finite() || frequency <= 120.0 {
        0.0
    } else if frequency < 250.0 {
        0.40 * (frequency - 120.0) / 130.0
    } else if frequency < 4_000.0 {
        0.40
    } else if frequency < 8_000.0 {
        0.40 + 0.15 * (frequency - 4_000.0) / 4_000.0
    } else if frequency < 10_000.0 {
        0.55 - 0.55 * (frequency - 8_000.0) / 2_000.0
    } else {
        0.0
    }
}

/// Frequency-shaped noise-only floor weight.
///
/// `1` below 120 Hz, ramping to `0.55` by 250 Hz, `0.55` to 4 kHz, ramping to
/// `0.85` by 8 kHz and to `1` by 10 kHz.
fn floor_weight(frequency: f32, nyquist: f32) -> f32 {
    if !frequency.is_finite() {
        return 1.0;
    }
    let frequency = frequency.clamp(0.0, nyquist.max(0.0));
    if frequency < 120.0 {
        1.0
    } else if frequency < 250.0 {
        1.0 - 0.45 * (frequency - 120.0) / 130.0
    } else if frequency < 4_000.0 {
        0.55
    } else if frequency < 8_000.0 {
        0.55 + 0.30 * (frequency - 4_000.0) / 4_000.0
    } else if frequency < 10_000.0 {
        0.85 + 0.15 * (frequency - 8_000.0) / 2_000.0
    } else {
        1.0
    }
}

/// Exponential integral `E1(x)` using Abramowitz & Stegun 5.1.53 / 5.1.56.
fn e1_approx(x: f32) -> f32 {
    if !x.is_finite() || x <= 0.0 {
        return 0.0;
    }
    if x > 10.0 {
        return 0.0;
    }
    if x <= 1.0 {
        let poly = 0.999_991_93
            + x * (-0.249_910_55 + x * (0.055_199_68 + x * (-0.009_760_04 + x * 0.001_078_57)));
        -x.ln() - 0.577_215_7 + x * poly
    } else {
        let numerator = x * x + 2.334_733 * x + 0.250_621;
        let denominator = x * x + 3.330_657 * x + 1.681_534;
        (-x).exp() / x * numerator / denominator
    }
}

fn read(params: &ParamSet, key: &str, default: f32) -> f32 {
    let value = params.get_key(key);
    if value.is_finite() { value } else { default }
}

fn sanitize(sample: f32) -> f32 {
    if sample.is_finite() { sample } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::BuiltinKind;

    const RATE: f32 = 16_000.0;

    fn params(strength: f32, reduction_db: f32, smoothing: f32) -> Arc<ParamSet> {
        let params = ParamSet::new(BuiltinKind::NoiseRemoval);
        for (key, value) in [
            ("strength", strength),
            ("reduction_db", reduction_db),
            ("smoothing", smoothing),
        ] {
            let index = params
                .index_of(key)
                .unwrap_or_else(|| panic!("missing noise-removal parameter {key}"));
            params.set(index, value);
        }
        params
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn noise(len: usize, amplitude: f32) -> Vec<f32> {
        let mut state = 0x1234_5678_u32;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let unit = (state >> 8) as f32 / (1_u32 << 24) as f32;
                (unit * 2.0 - 1.0) * amplitude
            })
            .collect()
    }

    fn tone(len: usize, frequency: f32, amplitude: f32) -> Vec<f32> {
        (0..len)
            .map(|index| {
                amplitude * (2.0 * std::f32::consts::PI * frequency * index as f32 / RATE).sin()
            })
            .collect()
    }

    /// Synthetic vowel: F0 120 Hz through formants at 700/1220/2600 Hz.
    fn vowel(len: usize) -> Vec<f32> {
        let formants = [
            (700.0_f32, 130.0_f32, 1.0_f32),
            (1_220.0, 160.0, 0.7),
            (2_600.0, 220.0, 0.45),
        ];
        let mut output = vec![0.0_f32; len];
        let mut harmonic = 1usize;
        while (harmonic as f32) * 120.0 < RATE * 0.5 {
            let frequency = harmonic as f32 * 120.0;
            let mut amplitude = 0.0_f32;
            for (center, bandwidth, gain) in formants {
                let offset = (frequency - center) / bandwidth;
                amplitude += gain / (1.0 + offset * offset);
            }
            amplitude = amplitude.max(0.01) / (harmonic as f32).sqrt();
            for (index, sample) in output.iter_mut().enumerate() {
                *sample += amplitude
                    * (2.0 * std::f32::consts::PI * frequency * index as f32 / RATE).sin();
            }
            harmonic += 1;
        }
        let current = rms(&output);
        if current > 0.0 {
            let scale = 0.1 / current;
            for sample in &mut output {
                *sample *= scale;
            }
        }
        output
    }

    /// Mean short-time power in `low..=high` Hz, using 256-point frames.
    fn band_energy(samples: &[f32], low: f32, high: f32) -> f32 {
        let fft = Fft::new();
        let mut re = [0.0_f32; FFT_SIZE];
        let mut im = [0.0_f32; FFT_SIZE];
        let mut total = 0.0_f32;
        let mut frames = 0.0_f32;
        for chunk in samples.chunks_exact(FFT_SIZE) {
            for index in 0..FFT_SIZE {
                let hann = 0.5 - 0.5 * (2.0 * PI * index as f64 / FFT_SIZE as f64).cos();
                re[index] = chunk[index] * hann as f32;
                im[index] = 0.0;
            }
            fft.run(&mut re, &mut im, false);
            for bin in 0..BINS {
                let frequency = bin as f32 * RATE / FFT_SIZE as f32;
                if frequency >= low && frequency <= high {
                    total += re[bin] * re[bin] + im[bin] * im[bin];
                }
            }
            frames += 1.0;
        }
        (total / frames.max(1.0)).sqrt()
    }

    fn db(ratio: f32) -> f32 {
        20.0 * ratio.max(1.0e-12).log10()
    }

    fn process_blocks(
        dsp: &mut Spectral,
        params: &ParamSet,
        input: &[f32],
        block: usize,
    ) -> Vec<f32> {
        let mut left = input.to_vec();
        let mut right = input.to_vec();
        let mut offset = 0;
        while offset < input.len() {
            let len = (input.len() - offset).min(block);
            dsp.process(
                &mut left[offset..offset + len],
                &mut right[offset..offset + len],
                params,
            );
            offset += len;
        }
        left
    }

    #[test]
    fn silence_stays_silent() {
        let params = ParamSet::new(BuiltinKind::NoiseRemoval);
        let mut dsp = Spectral::new(f64::from(RATE));
        let mut left = vec![0.0_f32; 512];
        let mut right = vec![0.0_f32; 512];
        for _ in 0..16 {
            dsp.process(&mut left, &mut right, &params);
        }
        assert!(left.iter().all(|sample| *sample == 0.0));
        assert!(right.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn steady_noise_and_tone_are_attenuated() {
        let params = params(100.0, 30.0, 20.0);
        let len = 4 * RATE as usize;
        let steady_noise = noise(len, 0.05);
        let steady_tone = tone(len, 300.0, 0.2);
        let input: Vec<f32> = steady_tone
            .iter()
            .zip(&steady_noise)
            .map(|(tone, noise)| tone + noise)
            .collect();

        let mut dsp = Spectral::new(f64::from(RATE));
        let output = process_blocks(&mut dsp, &params, &input, 512);

        let window = (0.5 * RATE) as usize;
        let start = len - window;
        let input_rms = rms(&input[start..]);
        let output_rms = rms(&output[start..]);
        assert!(
            output_rms < input_rms * 0.5,
            "output {output_rms} input {input_rms}"
        );
        assert!(output.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn high_frequency_steady_tone_is_attenuated() {
        let params = params(100.0, 30.0, 20.0);
        let len = 4 * RATE as usize;
        let input = tone(len, 6_000.0, 0.2);
        let mut dsp = Spectral::new(f64::from(RATE));
        let output = process_blocks(&mut dsp, &params, &input, 512);
        let window = (0.5 * RATE) as usize;
        let start = len - window;
        let input_rms = rms(&input[start..]);
        let output_rms = rms(&output[start..]);
        assert!(
            output_rms < input_rms * 0.5,
            "high tone output {output_rms} input {input_rms} ratio {}",
            output_rms / input_rms
        );
    }

    #[test]
    fn high_band_noise_is_attenuated_by_at_least_10_db() {
        let rate = 48_000.0_f32;
        let params = params(100.0, 30.0, 30.0);
        let len = 4 * rate as usize;
        let input = noise(len, 0.08);
        let mut dsp = Spectral::new(f64::from(rate));
        let mut left = input.clone();
        let mut right = input.clone();
        let mut offset = 0;
        while offset < len {
            let end = (offset + 512).min(len);
            dsp.process(&mut left[offset..end], &mut right[offset..end], &params);
            offset = end;
        }
        let start = len - rate as usize;
        let input_band = band_energy_at(&input[start..], 48_000.0, 6_000.0, 23_000.0);
        let output_band = band_energy_at(&left[start..], 48_000.0, 6_000.0, 23_000.0);
        let attenuation = -db(output_band / input_band);
        assert!(
            attenuation >= 10.0,
            "high band attenuation {attenuation:.1} dB"
        );
    }

    fn band_energy_at(samples: &[f32], rate: f32, low: f32, high: f32) -> f32 {
        let fft = Fft::new();
        let mut re = [0.0_f32; FFT_SIZE];
        let mut im = [0.0_f32; FFT_SIZE];
        let mut total = 0.0_f32;
        let mut frames = 0.0_f32;
        for chunk in samples.chunks_exact(FFT_SIZE) {
            for index in 0..FFT_SIZE {
                let hann = 0.5 - 0.5 * (2.0 * PI * index as f64 / FFT_SIZE as f64).cos();
                re[index] = chunk[index] * hann as f32;
                im[index] = 0.0;
            }
            fft.run(&mut re, &mut im, false);
            for bin in 0..BINS {
                let frequency = bin as f32 * rate / FFT_SIZE as f32;
                if frequency >= low && frequency <= high {
                    total += re[bin] * re[bin] + im[bin] * im[bin];
                }
            }
            frames += 1.0;
        }
        (total / frames.max(1.0)).sqrt()
    }

    #[test]
    fn strength_is_continuous_not_a_switch() {
        let len = 4 * RATE as usize;
        let input = noise(len, 0.05);
        let mut levels = Vec::new();
        for strength in [5.0_f32, 25.0, 50.0, 75.0, 100.0] {
            let params = params(strength, 20.0, 50.0);
            params.set(params.index_of("voice").expect("voice"), 0.0);
            let mut dsp = Spectral::new(f64::from(RATE));
            let output = process_blocks(&mut dsp, &params, &input, 512);
            levels.push(rms(&output[len - 8_000..]));
        }
        for pair in levels.windows(2) {
            assert!(
                pair[0] > pair[1],
                "strength did not increase reduction: {levels:?}"
            );
        }
        assert!(
            levels[0] > levels[4] * 1.5,
            "strength range is too compressed: {levels:?}"
        );
    }

    #[test]
    fn voice_knob_preserves_more_of_the_voice_band() {
        let len = 4 * RATE as usize;
        let noise = noise(len, 0.03);
        let input: Vec<f32> = vowel(len)
            .iter()
            .zip(&noise)
            .map(|(voice, noise)| voice + noise)
            .collect();
        let band = |voice: f32| {
            let params = params(100.0, 60.0, 50.0);
            params.set(params.index_of("voice").expect("voice"), voice);
            let mut dsp = Spectral::new(f64::from(RATE));
            let output = process_blocks(&mut dsp, &params, &input, 512);
            band_energy(&output[len - 8_000..], 300.0, 3_500.0)
        };
        let unprotected = band(0.0);
        let protected = band(100.0);
        assert!(
            protected > unprotected * 1.2,
            "Voice knob did not preserve the voice band: {unprotected} -> {protected}"
        );
    }

    #[test]
    fn synthetic_vowel_survives_noise() {
        let rate = RATE;
        let preroll = rate as usize;
        let vowel_len = 3 * rate as usize;
        let total = preroll + vowel_len;

        let mut clean_full = vowel(total);
        for sample in &mut clean_full[..preroll] {
            *sample = 0.0;
        }
        let clean: Vec<f32> = clean_full[preroll..].to_vec();
        let speech = &clean_full[preroll..];

        // 10 dB SNR uniform noise.
        let vowel_rms = rms(speech);
        let noise_rms = vowel_rms / 10.0_f32.powf(10.0 / 20.0);
        let noise_input = noise(total, noise_rms * 3.0_f32.sqrt());
        let input: Vec<f32> = clean_full
            .iter()
            .zip(&noise_input)
            .map(|(vowel, noise)| vowel + noise)
            .collect();

        let params = params(50.0, 20.0, 50.0);
        let mut dsp = Spectral::new(f64::from(rate));
        let output = process_blocks(&mut dsp, &params, &input, 256);

        let steady = rate as usize;
        let output_slice = &output[preroll + vowel_len - steady..];
        let clean_slice = &clean[vowel_len - steady..];

        let clean_band = band_energy(clean_slice, 300.0, 3_500.0);
        let output_band = band_energy(output_slice, 300.0, 3_500.0);
        let difference = db(output_band / clean_band).abs();
        assert!(
            difference < 3.0,
            "vowel band moved {difference:.1} dB (out {output_band}, clean {clean_band})"
        );

        // A band where the vowel has little energy must still suppress noise.
        let input_noise_band =
            band_energy(&input[preroll + vowel_len - steady..], 4_500.0, 5_500.0);
        let output_noise_band = band_energy(output_slice, 4_500.0, 5_500.0);
        assert!(
            output_noise_band < input_noise_band * 0.7,
            "noise-only band not attenuated: {} -> {}",
            input_noise_band,
            output_noise_band
        );
    }

    #[test]
    fn onset_is_not_ducked() {
        let rate = RATE as usize;
        let noise_len = rate;
        let burst_len = rate / 2;
        let total = noise_len + burst_len;

        let mut burst = vowel(burst_len);
        let burst_rms = rms(&burst);
        for sample in &mut burst {
            *sample *= 0.4 / burst_rms.max(1.0e-9);
        }

        let noise_input = noise(total, 0.05);
        let mut input = noise_input.clone();
        input[noise_len..]
            .iter_mut()
            .zip(&burst)
            .for_each(|(slot, sample)| *slot += *sample);

        let params = params(80.0, 20.0, 50.0);
        let mut dsp = Spectral::new(f64::from(RATE));
        let output = process_blocks(&mut dsp, &params, &input, 128);

        let window = (0.03 * RATE) as usize;
        let onset = noise_len + FFT_SIZE;
        let input_rms = rms(&input[noise_len..noise_len + window]);
        let output_rms = rms(&output[onset..onset + window]);
        assert!(
            output_rms > input_rms * 0.5,
            "onset ducked: output {output_rms} input {input_rms}"
        );
    }

    #[test]
    fn noise_tracker_does_not_learn_a_vowel() {
        let seed_len = (0.5 * RATE) as usize;
        let vowel_len = (2.0 * RATE) as usize;
        let params = params(50.0, 20.0, 50.0);
        let mut dsp = Spectral::new(f64::from(RATE));

        let seed = noise(seed_len, 0.02);
        process_blocks(&mut dsp, &params, &seed, 128);
        let before: f32 = dsp.noise_psd.iter().sum();

        let speech = vowel(vowel_len);
        process_blocks(&mut dsp, &params, &speech, 128);
        let after: f32 = dsp.noise_psd.iter().sum();

        let rise = 10.0 * (after / before.max(EPS)).log10();
        assert!(rise < 1.5, "noise tracker learned the vowel: {rise:.2} dB");
    }

    fn first_non_zero(input_len: usize, block: usize) -> Option<usize> {
        let params = ParamSet::new(BuiltinKind::NoiseRemoval);
        let mut dsp = Spectral::new(f64::from(RATE));
        let mut input = vec![0.0_f32; input_len];
        input[0] = 1.0;
        let output = process_blocks(&mut dsp, &params, &input, block);
        output.iter().position(|sample| sample.abs() > 1.0e-6)
    }

    #[test]
    fn impulse_latency_is_exactly_256_samples() {
        assert_eq!(first_non_zero(2_048, 64), Some(FFT_SIZE));
        assert_eq!(first_non_zero(2_048, 256), Some(FFT_SIZE));
    }

    #[test]
    fn passthrough_is_unity_after_the_latency() {
        // Zero reduction forces the gain clamp to unity, so this isolates the
        // sqrt-Hann overlap-add reconstruction.
        let params = params(0.0, 0.0, 0.0);
        let len = 4_096;
        let input = noise(len, 0.3);
        let mut dsp = Spectral::new(f64::from(RATE));
        let output = process_blocks(&mut dsp, &params, &input, 128);

        assert!(output[..FFT_SIZE].iter().all(|sample| *sample == 0.0));
        for index in 0..len - FFT_SIZE {
            let expected = input[index];
            let actual = output[index + FFT_SIZE];
            assert!(
                (actual - expected).abs() < 1.0e-4,
                "sample {index}: {actual} != {expected}"
            );
        }
    }

    #[test]
    fn stationary_noise_gain_is_stable() {
        let params = params(20.0, 20.0, 50.0);
        let len = 6 * 48_000usize;
        let input = noise(len, 0.05);
        let mut dsp = Spectral::new(48_000.0);
        let mut block = 0usize;
        let mut previous: Option<[f32; BINS]> = None;
        let mut changes = 0.0f64;
        let mut hops = 0usize;
        while block < len {
            let end = (block + 128).min(len);
            let mut left = input[block..end].to_vec();
            let mut right = left.clone();
            dsp.process(&mut left, &mut right, &params);
            block = end;
            if block < 2 * 48_000 {
                continue;
            }
            let current = dsp.applied_gain;
            if let Some(previous) = previous {
                for bin in 0..BINS {
                    changes += f64::from((current[bin] - previous[bin]).abs());
                }
                hops += 1;
            }
            previous = Some(current);
        }
        let mean_change = changes / (hops * BINS) as f64;
        assert!(mean_change < 0.01, "mean gain change {mean_change}");
    }

    #[test]
    fn nan_input_does_not_poison_later_output() {
        let params = params(50.0, 20.0, 50.0);
        let mut dsp = Spectral::new(f64::from(RATE));

        let mut left = vec![f32::NAN; 512];
        let mut right = vec![f32::NAN; 512];
        left[1] = f32::INFINITY;
        right[2] = f32::NEG_INFINITY;
        dsp.process(&mut left, &mut right, &params);
        assert!(
            left.iter()
                .chain(right.iter())
                .all(|sample| sample.is_finite())
        );

        let input = tone(2_048, 300.0, 0.3);
        let output = process_blocks(&mut dsp, &params, &input, 64);
        assert!(output.iter().all(|sample| sample.is_finite()));
        let peak = output.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()));
        assert!(peak > 0.01, "later output stayed silent: peak {peak}");
    }
}
