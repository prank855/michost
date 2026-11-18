use super::*;

impl MicHostApp {
    pub(crate) fn request_measure(&mut self) {
        if self.measuring {
            return;
        }
        self.start_measurement();
    }

    pub(crate) fn start_measurement(&mut self) {
        let Some(backend) = self.backend.clone() else {
            self.last_error = Some("mic_host.dll is not loaded".to_string());
            return;
        };
        if self.session.is_none() {
            self.last_error = Some("start audio to measure the chain".to_string());
            return;
        }
        let selection = self.project.settings.output.clone();
        let Some(output) = self
            .devices
            .outputs
            .iter()
            .find(|device| {
                device.name == selection.name && device.api == selection.api && device.outputs > 0
            })
            .cloned()
        else {
            self.last_error =
                Some("the configured output device is not available for measurement".to_string());
            return;
        };
        let input_selection = self.project.settings.input.clone();
        let asio_duplex = selection.api == AudioApi::Asio
            && input_selection.api == AudioApi::Asio
            && selection.name == input_selection.name;
        if asio_duplex {
            let Some(input) = self
                .devices
                .inputs
                .iter()
                .find(|device| {
                    device.name == input_selection.name
                        && device.api == input_selection.api
                        && device.inputs > 0
                })
                .cloned()
            else {
                self.last_error = Some(format!(
                    "the selected ASIO input '{}' is not currently available",
                    input_selection.name
                ));
                return;
            };
            let request = MeasureRequest {
                output,
                input,
                output_channels: selection.channels.clamp(1, 2),
                output_mask: selection.channel_mask,
                input_channels: input_selection.channels.clamp(1, 8),
                input_mask: input_selection.channel_mask,
                output_block: self.project.settings.output_buffer_size,
                input_block: self.project.settings.buffer_size,
            };
            self.measuring = true;
            self.last_error = None;
            self.latency_error = None;
            logging::line("latency measurement: pausing audio for the ASIO duplex loopback");
            self.stop_audio_async_with_measure(true, Some(request));
            return;
        }
        let inputs = backend
            .enumerate_devices(DeviceKind::Input)
            .unwrap_or_else(|_| self.devices.inputs.clone());
        let Some(input) = find_loopback_input(&inputs, &output) else {
            self.last_error = Some(format!(
                "no loopback capture found for '{}'; add the cable's capture endpoint or use --selftest-latency",
                output.name
            ));
            return;
        };
        let Some(session) = self.session.as_mut() else {
            self.last_error = Some("start audio to measure the chain".to_string());
            return;
        };
        if let Err(error) = session.begin_chain_measurement() {
            self.last_error = Some(format!("starting the chain measurement: {error}"));
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.measure_worker = Some(receiver);
        self.measuring = true;
        self.last_error = None;
        self.latency_error = None;
        logging::line(format!(
            "latency measurement: chain marker -> '{}'",
            input.name
        ));
        let spawned = std::thread::Builder::new()
            .name("michost-latency".to_string())
            .spawn(move || {
                let result = latency::capture_input(backend.as_ref(), &input, 2);
                if let Err(error) = &result {
                    logging::line(format!("latency measurement: {error}"));
                }
                let _ = sender.send(result);
            });
        if let Err(error) = spawned {
            self.measuring = false;
            self.measure_worker = None;
            self.last_error = Some(format!("starting the latency measurement: {error}"));
        }
    }

    pub(crate) fn poll_measure(&mut self) {
        let Some(receiver) = self.measure_worker.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(capture)) => {
                let clicks = self
                    .session
                    .as_mut()
                    .map(|session| session.end_chain_measurement())
                    .unwrap_or_default();
                match latency::correlate(&capture, &clicks) {
                    Ok(result) => {
                        logging::line(format!(
                            "latency measurement: p50 {:.1} ms, p95 {:.1} ms",
                            result.p50_ms, result.p95_ms
                        ));
                        self.measured = Some(result);
                        self.latency_error = None;
                    }
                    Err(error) => {
                        self.latency_error = Some(error.clone());
                        self.last_error = Some(format!("latency measurement: {error}"));
                    }
                }
                self.measuring = false;
            }
            Ok(Err(error)) => {
                if let Some(session) = self.session.as_mut() {
                    let _ = session.end_chain_measurement();
                }
                self.measuring = false;
                self.latency_error = Some(error.clone());
                self.last_error = Some(format!("latency measurement: {error}"));
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.measure_worker = Some(receiver);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.measuring = false;
                self.last_error = Some("the latency measurement stopped unexpectedly".to_string());
            }
        }
    }
}
