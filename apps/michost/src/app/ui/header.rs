use super::super::*;

use eframe::egui;
impl MicHostApp {
    pub(crate) fn ui_header(&mut self, ui: &mut egui::Ui) {
        let loading = self
            .pending_add
            .as_ref()
            .map(|pending| pending.name.clone());
        let retrying_in = self
            .auto_retry_at
            .map(|retry_at| retry_at.saturating_duration_since(Instant::now()));
        ui.horizontal(|ui| {
            let (dot_rect, _) =
                ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(dot_rect.center(), 4.0, self.state.color());
            ui.add(egui::Label::new(
                egui::RichText::new(self.state.label()).strong(),
            ));
            if self.engine_paused {
                ui.label(egui::RichText::new("paused").weak());
            }
            ui.separator();
            match self.state {
                EngineState::Starting => {
                    ui.spinner();
                    let text = if self.cancel_start {
                        "Stopping...".to_string()
                    } else if let Some(progress) = crate::audio_session::startup_progress() {
                        format!("Starting audio... {progress}")
                    } else {
                        "Starting audio...".to_string()
                    };
                    ui.label(text);
                }
                EngineState::Stopping => {
                    ui.spinner();
                    ui.label("Stopping audio...");
                }
                EngineState::Error => {
                    if let Some(remaining) = retrying_in {
                        ui.label(format!("retrying in {:.0} s", remaining.as_secs_f32()));
                    }
                }
                EngineState::Stopped | EngineState::Running => {}
            }
            if !self.device_draft.matches(&self.project.settings) {
                let apply = ui
                    .add_enabled(
                        !self.measuring,
                        egui::Button::new(egui::RichText::new("Apply device changes").strong())
                            .fill(egui::Color32::from_rgb(210, 145, 40)),
                    )
                    .on_hover_text("Restart audio with the selected devices and channels")
                    .on_disabled_hover_text("Wait for the latency measurement to finish");
                if apply.clicked() {
                    self.commit_device_draft();
                }
            }
            let monitor_pending = self.monitor_pending.is_some();
            // Keep the button usable while monitoring is desired so a pending
            // retry can still be cancelled.
            let monitor_available = monitor_pending || self.monitor || self.monitor_selection().is_some();
            let monitor_active = self.monitor;
            let monitor_name = self
                .monitor_selection()
                .map(|selection| selection.name)
                .unwrap_or_default();
            let mut monitor_toggled = false;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = monitor_button(ui, monitor_active, monitor_available);
                let hint = if monitor_pending && monitor_active {
                    if monitor_name.is_empty() {
                        "Retrying the monitor output; audio keeps running. Click to stop monitoring."
                            .to_string()
                    } else {
                        format!(
                            "Retrying the monitor output on '{}'; audio keeps running. Click to stop monitoring.",
                            monitor_name
                        )
                    }
                } else if !monitor_available {
                    "No default playback device is available for monitoring".to_string()
                } else if monitor_active {
                    if monitor_name.is_empty() {
                        "Monitoring is active. Click to stop monitoring.".to_string()
                    } else {
                        format!(
                            "Monitoring on '{}' alongside the configured output. Click to stop monitoring.",
                            monitor_name
                        )
                    }
                } else {
                    format!(
                        "Monitor the chain on '{}'. Use headphones to avoid feedback.",
                        monitor_name
                    )
                };
                let response = response.on_hover_text(hint);
                if response.clicked() {
                    monitor_toggled = true;
                }
                if ui
                    .small_button("Settings")
                    .on_hover_text("Choose the API and device used for monitoring")
                    .clicked()
                {
                    self.show_monitor_settings = true;
                }
                if let Some(name) = &loading {
                    ui.label(format!("Loading {name}..."));
                    ui.spinner();
                }
            });
            if monitor_toggled {
                self.toggle_monitor();
            }
        });

        let red = egui::Color32::from_rgb(235, 105, 105);
        let amber = egui::Color32::from_rgb(230, 180, 80);
        if let Some(error) = &self.backend_error {
            ui.add(
                egui::Label::new(egui::RichText::new(short_text(error, 110)).color(red)).truncate(),
            )
            .on_hover_text(error);
        }
        if let Some(error) = &self.last_error {
            ui.add(
                egui::Label::new(egui::RichText::new(short_text(error, 110)).color(red)).truncate(),
            )
            .on_hover_text(error);
        }
        if let Some(error) = &self.scan.error {
            ui.add(
                egui::Label::new(egui::RichText::new(short_text(error, 110)).color(amber))
                    .truncate(),
            )
            .on_hover_text(error);
        }
        if let Some(error) = &self.device_error {
            ui.add(
                egui::Label::new(egui::RichText::new(short_text(error, 110)).color(amber))
                    .truncate(),
            )
            .on_hover_text(error);
        }
        if !self.warnings.is_empty() {
            egui::CollapsingHeader::new(format!("Warnings ({})", self.warnings.len()))
                .default_open(self.warnings.len() <= 2)
                .show(ui, |ui| {
                    for warning in &self.warnings {
                        ui.label(warning);
                    }
                });
        }

        if let Some(session) = &self.session {
            let input = session.input();
            let output = session.output();
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("IN").weak().small());
                        ui.add(egui::Label::new(short_text(&input.device, 30)).truncate())
                            .on_hover_text(&input.device);
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {:.1} kHz · {} smp",
                                api_label(input.api),
                                input.sample_rate / 1000.0,
                                input.buffer_size
                            ))
                            .weak()
                            .small(),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("OUT").weak().small());
                        ui.add(egui::Label::new(short_text(&output.device, 30)).truncate())
                            .on_hover_text(&output.device);
                        if session.is_virtual_mic_output() {
                            let stats = session.virtual_mic_stats();
                            ui.label(
                                egui::RichText::new("(virtual mic)")
                                    .color(egui::Color32::from_rgb(110, 210, 140))
                                    .small(),
                            )
                            .on_hover_text(format!(
                                "Virtual microphone ring: {} frames buffered; overruns {}; underruns {}.",
                                stats.vmic_buffered_frames,
                                stats.vmic_overruns,
                                stats.vmic_underruns
                            ));
                            ui.label(
                                egui::RichText::new(format!(
                                    "virtual · {:.1} kHz · {} smp packet",
                                    output.sample_rate / 1000.0,
                                    output.buffer_size
                                ))
                                .weak()
                                .small(),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · {:.1} kHz · {} smp",
                                    api_label(output.api),
                                    output.sample_rate / 1000.0,
                                    output.buffer_size
                                ))
                                .weak()
                                .small(),
                            );
                        }
                    });
                });
                let remaining = ui.available_width();
                let meters_width = 250.0;
                if remaining > meters_width + 8.0 {
                    ui.add_space(remaining - meters_width);
                    ui.vertical(|ui| {
                        lufs_bar(
                            ui,
                            "IN",
                            session.input_lufs(),
                            egui::Color32::from_rgb(100, 160, 220),
                        );
                        lufs_bar(
                            ui,
                            "OUT",
                            session.output_lufs(),
                            egui::Color32::from_rgb(90, 200, 150),
                        );
                    });
                }
            });
            let estimated = session.end_to_end_milliseconds();
            let current = session.current_end_to_end_milliseconds();
            let queue = session.queue_milliseconds();
            let target = session.target_queue_milliseconds();
            let breakdown = session.latency_breakdown();
            let measured = self.measured.as_ref();
            let input_path_ms = breakdown.input_device_ms + breakdown.input_buffer_ms;
            let (latency_title, latency_value, latency_hover) = if let Some(measured) = measured {
                let p50 = input_path_ms + measured.p50_ms;
                let p95 = input_path_ms + measured.p95_ms;
                (
                    "Latency",
                    format!("{:>7.1} ms", p50),
                    format!(
                        "Chain-inclusive host latency: mic input device {:.1} + input buffer {:.1} ms + measured chain marker path {:.1} ms = {:.1} ms.
p95 total {:.1} ms (marker p95 {:.1} ms). The marker is injected before the chain and captured at the cable, so graph, plugin chain, host queue, output, cable and capture are included.
Excludes the receiving app and the headphone monitor path. The marker is briefly audible in the virtual mic.",
                        breakdown.input_device_ms,
                        breakdown.input_buffer_ms,
                        measured.p50_ms,
                        p50,
                        p95,
                        measured.p95_ms
                    ),
                )
            } else {
                (
                    "Host estimate",
                    format!("{:>7.1} ms", estimated),
                    format!(
                        "Host estimate from buffers: input {:.1}+{:.1} ms + graph {:.1}+{:.1} ms + output {:.1}+{:.1} ms + queue {:.1} ms.
Current queue {queue:.1} ms (target {target:.1} ms); instantaneous estimate ~{current:.1} ms.
It excludes the cable pipe and capture engine. Use Measure to run the chain-inclusive marker test and get the host-to-cable total.",
                        breakdown.input_device_ms,
                        breakdown.input_buffer_ms,
                        breakdown.graph_ms,
                        breakdown.chain_ms,
                        breakdown.output_device_ms,
                        breakdown.output_buffer_ms,
                        breakdown.output_queue_ms
                    ),
                )
            };
            ui.columns(5, |columns| {
                header_stat(
                    &mut columns[0],
                    latency_title,
                    latency_value.trim(),
                    &latency_hover,
                );
                header_stat(
                    &mut columns[1],
                    "CPU",
                    &format!("{:.1}%", session.cpu_usage()),
                    "1 s average of chain processing time relative to the block period.",
                );
                header_stat(
                    &mut columns[2],
                    "Dropouts",
                    &format!(
                        "{} in / {} out",
                        session.input_dropouts(),
                        session.output_underruns()
                    ),
                    "Input callback drops / output underruns since the session started.",
                );
                header_stat(
                    &mut columns[3],
                    "Block",
                    &format!("{}", session.block_size()),
                    "Graph block size in frames.",
                );
                header_stat(
                    &mut columns[4],
                    "Plugin latency",
                    &format!("{} smp", session.total_latency()),
                    "Reported algorithmic latency of the active plugin chain. This is one component of Latency; the measured marker path also includes the graph, host output queue, output device, cable and capture.",
                );
            });
            if let Some(capture_engine_ms) = session.capture_engine_ms() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Capture engine").weak().small());
                    ui.label(
                        egui::RichText::new(format!(
                            "~{capture_engine_ms:.0} ms (estimate; app-dependent)"
                        ))
                        .monospace(),
                    )
                    .on_hover_text(
                        "Estimated Windows capture-engine period. It is not part of the host total above and is not measured here.",
                    );
                    ui.separator();
                    ui.label(egui::RichText::new("App buffer: unknown").weak().small());
                });
            }
            if let Some(second) = session.second_input() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("SC IN").weak());
                    ui.add(egui::Label::new(short_text(&second.device, 24)).truncate())
                        .on_hover_text(&second.device);
                    ui.label(format!(
                        "{} · {:.1} kHz · buffer {} · starved {} / drops {}",
                        api_label(second.api),
                        second.sample_rate / 1000.0,
                        second.buffer_size,
                        session.second_input_starved(),
                        session.second_input_dropouts()
                    ));
                });
            }
        }
    }
}
