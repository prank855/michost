use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, CString, c_char, c_void};
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use libloading::Library;
use michost_host_abi::{
    ABI_VERSION, BackendError, BackendResult, DeviceInfo, DeviceKind, DeviceLatency, DeviceStream,
    HostBackend, MH_ABI_MISMATCH, MH_INVALID_ARGUMENT, MH_LAYOUT_CALLBACKS, MH_LAYOUT_DEVICE_INFO,
    MH_LAYOUT_LATENCY, MH_LAYOUT_PARAM_INFO, MH_LAYOUT_PLUGIN_DESCRIPTOR,
    MH_LAYOUT_STREAM_CALLBACKS, MH_LAYOUT_STREAM_CONFIG, MH_NOT_FOUND, MH_OK, MhCallbacks,
    MhDeviceChangedCallback, MhDeviceInfo, MhLatency, MhParamInfo, MhPluginDescriptor,
    MhScanProgress, MhStreamCallbacks, MhStreamConfig, MhStreamErrorCallback,
    MhStreamInputCallback, MhStreamOutputCallback, ParamInfo, PluginChannels, PluginDescriptor,
    PluginHandle, PluginHost, StateKind, StreamCallbacks, StreamConfig, StreamHost, UiEvent,
};

const MAX_DEVICES: usize = 256;
const SCAN_LIMIT: usize = 4096;
const DESCRIBE_LIMIT: usize = 1024;
const MAX_PROCESS_CHANNELS: usize = 32;

type FnAbiVersion = unsafe extern "C" fn() -> u32;
type FnLayoutSize = unsafe extern "C" fn(kind: i32) -> u32;
type FnInit =
    unsafe extern "C" fn(callbacks: *const MhCallbacks, out_manager: *mut *mut c_void) -> i32;
type FnSetCallbacks =
    unsafe extern "C" fn(manager: *mut c_void, callbacks: *const MhCallbacks) -> i32;
type FnShutdown = unsafe extern "C" fn(manager: *mut c_void);
type FnSetDeviceChangeCallback = unsafe extern "C" fn(
    manager: *mut c_void,
    callback: Option<MhDeviceChangedCallback>,
    ctx: *mut c_void,
) -> i32;
type FnDeviceEnumerate = unsafe extern "C" fn(
    manager: *mut c_void,
    kind: i32,
    out: *mut MhDeviceInfo,
    max: i32,
    count: *mut i32,
) -> i32;
type FnStreamOpen = unsafe extern "C" fn(
    manager: *mut c_void,
    config: *const MhStreamConfig,
    callbacks: *const MhStreamCallbacks,
    out_stream: *mut *mut c_void,
) -> i32;
type FnStreamHandle = unsafe extern "C" fn(stream: *mut c_void) -> i32;
type FnStreamLatency = unsafe extern "C" fn(stream: *mut c_void, out: *mut MhLatency) -> i32;
type FnScanPaths = unsafe extern "C" fn(
    manager: *mut c_void,
    extra_paths: *const c_char,
    progress: MhScanProgress,
    progress_ctx: *mut c_void,
    out: *mut MhPluginDescriptor,
    max: i32,
    count: *mut i32,
) -> i32;
type FnPluginDescribe = unsafe extern "C" fn(
    manager: *mut c_void,
    path: *const c_char,
    out: *mut MhPluginDescriptor,
    max: i32,
    count: *mut i32,
) -> i32;
type FnPluginLoad = unsafe extern "C" fn(
    manager: *mut c_void,
    path: *const c_char,
    class_id: *const c_char,
    out_handle: *mut *mut c_void,
) -> i32;
type FnPluginLoadNamed = unsafe extern "C" fn(
    manager: *mut c_void,
    path: *const c_char,
    class_id: *const c_char,
    name: *const c_char,
    out_handle: *mut *mut c_void,
) -> i32;
type FnPluginFree = unsafe extern "C" fn(manager: *mut c_void, handle: *mut c_void) -> i32;
type FnPluginChannels = unsafe extern "C" fn(
    handle: *mut c_void,
    main_inputs: *mut i32,
    main_outputs: *mut i32,
    sidechain_inputs: *mut i32,
) -> i32;
type FnPluginPrepare = unsafe extern "C" fn(
    manager: *mut c_void,
    handle: *mut c_void,
    sample_rate: f64,
    max_block: i32,
    main_inputs: i32,
    main_outputs: i32,
    sidechain_channels: i32,
) -> i32;
type FnPluginActivate =
    unsafe extern "C" fn(manager: *mut c_void, handle: *mut c_void, active: i32) -> i32;
type FnPluginProcess = unsafe extern "C" fn(
    handle: *mut c_void,
    channels: *const *mut f32,
    num_channels: i32,
    frames: i32,
) -> i32;
type FnPluginLatency = unsafe extern "C" fn(handle: *mut c_void, out_samples: *mut i32) -> i32;
type FnPluginParamCount = unsafe extern "C" fn(handle: *mut c_void, out_count: *mut i32) -> i32;
type FnPluginParamInfo =
    unsafe extern "C" fn(handle: *mut c_void, index: i32, out: *mut MhParamInfo) -> i32;
type FnPluginParamSet = unsafe extern "C" fn(handle: *mut c_void, id: u32, normalized: f64) -> i32;
type FnPluginParamGet =
    unsafe extern "C" fn(handle: *mut c_void, id: u32, out_value: *mut f64) -> i32;
type FnPluginStateSize =
    unsafe extern "C" fn(handle: *mut c_void, kind: i32, out_size: *mut i32) -> i32;
type FnPluginStateExport = unsafe extern "C" fn(
    handle: *mut c_void,
    kind: i32,
    out: *mut u8,
    capacity: i32,
    written: *mut i32,
) -> i32;
type FnPluginStateImport =
    unsafe extern "C" fn(handle: *mut c_void, kind: i32, data: *const u8, len: i32) -> i32;
type FnPluginTakeEdited = unsafe extern "C" fn(handle: *mut c_void) -> i32;
type FnPluginEditorOpen = unsafe extern "C" fn(manager: *mut c_void, handle: *mut c_void) -> i32;
type FnPluginEditorClose = unsafe extern "C" fn(manager: *mut c_void, handle: *mut c_void) -> i32;
type FnPluginEditorIsOpen = unsafe extern "C" fn(handle: *mut c_void, out_open: *mut i32) -> i32;

macro_rules! load_export {
    ($library:expr, $name:literal, $ty:ty) => {{
        let symbol =
            unsafe { $library.get::<$ty>(concat!($name, "\0").as_bytes()) }.map_err(|error| {
                BackendError::failed(format!("missing mic_host export {}: {error}", $name))
            })?;
        *symbol
    }};
}

struct MhApi {
    abi_version: FnAbiVersion,
    layout_size: FnLayoutSize,
    init: FnInit,
    set_callbacks: FnSetCallbacks,
    set_device_change_callback: Option<FnSetDeviceChangeCallback>,
    shutdown: FnShutdown,
    device_enumerate: FnDeviceEnumerate,
    stream_open: FnStreamOpen,
    stream_start: FnStreamHandle,
    stream_stop: FnStreamHandle,
    stream_close: FnStreamHandle,
    stream_latency: FnStreamLatency,
    stream_control_panel: FnStreamHandle,
    scan_paths: FnScanPaths,
    plugin_describe: FnPluginDescribe,
    plugin_load: FnPluginLoad,
    plugin_load_named: Option<FnPluginLoadNamed>,
    plugin_free: FnPluginFree,
    plugin_channels: FnPluginChannels,
    plugin_prepare: FnPluginPrepare,
    plugin_activate: FnPluginActivate,
    plugin_process: FnPluginProcess,
    plugin_latency: FnPluginLatency,
    plugin_param_count: FnPluginParamCount,
    plugin_param_info: FnPluginParamInfo,
    plugin_param_set: FnPluginParamSet,
    plugin_param_get: FnPluginParamGet,
    plugin_state_size: FnPluginStateSize,
    plugin_state_export: FnPluginStateExport,
    plugin_state_import: FnPluginStateImport,
    plugin_take_edited: FnPluginTakeEdited,
    plugin_editor_open: FnPluginEditorOpen,
    plugin_editor_close: FnPluginEditorClose,
    plugin_editor_is_open: FnPluginEditorIsOpen,
}

impl MhApi {
    fn load(library: &Library) -> BackendResult<Self> {
        Ok(Self {
            abi_version: load_export!(library, "mh_abi_version", FnAbiVersion),
            layout_size: load_export!(library, "mh_layout_size", FnLayoutSize),
            init: load_export!(library, "mh_init", FnInit),
            set_callbacks: load_export!(library, "mh_set_callbacks", FnSetCallbacks),
            set_device_change_callback: unsafe {
                library
                    .get::<FnSetDeviceChangeCallback>(b"mh_set_device_change_callback\0")
                    .ok()
                    .map(|symbol| *symbol)
            },
            shutdown: load_export!(library, "mh_shutdown", FnShutdown),
            device_enumerate: load_export!(library, "mh_device_enumerate", FnDeviceEnumerate),
            stream_open: load_export!(library, "mh_stream_open", FnStreamOpen),
            stream_start: load_export!(library, "mh_stream_start", FnStreamHandle),
            stream_stop: load_export!(library, "mh_stream_stop", FnStreamHandle),
            stream_close: load_export!(library, "mh_stream_close", FnStreamHandle),
            stream_latency: load_export!(library, "mh_stream_latency", FnStreamLatency),
            stream_control_panel: load_export!(library, "mh_stream_control_panel", FnStreamHandle),
            scan_paths: load_export!(library, "mh_scan_paths", FnScanPaths),
            plugin_describe: load_export!(library, "mh_plugin_describe", FnPluginDescribe),
            plugin_load: load_export!(library, "mh_plugin_load", FnPluginLoad),
            plugin_load_named: unsafe {
                library
                    .get::<FnPluginLoadNamed>(b"mh_plugin_load_named ")
                    .ok()
                    .map(|symbol| *symbol)
            },
            plugin_free: load_export!(library, "mh_plugin_free", FnPluginFree),
            plugin_channels: load_export!(library, "mh_plugin_channels", FnPluginChannels),
            plugin_prepare: load_export!(library, "mh_plugin_prepare", FnPluginPrepare),
            plugin_activate: load_export!(library, "mh_plugin_activate", FnPluginActivate),
            plugin_process: load_export!(library, "mh_plugin_process", FnPluginProcess),
            plugin_latency: load_export!(library, "mh_plugin_latency", FnPluginLatency),
            plugin_param_count: load_export!(library, "mh_plugin_param_count", FnPluginParamCount),
            plugin_param_info: load_export!(library, "mh_plugin_param_info", FnPluginParamInfo),
            plugin_param_set: load_export!(library, "mh_plugin_param_set", FnPluginParamSet),
            plugin_param_get: load_export!(library, "mh_plugin_param_get", FnPluginParamGet),
            plugin_state_size: load_export!(library, "mh_plugin_state_size", FnPluginStateSize),
            plugin_state_export: load_export!(
                library,
                "mh_plugin_state_export",
                FnPluginStateExport
            ),
            plugin_state_import: load_export!(
                library,
                "mh_plugin_state_import",
                FnPluginStateImport
            ),
            plugin_take_edited: load_export!(library, "mh_plugin_take_edited", FnPluginTakeEdited),
            plugin_editor_open: load_export!(library, "mh_plugin_editor_open", FnPluginEditorOpen),
            plugin_editor_close: load_export!(
                library,
                "mh_plugin_editor_close",
                FnPluginEditorClose
            ),
            plugin_editor_is_open: load_export!(
                library,
                "mh_plugin_editor_is_open",
                FnPluginEditorIsOpen
            ),
        })
    }
}

struct ManagerPtr(*mut c_void);

unsafe impl Send for ManagerPtr {}
unsafe impl Sync for ManagerPtr {}

impl ManagerPtr {
    fn get(&self) -> *mut c_void {
        self.0
    }
}

#[derive(Default)]
struct CallbackState {
    events: Mutex<VecDeque<UiEvent>>,
    last_error: Mutex<Option<(i32, String)>>,
}

struct StreamCtx {
    callbacks: StreamCallbacks,
}

unsafe impl Send for StreamCtx {}

pub struct JuceBackend {
    api: MhApi,
    manager: ManagerPtr,
    callback_state: Arc<CallbackState>,
    device_changes: Arc<AtomicBool>,
    callbacks: Box<MhCallbacks>,
    streams: Mutex<HashMap<usize, Box<StreamCtx>>>,
    _library: Library,
}

unsafe impl Send for JuceBackend {}
unsafe impl Sync for JuceBackend {}

impl JuceBackend {
    pub fn load() -> BackendResult<Self> {
        let candidates = dll_candidates();
        if candidates.is_empty() {
            return Err(BackendError::new(
                MH_NOT_FOUND,
                "mic_host.dll not found; set MIC_HOST_DLL or build the C++ host",
            ));
        }

        let mut last_error = None;
        for candidate in candidates {
            match Self::from_path(&candidate) {
                Ok(backend) => return Ok(backend),
                Err(error) => last_error = Some(error),
            }
        }

        Err(last_error
            .unwrap_or_else(|| BackendError::new(MH_NOT_FOUND, "mic_host.dll could not be loaded")))
    }

    pub fn from_path(path: impl AsRef<Path>) -> BackendResult<Self> {
        let path = path.as_ref();
        let library = unsafe { Library::new(path) }.map_err(|error| {
            BackendError::failed(format!("failed to load {}: {error}", path.display()))
        })?;

        let api = MhApi::load(&library)?;

        let reported_version = unsafe { (api.abi_version)() };
        if reported_version != ABI_VERSION {
            return Err(BackendError::new(
                MH_ABI_MISMATCH,
                format!("mic_host ABI version {reported_version}, expected {ABI_VERSION}"),
            ));
        }

        let layouts = [
            (
                MH_LAYOUT_DEVICE_INFO,
                size_of::<MhDeviceInfo>(),
                "MHDeviceInfo",
            ),
            (
                MH_LAYOUT_STREAM_CONFIG,
                size_of::<MhStreamConfig>(),
                "MHStreamConfig",
            ),
            (
                MH_LAYOUT_STREAM_CALLBACKS,
                size_of::<MhStreamCallbacks>(),
                "MHStreamCallbacks",
            ),
            (MH_LAYOUT_LATENCY, size_of::<MhLatency>(), "MHLatency"),
            (
                MH_LAYOUT_PLUGIN_DESCRIPTOR,
                size_of::<MhPluginDescriptor>(),
                "MHPluginDescriptor",
            ),
            (
                MH_LAYOUT_PARAM_INFO,
                size_of::<MhParamInfo>(),
                "MHParamInfo",
            ),
            (MH_LAYOUT_CALLBACKS, size_of::<MhCallbacks>(), "MHCallbacks"),
        ];

        for (kind, expected, name) in layouts {
            let actual = unsafe { (api.layout_size)(kind) } as usize;
            if actual != expected {
                return Err(BackendError::new(
                    MH_ABI_MISMATCH,
                    format!("layout mismatch for {name}: dll {actual}, rust {expected}"),
                ));
            }
        }

        let callback_state = Arc::new(CallbackState::default());
        let callbacks = Box::new(MhCallbacks {
            abi_version: ABI_VERSION,
            size: size_of::<MhCallbacks>() as u32,
            ui_ctx: Arc::as_ptr(&callback_state) as *mut c_void,
            on_error: Some(backend_error_callback),
            on_latency_changed: Some(backend_latency_callback),
        });

        let mut manager: *mut c_void = null_mut();
        let status = unsafe { (api.init)(&*callbacks, &mut manager) };
        if status != MH_OK {
            return Err(BackendError::new(
                status,
                format!("mh_init failed with status {status}"),
            ));
        }
        if manager.is_null() {
            return Err(BackendError::failed("mh_init returned a null manager"));
        }

        let device_changes = Arc::new(AtomicBool::new(false));
        if let Some(set_device_change_callback) = api.set_device_change_callback {
            let ctx = Arc::as_ptr(&device_changes) as *mut AtomicBool as *mut c_void;
            let status =
                unsafe { set_device_change_callback(manager, Some(device_change_trampoline), ctx) };
            if status != MH_OK {
                unsafe { (api.shutdown)(manager) };
                return Err(BackendError::new(
                    status,
                    format!("mh_set_device_change_callback failed with status {status}"),
                ));
            }
        }

        Ok(Self {
            api,
            manager: ManagerPtr(manager),
            callback_state,
            device_changes,
            callbacks,
            streams: Mutex::new(HashMap::new()),
            _library: library,
        })
    }

    pub fn is_available() -> bool {
        Self::load().is_ok()
    }

    pub fn set_callbacks(&self) -> BackendResult<()> {
        let status = unsafe { (self.api.set_callbacks)(self.manager.get(), &*self.callbacks) };
        if status == MH_OK {
            Ok(())
        } else {
            Err(self.backend_error(status, "mh_set_callbacks"))
        }
    }

    pub fn poll_events(&self) -> Vec<UiEvent> {
        let mut events = self
            .callback_state
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        events.drain(..).collect()
    }

    pub fn take_device_changes(&self) -> bool {
        self.device_changes.swap(false, Ordering::AcqRel)
    }

    pub fn has_device_changes(&self) -> bool {
        self.device_changes.load(Ordering::Acquire)
    }

    pub fn device_change_notifications_supported(&self) -> bool {
        self.api.set_device_change_callback.is_some()
    }

    pub fn abi_version(&self) -> BackendResult<u32> {
        Ok(unsafe { (self.api.abi_version)() })
    }

    pub fn param_count(&self, handle: PluginHandle) -> BackendResult<u32> {
        let mut count = 0i32;
        let status = unsafe { (self.api.plugin_param_count)(handle.as_ptr(), &mut count) };
        if status != MH_OK {
            return Err(self.backend_error(status, "mh_plugin_param_count"));
        }
        if count < 0 {
            return Err(BackendError::failed(
                "mh_plugin_param_count returned a negative count",
            ));
        }
        Ok(count as u32)
    }

    pub fn param_info(&self, handle: PluginHandle, index: u32) -> BackendResult<ParamInfo> {
        let index = to_i32("parameter index", index)?;
        let mut info = MhParamInfo::default();
        let status = unsafe { (self.api.plugin_param_info)(handle.as_ptr(), index, &mut info) };
        if status != MH_OK {
            return Err(self.backend_error(status, "mh_plugin_param_info"));
        }
        Ok(ParamInfo::from(&info))
    }

    pub fn param_set(&self, handle: PluginHandle, id: u32, normalized: f64) -> BackendResult<()> {
        let status = unsafe { (self.api.plugin_param_set)(handle.as_ptr(), id, normalized) };
        if status == MH_OK {
            Ok(())
        } else {
            Err(self.backend_error(status, "mh_plugin_param_set"))
        }
    }

    pub fn param_get(&self, handle: PluginHandle, id: u32) -> BackendResult<f64> {
        let mut value = 0.0f64;
        let status = unsafe { (self.api.plugin_param_get)(handle.as_ptr(), id, &mut value) };
        if status != MH_OK {
            return Err(self.backend_error(status, "mh_plugin_param_get"));
        }
        Ok(value)
    }

    fn backend_error(&self, status: i32, operation: &str) -> BackendError {
        let detail = self
            .callback_state
            .last_error
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .filter(|(code, _)| *code == status)
            .map(|(_, message)| message);

        match detail {
            Some(message) if !message.is_empty() => {
                BackendError::new(status, format!("{operation} failed: {message}"))
            }
            _ => BackendError::new(status, format!("{operation} failed with status {status}")),
        }
    }

    fn record_error(&self, status: i32, operation: &str) {
        let error = self.backend_error(status, operation);
        if let Ok(mut events) = self.callback_state.events.lock() {
            events.push_back(UiEvent::DeviceError {
                status: error.status,
                message: error.message,
            });
        }
    }

    fn scan_impl(
        &self,
        extra_paths: *const c_char,
        mut progress: MhScanProgress,
        progress_ctx: *mut c_void,
    ) -> BackendResult<Vec<PluginDescriptor>> {
        let mut max = 64usize;
        loop {
            let mut buffer = vec![MhPluginDescriptor::default(); max];
            let mut count = 0i32;
            let status = unsafe {
                (self.api.scan_paths)(
                    self.manager.get(),
                    extra_paths,
                    progress,
                    progress_ctx,
                    buffer.as_mut_ptr(),
                    max as i32,
                    &mut count,
                )
            };
            if status != MH_OK {
                return Err(self.backend_error(status, "mh_scan"));
            }
            if count < 0 {
                return Err(BackendError::failed("mh_scan returned a negative count"));
            }

            // count is the true total; the native side writes at most max items.
            let total = count as usize;
            if total <= max || max >= SCAN_LIMIT {
                let len = total.min(max).min(SCAN_LIMIT);
                return Ok(buffer[..len].iter().map(PluginDescriptor::from).collect());
            }

            max = (max * 2).min(SCAN_LIMIT);
            progress = None;
        }
    }
}

impl Drop for JuceBackend {
    fn drop(&mut self) {
        let manager = self.manager.get();
        if !manager.is_null() {
            if let Some(set_device_change_callback) = self.api.set_device_change_callback {
                unsafe { set_device_change_callback(manager, None, null_mut()) };
            }
            unsafe { (self.api.shutdown)(manager) };
            self.manager.0 = null_mut();
        }

        self.streams
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

impl StreamHost for JuceBackend {
    fn enumerate_devices(&self, kind: DeviceKind) -> BackendResult<Vec<DeviceInfo>> {
        let mut max = 64usize;
        loop {
            let mut buffer = vec![MhDeviceInfo::default(); max];
            let mut count = 0i32;
            let status = unsafe {
                (self.api.device_enumerate)(
                    self.manager.get(),
                    kind.as_i32(),
                    buffer.as_mut_ptr(),
                    max as i32,
                    &mut count,
                )
            };
            if status != MH_OK {
                return Err(self.backend_error(status, "mh_device_enumerate"));
            }
            if count < 0 {
                return Err(BackendError::failed(
                    "mh_device_enumerate returned a negative count",
                ));
            }

            // count is the true total; the native side writes at most max items.
            let total = count as usize;
            if total <= max || max >= MAX_DEVICES {
                let len = total.min(max).min(MAX_DEVICES);
                return Ok(buffer[..len].iter().map(DeviceInfo::from).collect());
            }

            max = (max * 2).min(MAX_DEVICES);
        }
    }

    fn open_stream(
        &self,
        config: &StreamConfig,
        callbacks: StreamCallbacks,
    ) -> BackendResult<DeviceStream> {
        let ffi_config = MhStreamConfig::from(config);
        let mut context = Box::new(StreamCtx { callbacks });
        let ffi_callbacks = MhStreamCallbacks {
            size: size_of::<MhStreamCallbacks>() as u32,
            pad: 0,
            ctx: (&mut *context as *mut StreamCtx).cast::<c_void>(),
            on_input: callbacks
                .on_input
                .map(|_| stream_input_trampoline as MhStreamInputCallback),
            on_output: callbacks
                .on_output
                .map(|_| stream_output_trampoline as MhStreamOutputCallback),
            on_error: callbacks
                .on_error
                .map(|_| stream_error_trampoline as MhStreamErrorCallback),
        };

        let mut handle: *mut c_void = null_mut();
        let status = unsafe {
            (self.api.stream_open)(self.manager.get(), &ffi_config, &ffi_callbacks, &mut handle)
        };
        if status != MH_OK {
            return Err(self.backend_error(status, "mh_stream_open"));
        }
        if handle.is_null() {
            return Err(BackendError::failed(
                "mh_stream_open returned a null stream",
            ));
        }

        self.streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(handle as usize, context);

        Ok(DeviceStream::from_raw(handle))
    }

    fn start_stream(&self, stream: &DeviceStream) -> BackendResult<()> {
        let status = unsafe { (self.api.stream_start)(stream.as_ptr()) };
        if status == MH_OK {
            Ok(())
        } else {
            Err(self.backend_error(status, "mh_stream_start"))
        }
    }

    fn stop_stream(&self, stream: &DeviceStream) -> BackendResult<()> {
        let status = unsafe { (self.api.stream_stop)(stream.as_ptr()) };
        if status == MH_OK {
            Ok(())
        } else {
            Err(self.backend_error(status, "mh_stream_stop"))
        }
    }

    fn close_stream(&self, stream: DeviceStream) {
        if stream.is_null() {
            return;
        }

        let status = unsafe { (self.api.stream_close)(stream.as_ptr()) };
        if status == MH_OK {
            let removed = self
                .streams
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&(stream.as_ptr() as usize));
            drop(removed);
        } else {
            // The native stream stays registered and may still use the context.
            self.record_error(status, "mh_stream_close");
        }
    }

    fn stream_latency(&self, stream: &DeviceStream) -> BackendResult<DeviceLatency> {
        let mut latency = MhLatency::default();
        let status = unsafe { (self.api.stream_latency)(stream.as_ptr(), &mut latency) };
        if status != MH_OK {
            return Err(self.backend_error(status, "mh_stream_latency"));
        }
        Ok(DeviceLatency::from(&latency))
    }

    fn stream_control_panel(&self, stream: &DeviceStream) -> BackendResult<()> {
        let status = unsafe { (self.api.stream_control_panel)(stream.as_ptr()) };
        if status == MH_OK {
            Ok(())
        } else {
            Err(self.backend_error(status, "mh_stream_control_panel"))
        }
    }
}

impl PluginHost for JuceBackend {
    fn scan_with_paths(
        &self,
        extra_paths: &str,
        progress: &mut dyn FnMut(&PluginDescriptor),
    ) -> BackendResult<Vec<PluginDescriptor>> {
        let extra_paths = c_string(extra_paths, "VST3 scan paths")?;
        let mut progress_ref = progress;
        let progress_ctx =
            (&mut progress_ref as *mut &mut dyn FnMut(&PluginDescriptor)).cast::<c_void>();
        self.scan_impl(
            extra_paths.as_ptr(),
            Some(scan_progress_trampoline),
            progress_ctx,
        )
    }

    fn plugin_descriptions(&self, path: &str) -> BackendResult<Vec<PluginDescriptor>> {
        let path = c_string(path, "plugin path")?;
        let mut max = 16usize;
        loop {
            let mut buffer = vec![MhPluginDescriptor::default(); max];
            let mut count = 0i32;
            let status = unsafe {
                (self.api.plugin_describe)(
                    self.manager.get(),
                    path.as_ptr(),
                    buffer.as_mut_ptr(),
                    max as i32,
                    &mut count,
                )
            };
            if status != MH_OK {
                return Err(self.backend_error(status, "mh_plugin_describe"));
            }
            if count < 0 {
                return Err(BackendError::failed(
                    "mh_plugin_describe returned a negative count",
                ));
            }

            // count is the true total; the native side writes at most max items.
            let total = count as usize;
            if total <= max || max >= DESCRIBE_LIMIT {
                let len = total.min(max).min(DESCRIBE_LIMIT);
                return Ok(buffer[..len].iter().map(PluginDescriptor::from).collect());
            }

            max = (max * 2).min(DESCRIBE_LIMIT);
        }
    }

    fn load_plugin(&self, path: &str, class_id: &str) -> BackendResult<PluginHandle> {
        let path = c_string(path, "plugin path")?;
        let class_id = c_string(class_id, "plugin class id")?;
        let mut handle: *mut c_void = null_mut();
        let status = unsafe {
            (self.api.plugin_load)(
                self.manager.get(),
                path.as_ptr(),
                class_id.as_ptr(),
                &mut handle,
            )
        };
        if status != MH_OK {
            return Err(self.backend_error(status, "mh_plugin_load"));
        }
        if handle.is_null() {
            return Err(BackendError::failed(
                "mh_plugin_load returned a null handle",
            ));
        }
        Ok(PluginHandle::from_raw(handle))
    }

    fn load_plugin_named(
        &self,
        path: &str,
        class_id: &str,
        name: &str,
    ) -> BackendResult<PluginHandle> {
        let Some(load_named) = self.api.plugin_load_named else {
            return self.load_plugin(path, class_id);
        };
        let path = c_string(path, "plugin path")?;
        let class_id = c_string(class_id, "plugin class id")?;
        let name = c_string(name, "plugin name")?;
        let mut handle: *mut c_void = null_mut();
        let status = unsafe {
            load_named(
                self.manager.get(),
                path.as_ptr(),
                class_id.as_ptr(),
                name.as_ptr(),
                &mut handle,
            )
        };
        if status != MH_OK {
            return Err(self.backend_error(status, "mh_plugin_load_named"));
        }
        if handle.is_null() {
            return Err(BackendError::failed(
                "mh_plugin_load_named returned a null handle",
            ));
        }
        Ok(PluginHandle::from_raw(handle))
    }

    fn free_plugin(&self, handle: PluginHandle) -> BackendResult<()> {
        let status = unsafe { (self.api.plugin_free)(self.manager.get(), handle.as_ptr()) };
        if status == MH_OK {
            Ok(())
        } else {
            Err(self.backend_error(status, "mh_plugin_free"))
        }
    }

    fn plugin_channels(&self, handle: PluginHandle) -> BackendResult<PluginChannels> {
        let mut main_inputs = 0i32;
        let mut main_outputs = 0i32;
        let mut sidechain_inputs = 0i32;
        let status = unsafe {
            (self.api.plugin_channels)(
                handle.as_ptr(),
                &mut main_inputs,
                &mut main_outputs,
                &mut sidechain_inputs,
            )
        };
        if status != MH_OK {
            return Err(self.backend_error(status, "mh_plugin_channels"));
        }

        Ok(PluginChannels {
            main_inputs: channel_count("main input count", main_inputs)?,
            main_outputs: channel_count("main output count", main_outputs)?,
            sidechain_inputs: channel_count("sidechain input count", sidechain_inputs)?,
        })
    }

    fn prepare_plugin(
        &self,
        handle: PluginHandle,
        sample_rate: f64,
        max_block: u32,
        main_inputs: u32,
        main_outputs: u32,
        sidechain_channels: u32,
    ) -> BackendResult<()> {
        let max_block = to_i32("max block size", max_block)?;
        let main_inputs = to_i32("main input count", main_inputs)?;
        let main_outputs = to_i32("main output count", main_outputs)?;
        let sidechain_channels = to_i32("sidechain channel count", sidechain_channels)?;
        let status = unsafe {
            (self.api.plugin_prepare)(
                self.manager.get(),
                handle.as_ptr(),
                sample_rate,
                max_block,
                main_inputs,
                main_outputs,
                sidechain_channels,
            )
        };
        if status == MH_OK {
            Ok(())
        } else {
            Err(self.backend_error(status, "mh_plugin_prepare"))
        }
    }

    fn activate_plugin(&self, handle: PluginHandle, active: bool) -> BackendResult<()> {
        let status = unsafe {
            (self.api.plugin_activate)(self.manager.get(), handle.as_ptr(), i32::from(active))
        };
        if status == MH_OK {
            Ok(())
        } else {
            Err(self.backend_error(status, "mh_plugin_activate"))
        }
    }

    fn process_plugin(
        &self,
        handle: PluginHandle,
        channels: &mut [&mut [f32]],
        frames: usize,
    ) -> BackendResult<()> {
        if channels.len() > MAX_PROCESS_CHANNELS {
            return Err(BackendError::invalid(format!(
                "channel count {} exceeds maximum {MAX_PROCESS_CHANNELS}",
                channels.len()
            )));
        }
        if frames > michost_graph::MAX_FRAMES {
            return Err(BackendError::invalid(format!(
                "frame count {frames} exceeds maximum {}",
                michost_graph::MAX_FRAMES
            )));
        }
        if channels.iter().any(|channel| channel.len() < frames) {
            return Err(BackendError::invalid(
                "a channel is shorter than the requested frame count",
            ));
        }
        let frames = to_i32("frame count", u32::try_from(frames).unwrap_or(u32::MAX))?;

        let mut pointers = [null_mut::<f32>(); MAX_PROCESS_CHANNELS];
        for (index, channel) in channels.iter_mut().enumerate() {
            pointers[index] = channel.as_mut_ptr();
        }

        let status = unsafe {
            (self.api.plugin_process)(
                handle.as_ptr(),
                pointers.as_ptr(),
                channels.len() as i32,
                frames,
            )
        };
        if status == MH_OK {
            Ok(())
        } else {
            // Graph-thread path: no locking or message formatting here.
            Err(BackendError::new(status, ""))
        }
    }

    fn plugin_latency(&self, handle: PluginHandle) -> BackendResult<u32> {
        let mut samples = 0i32;
        let status = unsafe { (self.api.plugin_latency)(handle.as_ptr(), &mut samples) };
        if status != MH_OK {
            return Err(self.backend_error(status, "mh_plugin_latency"));
        }
        if samples < 0 {
            return Err(BackendError::failed(
                "mh_plugin_latency returned a negative latency",
            ));
        }
        Ok(samples as u32)
    }

    fn state_export(&self, handle: PluginHandle, kind: StateKind) -> BackendResult<Vec<u8>> {
        let kind = kind.as_i32();
        let mut size = 0i32;
        let status = unsafe { (self.api.plugin_state_size)(handle.as_ptr(), kind, &mut size) };
        if status != MH_OK {
            return Err(self.backend_error(status, "mh_plugin_state_size"));
        }
        if size < 0 {
            return Err(BackendError::failed(
                "mh_plugin_state_size returned a negative size",
            ));
        }
        if size == 0 {
            return Ok(Vec::new());
        }

        // Some plugins (notably iZotope RX 11 while its editor is open) return
        // a slightly different size on the second getStateInformation call.
        // Grow the buffer and retry instead of failing on the size race.
        let mut capacity = size as usize;
        for _ in 0..4 {
            let mut data = vec![0u8; capacity];
            let mut written = 0i32;
            let status = unsafe {
                (self.api.plugin_state_export)(
                    handle.as_ptr(),
                    kind,
                    data.as_mut_ptr(),
                    capacity.min(i32::MAX as usize) as i32,
                    &mut written,
                )
            };
            if status == MH_INVALID_ARGUMENT {
                capacity = capacity.saturating_mul(2).clamp(1024, 64 * 1024 * 1024);
                continue;
            }
            if status != MH_OK {
                return Err(self.backend_error(status, "mh_plugin_state_export"));
            }
            if written < 0 || written as usize > capacity {
                return Err(BackendError::failed(format!(
                    "mh_plugin_state_export wrote {written} bytes into a {capacity} byte buffer"
                )));
            }
            data.truncate(written as usize);
            return Ok(data);
        }
        Err(BackendError::failed(
            "the plugin state kept growing while exporting",
        ))
    }

    fn state_import(
        &self,
        handle: PluginHandle,
        kind: StateKind,
        data: &[u8],
    ) -> BackendResult<()> {
        let len = to_i32(
            "state length",
            u32::try_from(data.len()).unwrap_or(u32::MAX),
        )?;
        let status = unsafe {
            (self.api.plugin_state_import)(handle.as_ptr(), kind.as_i32(), data.as_ptr(), len)
        };
        if status == MH_OK {
            Ok(())
        } else {
            Err(self.backend_error(status, "mh_plugin_state_import"))
        }
    }

    fn take_plugin_edited(&self, handle: PluginHandle) -> BackendResult<bool> {
        Ok(unsafe { (self.api.plugin_take_edited)(handle.as_ptr()) } != 0)
    }

    fn open_editor(&self, handle: PluginHandle) -> BackendResult<()> {
        let status = unsafe { (self.api.plugin_editor_open)(self.manager.get(), handle.as_ptr()) };
        if status == MH_OK {
            Ok(())
        } else {
            Err(self.backend_error(status, "mh_plugin_editor_open"))
        }
    }

    fn close_editor(&self, handle: PluginHandle) -> BackendResult<()> {
        let status = unsafe { (self.api.plugin_editor_close)(self.manager.get(), handle.as_ptr()) };
        if status == MH_OK {
            Ok(())
        } else {
            Err(self.backend_error(status, "mh_plugin_editor_close"))
        }
    }

    fn editor_open(&self, handle: PluginHandle) -> BackendResult<bool> {
        let mut open = 0i32;
        let status = unsafe { (self.api.plugin_editor_is_open)(handle.as_ptr(), &mut open) };
        if status != MH_OK {
            return Err(self.backend_error(status, "mh_plugin_editor_is_open"));
        }
        Ok(open != 0)
    }
}

impl HostBackend for JuceBackend {
    fn name(&self) -> &'static str {
        "juce"
    }

    fn abi_version(&self) -> BackendResult<u32> {
        JuceBackend::abi_version(self)
    }

    fn poll_events(&self) -> Vec<UiEvent> {
        JuceBackend::poll_events(self)
    }
}

fn dll_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(path) = std::env::var_os("MIC_HOST_DLL") {
        if !path.is_empty() {
            push_candidate(&mut candidates, PathBuf::from(path));
        }
    }

    // Exe-relative only: the working directory can be attacker-controlled
    // (e.g. a shortcut's "start in") and must never seed DLL search paths.
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().take(8) {
            push_candidate(&mut candidates, ancestor.join("mic_host.dll"));
        }
    }

    candidates
}

fn push_candidate(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    if path.is_file() && !candidates.contains(&path) {
        candidates.push(path);
    }
}

#[cfg(test)]
fn find_dll() -> Option<PathBuf> {
    dll_candidates().into_iter().next()
}

fn c_string(value: &str, what: &str) -> BackendResult<CString> {
    CString::new(value).map_err(|_| BackendError::invalid(format!("{what} contains a NUL byte")))
}

fn to_i32(what: &str, value: u32) -> BackendResult<i32> {
    i32::try_from(value)
        .map_err(|_| BackendError::invalid(format!("{what} is out of range: {value}")))
}

fn channel_count(what: &str, value: i32) -> BackendResult<u16> {
    u16::try_from(value)
        .map_err(|_| BackendError::invalid(format!("{what} is out of range: {value}")))
}

unsafe extern "C" fn backend_error_callback(
    ui_ctx: *mut c_void,
    status: i32,
    message: *const c_char,
) {
    if ui_ctx.is_null() {
        return;
    }
    let state = unsafe { &*(ui_ctx as *const CallbackState) };
    let message = if message.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    };

    if let Ok(mut last_error) = state.last_error.lock() {
        *last_error = Some((status, message.clone()));
    }
    if let Ok(mut events) = state.events.lock() {
        events.push_back(UiEvent::DeviceError { status, message });
    }
}

unsafe extern "C" fn backend_latency_callback(
    ui_ctx: *mut c_void,
    plugin: *mut c_void,
    latency_samples: i32,
) {
    if ui_ctx.is_null() {
        return;
    }
    let state = unsafe { &*(ui_ctx as *const CallbackState) };
    if let Ok(mut events) = state.events.lock() {
        events.push_back(UiEvent::LatencyChanged {
            plugin: PluginHandle::from_raw(plugin),
            latency: latency_samples,
        });
    }
}

unsafe extern "C" fn device_change_trampoline(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    let flag = unsafe { &*(ctx as *const AtomicBool) };
    flag.store(true, Ordering::Release);
}

unsafe extern "C" fn stream_input_trampoline(
    ctx: *mut c_void,
    channels: *const *const f32,
    num_channels: i32,
    frames: i32,
    sample_pos: i64,
) {
    if ctx.is_null() || channels.is_null() {
        return;
    }
    let stream = unsafe { &*(ctx as *const StreamCtx) };
    let Some(callback) = stream.callbacks.on_input else {
        return;
    };
    let user_ctx = stream.callbacks.ctx;
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        callback(user_ctx, channels, num_channels, frames, sample_pos);
    }));
}

unsafe extern "C" fn stream_output_trampoline(
    ctx: *mut c_void,
    channels: *const *mut f32,
    num_channels: i32,
    frames: i32,
    sample_pos: i64,
) {
    if ctx.is_null() || channels.is_null() {
        return;
    }
    let stream = unsafe { &*(ctx as *const StreamCtx) };
    let Some(callback) = stream.callbacks.on_output else {
        return;
    };
    let user_ctx = stream.callbacks.ctx;
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        callback(user_ctx, channels, num_channels, frames, sample_pos);
    }));
}

unsafe extern "C" fn stream_error_trampoline(
    ctx: *mut c_void,
    status: i32,
    message: *const c_char,
) {
    if ctx.is_null() {
        return;
    }
    let stream = unsafe { &*(ctx as *const StreamCtx) };
    let Some(callback) = stream.callbacks.on_error else {
        return;
    };
    let user_ctx = stream.callbacks.ctx;
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        callback(user_ctx, status, message);
    }));
}

unsafe extern "C" fn scan_progress_trampoline(
    ctx: *mut c_void,
    descriptor: *const MhPluginDescriptor,
    _done: i32,
    _total: i32,
) {
    if ctx.is_null() || descriptor.is_null() {
        return;
    }
    let progress = unsafe { &mut *(ctx as *mut &mut dyn FnMut(&PluginDescriptor)) };
    let descriptor = PluginDescriptor::from(unsafe { &*descriptor });
    let _ = catch_unwind(AssertUnwindSafe(|| (*progress)(&descriptor)));
}

#[cfg(test)]
mod tests;
