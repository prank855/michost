#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![cfg_attr(not(debug_assertions), allow(dead_code, unused_mut))]

mod activation;
mod app;
mod audio_session;
mod autostart;
mod bridge;
mod builtin_ui;
mod device_cache;
mod latency;
mod logging;
mod oversampling;
mod plugin_cache;
mod project_state;
#[cfg(debug_assertions)]
mod selftest;
mod tray;

use std::ffi::c_void;
use std::sync::Arc;

use eframe::egui;
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;

use app::MicHostApp;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("MicHost - microphone VST3 chain host");
        println!("  --show       show the settings window on launch (default)");
        println!("  --hidden     start in the tray without showing the window");
        println!("  --autostart  start hidden (used by launch at login)");
        #[cfg(debug_assertions)]
        {
            println!("  --selftest          load mic_host.dll and enumerate devices");
            println!("  --selftest-builtins  process every native built-in effect and state blob");
            println!("  --selftest-streams  run a 2 s mic-to-virtual-cable loopback");
            println!(
                "  --selftest-safety <ms>  stream selftest with an explicit output safety buffer"
            );
            println!(
                "  --selftest-plugin <path.vst3>  load, prepare, process and restore one plugin"
            );
            println!(
                "  --selftest-chain <path.vst3>   run the plugin in the live chain with state restore"
            );
            println!(
                "  --selftest-second-input [path.vst3]  run the primary input with a second device as sidechain"
            );
            println!(
                "  --selftest-latency [seconds] [--output NAME] [--input NAME] [--api wasapi|wasapi-low|wasapi-exclusive|asio] [--block N (0 = default)]"
            );
            println!(
                "                     measure callback-to-callback loopback latency through a virtual cable"
            );
            println!(
                "  --selftest-virtual-mic-driver  open the virtual mic driver and validate its ring"
            );
            println!(
                "  --selftest-virtual-mic [seconds]  write a marker into the virtual mic and measure the capture path"
            );
            println!(
                "  --screenshot <path> [--show-folders] [--show-monitor] [--measure]  render the UI, save a PNG and exit (in-app UI test)"
            );
        }
        return;
    }
    #[cfg(not(debug_assertions))]
    if args
        .iter()
        .any(|arg| arg.starts_with("--selftest") || arg == "--screenshot")
    {
        eprintln!("MicHost: testing options are only available in debug builds");
        std::process::exit(2);
    }

    #[cfg(debug_assertions)]
    {
        if args.iter().any(|arg| arg == "--selftest") {
            std::process::exit(selftest::run());
        }
        if args.iter().any(|arg| arg == "--selftest-builtins") {
            std::process::exit(selftest::builtins());
        }
        if args.iter().any(|arg| arg == "--selftest-streams") {
            std::process::exit(selftest::streams(2, 0));
        }
        if let Some(index) = args.iter().position(|arg| arg == "--selftest-safety") {
            let safety = args
                .get(index + 1)
                .and_then(|value| value.parse().ok())
                .unwrap_or(5);
            std::process::exit(selftest::streams(2, safety));
        }
        if args
            .iter()
            .any(|arg| arg == "--selftest-virtual-mic-driver")
        {
            std::process::exit(selftest::virtual_mic_driver());
        }
        if let Some(index) = args.iter().position(|arg| arg == "--selftest-virtual-mic") {
            let seconds = args
                .get(index + 1)
                .and_then(|value| value.parse().ok())
                .unwrap_or(2);
            std::process::exit(selftest::virtual_mic(seconds));
        }
        if args.iter().any(|arg| arg == "--selftest-asio") {
            std::process::exit(selftest::cross_api());
        }
        if let Some(index) = args.iter().position(|arg| arg == "--selftest-latency") {
            std::process::exit(latency::run(parse_latency_options(&args[index + 1..])));
        }
        if let Some(index) = args.iter().position(|arg| arg == "--selftest-monitor") {
            let positional: Vec<&String> = args[index + 1..]
                .iter()
                .filter(|value| !value.starts_with("--"))
                .collect();
            let device = positional.first().map(|value| value.as_str());
            let input = positional.get(1).map(|value| value.as_str());
            std::process::exit(selftest::monitor(2, device, input));
        }
        if args.iter().any(|arg| arg == "--selftest-scan") {
            std::process::exit(selftest::scan());
        }
        if args.iter().any(|arg| arg == "--selftest-scan-isolated") {
            std::process::exit(selftest::isolated_scan());
        }
        if let Some(index) = args.iter().position(|arg| arg == "--selftest-second-input") {
            let path = args
                .get(index + 1)
                .filter(|value| !value.starts_with("--"))
                .cloned();
            std::process::exit(selftest::second_input(path.as_deref()));
        }
        if let Some(index) = args.iter().position(|arg| arg == "--selftest-editor") {
            let path = args
                .get(index + 1)
                .filter(|value| !value.starts_with("--"))
                .cloned()
                .unwrap_or_default();
            std::process::exit(selftest::editor(&path));
        }
        if let Some(index) = args.iter().position(|arg| arg == "--selftest-chain") {
            let path = args
                .get(index + 1)
                .cloned()
                .unwrap_or_else(|| "C:/Program Files/Common Files/VST3".to_string());
            std::process::exit(selftest::chain(&path));
        }
        if let Some(index) = args.iter().position(|arg| arg == "--selftest-plugin") {
            let path = args
                .get(index + 1)
                .cloned()
                .unwrap_or_else(|| "C:/Program Files/Common Files/VST3".to_string());
            std::process::exit(selftest::plugin(&path));
        }
    }

    logging::line("MicHost starting");
    let _single_instance = match single_instance() {
        Ok(Some(mutex)) => Some(mutex),
        Ok(None) => {
            logging::line("MicHost is already running; asking it to show");
            if let Err(error) = activation::signal_existing() {
                logging::line(error);
            }
            return;
        }
        Err(error) => {
            logging::line(error);
            None
        }
    };

    #[cfg(debug_assertions)]
    let screenshot = args
        .iter()
        .position(|arg| arg == "--screenshot")
        .and_then(|index| args.get(index + 1))
        .filter(|value| !value.starts_with("--"))
        .map(std::path::PathBuf::from);
    #[cfg(not(debug_assertions))]
    let screenshot: Option<std::path::PathBuf> = None;
    #[cfg(debug_assertions)]
    let show_folders = args.iter().any(|arg| arg == "--show-folders");
    #[cfg(not(debug_assertions))]
    let show_folders = false;
    #[cfg(debug_assertions)]
    let show_monitor = args.iter().any(|arg| arg == "--show-monitor");
    #[cfg(not(debug_assertions))]
    let show_monitor = false;
    #[cfg(debug_assertions)]
    let auto_measure = args.iter().any(|arg| arg == "--measure");
    #[cfg(not(debug_assertions))]
    let auto_measure = false;
    let show = screenshot.is_some()
        || !args
            .iter()
            .any(|arg| arg == "--hidden" || arg == "--autostart");
    if let Err(error) = run(show, screenshot, show_folders, show_monitor, auto_measure) {
        logging::line(format!("MicHost failed: {error}"));
    }
}

#[cfg(debug_assertions)]
fn parse_latency_options(args: &[String]) -> latency::Options {
    let mut options = latency::Options::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--output" => {
                options.output = args.get(index + 1).cloned();
                index += 2;
            }
            "--input" => {
                options.input = args.get(index + 1).cloned();
                index += 2;
            }
            "--api" => {
                if let Some(api) = args
                    .get(index + 1)
                    .and_then(|value| latency::parse_api(value))
                {
                    options.api = api;
                }
                index += 2;
            }
            "--seconds" => {
                if let Some(seconds) = args
                    .get(index + 1)
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    options.seconds = seconds;
                }
                index += 2;
            }
            "--block" => {
                if let Some(block) = args
                    .get(index + 1)
                    .and_then(|value| value.parse::<u32>().ok())
                {
                    options.block_size = block;
                }
                index += 2;
            }
            value if !value.starts_with("--") => {
                if let Ok(seconds) = value.parse::<u64>() {
                    options.seconds = seconds;
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    options
}

fn run(
    show: bool,
    screenshot: Option<std::path::PathBuf>,
    show_folders: bool,
    show_monitor: bool,
    auto_measure: bool,
) -> Result<(), String> {
    let size = [900.0, 660.0];
    let options = eframe::NativeOptions {
        centered: true,
        persist_window: false,
        viewport: egui::ViewportBuilder::default()
            .with_title("MicHost")
            .with_inner_size(size)
            .with_min_inner_size(size)
            .with_max_inner_size(size)
            .with_resizable(false)
            .with_maximize_button(false)
            .with_taskbar(true)
            .with_visible(show)
            .with_icon(Arc::new(tray::window_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "MicHost",
        options,
        Box::new(move |cc| {
            Ok(Box::new(MicHostApp::new(
                cc,
                show,
                screenshot,
                show_folders,
                show_monitor,
                auto_measure,
            )))
        }),
    )
    .map_err(|error| format!("running the settings window: {error}"))
}

fn single_instance() -> Result<Option<*mut c_void>, String> {
    let name: Vec<u16> = "Local\\MicHost.SingleInstance\0".encode_utf16().collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err("creating the single instance mutex failed".to_string());
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_ALREADY_EXISTS {
        Ok(None)
    } else {
        Ok(Some(handle))
    }
}
