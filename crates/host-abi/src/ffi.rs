use std::ffi::{c_char, c_void};

use crate::constants::{
    ABI_VERSION, MH_DEVICE_LEN, MH_ID_LEN, MH_MAX_BUFFER_SIZES, MH_NAME_LEN, MH_PATH_LEN,
    MH_UNITS_LEN,
};
use crate::types::{
    AudioApi, DeviceInfo, DeviceLatency, ParamInfo, PluginDescriptor, StreamConfig,
};

pub fn write_str(dst: &mut [u8], value: &str) {
    if dst.is_empty() {
        return;
    }

    dst.fill(0);
    let capacity = dst.len() - 1;
    let mut len = value.len().min(capacity);
    while len > 0 && !value.is_char_boundary(len) {
        len -= 1;
    }
    dst[..len].copy_from_slice(&value.as_bytes()[..len]);
}

pub fn read_str(src: &[u8]) -> String {
    let end = src.iter().position(|byte| *byte == 0).unwrap_or(src.len());
    String::from_utf8_lossy(&src[..end]).into_owned()
}

pub type MhErrorCallback =
    unsafe extern "C" fn(ctx: *mut c_void, status: i32, message: *const c_char);
pub type MhLatencyChangedCallback =
    unsafe extern "C" fn(ctx: *mut c_void, plugin: *mut c_void, latency_samples: i32);
pub type MhDeviceChangedCallback = unsafe extern "C" fn(ctx: *mut c_void);
pub type MhStreamInputCallback = unsafe extern "C" fn(
    ctx: *mut c_void,
    channels: *const *const f32,
    num_channels: i32,
    frames: i32,
    sample_pos: i64,
);
pub type MhStreamOutputCallback = unsafe extern "C" fn(
    ctx: *mut c_void,
    channels: *const *mut f32,
    num_channels: i32,
    frames: i32,
    sample_pos: i64,
);
pub type MhStreamErrorCallback =
    unsafe extern "C" fn(ctx: *mut c_void, status: i32, message: *const c_char);

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MhCallbacks {
    pub abi_version: u32,
    pub size: u32,
    pub ui_ctx: *mut c_void,
    pub on_error: Option<MhErrorCallback>,
    pub on_latency_changed: Option<MhLatencyChangedCallback>,
}

impl Default for MhCallbacks {
    fn default() -> Self {
        Self {
            abi_version: ABI_VERSION,
            size: std::mem::size_of::<Self>() as u32,
            ui_ctx: std::ptr::null_mut(),
            on_error: None,
            on_latency_changed: None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MhDeviceInfo {
    pub name: [u8; MH_DEVICE_LEN],
    pub id: [u8; MH_ID_LEN],
    pub inputs: u32,
    pub outputs: u32,
    pub default_sample_rate: f64,
    pub api: i32,
    pub is_default: u8,
    pub has_control_panel: u8,
    pub pad: [u8; 2],
    pub buffer_sizes: [u32; MH_MAX_BUFFER_SIZES],
    pub buffer_size_count: u32,
    pub pad2: u32,
}

impl Default for MhDeviceInfo {
    fn default() -> Self {
        Self {
            name: [0; MH_DEVICE_LEN],
            id: [0; MH_ID_LEN],
            inputs: 0,
            outputs: 0,
            default_sample_rate: 0.0,
            api: 0,
            is_default: 0,
            has_control_panel: 0,
            pad: [0; 2],
            buffer_sizes: [0; MH_MAX_BUFFER_SIZES],
            buffer_size_count: 0,
            pad2: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MhStreamConfig {
    pub device: [u8; MH_DEVICE_LEN],
    pub sample_rate: f64,
    pub block_size: u32,
    pub channels: u32,
    pub api: i32,
    pub direction: i32,
    pub channel_mask: u64,
    pub output_channels: u32,
    pub output_channel_mask: u64,
}

impl Default for MhStreamConfig {
    fn default() -> Self {
        Self {
            device: [0; MH_DEVICE_LEN],
            sample_rate: 0.0,
            block_size: 0,
            channels: 0,
            api: 0,
            direction: 0,
            channel_mask: 0,
            output_channels: 0,
            output_channel_mask: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MhStreamCallbacks {
    pub size: u32,
    pub pad: u32,
    pub ctx: *mut c_void,
    pub on_input: Option<MhStreamInputCallback>,
    pub on_output: Option<MhStreamOutputCallback>,
    pub on_error: Option<MhStreamErrorCallback>,
}

impl Default for MhStreamCallbacks {
    fn default() -> Self {
        Self {
            size: std::mem::size_of::<Self>() as u32,
            pad: 0,
            ctx: std::ptr::null_mut(),
            on_input: None,
            on_output: None,
            on_error: None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MhLatency {
    pub input: u32,
    pub output: u32,
    pub buffer: u32,
    pub sample_rate: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MhPluginDescriptor {
    pub path: [u8; MH_PATH_LEN],
    pub class_id: [u8; MH_ID_LEN],
    pub name: [u8; MH_NAME_LEN],
    pub vendor: [u8; MH_NAME_LEN],
    pub main_inputs: u32,
    pub main_outputs: u32,
    pub sidechain_inputs: u32,
    pub flags: u32,
    pub category: [u8; MH_NAME_LEN],
}

impl Default for MhPluginDescriptor {
    fn default() -> Self {
        Self {
            path: [0; MH_PATH_LEN],
            class_id: [0; MH_ID_LEN],
            name: [0; MH_NAME_LEN],
            vendor: [0; MH_NAME_LEN],
            main_inputs: 0,
            main_outputs: 0,
            sidechain_inputs: 0,
            flags: 0,
            category: [0; MH_NAME_LEN],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MhParamInfo {
    pub id: u32,
    pub pad: u32,
    pub name: [u8; MH_NAME_LEN],
    pub units: [u8; MH_UNITS_LEN],
    pub default_value: f64,
    pub step_count: u32,
    pub pad2: u32,
}

impl Default for MhParamInfo {
    fn default() -> Self {
        Self {
            id: 0,
            pad: 0,
            name: [0; MH_NAME_LEN],
            units: [0; MH_UNITS_LEN],
            default_value: 0.0,
            step_count: 0,
            pad2: 0,
        }
    }
}

pub type MhScanProgress = Option<
    unsafe extern "C" fn(
        ctx: *mut c_void,
        descriptor: *const MhPluginDescriptor,
        done: i32,
        total: i32,
    ),
>;

impl From<&DeviceInfo> for MhDeviceInfo {
    fn from(value: &DeviceInfo) -> Self {
        let mut out = Self::default();
        write_str(&mut out.name, &value.name);
        write_str(&mut out.id, &value.id);
        out.inputs = value.inputs;
        out.outputs = value.outputs;
        out.default_sample_rate = value.default_sample_rate;
        out.api = value.api.as_i32();
        out.is_default = u8::from(value.is_default);
        out.has_control_panel = u8::from(value.has_control_panel);
        let count = value.buffer_sizes.len().min(MH_MAX_BUFFER_SIZES);
        out.buffer_sizes[..count].copy_from_slice(&value.buffer_sizes[..count]);
        out.buffer_size_count = count as u32;
        out
    }
}

impl From<&MhDeviceInfo> for DeviceInfo {
    fn from(value: &MhDeviceInfo) -> Self {
        let count = (value.buffer_size_count as usize).min(MH_MAX_BUFFER_SIZES);
        Self {
            name: read_str(&value.name),
            id: read_str(&value.id),
            inputs: value.inputs,
            outputs: value.outputs,
            default_sample_rate: value.default_sample_rate,
            api: AudioApi::from_i32(value.api),
            is_default: value.is_default != 0,
            has_control_panel: value.has_control_panel != 0,
            buffer_sizes: value.buffer_sizes[..count].to_vec(),
        }
    }
}

impl From<&StreamConfig> for MhStreamConfig {
    fn from(value: &StreamConfig) -> Self {
        let mut out = Self::default();
        write_str(&mut out.device, &value.device);
        out.sample_rate = value.sample_rate;
        out.block_size = value.block_size;
        out.channels = value.channels;
        out.api = value.api.as_i32();
        out.direction = value.direction.as_i32();
        out.channel_mask = value.channel_mask;
        out.output_channels = value.output_channels;
        out.output_channel_mask = value.output_channel_mask;
        out
    }
}

impl From<&MhLatency> for DeviceLatency {
    fn from(value: &MhLatency) -> Self {
        Self {
            input: value.input,
            output: value.output,
            buffer: value.buffer,
            sample_rate: value.sample_rate,
        }
    }
}

impl From<&PluginDescriptor> for MhPluginDescriptor {
    fn from(value: &PluginDescriptor) -> Self {
        let mut out = Self::default();
        write_str(&mut out.path, &value.path);
        write_str(&mut out.class_id, &value.class_id);
        write_str(&mut out.name, &value.name);
        write_str(&mut out.vendor, &value.vendor);
        write_str(&mut out.category, &value.category);
        out.main_inputs = value.main_inputs;
        out.main_outputs = value.main_outputs;
        out.sidechain_inputs = value.sidechain_inputs;
        out.flags = value.flags;
        out
    }
}

impl From<&MhPluginDescriptor> for PluginDescriptor {
    fn from(value: &MhPluginDescriptor) -> Self {
        Self {
            path: read_str(&value.path),
            class_id: read_str(&value.class_id),
            name: read_str(&value.name),
            vendor: read_str(&value.vendor),
            category: read_str(&value.category),
            main_inputs: value.main_inputs,
            main_outputs: value.main_outputs,
            sidechain_inputs: value.sidechain_inputs,
            flags: value.flags,
        }
    }
}

impl From<&ParamInfo> for MhParamInfo {
    fn from(value: &ParamInfo) -> Self {
        let mut out = Self {
            id: value.id,
            ..Self::default()
        };
        write_str(&mut out.name, &value.name);
        write_str(&mut out.units, &value.units);
        out.default_value = value.default_value;
        out.step_count = value.step_count;
        out
    }
}

impl From<&MhParamInfo> for ParamInfo {
    fn from(value: &MhParamInfo) -> Self {
        Self {
            id: value.id,
            name: read_str(&value.name),
            units: read_str(&value.units),
            default_value: value.default_value,
            step_count: value.step_count,
        }
    }
}
