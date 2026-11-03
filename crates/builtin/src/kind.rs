use crate::param::ParamSpec;
use crate::{
    compressor, delay, dynamic_gate, echo, eq, gain, gate, limiter, noise_removal, pitch_shift,
    reverb,
};

/// Identifies a native built-in effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinKind {
    Gain,
    NoiseGate,
    Compressor,
    Eq,
    Limiter,
    NoiseRemoval,
    Reverb,
    Delay,
    Echo,
    PitchShift,
    DynamicGate,
}

impl BuiltinKind {
    /// All built-ins in enum order; index equals the stable state index.
    pub const ALL: [BuiltinKind; 11] = [
        BuiltinKind::Gain,
        BuiltinKind::NoiseGate,
        BuiltinKind::Compressor,
        BuiltinKind::Eq,
        BuiltinKind::Limiter,
        BuiltinKind::NoiseRemoval,
        BuiltinKind::Reverb,
        BuiltinKind::Delay,
        BuiltinKind::Echo,
        BuiltinKind::PitchShift,
        BuiltinKind::DynamicGate,
    ];

    /// Stable short identifier used in paths and project files.
    pub fn id(self) -> &'static str {
        match self {
            BuiltinKind::Gain => "gain",
            BuiltinKind::NoiseGate => "gate",
            BuiltinKind::Compressor => "compressor",
            BuiltinKind::Eq => "eq",
            BuiltinKind::Limiter => "limiter",
            BuiltinKind::NoiseRemoval => "noise_removal",
            BuiltinKind::Reverb => "reverb",
            BuiltinKind::Delay => "delay",
            BuiltinKind::Echo => "echo",
            BuiltinKind::PitchShift => "pitch_shift",
            BuiltinKind::DynamicGate => "dynamic_gate",
        }
    }

    /// Human-readable display name.
    pub fn name(self) -> &'static str {
        match self {
            BuiltinKind::Gain => "Gain",
            BuiltinKind::NoiseGate => "Noise Gate",
            BuiltinKind::Compressor => "Compressor",
            BuiltinKind::Eq => "EQ",
            BuiltinKind::Limiter => "Limiter",
            BuiltinKind::NoiseRemoval => "Noise Removal",
            BuiltinKind::Reverb => "Reverb",
            BuiltinKind::Delay => "Delay",
            BuiltinKind::Echo => "Echo",
            BuiltinKind::PitchShift => "Pitch Shift",
            BuiltinKind::DynamicGate => "Dynamic Noise Gate",
        }
    }

    /// Vendor shown by the plugin browser.
    pub fn vendor(self) -> &'static str {
        "MicHost"
    }

    /// Browser category.
    pub fn category(self) -> &'static str {
        match self {
            BuiltinKind::Gain | BuiltinKind::NoiseRemoval => "Utility",
            BuiltinKind::NoiseGate
            | BuiltinKind::DynamicGate
            | BuiltinKind::Compressor
            | BuiltinKind::Limiter => "Dynamics",
            BuiltinKind::Eq => "Filter",
            BuiltinKind::Reverb | BuiltinKind::Delay | BuiltinKind::Echo => "Ambience",
            BuiltinKind::PitchShift => "Pitch",
        }
    }

    /// Plugin path used by the graph and project file.
    pub fn path(self) -> &'static str {
        match self {
            BuiltinKind::Gain => "builtin:gain",
            BuiltinKind::NoiseGate => "builtin:gate",
            BuiltinKind::Compressor => "builtin:compressor",
            BuiltinKind::Eq => "builtin:eq",
            BuiltinKind::Limiter => "builtin:limiter",
            BuiltinKind::NoiseRemoval => "builtin:noise_removal",
            BuiltinKind::Reverb => "builtin:reverb",
            BuiltinKind::Delay => "builtin:delay",
            BuiltinKind::Echo => "builtin:echo",
            BuiltinKind::PitchShift => "builtin:pitch_shift",
            BuiltinKind::DynamicGate => "builtin:dynamic_gate",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        BuiltinKind::ALL.into_iter().find(|kind| kind.id() == id)
    }

    /// Accepts `builtin:<id>` or a bare `<id>`.
    pub fn from_path(path: &str) -> Option<Self> {
        let id = path.strip_prefix("builtin:").unwrap_or(path);
        BuiltinKind::from_id(id)
    }

    /// Parameter layout for this effect, owned by its DSP module.
    pub fn params(self) -> &'static [ParamSpec] {
        match self {
            BuiltinKind::Gain => gain::SPECS,
            BuiltinKind::NoiseGate => gate::SPECS,
            BuiltinKind::Compressor => compressor::SPECS,
            BuiltinKind::Eq => eq::SPECS,
            BuiltinKind::Limiter => limiter::SPECS,
            BuiltinKind::NoiseRemoval => noise_removal::SPECS,
            BuiltinKind::Reverb => reverb::SPECS,
            BuiltinKind::Delay => delay::SPECS,
            BuiltinKind::Echo => echo::SPECS,
            BuiltinKind::PitchShift => pitch_shift::SPECS,
            BuiltinKind::DynamicGate => dynamic_gate::SPECS,
        }
    }

    /// Declared latency in samples. 10 ms for Limiter, 50 ms for Pitch Shift.
    pub fn latency_samples(self, sample_rate: f64) -> u32 {
        let seconds = match self {
            BuiltinKind::Limiter => 0.010,
            BuiltinKind::PitchShift => 0.050,
            BuiltinKind::NoiseRemoval => return 256,
            _ => return 0,
        };
        if !sample_rate.is_finite() || sample_rate <= 0.0 {
            return 0;
        }
        (sample_rate * seconds).round() as u32
    }

    /// State-file kind index; stable because `ALL` follows the enum order.
    pub(crate) fn index(self) -> u8 {
        match self {
            BuiltinKind::Gain => 0,
            BuiltinKind::NoiseGate => 1,
            BuiltinKind::Compressor => 2,
            BuiltinKind::Eq => 3,
            BuiltinKind::Limiter => 4,
            BuiltinKind::NoiseRemoval => 5,
            BuiltinKind::Reverb => 6,
            BuiltinKind::Delay => 7,
            BuiltinKind::Echo => 8,
            BuiltinKind::PitchShift => 9,
            BuiltinKind::DynamicGate => 10,
        }
    }

    pub(crate) fn from_index(index: u8) -> Option<Self> {
        BuiltinKind::ALL.get(usize::from(index)).copied()
    }
}
