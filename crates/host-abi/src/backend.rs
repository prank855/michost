use std::sync::Arc;

use michost_graph::PluginProcessor;

use crate::error::BackendResult;
use crate::types::{
    DeviceInfo, DeviceKind, DeviceLatency, DeviceStream, PluginChannels, PluginDescriptor,
    PluginHandle, StateKind, StreamCallbacks, StreamConfig, UiEvent,
};

pub trait StreamHost: Send + Sync {
    fn enumerate_devices(&self, kind: DeviceKind) -> BackendResult<Vec<DeviceInfo>>;
    fn open_stream(
        &self,
        config: &StreamConfig,
        callbacks: StreamCallbacks,
    ) -> BackendResult<DeviceStream>;
    fn start_stream(&self, stream: &DeviceStream) -> BackendResult<()>;
    fn stop_stream(&self, stream: &DeviceStream) -> BackendResult<()>;
    fn close_stream(&self, stream: DeviceStream);
    fn stream_latency(&self, stream: &DeviceStream) -> BackendResult<DeviceLatency>;
    fn stream_control_panel(&self, stream: &DeviceStream) -> BackendResult<()>;
}

pub trait PluginHost: Send + Sync {
    fn scan(
        &self,
        progress: &mut dyn FnMut(&PluginDescriptor),
    ) -> BackendResult<Vec<PluginDescriptor>> {
        self.scan_with_paths("", progress)
    }
    fn scan_with_paths(
        &self,
        extra_paths: &str,
        progress: &mut dyn FnMut(&PluginDescriptor),
    ) -> BackendResult<Vec<PluginDescriptor>>;
    fn plugin_descriptions(&self, path: &str) -> BackendResult<Vec<PluginDescriptor>>;
    fn load_plugin(&self, path: &str, class_id: &str) -> BackendResult<PluginHandle>;
    fn load_plugin_named(
        &self,
        path: &str,
        class_id: &str,
        _name: &str,
    ) -> BackendResult<PluginHandle> {
        self.load_plugin(path, class_id)
    }
    fn free_plugin(&self, handle: PluginHandle) -> BackendResult<()>;
    fn plugin_channels(&self, handle: PluginHandle) -> BackendResult<PluginChannels>;
    fn prepare_plugin(
        &self,
        handle: PluginHandle,
        sample_rate: f64,
        max_block: u32,
        main_inputs: u32,
        main_outputs: u32,
        sidechain_channels: u32,
    ) -> BackendResult<()>;
    fn activate_plugin(&self, handle: PluginHandle, active: bool) -> BackendResult<()>;
    fn process_plugin(
        &self,
        handle: PluginHandle,
        channels: &mut [&mut [f32]],
        frames: usize,
    ) -> BackendResult<()>;
    fn plugin_latency(&self, handle: PluginHandle) -> BackendResult<u32>;
    fn state_export(&self, handle: PluginHandle, kind: StateKind) -> BackendResult<Vec<u8>>;
    fn state_import(&self, handle: PluginHandle, kind: StateKind, data: &[u8])
    -> BackendResult<()>;
    fn take_plugin_edited(&self, handle: PluginHandle) -> BackendResult<bool>;
    fn open_editor(&self, handle: PluginHandle) -> BackendResult<()>;
    fn close_editor(&self, handle: PluginHandle) -> BackendResult<()>;
    fn editor_open(&self, handle: PluginHandle) -> BackendResult<bool>;
}

pub trait HostBackend: StreamHost + PluginHost {
    fn name(&self) -> &'static str;
    fn abi_version(&self) -> BackendResult<u32>;
    fn poll_events(&self) -> Vec<UiEvent>;
}

pub struct BackendProcessor {
    host: Arc<dyn PluginHost>,
    handle: PluginHandle,
}

impl BackendProcessor {
    pub fn new(host: Arc<dyn PluginHost>, handle: PluginHandle) -> Self {
        Self { host, handle }
    }

    pub fn host(&self) -> &Arc<dyn PluginHost> {
        &self.host
    }

    pub fn handle(&self) -> PluginHandle {
        self.handle
    }
}

impl PluginProcessor for BackendProcessor {
    fn process(&mut self, channels: &mut [&mut [f32]], frames: usize) {
        let _ = self.host.process_plugin(self.handle, channels, frames);
    }
}
