use super::*;

impl MicHostApp {
    pub(crate) fn refresh_devices(&mut self) {
        self.measured = None;
        self.asio_round_trip = None;
        self.latency_error = None;
        if self.device_worker.is_some()
            || self.session_worker.is_some()
            || self.stop_worker.is_some()
        {
            return;
        }
        if self.session.is_some() {
            self.rescan_after_stop = true;
            self.stop_audio_async(false);
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        self.device_error = None;
        self.device_worker = Some(device_cache::refresh(backend));
    }

    pub(crate) fn poll_devices(&mut self) {
        let Some(receiver) = self.device_worker.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(cache)) => {
                let pending_before = !self.device_draft.matches(&self.project.settings);
                self.devices = cache;
                // A completed refresh supersedes any probe started earlier.
                self.device_generation = self.device_generation.wrapping_add(1);
                self.probed_outputs = None;
                self.device_error = None;
                self.device_change_quiet_until = Instant::now() + Duration::from_secs(2);
                if self.apply_automatic_apis() {
                    self.mark_dirty();
                }
                self.apply_device_defaults();
                if self.heal_device_selections() {
                    self.mark_dirty();
                    self.save_now();
                }
                if !pending_before {
                    self.device_draft = DeviceDraft::from_settings(&self.project.settings);
                }
            }
            Ok(Err(error)) => {
                self.device_error = Some(error);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.device_worker = Some(receiver);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.device_error = Some("the device worker stopped".to_string());
            }
        }
    }

    pub(crate) fn apply_device_defaults(&mut self) {
        if self.devices.is_empty() {
            return;
        }
        let mut changed = false;
        if self.project.settings.input.name.is_empty() {
            if let Some(device) = default_device(&self.devices.inputs) {
                self.project.settings.input = selection_for(device, device.inputs.min(2));
                changed = true;
            }
        }
        if self.project.settings.output.name.is_empty() {
            if let Some(device) = default_device(&self.devices.outputs) {
                self.project.settings.output = selection_for(device, device.outputs.min(2));
                changed = true;
            }
        }
        if changed {
            self.mark_dirty();
        }
        if self.apply_automatic_apis() {
            self.mark_dirty();
        }
    }

    pub(crate) fn apply_automatic_apis(&mut self) -> bool {
        if self.project.settings.advanced_mode {
            return false;
        }
        apply_automatic_apis_to(&self.devices, &mut self.project.settings)
    }

    /// Rebinds a selection whose endpoint was renamed by Windows. The monitor
    /// heals live; the input/output/sidechain selections only change while
    /// audio is stopped (changing them restarts the session). Returns true when
    /// anything moved so the caller persists exactly once per device list.
    pub(crate) fn heal_device_selections(&mut self) -> bool {
        let mut healed = false;
        if let Some(line) = heal_selection(
            &self.devices.outputs,
            &mut self.project.settings.monitor_output,
            false,
        ) {
            logging::line(line);
            healed = true;
        }
        if self.session.is_none() {
            if let Some(line) =
                heal_selection(&self.devices.inputs, &mut self.project.settings.input, true)
            {
                logging::line(line);
                healed = true;
            }
            if let Some(line) = heal_selection(
                &self.devices.outputs,
                &mut self.project.settings.output,
                false,
            ) {
                logging::line(line);
                healed = true;
            }
            if self.project.settings.sidechain_input_enabled
                && let Some(line) = heal_selection(
                    &self.devices.inputs,
                    &mut self.project.settings.sidechain_input,
                    true,
                )
            {
                logging::line(line);
                healed = true;
            }
        }
        healed
    }

    pub(crate) fn select_input(&mut self, name: &str) {
        let device = selected_device(
            &self.devices.inputs,
            name,
            self.device_draft.input.api,
            self.project.settings.advanced_mode,
            true,
        )
        .cloned();
        if let Some(device) = device {
            let current = self.device_draft.input.channels;
            let channels = if current == 0 {
                device.inputs.clamp(1, 2)
            } else {
                current.clamp(1, device.inputs.max(1))
            };
            self.device_draft.input = DeviceSelection {
                api: device.api,
                name: device.name,
                id: device.id,
                channels,
                channel_mask: 0,
            };
        }
    }

    pub(crate) fn select_output(&mut self, name: &str) {
        let device = selected_device(
            &self.devices.outputs,
            name,
            self.device_draft.output.api,
            self.project.settings.advanced_mode,
            false,
        )
        .cloned();
        if let Some(device) = device {
            let current = self.device_draft.output.channels;
            let channels = if current == 0 {
                device.outputs.clamp(1, 2)
            } else {
                current.clamp(1, 2)
            };
            self.device_draft.output = DeviceSelection {
                api: device.api,
                name: device.name,
                id: device.id,
                channels,
                channel_mask: 0,
            };
        }
    }

    pub(crate) fn select_sidechain_input(&mut self, name: &str) {
        let device = selected_device(
            &self.devices.inputs,
            name,
            self.device_draft.sidechain_input.api,
            self.project.settings.advanced_mode,
            true,
        )
        .cloned();
        if let Some(device) = device {
            let current = self.device_draft.sidechain_input.channels;
            let channels = if current == 0 {
                device.inputs.clamp(1, 2)
            } else {
                current.clamp(1, device.inputs.max(1))
            };
            self.device_draft.sidechain_input = DeviceSelection {
                api: device.api,
                name: device.name,
                id: device.id,
                channels,
                channel_mask: 0,
            };
        }
    }

    pub(crate) fn commit_device_draft(&mut self) {
        if self.device_draft.matches(&self.project.settings) {
            return;
        }
        if self.measuring {
            self.last_error = Some("wait for the latency measurement to finish".to_string());
            return;
        }
        self.device_draft.apply_to(&mut self.project.settings);
        self.measured = None;
        self.asio_round_trip = None;
        self.latency_error = None;
        self.mark_dirty();
        if self.session.is_some() || self.session_worker.is_some() || self.stop_worker.is_some() {
            self.stop_audio_async(true);
        } else {
            self.auto_retry_at = None;
            self.state = EngineState::Stopped;
        }
    }

    pub(crate) fn settings_changed(&mut self) {
        self.measured = None;
        self.asio_round_trip = None;
        self.latency_error = None;
        let pending_before = !self.device_draft.matches(&self.project.settings);
        if !self.project.settings.advanced_mode && !pending_before && self.apply_automatic_apis() {
            self.device_draft = DeviceDraft::from_settings(&self.project.settings);
        }
        if self.session.is_some() || self.session_worker.is_some() || self.stop_worker.is_some() {
            self.stop_audio_async(true);
        }
        self.mark_dirty();
    }

    pub(crate) fn can_start(&self) -> bool {
        self.backend.is_some()
            && !self.project.settings.input.name.is_empty()
            && ((cfg!(debug_assertions)
                && self.project.settings.output_kind == OutputKind::VirtualMic)
                || !self.project.settings.output.name.is_empty())
            && (!self.project.settings.sidechain_input_enabled
                || !self.project.settings.sidechain_input.name.is_empty())
    }

    pub(crate) fn actual_buffer(&self, input: bool) -> Option<u32> {
        let selection = if input {
            &self.project.settings.input
        } else {
            &self.project.settings.output
        };
        if let Some(session) = &self.session {
            let endpoint = if input {
                session.input()
            } else {
                session.output()
            };
            if endpoint.device == selection.name && endpoint.api == selection.api {
                return Some(endpoint.buffer_size);
            }
        }
        let cached = if input {
            &self.last_input_buffer
        } else {
            &self.last_output_buffer
        };
        cached.as_ref().and_then(|(name, api, size)| {
            (name == &selection.name && *api == selection.api).then_some(*size)
        })
    }

    pub(crate) fn buffer_options(&self, input: bool) -> Vec<u32> {
        let (devices, selection) = if input {
            (&self.devices.inputs, &self.device_draft.input)
        } else {
            (&self.devices.outputs, &self.device_draft.output)
        };
        let current = if input {
            self.device_draft.buffer_size
        } else {
            self.device_draft.output_buffer_size
        };
        let mut sizes = if let Some(device) = devices
            .iter()
            .find(|device| device.name == selection.name && device.api == selection.api)
            && !device.buffer_sizes.is_empty()
        {
            let mut sizes = device.buffer_sizes.clone();
            sizes.sort_unstable();
            sizes.dedup();
            sizes
        } else {
            vec![64, 128, 256, 512, 1024, 2048]
        };
        if current > 0 && !sizes.contains(&current) {
            sizes.push(current);
        }
        if let Some(actual) = self.actual_buffer(input)
            && actual > 0
            && !sizes.contains(&actual)
        {
            sizes.push(actual);
        }
        sizes.sort_unstable();
        sizes.dedup();
        sizes
    }
}
