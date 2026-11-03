use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use egui::viewport::ViewportCommand;
use michost_builtin::{BuiltinKind, ParamSet};
use michost_graph::{ChainEntry, PluginSpec, SidechainSource};
use michost_host_abi::{
    AudioApi, DeviceInfo, DeviceKind, MH_NOT_SUPPORTED, PluginDescriptor, StreamHost,
};
use michost_host_juce::JuceBackend;
use michost_project::{DeviceSelection, MonoSource, OutputKind, PROJECT_EXTENSION, Project};
use michost_vmic::VmicClient;

use crate::activation::Activation;
use crate::audio_session::{
    AudioSession, PreparedPlugin, discard_prepared, prepare_plugin_for_insert,
};
use crate::autostart;
use crate::builtin_ui::{self, BuiltinEditor};
use crate::device_cache::{self, DeviceCache};
use crate::latency;
use crate::logging;
use crate::plugin_cache::{self, PluginCache, ScanUpdate};
use crate::project_state;
use crate::tray::{Tray, TrayCommand};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngineState {
    Stopped,
    Starting,
    Running,
    Error,
}

impl EngineState {
    fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::Error => "Error",
        }
    }

    fn color(self) -> egui::Color32 {
        match self {
            Self::Stopped => egui::Color32::from_rgb(150, 155, 165),
            Self::Starting => egui::Color32::from_rgb(230, 180, 80),
            Self::Running => egui::Color32::from_rgb(110, 210, 140),
            Self::Error => egui::Color32::from_rgb(235, 105, 105),
        }
    }
}

#[derive(Default)]
struct ScanState {
    receiver: Option<Receiver<ScanUpdate>>,
    active: bool,
    done: usize,
    total: usize,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PickerGrouping {
    #[default]
    Vendor,
    Category,
}

struct PickerCache {
    generation: u64,
    search: String,
    grouping: PickerGrouping,
    groups: BTreeMap<String, Vec<(usize, u32)>>,
    shown: usize,
}

#[derive(Default)]
struct AddState {
    open: bool,
    search: String,
    focus_search: bool,
    grouping: PickerGrouping,
    picker: Option<PickerCache>,
}

type SessionStartResult = Result<(AudioSession, Project), String>;

const START_RETRY_DELAY: Duration = Duration::from_secs(3);
const ADD_PLUGIN_BUTTON_HEIGHT: f32 = 28.0;
const POINTER_REPAINT_INTERVAL: Duration = Duration::from_millis(40);

struct PendingAdd {
    name: String,
    spec: PluginSpec,
    session_epoch: u64,
    receiver: Receiver<Result<PreparedPlugin, String>>,
}

enum ChainAction {
    Move(usize, usize),
    Remove(usize),
    Bypass(usize, bool),
    Oversampling(usize, u8),
    Sidechain(usize, SidechainSource),
    OpenEditor(usize),
    Add(PluginDescriptor),
}

pub struct MicHostApp {
    backend: Option<Arc<JuceBackend>>,
    backend_error: Option<String>,
    project: Project,
    project_path: Option<PathBuf>,
    session: Option<AudioSession>,
    session_worker: Option<Receiver<SessionStartResult>>,
    cancel_start: bool,
    cancel_flag: Arc<AtomicBool>,
    state: EngineState,
    last_error: Option<String>,
    warnings: Vec<String>,
    devices: DeviceCache,
    vmic_installed: bool,
    device_worker: Option<Receiver<Result<DeviceCache, String>>>,
    device_error: Option<String>,
    device_change_pending: bool,
    device_change_quiet_until: Instant,
    plugins: Vec<PluginDescriptor>,
    vst3_paths: Vec<String>,
    disabled_vst3_paths: Vec<String>,
    show_vst3_folders: bool,
    plugin_generation: u64,
    scan: ScanState,
    tray: Option<Tray>,
    activation: Option<Activation>,
    add: AddState,
    dirty: bool,
    save_deadline: Option<Instant>,
    plugin_autosave_at: Instant,
    saved_flash_until: Option<Instant>,
    latency_timer: Instant,
    engine_paused: bool,
    auto_retry_at: Option<Instant>,
    pending_add: Option<PendingAdd>,
    next_plugin_id: u64,
    session_epoch: u64,
    created_at: Instant,
    first_frame_logged: bool,
    hidden: bool,
    monitor: bool,
    show_monitor_settings: bool,
    auto_measure: bool,
    last_input_buffer: Option<(String, AudioApi, u32)>,
    last_output_buffer: Option<(String, AudioApi, u32)>,
    measuring: bool,
    plugin_edit_at: Instant,
    measure_worker: Option<mpsc::Receiver<Result<latency::Capture, String>>>,
    measured: Option<latency::Measurement>,
    builtin_editors: Vec<BuiltinEditor>,
    quitting: bool,
    tray_running: bool,
    tooltip: String,
    #[cfg(debug_assertions)]
    screenshot_path: Option<std::path::PathBuf>,
    #[cfg(debug_assertions)]
    screenshot_ready_at: Option<Instant>,
    #[cfg(debug_assertions)]
    screenshot_deadline: Instant,
    #[cfg(debug_assertions)]
    screenshot_last_request: Option<Instant>,
    last_frame: Instant,
}

impl MicHostApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        show_on_start: bool,
        screenshot_path: Option<std::path::PathBuf>,
        show_folders: bool,
        show_monitor: bool,
        auto_measure: bool,
    ) -> Self {
        #[cfg(not(debug_assertions))]
        let _ = screenshot_path;
        let created_at = Instant::now();
        cc.egui_ctx.options_mut(|options| {
            options.zoom_with_keyboard = false;
        });
        cc.egui_ctx.set_zoom_factor(1.0);
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.interact_size.y = 22.0;
        style.spacing.scroll.floating = false;
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating_width = 8.0;
        cc.egui_ctx.set_style(style);

        let (project, project_path) = load_startup_project();
        let (backend, backend_error) = match JuceBackend::load() {
            Ok(backend) => (Some(Arc::new(backend)), None),
            Err(error) => {
                logging::line(format!("loading mic_host.dll: {error}"));
                (
                    None,
                    Some(format!(
                        "mic_host.dll could not be loaded: {error}. Build cpp/ or set MIC_HOST_DLL."
                    )),
                )
            }
        };
        let devices = DeviceCache::load().unwrap_or_default();
        let plugin_cache = PluginCache::load();
        let plugins = plugin_cache
            .as_ref()
            .map(|cache| cache.descriptors.clone())
            .unwrap_or_default();
        let (vst3_paths, disabled_vst3_paths) = plugin_cache
            .map(|cache| (cache.vst3_paths, cache.disabled_vst3_paths))
            .unwrap_or_default();
        let tray = match Tray::new(cc.egui_ctx.clone()) {
            Ok(tray) => Some(tray),
            Err(error) => {
                logging::line(format!("tray icon: {error}"));
                None
            }
        };
        let activation = match Activation::create(cc.egui_ctx.clone()) {
            Ok(activation) => Some(activation),
            Err(error) => {
                logging::line(format!("activation event: {error}"));
                None
            }
        };

        let next_plugin_id = project.next_plugin_id();
        let monitor_enabled = project.settings.monitor_enabled;
        let mut app = Self {
            backend,
            backend_error,
            project,
            project_path,
            session: None,
            session_worker: None,
            cancel_start: false,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            state: EngineState::Stopped,
            last_error: None,
            warnings: Vec::new(),
            devices,
            vmic_installed: cfg!(debug_assertions) && VmicClient::is_installed(),
            device_worker: None,
            device_error: None,
            device_change_pending: false,
            device_change_quiet_until: Instant::now() + Duration::from_secs(2),
            plugins,
            vst3_paths,
            disabled_vst3_paths,
            show_vst3_folders: show_folders,
            plugin_generation: 0,
            scan: ScanState::default(),
            tray,
            activation,
            add: AddState::default(),
            dirty: false,
            save_deadline: None,
            plugin_autosave_at: Instant::now(),
            saved_flash_until: None,
            latency_timer: Instant::now(),
            engine_paused: false,
            auto_retry_at: None,
            pending_add: None,
            next_plugin_id,
            session_epoch: 0,
            created_at,
            first_frame_logged: false,
            hidden: !show_on_start,
            monitor: monitor_enabled,
            show_monitor_settings: show_monitor,
            auto_measure,
            last_input_buffer: None,
            last_output_buffer: None,
            measuring: false,
            plugin_edit_at: Instant::now(),
            measure_worker: None,
            measured: None,
            builtin_editors: Vec::new(),
            quitting: false,
            tray_running: false,
            tooltip: "MicHost - stopped".to_string(),
            #[cfg(debug_assertions)]
            screenshot_path,
            #[cfg(debug_assertions)]
            screenshot_ready_at: None,
            #[cfg(debug_assertions)]
            screenshot_deadline: Instant::now() + Duration::from_secs(20),
            #[cfg(debug_assertions)]
            screenshot_last_request: None,
            last_frame: Instant::now(),
        };
        app.project.settings.launch_at_login = autostart::is_enabled();
        if app.project.settings.input.name.is_empty() || app.project.settings.output.name.is_empty()
        {
            app.refresh_devices();
        }
        if app.apply_automatic_apis() {
            app.mark_dirty();
        }
        if !show_on_start && app.can_start() {
            app.start_audio();
        }
        if app.tray.is_none() && app.hidden {
            app.hidden = false;
            cc.egui_ctx
                .send_viewport_cmd(ViewportCommand::Visible(true));
        }
        logging::line(format!(
            "app constructed in {:.1} ms",
            app.created_at.elapsed().as_secs_f64() * 1000.0
        ));
        app
    }

    fn refresh_devices(&mut self) {
        self.measured = None;
        if self.device_worker.is_some() || self.session_worker.is_some() {
            return;
        }
        if self.session.is_some() {
            self.stop_audio();
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        self.device_error = None;
        self.device_worker = Some(device_cache::refresh(backend));
    }

    fn poll_devices(&mut self) {
        let Some(receiver) = self.device_worker.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(cache)) => {
                self.devices = cache;
                self.vmic_installed = cfg!(debug_assertions) && VmicClient::is_installed();
                self.device_error = None;
                self.device_change_quiet_until = Instant::now() + Duration::from_secs(2);
                if self.apply_automatic_apis() {
                    self.mark_dirty();
                }
                self.apply_device_defaults();
            }
            Ok(Err(error)) => {
                self.device_error = Some(error);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.device_worker = Some(receiver);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.device_error = Some("the device worker stopped".to_string());
            }
        }
    }

    fn apply_device_defaults(&mut self) {
        if self.devices.is_empty() {
            return;
        }
        let mut changed = false;
        if self.project.settings.input.name.is_empty() {
            if let Some(device) = default_device(&self.devices.inputs) {
                self.project.settings.input = selection_for(device, device.inputs.min(2));
                changed = true;
            }
        }
        if self.project.settings.output.name.is_empty() {
            if let Some(device) = default_device(&self.devices.outputs) {
                self.project.settings.output = selection_for(device, device.outputs.min(2));
                changed = true;
            }
        }
        if changed {
            self.mark_dirty();
        }
        if self.apply_automatic_apis() {
            self.mark_dirty();
        }
    }

    fn apply_automatic_apis(&mut self) -> bool {
        if self.project.settings.advanced_mode {
            return false;
        }
        let devices = &self.devices;
        let settings = &mut self.project.settings;
        let mut changed = false;
        let input_api = automatic_api(
            &devices.inputs,
            &settings.input.name,
            settings.input.api,
            true,
        );
        changed |= set_selection_api(&devices.inputs, &mut settings.input, input_api, true);
        let output_api = automatic_api(
            &devices.outputs,
            &settings.output.name,
            settings.output.api,
            false,
        );
        changed |= set_selection_api(&devices.outputs, &mut settings.output, output_api, false);
        let sidechain_api = automatic_api(
            &devices.inputs,
            &settings.sidechain_input.name,
            settings.sidechain_input.api,
            true,
        );
        changed |= set_selection_api(
            &devices.inputs,
            &mut settings.sidechain_input,
            sidechain_api,
            true,
        );
        changed
    }

    fn start_audio(&mut self) {
        if self.session.is_some() || self.session_worker.is_some() {
            return;
        }
        if self.device_worker.is_some() {
            self.last_error = Some("wait for the device scan to finish".to_string());
            return;
        }
        let Some(backend) = self.backend.clone() else {
            self.state = EngineState::Error;
            self.last_error = self.backend_error.clone();
            return;
        };

        let mut project = self.project.clone();
        let monitor_selection = self.monitor_selection();
        let duplex_monitor = monitor_selection.as_ref().is_some_and(|monitor| {
            self.project.settings.input.api == AudioApi::Asio
                && monitor.api == AudioApi::Asio
                && monitor.name == self.project.settings.input.name
        });
        let monitor = if self.monitor || duplex_monitor {
            monitor_selection
        } else {
            None
        };
        let mono_source = if self.project.settings.input.channels == 1 {
            Some(match self.project.settings.input_mono_source {
                MonoSource::Left => 0,
                MonoSource::Right => 1,
            })
        } else {
            None
        };
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.session_worker = Some(receiver);
        self.cancel_flag = cancel.clone();
        self.cancel_start = false;
        self.auto_retry_at = None;
        self.state = EngineState::Starting;
        self.last_error = None;
        self.tooltip = "MicHost - starting".to_string();
        self.set_tray_running(true);
        logging::line("starting audio");

        let spawned = std::thread::Builder::new()
            .name("michost-session".to_string())
            .spawn(move || {
                let result = AudioSession::start_with_monitor(
                    backend,
                    &mut project,
                    mono_source,
                    Some(cancel.as_ref()),
                    monitor.as_ref(),
                )
                .map(|session| (session, project));
                if let Err(error) = &result {
                    logging::line(format!("starting audio: {error}"));
                }
                let _ = sender.send(result);
            });
        if let Err(error) = spawned {
            self.session_worker = None;
            self.state = EngineState::Error;
            self.last_error = Some(format!("starting the session worker: {error}"));
            self.tooltip = "MicHost - error".to_string();
        }
    }

    fn merge_plugin_metadata(&mut self, started: &Project) {
        let mut changed = false;
        for started_entry in &started.chain {
            if let Some(current) = self
                .project
                .chain
                .iter_mut()
                .find(|entry| entry.id == started_entry.id)
            {
                let plugin = &started_entry.plugin;
                if current.plugin.main_inputs != plugin.main_inputs
                    || current.plugin.main_outputs != plugin.main_outputs
                    || current.plugin.sidechain_inputs != plugin.sidechain_inputs
                {
                    current.plugin.main_inputs = plugin.main_inputs;
                    current.plugin.main_outputs = plugin.main_outputs;
                    current.plugin.sidechain_inputs = plugin.sidechain_inputs;
                    changed = true;
                }
            }
        }
        if changed {
            self.mark_dirty();
        }
    }

    fn poll_session_worker(&mut self) {
        let Some(receiver) = self.session_worker.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok((mut session, started_project))) => {
                self.session_worker = None;
                let cancelled = self.cancel_start;
                self.cancel_start = false;
                if cancelled {
                    drop(session);
                    self.state = EngineState::Stopped;
                    self.auto_retry_at = None;
                    self.tooltip = "MicHost - stopped".to_string();
                    self.set_tray_running(false);
                    self.save_now();
                    return;
                }
                self.session_epoch = self.session_epoch.wrapping_add(1);
                self.warnings = session.warnings.clone();
                logging::line(format!(
                    "audio running: {} Hz in, {} Hz out, block {}, latency {:.1} ms",
                    session.input().sample_rate as u32,
                    session.output().sample_rate as u32,
                    session.block_size(),
                    session.end_to_end_milliseconds()
                ));
                self.merge_plugin_metadata(&started_project);
                self.last_input_buffer = Some((
                    session.input().device.clone(),
                    session.input().api,
                    session.input().buffer_size,
                ));
                self.last_output_buffer = Some((
                    session.output().device.clone(),
                    session.output().api,
                    session.output().buffer_size,
                ));
                session.clear_plugin_edits();
                session.set_monitor_trim(self.project.settings.monitor_trim_db);
                if self.monitor {
                    match self.monitor_selection() {
                        Some(selection) => {
                            if let Err(error) = session.start_monitor(
                                &selection.name,
                                selection.api,
                                selection.channels,
                                selection.channel_mask,
                            ) {
                                self.monitor = false;
                                self.project.settings.monitor_enabled = false;
                                logging::line(format!("monitor: {error}"));
                                self.last_error = Some(format!("monitor: {error}"));
                                self.mark_dirty();
                                self.save_now();
                            } else {
                                logging::line(format!("monitor: enabled on '{}'", selection.name));
                            }
                        }
                        None => {
                            self.monitor = false;
                            self.project.settings.monitor_enabled = false;
                            self.last_error = Some(
                                "no monitor output is available; choose a device in Monitor settings"
                                    .to_string(),
                            );
                            self.mark_dirty();
                            self.save_now();
                        }
                    }
                }
                self.session = Some(session);
                self.state = EngineState::Running;
                self.auto_retry_at = None;
                self.tooltip = "MicHost - running".to_string();
                self.set_tray_running(true);
                self.mark_dirty();
                self.save_now();
            }
            Ok(Err(error)) => {
                self.session_worker = None;
                let cancelled = self.cancel_start;
                self.cancel_start = false;
                if cancelled {
                    self.state = EngineState::Stopped;
                    self.auto_retry_at = None;
                    self.tooltip = "MicHost - stopped".to_string();
                } else {
                    self.last_error = Some(error);
                    self.state = EngineState::Error;
                    self.auto_retry_at = Some(Instant::now() + START_RETRY_DELAY);
                    self.tooltip = "MicHost - error".to_string();
                }
                self.set_tray_running(false);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.session_worker = Some(receiver);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.session_worker = None;
                self.cancel_start = false;
                self.last_error = Some("the session worker stopped unexpectedly".to_string());
                self.state = EngineState::Error;
                self.auto_retry_at = Some(Instant::now() + START_RETRY_DELAY);
                self.tooltip = "MicHost - error".to_string();
                self.set_tray_running(false);
            }
        }
    }

    fn stop_audio(&mut self) {
        if self.session_worker.is_some() {
            self.cancel_flag.store(true, Ordering::Release);
            self.cancel_start = true;
            self.state = EngineState::Starting;
            self.tooltip = "MicHost - stopping".to_string();
            self.set_tray_running(false);
            self.mark_dirty();
            self.save_now();
            return;
        }
        if let Some(mut session) = self.session.take() {
            logging::line("audio stopped");
            self.builtin_editors.clear();
            session.stop(Some(&mut self.project));
            self.warnings.extend(session.warnings.iter().cloned());
            self.session_epoch = self.session_epoch.wrapping_add(1);
            self.mark_dirty();
        }
        self.cancel_start = false;
        self.auto_retry_at = None;
        self.state = EngineState::Stopped;
        self.tooltip = "MicHost - stopped".to_string();
        self.set_tray_running(false);
        self.save_now();
    }

    fn ensure_engine(&mut self, now: Instant) {
        if self.quitting || self.engine_paused || self.measuring {
            return;
        }
        if self.session.is_some() || self.session_worker.is_some() || self.device_worker.is_some() {
            return;
        }
        if !self.can_start() {
            return;
        }
        if let Some(retry_at) = self.auto_retry_at {
            if now < retry_at {
                return;
            }
        }
        self.auto_retry_at = None;
        self.start_audio();
    }

    fn open_device_panel(&mut self, input: bool) {
        let Some(session) = self.session.as_ref() else {
            self.last_error = Some("start audio to open device panels".to_string());
            return;
        };
        if let Err(error) = session.control_panel(input) {
            self.last_error = Some(error);
        }
    }

    fn set_tray_running(&mut self, running: bool) {
        let changed = self.tray_running != running;
        self.tray_running = running;
        if let Some(tray) = &self.tray {
            if changed {
                tray.set_running(running);
            }
            tray.set_tooltip(&self.tooltip);
        }
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        let commands = self.tray.as_ref().map(Tray::poll).unwrap_or_default();
        for command in commands {
            match command {
                TrayCommand::Show => self.show_window(ctx),
                TrayCommand::Start => {
                    self.show_window(ctx);
                    self.engine_paused = false;
                    self.auto_retry_at = None;
                }
                TrayCommand::Stop => {
                    self.show_window(ctx);
                    self.engine_paused = true;
                    self.auto_retry_at = None;
                    self.stop_audio();
                }
                TrayCommand::Quit => {
                    logging::line("quit requested from the tray");
                    self.quitting = true;
                    self.quit_now();
                }
            }
        }
    }

    fn poll_pending_add(&mut self) {
        let Some(pending) = self.pending_add.take() else {
            return;
        };
        match pending.receiver.try_recv() {
            Ok(Ok(prepared)) => {
                let running = self.state == EngineState::Running
                    && self.session.is_some()
                    && self.session_epoch == pending.session_epoch;
                if running {
                    let id = self.allocate_plugin_id();
                    let mut spec = pending.spec;
                    spec.main_inputs = prepared.main_inputs;
                    spec.main_outputs = prepared.main_outputs;
                    spec.sidechain_inputs = prepared.native_sidechain;
                    self.project.chain.push(ChainEntry {
                        id,
                        plugin: spec,
                        bypassed: false,
                        sidechain_source: SidechainSource::Off,
                        oversampling: 1,
                    });
                    let inserted = self.session.as_mut().map(|session| {
                        session.insert_prepared_with_project(&self.project, id, prepared)
                    });
                    if let Some(Err(error)) = inserted {
                        self.project.chain.retain(|entry| entry.id != id);
                        self.last_error = Some(error);
                        return;
                    }
                    self.mark_dirty();
                    self.save_now();
                    return;
                }
                if let Some(backend) = self.backend.clone() {
                    discard_prepared(&backend, prepared);
                }
            }
            Ok(Err(error)) => {
                self.last_error = Some(error);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.pending_add = Some(pending);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.last_error = Some(format!(
                    "loading '{}': the plugin worker stopped",
                    pending.name
                ));
            }
        }
    }

    fn begin_add(&mut self, descriptor: &PluginDescriptor) {
        if self.pending_add.is_some() {
            return;
        }
        if let Some(kind) = BuiltinKind::from_path(&descriptor.path) {
            self.begin_add_builtin(kind);
            return;
        }
        if self.state == EngineState::Running && self.session.is_some() {
            let Some(backend) = self.backend.clone() else {
                self.last_error = Some("mic_host.dll is not loaded".to_string());
                return;
            };
            let Some(session) = self.session.as_ref() else {
                return;
            };
            let sample_rate = session.sample_rate();
            let block_size = session.block_size();
            let name = descriptor.name.clone();
            let path = descriptor.path.clone();
            let class_id = descriptor.class_id.clone();
            let spec = descriptor.to_spec();
            let (sender, receiver) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("michost-insert".to_string())
                .spawn(move || {
                    let result = prepare_plugin_for_insert(
                        backend,
                        &path,
                        &class_id,
                        &name,
                        sample_rate,
                        block_size,
                        0,
                        None,
                    );
                    let _ = sender.send(result);
                });
            match spawned {
                Ok(_) => {
                    self.pending_add = Some(PendingAdd {
                        name: descriptor.name.clone(),
                        spec,
                        session_epoch: self.session_epoch,
                        receiver,
                    });
                }
                Err(error) => {
                    self.last_error = Some(format!("starting the plugin worker: {error}"));
                }
            }
            return;
        }
        if self.session_worker.is_some() {
            self.stop_audio();
        }
        let id = self.allocate_plugin_id();
        self.project.chain.push(ChainEntry {
            id,
            plugin: descriptor.to_spec(),
            bypassed: false,
            sidechain_source: SidechainSource::Off,
            oversampling: 1,
        });
        self.mark_dirty();
        self.save_now();
    }

    fn begin_add_builtin(&mut self, kind: BuiltinKind) {
        if !cfg!(debug_assertions) {
            self.last_error =
                Some("built-in effects are not included in release builds".to_string());
            return;
        }
        if self.pending_add.is_some() {
            return;
        }
        if self.session_worker.is_some() {
            self.stop_audio();
        }
        let id = self.allocate_plugin_id();
        let params = ParamSet::new(kind);
        self.project.chain.push(ChainEntry {
            id,
            plugin: builtin_spec(kind),
            bypassed: false,
            sidechain_source: SidechainSource::Off,
            oversampling: 1,
        });
        self.project.set_plugin_state(id, params.encode());
        if self.state == EngineState::Running
            && let Some(session) = self.session.as_mut()
            && let Err(error) =
                session.insert_builtin_with_project(&self.project, id, kind, params, kind.name())
        {
            self.project.chain.retain(|entry| entry.id != id);
            self.project.remove_plugin_state(id);
            self.last_error = Some(error);
            return;
        }
        self.mark_dirty();
        self.save_now();
    }

    fn poll_scan(&mut self) {
        let Some(receiver) = self.scan.receiver.take() else {
            return;
        };
        let mut keep = true;
        let mut done = None;
        let mut error = None;
        let mut warning = None;
        loop {
            match receiver.try_recv() {
                Ok(ScanUpdate::Progress {
                    done: current,
                    total,
                }) => {
                    self.scan.done = current;
                    self.scan.total = total;
                }
                Ok(ScanUpdate::Warning(message)) => {
                    warning = Some(message);
                }
                Ok(ScanUpdate::Done(descriptors)) => {
                    done = Some(descriptors);
                    keep = false;
                    break;
                }
                Ok(ScanUpdate::Error(message)) => {
                    error = Some(message);
                    keep = false;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    keep = false;
                    break;
                }
            }
        }
        if let Some(message) = warning {
            self.scan.error = Some(message);
        }
        if let Some(descriptors) = done {
            self.plugins = descriptors;
            self.plugin_generation = self.plugin_generation.wrapping_add(1);
            self.scan.active = false;
            self.scan.done = self.scan.total;
            logging::line(format!(
                "plugin scan finished: {} plugins",
                self.plugins.len()
            ));
        } else if let Some(error) = error {
            self.scan.active = false;
            self.scan.error = Some(error.clone());
            logging::line(error);
        } else if keep {
            self.scan.receiver = Some(receiver);
        } else {
            self.scan.active = false;
        }
    }

    fn save_plugin_paths(&self) {
        let cache = PluginCache {
            saved_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
            descriptors: self.plugins.clone(),
            vst3_paths: self.vst3_paths.clone(),
            disabled_vst3_paths: self.disabled_vst3_paths.clone(),
        };
        cache.save();
    }

    fn ui_vst3_folders(&mut self, ctx: &egui::Context) {
        if !self.show_vst3_folders {
            return;
        }
        let mut open = self.show_vst3_folders;
        egui::Window::new("VST3 folders")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(
                        "The scanner uses these standard locations plus custom folders. Uncheck a default location to skip it during scans; it stays in this list. VST3_PATH entries appear here too.",
                    )
                    .weak(),
                );
                ui.add_space(6.0);

                ui.label(egui::RichText::new("Default locations").strong());
                let defaults = plugin_cache::default_vst3_paths();
                if defaults.is_empty() {
                    ui.label(egui::RichText::new("No default VST3 locations found.").weak());
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("vst3_default_locations")
                        .max_height(230.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for path in &defaults {
                                let mut enabled = !self
                                    .disabled_vst3_paths
                                    .iter()
                                    .any(|disabled| plugin_cache::same_path(disabled, path));
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(&mut enabled, "")
                                        .on_hover_text("Scan this location")
                                        .changed()
                                    {
                                        if enabled {
                                            self.disabled_vst3_paths.retain(|disabled| {
                                                !plugin_cache::same_path(disabled, path)
                                            });
                                        } else if !self
                                            .disabled_vst3_paths
                                            .iter()
                                            .any(|disabled| plugin_cache::same_path(disabled, path))
                                        {
                                            self.disabled_vst3_paths.push(path.clone());
                                        }
                                        self.save_plugin_paths();
                                    }
                                    ui.add(
                                        egui::Label::new(short_text(path, 74)).truncate(),
                                    )
                                    .on_hover_text(path);
                                    if enabled {
                                        ui.label(egui::RichText::new("scanned").weak().small());
                                    } else {
                                        ui.label(
                                            egui::RichText::new("disabled")
                                                .color(egui::Color32::from_rgb(230, 180, 80))
                                                .small(),
                                        );
                                    }
                                    if !std::path::Path::new(path).exists() {
                                        ui.label(egui::RichText::new("not found").weak().small());
                                    }
                                });
                            }
                        });
                }

                ui.add_space(8.0);
                ui.label(egui::RichText::new("Custom folders").strong());
                let mut remove = None;
                if self.vst3_paths.is_empty() {
                    ui.label(egui::RichText::new("No custom folders.").weak());
                } else {
                    for (index, path) in self.vst3_paths.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(path.as_str()).truncate())
                                .on_hover_text(path);
                            if ui.small_button("Remove").clicked() {
                                remove = Some(index);
                            }
                        });
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Add folder...").clicked()
                        && let Some(folder) = rfd::FileDialog::new().pick_folder()
                    {
                        let path = folder.display().to_string();
                        if !self
                            .vst3_paths
                            .iter()
                            .any(|existing| existing.eq_ignore_ascii_case(&path))
                        {
                            self.vst3_paths.push(path);
                            self.save_plugin_paths();
                        }
                    }
                    if ui.button("Rescan now").clicked() {
                        self.start_scan();
                    }
                });
                if let Some(index) = remove {
                    self.vst3_paths.remove(index);
                    self.save_plugin_paths();
                }
                ui.label(egui::RichText::new("Rescan to apply folder changes.").weak());
            });
        self.show_vst3_folders = open;
    }

    fn start_scan(&mut self) {
        if self.scan.active {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            self.scan.error = Some("mic_host.dll is not loaded".to_string());
            return;
        };
        self.scan = ScanState {
            receiver: Some(plugin_cache::start(
                backend,
                self.vst3_paths.clone(),
                self.disabled_vst3_paths.clone(),
            )),
            active: true,
            done: 0,
            total: 0,
            error: None,
        };
        logging::line("scanning for VST3 plugins");
    }

    fn poll_backend(&mut self) {
        let events = self
            .backend
            .as_ref()
            .map(|backend| backend.poll_events())
            .unwrap_or_default();
        for event in events {
            match event {
                michost_host_abi::UiEvent::DeviceError { status, message } => {
                    logging::line(format!("device error {status}: {message}"));
                    self.last_error = Some(message);
                    if status != MH_NOT_SUPPORTED && self.state == EngineState::Running {
                        self.stop_audio();
                        self.state = EngineState::Error;
                        self.auto_retry_at = Some(Instant::now() + START_RETRY_DELAY);
                    }
                }
                michost_host_abi::UiEvent::LatencyChanged { .. } => {}
            }
        }
    }

    fn poll_latency(&mut self) {
        if self.session.is_none() || self.latency_timer.elapsed() < Duration::from_millis(750) {
            return;
        }
        self.latency_timer = Instant::now();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.refresh_latencies() {
            Ok(true) => {
                if let Err(error) = session.rebuild_engine(&self.project) {
                    logging::line(format!("rebuilding for latency: {error}"));
                }
            }
            Ok(false) => {}
            Err(error) => self.last_error = Some(error),
        }
        session.collect_retired();
    }

    fn quit_now(&mut self) {
        arm_quit_watchdog();
        if let Some(session) = self.session.as_mut() {
            session.quiesce(&mut self.project);
        }
        self.dirty = true;
        self.save_now();
        logging::line("MicHost exited");
        std::process::exit(0);
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.save_deadline = Some(Instant::now() + Duration::from_millis(400));
    }

    fn save_now(&mut self) {
        if !self.dirty {
            return;
        }
        let mut saved = true;
        if let Some(path) = self.project_path.clone() {
            if let Err(error) = self.project.save(&path) {
                logging::line(format!("saving project: {error}"));
                self.last_error = Some(error.to_string());
                saved = false;
            }
        }
        match self.project.save_default() {
            Ok(_) => {}
            Err(error) => {
                logging::line(format!("saving the default project: {error}"));
                self.last_error = Some(error.to_string());
                saved = false;
            }
        }
        if saved {
            if let Some(path) = &self.project_path {
                project_state::remember(path);
            }
            self.saved_flash_until = Some(Instant::now() + Duration::from_secs(5));
            self.dirty = false;
        }
        self.save_deadline = None;
    }

    fn new_project(&mut self) {
        let Some(path) = project_dialog()
            .add_filter("MicHost project", &[PROJECT_EXTENSION])
            .set_file_name("MicHost.mhchain")
            .save_file()
        else {
            return;
        };
        if self.session.is_some() || self.session_worker.is_some() {
            self.stop_audio();
        }

        let mut project = Project::default();
        project.settings.advanced_mode = self.project.settings.advanced_mode;
        project.settings.close_to_tray = self.project.settings.close_to_tray;
        project.settings.launch_at_login = self.project.settings.launch_at_login;
        if let Some(device) = default_device(&self.devices.inputs) {
            project.settings.input = selection_for(device, device.inputs.min(2));
        }
        if let Some(device) = default_device(&self.devices.outputs) {
            project.settings.output = selection_for(device, device.outputs.min(2));
        }

        if let Err(error) = project.save(&path) {
            logging::line(format!("creating project: {error}"));
            self.last_error = Some(format!("creating project: {error}"));
            return;
        }

        self.next_plugin_id = project.next_plugin_id();
        self.project = project;
        self.project_path = Some(path.clone());
        project_state::remember(&path);
        self.warnings.clear();
        self.last_error = None;
        self.measured = None;
        self.measuring = false;
        self.measure_worker = None;
        self.pending_add = None;
        self.add.open = false;
        self.add.picker = None;
        self.last_input_buffer = None;
        self.last_output_buffer = None;
        self.state = EngineState::Stopped;
        self.mark_dirty();
        self.save_now();
        logging::line(format!("new project: {}", path.display()));
    }

    fn open_project(&mut self) {
        let Some(path) = project_dialog()
            .add_filter("MicHost project", &[PROJECT_EXTENSION])
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return;
        };
        match Project::load(&path) {
            Ok(mut project) => {
                if self.session.is_some() || self.session_worker.is_some() {
                    self.stop_audio();
                }
                let removed = strip_builtins(&mut project, cfg!(debug_assertions));
                project_state::remember(&path);
                self.next_plugin_id = project.next_plugin_id();
                self.project = project;
                self.project_path = Some(path);
                self.warnings.clear();
                self.warnings.extend(removed.into_iter().map(|name| {
                    format!(
                        "built-in effect '{name}' is not included in release builds and was removed"
                    )
                }));
                self.last_error = None;
                self.state = EngineState::Stopped;
                self.mark_dirty();
                self.save_now();
            }
            Err(error) => {
                logging::line(format!("opening project: {error}"));
                self.last_error = Some(error.to_string());
            }
        }
    }

    fn save_project_as(&mut self) {
        let Some(path) = project_dialog()
            .add_filter("MicHost project", &[PROJECT_EXTENSION])
            .set_file_name("MicHost.mhchain")
            .save_file()
        else {
            return;
        };
        match self.project.save(&path) {
            Ok(()) => {
                project_state::remember(&path);
                self.project_path = Some(path);
                self.mark_dirty();
                self.save_now();
            }
            Err(error) => {
                logging::line(format!("saving project: {error}"));
                self.last_error = Some(error.to_string());
            }
        }
    }

    fn select_input(&mut self, name: &str) {
        let device = selected_device(
            &self.devices.inputs,
            name,
            self.project.settings.input.api,
            self.project.settings.advanced_mode,
            true,
        )
        .cloned();
        if let Some(device) = device {
            let channels = self
                .project
                .settings
                .input
                .channels
                .clamp(1, device.inputs.max(1));
            self.project.settings.input = DeviceSelection {
                api: device.api,
                name: device.name,
                id: device.id,
                channels,
                channel_mask: 0,
            };
            self.settings_changed();
        }
    }

    fn select_output(&mut self, name: &str) {
        let device = selected_device(
            &self.devices.outputs,
            name,
            self.project.settings.output.api,
            self.project.settings.advanced_mode,
            false,
        )
        .cloned();
        if let Some(device) = device {
            let channels = self
                .project
                .settings
                .output
                .channels
                .clamp(1, device.outputs.max(1));
            self.project.settings.output = DeviceSelection {
                api: device.api,
                name: device.name,
                id: device.id,
                channels,
                channel_mask: 0,
            };
            self.settings_changed();
        }
    }

    fn select_sidechain_input(&mut self, name: &str) {
        let device = selected_device(
            &self.devices.inputs,
            name,
            self.project.settings.sidechain_input.api,
            self.project.settings.advanced_mode,
            true,
        )
        .cloned();
        if let Some(device) = device {
            let channels = self
                .project
                .settings
                .sidechain_input
                .channels
                .clamp(1, device.inputs.max(1));
            self.project.settings.sidechain_input = DeviceSelection {
                api: device.api,
                name: device.name,
                id: device.id,
                channels,
                channel_mask: 0,
            };
            self.settings_changed();
        }
    }

    fn settings_changed(&mut self) {
        self.measured = None;
        if !self.project.settings.advanced_mode {
            self.apply_automatic_apis();
        }
        if self.session.is_some() || self.session_worker.is_some() {
            self.stop_audio();
        }
        self.mark_dirty();
    }

    fn apply_chain_action(&mut self, action: ChainAction) {
        match action {
            ChainAction::Move(from, to) => {
                self.cancel_start_for_edit();
                if move_chain_entry(&mut self.project.chain, from, to) {
                    self.rebuild_chain();
                }
            }
            ChainAction::Remove(index) => {
                if index >= self.project.chain.len() {
                    return;
                }
                self.cancel_start_for_edit();
                let entry = self.project.chain.remove(index);
                self.project.remove_plugin_state(entry.id);
                self.project.prune_plugin_states();
                self.builtin_editors
                    .retain(|editor| editor.entry_id != entry.id);
                if let Some(session) = self.session.as_mut() {
                    session.retire_plugin(entry.id);
                    if let Err(error) = session.rebuild_engine(&self.project) {
                        logging::line(format!("retiring plugin {}: {error}", entry.id));
                        self.last_error = Some(error);
                        self.stop_audio();
                    }
                }
                self.mark_dirty();
                self.save_now();
            }
            ChainAction::Bypass(index, bypassed) => {
                self.cancel_start_for_edit();
                if let Some(entry) = self.project.chain.get_mut(index) {
                    entry.bypassed = bypassed;
                    self.rebuild_chain();
                }
            }
            ChainAction::Oversampling(index, factor) => {
                if let Some(entry) = self.project.chain.get_mut(index) {
                    entry.oversampling = factor;
                }
                self.settings_changed();
            }
            ChainAction::Sidechain(index, source) => {
                self.cancel_start_for_edit();
                if let Some(entry) = self.project.chain.get_mut(index) {
                    entry.sidechain_source = source;
                }
                self.mark_dirty();
                self.stop_audio();
                self.save_now();
            }
            ChainAction::OpenEditor(index) => {
                let Some(entry) = self.project.chain.get(index) else {
                    return;
                };
                let Some(session) = self.session.as_ref() else {
                    self.last_error = Some("start audio to open plugin editors".to_string());
                    return;
                };
                if let Some((kind, params, analysis)) = session.builtin(entry.id) {
                    if !self
                        .builtin_editors
                        .iter()
                        .any(|editor| editor.entry_id == entry.id)
                    {
                        self.builtin_editors
                            .push(BuiltinEditor::new(entry.id, kind, params, analysis));
                    }
                    return;
                }
                if let Err(error) = session.open_editor(entry.id) {
                    self.last_error = Some(error);
                }
            }
            ChainAction::Add(descriptor) => self.begin_add(&descriptor),
        }
    }

    fn allocate_plugin_id(&mut self) -> u64 {
        let id = self.next_plugin_id.max(1);
        self.next_plugin_id = id.saturating_add(1);
        id
    }

    fn cancel_start_for_edit(&mut self) {
        if self.session_worker.is_some() {
            self.stop_audio();
        }
    }

    fn rebuild_chain(&mut self) {
        self.measured = None;
        if let Some(session) = self.session.as_mut() {
            match session.rebuild_engine(&self.project) {
                Ok(()) => {}
                Err(error) => {
                    logging::line(format!("hot-swapping the chain: {error}"));
                    self.last_error = Some(error);
                    self.stop_audio();
                    return;
                }
            }
        }
        self.mark_dirty();
        self.save_now();
    }

    fn can_start(&self) -> bool {
        self.backend.is_some()
            && !self.project.settings.input.name.is_empty()
            && ((cfg!(debug_assertions)
                && self.project.settings.output_kind == OutputKind::VirtualMic)
                || !self.project.settings.output.name.is_empty())
            && (!self.project.settings.sidechain_input_enabled
                || !self.project.settings.sidechain_input.name.is_empty())
    }

    fn actual_buffer(&self, input: bool) -> Option<u32> {
        let selection = if input {
            &self.project.settings.input
        } else {
            &self.project.settings.output
        };
        if let Some(session) = &self.session {
            let endpoint = if input {
                session.input()
            } else {
                session.output()
            };
            if endpoint.device == selection.name && endpoint.api == selection.api {
                return Some(endpoint.buffer_size);
            }
        }
        let cached = if input {
            &self.last_input_buffer
        } else {
            &self.last_output_buffer
        };
        cached.as_ref().and_then(|(name, api, size)| {
            (name == &selection.name && *api == selection.api).then_some(*size)
        })
    }

    fn buffer_options(&self, input: bool) -> Vec<u32> {
        let (devices, selection) = if input {
            (&self.devices.inputs, &self.project.settings.input)
        } else {
            (&self.devices.outputs, &self.project.settings.output)
        };
        let current = if input {
            self.project.settings.buffer_size
        } else {
            self.project.settings.output_buffer_size
        };
        let mut sizes = if let Some(device) = devices
            .iter()
            .find(|device| device.name == selection.name && device.api == selection.api)
            && !device.buffer_sizes.is_empty()
        {
            let mut sizes = device.buffer_sizes.clone();
            sizes.sort_unstable();
            sizes.dedup();
            sizes
        } else {
            vec![64, 128, 256, 512, 1024, 2048]
        };
        if current > 0 && !sizes.contains(&current) {
            sizes.push(current);
            sizes.sort_unstable();
        }
        sizes
    }

    fn monitor_selection(&self) -> Option<DeviceSelection> {
        let configured = &self.project.settings.monitor_output;
        if !configured.name.is_empty() {
            let device = self.devices.outputs.iter().find(|device| {
                device.outputs > 0 && device.name == configured.name && device.api == configured.api
            })?;
            let limit = if device.outputs >= 64 {
                u64::MAX
            } else {
                (1u64 << device.outputs) - 1
            };
            let mask = configured.channel_mask & limit;
            let channels = if mask != 0 {
                mask.count_ones().min(device.outputs).max(1)
            } else {
                configured.channels.clamp(1, device.outputs.min(2))
            };
            let mut selection = selection_for(device, channels);
            selection.channel_mask = mask;
            return Some(selection);
        }
        let default = default_device(&self.devices.outputs)?;
        if default.name == self.project.settings.output.name {
            return None;
        }
        let name = default.name.as_str();
        let device = self
            .devices
            .outputs
            .iter()
            .find(|device| {
                device.name == name && device.outputs > 0 && device.api == AudioApi::WasapiShared
            })
            .or_else(|| {
                self.devices.outputs.iter().find(|device| {
                    device.name == name
                        && device.outputs > 0
                        && device.api == AudioApi::WasapiLowLatency
                })
            })
            .or_else(|| {
                self.devices
                    .outputs
                    .iter()
                    .find(|device| device.name == name && device.outputs > 0)
            })?;
        Some(selection_for(device, device.outputs.min(2)))
    }

    fn toggle_monitor(&mut self) {
        if self.monitor {
            self.monitor = false;
            self.project.settings.monitor_enabled = false;
            logging::line("monitor: disabled");
            if let Some(session) = self.session.as_mut() {
                session.stop_monitor();
            }
            self.mark_dirty();
            self.save_now();
            return;
        }
        let Some(selection) = self.monitor_selection() else {
            self.last_error = Some(
                "no monitor output is available; choose a device in Monitor settings".to_string(),
            );
            return;
        };
        self.monitor = true;
        self.project.settings.monitor_enabled = true;
        logging::line("monitor: enabled");
        if let Some(session) = self.session.as_mut() {
            if let Err(error) = session.start_monitor(
                &selection.name,
                selection.api,
                selection.channels,
                selection.channel_mask,
            ) {
                self.monitor = false;
                self.project.settings.monitor_enabled = false;
                logging::line(format!("monitor: {error}"));
                self.last_error = Some(format!("monitor: {error}"));
            }
        }
        self.mark_dirty();
        self.save_now();
    }

    fn monitor_uses_input_asio(&self) -> bool {
        let monitor = &self.project.settings.monitor_output;
        !monitor.name.is_empty()
            && monitor.api == AudioApi::Asio
            && self.project.settings.input.api == AudioApi::Asio
            && monitor.name == self.project.settings.input.name
    }

    fn monitor_settings_changed(&mut self) {
        self.mark_dirty();
        self.save_now();
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.monitor_is_duplex())
            || self.monitor_uses_input_asio()
        {
            self.settings_changed();
            return;
        }
        if !self.monitor {
            if let Some(session) = self.session.as_mut() {
                session.close_monitor();
            }
            return;
        }
        let Some(selection) = self.monitor_selection() else {
            self.monitor = false;
            self.project.settings.monitor_enabled = false;
            self.last_error = Some("the selected monitor output is not available".to_string());
            if let Some(session) = self.session.as_mut() {
                session.close_monitor();
            }
            self.mark_dirty();
            self.save_now();
            return;
        };
        if let Some(session) = self.session.as_mut() {
            session.close_monitor();
            if let Err(error) = session.start_monitor(
                &selection.name,
                selection.api,
                selection.channels,
                selection.channel_mask,
            ) {
                self.monitor = false;
                self.project.settings.monitor_enabled = false;
                self.last_error = Some(format!("monitor: {error}"));
                logging::line(format!("monitor: {error}"));
                self.mark_dirty();
                self.save_now();
            } else {
                logging::line(format!("monitor: switched to '{}'", selection.name));
            }
        }
    }

    fn request_measure(&mut self) {
        if self.measuring {
            return;
        }
        self.start_measurement();
    }

    fn start_measurement(&mut self) {
        let Some(backend) = self.backend.clone() else {
            self.last_error = Some("mic_host.dll is not loaded".to_string());
            return;
        };
        if self.session.is_none() {
            self.last_error = Some("start audio to measure the chain".to_string());
            return;
        }
        let selection = self.project.settings.output.clone();
        let Some(output) = self
            .devices
            .outputs
            .iter()
            .find(|device| {
                device.name == selection.name && device.api == selection.api && device.outputs > 0
            })
            .cloned()
        else {
            self.last_error =
                Some("the configured output device is not available for measurement".to_string());
            return;
        };
        let inputs = backend
            .enumerate_devices(DeviceKind::Input)
            .unwrap_or_else(|_| self.devices.inputs.clone());
        let Some(input) = find_loopback_input(&inputs, &output) else {
            self.last_error = Some(format!(
                "no loopback capture found for '{}'; add the cable's capture endpoint or use --selftest-latency",
                output.name
            ));
            return;
        };
        let Some(session) = self.session.as_mut() else {
            self.last_error = Some("start audio to measure the chain".to_string());
            return;
        };
        if let Err(error) = session.begin_chain_measurement() {
            self.last_error = Some(format!("starting the chain measurement: {error}"));
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.measure_worker = Some(receiver);
        self.measuring = true;
        self.last_error = None;
        logging::line(format!(
            "latency measurement: chain marker -> '{}'",
            input.name
        ));
        let spawned = std::thread::Builder::new()
            .name("michost-latency".to_string())
            .spawn(move || {
                let result = latency::capture_input(backend.as_ref(), &input, 2);
                if let Err(error) = &result {
                    logging::line(format!("latency measurement: {error}"));
                }
                let _ = sender.send(result);
            });
        if let Err(error) = spawned {
            self.measuring = false;
            self.measure_worker = None;
            self.last_error = Some(format!("starting the latency measurement: {error}"));
        }
    }

    fn poll_measure(&mut self) {
        let Some(receiver) = self.measure_worker.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(capture)) => {
                let clicks = self
                    .session
                    .as_mut()
                    .map(|session| session.end_chain_measurement())
                    .unwrap_or_default();
                match latency::correlate(&capture, &clicks) {
                    Ok(result) => {
                        logging::line(format!(
                            "latency measurement: p50 {:.1} ms, p95 {:.1} ms",
                            result.p50_ms, result.p95_ms
                        ));
                        self.measured = Some(result);
                    }
                    Err(error) => {
                        self.last_error = Some(format!("latency measurement: {error}"));
                    }
                }
                self.measuring = false;
            }
            Ok(Err(error)) => {
                if let Some(session) = self.session.as_mut() {
                    let _ = session.end_chain_measurement();
                }
                self.measuring = false;
                self.last_error = Some(format!("latency measurement: {error}"));
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.measure_worker = Some(receiver);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.measuring = false;
                self.last_error = Some("the latency measurement stopped unexpectedly".to_string());
            }
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        self.save_now();
        if let Some(session) = &self.session {
            session.close_editors();
        }
        self.builtin_editors.clear();
        self.hidden = true;
        crate::activation::hide_window();
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn show_builtin_editors(&mut self, ctx: &egui::Context) {
        if self.builtin_editors.is_empty() {
            return;
        }
        let mut keep = Vec::with_capacity(self.builtin_editors.len());
        for editor in std::mem::take(&mut self.builtin_editors) {
            if builtin_ui::show(ctx, &editor) {
                keep.push(editor);
            }
        }
        self.builtin_editors = keep;
        if !self.builtin_editors.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn update_tray_tooltip(&mut self) {
        if self.tray.is_none() {
            return;
        }
        let tooltip = if self.state == EngineState::Running {
            "MicHost - running"
        } else if self.state == EngineState::Error {
            "MicHost - error"
        } else {
            "MicHost - stopped"
        };
        if tooltip != self.tooltip {
            self.tooltip = tooltip.to_string();
            if let Some(tray) = &self.tray {
                tray.set_tooltip(tooltip);
            }
        }
    }

    #[cfg(debug_assertions)]
    fn poll_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot_path.clone() else {
            return;
        };
        let image = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let width = image.width() as u32;
            let height = image.height() as u32;
            let mut pixels = Vec::with_capacity(image.pixels.len() * 4);
            for pixel in &image.pixels {
                pixels.extend_from_slice(&pixel.to_array());
            }
            match image::save_buffer(&path, &pixels, width, height, image::ColorType::Rgba8) {
                Ok(()) => logging::line(format!("screenshot saved to {}", path.display())),
                Err(error) => logging::line(format!("saving screenshot: {error}")),
            }
            if let Some(session) = self.session.as_mut() {
                session.quiesce(&mut self.project);
            }
            std::process::exit(0);
        }

        let now = Instant::now();
        if self.screenshot_ready_at.is_none()
            && (self.state == EngineState::Running || now >= self.screenshot_deadline)
        {
            self.screenshot_ready_at = Some(now + Duration::from_millis(500));
        }
        let due = match self.screenshot_ready_at {
            Some(at) => at,
            None => {
                ctx.request_repaint_after(Duration::from_millis(50));
                return;
            }
        };
        if now < due {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        if self
            .screenshot_last_request
            .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            self.screenshot_last_request = Some(now);
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn ui_header(&mut self, ui: &mut egui::Ui) {
        let loading = self
            .pending_add
            .as_ref()
            .map(|pending| pending.name.clone());
        let retrying_in = self
            .auto_retry_at
            .map(|retry_at| retry_at.saturating_duration_since(Instant::now()));
        ui.horizontal(|ui| {
            let (dot_rect, _) =
                ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(dot_rect.center(), 4.0, self.state.color());
            ui.add(egui::Label::new(
                egui::RichText::new(self.state.label()).strong(),
            ));
            if self.engine_paused {
                ui.label(egui::RichText::new("paused").weak());
            }
            ui.separator();
            match self.state {
                EngineState::Starting => {
                    ui.spinner();
                    let text = if self.cancel_start {
                        "Stopping...".to_string()
                    } else if let Some(progress) = crate::audio_session::startup_progress() {
                        format!("Starting audio... {progress}")
                    } else {
                        "Starting audio...".to_string()
                    };
                    ui.label(text);
                }
                EngineState::Error => {
                    if let Some(remaining) = retrying_in {
                        ui.label(format!("retrying in {:.0} s", remaining.as_secs_f32()));
                    }
                }
                EngineState::Stopped | EngineState::Running => {}
            }
            let monitor_available = self.monitor_selection().is_some();
            let monitor_active = self.monitor;
            let monitor_name = self
                .monitor_selection()
                .map(|selection| selection.name)
                .unwrap_or_default();
            let mut monitor_toggled = false;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = monitor_button(ui, monitor_active, monitor_available);
                let hint = if !monitor_available {
                    "No default playback device is available for monitoring".to_string()
                } else if monitor_active {
                    format!(
                        "Monitoring on '{}' alongside the configured output. Click to stop monitoring.",
                        monitor_name
                    )
                } else {
                    format!(
                        "Monitor the chain on '{}'. Use headphones to avoid feedback.",
                        monitor_name
                    )
                };
                let response = response.on_hover_text(hint);
                if response.clicked() {
                    monitor_toggled = true;
                }
                if ui
                    .small_button("Settings")
                    .on_hover_text("Choose the API and device used for monitoring")
                    .clicked()
                {
                    self.show_monitor_settings = true;
                }
                if let Some(name) = &loading {
                    ui.label(format!("Loading {name}..."));
                    ui.spinner();
                }
            });
            if monitor_toggled {
                self.toggle_monitor();
            }
        });

        let red = egui::Color32::from_rgb(235, 105, 105);
        let amber = egui::Color32::from_rgb(230, 180, 80);
        if let Some(error) = &self.backend_error {
            ui.add(
                egui::Label::new(egui::RichText::new(short_text(error, 110)).color(red)).truncate(),
            )
            .on_hover_text(error);
        }
        if let Some(error) = &self.last_error {
            ui.add(
                egui::Label::new(egui::RichText::new(short_text(error, 110)).color(red)).truncate(),
            )
            .on_hover_text(error);
        }
        if let Some(error) = &self.scan.error {
            ui.add(
                egui::Label::new(egui::RichText::new(short_text(error, 110)).color(amber))
                    .truncate(),
            )
            .on_hover_text(error);
        }
        if let Some(error) = &self.device_error {
            ui.add(
                egui::Label::new(egui::RichText::new(short_text(error, 110)).color(amber))
                    .truncate(),
            )
            .on_hover_text(error);
        }
        if !self.warnings.is_empty() {
            egui::CollapsingHeader::new(format!("Warnings ({})", self.warnings.len()))
                .default_open(self.warnings.len() <= 2)
                .show(ui, |ui| {
                    for warning in &self.warnings {
                        ui.label(warning);
                    }
                });
        }

        if let Some(session) = &self.session {
            let input = session.input();
            let output = session.output();
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("IN").weak().small());
                        ui.add(egui::Label::new(short_text(&input.device, 30)).truncate())
                            .on_hover_text(&input.device);
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {:.1} kHz · {} smp",
                                api_label(input.api),
                                input.sample_rate / 1000.0,
                                input.buffer_size
                            ))
                            .weak()
                            .small(),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("OUT").weak().small());
                        ui.add(egui::Label::new(short_text(&output.device, 30)).truncate())
                            .on_hover_text(&output.device);
                        if session.is_virtual_mic_output() {
                            let stats = session.virtual_mic_stats();
                            ui.label(
                                egui::RichText::new("(virtual mic)")
                                    .color(egui::Color32::from_rgb(110, 210, 140))
                                    .small(),
                            )
                            .on_hover_text(format!(
                                "Virtual microphone ring: {} frames buffered; overruns {}; underruns {}.",
                                stats.vmic_buffered_frames,
                                stats.vmic_overruns,
                                stats.vmic_underruns
                            ));
                            ui.label(
                                egui::RichText::new(format!(
                                    "virtual · {:.1} kHz · {} smp packet",
                                    output.sample_rate / 1000.0,
                                    output.buffer_size
                                ))
                                .weak()
                                .small(),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · {:.1} kHz · {} smp",
                                    api_label(output.api),
                                    output.sample_rate / 1000.0,
                                    output.buffer_size
                                ))
                                .weak()
                                .small(),
                            );
                        }
                    });
                });
                let remaining = ui.available_width();
                let meters_width = 250.0;
                if remaining > meters_width + 8.0 {
                    ui.add_space(remaining - meters_width);
                    ui.vertical(|ui| {
                        lufs_bar(
                            ui,
                            "IN",
                            session.input_lufs(),
                            egui::Color32::from_rgb(100, 160, 220),
                        );
                        lufs_bar(
                            ui,
                            "OUT",
                            session.output_lufs(),
                            egui::Color32::from_rgb(90, 200, 150),
                        );
                    });
                }
            });
            let estimated = session.end_to_end_milliseconds();
            let current = session.current_end_to_end_milliseconds();
            let queue = session.queue_milliseconds();
            let target = session.target_queue_milliseconds();
            let breakdown = session.latency_breakdown();
            let measured = self.measured.as_ref();
            let input_path_ms = breakdown.input_device_ms + breakdown.input_buffer_ms;
            let (latency_title, latency_value, latency_hover) = if let Some(measured) = measured {
                let p50 = input_path_ms + measured.p50_ms;
                let p95 = input_path_ms + measured.p95_ms;
                (
                    "Latency",
                    format!("{:>7.1} ms", p50),
                    format!(
                        "Chain-inclusive host latency: mic input device {:.1} + input buffer {:.1} ms + measured chain marker path {:.1} ms = {:.1} ms.
p95 total {:.1} ms (marker p95 {:.1} ms). The marker is injected before the chain and captured at the cable, so graph, plugin chain, host queue, output, cable and capture are included.
Excludes the receiving app and the headphone monitor path. The marker is briefly audible in the virtual mic.",
                        breakdown.input_device_ms,
                        breakdown.input_buffer_ms,
                        measured.p50_ms,
                        p50,
                        p95,
                        measured.p95_ms
                    ),
                )
            } else {
                (
                    "Host estimate",
                    format!("{:>7.1} ms", estimated),
                    format!(
                        "Host estimate from buffers: input {:.1}+{:.1} ms + graph {:.1}+{:.1} ms + output {:.1}+{:.1} ms + queue {:.1} ms.
Current queue {queue:.1} ms (target {target:.1} ms); instantaneous estimate ~{current:.1} ms.
It excludes the cable pipe and capture engine. Use Measure to run the chain-inclusive marker test and get the host-to-cable total.",
                        breakdown.input_device_ms,
                        breakdown.input_buffer_ms,
                        breakdown.graph_ms,
                        breakdown.chain_ms,
                        breakdown.output_device_ms,
                        breakdown.output_buffer_ms,
                        breakdown.output_queue_ms
                    ),
                )
            };
            ui.columns(5, |columns| {
                header_stat(
                    &mut columns[0],
                    latency_title,
                    latency_value.trim(),
                    &latency_hover,
                );
                header_stat(
                    &mut columns[1],
                    "CPU",
                    &format!("{:.1}%", session.cpu_usage()),
                    "1 s average of chain processing time relative to the block period.",
                );
                header_stat(
                    &mut columns[2],
                    "Dropouts",
                    &format!(
                        "{} in / {} out",
                        session.input_dropouts(),
                        session.output_underruns()
                    ),
                    "Input callback drops / output underruns since the session started.",
                );
                header_stat(
                    &mut columns[3],
                    "Block",
                    &format!("{}", session.block_size()),
                    "Graph block size in frames.",
                );
                header_stat(
                    &mut columns[4],
                    "Plugin latency",
                    &format!("{} smp", session.total_latency()),
                    "Reported algorithmic latency of the active plugin chain. This is one component of Latency; the measured marker path also includes the graph, host output queue, output device, cable and capture.",
                );
            });
            if let Some(capture_engine_ms) = session.capture_engine_ms() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Capture engine").weak().small());
                    ui.label(
                        egui::RichText::new(format!(
                            "~{capture_engine_ms:.0} ms (estimate; app-dependent)"
                        ))
                        .monospace(),
                    )
                    .on_hover_text(
                        "Estimated Windows capture-engine period. It is not part of the host total above and is not measured here.",
                    );
                    ui.separator();
                    ui.label(egui::RichText::new("App buffer: unknown").weak().small());
                });
            }
            if let Some(second) = session.second_input() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("SC IN").weak());
                    ui.add(egui::Label::new(short_text(&second.device, 24)).truncate())
                        .on_hover_text(&second.device);
                    ui.label(format!(
                        "{} · {:.1} kHz · buffer {} · starved {} / drops {}",
                        api_label(second.api),
                        second.sample_rate / 1000.0,
                        second.buffer_size,
                        session.second_input_starved(),
                        session.second_input_dropouts()
                    ));
                });
            }
        }
    }

    fn ui_devices(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.heading("Audio");
        ui.add_space(6.0);

        let mut changed = false;
        let combo_width = 240.0;

        ui.label(egui::RichText::new("INPUT").strong());
        let advanced = self.project.settings.advanced_mode;
        egui::Grid::new("input_settings")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("API");
                let mut api = self.project.settings.input.api;
                if advanced {
                    egui::ComboBox::from_id_salt("input_api")
                        .selected_text(api_label(api))
                        .width(combo_width)
                        .show_ui(ui, |ui| {
                            for option in API_OPTIONS {
                                ui.selectable_value(&mut api, option, api_label(option));
                            }
                        });
                    ui.end_row();
                    if api != self.project.settings.input.api {
                        let mut selection = self.project.settings.input.clone();
                        set_selection_api(&self.devices.inputs, &mut selection, api, true);
                        self.project.settings.input = selection;
                        changed = true;
                    }
                } else {
                    ui.label(egui::RichText::new(automatic_api_label(api)).weak());
                    ui.end_row();
                }

                ui.label("Device");
                let inputs: Vec<String> = if advanced {
                    self.devices
                        .inputs
                        .iter()
                        .filter(|device| device.api == api && device.inputs > 0)
                        .map(|device| device.name.clone())
                        .collect()
                } else {
                    unique_device_names(&self.devices.inputs, true)
                };
                let selected = if self.project.settings.input.name.is_empty() {
                    "Select input".to_string()
                } else {
                    self.project.settings.input.name.clone()
                };
                let mut chosen = selected.clone();
                egui::ComboBox::from_id_salt("input_device")
                    .selected_text(&selected)
                    .width(combo_width)
                    .show_ui(ui, |ui| {
                        for name in &inputs {
                            ui.selectable_value(&mut chosen, name.clone(), name);
                        }
                    });
                ui.end_row();
                if chosen != selected {
                    self.select_input(&chosen);
                }

                ui.label("Channels");
                let max_input_channels = self
                    .devices
                    .inputs
                    .iter()
                    .find(|device| {
                        device.api == self.project.settings.input.api
                            && device.name == self.project.settings.input.name
                    })
                    .map(|device| device.inputs.clamp(1, 8))
                    .unwrap_or(8);
                let mut input_channels = self
                    .project
                    .settings
                    .input
                    .channels
                    .clamp(1, max_input_channels);
                let has_two = max_input_channels >= 2;
                let mut mono = if has_two {
                    self.project.settings.input_mono_source
                } else {
                    MonoSource::Left
                };
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("input_channels")
                        .selected_text(channel_label(input_channels))
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for count in 1..=max_input_channels {
                                ui.selectable_value(
                                    &mut input_channels,
                                    count,
                                    channel_label(count),
                                );
                            }
                        });
                    if input_channels == 1 {
                        let mono_row = ui.add_enabled_ui(has_two, |ui| {
                            egui::ComboBox::from_id_salt("input_mono_source")
                                .selected_text(mono_label(mono))
                                .width(80.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut mono, MonoSource::Left, "Left");
                                    ui.selectable_value(&mut mono, MonoSource::Right, "Right");
                                });
                        });
                        if !has_two {
                            mono_row.response.on_disabled_hover_text(
                                "The selected input has fewer than 2 channels; mono left is used.",
                            );
                        }
                    }
                });
                ui.end_row();
                if input_channels != self.project.settings.input.channels {
                    self.project.settings.input.channels = input_channels;
                    changed = true;
                }
                if has_two {
                    if mono != self.project.settings.input_mono_source {
                        self.project.settings.input_mono_source = mono;
                        changed = true;
                    }
                } else if self.project.settings.input_mono_source != MonoSource::Left {
                    self.project.settings.input_mono_source = MonoSource::Left;
                    changed = true;
                }
            });

        ui.add_space(6.0);
        ui.label(egui::RichText::new("OUTPUT").strong());
        egui::Grid::new("output_settings")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Output");
                let project_kind = self.project.settings.output_kind;
                let mut output_kind = if cfg!(debug_assertions) {
                    project_kind
                } else {
                    OutputKind::Device
                };
                egui::ComboBox::from_id_salt("output_kind")
                    .selected_text(output_kind_label(output_kind))
                    .width(combo_width)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut output_kind, OutputKind::Device, "Device");
                        #[cfg(debug_assertions)]
                        ui.selectable_value(
                            &mut output_kind,
                            OutputKind::VirtualMic,
                            "MicHost Virtual Microphone",
                        );
                    });
                ui.end_row();
                if cfg!(debug_assertions) && output_kind != project_kind {
                    self.project.settings.output_kind = output_kind;
                    changed = true;
                }

                #[cfg(debug_assertions)]
                if output_kind == OutputKind::VirtualMic {
                    ui.label("Driver");
                    if self.vmic_installed {
                        ui.label(egui::RichText::new("Installed").monospace());
                    } else {
                        ui.label(egui::RichText::new("Not installed").monospace())
                            .on_hover_text(
                                "The MicHost Virtual Microphone driver is a separate install. Without it the configured physical output is used.",
                            );
                    }
                    ui.end_row();
                    ui.label("Status");
                    ui.label(
                        egui::RichText::new(
                            "Install the driver separately, then select it as the app's microphone.",
                        )
                        .weak(),
                    );
                    ui.end_row();
                }
                if output_kind == OutputKind::Device {
                ui.label("API");
                let mut api = self.project.settings.output.api;
                if advanced {
                    egui::ComboBox::from_id_salt("output_api")
                        .selected_text(api_label(api))
                        .width(combo_width)
                        .show_ui(ui, |ui| {
                            for option in API_OPTIONS {
                                ui.selectable_value(&mut api, option, api_label(option));
                            }
                        });
                    ui.end_row();
                    if api != self.project.settings.output.api {
                        let mut selection = self.project.settings.output.clone();
                        set_selection_api(&self.devices.outputs, &mut selection, api, false);
                        self.project.settings.output = selection;
                        changed = true;
                    }
                } else {
                    ui.label(egui::RichText::new(automatic_api_label(api)).weak());
                    ui.end_row();
                }

                ui.label("Device");
                let outputs: Vec<String> = if advanced {
                    self.devices
                        .outputs
                        .iter()
                        .filter(|device| device.api == api && device.outputs > 0)
                        .map(|device| device.name.clone())
                        .collect()
                } else {
                    unique_device_names(&self.devices.outputs, false)
                };
                let selected = if self.project.settings.output.name.is_empty() {
                    "Select output".to_string()
                } else {
                    self.project.settings.output.name.clone()
                };
                let mut chosen = selected.clone();
                egui::ComboBox::from_id_salt("output_device")
                    .selected_text(&selected)
                    .width(combo_width)
                    .show_ui(ui, |ui| {
                        for name in &outputs {
                            ui.selectable_value(&mut chosen, name.clone(), name);
                        }
                    });
                ui.end_row();
                if chosen != selected {
                    self.select_output(&chosen);
                }

                ui.label("Channels");
                let mut output_channels = self.project.settings.output.channels.clamp(1, 2);
                egui::ComboBox::from_id_salt("output_channels")
                    .selected_text(if output_channels == 1 {
                        "1 (mono)".to_string()
                    } else {
                        "2 (stereo)".to_string()
                    })
                    .width(combo_width)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut output_channels, 1, "1 (mono)");
                        ui.selectable_value(&mut output_channels, 2, "2 (stereo)");
                    });
                ui.end_row();
                if output_channels != self.project.settings.output.channels {
                    self.project.settings.output.channels = output_channels;
                    changed = true;
                }
                }
            });

        ui.add_space(6.0);
        egui::CollapsingHeader::new("SECOND INPUT (SIDECHAIN)")
            .default_open(self.project.settings.sidechain_input_enabled)
            .show(ui, |ui| {
                let mut enabled = self.project.settings.sidechain_input_enabled;
                if ui
                    .checkbox(&mut enabled, "Enable second input device")
                    .changed()
                {
                    self.project.settings.sidechain_input_enabled = enabled;
                    if enabled && self.project.settings.sidechain_input.name.is_empty() {
                        if let Some(device) = self.devices.inputs.iter().find(|device| {
                            device.inputs > 0
                                && !(device.name == self.project.settings.input.name
                                    && device.api == self.project.settings.input.api)
                        }) {
                            self.project.settings.sidechain_input =
                                selection_for(device, device.inputs.min(2));
                        }
                    }
                    changed = true;
                }
                ui.add_enabled_ui(enabled, |ui| {
                    egui::Grid::new("sidechain_settings")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("API");
                            let mut api = self.project.settings.sidechain_input.api;
                            if advanced {
                                egui::ComboBox::from_id_salt("sidechain_api")
                                    .selected_text(api_label(api))
                                    .width(combo_width)
                                    .show_ui(ui, |ui| {
                                        for option in API_OPTIONS {
                                            ui.selectable_value(
                                                &mut api,
                                                option,
                                                api_label(option),
                                            );
                                        }
                                    });
                                ui.end_row();
                                if api != self.project.settings.sidechain_input.api {
                                    let mut selection =
                                        self.project.settings.sidechain_input.clone();
                                    set_selection_api(
                                        &self.devices.inputs,
                                        &mut selection,
                                        api,
                                        true,
                                    );
                                    self.project.settings.sidechain_input = selection;
                                    changed = true;
                                }
                            } else {
                                ui.label(egui::RichText::new(automatic_api_label(api)).weak());
                                ui.end_row();
                            }

                            ui.label("Device");
                            let inputs: Vec<String> = if advanced {
                                self.devices
                                    .inputs
                                    .iter()
                                    .filter(|device| {
                                        device.api == self.project.settings.sidechain_input.api
                                            && device.inputs > 0
                                    })
                                    .map(|device| device.name.clone())
                                    .collect()
                            } else {
                                unique_device_names(&self.devices.inputs, true)
                            };
                            let selected = if self.project.settings.sidechain_input.name.is_empty()
                            {
                                "Select sidechain".to_string()
                            } else {
                                self.project.settings.sidechain_input.name.clone()
                            };
                            let mut chosen = selected.clone();
                            egui::ComboBox::from_id_salt("sidechain_device")
                                .selected_text(&selected)
                                .width(combo_width)
                                .show_ui(ui, |ui| {
                                    for name in &inputs {
                                        ui.selectable_value(&mut chosen, name.clone(), name);
                                    }
                                });
                            ui.end_row();
                            if chosen != selected {
                                self.select_sidechain_input(&chosen);
                            }

                            ui.label("Channels");
                            let max_sidechain_channels = self
                                .devices
                                .inputs
                                .iter()
                                .find(|device| {
                                    device.api == self.project.settings.sidechain_input.api
                                        && device.name == self.project.settings.sidechain_input.name
                                })
                                .map(|device| device.inputs.clamp(1, 8))
                                .unwrap_or(8);
                            let mut channels = self
                                .project
                                .settings
                                .sidechain_input
                                .channels
                                .clamp(1, max_sidechain_channels);
                            egui::ComboBox::from_id_salt("sidechain_channels")
                                .selected_text(channel_label(channels))
                                .width(combo_width)
                                .show_ui(ui, |ui| {
                                    for count in 1..=max_sidechain_channels {
                                        ui.selectable_value(
                                            &mut channels,
                                            count,
                                            channel_label(count),
                                        );
                                    }
                                });
                            ui.end_row();
                            if channels != self.project.settings.sidechain_input.channels {
                                self.project.settings.sidechain_input.channels = channels;
                                changed = true;
                            }
                        });
                });
            });

        ui.add_space(6.0);
        ui.label(egui::RichText::new("FORMAT").strong());
        let virtual_mic_output =
            cfg!(debug_assertions) && self.project.settings.output_kind == OutputKind::VirtualMic;
        let input_buffers = self.buffer_options(true);
        let output_buffers = self.buffer_options(false);
        let input_default_label = default_buffer_label(self.actual_buffer(true));
        let output_default_label = default_buffer_label(self.actual_buffer(false));
        let format_width = 150.0;
        ui.horizontal(|ui| {
            if !virtual_mic_output {
                ui.vertical(|ui| {
                    ui.label("Safety buffer").on_hover_text(
                        "Extra output queue. Auto is safest; a smaller value lowers latency but may underrun.",
                    );
                    let mut safety = self.project.settings.safety_buffer_ms;
                    egui::ComboBox::from_id_salt("safety_buffer")
                        .selected_text(if safety == 0 {
                            "Auto".to_string()
                        } else {
                            format!("{safety} ms")
                        })
                        .width(format_width)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut safety, 0, "Auto");
                            for option in [2, 5, 10, 20, 40] {
                                ui.selectable_value(&mut safety, option, format!("{option} ms"));
                            }
                        });
                    if safety != self.project.settings.safety_buffer_ms {
                        self.project.settings.safety_buffer_ms = safety;
                        changed = true;
                    }
                });
                ui.add_space(10.0);
            }
            ui.vertical(|ui| {
                ui.label("Sample rate");
                let mut rate = self.project.settings.sample_rate;
                egui::ComboBox::from_id_salt("sample_rate")
                    .selected_text(if rate <= 0.0 {
                        "Device default".to_string()
                    } else {
                        format!("{rate:.0} Hz")
                    })
                    .width(format_width)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut rate, 0.0, "Device default");
                        for option in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0] {
                            ui.selectable_value(&mut rate, option, format!("{option:.0} Hz"));
                        }
                    });
                if rate != self.project.settings.sample_rate {
                    self.project.settings.sample_rate = rate;
                    changed = true;
                }
            });
        });

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label("Input buffer").on_hover_text(
                    "Input device and graph block. Independent of the output buffer. 0 uses the device default.",
                );
                let mut buffer = self.project.settings.buffer_size;
                egui::ComboBox::from_id_salt("buffer_size")
                    .selected_text(if buffer == 0 {
                        input_default_label.clone()
                    } else {
                        format!("{buffer} frames")
                    })
                    .width(format_width)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut buffer, 0, &input_default_label);
                        for option in &input_buffers {
                            ui.selectable_value(
                                &mut buffer,
                                *option,
                                format!("{option} frames"),
                            );
                        }
                    });
                if buffer != self.project.settings.buffer_size {
                    self.project.settings.buffer_size = buffer;
                    changed = true;
                }
            });
            if !virtual_mic_output {
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label("Output buffer").on_hover_text(
                        "Output device buffer, independent of the input. Lower values reduce the host output queue but may underrun. 0 uses the device default.",
                    );
                    let mut output_buffer = self.project.settings.output_buffer_size;
                    egui::ComboBox::from_id_salt("output_buffer_size")
                        .selected_text(if output_buffer == 0 {
                            output_default_label.clone()
                        } else {
                            format!("{output_buffer} frames")
                        })
                        .width(format_width)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut output_buffer, 0, &output_default_label);
                            for option in &output_buffers {
                                ui.selectable_value(
                                    &mut output_buffer,
                                    *option,
                                    format!("{option} frames"),
                                );
                            }
                        });
                    if output_buffer != self.project.settings.output_buffer_size {
                        self.project.settings.output_buffer_size = output_buffer;
                        changed = true;
                    }
                });
            }
        });

        ui.horizontal(|ui| {
            ui.label("Chain latency").on_hover_text(
                "Run a 2 s chain-inclusive marker test: a click train is injected before the plugin chain and captured at the configured output. Audio keeps running; the marker is briefly audible in the virtual mic.",
            );
            if self.measuring {
                ui.spinner();
                ui.label("Measuring chain...");
            } else if ui.button("Measure").clicked() {
                self.request_measure();
            }
            if let Some(measured) = &self.measured {
                ui.label(format!("Measured {:.1} ms", measured.p50_ms)).on_hover_text(format!(
                    "Chain-inclusive marker p50 {:.1} ms, p95 {:.1} ms (min {:.1}, max {:.1}); {} matched of {} clicks.
Includes graph, plugin chain, host output queue, output device, cable and capture; excludes the mic input and the receiving app. The marker is briefly audible in the virtual mic.",
                    measured.p50_ms,
                    measured.p95_ms,
                    measured.min_ms,
                    measured.max_ms,
                    measured.matched,
                    measured.clicks
                ));
            }
        });

        ui.add_space(6.0);
        ui.label(egui::RichText::new("STARTUP").strong());
        let mut advanced_mode = self.project.settings.advanced_mode;
        let mut close_to_tray = self.project.settings.close_to_tray;
        let mut launch = self.project.settings.launch_at_login;
        let (advanced_toggled, close_toggled, launch_toggled) = ui
            .horizontal_wrapped(|ui| {
                let advanced_toggled = ui
                    .checkbox(&mut advanced_mode, "Advanced Mode")
                    .on_hover_text(
                        "Show manual API selection for input, output and sidechain devices",
                    )
                    .changed();
                let close_toggled = ui
                    .checkbox(&mut close_to_tray, "Close to tray")
                    .on_hover_text(
                        "Closing the window keeps MicHost and the engine running in the tray",
                    )
                    .changed();
                let launch_toggled = ui.checkbox(&mut launch, "Launch at login").changed();
                (advanced_toggled, close_toggled, launch_toggled)
            })
            .inner;
        if advanced_toggled {
            self.project.settings.advanced_mode = advanced_mode;
            let api_changed = if advanced_mode {
                false
            } else {
                self.apply_automatic_apis()
            };
            if api_changed {
                self.settings_changed();
            } else {
                self.mark_dirty();
            }
        }
        if close_toggled {
            self.project.settings.close_to_tray = close_to_tray;
            self.mark_dirty();
        }
        if launch_toggled {
            match autostart::set_enabled(launch) {
                Ok(()) => {
                    self.project.settings.launch_at_login = launch;
                    self.mark_dirty();
                }
                Err(error) => self.last_error = Some(error),
            }
        }
        if changed {
            self.settings_changed();
        }
    }

    fn ui_monitor_settings(&mut self, ctx: &egui::Context) {
        if !self.show_monitor_settings {
            return;
        }
        let mut open = self.show_monitor_settings;
        let outputs = &self.devices.outputs;
        let default_output = default_device(outputs).cloned();
        let configured = self.project.settings.monitor_output.clone();
        let mut follow_default = configured.name.is_empty();
        let mut selection = if follow_default {
            default_output
                .as_ref()
                .map(|device| selection_for(device, device.outputs.min(2)))
                .unwrap_or_default()
        } else {
            configured.clone()
        };
        let main_output = self.project.settings.output.clone();
        let mut monitor_trim = self.project.settings.monitor_trim_db.clamp(-24.0, 12.0);
        let mut trim_changed = false;
        let mut changed = false;

        egui::Window::new("Monitor output")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(450.0)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Plays the processed chain on a second output. Use headphones to avoid feedback.",
                    )
                    .weak(),
                );
                ui.label(
                    egui::RichText::new(
                        "If the interface's direct monitoring or loopback is on, its dry input is mixed with this processed monitor. Turn it off in the device control panel when comparing against the WASAPI output.",
                    )
                    .weak(),
                );
                ui.add_space(4.0);
                if ui
                    .checkbox(
                        &mut follow_default,
                        "Use the system default playback device",
                    )
                    .changed()
                {
                    if follow_default {
                        selection = default_output
                            .as_ref()
                            .map(|device| selection_for(device, device.outputs.min(2)))
                            .unwrap_or_default();
                    } else if let Some(device) = outputs
                        .iter()
                        .find(|device| device.outputs > 0 && device.name != main_output.name)
                        .or_else(|| outputs.iter().find(|device| device.outputs > 0))
                    {
                        selection = selection_for(device, device.outputs.min(2));
                    }
                    changed = true;
                }

                if follow_default {
                    ui.label(
                        egui::RichText::new(format!(
                            "System default: {}",
                            default_output
                                .as_ref()
                                .map(|device| device.name.as_str())
                                .unwrap_or("not available")
                        ))
                        .weak(),
                    );
                } else {
                    ui.horizontal(|ui| {
                        ui.label("API");
                        let mut apis: Vec<AudioApi> = Vec::new();
                        for device in outputs.iter().filter(|device| device.outputs > 0) {
                            if !apis.contains(&device.api) {
                                apis.push(device.api);
                            }
                        }
                        egui::ComboBox::from_id_salt("monitor_api")
                            .selected_text(api_label(selection.api))
                            .width(150.0)
                            .show_ui(ui, |ui| {
                                for api in apis {
                                    if ui
                                        .selectable_value(
                                            &mut selection.api,
                                            api,
                                            api_label(api),
                                        )
                                        .changed()
                                    {
                                        if let Some(device) = outputs.iter().find(|device| {
                                            device.outputs > 0 && device.api == api
                                        }) {
                                            selection = selection_for(
                                                device,
                                                device.outputs.min(2),
                                            );
                                        }
                                        changed = true;
                                    }
                                }
                            });
                    });
                    ui.horizontal(|ui| {
                        ui.label("Device");
                        let selected = if selection.name.is_empty() {
                            "Select device".to_string()
                        } else {
                            short_text(&selection.name, 34)
                        };
                        let selected_api = selection.api;
                        egui::ComboBox::from_id_salt("monitor_device")
                            .selected_text(selected)
                            .width(240.0)
                            .show_ui(ui, |ui| {
                                for device in outputs.iter().filter(|device| {
                                    device.outputs > 0 && device.api == selected_api
                                }) {
                                    if ui
                                        .selectable_value(
                                            &mut selection.name,
                                            device.name.clone(),
                                            short_text(&device.name, 40),
                                        )
                                        .on_hover_text(&device.name)
                                        .changed()
                                    {
                                        selection = selection_for(
                                            device,
                                            device.outputs.min(2),
                                        );
                                        changed = true;
                                    }
                                }
                            });
                    });
                    let max_outputs = outputs
                        .iter()
                        .find(|device| {
                            device.outputs > 0
                                && device.name == selection.name
                                && device.api == selection.api
                        })
                        .map(|device| device.outputs)
                        .unwrap_or(0);
                    ui.horizontal(|ui| {
                        ui.label("Output");
                        let current_start = if selection.channel_mask != 0 {
                            selection.channel_mask.trailing_zeros() & !1
                        } else {
                            0
                        };
                        let current_len = selection.channels.clamp(1, 2);
                        let current_label = if max_outputs <= 1 {
                            "Ch 1".to_string()
                        } else if current_len == 1 {
                            format!("Ch {}", current_start + 1)
                        } else {
                            format!(
                                "Ch {}-{}",
                                current_start + 1,
                                (current_start + 2).min(max_outputs)
                            )
                        };
                        egui::ComboBox::from_id_salt("monitor_channels")
                            .selected_text(current_label)
                            .width(110.0)
                            .show_ui(ui, |ui| {
                                if max_outputs <= 1 {
                                    ui.label("Ch 1");
                                } else {
                                    for start in (0..max_outputs).step_by(2) {
                                        let end = (start + 2).min(max_outputs);
                                        let mask = if end == start + 1 {
                                            1u64 << start
                                        } else {
                                            (3u64 << start) & ((1u64 << end) - 1)
                                        };
                                        let label = if end == start + 1 {
                                            format!("Ch {}", start + 1)
                                        } else {
                                            format!("Ch {}-{}", start + 1, end)
                                        };
                                        if ui
                                            .selectable_value(
                                                &mut selection.channel_mask,
                                                mask,
                                                label,
                                            )
                                            .changed()
                                        {
                                            selection.channels = end - start;
                                            changed = true;
                                        }
                                    }
                                }
                            });
                    });
                    if outputs.iter().all(|device| {
                        device.outputs == 0
                            || device.name != selection.name
                            || device.api != selection.api
                    }) {
                        ui.label(
                            egui::RichText::new("This monitor device is not currently available.")
                                .color(egui::Color32::from_rgb(230, 180, 80)),
                        );
                    }
                    if selection.name == main_output.name && selection.api == main_output.api {
                        ui.label(
                            egui::RichText::new(
                                "This is also the main output; use headphones or choose another device.",
                            )
                            .color(egui::Color32::from_rgb(230, 180, 80)),
                        );
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Trim").on_hover_text(
                        "Monitor-only level. ASIO bypasses the Windows endpoint volume; use this to match the WASAPI output. The main output is not affected.",
                    );
                    if ui
                        .add(
                            egui::Slider::new(&mut monitor_trim, -24.0..=12.0)
                                .suffix(" dB")
                                .fixed_decimals(1),
                        )
                        .changed()
                    {
                        trim_changed = true;
                    }
                    if ui.small_button("Reset").clicked() {
                        monitor_trim = 0.0;
                        trim_changed = true;
                    }
                });
            });

        self.show_monitor_settings = open;
        if trim_changed {
            self.project.settings.monitor_trim_db = monitor_trim;
            if let Some(session) = self.session.as_mut() {
                session.set_monitor_trim(monitor_trim);
            }
            self.mark_dirty();
        }
        if changed {
            self.project.settings.monitor_output = if follow_default {
                DeviceSelection::default()
            } else {
                selection
            };
            self.monitor_settings_changed();
        }
    }

    fn ui_chain(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("Plugins");
            ui.label(egui::RichText::new(format!("{}", self.project.chain.len())).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.add.open && ui.button("Close picker").clicked() {
                    self.add.open = false;
                    self.add.search.clear();
                }
                let scan_label = if self.scan.active {
                    "Scanning..."
                } else {
                    "Scan VST3"
                };
                if ui
                    .add_enabled(!self.scan.active, egui::Button::new(scan_label))
                    .clicked()
                {
                    self.start_scan();
                }
                if ui
                    .button("Folders")
                    .on_hover_text("Manage default and custom VST3 scan folders")
                    .clicked()
                {
                    self.show_vst3_folders = true;
                }
            });
        });
        if let Some(pending) = &self.pending_add {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Loading {}...", pending.name));
            });
        }
        if !self.add.open {
            let hint = if self.session.is_some() {
                "Audio flows top to bottom. Drag the grip or use Up/Down to reorder. Bypass passes audio through unchanged."
            } else {
                "Audio flows top to bottom. Start audio to open plugin editors. Drag the grip or use Up/Down to reorder."
            };
            ui.label(egui::RichText::new(hint).weak());
        }
        if self.scan.active {
            ui.horizontal(|ui| {
                ui.spinner();
                if self.scan.total > 0 {
                    ui.label(format!("{}/{} modules", self.scan.done, self.scan.total));
                } else {
                    ui.label("Scanning plugin modules...");
                }
            });
        }

        if self.add.open {
            self.ui_add_plugin(ui);
            return;
        }

        if self.project.chain.is_empty() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(18.0);
                    ui.label(egui::RichText::new("No plugins yet").strong());
                    ui.label(
                        egui::RichText::new(
                            "With no plugins, the microphone passes straight to the output when audio is running. Use + below to add a plugin.",
                        )
                        .weak(),
                    );
                    ui.add_space(18.0);
                });
            });
            self.ui_add_plugin_button(ui);
            return;
        }

        let mut action = None;
        let count = self.project.chain.len();
        let footer = ADD_PLUGIN_BUTTON_HEIGHT + 10.0;
        let scroll_height = (ui.available_height() - footer).max(80.0);
        egui::ScrollArea::vertical()
            .max_height(scroll_height)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for index in 0..count {
                    if let Some(next) = self.ui_chain_row(ui, index) {
                        action = Some(next);
                    }
                }
            });
        if let Some(action) = action {
            self.apply_chain_action(action);
        }
        ui.add_space(4.0);
        self.ui_add_plugin_button(ui);
    }

    fn ui_add_plugin_button(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let add = ui
            .add_enabled_ui(self.pending_add.is_none(), |ui| {
                ui.add_sized(
                    [ui.available_width(), ADD_PLUGIN_BUTTON_HEIGHT],
                    egui::Button::new(egui::RichText::new("+  Add Plugin").size(16.0)),
                )
            })
            .inner;
        if add.clicked() {
            self.add.open = true;
            self.add.focus_search = true;
        }
        add.on_hover_text("Add a plugin to the chain")
            .on_disabled_hover_text("Wait for the plugin to finish loading");
    }

    fn ui_chain_row(&mut self, ui: &mut egui::Ui, index: usize) -> Option<ChainAction> {
        let entry = self.project.chain.get(index)?.clone();
        let count = self.project.chain.len();
        let id = entry.id;
        let name = entry.plugin.name.clone();
        let vendor = entry.plugin.vendor.clone();
        let native_sidechain = entry.plugin.sidechain_inputs;
        let stats = self.session.as_ref().map(|session| {
            let samples = session.plugin_latency(id).unwrap_or(0);
            let milliseconds = session.plugin_latency_ms(id).unwrap_or(0.0);
            let cpu = session.plugin_cpu_fraction(id).unwrap_or(0.0) * 100.0;
            let memory = session
                .plugin_memory_bytes(id)
                .map(|bytes| format!("~{:.0} MB", bytes as f64 / 1_048_576.0))
                .unwrap_or_else(|| "—".to_string());
            (
                samples,
                format!("{milliseconds:.1} ms · CPU {cpu:.1}% · RAM {memory}"),
            )
        });
        let can_edit = self.session.is_some();
        let captured = self.project.settings.input.channels.clamp(1, 8) as u16;
        let second_enabled = self.project.settings.sidechain_input_enabled;
        let aux_channels = self.project.settings.sidechain_input.channels.clamp(1, 8);
        let mut action = None;

        let frame = egui::Frame::group(ui.style()).inner_margin(egui::Margin::symmetric(10, 8));
        let (_, payload) = ui.dnd_drop_zone::<usize, _>(frame, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.dnd_drag_source(egui::Id::new(("chain_grip", id)), index, |ui| {
                    ui.label(egui::RichText::new("::").monospace().size(16.0).weak())
                        .on_hover_text("Drag to reorder");
                });
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{}. {}", index + 1, name)).strong(),
                    )
                    .truncate(),
                )
                .on_hover_text(&name);
                if !vendor.is_empty() {
                    ui.add(egui::Label::new(egui::RichText::new(vendor).weak()).truncate());
                }
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let bus_text = if entry.plugin.main_inputs == 0 && entry.plugin.main_outputs == 0 {
                    "Bus info after Start".to_string()
                } else {
                    format!(
                        "{} in / {} out",
                        entry.plugin.main_inputs, entry.plugin.main_outputs
                    )
                };
                ui.label(egui::RichText::new(bus_text).weak())
                    .on_hover_text("Bus counts are read from the loaded plugin.");
                let mut bypassed = entry.bypassed;
                if ui
                    .checkbox(&mut bypassed, "Bypass")
                    .on_hover_text("Pass audio through this plugin without processing")
                    .changed()
                {
                    action = Some(ChainAction::Bypass(index, bypassed));
                }
                let mut oversampling =
                    michost_graph::normalize_oversampling(entry.oversampling);
                egui::ComboBox::from_id_salt(("oversampling", id))
                    .selected_text(match oversampling {
                        2 => "OS 2x",
                        4 => "OS 4x",
                        _ => "OS off",
                    })
                    .width(64.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut oversampling, 1, "OS off");
                        ui.selectable_value(&mut oversampling, 2, "OS 2x");
                        ui.selectable_value(&mut oversampling, 4, "OS 4x");
                    })
                    .response
                    .on_hover_text(
                        "Oversample this plugin only. 2x/4x raise its processing rate; some plugins only support 44.1/48/96 kHz.",
                    );
                if oversampling != entry.oversampling {
                    action = Some(ChainAction::Oversampling(index, oversampling));
                }
                if native_sidechain > 0 {
                    let mut source = entry.sidechain_source;
                    egui::ComboBox::from_id_salt(("sidechain", id))
                        .selected_text(sidechain_label(source))
                        .width(112.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut source, SidechainSource::Off, "Off");
                            ui.selectable_value(&mut source, SidechainSource::MainMic, "Main mic");
                            for channel in 1..captured {
                                ui.selectable_value(
                                    &mut source,
                                    SidechainSource::InputChannel(channel),
                                    format!("Input {}", channel + 1),
                                );
                            }
                            if second_enabled {
                                ui.separator();
                                for channel in 0..aux_channels {
                                    ui.selectable_value(
                                        &mut source,
                                        SidechainSource::SecondInput(channel as u16),
                                        format!("Aux {}", channel + 1),
                                    );
                                }
                            }
                            ui.selectable_value(&mut source, SidechainSource::Silence, "Silence");
                        });
                    if source != entry.sidechain_source {
                        action = Some(ChainAction::Sidechain(index, source));
                    }
                }
                });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if let Some((samples, text)) = &stats {
                    ui.label(egui::RichText::new(text).weak())
                        .on_hover_text(format!("{samples} samples"));
                } else {
                    ui.label(egui::RichText::new("—").weak());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("Remove")
                        .on_hover_text("Remove this plugin from the chain")
                        .clicked()
                    {
                        action = Some(ChainAction::Remove(index));
                    }
                    if ui
                        .add_enabled(index + 1 < count, egui::Button::new("Down"))
                        .on_hover_text("Move down")
                        .clicked()
                    {
                        action = Some(ChainAction::Move(index, index + 1));
                    }
                    if ui
                        .add_enabled(index > 0, egui::Button::new("Up"))
                        .on_hover_text("Move up")
                        .clicked()
                    {
                        action = Some(ChainAction::Move(index, index - 1));
                    }
                    if ui
                        .add_enabled(can_edit, egui::Button::new("Edit"))
                        .on_disabled_hover_text("Start audio to open the plugin editor")
                        .on_hover_text("Open the plugin editor")
                        .clicked()
                    {
                        action = Some(ChainAction::OpenEditor(index));
                    }
            });
            });
        });
        if let Some(from) = payload {
            if *from != index {
                action = Some(ChainAction::Move(*from, index));
            }
        }
        action
    }

    fn ui_add_plugin(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Add a MicHost built-in effect or pick an installed VST3 plugin.")
                .weak(),
        );
        let search_edit = ui.add(
            egui::TextEdit::singleline(&mut self.add.search)
                .hint_text("Search by name, vendor or category")
                .desired_width(f32::INFINITY),
        );
        if self.add.focus_search {
            search_edit.request_focus();
            self.add.focus_search = false;
        }

        let search = normalize_search(&self.add.search);
        let mut builtin_choice = None;
        if cfg!(debug_assertions) {
            let builtins: Vec<BuiltinKind> = BuiltinKind::ALL
                .into_iter()
                .filter(|kind| {
                    search.is_empty()
                        || descriptor_score(&self.add.search, &builtin_descriptor(*kind)).is_some()
                })
                .collect();
            if !builtins.is_empty() {
                ui.add_space(2.0);
                ui.label(egui::RichText::new("MicHost built-ins").strong());
                ui.horizontal_wrapped(|ui| {
                    for kind in builtins {
                        let response = ui
                            .add_sized([150.0, 24.0], egui::Button::new(kind.name()))
                            .on_hover_text(format!("{} · built-in", kind.category()));
                        if response.clicked() {
                            builtin_choice = Some(kind);
                        }
                    }
                });
                ui.separator();
            }
        }
        if let Some(kind) = builtin_choice {
            self.add.open = false;
            self.add.search.clear();
            self.add.focus_search = false;
            self.begin_add_builtin(kind);
            return;
        }

        if self.plugins.is_empty() {
            ui.add_space(8.0);
            ui.label("No VST3 plugins cached yet.");
            if ui.button("Scan VST3 plugins").clicked() {
                self.start_scan();
            }
            return;
        }
        if !self.scan.active
            && self
                .plugins
                .iter()
                .all(|descriptor| descriptor.category.is_empty())
        {
            ui.label(
                egui::RichText::new(
                    "Plugin types are missing from the cache. Press Scan VST3 to refresh them.",
                )
                .weak(),
            );
        }
        ui.horizontal(|ui| {
            ui.label("Sort by");
            ui.selectable_value(&mut self.add.grouping, PickerGrouping::Vendor, "Vendor");
            ui.selectable_value(&mut self.add.grouping, PickerGrouping::Category, "Category");
        });

        let search = normalize_search(&self.add.search);
        let generation = self.plugin_generation;
        let grouping = self.add.grouping;
        let stale = self.add.picker.as_ref().is_none_or(|cache| {
            cache.generation != generation || cache.search != search || cache.grouping != grouping
        });
        if stale {
            self.add.picker = Some(build_picker_cache(
                &self.plugins,
                generation,
                search,
                grouping,
            ));
        }

        let mut choice = None;
        if let Some(cache) = self.add.picker.as_ref() {
            ui.label(
                egui::RichText::new(format!(
                    "{} plugins cached · {} shown",
                    self.plugins.len(),
                    cache.shown
                ))
                .weak(),
            );
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(ui.available_height())
                .show(ui, |ui| {
                    for (group, entries) in &cache.groups {
                        egui::CollapsingHeader::new(format!("{} ({})", group, entries.len()))
                            .default_open(!cache.search.is_empty())
                            .show(ui, |ui| {
                                for (index, _score) in entries {
                                    let descriptor = &self.plugins[*index];
                                    let category = display_category(&descriptor.category);
                                    let category_detail = if category.is_empty() {
                                        "uncategorised".to_string()
                                    } else {
                                        category.clone()
                                    };
                                    let text = if category.is_empty()
                                        || cache.grouping == PickerGrouping::Category
                                    {
                                        descriptor.name.clone()
                                    } else {
                                        format!("{}  ·  {}", descriptor.name, category)
                                    };
                                    let details = if descriptor.main_inputs == 0
                                        && descriptor.main_outputs == 0
                                    {
                                        format!(
                                            "{} · {} · bus counts are read when the plugin loads",
                                            descriptor.vendor, category_detail
                                        )
                                    } else {
                                        format!(
                                            "{} · {} · {} in / {} out{}",
                                            descriptor.vendor,
                                            category_detail,
                                            descriptor.main_inputs,
                                            descriptor.main_outputs,
                                            if descriptor.sidechain_inputs > 0 {
                                                format!(" · SC {}", descriptor.sidechain_inputs)
                                            } else {
                                                String::new()
                                            }
                                        )
                                    };
                                    if ui
                                        .add_sized(
                                            [ui.available_width(), 26.0],
                                            egui::Button::new(text),
                                        )
                                        .on_hover_text(details)
                                        .clicked()
                                    {
                                        choice = Some(*index);
                                    }
                                }
                            });
                    }
                });
        }
        if let Some(index) = choice {
            let descriptor = self.plugins[index].clone();
            self.add.open = false;
            self.add.search.clear();
            self.add.focus_search = false;
            self.apply_chain_action(ChainAction::Add(descriptor));
        }
    }

    fn device_panel_direction(&self) -> Option<bool> {
        if self.devices.inputs.iter().any(|device| {
            device.api == self.project.settings.input.api
                && device.name == self.project.settings.input.name
                && device.has_control_panel
        }) {
            Some(true)
        } else if self.devices.outputs.iter().any(|device| {
            device.api == self.project.settings.output.api
                && device.name == self.project.settings.output.name
                && device.has_control_panel
        }) {
            Some(false)
        } else {
            None
        }
    }

    fn ui_footer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("New project...")
                .on_hover_text("Create a new empty chain and save it to a new project file")
                .clicked()
            {
                self.new_project();
            }
            if ui.button("Save project as...").clicked() {
                self.save_project_as();
            }
            if ui.button("Open project...").clicked() {
                self.open_project();
            }
            if ui
                .button("Rescan")
                .on_hover_text("Stop audio, re-enumerate devices, and refresh the device lists")
                .clicked()
            {
                self.refresh_devices();
            }
            if let Some(input) = self.device_panel_direction() {
                if ui
                    .add_enabled(self.session.is_some(), egui::Button::new("Device panel"))
                    .on_disabled_hover_text("Start audio to open the device control panel")
                    .clicked()
                {
                    self.open_device_panel(input);
                }
            }
            if self.device_worker.is_some() {
                ui.spinner();
            }
            ui.separator();
            let path = self
                .project_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| {
                    Project::default_path()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "no project path".to_string())
                });
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("Project: {}", short_text(&path, 40))).weak(),
                )
                .truncate(),
            )
            .on_hover_text(&path);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.dirty {
                    let status = if self.save_deadline.is_some() {
                        "Saving..."
                    } else {
                        "Unsaved changes"
                    };
                    ui.label(egui::RichText::new(status).weak());
                } else if let Some(until) = self.saved_flash_until {
                    let remaining = until.saturating_duration_since(Instant::now());
                    if !remaining.is_zero() {
                        let alpha = remaining.as_secs_f32().min(1.0);
                        ui.label(
                            egui::RichText::new("Auto-saved").color(
                                egui::Color32::from_rgb(110, 210, 140).gamma_multiply(alpha),
                            ),
                        );
                    }
                }
            });
        });
    }
}

impl eframe::App for MicHostApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.options_mut(|options| {
            options.zoom_with_keyboard = false;
        });
        if (ctx.zoom_factor() - 1.0).abs() > f32::EPSILON {
            ctx.set_zoom_factor(1.0);
        }
        let now = Instant::now();
        self.poll_tray(ctx);
        if ctx.input(|input| input.viewport().close_requested()) && !self.quitting {
            if self.project.settings.close_to_tray {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.hide_window(ctx);
            } else {
                logging::line("quit requested by the window close button");
                self.quitting = true;
                self.save_now();
                arm_quit_watchdog();
            }
        }
        if self.activation.as_ref().is_some_and(Activation::poll) {
            self.show_window(ctx);
        }
        self.poll_devices();
        self.poll_scan();
        self.poll_backend();
        self.poll_session_worker();
        if let Some(session) = self.session.as_mut() {
            session.poll_monitor();
        }
        self.poll_measure();
        if self.auto_measure
            && self.state == EngineState::Running
            && self.session.is_some()
            && !self.measuring
        {
            self.auto_measure = false;
            self.request_measure();
        }
        if now >= self.plugin_edit_at && self.session.is_some() {
            self.plugin_edit_at = now + Duration::from_millis(500);
            let changed = if let Some(session) = &self.session {
                session.take_plugin_edits(&mut self.project)
            } else {
                false
            };
            if changed {
                self.measured = None;
                let now = Instant::now();
                if now >= self.plugin_autosave_at {
                    self.plugin_autosave_at = now + Duration::from_secs(5);
                    self.mark_dirty();
                } else {
                    self.dirty = true;
                }
            }
        }
        self.poll_pending_add();
        self.poll_latency();
        let device_changes = self
            .backend
            .as_ref()
            .is_some_and(|backend| backend.take_device_changes());
        if device_changes {
            if !self.device_change_pending {
                logging::line("audio device list changed");
                if self.session.is_some() {
                    self.last_error = Some(
                        "audio devices changed; restart audio to apply the new device list"
                            .to_string(),
                    );
                }
            }
            self.device_change_pending = true;
        }
        if self.device_change_pending
            && Instant::now() >= self.device_change_quiet_until
            && self.session.is_none()
            && self.session_worker.is_none()
            && self.device_worker.is_none()
        {
            self.device_change_pending = false;
            self.refresh_devices();
        }
        if let Some(session) = self.session.as_mut() {
            session.collect_retired();
        }

        if let Some(deadline) = self.save_deadline {
            if Instant::now() >= deadline {
                self.save_now();
            }
        }
        if self.dirty && self.save_deadline.is_none() && Instant::now() >= self.plugin_autosave_at {
            self.save_now();
        }
        if self
            .saved_flash_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.saved_flash_until = None;
        }

        #[cfg(debug_assertions)]
        if self.screenshot_path.is_some() {
            self.poll_screenshot(ctx);
        }

        let pointer_only = ctx.input(|input| {
            !input.events.is_empty()
                && input
                    .events
                    .iter()
                    .all(|event| matches!(event, egui::Event::PointerMoved(_)))
                && !input.pointer.any_down()
        });
        if pointer_only {
            let elapsed = self.last_frame.elapsed();
            if elapsed < POINTER_REPAINT_INTERVAL {
                std::thread::sleep(POINTER_REPAINT_INTERVAL - elapsed);
            }
        }
        self.last_frame = Instant::now();

        if !self.first_frame_logged {
            self.first_frame_logged = true;
            logging::line(format!(
                "first frame in {:.1} ms",
                self.created_at.elapsed().as_secs_f64() * 1000.0
            ));
        }

        let minimized = ctx.input(|input| input.viewport().minimized.unwrap_or(false));
        if self.hidden || minimized {
            if minimized {
                self.builtin_editors.clear();
            }
            self.update_tray_tooltip();
            self.ensure_engine(Instant::now());
            ctx.request_repaint_after(Duration::from_secs(1));
            return;
        }

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            self.ui_header(ui);
        });
        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            self.ui_footer(ui);
        });
        egui::SidePanel::left("audio_settings")
            .resizable(false)
            .exact_width(380.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                    )
                    .show(ui, |ui| {
                        self.ui_devices(ui);
                    });
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_chain(ui);
        });

        self.ui_vst3_folders(ctx);
        self.ui_monitor_settings(ctx);
        self.show_builtin_editors(ctx);

        self.update_tray_tooltip();

        self.ensure_engine(Instant::now());

        let flash_active = self.saved_flash_until.is_some();
        let interval = if self.state == EngineState::Running || flash_active {
            Duration::from_millis(200)
        } else {
            Duration::from_millis(500)
        };
        ctx.request_repaint_after(interval);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.quit_now();
    }
}

fn arm_quit_watchdog() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(10));
        logging::line("MicHost quit is still running after 10 s; forcing exit");
        std::process::exit(0);
    });
}

fn default_buffer_label(size: Option<u32>) -> String {
    match size {
        Some(size) => format!("Device default ({size})"),
        None => "Device default".to_string(),
    }
}

fn monitor_button(ui: &mut egui::Ui, active: bool, enabled: bool) -> egui::Response {
    let size = egui::vec2(24.0, 22.0);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        if enabled && response.hovered() {
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(4), visuals.weak_bg_fill);
        }
        let color = if !enabled {
            ui.visuals().weak_text_color()
        } else if active {
            egui::Color32::from_rgb(86, 196, 126)
        } else {
            visuals.fg_stroke.color
        };
        if active {
            ui.painter().rect_filled(
                rect,
                egui::CornerRadius::same(4),
                color.gamma_multiply(0.18),
            );
        }

        let center = rect.center();
        let cx = center.x;
        let cy = center.y + 0.75;
        let radius = 6.5;
        let cup_size = egui::vec2(4.0, 6.0);
        let cup_offset = 5.5;

        for offset in [-cup_offset, cup_offset] {
            let cup = egui::Rect::from_center_size(egui::pos2(cx + offset, cy + 2.0), cup_size);
            ui.painter()
                .rect_filled(cup, egui::CornerRadius::same(2), color);
        }

        let mut points = Vec::with_capacity(25);
        for step in 0..=24 {
            let angle = std::f32::consts::PI + std::f32::consts::PI * (step as f32) / 24.0;
            points.push(egui::pos2(
                cx + radius * angle.cos(),
                cy + radius * angle.sin(),
            ));
        }
        ui.painter()
            .add(egui::Shape::line(points, egui::Stroke::new(1.5_f32, color)));
    }
    response
}

const API_OPTIONS: [AudioApi; 4] = [
    AudioApi::WasapiShared,
    AudioApi::WasapiLowLatency,
    AudioApi::WasapiExclusive,
    AudioApi::Asio,
];

fn api_label(api: AudioApi) -> &'static str {
    match api {
        AudioApi::WasapiShared => "WASAPI shared",
        AudioApi::WasapiLowLatency => "WASAPI low latency",
        AudioApi::WasapiExclusive => "WASAPI exclusive",
        AudioApi::Asio => "ASIO",
        AudioApi::Other => "Other",
    }
}

fn output_kind_label(kind: OutputKind) -> &'static str {
    match kind {
        OutputKind::Device => "Device",
        OutputKind::VirtualMic => "MicHost Virtual Microphone",
    }
}

fn selection_for(device: &DeviceInfo, channels: u32) -> DeviceSelection {
    DeviceSelection {
        api: device.api,
        name: device.name.clone(),
        id: device.id.clone(),
        channels: channels.max(1),
        channel_mask: 0,
    }
}

fn find_loopback_input(inputs: &[DeviceInfo], output: &DeviceInfo) -> Option<DeviceInfo> {
    let lower = output.name.to_lowercase();
    let wanted = lower.replace("input", "output");
    let rank = |api: AudioApi| match api {
        AudioApi::WasapiShared => 0,
        AudioApi::WasapiLowLatency => 1,
        AudioApi::WasapiExclusive => 2,
        _ => 3,
    };
    let find = |name: &str| {
        inputs
            .iter()
            .filter(|device| device.inputs > 0 && device.name.to_lowercase() == name)
            .min_by_key(|device| rank(device.api))
            .cloned()
    };
    if let Some(device) = find(&wanted) {
        return Some(device);
    }
    if lower.contains("cable input") {
        let is_cable_capture = |name: &str| {
            let name = name.to_lowercase();
            name.contains("cable output")
                || (name.contains("vb-audio virtual cable") && name.contains("microphone"))
        };
        return inputs
            .iter()
            .filter(|device| device.inputs > 0 && is_cable_capture(&device.name))
            .min_by_key(|device| rank(device.api))
            .cloned();
    }
    None
}

fn default_device(devices: &[DeviceInfo]) -> Option<&DeviceInfo> {
    devices
        .iter()
        .find(|device| device.is_default)
        .or_else(|| {
            devices
                .iter()
                .find(|device| device.api == AudioApi::WasapiShared)
        })
        .or_else(|| devices.first())
}

fn sidechain_label(source: SidechainSource) -> String {
    match source {
        SidechainSource::Off => "SC: off".to_string(),
        SidechainSource::MainMic => "SC: main mic".to_string(),
        SidechainSource::InputChannel(channel) => format!("SC: input {}", channel + 1),
        SidechainSource::SecondInput(channel) => format!("SC: aux {}", channel + 1),
        SidechainSource::Silence => "SC: silence".to_string(),
    }
}

fn strip_builtins(project: &mut Project, allow: bool) -> Vec<String> {
    if allow {
        Vec::new()
    } else {
        project.remove_builtin_entries()
    }
}

fn load_startup_project() -> (Project, Option<PathBuf>) {
    if let Some(path) = project_state::read() {
        if path.is_file() {
            match Project::load(&path) {
                Ok(mut project) => {
                    for name in strip_builtins(&mut project, cfg!(debug_assertions)) {
                        logging::line(format!("release build: removed built-in effect '{name}'"));
                    }
                    logging::line(format!("loaded project {}", path.display()));
                    return (project, Some(path));
                }
                Err(error) => {
                    logging::line(format!("loading last project {}: {error}", path.display()));
                    project_state::clear();
                }
            }
        } else {
            project_state::clear();
        }
    }
    let mut project = match Project::load_default() {
        Ok(Some(project)) => project,
        Ok(None) => Project::new(),
        Err(error) => {
            logging::line(format!("loading the default project: {error}"));
            Project::new()
        }
    };
    for name in strip_builtins(&mut project, cfg!(debug_assertions)) {
        logging::line(format!("release build: removed built-in effect '{name}'"));
    }
    (project, None)
}

fn header_stat(ui: &mut egui::Ui, label: &str, value: &str, hover: &str) {
    ui.label(egui::RichText::new(label).weak().small())
        .on_hover_text(hover);
    ui.label(egui::RichText::new(value).monospace())
        .on_hover_text(hover);
}

fn lufs_bar(ui: &mut egui::Ui, label: &str, lufs: f32, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [28.0, 14.0],
            egui::Label::new(egui::RichText::new(label).weak().small()),
        );
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(120.0, 10.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, egui::Color32::from_rgb(44, 48, 56));
        let normalized = (lufs.clamp(-60.0, 0.0) + 60.0) / 60.0;
        if normalized > 0.0 {
            let fill = egui::Rect::from_min_size(
                rect.min,
                egui::vec2(rect.width() * normalized, rect.height()),
            );
            painter.rect_filled(fill, 3.0, color);
        }
        let text = if lufs <= -119.0 {
            "—".to_string()
        } else {
            format!("{:>5.1} LUFS", lufs)
        };
        ui.add_sized(
            [74.0, 14.0],
            egui::Label::new(egui::RichText::new(text).monospace().small()),
        );
        response.on_hover_text(
            "Momentary K-weighted loudness (400 ms, BS.1770). IN is before the chain; OUT is after it.",
        );
    });
}

fn short_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut result: String = text.chars().take(max.saturating_sub(1)).collect();
        result.push('…');
        result
    }
}

fn channel_label(count: u32) -> String {
    match count {
        1 => "1 (mono)".to_string(),
        2 => "2 (stereo)".to_string(),
        other => format!("{other} channels"),
    }
}

fn mono_label(source: MonoSource) -> &'static str {
    match source {
        MonoSource::Left => "Left",
        MonoSource::Right => "Right",
    }
}

fn automatic_api_label(api: AudioApi) -> &'static str {
    match api {
        AudioApi::WasapiLowLatency => "Automatic: WASAPI low-latency",
        AudioApi::WasapiShared => "Automatic: WASAPI shared",
        AudioApi::WasapiExclusive => "Automatic: WASAPI exclusive (kept)",
        AudioApi::Asio => "Automatic: ASIO (kept)",
        AudioApi::Other => "Automatic",
    }
}

fn device_channels(device: &DeviceInfo, input: bool) -> u32 {
    if input { device.inputs } else { device.outputs }
}

fn automatic_api(devices: &[DeviceInfo], name: &str, current: AudioApi, input: bool) -> AudioApi {
    if name.is_empty() {
        if devices.iter().any(|device| {
            device.api == AudioApi::WasapiLowLatency && device_channels(device, input) > 0
        }) {
            return AudioApi::WasapiLowLatency;
        }
        if devices.iter().any(|device| {
            device.api == AudioApi::WasapiShared && device_channels(device, input) > 0
        }) {
            return AudioApi::WasapiShared;
        }
        return current;
    }
    if devices.iter().any(|device| {
        device.api == AudioApi::WasapiLowLatency
            && device.name == name
            && device_channels(device, input) > 0
    }) {
        return AudioApi::WasapiLowLatency;
    }
    if devices.iter().any(|device| {
        device.api == AudioApi::WasapiShared
            && device.name == name
            && device_channels(device, input) > 0
    }) {
        return AudioApi::WasapiShared;
    }
    current
}

fn set_selection_api(
    devices: &[DeviceInfo],
    selection: &mut DeviceSelection,
    api: AudioApi,
    input: bool,
) -> bool {
    if selection.api == api {
        return false;
    }
    selection.api = api;
    if let Some(device) = devices.iter().find(|device| {
        device.api == api && device.name == selection.name && device_channels(device, input) > 0
    }) {
        selection.id = device.id.clone();
    }
    true
}

fn unique_device_names(devices: &[DeviceInfo], input: bool) -> Vec<String> {
    let mut names = Vec::new();
    for device in devices {
        if device_channels(device, input) > 0 && !names.contains(&device.name) {
            names.push(device.name.clone());
        }
    }
    names
}

fn selected_device<'a>(
    devices: &'a [DeviceInfo],
    name: &str,
    current: AudioApi,
    advanced: bool,
    input: bool,
) -> Option<&'a DeviceInfo> {
    let matches = |device: &DeviceInfo| device.name == name && device_channels(device, input) > 0;
    if advanced {
        return devices
            .iter()
            .find(|device| matches(device) && device.api == current);
    }
    if !matches!(current, AudioApi::WasapiLowLatency | AudioApi::WasapiShared)
        && let Some(device) = devices
            .iter()
            .find(|device| matches(device) && device.api == current)
    {
        return Some(device);
    }
    for api in [AudioApi::WasapiLowLatency, AudioApi::WasapiShared] {
        if let Some(device) = devices
            .iter()
            .find(|device| matches(device) && device.api == api)
        {
            return Some(device);
        }
    }
    devices
        .iter()
        .find(|device| matches(device) && device.api == current)
        .or_else(|| devices.iter().find(|device| matches(device)))
}

fn project_dialog() -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new();
    match Project::default_path().and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
    {
        Some(directory) => dialog.set_directory(directory),
        None => dialog,
    }
}

fn builtin_spec(kind: BuiltinKind) -> PluginSpec {
    PluginSpec {
        path: kind.path().to_string(),
        class_id: kind.path().to_string(),
        name: kind.name().to_string(),
        vendor: kind.vendor().to_string(),
        main_inputs: 2,
        main_outputs: 2,
        sidechain_inputs: 0,
    }
}

fn builtin_descriptor(kind: BuiltinKind) -> PluginDescriptor {
    PluginDescriptor {
        path: kind.path().to_string(),
        class_id: kind.path().to_string(),
        name: kind.name().to_string(),
        vendor: kind.vendor().to_string(),
        category: format!("Built-in / {}", kind.category()),
        main_inputs: 2,
        main_outputs: 2,
        sidechain_inputs: 0,
        flags: 0,
    }
}

fn build_picker_cache(
    plugins: &[PluginDescriptor],
    generation: u64,
    search: String,
    grouping: PickerGrouping,
) -> PickerCache {
    let mut groups: BTreeMap<String, Vec<(usize, u32)>> = BTreeMap::new();
    for (index, descriptor) in plugins.iter().enumerate() {
        let score = if search.is_empty() {
            0
        } else {
            match descriptor_score(&search, descriptor) {
                Some(score) => score,
                None => continue,
            }
        };
        let key = match grouping {
            PickerGrouping::Vendor => {
                if descriptor.vendor.is_empty() {
                    "Unknown vendor".to_string()
                } else {
                    descriptor.vendor.clone()
                }
            }
            PickerGrouping::Category => {
                let category = display_category(&descriptor.category);
                if category.is_empty() {
                    "Uncategorised".to_string()
                } else {
                    category
                }
            }
        };
        groups.entry(key).or_default().push((index, score));
    }
    for entries in groups.values_mut() {
        if !search.is_empty() {
            entries.sort_by(|left, right| {
                right
                    .1
                    .cmp(&left.1)
                    .then_with(|| plugins[left.0].name.cmp(&plugins[right.0].name))
            });
        } else if grouping == PickerGrouping::Category {
            entries.sort_by(|left, right| {
                plugins[left.0]
                    .name
                    .to_lowercase()
                    .cmp(&plugins[right.0].name.to_lowercase())
            });
        }
    }
    let shown = groups.values().map(Vec::len).sum();
    PickerCache {
        generation,
        search,
        grouping,
        groups,
        shown,
    }
}

fn display_category(category: &str) -> String {
    category
        .split('|')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

fn normalize_search(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect()
}

fn fuzzy_score(query: &str, text: &str) -> Option<u32> {
    let query = normalize_search(query);
    if query.is_empty() {
        return Some(0);
    }
    let target = normalize_search(text);
    if target.is_empty() {
        return None;
    }
    if let Some(position) = target.find(&query) {
        let prefix_bonus = if position == 0 { 200 } else { 0 };
        return Some(1200 + prefix_bonus - position.min(500) as u32);
    }
    let query_chars: Vec<char> = query.chars().collect();
    let mut matched = 0usize;
    let mut gaps = 0u32;
    let mut previous = None;
    for (position, character) in target.chars().enumerate() {
        if matched < query_chars.len() && character == query_chars[matched] {
            if let Some(previous_position) = previous {
                gaps = gaps.saturating_add((position - previous_position - 1) as u32);
            }
            previous = Some(position);
            matched += 1;
        }
    }
    if matched != query_chars.len() {
        return None;
    }
    Some(600u32.saturating_sub(gaps.min(599)))
}

fn descriptor_score(query: &str, descriptor: &PluginDescriptor) -> Option<u32> {
    let name = fuzzy_score(query, &descriptor.name).map(|score| score + 50);
    let vendor = fuzzy_score(query, &descriptor.vendor);
    let category = fuzzy_score(query, &descriptor.category);
    [name, vendor, category].into_iter().flatten().max()
}

fn move_chain_entry(chain: &mut Vec<ChainEntry>, from: usize, to: usize) -> bool {
    if from == to || from >= chain.len() || to >= chain.len() {
        return false;
    }
    let entry = chain.remove(from);
    chain.insert(to, entry);
    true
}

#[cfg(test)]
mod tests {
    use super::{
        PickerGrouping, build_picker_cache, builtin_descriptor, builtin_spec, descriptor_score,
        display_category, find_loopback_input, fuzzy_score, move_chain_entry, normalize_search,
        strip_builtins,
    };
    use michost_builtin::BuiltinKind;
    use michost_graph::{ChainEntry, PluginSpec};
    use michost_host_abi::{AudioApi, DeviceInfo, PluginDescriptor};
    use michost_project::Project;

    fn entry(id: u64) -> ChainEntry {
        ChainEntry {
            id,
            plugin: PluginSpec::new("C:/x.vst3", "class", format!("Plugin {id}"), "Vendor"),
            bypassed: false,
            sidechain_source: michost_graph::SidechainSource::Off,
            oversampling: 1,
        }
    }

    fn ids(chain: &[ChainEntry]) -> Vec<u64> {
        chain.iter().map(|entry| entry.id).collect()
    }

    fn input_device(name: &str, api: AudioApi, channels: u32) -> DeviceInfo {
        DeviceInfo {
            name: name.to_string(),
            api,
            inputs: channels,
            ..DeviceInfo::default()
        }
    }

    #[test]
    fn maps_cable_output_and_prefers_shared_capture() {
        let output = DeviceInfo {
            name: "CABLE Input (VB-Audio Virtual Cable)".to_string(),
            api: AudioApi::WasapiExclusive,
            outputs: 2,
            ..DeviceInfo::default()
        };
        let inputs = [
            input_device(
                "CABLE Output (VB-Audio Virtual Cable)",
                AudioApi::WasapiShared,
                2,
            ),
            input_device(
                "CABLE Output (VB-Audio Virtual Cable)",
                AudioApi::WasapiExclusive,
                2,
            ),
            input_device(
                "CABLE Output (VB-Audio Virtual Cable)",
                AudioApi::WasapiLowLatency,
                2,
            ),
        ];
        let found = find_loopback_input(&inputs, &output).unwrap();
        assert_eq!(found.api, AudioApi::WasapiShared);
    }

    #[test]
    fn maps_vb_cable_microphone_capture_endpoint() {
        let output = DeviceInfo {
            name: "CABLE Input (VB-Audio Virtual Cable)".to_string(),
            api: AudioApi::WasapiExclusive,
            outputs: 2,
            ..DeviceInfo::default()
        };
        let inputs = [input_device(
            "Microphone (VB-Audio Virtual Cable)",
            AudioApi::WasapiShared,
            2,
        )];
        let found = find_loopback_input(&inputs, &output).unwrap();
        assert_eq!(found.name, "Microphone (VB-Audio Virtual Cable)");
    }

    #[test]
    fn maps_named_virtual_output() {
        let output = DeviceInfo {
            name: "Voicemeeter Input".to_string(),
            api: AudioApi::WasapiShared,
            outputs: 2,
            ..DeviceInfo::default()
        };
        let inputs = [input_device(
            "Voicemeeter Output",
            AudioApi::WasapiShared,
            2,
        )];
        assert!(find_loopback_input(&inputs, &output).is_some());
    }

    #[test]
    fn rejects_physical_output_without_loopback() {
        let output = DeviceInfo {
            name: "Main (MOTU M Series)".to_string(),
            api: AudioApi::WasapiShared,
            outputs: 2,
            ..DeviceInfo::default()
        };
        let inputs = [input_device(
            "Do Not Use (MOTU M Series)",
            AudioApi::Asio,
            6,
        )];
        assert!(find_loopback_input(&inputs, &output).is_none());
    }

    #[test]
    fn moves_chain_entries_both_directions() {
        let mut chain = vec![entry(1), entry(2), entry(3)];
        assert!(move_chain_entry(&mut chain, 0, 2));
        assert_eq!(ids(&chain), [2, 3, 1]);
        assert!(move_chain_entry(&mut chain, 2, 0));
        assert_eq!(ids(&chain), [1, 2, 3]);
    }

    #[test]
    fn rejects_out_of_range_moves() {
        let mut chain = vec![entry(1), entry(2)];
        assert!(!move_chain_entry(&mut chain, 0, 0));
        assert!(!move_chain_entry(&mut chain, 2, 0));
        assert!(!move_chain_entry(&mut chain, 0, 2));
        assert_eq!(ids(&chain), [1, 2]);
    }

    #[test]
    fn fuzzy_search_matches_compacted_queries() {
        let name = "FabFilter Pro-Q 4";
        for query in ["proq", "q4", "pro q4", "fab", "FabFilter"] {
            assert!(fuzzy_score(query, name).is_some(), "query {query}");
        }
        assert!(fuzzy_score("zzzz", name).is_none());
        assert_eq!(normalize_search("Pro-Q 4"), "proq4");
    }

    #[test]
    fn fuzzy_search_prefers_prefix_and_substring() {
        let substring = fuzzy_score("proq", "FabFilter Pro-Q 4").unwrap();
        let subsequence = fuzzy_score("ffpq", "FabFilter Pro-Q 4").unwrap();
        assert!(substring > subsequence);
        assert!(fuzzy_score("fab", "FabFilter Pro-Q 4").unwrap() > substring);
    }

    #[test]
    fn plugin_search_matches_category_and_formats_it() {
        let descriptor = PluginDescriptor {
            name: "Pro-Q 4".to_owned(),
            vendor: "FabFilter".to_owned(),
            category: "Fx|EQ".to_owned(),
            ..PluginDescriptor::default()
        };
        assert!(descriptor_score("eq", &descriptor).is_some());
        assert!(descriptor_score("fxeq", &descriptor).is_some());
        assert!(descriptor_score("reverb", &descriptor).is_none());
        assert_eq!(display_category("Fx|EQ"), "Fx / EQ");
        assert_eq!(
            display_category(" Instrument | Synth "),
            "Instrument / Synth"
        );
        assert_eq!(display_category(""), "");
    }

    #[test]
    fn release_mode_strips_builtin_entries() {
        let mut project = Project::new();
        project.chain.push(ChainEntry {
            id: 1,
            plugin: builtin_spec(BuiltinKind::Eq),
            bypassed: false,
            sidechain_source: michost_graph::SidechainSource::Off,
            oversampling: 1,
        });

        let removed = strip_builtins(&mut project, false);
        assert_eq!(removed, vec!["EQ".to_string()]);
        assert!(project.chain.is_empty());
        assert!(strip_builtins(&mut project, true).is_empty());
    }

    #[test]
    fn builtins_are_identified_by_path_and_advertise_stereo() {
        for kind in BuiltinKind::ALL {
            let spec = builtin_spec(kind);
            assert_eq!(BuiltinKind::from_path(&spec.path), Some(kind));
            let descriptor = builtin_descriptor(kind);
            assert_eq!(descriptor.main_inputs, 2);
            assert_eq!(descriptor.main_outputs, 2);
            assert_eq!(BuiltinKind::from_path(&descriptor.path), Some(kind));
        }
    }

    #[test]
    fn picker_groups_by_vendor_or_category() {
        let descriptor = |name: &str, vendor: &str, category: &str| PluginDescriptor {
            name: name.to_owned(),
            vendor: vendor.to_owned(),
            category: category.to_owned(),
            ..PluginDescriptor::default()
        };
        let plugins = [
            descriptor("Zeta", "Vendor A", "Fx|EQ"),
            descriptor("Alpha", "Vendor B", "Fx|Dynamics"),
            descriptor("Beta", "Vendor A", "Fx|EQ"),
        ];

        let vendor = build_picker_cache(&plugins, 0, String::new(), PickerGrouping::Vendor);
        assert_eq!(
            vendor.groups.keys().cloned().collect::<Vec<_>>(),
            ["Vendor A", "Vendor B"]
        );
        assert_eq!(vendor.groups["Vendor A"].len(), 2);

        let category = build_picker_cache(&plugins, 0, String::new(), PickerGrouping::Category);
        assert_eq!(
            category.groups.keys().cloned().collect::<Vec<_>>(),
            ["Fx / Dynamics", "Fx / EQ"]
        );
        let eq = &category.groups["Fx / EQ"];
        assert_eq!(
            eq.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            [2, 0]
        );
    }
}
