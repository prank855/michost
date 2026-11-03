use std::sync::Arc;
use std::time::{Duration, Instant};

use michost_builtin::{AudioAnalysis, BuiltinKind, BuiltinProcessor, ParamSet};
use michost_graph::PluginProcessor;
use michost_host_abi::{AudioApi, DeviceInfo, DeviceKind, PluginHost, StateKind, StreamHost};
use michost_host_juce::JuceBackend;
use michost_project::{DeviceSelection, Project};
use michost_vmic::{DEFAULT_FORMAT, VirtualMicSink, VmicClient, VmicError};

use crate::audio_session::{AudioSession, prepare_plugin_for_insert};
use crate::logging;

pub fn run() -> i32 {
    logging::line("selftest: loading the native host");
    let backend = match JuceBackend::load() {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            logging::line(format!("selftest failed: {error}"));
            println!("MicHost selftest: FAILED: {error}");
            return 1;
        }
    };

    match backend.abi_version() {
        Ok(version) => {
            println!("MicHost selftest: mic_host ABI {version}");
            println!(
                "MicHost selftest: device-change notifications {}",
                if backend.device_change_notifications_supported() {
                    "available"
                } else {
                    "unavailable"
                }
            );
            logging::line(format!("selftest: mic_host ABI {version}"));
        }
        Err(error) => {
            logging::line(format!("selftest failed: {error}"));
            println!("MicHost selftest: FAILED: {error}");
            return 1;
        }
    }

    for (kind, label) in [
        (DeviceKind::Input, "inputs"),
        (DeviceKind::Output, "outputs"),
    ] {
        match backend.enumerate_devices(kind) {
            Ok(devices) => {
                println!("MicHost selftest: {} {label}", devices.len());
                logging::line(format!("selftest: {} {label}", devices.len()));
                for device in devices {
                    println!(
                        "  [{}] {} ({} in, {} out, {} Hz)",
                        api_name(device.api),
                        device.name,
                        device.inputs,
                        device.outputs,
                        device.default_sample_rate as u32
                    );
                }
            }
            Err(error) => {
                logging::line(format!("selftest failed: {error}"));
                println!("MicHost selftest: FAILED: {error}");
                return 1;
            }
        }
    }

    println!("MicHost selftest: OK (no streams opened)");
    logging::line("selftest: OK");
    0
}

pub fn streams(seconds: u64, safety_ms: u32) -> i32 {
    let backend = match JuceBackend::load() {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            logging::line(format!("stream selftest failed: {error}"));
            println!("MicHost stream selftest: FAILED: {error}");
            return 1;
        }
    };
    let inputs = match backend.enumerate_devices(DeviceKind::Input) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost stream selftest: FAILED: {error}");
            return 1;
        }
    };
    let outputs = match backend.enumerate_devices(DeviceKind::Output) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost stream selftest: FAILED: {error}");
            return 1;
        }
    };
    let Some(input) = inputs
        .iter()
        .find(|device| device.api == AudioApi::WasapiShared && device.inputs > 0)
    else {
        println!("MicHost stream selftest: SKIP (no WASAPI input)");
        return 0;
    };
    let Some(output) = outputs.iter().find(|device| {
        device.api == AudioApi::WasapiShared
            && device.outputs > 0
            && device.name.to_lowercase().contains("cable input")
    }) else {
        println!(
            "MicHost stream selftest: SKIP (no virtual cable output; avoids audible monitoring)"
        );
        return 0;
    };

    println!(
        "MicHost stream selftest: {} -> {} for {seconds}s",
        input.name, output.name
    );
    let mut project = Project::new();
    project.settings.input = selection(input);
    project.settings.output = selection(output);
    project.settings.sample_rate = 0.0;
    project.settings.buffer_size = 0;
    project.settings.safety_buffer_ms = safety_ms;

    let mut session = match AudioSession::start(backend, &mut project) {
        Ok(session) => session,
        Err(error) => {
            logging::line(format!("stream selftest failed: {error}"));
            println!("MicHost stream selftest: FAILED: {error}");
            return 1;
        }
    };
    std::thread::sleep(Duration::from_secs(seconds.max(1)));
    let frames = session.frames_processed();
    let in_xruns = session.input_dropouts();
    let out_xruns = session.output_underruns();
    let latency = session.end_to_end_milliseconds();
    println!(
        "MicHost stream selftest: {frames} frames, safety {safety_ms} ms, latency {latency:.1} ms, xruns in {in_xruns} / out {out_xruns}"
    );
    session.stop(Some(&mut project));
    if frames == 0 {
        println!("MicHost stream selftest: FAILED (no frames processed)");
        return 1;
    }
    println!("MicHost stream selftest: OK");
    0
}

pub fn virtual_mic_driver() -> i32 {
    let client = match VmicClient::open() {
        Ok(client) => client,
        Err(error) if virtual_mic_skip(&error) => {
            println!("MicHost virtual mic driver selftest: SKIP: {error}");
            return 0;
        }
        Err(error) => {
            println!("MicHost virtual mic driver selftest: FAILED: {error}");
            return 1;
        }
    };
    let version = match client.driver_version() {
        Ok(version) => version,
        Err(error) if virtual_mic_skip(&error) => {
            println!("MicHost virtual mic driver selftest: SKIP: {error}");
            return 0;
        }
        Err(error) => {
            println!("MicHost virtual mic driver selftest: FAILED: driver version: {error}");
            return 1;
        }
    };
    println!("MicHost virtual mic driver selftest: driver ABI version {version}");

    let info = match client.open_writer(DEFAULT_FORMAT) {
        Ok(info) => info,
        Err(error) if virtual_mic_skip(&error) => {
            println!("MicHost virtual mic driver selftest: SKIP: {error}");
            return 0;
        }
        Err(error) => {
            println!("MicHost virtual mic driver selftest: FAILED: open writer: {error}");
            return 1;
        }
    };
    println!(
        "MicHost virtual mic driver selftest: writer {} Hz, {} ch, packet {} frames, ring {} frames, section '{}'",
        info.format.sample_rate,
        info.format.channels,
        info.format.packet_frames,
        info.ring_frames,
        info.section_name
    );
    let view = match client.map_ring(&info) {
        Ok(view) => view,
        Err(error) => {
            let _ = client.close_writer();
            println!("MicHost virtual mic driver selftest: FAILED: map ring: {error}");
            return 1;
        }
    };
    if let Err(error) = view.validate() {
        let _ = client.close_writer();
        println!("MicHost virtual mic driver selftest: FAILED: ring layout: {error}");
        return 1;
    }
    let format = view.format();
    println!(
        "MicHost virtual mic driver selftest: ring validated: {} bytes, {} Hz, {} ch, {} frame packets, {} frames",
        view.len(),
        format.sample_rate,
        format.channels,
        format.packet_frames,
        view.ring_frames()
    );
    if let Err(error) = client.close_writer() {
        println!("MicHost virtual mic driver selftest: FAILED: close writer: {error}");
        return 1;
    }
    println!("MicHost virtual mic driver selftest: OK");
    0
}

pub fn virtual_mic(seconds: u64) -> i32 {
    let seconds = seconds.clamp(1, 60);
    let mut sink = match VirtualMicSink::open(DEFAULT_FORMAT) {
        Ok(sink) => sink,
        Err(error) if virtual_mic_skip(&error) => {
            println!("MicHost virtual mic selftest: SKIP: {error}");
            return 0;
        }
        Err(error) => {
            println!("MicHost virtual mic selftest: FAILED: open: {error}");
            return 1;
        }
    };
    let backend = match JuceBackend::load() {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            println!("MicHost virtual mic selftest: FAILED: {error}");
            return 1;
        }
    };
    let format = sink.format();
    let channels = format.channels.max(1) as usize;
    let packet_frames = format.packet_frames.max(1) as usize;
    let rate = format.sample_rate.max(8_000) as f64;
    let ring_frames = sink.ring_frames();
    let prefill = sink.prefill(packet_frames.saturating_mul(2));
    println!(
        "MicHost virtual mic selftest: sink {} Hz, {} ch, packet {} frames, ring {} frames, prefilled {} frames",
        format.sample_rate, format.channels, format.packet_frames, ring_frames, prefill.written
    );

    let read_index_start = sink.stats().read_index;
    let endpoint = match backend.enumerate_devices(DeviceKind::Input) {
        Ok(devices) => devices.into_iter().find(|device| {
            device.inputs > 0
                && device
                    .name
                    .to_lowercase()
                    .contains("michost virtual microphone")
        }),
        Err(_) => None,
    };
    let has_endpoint = endpoint.is_some();
    let capture = endpoint.map(|device| {
        let backend = backend.clone();
        std::thread::spawn(move || crate::latency::capture_input(&backend, &device, seconds))
    });

    const BURST_FRAMES: usize = 64;
    let period_frames = ((rate * 0.5).round() as u64).max(BURST_FRAMES as u64);
    let total_frames = (rate * seconds as f64).round() as u64;
    let mut block = vec![0.0f32; packet_frames * channels];
    let mut position = 0u64;
    let mut clicks = Vec::new();
    let mut push_overruns = 0u64;
    let started = Instant::now();
    while position < total_frames {
        block.fill(0.0);
        let remainder = position % period_frames;
        let next_burst = if remainder == 0 {
            position
        } else {
            position + (period_frames - remainder)
        };
        if next_burst < position + packet_frames as u64 {
            let offset = (next_burst - position) as usize;
            let count = BURST_FRAMES.min(packet_frames - offset);
            for index in 0..count {
                let value = 0.4 * (1.0 - index as f32 / BURST_FRAMES as f32);
                for channel in 0..channels {
                    block[(offset + index) * channels + channel] = value;
                }
            }
            clicks.push(started + Duration::from_secs_f64(next_burst as f64 / rate));
        }
        if sink.push(&block).overrun {
            push_overruns += 1;
        }
        position += packet_frames as u64;
        let target = started + Duration::from_secs_f64(position as f64 / rate);
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        }
    }
    let stats = sink.stats();
    let read_advance = stats.read_index.saturating_sub(read_index_start);
    println!(
        "MicHost virtual mic selftest: wrote {position} frames, read_index advanced {read_advance}, ring overruns {} / underruns {}, push overruns {push_overruns}",
        stats.overruns, stats.underruns
    );
    drop(sink);

    if has_endpoint {
        match capture.map(|handle| handle.join()) {
            Some(Ok(Ok(capture))) => match crate::latency::correlate(&capture, &clicks) {
                Ok(measured) => {
                    println!(
                        "MicHost virtual mic selftest: capture p50 {:.1} ms, p95 {:.1} ms ({} matched of {} clicks; capture {} Hz)",
                        measured.p50_ms,
                        measured.p95_ms,
                        measured.matched,
                        measured.clicks,
                        measured.input_rate
                    );
                    println!("MicHost virtual mic selftest: OK");
                    return 0;
                }
                Err(error) => {
                    println!(
                        "MicHost virtual mic selftest: FAILED: capture correlation: {error} (read_index advanced {read_advance})"
                    );
                    return 1;
                }
            },
            Some(Ok(Err(error))) => {
                println!("MicHost virtual mic selftest: FAILED: capture: {error}");
                return 1;
            }
            Some(Err(_)) => {
                println!("MicHost virtual mic selftest: FAILED: the capture thread panicked");
                return 1;
            }
            None => {}
        }
    }

    let markers_consumed = read_advance > prefill.written as u64;
    if markers_consumed && push_overruns == 0 && stats.overruns == 0 {
        println!(
            "MicHost virtual mic selftest: capture endpoint not enumerable; driver consumed {read_advance} frames from the ring ({markers_consumed} beyond prefill)"
        );
        println!("MicHost virtual mic selftest: OK (ring consumption only)");
        0
    } else if read_advance == 0 {
        println!(
            "MicHost virtual mic selftest: SKIP (capture endpoint not enumerable and the driver consumed no frames; open an app on 'MicHost Virtual Microphone')"
        );
        0
    } else {
        println!(
            "MicHost virtual mic selftest: FAILED (marker ring path: read {read_advance} frames, prefill {}, push overruns {push_overruns}, ring overruns {})",
            prefill.written, stats.overruns
        );
        1
    }
}

fn virtual_mic_skip(error: &VmicError) -> bool {
    matches!(
        error,
        VmicError::NotInstalled(_) | VmicError::Unsupported(_)
    )
}

pub fn monitor(seconds: u64, monitor_name: Option<&str>, input_name: Option<&str>) -> i32 {
    let backend = match JuceBackend::load() {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            println!("MicHost monitor selftest: FAILED: {error}");
            return 1;
        }
    };
    let inputs = match backend.enumerate_devices(DeviceKind::Input) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost monitor selftest: FAILED: {error}");
            return 1;
        }
    };
    let outputs = match backend.enumerate_devices(DeviceKind::Output) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost monitor selftest: FAILED: {error}");
            return 1;
        }
    };
    let input = match input_name {
        Some(name) => {
            let name = name.to_lowercase();
            inputs
                .iter()
                .find(|device| device.inputs > 0 && device.name.to_lowercase().contains(&name))
        }
        None => inputs
            .iter()
            .find(|device| device.api == AudioApi::WasapiShared && device.inputs > 0),
    };
    let Some(input) = input else {
        println!("MicHost monitor selftest: SKIP (no matching input)");
        return 0;
    };
    let Some(output) = outputs.iter().find(|device| {
        device.api == AudioApi::WasapiShared
            && device.outputs > 0
            && device.name.to_lowercase().contains("cable input")
    }) else {
        println!("MicHost monitor selftest: SKIP (no virtual cable output)");
        return 0;
    };
    let wanted = monitor_name.unwrap_or("cable input").to_lowercase();
    let Some(monitor) = outputs.iter().find(|device| {
        device.api == AudioApi::WasapiShared
            && device.outputs > 0
            && device.name.to_lowercase().contains(&wanted)
    }) else {
        println!("MicHost monitor selftest: SKIP (no monitor output matching '{wanted}')");
        return 0;
    };

    println!(
        "MicHost monitor selftest: {} -> {}, monitoring on '{}' for {seconds}s",
        input.name, output.name, monitor.name
    );
    let mut project = Project::new();
    project.settings.input = selection(input);
    project.settings.output = selection(output);
    project.settings.sample_rate = 0.0;
    project.settings.buffer_size = 0;
    let mut session = match AudioSession::start(backend, &mut project) {
        Ok(session) => session,
        Err(error) => {
            println!("MicHost monitor selftest: FAILED: {error}");
            return 1;
        }
    };
    let warm_started = std::time::Instant::now();
    if let Err(error) = session.warm_monitor(&monitor.name, monitor.api, monitor.outputs.min(2), 0)
    {
        println!("MicHost monitor selftest: FAILED warming monitor: {error}");
        session.stop(Some(&mut project));
        return 1;
    }
    let warm_ms = warm_started.elapsed().as_secs_f64() * 1000.0;
    let started = std::time::Instant::now();
    if let Err(error) = session.start_monitor(&monitor.name, monitor.api, monitor.outputs.min(2), 0)
    {
        println!("MicHost monitor selftest: FAILED starting monitor: {error}");
        session.stop(Some(&mut project));
        return 1;
    }
    let enable_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after_enable = session.frames_processed();
    std::thread::sleep(Duration::from_secs(seconds.max(1)));
    let frames = session.frames_processed();
    let main_xruns = session.output_underruns();
    let monitor_xruns = session.monitor_underruns();
    let during = frames.saturating_sub(after_enable);
    let expected = (session.sample_rate() * seconds.max(1) as f64).round() as u64;
    session.stop_monitor();
    std::thread::sleep(Duration::from_millis(300));
    let frames_after = session.frames_processed();
    session.stop(Some(&mut project));
    println!(
        "MicHost monitor selftest: monitor warm took {warm_ms:.1} ms, enable took {enable_ms:.2} ms; {during} frames during the run (expected ~{expected}), main xruns {main_xruns}, monitor underruns {monitor_xruns}, {frames_after} frames after monitor off"
    );
    if frames == 0 || frames_after <= frames {
        println!("MicHost monitor selftest: FAILED (the main output did not keep running)");
        return 1;
    }
    if expected > 0 && during.saturating_mul(10) < expected.saturating_mul(9) {
        println!("MicHost monitor selftest: WARNING (the graph stalled while enabling monitoring)");
    }
    println!("MicHost monitor selftest: OK");
    0
}

pub fn cross_api() -> i32 {
    let backend = match JuceBackend::load() {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            println!("MicHost cross-API selftest: FAILED: {error}");
            return 1;
        }
    };
    let inputs = match backend.enumerate_devices(DeviceKind::Input) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost cross-API selftest: FAILED: {error}");
            return 1;
        }
    };
    let outputs = match backend.enumerate_devices(DeviceKind::Output) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost cross-API selftest: FAILED: {error}");
            return 1;
        }
    };
    let cable_output = outputs.iter().find(|device| {
        device.api == AudioApi::WasapiShared
            && device.outputs > 0
            && device.name.to_lowercase().contains("cable input")
    });
    let cable_input = inputs.iter().find(|device| {
        device.api == AudioApi::WasapiShared
            && device.inputs > 0
            && device.name.to_lowercase().contains("cable output")
    });

    let mut ran = 0;
    if let Some(output) = cable_output {
        for input in inputs
            .iter()
            .filter(|device| device.api == AudioApi::Asio && device.inputs > 0)
        {
            if run_cross(
                backend.clone(),
                input,
                output,
                &format!("ASIO input '{}' -> WASAPI virtual output", input.name),
            ) == 0
            {
                ran += 1;
                break;
            }
        }
    } else {
        println!("MicHost cross-API selftest: SKIP ASIO input -> WASAPI (missing device)");
    }
    if let Some(input) = cable_input {
        for output in outputs
            .iter()
            .filter(|device| device.api == AudioApi::Asio && device.outputs > 0)
        {
            if run_cross(
                backend.clone(),
                input,
                output,
                &format!("WASAPI virtual input -> ASIO output '{}'", output.name),
            ) == 0
            {
                ran += 1;
                break;
            }
        }
    } else {
        println!("MicHost cross-API selftest: SKIP WASAPI -> ASIO output (missing device)");
    }
    let asio_inputs: Vec<&DeviceInfo> = inputs
        .iter()
        .filter(|device| device.api == AudioApi::Asio && device.inputs > 0)
        .collect();
    let asio_outputs: Vec<&DeviceInfo> = outputs
        .iter()
        .filter(|device| device.api == AudioApi::Asio && device.outputs > 0)
        .collect();
    let is_virtual = |name: &str| name.to_lowercase().contains("voicemeeter");

    let same_driver = asio_inputs
        .iter()
        .filter(|input| is_virtual(&input.name))
        .find_map(|input| {
            asio_outputs
                .iter()
                .find(|output| output.name == input.name)
                .map(|output| (*input, *output))
        });
    if let Some((input, output)) = same_driver {
        if run_cross(
            backend.clone(),
            input,
            output,
            &format!("ASIO same driver '{}' -> '{}'", input.name, output.name),
        ) == 0
        {
            ran += 1;
        }
    } else {
        println!("MicHost cross-API selftest: SKIP ASIO same-driver pair (no virtual ASIO driver)");
    }

    let different_driver = asio_inputs
        .iter()
        .find(|input| !is_virtual(&input.name))
        .and_then(|input| {
            asio_outputs
                .iter()
                .find(|output| is_virtual(&output.name) && output.name != input.name)
                .map(|output| (*input, *output))
        });
    if let Some((input, output)) = different_driver {
        if run_cross(
            backend.clone(),
            input,
            output,
            &format!(
                "ASIO different drivers '{}' -> '{}'",
                input.name, output.name
            ),
        ) == 0
        {
            ran += 1;
        }
    } else {
        println!(
            "MicHost cross-API selftest: SKIP ASIO different-driver pair (needs a non-virtual input and a virtual output)"
        );
    }
    if ran == 0 {
        println!("MicHost cross-API selftest: FAILED (no ASIO stream could open)");
        return 1;
    }
    println!("MicHost cross-API selftest: OK");
    0
}

fn run_cross(
    backend: Arc<JuceBackend>,
    input: &DeviceInfo,
    output: &DeviceInfo,
    label: &str,
) -> i32 {
    let mut project = Project::new();
    project.settings.input = selection(input);
    project.settings.output = selection(output);
    let mut session = match AudioSession::start(backend, &mut project) {
        Ok(session) => session,
        Err(error) => {
            println!("MicHost cross-API selftest: FAILED {label}: {error}");
            return 1;
        }
    };
    std::thread::sleep(Duration::from_secs(1));
    let frames = session.frames_processed();
    let in_xruns = session.input_dropouts();
    let out_xruns = session.output_underruns();
    println!(
        "MicHost cross-API selftest: {label}: {frames} frames, xruns in {in_xruns} / out {out_xruns}, {} Hz -> {} Hz",
        session.input().sample_rate as u32,
        session.output().sample_rate as u32
    );
    session.stop(Some(&mut project));
    if frames == 0 {
        return 1;
    }
    0
}

pub fn builtins() -> i32 {
    const RATE: f64 = 48_000.0;
    let mut failures = 0;

    for kind in BuiltinKind::ALL {
        let params = ParamSet::new(kind);
        let analysis = AudioAnalysis::new(RATE as f32);
        let mut processor =
            BuiltinProcessor::with_analysis(kind, params.clone(), RATE, analysis.clone());
        let mut left = vec![0.0f32; 512];
        let mut right = vec![0.0f32; 512];
        let mut peak = 0.0f32;
        let mut finite = true;

        for block in 0..6 {
            for index in 0..left.len() {
                let phase = (block * left.len() + index) as f32 * 0.031;
                left[index] = phase.sin() * 0.25;
                right[index] = (phase * 0.97).cos() * 0.25;
            }
            let frames = left.len();
            let mut channels = [left.as_mut_slice(), right.as_mut_slice()];
            processor.process(&mut channels, frames);
            for sample in left.iter().chain(right.iter()) {
                finite &= sample.is_finite();
                peak = peak.max(sample.abs());
            }
        }

        for index in 0..params.specs().len() {
            params.set_normalized(index, 0.0);
            params.set_normalized(index, 1.0);
            params.set_normalized(index, 0.5);
        }
        for index in 0..left.len() {
            let phase = (6 * left.len() + index) as f32 * 0.031;
            left[index] = phase.sin() * 0.25;
            right[index] = (phase * 0.97).cos() * 0.25;
        }
        let frames = left.len();
        let mut channels = [left.as_mut_slice(), right.as_mut_slice()];
        processor.process(&mut channels, frames);
        for sample in left.iter().chain(right.iter()) {
            finite &= sample.is_finite();
            peak = peak.max(sample.abs());
        }

        let state = params.encode();
        let restored = ParamSet::new(kind);
        let decoded = restored.decode(&state);
        let state_ok = decoded
            && (0..params.specs().len()).all(|index| {
                (params.get(index) - restored.get(index)).abs() <= f32::EPSILON.max(1.0e-6)
            });
        params.set_all_defaults();

        let mut scope_left = vec![0.0f32; 1024];
        let mut scope_right = vec![0.0f32; 1024];
        let snapshot = analysis.snapshot(&mut scope_left, &mut scope_right);
        let analysis_ok = snapshot.sample_rate == RATE as f32
            && snapshot.input_peak_db > -60.0
            && snapshot.output_peak_db.is_finite()
            && snapshot.reduction_db.is_finite()
            && scope_left.iter().all(|sample| sample.is_finite())
            && scope_right.iter().all(|sample| sample.is_finite())
            && scope_left.iter().any(|sample| *sample != 0.0);

        let ok = finite && state_ok && analysis_ok && peak < 1.0e6;
        println!(
            "MicHost built-in selftest: {:<13} {} · latency {} smp · peak {:.3} · state {} · tap {:.1} dB",
            kind.name(),
            if ok { "OK" } else { "FAILED" },
            kind.latency_samples(RATE),
            peak,
            if state_ok { "OK" } else { "FAILED" },
            snapshot.input_peak_db
        );
        if !ok {
            failures += 1;
        }
    }

    if failures == 0 {
        println!(
            "MicHost built-in selftest: OK ({} effects)",
            BuiltinKind::ALL.len()
        );
        0
    } else {
        println!("MicHost built-in selftest: FAILED ({failures} effects)");
        1
    }
}

pub fn isolated_scan() -> i32 {
    let backend = match JuceBackend::load() {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            println!("MicHost isolated scan selftest: FAILED: {error}");
            return 1;
        }
    };
    let started = std::time::Instant::now();
    let receiver = crate::plugin_cache::start(backend, Vec::new(), Vec::new());
    loop {
        match receiver.recv_timeout(Duration::from_secs(120)) {
            Ok(crate::plugin_cache::ScanUpdate::Progress { done, total }) => {
                if done > 0 && done % 10 == 0 {
                    println!("  scanned {done}/{total} modules");
                }
            }
            Ok(crate::plugin_cache::ScanUpdate::Warning(message)) => {
                println!("  warning: {message}");
            }
            Ok(crate::plugin_cache::ScanUpdate::Done(descriptors)) => {
                println!(
                    "MicHost isolated scan selftest: {} plugins in {:.1} s",
                    descriptors.len(),
                    started.elapsed().as_secs_f64()
                );
                if descriptors.is_empty() {
                    println!("MicHost isolated scan selftest: FAILED");
                    return 1;
                }
                println!("MicHost isolated scan selftest: OK");
                return 0;
            }
            Ok(crate::plugin_cache::ScanUpdate::Error(error)) => {
                println!("MicHost isolated scan selftest: FAILED: {error}");
                return 1;
            }
            Err(error) => {
                println!("MicHost isolated scan selftest: FAILED: {error}");
                return 1;
            }
        }
    }
}

pub fn scan() -> i32 {
    let backend = match JuceBackend::load() {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            println!("MicHost scan selftest: FAILED: {error}");
            return 1;
        }
    };
    let started = std::time::Instant::now();
    let bundles = match backend.scan(&mut |_| {}) {
        Ok(bundles) => bundles,
        Err(error) => {
            println!("MicHost scan selftest: FAILED: {error}");
            return 1;
        }
    };
    let mut classes = 0usize;
    let mut failed = 0usize;
    for bundle in &bundles {
        match backend.plugin_descriptions(&bundle.path) {
            Ok(descriptors) => classes += descriptors.len(),
            Err(_) => failed += 1,
        }
    }
    println!(
        "MicHost scan selftest: {} bundles, {} classes, {} unreadable in {:.1} s",
        bundles.len(),
        classes,
        failed,
        started.elapsed().as_secs_f64()
    );
    if bundles.is_empty() || classes == 0 {
        println!("MicHost scan selftest: FAILED");
        return 1;
    }
    println!("MicHost scan selftest: OK");
    0
}

pub fn plugin(path: &str) -> i32 {
    plugin_inner(path, false)
}

pub fn editor(path: &str) -> i32 {
    plugin_inner(path, true)
}

fn plugin_inner(path: &str, exercise_editor: bool) -> i32 {
    let backend = match JuceBackend::load() {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            println!("MicHost plugin selftest: FAILED: {error}");
            return 1;
        }
    };
    let descriptors = match backend.plugin_descriptions(path) {
        Ok(descriptors) => descriptors,
        Err(error) => {
            println!("MicHost plugin selftest: FAILED: {error}");
            return 1;
        }
    };
    if descriptors.is_empty() {
        println!("MicHost plugin selftest: FAILED: no classes in {path}");
        return 1;
    }
    for descriptor in &descriptors {
        println!(
            "  class {} | {} | {} in / {} out / SC {}",
            descriptor.class_id,
            descriptor.name,
            descriptor.main_inputs,
            descriptor.main_outputs,
            descriptor.sidechain_inputs
        );
    }
    let Some(descriptor) = descriptors.first() else {
        println!("MicHost plugin selftest: FAILED: no class");
        return 1;
    };

    let handle = match backend.load_plugin(path, &descriptor.class_id) {
        Ok(handle) => handle,
        Err(error) => {
            println!("MicHost plugin selftest: FAILED: load: {error}");
            return 1;
        }
    };
    let channels = match backend.plugin_channels(handle) {
        Ok(channels) => channels,
        Err(error) => {
            println!("MicHost plugin selftest: FAILED: channels: {error}");
            let _ = backend.free_plugin(handle);
            return 1;
        }
    };
    println!(
        "Loaded {}: main {} in / {} out, sidechain {}",
        descriptor.name, channels.main_inputs, channels.main_outputs, channels.sidechain_inputs
    );
    let sidechain = channels.sidechain_inputs.min(2);
    if let Err(error) = backend.prepare_plugin(
        handle,
        48_000.0,
        256,
        channels.main_inputs as u32,
        channels.main_outputs as u32,
        sidechain as u32,
    ) {
        println!("MicHost plugin selftest: FAILED: prepare: {error}");
        let _ = backend.free_plugin(handle);
        return 1;
    }
    if let Err(error) = backend.activate_plugin(handle, true) {
        println!("MicHost plugin selftest: FAILED: activate: {error}");
        let _ = backend.free_plugin(handle);
        return 1;
    }
    if exercise_editor {
        println!("MicHost plugin selftest: opening the editor");
        if let Err(error) = backend.open_editor(handle) {
            println!("MicHost plugin selftest: FAILED: open editor: {error}");
            let _ = backend.activate_plugin(handle, false);
            let _ = backend.free_plugin(handle);
            return 1;
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
        match backend.state_export(handle, StateKind::Component) {
            Ok(state) => println!(
                "MicHost plugin selftest: state export while the editor is open: {} bytes",
                state.len()
            ),
            Err(error) => println!(
                "MicHost plugin selftest: state export while the editor is open failed: {error}"
            ),
        }
        if let Err(error) = backend.close_editor(handle) {
            println!("MicHost plugin selftest: FAILED: close editor: {error}");
            let _ = backend.activate_plugin(handle, false);
            let _ = backend.free_plugin(handle);
            return 1;
        }
        match backend.state_export(handle, StateKind::Component) {
            Ok(state) => println!(
                "MicHost plugin selftest: state export after the editor closed: {} bytes",
                state.len()
            ),
            Err(error) => println!(
                "MicHost plugin selftest: state export after the editor closed failed: {error}"
            ),
        }
        println!("MicHost plugin selftest: editor opened and closed");
    }
    let prepared = backend.plugin_channels(handle).unwrap_or(channels);
    let total_in = prepared.main_inputs as usize + sidechain as usize;
    let total_out = prepared.main_outputs as usize;
    let process_channels = total_in.max(total_out).max(1);
    let mut buffers: Vec<Vec<f32>> = (0..process_channels)
        .map(|channel| {
            (0..256usize)
                .map(|frame| (frame as f32 * 0.01 + channel as f32 * 0.1).sin() * 0.05)
                .collect()
        })
        .collect();
    let mut refs: Vec<&mut [f32]> = buffers
        .iter_mut()
        .map(|buffer| buffer.as_mut_slice())
        .collect();
    if let Err(error) = backend.process_plugin(handle, &mut refs, 256) {
        println!("MicHost plugin selftest: FAILED: process: {error}");
        let _ = backend.activate_plugin(handle, false);
        let _ = backend.free_plugin(handle);
        return 1;
    }
    let finite = buffers
        .iter()
        .all(|buffer| buffer.iter().all(|sample| sample.is_finite()));
    let latency = backend.plugin_latency(handle).unwrap_or(0);
    let state = match backend.state_export(handle, StateKind::Component) {
        Ok(state) => state,
        Err(error) => {
            println!("MicHost plugin selftest: FAILED: state export: {error}");
            let _ = backend.activate_plugin(handle, false);
            let _ = backend.free_plugin(handle);
            return 1;
        }
    };
    let restored = backend
        .state_import(handle, StateKind::Component, &state)
        .map_err(|error| error.to_string());
    println!(
        "Processed {process_channels} ch x 256 frames, finite={finite}, latency={latency} smp, state={} bytes, restore={}",
        state.len(),
        match &restored {
            Ok(()) => "ok",
            Err(error) => error.as_str(),
        }
    );
    let _ = backend.activate_plugin(handle, false);
    let _ = backend.free_plugin(handle);
    if !finite {
        println!("MicHost plugin selftest: FAILED: non-finite output");
        return 1;
    }
    if let Err(error) = restored {
        println!("MicHost plugin selftest: FAILED: state restore: {error}");
        return 1;
    }
    println!("MicHost plugin selftest: OK");
    0
}

pub fn chain(path: &str) -> i32 {
    let backend = match JuceBackend::load() {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            println!("MicHost chain selftest: FAILED: {error}");
            return 1;
        }
    };
    let descriptors = match backend.plugin_descriptions(path) {
        Ok(descriptors) => descriptors,
        Err(error) => {
            println!("MicHost chain selftest: FAILED: {error}");
            return 1;
        }
    };
    let Some(descriptor) = descriptors.first() else {
        println!("MicHost chain selftest: FAILED: no usable class in {path}");
        return 1;
    };
    let inputs = match backend.enumerate_devices(DeviceKind::Input) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost chain selftest: FAILED: {error}");
            return 1;
        }
    };
    let outputs = match backend.enumerate_devices(DeviceKind::Output) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost chain selftest: FAILED: {error}");
            return 1;
        }
    };
    let Some(input) = inputs
        .iter()
        .find(|device| device.api == AudioApi::WasapiShared && device.inputs > 0)
    else {
        println!("MicHost chain selftest: SKIP (no WASAPI input)");
        return 0;
    };
    let Some(output) = outputs.iter().find(|device| {
        device.api == AudioApi::WasapiShared
            && device.outputs > 0
            && device.name.to_lowercase().contains("cable input")
    }) else {
        println!("MicHost chain selftest: SKIP (no virtual cable output)");
        return 0;
    };

    let mut project = Project::new();
    project.settings.input = selection(input);
    project.settings.output = selection(output);
    println!(
        "MicHost chain selftest: {} via {} -> {}",
        descriptor.name, input.name, output.name
    );

    let mut session = match AudioSession::start(backend.clone(), &mut project) {
        Ok(session) => session,
        Err(error) => {
            println!("MicHost chain selftest: FAILED: start: {error}");
            return 1;
        }
    };
    std::thread::sleep(Duration::from_secs(1));
    let frames_start = session.frames_processed();
    if frames_start == 0 {
        println!("MicHost chain selftest: FAILED (no frames before insert)");
        session.stop(Some(&mut project));
        return 1;
    }
    let rate = session.sample_rate();
    let block = session.block_size();
    let auxiliary = descriptor.sidechain_inputs > 0;

    project.chain.push(michost_graph::ChainEntry {
        id: 1,
        plugin: descriptor.to_spec(),
        bypassed: false,
        sidechain_source: michost_graph::SidechainSource::MainMic,
        oversampling: 1,
    });
    let prepared = match prepare_plugin_for_insert(
        backend.clone(),
        &descriptor.path,
        &descriptor.class_id,
        &descriptor.name,
        rate,
        block,
        if auxiliary { 1 } else { 0 },
        None,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            println!("MicHost chain selftest: FAILED: prepare: {error}");
            session.stop(Some(&mut project));
            return 1;
        }
    };
    if let Err(error) = session.insert_prepared(1, prepared) {
        println!("MicHost chain selftest: FAILED: insert: {error}");
        session.stop(Some(&mut project));
        return 1;
    }
    if let Err(error) = session.rebuild_engine(&project) {
        println!("MicHost chain selftest: FAILED: rebuild after insert: {error}");
        session.stop(Some(&mut project));
        return 1;
    }
    std::thread::sleep(Duration::from_secs(1));
    let frames_added = session.frames_processed();
    let in_xruns = session.input_dropouts();
    let out_xruns = session.output_underruns();
    let latency = session.end_to_end_milliseconds();
    let cpu = session
        .plugin_cpu_fraction(1)
        .map(|fraction| fraction * 100.0)
        .unwrap_or(0.0);
    let memory = session.plugin_memory_bytes(1).unwrap_or(0);
    let plugin_latency_ms = session.plugin_latency_ms(1).unwrap_or(0.0);
    println!(
        "MicHost chain selftest: hot insert {frames_start} -> {frames_added} frames, latency {latency:.1} ms, xruns in {in_xruns} / out {out_xruns}"
    );
    println!(
        "  stats: cpu {cpu:.1}%, ram {memory} bytes (estimate), latency {plugin_latency_ms:.2} ms, chain {} smp, block {block}",
        session.total_latency()
    );
    if frames_added <= frames_start {
        println!("MicHost chain selftest: FAILED (frames did not advance after insert)");
        session.stop(Some(&mut project));
        return 1;
    }

    if let Some(loopback) = inputs.iter().find(|device| {
        device.inputs > 0
            && device.name.to_lowercase().contains("cable output")
            && device.api == AudioApi::WasapiShared
    }) {
        if let Err(error) = session.begin_chain_measurement() {
            println!("MicHost chain selftest: FAILED: start chain measurement: {error}");
            session.stop(Some(&mut project));
            return 1;
        }
        let capture = match crate::latency::capture_input(&backend, loopback, 2) {
            Ok(capture) => capture,
            Err(error) => {
                let _ = session.end_chain_measurement();
                println!("MicHost chain selftest: FAILED: capture chain marker: {error}");
                session.stop(Some(&mut project));
                return 1;
            }
        };
        let clicks = session.end_chain_measurement();
        match crate::latency::correlate(&capture, &clicks) {
            Ok(measured) => println!(
                "MicHost chain selftest: chain marker p50 {:.1} ms, p95 {:.1} ms ({} matched), plugin latency {plugin_latency_ms:.1} ms",
                measured.p50_ms, measured.p95_ms, measured.matched
            ),
            Err(error) => {
                println!("MicHost chain selftest: FAILED: correlate chain marker: {error}");
                session.stop(Some(&mut project));
                return 1;
            }
        }
    } else {
        println!("MicHost chain selftest: SKIP chain marker (no WASAPI cable capture)");
    }

    let frames_before_remove = session.frames_processed();
    project.chain.clear();
    session.retire_plugin(1);
    if let Err(error) = session.rebuild_engine(&project) {
        println!("MicHost chain selftest: FAILED: rebuild after retire: {error}");
        session.stop(Some(&mut project));
        return 1;
    }
    std::thread::sleep(Duration::from_secs(1));
    let frames_after_remove = session.frames_processed();
    let removed_xruns = session.output_underruns();
    println!(
        "MicHost chain selftest: hot remove {frames_before_remove} -> {frames_after_remove} frames, xruns out {removed_xruns}"
    );
    if frames_after_remove <= frames_before_remove {
        println!("MicHost chain selftest: FAILED (frames did not advance after retire)");
        session.stop(Some(&mut project));
        return 1;
    }

    let second = michost_graph::ChainEntry {
        id: 2,
        plugin: descriptor.to_spec(),
        bypassed: false,
        sidechain_source: michost_graph::SidechainSource::MainMic,
        oversampling: 1,
    };
    project.chain.push(second);
    let prepared = match prepare_plugin_for_insert(
        backend.clone(),
        &descriptor.path,
        &descriptor.class_id,
        &descriptor.name,
        rate,
        block,
        if auxiliary { 1 } else { 0 },
        None,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            println!("MicHost chain selftest: FAILED: prepare again: {error}");
            session.stop(Some(&mut project));
            return 1;
        }
    };
    if let Err(error) = session.insert_prepared_with_project(&project, 2, prepared) {
        println!("MicHost chain selftest: FAILED: insert again: {error}");
        session.stop(Some(&mut project));
        return 1;
    }
    std::thread::sleep(Duration::from_secs(1));
    let frames_warm = session.frames_processed();
    session.stop(Some(&mut project));
    let state_len = project
        .plugin_state(2)
        .map(|state| state.len())
        .unwrap_or(0);
    println!(
        "MicHost chain selftest: reinserted entry 2, {frames_warm} frames, state {state_len} bytes"
    );
    if frames_warm <= frames_after_remove || state_len == 0 {
        println!("MicHost chain selftest: FAILED (frames={frames_warm}, state={state_len})");
        return 1;
    }

    let mut session = match AudioSession::start_with_options(backend, &mut project, Some(0)) {
        Ok(session) => session,
        Err(error) => {
            println!("MicHost chain selftest: FAILED: restart with state: {error}");
            return 1;
        }
    };
    std::thread::sleep(Duration::from_secs(1));
    let frames = session.frames_processed();
    let out_xruns = session.output_underruns();
    println!(
        "MicHost chain selftest: restored run {frames} frames, state {state_len} bytes, mono left, xruns out {out_xruns}"
    );
    session.stop(Some(&mut project));
    if frames == 0 {
        println!("MicHost chain selftest: FAILED (no frames after restore)");
        return 1;
    }
    println!("MicHost chain selftest: OK");
    0
}

pub fn second_input(plugin_path: Option<&str>) -> i32 {
    let backend = match JuceBackend::load() {
        Ok(backend) => Arc::new(backend),
        Err(error) => {
            println!("MicHost second input selftest: FAILED: {error}");
            return 1;
        }
    };
    let inputs = match backend.enumerate_devices(DeviceKind::Input) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost second input selftest: FAILED: {error}");
            return 1;
        }
    };
    let outputs = match backend.enumerate_devices(DeviceKind::Output) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost second input selftest: FAILED: {error}");
            return 1;
        }
    };

    let Some(primary) = inputs
        .iter()
        .find(|device| device.api == AudioApi::WasapiShared && device.inputs > 0)
    else {
        println!("MicHost second input selftest: SKIP (no WASAPI input)");
        return 0;
    };
    let Some(second) = inputs.iter().find(|device| {
        device.api == AudioApi::WasapiShared
            && device.inputs >= 2
            && device.name.to_lowercase().contains("cable output")
    }) else {
        println!("MicHost second input selftest: SKIP (no WASAPI virtual input)");
        return 0;
    };
    let Some(output) = outputs.iter().find(|device| {
        device.api == AudioApi::WasapiShared
            && device.outputs > 0
            && device.name.to_lowercase().contains("cable input")
    }) else {
        println!("MicHost second input selftest: SKIP (no virtual cable output)");
        return 0;
    };
    if second.id == primary.id {
        println!(
            "MicHost second input selftest: SKIP (primary and sidechain device are identical)"
        );
        return 0;
    }

    println!(
        "MicHost second input selftest: {} + sidechain {} -> {} for 2s",
        primary.name, second.name, output.name
    );
    let mut project = Project::new();
    project.settings.input = selection(primary);
    project.settings.output = selection(output);
    project.settings.sidechain_input = selection(second);
    project.settings.sidechain_input_enabled = true;
    project.settings.sample_rate = 0.0;
    project.settings.buffer_size = 0;

    let mut used_plugin = false;
    if let Some(path) = plugin_path {
        if std::path::Path::new(path).exists() {
            let descriptors = match backend.plugin_descriptions(path) {
                Ok(descriptors) => descriptors,
                Err(error) => {
                    println!("MicHost second input selftest: FAILED: descriptions: {error}");
                    return 1;
                }
            };
            let descriptor = descriptors.first();
            let Some(descriptor) = descriptor else {
                println!("MicHost second input selftest: FAILED: no usable class in {path}");
                return 1;
            };
            println!(
                "  plugin {} ({} in / {} out / SC {}) with sidechain source Aux 1",
                descriptor.name,
                descriptor.main_inputs,
                descriptor.main_outputs,
                descriptor.sidechain_inputs
            );
            project.chain.push(michost_graph::ChainEntry {
                id: 1,
                plugin: descriptor.to_spec(),
                bypassed: false,
                sidechain_source: michost_graph::SidechainSource::SecondInput(0),
                oversampling: 1,
            });
            used_plugin = true;
        } else {
            println!("  plugin path not found: {path} (running with an empty chain)");
        }
    }

    let mut session = match AudioSession::start(backend.clone(), &mut project) {
        Ok(session) => session,
        Err(error) => {
            println!("MicHost second input selftest: FAILED: start: {error}");
            return 1;
        }
    };
    if let Some(info) = session.second_input() {
        println!(
            "  sidechain device {} ch @ {:.0} Hz, buffer {}",
            info.channels, info.sample_rate, info.buffer_size
        );
    }
    for warning in &session.warnings {
        println!("  warning: {warning}");
    }
    std::thread::sleep(Duration::from_secs(2));
    let frames = session.frames_processed();
    let in_xruns = session.input_dropouts();
    let out_xruns = session.output_underruns();
    let starved = session.second_input_starved();
    let second_drops = session.second_input_dropouts();
    let latency = session.end_to_end_milliseconds();
    println!(
        "MicHost second input selftest: {frames} frames, latency {latency:.1} ms, xruns in {in_xruns} / out {out_xruns}, sidechain starved {starved} frames, sidechain drops {second_drops}"
    );
    session.stop(Some(&mut project));
    if frames == 0 {
        println!("MicHost second input selftest: FAILED (no frames processed)");
        return 1;
    }
    let state_len = project
        .plugin_state(1)
        .map(|state| state.len())
        .unwrap_or(0);
    if used_plugin && state_len == 0 {
        println!("MicHost second input selftest: FAILED (no plugin state exported)");
        return 1;
    }

    if used_plugin {
        let mut session = match AudioSession::start(backend, &mut project) {
            Ok(session) => session,
            Err(error) => {
                println!("MicHost second input selftest: FAILED: restart with state: {error}");
                return 1;
            }
        };
        std::thread::sleep(Duration::from_secs(1));
        let frames = session.frames_processed();
        let in_xruns = session.input_dropouts();
        let out_xruns = session.output_underruns();
        let starved = session.second_input_starved();
        println!(
            "MicHost second input selftest: restored run {frames} frames, state {state_len} bytes, xruns in {in_xruns} / out {out_xruns}, sidechain starved {starved} frames"
        );
        session.stop(Some(&mut project));
        if frames == 0 {
            println!("MicHost second input selftest: FAILED (no frames after restore)");
            return 1;
        }
    }

    println!("MicHost second input selftest: OK");
    0
}

fn selection(device: &DeviceInfo) -> DeviceSelection {
    DeviceSelection {
        api: device.api,
        name: device.name.clone(),
        id: device.id.clone(),
        channels: device.inputs.max(device.outputs).clamp(1, 2),
        channel_mask: 0,
    }
}

fn api_name(api: michost_host_abi::AudioApi) -> &'static str {
    match api {
        michost_host_abi::AudioApi::WasapiShared => "WASAPI",
        michost_host_abi::AudioApi::WasapiLowLatency => "WASAPI LL",
        michost_host_abi::AudioApi::WasapiExclusive => "WASAPI EX",
        michost_host_abi::AudioApi::Asio => "ASIO",
        michost_host_abi::AudioApi::Other => "OTHER",
    }
}
