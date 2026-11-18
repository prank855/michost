use super::super::*;

use eframe::egui;
impl MicHostApp {
    pub(crate) fn ui_devices(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.heading("Audio");
        ui.add_space(6.0);

        let pending = !self.device_draft.matches(&self.project.settings);
        let mut apply = false;
        let mut revert = false;
        if pending {
            egui::Frame::group(ui.style())
                .fill(egui::Color32::from_rgb(60, 52, 24))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new("Device selection changed")
                                .strong()
                                .color(egui::Color32::from_rgb(255, 220, 120)),
                        );
                        ui.label(
                            egui::RichText::new("Apply to restart audio with the new selection.")
                                .color(egui::Color32::from_rgb(230, 210, 160)),
                        );
                        if ui
                            .add_enabled(
                                !self.measuring,
                                egui::Button::new(egui::RichText::new("Apply").strong())
                                    .fill(egui::Color32::from_rgb(210, 145, 40)),
                            )
                            .on_disabled_hover_text("Wait for the latency measurement to finish")
                            .clicked()
                        {
                            apply = true;
                        }
                        if ui.button("Revert").clicked() {
                            revert = true;
                        }
                    });
                });
            ui.add_space(6.0);
        }
        if apply {
            self.commit_device_draft();
        }
        if revert {
            self.device_draft = DeviceDraft::from_settings(&self.project.settings);
        }

        let combo_width = 240.0;

        ui.label(egui::RichText::new("INPUT").strong());
        let advanced = self.project.settings.advanced_mode;
        egui::Grid::new("input_settings")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("API");
                let mut api = self.device_draft.input.api;
                if advanced {
                    let apis = available_apis(&self.devices.inputs, true);
                    if apis.len() <= 1 {
                        ui.label(egui::RichText::new(api_label(api)).weak());
                    } else {
                        egui::ComboBox::from_id_salt("input_api")
                            .selected_text(api_label(api))
                            .width(combo_width)
                            .show_ui(ui, |ui| {
                                for &option in &apis {
                                    ui.selectable_value(&mut api, option, api_label(option));
                                }
                            });
                    }
                    ui.end_row();
                    if api != self.device_draft.input.api {
                        let mut selection = self.device_draft.input.clone();
                        if switch_selection_api(&self.devices.inputs, &mut selection, api, true) {
                            self.device_draft.input = selection;
                        }
                    }
                } else {
                    ui.label(egui::RichText::new(automatic_api_label(api)).weak());
                    ui.end_row();
                }

                ui.label("Device");
                let inputs: Vec<String> = if advanced {
                    self.devices
                        .inputs
                        .iter()
                        .filter(|device| device.api == api && device.inputs > 0)
                        .map(|device| device.name.clone())
                        .collect()
                } else {
                    unique_device_names(&self.devices.inputs, true)
                };
                let selected = if self.device_draft.input.name.is_empty() {
                    "Select input".to_string()
                } else {
                    self.device_draft.input.name.clone()
                };
                if inputs.len() <= 1 {
                    if let Some(only) = inputs.first() {
                        ui.label(egui::RichText::new(only).weak());
                        if only != &selected {
                            self.select_input(only);
                        }
                    } else {
                        ui.label(egui::RichText::new("No devices").weak());
                    }
                } else {
                    let mut chosen = selected.clone();
                    egui::ComboBox::from_id_salt("input_device")
                        .selected_text(&selected)
                        .width(combo_width)
                        .show_ui(ui, |ui| {
                            for name in &inputs {
                                ui.selectable_value(&mut chosen, name.clone(), name);
                            }
                        });
                    if chosen != selected {
                        self.select_input(&chosen);
                    }
                }
                ui.end_row();

                let available_inputs = self
                    .devices
                    .inputs
                    .iter()
                    .find(|device| {
                        device.api == self.device_draft.input.api
                            && device.name == self.device_draft.input.name
                    })
                    .map(|device| device.inputs)
                    .unwrap_or(0);
                ui.label("Channels");
                if available_inputs <= 1 {
                    normalize_selection_channels(&mut self.device_draft.input, available_inputs, 8);
                    ui.label(
                        egui::RichText::new(if available_inputs == 1 {
                            "Ch 1"
                        } else {
                            "No channels"
                        })
                        .weak(),
                    );
                } else {
                    channel_mask_editor(
                        ui,
                        "input_channels",
                        available_inputs,
                        &mut self.device_draft.input,
                        8,
                    );
                }
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.label(egui::RichText::new("OUTPUT").strong());
        egui::Grid::new("output_settings")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("API");
                let mut api = self.device_draft.output.api;
                if advanced {
                    let apis = available_apis(&self.devices.outputs, false);
                    if apis.len() <= 1 {
                        ui.label(egui::RichText::new(api_label(api)).weak());
                    } else {
                        egui::ComboBox::from_id_salt("output_api")
                            .selected_text(api_label(api))
                            .width(combo_width)
                            .show_ui(ui, |ui| {
                                for &option in &apis {
                                    ui.selectable_value(&mut api, option, api_label(option));
                                }
                            });
                    }
                    ui.end_row();
                    if api != self.device_draft.output.api {
                        let mut selection = self.device_draft.output.clone();
                        if switch_selection_api(&self.devices.outputs, &mut selection, api, false) {
                            self.device_draft.output = selection;
                        }
                    }
                } else {
                    ui.label(egui::RichText::new(automatic_api_label(api)).weak());
                    ui.end_row();
                }

                ui.label("Device");
                let outputs: Vec<String> = if advanced {
                    self.devices
                        .outputs
                        .iter()
                        .filter(|device| device.api == api && device.outputs > 0)
                        .map(|device| device.name.clone())
                        .collect()
                } else {
                    unique_device_names(&self.devices.outputs, false)
                };
                let selected = if self.device_draft.output.name.is_empty() {
                    "Select output".to_string()
                } else {
                    self.device_draft.output.name.clone()
                };
                if outputs.len() <= 1 {
                    if let Some(only) = outputs.first() {
                        ui.label(egui::RichText::new(only).weak());
                        if only != &selected {
                            self.select_output(only);
                        }
                    } else {
                        ui.label(egui::RichText::new("No devices").weak());
                    }
                } else {
                    let mut chosen = selected.clone();
                    egui::ComboBox::from_id_salt("output_device")
                        .selected_text(&selected)
                        .width(combo_width)
                        .show_ui(ui, |ui| {
                            for name in &outputs {
                                ui.selectable_value(&mut chosen, name.clone(), name);
                            }
                        });
                    if chosen != selected {
                        self.select_output(&chosen);
                    }
                }
                ui.end_row();

                let available_outputs = self
                    .devices
                    .outputs
                    .iter()
                    .find(|device| {
                        device.api == self.device_draft.output.api
                            && device.name == self.device_draft.output.name
                    })
                    .map(|device| device.outputs)
                    .unwrap_or(0);
                ui.label("Channels");
                if available_outputs <= 1 {
                    normalize_selection_channels(
                        &mut self.device_draft.output,
                        available_outputs,
                        2,
                    );
                    ui.label(
                        egui::RichText::new(if available_outputs == 1 {
                            "Ch 1"
                        } else {
                            "No channels"
                        })
                        .weak(),
                    );
                } else {
                    channel_mask_editor(
                        ui,
                        "output_channels",
                        available_outputs,
                        &mut self.device_draft.output,
                        2,
                    );
                }
                ui.end_row();
            });

        ui.add_space(6.0);
        egui::CollapsingHeader::new("SECOND INPUT (SIDECHAIN)")
            .default_open(self.device_draft.sidechain_input_enabled)
            .show(ui, |ui| {
                let mut enabled = self.device_draft.sidechain_input_enabled;
                if ui
                    .checkbox(&mut enabled, "Enable second input device")
                    .on_hover_text(
                        "The second input is for plugin sidechains; it may use a different device and API.",
                    )
                    .changed()
                {
                    self.device_draft.sidechain_input_enabled = enabled;
                    if enabled && self.device_draft.sidechain_input.name.is_empty() {
                        if let Some(device) = self.devices.inputs.iter().find(|device| {
                            device.inputs > 0
                                && !(device.name == self.device_draft.input.name
                                    && device.api == self.device_draft.input.api)
                        }) {
                            self.device_draft.sidechain_input =
                                selection_for(device, device.inputs.min(2));
                        }
                    }
                }
                ui.add_enabled_ui(enabled, |ui| {
                    egui::Grid::new("sidechain_settings")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("API");
                            let mut api = self.device_draft.sidechain_input.api;
                            if advanced {
                                let apis = available_apis(&self.devices.inputs, true);
                                if apis.len() <= 1 {
                                    ui.label(egui::RichText::new(api_label(api)).weak());
                                } else {
                                    egui::ComboBox::from_id_salt("sidechain_api")
                                        .selected_text(api_label(api))
                                        .width(combo_width)
                                        .show_ui(ui, |ui| {
                                            for &option in &apis {
                                                ui.selectable_value(
                                                    &mut api,
                                                    option,
                                                    api_label(option),
                                                );
                                            }
                                        });
                                }
                                ui.end_row();
                                if api != self.device_draft.sidechain_input.api {
                                    let mut selection =
                                        self.device_draft.sidechain_input.clone();
                                    if switch_selection_api(
                                        &self.devices.inputs,
                                        &mut selection,
                                        api,
                                        true,
                                    ) {
                                        self.device_draft.sidechain_input = selection;
                                    }
                                }
                            } else {
                                ui.label(egui::RichText::new(automatic_api_label(api)).weak());
                                ui.end_row();
                            }

                            ui.label("Device");
                            let inputs: Vec<String> = if advanced {
                                self.devices
                                    .inputs
                                    .iter()
                                    .filter(|device| {
                                        device.api == self.device_draft.sidechain_input.api
                                            && device.inputs > 0
                                    })
                                    .map(|device| device.name.clone())
                                    .collect()
                            } else {
                                unique_device_names(&self.devices.inputs, true)
                            };
                            let selected = if self.device_draft.sidechain_input.name.is_empty() {
                                "Select sidechain".to_string()
                            } else {
                                self.device_draft.sidechain_input.name.clone()
                            };
                            if inputs.len() <= 1 {
                                if let Some(only) = inputs.first() {
                                    ui.label(egui::RichText::new(only).weak());
                                    if only != &selected {
                                        self.select_sidechain_input(only);
                                    }
                                } else {
                                    ui.label(egui::RichText::new("No devices").weak());
                                }
                            } else {
                                let mut chosen = selected.clone();
                                egui::ComboBox::from_id_salt("sidechain_device")
                                    .selected_text(&selected)
                                    .width(combo_width)
                                    .show_ui(ui, |ui| {
                                        for name in &inputs {
                                            ui.selectable_value(&mut chosen, name.clone(), name);
                                        }
                                    });
                                if chosen != selected {
                                    self.select_sidechain_input(&chosen);
                                }
                            }
                            ui.end_row();

                            let available = self
                                .devices
                                .inputs
                                .iter()
                                .find(|device| {
                                    device.api == self.device_draft.sidechain_input.api
                                        && device.name == self.device_draft.sidechain_input.name
                                })
                                .map(|device| device.inputs)
                                .unwrap_or(0);
                            ui.label("Channels");
                            if available <= 1 {
                                normalize_selection_channels(
                                    &mut self.device_draft.sidechain_input,
                                    available,
                                    8,
                                );
                                ui.label(
                                    egui::RichText::new(if available == 1 {
                                        "Ch 1"
                                    } else {
                                        "No channels"
                                    })
                                    .weak(),
                                );
                            } else {
                                channel_mask_editor(
                                    ui,
                                    "sidechain_channels",
                                    available,
                                    &mut self.device_draft.sidechain_input,
                                    8,
                                );
                            }
                            ui.end_row();
                        });
                });
            });

        ui.add_space(6.0);
        ui.label(egui::RichText::new("FORMAT").strong());
        let virtual_mic_output =
            cfg!(debug_assertions) && self.project.settings.output_kind == OutputKind::VirtualMic;
        let input_buffers = self.buffer_options(true);
        let output_buffers = self.buffer_options(false);
        let input_default_label = default_buffer_label(self.actual_buffer(true));
        let output_default_label = default_buffer_label(self.actual_buffer(false));
        let format_width = 150.0;
        ui.horizontal(|ui| {
            if !virtual_mic_output {
                ui.vertical(|ui| {
                    ui.label("Safety buffer").on_hover_text(
                        "Extra output queue. Auto is safest; a smaller value lowers latency but may underrun.",
                    );
                    let mut safety = self.device_draft.safety_buffer_ms;
                    egui::ComboBox::from_id_salt("safety_buffer")
                        .selected_text(if safety == 0 {
                            "Auto".to_string()
                        } else {
                            format!("{safety} ms")
                        })
                        .width(format_width)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut safety, 0, "Auto");
                            for option in [2, 5, 10, 20, 40] {
                                ui.selectable_value(&mut safety, option, format!("{option} ms"));
                            }
                        });
                    self.device_draft.safety_buffer_ms = safety;
                });
                ui.add_space(10.0);
            }
            ui.vertical(|ui| {
                ui.label("Sample rate");
                let mut rate = self.device_draft.sample_rate;
                let actual_rate = self
                    .session
                    .as_ref()
                    .map(|session| session.input().sample_rate)
                    .unwrap_or(0.0);
                egui::ComboBox::from_id_salt("sample_rate")
                    .selected_text(if rate <= 0.0 {
                        if actual_rate > 0.0 {
                            format!("Device default ({actual_rate:.0} Hz)")
                        } else {
                            "Device default".to_string()
                        }
                    } else {
                        format!("{rate:.0} Hz")
                    })
                    .width(format_width)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut rate, 0.0, "Device default");
                        for option in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0] {
                            ui.selectable_value(&mut rate, option, format!("{option:.0} Hz"));
                        }
                    });
                self.device_draft.sample_rate = rate;
            });
        });

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label("Input buffer").on_hover_text(
                    "Input device and graph block. Independent of the output buffer. 0 uses the device default.",
                );
                let mut buffer = self.device_draft.buffer_size;
                egui::ComboBox::from_id_salt("buffer_size")
                    .selected_text(if buffer == 0 {
                        input_default_label.clone()
                    } else {
                        format!("{buffer} frames")
                    })
                    .width(format_width)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut buffer, 0, &input_default_label);
                        for option in &input_buffers {
                            ui.selectable_value(
                                &mut buffer,
                                *option,
                                format!("{option} frames"),
                            );
                        }
                    });
                self.device_draft.buffer_size = buffer;
            });
            if !virtual_mic_output {
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label("Output buffer").on_hover_text(
                        "Output device buffer, independent of the input. Lower values reduce the host output queue but may underrun. 0 uses the device default.",
                    );
                    let mut output_buffer = self.device_draft.output_buffer_size;
                    egui::ComboBox::from_id_salt("output_buffer_size")
                        .selected_text(if output_buffer == 0 {
                            output_default_label.clone()
                        } else {
                            format!("{output_buffer} frames")
                        })
                        .width(format_width)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut output_buffer, 0, &output_default_label);
                            for option in &output_buffers {
                                ui.selectable_value(
                                    &mut output_buffer,
                                    *option,
                                    format!("{option} frames"),
                                );
                            }
                        });
                    self.device_draft.output_buffer_size = output_buffer;
                });
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label("Chain latency").on_hover_text(
                "Run a 2 s marker test. WASAPI cable routing keeps audio running and measures the chain marker to the cable. For ASIO input and output on the same device, audio pauses for 2 s and measures the selected output -> physical loopback -> selected input round trip; that excludes the plugin chain.",
            );
            if self.measuring {
                ui.spinner();
                ui.label(if self.session.is_none() {
                    "Measuring ASIO loopback (audio paused)..."
                } else {
                    "Measuring chain..."
                });
            } else if ui.button("Measure").clicked() {
                self.request_measure();
            }
            if let Some(measured) = &self.measured {
                ui.label(format!("Measured {:.1} ms", measured.p50_ms)).on_hover_text(format!(
                    "Chain-inclusive marker p50 {:.1} ms, p95 {:.1} ms (min {:.1}, max {:.1}); {} matched of {} clicks.
Includes graph, plugin chain, host output queue, output device, cable and capture; excludes the mic input and the receiving app. The marker is briefly audible in the virtual mic.",
                    measured.p50_ms,
                    measured.p95_ms,
                    measured.min_ms,
                    measured.max_ms,
                    measured.matched,
                    measured.clicks
                ));
            }
            if let Some(error) = &self.latency_error {
                ui.label(
                    egui::RichText::new("Measurement failed")
                        .color(egui::Color32::from_rgb(235, 120, 105)),
                )
                .on_hover_text(error);
            }
            if let Some(round_trip) = &self.asio_round_trip {
                ui.label(format!("ASIO loopback {:.1} ms", round_trip.p50_ms))
                    .on_hover_text(format!(
                        "Measured ASIO output -> physical loopback -> ASIO input round trip: p50 {:.1} ms, p95 {:.1} ms (min {:.1}, max {:.1}); {} matched of {} clicks.
Includes the ASIO output and input buffers and the loopback; excludes the plugin chain, host graph and queue. Use the host estimate for total latency. The output and input must be the same ASIO device and the loopback must reach the first selected input channel.",
                        round_trip.p50_ms,
                        round_trip.p95_ms,
                        round_trip.min_ms,
                        round_trip.max_ms,
                        round_trip.matched,
                        round_trip.clicks
                    ));
            }
        });

        ui.add_space(6.0);
        ui.label(egui::RichText::new("STARTUP").strong());
        let mut advanced_mode = self.project.settings.advanced_mode;
        let mut close_to_tray = self.project.settings.close_to_tray;
        let mut launch = self.project.settings.launch_at_login;
        let (advanced_toggled, close_toggled, launch_toggled) = ui
            .horizontal_wrapped(|ui| {
                let advanced_toggled = ui
                    .checkbox(&mut advanced_mode, "Advanced Mode")
                    .on_hover_text(
                        "Show manual API selection for input, output and sidechain devices",
                    )
                    .changed();
                let close_toggled = ui
                    .checkbox(&mut close_to_tray, "Close to tray")
                    .on_hover_text(
                        "Closing the window keeps MicHost and the engine running in the tray",
                    )
                    .changed();
                let launch_toggled = ui.checkbox(&mut launch, "Launch at login").changed();
                (advanced_toggled, close_toggled, launch_toggled)
            })
            .inner;
        if advanced_toggled {
            self.project.settings.advanced_mode = advanced_mode;
            if !advanced_mode {
                let mut candidate = self.project.settings.clone();
                if apply_automatic_apis_to(&self.devices, &mut candidate) {
                    let monitor_changed =
                        candidate.monitor_output != self.project.settings.monitor_output;
                    self.device_draft.input = candidate.input;
                    self.device_draft.output = candidate.output;
                    self.device_draft.sidechain_input = candidate.sidechain_input;
                    if monitor_changed {
                        self.project.settings.monitor_output = candidate.monitor_output;
                        self.monitor_settings_changed();
                    }
                }
            }
            self.mark_dirty();
        }
        if close_toggled {
            self.project.settings.close_to_tray = close_to_tray;
            self.mark_dirty();
        }
        if launch_toggled {
            match autostart::set_enabled(launch) {
                Ok(()) => {
                    self.project.settings.launch_at_login = launch;
                    self.mark_dirty();
                }
                Err(error) => self.last_error = Some(error),
            }
        }
    }
}
