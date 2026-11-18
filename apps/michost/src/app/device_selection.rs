//! Device selection, channel masks and automatic API resolution.
//!
//! Pure decision logic: these helpers never open a stream and never touch the
//! audio engine, so they can be unit-tested against synthetic device lists.

use michost_graph::SidechainSource;
use michost_host_abi::{AudioApi, DeviceInfo};
use michost_project::{DeviceSelection, ProjectSettings};

use crate::device_cache::DeviceCache;

pub(crate) fn api_label(api: AudioApi) -> &'static str {
    match api {
        AudioApi::WasapiShared => "WASAPI shared",
        AudioApi::WasapiLowLatency => "WASAPI low latency",
        AudioApi::WasapiExclusive => "WASAPI exclusive",
        AudioApi::Asio => "ASIO",
        AudioApi::Other => "Other",
    }
}

pub(crate) fn selection_for(device: &DeviceInfo, channels: u32) -> DeviceSelection {
    DeviceSelection {
        api: device.api,
        name: device.name.clone(),
        id: device.id.clone(),
        channels: channels.max(1),
        channel_mask: 0,
    }
}

/// Text after the first `" ("` of an endpoint name, used to recognise the same
/// hardware after Windows renames its role prefix (Main/Headphone).
pub(crate) fn device_model_suffix(name: &str) -> Option<&str> {
    let (_, suffix) = name.split_once(" (")?;
    let suffix = suffix.strip_suffix(')').unwrap_or(suffix).trim_end();
    if suffix.is_empty() {
        None
    } else {
        Some(suffix)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SelectionMatch {
    Exact,
    Remapped(DeviceInfo),
    Missing,
}

/// Decides whether a stored selection still resolves against a device list.
/// An exact `(api, name)` match wins. Only when the configured name is gone
/// does it fall back to a unique device with the same model suffix, preferring
/// the same API; two candidates are ambiguous and are never guessed.
pub(crate) fn resolve_selection(
    devices: &[DeviceInfo],
    selection: &DeviceSelection,
    input: bool,
) -> SelectionMatch {
    if selection.name.is_empty() {
        return SelectionMatch::Missing;
    }
    if devices.iter().any(|device| {
        device.name == selection.name
            && device.api == selection.api
            && device_channels(device, input) > 0
    }) {
        return SelectionMatch::Exact;
    }
    if devices.iter().any(|device| device.name == selection.name) {
        // The name still exists (only its API or direction differs), so this
        // is not a rename; leave it to the automatic API handling.
        return SelectionMatch::Missing;
    }
    let Some(suffix) = device_model_suffix(&selection.name) else {
        return SelectionMatch::Missing;
    };
    let candidates: Vec<&DeviceInfo> = devices
        .iter()
        .filter(|device| {
            device_channels(device, input) > 0 && device_model_suffix(&device.name) == Some(suffix)
        })
        .collect();
    let same_api: Vec<&DeviceInfo> = candidates
        .iter()
        .copied()
        .filter(|device| device.api == selection.api)
        .collect();
    let matched = if same_api.len() == 1 {
        Some(same_api[0])
    } else if candidates.len() == 1 {
        Some(candidates[0])
    } else {
        None
    };
    match matched {
        Some(device) => SelectionMatch::Remapped(device.clone()),
        None => SelectionMatch::Missing,
    }
}

/// Moves a selection onto a freshly enumerated device, keeping the channel
/// mask when it still fits and clamping the channel count otherwise.
pub(crate) fn remap_selection(selection: &mut DeviceSelection, device: &DeviceInfo, input: bool) {
    let available = device_channels(device, input).max(1);
    let max = if input { 8 } else { 2 };
    selection.api = device.api;
    selection.name = device.name.clone();
    selection.id = device.id.clone();
    if selection.channel_mask != 0 {
        let mask = retain_mask_bits(selection.channel_mask & channel_limit(available), max);
        selection.channel_mask = mask;
        if mask != 0 {
            selection.channels = mask.count_ones();
            return;
        }
    }
    selection.channels = selection.channels.clamp(1, available.min(max).max(1));
}

/// Heals one selection against a device list and returns the log line for a
/// remap. Values that cannot be resolved unambiguously are left untouched.
pub(crate) fn heal_selection(
    devices: &[DeviceInfo],
    selection: &mut DeviceSelection,
    input: bool,
) -> Option<String> {
    if selection.name.is_empty() {
        return None;
    }
    let SelectionMatch::Remapped(device) = resolve_selection(devices, selection, input) else {
        return None;
    };
    let old = selection.name.clone();
    remap_selection(selection, &device, input);
    Some(format!(
        "devices: '{}' -> '{}' ({})",
        old,
        selection.name,
        api_label(selection.api)
    ))
}

pub(crate) fn exact_output<'a>(
    devices: &'a [DeviceInfo],
    selection: &DeviceSelection,
) -> Option<&'a DeviceInfo> {
    devices.iter().find(|device| {
        device.outputs > 0 && device.name == selection.name && device.api == selection.api
    })
}

/// The output device with this name, preferring shared over low-latency over
/// any other API, as the follow-default monitor has always done.
pub(crate) fn output_by_name<'a>(devices: &'a [DeviceInfo], name: &str) -> Option<&'a DeviceInfo> {
    devices
        .iter()
        .find(|device| {
            device.name == name && device.outputs > 0 && device.api == AudioApi::WasapiShared
        })
        .or_else(|| {
            devices.iter().find(|device| {
                device.name == name
                    && device.outputs > 0
                    && device.api == AudioApi::WasapiLowLatency
            })
        })
        .or_else(|| {
            devices
                .iter()
                .find(|device| device.name == name && device.outputs > 0)
        })
}

pub(crate) fn configured_monitor_selection(
    device: &DeviceInfo,
    configured: &DeviceSelection,
) -> DeviceSelection {
    let limit = channel_limit(device.outputs);
    let mask = configured.channel_mask & limit;
    let channels = if mask != 0 {
        mask.count_ones().min(device.outputs.max(1))
    } else {
        configured.channels.clamp(1, device.outputs.clamp(1, 2))
    };
    let mut selection = selection_for(device, channels);
    selection.channel_mask = mask;
    selection
}

pub(crate) fn find_loopback_input(
    inputs: &[DeviceInfo],
    output: &DeviceInfo,
) -> Option<DeviceInfo> {
    let lower = output.name.to_lowercase();
    let wanted = lower.replace("input", "output");
    let rank = |api: AudioApi| match api {
        AudioApi::WasapiShared => 0,
        AudioApi::WasapiLowLatency => 1,
        AudioApi::WasapiExclusive => 2,
        _ => 3,
    };
    let find = |name: &str| {
        inputs
            .iter()
            .filter(|device| device.inputs > 0 && device.name.to_lowercase() == name)
            .min_by_key(|device| rank(device.api))
            .cloned()
    };
    if let Some(device) = find(&wanted) {
        return Some(device);
    }
    if lower.contains("cable input") {
        let is_cable_capture = |name: &str| {
            let name = name.to_lowercase();
            name.contains("cable output")
                || (name.contains("vb-audio virtual cable") && name.contains("microphone"))
        };
        return inputs
            .iter()
            .filter(|device| device.inputs > 0 && is_cable_capture(&device.name))
            .min_by_key(|device| rank(device.api))
            .cloned();
    }
    None
}

pub(crate) fn default_device(devices: &[DeviceInfo]) -> Option<&DeviceInfo> {
    devices
        .iter()
        .find(|device| device.is_default)
        .or_else(|| {
            devices
                .iter()
                .find(|device| device.api == AudioApi::WasapiShared)
        })
        .or_else(|| devices.first())
}

pub(crate) fn sidechain_label(source: SidechainSource) -> String {
    match source {
        SidechainSource::Off => "SC: off".to_string(),
        SidechainSource::MainMic => "SC: main mic".to_string(),
        SidechainSource::InputChannel(channel) => format!("SC: input {}", channel + 1),
        SidechainSource::SecondInput(channel) => format!("SC: aux {}", channel + 1),
        SidechainSource::Silence => "SC: silence".to_string(),
    }
}

pub(crate) fn channel_limit(available: u32) -> u64 {
    if available >= 64 {
        u64::MAX
    } else if available == 0 {
        0
    } else {
        (1u64 << available) - 1
    }
}

pub(crate) fn default_channel_mask(selection: &DeviceSelection, available: u32) -> u64 {
    let limit = channel_limit(available);
    if selection.channel_mask != 0 {
        return selection.channel_mask & limit;
    }
    let count = selection.channels.clamp(1, available.max(1)) as u64;
    if count >= 64 {
        limit
    } else {
        ((1u64 << count) - 1) & limit
    }
}

pub(crate) fn normalize_selection_channels(
    selection: &mut DeviceSelection,
    available: u32,
    max: u32,
) {
    if available == 0 {
        return;
    }
    let limit = channel_limit(available);
    if selection.channels == 0 {
        selection.channels = if max >= 2 { 2 } else { 1 };
        selection.channel_mask = 0;
    }
    if selection.channel_mask != 0 {
        let masked = selection.channel_mask & limit;
        let count = masked.count_ones();
        if masked != selection.channel_mask
            || count == 0
            || count > max
            || count != selection.channels
        {
            selection.channel_mask = 0;
            selection.channels = selection.channels.clamp(1, available.min(max).max(1));
        }
    } else if selection.channels > available || selection.channels > max {
        selection.channels = selection.channels.clamp(1, available.min(max).max(1));
    }
}

pub(crate) fn channel_mask_label(mask: u64, available: u32) -> String {
    let limit = channel_limit(available);
    let mut mask = mask & limit;
    if mask == 0 && available > 0 {
        mask = 1;
    }
    let mut parts = Vec::new();
    let mut channel = 0u32;
    while channel < available.min(64) {
        if mask & (1u64 << channel) == 0 {
            channel += 1;
            continue;
        }
        let start = channel;
        while channel + 1 < available && channel + 1 < 64 && mask & (1u64 << (channel + 1)) != 0 {
            channel += 1;
        }
        if start == channel {
            parts.push(format!("{}", start + 1));
        } else {
            parts.push(format!("{}-{}", start + 1, channel + 1));
        }
        channel += 1;
    }
    if parts.is_empty() {
        "Select channels".to_string()
    } else {
        format!("Ch {}", parts.join(", "))
    }
}

pub(crate) fn automatic_api_label(api: AudioApi) -> &'static str {
    match api {
        AudioApi::WasapiLowLatency => "Automatic: WASAPI low-latency",
        AudioApi::WasapiShared => "Automatic: WASAPI shared",
        AudioApi::WasapiExclusive => "Automatic: WASAPI exclusive (kept)",
        AudioApi::Asio => "Automatic: ASIO (kept)",
        AudioApi::Other => "Automatic",
    }
}

pub(crate) fn device_channels(device: &DeviceInfo, input: bool) -> u32 {
    if input { device.inputs } else { device.outputs }
}

pub(crate) fn apply_automatic_apis_to(
    devices: &DeviceCache,
    settings: &mut ProjectSettings,
) -> bool {
    let mut changed = false;
    let input_api = automatic_api(
        &devices.inputs,
        &settings.input.name,
        settings.input.api,
        true,
    );
    changed |= set_selection_api(&devices.inputs, &mut settings.input, input_api, true);
    let output_api = automatic_api(
        &devices.outputs,
        &settings.output.name,
        settings.output.api,
        false,
    );
    changed |= set_selection_api(&devices.outputs, &mut settings.output, output_api, false);
    let sidechain_api = automatic_api(
        &devices.inputs,
        &settings.sidechain_input.name,
        settings.sidechain_input.api,
        true,
    );
    changed |= set_selection_api(
        &devices.inputs,
        &mut settings.sidechain_input,
        sidechain_api,
        true,
    );
    // A configured monitor output follows the same automatic-API rule as the
    // main output; an empty (follow-default) selection is left untouched.
    if !settings.monitor_output.name.is_empty() {
        let monitor_api = automatic_api(
            &devices.outputs,
            &settings.monitor_output.name,
            settings.monitor_output.api,
            false,
        );
        changed |= set_selection_api(
            &devices.outputs,
            &mut settings.monitor_output,
            monitor_api,
            false,
        );
    }
    changed
}

pub(crate) fn automatic_api(
    devices: &[DeviceInfo],
    name: &str,
    current: AudioApi,
    input: bool,
) -> AudioApi {
    if name.is_empty() {
        if devices.iter().any(|device| {
            device.api == AudioApi::WasapiLowLatency && device_channels(device, input) > 0
        }) {
            return AudioApi::WasapiLowLatency;
        }
        if devices.iter().any(|device| {
            device.api == AudioApi::WasapiShared && device_channels(device, input) > 0
        }) {
            return AudioApi::WasapiShared;
        }
        return current;
    }
    if devices.iter().any(|device| {
        device.api == AudioApi::WasapiLowLatency
            && device.name == name
            && device_channels(device, input) > 0
    }) {
        return AudioApi::WasapiLowLatency;
    }
    if devices.iter().any(|device| {
        device.api == AudioApi::WasapiShared
            && device.name == name
            && device_channels(device, input) > 0
    }) {
        return AudioApi::WasapiShared;
    }
    current
}

pub(crate) fn set_selection_api(
    devices: &[DeviceInfo],
    selection: &mut DeviceSelection,
    api: AudioApi,
    input: bool,
) -> bool {
    if selection.api == api {
        return false;
    }
    selection.api = api;
    if let Some(device) = devices.iter().find(|device| {
        device.api == api && device.name == selection.name && device_channels(device, input) > 0
    }) {
        selection.id = device.id.clone();
        let available = device_channels(device, input);
        let max = if input { 8 } else { 2 };
        if selection.channel_mask == 0 {
            selection.channels = selection.channels.clamp(1, available.min(max).max(1));
        } else {
            let mask = retain_mask_bits(selection.channel_mask & channel_limit(available), max);
            selection.channel_mask = mask;
            if mask == 0 {
                selection.channels = selection.channels.clamp(1, available.min(max).max(1));
            } else {
                selection.channels = mask.count_ones();
            }
        }
    }
    true
}

/// Keeps the lowest selected mask bits when a selection exceeds the channel
/// cap for its direction, so a preserved mask always fits the new device.
pub(crate) fn retain_mask_bits(mask: u64, max: u32) -> u64 {
    let mut mask = mask;
    while max < 64 && mask.count_ones() > max {
        let highest = 63 - mask.leading_zeros();
        mask &= !(1u64 << highest);
    }
    mask
}

/// Switches a draft selection to another API. The same-named device is kept
/// (with a fitting channel mask) when the new API exposes it; otherwise the
/// first device of that API replaces it. An API with no devices is rejected
/// without mutating the draft.
pub(crate) fn switch_selection_api(
    devices: &[DeviceInfo],
    selection: &mut DeviceSelection,
    api: AudioApi,
    input: bool,
) -> bool {
    let available = devices
        .iter()
        .any(|device| device.api == api && device_channels(device, input) > 0);
    if !available || !set_selection_api(devices, selection, api, input) {
        return false;
    }
    let same_device = devices.iter().any(|device| {
        device.api == api && device.name == selection.name && device_channels(device, input) > 0
    });
    if !same_device
        && let Some(device) = devices
            .iter()
            .find(|device| device.api == api && device_channels(device, input) > 0)
    {
        *selection = selection_for(device, device_channels(device, input).min(2));
    }
    true
}

pub(crate) fn available_apis(devices: &[DeviceInfo], input: bool) -> Vec<AudioApi> {
    let mut apis = Vec::new();
    for device in devices {
        if device_channels(device, input) > 0 && !apis.contains(&device.api) {
            apis.push(device.api);
        }
    }
    apis
}

pub(crate) fn unique_device_names(devices: &[DeviceInfo], input: bool) -> Vec<String> {
    let mut names = Vec::new();
    for device in devices {
        if device_channels(device, input) > 0 && !names.contains(&device.name) {
            names.push(device.name.clone());
        }
    }
    names
}

pub(crate) fn selected_device<'a>(
    devices: &'a [DeviceInfo],
    name: &str,
    current: AudioApi,
    advanced: bool,
    input: bool,
) -> Option<&'a DeviceInfo> {
    let matches = |device: &DeviceInfo| device.name == name && device_channels(device, input) > 0;
    if advanced {
        return devices
            .iter()
            .find(|device| matches(device) && device.api == current);
    }
    if !matches!(current, AudioApi::WasapiLowLatency | AudioApi::WasapiShared)
        && let Some(device) = devices
            .iter()
            .find(|device| matches(device) && device.api == current)
    {
        return Some(device);
    }
    for api in [AudioApi::WasapiLowLatency, AudioApi::WasapiShared] {
        if let Some(device) = devices
            .iter()
            .find(|device| matches(device) && device.api == api)
        {
            return Some(device);
        }
    }
    devices
        .iter()
        .find(|device| matches(device) && device.api == current)
        .or_else(|| devices.iter().find(|device| matches(device)))
}
