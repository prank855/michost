use super::super::*;

use eframe::egui;

impl MicHostApp {
    pub(crate) fn device_panel_direction(&self) -> Option<bool> {
        if self.devices.inputs.iter().any(|device| {
            device.api == self.project.settings.input.api
                && device.name == self.project.settings.input.name
                && device.has_control_panel
        }) {
            Some(true)
        } else if self.devices.outputs.iter().any(|device| {
            device.api == self.project.settings.output.api
                && device.name == self.project.settings.output.name
                && device.has_control_panel
        }) {
            Some(false)
        } else {
            None
        }
    }

    pub(crate) fn ui_footer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("New project...")
                .on_hover_text("Create a new empty chain and save it to a new project file")
                .clicked()
            {
                self.new_project();
            }
            if ui.button("Save project as...").clicked() {
                self.save_project_as();
            }
            if ui.button("Open project...").clicked() {
                self.open_project();
            }
            if ui
                .button("Rescan")
                .on_hover_text("Stop audio, re-enumerate devices, and refresh the device lists")
                .clicked()
            {
                self.refresh_devices();
            }
            if let Some(input) = self.device_panel_direction() {
                if ui
                    .add_enabled(self.session.is_some(), egui::Button::new("Device panel"))
                    .on_disabled_hover_text("Start audio to open the device control panel")
                    .clicked()
                {
                    self.open_device_panel(input);
                }
            }
            if self.device_worker.is_some() {
                ui.spinner();
            }
            ui.separator();
            let path = self
                .project_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| {
                    Project::default_path()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "no project path".to_string())
                });
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("Project: {}", short_text(&path, 40))).weak(),
                )
                .truncate(),
            )
            .on_hover_text(&path);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.dirty {
                    let status = if self.save_deadline.is_some() {
                        "Saving..."
                    } else {
                        "Unsaved changes"
                    };
                    ui.label(egui::RichText::new(status).weak());
                } else if let Some(until) = self.saved_flash_until {
                    let remaining = until.saturating_duration_since(Instant::now());
                    if !remaining.is_zero() {
                        let alpha = remaining.as_secs_f32().min(1.0);
                        ui.label(
                            egui::RichText::new("Auto-saved").color(
                                egui::Color32::from_rgb(110, 210, 140).gamma_multiply(alpha),
                            ),
                        );
                    }
                }
            });
        });
    }
}
