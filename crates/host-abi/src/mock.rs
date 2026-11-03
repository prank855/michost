use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::backend::{HostBackend, PluginHost, StreamHost};
use crate::constants::ABI_VERSION;
use crate::error::{BackendError, BackendResult};
use crate::types::{
    DeviceInfo, DeviceKind, DeviceLatency, DeviceStream, PluginChannels, PluginDescriptor,
    PluginHandle, StateKind, StreamCallbacks, StreamConfig, UiEvent,
};

#[derive(Clone, Debug, PartialEq)]
pub struct PrepareCall {
    pub handle: PluginHandle,
    pub sample_rate: f64,
    pub max_block: u32,
    pub main_inputs: u32,
    pub main_outputs: u32,
    pub sidechain_channels: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StreamOpenCall {
    pub config: StreamConfig,
    pub handle: DeviceStream,
}

struct MockState {
    devices: Vec<DeviceInfo>,
    descriptors: Vec<PluginDescriptor>,
    opened: Vec<StreamOpenCall>,
    started: Vec<DeviceStream>,
    stopped: Vec<DeviceStream>,
    closed: Vec<DeviceStream>,
    prepared: Vec<PrepareCall>,
    activated: Vec<(PluginHandle, bool)>,
    loaded: Vec<(String, String)>,
    latency: i32,
    stream_latency: DeviceLatency,
    channels: PluginChannels,
    events: VecDeque<UiEvent>,
    next_plugin: usize,
    next_stream: usize,
    gain: f32,
    bias: f32,
    processed_frames: usize,
    states: HashMap<(PluginHandle, StateKind), Vec<u8>>,
    editors: HashSet<PluginHandle>,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            devices: Vec::new(),
            descriptors: Vec::new(),
            opened: Vec::new(),
            started: Vec::new(),
            stopped: Vec::new(),
            closed: Vec::new(),
            prepared: Vec::new(),
            activated: Vec::new(),
            loaded: Vec::new(),
            latency: 0,
            stream_latency: DeviceLatency {
                input: 0,
                output: 0,
                buffer: 0,
                sample_rate: 48_000,
            },
            channels: PluginChannels {
                main_inputs: 2,
                main_outputs: 2,
                sidechain_inputs: 0,
            },
            events: VecDeque::new(),
            next_plugin: 0x1000,
            next_stream: 0x2000,
            gain: 1.0,
            bias: 0.0,
            processed_frames: 0,
            states: HashMap::new(),
            editors: HashSet::new(),
        }
    }
}

pub struct MockBackend {
    state: Mutex<MockState>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            state: Mutex::new(MockState::default()),
        }
    }
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn add_device(&self, device: DeviceInfo) {
        self.state().devices.push(device);
    }

    pub fn set_devices(&self, devices: Vec<DeviceInfo>) {
        self.state().devices = devices;
    }

    pub fn add_descriptor(&self, descriptor: PluginDescriptor) {
        self.state().descriptors.push(descriptor);
    }

    pub fn set_descriptors(&self, descriptors: Vec<PluginDescriptor>) {
        self.state().descriptors = descriptors;
    }

    pub fn set_channels(&self, main_inputs: u16, main_outputs: u16, sidechain_inputs: u16) {
        self.state().channels = PluginChannels {
            main_inputs,
            main_outputs,
            sidechain_inputs,
        };
    }

    pub fn set_latency(&self, samples: i32) {
        self.state().latency = samples;
    }

    pub fn set_stream_latency(&self, latency: DeviceLatency) {
        self.state().stream_latency = latency;
    }

    pub fn set_gain(&self, gain: f32) {
        self.state().gain = gain;
    }

    pub fn set_bias(&self, bias: f32) {
        self.state().bias = bias;
    }

    pub fn push_event(&self, event: UiEvent) {
        self.state().events.push_back(event);
    }

    pub fn prepared(&self) -> Vec<PrepareCall> {
        self.state().prepared.clone()
    }

    pub fn activated(&self) -> Vec<(PluginHandle, bool)> {
        self.state().activated.clone()
    }

    pub fn opened(&self) -> Vec<StreamOpenCall> {
        self.state().opened.clone()
    }

    pub fn started(&self) -> Vec<DeviceStream> {
        self.state().started.clone()
    }

    pub fn stopped(&self) -> Vec<DeviceStream> {
        self.state().stopped.clone()
    }

    pub fn closed(&self) -> Vec<DeviceStream> {
        self.state().closed.clone()
    }

    pub fn load_calls(&self) -> Vec<(String, String)> {
        self.state().loaded.clone()
    }

    pub fn processed_frames(&self) -> usize {
        self.state().processed_frames
    }

    fn plugin_handle(&self) -> PluginHandle {
        let mut state = self.state();
        state.next_plugin += 1;
        PluginHandle::from_raw(state.next_plugin as *mut c_void)
    }

    fn stream_handle(&self) -> DeviceStream {
        let mut state = self.state();
        state.next_stream += 1;
        DeviceStream::from_raw(state.next_stream as *mut c_void)
    }
}

impl StreamHost for MockBackend {
    fn enumerate_devices(&self, kind: DeviceKind) -> BackendResult<Vec<DeviceInfo>> {
        let state = self.state();
        let devices = state
            .devices
            .iter()
            .filter(|device| match kind {
                DeviceKind::Input => device.inputs > 0,
                DeviceKind::Output => device.outputs > 0,
            })
            .cloned()
            .collect();
        Ok(devices)
    }

    fn open_stream(
        &self,
        config: &StreamConfig,
        _callbacks: StreamCallbacks,
    ) -> BackendResult<DeviceStream> {
        let handle = self.stream_handle();
        self.state().opened.push(StreamOpenCall {
            config: config.clone(),
            handle,
        });
        Ok(handle)
    }

    fn start_stream(&self, stream: &DeviceStream) -> BackendResult<()> {
        self.state().started.push(*stream);
        Ok(())
    }

    fn stop_stream(&self, stream: &DeviceStream) -> BackendResult<()> {
        self.state().stopped.push(*stream);
        Ok(())
    }

    fn close_stream(&self, stream: DeviceStream) {
        self.state().closed.push(stream);
    }

    fn stream_latency(&self, _stream: &DeviceStream) -> BackendResult<DeviceLatency> {
        Ok(self.state().stream_latency)
    }

    fn stream_control_panel(&self, _stream: &DeviceStream) -> BackendResult<()> {
        Ok(())
    }
}

impl PluginHost for MockBackend {
    fn scan_with_paths(
        &self,
        _extra_paths: &str,
        progress: &mut dyn FnMut(&PluginDescriptor),
    ) -> BackendResult<Vec<PluginDescriptor>> {
        let descriptors = self.state().descriptors.clone();
        for descriptor in &descriptors {
            progress(descriptor);
        }
        Ok(descriptors)
    }

    fn plugin_descriptions(&self, path: &str) -> BackendResult<Vec<PluginDescriptor>> {
        let descriptors = self
            .state()
            .descriptors
            .iter()
            .filter(|descriptor| descriptor.path == path)
            .cloned()
            .collect();
        Ok(descriptors)
    }

    fn load_plugin(&self, path: &str, class_id: &str) -> BackendResult<PluginHandle> {
        if path.is_empty() {
            return Err(BackendError::invalid("plugin path must not be empty"));
        }
        self.state()
            .loaded
            .push((path.to_owned(), class_id.to_owned()));
        Ok(self.plugin_handle())
    }

    fn free_plugin(&self, handle: PluginHandle) -> BackendResult<()> {
        let mut state = self.state();
        state.editors.remove(&handle);
        state.states.retain(|(stored, _), _| *stored != handle);
        Ok(())
    }

    fn plugin_channels(&self, _handle: PluginHandle) -> BackendResult<PluginChannels> {
        Ok(self.state().channels)
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
        self.state().prepared.push(PrepareCall {
            handle,
            sample_rate,
            max_block,
            main_inputs,
            main_outputs,
            sidechain_channels,
        });
        Ok(())
    }

    fn activate_plugin(&self, handle: PluginHandle, active: bool) -> BackendResult<()> {
        self.state().activated.push((handle, active));
        Ok(())
    }

    fn process_plugin(
        &self,
        _handle: PluginHandle,
        channels: &mut [&mut [f32]],
        frames: usize,
    ) -> BackendResult<()> {
        let (gain, bias) = {
            let mut state = self.state();
            state.processed_frames += frames;
            (state.gain, state.bias)
        };
        for channel in channels {
            let count = channel.len().min(frames);
            for sample in &mut channel[..count] {
                *sample = *sample * gain + bias;
            }
        }
        Ok(())
    }

    fn plugin_latency(&self, _handle: PluginHandle) -> BackendResult<u32> {
        Ok(self.state().latency.max(0) as u32)
    }

    fn state_export(&self, handle: PluginHandle, kind: StateKind) -> BackendResult<Vec<u8>> {
        Ok(self
            .state()
            .states
            .get(&(handle, kind))
            .cloned()
            .unwrap_or_default())
    }

    fn state_import(
        &self,
        handle: PluginHandle,
        kind: StateKind,
        data: &[u8],
    ) -> BackendResult<()> {
        self.state().states.insert((handle, kind), data.to_vec());
        Ok(())
    }

    fn take_plugin_edited(&self, _handle: PluginHandle) -> BackendResult<bool> {
        Ok(false)
    }

    fn open_editor(&self, handle: PluginHandle) -> BackendResult<()> {
        self.state().editors.insert(handle);
        Ok(())
    }

    fn close_editor(&self, handle: PluginHandle) -> BackendResult<()> {
        self.state().editors.remove(&handle);
        Ok(())
    }

    fn editor_open(&self, handle: PluginHandle) -> BackendResult<bool> {
        Ok(self.state().editors.contains(&handle))
    }
}

impl HostBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn abi_version(&self) -> BackendResult<u32> {
        Ok(ABI_VERSION)
    }

    fn poll_events(&self) -> Vec<UiEvent> {
        self.state().events.drain(..).collect()
    }
}
