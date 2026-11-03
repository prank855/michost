//! Shared-memory layout for the MicHost virtual microphone.
//!
//! `MH_VMIC_RING` is created by the driver in a named section object. The
//! header is followed by `ring_frames * channels` interleaved `float32`
//! samples. All multi-byte fields are little-endian, matching x64/ARM64
//! Windows. The driver writes `read_index`, `underruns`, `qpc_frequency` and
//! `consumer_active`; MicHost writes `write_index`, `overruns` and
//! `last_write_qpc`.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::VmicError;

/// `'MHVM'` in little-endian order.
pub const MAGIC: u32 = 0x4D48_564D;
/// ABI version checked by both sides.
pub const VERSION: u32 = 1;
/// Header bytes before the interleaved sample data.
pub const HEADER_SIZE: usize = 80;
/// Samples are always 32-bit float.
pub const BYTES_PER_SAMPLE: u32 = 4;
/// Smallest packet the driver accepts.
pub const MIN_PACKET_FRAMES: u32 = 16;
/// Largest packet the driver accepts; keeps the first copy bounded.
pub const MAX_PACKET_FRAMES: u32 = 16_384;
/// Smallest ring capacity the driver accepts (four default packets).
pub const RING_FRAMES_MIN: u32 = 2048;
/// Largest ring capacity the driver accepts.
pub const RING_FRAMES_MAX: u32 = 1_048_576;

/// Mix format negotiated with the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub sample_rate: u32,
    pub channels: u32,
    /// Driver packet size in frames; also the `GetReadPacket` granularity.
    pub packet_frames: u32,
}

impl Format {
    pub const fn bytes_per_frame(&self) -> usize {
        self.channels as usize * BYTES_PER_SAMPLE as usize
    }

    pub const fn packet_bytes(&self) -> usize {
        self.bytes_per_frame() * self.packet_frames as usize
    }

    /// Validates the format against the driver bounds.
    pub fn validate(&self) -> Result<(), VmicError> {
        if self.channels == 0 || self.channels > 2 {
            return Err(VmicError::Layout(format!(
                "virtual mic channel count {} is out of range (1-2)",
                self.channels
            )));
        }
        if !(8_000..=384_000).contains(&self.sample_rate) {
            return Err(VmicError::Layout(format!(
                "virtual mic sample rate {} is out of range",
                self.sample_rate
            )));
        }
        if !(MIN_PACKET_FRAMES..=MAX_PACKET_FRAMES).contains(&self.packet_frames) {
            return Err(VmicError::Layout(format!(
                "virtual mic packet size {} is out of range",
                self.packet_frames
            )));
        }
        Ok(())
    }
}

/// Default format requested by MicHost when the driver has no preference.
pub const DEFAULT_FORMAT: Format = Format {
    sample_rate: 48_000,
    channels: 2,
    packet_frames: 480,
};

/// Driver-owned ring header. Field order is the ABI; do not reorder.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RingHeader {
    pub magic: u32,
    pub version: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub bytes_per_sample: u32,
    pub packet_frames: u32,
    pub ring_frames: u32,
    pub writer_active: u32,
    pub write_index: u64,
    pub read_index: u64,
    pub overruns: u64,
    pub underruns: u64,
    pub qpc_frequency: u64,
    pub last_write_qpc: u64,
}

/// Point-in-time counters read from the shared header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RingStats {
    pub packet_frames: u32,
    pub write_index: u64,
    pub read_index: u64,
    pub buffered_frames: u64,
    pub ring_frames: u32,
    pub overruns: u64,
    pub underruns: u64,
    pub qpc_frequency: u64,
    pub last_write_qpc: u64,
    pub writer_active: bool,
}

/// A mapped view of the driver section. The pointer is valid for the lifetime
/// of the owner (the section mapping or a test buffer); the writer never frees
/// it.
pub struct RingView {
    base: *mut u8,
    len: usize,
    _owner: Option<Arc<dyn Any + Send + Sync>>,
}

// The view is moved to the pump thread; the driver only writes the header and
// reads the sample area, and the pump is the single host-side writer.
unsafe impl Send for RingView {}

impl RingView {
    /// # Safety
    /// `base` must point to at least `len` readable and writable bytes that
    /// stay mapped until the view is dropped.
    pub unsafe fn from_raw(base: *mut u8, len: usize) -> Self {
        Self {
            base,
            len,
            _owner: None,
        }
    }

    /// Creates a view that keeps `owner` alive for as long as the view exists.
    /// The owner is the section mapping; dropping the last view/view-writer
    /// reference unmaps it.
    ///
    /// # Safety
    /// `base` must point to at least `len` bytes inside `owner` that stay
    /// mapped while `owner` is alive.
    pub unsafe fn from_raw_owned(
        base: *mut u8,
        len: usize,
        owner: Arc<dyn Any + Send + Sync>,
    ) -> Self {
        Self {
            base,
            len,
            _owner: Some(owner),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Checks magic, version, format fields, sizes and alignment.
    pub fn validate(&self) -> Result<(), VmicError> {
        if self.base.is_null() {
            return Err(VmicError::Layout(
                "the virtual mic section is null".to_string(),
            ));
        }
        if (self.base as usize) % 8 != 0 {
            return Err(VmicError::Layout(
                "the virtual mic section is not 8-byte aligned".to_string(),
            ));
        }
        if self.len < HEADER_SIZE {
            return Err(VmicError::Layout(format!(
                "the virtual mic section is {} bytes, shorter than the {HEADER_SIZE}-byte header",
                self.len
            )));
        }
        if self.u32_at(0) != MAGIC {
            return Err(VmicError::Layout(format!(
                "the virtual mic section magic is 0x{:08X}, expected 0x{MAGIC:08X}",
                self.u32_at(0)
            )));
        }
        let version = self.u32_at(4);
        if version != VERSION {
            return Err(VmicError::Layout(format!(
                "virtual mic driver ABI version {version} does not match host version {VERSION}"
            )));
        }
        if self.u32_at(16) != BYTES_PER_SAMPLE {
            return Err(VmicError::Layout(
                "the virtual mic driver does not use float32 samples".to_string(),
            ));
        }
        let channels = self.u32_at(12);
        let ring_frames = self.u32_at(24);
        if channels == 0 || ring_frames == 0 {
            return Err(VmicError::Layout(
                "the virtual mic section reports zero channels or ring frames".to_string(),
            ));
        }
        let needed =
            HEADER_SIZE + ring_frames as usize * channels as usize * BYTES_PER_SAMPLE as usize;
        if self.len < needed {
            return Err(VmicError::Layout(format!(
                "the virtual mic section is {} bytes, need {needed} for {ring_frames} frames of {channels} channels",
                self.len
            )));
        }
        Ok(())
    }

    pub fn format(&self) -> Format {
        Format {
            sample_rate: self.u32_at(8),
            channels: self.u32_at(12),
            packet_frames: self.u32_at(20),
        }
    }

    pub fn ring_frames(&self) -> u32 {
        self.u32_at(24)
    }

    pub fn writer_active(&self) -> bool {
        self.u32_at(28) != 0
    }

    pub fn write_index(&self) -> u64 {
        self.atomic(32).load(Ordering::Acquire)
    }

    pub fn read_index(&self) -> u64 {
        self.atomic(40).load(Ordering::Acquire)
    }

    pub fn buffered_frames(&self) -> u64 {
        self.write_index().wrapping_sub(self.read_index())
    }

    pub fn overruns(&self) -> u64 {
        self.atomic(48).load(Ordering::Relaxed)
    }

    pub fn underruns(&self) -> u64 {
        self.atomic(56).load(Ordering::Relaxed)
    }

    pub fn qpc_frequency(&self) -> u64 {
        self.atomic(64).load(Ordering::Relaxed)
    }

    pub fn last_write_qpc(&self) -> u64 {
        self.atomic(72).load(Ordering::Relaxed)
    }

    pub fn set_write_index(&self, value: u64) {
        self.atomic(32).store(value, Ordering::Release);
    }

    pub fn set_last_write_qpc(&self, value: u64) {
        self.atomic(72).store(value, Ordering::Relaxed);
    }

    pub fn add_overrun(&self) {
        self.atomic(48).fetch_add(1, Ordering::Relaxed);
    }

    pub fn stats(&self) -> RingStats {
        let write_index = self.write_index();
        let read_index = self.read_index();
        RingStats {
            packet_frames: self.u32_at(20),
            write_index,
            read_index,
            buffered_frames: write_index.wrapping_sub(read_index),
            ring_frames: self.ring_frames(),
            overruns: self.overruns(),
            underruns: self.underruns(),
            qpc_frequency: self.qpc_frequency(),
            last_write_qpc: self.last_write_qpc(),
            writer_active: self.writer_active(),
        }
    }

    /// Interleaved sample storage. Callers must have validated the view and
    /// stay inside `ring_frames * channels`.
    ///
    /// # Safety
    /// `offset_frames * channels` must be less than `ring_frames * channels`.
    pub unsafe fn sample_ptr(&self, offset_frames: usize, channels: usize) -> *mut f32 {
        // SAFETY: caller guarantees the offset and that the mapping is live.
        unsafe {
            self.base
                .add(HEADER_SIZE + offset_frames * channels * 4)
                .cast::<f32>()
        }
    }

    fn atomic(&self, offset: usize) -> &AtomicU64 {
        // SAFETY: validate() checked alignment and length; the field is 8-byte
        // aligned because the header starts at offset 0 and these offsets are
        // multiples of 8. Atomics shared with the kernel are plain aligned
        // loads/stores on Windows.
        unsafe { AtomicU64::from_ptr(self.base.add(offset).cast::<u64>()) }
    }

    fn u32_at(&self, offset: usize) -> u32 {
        debug_assert!(offset + 4 <= self.len);
        // SAFETY: validate() and the fixed header size guarantee the range.
        unsafe { std::ptr::read_volatile(self.base.add(offset).cast::<u32>()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_matches_abi() {
        assert_eq!(std::mem::size_of::<RingHeader>(), HEADER_SIZE);
        assert_eq!(std::mem::offset_of!(RingHeader, write_index), 32);
        assert_eq!(std::mem::offset_of!(RingHeader, read_index), 40);
        assert_eq!(std::mem::offset_of!(RingHeader, overruns), 48);
        assert_eq!(std::mem::offset_of!(RingHeader, underruns), 56);
        assert_eq!(std::mem::offset_of!(RingHeader, qpc_frequency), 64);
        assert_eq!(std::mem::offset_of!(RingHeader, last_write_qpc), 72);
    }

    #[test]
    fn format_bounds_are_enforced() {
        assert!(DEFAULT_FORMAT.validate().is_ok());
        assert!(
            Format {
                channels: 0,
                ..DEFAULT_FORMAT
            }
            .validate()
            .is_err()
        );
        assert!(
            Format {
                packet_frames: 0,
                ..DEFAULT_FORMAT
            }
            .validate()
            .is_err()
        );
        assert!(
            Format {
                sample_rate: 0,
                ..DEFAULT_FORMAT
            }
            .validate()
            .is_err()
        );
    }
}
