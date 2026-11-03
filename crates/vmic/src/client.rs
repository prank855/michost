//! Control-device client and the host-side sink.
//!
//! The Windows control IOCTLs and section mapping live in the `windows_impl`
//! module below; the public surface stays stable for the host integration:
//!
//! ```text
//! let sink = VirtualMicSink::open(Format { sample_rate: 48_000, channels: 2, packet_frames: 480 })?;
//! sink.push(interleaved);
//! let stats = sink.stats();
//! ```

use crate::VmicError;
use crate::layout::{Format, RingStats, RingView};
use crate::writer::{RingWriter, WriteReport};

/// Name of the control device object exposed by the driver.
pub const CONTROL_DEVICE: &str = r"\\.\MicHostVirtualMic";

/// Information returned by `OPEN_WRITER`/`GET_SECTION_INFO`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterInfo {
    /// Format the driver exposes on the capture pin.
    pub format: Format,
    /// Ring capacity in frames (driver chosen, at least two packets).
    pub ring_frames: u32,
    /// Base-named section visible to the writer process.
    pub section_name: String,
}

/// Driver-side counters that are not in the shared header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DriverStats {
    pub consumer_active: bool,
    pub underruns: u64,
    pub read_index: u64,
    pub qpc_frequency: u64,
}

#[cfg(windows)]
pub use windows_impl::VmicClient;

#[cfg(not(windows))]
pub use stub_impl::VmicClient;

/// Opens the control device, takes the single writer slot, maps the section
/// and returns a configured ring writer. Fails with `NotInstalled` when the
/// driver is absent; callers must fall back to a physical output.
pub struct VirtualMicSink {
    client: VmicClient,
    writer: RingWriter,
}

impl VirtualMicSink {
    pub fn open(format: Format) -> Result<Self, VmicError> {
        format.validate()?;
        let client = VmicClient::open()?;
        let info = client.open_writer(format)?;
        let view = client.map_ring(&info)?;
        let writer = match RingWriter::new(view) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = client.close_writer();
                return Err(error);
            }
        };
        if writer.format() != info.format {
            let _ = client.close_writer();
            return Err(VmicError::Layout(format!(
                "virtual mic format mismatch: section reports {:?}, driver reports {:?}",
                writer.format(),
                info.format
            )));
        }
        Ok(Self { client, writer })
    }

    pub fn format(&self) -> Format {
        self.writer.format()
    }

    pub fn ring_frames(&self) -> u32 {
        self.writer.stats().ring_frames
    }

    /// Non-blocking write of an interleaved block.
    pub fn push(&mut self, interleaved: &[f32]) -> WriteReport {
        self.writer.write_frames(interleaved)
    }

    /// Pre-fills silence so a freshly started endpoint has data to read.
    pub fn prefill(&mut self, frames: usize) -> WriteReport {
        self.writer.prefill_silence(frames)
    }

    pub fn stats(&self) -> RingStats {
        self.writer.stats()
    }

    pub fn driver_stats(&self) -> Result<DriverStats, VmicError> {
        self.client.stats()
    }

    pub fn client(&self) -> &VmicClient {
        &self.client
    }
}

impl Drop for VirtualMicSink {
    fn drop(&mut self) {
        let _ = self.client.close_writer();
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::sync::{Arc, Mutex};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_BUSY, ERROR_FILE_NOT_FOUND, ERROR_INVALID_HANDLE, ERROR_INVALID_NAME,
        ERROR_NOT_FOUND, ERROR_PATH_NOT_FOUND, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Memory::{
        FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
        UnmapViewOfFile,
    };

    use super::{CONTROL_DEVICE, DriverStats, Format, RingView, VmicError, WriterInfo};
    use crate::layout::{BYTES_PER_SAMPLE, HEADER_SIZE, RING_FRAMES_MAX, RING_FRAMES_MIN, VERSION};
    use crate::layout::{MAX_PACKET_FRAMES, MIN_PACKET_FRAMES};

    const IOCTL_GET_VERSION: u32 = 0x0022_2000;
    const IOCTL_OPEN_WRITER: u32 = 0x0022_2004;
    const IOCTL_CLOSE_WRITER: u32 = 0x0022_2008;
    const IOCTL_GET_STATS: u32 = 0x0022_200C;
    const IOCTL_GET_SECTION_INFO: u32 = 0x0022_2014;

    /// `STATUS_DEVICE_BUSY`; DeviceIoControl can surface it unmapped.
    const STATUS_DEVICE_BUSY: u32 = 0xC000_00BB;
    const SECTION_INFO_WIRE_BYTES: usize = 24;
    const SECTION_NAME_MAX_BYTES: usize = 1024;

    #[repr(C)]
    #[allow(dead_code)]
    struct OpenWriterIn {
        sample_rate: u32,
        channels: u32,
        packet_frames: u32,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct SectionInfoWire {
        ring_frames: u32,
        packet_frames: u32,
        sample_rate: u32,
        channels: u32,
        name_bytes: u32,
        reserved: u32,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct StatsWire {
        consumer_active: u32,
        reserved: u32,
        underruns: u64,
        read_index: u64,
        qpc_frequency: u64,
    }

    /// A live section mapping. The view pointer stays valid until `Drop`.
    struct MappedSection {
        base: *mut u8,
        len: usize,
        mapping: HANDLE,
    }

    // The map/view are process-wide kernel objects; only the owning writer
    // thread touches the bytes.
    unsafe impl Send for MappedSection {}
    unsafe impl Sync for MappedSection {}

    impl MappedSection {
        fn ring_view(self: &Arc<Self>) -> RingView {
            // SAFETY: the mapping is live while the returned view holds this
            // `Arc` (or while `self` is alive) and covers `len` bytes.
            unsafe { RingView::from_raw_owned(self.base, self.len, self.clone()) }
        }
    }

    impl Drop for MappedSection {
        fn drop(&mut self) {
            // SAFETY: both handles were produced by the Win32 calls below and
            // are released exactly once.
            unsafe {
                UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.base.cast(),
                });
                CloseHandle(self.mapping);
            }
        }
    }

    pub struct VmicClient {
        handle: HANDLE,
        mapping: Mutex<Option<Arc<MappedSection>>>,
    }

    // SAFETY: Win32 handles are process-wide and the mapped view is only read
    // and written by the single writer that owns `RingView`.
    unsafe impl Send for VmicClient {}

    impl VmicClient {
        pub fn open() -> Result<Self, VmicError> {
            let path = wide_string(CONTROL_DEVICE);
            // SAFETY: `path` is NUL-terminated and outlives the call.
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                let code = last_error();
                return Err(match code {
                    ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND | ERROR_INVALID_NAME => {
                        VmicError::NotInstalled(not_installed_message(code))
                    }
                    _ => VmicError::Io(format!(
                        "CreateFileW({CONTROL_DEVICE}) failed: {}",
                        win32_message(code)
                    )),
                });
            }
            Ok(Self {
                handle,
                mapping: Mutex::new(None),
            })
        }

        pub fn is_installed() -> bool {
            Self::open().is_ok()
        }

        pub fn driver_version(&self) -> Result<u32, VmicError> {
            let mut version: u32 = 0;
            let returned = self.ioctl(
                IOCTL_GET_VERSION,
                std::ptr::null(),
                0,
                std::ptr::from_mut(&mut version).cast(),
                size_of::<u32>() as u32,
            )?;
            if (returned as usize) < size_of::<u32>() {
                return Err(VmicError::Layout(format!(
                    "GET_VERSION returned {returned} bytes, expected {}",
                    size_of::<u32>()
                )));
            }
            if version == 0 {
                return Err(VmicError::Layout(
                    "the virtual mic driver reports version 0".to_string(),
                ));
            }
            if version > VERSION {
                return Err(VmicError::Layout(format!(
                    "virtual mic driver version {version} is newer than the host supports ({VERSION}); update MicHost"
                )));
            }
            Ok(version)
        }

        pub fn open_writer(&self, format: Format) -> Result<WriterInfo, VmicError> {
            format.validate()?;
            let request = OpenWriterIn {
                sample_rate: format.sample_rate,
                channels: format.channels,
                packet_frames: format.packet_frames,
            };
            self.ioctl_raw(
                IOCTL_OPEN_WRITER,
                std::ptr::from_ref(&request).cast(),
                size_of::<OpenWriterIn>() as u32,
                std::ptr::null_mut(),
                0,
            )
            .map_err(|code| match code {
                ERROR_BUSY | STATUS_DEVICE_BUSY => VmicError::Busy(format!(
                    "the virtual mic already has a writer ({})",
                    win32_message(code)
                )),
                _ => VmicError::Io(format!("OPEN_WRITER failed: {}", win32_message(code))),
            })?;

            let result = self.read_section_info(format);
            if result.is_err() {
                // OPEN_WRITER succeeded, so release the slot before returning.
                let _ = self.close_writer();
            }
            result
        }

        pub fn close_writer(&self) -> Result<(), VmicError> {
            match self.ioctl_raw(
                IOCTL_CLOSE_WRITER,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
            ) {
                Ok(_) => Ok(()),
                Err(ERROR_NOT_FOUND | ERROR_INVALID_HANDLE) => Ok(()),
                Err(code) => Err(VmicError::Io(format!(
                    "CLOSE_WRITER failed: {}",
                    win32_message(code)
                ))),
            }
        }

        pub fn map_ring(&self, info: &WriterInfo) -> Result<RingView, VmicError> {
            let section = Arc::new(map_named_section(
                &info.section_name,
                info.ring_frames,
                info.format.channels,
            )?);
            let view = section.ring_view();
            view.validate()?;
            let mut slot = self.mapping.lock().map_err(|_| {
                VmicError::Io("the virtual mic mapping lock is poisoned".to_string())
            })?;
            if slot.is_some() {
                return Err(VmicError::Layout(
                    "the virtual mic section is already mapped".to_string(),
                ));
            }
            // The view keeps `section` alive even if this client drops first.
            *slot = Some(section);
            Ok(view)
        }

        pub fn stats(&self) -> Result<DriverStats, VmicError> {
            let mut wire: StatsWire = StatsWire {
                consumer_active: 0,
                reserved: 0,
                underruns: 0,
                read_index: 0,
                qpc_frequency: 0,
            };
            let returned = self.ioctl(
                IOCTL_GET_STATS,
                std::ptr::null(),
                0,
                std::ptr::from_mut(&mut wire).cast(),
                size_of::<StatsWire>() as u32,
            )?;
            if (returned as usize) < size_of::<StatsWire>() {
                return Err(VmicError::Layout(format!(
                    "GET_STATS returned {returned} bytes, expected {}",
                    size_of::<StatsWire>()
                )));
            }
            Ok(DriverStats {
                consumer_active: wire.consumer_active != 0,
                underruns: wire.underruns,
                read_index: wire.read_index,
                qpc_frequency: wire.qpc_frequency,
            })
        }

        fn read_section_info(&self, requested: Format) -> Result<WriterInfo, VmicError> {
            let mut buffer = [0u8; SECTION_INFO_WIRE_BYTES + SECTION_NAME_MAX_BYTES];
            let returned = self.ioctl(
                IOCTL_GET_SECTION_INFO,
                std::ptr::null(),
                0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
            )? as usize;
            if returned < SECTION_INFO_WIRE_BYTES {
                return Err(VmicError::Layout(format!(
                    "GET_SECTION_INFO returned {returned} bytes, expected at least {SECTION_INFO_WIRE_BYTES}"
                )));
            }
            // SAFETY: `SectionInfoWire` is plain data and the call returned at
            // least its size; `read_unaligned` tolerates the buffer alignment.
            let info =
                unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<SectionInfoWire>()) };

            let name_bytes = info.name_bytes as usize;
            if name_bytes == 0 || name_bytes % 2 != 0 || name_bytes > SECTION_NAME_MAX_BYTES {
                return Err(VmicError::Layout(format!(
                    "the virtual mic section name length {name_bytes} is invalid"
                )));
            }
            if returned < SECTION_INFO_WIRE_BYTES + name_bytes {
                return Err(VmicError::Layout(format!(
                    "GET_SECTION_INFO returned {returned} bytes but the section name needs {}",
                    SECTION_INFO_WIRE_BYTES + name_bytes
                )));
            }

            if info.sample_rate != requested.sample_rate || info.channels != requested.channels {
                return Err(VmicError::Layout(format!(
                    "the virtual mic driver negotiated {} Hz / {} channels instead of {} Hz / {} channels",
                    info.sample_rate, info.channels, requested.sample_rate, requested.channels
                )));
            }
            if !(MIN_PACKET_FRAMES..=MAX_PACKET_FRAMES).contains(&info.packet_frames) {
                return Err(VmicError::Layout(format!(
                    "the virtual mic driver reports packet size {} outside {MIN_PACKET_FRAMES}..={MAX_PACKET_FRAMES}",
                    info.packet_frames
                )));
            }
            if !(RING_FRAMES_MIN..=RING_FRAMES_MAX).contains(&info.ring_frames) {
                return Err(VmicError::Layout(format!(
                    "the virtual mic driver reports ring frames {} outside {RING_FRAMES_MIN}..={RING_FRAMES_MAX}",
                    info.ring_frames
                )));
            }
            if u64::from(info.ring_frames) < 2 * u64::from(info.packet_frames) {
                return Err(VmicError::Layout(format!(
                    "the virtual mic ring of {} frames holds fewer than two {}-frame packets",
                    info.ring_frames, info.packet_frames
                )));
            }
            section_len(info.ring_frames, info.channels)?;

            let name = &buffer[SECTION_INFO_WIRE_BYTES..SECTION_INFO_WIRE_BYTES + name_bytes];
            let units: Vec<u16> = name
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            let end = units
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(units.len());
            let section_name = String::from_utf16(&units[..end]).map_err(|_| {
                VmicError::Layout("the virtual mic section name is not valid UTF-16".to_string())
            })?;
            if section_name.is_empty() {
                return Err(VmicError::Layout(
                    "the virtual mic driver returned an empty section name".to_string(),
                ));
            }

            Ok(WriterInfo {
                format: Format {
                    sample_rate: info.sample_rate,
                    channels: info.channels,
                    packet_frames: info.packet_frames,
                },
                ring_frames: info.ring_frames,
                section_name,
            })
        }

        fn ioctl_raw(
            &self,
            code: u32,
            input: *const c_void,
            input_len: u32,
            output: *mut c_void,
            output_len: u32,
        ) -> Result<u32, u32> {
            let mut returned = 0u32;
            // SAFETY: the caller supplies buffers valid for the advertised
            // lengths and `self.handle` stays open for the life of `self`.
            let ok = unsafe {
                DeviceIoControl(
                    self.handle,
                    code,
                    input,
                    input_len,
                    output,
                    output_len,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                Err(last_error())
            } else {
                Ok(returned)
            }
        }

        fn ioctl(
            &self,
            code: u32,
            input: *const c_void,
            input_len: u32,
            output: *mut c_void,
            output_len: u32,
        ) -> Result<u32, VmicError> {
            self.ioctl_raw(code, input, input_len, output, output_len)
                .map_err(|error| {
                    VmicError::Io(format!(
                        "DeviceIoControl(0x{code:08X}) failed: {}",
                        win32_message(error)
                    ))
                })
        }
    }

    impl Drop for VmicClient {
        fn drop(&mut self) {
            let _ = self.close_writer();
            // SAFETY: the handle came from `CreateFileW` and is closed once.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    fn map_named_section(
        name: &str,
        ring_frames: u32,
        channels: u32,
    ) -> Result<MappedSection, VmicError> {
        let len = section_len(ring_frames, channels)?;
        let wide = wide_string(name);
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        let mapping = unsafe { OpenFileMappingW(FILE_MAP_READ | FILE_MAP_WRITE, 0, wide.as_ptr()) };
        if mapping.is_null() {
            let code = last_error();
            return Err(match code {
                ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND | ERROR_INVALID_NAME => {
                    VmicError::Layout(format!(
                        "the virtual mic section {name:?} does not exist ({})",
                        win32_message(code)
                    ))
                }
                _ => VmicError::Io(format!(
                    "OpenFileMappingW({name}) failed: {}",
                    win32_message(code)
                )),
            });
        }
        // SAFETY: `mapping` is open and `len` fits the section size requested
        // by the driver.
        let address = unsafe { MapViewOfFile(mapping, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, len) };
        if address.Value.is_null() {
            let code = last_error();
            // SAFETY: release the section handle; nothing mapped it.
            unsafe { CloseHandle(mapping) };
            return Err(VmicError::Io(format!(
                "MapViewOfFile({name}) failed: {}",
                win32_message(code)
            )));
        }
        Ok(MappedSection {
            base: address.Value.cast(),
            len,
            mapping,
        })
    }

    fn section_len(ring_frames: u32, channels: u32) -> Result<usize, VmicError> {
        if ring_frames == 0 || channels == 0 {
            return Err(VmicError::Layout(
                "the virtual mic section reports zero ring frames or channels".to_string(),
            ));
        }
        let sample_bytes = (ring_frames as usize)
            .checked_mul(channels as usize)
            .and_then(|samples| samples.checked_mul(BYTES_PER_SAMPLE as usize))
            .ok_or_else(|| {
                VmicError::Layout("the virtual mic section size overflows".to_string())
            })?;
        let len = HEADER_SIZE.checked_add(sample_bytes).ok_or_else(|| {
            VmicError::Layout("the virtual mic section size overflows".to_string())
        })?;
        if len > isize::MAX as usize {
            return Err(VmicError::Layout(
                "the virtual mic section is too large to map".to_string(),
            ));
        }
        Ok(len)
    }

    fn wide_string(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error() -> u32 {
        // SAFETY: no arguments; reads the thread-local last-error value.
        unsafe { GetLastError() }
    }

    fn win32_message(code: u32) -> String {
        format!("Windows error {code}")
    }

    fn not_installed_message(code: u32) -> String {
        format!(
            "the MicHost virtual microphone driver (mic_host_vmic.sys) is not installed or not running ({}); install or start the virtual microphone driver, or choose a physical output",
            win32_message(code)
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicU32, Ordering};

        use crate::layout::MAGIC;
        use crate::writer::RingWriter;

        use windows_sys::Win32::System::Memory::{CreateFileMappingW, PAGE_READWRITE};

        static NEXT_NAME: AtomicU32 = AtomicU32::new(0);

        fn test_section_name() -> String {
            let id = NEXT_NAME.fetch_add(1, Ordering::Relaxed);
            format!("MicHostVmicTest-{}-{id}", std::process::id())
        }

        fn write_header_u32(base: *mut u8, offset: usize, value: u32) {
            // SAFETY: callers pass a live mapping that covers the 80-byte header.
            unsafe { base.add(offset).cast::<u32>().write_volatile(value) };
        }

        struct SectionFile {
            handle: HANDLE,
        }

        impl Drop for SectionFile {
            fn drop(&mut self) {
                // SAFETY: the handle came from CreateFileMappingW.
                unsafe { CloseHandle(self.handle) };
            }
        }

        #[test]
        fn wire_layout_matches_abi() {
            assert_eq!(size_of::<OpenWriterIn>(), 12);
            assert_eq!(std::mem::offset_of!(OpenWriterIn, sample_rate), 0);
            assert_eq!(std::mem::offset_of!(OpenWriterIn, channels), 4);
            assert_eq!(std::mem::offset_of!(OpenWriterIn, packet_frames), 8);

            assert_eq!(size_of::<SectionInfoWire>(), 24);
            assert_eq!(std::mem::offset_of!(SectionInfoWire, ring_frames), 0);
            assert_eq!(std::mem::offset_of!(SectionInfoWire, packet_frames), 4);
            assert_eq!(std::mem::offset_of!(SectionInfoWire, sample_rate), 8);
            assert_eq!(std::mem::offset_of!(SectionInfoWire, channels), 12);
            assert_eq!(std::mem::offset_of!(SectionInfoWire, name_bytes), 16);

            assert_eq!(size_of::<StatsWire>(), 32);
            assert_eq!(std::mem::offset_of!(StatsWire, consumer_active), 0);
            assert_eq!(std::mem::offset_of!(StatsWire, underruns), 8);
            assert_eq!(std::mem::offset_of!(StatsWire, read_index), 16);
            assert_eq!(std::mem::offset_of!(StatsWire, qpc_frequency), 24);
        }

        #[test]
        fn named_section_round_trip() {
            let ring_frames = 1024u32;
            let channels = 2u32;
            let packet_frames = 480u32;
            let len = section_len(ring_frames, channels).unwrap();
            let name = test_section_name();
            let wide = wide_string(&name);
            // SAFETY: pagefile-backed section with a NUL-terminated name.
            let file = unsafe {
                CreateFileMappingW(
                    INVALID_HANDLE_VALUE,
                    std::ptr::null(),
                    PAGE_READWRITE,
                    0,
                    len as u32,
                    wide.as_ptr(),
                )
            };
            if file.is_null() {
                eprintln!(
                    "skipping virtual mic section round trip: CreateFileMappingW failed ({})",
                    win32_message(last_error())
                );
                return;
            }
            let _file = SectionFile { handle: file };

            let section = match map_named_section(&name, ring_frames, channels) {
                Ok(section) => section,
                Err(error) => {
                    eprintln!("skipping virtual mic section round trip: {error}");
                    return;
                }
            };
            write_header_u32(section.base, 0, MAGIC);
            write_header_u32(section.base, 4, VERSION);
            write_header_u32(section.base, 8, 48_000);
            write_header_u32(section.base, 12, channels);
            write_header_u32(section.base, 16, BYTES_PER_SAMPLE);
            write_header_u32(section.base, 20, packet_frames);
            write_header_u32(section.base, 24, ring_frames);
            write_header_u32(section.base, 28, 1);

            let section = std::sync::Arc::new(section);
            let mut writer = RingWriter::new(section.ring_view()).unwrap();
            let block = [1.0f32, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0];
            assert_eq!(writer.write_frames(&block).written, 4);
            assert_eq!(writer.stats().write_index, 4);

            let second =
                std::sync::Arc::new(map_named_section(&name, ring_frames, channels).unwrap());
            let view = second.ring_view();
            view.validate().unwrap();
            assert_eq!(view.write_index(), 4);
            // SAFETY: `view` was validated and covers the whole ring.
            let samples = unsafe {
                std::slice::from_raw_parts(view.sample_ptr(0, channels as usize), block.len())
            };
            assert_eq!(samples, block.as_slice());
        }

        #[test]
        fn missing_driver_reports_not_installed() {
            if VmicClient::is_installed() {
                return;
            }
            match VmicClient::open() {
                Err(VmicError::NotInstalled(message)) => {
                    assert!(message.contains("mic_host_vmic.sys"));
                }
                Err(error) => panic!("expected NotInstalled, got {error}"),
                Ok(_) => panic!("expected NotInstalled but the driver opened"),
            }
        }
    }
}

#[cfg(not(windows))]
mod stub_impl {
    use super::{CONTROL_DEVICE, DriverStats, Format, RingView, VmicError, WriterInfo};

    pub struct VmicClient {
        _private: (),
    }

    impl VmicClient {
        pub fn open() -> Result<Self, VmicError> {
            let _ = CONTROL_DEVICE;
            Err(VmicError::Unsupported(
                "virtual microphones require Windows".to_string(),
            ))
        }

        pub fn is_installed() -> bool {
            false
        }

        pub fn driver_version(&self) -> Result<u32, VmicError> {
            Err(VmicError::Unsupported(
                "virtual microphones require Windows".to_string(),
            ))
        }

        pub fn open_writer(&self, format: Format) -> Result<WriterInfo, VmicError> {
            let _ = format;
            Err(VmicError::Unsupported(
                "virtual microphones require Windows".to_string(),
            ))
        }

        pub fn close_writer(&self) -> Result<(), VmicError> {
            Ok(())
        }

        pub fn map_ring(&self, info: &WriterInfo) -> Result<RingView, VmicError> {
            let _ = info;
            Err(VmicError::Unsupported(
                "virtual microphones require Windows".to_string(),
            ))
        }

        pub fn stats(&self) -> Result<DriverStats, VmicError> {
            Err(VmicError::Unsupported(
                "virtual microphones require Windows".to_string(),
            ))
        }
    }
}
