use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use michost_graph::{ChainEngine, MAX_FRAMES, drift_correction};

use michost_host_abi::{
    AudioApi, DeviceStream, StreamCallbacks, StreamConfig, StreamDirection, StreamHost,
};
use michost_host_juce::JuceBackend;
use michost_vmic::VirtualMicSink;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateEventW, CreateWaitableTimerExW, SetEvent,
    SetWaitableTimer, TIMER_ALL_ACCESS, WaitForSingleObject,
};

pub const MAX_INPUT_CHANNELS: usize = 8;
pub const MAX_OUTPUT_CHANNELS: usize = 2;
const RESAMPLE_MAX: usize = MAX_FRAMES * 10;
const ENGINE_DRIFT: f64 = 0.005;
const VMIC_PUMP_INTERVAL: Duration = Duration::from_millis(1);

/// Counters for the virtual microphone pump, surfaced next to the device stats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BridgeStats {
    pub vmic_overruns: u64,
    pub vmic_underruns: u64,
    pub vmic_buffered_frames: u64,
}

/// Output ring target used by the pump: 1.5-2 driver packets.
pub fn virtual_mic_ring_target(packet_frames: usize, ring_frames: usize) -> usize {
    if packet_frames == 0 || ring_frames == 0 {
        return 0;
    }
    packet_frames
        .saturating_mul(3)
        .div_euclid(2)
        .min(ring_frames)
        .max(packet_frames.min(ring_frames))
}

/// Output ring target for the graph when the driver sink is the output.
pub fn virtual_mic_output_target(block_size: usize, packet_frames: usize) -> usize {
    block_size.max(packet_frames).max(128)
}

struct WakeEvent {
    handle: HANDLE,
}

unsafe impl Send for WakeEvent {}
unsafe impl Sync for WakeEvent {}

impl WakeEvent {
    fn new() -> Result<Self, String> {
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if handle.is_null() {
            Err("could not create the graph wake event".to_string())
        } else {
            Ok(Self { handle })
        }
    }

    fn set(&self) {
        unsafe {
            SetEvent(self.handle);
        }
    }

    fn wait(&self, milliseconds: u32) {
        unsafe {
            WaitForSingleObject(self.handle, milliseconds);
        }
    }
}

impl Drop for WakeEvent {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

pub struct InputContext {
    producers: Vec<rtrb::Producer<f32>>,
    wake: Arc<WakeEvent>,
    dropouts: Arc<AtomicU64>,
    monitor: Option<Box<OutputContext>>,
}

pub struct InputSource {
    context: Box<InputContext>,
    consumers: Vec<rtrb::Consumer<f32>>,
    wake: Arc<WakeEvent>,
    pub monitor_producers: Vec<rtrb::Producer<f32>>,
    pub monitor_capacity: usize,
}

pub struct OutputContext {
    consumers: Vec<rtrb::Consumer<f32>>,
    underruns: Arc<AtomicU64>,
}

pub struct OutputSink {
    context: Box<OutputContext>,
    producers: Vec<rtrb::Producer<f32>>,
    capacity: usize,
}

pub struct SecondInput {
    pub stream: DeviceStream,
    pub source: InputSource,
    pub rate: f64,
    pub capacity: usize,
    pub buffer: u32,
}

const MONITOR_CAPACITY: usize = 32_768;

enum MonitorUpdate {
    Install {
        producers: Vec<rtrb::Producer<f32>>,
        rate: f64,
        target: usize,
        capacity: usize,
    },
    SetEnabled(bool),
}

struct MonitorRetired {
    _producers: Vec<rtrb::Producer<f32>>,
}

enum MeasureUpdate {
    Enable,
    Disable,
}

pub fn input_source(channels: usize, capacity: usize) -> Result<InputSource, String> {
    if channels == 0 || channels > MAX_INPUT_CHANNELS {
        return Err("input stream channel count is out of range".to_string());
    }
    if capacity < 256 {
        return Err("input ring capacity is too small".to_string());
    }
    let wake = Arc::new(WakeEvent::new()?);
    let dropouts = Arc::new(AtomicU64::new(0));
    let mut producers = Vec::with_capacity(channels);
    let mut consumers = Vec::with_capacity(channels);
    for _ in 0..channels {
        let (producer, consumer) = rtrb::RingBuffer::new(capacity);
        producers.push(producer);
        consumers.push(consumer);
    }
    Ok(InputSource {
        context: Box::new(InputContext {
            producers,
            wake: wake.clone(),
            dropouts,
            monitor: None,
        }),
        consumers,
        wake,
        monitor_producers: Vec::new(),
        monitor_capacity: 0,
    })
}

/// Creates an input source that also owns the output ring for a duplex ASIO
/// stream (input plus headphone/monitor outputs on the same driver).
pub fn input_source_with_monitor(
    channels: usize,
    capacity: usize,
    monitor_channels: usize,
    monitor_capacity: usize,
) -> Result<InputSource, String> {
    let mut source = input_source(channels, capacity)?;
    let sink = output_sink(monitor_channels, monitor_capacity)?;
    let OutputSink {
        context,
        producers,
        capacity,
    } = sink;
    source.context.monitor = Some(context);
    source.monitor_producers = producers;
    source.monitor_capacity = capacity;
    Ok(source)
}

pub fn input_callbacks(source: &mut InputSource) -> StreamCallbacks {
    StreamCallbacks {
        ctx: (&mut *source.context as *mut InputContext).cast::<c_void>(),
        on_input: Some(input_callback),
        on_output: if source.context.monitor.is_some() {
            Some(duplex_output_callback)
        } else {
            None
        },
        on_error: Some(error_callback),
    }
}

pub fn output_sink(channels: usize, capacity: usize) -> Result<OutputSink, String> {
    if channels == 0 || channels > MAX_OUTPUT_CHANNELS {
        return Err("output stream channel count is out of range".to_string());
    }
    if capacity < 256 {
        return Err("output ring capacity is too small".to_string());
    }
    let underruns = Arc::new(AtomicU64::new(0));
    let mut producers = Vec::with_capacity(channels);
    let mut consumers = Vec::with_capacity(channels);
    for _ in 0..channels {
        let (producer, consumer) = rtrb::RingBuffer::new(capacity);
        producers.push(producer);
        consumers.push(consumer);
    }
    Ok(OutputSink {
        context: Box::new(OutputContext {
            consumers,
            underruns,
        }),
        producers,
        capacity,
    })
}

pub fn output_callbacks(sink: &mut OutputSink) -> StreamCallbacks {
    StreamCallbacks {
        ctx: (&mut *sink.context as *mut OutputContext).cast::<c_void>(),
        on_input: None,
        on_output: Some(output_callback),
        on_error: Some(error_callback),
    }
}

unsafe extern "C" fn input_callback(
    ctx: *mut c_void,
    channels: *const *const f32,
    num_channels: i32,
    frames: i32,
    _sample_pos: i64,
) {
    if ctx.is_null() || channels.is_null() || frames <= 0 {
        return;
    }
    let context = unsafe { &mut *(ctx as *mut InputContext) };
    let frames = frames as usize;
    let available = num_channels.max(0) as usize;

    if context
        .producers
        .iter()
        .any(|producer| producer.slots() < frames)
    {
        context.dropouts.fetch_add(1, Ordering::Relaxed);
        context.wake.set();
        return;
    }

    for (index, producer) in context.producers.iter_mut().enumerate() {
        let data = unsafe { *channels.add(index) };
        if index >= available || data.is_null() {
            for _ in 0..frames {
                let _ = producer.push(0.0);
            }
        } else {
            let samples = unsafe { std::slice::from_raw_parts(data, frames) };
            for &sample in samples {
                let _ = producer.push(sample);
            }
        }
    }
    context.wake.set();
}

unsafe extern "C" fn output_callback(
    ctx: *mut c_void,
    channels: *const *mut f32,
    num_channels: i32,
    frames: i32,
    _sample_pos: i64,
) {
    if ctx.is_null() {
        return;
    }
    let context = unsafe { &mut *(ctx as *mut OutputContext) };
    render_output(context, channels, num_channels, frames);
}

unsafe extern "C" fn duplex_output_callback(
    ctx: *mut c_void,
    channels: *const *mut f32,
    num_channels: i32,
    frames: i32,
    _sample_pos: i64,
) {
    if ctx.is_null() {
        return;
    }
    let context = unsafe { &mut *(ctx as *mut InputContext) };
    if let Some(monitor) = context.monitor.as_mut() {
        render_output(monitor, channels, num_channels, frames);
    }
}

fn render_output(
    context: &mut OutputContext,
    channels: *const *mut f32,
    num_channels: i32,
    frames: i32,
) {
    if channels.is_null() || frames <= 0 {
        return;
    }
    let frames = frames as usize;
    let available = num_channels.max(0) as usize;
    let mut underrun = false;

    for (index, consumer) in context.consumers.iter_mut().enumerate() {
        let data = unsafe { *channels.add(index) };
        if index >= available || data.is_null() {
            for _ in 0..frames {
                let _ = consumer.pop();
            }
            continue;
        }
        for frame in 0..frames {
            match consumer.pop() {
                Ok(sample) => unsafe { *data.add(frame) = sample },
                Err(_) => {
                    unsafe { *data.add(frame) = 0.0 };
                    underrun = true;
                }
            }
        }
    }
    if underrun {
        context.underruns.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe extern "C" fn error_callback(
    _ctx: *mut c_void,
    _status: i32,
    _message: *const std::ffi::c_char,
) {
}

/// BS.1770 K-weighting: high-shelf pre-filter followed by the RLB high-pass.
#[derive(Clone, Copy)]
struct KFilter {
    pre_b: [f32; 3],
    pre_a: [f32; 2],
    pre_z: [f32; 2],
    hp_b: [f32; 3],
    hp_a: [f32; 2],
    hp_z: [f32; 2],
}

impl KFilter {
    fn new(sample_rate: f64) -> Self {
        let rate = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            48_000.0
        };
        // BS.1770-4 pre-filter (high shelf).
        let f0 = 1_681.974_450_955_533_f64;
        let gain_db = 3.999_843_853_973_347_f64;
        let q = 0.707_175_236_955_419_6_f64;
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let vh = 10.0_f64.powf(gain_db / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let a0 = 1.0 + k / q + k * k;
        let pre_b = [
            ((vh + vb * k / q + k * k) / a0) as f32,
            (2.0 * (k * k - vh) / a0) as f32,
            ((vh - vb * k / q + k * k) / a0) as f32,
        ];
        let pre_a = [
            (2.0 * (k * k - 1.0) / a0) as f32,
            ((1.0 - k / q + k * k) / a0) as f32,
        ];

        // BS.1770 RLB high-pass.
        let f0 = 38.135_470_876_024_44_f64;
        let q = 0.500_327_037_323_877_3_f64;
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let a0 = 1.0 + k / q + k * k;
        let hp_b = [(1.0 / a0) as f32, (-2.0 / a0) as f32, (1.0 / a0) as f32];
        let hp_a = [
            (2.0 * (k * k - 1.0) / a0) as f32,
            ((1.0 - k / q + k * k) / a0) as f32,
        ];

        Self {
            pre_b,
            pre_a,
            pre_z: [0.0; 2],
            hp_b,
            hp_a,
            hp_z: [0.0; 2],
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let pre = self.pre_b[0] * input + self.pre_z[0];
        self.pre_z[0] = self.pre_b[1] * input - self.pre_a[0] * pre + self.pre_z[1];
        self.pre_z[1] = self.pre_b[2] * input - self.pre_a[1] * pre;

        let output = self.hp_b[0] * pre + self.hp_z[0];
        self.hp_z[0] = self.hp_b[1] * pre - self.hp_a[0] * output + self.hp_z[1];
        self.hp_z[1] = self.hp_b[2] * pre - self.hp_a[1] * output;

        if !output.is_finite() {
            self.pre_z = [0.0; 2];
            self.hp_z = [0.0; 2];
            return 0.0;
        }
        output
    }
}

/// Momentary (400 ms) K-weighted loudness per channel pair, BS.1770.
struct LoudnessMeter {
    left: KFilter,
    right: KFilter,
    channels: usize,
    mean_square: f32,
    sample_rate: f64,
}

impl LoudnessMeter {
    fn new(sample_rate: f64, channels: usize) -> Self {
        Self {
            left: KFilter::new(sample_rate),
            right: KFilter::new(sample_rate),
            channels: channels.max(1),
            mean_square: 0.0,
            sample_rate: if sample_rate.is_finite() && sample_rate > 0.0 {
                sample_rate
            } else {
                48_000.0
            },
        }
    }

    fn process(&mut self, left: &[f32], right: &[f32]) -> f32 {
        let frames = if self.channels > 1 {
            left.len().min(right.len())
        } else {
            left.len()
        };
        if frames == 0 {
            return self.lufs();
        }
        let mut sum = 0.0f64;
        for index in 0..frames {
            let l = self.left.process(left[index]);
            sum += f64::from(l * l);
            if self.channels > 1 {
                let r = self.right.process(right[index]);
                sum += f64::from(r * r);
            }
        }
        let mean_square = sum / (frames as f64 * self.channels.min(2) as f64);
        let coefficient = (1.0 - (-(frames as f64) / (0.4 * self.sample_rate)).exp()) as f32;
        self.mean_square += coefficient * (mean_square as f32 - self.mean_square);
        self.lufs()
    }

    fn lufs(&self) -> f32 {
        if self.mean_square > 1.0e-12 {
            -0.691 + 10.0 * self.mean_square.log10()
        } else {
            -120.0
        }
    }
}

pub struct StreamBridge {
    host: Arc<JuceBackend>,
    input_stream: Option<DeviceStream>,
    second_input_stream: Option<DeviceStream>,
    output_stream: Option<DeviceStream>,
    input_context: Option<Box<InputContext>>,
    second_input_context: Option<Box<InputContext>>,
    output_context: Option<Box<OutputContext>>,
    engine_tx: rtrb::Producer<ChainEngine>,
    retired: rtrb::Consumer<ChainEngine>,
    graph: Option<JoinHandle<()>>,
    vmic_pump: Option<JoinHandle<()>>,
    vmic_overruns: Arc<AtomicU64>,
    vmic_underruns: Arc<AtomicU64>,
    vmic_fill: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    fill: Arc<AtomicUsize>,
    frames: Arc<AtomicU64>,
    cpu: Arc<AtomicU32>,
    second_starved: Arc<AtomicU64>,
    input_lufs: Arc<AtomicU32>,
    output_lufs: Arc<AtomicU32>,
    monitor_stream: Option<DeviceStream>,
    monitor_context: Option<Box<OutputContext>>,
    monitor_underruns: Option<Arc<AtomicU64>>,
    monitor_active: bool,
    duplex_monitor: bool,
    monitor_gain: Arc<AtomicU32>,
    monitor_tx: rtrb::Producer<MonitorUpdate>,
    monitor_retired: rtrb::Consumer<MonitorRetired>,
    measure_tx: rtrb::Producer<MeasureUpdate>,
    measure_clicks: rtrb::Consumer<Instant>,
}

impl StreamBridge {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        host: Arc<JuceBackend>,
        input_stream: DeviceStream,
        output_stream: Option<DeviceStream>,
        vmic: Option<VirtualMicSink>,
        input_source: InputSource,
        output_sink: OutputSink,
        engine: ChainEngine,
        engine_rate: f64,
        output_rate: f64,
        block_size: usize,
        output_target: usize,
        second_input: Option<SecondInput>,
        monitor_trim_db: f32,
    ) -> Result<Self, String> {
        if block_size == 0 || block_size > MAX_FRAMES {
            return Err("the graph block size is out of range".to_string());
        }
        if engine_rate <= 0.0 || output_rate <= 0.0 {
            return Err("device sample rates must be positive".to_string());
        }
        if output_target < block_size {
            return Err("the output ring target is too small".to_string());
        }
        if second_input
            .as_ref()
            .is_some_and(|second| second.rate <= 0.0 || second.capacity < 256)
        {
            return Err("the sidechain input rate or ring size is invalid".to_string());
        }

        let InputSource {
            context: input_context,
            consumers,
            wake,
            monitor_producers,
            monitor_capacity,
        } = input_source;
        let input_dropouts = input_context.dropouts.clone();
        let duplex_monitor = !monitor_producers.is_empty();
        let duplex_monitor_capacity = monitor_capacity;
        let monitor_gain = Arc::new(AtomicU32::new(db_to_gain(monitor_trim_db).to_bits()));
        let OutputSink {
            context: mut output_context,
            producers,
            capacity: output_capacity,
            ..
        } = output_sink;
        let output_consumers = if vmic.is_some() {
            std::mem::take(&mut output_context.consumers)
        } else {
            Vec::new()
        };

        let (
            second_stream,
            second_context,
            second_consumers,
            second_rate,
            second_capacity,
            second_buffer,
        ) = match second_input {
            Some(SecondInput {
                stream,
                source,
                rate,
                capacity,
                buffer,
            }) => {
                let InputSource {
                    context, consumers, ..
                } = source;
                (
                    Some(stream),
                    Some(context),
                    consumers,
                    rate,
                    capacity,
                    buffer,
                )
            }
            None => (None, None, Vec::new(), 0.0, 0, 0),
        };
        let second_starved = Arc::new(AtomicU64::new(0));

        let input_channels = consumers.len();
        let output_channels = producers.len();
        let stop = Arc::new(AtomicBool::new(false));
        let fill = Arc::new(AtomicUsize::new(0));
        let frames = Arc::new(AtomicU64::new(0));
        let cpu = Arc::new(AtomicU32::new(0));
        let input_lufs = Arc::new(AtomicU32::new((-120.0f32).to_bits()));
        let output_lufs = Arc::new(AtomicU32::new((-120.0f32).to_bits()));
        let vmic_overruns = Arc::new(AtomicU64::new(0));
        let vmic_underruns = Arc::new(AtomicU64::new(0));
        let vmic_fill = Arc::new(AtomicU64::new(0));

        let (engine_tx, engine_rx) = rtrb::RingBuffer::new(4);
        let (retired_tx, retired) = rtrb::RingBuffer::new(8);
        let (monitor_tx, monitor_rx) = rtrb::RingBuffer::new(4);
        let (monitor_retired_tx, monitor_retired) = rtrb::RingBuffer::new(4);
        let (measure_tx, measure_rx) = rtrb::RingBuffer::new(4);
        let (measure_clicks_tx, measure_clicks) = rtrb::RingBuffer::new(64);

        if let Err(error) = host.start_stream(&input_stream) {
            host.close_stream(input_stream);
            if let Some(stream) = output_stream {
                host.close_stream(stream);
            }
            if let Some(stream) = second_stream {
                host.close_stream(stream);
            }
            return Err(error.to_string());
        }
        if let Some(stream) = second_stream {
            if let Err(error) = host.start_stream(&stream) {
                let _ = host.stop_stream(&input_stream);
                host.close_stream(input_stream);
                if let Some(output) = output_stream {
                    host.close_stream(output);
                }
                host.close_stream(stream);
                return Err(format!("starting the sidechain input: {error}"));
            }
        }

        let second_channels = second_consumers.len();
        let second_target = if second_channels > 0 && second_rate > 0.0 {
            let engine_block = (block_size as f64 * second_rate / engine_rate).ceil() as usize;
            let cap = (second_capacity / 2).max(1);
            ((second_buffer as usize) * 2 + engine_block.saturating_mul(2))
                .max(engine_block.saturating_mul(3))
                .max(1024)
                .min(cap)
        } else {
            0
        };
        if second_target > 0 {
            let deadline = Instant::now() + Duration::from_millis(200);
            while Instant::now() < deadline
                && second_consumers
                    .iter()
                    .map(rtrb::Consumer::slots)
                    .min()
                    .unwrap_or(0)
                    < second_target
            {
                thread::sleep(Duration::from_millis(2));
            }
        }
        let state = GraphState {
            engine,
            engine_rx,
            retired_tx,
            monitor_rx,
            monitor_retired_tx,
            duplex_monitor,
            duplex_monitor_producers: monitor_producers,
            duplex_monitor_capacity,
            monitor_gain: monitor_gain.clone(),
            measure_rx,
            measure_clicks_tx,
            consumers,
            producers,
            second_consumers,
            wake,
            stop: stop.clone(),
            fill: fill.clone(),
            frames: frames.clone(),
            cpu: cpu.clone(),
            input_dropouts,
            second_starved: second_starved.clone(),
            input_meter: LoudnessMeter::new(engine_rate, input_channels),
            output_meter: LoudnessMeter::new(engine_rate, output_channels),
            input_lufs: input_lufs.clone(),
            output_lufs: output_lufs.clone(),
            block_size,
            engine_rate,
            output_rate,
            second_rate,
            second_target,
            output_capacity,
            output_target: output_target.min(output_capacity / 2),
            input_channels,
            output_channels,
            second_channels,
        };

        let graph = match thread::Builder::new()
            .name("michost-graph".to_string())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || graph_loop(state))
        {
            Ok(graph) => graph,
            Err(error) => {
                let _ = host.stop_stream(&input_stream);
                host.close_stream(input_stream);
                if let Some(stream) = output_stream {
                    host.close_stream(stream);
                }
                if let Some(stream) = second_stream {
                    let _ = host.stop_stream(&stream);
                    host.close_stream(stream);
                }
                return Err(format!("starting the graph thread: {error}"));
            }
        };

        let vmic_pump = match vmic {
            Some(mut sink) => {
                let format = sink.format();
                let target = virtual_mic_ring_target(
                    format.packet_frames as usize,
                    sink.ring_frames() as usize,
                );
                sink.prefill(target);
                let pump_stop = stop.clone();
                let overruns = vmic_overruns.clone();
                let underruns = vmic_underruns.clone();
                let pump_fill = vmic_fill.clone();
                match thread::Builder::new()
                    .name("michost-vmic".to_string())
                    .spawn(move || {
                        vmic_loop(
                            sink,
                            output_consumers,
                            pump_stop,
                            overruns,
                            underruns,
                            pump_fill,
                        )
                    }) {
                    Ok(pump) => Some(pump),
                    Err(error) => {
                        stop.store(true, Ordering::Release);
                        input_context.wake.set();
                        let _ = graph.join();
                        let _ = host.stop_stream(&input_stream);
                        if let Some(stream) = second_stream.as_ref() {
                            let _ = host.stop_stream(stream);
                        }
                        host.close_stream(input_stream);
                        if let Some(stream) = output_stream {
                            host.close_stream(stream);
                        }
                        if let Some(stream) = second_stream {
                            host.close_stream(stream);
                        }
                        return Err(format!("starting the virtual microphone pump: {error}"));
                    }
                }
            }
            None => None,
        };

        if vmic_pump.is_none() {
            let prefill = output_target
                .saturating_add(block_size)
                .min(output_capacity / 2);
            let deadline = Instant::now() + Duration::from_millis(500);
            while fill.load(Ordering::Relaxed) < prefill && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }
            let Some(stream) = output_stream else {
                stop.store(true, Ordering::Release);
                input_context.wake.set();
                let _ = graph.join();
                let _ = host.stop_stream(&input_stream);
                if let Some(second) = second_stream.as_ref() {
                    let _ = host.stop_stream(second);
                }
                host.close_stream(input_stream);
                if let Some(second) = second_stream {
                    host.close_stream(second);
                }
                return Err("the physical output stream was not opened".to_string());
            };
            if let Err(error) = host.start_stream(&stream) {
                let _ = host.stop_stream(&input_stream);
                if let Some(second) = second_stream.as_ref() {
                    let _ = host.stop_stream(second);
                }
                stop.store(true, Ordering::Release);
                input_context.wake.set();
                let _ = graph.join();
                host.close_stream(input_stream);
                host.close_stream(stream);
                if let Some(second) = second_stream {
                    host.close_stream(second);
                }
                return Err(error.to_string());
            }
        }

        Ok(Self {
            host,
            input_stream: Some(input_stream),
            second_input_stream: second_stream,
            output_stream,
            input_context: Some(input_context),
            second_input_context: second_context,
            output_context: Some(output_context),
            engine_tx,
            retired,
            graph: Some(graph),
            vmic_pump,
            vmic_overruns,
            vmic_underruns,
            vmic_fill,
            stop,
            fill,
            frames,
            cpu,
            second_starved,
            input_lufs,
            output_lufs,
            monitor_stream: None,
            monitor_context: None,
            monitor_underruns: None,
            monitor_active: false,
            duplex_monitor,
            monitor_gain,
            monitor_tx,
            monitor_retired,
            measure_tx,
            measure_clicks,
        })
    }

    pub fn begin_chain_measurement(&mut self) -> Result<(), String> {
        while self.measure_clicks.pop().is_ok() {}
        self.measure_tx
            .push(MeasureUpdate::Enable)
            .map_err(|_| "the measurement queue is full".to_string())
    }

    pub fn end_chain_measurement(&mut self) -> Vec<Instant> {
        let _ = self.measure_tx.push(MeasureUpdate::Disable);
        let mut clicks = Vec::new();
        while let Ok(click) = self.measure_clicks.pop() {
            clicks.push(click);
        }
        clicks
    }

    pub fn input_lufs(&self) -> f32 {
        f32::from_bits(self.input_lufs.load(Ordering::Relaxed))
    }

    pub fn output_lufs(&self) -> f32 {
        f32::from_bits(self.output_lufs.load(Ordering::Relaxed))
    }

    pub fn replace_engine(&mut self, engine: ChainEngine) -> Result<(), String> {
        self.engine_tx
            .push(engine)
            .map_err(|_| "the engine update queue is full".to_string())
    }

    pub fn collect_retired(&mut self) {
        while let Ok(engine) = self.retired.pop() {
            drop(engine);
        }
    }

    pub fn warm_monitor(
        &mut self,
        device: &str,
        api: AudioApi,
        channels: u32,
        channel_mask: u64,
    ) -> Result<(), String> {
        if self.duplex_monitor {
            return Ok(());
        }
        if self.monitor_stream.is_some() {
            return Ok(());
        }
        if device.trim().is_empty() {
            return Err("no monitor device is available".to_string());
        }
        let channels = channels.clamp(1, MAX_OUTPUT_CHANNELS as u32);
        let mut sink = output_sink(channels as usize, MONITOR_CAPACITY)?;
        let callbacks = output_callbacks(&mut sink);
        let OutputSink {
            context,
            producers,
            capacity,
        } = sink;
        let config = StreamConfig {
            device: device.to_string(),
            sample_rate: 0.0,
            block_size: 0,
            channels,
            api,
            direction: StreamDirection::Output,
            channel_mask,
            output_channels: 0,
            output_channel_mask: 0,
        };
        let stream = match self.host.open_stream(&config, callbacks) {
            Ok(stream) => stream,
            Err(error) => return Err(error.to_string()),
        };
        let latency = match self.host.stream_latency(&stream) {
            Ok(latency) => latency,
            Err(error) => {
                self.host.close_stream(stream);
                return Err(error.to_string());
            }
        };
        if latency.sample_rate == 0 {
            self.host.close_stream(stream);
            return Err("the monitor device reported no sample rate".to_string());
        }
        let rate = latency.sample_rate as f64;
        let target = (latency.buffer as usize)
            .saturating_mul(2)
            .clamp(1024, (capacity / 2).max(1024));
        let underruns = context.underruns.clone();
        if self
            .monitor_tx
            .push(MonitorUpdate::Install {
                producers,
                rate,
                target,
                capacity,
            })
            .is_err()
        {
            self.host.close_stream(stream);
            return Err("the monitor update queue is full".to_string());
        }
        if let Err(error) = self.host.start_stream(&stream) {
            let _ = self.monitor_tx.push(MonitorUpdate::SetEnabled(false));
            self.host.close_stream(stream);
            return Err(error.to_string());
        }
        self.monitor_stream = Some(stream);
        self.monitor_context = Some(context);
        self.monitor_underruns = Some(underruns);
        Ok(())
    }

    pub fn start_monitor(
        &mut self,
        device: &str,
        api: AudioApi,
        channels: u32,
        channel_mask: u64,
    ) -> Result<(), String> {
        if !self.duplex_monitor && self.monitor_stream.is_none() {
            self.warm_monitor(device, api, channels, channel_mask)?;
        }
        if self.monitor_active {
            return Ok(());
        }
        if self
            .monitor_tx
            .push(MonitorUpdate::SetEnabled(true))
            .is_err()
        {
            return Err("the monitor update queue is full".to_string());
        }
        self.monitor_active = true;
        Ok(())
    }

    pub fn stop_monitor(&mut self) {
        if !self.monitor_active {
            return;
        }
        let _ = self.monitor_tx.push(MonitorUpdate::SetEnabled(false));
        self.monitor_active = false;
    }

    /// Stops and closes the monitor stream. Use this when the selected monitor
    /// device/API changes; `stop_monitor` keeps the warmed stream for an
    /// instant re-enable on the same device.
    pub fn close_monitor(&mut self) {
        self.monitor_active = false;
        if self.duplex_monitor {
            let _ = self.monitor_tx.push(MonitorUpdate::SetEnabled(false));
            return;
        }
        if let Some(stream) = self.monitor_stream.take() {
            let _ = self.monitor_tx.push(MonitorUpdate::SetEnabled(false));
            let _ = self.host.stop_stream(&stream);
            self.host.close_stream(stream);
        }
        self.monitor_context = None;
        self.monitor_underruns = None;
    }

    pub fn monitor_is_duplex(&self) -> bool {
        self.duplex_monitor
    }

    pub fn set_monitor_trim(&self, db: f32) {
        self.monitor_gain
            .store(db_to_gain(db).to_bits(), Ordering::Relaxed);
    }

    pub fn poll_monitor(&mut self) {
        while let Ok(retired) = self.monitor_retired.pop() {
            drop(retired);
        }
    }

    pub fn monitor_underruns(&self) -> u64 {
        self.monitor_underruns
            .as_ref()
            .map(|underruns| underruns.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn frames_processed(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    pub fn cpu_usage(&self) -> f32 {
        f32::from_bits(self.cpu.load(Ordering::Relaxed))
    }

    pub fn input_dropouts(&self) -> u64 {
        self.input_context
            .as_ref()
            .map(|context| context.dropouts.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn second_input_dropouts(&self) -> u64 {
        self.second_input_context
            .as_ref()
            .map(|context| context.dropouts.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn second_input_starved(&self) -> u64 {
        self.second_starved.load(Ordering::Relaxed)
    }

    pub fn output_underruns(&self) -> u64 {
        self.output_context
            .as_ref()
            .map(|context| context.underruns.load(Ordering::Relaxed))
            .unwrap_or(0)
            .saturating_add(self.vmic_underruns.load(Ordering::Relaxed))
    }

    pub fn output_fill(&self) -> usize {
        self.fill.load(Ordering::Relaxed)
    }

    pub fn stats(&self) -> BridgeStats {
        BridgeStats {
            vmic_overruns: self.vmic_overruns.load(Ordering::Relaxed),
            vmic_underruns: self.vmic_underruns.load(Ordering::Relaxed),
            vmic_buffered_frames: self.vmic_fill.load(Ordering::Relaxed),
        }
    }

    pub fn control_panel(&self, input: bool) -> Result<(), String> {
        let stream = if input {
            self.input_stream.as_ref()
        } else {
            self.output_stream.as_ref()
        }
        .ok_or_else(|| "the device stream is not open".to_string())?;
        self.host
            .stream_control_panel(stream)
            .map_err(|error| error.to_string())
    }

    pub fn stop(&mut self) {
        self.monitor_active = false;
        if let Some(stream) = self.monitor_stream.take() {
            let _ = self.monitor_tx.push(MonitorUpdate::SetEnabled(false));
            let _ = self.host.stop_stream(&stream);
            self.host.close_stream(stream);
        }
        if let Some(stream) = self.output_stream.take() {
            let _ = self.host.stop_stream(&stream);
            self.host.close_stream(stream);
        }
        if let Some(stream) = self.second_input_stream.take() {
            let _ = self.host.stop_stream(&stream);
            self.host.close_stream(stream);
        }
        if let Some(stream) = self.input_stream.take() {
            let _ = self.host.stop_stream(&stream);
            self.host.close_stream(stream);
        }
        self.stop.store(true, Ordering::Release);
        if let Some(context) = self.input_context.as_ref() {
            context.wake.set();
        }
        if let Some(context) = self.second_input_context.as_ref() {
            context.wake.set();
        }
        if let Some(graph) = self.graph.take() {
            let _ = graph.join();
        }
        if let Some(pump) = self.vmic_pump.take() {
            let _ = pump.join();
        }
        self.input_context = None;
        self.second_input_context = None;
        self.output_context = None;
        self.monitor_context = None;
        self.monitor_underruns = None;
        self.collect_retired();
        self.poll_monitor();
    }
}

impl Drop for StreamBridge {
    fn drop(&mut self) {
        self.stop();
    }
}

struct GraphState {
    engine: ChainEngine,
    engine_rx: rtrb::Consumer<ChainEngine>,
    retired_tx: rtrb::Producer<ChainEngine>,
    monitor_rx: rtrb::Consumer<MonitorUpdate>,
    monitor_retired_tx: rtrb::Producer<MonitorRetired>,
    duplex_monitor: bool,
    duplex_monitor_producers: Vec<rtrb::Producer<f32>>,
    duplex_monitor_capacity: usize,
    monitor_gain: Arc<AtomicU32>,
    measure_rx: rtrb::Consumer<MeasureUpdate>,
    measure_clicks_tx: rtrb::Producer<Instant>,
    consumers: Vec<rtrb::Consumer<f32>>,
    producers: Vec<rtrb::Producer<f32>>,
    second_consumers: Vec<rtrb::Consumer<f32>>,
    wake: Arc<WakeEvent>,
    stop: Arc<AtomicBool>,
    fill: Arc<AtomicUsize>,
    frames: Arc<AtomicU64>,
    cpu: Arc<AtomicU32>,
    input_dropouts: Arc<AtomicU64>,
    second_starved: Arc<AtomicU64>,
    input_meter: LoudnessMeter,
    output_meter: LoudnessMeter,
    input_lufs: Arc<AtomicU32>,
    output_lufs: Arc<AtomicU32>,
    block_size: usize,
    engine_rate: f64,
    output_rate: f64,
    second_rate: f64,
    second_target: usize,
    output_capacity: usize,
    output_target: usize,
    input_channels: usize,
    output_channels: usize,
    second_channels: usize,
}

fn db_to_gain(db: f32) -> f32 {
    if db.is_finite() {
        10.0f32.powf(db / 20.0).clamp(0.0, 4.0)
    } else {
        1.0
    }
}

fn graph_loop(state: GraphState) {
    let _priority = AudioThreadPriority::acquire();
    let GraphState {
        mut engine,
        mut engine_rx,
        mut retired_tx,
        mut monitor_rx,
        mut monitor_retired_tx,
        duplex_monitor,
        duplex_monitor_producers,
        duplex_monitor_capacity,
        monitor_gain,
        mut measure_rx,
        mut measure_clicks_tx,
        mut consumers,
        mut producers,
        mut second_consumers,
        wake,
        stop,
        fill,
        frames,
        cpu,
        input_dropouts,
        second_starved,
        mut input_meter,
        mut output_meter,
        input_lufs,
        output_lufs,
        block_size,
        engine_rate,
        output_rate,
        second_rate,
        second_target,
        output_capacity,
        output_target,
        input_channels,
        output_channels,
        second_channels,
    } = state;

    let mut input_buffers: Vec<Vec<f32>> = (0..input_channels)
        .map(|_| vec![0.0f32; block_size])
        .collect();
    let mut second_buffers: Vec<Vec<f32>> = (0..second_channels)
        .map(|_| vec![0.0f32; block_size])
        .collect();
    let mut second_resamplers: Vec<BlockResampler> = (0..second_channels)
        .map(|_| BlockResampler::new(second_rate / engine_rate))
        .collect();
    let mut output_l = vec![0.0f32; block_size];
    let mut output_r = vec![0.0f32; block_size];
    let mut resampled_l = vec![0.0f32; RESAMPLE_MAX];
    let mut resampled_r = vec![0.0f32; RESAMPLE_MAX];
    let mut resampler_l = BlockResampler::new(engine_rate / output_rate);
    let mut resampler_r = BlockResampler::new(engine_rate / output_rate);
    let duplex = duplex_monitor;
    let mut monitor_producers: Option<Vec<rtrb::Producer<f32>>> =
        (!duplex_monitor_producers.is_empty()).then_some(duplex_monitor_producers);
    let mut monitor_enabled = false;
    let mut monitor_rate = if duplex { engine_rate } else { 0.0 };
    let mut monitor_target = if duplex {
        (block_size.saturating_mul(2)).clamp(1024, (duplex_monitor_capacity / 2).max(1024))
    } else {
        0
    };
    let mut monitor_capacity = duplex_monitor_capacity;
    let mut monitor_resamplers = [BlockResampler::new(1.0), BlockResampler::new(1.0)];
    let mut monitor_l = vec![0.0f32; RESAMPLE_MAX];
    let mut monitor_r = vec![0.0f32; RESAMPLE_MAX];
    let measure_period = ((engine_rate * 0.5).round() as u64).max(64);
    let mut measure_pattern = vec![0.0f32; 64];
    for (index, sample) in measure_pattern.iter_mut().enumerate() {
        *sample =
            (0.4 * (2.0 * std::f64::consts::PI * 800.0 * index as f64 / engine_rate).cos()) as f32;
    }
    let mut measure_enabled = false;
    let mut measure_position = 0u64;
    let block_period = block_size as f64 / engine_rate;
    let mut cpu_window_time = 0.0f64;
    let mut cpu_window_period = 0.0f64;

    while !stop.load(Ordering::Acquire) {
        while let Ok(next) = engine_rx.pop() {
            let previous = std::mem::replace(&mut engine, next);
            if let Err(rtrb::PushError::Full(previous)) = retired_tx.push(previous) {
                std::mem::forget(previous);
            }
        }
        while let Ok(update) = monitor_rx.pop() {
            match update {
                MonitorUpdate::Install {
                    producers,
                    rate,
                    target,
                    capacity,
                } => {
                    if let Some(previous) = monitor_producers.replace(producers) {
                        retire_monitor(&mut monitor_retired_tx, previous);
                    }
                    monitor_rate = rate;
                    monitor_target = target.max(1);
                    monitor_capacity = capacity;
                    let step = engine_rate / rate;
                    for resampler in &mut monitor_resamplers {
                        *resampler = BlockResampler::new(step);
                    }
                    monitor_enabled = false;
                }
                MonitorUpdate::SetEnabled(enabled) => monitor_enabled = enabled,
            }
        }
        while let Ok(update) = measure_rx.pop() {
            match update {
                MeasureUpdate::Enable => {
                    measure_enabled = true;
                    measure_position = 0;
                }
                MeasureUpdate::Disable => measure_enabled = false,
            }
        }

        let available = consumers
            .iter()
            .map(rtrb::Consumer::slots)
            .min()
            .unwrap_or(0);
        if available < block_size {
            wake.wait(5);
            continue;
        }
        if available > block_size.saturating_mul(8) {
            let drop_frames = available - block_size;
            for consumer in &mut consumers {
                for _ in 0..drop_frames {
                    let _ = consumer.pop();
                }
            }
            input_dropouts.fetch_add(1, Ordering::Relaxed);
        }

        for (buffer, consumer) in input_buffers.iter_mut().zip(consumers.iter_mut()) {
            for sample in buffer.iter_mut() {
                *sample = consumer.pop().unwrap_or(0.0);
            }
        }
        let input_lufs_value = input_meter.process(
            &input_buffers[0],
            input_buffers.get(1).map(Vec::as_slice).unwrap_or(&[]),
        );
        input_lufs.store(input_lufs_value.to_bits(), Ordering::Relaxed);

        if measure_enabled {
            for frame in 0..block_size {
                let index = (measure_position % measure_period) as usize;
                if index < measure_pattern.len() {
                    let value = measure_pattern[index];
                    for buffer in input_buffers.iter_mut() {
                        buffer[frame] += value;
                    }
                }
                if index == 0 {
                    let _ = measure_clicks_tx
                        .push(Instant::now() + Duration::from_secs_f64(frame as f64 / engine_rate));
                }
                measure_position += 1;
            }
        }

        let mut input_refs: [&[f32]; MAX_INPUT_CHANNELS] = [&[]; MAX_INPUT_CHANNELS];
        for (slot, buffer) in input_refs.iter_mut().zip(input_buffers.iter()) {
            *slot = buffer.as_slice();
        }
        let mut second_refs: [&[f32]; MAX_INPUT_CHANNELS] = [&[]; MAX_INPUT_CHANNELS];
        if second_channels > 0 {
            let available = second_consumers
                .iter()
                .map(rtrb::Consumer::slots)
                .min()
                .unwrap_or(0);
            let correction = drift_correction(available, second_target, ENGINE_DRIFT);
            let step = (second_rate / engine_rate * correction).clamp(0.1, 10.0);
            let needed = ((block_size as f64 * step).ceil() as usize).saturating_add(2);
            if available < needed {
                for buffer in second_buffers.iter_mut() {
                    buffer.fill(0.0);
                }
                second_starved.fetch_add(block_size as u64, Ordering::Relaxed);
            } else {
                for ((resampler, buffer), consumer) in second_resamplers
                    .iter_mut()
                    .zip(second_buffers.iter_mut())
                    .zip(second_consumers.iter_mut())
                {
                    resampler.set_step(step);
                    let mut remaining = available;
                    let (produced, _) = resampler.process_stream(buffer.as_mut_slice(), || {
                        if remaining == 0 {
                            return None;
                        }
                        match consumer.pop() {
                            Ok(sample) => {
                                remaining -= 1;
                                Some(sample)
                            }
                            Err(_) => None,
                        }
                    });
                    if produced < block_size {
                        buffer[produced..].fill(0.0);
                        second_starved.fetch_add((block_size - produced) as u64, Ordering::Relaxed);
                    }
                }
            }
            for (slot, buffer) in second_refs.iter_mut().zip(second_buffers.iter()) {
                *slot = buffer.as_slice();
            }
        }
        let mut output_refs: [&mut [f32]; MAX_OUTPUT_CHANNELS] =
            [output_l.as_mut_slice(), output_r.as_mut_slice()];

        let started = Instant::now();
        engine.process(
            &input_refs[..input_channels],
            &second_refs[..second_channels],
            &mut output_refs[..output_channels.min(MAX_OUTPUT_CHANNELS)],
            block_size,
        );
        let output_lufs_value = output_meter.process(&output_l, &output_r);
        output_lufs.store(output_lufs_value.to_bits(), Ordering::Relaxed);

        if block_period > 0.0 {
            cpu_window_time += started.elapsed().as_secs_f64();
            cpu_window_period += block_period;
            if cpu_window_period >= 1.0 {
                let usage = (cpu_window_time / cpu_window_period * 100.0) as f32;
                cpu.store(usage.to_bits(), Ordering::Relaxed);
                cpu_window_time = 0.0;
                cpu_window_period = 0.0;
            }
        }

        let free = producers
            .iter()
            .map(rtrb::Producer::slots)
            .min()
            .unwrap_or(0);
        let current_fill = output_capacity.saturating_sub(free);
        let correction = drift_correction(current_fill, output_target, ENGINE_DRIFT);
        let step = (engine_rate / output_rate * correction).clamp(0.1, 10.0);
        resampler_l.set_step(step);
        resampler_r.set_step(step);

        let produced_l = resampler_l.process_block(&output_l, &mut resampled_l);
        let produced_r = if output_channels > 1 {
            resampler_r.process_block(&output_r, &mut resampled_r)
        } else {
            0
        };
        let produced = if output_channels > 1 {
            produced_l.min(produced_r)
        } else {
            produced_l
        };
        let push = produced.min(free);
        for sample in &resampled_l[..push] {
            let _ = producers[0].push(*sample);
        }
        if output_channels > 1 {
            for sample in &resampled_r[..push] {
                let _ = producers[1].push(*sample);
            }
        }
        let free_after = producers
            .iter()
            .map(rtrb::Producer::slots)
            .min()
            .unwrap_or(0);
        if monitor_enabled
            && let Some(monitor) = monitor_producers.as_mut()
            && monitor_rate > 0.0
            && !monitor.is_empty()
        {
            let channels = monitor.len().min(MAX_OUTPUT_CHANNELS);
            let free = monitor.iter().map(rtrb::Producer::slots).min().unwrap_or(0);
            let monitor_fill = monitor_capacity.saturating_sub(free);
            let correction = drift_correction(monitor_fill, monitor_target, ENGINE_DRIFT);
            let step = (engine_rate / monitor_rate * correction).clamp(0.1, 10.0);
            for resampler in &mut monitor_resamplers[..channels] {
                resampler.set_step(step);
            }
            let produced_l = monitor_resamplers[0].process_block(&output_l, &mut monitor_l);
            let produced = if channels > 1 {
                let produced_r = monitor_resamplers[1].process_block(&output_r, &mut monitor_r);
                produced_l.min(produced_r)
            } else {
                produced_l
            };
            let push = produced.min(free);
            let gain = f32::from_bits(monitor_gain.load(Ordering::Relaxed));
            for sample in &monitor_l[..push] {
                let _ = monitor[0].push(*sample * gain);
            }
            if channels > 1 {
                for sample in &monitor_r[..push] {
                    let _ = monitor[1].push(*sample * gain);
                }
            }
        }
        fill.store(
            output_capacity.saturating_sub(free_after),
            Ordering::Relaxed,
        );
        frames.fetch_add(block_size as u64, Ordering::Relaxed);
    }
}

fn vmic_loop(
    mut sink: VirtualMicSink,
    mut consumers: Vec<rtrb::Consumer<f32>>,
    stop: Arc<AtomicBool>,
    overruns: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
    fill: Arc<AtomicU64>,
) {
    let format = sink.format();
    let channels = format.channels as usize;
    if channels == 0 || consumers.len() != channels {
        return;
    }
    let ring_frames = sink.ring_frames() as usize;
    let mut scratch = vec![0.0f32; ring_frames.saturating_mul(channels).max(channels)];
    let timer = PumpTimer::new();

    while !stop.load(Ordering::Acquire) {
        let stats = sink.stats();
        let live_packet = if stats.packet_frames > 0 {
            stats.packet_frames as usize
        } else {
            format.packet_frames as usize
        };
        let target = virtual_mic_ring_target(live_packet, stats.ring_frames as usize);
        let before = (stats.buffered_frames as usize).min(stats.ring_frames as usize);
        if before < target {
            let available = consumers
                .iter()
                .map(rtrb::Consumer::slots)
                .min()
                .unwrap_or(0);
            let wanted = (target - before)
                .min(available)
                .min(scratch.len() / channels);
            if wanted > 0 {
                for frame in 0..wanted {
                    for (channel, consumer) in consumers.iter_mut().enumerate() {
                        scratch[frame * channels + channel] = consumer.pop().unwrap_or(0.0);
                    }
                }
                sink.push(&scratch[..wanted * channels]);
            }
        }
        let after = sink.stats();
        fill.store(
            (after.buffered_frames as usize).min(after.ring_frames as usize) as u64,
            Ordering::Relaxed,
        );
        overruns.store(after.overruns, Ordering::Relaxed);
        underruns.store(after.underruns, Ordering::Relaxed);
        match &timer {
            Some(timer) => timer.wait(),
            None => thread::sleep(VMIC_PUMP_INTERVAL),
        }
    }
}

struct PumpTimer {
    handle: HANDLE,
}

unsafe impl Send for PumpTimer {}

impl PumpTimer {
    fn new() -> Option<Self> {
        // SAFETY: no attributes/name are required for a private timer.
        let handle = unsafe {
            CreateWaitableTimerExW(
                std::ptr::null(),
                std::ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        (!handle.is_null()).then_some(Self { handle })
    }

    fn wait(&self) {
        // SAFETY: the handle is owned by this timer and the due time is a
        // relative 1 ms interval (100 ns units).
        let due = -10_000i64;
        unsafe {
            SetWaitableTimer(self.handle, &due, 0, None, std::ptr::null(), 0);
            WaitForSingleObject(self.handle, 2);
        }
    }
}

impl Drop for PumpTimer {
    fn drop(&mut self) {
        // SAFETY: the handle was created by CreateWaitableTimerExW.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

fn retire_monitor(
    sender: &mut rtrb::Producer<MonitorRetired>,
    producers: Vec<rtrb::Producer<f32>>,
) {
    if let Err(rtrb::PushError::Full(retired)) = sender.push(MonitorRetired {
        _producers: producers,
    }) {
        std::mem::forget(retired);
    }
}

struct BlockResampler {
    step: f64,
    frac: f64,
    last: f32,
    current: f32,
    next: f32,
    next_ready: bool,
    started: bool,
}

impl BlockResampler {
    fn new(step: f64) -> Self {
        Self {
            step: sanitize_step(step),
            frac: 0.0,
            last: 0.0,
            current: 0.0,
            next: 0.0,
            next_ready: false,
            started: false,
        }
    }

    fn set_step(&mut self, step: f64) {
        self.step = sanitize_step(step);
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32]) -> usize {
        if input.is_empty() || output.is_empty() {
            return 0;
        }
        let length = input.len() as f64;
        let last_index = input.len() - 1;
        let mut position = self.frac;
        let mut produced = 0;
        while produced < output.len() {
            if position < 0.0 {
                let alpha = (position + 1.0).clamp(0.0, 1.0) as f32;
                output[produced] = self.last + (input[0] - self.last) * alpha;
                produced += 1;
                position += self.step;
                continue;
            }
            if position > last_index as f64 {
                break;
            }
            if position == last_index as f64 {
                output[produced] = input[last_index];
                produced += 1;
                position += self.step;
                continue;
            }
            let index = position.floor() as usize;
            let fraction = (position - index as f64) as f32;
            let current = input[index];
            let next = input[index + 1];
            output[produced] = current + (next - current) * fraction;
            produced += 1;
            position += self.step;
        }
        self.frac = (position - length).max(-1.0);
        self.last = input[last_index];
        produced
    }

    fn process_stream(
        &mut self,
        output: &mut [f32],
        mut input: impl FnMut() -> Option<f32>,
    ) -> (usize, usize) {
        let mut produced = 0usize;
        let mut consumed = 0usize;
        while produced < output.len() {
            if !self.started {
                match input() {
                    Some(sample) => {
                        self.current = sample;
                        self.started = true;
                        consumed += 1;
                    }
                    None => break,
                }
            }
            while self.frac >= 1.0 {
                if !self.next_ready {
                    match input() {
                        Some(sample) => {
                            self.next = sample;
                            self.next_ready = true;
                            consumed += 1;
                        }
                        None => return (produced, consumed),
                    }
                }
                self.current = self.next;
                self.next_ready = false;
                self.frac -= 1.0;
            }
            if self.frac > 0.0 {
                if !self.next_ready {
                    match input() {
                        Some(sample) => {
                            self.next = sample;
                            self.next_ready = true;
                            consumed += 1;
                        }
                        None => break,
                    }
                }
                output[produced] = self.current + (self.next - self.current) * self.frac as f32;
            } else {
                output[produced] = self.current;
            }
            produced += 1;
            self.frac += self.step;
        }
        (produced, consumed)
    }
}

fn sanitize_step(step: f64) -> f64 {
    if step.is_nan() {
        1.0
    } else {
        step.clamp(0.1, 10.0)
    }
}

#[cfg(windows)]
struct AudioThreadPriority(*mut c_void);

#[cfg(windows)]
impl AudioThreadPriority {
    fn acquire() -> Option<Self> {
        #[link(name = "avrt")]
        unsafe extern "system" {
            fn AvSetMmThreadCharacteristicsW(name: *const u16, index: *mut u32) -> *mut c_void;
        }
        let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut index = 0u32;
        let handle = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut index) };
        (!handle.is_null()).then_some(Self(handle))
    }
}

#[cfg(windows)]
impl Drop for AudioThreadPriority {
    fn drop(&mut self) {
        #[link(name = "avrt")]
        unsafe extern "system" {
            fn AvRevertMmThreadCharacteristics(handle: *mut c_void) -> i32;
        }
        unsafe {
            AvRevertMmThreadCharacteristics(self.0);
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod trim_tests {
    use super::db_to_gain;

    #[test]
    fn monitor_trim_maps_db_to_linear_gain() {
        assert!((db_to_gain(0.0) - 1.0).abs() < 1.0e-6);
        assert!((db_to_gain(-6.020_6) - 0.5).abs() < 1.0e-3);
        assert!(db_to_gain(f32::NAN) == 1.0);
    }
}
