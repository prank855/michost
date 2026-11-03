use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use michost_host_abi::{PluginDescriptor, PluginHost};
use michost_host_juce::JuceBackend;
use serde::{Deserialize, Serialize};

use crate::logging;

const PROBE_TIMEOUT: Duration = Duration::from_secs(90);
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PluginCache {
    #[serde(default)]
    pub saved_unix: u64,
    #[serde(default)]
    pub descriptors: Vec<PluginDescriptor>,
    #[serde(default)]
    pub vst3_paths: Vec<String>,
    #[serde(default)]
    pub disabled_vst3_paths: Vec<String>,
}

impl PluginCache {
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
}

#[derive(Clone, Debug)]
pub enum ScanUpdate {
    Progress { done: usize, total: usize },
    Warning(String),
    Done(Vec<PluginDescriptor>),
    Error(String),
}

pub fn start(
    backend: Arc<JuceBackend>,
    vst3_paths: Vec<String>,
    disabled_vst3_paths: Vec<String>,
) -> Receiver<ScanUpdate> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("michost-scan".to_string())
        .spawn(move || scan(backend, sender, vst3_paths, disabled_vst3_paths))
        .ok();
    receiver
}

fn scan_tokens(vst3_paths: &[String], disabled_vst3_paths: &[String]) -> String {
    let mut tokens = vst3_paths.to_vec();
    tokens.extend(disabled_vst3_paths.iter().map(|path| format!("!{path}")));
    tokens.join(";")
}

pub fn same_path(left: &str, right: &str) -> bool {
    left.trim_end_matches(&['\\', '/'][..])
        .eq_ignore_ascii_case(right.trim_end_matches(&['\\', '/'][..]))
}

/// The default locations scanned by the native host. Keep this list in the same
/// order as `searchPaths()` in `cpp/src/scan.cpp`.
pub fn default_vst3_paths() -> Vec<String> {
    fn push(paths: &mut Vec<String>, path: PathBuf) {
        let text = path.display().to_string();
        if !text.is_empty() && !paths.iter().any(|existing| same_path(existing, &text)) {
            paths.push(text);
        }
    }

    let mut paths = Vec::new();
    let common = std::env::var("COMMONPROGRAMFILES")
        .unwrap_or_else(|_| "C:\\Program Files\\Common Files".to_string());
    push(&mut paths, PathBuf::from(common).join("VST3"));

    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let root = PathBuf::from(local);
        push(
            &mut paths,
            root.join("Programs").join("Common").join("VST3"),
        );
        push(&mut paths, root.join("Programs").join("VST3"));
        push(&mut paths, root.join("VST3"));
    }

    for variable in ["PROGRAMFILES", "APPDATA", "USERPROFILE", "PROGRAMDATA"] {
        if let Ok(root) = std::env::var(variable) {
            push(&mut paths, PathBuf::from(root).join("VST3"));
        }
    }

    if let Ok(extra) = std::env::var("VST3_PATH") {
        for token in extra.split(';') {
            if !token.trim().is_empty() {
                push(&mut paths, PathBuf::from(token.trim()));
            }
        }
    }
    paths
}

fn scan(
    backend: Arc<JuceBackend>,
    sender: Sender<ScanUpdate>,
    vst3_paths: Vec<String>,
    disabled_vst3_paths: Vec<String>,
) {
    let mut progress = |_: &PluginDescriptor| {};
    let extra_paths = scan_tokens(&vst3_paths, &disabled_vst3_paths);
    let bundles = match backend.scan_with_paths(&extra_paths, &mut progress) {
        Ok(bundles) => bundles,
        Err(error) => {
            let _ = sender.send(ScanUpdate::Error(format!("scanning plugins: {error}")));
            return;
        }
    };

    let total = bundles.len();
    let mut seen = HashSet::new();
    let mut descriptors = Vec::new();
    for (index, bundle) in bundles.into_iter().enumerate() {
        let _ = sender.send(ScanUpdate::Progress { done: index, total });
        let entries = match probe_descriptions(&bundle.path) {
            Ok(Some(descriptors)) if descriptors.is_empty() => continue,
            Ok(Some(descriptors)) => descriptors,
            Ok(None) => match backend.plugin_descriptions(&bundle.path) {
                Ok(descriptors) if !descriptors.is_empty() => descriptors,
                _ => continue,
            },
            Err(error) => {
                let message = format!("scan skipped {}: {error}", bundle.path);
                logging::line(&message);
                let _ = sender.send(ScanUpdate::Warning(message));
                continue;
            }
        };
        for descriptor in entries {
            let key = (descriptor.class_id.clone(), descriptor.name.clone());
            if descriptor.class_id.is_empty() {
                if seen.contains(&key) {
                    continue;
                }
                seen.insert(key);
            } else if !seen.insert(key) {
                continue;
            }
            descriptors.push(descriptor);
        }
    }

    descriptors.sort_by(|left, right| {
        left.vendor
            .to_lowercase()
            .cmp(&right.vendor.to_lowercase())
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.path.cmp(&right.path))
    });
    let _ = sender.send(ScanUpdate::Progress { done: total, total });
    if descriptors.is_empty() && total > 0 {
        let previous = PluginCache::load()
            .map(|cache| cache.descriptors)
            .unwrap_or_default();
        let message = "the plugin scan produced no descriptors; keeping the previous plugin cache"
            .to_string();
        logging::line(&message);
        let _ = sender.send(ScanUpdate::Warning(message));
        let _ = sender.send(ScanUpdate::Done(previous));
        return;
    }
    let cache = PluginCache {
        saved_unix: now(),
        descriptors: descriptors.clone(),
        vst3_paths,
        disabled_vst3_paths,
    };
    cache.save();
    let _ = sender.send(ScanUpdate::Done(descriptors));
}

fn probe_descriptions(path: &str) -> Result<Option<Vec<PluginDescriptor>>, String> {
    let Some(executable) = probe_executable() else {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            logging::line("michost-probe.exe not found; describing plugins in-process");
        });
        return Ok(None);
    };

    let mut command = Command::new(executable);
    command
        .arg("--describe")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("starting the probe process: {error}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "the probe process has no stdout".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "the probe process has no stderr".to_string())?;
    let (stdout_sender, stdout_receiver) = mpsc::channel();
    let _stdout_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let result = stdout.read_to_string(&mut text);
        let _ = stdout_sender.send((result, text));
    });
    let (stderr_sender, stderr_receiver) = mpsc::channel();
    let _stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        let _ = stderr_sender.send(text);
    });
    let started = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (read_result, text) = stdout_receiver
                    .recv_timeout(PIPE_DRAIN_TIMEOUT)
                    .map_err(|_| "the probe exited but its output pipe stayed open".to_string())?;
                read_result.map_err(|error| format!("reading the probe output: {error}"))?;
                let stderr = stderr_receiver
                    .recv_timeout(PIPE_DRAIN_TIMEOUT)
                    .unwrap_or_default();
                match parse_probe_output(&text) {
                    Ok(descriptors) => {
                        log_probe_warnings(&stderr);
                        return Ok(Some(descriptors));
                    }
                    Err(error) => {
                        let detail = probe_error_detail(&text, &stderr);
                        return Err(format!("probe exited with {status}: {error}; {detail}"));
                    }
                }
            }
            Ok(None) if started.elapsed() >= PROBE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                let stderr = stderr_receiver
                    .recv_timeout(PIPE_DRAIN_TIMEOUT)
                    .unwrap_or_default();
                let detail = probe_error_detail("", &stderr);
                return Err(format!(
                    "probe timed out after {} seconds: {detail}",
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => return Err(format!("waiting for the probe process: {error}")),
        }
    }
}

fn parse_probe_output(text: &str) -> Result<Vec<PluginDescriptor>, String> {
    let json = text
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("MICHPROBE:"))
        .ok_or_else(|| "the probe did not emit a MICHPROBE line".to_string())?;
    serde_json::from_str(json).map_err(|error| format!("parsing the probe output: {error}"))
}

fn log_probe_warnings(stderr: &str) {
    for line in stderr.lines() {
        let line = line.trim();
        if line.starts_with("MICHPROBE_WARN:") {
            logging::line(line);
        }
    }
}

fn probe_error_detail(stdout: &str, stderr: &str) -> String {
    stderr
        .lines()
        .chain(stdout.lines())
        .rev()
        .find(|line| line.trim_start().starts_with("MICHPROBE_ERR:"))
        .or_else(|| stderr.lines().rev().find(|line| !line.trim().is_empty()))
        .or_else(|| stdout.lines().rev().find(|line| !line.trim().is_empty()))
        .map(str::trim)
        .unwrap_or("no output")
        .to_string()
}

fn probe_executable() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "michost-probe.exe"
    } else {
        "michost-probe"
    };
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    if let Ok(current) = std::env::current_dir() {
        for profile in ["debug", "release"] {
            let candidate = current.join("target").join(profile).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
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
        .map(|value| PathBuf::from(value).join("MicHost").join("plugins.json"))
}

#[cfg(test)]
mod tests {
    use super::parse_probe_output;

    #[test]
    fn parses_probe_line_after_noise() {
        let json = r#"[{"path":"C:/x.vst3","class_id":"1","name":"One","vendor":"V","main_inputs":2,"main_outputs":2,"sidechain_inputs":0,"flags":0}]"#;
        let text = format!(
            "plugin noise
MICHPROBE:{json}
"
        );
        let descriptors = parse_probe_output(&text).expect("parse");
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].name, "One");
        assert_eq!(descriptors[0].category, "");
    }

    #[test]
    fn parses_empty_probe_list() {
        let descriptors = parse_probe_output("MICHPROBE:[]").expect("parse");
        assert!(descriptors.is_empty());
    }

    #[test]
    fn rejects_missing_marker() {
        assert!(parse_probe_output("[]").is_err());
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(parse_probe_output("MICHPROBE:{").is_err());
    }

    #[test]
    fn probe_error_detail_prefers_stderr_marker() {
        let detail =
            super::probe_error_detail("plugin noise\n", "warning\nMICHPROBE_ERR:load failed\n");
        assert_eq!(detail, "MICHPROBE_ERR:load failed");
    }

    #[test]
    fn probe_error_detail_falls_back_to_empty() {
        assert_eq!(super::probe_error_detail("", "  \n"), "no output");
    }

    #[test]
    fn scan_paths_compare_case_insensitively_without_trailing_separator() {
        assert!(super::same_path(
            "C:\\Program Files\\Common Files\\VST3",
            "c:\\program files\\common files\\vst3\\"
        ));
        assert!(!super::same_path(
            "C:\\Program Files\\Common Files\\VST3",
            "D:\\VST3"
        ));
    }

    #[test]
    fn scan_tokens_exclude_disabled_defaults() {
        assert_eq!(
            super::scan_tokens(
                &[r"C:\Custom".to_string()],
                &[r"C:\Defaults\VST3".to_string()],
            ),
            r"C:\Custom;!C:\Defaults\VST3"
        );
    }

    #[test]
    fn default_scan_paths_are_unique() {
        let paths = super::default_vst3_paths();
        assert!(!paths.is_empty());
        for (index, path) in paths.iter().enumerate() {
            assert!(
                !paths[..index]
                    .iter()
                    .any(|existing| super::same_path(existing, path)),
                "duplicate default VST3 path {path}"
            );
        }
    }
}
