use super::*;

impl MicHostApp {
    pub(crate) fn quit_now(&mut self) {
        arm_quit_watchdog();
        if let Some(receiver) = self.stop_worker.take() {
            // The stop worker owns the session and the last exported plugin
            // state. Wait for its outcome (bounded by the quit watchdog) so the
            // project saved on exit is not stale.
            match receiver.recv_timeout(QUIT_STOP_WAIT) {
                Ok(outcome) => self.merge_stop_outcome(outcome),
                Err(error) => logging::line(format!(
                    "quit: the audio change did not finish in time ({error}); saving the last known state"
                )),
            }
            self.stop_restart = false;
            self.measuring = false;
        }
        if let Some(session) = self.session.as_mut() {
            session.quiesce(&mut self.project);
        }
        self.dirty = true;
        self.save_now();
        logging::line("MicHost exited");
        std::process::exit(0);
    }

    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
        self.save_deadline = Some(Instant::now() + Duration::from_millis(400));
    }

    /// Persists device metadata after a successful start, unless the loaded
    /// project had built-in entries stripped. In that case the warning is
    /// shown and the user's file is left untouched until a real edit or quit.
    pub(crate) fn save_after_session_start(&mut self) {
        if self.startup_warnings.is_empty() {
            self.mark_dirty();
            self.save_now();
        } else {
            self.dirty = false;
            self.save_deadline = None;
        }
    }

    pub(crate) fn save_now(&mut self) {
        if !self.dirty {
            return;
        }
        let mut saved = true;
        if let Some(path) = self.project_path.clone() {
            if let Err(error) = self.project.save(&path) {
                logging::line(format!("saving project: {error}"));
                self.last_error = Some(error.to_string());
                saved = false;
            }
        }
        match self.project.save_default() {
            Ok(_) => {}
            Err(error) => {
                logging::line(format!("saving the default project: {error}"));
                self.last_error = Some(error.to_string());
                saved = false;
            }
        }
        if saved {
            if let Some(path) = &self.project_path {
                project_state::remember(path);
            }
            self.saved_flash_until = Some(Instant::now() + Duration::from_secs(5));
            self.dirty = false;
        }
        self.save_deadline = None;
    }

    pub(crate) fn new_project(&mut self) {
        if self.stop_worker.is_some() {
            self.last_error = Some("wait for the current audio change to finish".to_string());
            return;
        }
        let Some(path) = project_dialog()
            .add_filter("MicHost project", &[PROJECT_EXTENSION])
            .set_file_name("MicHost.mhchain")
            .save_file()
        else {
            return;
        };
        if self.session.is_some() || self.session_worker.is_some() {
            self.stop_audio();
        }

        let mut project = Project::default();
        project.settings.advanced_mode = self.project.settings.advanced_mode;
        project.settings.close_to_tray = self.project.settings.close_to_tray;
        project.settings.launch_at_login = self.project.settings.launch_at_login;
        if let Some(device) = default_device(&self.devices.inputs) {
            project.settings.input = selection_for(device, device.inputs.min(2));
        }
        if let Some(device) = default_device(&self.devices.outputs) {
            project.settings.output = selection_for(device, device.outputs.min(2));
        }

        if let Err(error) = project.save(&path) {
            logging::line(format!("creating project: {error}"));
            self.last_error = Some(format!("creating project: {error}"));
            return;
        }

        self.next_plugin_id = project.next_plugin_id();
        self.project = project;
        self.device_draft = DeviceDraft::from_settings(&self.project.settings);
        self.project_path = Some(path.clone());
        project_state::remember(&path);
        self.monitor = self.project.settings.monitor_enabled;
        self.startup_warnings.clear();
        self.warnings.clear();
        self.last_error = None;
        self.measured = None;
        self.asio_round_trip = None;
        self.latency_error = None;
        self.measuring = false;
        self.measure_worker = None;
        self.pending_add = None;
        self.add.open = false;
        self.add.picker = None;
        self.last_input_buffer = None;
        self.last_output_buffer = None;
        self.state = EngineState::Stopped;
        self.mark_dirty();
        self.save_now();
        logging::line(format!("new project: {}", path.display()));
    }

    pub(crate) fn open_project(&mut self) {
        if self.stop_worker.is_some() {
            self.last_error = Some("wait for the current audio change to finish".to_string());
            return;
        }
        let Some(path) = project_dialog()
            .add_filter("MicHost project", &[PROJECT_EXTENSION])
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return;
        };
        match Project::load(&path) {
            Ok(mut project) => {
                if self.session.is_some() || self.session_worker.is_some() {
                    self.stop_audio();
                }
                let removed = strip_builtins(&mut project, cfg!(debug_assertions));
                project_state::remember(&path);
                self.next_plugin_id = project.next_plugin_id();
                self.project = project;
                self.device_draft = DeviceDraft::from_settings(&self.project.settings);
                self.project_path = Some(path);
                self.monitor = self.project.settings.monitor_enabled;
                self.measured = None;
                self.asio_round_trip = None;
                self.latency_error = None;
                self.measuring = false;
                self.measure_worker = None;
                self.startup_warnings = removed
                    .iter()
                    .map(|name| {
                        format!(
                            "built-in effect '{name}' is not included in release builds and was removed"
                        )
                    })
                    .collect();
                self.warnings.clear();
                self.warnings.extend(self.startup_warnings.iter().cloned());
                self.last_error = None;
                self.state = EngineState::Stopped;
                if removed.is_empty() {
                    self.mark_dirty();
                    self.save_now();
                } else {
                    // Show the warning instead of rewriting the stripped
                    // project behind the user's back; the next edit saves it.
                    self.dirty = false;
                    self.save_deadline = None;
                }
            }
            Err(error) => {
                logging::line(format!("opening project: {error}"));
                self.last_error = Some(error.to_string());
            }
        }
    }

    pub(crate) fn save_project_as(&mut self) {
        let Some(path) = project_dialog()
            .add_filter("MicHost project", &[PROJECT_EXTENSION])
            .set_file_name("MicHost.mhchain")
            .save_file()
        else {
            return;
        };
        match self.project.save(&path) {
            Ok(()) => {
                project_state::remember(&path);
                self.project_path = Some(path);
                self.mark_dirty();
                self.save_now();
            }
            Err(error) => {
                logging::line(format!("saving project: {error}"));
                self.last_error = Some(error.to_string());
            }
        }
    }
}
