use super::*;

impl MicHostApp {
    pub(crate) fn set_tray_running(&mut self, running: bool) {
        let changed = self.tray_running != running;
        self.tray_running = running;
        if let Some(tray) = &self.tray {
            if changed {
                tray.set_running(running);
            }
            tray.set_tooltip(&self.tooltip);
        }
    }

    pub(crate) fn poll_tray(&mut self, ctx: &egui::Context) {
        let commands = self.tray.as_ref().map(Tray::poll).unwrap_or_default();
        for command in commands {
            match command {
                TrayCommand::Show => self.show_window(ctx),
                TrayCommand::Start => {
                    self.show_window(ctx);
                    self.engine_paused = false;
                    self.auto_retry_at = None;
                }
                TrayCommand::Stop => {
                    self.show_window(ctx);
                    self.engine_paused = true;
                    self.auto_retry_at = None;
                    self.stop_audio_async(false);
                }
                TrayCommand::Quit => {
                    logging::line("quit requested from the tray");
                    self.quitting = true;
                    self.quit_now();
                }
            }
        }
    }

    pub(crate) fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        self.hidden_frames = 0;
        self.hidden_flag.store(false, Ordering::Release);
        crate::activation::show_window();
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    pub(crate) fn hide_window(&mut self, ctx: &egui::Context) {
        self.save_now();
        if let Some(session) = &self.session {
            session.close_editors();
        }
        self.builtin_editors.clear();
        self.hidden = true;
        self.hidden_flag.store(true, Ordering::Release);
        crate::activation::hide_window();
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    /// Keeps a `--hidden`/`--autostart` window out of sight. eframe calls
    /// `set_visible(true)` once after the first painted frame regardless of
    /// `with_visible(false)` (eframe 0.32 `EpiIntegration::post_rendering`), so
    /// the window must be hidden again on each hidden update. The first two
    /// hidden frames repaint immediately to undo that forced show before it
    /// can linger. The shared hidden flag then keeps `HiddenWake` posting
    /// `WM_PAINT` even if eframe briefly left the window visible, so a missed
    /// follow-up frame cannot leave a black window on screen after login.
    pub(crate) fn keep_window_hidden(&mut self, ctx: &egui::Context) {
        self.hidden_flag.store(true, Ordering::Release);
        crate::activation::hide_window();
        if hidden_repaint_now(self.hidden_frames) {
            self.hidden_frames = self.hidden_frames.saturating_add(1);
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    pub(crate) fn update_tray_tooltip(&mut self) {
        if self.tray.is_none() {
            return;
        }
        let tooltip = if self.state == EngineState::Running {
            "MicHost - running"
        } else if self.state == EngineState::Error {
            "MicHost - error"
        } else {
            "MicHost - stopped"
        };
        if tooltip != self.tooltip {
            self.tooltip = tooltip.to_string();
            if let Some(tray) = &self.tray {
                tray.set_tooltip(tooltip);
            }
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn poll_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot_path.clone() else {
            return;
        };
        let image = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let width = image.width() as u32;
            let height = image.height() as u32;
            let mut pixels = Vec::with_capacity(image.pixels.len() * 4);
            for pixel in &image.pixels {
                pixels.extend_from_slice(&pixel.to_array());
            }
            match image::save_buffer(&path, &pixels, width, height, image::ColorType::Rgba8) {
                Ok(()) => logging::line(format!("screenshot saved to {}", path.display())),
                Err(error) => logging::line(format!("saving screenshot: {error}")),
            }
            if let Some(session) = self.session.as_mut() {
                session.quiesce(&mut self.project);
            }
            std::process::exit(0);
        }

        let now = Instant::now();
        if self.screenshot_ready_at.is_none()
            && (self.state == EngineState::Running || now >= self.screenshot_deadline)
        {
            self.screenshot_ready_at = Some(now + Duration::from_millis(500));
        }
        let due = match self.screenshot_ready_at {
            Some(at) => at,
            None => {
                ctx.request_repaint_after(Duration::from_millis(50));
                return;
            }
        };
        if now < due {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        if self
            .screenshot_last_request
            .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            self.screenshot_last_request = Some(now);
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}
