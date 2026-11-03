//! Non-blocking writer for the shared ring.
//!
//! The graph/pump thread is the only writer. `write_frames` never blocks,
//! never allocates and never advances the driver-owned `read_index`: when the
//! ring is full it writes what fits and counts an overrun.

use crate::layout::{RingStats, RingView};
use crate::{Format, VmicError};

/// Result of one ring write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteReport {
    /// Frames copied into the ring.
    pub written: usize,
    /// Frames skipped because the ring was full.
    pub skipped: usize,
    /// True when at least one frame was skipped.
    pub overrun: bool,
}

/// Monotonic QPC timestamp, or 0 on non-Windows test builds.
pub fn now_qpc() -> u64 {
    #[cfg(windows)]
    {
        // SAFETY: QueryPerformanceCounter writes to a valid local.
        let mut counter = 0i64;
        if unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut counter) }
            != 0
        {
            return counter as u64;
        }
        0
    }
    #[cfg(not(windows))]
    {
        0
    }
}

pub struct RingWriter {
    view: RingView,
    format: Format,
}

impl RingWriter {
    /// Wraps a mapped view after validating the driver header.
    pub fn new(view: RingView) -> Result<Self, VmicError> {
        view.validate()?;
        let format = view.format();
        format.validate()?;
        if view.ring_frames() > crate::RING_FRAMES_MAX {
            return Err(VmicError::Layout(
                "the virtual mic ring is larger than the supported maximum".to_string(),
            ));
        }
        Ok(Self { view, format })
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn stats(&self) -> RingStats {
        self.view.stats()
    }

    pub fn buffered_frames(&self) -> u64 {
        self.view.buffered_frames()
    }

    /// Frames that fit before the driver catches up with the writer.
    pub fn free_frames(&self) -> u64 {
        let capacity = self.view.ring_frames() as u64;
        let used = self.view.buffered_frames();
        capacity.saturating_sub(used.min(capacity))
    }

    /// Writes one interleaved block. `interleaved.len()` must be a multiple of
    /// the channel count; a trailing partial frame is ignored.
    pub fn write_frames(&mut self, interleaved: &[f32]) -> WriteReport {
        let channels = self.format.channels as usize;
        debug_assert_eq!(channels, self.view.format().channels as usize);
        let frames = interleaved.len() / channels;
        if frames == 0 {
            return WriteReport::default();
        }
        let capacity = self.view.ring_frames() as u64;
        let write_index = self.view.write_index();
        let free = self.free_frames();
        let to_write = (frames as u64).min(free) as usize;
        if to_write > 0 {
            self.copy(interleaved, write_index, to_write);
            self.view
                .set_write_index(write_index.wrapping_add(to_write as u64));
            self.view.set_last_write_qpc(now_qpc());
        }
        if to_write < frames {
            self.view.add_overrun();
            return WriteReport {
                written: to_write,
                skipped: frames - to_write,
                overrun: true,
            };
        }
        debug_assert!(capacity >= to_write as u64);
        WriteReport {
            written: to_write,
            skipped: 0,
            overrun: false,
        }
    }

    /// Fills the ring with silence up to `frames`, bounded by free space.
    /// Used at writer start so the endpoint does not underrun immediately.
    pub fn prefill_silence(&mut self, frames: usize) -> WriteReport {
        let frames = frames.min(self.format.packet_frames as usize * 4);
        if frames == 0 {
            return WriteReport::default();
        }
        let capacity = self.view.ring_frames() as u64;
        let write_index = self.view.write_index();
        let free = self.free_frames();
        let to_write = (frames as u64).min(free) as usize;
        if to_write > 0 {
            self.zero(write_index, to_write);
            self.view
                .set_write_index(write_index.wrapping_add(to_write as u64));
            self.view.set_last_write_qpc(now_qpc());
        }
        if to_write < frames {
            self.view.add_overrun();
            return WriteReport {
                written: to_write,
                skipped: frames - to_write,
                overrun: true,
            };
        }
        debug_assert!(capacity >= to_write as u64);
        WriteReport {
            written: to_write,
            skipped: 0,
            overrun: false,
        }
    }

    fn copy(&self, source: &[f32], write_index: u64, frames: usize) {
        let channels = self.format.channels as usize;
        let capacity = self.view.ring_frames() as usize;
        let start = (write_index % capacity as u64) as usize;
        let first = frames.min(capacity - start);
        let second = frames - first;
        // SAFETY: `RingWriter::new` validated the view and the offsets below
        // stay inside `capacity`; `source` holds at least `frames` frames.
        unsafe {
            let dst = self.view.sample_ptr(start, channels);
            std::ptr::copy_nonoverlapping(source.as_ptr(), dst, first * channels);
            if second > 0 {
                let dst = self.view.sample_ptr(0, channels);
                std::ptr::copy_nonoverlapping(
                    source.as_ptr().add(first * channels),
                    dst,
                    second * channels,
                );
            }
        }
    }

    fn zero(&self, write_index: u64, frames: usize) {
        let channels = self.format.channels as usize;
        let capacity = self.view.ring_frames() as usize;
        let start = (write_index % capacity as u64) as usize;
        let first = frames.min(capacity - start);
        let second = frames - first;
        // SAFETY: same bounds argument as `copy`.
        unsafe {
            let dst = self.view.sample_ptr(start, channels);
            std::ptr::write_bytes(dst.cast::<u8>(), 0, first * channels * 4);
            if second > 0 {
                let dst = self.view.sample_ptr(0, channels);
                std::ptr::write_bytes(dst.cast::<u8>(), 0, second * channels * 4);
            }
        }
    }
}

impl RingWriter {
    /// Buffered frames as seen by the UI without touching the ring.
    pub fn buffered(&self) -> u64 {
        self.view.buffered_frames()
    }

    pub fn writer_active(&self) -> bool {
        self.view.writer_active()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Format, HEADER_SIZE, MAGIC, VERSION};

    struct TestRing {
        storage: Vec<u64>,
    }

    impl TestRing {
        fn new(ring_frames: u32, channels: u32, packet_frames: u32) -> Self {
            let bytes = HEADER_SIZE + ring_frames as usize * channels as usize * 4;
            let mut storage = vec![0u64; bytes.div_ceil(8)];
            let base = storage.as_mut_ptr().cast::<u8>();
            // SAFETY: storage is live, aligned and large enough.
            unsafe {
                write_u32(base, 0, MAGIC);
                write_u32(base, 4, VERSION);
                write_u32(base, 8, 48_000);
                write_u32(base, 12, channels);
                write_u32(base, 16, 4);
                write_u32(base, 20, packet_frames);
                write_u32(base, 24, ring_frames);
                write_u32(base, 28, 1);
            }
            Self { storage }
        }

        fn view(&mut self) -> RingView {
            let len = self.storage.len() * 8;
            // SAFETY: the storage outlives the view in each test.
            unsafe { RingView::from_raw(self.storage.as_mut_ptr().cast::<u8>(), len) }
        }

        fn set_read_index(&mut self, value: u64) {
            let base = self.storage.as_mut_ptr().cast::<u8>();
            // SAFETY: the header is live and the field is 8-byte aligned.
            unsafe { base.add(40).cast::<u64>().write_volatile(value) };
        }
    }

    unsafe fn write_u32(base: *mut u8, offset: usize, value: u32) {
        // SAFETY: caller provides a valid header base.
        unsafe { base.add(offset).cast::<u32>().write_volatile(value) };
    }

    #[test]
    fn writes_interleaved_frames_and_advances_index() {
        let mut ring = TestRing::new(8, 2, 64);
        let mut writer = RingWriter::new(ring.view()).unwrap();
        let report = writer.write_frames(&[1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
        assert_eq!(
            report,
            WriteReport {
                written: 3,
                skipped: 0,
                overrun: false
            }
        );
        assert_eq!(writer.buffered_frames(), 3);
        assert!(writer.stats().last_write_qpc > 0 || cfg!(not(windows)));
    }

    #[test]
    fn wraps_at_capacity() {
        let mut ring = TestRing::new(4, 1, 64);
        let mut writer = RingWriter::new(ring.view()).unwrap();
        assert_eq!(writer.write_frames(&[1.0, 2.0, 3.0]).written, 3);
        // Three frames are buffered; the driver consumes two.
        ring.set_read_index(2);
        let report = writer.write_frames(&[4.0, 5.0, 6.0]);
        assert_eq!(report.written, 3);
        let data = unsafe { std::slice::from_raw_parts(writer.view.sample_ptr(0, 1), 4) };
        assert_eq!(data, &[5.0, 6.0, 3.0, 4.0]);
    }

    #[test]
    fn full_ring_skips_and_counts_overrun() {
        let mut ring = TestRing::new(4, 1, 64);
        let mut writer = RingWriter::new(ring.view()).unwrap();
        assert_eq!(writer.write_frames(&[1.0, 2.0, 3.0, 4.0]).written, 4);
        let report = writer.write_frames(&[5.0, 6.0]);
        assert_eq!(
            report,
            WriteReport {
                written: 0,
                skipped: 2,
                overrun: true
            }
        );
        assert_eq!(writer.stats().overruns, 1);
    }

    #[test]
    fn prefill_fills_silence() {
        let mut ring = TestRing::new(16, 2, 64);
        let mut writer = RingWriter::new(ring.view()).unwrap();
        assert_eq!(writer.prefill_silence(8).written, 8);
        assert_eq!(writer.buffered_frames(), 8);
        let data = unsafe { std::slice::from_raw_parts(writer.view.sample_ptr(0, 2), 16) };
        assert!(data.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn rejects_bad_layout() {
        let mut ring = TestRing::new(8, 2, 64);
        unsafe { write_u32(ring.storage.as_mut_ptr().cast::<u8>(), 4, VERSION + 1) };
        assert!(RingWriter::new(ring.view()).is_err());
    }

    #[test]
    fn format_round_trips() {
        let mut ring = TestRing::new(8, 2, 64);
        let writer = RingWriter::new(ring.view()).unwrap();
        assert_eq!(
            writer.format(),
            Format {
                sample_rate: 48_000,
                channels: 2,
                packet_frames: 64
            }
        );
    }
}
