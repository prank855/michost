use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::BuiltinKind;

/// How a parameter value maps between its real range and 0..=1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamCurve {
    /// Uniform mapping, `n = (value - min) / (max - min)`.
    Linear,
    /// Ratio mapping, `n = ln(value / min) / ln(max / min)`.
    ///
    /// Log specifications must have `min > 0`; otherwise the mapping falls
    /// back to [`ParamCurve::Linear`] so values stay finite.
    Log,
}

/// Editor control used for a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamStyle {
    Knob,
    Toggle,
    /// Discrete choices rendered as a segmented selector. The parameter value
    /// is the zero-based choice index (clamped to `0..len-1`).
    Choice(&'static [&'static str]),
}

/// Static description of one parameter.
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    /// Stable machine key used by state and UI code.
    pub key: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Display unit (`"dB"`, `"%"`, `"ms"`, `"s"`, `""`, ...). Units print
    /// after a single space.
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub curve: ParamCurve,
    /// Digits after the decimal point in the UI.
    pub decimals: u8,
    pub style: ParamStyle,
    /// Editor section (group) this control belongs to.
    pub section: &'static str,
    /// Show this control only while another parameter has a value:
    /// `Some((key, value))`.
    pub visible_when: Option<(&'static str, f32)>,
}

impl ParamSpec {
    fn span(&self) -> f32 {
        self.max - self.min
    }

    fn uses_log(&self) -> bool {
        matches!(self.curve, ParamCurve::Log) && self.min > 0.0 && self.max > self.min
    }

    /// Real value to normalized 0..=1 position.
    pub fn normalized(&self, value: f32) -> f32 {
        let value = self.clamp(value);
        if self.span() <= 0.0 {
            return 0.0;
        }
        let n = if self.uses_log() {
            (value / self.min).ln() / (self.max / self.min).ln()
        } else {
            (value - self.min) / self.span()
        };
        if n.is_finite() {
            n.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Normalized 0..=1 position to real value.
    pub fn from_normalized(&self, normalized: f32) -> f32 {
        let n = if normalized.is_finite() {
            normalized.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if self.span() <= 0.0 {
            return self.min;
        }
        let value = if self.uses_log() {
            self.min * (self.max / self.min).powf(n)
        } else {
            self.min + n * self.span()
        };
        self.clamp(value)
    }

    /// Clamps a real value. Non-finite input becomes the default.
    pub fn clamp(&self, value: f32) -> f32 {
        if !value.is_finite() {
            return self.default;
        }
        value.max(self.min).min(self.max)
    }

    /// Formats a real value for the editor. Toggles print `On` / `Off`.
    pub fn format_value(&self, value: f32) -> String {
        match self.style {
            ParamStyle::Toggle => {
                return if self.normalized(value) >= 0.5 {
                    "On".to_string()
                } else {
                    "Off".to_string()
                };
            }
            ParamStyle::Choice(choices) => {
                if choices.is_empty() {
                    return String::new();
                }
                let index = self
                    .clamp(value)
                    .round()
                    .clamp(0.0, (choices.len() - 1) as f32) as usize;
                return choices[index].to_string();
            }
            ParamStyle::Knob => {}
        }
        let value = self.clamp(value);
        let number = format!("{:.*}", self.decimals as usize, value);
        if self.unit.is_empty() {
            number
        } else {
            format!("{number} {}", self.unit)
        }
    }
}

const STATE_MAGIC: [u8; 4] = *b"MHB1";
const STATE_HEADER: usize = 12;

/// Shared, lock-free parameter values for one built-in instance.
///
/// The UI thread writes; the audio thread reads. All values are stored as
/// `f32` bits in atomics, so neither side blocks or allocates.
pub struct ParamSet {
    kind: BuiltinKind,
    specs: &'static [ParamSpec],
    values: Vec<AtomicU32>,
    edited: AtomicBool,
}

impl ParamSet {
    pub fn new(kind: BuiltinKind) -> Arc<Self> {
        let specs = kind.params();
        let values = specs
            .iter()
            .map(|spec| AtomicU32::new(spec.default.to_bits()))
            .collect();
        Arc::new(Self {
            kind,
            specs,
            values,
            edited: AtomicBool::new(false),
        })
    }

    pub fn kind(&self) -> BuiltinKind {
        self.kind
    }

    pub fn specs(&self) -> &'static [ParamSpec] {
        self.specs
    }

    /// Current value; `0.0` for an out-of-range index.
    pub fn get(&self, index: usize) -> f32 {
        self.values
            .get(index)
            .map(|slot| f32::from_bits(slot.load(Ordering::Relaxed)))
            .unwrap_or(0.0)
    }

    /// Clamps and stores a value, then marks the state edited.
    pub fn set(&self, index: usize, value: f32) {
        let Some(slot) = self.values.get(index) else {
            return;
        };
        let spec = &self.specs[index];
        slot.store(spec.clamp(value).to_bits(), Ordering::Relaxed);
        self.edited.store(true, Ordering::Relaxed);
    }

    pub fn normalized(&self, index: usize) -> f32 {
        match self.specs.get(index) {
            Some(spec) => spec.normalized(self.get(index)),
            None => 0.0,
        }
    }

    pub fn set_normalized(&self, index: usize, normalized: f32) {
        let Some(spec) = self.specs.get(index) else {
            return;
        };
        self.set(index, spec.from_normalized(normalized));
    }

    pub fn default_normalized(&self, index: usize) -> f32 {
        match self.specs.get(index) {
            Some(spec) => spec.normalized(spec.default),
            None => 0.0,
        }
    }

    /// Restores every default without marking the state edited.
    pub fn set_all_defaults(&self) {
        for (slot, spec) in self.values.iter().zip(self.specs) {
            slot.store(spec.default.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.specs.iter().position(|spec| spec.key == key)
    }

    /// Value for `key`; `0.0` when the key does not exist.
    pub fn get_key(&self, key: &str) -> f32 {
        match self.index_of(key) {
            Some(index) => self.get(index),
            None => 0.0,
        }
    }

    /// Returns whether a change happened since the last call and clears it.
    pub fn take_edited(&self) -> bool {
        self.edited.swap(false, Ordering::Relaxed)
    }

    /// Versioned binary state: `MHB1`, u32 LE count, kind index, 3 reserved
    /// zero bytes, then `count` f32 LE values.
    pub fn encode(&self) -> Vec<u8> {
        let count = self.values.len();
        let mut bytes = Vec::with_capacity(STATE_HEADER + count * 4);
        bytes.extend_from_slice(&STATE_MAGIC);
        bytes.extend_from_slice(&(count as u32).to_le_bytes());
        bytes.push(self.kind.index());
        bytes.extend_from_slice(&[0, 0, 0]);
        for slot in &self.values {
            bytes.extend_from_slice(&slot.load(Ordering::Relaxed).to_le_bytes());
        }
        bytes
    }

    /// Restores state written by [`ParamSet::encode`].
    ///
    /// Rejects wrong magic, version, kind and truncated input. States with
    /// fewer or more values than the current spec list are accepted: leading
    /// values are restored, missing trailing parameters keep their defaults,
    /// and extra trailing values are ignored. Legacy 16-value EQ and 7-value
    /// gate states are migrated to their current layouts. Values are clamped;
    /// non-finite values use their defaults.
    pub fn decode(&self, bytes: &[u8]) -> bool {
        if bytes.len() < STATE_HEADER || bytes[0..4] != STATE_MAGIC {
            return false;
        }
        let count = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        // States may have fewer or more values than the current spec list:
        // older files restore their leading values and newer/removed text
        // parameters are ignored. The byte length still has to match `count`.
        let Some(expected) = count
            .checked_mul(4)
            .and_then(|n| n.checked_add(STATE_HEADER))
        else {
            return false;
        };
        if bytes.len() != expected {
            return false;
        }
        let Some(kind) = BuiltinKind::from_index(bytes[8]) else {
            return false;
        };
        if kind != self.kind {
            return false;
        }

        let raw: Vec<f32> = bytes[STATE_HEADER..]
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        let raw = match self.kind {
            BuiltinKind::Eq if count == crate::eq::LEGACY_VALUE_COUNT => {
                crate::eq::migrate_legacy(&raw)
            }
            BuiltinKind::NoiseGate if count == crate::gate::LEGACY_VALUE_COUNT => {
                crate::gate::migrate_legacy(&raw)
            }
            _ => raw,
        };

        let values: Vec<u32> = self
            .specs
            .iter()
            .zip(raw)
            .map(|(spec, value)| spec.clamp(value).to_bits())
            .collect();
        for (slot, bits) in self.values.iter().zip(values) {
            slot.store(bits, Ordering::Relaxed);
        }
        true
    }
}
