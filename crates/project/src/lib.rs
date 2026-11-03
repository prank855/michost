use michost_host_abi::AudioApi;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const SCHEMA_VERSION: u32 = 1;
pub const PROJECT_EXTENSION: &str = "mhchain";

const fn default_close_to_tray() -> bool {
    true
}

const fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonoSource {
    #[default]
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    #[default]
    Device,
    VirtualMic,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceSelection {
    #[serde(default)]
    pub api: AudioApi,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub channels: u32,
    /// Bit mask of device channels (bit 0 = first channel); zero selects the
    /// first `channels` channels.
    #[serde(default)]
    pub channel_mask: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectSettings {
    #[serde(default)]
    pub input: DeviceSelection,
    #[serde(default)]
    pub input_mono_source: MonoSource,
    #[serde(default)]
    pub output: DeviceSelection,
    #[serde(default)]
    pub output_kind: OutputKind,
    #[serde(default)]
    pub sidechain_input: DeviceSelection,
    #[serde(default)]
    pub sidechain_input_enabled: bool,
    #[serde(default)]
    pub sample_rate: f64,
    #[serde(default)]
    pub buffer_size: u32,
    #[serde(default)]
    pub output_buffer_size: u32,
    #[serde(default)]
    pub safety_buffer_ms: u32,
    #[serde(default)]
    pub advanced_mode: bool,
    #[serde(default = "default_close_to_tray")]
    pub close_to_tray: bool,
    #[serde(default)]
    pub launch_at_login: bool,
    /// Optional second output used for headphone monitoring. Empty name follows
    /// the system default playback device.
    #[serde(default)]
    pub monitor_output: DeviceSelection,
    #[serde(default)]
    pub monitor_enabled: bool,
    /// Monitor-only gain trim in dB.
    #[serde(default)]
    pub monitor_trim_db: f32,
}

#[allow(deprecated)]
impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            input: DeviceSelection::default(),
            input_mono_source: MonoSource::default(),
            output: DeviceSelection::default(),
            output_kind: OutputKind::default(),
            sidechain_input: DeviceSelection::default(),
            sidechain_input_enabled: false,
            sample_rate: 0.0,
            buffer_size: 0,
            output_buffer_size: 0,
            safety_buffer_ms: 0,
            advanced_mode: false,
            close_to_tray: true,
            launch_at_login: false,
            monitor_output: DeviceSelection::default(),
            monitor_enabled: false,
            monitor_trim_db: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginState {
    pub component: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub settings: ProjectSettings,
    #[serde(default)]
    pub chain: Vec<michost_graph::ChainEntry>,
    #[serde(default, with = "plugin_states_serde")]
    pub plugin_states: BTreeMap<u64, PluginState>,
}

impl Default for Project {
    fn default() -> Self {
        Self::new()
    }
}

impl Project {
    pub fn new() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            settings: ProjectSettings::default(),
            chain: Vec::new(),
            plugin_states: BTreeMap::new(),
        }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|error| ProjectError::Io(format!("{}: {error}", path.display())))?;
        Self::from_json(&text)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ProjectError> {
        let path = path.as_ref();
        let json = serde_json::to_string_pretty(self)
            .map_err(|error| ProjectError::Format(error.to_string()))?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| ProjectError::Io(format!("{}: {error}", parent.display())))?;
        }
        let temp = temp_path(path);
        let written = std::fs::File::create(&temp).and_then(|mut file| {
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, path)
        });
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(ProjectError::Io(format!("{}: {error}", path.display())));
        }
        Ok(())
    }

    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("APPDATA")
            .filter(|value| !value.is_empty())
            .map(|value| PathBuf::from(value).join("MicHost").join("project.json"))
    }

    pub fn load_default() -> Result<Option<Self>, ProjectError> {
        match Self::default_path() {
            Some(path) => Self::load_if_exists(&path),
            None => Ok(None),
        }
    }

    pub fn save_default(&self) -> Result<PathBuf, ProjectError> {
        let path = Self::default_path().ok_or_else(|| {
            ProjectError::Io("APPDATA is not set; cannot resolve the default project path".into())
        })?;
        self.save(&path)?;
        Ok(path)
    }

    pub fn plugin_state(&self, id: u64) -> Option<&[u8]> {
        self.plugin_states
            .get(&id)
            .map(|state| state.component.as_slice())
    }

    pub fn set_plugin_state(&mut self, id: u64, component: Vec<u8>) {
        self.plugin_states.insert(id, PluginState { component });
    }

    pub fn remove_plugin_state(&mut self, id: u64) {
        self.plugin_states.remove(&id);
    }

    pub fn next_plugin_id(&self) -> u64 {
        self.chain
            .iter()
            .map(|entry| entry.id)
            .max()
            .map_or(1, |id| id.saturating_add(1).max(1))
    }

    pub fn prune_plugin_states(&mut self) {
        let live: BTreeSet<u64> = self.chain.iter().map(|entry| entry.id).collect();
        self.plugin_states.retain(|id, _| live.contains(id));
    }

    /// Removes built-in effects and their saved state. Used by release builds,
    /// which do not include the built-in processors.
    pub fn remove_builtin_entries(&mut self) -> Vec<String> {
        let mut removed = Vec::new();
        self.chain.retain(|entry| {
            if entry.plugin.path.starts_with("builtin:") {
                removed.push(entry.plugin.name.clone());
                false
            } else {
                true
            }
        });
        if !removed.is_empty() {
            self.prune_plugin_states();
        }
        removed
    }

    fn from_json(text: &str) -> Result<Self, ProjectError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|error| ProjectError::Format(error.to_string()))?;
        let version = match value.get("schema_version") {
            None => SCHEMA_VERSION,
            Some(version) => version
                .as_u64()
                .and_then(|version| u32::try_from(version).ok())
                .ok_or_else(|| {
                    ProjectError::Format("schema_version must be an unsigned 32-bit integer".into())
                })?,
        };
        if version > SCHEMA_VERSION {
            return Err(ProjectError::UnsupportedVersion(version));
        }
        let project: Self = serde_json::from_value(value)
            .map_err(|error| ProjectError::Format(error.to_string()))?;
        project.validate_chain()?;
        Ok(project)
    }

    fn validate_chain(&self) -> Result<(), ProjectError> {
        let mut seen = BTreeSet::new();
        for entry in &self.chain {
            if !seen.insert(entry.id) {
                return Err(ProjectError::Format(format!(
                    "duplicate chain plugin id {}",
                    entry.id
                )));
            }
        }
        Ok(())
    }

    fn load_if_exists(path: &Path) -> Result<Option<Self>, ProjectError> {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => Self::load(path).map(Some),
            Ok(_) => Err(ProjectError::Io(format!(
                "{} is not a regular file",
                path.display()
            ))),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ProjectError::Io(format!("{}: {error}", path.display()))),
        }
    }
}

fn temp_path(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let counter = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().map_or_else(
        || OsString::from("project.json"),
        |name| name.to_os_string(),
    );
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".{}.{}.tmp", std::process::id(), counter));
    path.with_file_name(temp_name)
}

mod plugin_states_serde {
    use super::PluginState;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub(super) fn serialize<S>(
        states: &BTreeMap<u64, PluginState>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let encoded: BTreeMap<String, String> = states
            .iter()
            .filter(|(_, state)| !state.component.is_empty())
            .map(|(id, state)| (id.to_string(), STANDARD.encode(&state.component)))
            .collect();
        encoded.serialize(serializer)
    }

    pub(super) fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<u64, PluginState>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = BTreeMap::<String, String>::deserialize(deserializer)?;
        let mut states = BTreeMap::new();
        for (id, data) in encoded {
            let id = id.parse::<u64>().map_err(serde::de::Error::custom)?;
            if data.is_empty() {
                continue;
            }
            let component = STANDARD
                .decode(data.as_bytes())
                .map_err(serde::de::Error::custom)?;
            states.insert(id, PluginState { component });
        }
        Ok(states)
    }
}

#[derive(Debug)]
pub enum ProjectError {
    Io(String),
    Format(String),
    UnsupportedVersion(u32),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "I/O error: {message}"),
            Self::Format(message) => write!(formatter, "format error: {message}"),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "unsupported project schema version {version} (latest supported is {SCHEMA_VERSION})"
            ),
        }
    }
}

impl std::error::Error for ProjectError {}

#[cfg(test)]
mod tests;
