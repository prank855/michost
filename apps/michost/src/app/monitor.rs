use super::*;

impl MicHostApp {
    pub(crate) fn monitor_selection(&self) -> Option<DeviceSelection> {
        let configured = &self.project.settings.monitor_output;
        // A probe enumerates names fresh while audio keeps running, so it wins
        // over a cached list that may still show the old endpoint.
        let outputs = self
            .probed_outputs
            .as_deref()
            .filter(|outputs| !outputs.is_empty())
            .unwrap_or(&self.devices.outputs);
        if !configured.name.is_empty() {
            let device = exact_output(outputs, configured)?;
            return Some(configured_monitor_selection(device, configured));
        }
        let default = default_device(outputs)?;
        if default.name == self.project.settings.output.name {
            return None;
        }
        let device = output_by_name(outputs, &default.name)?;
        Some(selection_for(device, device.outputs.min(2)))
    }

    /// Starts a monitor probe on the device worker. The probe enumerates the
    /// current output list and never writes `devices.json` or stops audio. All
    /// callers share one 30 s throttle, regardless of pending state.
    pub(crate) fn request_monitor_probe(&mut self) {
        if self.device_probe.is_some()
            || self.device_worker.is_some()
            || self.measure_worker.is_some()
        {
            return;
        }
        let now = Instant::now();
        if self
            .monitor_probe_at
            .is_some_and(|at| now.saturating_duration_since(at) < MONITOR_PROBE_INTERVAL)
        {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        self.monitor_probe_at = Some(now);
        if let Some(pending) = self.monitor_pending.as_mut() {
            pending.waiting_for_probe = true;
        }
        self.device_probe = Some((self.device_generation, device_cache::probe_outputs(backend)));
    }

    pub(crate) fn poll_device_probe(&mut self) {
        let Some((generation, receiver)) = self.device_probe.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(outputs)) => {
                if generation != self.device_generation {
                    // A full refresh finished while this probe ran; its list is
                    // newer, so retry against that instead of saving this one.
                    if let Some(pending) = self.monitor_pending.as_mut() {
                        pending.waiting_for_probe = false;
                        pending.next_at = Instant::now();
                    }
                    return;
                }
                let healed =
                    heal_selection(&outputs, &mut self.project.settings.monitor_output, false);
                self.probed_outputs = Some(outputs);
                if let Some(line) = healed {
                    logging::line(line);
                    self.mark_dirty();
                    self.save_now();
                }
                if let Some(pending) = self.monitor_pending.as_mut() {
                    pending.waiting_for_probe = false;
                    pending.next_at = Instant::now();
                }
                self.apply_monitor();
            }
            Ok(Err(error)) => {
                logging::line(format!("monitor: output probe failed: {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.device_probe = Some((generation, receiver));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                logging::line("monitor: the output probe stopped");
            }
        }
    }

    pub(crate) fn apply_monitor(&mut self) {
        self.apply_monitor_named("active");
    }

    /// Starts (or restarts) the monitor on the currently resolved device. A
    /// failure schedules a retry instead of clearing the monitor intent,
    /// touching `last_error` or restarting the engine.
    pub(crate) fn apply_monitor_named(&mut self, verb: &str) {
        if !self.monitor {
            return;
        }
        if self.session.is_none()
            || self.session_worker.is_some()
            || self.stop_worker.is_some()
            || self.device_worker.is_some()
            || self.monitor_open.is_some()
        {
            return;
        }
        let Some(selection) = self.monitor_selection() else {
            if self.project.settings.monitor_output.name.is_empty() {
                // Follow-default with the main output as the system default:
                // the chain is already audible there, so there is no separate
                // monitor stream and nothing to retry.
                self.monitor_pending = None;
                self.monitor_notice = None;
            } else {
                self.monitor_failed(MH_NOT_FOUND, "no monitor output is available");
            }
            return;
        };
        let (active, ready) = match self.session.as_ref() {
            Some(session) => (
                session.monitor_is_active_on(&selection.name, selection.api),
                session.monitor_is_duplex()
                    || session.monitor_is_open_on(&selection.name, selection.api),
            ),
            None => return,
        };
        if active {
            self.monitor_pending = None;
            self.monitor_notice = None;
            return;
        }
        if ready {
            // Duplex or a warmed stream: enabling is just a queue update.
            let result = match self.session.as_mut() {
                Some(session) => session.start_monitor(
                    &selection.name,
                    selection.api,
                    selection.channels,
                    selection.channel_mask,
                ),
                None => return,
            };
            match result {
                Ok(()) => self.monitor_started(&selection.name, verb),
                Err(error) => self.monitor_failed(error.status, &error.message),
            }
            return;
        }
        self.start_monitor_open(selection);
    }

    /// Opens the monitor on a worker; a slow or hung driver must not block the
    /// UI. `poll_monitor_open` installs the result on the UI thread.
    fn start_monitor_open(&mut self, selection: DeviceSelection) {
        let Some(backend) = self.backend.clone() else {
            self.monitor_failed(MH_ERR, "mic_host.dll is not loaded");
            return;
        };
        let device = selection.name.clone();
        let api = selection.api;
        let channels = selection.channels;
        let channel_mask = selection.channel_mask;
        let (sender, receiver) = mpsc::channel();
        let spawn = std::thread::Builder::new()
            .name("michost-monitor".to_string())
            .spawn(move || {
                let result =
                    bridge::prepare_monitor(&backend, &device, api, channels, channel_mask);
                if let Err(std::sync::mpsc::SendError(result)) = sender.send(result)
                    && let Ok(prepared) = result
                {
                    bridge::discard_prepared_monitor(&backend, prepared);
                }
            });
        match spawn {
            Ok(_) => self.monitor_open = Some((selection, receiver)),
            Err(error) => {
                self.monitor_failed(MH_ERR, &format!("starting the monitor worker: {error}"))
            }
        }
    }

    pub(crate) fn poll_monitor_open(&mut self) {
        let Some((selection, receiver)) = self.monitor_open.take() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.monitor_open = Some((selection, receiver));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(MonitorError::new(
                MH_ERR,
                "the monitor worker stopped unexpectedly",
            )),
        };
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.monitor_failed(error.status, &error.message);
                return;
            }
        };
        let desired = self.monitor
            && self.monitor_selection().is_some_and(|current| {
                current.name == selection.name && current.api == selection.api
            });
        if !desired {
            if let Some(backend) = self.backend.as_ref() {
                bridge::discard_prepared_monitor(backend, prepared);
            }
            return;
        }
        let Some(session) = self.session.as_mut() else {
            if let Some(backend) = self.backend.as_ref() {
                bridge::discard_prepared_monitor(backend, prepared);
            }
            return;
        };
        let installed = session.install_prepared_monitor(prepared);
        match installed {
            Ok(()) => self.monitor_started(&selection.name, "active"),
            Err(error) => self.monitor_failed(error.status, &error.message),
        }
    }

    fn monitor_started(&mut self, name: &str, verb: &str) {
        let attempts = self
            .monitor_pending
            .take()
            .map(|pending| pending.attempts)
            .unwrap_or(0);
        self.monitor_notice = None;
        if attempts > 0 {
            logging::line(format!(
                "monitor: recovered on '{name}' after {attempts} attempts"
            ));
        } else {
            logging::line(format!("monitor: {verb} on '{name}'"));
        }
    }

    pub(crate) fn monitor_failed(&mut self, status: i32, message: &str) {
        let now = Instant::now();
        let missing = status == MH_DEVICE_MISSING || status == MH_NOT_FOUND;
        let pending = self.monitor_pending.get_or_insert(MonitorPending {
            next_at: now,
            attempts: 0,
            last_status: None,
            last_message: None,
            waiting_for_probe: false,
        });
        let signature_changed =
            pending.last_status != Some(status) || pending.last_message.as_deref() != Some(message);
        pending.attempts = if signature_changed {
            1
        } else {
            pending.attempts.saturating_add(1)
        };
        pending.last_status = Some(status);
        pending.last_message = Some(message.to_string());
        pending.next_at = now + monitor_retry_delay(pending.attempts);
        pending.waiting_for_probe = missing;
        if signature_changed {
            logging::line(format!("monitor: {message} (status {status})"));
        }
        self.monitor_notice = Some(MONITOR_NOTICE_RETRY.to_string());
        if missing {
            self.request_monitor_probe();
        }
    }

    pub(crate) fn poll_monitor_recovery(&mut self) {
        if !self.monitor {
            return;
        }
        let now = Instant::now();
        let (attempts, waiting, ready) = match self.monitor_pending.as_ref() {
            Some(pending) => (
                pending.attempts,
                pending.waiting_for_probe,
                now >= pending.next_at,
            ),
            None => return,
        };
        if !ready || self.device_probe.is_some() || self.monitor_open.is_some() {
            return;
        }
        if monitor_should_park(attempts, waiting) {
            // Repeated identical failures with no new endpoint list: stop
            // hammering the driver and retry only when a probe or device
            // change reports a new list. The probe itself is rate-limited, so
            // this still picks up an endpoint that appears without a change
            // notification.
            if self.monitor_notice.as_deref() != Some(MONITOR_NOTICE_WAITING) {
                logging::line("monitor: waiting for a device change");
                self.monitor_notice = Some(MONITOR_NOTICE_WAITING.to_string());
            }
            self.request_monitor_probe();
            if let Some(pending) = self.monitor_pending.as_mut() {
                pending.next_at = now + MONITOR_PROBE_INTERVAL;
            }
            return;
        }
        self.apply_monitor();
    }

    pub(crate) fn toggle_monitor(&mut self) {
        if self.monitor {
            self.monitor = false;
            self.project.settings.monitor_enabled = false;
            self.monitor_pending = None;
            self.monitor_notice = None;
            logging::line("monitor: disabled");
            if let Some(session) = self.session.as_mut() {
                session.stop_monitor();
            }
            self.mark_dirty();
            self.save_now();
            return;
        }
        self.monitor = true;
        self.project.settings.monitor_enabled = true;
        self.monitor_pending = None;
        self.monitor_notice = None;
        logging::line("monitor: enabled");
        self.apply_monitor();
        self.mark_dirty();
        self.save_now();
    }

    pub(crate) fn monitor_uses_input_asio(&self) -> bool {
        let monitor = &self.project.settings.monitor_output;
        !monitor.name.is_empty()
            && monitor.api == AudioApi::Asio
            && self.project.settings.input.api == AudioApi::Asio
            && monitor.name == self.project.settings.input.name
    }

    pub(crate) fn monitor_settings_changed(&mut self) {
        self.monitor_pending = None;
        self.monitor_notice = None;
        self.mark_dirty();
        self.save_now();
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.monitor_is_duplex())
            || self.monitor_uses_input_asio()
        {
            self.settings_changed();
            return;
        }
        if let Some(session) = self.session.as_mut() {
            session.close_monitor();
        }
        if !self.monitor {
            return;
        }
        // A failure here keeps the desired monitor selection and schedules a
        // retry; it never disables monitoring or restarts the engine.
        self.apply_monitor();
    }
}
