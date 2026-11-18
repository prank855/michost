use super::super::*;

use eframe::egui;

impl MicHostApp {
    pub(crate) fn ui_monitor_settings(&mut self, ctx: &egui::Context) {
        if !self.show_monitor_settings {
            return;
        }
        let mut open = self.show_monitor_settings;
        // Prefer a live probe list so an endpoint Windows renamed is still
        // selectable while the cached list is stale.
        let outputs: &[DeviceInfo] = self
            .probed_outputs
            .as_deref()
            .filter(|outputs| !outputs.is_empty())
            .unwrap_or(&self.devices.outputs);
        let notice = self.monitor_notice.clone();
        let default_output = default_device(outputs).cloned();
        let configured = self.project.settings.monitor_output.clone();
        let mut follow_default = configured.name.is_empty();
        let mut selection = if follow_default {
            default_output
                .as_ref()
                .map(|device| selection_for(device, device.outputs.min(2)))
                .unwrap_or_default()
        } else {
            configured.clone()
        };
        let main_output = self.project.settings.output.clone();
        let mut monitor_trim = self.project.settings.monitor_trim_db.clamp(-24.0, 12.0);
        let mut trim_changed = false;
        let mut changed = false;

        egui::Window::new("Monitor output")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(450.0)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Plays the processed chain on a second output. Use headphones to avoid feedback.",
                    )
                    .weak(),
                );
                ui.label(
                    egui::RichText::new(
                        "If the interface's direct monitoring or loopback is on, its dry input is mixed with this processed monitor. Turn it off in the device control panel when comparing against the WASAPI output.",
                    )
                    .weak(),
                );
                ui.add_space(4.0);
                if let Some(notice) = &notice {
                    ui.label(
                        egui::RichText::new(notice.as_str())
                            .color(egui::Color32::from_rgb(230, 180, 80)),
                    );
                }
                if ui
                    .checkbox(
                        &mut follow_default,
                        "Use the system default playback device",
                    )
                    .changed()
                {
                    if follow_default {
                        selection = default_output
                            .as_ref()
                            .map(|device| selection_for(device, device.outputs.min(2)))
                            .unwrap_or_default();
                    } else if let Some(device) = outputs
                        .iter()
                        .find(|device| device.outputs > 0 && device.name != main_output.name)
                        .or_else(|| outputs.iter().find(|device| device.outputs > 0))
                    {
                        selection = selection_for(device, device.outputs.min(2));
                    }
                    changed = true;
                }

                if follow_default {
                    ui.label(
                        egui::RichText::new(format!(
                            "System default: {}",
                            default_output
                                .as_ref()
                                .map(|device| device.name.as_str())
                                .unwrap_or("not available")
                        ))
                        .weak(),
                    );
                } else {
                    ui.horizontal(|ui| {
                        ui.label("API");
                        let apis = available_apis(outputs, false);
                        if apis.len() <= 1 {
                            ui.label(egui::RichText::new(api_label(selection.api)).weak());
                        } else {
                            egui::ComboBox::from_id_salt("monitor_api")
                                .selected_text(api_label(selection.api))
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    for api in apis {
                                        let selected = selection.api == api;
                                        if ui.selectable_label(selected, api_label(api)).clicked()
                                            && !selected
                                            && switch_selection_api(
                                                outputs,
                                                &mut selection,
                                                api,
                                                false,
                                            )
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Device");
                        let selected = if selection.name.is_empty() {
                            "Select device".to_string()
                        } else {
                            short_text(&selection.name, 34)
                        };
                        let selected_api = selection.api;
                        let mut devices: Vec<_> = outputs
                            .iter()
                            .filter(|device| {
                                device.outputs > 0 && device.api == selected_api
                            })
                            .collect();
                        if devices.len() <= 1 {
                            if let Some(device) = devices.first() {
                                ui.label(
                                    egui::RichText::new(short_text(&device.name, 40))
                                        .weak(),
                                );
                                if selection.name != device.name {
                                    selection = selection_for(device, device.outputs.min(2));
                                    changed = true;
                                }
                            } else {
                                ui.label(egui::RichText::new("No devices").weak());
                            }
                        } else {
                            egui::ComboBox::from_id_salt("monitor_device")
                                .selected_text(selected)
                                .width(240.0)
                                .show_ui(ui, |ui| {
                                    for device in devices.drain(..) {
                                        if ui
                                            .selectable_value(
                                                &mut selection.name,
                                                device.name.clone(),
                                                short_text(&device.name, 40),
                                            )
                                            .on_hover_text(&device.name)
                                            .changed()
                                        {
                                            selection = selection_for(
                                                device,
                                                device.outputs.min(2),
                                            );
                                            changed = true;
                                        }
                                    }
                                });
                        }
                    });
                    let max_outputs = outputs
                        .iter()
                        .find(|device| {
                            device.outputs > 0
                                && device.name == selection.name
                                && device.api == selection.api
                        })
                        .map(|device| device.outputs)
                        .unwrap_or(0);
                    ui.horizontal(|ui| {
                        ui.label("Output");
                        if max_outputs <= 1 {
                            normalize_selection_channels(&mut selection, max_outputs, 2);
                            ui.label(
                                egui::RichText::new(if max_outputs == 1 {
                                    "Ch 1"
                                } else {
                                    "No channels"
                                })
                                .weak(),
                            );
                        } else {
                            changed |= channel_mask_editor(
                                ui,
                                "monitor_channels",
                                max_outputs,
                                &mut selection,
                                2,
                            );
                        }
                    });
                    if outputs.iter().all(|device| {
                        device.outputs == 0
                            || device.name != selection.name
                            || device.api != selection.api
                    }) {
                        ui.label(
                            egui::RichText::new("This monitor device is not currently available.")
                                .color(egui::Color32::from_rgb(230, 180, 80)),
                        );
                    }
                    if selection.name == main_output.name && selection.api == main_output.api {
                        ui.label(
                            egui::RichText::new(
                                "This is also the main output; use headphones or choose another device.",
                            )
                            .color(egui::Color32::from_rgb(230, 180, 80)),
                        );
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Trim").on_hover_text(
                        "Monitor-only level. ASIO bypasses the Windows endpoint volume; use this to match the WASAPI output. The main output is not affected.",
                    );
                    if ui
                        .add(
                            egui::Slider::new(&mut monitor_trim, -24.0..=12.0)
                                .suffix(" dB")
                                .fixed_decimals(1),
                        )
                        .changed()
                    {
                        trim_changed = true;
                    }
                    if ui.small_button("Reset").clicked() {
                        monitor_trim = 0.0;
                        trim_changed = true;
                    }
                });
            });

        self.show_monitor_settings = open;
        if trim_changed {
            self.project.settings.monitor_trim_db = monitor_trim;
            if let Some(session) = self.session.as_mut() {
                session.set_monitor_trim(monitor_trim);
            }
            self.mark_dirty();
        }
        if changed {
            self.project.settings.monitor_output = if follow_default {
                DeviceSelection::default()
            } else {
                selection
            };
            self.monitor_settings_changed();
        }
    }
}
