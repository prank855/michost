//! Shared egui widgets used by more than one view.

use eframe::egui;
use michost_project::DeviceSelection;

use super::device_selection::{
    channel_limit, channel_mask_label, default_channel_mask, normalize_selection_channels,
};

pub(crate) fn default_buffer_label(size: Option<u32>) -> String {
    match size {
        Some(size) => format!("Device default ({size})"),
        None => "Device default".to_string(),
    }
}

pub(crate) fn monitor_button(ui: &mut egui::Ui, active: bool, enabled: bool) -> egui::Response {
    let size = egui::vec2(24.0, 22.0);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        if enabled && response.hovered() {
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(4), visuals.weak_bg_fill);
        }
        let color = if !enabled {
            ui.visuals().weak_text_color()
        } else if active {
            egui::Color32::from_rgb(86, 196, 126)
        } else {
            visuals.fg_stroke.color
        };
        if active {
            ui.painter().rect_filled(
                rect,
                egui::CornerRadius::same(4),
                color.gamma_multiply(0.18),
            );
        }

        let center = rect.center();
        let cx = center.x;
        let cy = center.y + 0.75;
        let radius = 6.5;
        let cup_size = egui::vec2(4.0, 6.0);
        let cup_offset = 5.5;

        for offset in [-cup_offset, cup_offset] {
            let cup = egui::Rect::from_center_size(egui::pos2(cx + offset, cy + 2.0), cup_size);
            ui.painter()
                .rect_filled(cup, egui::CornerRadius::same(2), color);
        }

        let mut points = Vec::with_capacity(25);
        for step in 0..=24 {
            let angle = std::f32::consts::PI + std::f32::consts::PI * (step as f32) / 24.0;
            points.push(egui::pos2(
                cx + radius * angle.cos(),
                cy + radius * angle.sin(),
            ));
        }
        ui.painter()
            .add(egui::Shape::line(points, egui::Stroke::new(1.5_f32, color)));
    }
    response
}

/// The first two hidden frames must repaint immediately: eframe force-shows
/// the window once after the first painted frame, so only a follow-up frame
pub(crate) fn bypass_checkbox(ui: &mut egui::Ui, bypassed: &mut bool) -> egui::Response {
    ui.scope(|ui| {
        let strong = ui.visuals().strong_text_color();
        let extreme = ui.visuals().extreme_bg_color;
        let border = egui::Stroke::new(1.5_f32, strong.gamma_multiply(0.9));
        let visuals = ui.visuals_mut();
        visuals.widgets.inactive.bg_fill = extreme;
        visuals.widgets.inactive.bg_stroke = border;
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(2.0_f32, strong);
        visuals.widgets.hovered.bg_stroke = border;
        visuals.widgets.active.bg_stroke = border;
        ui.spacing_mut().icon_width = 18.0_f32;
        ui.spacing_mut().icon_width_inner = 10.0_f32;
        ui.checkbox(bypassed, "Bypass")
    })
    .inner
}

pub(crate) fn header_stat(ui: &mut egui::Ui, label: &str, value: &str, hover: &str) {
    ui.label(egui::RichText::new(label).weak().small())
        .on_hover_text(hover);
    ui.label(egui::RichText::new(value).monospace())
        .on_hover_text(hover);
}

pub(crate) fn lufs_bar(ui: &mut egui::Ui, label: &str, lufs: f32, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [28.0, 14.0],
            egui::Label::new(egui::RichText::new(label).weak().small()),
        );
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(120.0, 10.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, egui::Color32::from_rgb(44, 48, 56));
        let normalized = (lufs.clamp(-60.0, 0.0) + 60.0) / 60.0;
        if normalized > 0.0 {
            let fill = egui::Rect::from_min_size(
                rect.min,
                egui::vec2(rect.width() * normalized, rect.height()),
            );
            painter.rect_filled(fill, 3.0, color);
        }
        let text = if lufs <= -119.0 {
            "—".to_string()
        } else {
            format!("{:>5.1} LUFS", lufs)
        };
        ui.add_sized(
            [74.0, 14.0],
            egui::Label::new(egui::RichText::new(text).monospace().small()),
        );
        response.on_hover_text(
            "Momentary K-weighted loudness (400 ms, BS.1770). IN is before the chain; OUT is after it.",
        );
    });
}

pub(crate) fn short_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut result: String = text.chars().take(max.saturating_sub(1)).collect();
        result.push('…');
        result
    }
}

pub(crate) fn channel_mask_editor(
    ui: &mut egui::Ui,
    id: &str,
    available: u32,
    selection: &mut DeviceSelection,
    max_selected: u32,
) -> bool {
    if available == 0 {
        ui.label(egui::RichText::new("No channels").weak());
        return false;
    }
    let limit = channel_limit(available);
    normalize_selection_channels(selection, available, max_selected);
    let mut mask = default_channel_mask(selection, available);
    let mut changed = false;
    let label = channel_mask_label(mask, available);
    egui::ComboBox::from_id_salt(id)
        .selected_text(label)
        .width(170.0)
        .show_ui(ui, |ui| {
            let effective_max = max_selected.min(available);
            let hint = if effective_max >= available {
                if available == 1 {
                    "Select the only device channel".to_string()
                } else {
                    format!("Select up to {available} device channels")
                }
            } else {
                format!("Select up to {effective_max} of {available} device channels")
            };
            ui.label(egui::RichText::new(hint).weak());
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let count = mask.count_ones();
                        for channel in 0..available.min(64) {
                            let bit = 1u64 << channel;
                            let mut selected = mask & bit != 0;
                            let can_toggle = if selected {
                                count > 1
                            } else {
                                count < max_selected
                            };
                            let response = ui.add_enabled(
                                can_toggle,
                                egui::Checkbox::new(&mut selected, format!("{}", channel + 1)),
                            );
                            if response.changed() {
                                if selected {
                                    mask |= bit;
                                } else {
                                    mask &= !bit;
                                }
                                changed = true;
                            }
                        }
                    });
                });
        });
    if changed {
        if mask == 0 {
            mask = 1;
        }
        selection.channel_mask = mask & limit;
        selection.channels = selection.channel_mask.count_ones().max(1);
    }
    changed
}
