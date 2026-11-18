use super::*;

impl MicHostApp {
    pub(crate) fn start_audio(&mut self) {
        if self.session.is_some() || self.session_worker.is_some() || self.stop_worker.is_some() {
            return;
        }
        if self.device_worker.is_some() {
            self.last_error = Some("wait for the device scan to finish".to_string());
            return;
        }
        let Some(backend) = self.backend.clone() else {
            self.state = EngineState::Error;
            self.last_error = self.backend_error.clone();
            return;
        };

        let mut project = self.project.clone();
        let monitor_selection = self.monitor_selection();
        let duplex_monitor = monitor_selection.as_ref().is_some_and(|monitor| {
            self.project.settings.input.api == AudioApi::Asio
                && monitor.api == AudioApi::Asio
                && monitor.name == self.project.settings.input.name
        });
        let monitor = if self.monitor || duplex_monitor {
            monitor_selection
        } else {
            None
        };
        let mono_source = if self.project.settings.input.channels == 1 {
            Some(0)
        } else {
            None
        };
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.session_worker = Some(receiver);
        self.cancel_flag = cancel.clone();
        self.cancel_start = false;
        self.auto_retry_at = None;
        self.state = EngineState::Starting;
        self.last_error = None;
        self.tooltip = "MicHost - starting".to_string();
        self.set_tray_running(true);
        logging::line("starting audio");

        let spawned = std::thread::Builder::new()
            .name("michost-session".to_string())
            .spawn(move || {
                let result = AudioSession::start_with_monitor(
                    backend,
                    &mut project,
                    mono_source,
                    Some(cancel.as_ref()),
                    monitor.as_ref(),
                )
                .map(|session| (session, project));
                if let Err(error) = &result {
                    logging::line(format!("starting audio: {error}"));
                }
                let _ = sender.send(result);
            });
        if let Err(error) = spawned {
            self.session_worker = None;
            self.state = EngineState::Error;
            self.last_error = Some(format!("starting the session worker: {error}"));
            self.tooltip = "MicHost - error".to_string();
        }
    }

    pub(crate) fn merge_plugin_metadata(&mut self, started: &Project) {
        let mut changed = false;
        for started_entry in &started.chain {
            if let Some(current) = self
                .project
                .chain
                .iter_mut()
                .find(|entry| entry.id == started_entry.id)
            {
                let plugin = &started_entry.plugin;
                if current.plugin.main_inputs != plugin.main_inputs
                    || current.plugin.main_outputs != plugin.main_outputs
                    || current.plugin.sidechain_inputs != plugin.sidechain_inputs
                {
                    current.plugin.main_inputs = plugin.main_inputs;
                    current.plugin.main_outputs = plugin.main_outputs;
                    current.plugin.sidechain_inputs = plugin.sidechain_inputs;
                    changed = true;
                }
            }
        }
        if changed {
            self.mark_dirty();
        }
    }

    pub(crate) fn poll_session_worker(&mut self) {
        let Some(receiver) = self.session_worker.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok((mut session, started_project))) => {
                self.session_worker = None;
                let cancelled = self.cancel_start;
                self.cancel_start = false;
                if cancelled {
                    let _ = std::thread::Builder::new()
                        .name("michost-discard-session".to_string())
                        .spawn(move || drop(session));
                    self.state = EngineState::Stopped;
                    self.auto_retry_at = None;
                    self.tooltip = "MicHost - stopped".to_string();
                    self.set_tray_running(false);
                    self.save_now();
                    let restart = self.stop_restart && !self.engine_paused;
                    self.stop_restart = false;
                    if restart {
                        self.start_audio();
                    }
                    return;
                }
                self.session_epoch = self.session_epoch.wrapping_add(1);
                self.warnings = self.startup_warnings.clone();
                self.warnings.extend(session.warnings.iter().cloned());
                logging::line(format!(
                    "audio running: {} Hz in, {} Hz out, block {}, latency {:.1} ms",
                    session.input().sample_rate as u32,
                    session.output().sample_rate as u32,
                    session.block_size(),
                    session.end_to_end_milliseconds()
                ));
                self.merge_plugin_metadata(&started_project);
                self.last_input_buffer = Some((
                    session.input().device.clone(),
                    session.input().api,
                    session.input().buffer_size,
                ));
                self.last_output_buffer = Some((
                    session.output().device.clone(),
                    session.output().api,
                    session.output().buffer_size,
                ));
                session.clear_plugin_edits();
                session.set_monitor_trim(self.project.settings.monitor_trim_db);
                self.session = Some(session);
                self.state = EngineState::Running;
                self.auto_retry_at = None;
                self.tooltip = "MicHost - running".to_string();
                self.set_tray_running(true);
                self.save_after_session_start();
                if self.monitor {
                    // A failed monitor open keeps the monitor desired and is
                    // retried by the recovery path; it never fails the session.
                    self.apply_monitor_named("enabled");
                }
            }
            Ok(Err(error)) => {
                self.session_worker = None;
                let cancelled = self.cancel_start;
                self.cancel_start = false;
                if cancelled {
                    self.state = EngineState::Stopped;
                    self.auto_retry_at = None;
                    self.tooltip = "MicHost - stopped".to_string();
                } else {
                    self.last_error = Some(error);
                    self.state = EngineState::Error;
                    self.auto_retry_at = Some(Instant::now() + START_RETRY_DELAY);
                    self.tooltip = "MicHost - error".to_string();
                }
                self.set_tray_running(false);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.session_worker = Some(receiver);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.session_worker = None;
                self.cancel_start = false;
                self.last_error = Some("the session worker stopped unexpectedly".to_string());
                self.state = EngineState::Error;
                self.auto_retry_at = Some(Instant::now() + START_RETRY_DELAY);
                self.tooltip = "MicHost - error".to_string();
                self.set_tray_running(false);
            }
        }
    }

    pub(crate) fn stop_audio(&mut self) {
        if self.session_worker.is_some() {
            self.cancel_flag.store(true, Ordering::Release);
            self.cancel_start = true;
            self.state = EngineState::Starting;
            self.tooltip = "MicHost - stopping".to_string();
            self.set_tray_running(false);
            self.mark_dirty();
            self.save_now();
            return;
        }
        if let Some(mut session) = self.session.take() {
            logging::line("audio stopped");
            self.builtin_editors.clear();
            session.stop(Some(&mut self.project));
            self.warnings.extend(session.warnings.iter().cloned());
            self.session_epoch = self.session_epoch.wrapping_add(1);
            self.mark_dirty();
        }
        self.cancel_start = false;
        self.auto_retry_at = None;
        self.state = EngineState::Stopped;
        self.tooltip = "MicHost - stopped".to_string();
        self.set_tray_running(false);
        self.save_now();
    }

    pub(crate) fn stop_audio_async(&mut self, restart: bool) {
        self.stop_audio_async_with_measure(restart, None);
    }

    pub(crate) fn stop_audio_async_with_measure(
        &mut self,
        restart: bool,
        measure: Option<MeasureRequest>,
    ) {
        // The latest normal stop/restart request is authoritative, so a tray
        // Stop can cancel a restart queued behind an in-flight stop. A
        // measurement stop keeps its own restart intent (ASIO duplex resume)
        // but never overrides a newer normal request.
        if measure.is_none() {
            self.stop_restart = restart;
        } else if restart {
            self.stop_restart = true;
        }
        if self.stop_worker.is_some() {
            if measure.is_some() {
                self.last_error = Some("wait for the current audio change to finish".to_string());
                self.measuring = false;
            }
            return;
        }
        if self.session_worker.is_some() {
            if measure.is_some() {
                self.last_error = Some("wait for audio to finish starting".to_string());
                self.measuring = false;
                return;
            }
            self.cancel_flag.store(true, Ordering::Release);
            self.cancel_start = true;
            self.state = EngineState::Stopping;
            self.tooltip = "MicHost - stopping".to_string();
            self.set_tray_running(false);
            self.mark_dirty();
            return;
        }
        let Some(mut session) = self.session.take() else {
            if measure.is_some() {
                self.last_error = Some("start audio to measure the chain".to_string());
                self.measuring = false;
            }
            let restart = self.stop_restart && !self.engine_paused;
            self.stop_restart = false;
            if restart {
                self.start_audio();
            }
            return;
        };
        self.builtin_editors.clear();
        let mut project = self.project.clone();
        let backend = self.backend.clone();
        let (sender, receiver) = mpsc::channel();
        self.stop_worker = Some(receiver);
        self.state = EngineState::Stopping;
        self.tooltip = "MicHost - stopping".to_string();
        self.set_tray_running(false);
        logging::line("audio stopping");
        let spawned = std::thread::Builder::new()
            .name("michost-stop".to_string())
            .spawn(move || {
                session.stop(Some(&mut project));
                let warnings = session.warnings.clone();
                drop(session);
                let measurement = measure.map(|request| {
                    let Some(backend) = backend else {
                        return Err("mic_host.dll is not loaded".to_string());
                    };
                    let block = if request.input_block > 0 {
                        request.input_block
                    } else {
                        request.output_block
                    };
                    latency::measure_duplex(
                        backend.as_ref(),
                        &request.output,
                        &request.input,
                        2,
                        request.output.api,
                        request.output_channels,
                        request.output_mask,
                        request.input_channels,
                        request.input_mask,
                        block,
                    )
                });
                let _ = sender.send(StopOutcome {
                    project,
                    warnings,
                    measurement,
                });
            });
        if let Err(error) = spawned {
            self.stop_worker = None;
            self.stop_restart = false;
            self.measuring = false;
            self.state = EngineState::Error;
            self.last_error = Some(format!("starting the stop worker: {error}"));
            self.tooltip = "MicHost - error".to_string();
        }
    }

    /// Merges the plugin state, warnings and measurement produced by a stop
    /// worker into the live project. Used by the normal poll path and by quit,
    /// which must not save while an in-flight stop still owns the session.
    pub(crate) fn merge_stop_outcome(&mut self, outcome: StopOutcome) {
        for (id, state) in outcome.project.plugin_states {
            if self.project.chain.iter().any(|entry| entry.id == id) {
                self.project.plugin_states.insert(id, state);
            }
        }
        self.project.prune_plugin_states();
        self.warnings.extend(outcome.warnings);
        if let Some(measurement) = outcome.measurement {
            match measurement {
                Ok(measurement) => {
                    self.asio_round_trip = Some(measurement);
                    self.latency_error = None;
                }
                Err(error) => {
                    self.latency_error = Some(error.clone());
                    self.last_error = Some(format!("latency measurement: {error}"));
                }
            }
            self.measuring = false;
        }
    }

    pub(crate) fn poll_stop_worker(&mut self) {
        let Some(receiver) = self.stop_worker.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(outcome) => {
                self.stop_worker = None;
                self.merge_stop_outcome(outcome);
                self.session_epoch = self.session_epoch.wrapping_add(1);
                self.state = EngineState::Stopped;
                self.auto_retry_at = None;
                self.tooltip = "MicHost - stopped".to_string();
                self.set_tray_running(false);
                self.save_after_session_start();
                let rescan = self.rescan_after_stop;
                self.rescan_after_stop = false;
                let restart = self.stop_restart && !self.engine_paused;
                self.stop_restart = false;
                if rescan {
                    self.refresh_devices();
                } else if restart {
                    self.start_audio();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.stop_worker = Some(receiver);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.stop_worker = None;
                let restart = self.stop_restart && !self.engine_paused;
                self.stop_restart = false;
                self.measuring = false;
                self.state = EngineState::Error;
                self.last_error = Some("the stop worker stopped unexpectedly".to_string());
                self.tooltip = "MicHost - error".to_string();
                self.set_tray_running(false);
                if restart {
                    self.start_audio();
                }
            }
        }
    }

    pub(crate) fn ensure_engine(&mut self, now: Instant) {
        if self.quitting || self.engine_paused || self.measuring {
            return;
        }
        if self.session.is_some()
            || self.session_worker.is_some()
            || self.stop_worker.is_some()
            || self.device_worker.is_some()
        {
            return;
        }
        if !self.can_start() {
            return;
        }
        if let Some(retry_at) = self.auto_retry_at {
            if now < retry_at {
                return;
            }
        }
        self.auto_retry_at = None;
        self.start_audio();
    }

    pub(crate) fn open_device_panel(&mut self, input: bool) {
        let Some(session) = self.session.as_ref() else {
            self.last_error = Some("start audio to open device panels".to_string());
            return;
        };
        if let Err(error) = session.control_panel(input) {
            self.last_error = Some(error);
        }
    }
}
