use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// Samples retained per channel for editor visualisation.
pub const ANALYSIS_CAPACITY: usize = 4096;
const SILENCE_DB: f32 = -120.0;

/// A lock-free tap the audio thread writes and the editor reads.
///
/// Samples are stored as `f32` bits in atomics. Snapshot reads are not
/// synchronised sample-by-sample across channels; the result is intended for
/// display only, so a torn frame is harmless. `push` performs no allocation.
pub struct AudioAnalysis {
    left: Box<[AtomicU32]>,
    right: Box<[AtomicU32]>,
    output_left: Box<[AtomicU32]>,
    output_right: Box<[AtomicU32]>,
    write: AtomicUsize,
    output_write: AtomicUsize,
    sample_rate: AtomicU32,
    input_peak_db: AtomicU32,
    output_peak_db: AtomicU32,
    reduction_db: AtomicU32,
    noise_floor_db: AtomicU32,
}

#[derive(Clone, Copy, Debug)]
pub struct AnalysisSnapshot {
    pub sample_rate: f32,
    pub input_peak_db: f32,
    pub output_peak_db: f32,
    pub reduction_db: f32,
    pub noise_floor_db: f32,
}

impl Default for AnalysisSnapshot {
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            input_peak_db: SILENCE_DB,
            output_peak_db: SILENCE_DB,
            reduction_db: 0.0,
            noise_floor_db: SILENCE_DB,
        }
    }
}

impl AudioAnalysis {
    pub fn new(sample_rate: f32) -> Arc<Self> {
        let rate = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            48_000.0
        };
        Arc::new(Self {
            left: (0..ANALYSIS_CAPACITY)
                .map(|_| AtomicU32::new(0.0f32.to_bits()))
                .collect(),
            right: (0..ANALYSIS_CAPACITY)
                .map(|_| AtomicU32::new(0.0f32.to_bits()))
                .collect(),
            output_left: (0..ANALYSIS_CAPACITY)
                .map(|_| AtomicU32::new(0.0f32.to_bits()))
                .collect(),
            output_right: (0..ANALYSIS_CAPACITY)
                .map(|_| AtomicU32::new(0.0f32.to_bits()))
                .collect(),
            write: AtomicUsize::new(0),
            output_write: AtomicUsize::new(0),
            sample_rate: AtomicU32::new(rate.to_bits()),
            input_peak_db: AtomicU32::new(SILENCE_DB.to_bits()),
            output_peak_db: AtomicU32::new(SILENCE_DB.to_bits()),
            reduction_db: AtomicU32::new(0.0f32.to_bits()),
            noise_floor_db: AtomicU32::new(SILENCE_DB.to_bits()),
        })
    }

    pub fn sample_rate(&self) -> f32 {
        f32::from_bits(self.sample_rate.load(Ordering::Relaxed))
    }

    /// Copies the input block into the ring and records its peak. Real-time safe.
    pub fn push(&self, left: &[f32], right: &[f32]) {
        let frames = left.len().min(right.len()).min(ANALYSIS_CAPACITY);
        if frames == 0 {
            return;
        }
        let mut write = self.write.load(Ordering::Relaxed);
        let mut peak = 0.0f32;
        for index in 0..frames {
            let l = left[index];
            let r = right[index];
            peak = peak.max(l.abs()).max(r.abs());
            self.left[write].store(l.to_bits(), Ordering::Relaxed);
            self.right[write].store(r.to_bits(), Ordering::Relaxed);
            write += 1;
            if write == ANALYSIS_CAPACITY {
                write = 0;
            }
        }
        self.write.store(write, Ordering::Release);
        self.input_peak_db
            .store(linear_to_db(peak).to_bits(), Ordering::Relaxed);
    }

    /// Copies the post-DSP block into the output ring and updates the meters.
    /// Real-time safe; call after the processor has written the block.
    pub fn push_output(&self, left: &[f32], right: &[f32]) {
        let frames = left.len().min(right.len()).min(ANALYSIS_CAPACITY);
        if frames == 0 {
            return;
        }
        let mut write = self.output_write.load(Ordering::Relaxed);
        let mut peak = 0.0f32;
        for index in 0..frames {
            let l = left[index];
            let r = right[index];
            peak = peak.max(l.abs()).max(r.abs());
            self.output_left[write].store(l.to_bits(), Ordering::Relaxed);
            self.output_right[write].store(r.to_bits(), Ordering::Relaxed);
            write += 1;
            if write == ANALYSIS_CAPACITY {
                write = 0;
            }
        }
        self.output_write.store(write, Ordering::Release);
        self.set_output_peak(peak);
    }

    /// Records the output block peak and derives the live gain reduction.
    pub fn record_output_peak(&self, peak: f32) {
        self.set_output_peak(peak);
    }

    /// Publishes an auto-detected noise floor for editors that show it.
    pub fn set_noise_floor_db(&self, db: f32) {
        let db = if db.is_finite() {
            db.clamp(-120.0, 0.0)
        } else {
            SILENCE_DB
        };
        self.noise_floor_db.store(db.to_bits(), Ordering::Relaxed);
    }

    fn set_output_peak(&self, peak: f32) {
        let input = f32::from_bits(self.input_peak_db.load(Ordering::Relaxed));
        let output = linear_to_db(peak);
        self.output_peak_db
            .store(output.to_bits(), Ordering::Relaxed);
        self.reduction_db
            .store((input - output).max(0.0).to_bits(), Ordering::Relaxed);
    }

    /// Copies the newest input samples in chronological order and returns the
    /// meters.
    pub fn snapshot(&self, left: &mut [f32], right: &mut [f32]) -> AnalysisSnapshot {
        copy_ring(
            &self.left,
            &self.right,
            self.write.load(Ordering::Acquire),
            left,
            right,
        );
        self.meters()
    }

    /// Copies the newest input and post-DSP samples in chronological order.
    pub fn snapshot_with_output(
        &self,
        left: &mut [f32],
        right: &mut [f32],
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) -> AnalysisSnapshot {
        copy_ring(
            &self.left,
            &self.right,
            self.write.load(Ordering::Acquire),
            left,
            right,
        );
        copy_ring(
            &self.output_left,
            &self.output_right,
            self.output_write.load(Ordering::Acquire),
            output_left,
            output_right,
        );
        self.meters()
    }

    fn meters(&self) -> AnalysisSnapshot {
        AnalysisSnapshot {
            sample_rate: self.sample_rate(),
            input_peak_db: f32::from_bits(self.input_peak_db.load(Ordering::Relaxed)),
            output_peak_db: f32::from_bits(self.output_peak_db.load(Ordering::Relaxed)),
            reduction_db: f32::from_bits(self.reduction_db.load(Ordering::Relaxed)),
            noise_floor_db: f32::from_bits(self.noise_floor_db.load(Ordering::Relaxed)),
        }
    }
}

fn copy_ring(
    source_left: &[AtomicU32],
    source_right: &[AtomicU32],
    write: usize,
    left: &mut [f32],
    right: &mut [f32],
) {
    let frames = left.len().min(right.len()).min(ANALYSIS_CAPACITY);
    let start = (write + ANALYSIS_CAPACITY - frames) % ANALYSIS_CAPACITY;
    for (offset, (left, right)) in left[..frames]
        .iter_mut()
        .zip(right[..frames].iter_mut())
        .enumerate()
    {
        let slot = (start + offset) % ANALYSIS_CAPACITY;
        *left = f32::from_bits(source_left[slot].load(Ordering::Relaxed));
        *right = f32::from_bits(source_right[slot].load(Ordering::Relaxed));
    }
    left[frames..].fill(0.0);
    right[frames..].fill(0.0);
}

fn linear_to_db(value: f32) -> f32 {
    if value.is_finite() && value > 1.0e-9 {
        20.0 * value.log10()
    } else {
        SILENCE_DB
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_returns_the_newest_samples_in_order() {
        let analysis = AudioAnalysis::new(48_000.0);
        analysis.push(&[1.0, 2.0, 3.0], &[-1.0, -2.0, -3.0]);
        let mut left = [0.0; 8];
        let mut right = [0.0; 8];
        let snapshot = analysis.snapshot(&mut left, &mut right);
        assert_eq!(snapshot.sample_rate, 48_000.0);
        assert_eq!(&left[5..8], &[1.0, 2.0, 3.0]);
        assert_eq!(&right[5..8], &[-1.0, -2.0, -3.0]);
        assert_eq!(&left[..5], &[0.0; 5]);
    }

    #[test]
    fn peaks_and_reduction_are_reported_in_db() {
        let analysis = AudioAnalysis::new(48_000.0);
        analysis.push(&[0.5, -0.25], &[0.25, -0.5]);
        analysis.record_output_peak(0.25);
        let mut left = [0.0; 2];
        let mut right = [0.0; 2];
        let snapshot = analysis.snapshot(&mut left, &mut right);
        assert!((snapshot.input_peak_db - (-6.0206)).abs() < 0.01);
        assert!((snapshot.output_peak_db - (-12.0412)).abs() < 0.01);
        assert!((snapshot.reduction_db - 6.0206).abs() < 0.01);
    }

    #[test]
    fn snapshot_with_output_returns_input_and_processed_samples() {
        let analysis = AudioAnalysis::new(48_000.0);
        analysis.push(&[0.5, -0.25], &[0.25, -0.5]);
        analysis.push_output(&[0.125, -0.0625], &[0.0625, -0.125]);
        let mut left = [0.0; 2];
        let mut right = [0.0; 2];
        let mut output_left = [0.0; 2];
        let mut output_right = [0.0; 2];
        let snapshot = analysis.snapshot_with_output(
            &mut left,
            &mut right,
            &mut output_left,
            &mut output_right,
        );
        assert_eq!(left, [0.5, -0.25]);
        assert_eq!(right, [0.25, -0.5]);
        assert_eq!(output_left, [0.125, -0.0625]);
        assert_eq!(output_right, [0.0625, -0.125]);
        assert!((snapshot.output_peak_db - (-18.0618)).abs() < 0.01);
    }

    #[test]
    fn a_block_longer_than_the_ring_is_truncated_without_panicking() {
        let analysis = AudioAnalysis::new(48_000.0);
        let input = vec![0.5; ANALYSIS_CAPACITY + 64];
        analysis.push(&input, &input);
        let mut left = vec![0.0; 16];
        let mut right = vec![0.0; 16];
        analysis.snapshot(&mut left, &mut right);
        assert!(left.iter().all(|sample| *sample == 0.5));
    }
}
