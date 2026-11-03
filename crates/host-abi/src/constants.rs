pub const ABI_VERSION: u32 = 3;

pub const MH_PATH_LEN: usize = 640;
pub const MH_ID_LEN: usize = 128;
pub const MH_NAME_LEN: usize = 128;
pub const MH_UNITS_LEN: usize = 32;
pub const MH_DEVICE_LEN: usize = 256;
pub const MH_MESSAGE_LEN: usize = 512;
pub const MH_MAX_BUFFER_SIZES: usize = 16;

pub const MH_OK: i32 = 0;
pub const MH_ERR: i32 = 1;
pub const MH_NOT_SUPPORTED: i32 = 2;
pub const MH_INVALID_ARGUMENT: i32 = 3;
pub const MH_ABI_MISMATCH: i32 = 4;
pub const MH_TIMEOUT: i32 = 5;
pub const MH_NOT_FOUND: i32 = 6;

pub const MH_STREAM_INPUT: i32 = 0;
pub const MH_STREAM_OUTPUT: i32 = 1;

pub const MH_DEVICE_KIND_INPUT: i32 = 0;
pub const MH_DEVICE_KIND_OUTPUT: i32 = 1;

pub const MH_STATE_COMPONENT: i32 = 0;
pub const MH_STATE_CONTROLLER: i32 = 1;

pub const MH_LAYOUT_DEVICE_INFO: i32 = 0;
pub const MH_LAYOUT_STREAM_CONFIG: i32 = 1;
pub const MH_LAYOUT_STREAM_CALLBACKS: i32 = 2;
pub const MH_LAYOUT_LATENCY: i32 = 3;
pub const MH_LAYOUT_PLUGIN_DESCRIPTOR: i32 = 4;
pub const MH_LAYOUT_PARAM_INFO: i32 = 5;
pub const MH_LAYOUT_CALLBACKS: i32 = 6;
