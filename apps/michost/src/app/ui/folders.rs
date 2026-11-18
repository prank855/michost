use super::super::*;

use eframe::egui;

impl MicHostApp {
    pub(crate) fn ui_vst3_folders(&mut self, ctx: &egui::Context) {
        if !self.show_vst3_folders {
            return;
        }
        let mut open = self.show_vst3_folders;
        egui::Window::new("VST3 folders")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(
                        "The scanner uses these standard locations plus custom folders. Uncheck a default location to skip it during scans; it stays in this list. VST3_PATH entries appear here too.",
                    )
                    .weak(),
                );
                ui.add_space(6.0);

                ui.label(egui::RichText::new("Default locations").strong());
                let defaults = plugin_cache::default_vst3_paths();
                if defaults.is_empty() {
                    ui.label(egui::RichText::new("No default VST3 locations found.").weak());
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("vst3_default_locations")
                        .max_height(230.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for path in &defaults {
                                let mut enabled = !self
                                    .disabled_vst3_paths
                                    .iter()
                                    .any(|disabled| plugin_cache::same_path(disabled, path));
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(&mut enabled, "")
                                        .on_hover_text("Scan this location")
                                        .changed()
                                    {
                                        if enabled {
                                            self.disabled_vst3_paths.retain(|disabled| {
                                                !plugin_cache::same_path(disabled, path)
                                            });
                                        } else if !self
                                            .disabled_vst3_paths
                                            .iter()
                                            .any(|disabled| plugin_cache::same_path(disabled, path))
                                        {
                                            self.disabled_vst3_paths.push(path.clone());
                                        }
                                        self.save_plugin_paths();
                                    }
                                    ui.add(
                                        egui::Label::new(short_text(path, 74)).truncate(),
                                    )
                                    .on_hover_text(path);
                                    if enabled {
                                        ui.label(egui::RichText::new("scanned").weak().small());
                                    } else {
                                        ui.label(
                                            egui::RichText::new("disabled")
                                                .color(egui::Color32::from_rgb(230, 180, 80))
                                                .small(),
                                        );
                                    }
                                    if !std::path::Path::new(path).exists() {
                                        ui.label(egui::RichText::new("not found").weak().small());
                                    }
                                });
                            }
                        });
                }

                ui.add_space(8.0);
                ui.label(egui::RichText::new("Custom folders").strong());
                let mut remove = None;
                if self.vst3_paths.is_empty() {
                    ui.label(egui::RichText::new("No custom folders.").weak());
                } else {
                    for (index, path) in self.vst3_paths.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(path.as_str()).truncate())
                                .on_hover_text(path);
                            if ui.small_button("Remove").clicked() {
                                remove = Some(index);
                            }
                        });
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Add folder...").clicked()
                        && let Some(folder) = rfd::FileDialog::new().pick_folder()
                    {
                        let path = folder.display().to_string();
                        if !self
                            .vst3_paths
                            .iter()
                            .any(|existing| existing.eq_ignore_ascii_case(&path))
                        {
                            self.vst3_paths.push(path);
                            self.save_plugin_paths();
                        }
                    }
                    if ui.button("Rescan now").clicked() {
                        self.start_scan();
                    }
                });
                if let Some(index) = remove {
                    self.vst3_paths.remove(index);
                    self.save_plugin_paths();
                }
                ui.label(egui::RichText::new("Rescan to apply folder changes.").weak());
            });
        self.show_vst3_folders = open;
    }
}
