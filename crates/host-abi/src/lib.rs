pub mod backend;
pub mod constants;
pub mod error;
pub mod ffi;
pub mod mock;
pub mod types;

pub use backend::{BackendProcessor, HostBackend, PluginHost, StreamHost};
pub use constants::*;
pub use error::{BackendError, BackendResult};
pub use ffi::{
    MhCallbacks, MhDeviceChangedCallback, MhDeviceInfo, MhErrorCallback, MhLatency,
    MhLatencyChangedCallback, MhParamInfo, MhPluginDescriptor, MhScanProgress, MhStreamCallbacks,
    MhStreamConfig, MhStreamErrorCallback, MhStreamInputCallback, MhStreamOutputCallback, read_str,
    write_str,
};
pub use mock::{MockBackend, PrepareCall};
pub use types::{
    AudioApi, DeviceInfo, DeviceKind, DeviceLatency, DeviceStream, ErrorCallback, InputCallback,
    OutputCallback, ParamInfo, PluginChannels, PluginDescriptor, PluginHandle, StateKind,
    StreamCallbacks, StreamConfig, StreamDirection, UiEvent,
};

#[cfg(test)]
mod tests;
