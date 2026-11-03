use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const MAX_FRAMES: usize = 4096;
pub const MAX_PROCESS_CHANNELS: usize = 32;
pub const MAX_PLUGINS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PluginSpec {
    pub path: String,
    pub class_id: String,
    pub name: String,
    pub vendor: String,
    pub main_inputs: u16,
    pub main_outputs: u16,
    pub sidechain_inputs: u16,
}

impl PluginSpec {
    pub fn new(
        path: impl Into<String>,
        class_id: impl Into<String>,
        name: impl Into<String>,
        vendor: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            class_id: class_id.into(),
            name: name.into(),
            vendor: vendor.into(),
            main_inputs: 2,
            main_outputs: 2,
            sidechain_inputs: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SidechainSource {
    #[default]
    Off,
    MainMic,
    InputChannel(u16),
    SecondInput(u16),
    Silence,
}

impl SidechainSource {
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Off)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChainEntry {
    pub id: u64,
    pub plugin: PluginSpec,
    #[serde(default)]
    pub bypassed: bool,
    #[serde(default)]
    pub sidechain_source: SidechainSource,
    /// Per-plugin oversampling factor: 1 (off), 2 or 4.
    #[serde(default = "default_oversampling")]
    pub oversampling: u8,
}

pub fn default_oversampling() -> u8 {
    1
}

pub fn normalize_oversampling(factor: u8) -> u8 {
    match factor {
        2 => 2,
        4 => 4,
        _ => 1,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PlanNode {
    pub id: u64,
    pub main_inputs: u16,
    pub main_outputs: u16,
    pub sidechain_channels: u16,
    pub sidechain_source: SidechainSource,
    pub bypassed: bool,
    pub latency: u32,
}

impl PlanNode {
    pub fn total_inputs(&self) -> usize {
        self.main_inputs as usize + self.sidechain_channels as usize
    }

    pub fn total_outputs(&self) -> usize {
        self.main_outputs as usize
    }
}

#[derive(Clone, Debug)]
pub struct ChainPlan {
    pub nodes: Vec<PlanNode>,
    pub block_size: usize,
    pub sample_rate: f64,
    pub mono_source: Option<u8>,
}

impl ChainPlan {
    pub fn from_nodes(nodes: Vec<PlanNode>, block_size: usize) -> Self {
        Self {
            nodes,
            block_size,
            sample_rate: 0.0,
            mono_source: None,
        }
    }

    pub fn set_processing_options(&mut self, sample_rate: f64, mono_source: Option<u8>) {
        self.sample_rate = sample_rate;
        self.mono_source = mono_source;
    }

    pub fn total_latency(&self) -> u32 {
        self.nodes
            .iter()
            .filter(|node| !node.bypassed)
            .fold(0, |total, node| total.saturating_add(node.latency))
    }

    pub fn active_nodes(&self) -> usize {
        self.nodes.iter().filter(|node| !node.bypassed).count()
    }
}

pub trait PluginProcessor: Send {
    fn process(&mut self, channels: &mut [&mut [f32]], frames: usize);
}

#[derive(Debug, Default)]
pub struct PluginMeter {
    calls: AtomicU64,
    frames: AtomicU64,
    busy_nanos: AtomicU64,
    window_busy_nanos: AtomicU64,
    window_period_nanos: AtomicU64,
    cpu_fraction_bits: AtomicU32,
}

impl PluginMeter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, frames: usize, elapsed: Duration, sample_rate: f64) {
        if frames == 0 || sample_rate <= 0.0 || sample_rate.is_nan() {
            return;
        }
        let nanos = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        let period_nanos = ((frames as f64 / sample_rate) * 1.0e9) as u64;
        if period_nanos == 0 {
            return;
        }
        self.busy_nanos.fetch_add(nanos, Ordering::Relaxed);
        self.frames.fetch_add(frames as u64, Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);

        let window_busy = self.window_busy_nanos.fetch_add(nanos, Ordering::Relaxed) + nanos;
        let window_period = self
            .window_period_nanos
            .fetch_add(period_nanos, Ordering::Relaxed)
            + period_nanos;
        if window_period >= 1_000_000_000 {
            let fraction = (window_busy as f64 / window_period as f64).clamp(0.0, 64.0) as f32;
            self.cpu_fraction_bits
                .store(fraction.to_bits(), Ordering::Relaxed);
            self.window_busy_nanos.store(0, Ordering::Relaxed);
            self.window_period_nanos.store(0, Ordering::Relaxed);
        }
    }

    pub fn cpu_fraction(&self) -> f32 {
        f32::from_bits(self.cpu_fraction_bits.load(Ordering::Relaxed))
    }

    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    pub fn busy_nanos(&self) -> u64 {
        self.busy_nanos.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.calls.store(0, Ordering::Relaxed);
        self.frames.store(0, Ordering::Relaxed);
        self.busy_nanos.store(0, Ordering::Relaxed);
        self.window_busy_nanos.store(0, Ordering::Relaxed);
        self.window_period_nanos.store(0, Ordering::Relaxed);
        self.cpu_fraction_bits.store(0, Ordering::Relaxed);
    }
}

pub struct MeteredProcessor {
    inner: Box<dyn PluginProcessor>,
    meter: Arc<PluginMeter>,
    sample_rate: f64,
}

impl MeteredProcessor {
    pub fn new(inner: Box<dyn PluginProcessor>, meter: Arc<PluginMeter>, sample_rate: f64) -> Self {
        Self {
            inner,
            meter,
            sample_rate,
        }
    }
}

impl PluginProcessor for MeteredProcessor {
    fn process(&mut self, channels: &mut [&mut [f32]], frames: usize) {
        let started = Instant::now();
        self.inner.process(channels, frames);
        self.meter
            .record(frames, started.elapsed(), self.sample_rate);
    }
}

pub fn samples_to_milliseconds(samples: u32, sample_rate: f64) -> Option<f64> {
    if sample_rate <= 0.0 || sample_rate.is_nan() {
        return None;
    }
    Some(samples as f64 * 1000.0 / sample_rate)
}

#[derive(Debug)]
pub struct EngineError(String);

impl EngineError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for EngineError {}

type ProcessorSlot = Option<Box<dyn PluginProcessor>>;

pub struct ChainEngine {
    plan: ChainPlan,
    block_size: usize,
    processors: Vec<ProcessorSlot>,
    main_l: Vec<f32>,
    main_r: Vec<f32>,
    dry: Vec<f32>,
    process_buf: Vec<f32>,
}

impl ChainEngine {
    pub fn new(
        plan: ChainPlan,
        processors: Vec<(u64, Box<dyn PluginProcessor>)>,
        block_size: usize,
    ) -> Result<Self, EngineError> {
        if !(1..=MAX_FRAMES).contains(&block_size) {
            return Err(EngineError::new("block size out of range"));
        }
        if plan.nodes.len() > MAX_PLUGINS {
            return Err(EngineError::new("too many plugins"));
        }
        for (index, node) in plan.nodes.iter().enumerate() {
            let total_inputs = node.total_inputs();
            let total_outputs = node.total_outputs();
            let process_channels = total_inputs.max(total_outputs);
            if total_inputs > MAX_PROCESS_CHANNELS
                || total_outputs > MAX_PROCESS_CHANNELS
                || process_channels > MAX_PROCESS_CHANNELS
            {
                return Err(EngineError::new("channel count out of range"));
            }
            if plan.nodes[..index].iter().any(|other| other.id == node.id) {
                return Err(EngineError::new("duplicate node id"));
            }
        }

        let mut slots: Vec<ProcessorSlot> = (0..plan.nodes.len()).map(|_| None).collect();
        for (id, processor) in processors {
            if let Some(index) = plan.nodes.iter().position(|node| node.id == id) {
                slots[index] = Some(processor);
            }
        }
        for (node, slot) in plan.nodes.iter().zip(slots.iter()) {
            if !node.bypassed && node.total_outputs() > 0 && slot.is_none() {
                return Err(EngineError::new("missing processor"));
            }
        }

        Ok(Self {
            plan,
            block_size,
            processors: slots,
            main_l: vec![0.0; MAX_FRAMES],
            main_r: vec![0.0; MAX_FRAMES],
            dry: vec![0.0; MAX_PROCESS_CHANNELS * MAX_FRAMES],
            process_buf: vec![0.0; MAX_PROCESS_CHANNELS * MAX_FRAMES],
        })
    }

    pub fn process(
        &mut self,
        inputs: &[&[f32]],
        second_inputs: &[&[f32]],
        outputs: &mut [&mut [f32]],
        frames: usize,
    ) {
        if frames == 0 {
            return;
        }
        let n = frames.min(MAX_FRAMES);
        let Self {
            plan,
            processors,
            main_l,
            main_r,
            dry,
            process_buf,
            ..
        } = self;

        let raw_channels = inputs.len().min(MAX_PROCESS_CHANNELS);
        for (row, source) in inputs.iter().take(raw_channels).enumerate() {
            let start = row * MAX_FRAMES;
            let destination = &mut dry[start..start + n];
            let copied = source.len().min(n);
            destination[..copied].copy_from_slice(&source[..copied]);
            destination[copied..].fill(0.0);
        }

        let mono_source = plan.mono_source;
        for index in 0..n {
            let left = if raw_channels == 0 { 0.0 } else { dry[index] };
            let right = match raw_channels {
                0 => 0.0,
                1 => dry[index],
                _ => dry[MAX_FRAMES + index],
            };
            match mono_source {
                Some(1) => {
                    main_l[index] = right;
                    main_r[index] = right;
                }
                Some(0) => {
                    main_l[index] = left;
                    main_r[index] = left;
                }
                _ => {
                    main_l[index] = left;
                    main_r[index] = right;
                }
            }
        }

        for (node, processor) in plan.nodes.iter().zip(processors.iter_mut()) {
            if node.bypassed {
                continue;
            }
            let main_inputs = node.main_inputs as usize;
            let main_outputs = node.main_outputs as usize;
            let sidechain_channels = node.sidechain_channels as usize;
            let process_channels = node.total_inputs().max(node.total_outputs());

            for channel in process_buf.chunks_mut(MAX_FRAMES).take(process_channels) {
                channel[..n].fill(0.0);
            }

            if main_inputs > 0 {
                process_buf[..n].copy_from_slice(&main_l[..n]);
            }
            if main_inputs > 1 {
                let start = MAX_FRAMES;
                process_buf[start..start + n].copy_from_slice(&main_r[..n]);
            }

            if node.sidechain_source.is_active() && sidechain_channels > 0 {
                match node.sidechain_source {
                    SidechainSource::MainMic => {
                        if raw_channels > 0 {
                            for aux in 0..sidechain_channels {
                                let source_row = match aux {
                                    0 => 0,
                                    1 if raw_channels > 1 => 1,
                                    1 => 0,
                                    _ => continue,
                                };
                                let destination_start = (main_inputs + aux) * MAX_FRAMES;
                                let source_start = source_row * MAX_FRAMES;
                                process_buf[destination_start..destination_start + n]
                                    .copy_from_slice(&dry[source_start..source_start + n]);
                            }
                        }
                    }
                    SidechainSource::InputChannel(base) => {
                        let base = base as usize;
                        if base < raw_channels {
                            for aux in 0..sidechain_channels {
                                let source_row = if base + aux < raw_channels {
                                    base + aux
                                } else {
                                    base
                                };
                                let destination_start = (main_inputs + aux) * MAX_FRAMES;
                                let source_start = source_row * MAX_FRAMES;
                                process_buf[destination_start..destination_start + n]
                                    .copy_from_slice(&dry[source_start..source_start + n]);
                            }
                        }
                    }
                    SidechainSource::SecondInput(base) => {
                        let base = base as usize;
                        let second_channels = second_inputs.len();
                        if base < second_channels {
                            for aux in 0..sidechain_channels {
                                let source_row = if base + aux < second_channels {
                                    base + aux
                                } else {
                                    base
                                };
                                let destination_start = (main_inputs + aux) * MAX_FRAMES;
                                let destination =
                                    &mut process_buf[destination_start..destination_start + n];
                                let source = second_inputs[source_row];
                                let copied = source.len().min(n);
                                destination[..copied].copy_from_slice(&source[..copied]);
                                destination[copied..].fill(0.0);
                            }
                        }
                    }
                    SidechainSource::Silence | SidechainSource::Off => {}
                }
            }

            if let Some(processor) = processor.as_mut() {
                {
                    let mut channel_slices: [&mut [f32]; MAX_PROCESS_CHANNELS] =
                        std::array::from_fn(|_| &mut [] as &mut [f32]);
                    let mut chunks = process_buf.chunks_mut(MAX_FRAMES);
                    for slot in channel_slices.iter_mut().take(process_channels) {
                        if let Some(chunk) = chunks.next() {
                            *slot = &mut chunk[..n];
                        }
                    }
                    processor.process(&mut channel_slices[..process_channels], n);
                }
            }

            if main_outputs == 0 {
                main_l[..n].fill(0.0);
                main_r[..n].fill(0.0);
            } else {
                main_l[..n].copy_from_slice(&process_buf[..n]);
                if main_outputs > 1 {
                    main_r[..n].copy_from_slice(&process_buf[MAX_FRAMES..MAX_FRAMES + n]);
                } else {
                    main_r[..n].copy_from_slice(&process_buf[..n]);
                }
            }
        }

        if let Some(output) = outputs.first_mut() {
            let copied = output.len().min(n);
            output[..copied].copy_from_slice(&main_l[..copied]);
        }
        if let Some(output) = outputs.get_mut(1) {
            let copied = output.len().min(n);
            output[..copied].copy_from_slice(&main_r[..copied]);
        }
    }

    pub fn total_latency(&self) -> u32 {
        self.plan.total_latency()
    }

    pub fn estimated_memory_bytes(&self) -> usize {
        let buffer_samples = self.main_l.capacity()
            + self.main_r.capacity()
            + self.dry.capacity()
            + self.process_buf.capacity();
        let mut bytes = buffer_samples * size_of::<f32>();
        bytes += self.plan.nodes.capacity() * size_of::<PlanNode>();
        bytes += self.processors.capacity() * size_of::<ProcessorSlot>();
        for node in &self.plan.nodes {
            if !node.bypassed {
                bytes +=
                    node.total_inputs().max(node.total_outputs()) * MAX_FRAMES * size_of::<f32>();
            }
        }
        bytes
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }
}

pub fn drift_correction(fill: usize, target: usize, max_correction: f64) -> f64 {
    if target == 0 {
        return 1.0;
    }
    let max_correction = if max_correction.is_nan() {
        0.0
    } else {
        max_correction.clamp(0.0, 0.1)
    };
    let error = (fill as f64 - target as f64) / target as f64;
    let correction = 1.0 + 0.1 * error;
    correction.clamp(1.0 - max_correction, 1.0 + max_correction)
}

#[cfg(test)]
mod tests;
