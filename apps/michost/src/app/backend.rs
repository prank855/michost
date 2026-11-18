use super::*;

impl MicHostApp {
    pub(crate) fn poll_backend(&mut self) {
        let events = self
            .backend
            .as_ref()
            .map(|backend| backend.poll_events())
            .unwrap_or_default();
        for event in events {
            match event {
                michost_host_abi::UiEvent::DeviceError { status, message } => {
                    logging::line(format!("device error {status}: {message}"));
                    self.last_error = Some(message);
                    if status != MH_NOT_SUPPORTED && self.state == EngineState::Running {
                        logging::line("device error; restarting audio");
                        self.auto_retry_at = None;
                        self.stop_audio_async(true);
                    }
                }
                michost_host_abi::UiEvent::LatencyChanged { .. } => {}
            }
        }
    }

    pub(crate) fn poll_latency(&mut self) {
        if self.session.is_none() || self.latency_timer.elapsed() < Duration::from_millis(750) {
            return;
        }
        self.latency_timer = Instant::now();
        let mut endpoint_refresh = None;
        let mut last_input = None;
        let mut last_output = None;
        if let Some(session) = self.session.as_mut() {
            match session.refresh_endpoints() {
                Ok(refresh) => {
                    if refresh.changed {
                        last_input = Some((
                            session.input().device.clone(),
                            session.input().api,
                            session.input().buffer_size,
                        ));
                        last_output = Some((
                            session.output().device.clone(),
                            session.output().api,
                            session.output().buffer_size,
                        ));
                    }
                    endpoint_refresh = Some(refresh);
                }
                Err(error) => {
                    logging::line(format!("refreshing device endpoints: {error}"));
                }
            }
            let rate_changed = endpoint_refresh.is_some_and(|refresh| refresh.rate_changed);
            if !rate_changed {
                match session.refresh_latencies() {
                    Ok(true) => {
                        if let Err(error) = session.rebuild_engine(&self.project) {
                            logging::line(format!("rebuilding for latency: {error}"));
                        }
                    }
                    Ok(false) => {}
                    Err(error) => self.last_error = Some(error),
                }
            }
            session.collect_retired();
        }
        if let Some(value) = last_input {
            self.last_input_buffer = Some(value);
        }
        if let Some(value) = last_output {
            self.last_output_buffer = Some(value);
        }
        if let Some(refresh) = endpoint_refresh {
            if refresh.rate_changed {
                logging::line(
                    "the active device changed sample rate; restarting audio at the device rate",
                );
                self.project.settings.sample_rate = 0.0;
                self.device_draft.sample_rate = 0.0;
                self.mark_dirty();
            }
            let auto_buffer = self.project.settings.buffer_size == 0
                || self.project.settings.output_buffer_size == 0;
            if refresh.rate_changed || (refresh.buffer_changed && auto_buffer) {
                self.stop_audio_async(true);
            }
        }
    }
}
