use std::ffi::c_void;
use std::time::{Duration, Instant};

use michost_host_abi::{
    AudioApi, DeviceInfo, DeviceKind, DeviceLatency, StreamCallbacks, StreamConfig,
    StreamDirection, StreamHost,
};
use michost_host_juce::JuceBackend;

const BURST_FRAMES: usize = 64;
const PERIOD_MILLIS: u64 = 500;
const MAX_DELAY_MILLIS: u64 = 400;
const RETRIGGER_FRAMES: usize = 1024;
const MAX_CAPTURE_FRAMES: usize = 64_000_000;

#[derive(Clone, Debug)]
pub struct Options {
    pub seconds: u64,
    pub output: Option<String>,
    pub input: Option<String>,
    pub api: AudioApi,
    pub block_size: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            seconds: 10,
            output: None,
            input: None,
            api: AudioApi::WasapiShared,
            block_size: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Measurement {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub matched: usize,
    pub clicks: usize,
    pub onsets: usize,
    pub dropped: u64,
    pub output_rate: u32,
    pub input_rate: u32,
    pub output_buffer: u32,
    pub input_buffer: u32,
    pub output_latency: u32,
    pub input_latency: u32,
}

pub fn parse_api(value: &str) -> Option<AudioApi> {
    match value.to_lowercase().as_str() {
        "wasapi" | "shared" | "wasapi-shared" => Some(AudioApi::WasapiShared),
        "wasapi-low" | "low" | "low-latency" => Some(AudioApi::WasapiLowLatency),
        "wasapi-exclusive" | "exclusive" => Some(AudioApi::WasapiExclusive),
        "asio" => Some(AudioApi::Asio),
        _ => None,
    }
}

struct OutputContext {
    rate: f64,
    period: u64,
    pattern: Vec<f32>,
    position: u64,
    clicks: rtrb::Producer<Instant>,
}

struct InputContext {
    rate: f64,
    samples: rtrb::Producer<f32>,
    blocks: rtrb::Producer<BlockStamp>,
    written: u64,
    dropped: u64,
}

struct BlockStamp {
    start: u64,
    time: Instant,
}

pub fn run(options: Options) -> i32 {
    let seconds = options.seconds.max(1);
    let backend = match JuceBackend::load() {
        Ok(backend) => backend,
        Err(error) => {
            println!("MicHost latency selftest: FAILED: {error}");
            return 1;
        }
    };
    let inputs = match backend.enumerate_devices(DeviceKind::Input) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost latency selftest: FAILED: {error}");
            return 1;
        }
    };
    let outputs = match backend.enumerate_devices(DeviceKind::Output) {
        Ok(devices) => devices,
        Err(error) => {
            println!("MicHost latency selftest: FAILED: {error}");
            return 1;
        }
    };

    let api = options.api;
    let Some(output) = pick(
        &outputs,
        DeviceKind::Output,
        options.output.as_deref(),
        api,
        "cable input",
    ) else {
        println!(
            "MicHost latency selftest: SKIP (no matching {} output; use --output <name>)",
            api_name(api)
        );
        return 0;
    };
    let Some(input) = pick(
        &inputs,
        DeviceKind::Input,
        options.input.as_deref(),
        api,
        "cable output",
    ) else {
        println!(
            "MicHost latency selftest: SKIP (no matching {} input; use --input <name>)",
            api_name(api)
        );
        return 0;
    };

    let result = match measure(
        &backend,
        output,
        input,
        seconds,
        api,
        api,
        options.block_size,
        options.block_size,
    ) {
        Ok(result) => result,
        Err(error) => {
            println!("MicHost latency selftest: FAILED: {error}");
            return 1;
        }
    };
    println!(
        "MicHost latency selftest: '{}' -> '{}' via {} for {seconds}s",
        output.name,
        input.name,
        api_name(api)
    );
    println!(
        "MicHost latency selftest: {:.0} Hz / {:.0} Hz, stream buffers {} / {}, device latency out {} / in {} samples",
        result.output_rate,
        result.input_rate,
        result.output_buffer,
        result.input_buffer,
        result.output_latency,
        result.input_latency
    );
    println!(
        "MicHost latency selftest: {} clicks written, {} onsets captured, {} matched, {} dropped frames",
        result.clicks, result.onsets, result.matched, result.dropped
    );
    println!(
        "MicHost latency selftest: callback-to-callback p50 {:.1} ms, p95 {:.1} ms (min {:.1}, max {:.1})",
        result.p50_ms, result.p95_ms, result.min_ms, result.max_ms
    );
    println!(
        "MicHost latency selftest: this measures the output/cable/capture loopback including the output callback/buffer lead; it excludes the mic input, MicHost's host output ring and the app buffer. Compare runs at the same block/API"
    );
    println!("MicHost latency selftest: OK");
    0
}

#[allow(clippy::too_many_arguments)]
pub fn measure(
    backend: &JuceBackend,
    output: &DeviceInfo,
    input: &DeviceInfo,
    seconds: u64,
    output_api: AudioApi,
    input_api: AudioApi,
    output_block: u32,
    input_block: u32,
) -> Result<Measurement, String> {
    let seconds = seconds.max(1);
    let out_channels = output.outputs.clamp(1, 2);
    let in_channels = input.inputs.clamp(1, 2);
    let default_rate = if input.default_sample_rate > 0.0 {
        input.default_sample_rate
    } else {
        48_000.0
    };
    let block_hint = if input_block > 0 { input_block } else { 512 };

    let capture_capacity = ((default_rate.max(48_000.0) * (seconds as f64 + 2.0)) as usize)
        .clamp(4096, MAX_CAPTURE_FRAMES);
    let (samples_tx, mut samples_rx) = rtrb::RingBuffer::new(capture_capacity);
    let (blocks_tx, mut blocks_rx) = rtrb::RingBuffer::new(
        ((default_rate * (seconds as f64 + 2.0) / block_hint as f64) as usize).clamp(4096, 262_144),
    );
    let (clicks_tx, mut clicks_rx) =
        rtrb::RingBuffer::new((seconds as usize * 2 + 16).clamp(16, 4096));

    let mut output_context = Box::new(OutputContext {
        rate: default_rate,
        period: ((default_rate * PERIOD_MILLIS as f64) / 1000.0).round() as u64,
        pattern: Vec::new(),
        position: 0,
        clicks: clicks_tx,
    });
    let mut input_context = Box::new(InputContext {
        rate: default_rate,
        samples: samples_tx,
        blocks: blocks_tx,
        written: 0,
        dropped: 0,
    });

    let output_callbacks = StreamCallbacks {
        ctx: (&mut *output_context as *mut OutputContext).cast::<c_void>(),
        on_input: None,
        on_output: Some(output_callback),
        on_error: None,
    };
    let input_callbacks = StreamCallbacks {
        ctx: (&mut *input_context as *mut InputContext).cast::<c_void>(),
        on_input: Some(input_callback),
        on_output: None,
        on_error: None,
    };

    let output_config = StreamConfig {
        device: output.name.clone(),
        sample_rate: 0.0,
        block_size: output_block,
        channels: out_channels,
        api: output_api,
        direction: StreamDirection::Output,
        channel_mask: 0,
        output_channels: 0,
        output_channel_mask: 0,
    };
    let output_stream = backend
        .open_stream(&output_config, output_callbacks)
        .map_err(|error| {
            format!(
                "opening output '{}': {error}; {}",
                output.name,
                buffer_hint(output)
            )
        })?;
    let output_latency = match backend.stream_latency(&output_stream) {
        Ok(latency) => latency,
        Err(error) => {
            backend.close_stream(output_stream);
            return Err(format!("reading output latency: {error}"));
        }
    };
    let out_rate = if output_latency.sample_rate > 0 {
        output_latency.sample_rate as f64
    } else {
        default_rate
    };
    output_context.rate = out_rate;
    output_context.period = ((out_rate * PERIOD_MILLIS as f64) / 1000.0)
        .round()
        .max(BURST_FRAMES as f64) as u64;
    output_context.pattern = make_pattern(out_rate, output_context.period as usize);

    let input_config = StreamConfig {
        device: input.name.clone(),
        sample_rate: 0.0,
        block_size: input_block,
        channels: in_channels,
        api: input_api,
        direction: StreamDirection::Input,
        channel_mask: 0,
        output_channels: 0,
        output_channel_mask: 0,
    };
    let input_stream = match backend.open_stream(&input_config, input_callbacks) {
        Ok(stream) => stream,
        Err(error) => {
            backend.close_stream(output_stream);
            return Err(format!(
                "opening input '{}': {error}; {}",
                input.name,
                buffer_hint(input)
            ));
        }
    };
    let input_latency = match backend.stream_latency(&input_stream) {
        Ok(latency) => latency,
        Err(error) => {
            backend.close_stream(input_stream);
            backend.close_stream(output_stream);
            return Err(format!("reading input latency: {error}"));
        }
    };
    let in_rate = if input_latency.sample_rate > 0 {
        input_latency.sample_rate as f64
    } else {
        default_rate
    };
    input_context.rate = in_rate;

    if let Err(error) = backend.start_stream(&input_stream) {
        backend.close_stream(input_stream);
        backend.close_stream(output_stream);
        return Err(format!("starting input: {error}"));
    }
    if let Err(error) = backend.start_stream(&output_stream) {
        let _ = backend.stop_stream(&input_stream);
        backend.close_stream(input_stream);
        backend.close_stream(output_stream);
        return Err(format!("starting output: {error}"));
    }

    std::thread::sleep(Duration::from_secs(seconds));

    let _ = backend.stop_stream(&input_stream);
    let _ = backend.stop_stream(&output_stream);
    backend.close_stream(input_stream);
    backend.close_stream(output_stream);

    let mut captured = Vec::new();
    while let Ok(sample) = samples_rx.pop() {
        captured.push(sample);
    }
    let mut blocks = Vec::new();
    while let Ok(stamp) = blocks_rx.pop() {
        blocks.push(stamp);
    }
    let mut clicks = Vec::new();
    while let Ok(click) = clicks_rx.pop() {
        clicks.push(click);
    }
    let dropped = input_context.dropped;

    if captured.is_empty() {
        return Err("no capture frames".to_string());
    }
    if dropped > 0 {
        return Err(format!(
            "{dropped} capture frames dropped; lower the duration or rate"
        ));
    }
    let peak = captured
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if peak < 0.02 {
        return Err(format!(
            "no loopback signal (peak {peak:.3}); check the cable routing"
        ));
    }
    let threshold = (peak * 0.2).max(0.02);
    let onsets = detect_onsets(&captured, threshold);
    let mut used = vec![false; onsets.len()];
    let mut delays = Vec::new();
    for &click in &clicks {
        let mut best: Option<(usize, Duration)> = None;
        for (index, &onset) in onsets.iter().enumerate() {
            if used[index] {
                continue;
            }
            let Some(time) = sample_time(&blocks, in_rate, onset as u64) else {
                continue;
            };
            let Some(delta) = time.checked_duration_since(click) else {
                continue;
            };
            if delta > Duration::from_millis(MAX_DELAY_MILLIS) {
                continue;
            }
            if best.is_none_or(|(_, current)| delta < current) {
                best = Some((index, delta));
            }
        }
        if let Some((index, delta)) = best {
            used[index] = true;
            delays.push(delta.as_secs_f64() * 1000.0);
        }
    }
    if delays.len() < 2 {
        return Err("could not match the click train".to_string());
    }
    delays.sort_by(|left, right| left.total_cmp(right));
    Ok(Measurement {
        p50_ms: percentile(&delays, 0.50),
        p95_ms: percentile(&delays, 0.95),
        min_ms: delays[0],
        max_ms: delays[delays.len() - 1],
        matched: delays.len(),
        clicks: clicks.len(),
        onsets: onsets.len(),
        dropped,
        output_rate: out_rate as u32,
        input_rate: in_rate as u32,
        output_buffer: output_latency.buffer,
        input_buffer: input_latency.buffer,
        output_latency: output_latency.output,
        input_latency: input_latency.input,
    })
}

pub struct Capture {
    samples: Vec<f32>,
    blocks: Vec<BlockStamp>,
    rate: f64,
    latency: DeviceLatency,
    dropped: u64,
}

pub fn capture_input(
    backend: &JuceBackend,
    input: &DeviceInfo,
    seconds: u64,
) -> Result<Capture, String> {
    let seconds = seconds.max(1);
    let channels = input.inputs.clamp(1, 2);
    let default_rate = if input.default_sample_rate > 0.0 {
        input.default_sample_rate
    } else {
        48_000.0
    };
    let capture_capacity = ((default_rate.max(48_000.0) * (seconds as f64 + 2.0)) as usize)
        .clamp(4096, MAX_CAPTURE_FRAMES);
    let (samples_tx, mut samples_rx) = rtrb::RingBuffer::new(capture_capacity);
    let (blocks_tx, mut blocks_rx) = rtrb::RingBuffer::new(
        ((default_rate * (seconds as f64 + 2.0) / 512.0) as usize).clamp(4096, 262_144),
    );
    let mut context = Box::new(InputContext {
        rate: default_rate,
        samples: samples_tx,
        blocks: blocks_tx,
        written: 0,
        dropped: 0,
    });
    let callbacks = StreamCallbacks {
        ctx: (&mut *context as *mut InputContext).cast::<c_void>(),
        on_input: Some(input_callback),
        on_output: None,
        on_error: None,
    };
    let config = StreamConfig {
        device: input.name.clone(),
        sample_rate: 0.0,
        block_size: 0,
        channels,
        api: input.api,
        direction: StreamDirection::Input,
        channel_mask: 0,
        output_channels: 0,
        output_channel_mask: 0,
    };
    let stream = backend.open_stream(&config, callbacks).map_err(|error| {
        format!(
            "opening input '{}': {error}; {}",
            input.name,
            buffer_hint(input)
        )
    })?;
    let latency = match backend.stream_latency(&stream) {
        Ok(latency) => latency,
        Err(error) => {
            backend.close_stream(stream);
            return Err(format!("reading input latency: {error}"));
        }
    };
    let rate = if latency.sample_rate > 0 {
        latency.sample_rate as f64
    } else {
        default_rate
    };
    context.rate = rate;
    if let Err(error) = backend.start_stream(&stream) {
        backend.close_stream(stream);
        return Err(format!("starting input: {error}"));
    }
    std::thread::sleep(Duration::from_secs(seconds));
    let _ = backend.stop_stream(&stream);
    backend.close_stream(stream);

    let mut samples = Vec::new();
    while let Ok(sample) = samples_rx.pop() {
        samples.push(sample);
    }
    let mut blocks = Vec::new();
    while let Ok(stamp) = blocks_rx.pop() {
        blocks.push(stamp);
    }
    let dropped = context.dropped;
    if samples.is_empty() {
        return Err("no capture frames".to_string());
    }
    if dropped > 0 {
        return Err(format!(
            "{dropped} capture frames dropped; lower the duration or rate"
        ));
    }
    Ok(Capture {
        samples,
        blocks,
        rate,
        latency,
        dropped,
    })
}

pub fn correlate(capture: &Capture, clicks: &[std::time::Instant]) -> Result<Measurement, String> {
    let peak = capture
        .samples
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if peak < 0.02 {
        return Err("no loopback signal; check the output/capture routing and bypass plugins that mute or gate the marker".to_string());
    }
    let threshold = (peak * 0.2).max(0.02);
    let onsets = detect_onsets(&capture.samples, threshold);
    let mut used = vec![false; onsets.len()];
    let mut delays = Vec::new();
    for &click in clicks {
        let mut best: Option<(usize, Duration)> = None;
        for (index, &onset) in onsets.iter().enumerate() {
            if used[index] {
                continue;
            }
            let Some(time) = sample_time(&capture.blocks, capture.rate, onset as u64) else {
                continue;
            };
            let Some(delta) = time.checked_duration_since(click) else {
                continue;
            };
            if delta > Duration::from_millis(MAX_DELAY_MILLIS) {
                continue;
            }
            if best.is_none_or(|(_, current)| delta < current) {
                best = Some((index, delta));
            }
        }
        if let Some((index, delta)) = best {
            used[index] = true;
            delays.push(delta.as_secs_f64() * 1000.0);
        }
    }
    if delays.len() < 2 {
        return Err("could not match the marker; the chain may be filtering it".to_string());
    }
    delays.sort_by(|left, right| left.total_cmp(right));
    Ok(Measurement {
        p50_ms: percentile(&delays, 0.50),
        p95_ms: percentile(&delays, 0.95),
        min_ms: delays[0],
        max_ms: delays[delays.len() - 1],
        matched: delays.len(),
        clicks: clicks.len(),
        onsets: onsets.len(),
        dropped: capture.dropped,
        output_rate: 0,
        input_rate: capture.rate as u32,
        output_buffer: 0,
        input_buffer: capture.latency.buffer,
        output_latency: 0,
        input_latency: capture.latency.input,
    })
}

fn pick<'a>(
    devices: &'a [DeviceInfo],
    kind: DeviceKind,
    wanted: Option<&str>,
    api: AudioApi,
    needle: &str,
) -> Option<&'a DeviceInfo> {
    let has_channels = |device: &DeviceInfo| match kind {
        DeviceKind::Input => device.inputs > 0,
        DeviceKind::Output => device.outputs > 0,
    };
    if let Some(wanted) = wanted {
        let wanted = wanted.to_lowercase();
        if let Some(device) = devices.iter().find(|device| {
            device.api == api && has_channels(device) && device.name.to_lowercase() == wanted
        }) {
            return Some(device);
        }
        return devices.iter().find(|device| {
            device.api == api
                && has_channels(device)
                && device.name.to_lowercase().contains(&wanted)
        });
    }
    devices.iter().find(|device| {
        device.api == api && has_channels(device) && device.name.to_lowercase().contains(needle)
    })
}

fn make_pattern(rate: f64, period: usize) -> Vec<f32> {
    let mut pattern = vec![0.0f32; period.max(BURST_FRAMES)];
    let frequency = 800.0;
    for (index, sample) in pattern.iter_mut().take(BURST_FRAMES).enumerate() {
        *sample =
            (0.4 * (2.0 * std::f64::consts::PI * frequency * index as f64 / rate).cos()) as f32;
    }
    pattern
}

unsafe extern "C" fn output_callback(
    ctx: *mut c_void,
    channels: *const *mut f32,
    num_channels: i32,
    frames: i32,
    _sample_pos: i64,
) {
    if ctx.is_null() || channels.is_null() || frames <= 0 || num_channels <= 0 {
        return;
    }
    unsafe {
        let context = &mut *(ctx as *mut OutputContext);
        let frames = frames as usize;
        let count = num_channels as usize;
        let mut position = context.position;
        for frame in 0..frames {
            let index = (position % context.period) as usize;
            let value = context.pattern[index];
            for channel in 0..count {
                let target = *channels.add(channel);
                if !target.is_null() {
                    *target.add(frame) = value;
                }
            }
            if index == 0 {
                let _ = context
                    .clicks
                    .push(Instant::now() + Duration::from_secs_f64(frame as f64 / context.rate));
            }
            position += 1;
        }
        context.position = position;
    }
}

unsafe extern "C" fn input_callback(
    ctx: *mut c_void,
    channels: *const *const f32,
    num_channels: i32,
    frames: i32,
    _sample_pos: i64,
) {
    if ctx.is_null() || channels.is_null() || frames <= 0 || num_channels <= 0 {
        return;
    }
    unsafe {
        let context = &mut *(ctx as *mut InputContext);
        let frames = frames as usize;
        let source = *channels;
        if source.is_null() {
            return;
        }
        let time = Instant::now() - Duration::from_secs_f64(frames as f64 / context.rate);
        let start = context.written;
        for frame in 0..frames {
            let sample = *source.add(frame);
            if context.samples.push(sample).is_err() {
                context.dropped += 1;
            }
        }
        let _ = context.blocks.push(BlockStamp { start, time });
        context.written = context.written.saturating_add(frames as u64);
    }
}

fn detect_onsets(samples: &[f32], threshold: f32) -> Vec<usize> {
    let mut onsets = Vec::new();
    let mut armed = true;
    let mut last = 0usize;
    let mut has_last = false;
    for (index, sample) in samples.iter().enumerate() {
        let magnitude = sample.abs();
        if armed && magnitude > threshold {
            if !has_last || index > last + RETRIGGER_FRAMES {
                onsets.push(index);
                last = index;
                has_last = true;
            }
            armed = false;
        } else if !armed && magnitude < threshold * 0.25 {
            armed = true;
        }
    }
    onsets
}

fn sample_time(blocks: &[BlockStamp], rate: f64, index: u64) -> Option<Instant> {
    let position = blocks.partition_point(|block| block.start <= index);
    if position == 0 {
        return None;
    }
    let block = &blocks[position - 1];
    Some(block.time + Duration::from_secs_f64((index - block.start) as f64 / rate))
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let index = ((values.len() - 1) as f64 * fraction).round() as usize;
    values[index.min(values.len() - 1)]
}

fn buffer_hint(device: &DeviceInfo) -> String {
    if device.buffer_sizes.is_empty() {
        "try block 0 for the device default".to_string()
    } else {
        let sizes: Vec<String> = device.buffer_sizes.iter().map(u32::to_string).collect();
        format!("supported buffers: {}", sizes.join(", "))
    }
}

fn api_name(api: AudioApi) -> &'static str {
    match api {
        AudioApi::WasapiShared => "WASAPI shared",
        AudioApi::WasapiLowLatency => "WASAPI low-latency",
        AudioApi::WasapiExclusive => "WASAPI exclusive",
        AudioApi::Asio => "ASIO",
        AudioApi::Other => "unknown API",
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_onsets, percentile};

    #[test]
    fn detects_each_burst_once() {
        let mut samples = vec![0.0f32; 6000];
        for sample in &mut samples[100..164] {
            *sample = 0.4;
        }
        for sample in &mut samples[3000..3064] {
            *sample = 0.4;
        }
        assert_eq!(detect_onsets(&samples, 0.1), vec![100, 3000]);
    }

    #[test]
    fn percentile_clamps_edges() {
        let values = [10.0, 20.0, 30.0];
        assert_eq!(percentile(&values, 0.0), 10.0);
        assert_eq!(percentile(&values, 0.5), 20.0);
        assert_eq!(percentile(&values, 1.0), 30.0);
    }
}
