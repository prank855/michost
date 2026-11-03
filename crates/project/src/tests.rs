use super::*;
use michost_graph::{ChainEntry, PluginSpec, SidechainSource};
use serde_json::{Value, json};
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("michost-project-{}-{}", std::process::id(), id));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn plugin(id: u64, name: &str, bypassed: bool, sidechain_source: SidechainSource) -> ChainEntry {
    let mut plugin = PluginSpec::new(
        format!("C:/plugins/{name}.vst3"),
        format!("com.example.{name}"),
        name,
        "Example",
    );
    plugin.main_inputs = 2;
    plugin.main_outputs = 2;
    plugin.sidechain_inputs = 1;
    ChainEntry {
        id,
        plugin,
        bypassed,
        sidechain_source,
        oversampling: 1,
    }
}

fn sample_project() -> Project {
    let mut project = Project::new();
    project.settings.input = DeviceSelection {
        api: AudioApi::default(),
        name: "Microphone".into(),
        id: "{input-id}".into(),
        channels: 2,
        channel_mask: 0,
    };
    project.settings.output = DeviceSelection {
        api: AudioApi::default(),
        name: "Speakers".into(),
        id: "{output-id}".into(),
        channels: 2,
        channel_mask: 0,
    };
    project.settings.sidechain_input = DeviceSelection {
        api: AudioApi::WasapiShared,
        name: "CABLE Output".into(),
        id: "{sidechain-id}".into(),
        channels: 2,
        channel_mask: 0,
    };
    project.settings.sidechain_input_enabled = true;
    project.settings.sample_rate = 48_000.0;
    project.settings.buffer_size = 256;
    project.settings.safety_buffer_ms = 10;
    project.settings.input_mono_source = MonoSource::Right;
    project.settings.advanced_mode = true;
    project.settings.close_to_tray = false;
    project.settings.launch_at_login = true;
    project.chain = vec![
        plugin(2, "first", false, SidechainSource::Off),
        plugin(5, "second", true, SidechainSource::InputChannel(2)),
    ];
    project.set_plugin_state(2, vec![1, 2, 3, 250]);
    project.set_plugin_state(5, vec![9, 8, 7]);
    project
}

#[test]
fn roundtrip_preserves_settings_chain_and_states() {
    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    let project = sample_project();
    project.save(&path).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("{\n"));
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded, project);
    assert_eq!(loaded.chain[0].id, 2);
    assert!(!loaded.chain[0].bypassed);
    assert_eq!(loaded.chain[1].id, 5);
    assert!(loaded.chain[1].bypassed);
    assert_eq!(
        loaded.chain[1].sidechain_source,
        SidechainSource::InputChannel(2)
    );
    assert!(loaded.settings.sidechain_input_enabled);
    assert_eq!(loaded.settings.sidechain_input.name, "CABLE Output");
    assert_eq!(loaded.settings.sidechain_input.api, AudioApi::WasapiShared);
    assert_eq!(loaded.settings.sidechain_input.channels, 2);
    assert_eq!(loaded.settings.input_mono_source, MonoSource::Right);
    assert!(loaded.settings.advanced_mode);
    assert!(!loaded.settings.close_to_tray);
    assert_eq!(loaded.plugin_state(2), Some([1, 2, 3, 250].as_slice()));
    assert_eq!(loaded.plugin_state(5), Some([9, 8, 7].as_slice()));
}

#[test]
fn plugin_states_are_base64_and_empty_states_are_omitted() {
    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    let mut project = sample_project();
    project.set_plugin_state(9, Vec::new());
    project.save(&path).unwrap();
    let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let states = value["plugin_states"].as_object().unwrap();
    assert_eq!(states.len(), 2);
    assert!(!states.contains_key("9"));
    let encoded = states["2"].as_str().unwrap();
    assert_eq!(encoded, "AQID+g==");
    assert_eq!(states["5"].as_str().unwrap(), "CQgH");
}

#[test]
fn future_schema_version_is_rejected() {
    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    sample_project().save(&path).unwrap();
    let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value["schema_version"] = json!(SCHEMA_VERSION + 1);
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    match Project::load(&path) {
        Err(ProjectError::UnsupportedVersion(version)) => {
            assert_eq!(version, SCHEMA_VERSION + 1);
        }
        other => panic!("expected unsupported version error, got {other:?}"),
    }
    value["schema_version"] = json!(0);
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    assert_eq!(Project::load(&path).unwrap().schema_version, 0);
}

#[test]
fn missing_schema_version_loads_as_version_one() {
    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    sample_project().save(&path).unwrap();
    let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("schema_version");
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded.schema_version, SCHEMA_VERSION);
    assert_eq!(loaded.chain.len(), 2);
}

#[test]
fn duplicate_chain_ids_are_rejected() {
    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    let mut project = sample_project();
    project
        .chain
        .push(plugin(5, "duplicate", false, SidechainSource::Off));
    project.save(&path).unwrap();
    match Project::load(&path) {
        Err(ProjectError::Format(message)) => assert!(message.contains("duplicate")),
        other => panic!("expected format error, got {other:?}"),
    }
}

#[test]
fn plugin_ids_advance_and_prune() {
    let mut project = sample_project();
    assert_eq!(project.next_plugin_id(), 6);
    project.set_plugin_state(99, vec![1]);
    project.set_plugin_state(1, vec![2]);
    project.prune_plugin_states();
    assert!(project.plugin_states.contains_key(&2));
    assert!(project.plugin_states.contains_key(&5));
    assert!(!project.plugin_states.contains_key(&99));
    assert!(!project.plugin_states.contains_key(&1));
    assert_eq!(project.next_plugin_id(), 6);
    project.remove_plugin_state(2);
    assert!(project.plugin_state(2).is_none());
    project.chain.clear();
    assert_eq!(project.next_plugin_id(), 1);
    assert_eq!(Project::new().next_plugin_id(), 1);
}

#[test]
fn remove_builtin_entries_drops_effects_and_state() {
    let mut project = sample_project();
    let builtin = ChainEntry {
        id: 10,
        plugin: PluginSpec::new("builtin:eq", "builtin.eq", "EQ", "MicHost"),
        bypassed: false,
        sidechain_source: SidechainSource::Off,
        oversampling: 1,
    };
    project.chain.push(builtin);
    project.set_plugin_state(10, vec![1, 2, 3]);
    project.set_plugin_state(2, vec![4]);

    let removed = project.remove_builtin_entries();

    assert_eq!(removed, vec!["EQ".to_string()]);
    assert!(project.chain.iter().all(|entry| entry.id != 10));
    assert!(project.plugin_state(10).is_none());
    assert!(project.plugin_state(2).is_some());
}

#[test]
fn atomic_save_replaces_and_cleans_up() {
    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    let first = sample_project();
    first.save(&path).unwrap();
    assert_eq!(temp.entries(), vec!["project.json".to_string()]);

    let mut second = Project::new();
    second.chain = vec![plugin(7, "only", false, SidechainSource::Off)];
    second.settings.buffer_size = 512;
    second.save(&path).unwrap();
    assert_eq!(temp.entries(), vec!["project.json".to_string()]);
    assert_eq!(Project::load(&path).unwrap(), second);
}

#[test]
fn failed_save_keeps_original_and_removes_temp_file() {
    let temp = TempDir::new();
    let original = temp.path().join("original.json");
    let project = sample_project();
    project.save(&original).unwrap();
    let before = fs::read(&original).unwrap();

    let directory_target = temp.path().join("target");
    fs::create_dir(&directory_target).unwrap();
    assert!(project.save(&directory_target).is_err());
    assert_eq!(fs::read(&original).unwrap(), before);
    assert_eq!(
        temp.entries(),
        vec!["original.json".to_string(), "target".to_string()]
    );
    assert!(Project::load(&original).is_ok());
}

#[test]
fn legacy_project_without_sidechain_settings_loads() {
    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    sample_project().save(&path).unwrap();
    let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let settings = value["settings"].as_object_mut().unwrap();
    settings.remove("sidechain_input");
    settings.remove("sidechain_input_enabled");
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert!(!loaded.settings.sidechain_input_enabled);
    assert_eq!(loaded.settings.sidechain_input, DeviceSelection::default());
    assert_eq!(loaded.chain.len(), 2);

    let minimal = json!({
        "settings": {"input": {"name": "Mic"}, "output": {"name": "Speakers"}},
        "chain": []
    });
    fs::write(&path, serde_json::to_string(&minimal).unwrap()).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert!(!loaded.settings.sidechain_input_enabled);
    assert_eq!(loaded.settings.sidechain_input.name, "");
}

#[test]
fn second_input_sidechain_source_serde_roundtrip() {
    for source in [
        SidechainSource::Off,
        SidechainSource::MainMic,
        SidechainSource::InputChannel(3),
        SidechainSource::SecondInput(1),
        SidechainSource::Silence,
    ] {
        let encoded = serde_json::to_string(&source).unwrap();
        let decoded: SidechainSource = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, source);
    }
    assert_eq!(
        serde_json::to_string(&SidechainSource::SecondInput(0)).unwrap(),
        "{\"SecondInput\":0}"
    );
}

#[test]
fn defaults_include_new_settings() {
    let settings = ProjectSettings::default();
    assert!(!settings.advanced_mode);
    assert!(settings.close_to_tray);
    assert_eq!(settings.input_mono_source, MonoSource::Left);
    assert_eq!(MonoSource::default(), MonoSource::Left);
}

#[test]
fn new_settings_serde_shapes_and_saved_fields() {
    assert_eq!(
        serde_json::to_string(&MonoSource::Left).unwrap(),
        "\"Left\""
    );
    assert_eq!(
        serde_json::to_string(&MonoSource::Right).unwrap(),
        "\"Right\""
    );

    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    sample_project().save(&path).unwrap();
    let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["settings"]["input_mono_source"], json!("Right"));
    assert_eq!(value["settings"]["advanced_mode"], json!(true));
    assert_eq!(value["settings"]["close_to_tray"], json!(false));

    let defaults: Value = serde_json::to_value(Project::new()).unwrap();
    assert_eq!(defaults["settings"]["input_mono_source"], json!("Left"));
    assert_eq!(defaults["settings"]["advanced_mode"], json!(false));
    assert_eq!(defaults["settings"]["close_to_tray"], json!(true));
}

#[test]
fn legacy_project_without_new_settings_loads() {
    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    sample_project().save(&path).unwrap();
    let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let settings = value["settings"].as_object_mut().unwrap();
    settings.remove("input_mono_source");
    settings.remove("advanced_mode");
    settings.remove("close_to_tray");
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded.settings.input_mono_source, MonoSource::Left);
    assert!(!loaded.settings.advanced_mode);
    assert!(loaded.settings.close_to_tray);
    assert_eq!(loaded.chain.len(), 2);

    let minimal = json!({
        "settings": {"input": {"name": "Mic"}, "output": {"name": "Speakers"}},
        "chain": []
    });
    fs::write(&path, serde_json::to_string(&minimal).unwrap()).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded.settings.input_mono_source, MonoSource::Left);
    assert!(!loaded.settings.advanced_mode);
    assert!(loaded.settings.close_to_tray);
}

#[test]
fn output_kind_defaults_to_device_and_round_trips() {
    assert_eq!(OutputKind::default(), OutputKind::Device);
    assert_eq!(
        serde_json::to_string(&OutputKind::Device).unwrap(),
        "\"device\""
    );
    assert_eq!(
        serde_json::to_string(&OutputKind::VirtualMic).unwrap(),
        "\"virtual_mic\""
    );

    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    let mut project = sample_project();
    project.settings.output_kind = OutputKind::VirtualMic;
    project.save(&path).unwrap();
    let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["settings"]["output_kind"], json!("virtual_mic"));
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded.settings.output_kind, OutputKind::VirtualMic);
    assert_eq!(loaded.settings.output, project.settings.output);

    let records_only_kind = json!({
        "settings": {"output_kind": "virtual_mic"},
    });
    fs::write(&path, serde_json::to_string(&records_only_kind).unwrap()).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded.settings.output_kind, OutputKind::VirtualMic);
}

#[test]
fn chain_oversampling_defaults_to_one_and_round_trips() {
    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    let mut project = sample_project();
    project.chain[0].oversampling = 4;
    project.save(&path).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded.chain[0].oversampling, 4);

    let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value["chain"][0]
        .as_object_mut()
        .unwrap()
        .remove("oversampling");
    fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded.chain[0].oversampling, 1);
}

#[test]
fn monitor_output_and_enabled_round_trip_with_legacy_defaults() {
    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    let project = sample_project();
    assert!(project.settings.monitor_output.name.is_empty());
    assert!(!project.settings.monitor_enabled);
    project.save(&path).unwrap();

    let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value["settings"]
        .as_object_mut()
        .unwrap()
        .remove("monitor_output");
    value["settings"]
        .as_object_mut()
        .unwrap()
        .remove("monitor_enabled");
    fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert!(loaded.settings.monitor_output.name.is_empty());
    assert!(!loaded.settings.monitor_enabled);

    let project = sample_project();
    assert_eq!(project.settings.monitor_trim_db, 0.0);
    let mut project = sample_project();
    project.settings.monitor_output = DeviceSelection {
        api: AudioApi::WasapiShared,
        name: "Headphones".into(),
        id: "{monitor-id}".into(),
        channels: 2,
        channel_mask: 12,
    };
    project.settings.monitor_enabled = true;
    project.settings.monitor_trim_db = -6.5;
    project.save(&path).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded.settings.monitor_output.name, "Headphones");
    assert_eq!(loaded.settings.monitor_output.channels, 2);
    assert_eq!(loaded.settings.monitor_output.channel_mask, 12);
    assert!(loaded.settings.monitor_enabled);
    assert!((loaded.settings.monitor_trim_db + 6.5).abs() < 1.0e-6);
}

#[test]
fn legacy_project_without_output_kind_loads_as_device() {
    let temp = TempDir::new();
    let path = temp.path().join("project.json");
    sample_project().save(&path).unwrap();
    let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value["settings"]
        .as_object_mut()
        .unwrap()
        .remove("output_kind");
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded.settings.output_kind, OutputKind::Device);
    assert_eq!(loaded.settings.output, sample_project().settings.output);

    let minimal = json!({
        "settings": {"input": {"name": "Mic"}, "output": {"name": "Speakers"}},
        "chain": []
    });
    fs::write(&path, serde_json::to_string(&minimal).unwrap()).unwrap();
    let loaded = Project::load(&path).unwrap();
    assert_eq!(loaded.settings.output_kind, OutputKind::Device);
}

#[test]
fn default_path_shape_matches_appdata() {
    match std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
        Some(appdata) => {
            let path = Project::default_path().expect("APPDATA is set");
            assert!(path.starts_with(PathBuf::from(appdata)));
            assert!(path.ends_with(Path::new("MicHost").join("project.json")));
        }
        None => assert!(Project::default_path().is_none()),
    }
}

#[test]
fn missing_default_file_is_none() {
    let temp = TempDir::new();
    let path = temp.path().join("absent").join("project.json");
    assert!(Project::load_if_exists(&path).unwrap().is_none());
}
