use std::ffi::{c_char, c_void};

use serde::{Deserialize, Serialize};

use crate::constants::{
    MH_DEVICE_KIND_INPUT, MH_DEVICE_KIND_OUTPUT, MH_STATE_COMPONENT, MH_STATE_CONTROLLER,
    MH_STREAM_INPUT, MH_STREAM_OUTPUT,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioApi {
    #[default]
    WasapiShared,
    WasapiLowLatency,
    WasapiExclusive,
    Asio,
    Other,
}

impl AudioApi {
    pub fn as_i32(self) -> i32 {
        match self {
            Self::WasapiShared => 0,
            Self::WasapiLowLatency => 1,
            Self::WasapiExclusive => 2,
            Self::Asio => 3,
            Self::Other => -1,
        }
    }

    pub fn from_i32(value: i32) -> Self {
        match value {
            0 => Self::WasapiShared,
            1 => Self::WasapiLowLatency,
            2 => Self::WasapiExclusive,
            3 => Self::Asio,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeviceKind {
    #[default]
    Input,
    Output,
}

impl DeviceKind {
    pub fn as_i32(self) -> i32 {
        match self {
            Self::Input => MH_DEVICE_KIND_INPUT,
            Self::Output => MH_DEVICE_KIND_OUTPUT,
        }
    }

    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            MH_DEVICE_KIND_INPUT => Some(Self::Input),
            MH_DEVICE_KIND_OUTPUT => Some(Self::Output),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub id: String,
    pub inputs: u32,
    pub outputs: u32,
    pub default_sample_rate: f64,
    pub api: AudioApi,
    pub is_default: bool,
    pub has_control_panel: bool,
    pub buffer_sizes: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceLatency {
    pub input: u32,
    pub output: u32,
    pub buffer: u32,
    pub sample_rate: u32,
}

impl DeviceLatency {
    pub fn round_trip(self) -> u32 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.buffer)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamDirection {
    #[default]
    Input,
    Output,
}

impl StreamDirection {
    pub fn as_i32(self) -> i32 {
        match self {
            Self::Input => MH_STREAM_INPUT,
            Self::Output => MH_STREAM_OUTPUT,
        }
    }

    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            MH_STREAM_INPUT => Some(Self::Input),
            MH_STREAM_OUTPUT => Some(Self::Output),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamConfig {
    pub device: String,
    pub sample_rate: f64,
    pub block_size: u32,
    pub channels: u32,
    pub api: AudioApi,
    pub direction: StreamDirection,
    /// Bit mask of device channels (bit 0 = first channel). Zero selects the
    /// first `channels` channels.
    pub channel_mask: u64,
    /// Present only on duplex input streams opened for same-driver monitoring.
    pub output_channels: u32,
    pub output_channel_mask: u64,
}

pub type InputCallback = unsafe extern "C" fn(
    ctx: *mut c_void,
    channels: *const *const f32,
    num_channels: i32,
    frames: i32,
    sample_pos: i64,
);

pub type OutputCallback = unsafe extern "C" fn(
    ctx: *mut c_void,
    channels: *const *mut f32,
    num_channels: i32,
    frames: i32,
    sample_pos: i64,
);

pub type ErrorCallback =
    unsafe extern "C" fn(ctx: *mut c_void, status: i32, message: *const c_char);

#[derive(Clone, Copy, Debug, Default)]
pub struct StreamCallbacks {
    pub ctx: *mut c_void,
    pub on_input: Option<InputCallback>,
    pub on_output: Option<OutputCallback>,
    pub on_error: Option<ErrorCallback>,
}

#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DeviceStream(*mut c_void);

impl DeviceStream {
    pub fn null() -> Self {
        Self(std::ptr::null_mut())
    }

    pub fn from_raw(ptr: *mut c_void) -> Self {
        Self(ptr)
    }

    pub fn as_ptr(self) -> *mut c_void {
        self.0
    }

    pub fn is_null(self) -> bool {
        self.0.is_null()
    }
}

impl std::fmt::Debug for DeviceStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceStream({:p})", self.0)
    }
}

unsafe impl Send for DeviceStream {}

#[repr(transparent)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PluginHandle(*mut c_void);

impl PluginHandle {
    pub fn null() -> Self {
        Self(std::ptr::null_mut())
    }

    pub fn from_raw(ptr: *mut c_void) -> Self {
        Self(ptr)
    }

    pub fn as_ptr(self) -> *mut c_void {
        self.0
    }

    pub fn is_null(self) -> bool {
        self.0.is_null()
    }
}

impl std::fmt::Debug for PluginHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PluginHandle({:p})", self.0)
    }
}

unsafe impl Send for PluginHandle {}
unsafe impl Sync for PluginHandle {}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDescriptor {
    pub path: String,
    pub class_id: String,
    pub name: String,
    pub vendor: String,
    /// VST3 factory subcategories, for example `Fx|EQ` or `Instrument|Synth`.
    #[serde(default)]
    pub category: String,
    pub main_inputs: u32,
    pub main_outputs: u32,
    pub sidechain_inputs: u32,
    pub flags: u32,
}

impl PluginDescriptor {
    pub fn to_spec(&self) -> michost_graph::PluginSpec {
        michost_graph::PluginSpec {
            path: self.path.clone(),
            class_id: self.class_id.clone(),
            name: self.name.clone(),
            vendor: self.vendor.clone(),
            main_inputs: u16::try_from(self.main_inputs).unwrap_or(u16::MAX),
            main_outputs: u16::try_from(self.main_outputs).unwrap_or(u16::MAX),
            sidechain_inputs: u16::try_from(self.sidechain_inputs).unwrap_or(u16::MAX),
        }
    }

    pub fn from_spec(spec: &michost_graph::PluginSpec) -> Self {
        Self {
            path: spec.path.clone(),
            class_id: spec.class_id.clone(),
            name: spec.name.clone(),
            vendor: spec.vendor.clone(),
            category: String::new(),
            main_inputs: u32::from(spec.main_inputs),
            main_outputs: u32::from(spec.main_outputs),
            sidechain_inputs: u32::from(spec.sidechain_inputs),
            flags: 0,
        }
    }
}

impl From<&PluginDescriptor> for michost_graph::PluginSpec {
    fn from(value: &PluginDescriptor) -> Self {
        value.to_spec()
    }
}

impl From<&michost_graph::PluginSpec> for PluginDescriptor {
    fn from(value: &michost_graph::PluginSpec) -> Self {
        Self::from_spec(value)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginChannels {
    pub main_inputs: u16,
    pub main_outputs: u16,
    pub sidechain_inputs: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub units: String,
    pub default_value: f64,
    pub step_count: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StateKind {
    #[default]
    Component,
    Controller,
}

impl StateKind {
    pub fn as_i32(self) -> i32 {
        match self {
            Self::Component => MH_STATE_COMPONENT,
            Self::Controller => MH_STATE_CONTROLLER,
        }
    }

    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            MH_STATE_COMPONENT => Some(Self::Component),
            MH_STATE_CONTROLLER => Some(Self::Controller),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEvent {
    DeviceError { status: i32, message: String },
    LatencyChanged { plugin: PluginHandle, latency: i32 },
}
