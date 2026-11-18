//! Startup project loading and process-lifetime helpers.

use std::path::PathBuf;
use std::time::Duration;

use michost_project::Project;

use crate::{logging, project_state};

pub(crate) fn arm_quit_watchdog() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(10));
        logging::line("MicHost quit is still running after 10 s; forcing exit");
        std::process::exit(0);
    });
}
/// The first two hidden frames must repaint immediately: eframe force-shows
/// the window once after the first painted frame, so only a follow-up frame
/// can hide it again. Later hidden frames fall back to the 1 s poll.
pub(crate) fn hidden_repaint_now(hidden_frames: u8) -> bool {
    hidden_frames < 2
}

pub(crate) fn strip_builtins(project: &mut Project, allow: bool) -> Vec<String> {
    if allow {
        Vec::new()
    } else {
        project.remove_builtin_entries()
    }
}

pub(crate) fn load_startup_project() -> (Project, Option<PathBuf>, Vec<String>) {
    if let Some(path) = project_state::read() {
        if path.is_file() {
            match Project::load(&path) {
                Ok(mut project) => {
                    let mut warnings = Vec::new();
                    for name in strip_builtins(&mut project, cfg!(debug_assertions)) {
                        logging::line(format!("release build: removed built-in effect '{name}'"));
                        warnings.push(format!(
                            "built-in effect '{name}' is not included in release builds and was removed"
                        ));
                    }
                    logging::line(format!("loaded project {}", path.display()));
                    return (project, Some(path), warnings);
                }
                Err(error) => {
                    logging::line(format!("loading last project {}: {error}", path.display()));
                    project_state::clear();
                }
            }
        } else {
            project_state::clear();
        }
    }
    let mut project = match Project::load_default() {
        Ok(Some(project)) => project,
        Ok(None) => Project::new(),
        Err(error) => {
            logging::line(format!("loading the default project: {error}"));
            Project::new()
        }
    };
    let mut warnings = Vec::new();
    for name in strip_builtins(&mut project, cfg!(debug_assertions)) {
        logging::line(format!("release build: removed built-in effect '{name}'"));
        warnings.push(format!(
            "built-in effect '{name}' is not included in release builds and was removed"
        ));
    }
    (project, None, warnings)
}
pub(crate) fn project_dialog() -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new();
    match Project::default_path().and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
    {
        Some(directory) => dialog.set_directory(directory),
        None => dialog,
    }
}
