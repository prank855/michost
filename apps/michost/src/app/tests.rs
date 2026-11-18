//! Unit tests for the app's pure helpers.

use super::{
    DeviceDraft, PickerGrouping, SelectionMatch, apply_automatic_apis_to, build_picker_cache,
    builtin_descriptor, builtin_spec, channel_limit, channel_mask_label, default_channel_mask,
    descriptor_score, device_model_suffix, display_category, find_loopback_input, fuzzy_score,
    heal_selection, hidden_repaint_now, move_chain_entry, normalize_search,
    normalize_selection_channels, resolve_selection, strip_builtins, switch_selection_api,
};
use crate::device_cache::DeviceCache;
use michost_builtin::BuiltinKind;
use michost_graph::{ChainEntry, PluginSpec};
use michost_host_abi::{AudioApi, DeviceInfo, PluginDescriptor};
use michost_project::{DeviceSelection, Project, ProjectSettings};

fn entry(id: u64) -> ChainEntry {
    ChainEntry {
        id,
        plugin: PluginSpec::new("C:/x.vst3", "class", format!("Plugin {id}"), "Vendor"),
        bypassed: false,
        sidechain_source: michost_graph::SidechainSource::Off,
        oversampling: 1,
    }
}

fn ids(chain: &[ChainEntry]) -> Vec<u64> {
    chain.iter().map(|entry| entry.id).collect()
}

fn input_device(name: &str, api: AudioApi, channels: u32) -> DeviceInfo {
    DeviceInfo {
        name: name.to_string(),
        api,
        inputs: channels,
        ..DeviceInfo::default()
    }
}

fn output_device(name: &str, api: AudioApi, channels: u32) -> DeviceInfo {
    DeviceInfo {
        name: name.to_string(),
        api,
        outputs: channels,
        ..DeviceInfo::default()
    }
}

fn stored_selection(name: &str, api: AudioApi) -> DeviceSelection {
    DeviceSelection {
        api,
        name: name.to_string(),
        channels: 2,
        ..DeviceSelection::default()
    }
}

#[test]
fn resolve_exact_match_wins() {
    let devices = [
        output_device("Main (MOTU M Series)", AudioApi::WasapiShared, 2),
        output_device("Headphone (MOTU M Series)", AudioApi::WasapiShared, 2),
    ];
    let selection = stored_selection("Headphone (MOTU M Series)", AudioApi::WasapiShared);
    assert!(matches!(
        resolve_selection(&devices, &selection, false),
        SelectionMatch::Exact
    ));
    assert_eq!(
        device_model_suffix("Main (MOTU M Series)"),
        Some("MOTU M Series")
    );
    assert_eq!(device_model_suffix("Voicemeeter Input"), None);
    assert_eq!(device_model_suffix("Main ()"), None);
}

#[test]
fn resolve_renamed_parenthetical_suffix_remaps() {
    let devices = [DeviceInfo {
        id: "wasapi|Headphone (MOTU M Series)".to_string(),
        ..output_device("Headphone (MOTU M Series)", AudioApi::WasapiShared, 2)
    }];
    let selection = DeviceSelection {
        id: "wasapi|Main (MOTU M Series)".to_string(),
        ..stored_selection("Main (MOTU M Series)", AudioApi::WasapiShared)
    };
    match resolve_selection(&devices, &selection, false) {
        SelectionMatch::Remapped(device) => {
            assert_eq!(device.name, "Headphone (MOTU M Series)");
            assert_eq!(device.id, "wasapi|Headphone (MOTU M Series)");
        }
        other => panic!("expected a remap, got {other:?}"),
    }
}

#[test]
fn resolve_two_suffix_candidates_is_missing() {
    let devices = [
        output_device("Headphone (MOTU M Series)", AudioApi::WasapiShared, 2),
        output_device("Speakers (MOTU M Series)", AudioApi::WasapiShared, 2),
    ];
    let selection = stored_selection("Main (MOTU M Series)", AudioApi::WasapiShared);
    assert!(matches!(
        resolve_selection(&devices, &selection, false),
        SelectionMatch::Missing
    ));
}

#[test]
fn resolve_missing_keeps_value() {
    let devices = [output_device(
        "Headphone (Other Interface)",
        AudioApi::WasapiShared,
        2,
    )];
    let selection = stored_selection("Main (MOTU M Series)", AudioApi::WasapiShared);
    assert!(matches!(
        resolve_selection(&devices, &selection, false),
        SelectionMatch::Missing
    ));
    let mut settings = ProjectSettings {
        output: selection.clone(),
        ..ProjectSettings::default()
    };
    assert_eq!(heal_selection(&devices, &mut settings.output, false), None);
    assert_eq!(settings.output, selection);
}

#[test]
fn heal_is_idempotent() {
    let devices = [output_device(
        "Headphone (MOTU M Series)",
        AudioApi::WasapiShared,
        2,
    )];
    let mut settings = ProjectSettings {
        monitor_output: stored_selection("Main (MOTU M Series)", AudioApi::WasapiShared),
        ..ProjectSettings::default()
    };
    let line = heal_selection(&devices, &mut settings.monitor_output, false).unwrap();
    assert!(line.contains("'Headphone (MOTU M Series)'"), "{line}");
    assert_eq!(settings.monitor_output.name, "Headphone (MOTU M Series)");
    assert!(matches!(
        resolve_selection(&devices, &settings.monitor_output, false),
        SelectionMatch::Exact
    ));
    assert_eq!(
        heal_selection(&devices, &mut settings.monitor_output, false),
        None
    );
}

#[test]
fn apply_automatic_apis_includes_monitor_output() {
    let cache = DeviceCache {
        outputs: vec![output_device(
            "Studio (USB Audio)",
            AudioApi::WasapiShared,
            2,
        )],
        ..DeviceCache::default()
    };
    let mut settings = ProjectSettings {
        monitor_output: stored_selection("Studio (USB Audio)", AudioApi::WasapiExclusive),
        ..ProjectSettings::default()
    };
    assert!(apply_automatic_apis_to(&cache, &mut settings));
    assert_eq!(settings.monitor_output.api, AudioApi::WasapiShared);
    assert_eq!(settings.monitor_output.name, "Studio (USB Audio)");

    // Follow-default (empty name) keeps the empty selection.
    let mut follow = ProjectSettings::default();
    let before = follow.monitor_output.clone();
    apply_automatic_apis_to(&cache, &mut follow);
    assert_eq!(follow.monitor_output, before);
}

#[test]
fn channel_masks_default_to_the_first_channels() {
    let selection = DeviceSelection {
        channels: 2,
        ..DeviceSelection::default()
    };
    assert_eq!(default_channel_mask(&selection, 8), 0b11);
    let explicit = DeviceSelection {
        channels: 2,
        channel_mask: 0b1001,
        ..DeviceSelection::default()
    };
    assert_eq!(default_channel_mask(&explicit, 8), 0b1001);
    assert_eq!(channel_limit(64), u64::MAX);
    assert_eq!(channel_limit(0), 0);
}

#[test]
fn channel_mask_labels_group_ranges() {
    assert_eq!(channel_mask_label(0b1011, 8), "Ch 1-2, 4");
    assert_eq!(channel_mask_label(0b10000, 8), "Ch 5");
    assert_eq!(channel_mask_label(0, 8), "Ch 1");
}

#[test]
fn normalizes_stale_channel_masks() {
    let mut selection = DeviceSelection {
        channels: 3,
        channel_mask: 0b111,
        ..DeviceSelection::default()
    };
    normalize_selection_channels(&mut selection, 2, 2);
    assert_eq!(selection.channel_mask, 0);
    assert_eq!(selection.channels, 2);

    let mut valid = DeviceSelection {
        channels: 2,
        channel_mask: 0b1010,
        ..DeviceSelection::default()
    };
    normalize_selection_channels(&mut valid, 8, 2);
    assert_eq!(valid.channel_mask, 0b1010);
    assert_eq!(valid.channels, 2);

    let mut beyond = DeviceSelection {
        channels: 2,
        channel_mask: 0b1_0000_0001,
        ..DeviceSelection::default()
    };
    normalize_selection_channels(&mut beyond, 8, 2);
    assert_eq!(beyond.channel_mask, 0);
}

#[test]
fn api_switch_preserves_a_fitting_channel_mask() {
    let devices = [
        input_device("Mic", AudioApi::Asio, 4),
        input_device("Mic", AudioApi::WasapiShared, 4),
    ];
    let mut selection = DeviceSelection {
        api: AudioApi::Asio,
        name: "Mic".to_string(),
        channels: 3,
        channel_mask: 0b1011,
        ..DeviceSelection::default()
    };
    assert!(switch_selection_api(
        &devices,
        &mut selection,
        AudioApi::WasapiShared,
        true,
    ));
    assert_eq!(selection.api, AudioApi::WasapiShared);
    assert_eq!(selection.channels, 3);
    assert_eq!(selection.channel_mask, 0b1011);
}

#[test]
fn api_switch_clamps_mask_to_device_and_direction_cap() {
    let inputs = [
        input_device("Mic", AudioApi::Asio, 8),
        input_device("Mic", AudioApi::WasapiShared, 2),
    ];
    let mut selection = DeviceSelection {
        api: AudioApi::Asio,
        name: "Mic".to_string(),
        channels: 2,
        channel_mask: 0b1010,
        ..DeviceSelection::default()
    };
    switch_selection_api(&inputs, &mut selection, AudioApi::WasapiShared, true);
    assert_eq!(selection.channel_mask, 0b10);
    assert_eq!(selection.channels, 1);

    let outputs = [
        DeviceInfo {
            name: "Out".to_string(),
            api: AudioApi::Asio,
            outputs: 8,
            ..DeviceInfo::default()
        },
        DeviceInfo {
            name: "Out".to_string(),
            api: AudioApi::WasapiShared,
            outputs: 8,
            ..DeviceInfo::default()
        },
    ];
    let mut selection = DeviceSelection {
        api: AudioApi::Asio,
        name: "Out".to_string(),
        channels: 4,
        channel_mask: 0b1111,
        ..DeviceSelection::default()
    };
    switch_selection_api(&outputs, &mut selection, AudioApi::WasapiShared, false);
    assert_eq!(selection.channel_mask, 0b11);
    assert_eq!(selection.channels, 2);
}

#[test]
fn api_switch_rejects_an_api_without_devices() {
    let devices = [input_device("Mic", AudioApi::WasapiShared, 2)];
    let mut selection = DeviceSelection {
        api: AudioApi::WasapiShared,
        name: "Mic".to_string(),
        channels: 2,
        ..DeviceSelection::default()
    };
    assert!(!switch_selection_api(
        &devices,
        &mut selection,
        AudioApi::Asio,
        true,
    ));
    assert_eq!(selection.api, AudioApi::WasapiShared);
}

#[test]
fn device_draft_round_trips_device_selections() {
    let mut settings = ProjectSettings::default();
    settings.input.channels = 2;
    settings.output.channels = 2;
    settings.sidechain_input_enabled = true;
    let draft = DeviceDraft::from_settings(&settings);
    assert!(draft.matches(&settings));
    let mut changed = settings.clone();
    changed.input.channel_mask = 0b1000;
    changed.input.channels = 1;
    assert!(!draft.matches(&changed));
    draft.apply_to(&mut changed);
    assert!(draft.matches(&changed));
}

#[test]
fn maps_cable_output_and_prefers_shared_capture() {
    let output = DeviceInfo {
        name: "CABLE Input (VB-Audio Virtual Cable)".to_string(),
        api: AudioApi::WasapiExclusive,
        outputs: 2,
        ..DeviceInfo::default()
    };
    let inputs = [
        input_device(
            "CABLE Output (VB-Audio Virtual Cable)",
            AudioApi::WasapiShared,
            2,
        ),
        input_device(
            "CABLE Output (VB-Audio Virtual Cable)",
            AudioApi::WasapiExclusive,
            2,
        ),
        input_device(
            "CABLE Output (VB-Audio Virtual Cable)",
            AudioApi::WasapiLowLatency,
            2,
        ),
    ];
    let found = find_loopback_input(&inputs, &output).unwrap();
    assert_eq!(found.api, AudioApi::WasapiShared);
}

#[test]
fn maps_vb_cable_microphone_capture_endpoint() {
    let output = DeviceInfo {
        name: "CABLE Input (VB-Audio Virtual Cable)".to_string(),
        api: AudioApi::WasapiExclusive,
        outputs: 2,
        ..DeviceInfo::default()
    };
    let inputs = [input_device(
        "Microphone (VB-Audio Virtual Cable)",
        AudioApi::WasapiShared,
        2,
    )];
    let found = find_loopback_input(&inputs, &output).unwrap();
    assert_eq!(found.name, "Microphone (VB-Audio Virtual Cable)");
}

#[test]
fn maps_named_virtual_output() {
    let output = DeviceInfo {
        name: "Voicemeeter Input".to_string(),
        api: AudioApi::WasapiShared,
        outputs: 2,
        ..DeviceInfo::default()
    };
    let inputs = [input_device(
        "Voicemeeter Output",
        AudioApi::WasapiShared,
        2,
    )];
    assert!(find_loopback_input(&inputs, &output).is_some());
}

#[test]
fn rejects_physical_output_without_loopback() {
    let output = DeviceInfo {
        name: "Main (MOTU M Series)".to_string(),
        api: AudioApi::WasapiShared,
        outputs: 2,
        ..DeviceInfo::default()
    };
    let inputs = [input_device(
        "Do Not Use (MOTU M Series)",
        AudioApi::Asio,
        6,
    )];
    assert!(find_loopback_input(&inputs, &output).is_none());
}

#[test]
fn moves_chain_entries_both_directions() {
    let mut chain = vec![entry(1), entry(2), entry(3)];
    assert!(move_chain_entry(&mut chain, 0, 2));
    assert_eq!(ids(&chain), [2, 3, 1]);
    assert!(move_chain_entry(&mut chain, 2, 0));
    assert_eq!(ids(&chain), [1, 2, 3]);
}

#[test]
fn rejects_out_of_range_moves() {
    let mut chain = vec![entry(1), entry(2)];
    assert!(!move_chain_entry(&mut chain, 0, 0));
    assert!(!move_chain_entry(&mut chain, 2, 0));
    assert!(!move_chain_entry(&mut chain, 0, 2));
    assert_eq!(ids(&chain), [1, 2]);
}

#[test]
fn fuzzy_search_matches_compacted_queries() {
    let name = "FabFilter Pro-Q 4";
    for query in ["proq", "q4", "pro q4", "fab", "FabFilter"] {
        assert!(fuzzy_score(query, name).is_some(), "query {query}");
    }
    assert!(fuzzy_score("zzzz", name).is_none());
    assert_eq!(normalize_search("Pro-Q 4"), "proq4");
}

#[test]
fn fuzzy_search_prefers_prefix_and_substring() {
    let substring = fuzzy_score("proq", "FabFilter Pro-Q 4").unwrap();
    let subsequence = fuzzy_score("ffpq", "FabFilter Pro-Q 4").unwrap();
    assert!(substring > subsequence);
    assert!(fuzzy_score("fab", "FabFilter Pro-Q 4").unwrap() > substring);
}

#[test]
fn plugin_search_matches_category_and_formats_it() {
    let descriptor = PluginDescriptor {
        name: "Pro-Q 4".to_owned(),
        vendor: "FabFilter".to_owned(),
        category: "Fx|EQ".to_owned(),
        ..PluginDescriptor::default()
    };
    assert!(descriptor_score("eq", &descriptor).is_some());
    assert!(descriptor_score("fxeq", &descriptor).is_some());
    assert!(descriptor_score("reverb", &descriptor).is_none());
    assert_eq!(display_category("Fx|EQ"), "Fx / EQ");
    assert_eq!(
        display_category(" Instrument | Synth "),
        "Instrument / Synth"
    );
    assert_eq!(display_category(""), "");
}

#[test]
fn hidden_window_repaints_immediately_then_polls() {
    assert!(hidden_repaint_now(0));
    assert!(hidden_repaint_now(1));
    assert!(!hidden_repaint_now(2));
    assert!(!hidden_repaint_now(u8::MAX));
}

#[test]
fn release_mode_strips_builtin_entries() {
    let mut project = Project::new();
    project.chain.push(ChainEntry {
        id: 1,
        plugin: builtin_spec(BuiltinKind::Eq),
        bypassed: false,
        sidechain_source: michost_graph::SidechainSource::Off,
        oversampling: 1,
    });

    let removed = strip_builtins(&mut project, false);
    assert_eq!(removed, vec!["EQ".to_string()]);
    assert!(project.chain.is_empty());
    assert!(strip_builtins(&mut project, true).is_empty());
}

#[test]
fn builtins_are_identified_by_path_and_advertise_stereo() {
    for kind in BuiltinKind::ALL {
        let spec = builtin_spec(kind);
        assert_eq!(BuiltinKind::from_path(&spec.path), Some(kind));
        let descriptor = builtin_descriptor(kind);
        assert_eq!(descriptor.main_inputs, 2);
        assert_eq!(descriptor.main_outputs, 2);
        assert_eq!(BuiltinKind::from_path(&descriptor.path), Some(kind));
    }
}

#[test]
fn picker_groups_by_vendor_or_category() {
    let descriptor = |name: &str, vendor: &str, category: &str| PluginDescriptor {
        name: name.to_owned(),
        vendor: vendor.to_owned(),
        category: category.to_owned(),
        ..PluginDescriptor::default()
    };
    let plugins = [
        descriptor("Zeta", "Vendor A", "Fx|EQ"),
        descriptor("Alpha", "Vendor B", "Fx|Dynamics"),
        descriptor("Beta", "Vendor A", "Fx|EQ"),
    ];

    let vendor = build_picker_cache(&plugins, 0, String::new(), PickerGrouping::Vendor);
    assert_eq!(
        vendor.groups.keys().cloned().collect::<Vec<_>>(),
        ["Vendor A", "Vendor B"]
    );
    assert_eq!(vendor.groups["Vendor A"].len(), 2);

    let category = build_picker_cache(&plugins, 0, String::new(), PickerGrouping::Category);
    assert_eq!(
        category.groups.keys().cloned().collect::<Vec<_>>(),
        ["Fx / Dynamics", "Fx / EQ"]
    );
    let eq = &category.groups["Fx / EQ"];
    assert_eq!(
        eq.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
        [2, 0]
    );
}
