//! Pending device settings (the draft shown before Apply).

use michost_project::{DeviceSelection, ProjectSettings};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DeviceDraft {
    pub(crate) input: DeviceSelection,
    pub(crate) output: DeviceSelection,
    pub(crate) sidechain_input: DeviceSelection,
    pub(crate) sidechain_input_enabled: bool,
    pub(crate) sample_rate: f64,
    pub(crate) buffer_size: u32,
    pub(crate) output_buffer_size: u32,
    pub(crate) safety_buffer_ms: u32,
}

impl DeviceDraft {
    pub(crate) fn from_settings(settings: &ProjectSettings) -> Self {
        Self {
            input: settings.input.clone(),
            output: settings.output.clone(),
            sidechain_input: settings.sidechain_input.clone(),
            sidechain_input_enabled: settings.sidechain_input_enabled,
            sample_rate: settings.sample_rate,
            buffer_size: settings.buffer_size,
            output_buffer_size: settings.output_buffer_size,
            safety_buffer_ms: settings.safety_buffer_ms,
        }
    }

    pub(crate) fn matches(&self, settings: &ProjectSettings) -> bool {
        self.input == settings.input
            && self.output == settings.output
            && self.sidechain_input == settings.sidechain_input
            && self.sidechain_input_enabled == settings.sidechain_input_enabled
            && self.sample_rate == settings.sample_rate
            && self.buffer_size == settings.buffer_size
            && self.output_buffer_size == settings.output_buffer_size
            && self.safety_buffer_ms == settings.safety_buffer_ms
    }

    pub(crate) fn apply_to(&self, settings: &mut ProjectSettings) {
        settings.input = self.input.clone();
        settings.output = self.output.clone();
        settings.sidechain_input = self.sidechain_input.clone();
        settings.sidechain_input_enabled = self.sidechain_input_enabled;
        settings.sample_rate = self.sample_rate;
        settings.buffer_size = self.buffer_size;
        settings.output_buffer_size = self.output_buffer_size;
        settings.safety_buffer_ms = self.safety_buffer_ms;
    }
}
