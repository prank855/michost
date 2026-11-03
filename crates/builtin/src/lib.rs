//! Native built-in effects for MicHost.
//!
//! Built-ins behave like VST3 plugins from the graph's point of view: the app
//! selects one by `PluginSpec.path = "builtin:<id>"`, owns an
//! [`std::sync::Arc<ParamSet>`], and inserts a [`BuiltinProcessor`] into the
//! plugin chain. Parameters live in atomics so the audio thread never locks or
//! allocates; the UI writes values through the shared [`ParamSet`].
//!
//! Every effect lives in its own module and exposes the same contract:
//! `pub static SPECS: &[ParamSpec]` and a `Dsp` type implementing
//! [`BuiltinDsp`], created with `Dsp::new(sample_rate)` (the Dynamic Noise Gate
//! also receives the optional analysis tap). [`BuiltinKind::params`] and the
//! internal `make_dsp` dispatch to those modules.

use std::sync::Arc;

mod analysis;
mod kind;
mod param;
mod processor;

pub mod compressor;
pub mod delay;
pub mod dsp_util;
pub mod dynamic_gate;
pub mod echo;
pub mod eq;
pub mod gain;
pub mod gate;
pub mod limiter;
pub mod noise_removal;
pub mod pitch_shift;
pub mod reverb;

pub use analysis::{ANALYSIS_CAPACITY, AnalysisSnapshot, AudioAnalysis};
pub use kind::BuiltinKind;
pub use param::{ParamCurve, ParamSet, ParamSpec, ParamStyle};
pub use processor::BuiltinProcessor;

/// Maximum block size handed to a built-in processor per `process` call.
///
/// The graph never supplies more than this, but [`BuiltinProcessor`] also
/// chunks defensively so a larger block cannot overrun scratch buffers.
pub const MAX_BLOCK: usize = 16_384;

/// Allocation-free stereo DSP unit.
///
/// `left` and `right` always have the same length in `process`, are never
/// longer than [`MAX_BLOCK`], and are not aliased.
pub trait BuiltinDsp: Send {
    /// Processes one block in place. Must not allocate, lock, log or block.
    fn process(&mut self, left: &mut [f32], right: &mut [f32], params: &ParamSet);

    /// Clears all internal state (delay lines, filters, envelopes).
    fn reset(&mut self);
}

fn make_dsp(
    kind: BuiltinKind,
    sample_rate: f64,
    analysis: Option<Arc<AudioAnalysis>>,
) -> Box<dyn BuiltinDsp> {
    match kind {
        BuiltinKind::Gain => Box::new(gain::Dsp::new(sample_rate)),
        BuiltinKind::NoiseGate => Box::new(gate::Dsp::new(sample_rate)),
        BuiltinKind::Compressor => Box::new(compressor::Dsp::new(sample_rate)),
        BuiltinKind::Eq => Box::new(eq::Dsp::new(sample_rate)),
        BuiltinKind::Limiter => Box::new(limiter::Dsp::new(sample_rate)),
        BuiltinKind::NoiseRemoval => Box::new(noise_removal::Dsp::new(sample_rate)),
        BuiltinKind::Reverb => Box::new(reverb::Dsp::new(sample_rate)),
        BuiltinKind::Delay => Box::new(delay::Dsp::new(sample_rate)),
        BuiltinKind::Echo => Box::new(echo::Dsp::new(sample_rate)),
        BuiltinKind::PitchShift => Box::new(pitch_shift::Dsp::new(sample_rate)),
        BuiltinKind::DynamicGate => Box::new(dynamic_gate::Dsp::new(sample_rate, analysis)),
    }
}

#[cfg(test)]
mod tests;
