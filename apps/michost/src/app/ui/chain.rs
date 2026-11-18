use super::super::*;

use eframe::egui;
use michost_builtin::BuiltinKind;
use michost_graph::SidechainSource;

impl MicHostApp {
    pub(crate) fn ui_chain(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("Plugins");
            ui.label(egui::RichText::new(format!("{}", self.project.chain.len())).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.add.open && ui.button("Close picker").clicked() {
                    self.add.open = false;
                    self.add.search.clear();
                }
                let scan_label = if self.scan.active {
                    "Scanning..."
                } else {
                    "Scan VST3"
                };
                if ui
                    .add_enabled(!self.scan.active, egui::Button::new(scan_label))
                    .clicked()
                {
                    self.start_scan();
                }
                if ui
                    .button("Folders")
                    .on_hover_text("Manage default and custom VST3 scan folders")
                    .clicked()
                {
                    self.show_vst3_folders = true;
                }
            });
        });
        if let Some(pending) = &self.pending_add {
            ui.horizontal(|ui| {
                ui.spinner();
                let verb = if pending.template.is_some() {
                    "Duplicating"
                } else {
                    "Loading"
                };
                ui.label(format!("{verb} {}...", pending.name));
            });
        }
        if !self.add.open {
            let hint = if self.session.is_some() {
                "Audio flows top to bottom. Drag the grip or use Up/Down to reorder. Bypass passes audio through unchanged."
            } else {
                "Audio flows top to bottom. Start audio to open plugin editors. Drag the grip or use Up/Down to reorder."
            };
            ui.label(egui::RichText::new(hint).weak());
        }
        if self.scan.active {
            ui.horizontal(|ui| {
                ui.spinner();
                if self.scan.total > 0 {
                    ui.label(format!("{}/{} modules", self.scan.done, self.scan.total));
                } else {
                    ui.label("Scanning plugin modules...");
                }
            });
        }

        if self.add.open {
            self.ui_add_plugin(ui);
            return;
        }

        if self.project.chain.is_empty() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(18.0);
                    ui.label(egui::RichText::new("No plugins yet").strong());
                    ui.label(
                        egui::RichText::new(
                            "With no plugins, the microphone passes straight to the output when audio is running. Use + below to add a plugin.",
                        )
                        .weak(),
                    );
                    ui.add_space(18.0);
                });
            });
            self.ui_add_plugin_button(ui);
            return;
        }

        let mut action = None;
        let count = self.project.chain.len();
        let footer = ADD_PLUGIN_BUTTON_HEIGHT + 10.0;
        let scroll_height = (ui.available_height() - footer).max(80.0);
        egui::ScrollArea::vertical()
            .max_height(scroll_height)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for index in 0..count {
                    if let Some(next) = self.ui_chain_row(ui, index) {
                        action = Some(next);
                    }
                }
            });
        if let Some(action) = action {
            self.apply_chain_action(action);
        }
        ui.add_space(4.0);
        self.ui_add_plugin_button(ui);
    }

    pub(crate) fn ui_add_plugin_button(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let add = ui
            .add_enabled_ui(self.pending_add.is_none(), |ui| {
                ui.add_sized(
                    [ui.available_width(), ADD_PLUGIN_BUTTON_HEIGHT],
                    egui::Button::new(egui::RichText::new("+  Add Plugin").size(16.0)),
                )
            })
            .inner;
        if add.clicked() {
            self.add.open = true;
            self.add.focus_search = true;
        }
        add.on_hover_text("Add a plugin to the chain")
            .on_disabled_hover_text("Wait for the plugin to finish loading");
    }

    pub(crate) fn ui_chain_row(&mut self, ui: &mut egui::Ui, index: usize) -> Option<ChainAction> {
        let entry = self.project.chain.get(index)?.clone();
        let count = self.project.chain.len();
        let id = entry.id;
        let name = entry.plugin.name.clone();
        let vendor = entry.plugin.vendor.clone();
        let native_sidechain = entry.plugin.sidechain_inputs;
        let stats = self.session.as_ref().map(|session| {
            let samples = session.plugin_latency(id).unwrap_or(0);
            let milliseconds = session.plugin_latency_ms(id).unwrap_or(0.0);
            let cpu = session.plugin_cpu_fraction(id).unwrap_or(0.0) * 100.0;
            let memory = session
                .plugin_memory_bytes(id)
                .map(|bytes| format!("~{:.0} MB", bytes as f64 / 1_048_576.0))
                .unwrap_or_else(|| "—".to_string());
            (
                samples,
                format!("{milliseconds:.1} ms · CPU {cpu:.1}% · RAM {memory}"),
            )
        });
        let can_edit = self.session.is_some();
        let captured = self.project.settings.input.channels.clamp(1, 8) as u16;
        let second_enabled = self.project.settings.sidechain_input_enabled;
        let aux_channels = self.project.settings.sidechain_input.channels.clamp(1, 8);
        let mut action = None;

        let frame = egui::Frame::group(ui.style()).inner_margin(egui::Margin::symmetric(10, 8));
        let (_, payload) = ui.dnd_drop_zone::<usize, _>(frame, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let ctx = ui.ctx().clone();
                let (grip_rect, grip_response) = ui.allocate_exact_size(
                    egui::vec2(14.0, 18.0),
                    egui::Sense::drag(),
                );
                ui.painter().text(
                    grip_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "::",
                    egui::FontId::monospace(16.0),
                    ui.visuals().weak_text_color(),
                );
                let grip_response = grip_response.on_hover_text("Drag to reorder");
                if grip_response.dragged() {
                    grip_response.dnd_set_drag_payload(index);
                    if let Some(position) = ctx.pointer_interact_pos() {
                        egui::Area::new(egui::Id::new(("chain_drag_preview", id)))
                            .order(egui::Order::Tooltip)
                            .fixed_pos(position + egui::vec2(16.0, 12.0))
                            .show(&ctx, |ui| {
                                egui::Frame::group(ui.style())
                                    .fill(ui.visuals().widgets.active.bg_fill)
                                    .stroke(ui.visuals().widgets.active.bg_stroke)
                                    .inner_margin(egui::Margin::symmetric(10, 8))
                                    .show(ui, |ui| {
                                        ui.set_width(320.0);
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{}. {}",
                                                    index + 1,
                                                    name
                                                ))
                                                .strong(),
                                            );
                                            if !vendor.is_empty() {
                                                ui.label(egui::RichText::new(&vendor).weak());
                                            }
                                        });
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{} in / {} out",
                                                    entry.plugin.main_inputs,
                                                    entry.plugin.main_outputs
                                                ))
                                                .weak(),
                                            );
                                            if entry.bypassed {
                                                ui.label(
                                                    egui::RichText::new("Bypassed").weak(),
                                                );
                                            }
                                            if entry.oversampling > 1 {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "OS {}x",
                                                        entry.oversampling
                                                    ))
                                                    .weak(),
                                                );
                                            }
                                            if entry.sidechain_source
                                                != SidechainSource::Off
                                            {
                                                ui.label(
                                                    egui::RichText::new("Sidechain").weak(),
                                                );
                                            }
                                        });
                                    });
                            });
                    }
                }
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{}. {}", index + 1, name)).strong(),
                    )
                    .truncate(),
                )
                .on_hover_text(&name);
                if !vendor.is_empty() {
                    ui.add(egui::Label::new(egui::RichText::new(vendor).weak()).truncate());
                }
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let bus_text = if entry.plugin.main_inputs == 0 && entry.plugin.main_outputs == 0 {
                    "Bus info after Start".to_string()
                } else {
                    format!(
                        "{} in / {} out",
                        entry.plugin.main_inputs, entry.plugin.main_outputs
                    )
                };
                ui.label(egui::RichText::new(bus_text).weak())
                    .on_hover_text("Bus counts are read from the loaded plugin.");
                let mut bypassed = entry.bypassed;
                if bypass_checkbox(ui, &mut bypassed)
                    .on_hover_text("Pass audio through this plugin without processing")
                    .changed()
                {
                    action = Some(ChainAction::Bypass(index, bypassed));
                }
                let mut oversampling =
                    michost_graph::normalize_oversampling(entry.oversampling);
                egui::ComboBox::from_id_salt(("oversampling", id))
                    .selected_text(match oversampling {
                        2 => "OS 2x",
                        4 => "OS 4x",
                        _ => "OS off",
                    })
                    .width(64.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut oversampling, 1, "OS off");
                        ui.selectable_value(&mut oversampling, 2, "OS 2x");
                        ui.selectable_value(&mut oversampling, 4, "OS 4x");
                    })
                    .response
                    .on_hover_text(
                        "Oversample this plugin only. 2x/4x raise its processing rate; some plugins only support 44.1/48/96 kHz.",
                    );
                if oversampling != entry.oversampling {
                    action = Some(ChainAction::Oversampling(index, oversampling));
                }
                if native_sidechain > 0 {
                    let mut source = entry.sidechain_source;
                    egui::ComboBox::from_id_salt(("sidechain", id))
                        .selected_text(sidechain_label(source))
                        .width(112.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut source, SidechainSource::Off, "Off");
                            ui.selectable_value(&mut source, SidechainSource::MainMic, "Main mic");
                            for channel in 1..captured {
                                ui.selectable_value(
                                    &mut source,
                                    SidechainSource::InputChannel(channel),
                                    format!("Input {}", channel + 1),
                                );
                            }
                            if second_enabled {
                                ui.separator();
                                for channel in 0..aux_channels {
                                    ui.selectable_value(
                                        &mut source,
                                        SidechainSource::SecondInput(channel as u16),
                                        format!("Aux {}", channel + 1),
                                    );
                                }
                            }
                            ui.selectable_value(&mut source, SidechainSource::Silence, "Silence");
                        });
                    if source != entry.sidechain_source {
                        action = Some(ChainAction::Sidechain(index, source));
                    }
                }
                });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if let Some((samples, text)) = &stats {
                    ui.label(egui::RichText::new(text).weak())
                        .on_hover_text(format!("{samples} samples"));
                } else {
                    ui.label(egui::RichText::new("—").weak());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("Remove")
                        .on_hover_text("Remove this plugin from the chain")
                        .clicked()
                    {
                        action = Some(ChainAction::Remove(index));
                    }
                    if ui
                        .add_enabled(index + 1 < count, egui::Button::new("Down"))
                        .on_hover_text("Move down")
                        .clicked()
                    {
                        action = Some(ChainAction::Move(index, index + 1));
                    }
                    if ui
                        .add_enabled(index > 0, egui::Button::new("Up"))
                        .on_hover_text("Move up")
                        .clicked()
                    {
                        action = Some(ChainAction::Move(index, index - 1));
                    }
                    if ui
                        .add_enabled(can_edit, egui::Button::new("Edit"))
                        .on_disabled_hover_text("Start audio to open the plugin editor")
                        .on_hover_text("Open the plugin editor")
                        .clicked()
                    {
                        action = Some(ChainAction::OpenEditor(index));
                    }
                    if ui
                        .add_enabled(
                            self.pending_add.is_none(),
                            egui::Button::new("Duplicate"),
                        )
                        .on_disabled_hover_text("Wait for the plugin to finish loading")
                        .on_hover_text(
                            "Copy this plugin, including its saved state, after itself",
                        )
                        .clicked()
                    {
                        action = Some(ChainAction::Duplicate(index));
                    }
            });
            });
        });
        if let Some(from) = payload {
            if *from != index {
                action = Some(ChainAction::Move(*from, index));
            }
        }
        action
    }

    pub(crate) fn ui_add_plugin(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Add a MicHost built-in effect or pick an installed VST3 plugin.")
                .weak(),
        );
        let search_edit = ui.add(
            egui::TextEdit::singleline(&mut self.add.search)
                .hint_text("Search by name, vendor or category")
                .desired_width(f32::INFINITY),
        );
        if self.add.focus_search {
            search_edit.request_focus();
            self.add.focus_search = false;
        }

        let search = normalize_search(&self.add.search);
        let mut builtin_choice = None;
        if cfg!(debug_assertions) {
            let builtins: Vec<BuiltinKind> = BuiltinKind::ALL
                .into_iter()
                .filter(|kind| {
                    search.is_empty()
                        || descriptor_score(&self.add.search, &builtin_descriptor(*kind)).is_some()
                })
                .collect();
            if !builtins.is_empty() {
                ui.add_space(2.0);
                ui.label(egui::RichText::new("MicHost built-ins").strong());
                ui.horizontal_wrapped(|ui| {
                    for kind in builtins {
                        let response = ui
                            .add_sized([150.0, 24.0], egui::Button::new(kind.name()))
                            .on_hover_text(format!("{} · built-in", kind.category()));
                        if response.clicked() {
                            builtin_choice = Some(kind);
                        }
                    }
                });
                ui.separator();
            }
        }
        if let Some(kind) = builtin_choice {
            self.add.open = false;
            self.add.search.clear();
            self.add.focus_search = false;
            self.begin_add_builtin(kind);
            return;
        }

        if self.plugins.is_empty() {
            ui.add_space(8.0);
            ui.label("No VST3 plugins cached yet.");
            if ui.button("Scan VST3 plugins").clicked() {
                self.start_scan();
            }
            return;
        }
        if !self.scan.active
            && self
                .plugins
                .iter()
                .all(|descriptor| descriptor.category.is_empty())
        {
            ui.label(
                egui::RichText::new(
                    "Plugin types are missing from the cache. Press Scan VST3 to refresh them.",
                )
                .weak(),
            );
        }
        ui.horizontal(|ui| {
            ui.label("Sort by");
            ui.selectable_value(&mut self.add.grouping, PickerGrouping::Vendor, "Vendor");
            ui.selectable_value(&mut self.add.grouping, PickerGrouping::Category, "Category");
        });

        let search = normalize_search(&self.add.search);
        let generation = self.plugin_generation;
        let grouping = self.add.grouping;
        let stale = self.add.picker.as_ref().is_none_or(|cache| {
            cache.generation != generation || cache.search != search || cache.grouping != grouping
        });
        if stale {
            self.add.picker = Some(build_picker_cache(
                &self.plugins,
                generation,
                search,
                grouping,
            ));
        }

        let mut choice = None;
        if let Some(cache) = self.add.picker.as_ref() {
            ui.label(
                egui::RichText::new(format!(
                    "{} plugins cached · {} shown",
                    self.plugins.len(),
                    cache.shown
                ))
                .weak(),
            );
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(ui.available_height())
                .show(ui, |ui| {
                    for (group, entries) in &cache.groups {
                        egui::CollapsingHeader::new(format!("{} ({})", group, entries.len()))
                            .default_open(!cache.search.is_empty())
                            .show(ui, |ui| {
                                for (index, _score) in entries {
                                    let descriptor = &self.plugins[*index];
                                    let category = display_category(&descriptor.category);
                                    let category_detail = if category.is_empty() {
                                        "uncategorised".to_string()
                                    } else {
                                        category.clone()
                                    };
                                    let text = if category.is_empty()
                                        || cache.grouping == PickerGrouping::Category
                                    {
                                        descriptor.name.clone()
                                    } else {
                                        format!("{}  ·  {}", descriptor.name, category)
                                    };
                                    let details = if descriptor.main_inputs == 0
                                        && descriptor.main_outputs == 0
                                    {
                                        format!(
                                            "{} · {} · bus counts are read when the plugin loads",
                                            descriptor.vendor, category_detail
                                        )
                                    } else {
                                        format!(
                                            "{} · {} · {} in / {} out{}",
                                            descriptor.vendor,
                                            category_detail,
                                            descriptor.main_inputs,
                                            descriptor.main_outputs,
                                            if descriptor.sidechain_inputs > 0 {
                                                format!(" · SC {}", descriptor.sidechain_inputs)
                                            } else {
                                                String::new()
                                            }
                                        )
                                    };
                                    if ui
                                        .add_sized(
                                            [ui.available_width(), 26.0],
                                            egui::Button::new(text),
                                        )
                                        .on_hover_text(details)
                                        .clicked()
                                    {
                                        choice = Some(*index);
                                    }
                                }
                            });
                    }
                });
        }
        if let Some(index) = choice {
            let descriptor = self.plugins[index].clone();
            self.add.open = false;
            self.add.search.clear();
            self.add.focus_search = false;
            self.apply_chain_action(ChainAction::Add(descriptor));
        }
    }
}
