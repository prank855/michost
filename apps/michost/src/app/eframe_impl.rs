use super::*;

use std::time::{Duration, Instant};

use eframe::egui;
use egui::viewport::ViewportCommand;

impl eframe::App for MicHostApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.options_mut(|options| {
            options.zoom_with_keyboard = false;
        });
        if (ctx.zoom_factor() - 1.0).abs() > f32::EPSILON {
            ctx.set_zoom_factor(1.0);
        }
        let now = Instant::now();
        self.poll_tray(ctx);
        if ctx.input(|input| input.viewport().close_requested()) && !self.quitting {
            if self.project.settings.close_to_tray {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.hide_window(ctx);
            } else {
                logging::line("quit requested by the window close button");
                self.quitting = true;
                self.save_now();
                arm_quit_watchdog();
            }
        }
        if self.activation.as_ref().is_some_and(Activation::poll) {
            self.show_window(ctx);
        }
        self.poll_devices();
        self.poll_scan();
        self.poll_backend();
        self.poll_session_worker();
        self.poll_stop_worker();
        if let Some(session) = self.session.as_mut() {
            session.poll_monitor();
        }
        self.poll_monitor_open();
        self.poll_device_probe();
        self.poll_monitor_recovery();
        self.poll_measure();
        if self.auto_measure
            && self.state == EngineState::Running
            && self.session.is_some()
            && !self.measuring
        {
            self.auto_measure = false;
            self.request_measure();
        }
        if now >= self.plugin_edit_at && self.session.is_some() {
            self.plugin_edit_at = now + Duration::from_millis(500);
            let changed = if let Some(session) = &self.session {
                session.take_plugin_edits(&mut self.project)
            } else {
                false
            };
            if changed {
                self.measured = None;
                let now = Instant::now();
                if now >= self.plugin_autosave_at {
                    self.plugin_autosave_at = now + Duration::from_secs(5);
                    self.mark_dirty();
                } else {
                    self.dirty = true;
                }
            }
        }
        self.poll_pending_add();
        self.poll_latency();
        let device_changes = self
            .backend
            .as_ref()
            .is_some_and(|backend| backend.take_device_changes());
        if device_changes {
            if !self.device_change_pending {
                logging::line("audio device list changed");
                if self.session.is_some() {
                    self.last_error = Some(
                        "audio devices changed; restart audio to apply the new device list"
                            .to_string(),
                    );
                }
            }
            self.device_change_pending = true;
            // A renamed endpoint can break a pending monitor retry while audio
            // runs. The probe enumerates the current output list without
            // writing the cache or stopping the session.
            if self.session.is_some() && self.monitor_pending.is_some() {
                self.request_monitor_probe();
            }
        }
        if self.device_change_pending
            && Instant::now() >= self.device_change_quiet_until
            && self.session.is_none()
            && self.session_worker.is_none()
            && self.device_worker.is_none()
        {
            self.device_change_pending = false;
            self.refresh_devices();
        }
        if let Some(session) = self.session.as_mut() {
            session.collect_retired();
        }

        if let Some(deadline) = self.save_deadline {
            if Instant::now() >= deadline {
                self.save_now();
            }
        }
        if self.dirty && self.save_deadline.is_none() && Instant::now() >= self.plugin_autosave_at {
            self.save_now();
        }
        if self
            .saved_flash_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.saved_flash_until = None;
        }

        #[cfg(debug_assertions)]
        if self.screenshot_path.is_some() {
            self.poll_screenshot(ctx);
        }

        let pointer_only = ctx.input(|input| {
            !input.events.is_empty()
                && input
                    .events
                    .iter()
                    .all(|event| matches!(event, egui::Event::PointerMoved(_)))
                && !input.pointer.any_down()
        });
        if pointer_only {
            let elapsed = self.last_frame.elapsed();
            if elapsed < POINTER_REPAINT_INTERVAL {
                std::thread::sleep(POINTER_REPAINT_INTERVAL - elapsed);
            }
        }
        self.last_frame = Instant::now();

        if !self.first_frame_logged {
            self.first_frame_logged = true;
            logging::line(format!(
                "first frame in {:.1} ms",
                self.created_at.elapsed().as_secs_f64() * 1000.0
            ));
        }

        let minimized = ctx.input(|input| input.viewport().minimized.unwrap_or(false));
        if self.hidden || minimized {
            if minimized {
                self.builtin_editors.clear();
            }
            self.update_tray_tooltip();
            self.ensure_engine(Instant::now());
            if self.hidden && !minimized {
                self.keep_window_hidden(ctx);
            } else {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
            return;
        }

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            self.ui_header(ui);
        });
        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            self.ui_footer(ui);
        });
        egui::SidePanel::left("audio_settings")
            .resizable(false)
            .exact_width(380.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                    )
                    .show(ui, |ui| {
                        self.ui_devices(ui);
                    });
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_chain(ui);
        });

        self.ui_vst3_folders(ctx);
        self.ui_monitor_settings(ctx);
        self.show_builtin_editors(ctx);

        self.update_tray_tooltip();

        self.ensure_engine(Instant::now());

        let flash_active = self.saved_flash_until.is_some();
        let interval = if self.stop_worker.is_some() || self.session_worker.is_some() {
            Duration::from_millis(100)
        } else if self.state == EngineState::Running || flash_active {
            Duration::from_millis(200)
        } else {
            Duration::from_millis(500)
        };
        ctx.request_repaint_after(interval);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.quit_now();
    }
}
