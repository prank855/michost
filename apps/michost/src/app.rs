//! Application core: state, construction and module wiring.
//!
//! Behavior lives in focused modules so each concern can be understood and
//! tested without an audio device. `MicHostApp` is a thin coordinator; views,
//! device selection, retry policy, session lifecycle and project persistence
//! each own their responsibility in a submodule.

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
    AudioApi, DeviceInfo, DeviceKind, MH_DEVICE_MISSING, MH_ERR, MH_NOT_FOUND, MH_NOT_SUPPORTED,
    PluginDescriptor, StreamHost,
};
use michost_host_juce::JuceBackend;
use michost_project::{DeviceSelection, OutputKind, PROJECT_EXTENSION, Project};

use crate::activation::{Activation, HiddenWake};
use crate::audio_session::{
    AudioSession, PreparedPlugin, discard_prepared, prepare_plugin_for_insert,
    prepare_plugin_for_insert_with_oversampling,
};
use crate::autostart;
use crate::bridge::{self, MonitorError};
use crate::builtin_ui::{self, BuiltinEditor};
use crate::device_cache::{self, DeviceCache};
use crate::latency;
use crate::logging;
use crate::plugin_cache::{self, PluginCache, ScanUpdate};
use crate::project_state;
use crate::tray::{Tray, TrayCommand};

mod backend;
mod device_selection;
mod devices;
mod eframe_impl;
mod measure;
mod monitor;
mod plugins;
mod project;
mod session;
mod ui;
mod window;

pub(crate) use self::device_selection::*;

mod device_draft;
mod monitor_recovery;
mod plugin_picker;
mod startup;
mod ui_widgets;

pub(crate) use self::device_draft::*;
pub(crate) use self::monitor_recovery::*;
pub(crate) use self::plugin_picker::*;
pub(crate) use self::startup::*;
pub(crate) use self::ui_widgets::*;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngineState {
    Stopped,
    Starting,
    Stopping,
    Running,
    Error,
}

impl EngineState {
    fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Starting => "Starting",
            Self::Stopping => "Stopping",
            Self::Running => "Running",
            Self::Error => "Error",
        }
    }

    fn color(self) -> egui::Color32 {
        match self {
            Self::Stopped => egui::Color32::from_rgb(150, 155, 165),
            Self::Starting => egui::Color32::from_rgb(230, 180, 80),
            Self::Stopping => egui::Color32::from_rgb(230, 180, 80),
            Self::Running => egui::Color32::from_rgb(110, 210, 140),
            Self::Error => egui::Color32::from_rgb(235, 105, 105),
        }
    }
}

pub(crate) struct StopOutcome {
    project: Project,
    warnings: Vec<String>,
    measurement: Option<Result<latency::Measurement, String>>,
}

#[derive(Clone)]
pub(crate) struct MeasureRequest {
    output: DeviceInfo,
    input: DeviceInfo,
    output_channels: u32,
    output_mask: u64,
    input_channels: u32,
    input_mask: u64,
    output_block: u32,
    input_block: u32,
}

#[derive(Default)]
struct ScanState {
    receiver: Option<Receiver<ScanUpdate>>,
    active: bool,
    done: usize,
    total: usize,
    error: Option<String>,
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
type DeviceProbe = Receiver<Result<Vec<DeviceInfo>, String>>;
type MonitorOpen = Receiver<Result<bridge::PreparedMonitor, MonitorError>>;

const START_RETRY_DELAY: Duration = Duration::from_secs(3);
const QUIT_STOP_WAIT: Duration = Duration::from_secs(5);
const ADD_PLUGIN_BUTTON_HEIGHT: f32 = 28.0;
const POINTER_REPAINT_INTERVAL: Duration = Duration::from_millis(40);

struct PendingAdd {
    name: String,
    spec: PluginSpec,
    session_epoch: u64,
    receiver: Receiver<Result<PreparedPlugin, String>>,
    insert_after: Option<u64>,
    template: Option<ChainEntry>,
}

pub(crate) enum ChainAction {
    Move(usize, usize),
    Remove(usize),
    Bypass(usize, bool),
    Oversampling(usize, u8),
    Sidechain(usize, SidechainSource),
    OpenEditor(usize),
    Duplicate(usize),
    Add(PluginDescriptor),
}

pub struct MicHostApp {
    backend: Option<Arc<JuceBackend>>,
    backend_error: Option<String>,
    project: Project,
    project_path: Option<PathBuf>,
    session: Option<AudioSession>,
    session_worker: Option<Receiver<SessionStartResult>>,
    stop_worker: Option<Receiver<StopOutcome>>,
    stop_restart: bool,
    rescan_after_stop: bool,
    device_draft: DeviceDraft,
    cancel_start: bool,
    cancel_flag: Arc<AtomicBool>,
    state: EngineState,
    last_error: Option<String>,
    warnings: Vec<String>,
    startup_warnings: Vec<String>,
    devices: DeviceCache,
    device_worker: Option<Receiver<Result<DeviceCache, String>>>,
    device_error: Option<String>,
    device_generation: u64,
    device_probe: Option<(u64, DeviceProbe)>,
    monitor_probe_at: Option<Instant>,
    probed_outputs: Option<Vec<DeviceInfo>>,
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
    #[expect(dead_code, reason = "held to stop the hidden repaint thread on drop")]
    hidden_wake: Option<HiddenWake>,
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
    hidden_frames: u8,
    hidden_flag: Arc<AtomicBool>,
    monitor: bool,
    monitor_pending: Option<MonitorPending>,
    monitor_notice: Option<String>,
    monitor_open: Option<(DeviceSelection, MonitorOpen)>,
    show_monitor_settings: bool,
    auto_measure: bool,
    last_input_buffer: Option<(String, AudioApi, u32)>,
    last_output_buffer: Option<(String, AudioApi, u32)>,
    measuring: bool,
    plugin_edit_at: Instant,
    measure_worker: Option<mpsc::Receiver<Result<latency::Capture, String>>>,
    measured: Option<latency::Measurement>,
    asio_round_trip: Option<latency::Measurement>,
    latency_error: Option<String>,
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

        let (project, project_path, startup_warnings) = load_startup_project();
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
        let hidden_flag = Arc::new(AtomicBool::new(!show_on_start));
        let hidden_wake = match HiddenWake::new(hidden_flag.clone()) {
            Ok(wake) => Some(wake),
            Err(error) => {
                logging::line(format!("hidden repaint thread: {error}"));
                None
            }
        };

        let next_plugin_id = project.next_plugin_id();
        let monitor_enabled = project.settings.monitor_enabled;
        let device_draft = DeviceDraft::from_settings(&project.settings);
        let mut app = Self {
            backend,
            backend_error,
            project,
            project_path,
            session: None,
            session_worker: None,
            stop_worker: None,
            stop_restart: false,
            rescan_after_stop: false,
            device_draft,
            cancel_start: false,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            state: EngineState::Stopped,
            last_error: None,
            warnings: Vec::new(),
            startup_warnings: Vec::new(),
            devices,
            device_worker: None,
            device_error: None,
            device_generation: 0,
            device_probe: None,
            monitor_probe_at: None,
            probed_outputs: None,
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
            hidden_wake,
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
            hidden_frames: 0,
            hidden_flag,
            monitor: monitor_enabled,
            monitor_pending: None,
            monitor_notice: None,
            monitor_open: None,
            show_monitor_settings: show_monitor,
            auto_measure,
            last_input_buffer: None,
            last_output_buffer: None,
            measuring: false,
            plugin_edit_at: Instant::now(),
            measure_worker: None,
            measured: None,
            asio_round_trip: None,
            latency_error: None,
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
        // A cached device list can be arbitrarily old (an endpoint may have
        // been renamed or removed since it was written), so always refresh at
        // startup. No session exists yet, so this never stops audio.
        app.refresh_devices();
        if app.apply_automatic_apis() {
            app.mark_dirty();
        }
        app.device_draft = DeviceDraft::from_settings(&app.project.settings);
        if app.tray.is_none() && app.hidden {
            app.hidden = false;
            app.hidden_flag.store(false, Ordering::Release);
            cc.egui_ctx
                .send_viewport_cmd(ViewportCommand::Visible(true));
        }
        if !startup_warnings.is_empty() {
            // Stripping built-ins happened on load, not from a user edit. Show
            // the warning but never rewrite the user's project on startup. The
            // list is re-applied when a session start replaces `warnings`.
            app.warnings.extend(startup_warnings.iter().cloned());
            app.startup_warnings = startup_warnings;
            app.dirty = false;
            app.save_deadline = None;
        }
        logging::line(format!(
            "app constructed in {:.1} ms",
            app.created_at.elapsed().as_secs_f64() * 1000.0
        ));
        app
    }
}
