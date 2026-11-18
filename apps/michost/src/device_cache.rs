use std::path::{Path, PathBuf};
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
        Self::load_from(&path)
    }

    pub fn save(&self) {
        let Some(path) = cache_path() else {
            return;
        };
        self.save_to(&path);
    }

    pub fn load_from(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save_to(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let temp = path.with_extension("json.tmp");
            if std::fs::write(&temp, json).is_ok() {
                let _ = std::fs::rename(&temp, path);
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

/// Enumerates outputs only on a worker thread. This probe never writes
/// `devices.json` and never starts or stops a stream; callers own any decision
/// to run it while audio is live.
#[allow(dead_code)]
pub fn probe_outputs(backend: Arc<JuceBackend>) -> Receiver<Result<Vec<DeviceInfo>, String>> {
    probe_outputs_with(move || {
        backend
            .enumerate_devices(DeviceKind::Output)
            .map_err(|error| format!("probing outputs: {error}"))
    })
}

/// Testable inner form of [`probe_outputs`]: the caller supplies the
/// enumeration so tests can run without a backend. The closure still runs on
/// the `michost-probe` worker thread and the cache is left untouched.
#[allow(dead_code)]
pub fn probe_outputs_with(
    enumerate: impl FnOnce() -> Result<Vec<DeviceInfo>, String> + Send + 'static,
) -> Receiver<Result<Vec<DeviceInfo>, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("michost-probe".to_string())
        .spawn(move || {
            let _ = sender.send(enumerate());
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

#[cfg(test)]
mod tests {
    use super::*;
    use michost_host_abi::AudioApi;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "michost-device-cache-{}-{}",
                std::process::id(),
                id
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn device(name: &str, api: AudioApi, inputs: u32, outputs: u32) -> DeviceInfo {
        DeviceInfo {
            name: name.to_string(),
            id: format!("{api:?}|{name}"),
            inputs,
            outputs,
            default_sample_rate: 48000.0,
            api,
            is_default: false,
            has_control_panel: false,
            buffer_sizes: vec![64, 128, 256],
        }
    }

    #[test]
    fn load_from_missing_file_returns_none() {
        let temp = TempDir::new();
        assert!(DeviceCache::load_from(&temp.path().join("devices.json")).is_none());
    }

    #[test]
    fn save_then_load_round_trips_saved_unix_and_ids() {
        let temp = TempDir::new();
        let path = temp.path().join("devices.json");
        let cache = DeviceCache {
            saved_unix: 1_700_000_123,
            inputs: vec![
                device("Microphone (USB)", AudioApi::WasapiShared, 1, 0),
                device("Line In", AudioApi::Asio, 2, 0),
            ],
            outputs: vec![
                device("Speakers (USB)", AudioApi::WasapiExclusive, 0, 2),
                device("Headphone (MOTU M Series)", AudioApi::Asio, 0, 2),
            ],
        };

        cache.save_to(&path);
        assert!(!path.with_extension("json.tmp").exists());

        let loaded = DeviceCache::load_from(&path).expect("saved cache should load");
        assert_eq!(loaded.saved_unix, cache.saved_unix);
        assert_eq!(loaded.inputs, cache.inputs);
        assert_eq!(loaded.outputs, cache.outputs);
        assert_eq!(loaded.outputs[0].id, "WasapiExclusive|Speakers (USB)");
        assert_eq!(loaded.outputs[1].id, "Asio|Headphone (MOTU M Series)");
    }

    #[test]
    fn probe_with_returns_the_enumerated_outputs() {
        let expected = vec![
            device("Speakers (USB)", AudioApi::WasapiShared, 0, 2),
            device("Headphone (MOTU M Series)", AudioApi::Asio, 0, 2),
        ];
        let outputs = expected.clone();
        let receiver = probe_outputs_with(move || Ok(outputs));
        let result = receiver
            .recv()
            .expect("probe should send a result")
            .expect("probe should succeed");
        assert_eq!(result, expected);
    }

    #[test]
    fn probe_with_propagates_errors() {
        let receiver = probe_outputs_with(|| Err("boom".to_string()));
        let result = receiver.recv().expect("probe should send a result");
        assert_eq!(result.unwrap_err(), "boom");
    }
}
