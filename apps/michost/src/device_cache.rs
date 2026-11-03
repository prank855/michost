use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};

use michost_host_abi::{DeviceInfo, DeviceKind, StreamHost};
use michost_host_juce::JuceBackend;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeviceCache {
    #[serde(default)]
    pub saved_unix: u64,
    #[serde(default)]
    pub inputs: Vec<DeviceInfo>,
    #[serde(default)]
    pub outputs: Vec<DeviceInfo>,
}

impl DeviceCache {
    pub fn load() -> Option<Self> {
        let path = cache_path()?;
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self) {
        let Some(path) = cache_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let temp = path.with_extension("json.tmp");
            if std::fs::write(&temp, json).is_ok() {
                let _ = std::fs::rename(&temp, &path);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.outputs.is_empty()
    }
}

pub fn refresh(backend: Arc<JuceBackend>) -> Receiver<Result<DeviceCache, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("michost-devices".to_string())
        .spawn(move || {
            let result = enumerate(&backend).map(|mut cache| {
                cache.saved_unix = now();
                cache.save();
                cache
            });
            let _ = sender.send(result);
        })
        .ok();
    receiver
}

fn enumerate(backend: &Arc<JuceBackend>) -> Result<DeviceCache, String> {
    let inputs = backend
        .enumerate_devices(DeviceKind::Input)
        .map_err(|error| format!("enumerating inputs: {error}"))?;
    let outputs = backend
        .enumerate_devices(DeviceKind::Output)
        .map_err(|error| format!("enumerating outputs: {error}"))?;
    Ok(DeviceCache {
        saved_unix: now(),
        inputs,
        outputs,
    })
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub fn cache_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(value).join("MicHost").join("devices.json"))
}
