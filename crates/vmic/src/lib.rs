//! MicHost virtual microphone contract.
//!
//! This crate owns the shared-memory ABI between `mic_host_vmic.sys` and the
//! MicHost process, the non-blocking ring writer used by the graph/pump thread,
//! and the user-mode control-device client. It never links against the driver.
//!
//! The authoritative design is `docs/virtual-microphone.md`. Keep the header
//! layout in `layout.rs` byte-for-byte in sync with `driver/inc/mh_vmic.h`.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod client;
pub mod layout;
pub mod writer;

pub use client::{VirtualMicSink, VmicClient, WriterInfo};
pub use layout::{
    BYTES_PER_SAMPLE, DEFAULT_FORMAT, Format, HEADER_SIZE, MAGIC, MAX_PACKET_FRAMES,
    MIN_PACKET_FRAMES, RING_FRAMES_MAX, RING_FRAMES_MIN, RingHeader, RingStats, RingView, VERSION,
};
pub use writer::{RingWriter, WriteReport};

use std::fmt;

/// Errors from the control device, the section mapping, or a layout mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmicError {
    /// The driver/control device is not installed or not reachable.
    NotInstalled(String),
    /// The driver rejected a second writer or a control request.
    Busy(String),
    /// The shared section or its header does not match this build.
    Layout(String),
    /// A Win32 call failed; the message contains the call and OS error.
    Io(String),
    /// The operation is not available on this platform or build.
    Unsupported(String),
}

impl fmt::Display for VmicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled(message)
            | Self::Busy(message)
            | Self::Layout(message)
            | Self::Io(message)
            | Self::Unsupported(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for VmicError {}
