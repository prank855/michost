use std::path::{Path, PathBuf};

fn state_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(|value| {
            PathBuf::from(value)
                .join("MicHost")
                .join("last-project.txt")
        })
}

pub fn read() -> Option<PathBuf> {
    let path = state_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(PathBuf::from(text))
    }
}

pub fn remember(path: &Path) {
    let Some(state) = state_path() else {
        return;
    };
    if let Some(parent) = state.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let temp = state.with_extension("txt.tmp");
    if std::fs::write(&temp, path.display().to_string()).is_ok() {
        let _ = std::fs::rename(&temp, &state);
    }
}

pub fn clear() {
    if let Some(path) = state_path() {
        let _ = std::fs::remove_file(path);
    }
}
