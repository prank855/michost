use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use michost_builtin::{AudioAnalysis, BuiltinKind, BuiltinProcessor, ParamSet};
use michost_graph::{
    ChainEngine, ChainEntry, ChainPlan, MAX_FRAMES, MeteredProcessor, PlanNode, PluginMeter,
    PluginProcessor, PluginSpec, SidechainSource, samples_to_milliseconds,
};
use michost_host_abi::{
    AudioApi, BackendProcessor, DeviceLatency, DeviceStream, PluginHandle, PluginHost, StateKind,
    StreamConfig, StreamDirection, StreamHost,
};
use michost_host_juce::JuceBackend;
use michost_project::{DeviceSelection, OutputKind, Project};
use michost_vmic::{Format as VmicFormat, VirtualMicSink};

use crate::bridge::{self, StreamBridge};
use crate::logging;

const MIN_SAMPLE_RATE: f64 = 8_000.0;
const MAX_SAMPLE_RATE: f64 = 384_000.0;
const MAX_RING_FRAMES: usize = 262_144;
const START_CANCELLED: &str = "starting audio was cancelled";

static STARTUP_PROGRESS: std::sync::LazyLock<std::sync::Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

fn set_startup_progress(text: impl Into<String>) {
    if let Ok(mut progress) = STARTUP_PROGRESS.lock() {
        *progress = Some(text.into());
    }
}

pub fn startup_progress() -> Option<String> {
    STARTUP_PROGRESS
        .lock()
        .ok()
        .and_then(|progress| progress.clone())
}

pub fn clear_startup_progress() {
    if let Ok(mut progress) = STARTUP_PROGRESS.lock() {
        *progress = None;
    }
}

pub struct LoadedPlugin {
    pub entry_id: u64,
    pub handle: Option<PluginHandle>,
    pub builtin: Option<BuiltinRuntime>,
    pub name: String,
    pub main_inputs: u16,
    pub main_outputs: u16,
    pub native_sidechain: u16,
    pub active_sidechain: u16,
    pub latency: u32,
    pub memory_bytes: Option<u64>,
    pub meter: Arc<PluginMeter>,
    pub oversampling: u8,
}

pub struct BuiltinRuntime {
    pub kind: BuiltinKind,
    pub params: Arc<ParamSet>,
    pub analysis: Arc<AudioAnalysis>,
}

pub struct PreparedPlugin {
    pub handle: PluginHandle,
    pub name: String,
    pub main_inputs: u16,
    pub main_outputs: u16,
    pub native_sidechain: u16,
    pub active_sidechain: u16,
    pub latency_samples: u32,
    pub memory_bytes: Option<u64>,
    pub meter: Arc<PluginMeter>,
}

pub struct EndpointInfo {
    pub device: String,
    pub api: AudioApi,
    pub channels: u32,
    pub sample_rate: f64,
    pub buffer_size: u32,
    pub latency: DeviceLatency,
}

pub struct LatencyBreakdown {
    pub input_device_ms: f64,
    pub input_buffer_ms: f64,
    pub graph_ms: f64,
    pub chain_ms: f64,
    pub output_device_ms: f64,
    pub output_buffer_ms: f64,
    pub output_queue_ms: f64,
    pub total_ms: f64,
}

pub struct AudioSession {
    backend: Arc<JuceBackend>,
    bridge: Option<StreamBridge>,
    plugins: Vec<LoadedPlugin>,
    retired: Vec<LoadedPlugin>,
    chain: Vec<ChainEntry>,
    input: EndpointInfo,
    output: EndpointInfo,
    second_input: Option<EndpointInfo>,
    sample_rate: f64,
    block_size: u32,
    mono_source: Option<u8>,
    total_latency: u32,
    output_target: usize,
    virtual_mic_active: bool,
    capture_engine_ms: Option<f64>,
    pub warnings: Vec<String>,
    stopped: bool,
}

impl AudioSession {
    pub fn start(backend: Arc<JuceBackend>, project: &mut Project) -> Result<Self, String> {
        Self::start_with_options(backend, project, None)
    }

    pub fn start_with_options(
        backend: Arc<JuceBackend>,
        project: &mut Project,
        mono_source: Option<u8>,
    ) -> Result<Self, String> {
        Self::start_with_cancel(backend, project, mono_source, None)
    }

    pub fn start_with_cancel(
        backend: Arc<JuceBackend>,
        project: &mut Project,
        mono_source: Option<u8>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        Self::start_impl(backend, project, mono_source, cancel, None)
    }

    pub fn start_with_monitor(
        backend: Arc<JuceBackend>,
        project: &mut Project,
        mono_source: Option<u8>,
        cancel: Option<&AtomicBool>,
        monitor: Option<&DeviceSelection>,
    ) -> Result<Self, String> {
        Self::start_impl(backend, project, mono_source, cancel, monitor)
    }

    fn start_impl(
        backend: Arc<JuceBackend>,
        project: &mut Project,
        mono_source: Option<u8>,
        cancel: Option<&AtomicBool>,
        monitor: Option<&DeviceSelection>,
    ) -> Result<Self, String> {
        let is_cancelled = || cancel.is_some_and(|flag| flag.load(Ordering::Acquire));
        if !matches!(mono_source, None | Some(0) | Some(1)) {
            return Err("the mono source must be 0 (left) or 1 (right)".to_string());
        }
        let started = Instant::now();
        let settings = project.settings.clone();
        if settings.input.name.is_empty() {
            return Err("select an input device".to_string());
        }
        if settings.output_kind == OutputKind::Device && settings.output.name.is_empty() {
            return Err("select an output device".to_string());
        }
        if settings.sidechain_input_enabled && settings.sidechain_input.name.is_empty() {
            return Err(
                "select a sidechain input device or disable the sidechain input".to_string(),
            );
        }
        if !settings.sample_rate.is_finite()
            || (settings.sample_rate != 0.0
                && !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&settings.sample_rate))
        {
            return Err(format!(
                "the project sample rate ({} Hz) is invalid; use 0 for automatic or a value between {} and {} Hz",
                settings.sample_rate, MIN_SAMPLE_RATE as u32, MAX_SAMPLE_RATE as u32
            ));
        }
        if is_cancelled() {
            return Err(START_CANCELLED.to_string());
        }

        let requested_input_channels = settings.input.channels.clamp(1, 8);
        let input_channels = if mono_source == Some(1) {
            requested_input_channels.max(2)
        } else {
            requested_input_channels
        };
        let virtual_mic_requested =
            cfg!(debug_assertions) && settings.output_kind == OutputKind::VirtualMic;
        let mut output_channels = if virtual_mic_requested {
            2
        } else {
            settings.output.channels.clamp(1, 2)
        };
        let nominal_rate = if settings.sample_rate > 0.0 {
            settings.sample_rate
        } else {
            48_000.0
        };
        let ring_frames = ((nominal_rate * 0.5) as usize).clamp(256, MAX_RING_FRAMES);

        let device_started = Instant::now();
        let duplex_monitor = monitor.is_some_and(|monitor| {
            settings.input.api == AudioApi::Asio
                && monitor.api == AudioApi::Asio
                && monitor.name == settings.input.name
        });
        let monitor_channels = monitor.map(|monitor| monitor.channels).unwrap_or(0);
        let mut input_source = if duplex_monitor {
            bridge::input_source_with_monitor(
                input_channels as usize,
                ring_frames,
                monitor_channels as usize,
                ring_frames,
            )?
        } else {
            bridge::input_source(input_channels as usize, ring_frames)?
        };
        if duplex_monitor {
            logging::line(format!(
                "audio session: duplex ASIO monitor on '{}' ({} ch, mask 0x{:X})",
                monitor.map(|monitor| monitor.name.as_str()).unwrap_or(""),
                monitor_channels,
                monitor.map(|monitor| monitor.channel_mask).unwrap_or(0)
            ));
        }
        let input_config = StreamConfig {
            device: settings.input.name.clone(),
            sample_rate: settings.sample_rate,
            block_size: settings.buffer_size,
            channels: input_channels,
            api: settings.input.api,
            direction: StreamDirection::Input,
            channel_mask: settings.input.channel_mask,
            output_channels: if duplex_monitor { monitor_channels } else { 0 },
            output_channel_mask: if duplex_monitor {
                monitor.map(|monitor| monitor.channel_mask).unwrap_or(0)
            } else {
                0
            },
        };
        let input_stream = match backend
            .open_stream(&input_config, bridge::input_callbacks(&mut input_source))
        {
            Ok(stream) => stream,
            Err(error) => {
                if mono_source == Some(1) && requested_input_channels < 2 {
                    return Err(format!(
                        "opening input '{}' with {input_channels} channels for the right-channel mono source: {error}",
                        settings.input.name
                    ));
                }
                return Err(format!("opening input '{}': {error}", settings.input.name));
            }
        };

        let input_latency = match backend.stream_latency(&input_stream) {
            Ok(latency) => latency,
            Err(error) => {
                backend.close_stream(input_stream);
                return Err(format!("reading input latency: {error}"));
            }
        };
        if input_latency.sample_rate == 0 {
            backend.close_stream(input_stream);
            return Err("the input device reports a zero sample rate".to_string());
        }
        let engine_rate = input_latency.sample_rate as f64;
        let block_size = if settings.buffer_size > 0 {
            settings.buffer_size
        } else if input_latency.buffer > 0 {
            input_latency.buffer
        } else {
            256
        }
        .clamp(16, MAX_FRAMES as u32);

        if is_cancelled() {
            backend.close_stream(input_stream);
            return Err(START_CANCELLED.to_string());
        }

        let mut second_open: Option<(bridge::InputSource, DeviceStream, DeviceLatency)> = None;
        if settings.sidechain_input_enabled {
            let second_channels = settings.sidechain_input.channels.clamp(1, 8);
            let mut second_source =
                match bridge::input_source(second_channels as usize, ring_frames) {
                    Ok(source) => source,
                    Err(error) => {
                        backend.close_stream(input_stream);
                        return Err(error);
                    }
                };
            let second_config = StreamConfig {
                device: settings.sidechain_input.name.clone(),
                sample_rate: settings.sample_rate,
                block_size: settings.buffer_size,
                channels: second_channels,
                api: settings.sidechain_input.api,
                direction: StreamDirection::Input,
                channel_mask: settings.sidechain_input.channel_mask,
                output_channels: 0,
                output_channel_mask: 0,
            };
            let second_stream = match backend
                .open_stream(&second_config, bridge::input_callbacks(&mut second_source))
            {
                Ok(stream) => stream,
                Err(error) => {
                    backend.close_stream(input_stream);
                    return Err(format!(
                        "opening sidechain input '{}': {error}",
                        settings.sidechain_input.name
                    ));
                }
            };
            let second_latency = match backend.stream_latency(&second_stream) {
                Ok(latency) => latency,
                Err(error) => {
                    backend.close_stream(second_stream);
                    backend.close_stream(input_stream);
                    return Err(format!("reading sidechain input latency: {error}"));
                }
            };
            if second_latency.sample_rate == 0 {
                backend.close_stream(second_stream);
                backend.close_stream(input_stream);
                return Err("the sidechain input device reports a zero sample rate".to_string());
            }
            if is_cancelled() {
                backend.close_stream(second_stream);
                backend.close_stream(input_stream);
                return Err(START_CANCELLED.to_string());
            }
            second_open = Some((second_source, second_stream, second_latency));
        }
        logging::line(format!(
            "audio session: input '{}' {} ch open in {:.1} ms",
            settings.input.name,
            input_channels,
            device_started.elapsed().as_secs_f64() * 1000.0
        ));

        if is_cancelled() {
            if let Some((_, stream, _)) = second_open.as_ref() {
                backend.close_stream(*stream);
            }
            backend.close_stream(input_stream);
            return Err(START_CANCELLED.to_string());
        }

        let plugin_started = Instant::now();
        let (mut plugins, processors, mut warnings) = match load_plugins(
            &backend,
            project,
            engine_rate,
            block_size,
            settings.sidechain_input_enabled,
        ) {
            Ok(result) => result,
            Err(error) => {
                if let Some((_, stream, _)) = second_open.as_ref() {
                    backend.close_stream(*stream);
                }
                backend.close_stream(input_stream);
                return Err(error);
            }
        };
        logging::line(format!(
            "audio session: {} plugins loaded and prepared in {:.1} ms",
            plugins.len(),
            plugin_started.elapsed().as_secs_f64() * 1000.0
        ));

        if is_cancelled() {
            free_plugins(&backend, &mut plugins);
            if let Some((_, stream, _)) = second_open.as_ref() {
                backend.close_stream(*stream);
            }
            backend.close_stream(input_stream);
            return Err(START_CANCELLED.to_string());
        }

        let engine_started = Instant::now();
        let plan = build_plan(
            &project.chain,
            &plugins,
            block_size as usize,
            engine_rate,
            mono_source,
        );
        let engine = match ChainEngine::new(plan, processors, block_size as usize) {
            Ok(engine) => engine,
            Err(error) => {
                free_plugins(&backend, &mut plugins);
                if let Some((_, stream, _)) = second_open.as_ref() {
                    backend.close_stream(*stream);
                }
                backend.close_stream(input_stream);
                return Err(format!("building the plugin chain: {error}"));
            }
        };
        let total_latency = engine.total_latency();
        logging::line(format!(
            "audio session: engine built in {:.2} ms, latency {} smp",
            engine_started.elapsed().as_secs_f64() * 1000.0,
            total_latency
        ));

        if is_cancelled() {
            free_plugins(&backend, &mut plugins);
            if let Some((_, stream, _)) = second_open.as_ref() {
                backend.close_stream(*stream);
            }
            backend.close_stream(input_stream);
            return Err(START_CANCELLED.to_string());
        }

        let output_started = Instant::now();
        let mut virtual_mic: Option<VirtualMicSink> = None;
        let mut vmic_packet_frames = 0usize;
        #[cfg(not(debug_assertions))]
        if settings.output_kind == OutputKind::VirtualMic {
            let message =
                "virtual microphone support is disabled in release builds; using the configured output"
                    .to_string();
            logging::line(format!("audio session: {message}"));
            warnings.push(message);
        }
        #[cfg(debug_assertions)]
        if virtual_mic_requested {
            let format = VmicFormat {
                sample_rate: michost_vmic::DEFAULT_FORMAT.sample_rate,
                channels: output_channels,
                packet_frames: 480,
            };
            match VirtualMicSink::open(format) {
                Ok(sink) => {
                    let actual = sink.format();
                    if (1..=2).contains(&actual.channels) {
                        output_channels = actual.channels;
                        vmic_packet_frames = actual.packet_frames as usize;
                        logging::line(format!(
                            "audio session: virtual microphone opened at {} Hz, {} ch, {} frame packets",
                            actual.sample_rate, actual.channels, actual.packet_frames
                        ));
                        virtual_mic = Some(sink);
                    } else {
                        let message = format!(
                            "virtual microphone unavailable (reported {} channels); using the configured output",
                            actual.channels
                        );
                        logging::line(format!("audio session: {message}"));
                        warnings.push(message);
                    }
                }
                Err(error) => {
                    let message = format!(
                        "virtual microphone unavailable ({error}); using the configured output"
                    );
                    logging::line(format!("audio session: {message}"));
                    warnings.push(message);
                }
            }
        }
        let virtual_mic_active = virtual_mic.is_some();
        let output_capacity = if virtual_mic_active {
            ring_frames.max((block_size as usize).saturating_mul(4))
        } else {
            ring_frames
        };
        let mut output_sink = match bridge::output_sink(output_channels as usize, output_capacity) {
            Ok(sink) => sink,
            Err(error) => {
                free_plugins(&backend, &mut plugins);
                if let Some((_, stream, _)) = second_open.as_ref() {
                    backend.close_stream(*stream);
                }
                backend.close_stream(input_stream);
                return Err(error);
            }
        };
        let (output_stream, output_latency, output_rate, output_device_name, output_target) =
            if virtual_mic_active {
                let format = virtual_mic
                    .as_ref()
                    .map(|sink| sink.format())
                    .unwrap_or(VmicFormat {
                        sample_rate: 48_000,
                        channels: output_channels,
                        packet_frames: 480,
                    });
                let sample_rate = format.sample_rate;
                let target =
                    bridge::virtual_mic_output_target(block_size as usize, vmic_packet_frames);
                let ring_target = bridge::virtual_mic_ring_target(
                    vmic_packet_frames,
                    virtual_mic
                        .as_ref()
                        .map(|sink| sink.ring_frames() as usize)
                        .unwrap_or(vmic_packet_frames),
                );
                let latency = DeviceLatency {
                    input: 0,
                    output: ring_target as u32,
                    buffer: 0,
                    sample_rate,
                };
                (
                    None,
                    latency,
                    sample_rate as f64,
                    "MicHost Virtual Microphone".to_string(),
                    target,
                )
            } else {
                let output_config = StreamConfig {
                    device: settings.output.name.clone(),
                    sample_rate: settings.sample_rate,
                    block_size: settings.output_buffer_size,
                    channels: output_channels,
                    api: settings.output.api,
                    direction: StreamDirection::Output,
                    channel_mask: settings.output.channel_mask,
                    output_channels: 0,
                    output_channel_mask: 0,
                };
                let output_stream = match backend
                    .open_stream(&output_config, bridge::output_callbacks(&mut output_sink))
                {
                    Ok(stream) => stream,
                    Err(error) => {
                        free_plugins(&backend, &mut plugins);
                        if let Some((_, stream, _)) = second_open.as_ref() {
                            backend.close_stream(*stream);
                        }
                        backend.close_stream(input_stream);
                        return Err(format!(
                            "opening output '{}': {error}",
                            settings.output.name
                        ));
                    }
                };
                let output_latency = match backend.stream_latency(&output_stream) {
                    Ok(latency) => latency,
                    Err(error) => {
                        backend.close_stream(output_stream);
                        free_plugins(&backend, &mut plugins);
                        if let Some((_, stream, _)) = second_open.as_ref() {
                            backend.close_stream(*stream);
                        }
                        backend.close_stream(input_stream);
                        return Err(format!("reading output latency: {error}"));
                    }
                };
                if output_latency.sample_rate == 0 {
                    backend.close_stream(output_stream);
                    free_plugins(&backend, &mut plugins);
                    if let Some((_, stream, _)) = second_open.as_ref() {
                        backend.close_stream(*stream);
                    }
                    backend.close_stream(input_stream);
                    return Err("the output device reports a zero sample rate".to_string());
                }
                if is_cancelled() {
                    backend.close_stream(output_stream);
                    free_plugins(&backend, &mut plugins);
                    if let Some((_, stream, _)) = second_open.as_ref() {
                        backend.close_stream(*stream);
                    }
                    backend.close_stream(input_stream);
                    return Err(START_CANCELLED.to_string());
                }
                let output_rate = output_latency.sample_rate as f64;
                let (output_margin, output_floor) = if settings.safety_buffer_ms > 0 {
                    let extra =
                        (output_rate * settings.safety_buffer_ms as f64 / 1000.0).round() as usize;
                    (extra, 128)
                } else if settings.output.api == AudioApi::Asio {
                    (128, 128)
                } else {
                    (512, 512)
                };
                let target = (block_size as usize)
                    .max(output_latency.buffer as usize + output_margin)
                    .max(output_floor)
                    .min(output_capacity / 2);
                (
                    Some(output_stream),
                    output_latency,
                    output_rate,
                    settings.output.name.clone(),
                    target,
                )
            };
        logging::line(format!(
            "audio session: output '{}' {} ch open in {:.1} ms",
            output_device_name,
            output_channels,
            output_started.elapsed().as_secs_f64() * 1000.0
        ));

        let second_endpoint = second_open.as_ref().map(|(_, _, latency)| EndpointInfo {
            device: settings.sidechain_input.name.clone(),
            api: settings.sidechain_input.api,
            channels: settings.sidechain_input.channels.clamp(1, 8),
            sample_rate: latency.sample_rate as f64,
            buffer_size: latency.buffer,
            latency: *latency,
        });
        let second_input =
            second_open
                .take()
                .map(|(source, stream, latency)| bridge::SecondInput {
                    stream,
                    source,
                    rate: latency.sample_rate as f64,
                    capacity: ring_frames,
                    buffer: latency.buffer,
                });

        let bridge_started = Instant::now();
        let bridge = match StreamBridge::start(
            backend.clone(),
            input_stream,
            output_stream,
            virtual_mic,
            input_source,
            output_sink,
            engine,
            engine_rate,
            output_rate,
            block_size as usize,
            output_target,
            second_input,
            settings.monitor_trim_db,
        ) {
            Ok(bridge) => bridge,
            Err(error) => {
                free_plugins(&backend, &mut plugins);
                return Err(format!("starting audio: {error}"));
            }
        };
        logging::line(format!(
            "audio session: streams started in {:.1} ms",
            bridge_started.elapsed().as_secs_f64() * 1000.0
        ));

        project.settings.input.channels = input_channels;
        project.settings.output.channels = output_channels;
        if settings.sidechain_input_enabled {
            project.settings.sidechain_input.channels =
                settings.sidechain_input.channels.clamp(1, 8);
        }
        logging::line(format!(
            "audio session: ready in {:.1} ms",
            started.elapsed().as_secs_f64() * 1000.0
        ));
        let mut session = Self {
            backend,
            bridge: Some(bridge),
            plugins,
            retired: Vec::new(),
            chain: project.chain.clone(),
            input: EndpointInfo {
                device: settings.input.name.clone(),
                api: settings.input.api,
                channels: input_channels,
                sample_rate: engine_rate,
                buffer_size: input_latency.buffer,
                latency: input_latency,
            },
            output: EndpointInfo {
                device: output_device_name,
                api: settings.output.api,
                channels: output_channels,
                sample_rate: output_rate,
                buffer_size: if virtual_mic_active {
                    vmic_packet_frames as u32
                } else {
                    output_latency.buffer
                },
                latency: output_latency,
            },
            second_input: second_endpoint,
            sample_rate: engine_rate,
            block_size,
            mono_source,
            total_latency,
            output_target,
            virtual_mic_active,
            capture_engine_ms: virtual_mic_active.then_some(10.0),
            warnings,
            stopped: false,
        };
        if let Some(monitor) = monitor
            && monitor.name != session.output.device
            && let Err(error) = session.warm_monitor(
                &monitor.name,
                monitor.api,
                monitor.channels,
                monitor.channel_mask,
            )
        {
            logging::line(format!("monitor: warm open failed: {error}"));
        }
        let latency = session.latency_breakdown();
        logging::line(format!(
            "audio latency: in {:.1}+{:.1} ms, graph {:.1}+{:.1} ms, out {:.1}+{:.1} ms, queue {:.1} ms = {:.1} ms",
            latency.input_device_ms,
            latency.input_buffer_ms,
            latency.graph_ms,
            latency.chain_ms,
            latency.output_device_ms,
            latency.output_buffer_ms,
            latency.output_queue_ms,
            latency.total_ms
        ));
        Ok(session)
    }

    pub fn stop(&mut self, project: Option<&mut Project>) {
        if self.stopped {
            return;
        }
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.stop();
        }
        if let Some(project) = project {
            self.export_plugin_states(project);
        }
        free_plugins(&self.backend, &mut self.plugins);
        free_plugins(&self.backend, &mut self.retired);
        self.chain.clear();
        self.bridge = None;
        self.stopped = true;
    }

    /// Exports the current plugin state without tearing down the session or
    /// freeing the plugin instances. Used by the fast quit path.
    /// Stops the audio streams (releasing ASIO/WASAPI drivers) but keeps the
    /// plugin instances alive so state can be exported and the process can exit.
    pub fn quiesce(&mut self, project: &mut Project) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.stop();
        }
        self.export_plugin_states(project);
    }

    pub fn export_plugin_states(&mut self, project: &mut Project) {
        for plugin in &self.plugins {
            if !project
                .chain
                .iter()
                .any(|entry| entry.id == plugin.entry_id)
            {
                continue;
            }
            if let Some(builtin) = &plugin.builtin {
                project.set_plugin_state(plugin.entry_id, builtin.params.encode());
                continue;
            }
            let Some(handle) = plugin.handle else {
                continue;
            };
            match self.backend.state_export(handle, StateKind::Component) {
                Ok(state) if !state.is_empty() => {
                    project.set_plugin_state(plugin.entry_id, state);
                }
                Ok(_) => {}
                Err(error) => self
                    .warnings
                    .push(format!("saving '{}' state: {error}", plugin.name)),
            }
        }
    }

    pub fn rebuild_engine(&mut self, project: &Project) -> Result<(), String> {
        self.publish_chain(project.chain.clone())
    }

    pub fn insert_prepared(
        &mut self,
        entry_id: u64,
        prepared: PreparedPlugin,
    ) -> Result<(), String> {
        self.register_prepared(entry_id, prepared)?;
        let mut provisional = false;
        if !self.chain.iter().any(|entry| entry.id == entry_id) {
            let plugin = self
                .plugins
                .iter()
                .find(|plugin| plugin.entry_id == entry_id)
                .ok_or_else(|| "the prepared plugin is not registered".to_string())?;
            self.chain.push(ChainEntry {
                id: entry_id,
                plugin: PluginSpec {
                    path: String::new(),
                    class_id: String::new(),
                    name: plugin.name.clone(),
                    vendor: String::new(),
                    main_inputs: plugin.main_inputs,
                    main_outputs: plugin.main_outputs,
                    sidechain_inputs: plugin.native_sidechain,
                },
                bypassed: false,
                sidechain_source: if plugin.active_sidechain > 0 {
                    SidechainSource::MainMic
                } else {
                    SidechainSource::Off
                },
                oversampling: 1,
            });
            provisional = true;
        }
        let chain = self.chain.clone();
        if let Err(error) = self.publish_chain(chain) {
            if provisional {
                self.chain.retain(|entry| entry.id != entry_id);
            }
            return Err(format!("publishing the chain after inserting: {error}"));
        }
        Ok(())
    }

    pub fn insert_prepared_with_project(
        &mut self,
        project: &Project,
        entry_id: u64,
        prepared: PreparedPlugin,
    ) -> Result<(), String> {
        if !project.chain.iter().any(|entry| entry.id == entry_id) {
            free_prepared(&self.backend, prepared);
            return Err(format!(
                "the project does not contain plugin entry {entry_id}"
            ));
        }
        self.register_prepared(entry_id, prepared)?;
        self.publish_chain(project.chain.clone())
    }

    pub fn retire_plugin(&mut self, entry_id: u64) {
        let Some(index) = self
            .plugins
            .iter()
            .position(|plugin| plugin.entry_id == entry_id)
        else {
            return;
        };
        let plugin = self.plugins.remove(index);
        if let Some(handle) = plugin.handle {
            let _ = self.backend.close_editor(handle);
        }
        self.retired.push(plugin);
        self.chain.retain(|entry| entry.id != entry_id);
        if self.bridge.is_none() {
            return;
        }
        let chain = self.chain.clone();
        if let Err(error) = self.publish_chain(chain) {
            logging::line(format!(
                "audio session: retiring plugin entry {entry_id}: {error}"
            ));
        }
    }

    fn register_prepared(&mut self, entry_id: u64, prepared: PreparedPlugin) -> Result<(), String> {
        if self.stopped || self.bridge.is_none() {
            free_prepared(&self.backend, prepared);
            return Err("audio is not running".to_string());
        }
        if self
            .plugins
            .iter()
            .any(|plugin| plugin.entry_id == entry_id)
            || self
                .retired
                .iter()
                .any(|plugin| plugin.entry_id == entry_id)
        {
            free_prepared(&self.backend, prepared);
            return Err(format!("plugin entry {entry_id} is already loaded"));
        }
        let PreparedPlugin {
            handle,
            name,
            main_inputs,
            main_outputs,
            native_sidechain,
            active_sidechain,
            latency_samples,
            memory_bytes,
            meter,
        } = prepared;
        self.plugins.push(LoadedPlugin {
            entry_id,
            handle: Some(handle),
            builtin: None,
            name,
            main_inputs,
            main_outputs,
            native_sidechain,
            active_sidechain,
            latency: latency_samples,
            memory_bytes,
            meter,
            oversampling: 1,
        });
        Ok(())
    }

    pub fn insert_builtin_with_project(
        &mut self,
        project: &Project,
        entry_id: u64,
        kind: BuiltinKind,
        params: Arc<ParamSet>,
        name: &str,
    ) -> Result<(), String> {
        if !cfg!(debug_assertions) {
            return Err("built-in effects are not included in release builds".to_string());
        }
        if self.stopped || self.bridge.is_none() {
            return Err("audio is not running".to_string());
        }
        if !project.chain.iter().any(|entry| entry.id == entry_id) {
            return Err(format!(
                "the project does not contain plugin entry {entry_id}"
            ));
        }
        if self
            .plugins
            .iter()
            .any(|plugin| plugin.entry_id == entry_id)
            || self
                .retired
                .iter()
                .any(|plugin| plugin.entry_id == entry_id)
        {
            return Err(format!("plugin entry {entry_id} is already loaded"));
        }
        let oversampling = michost_graph::normalize_oversampling(
            project
                .chain
                .iter()
                .find(|entry| entry.id == entry_id)
                .map(|entry| entry.oversampling)
                .unwrap_or(1),
        );
        let factor = f64::from(oversampling);
        let graph_rate = self.sample_rate * factor;
        let latency = plugin_latency_engine(kind.latency_samples(graph_rate), oversampling);
        let meter = Arc::new(PluginMeter::new());
        let analysis = AudioAnalysis::new(graph_rate as f32);
        self.plugins.push(LoadedPlugin {
            entry_id,
            handle: None,
            builtin: Some(BuiltinRuntime {
                kind,
                params,
                analysis,
            }),
            name: name.to_string(),
            main_inputs: 2,
            main_outputs: 2,
            native_sidechain: 0,
            active_sidechain: 0,
            latency,
            memory_bytes: None,
            meter,
            oversampling,
        });
        if let Err(error) = self.publish_chain(project.chain.clone()) {
            self.plugins.retain(|plugin| plugin.entry_id != entry_id);
            return Err(error);
        }
        Ok(())
    }

    fn publish_chain(&mut self, chain: Vec<ChainEntry>) -> Result<(), String> {
        if self.bridge.is_none() {
            return Err("audio is not running".to_string());
        }
        let engine = self.build_engine(&chain)?;
        let total_latency = engine.total_latency();
        let bridge = self
            .bridge
            .as_mut()
            .ok_or_else(|| "audio is not running".to_string())?;
        bridge.replace_engine(engine)?;
        self.total_latency = total_latency;
        self.chain = chain;
        Ok(())
    }

    fn build_engine(&self, chain: &[ChainEntry]) -> Result<ChainEngine, String> {
        let loaded: HashMap<u64, &LoadedPlugin> = self
            .plugins
            .iter()
            .map(|plugin| (plugin.entry_id, plugin))
            .collect();
        for entry in chain {
            if !loaded.contains_key(&entry.id) {
                return Err(format!(
                    "plugin '{}' is not loaded; prepare and insert it first",
                    entry.plugin.name
                ));
            }
        }
        let plan = build_plan(
            chain,
            &self.plugins,
            self.block_size as usize,
            self.sample_rate,
            self.mono_source,
        );
        let host: Arc<dyn PluginHost> = self.backend.clone();
        let mut processors: Vec<(u64, Box<dyn PluginProcessor>)> = Vec::new();
        for entry in chain {
            if entry.bypassed {
                continue;
            }
            let plugin = loaded
                .get(&entry.id)
                .ok_or_else(|| format!("plugin '{}' is not loaded", entry.plugin.name))?;
            let factor = michost_graph::normalize_oversampling(plugin.oversampling);
            let graph_rate = self.sample_rate * f64::from(factor);
            let channels = (plugin.main_inputs as usize + plugin.active_sidechain as usize)
                .max(plugin.main_outputs as usize);
            let processor: Box<dyn PluginProcessor> = if let Some(builtin) = &plugin.builtin {
                Box::new(BuiltinProcessor::with_analysis(
                    builtin.kind,
                    builtin.params.clone(),
                    graph_rate,
                    builtin.analysis.clone(),
                ))
            } else if let Some(handle) = plugin.handle {
                Box::new(BackendProcessor::new(host.clone(), handle))
            } else {
                return Err(format!("plugin '{}' has no processor", entry.plugin.name));
            };
            let processor = oversampled_processor(processor, factor, channels, self.block_size);
            processors.push((
                entry.id,
                Box::new(MeteredProcessor::new(
                    processor,
                    plugin.meter.clone(),
                    graph_rate,
                )),
            ));
        }
        ChainEngine::new(plan, processors, self.block_size as usize)
            .map_err(|error| error.to_string())
    }

    pub fn refresh_latencies(&mut self) -> Result<bool, String> {
        let mut changed = false;
        for plugin in &mut self.plugins {
            if plugin.builtin.is_some() {
                continue;
            }
            let Some(handle) = plugin.handle else {
                continue;
            };
            let graph_rate = self.sample_rate * f64::from(plugin.oversampling.max(1));
            let graph_latency = self
                .backend
                .plugin_latency(handle)
                .map_err(|error| format!("reading '{}' latency: {error}", plugin.name))?;
            let latency = plugin_latency_engine(graph_latency, plugin.oversampling);
            if graph_rate <= 0.0 {
                continue;
            }
            if latency != plugin.latency {
                plugin.latency = latency;
                changed = true;
            }
        }
        Ok(changed)
    }

    pub fn warm_monitor(
        &mut self,
        device: &str,
        api: AudioApi,
        channels: u32,
        channel_mask: u64,
    ) -> Result<(), String> {
        self.bridge
            .as_mut()
            .ok_or_else(|| "audio is not running".to_string())?
            .warm_monitor(device, api, channels, channel_mask)
    }

    pub fn start_monitor(
        &mut self,
        device: &str,
        api: AudioApi,
        channels: u32,
        channel_mask: u64,
    ) -> Result<(), String> {
        self.bridge
            .as_mut()
            .ok_or_else(|| "audio is not running".to_string())?
            .start_monitor(device, api, channels, channel_mask)
    }

    pub fn stop_monitor(&mut self) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.stop_monitor();
        }
    }

    pub fn close_monitor(&mut self) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.close_monitor();
        }
    }

    pub fn monitor_is_duplex(&self) -> bool {
        self.bridge
            .as_ref()
            .is_some_and(|bridge| bridge.monitor_is_duplex())
    }

    pub fn set_monitor_trim(&mut self, db: f32) {
        if let Some(bridge) = self.bridge.as_ref() {
            bridge.set_monitor_trim(db);
        }
    }

    pub fn poll_monitor(&mut self) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.poll_monitor();
        }
    }

    pub fn begin_chain_measurement(&mut self) -> Result<(), String> {
        self.bridge
            .as_mut()
            .ok_or_else(|| "audio is not running".to_string())?
            .begin_chain_measurement()
    }

    pub fn end_chain_measurement(&mut self) -> Vec<std::time::Instant> {
        self.bridge
            .as_mut()
            .map(StreamBridge::end_chain_measurement)
            .unwrap_or_default()
    }

    pub fn monitor_underruns(&self) -> u64 {
        self.bridge
            .as_ref()
            .map(StreamBridge::monitor_underruns)
            .unwrap_or(0)
    }

    pub fn close_editors(&self) {
        for plugin in &self.plugins {
            if let Some(handle) = plugin.handle {
                let _ = self.backend.close_editor(handle);
            }
        }
    }

    pub fn clear_plugin_edits(&self) {
        for plugin in &self.plugins {
            if let Some(builtin) = &plugin.builtin {
                builtin.params.take_edited();
            } else if let Some(handle) = plugin.handle {
                let _ = self.backend.take_plugin_edited(handle);
            }
        }
    }

    pub fn take_plugin_edits(&self, project: &mut Project) -> bool {
        let mut changed = false;
        for plugin in &self.plugins {
            if !project
                .chain
                .iter()
                .any(|entry| entry.id == plugin.entry_id)
            {
                continue;
            }
            if let Some(builtin) = &plugin.builtin {
                if builtin.params.take_edited() {
                    let state = builtin.params.encode();
                    if project.plugin_state(plugin.entry_id) != Some(state.as_slice()) {
                        project.set_plugin_state(plugin.entry_id, state);
                        changed = true;
                    }
                }
                continue;
            }
            let Some(handle) = plugin.handle else {
                continue;
            };
            // Heavy editors (e.g. iZotope RX 11) can signal edits
            // continuously. Keep the edit pending and export once the editor
            // closes, or on stop/quit after the bridge has been stopped.
            if self.backend.editor_open(handle).unwrap_or(false) {
                continue;
            }
            match self.backend.take_plugin_edited(handle) {
                Ok(true) => match self.backend.state_export(handle, StateKind::Component) {
                    Ok(state) if !state.is_empty() => {
                        if project.plugin_state(plugin.entry_id) != Some(state.as_slice()) {
                            project.set_plugin_state(plugin.entry_id, state);
                            changed = true;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => logging::line(format!(
                        "saving '{}' state after an edit: {error}",
                        plugin.name
                    )),
                },
                Ok(false) => {}
                Err(error) => logging::line(format!(
                    "reading the edit flag for '{}': {error}",
                    plugin.name
                )),
            }
        }
        changed
    }

    pub fn open_editor(&self, entry_id: u64) -> Result<(), String> {
        let plugin = self
            .plugins
            .iter()
            .find(|plugin| plugin.entry_id == entry_id)
            .ok_or_else(|| "the plugin is not loaded".to_string())?;
        let Some(handle) = plugin.handle else {
            return Err("built-in plugins use the MicHost editor window".to_string());
        };
        self.backend
            .open_editor(handle)
            .map_err(|error| format!("opening '{}': {error}", plugin.name))
    }

    pub fn builtin(
        &self,
        entry_id: u64,
    ) -> Option<(BuiltinKind, Arc<ParamSet>, Arc<AudioAnalysis>)> {
        self.plugins
            .iter()
            .find(|plugin| plugin.entry_id == entry_id)
            .and_then(|plugin| plugin.builtin.as_ref())
            .map(|builtin| {
                (
                    builtin.kind,
                    builtin.params.clone(),
                    builtin.analysis.clone(),
                )
            })
    }

    pub fn plugin_latency(&self, entry_id: u64) -> Option<u32> {
        self.plugins
            .iter()
            .find(|plugin| plugin.entry_id == entry_id)
            .map(|plugin| plugin.latency)
    }

    pub fn plugin_latency_ms(&self, entry_id: u64) -> Option<f64> {
        samples_to_milliseconds(self.plugin_latency(entry_id)?, self.sample_rate)
    }

    pub fn input_lufs(&self) -> f32 {
        self.bridge
            .as_ref()
            .map(StreamBridge::input_lufs)
            .unwrap_or(-120.0)
    }

    pub fn output_lufs(&self) -> f32 {
        self.bridge
            .as_ref()
            .map(StreamBridge::output_lufs)
            .unwrap_or(-120.0)
    }

    pub fn plugin_cpu_fraction(&self, entry_id: u64) -> Option<f32> {
        self.plugins
            .iter()
            .find(|plugin| plugin.entry_id == entry_id)
            .map(|plugin| plugin.meter.cpu_fraction())
    }

    pub fn plugin_memory_bytes(&self, entry_id: u64) -> Option<u64> {
        self.plugins
            .iter()
            .find(|plugin| plugin.entry_id == entry_id)
            .and_then(|plugin| plugin.memory_bytes)
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn control_panel(&self, input: bool) -> Result<(), String> {
        self.bridge
            .as_ref()
            .ok_or_else(|| "audio is not running".to_string())?
            .control_panel(input)
    }

    pub fn collect_retired(&mut self) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.collect_retired();
        }
    }

    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    pub fn total_latency(&self) -> u32 {
        self.total_latency
    }

    pub fn input(&self) -> &EndpointInfo {
        &self.input
    }

    pub fn output(&self) -> &EndpointInfo {
        &self.output
    }

    pub fn is_virtual_mic_output(&self) -> bool {
        self.virtual_mic_active
    }

    pub fn capture_engine_ms(&self) -> Option<f64> {
        self.capture_engine_ms
    }

    pub fn virtual_mic_stats(&self) -> bridge::BridgeStats {
        self.bridge
            .as_ref()
            .map(StreamBridge::stats)
            .unwrap_or_default()
    }

    pub fn second_input(&self) -> Option<&EndpointInfo> {
        self.second_input.as_ref()
    }

    fn static_latency_milliseconds(&self) -> f64 {
        if self.sample_rate <= 0.0 || self.output.sample_rate <= 0.0 {
            return 0.0;
        }
        let input_ms = (self.input.latency.input + self.input.latency.buffer) as f64 * 1000.0
            / self.input.sample_rate.max(1.0);
        let graph_ms =
            (self.block_size as f64 + self.total_latency as f64) * 1000.0 / self.sample_rate;
        let output_ms = (self.output.latency.output + self.output.latency.buffer) as f64 * 1000.0
            / self.output.sample_rate;
        input_ms + graph_ms + output_ms
    }

    pub fn end_to_end_milliseconds(&self) -> f64 {
        self.static_latency_milliseconds()
            + self.output_target as f64 * 1000.0 / self.output.sample_rate.max(1.0)
    }

    pub fn latency_breakdown(&self) -> LatencyBreakdown {
        let input_rate = self.input.sample_rate.max(1.0);
        let output_rate = self.output.sample_rate.max(1.0);
        let engine_rate = self.sample_rate.max(1.0);
        let input_device_ms = self.input.latency.input as f64 * 1000.0 / input_rate;
        let input_buffer_ms = self.input.latency.buffer as f64 * 1000.0 / input_rate;
        let graph_ms = self.block_size as f64 * 1000.0 / engine_rate;
        let chain_ms = self.total_latency as f64 * 1000.0 / engine_rate;
        let output_device_ms = self.output.latency.output as f64 * 1000.0 / output_rate;
        let output_buffer_ms = self.output.latency.buffer as f64 * 1000.0 / output_rate;
        let output_queue_ms = self.output_target as f64 * 1000.0 / output_rate;
        LatencyBreakdown {
            input_device_ms,
            input_buffer_ms,
            graph_ms,
            chain_ms,
            output_device_ms,
            output_buffer_ms,
            output_queue_ms,
            total_ms: input_device_ms
                + input_buffer_ms
                + graph_ms
                + chain_ms
                + output_device_ms
                + output_buffer_ms
                + output_queue_ms,
        }
    }

    pub fn current_end_to_end_milliseconds(&self) -> f64 {
        self.static_latency_milliseconds()
            + self.output_fill() as f64 * 1000.0 / self.output.sample_rate.max(1.0)
    }

    pub fn queue_milliseconds(&self) -> f64 {
        self.output_fill() as f64 * 1000.0 / self.output.sample_rate.max(1.0)
    }

    pub fn target_queue_milliseconds(&self) -> f64 {
        self.output_target as f64 * 1000.0 / self.output.sample_rate.max(1.0)
    }

    pub fn cpu_usage(&self) -> f32 {
        self.bridge
            .as_ref()
            .map(StreamBridge::cpu_usage)
            .unwrap_or(0.0)
    }

    pub fn frames_processed(&self) -> u64 {
        self.bridge
            .as_ref()
            .map(StreamBridge::frames_processed)
            .unwrap_or(0)
    }

    pub fn input_dropouts(&self) -> u64 {
        self.bridge
            .as_ref()
            .map(StreamBridge::input_dropouts)
            .unwrap_or(0)
    }

    pub fn output_underruns(&self) -> u64 {
        self.bridge
            .as_ref()
            .map(StreamBridge::output_underruns)
            .unwrap_or(0)
    }

    pub fn second_input_dropouts(&self) -> u64 {
        self.bridge
            .as_ref()
            .map(StreamBridge::second_input_dropouts)
            .unwrap_or(0)
    }

    pub fn second_input_starved(&self) -> u64 {
        self.bridge
            .as_ref()
            .map(StreamBridge::second_input_starved)
            .unwrap_or(0)
    }

    pub fn output_fill(&self) -> usize {
        self.bridge
            .as_ref()
            .map(StreamBridge::output_fill)
            .unwrap_or(0)
    }
}

impl Drop for AudioSession {
    fn drop(&mut self) {
        self.stop(None);
    }
}

type LoadedChain = (
    Vec<LoadedPlugin>,
    Vec<(u64, Box<dyn PluginProcessor>)>,
    Vec<String>,
);

fn scale_latency(samples: u32, factor: u8) -> u32 {
    let factor = u32::from(factor.max(1));
    if factor == 1 {
        samples
    } else {
        samples.div_ceil(factor)
    }
}

fn plugin_latency_engine(graph_latency: u32, factor: u8) -> u32 {
    let factor = michost_graph::normalize_oversampling(factor);
    scale_latency(graph_latency, factor).saturating_add(
        crate::oversampling::Oversampler::latency_samples(factor as usize),
    )
}

fn oversampled_processor(
    processor: Box<dyn PluginProcessor>,
    factor: u8,
    channels: usize,
    block_size: u32,
) -> Box<dyn PluginProcessor> {
    let factor = michost_graph::normalize_oversampling(factor);
    if factor <= 1 {
        processor
    } else {
        Box::new(crate::oversampling::OversampledProcessor::new(
            processor,
            factor,
            channels,
            block_size as usize,
        ))
    }
}

fn load_plugins(
    backend: &Arc<JuceBackend>,
    project: &mut Project,
    sample_rate: f64,
    block_size: u32,
    second_input_enabled: bool,
) -> Result<LoadedChain, String> {
    let host: Arc<dyn PluginHost> = backend.clone();
    let states = project.plugin_states.clone();
    let mut loaded: Vec<LoadedPlugin> = Vec::new();
    let mut processors: Vec<(u64, Box<dyn PluginProcessor>)> = Vec::new();
    let mut warnings = Vec::new();

    if !cfg!(debug_assertions) {
        for name in project.remove_builtin_entries() {
            warnings.push(format!(
                "'{name}' is a built-in effect; built-ins are not included in release builds"
            ));
        }
    }

    for entry in &mut project.chain {
        let name = entry.plugin.name.clone();
        let oversampling = michost_graph::normalize_oversampling(entry.oversampling);
        entry.oversampling = oversampling;
        let factor = f64::from(oversampling);
        let graph_rate = sample_rate * factor;
        let graph_block = block_size.saturating_mul(u32::from(oversampling));
        if let Some(kind) = BuiltinKind::from_path(&entry.plugin.path) {
            if entry.sidechain_source.is_active() {
                warnings.push(format!(
                    "'{name}' is a built-in plugin without a sidechain input; sidechain disabled"
                ));
                entry.sidechain_source = SidechainSource::Off;
            }
            entry.plugin.main_inputs = 2;
            entry.plugin.main_outputs = 2;
            entry.plugin.sidechain_inputs = 0;

            let params = ParamSet::new(kind);
            if let Some(state) = states.get(&entry.id) {
                params.decode(&state.component);
            }
            let analysis = AudioAnalysis::new(graph_rate as f32);
            let entry_id = entry.id;
            let bypassed = entry.bypassed;
            let latency = plugin_latency_engine(kind.latency_samples(graph_rate), oversampling);
            let meter = Arc::new(PluginMeter::new());
            loaded.push(LoadedPlugin {
                entry_id,
                handle: None,
                builtin: Some(BuiltinRuntime {
                    kind,
                    params: params.clone(),
                    analysis: analysis.clone(),
                }),
                name,
                main_inputs: 2,
                main_outputs: 2,
                native_sidechain: 0,
                active_sidechain: 0,
                latency,
                memory_bytes: None,
                meter: meter.clone(),
                oversampling,
            });
            if !bypassed {
                let processor = oversampled_processor(
                    Box::new(BuiltinProcessor::with_analysis(
                        kind, params, graph_rate, analysis,
                    )),
                    oversampling,
                    2,
                    block_size,
                );
                processors.push((
                    entry_id,
                    Box::new(MeteredProcessor::new(processor, meter, graph_rate)),
                ));
            }
            continue;
        }
        let plugin_started = Instant::now();
        let memory_before = process_memory_bytes();
        set_startup_progress(format!("loading {name}..."));
        let load_started = Instant::now();
        let handle = match backend.load_plugin_named(
            &entry.plugin.path,
            &entry.plugin.class_id,
            &entry.plugin.name,
        ) {
            Ok(handle) => handle,
            Err(error) => {
                free_plugins(backend, &mut loaded);
                return Err(format!("loading '{name}': {error}"));
            }
        };
        let load_ms = load_started.elapsed().as_secs_f64() * 1000.0;
        let native = match backend.plugin_channels(handle) {
            Ok(channels) => channels,
            Err(error) => {
                let _ = backend.free_plugin(handle);
                free_plugins(backend, &mut loaded);
                return Err(format!("reading '{name}' channels: {error}"));
            }
        };
        entry.plugin.main_inputs = native.main_inputs;
        entry.plugin.main_outputs = native.main_outputs;
        entry.plugin.sidechain_inputs = native.sidechain_inputs;

        let mut active_sidechain = 0u16;
        if !second_input_enabled
            && matches!(entry.sidechain_source, SidechainSource::SecondInput(_))
        {
            warnings.push(format!(
                "'{name}' uses the sidechain input device, which is disabled; sidechain disabled"
            ));
            entry.sidechain_source = SidechainSource::Off;
        }
        if entry.sidechain_source.is_active() {
            if native.sidechain_inputs > 0 {
                active_sidechain = native.sidechain_inputs;
            } else {
                warnings.push(format!(
                    "'{name}' has no sidechain input; sidechain disabled"
                ));
                entry.sidechain_source = SidechainSource::Off;
            }
        }

        let prepare_started = Instant::now();
        if let Err(error) = backend.prepare_plugin(
            handle,
            graph_rate,
            graph_block,
            native.main_inputs as u32,
            native.main_outputs as u32,
            active_sidechain as u32,
        ) {
            warnings.push(format!("preparing '{name}': {error}"));
            entry.sidechain_source = SidechainSource::Off;
            active_sidechain = 0;
            if let Err(error) = backend.prepare_plugin(
                handle,
                sample_rate,
                block_size,
                native.main_inputs as u32,
                native.main_outputs as u32,
                0,
            ) {
                let _ = backend.free_plugin(handle);
                free_plugins(backend, &mut loaded);
                return Err(format!("preparing '{name}': {error}"));
            }
        }

        let prepare_ms = prepare_started.elapsed().as_secs_f64() * 1000.0;
        let prepared = backend.plugin_channels(handle).unwrap_or(native);
        entry.plugin.main_inputs = prepared.main_inputs;
        entry.plugin.main_outputs = prepared.main_outputs;
        entry.plugin.sidechain_inputs = prepared.sidechain_inputs.max(native.sidechain_inputs);
        if active_sidechain > prepared.sidechain_inputs {
            active_sidechain = prepared.sidechain_inputs;
        }

        let state_started = Instant::now();
        if let Some(state) = states.get(&entry.id) {
            if !state.component.is_empty() {
                if let Err(error) =
                    backend.state_import(handle, StateKind::Component, &state.component)
                {
                    warnings.push(format!("restoring '{name}' state: {error}"));
                }
            }
        }

        let state_ms = state_started.elapsed().as_secs_f64() * 1000.0;
        let activate_started = Instant::now();
        if let Err(error) = backend.activate_plugin(handle, true) {
            let _ = backend.free_plugin(handle);
            free_plugins(backend, &mut loaded);
            return Err(format!("activating '{name}': {error}"));
        }

        let graph_latency = match backend.plugin_latency(handle) {
            Ok(latency) => latency,
            Err(error) => {
                let _ = backend.free_plugin(handle);
                free_plugins(backend, &mut loaded);
                return Err(format!("reading '{name}' latency: {error}"));
            }
        };

        let activate_ms = activate_started.elapsed().as_secs_f64() * 1000.0;
        logging::line(format!(
            "audio session: '{}' ready in {:.1} ms ({}x; load {:.1}, prepare {:.1}, state {:.1}, activate {:.1})",
            name,
            plugin_started.elapsed().as_secs_f64() * 1000.0,
            oversampling,
            load_ms,
            prepare_ms,
            state_ms,
            activate_ms
        ));
        let entry_id = entry.id;
        let bypassed = entry.bypassed;
        let latency = plugin_latency_engine(graph_latency, oversampling);
        let memory_bytes = match (memory_before, process_memory_bytes()) {
            (Some(before), Some(after)) => Some(after.saturating_sub(before)),
            _ => None,
        };
        let meter = Arc::new(PluginMeter::new());
        loaded.push(LoadedPlugin {
            entry_id,
            handle: Some(handle),
            builtin: None,
            name,
            main_inputs: entry.plugin.main_inputs,
            main_outputs: entry.plugin.main_outputs,
            native_sidechain: native.sidechain_inputs,
            active_sidechain,
            latency,
            memory_bytes,
            meter: meter.clone(),
            oversampling,
        });
        if !bypassed {
            let host = host.clone();
            let channels = (entry.plugin.main_inputs as usize + active_sidechain as usize)
                .max(entry.plugin.main_outputs as usize);
            let processor = oversampled_processor(
                Box::new(BackendProcessor::new(host, handle)),
                oversampling,
                channels,
                block_size,
            );
            processors.push((
                entry_id,
                Box::new(MeteredProcessor::new(processor, meter, graph_rate)),
            ));
        }
    }

    clear_startup_progress();
    Ok((loaded, processors, warnings))
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_plugin_for_insert(
    backend: Arc<JuceBackend>,
    path: &str,
    class_id: &str,
    name: &str,
    sample_rate: f64,
    block_size: u32,
    sidechain_channels: u16,
    state: Option<&[u8]>,
) -> Result<PreparedPlugin, String> {
    let started = Instant::now();
    let memory_before = process_memory_bytes();
    let handle = backend
        .load_plugin(path, class_id)
        .map_err(|error| format!("loading '{name}': {error}"))?;
    let native = match backend.plugin_channels(handle) {
        Ok(channels) => channels,
        Err(error) => {
            let _ = backend.free_plugin(handle);
            return Err(format!("reading '{name}' channels: {error}"));
        }
    };
    let load_ms = started.elapsed().as_secs_f64() * 1000.0;

    let mut active_sidechain = if sidechain_channels > 0 && native.sidechain_inputs > 0 {
        native.sidechain_inputs
    } else {
        0
    };
    let prepare_started = Instant::now();
    if let Err(error) = backend.prepare_plugin(
        handle,
        sample_rate,
        block_size,
        native.main_inputs as u32,
        native.main_outputs as u32,
        active_sidechain as u32,
    ) {
        if active_sidechain > 0 {
            active_sidechain = 0;
            if let Err(error) = backend.prepare_plugin(
                handle,
                sample_rate,
                block_size,
                native.main_inputs as u32,
                native.main_outputs as u32,
                0,
            ) {
                let _ = backend.free_plugin(handle);
                return Err(format!("preparing '{name}': {error}"));
            }
        } else {
            let _ = backend.free_plugin(handle);
            return Err(format!("preparing '{name}': {error}"));
        }
    }
    let prepare_ms = prepare_started.elapsed().as_secs_f64() * 1000.0;

    let prepared = backend.plugin_channels(handle).unwrap_or(native);
    let main_inputs = prepared.main_inputs;
    let main_outputs = prepared.main_outputs;
    let native_sidechain = prepared.sidechain_inputs.max(native.sidechain_inputs);
    if active_sidechain > prepared.sidechain_inputs {
        active_sidechain = prepared.sidechain_inputs;
    }

    if let Some(state) = state {
        if !state.is_empty() {
            if let Err(error) = backend.state_import(handle, StateKind::Component, state) {
                logging::line(format!("audio session: restoring '{name}' state: {error}"));
            }
        }
    }

    if let Err(error) = backend.activate_plugin(handle, true) {
        let _ = backend.free_plugin(handle);
        return Err(format!("activating '{name}': {error}"));
    }

    let latency_samples = match backend.plugin_latency(handle) {
        Ok(latency) => latency,
        Err(error) => {
            let _ = backend.free_plugin(handle);
            return Err(format!("reading '{name}' latency: {error}"));
        }
    };
    let memory_bytes = match (memory_before, process_memory_bytes()) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };
    logging::line(format!(
        "audio session: prepared '{name}' in {:.1} ms (load {load_ms:.1} ms, prepare {prepare_ms:.1} ms)",
        started.elapsed().as_secs_f64() * 1000.0
    ));
    Ok(PreparedPlugin {
        handle,
        name: name.to_string(),
        main_inputs,
        main_outputs,
        native_sidechain,
        active_sidechain,
        latency_samples,
        memory_bytes,
        meter: Arc::new(PluginMeter::new()),
    })
}

#[allow(dead_code)]
pub fn discard_prepared(backend: &JuceBackend, prepared: PreparedPlugin) {
    free_prepared(backend, prepared);
}

fn free_prepared(backend: &JuceBackend, prepared: PreparedPlugin) {
    let _ = backend.close_editor(prepared.handle);
    let _ = backend.free_plugin(prepared.handle);
}

#[cfg(windows)]
fn process_memory_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    (ok != 0).then_some(counters.WorkingSetSize as u64)
}

#[cfg(not(windows))]
fn process_memory_bytes() -> Option<u64> {
    None
}

fn free_plugins(backend: &Arc<JuceBackend>, plugins: &mut Vec<LoadedPlugin>) {
    for plugin in plugins.drain(..) {
        if let Some(handle) = plugin.handle {
            let _ = backend.close_editor(handle);
            let _ = backend.free_plugin(handle);
        }
    }
}

fn build_plan(
    entries: &[ChainEntry],
    plugins: &[LoadedPlugin],
    block_size: usize,
    sample_rate: f64,
    mono_source: Option<u8>,
) -> ChainPlan {
    let active: HashMap<u64, &LoadedPlugin> = plugins
        .iter()
        .map(|plugin| (plugin.entry_id, plugin))
        .collect();
    let nodes = entries
        .iter()
        .map(|entry| {
            let plugin = active.get(&entry.id);
            let sidechain_channels = if entry.sidechain_source.is_active() {
                plugin.map(|plugin| plugin.active_sidechain).unwrap_or(0)
            } else {
                0
            };
            PlanNode {
                id: entry.id,
                main_inputs: plugin
                    .map(|plugin| plugin.main_inputs)
                    .unwrap_or(entry.plugin.main_inputs),
                main_outputs: plugin
                    .map(|plugin| plugin.main_outputs)
                    .unwrap_or(entry.plugin.main_outputs),
                sidechain_channels,
                sidechain_source: entry.sidechain_source,
                bypassed: entry.bypassed,
                latency: plugin.map(|plugin| plugin.latency).unwrap_or(0),
            }
        })
        .collect();
    let mut plan = ChainPlan::from_nodes(nodes, block_size);
    plan.set_processing_options(sample_rate, mono_source);
    plan
}
