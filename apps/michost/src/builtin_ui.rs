use std::sync::Arc;
use std::time::Duration;

use egui::{
    Align, Align2, Color32, FontId, Frame, Layout, Margin, Mesh, Pos2, Rect, Response, RichText,
    Sense, Shape, Stroke, StrokeKind, Vec2,
};
use michost_builtin::{
    AnalysisSnapshot, AudioAnalysis, BuiltinKind, ParamSet, ParamSpec, ParamStyle, eq,
};

const ACCENT: Color32 = Color32::from_rgb(0x4E, 0xA1, 0xFF);
const SPECTRUM: Color32 = Color32::from_rgb(0x6F, 0x8C, 0xFF);
const SPECTRUM_LINE: Color32 = Color32::from_rgb(0xC3, 0xD3, 0xFF);
const SPECTRUM_PRE: Color32 = Color32::from_rgb(0x55, 0x66, 0x7E);
const SPECTRUM_PRE_LINE: Color32 = Color32::from_rgb(0x9A, 0xAA, 0xC2);
const EQ_CURVE: Color32 = Color32::from_rgb(0xF3, 0xC7, 0x5C);
const GR_COLOR: Color32 = Color32::from_rgb(0xF5, 0xB8, 0x4E);
const INPUT_LINE: Color32 = Color32::from_rgb(0xB6, 0xC3, 0xD8);
const BACKGROUND: Color32 = Color32::from_rgb(0x14, 0x16, 0x1B);
const GRAPH_TOP: Color32 = Color32::from_rgb(0x0D, 0x11, 0x19);
const GRAPH_BOTTOM: Color32 = Color32::from_rgb(0x08, 0x0A, 0x0F);
const KNOB_FILL: Color32 = Color32::from_rgb(0x2A, 0x2F, 0x3A);
const BORDER: Color32 = Color32::from_rgb(0x33, 0x39, 0x45);
const TRACK: Color32 = Color32::from_rgb(0x3A, 0x41, 0x4F);
const TEXT: Color32 = Color32::from_rgb(0xE4, 0xE9, 0xF2);
const MUTED: Color32 = Color32::from_rgb(0x8E, 0x99, 0xAC);

const KNOB_SIZE: f32 = 60.0;
const CELL_HEIGHT: f32 = 108.0;
/// Bottom-left of the knob arc, in screen coordinates (y grows downwards).
const KNOB_START: f32 = 3.0 * std::f32::consts::FRAC_PI_4;
/// 270 degrees of travel, ending at the bottom-right.
const KNOB_SWEEP: f32 = 1.5 * std::f32::consts::PI;
const DRAG_SENSITIVITY: f32 = 220.0;
const FINE_DRAG_SENSITIVITY: f32 = 900.0;
const WHEEL_SENSITIVITY: f32 = 500.0;

const FFT_SIZE: usize = 2048;
const SPECTRUM_BINS: usize = 256;
const SPECTRUM_MIN_HZ: f32 = 20.0;
const SPECTRUM_MAX_HZ: f32 = 20_000.0;
const SPECTRUM_FLOOR_DB: f32 = -66.0;
const GRAPH_TOP_DB: f32 = 0.0;
const GRAPH_BOTTOM_DB: f32 = -60.0;
const SPECTRUM_ATTACK: f32 = 0.5;
const SPECTRUM_RELEASE: f32 = 0.07;
const SPECTRUM_TILT_DB_PER_OCTAVE: f32 = 4.5;
const EQ_RESPONSE_MAX_DB: f32 = 24.0;
const EQ_RESPONSE_MIN_DB: f32 = -24.0;
const TOP_PAD: f32 = 8.0;
const CONTROL_TOP_FRACTION: f32 = 0.42;
const CONTROL_SCRIM_TOP_ALPHA: u8 = 140;
const CONTROL_SCRIM_BOTTOM_ALPHA: u8 = 230;
const HISTORY_LEN: usize = 256;
const HISTORY_INTERVAL: f64 = 1.0 / 30.0;
const HISTORY_CATCHUP_LIMIT: usize = 4;
const HISTORY_RESYNC_SECONDS: f64 = 0.5;
const LEVEL_ATTACK_SECONDS: f32 = 0.06;
const LEVEL_RELEASE_SECONDS: f32 = 0.35;
const REDUCTION_ATTACK_SECONDS: f32 = 0.08;
const REDUCTION_RELEASE_SECONDS: f32 = 0.50;

pub struct BuiltinEditor {
    pub entry_id: u64,
    pub kind: BuiltinKind,
    pub params: Arc<ParamSet>,
    pub analysis: Arc<AudioAnalysis>,
}

impl BuiltinEditor {
    pub fn new(
        entry_id: u64,
        kind: BuiltinKind,
        params: Arc<ParamSet>,
        analysis: Arc<AudioAnalysis>,
    ) -> Self {
        Self {
            entry_id,
            kind,
            params,
            analysis,
        }
    }
}

/// Shows one built-in editor as a floating window inside the main MicHost
/// window and returns false when the user closes it.
///
/// The editor deliberately does not use an egui viewport: on this eframe/glow
/// backend deferred viewports render an empty window and immediate viewports
/// pause the parent, which made the app look frozen. A floating window keeps
/// the main UI responsive and preserves the single-window VST-like layout.
pub fn show(ctx: &egui::Context, editor: &BuiltinEditor) -> bool {
    let screen = ctx.screen_rect();
    let desired = window_size(editor.kind);
    let size = egui::vec2(
        desired[0].min((screen.width() - 48.0).max(420.0)),
        desired[1].min((screen.height() - 48.0).max(340.0)),
    );
    let offset = (editor.entry_id % 5) as f32 * 18.0;
    let pos = Pos2::new(
        screen.center().x - size.x * 0.5 + offset,
        screen.center().y - size.y * 0.5 + offset,
    );

    let min_size = match editor.kind {
        BuiltinKind::Eq => egui::vec2(620.0, 520.0),
        BuiltinKind::NoiseGate | BuiltinKind::DynamicGate => egui::vec2(600.0, 520.0),
        _ => egui::vec2(420.0, 340.0),
    };
    let mut open = true;
    egui::Window::new(format!("MicHost — {}", editor.kind.name()))
        .id(egui::Id::new(("builtin-editor-window", editor.entry_id)))
        .open(&mut open)
        .default_size(size)
        .default_pos(pos)
        .min_size(min_size)
        .resizable(true)
        .collapsible(false)
        .constrain(true)
        .frame(
            Frame::NONE
                .fill(BACKGROUND)
                .corner_radius(9.0)
                .inner_margin(Margin::ZERO)
                .stroke(Stroke::new(1.0_f32, BORDER)),
        )
        .show(ctx, |ui| {
            apply_theme(ui);
            editor_content(ui, editor);
        });
    open
}

fn window_size(kind: BuiltinKind) -> [f32; 2] {
    match kind {
        BuiltinKind::Compressor => [820.0, 700.0],
        BuiltinKind::Eq => [760.0, 660.0],
        BuiltinKind::NoiseGate | BuiltinKind::DynamicGate => [720.0, 620.0],
        _ if kind.params().len() > 12 => [800.0, 680.0],
        _ => [600.0, 560.0],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackgroundMode {
    Spectrum,
    Loudness,
    Transfer,
}

fn background_mode(kind: BuiltinKind) -> BackgroundMode {
    match kind {
        BuiltinKind::Eq | BuiltinKind::NoiseRemoval | BuiltinKind::PitchShift => {
            BackgroundMode::Spectrum
        }
        BuiltinKind::NoiseGate | BuiltinKind::DynamicGate => BackgroundMode::Transfer,
        _ => BackgroundMode::Loudness,
    }
}

fn apply_theme(ui: &mut egui::Ui) {
    let style = ui.style_mut();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(8.0, 4.0);
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.panel_fill = BACKGROUND;
    style.visuals.window_fill = BACKGROUND;
    style.visuals.extreme_bg_color = KNOB_FILL;
    style.visuals.hyperlink_color = ACCENT;
    style.visuals.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    style.visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
}

fn editor_content(ui: &mut egui::Ui, editor: &BuiltinEditor) {
    ui.ctx().request_repaint_after(Duration::from_millis(33));

    let content_rect = ui.max_rect();
    let (raw_snapshot, spectrum) = analyse_frame(ui.ctx(), editor);
    let now = ui.ctx().input(|input| input.time);
    let snapshot = display_snapshot(ui.ctx(), editor.entry_id, now, &raw_snapshot);
    paint_background(ui, editor, &snapshot, &spectrum, content_rect);

    let specs = editor.kind.params();
    let mut groups = group_sections(specs);
    // Mode-specific controls belong next to the mode selector, not after the
    // shared Output section where they would be off-screen.
    if let Some(index) = groups.iter().position(|(section, _)| *section == "Hum") {
        let hum = groups.remove(index);
        groups.insert(0, hum);
    }
    let desired_top = content_rect.top() + content_rect.height() * CONTROL_TOP_FRACTION;
    let gap = (desired_top - ui.cursor().min.y).max(8.0);
    ui.add_space(gap);

    let controls_top = ui.cursor().min.y.min(content_rect.bottom());
    let scrim_rect = Rect::from_min_max(
        Pos2::new(content_rect.left(), controls_top),
        content_rect.max,
    );
    {
        let painter = ui.painter().with_clip_rect(content_rect);
        paint_vertical_gradient(
            &painter,
            scrim_rect,
            scrim(CONTROL_SCRIM_TOP_ALPHA),
            scrim(CONTROL_SCRIM_BOTTOM_ALPHA),
        );
    }

    let choice_indices: Vec<usize> = specs
        .iter()
        .enumerate()
        .filter(|(_, spec)| matches!(spec.style, ParamStyle::Choice(_)))
        .map(|(index, _)| index)
        .collect();
    if !choice_indices.is_empty() {
        ui.add_space(6.0);
        for index in choice_indices {
            choice_selector(ui, index, &specs[index], &editor.params);
            ui.add_space(4.0);
        }
    }

    let body_height = ui.available_height().max(80.0);
    let mut shown = 0usize;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(body_height)
        .show(ui, |ui| {
            for (section, indices) in &groups {
                let visible: Vec<usize> = indices
                    .iter()
                    .copied()
                    .filter(|index| {
                        let spec = &specs[*index];
                        !matches!(spec.style, ParamStyle::Choice(_))
                            && is_visible(spec, &editor.params)
                    })
                    .collect();
                if visible.is_empty() {
                    continue;
                }
                shown += visible.len();
                section_block(ui, section, &visible, specs, &editor.params, editor.kind);
                ui.add_space(10.0);
            }

            if shown == 0 {
                ui.add_space(12.0);
                ui.label(
                    RichText::new("No controls are available for the current settings.")
                        .size(12.0)
                        .color(MUTED),
                );
            }
        });
}

fn scrim(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(BACKGROUND.r(), BACKGROUND.g(), BACKGROUND.b(), alpha)
}

fn paint_vertical_gradient(painter: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    painter.add(Shape::mesh(mesh));
}

fn section_block(
    ui: &mut egui::Ui,
    section: &str,
    indices: &[usize],
    specs: &[ParamSpec],
    params: &ParamSet,
    kind: BuiltinKind,
) {
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(Vec2::splat(6.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.0, ACCENT);
        ui.label(
            RichText::new(section.to_uppercase())
                .size(9.0)
                .strong()
                .color(MUTED),
        );
    });
    ui.add_space(2.0);
    let (divider, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .rect_filled(divider, 0.0, BORDER.gamma_multiply(0.55));
    ui.add_space(5.0);
    controls_grid(ui, indices, specs, params, kind);
}

fn controls_grid(
    ui: &mut egui::Ui,
    indices: &[usize],
    specs: &[ParamSpec],
    params: &ParamSet,
    kind: BuiltinKind,
) {
    let width = ui.available_width().max(1.0);
    let columns = match kind {
        BuiltinKind::Eq => 3,
        BuiltinKind::NoiseGate | BuiltinKind::DynamicGate => 4,
        _ => grid_columns(width),
    };
    let gap = ui.spacing().item_spacing.x;
    let cell_width = ((width - gap * columns.saturating_sub(1) as f32) / columns as f32).max(64.0);

    for row in indices.chunks(columns) {
        ui.horizontal(|ui| {
            for &index in row {
                ui.allocate_ui_with_layout(
                    Vec2::new(cell_width, CELL_HEIGHT),
                    Layout::top_down(Align::Center),
                    |ui| {
                        ui.set_width(cell_width);
                        let spec = &specs[index];
                        match spec.style {
                            ParamStyle::Knob => knob(ui, index, spec, params),
                            ParamStyle::Toggle => toggle(ui, index, spec, params),
                            ParamStyle::Choice(_) => {}
                        }
                    },
                );
            }
        });
    }
}

fn knob(ui: &mut egui::Ui, index: usize, spec: &ParamSpec, params: &ParamSet) {
    let mut response = ui.allocate_response(Vec2::splat(KNOB_SIZE), Sense::click_and_drag());
    let fine = ui.input(|input| input.modifiers.shift);

    if response.double_clicked() {
        params.set_normalized(index, params.default_normalized(index));
        response.mark_changed();
    } else if response.dragged() {
        let delta = ui.input(|input| input.pointer.delta().y);
        if delta != 0.0 {
            let next = drag_normalized(params.normalized(index), delta, fine);
            params.set_normalized(index, next);
            response.mark_changed();
        }
    } else if response.hovered() {
        let scroll = ui.input_mut(|input| {
            let delta = input.smooth_scroll_delta.y;
            if delta.abs() > 0.01 {
                input.smooth_scroll_delta.y = 0.0;
            }
            delta
        });
        if scroll.abs() > 0.01 {
            let next = (params.normalized(index) + scroll / WHEEL_SENSITIVITY).clamp(0.0, 1.0);
            params.set_normalized(index, next);
            response.mark_changed();
        }
    }

    let active = response.hovered() || response.dragged();
    if active {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }

    let normalized = params.normalized(index);
    let value = spec.from_normalized(normalized);
    paint_knob(ui, &response, normalized, active);

    ui.add(
        egui::Label::new(RichText::new(spec.name).size(11.5).color(MUTED))
            .truncate()
            .selectable(false),
    );
    ui.label(
        RichText::new(spec.format_value(value))
            .size(12.5)
            .color(if active { ACCENT } else { TEXT }),
    );

    let tooltip = format!(
        "{}\nCurrent: {}\nRange: {} - {}\nDrag to adjust; Shift for fine; double-click to reset",
        spec.name,
        spec.format_value(value),
        spec.format_value(spec.min),
        spec.format_value(spec.max),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Slider,
            ui.is_enabled(),
            format!(
                "{}: {}",
                spec.name,
                spec.format_value(spec.from_normalized(normalized))
            ),
        )
    });
    let _ = response.on_hover_text(tooltip);
}

fn choice_selector(ui: &mut egui::Ui, index: usize, spec: &ParamSpec, params: &ParamSet) {
    let ParamStyle::Choice(choices) = spec.style else {
        return;
    };
    if choices.is_empty() {
        return;
    }
    let current = params
        .get(index)
        .round()
        .clamp(0.0, (choices.len() - 1) as f32) as usize;
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(spec.name.to_uppercase())
                .size(9.0)
                .strong()
                .color(MUTED),
        );
        for (choice, label) in choices.iter().enumerate() {
            let selected = choice == current;
            let text = RichText::new(*label).size(11.5).color(if selected {
                Color32::WHITE
            } else {
                TEXT
            });
            if ui.selectable_label(selected, text).clicked() {
                params.set(index, choice as f32);
            }
        }
    });
}

fn toggle(ui: &mut egui::Ui, index: usize, spec: &ParamSpec, params: &ParamSet) {
    let on = params.normalized(index) >= 0.5;
    let mut response = ui.allocate_response(Vec2::new(48.0, 24.0), Sense::click());
    if response.clicked() {
        params.set_normalized(index, if on { 0.0 } else { 1.0 });
        response.mark_changed();
    }

    let on = params.normalized(index) >= 0.5;
    let rect = response.rect;
    let radius = rect.height() * 0.5;
    let painter = ui.painter();
    painter.rect_filled(rect, radius, if on { ACCENT } else { TRACK });
    if response.hovered() {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.8)),
            StrokeKind::Inside,
        );
    }
    let thumb_radius = radius - 3.0;
    let thumb_x = if on {
        rect.right() - radius
    } else {
        rect.left() + radius
    };
    painter.circle_filled(
        Pos2::new(thumb_x, rect.center().y),
        thumb_radius,
        if on { Color32::WHITE } else { MUTED },
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    ui.add(
        egui::Label::new(RichText::new(spec.name).size(11.5).color(MUTED))
            .truncate()
            .selectable(false),
    );
    ui.label(
        RichText::new(if on { "On" } else { "Off" })
            .size(12.0)
            .strong()
            .color(if on { ACCENT } else { MUTED }),
    );

    let value = spec.from_normalized(params.normalized(index));
    let tooltip = format!(
        "{}: {}\nClick to toggle",
        spec.name,
        spec.format_value(value)
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), on, spec.name)
    });
    let _ = response.on_hover_text(tooltip);
}

fn paint_knob(ui: &egui::Ui, response: &Response, normalized: f32, active: bool) {
    let center = response.rect.center();
    let radius = KNOB_SIZE * 0.5;
    let painter = ui.painter();

    painter.circle_filled(center, radius, KNOB_FILL);
    painter.circle_stroke(center, radius, Stroke::new(1.0_f32, BORDER));

    let arc_radius = radius - 4.5;
    painter.add(egui::Shape::line(
        arc_points(center, arc_radius, KNOB_START, KNOB_SWEEP, 48),
        Stroke::new(3.0_f32, TRACK),
    ));
    if normalized > 0.002 {
        painter.add(egui::Shape::line(
            arc_points(center, arc_radius, KNOB_START, KNOB_SWEEP * normalized, 48),
            Stroke::new(3.0_f32, ACCENT),
        ));
    }

    let pointer = knob_angle(normalized);
    let direction = Vec2::new(pointer.cos(), pointer.sin());
    painter.line_segment(
        [
            center + direction * (radius * 0.30),
            center + direction * (radius * 0.72),
        ],
        Stroke::new(2.5_f32, if active { Color32::WHITE } else { TEXT }),
    );

    if active {
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(1.5_f32, ACCENT.gamma_multiply(0.7)),
        );
    }
}

fn arc_points(center: Pos2, radius: f32, start: f32, sweep: f32, segments: usize) -> Vec<Pos2> {
    let steps = segments.max(2);
    (0..=steps)
        .map(|step| {
            let angle = start + sweep * (step as f32 / steps as f32);
            Pos2::new(
                center.x + angle.cos() * radius,
                center.y + angle.sin() * radius,
            )
        })
        .collect()
}

fn grid_columns(available_width: f32) -> usize {
    if available_width >= 600.0 {
        6
    } else if available_width >= 420.0 {
        4
    } else if available_width >= 300.0 {
        3
    } else {
        2
    }
}

fn knob_angle(normalized: f32) -> f32 {
    KNOB_START + normalized.clamp(0.0, 1.0) * KNOB_SWEEP
}

fn drag_normalized(current: f32, delta_y: f32, fine: bool) -> f32 {
    let sensitivity = if fine {
        FINE_DRAG_SENSITIVITY
    } else {
        DRAG_SENSITIVITY
    };
    (current - delta_y / sensitivity).clamp(0.0, 1.0)
}

fn group_sections(specs: &[ParamSpec]) -> Vec<(&'static str, Vec<usize>)> {
    let mut groups: Vec<(&'static str, Vec<usize>)> = Vec::new();
    for (index, spec) in specs.iter().enumerate() {
        if let Some((_, entries)) = groups
            .iter_mut()
            .find(|(section, _)| *section == spec.section)
        {
            entries.push(index);
        } else {
            groups.push((spec.section, vec![index]));
        }
    }
    groups
}

fn value_matches(current: f32, expected: f32) -> bool {
    (current - expected).abs() <= 1e-4 * expected.abs().max(1.0)
}

fn is_visible_with(spec: &ParamSpec, mut lookup: impl FnMut(&str) -> Option<f32>) -> bool {
    match spec.visible_when {
        None => true,
        Some((key, expected)) => {
            lookup(key).is_some_and(|current| value_matches(current, expected))
        }
    }
}

fn is_visible(spec: &ParamSpec, params: &ParamSet) -> bool {
    is_visible_with(spec, |key| {
        params.index_of(key).map(|index| params.get(index))
    })
}

// ---------------------------------------------------------------------------
// Live analysis: FFT, log bins, smoothing
// ---------------------------------------------------------------------------

/// Persistent per-editor scratch so the FFT and its buffers are built once.
///
/// Stored in egui temp data keyed by entry id. Holds the Hann window, twiddle
/// tables and the sample/transform buffers, so a frame performs no FFT-sized
/// allocations.
#[derive(Clone)]
struct FftPlan {
    window: Vec<f32>,
    window_sum: f32,
    cos: Vec<f32>,
    sin: Vec<f32>,
    left: Vec<f32>,
    right: Vec<f32>,
    output_left: Vec<f32>,
    output_right: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl FftPlan {
    fn new() -> Self {
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|index| {
                let phase = 2.0 * std::f32::consts::PI * index as f32 / (FFT_SIZE - 1) as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        let window_sum = window.iter().sum();
        let cos = (0..FFT_SIZE)
            .map(|index| (2.0 * std::f32::consts::PI * index as f32 / FFT_SIZE as f32).cos())
            .collect();
        let sin = (0..FFT_SIZE)
            .map(|index| (2.0 * std::f32::consts::PI * index as f32 / FFT_SIZE as f32).sin())
            .collect();
        Self {
            window,
            window_sum,
            cos,
            sin,
            left: vec![0.0; FFT_SIZE],
            right: vec![0.0; FFT_SIZE],
            output_left: vec![0.0; FFT_SIZE],
            output_right: vec![0.0; FFT_SIZE],
            re: vec![0.0; FFT_SIZE],
            im: vec![0.0; FFT_SIZE],
        }
    }
}

/// Smoothed pre/post spectra for one editor, keyed by entry id.
#[derive(Clone)]
struct SpectrumMemory {
    input: [f32; SPECTRUM_BINS],
    output: [f32; SPECTRUM_BINS],
    primed: bool,
}

impl SpectrumMemory {
    fn new() -> Self {
        Self {
            input: [SPECTRUM_FLOOR_DB; SPECTRUM_BINS],
            output: [SPECTRUM_FLOOR_DB; SPECTRUM_BINS],
            primed: false,
        }
    }
}

#[derive(Clone, Copy)]
struct SpectrumFrame {
    input: [f32; SPECTRUM_BINS],
    output: [f32; SPECTRUM_BINS],
}

#[derive(Clone, Copy)]
struct DisplaySmoothing {
    input_db: f32,
    output_db: f32,
    reduction_db: f32,
    last_time: f64,
    primed: bool,
}

impl DisplaySmoothing {
    fn new() -> Self {
        Self {
            input_db: -120.0,
            output_db: -120.0,
            reduction_db: 0.0,
            last_time: f64::NAN,
            primed: false,
        }
    }
}

/// Meter ballistics for the displayed peaks and gain reduction. The raw
/// per-block peaks move every audio block and look like noise at 30-60 fps;
/// this follows them with a short attack and slower release, using real
/// elapsed time so frame rate does not change the feel.
fn display_snapshot(
    ctx: &egui::Context,
    entry_id: u64,
    now: f64,
    raw: &AnalysisSnapshot,
) -> AnalysisSnapshot {
    let id = egui::Id::new(("builtin-display-levels", entry_id));
    ctx.data_mut(|data| {
        let smoothing = data.get_temp_mut_or_insert_with(id, DisplaySmoothing::new);
        let dt = if smoothing.primed && now.is_finite() {
            (now - smoothing.last_time).clamp(0.001, 0.1) as f32
        } else {
            0.0
        };
        smoothing.last_time = now;
        if !smoothing.primed || dt <= 0.0 {
            smoothing.input_db = raw.input_peak_db;
            smoothing.output_db = raw.output_peak_db;
            smoothing.reduction_db = raw.reduction_db;
            smoothing.primed = true;
        } else {
            smoothing.input_db = follow(
                smoothing.input_db,
                raw.input_peak_db,
                LEVEL_ATTACK_SECONDS,
                LEVEL_RELEASE_SECONDS,
                dt,
            );
            smoothing.output_db = follow(
                smoothing.output_db,
                raw.output_peak_db,
                LEVEL_ATTACK_SECONDS,
                LEVEL_RELEASE_SECONDS,
                dt,
            );
            smoothing.reduction_db = follow(
                smoothing.reduction_db,
                raw.reduction_db,
                REDUCTION_ATTACK_SECONDS,
                REDUCTION_RELEASE_SECONDS,
                dt,
            );
        }
        AnalysisSnapshot {
            sample_rate: raw.sample_rate,
            input_peak_db: smoothing.input_db,
            output_peak_db: smoothing.output_db,
            reduction_db: smoothing.reduction_db,
            noise_floor_db: raw.noise_floor_db,
        }
    })
}

fn follow(current: f32, target: f32, attack: f32, release: f32, dt: f32) -> f32 {
    if !current.is_finite() || !target.is_finite() || !dt.is_finite() {
        return target;
    }
    let tau = if target.abs() > current.abs() {
        attack
    } else {
        release
    };
    let coefficient = 1.0 - (-dt / tau.max(1.0e-4)).exp();
    current + (target - current) * coefficient
}

fn analyse_frame(ctx: &egui::Context, editor: &BuiltinEditor) -> (AnalysisSnapshot, SpectrumFrame) {
    let plan_id = egui::Id::new(("builtin-spectrum-fft", editor.entry_id));
    let state_id = egui::Id::new(("builtin-spectrum-state", editor.entry_id));

    let (snapshot, input_targets, output_targets) = ctx.data_mut(|data| {
        let plan = data.get_temp_mut_or_insert_with(plan_id, FftPlan::new);
        let snapshot = editor.analysis.snapshot_with_output(
            &mut plan.left,
            &mut plan.right,
            &mut plan.output_left,
            &mut plan.output_right,
        );
        let rate = if snapshot.sample_rate.is_finite() && snapshot.sample_rate > 0.0 {
            snapshot.sample_rate
        } else {
            48_000.0
        };

        let tables = FftTables {
            window: &plan.window,
            window_sum: plan.window_sum,
            cos: &plan.cos,
            sin: &plan.sin,
        };
        let input = fft_targets(&plan.left, &tables, &mut plan.re, &mut plan.im, rate);
        let output = fft_targets(&plan.output_left, &tables, &mut plan.re, &mut plan.im, rate);

        (snapshot, input, output)
    });

    let frame = ctx.data_mut(|data| {
        let memory = data.get_temp_mut_or_insert_with(state_id, SpectrumMemory::new);
        if memory.primed {
            smooth_spectrum(
                &mut memory.input,
                &input_targets,
                SPECTRUM_ATTACK,
                SPECTRUM_RELEASE,
            );
            smooth_spectrum(
                &mut memory.output,
                &output_targets,
                SPECTRUM_ATTACK,
                SPECTRUM_RELEASE,
            );
        } else {
            memory.input = input_targets;
            memory.output = output_targets;
            memory.primed = true;
        }
        SpectrumFrame {
            input: memory.input,
            output: memory.output,
        }
    });

    (snapshot, frame)
}

struct FftTables<'a> {
    window: &'a [f32],
    window_sum: f32,
    cos: &'a [f32],
    sin: &'a [f32],
}

fn fft_targets(
    input: &[f32],
    tables: &FftTables<'_>,
    re: &mut [f32],
    im: &mut [f32],
    sample_rate: f32,
) -> [f32; SPECTRUM_BINS] {
    for ((re, (&sample, &weight)), im) in re
        .iter_mut()
        .zip(input.iter().zip(tables.window.iter()))
        .zip(im.iter_mut())
    {
        *re = sample * weight;
        *im = 0.0;
    }
    fft_in_place(re, im, tables.cos, tables.sin);

    let mut targets = [SPECTRUM_FLOOR_DB; SPECTRUM_BINS];
    for (bin, target) in targets.iter_mut().enumerate() {
        let low = log_bin_frequency(bin as f32 / SPECTRUM_BINS as f32);
        let high = log_bin_frequency((bin + 1) as f32 / SPECTRUM_BINS as f32);
        let center = (low * high).sqrt();
        let tilt_db = SPECTRUM_TILT_DB_PER_OCTAVE * (center / 1_000.0).log2();
        let (first, last) = fft_bin_range(low, high, FFT_SIZE, sample_rate);
        let mut peak = 0.0f32;
        for k in first..=last {
            peak = peak.max(bin_amplitude(re, im, k, tables.window_sum));
        }
        *target = amplitude_to_db(peak, tilt_db);
    }
    smooth_bins(&mut targets);
    targets
}

fn smooth_bins(values: &mut [f32; SPECTRUM_BINS]) {
    const KERNEL: [f32; 5] = [1.0, 4.0, 6.0, 4.0, 1.0];
    let source = *values;
    for (index, value) in values.iter_mut().enumerate() {
        let mut sum = 0.0;
        let mut weight = 0.0;
        for (offset, kernel) in KERNEL.iter().enumerate() {
            let source_index = (index + offset).saturating_sub(2).min(SPECTRUM_BINS - 1);
            sum += source[source_index] * kernel;
            weight += kernel;
        }
        *value = sum / weight;
    }
}

/// Iterative radix-2 FFT. `re`/`im` must be a power-of-two length and the
/// twiddle tables are `cos`/`sin` of `2*pi*k/n`.
fn fft_in_place(re: &mut [f32], im: &mut [f32], cos: &[f32], sin: &[f32]) {
    let n = re.len();
    if n < 2 || im.len() != n || cos.len() < n || sin.len() < n || !n.is_power_of_two() {
        return;
    }

    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2usize;
    while len <= n {
        let half = len / 2;
        let step = n / len;
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let twiddle = k * step;
                let (cos_w, sin_w) = (cos[twiddle], sin[twiddle]);
                let delta = start + half + k;
                let xr = re[delta];
                let xi = im[delta];
                let tr = xr * cos_w + xi * sin_w;
                let ti = xi * cos_w - xr * sin_w;
                let base = start + k;
                re[delta] = re[base] - tr;
                im[delta] = im[base] - ti;
                re[base] += tr;
                im[base] += ti;
            }
        }
        len <<= 1;
    }
}

/// Amplitude of FFT bin `k` for a real, Hann-windowed input. DC and Nyquist are
/// not doubled; all other bins get the one-sided factor of two.
fn bin_amplitude(re: &[f32], im: &[f32], k: usize, window_sum: f32) -> f32 {
    let n = re.len();
    if k >= n.min(im.len()) || window_sum <= 0.0 {
        return 0.0;
    }
    let scale = if k == 0 || k == n / 2 { 1.0 } else { 2.0 } / window_sum;
    re[k].hypot(im[k]) * scale
}

fn amplitude_to_db(amplitude: f32, tilt_db: f32) -> f32 {
    if !amplitude.is_finite() || amplitude <= 0.0 {
        SPECTRUM_FLOOR_DB
    } else {
        (20.0 * amplitude.log10() + tilt_db).clamp(SPECTRUM_FLOOR_DB, 0.0)
    }
}

/// Frequency at log position `t` in `0..=1` between 20 Hz and 20 kHz.
fn log_bin_frequency(t: f32) -> f32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    SPECTRUM_MIN_HZ * (SPECTRUM_MAX_HZ / SPECTRUM_MIN_HZ).powf(t)
}

/// Inclusive FFT bin range covering `[low_hz, high_hz]`, clamped to 1..=n/2.
fn fft_bin_range(low_hz: f32, high_hz: f32, n: usize, sample_rate: f32) -> (usize, usize) {
    let first = frequency_to_fft_bin(low_hz, n, sample_rate);
    let last = frequency_to_fft_bin(high_hz, n, sample_rate).max(first);
    (first, last)
}

fn frequency_to_fft_bin(freq_hz: f32, n: usize, sample_rate: f32) -> usize {
    let max_bin = (n / 2).max(1);
    if !freq_hz.is_finite() || !sample_rate.is_finite() || sample_rate <= 0.0 {
        return 1;
    }
    let bin = (freq_hz.max(0.0) / sample_rate * n as f32).round();
    (bin as usize).clamp(1, max_bin)
}

fn smooth_spectrum(
    current: &mut [f32; SPECTRUM_BINS],
    target: &[f32; SPECTRUM_BINS],
    attack: f32,
    release: f32,
) {
    for (value, &goal) in current.iter_mut().zip(target.iter()) {
        let coefficient = if goal > *value { attack } else { release };
        *value += (goal - *value) * coefficient;
    }
}

/// Maps a dB value to a y coordinate; values outside `min_db..=max_db` clamp.
fn map_db_to_y(db: f32, top: f32, bottom: f32, max_db: f32, min_db: f32) -> f32 {
    if !db.is_finite() || bottom == top {
        return bottom;
    }
    let fraction = ((db.clamp(min_db, max_db) - max_db) / (min_db - max_db)).clamp(0.0, 1.0);
    top + fraction * (bottom - top)
}

fn level_db_to_y(db: f32, top: f32, bottom: f32) -> f32 {
    map_db_to_y(db, top, bottom, GRAPH_TOP_DB, GRAPH_BOTTOM_DB)
}

fn frequency_to_x(freq_hz: f32, rect: Rect) -> f32 {
    let ratio = (freq_hz.max(SPECTRUM_MIN_HZ) / SPECTRUM_MIN_HZ).ln()
        / (SPECTRUM_MAX_HZ / SPECTRUM_MIN_HZ).ln();
    rect.left() + ratio.clamp(0.0, 1.0) * rect.width()
}

fn frequency_from_fraction(t: f32) -> f32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    SPECTRUM_MIN_HZ * (SPECTRUM_MAX_HZ / SPECTRUM_MIN_HZ).powf(t)
}

// ---------------------------------------------------------------------------
// Full-window backdrop: spectrum or loudness history
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct HistoryPoint {
    input_db: f32,
    output_db: f32,
    reduction_db: f32,
}

impl HistoryPoint {
    const SILENT: Self = Self {
        input_db: -120.0,
        output_db: -120.0,
        reduction_db: 0.0,
    };

    fn from_snapshot(snapshot: &AnalysisSnapshot) -> Self {
        Self {
            input_db: sanitize_db(snapshot.input_peak_db),
            output_db: sanitize_db(snapshot.output_peak_db),
            reduction_db: if snapshot.reduction_db.is_finite() {
                snapshot.reduction_db.clamp(0.0, 60.0)
            } else {
                0.0
            },
        }
    }
}

fn sanitize_db(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-120.0, 0.0)
    } else {
        -120.0
    }
}

/// Fixed ring of loudness samples; index 0 is always the oldest point.
#[derive(Clone)]
struct LoudnessHistory {
    points: [HistoryPoint; HISTORY_LEN],
    head: usize,
    count: usize,
    last_time: f64,
}

impl LoudnessHistory {
    fn new() -> Self {
        Self {
            points: [HistoryPoint::SILENT; HISTORY_LEN],
            head: 0,
            count: 0,
            last_time: f64::NAN,
        }
    }

    fn push(&mut self, point: HistoryPoint) {
        self.points[self.head] = point;
        self.head = (self.head + 1) % HISTORY_LEN;
        self.count = (self.count + 1).min(HISTORY_LEN);
    }

    /// Adds a sample at most once per fixed interval, independent of the UI
    /// frame rate. Mouse movement and hover repaints run the UI faster than
    /// 30 fps; sampling per frame made the history scroll speed depend on it.
    fn sample(&mut self, now: f64, point: HistoryPoint) {
        if !now.is_finite() {
            return;
        }
        if !self.last_time.is_finite() {
            self.push(point);
            self.last_time = now;
            return;
        }
        let elapsed = now - self.last_time;
        if elapsed < HISTORY_INTERVAL {
            return;
        }
        let steps = ((elapsed / HISTORY_INTERVAL).floor() as usize).clamp(1, HISTORY_CATCHUP_LIMIT);
        for _ in 0..steps {
            self.push(point);
        }
        self.last_time += steps as f64 * HISTORY_INTERVAL;
        if now - self.last_time >= HISTORY_RESYNC_SECONDS {
            self.last_time = now;
        }
    }

    fn count(&self) -> usize {
        self.count
    }

    fn get(&self, index: usize) -> Option<HistoryPoint> {
        if index >= self.count {
            return None;
        }
        let start = (self.head + HISTORY_LEN - self.count) % HISTORY_LEN;
        Some(self.points[(start + index) % HISTORY_LEN])
    }

    fn newest(&self) -> Option<HistoryPoint> {
        self.get(self.count.saturating_sub(1))
    }
}

fn paint_background(
    ui: &egui::Ui,
    editor: &BuiltinEditor,
    snapshot: &AnalysisSnapshot,
    spectrum: &SpectrumFrame,
    rect: Rect,
) {
    let detailed = rect.width() >= 360.0;
    let mode = background_mode(editor.kind);
    let painter = ui.painter().with_clip_rect(rect);
    paint_vertical_gradient(&painter, rect, GRAPH_TOP, GRAPH_BOTTOM);

    match mode {
        BackgroundMode::Spectrum => {
            let graph = display_graph_rect(rect);
            paint_backdrop_grid(&painter, graph, mode);
            paint_spectrum(
                &painter,
                graph,
                &spectrum.input,
                SpectrumStyle {
                    fill: SPECTRUM_PRE,
                    line: SPECTRUM_PRE_LINE,
                    fill_alpha: 30,
                    line_alpha: 150,
                    glow: false,
                },
            );
            paint_spectrum(
                &painter,
                graph,
                &spectrum.output,
                SpectrumStyle {
                    fill: SPECTRUM,
                    line: SPECTRUM_LINE,
                    fill_alpha: 44,
                    line_alpha: 220,
                    glow: true,
                },
            );
            if editor.kind != BuiltinKind::Eq {
                paint_peaks(&painter, graph, snapshot);
            }
            paint_frequency_axis(&painter, graph);
            let overlay = if editor.kind == BuiltinKind::Eq {
                Rect::from_min_max(
                    graph.min,
                    Pos2::new(
                        graph.right(),
                        (graph.bottom() - 16.0)
                            .max(graph.top() + 24.0)
                            .min(graph.bottom()),
                    ),
                )
            } else {
                graph
            };
            draw_overlays(&painter, overlay, editor, snapshot, detailed);
            paint_spectrum_legend(&painter, graph);
        }
        BackgroundMode::Loudness => {
            paint_backdrop_grid(&painter, rect, mode);
            paint_loudness(ui, editor, snapshot, rect);
            draw_overlays(&painter, rect, editor, snapshot, detailed);
        }
        BackgroundMode::Transfer => {
            let graph = display_graph_rect(rect);
            paint_gate_transfer(&painter, graph, editor, snapshot, detailed);
        }
    }
}

fn display_graph_rect(rect: Rect) -> Rect {
    let top = rect.top() + TOP_PAD;
    let bottom = (rect.top() + rect.height() * CONTROL_TOP_FRACTION - 8.0)
        .max(top + 24.0)
        .min(rect.bottom());
    Rect::from_min_max(Pos2::new(rect.left(), top), Pos2::new(rect.right(), bottom))
}

fn paint_loudness(ui: &egui::Ui, editor: &BuiltinEditor, snapshot: &AnalysisSnapshot, rect: Rect) {
    let id = egui::Id::new(("builtin-loudness-history", editor.entry_id));
    let painter = ui.painter().with_clip_rect(rect);
    // Copy the ring out of the egui data lock before painting. Painting can
    // touch context data (fonts/text) and re-entering `data_mut` while the
    // lock is held deadlocks the UI thread.
    let now = ui.ctx().input(|input| input.time);
    let history = ui.ctx().data_mut(|data| {
        let history = data.get_temp_mut_or_insert_with(id, LoudnessHistory::new);
        history.sample(now, HistoryPoint::from_snapshot(snapshot));
        history.clone()
    });
    paint_history(&painter, rect, &history);
}

fn paint_history(painter: &egui::Painter, rect: Rect, history: &LoudnessHistory) {
    let count = history.count();
    if count < 2 {
        return;
    }
    let spacing = rect.width() / HISTORY_LEN.saturating_sub(1) as f32;
    let mut input_points = Vec::with_capacity(count);
    let mut output_points = Vec::with_capacity(count);
    for index in 0..count {
        let Some(point) = history.get(index) else {
            continue;
        };
        let x = rect.right() - (count - 1 - index) as f32 * spacing;
        input_points.push(Pos2::new(
            x,
            level_db_to_y(point.input_db, rect.top(), rect.bottom()),
        ));
        output_points.push(Pos2::new(
            x,
            level_db_to_y(point.output_db, rect.top(), rect.bottom()),
        ));
    }
    if input_points.len() < 2 {
        return;
    }

    let band = Color32::from_rgba_unmultiplied(GR_COLOR.r(), GR_COLOR.g(), GR_COLOR.b(), 34);
    let mut mesh = Mesh::default();
    for (input, output) in input_points.iter().zip(output_points.iter()) {
        mesh.colored_vertex(*input, band);
        mesh.colored_vertex(*output, band);
    }
    for index in 0..input_points.len().saturating_sub(1) {
        let base = (index * 2) as u32;
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base + 1, base + 2, base + 3);
    }
    painter.add(Shape::mesh(mesh));

    painter.add(Shape::line(
        input_points,
        Stroke::new(1.0_f32, INPUT_LINE.gamma_multiply(0.75)),
    ));
    painter.add(Shape::line(output_points, Stroke::new(1.4_f32, ACCENT)));

    let latest = history.newest().unwrap_or(HistoryPoint::SILENT);
    let safe_top = rect.top() + TOP_PAD + 4.0;
    if latest.reduction_db > 0.15 {
        graph_text(
            painter,
            Pos2::new(rect.center().x, safe_top),
            Align2::CENTER_TOP,
            &format!("-{:.1} dB GR", latest.reduction_db),
            GR_COLOR,
        );
    }
    if rect.width() >= 360.0 {
        graph_text(
            painter,
            Pos2::new(rect.right() - 8.0, safe_top + 18.0),
            Align2::RIGHT_TOP,
            &format!("IN {:.1} dB", latest.input_db),
            INPUT_LINE,
        );
        graph_text(
            painter,
            Pos2::new(rect.right() - 8.0, safe_top + 32.0),
            Align2::RIGHT_TOP,
            &format!("OUT {:.1} dB", latest.output_db),
            ACCENT,
        );
    }
}

const DB_GUIDES: [f32; 11] = [
    0.0, -6.0, -12.0, -18.0, -24.0, -30.0, -36.0, -42.0, -48.0, -54.0, -60.0,
];
const FREQ_GUIDES: [(f32, &str); 10] = [
    (20.0, "20"),
    (50.0, "50"),
    (100.0, "100"),
    (200.0, "200"),
    (500.0, "500"),
    (1_000.0, "1k"),
    (2_000.0, "2k"),
    (5_000.0, "5k"),
    (10_000.0, "10k"),
    (20_000.0, "20k"),
];

fn paint_backdrop_grid(painter: &egui::Painter, rect: Rect, mode: BackgroundMode) {
    for level in DB_GUIDES {
        let y = level_db_to_y(level, rect.top(), rect.bottom());
        let major = (level as i32).rem_euclid(12) == 0;
        let color = if level == 0.0 {
            BORDER.gamma_multiply(0.55)
        } else if major {
            BORDER.gamma_multiply(0.28)
        } else {
            BORDER.gamma_multiply(0.14)
        };
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.0_f32, color),
        );
        if major {
            painter.text(
                Pos2::new(rect.left() + 5.0, y),
                Align2::LEFT_CENTER,
                format!("{level:.0}"),
                FontId::proportional(8.5),
                MUTED.gamma_multiply(0.62),
            );
        }
    }

    if !matches!(mode, BackgroundMode::Spectrum) {
        return;
    }

    for (freq, _) in FREQ_GUIDES {
        let x = frequency_to_x(freq, rect);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0_f32, BORDER.gamma_multiply(0.22)),
        );
    }
}

fn paint_frequency_axis(painter: &egui::Painter, rect: Rect) {
    let axis_y = rect.bottom();
    for (freq, label) in FREQ_GUIDES {
        let x = frequency_to_x(freq, rect);
        painter.line_segment(
            [Pos2::new(x, axis_y - 4.0), Pos2::new(x, axis_y)],
            Stroke::new(1.0_f32, BORDER.gamma_multiply(0.65)),
        );
        let align = if freq <= SPECTRUM_MIN_HZ {
            Align2::LEFT_BOTTOM
        } else if freq >= SPECTRUM_MAX_HZ {
            Align2::RIGHT_BOTTOM
        } else {
            Align2::CENTER_BOTTOM
        };
        graph_text(
            painter,
            Pos2::new(x, axis_y - 5.0),
            align,
            label,
            MUTED.gamma_multiply(0.9),
        );
    }
    painter.line_segment(
        [
            Pos2::new(rect.left(), axis_y),
            Pos2::new(rect.right(), axis_y),
        ],
        Stroke::new(1.0_f32, BORDER.gamma_multiply(0.5)),
    );
}

fn spectrum_points(spectrum: &[f32], rect: Rect) -> Vec<Pos2> {
    let count = spectrum.len();
    if count == 0 {
        return Vec::new();
    }
    spectrum
        .iter()
        .enumerate()
        .map(|(index, db)| {
            let t = (index as f32 + 0.5) / count as f32;
            Pos2::new(
                rect.left() + t * rect.width(),
                level_db_to_y(*db, rect.top(), rect.bottom()),
            )
        })
        .collect()
}

#[derive(Clone, Copy)]
struct SpectrumStyle {
    fill: Color32,
    line: Color32,
    fill_alpha: u8,
    line_alpha: u8,
    glow: bool,
}

fn paint_spectrum_fill(
    painter: &egui::Painter,
    rect: Rect,
    points: &[Pos2],
    color: Color32,
    alpha: u8,
) {
    if points.len() < 2 {
        return;
    }
    let bottom = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha / 8);
    let top = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);

    let mut mesh = Mesh::default();
    for point in points {
        mesh.colored_vertex(Pos2::new(point.x, rect.bottom()), bottom);
        mesh.colored_vertex(*point, top);
    }
    for index in 0..points.len().saturating_sub(1) {
        let base = (index * 2) as u32;
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base + 1, base + 2, base + 3);
    }
    painter.add(Shape::mesh(mesh));
}

fn paint_spectrum(painter: &egui::Painter, rect: Rect, spectrum: &[f32], style: SpectrumStyle) {
    let points = spectrum_points(spectrum, rect);
    paint_spectrum_fill(painter, rect, &points, style.fill, style.fill_alpha);
    if points.len() < 2 {
        return;
    }
    if style.glow {
        let glow =
            Color32::from_rgba_unmultiplied(style.fill.r(), style.fill.g(), style.fill.b(), 34);
        painter.add(Shape::line(points.clone(), Stroke::new(6.0_f32, glow)));
        let soft = Color32::from_rgba_unmultiplied(
            style.line.r(),
            style.line.g(),
            style.line.b(),
            style.line_alpha / 3,
        );
        painter.add(Shape::line(points.clone(), Stroke::new(2.4_f32, soft)));
    }
    let core = Color32::from_rgba_unmultiplied(
        style.line.r(),
        style.line.g(),
        style.line.b(),
        style.line_alpha,
    );
    painter.add(Shape::line(
        points,
        Stroke::new(if style.glow { 1.2_f32 } else { 1.0_f32 }, core),
    ));
}

fn paint_spectrum_legend(painter: &egui::Painter, rect: Rect) {
    let y = rect.top() + 4.0;
    graph_text(
        painter,
        Pos2::new(rect.right() - 42.0, y),
        Align2::RIGHT_TOP,
        "IN",
        SPECTRUM_PRE_LINE.gamma_multiply(0.85),
    );
    graph_text(
        painter,
        Pos2::new(rect.right() - 8.0, y),
        Align2::RIGHT_TOP,
        "OUT",
        SPECTRUM_LINE.gamma_multiply(0.9),
    );
    graph_text(
        painter,
        Pos2::new(rect.right() - 8.0, y + 13.0),
        Align2::RIGHT_TOP,
        "4.5 dB/oct",
        MUTED.gamma_multiply(0.6),
    );
}

fn paint_peaks(painter: &egui::Painter, plot: Rect, snapshot: &AnalysisSnapshot) {
    let input = snapshot.input_peak_db;
    let output = snapshot.output_peak_db;
    let silent = input <= -99.0 && output <= -99.0;
    let input_y = level_db_to_y(input, plot.top(), plot.bottom());
    let output_y = level_db_to_y(output, plot.top(), plot.bottom());

    if snapshot.reduction_db > 0.15 && input_y < output_y {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(plot.left(), input_y),
                Pos2::new(plot.right(), output_y),
            ),
            1.0,
            Color32::from_rgba_unmultiplied(GR_COLOR.r(), GR_COLOR.g(), GR_COLOR.b(), 38),
        );
    }

    painter.line_segment(
        [
            Pos2::new(plot.left(), input_y),
            Pos2::new(plot.right(), input_y),
        ],
        Stroke::new(1.0_f32, INPUT_LINE.gamma_multiply(0.8)),
    );
    painter.line_segment(
        [
            Pos2::new(plot.left(), output_y),
            Pos2::new(plot.right(), output_y),
        ],
        Stroke::new(1.4_f32, ACCENT),
    );

    if silent {
        return;
    }

    let safe_top = plot.top() + 4.0;
    if snapshot.reduction_db > 0.15 {
        graph_text(
            painter,
            Pos2::new(plot.center().x, safe_top),
            Align2::CENTER_TOP,
            &format!("-{:.1} dB GR", snapshot.reduction_db),
            GR_COLOR,
        );
    }
}

fn draw_overlays(
    painter: &egui::Painter,
    plot: Rect,
    editor: &BuiltinEditor,
    snapshot: &AnalysisSnapshot,
    detailed: bool,
) {
    let params = editor.params.as_ref();
    match editor.kind {
        BuiltinKind::Compressor => compressor_overlay(painter, plot, params, detailed),
        BuiltinKind::Limiter => limiter_overlay(painter, plot, params, snapshot, detailed),
        BuiltinKind::NoiseRemoval => noise_removal_overlay(painter, plot, params, detailed),
        BuiltinKind::Eq => eq_overlay(painter, plot, params, snapshot.sample_rate, detailed),
        _ => {}
    }
}

fn graph_text(painter: &egui::Painter, pos: Pos2, anchor: Align2, text: &str, color: Color32) {
    let font = FontId::proportional(9.5);
    let shadow = Color32::from_rgba_unmultiplied(0, 0, 0, 180);
    painter.text(
        pos + Vec2::new(1.0, 1.0),
        anchor,
        text,
        font.clone(),
        shadow,
    );
    painter.text(pos, anchor, text, font, color);
}

fn dashed(painter: &egui::Painter, from: Pos2, to: Pos2, stroke: Stroke) {
    for shape in Shape::dashed_line(&[from, to], stroke, 5.0, 4.0) {
        painter.add(shape);
    }
}

fn dotted(painter: &egui::Painter, from: Pos2, to: Pos2, stroke: Stroke) {
    for shape in Shape::dashed_line(&[from, to], stroke, 2.0, 3.0) {
        painter.add(shape);
    }
}

fn param_value(params: &ParamSet, key: &str) -> Option<f32> {
    params.index_of(key).map(|index| params.get(index))
}

fn format_hz(hz: f32) -> String {
    if hz >= 1_000.0 {
        format!("{:.1}k", hz / 1_000.0)
    } else {
        format!("{hz:.0}")
    }
}

fn compressor_overlay(painter: &egui::Painter, plot: Rect, params: &ParamSet, detailed: bool) {
    let Some(threshold) = param_value(params, "threshold_db") else {
        return;
    };
    let knee = param_value(params, "knee_db").unwrap_or(0.0).max(0.0);
    let y = level_db_to_y(threshold, plot.top(), plot.bottom());

    if knee > 0.05 {
        let high = (threshold + knee * 0.5).min(GRAPH_TOP_DB);
        let low = (threshold - knee * 0.5).max(GRAPH_BOTTOM_DB);
        let band = Rect::from_min_max(
            Pos2::new(plot.left(), level_db_to_y(high, plot.top(), plot.bottom())),
            Pos2::new(plot.right(), level_db_to_y(low, plot.top(), plot.bottom())),
        );
        painter.rect_filled(band, 2.0, ACCENT.gamma_multiply(0.10));
    }

    dashed(
        painter,
        Pos2::new(plot.left(), y),
        Pos2::new(plot.right(), y),
        Stroke::new(1.2_f32, ACCENT),
    );
    let safe_top = plot.top() + TOP_PAD + 4.0;
    if detailed {
        graph_text(
            painter,
            Pos2::new(plot.left() + 40.0, y.max(safe_top + 12.0)),
            Align2::LEFT_BOTTOM,
            &format!("Thr {threshold:.1} dB"),
            ACCENT,
        );
    }

    if let Some(ratio) = param_value(params, "ratio") {
        graph_text(
            painter,
            Pos2::new(plot.right() - 8.0, safe_top),
            Align2::RIGHT_TOP,
            &format!("{ratio:.1}:1"),
            ACCENT,
        );
    }

    if detailed {
        if let Some(makeup) = param_value(params, "makeup_db") {
            if makeup.abs() > 0.05 {
                let marker_y = level_db_to_y(makeup, plot.top(), plot.bottom());
                painter.circle_filled(Pos2::new(plot.right() - 7.0, marker_y), 3.0, GR_COLOR);
                graph_text(
                    painter,
                    Pos2::new(plot.right() - 13.0, marker_y),
                    Align2::RIGHT_CENTER,
                    &format!("Makeup {makeup:+.1}"),
                    GR_COLOR,
                );
            }
        }
    }

    if param_value(params, "multiband").is_some_and(|value| value >= 0.5) {
        for key in ["low_xover", "high_xover"] {
            if let Some(freq) = param_value(params, key) {
                let x = frequency_to_x(freq, plot);
                dashed(
                    painter,
                    Pos2::new(x, plot.top()),
                    Pos2::new(x, plot.bottom()),
                    Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.45)),
                );
                if detailed {
                    graph_text(
                        painter,
                        Pos2::new(x, plot.bottom() - 4.0),
                        Align2::CENTER_BOTTOM,
                        &format_hz(freq),
                        MUTED,
                    );
                }
            }
        }
    }
}

fn limiter_overlay(
    painter: &egui::Painter,
    plot: Rect,
    params: &ParamSet,
    snapshot: &AnalysisSnapshot,
    detailed: bool,
) {
    let Some(ceiling) = param_value(params, "ceiling_db") else {
        return;
    };
    let y = level_db_to_y(ceiling, plot.top(), plot.bottom());

    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(plot.left(), plot.top()),
            Pos2::new(plot.right(), y),
        ),
        2.0,
        GR_COLOR.gamma_multiply(0.14),
    );
    painter.line_segment(
        [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
        Stroke::new(1.6_f32, GR_COLOR),
    );

    if detailed {
        let safe_top = plot.top() + TOP_PAD + 4.0;
        graph_text(
            painter,
            Pos2::new(plot.left() + 40.0, y.max(safe_top + 22.0)),
            Align2::LEFT_BOTTOM,
            &format!("Ceiling {ceiling:.1} dB"),
            GR_COLOR,
        );
        let input = snapshot.input_peak_db;
        let label = if input <= -99.0 {
            "In —".to_string()
        } else {
            format!("In {input:.1} dB")
        };
        graph_text(
            painter,
            Pos2::new(plot.left() + 40.0, safe_top),
            Align2::LEFT_TOP,
            &label,
            INPUT_LINE,
        );
    }
}

const GATE_DISPLAY_MIN_DB: f32 = -80.0;
const GATE_DISPLAY_MAX_DB: f32 = 0.0;

fn gate_transfer_db(input_db: f32, threshold: f32, ratio: f32, range: f32) -> f32 {
    let slope = (ratio.max(1.0) - 1.0).max(0.0);
    let depth = (threshold - input_db).max(0.0);
    input_db - (depth * slope).min(range.max(0.0))
}

fn paint_gate_transfer(
    painter: &egui::Painter,
    rect: Rect,
    editor: &BuiltinEditor,
    snapshot: &AnalysisSnapshot,
    detailed: bool,
) {
    let params = editor.params.as_ref();
    let dynamic = editor.kind == BuiltinKind::DynamicGate;
    let floor_db = snapshot.noise_floor_db;
    let threshold = if dynamic {
        let offset = param_value(params, "offset_db").unwrap_or(6.0);
        (floor_db + offset).clamp(GATE_DISPLAY_MIN_DB, GATE_DISPLAY_MAX_DB)
    } else {
        param_value(params, "threshold_db").unwrap_or(-45.0)
    };
    let ratio = param_value(params, "ratio").unwrap_or(10.0);
    let range = param_value(params, "range_db").unwrap_or(40.0);
    let hysteresis = param_value(params, "hysteresis_db").unwrap_or(0.0);

    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + 8.0, rect.top() + 10.0),
        Pos2::new(rect.right() - 40.0, rect.bottom() - 18.0),
    );
    if plot.width() <= 0.0 || plot.height() <= 0.0 {
        return;
    }
    let span = GATE_DISPLAY_MAX_DB - GATE_DISPLAY_MIN_DB;
    let x_of = |db: f32| {
        plot.left()
            + ((db.clamp(GATE_DISPLAY_MIN_DB, GATE_DISPLAY_MAX_DB) - GATE_DISPLAY_MIN_DB) / span)
                * plot.width()
    };
    let y_of = |db: f32| {
        plot.bottom()
            - ((db.clamp(GATE_DISPLAY_MIN_DB, GATE_DISPLAY_MAX_DB) - GATE_DISPLAY_MIN_DB) / span)
                * plot.height()
    };

    for db in [
        -80.0_f32, -70.0, -60.0, -50.0, -40.0, -30.0, -20.0, -10.0, 0.0,
    ] {
        let x = x_of(db);
        let y = y_of(db);
        let color = BORDER.gamma_multiply(if (db as i32) % 20 == 0 { 0.32 } else { 0.16 });
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(1.0_f32, color),
        );
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(1.0_f32, color),
        );
        if detailed && (db as i32) % 20 == 0 {
            let x_align = if db <= GATE_DISPLAY_MIN_DB {
                Align2::LEFT_BOTTOM
            } else if db >= GATE_DISPLAY_MAX_DB {
                Align2::RIGHT_BOTTOM
            } else {
                Align2::CENTER_BOTTOM
            };
            graph_text(
                painter,
                Pos2::new(x, plot.bottom() + 3.0),
                x_align,
                &format!("{db:.0}"),
                MUTED.gamma_multiply(0.7),
            );
            graph_text(
                painter,
                Pos2::new(plot.right() + 5.0, y),
                Align2::LEFT_CENTER,
                &format!("{db:.0}"),
                MUTED.gamma_multiply(0.7),
            );
        }
    }

    painter.rect_stroke(
        plot,
        0.0,
        Stroke::new(1.0_f32, BORDER.gamma_multiply(0.55)),
        StrokeKind::Inside,
    );
    painter.line_segment(
        [
            Pos2::new(x_of(GATE_DISPLAY_MIN_DB), y_of(GATE_DISPLAY_MIN_DB)),
            Pos2::new(x_of(GATE_DISPLAY_MAX_DB), y_of(GATE_DISPLAY_MAX_DB)),
        ],
        Stroke::new(1.0_f32, MUTED.gamma_multiply(0.35)),
    );

    let close = (threshold - hysteresis.max(0.0)).clamp(GATE_DISPLAY_MIN_DB, GATE_DISPLAY_MAX_DB);
    let threshold_x = x_of(threshold.clamp(GATE_DISPLAY_MIN_DB, GATE_DISPLAY_MAX_DB));
    let close_x = x_of(close);
    if threshold_x > close_x {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(close_x, plot.top()),
                Pos2::new(threshold_x, plot.bottom()),
            ),
            0.0,
            ACCENT.gamma_multiply(0.05),
        );
    }
    dashed(
        painter,
        Pos2::new(threshold_x, plot.top()),
        Pos2::new(threshold_x, plot.bottom()),
        Stroke::new(1.1_f32, INPUT_LINE.gamma_multiply(0.55)),
    );
    dotted(
        painter,
        Pos2::new(close_x, plot.top()),
        Pos2::new(close_x, plot.bottom()),
        Stroke::new(1.0_f32, MUTED.gamma_multiply(0.5)),
    );

    let steps = 160usize;
    let curve: Vec<Pos2> = (0..=steps)
        .map(|step| {
            let t = step as f32 / steps as f32;
            let input = GATE_DISPLAY_MIN_DB + t * span;
            Pos2::new(
                x_of(input),
                y_of(gate_transfer_db(input, threshold, ratio, range)),
            )
        })
        .collect();
    let glow = Color32::from_rgba_unmultiplied(0xF4, 0xF7, 0xFF, 28);
    painter.add(Shape::line(curve.clone(), Stroke::new(6.0_f32, glow)));
    painter.add(Shape::line(
        curve,
        Stroke::new(2.0_f32, Color32::from_rgb(0xF4, 0xF7, 0xFF)),
    ));

    if detailed {
        let label_x = threshold_x.clamp(plot.left() + 42.0, plot.right() - 42.0);
        let label = if dynamic { "AUTO" } else { "THR" };
        graph_text(
            painter,
            Pos2::new(label_x, plot.top() + 3.0),
            Align2::CENTER_TOP,
            &format!("{label} {threshold:.1} dB"),
            INPUT_LINE,
        );
        if dynamic {
            let floor = if floor_db > -99.0 {
                format!("FLOOR {floor_db:.1} dB")
            } else {
                "FLOOR -".to_string()
            };
            graph_text(
                painter,
                Pos2::new(plot.left() + 4.0, plot.top() + 3.0),
                Align2::LEFT_TOP,
                &floor,
                MUTED,
            );
        }
    }

    let input_db = snapshot.input_peak_db;
    let output_db = snapshot.output_peak_db;
    if input_db.is_finite() && output_db.is_finite() && input_db > -99.0 && output_db > -99.0 {
        let point = Pos2::new(x_of(input_db), y_of(output_db));
        painter.circle_filled(point, 5.0, ACCENT.gamma_multiply(0.18));
        painter.circle_stroke(point, 5.0, Stroke::new(1.2_f32, ACCENT));
        painter.circle_filled(point, 2.0, ACCENT);
    }
}

fn noise_removal_overlay(painter: &egui::Painter, plot: Rect, params: &ParamSet, detailed: bool) {
    if let Some(reduction) = param_value(params, "reduction_db") {
        let target = -reduction;
        let y = level_db_to_y(target, plot.top(), plot.bottom());
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(plot.left(), plot.top()),
                Pos2::new(plot.right(), y),
            ),
            2.0,
            GR_COLOR.gamma_multiply(0.09),
        );
        dashed(
            painter,
            Pos2::new(plot.left(), y),
            Pos2::new(plot.right(), y),
            Stroke::new(1.1_f32, GR_COLOR.gamma_multiply(0.9)),
        );
        if detailed {
            graph_text(
                painter,
                Pos2::new(plot.right() - 8.0, y - 2.0),
                Align2::RIGHT_BOTTOM,
                &format!("Target {target:.1} dB"),
                GR_COLOR,
            );
        }
    }

    if let Some(low_cut) = param_value(params, "low_cut_hz") {
        let x = frequency_to_x(low_cut, plot);
        dotted(
            painter,
            Pos2::new(x, plot.top()),
            Pos2::new(x, plot.bottom()),
            Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.55)),
        );
        if detailed {
            graph_text(
                painter,
                Pos2::new(x, plot.top() + 4.0),
                Align2::CENTER_TOP,
                &format!("Low cut {}", format_hz(low_cut)),
                MUTED,
            );
        }
    }
}

fn eq_overlay(
    painter: &egui::Painter,
    plot: Rect,
    params: &ParamSet,
    sample_rate: f32,
    detailed: bool,
) {
    let zero_y = map_db_to_y(
        0.0,
        plot.top(),
        plot.bottom(),
        EQ_RESPONSE_MAX_DB,
        EQ_RESPONSE_MIN_DB,
    );
    painter.line_segment(
        [
            Pos2::new(plot.left(), zero_y),
            Pos2::new(plot.right(), zero_y),
        ],
        Stroke::new(1.0_f32, MUTED.gamma_multiply(0.42)),
    );
    if detailed {
        graph_text(
            painter,
            Pos2::new(plot.left() + 40.0, zero_y - 1.0),
            Align2::LEFT_BOTTOM,
            "0 dB",
            MUTED,
        );
    }

    let steps = 128usize;
    let points: Vec<Pos2> = (0..=steps)
        .map(|step| {
            let t = step as f32 / steps as f32;
            let x = plot.left() + t * plot.width();
            let response = eq_magnitude_db(params, frequency_from_fraction(t), sample_rate);
            Pos2::new(
                x,
                map_db_to_y(
                    response,
                    plot.top(),
                    plot.bottom(),
                    EQ_RESPONSE_MAX_DB,
                    EQ_RESPONSE_MIN_DB,
                ),
            )
        })
        .collect();
    let glow = Color32::from_rgba_unmultiplied(EQ_CURVE.r(), EQ_CURVE.g(), EQ_CURVE.b(), 32);
    painter.add(Shape::line(points.clone(), Stroke::new(7.0_f32, glow)));
    let soft = Color32::from_rgba_unmultiplied(EQ_CURVE.r(), EQ_CURVE.g(), EQ_CURVE.b(), 84);
    painter.add(Shape::line(points.clone(), Stroke::new(3.0_f32, soft)));
    painter.add(Shape::line(points, Stroke::new(1.8_f32, EQ_CURVE)));

    for band in EQ_BANDS {
        let freq = match band.frequency {
            EqFrequency::Param(key) => match param_value(params, key) {
                Some(freq) => freq,
                None => continue,
            },
            EqFrequency::Fixed(freq) => freq,
        };
        let gain_db = band
            .gain_key
            .and_then(|key| param_value(params, key))
            .unwrap_or(0.0);
        let active = match band.shape {
            EqBandShape::HighPass => freq > eq::LOW_CUT_MIN_HZ + 0.01,
            EqBandShape::LowPass => freq < eq::HIGH_CUT_MAX_HZ - 0.01,
            _ => gain_db.abs() > 0.05,
        };
        if !active {
            continue;
        }

        let x = frequency_to_x(freq, plot);
        let response = eq_magnitude_db(params, freq, sample_rate);
        let node = Pos2::new(
            x,
            map_db_to_y(
                response,
                plot.top(),
                plot.bottom(),
                EQ_RESPONSE_MAX_DB,
                EQ_RESPONSE_MIN_DB,
            ),
        );
        let color = if band.gain_key.is_some() {
            EQ_CURVE
        } else {
            MUTED
        };
        painter.circle_filled(node, 4.5, color);
        painter.circle_stroke(
            node,
            4.5,
            Stroke::new(1.0_f32, Color32::WHITE.gamma_multiply(0.8)),
        );
    }
}

// ---------------------------------------------------------------------------
// EQ magnitude response (RBJ biquads, mirrors the DSP implementation)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum EqBandShape {
    LowShelf,
    Peak,
    HighShelf,
    HighPass,
    LowPass,
}

#[derive(Clone, Copy)]
enum EqFrequency {
    Param(&'static str),
    Fixed(f32),
}

#[derive(Clone, Copy)]
struct EqBand {
    shape: EqBandShape,
    frequency: EqFrequency,
    gain_key: Option<&'static str>,
    /// Q for peak/cut filters, shelf slope for shelves.
    q: f32,
}

const EQ_BANDS: [EqBand; 5] = [
    EqBand {
        shape: EqBandShape::HighPass,
        frequency: EqFrequency::Param("low_cut_hz"),
        gain_key: None,
        q: eq::CUT_Q,
    },
    EqBand {
        shape: EqBandShape::LowShelf,
        frequency: EqFrequency::Fixed(eq::BASS_HZ),
        gain_key: Some("bass_db"),
        q: eq::SHELF_SLOPE,
    },
    EqBand {
        shape: EqBandShape::Peak,
        frequency: EqFrequency::Fixed(eq::MID_HZ),
        gain_key: Some("mid_db"),
        q: eq::MID_Q,
    },
    EqBand {
        shape: EqBandShape::HighShelf,
        frequency: EqFrequency::Fixed(eq::TREBLE_HZ),
        gain_key: Some("treble_db"),
        q: eq::SHELF_SLOPE,
    },
    EqBand {
        shape: EqBandShape::LowPass,
        frequency: EqFrequency::Param("high_cut_hz"),
        gain_key: None,
        q: eq::CUT_Q,
    },
];

#[derive(Clone, Copy, Debug)]
struct ResponseCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

const IDENTITY_RESPONSE: ResponseCoeffs = ResponseCoeffs {
    b0: 1.0,
    b1: 0.0,
    b2: 0.0,
    a1: 0.0,
    a2: 0.0,
};

fn eq_magnitude_db(params: &ParamSet, freq_hz: f32, sample_rate: f32) -> f32 {
    let rate = if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        48_000.0
    };
    let mut total = 0.0;
    for band in EQ_BANDS {
        let freq = match band.frequency {
            EqFrequency::Param(key) => {
                if params.index_of(key).is_none() {
                    continue;
                }
                params.get_key(key)
            }
            EqFrequency::Fixed(freq) => freq,
        };
        let gain_db = band.gain_key.map(|key| params.get_key(key)).unwrap_or(0.0);
        let active = match band.shape {
            EqBandShape::HighPass => freq > eq::LOW_CUT_MIN_HZ + 0.01,
            EqBandShape::LowPass => freq < eq::HIGH_CUT_MAX_HZ - 0.01,
            EqBandShape::LowShelf | EqBandShape::Peak | EqBandShape::HighShelf => {
                gain_db.abs() >= 1.0e-6
            }
        };
        if !active {
            continue;
        }
        let coeffs = match band.shape {
            EqBandShape::LowShelf => low_shelf_coeffs(rate, freq, gain_db, band.q),
            EqBandShape::Peak => peaking_coeffs(rate, freq, band.q, gain_db),
            EqBandShape::HighShelf => high_shelf_coeffs(rate, freq, gain_db, band.q),
            EqBandShape::HighPass => high_pass_coeffs(rate, freq, band.q),
            EqBandShape::LowPass => low_pass_coeffs(rate, freq, band.q),
        };
        total += response_db(coeffs, freq_hz, rate);
    }
    if params.index_of("output_db").is_some() {
        total += params.get_key("output_db");
    }
    total
}

fn sanitized_q(q: f32) -> f64 {
    f64::from(if q.is_finite() { q.max(1.0e-4) } else { 0.707 })
}

fn rbj_terms(sample_rate: f32, freq_hz: f32) -> Option<(f64, f64)> {
    if !sample_rate.is_finite() || sample_rate <= 0.0 || !freq_hz.is_finite() || freq_hz <= 0.0 {
        return None;
    }
    let nyquist = f64::from(sample_rate) * 0.5;
    let freq = f64::from(freq_hz).min(nyquist * 0.99);
    let w0 = 2.0 * std::f64::consts::PI * freq / f64::from(sample_rate);
    Some(w0.sin_cos())
}

fn normalized_coeffs(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> ResponseCoeffs {
    if !a0.is_finite() || a0 == 0.0 {
        return IDENTITY_RESPONSE;
    }
    let inv = 1.0 / a0;
    ResponseCoeffs {
        b0: (b0 * inv) as f32,
        b1: (b1 * inv) as f32,
        b2: (b2 * inv) as f32,
        a1: (a1 * inv) as f32,
        a2: (a2 * inv) as f32,
    }
}

fn peaking_coeffs(sample_rate: f32, freq_hz: f32, q: f32, gain_db: f32) -> ResponseCoeffs {
    let Some((sin_w0, cos_w0)) = rbj_terms(sample_rate, freq_hz) else {
        return IDENTITY_RESPONSE;
    };
    let amp = 10.0_f64.powf(f64::from(gain_db) / 40.0);
    let alpha = sin_w0 / (2.0 * sanitized_q(q));
    normalized_coeffs(
        1.0 + alpha * amp,
        -2.0 * cos_w0,
        1.0 - alpha * amp,
        1.0 + alpha / amp,
        -2.0 * cos_w0,
        1.0 - alpha / amp,
    )
}

fn low_shelf_coeffs(sample_rate: f32, freq_hz: f32, gain_db: f32, slope: f32) -> ResponseCoeffs {
    let Some((sin_w0, cos_w0)) = rbj_terms(sample_rate, freq_hz) else {
        return IDENTITY_RESPONSE;
    };
    let amp = 10.0_f64.powf(f64::from(gain_db) / 40.0);
    let s = f64::from(if slope.is_finite() {
        slope.clamp(0.1, 1.0)
    } else {
        1.0
    });
    let alpha = sin_w0 * 0.5 * ((amp + 1.0 / amp) * (1.0 / s - 1.0) + 2.0).sqrt();
    let sqrt_amp = amp.sqrt();
    normalized_coeffs(
        amp * ((amp + 1.0) - (amp - 1.0) * cos_w0 + 2.0 * sqrt_amp * alpha),
        2.0 * amp * ((amp - 1.0) - (amp + 1.0) * cos_w0),
        amp * ((amp + 1.0) - (amp - 1.0) * cos_w0 - 2.0 * sqrt_amp * alpha),
        (amp + 1.0) + (amp - 1.0) * cos_w0 + 2.0 * sqrt_amp * alpha,
        -2.0 * ((amp - 1.0) + (amp + 1.0) * cos_w0),
        (amp + 1.0) + (amp - 1.0) * cos_w0 - 2.0 * sqrt_amp * alpha,
    )
}

fn high_shelf_coeffs(sample_rate: f32, freq_hz: f32, gain_db: f32, slope: f32) -> ResponseCoeffs {
    let Some((sin_w0, cos_w0)) = rbj_terms(sample_rate, freq_hz) else {
        return IDENTITY_RESPONSE;
    };
    let amp = 10.0_f64.powf(f64::from(gain_db) / 40.0);
    let s = f64::from(if slope.is_finite() {
        slope.clamp(0.1, 1.0)
    } else {
        1.0
    });
    let alpha = sin_w0 * 0.5 * ((amp + 1.0 / amp) * (1.0 / s - 1.0) + 2.0).sqrt();
    let sqrt_amp = amp.sqrt();
    normalized_coeffs(
        amp * ((amp + 1.0) + (amp - 1.0) * cos_w0 + 2.0 * sqrt_amp * alpha),
        -2.0 * amp * ((amp - 1.0) + (amp + 1.0) * cos_w0),
        amp * ((amp + 1.0) + (amp - 1.0) * cos_w0 - 2.0 * sqrt_amp * alpha),
        (amp + 1.0) - (amp - 1.0) * cos_w0 + 2.0 * sqrt_amp * alpha,
        2.0 * ((amp - 1.0) - (amp + 1.0) * cos_w0),
        (amp + 1.0) - (amp - 1.0) * cos_w0 - 2.0 * sqrt_amp * alpha,
    )
}

fn high_pass_coeffs(sample_rate: f32, freq_hz: f32, q: f32) -> ResponseCoeffs {
    let Some((sin_w0, cos_w0)) = rbj_terms(sample_rate, freq_hz) else {
        return IDENTITY_RESPONSE;
    };
    let alpha = sin_w0 / (2.0 * sanitized_q(q));
    normalized_coeffs(
        (1.0 + cos_w0) * 0.5,
        -(1.0 + cos_w0),
        (1.0 + cos_w0) * 0.5,
        1.0 + alpha,
        -2.0 * cos_w0,
        1.0 - alpha,
    )
}

fn low_pass_coeffs(sample_rate: f32, freq_hz: f32, q: f32) -> ResponseCoeffs {
    let Some((sin_w0, cos_w0)) = rbj_terms(sample_rate, freq_hz) else {
        return IDENTITY_RESPONSE;
    };
    let alpha = sin_w0 / (2.0 * sanitized_q(q));
    normalized_coeffs(
        (1.0 - cos_w0) * 0.5,
        1.0 - cos_w0,
        (1.0 - cos_w0) * 0.5,
        1.0 + alpha,
        -2.0 * cos_w0,
        1.0 - alpha,
    )
}

fn response_db(coeffs: ResponseCoeffs, freq_hz: f32, sample_rate: f32) -> f32 {
    if !sample_rate.is_finite() || sample_rate <= 0.0 || !freq_hz.is_finite() || freq_hz <= 0.0 {
        return 0.0;
    }
    let nyquist = f64::from(sample_rate) * 0.5;
    let freq = f64::from(freq_hz).min(nyquist * 0.999);
    let w = 2.0 * std::f64::consts::PI * freq / f64::from(sample_rate);
    let (sin_w, cos_w) = w.sin_cos();
    let (sin_2w, cos_2w) = (2.0 * w).sin_cos();

    let b0 = f64::from(coeffs.b0);
    let b1 = f64::from(coeffs.b1);
    let b2 = f64::from(coeffs.b2);
    let a1 = f64::from(coeffs.a1);
    let a2 = f64::from(coeffs.a2);

    let num_re = b0 + b1 * cos_w + b2 * cos_2w;
    let num_im = -(b1 * sin_w + b2 * sin_2w);
    let den_re = 1.0 + a1 * cos_w + a2 * cos_2w;
    let den_im = -(a1 * sin_w + a2 * sin_2w);

    let numerator = num_re * num_re + num_im * num_im;
    let denominator = (den_re * den_re + den_im * den_im).max(1.0e-24);
    let magnitude = (numerator / denominator).sqrt().max(1.0e-9);
    (20.0 * magnitude.log10()) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use michost_builtin::ParamCurve;
    use std::f32::consts::PI;

    const fn spec(
        key: &'static str,
        section: &'static str,
        visible_when: Option<(&'static str, f32)>,
    ) -> ParamSpec {
        ParamSpec {
            key,
            name: key,
            unit: "",
            min: 0.0,
            max: 1.0,
            default: 0.0,
            curve: ParamCurve::Linear,
            decimals: 2,
            style: ParamStyle::Knob,
            section,
            visible_when,
        }
    }

    static TEST_SPECS: &[ParamSpec] = &[
        spec("attack", "Dynamics", None),
        spec("tone", "Tone", None),
        spec("release", "Dynamics", Some(("switch", 1.0))),
        spec("mix", "Tone", None),
        spec("sidechain", "Dynamics", None),
    ];

    #[test]
    fn sections_keep_first_appearance_order() {
        let groups = group_sections(TEST_SPECS);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "Dynamics");
        assert_eq!(groups[0].1, vec![0, 2, 4]);
        assert_eq!(groups[1].0, "Tone");
        assert_eq!(groups[1].1, vec![1, 3]);
    }

    #[test]
    fn visible_when_uses_tolerance_and_hides_unknown_keys() {
        let conditional = &TEST_SPECS[2];
        assert!(is_visible_with(conditional, |key| (key == "switch").then_some(1.0)));
        assert!(is_visible_with(conditional, |key| {
            (key == "switch").then_some(1.000_01)
        }));
        assert!(!is_visible_with(conditional, |key| {
            (key == "switch").then_some(0.0)
        }));
        assert!(!is_visible_with(conditional, |_| None));
        assert!(is_visible_with(&TEST_SPECS[0], |_| None));
    }

    #[test]
    fn knob_arc_maps_normalized_to_angles() {
        assert!((knob_angle(0.0) - KNOB_START).abs() < 1e-5);
        assert!((knob_angle(1.0) - (KNOB_START + KNOB_SWEEP)).abs() < 1e-4);
        assert!((knob_angle(0.5) - 1.5 * PI).abs() < 1e-4);
        assert_eq!(knob_angle(-1.0), knob_angle(0.0));
        assert_eq!(knob_angle(2.0), knob_angle(1.0));
    }

    #[test]
    fn drag_is_inverted_fine_and_clamped() {
        assert!((drag_normalized(0.5, -11.0, false) - 0.55).abs() < 1e-4);
        assert!((drag_normalized(0.5, -11.0, true) - (0.5 + 11.0 / 900.0)).abs() < 1e-4);
        assert_eq!(drag_normalized(0.0, 100.0, false), 0.0);
        assert_eq!(drag_normalized(1.0, -100.0, false), 1.0);
    }

    #[test]
    fn grid_columns_respond_to_width() {
        assert_eq!(grid_columns(666.0), 6);
        assert_eq!(grid_columns(506.0), 4);
        assert_eq!(grid_columns(366.0), 3);
        assert_eq!(grid_columns(200.0), 2);
    }

    #[test]
    fn fft_normalization_recovers_sine_amplitude() {
        let plan = FftPlan::new();
        let target_bin = 64usize;
        let mut re: Vec<f32> = plan
            .window
            .iter()
            .enumerate()
            .map(|(index, weight)| {
                let phase = 2.0 * PI * target_bin as f32 * index as f32 / FFT_SIZE as f32;
                0.5 * phase.sin() * weight
            })
            .collect();
        let mut im = vec![0.0f32; FFT_SIZE];
        fft_in_place(&mut re, &mut im, &plan.cos, &plan.sin);

        let amplitude = bin_amplitude(&re, &im, target_bin, plan.window_sum);
        assert!((amplitude - 0.5).abs() < 0.01, "amplitude {amplitude}");
        let neighbour = bin_amplitude(&re, &im, 40, plan.window_sum);
        assert!(neighbour < 0.02, "neighbour {neighbour}");
    }

    #[test]
    fn log_bins_span_20hz_to_20khz_and_map_inside_the_fft() {
        assert!((log_bin_frequency(0.0) - SPECTRUM_MIN_HZ).abs() < 1e-3);
        assert!((log_bin_frequency(1.0) - SPECTRUM_MAX_HZ).abs() < 0.1);

        let mut previous = 0.0f32;
        for bin in 0..SPECTRUM_BINS {
            let low = log_bin_frequency(bin as f32 / SPECTRUM_BINS as f32);
            let high = log_bin_frequency((bin + 1) as f32 / SPECTRUM_BINS as f32);
            let middle = log_bin_frequency((bin as f32 + 0.5) / SPECTRUM_BINS as f32);
            assert!(low >= previous - 1e-3);
            assert!(low < middle && middle < high);
            previous = low;

            let (first, last) = fft_bin_range(low, high, FFT_SIZE, 48_000.0);
            assert!(first >= 1 && last <= FFT_SIZE / 2 && first <= last);
        }
    }

    #[test]
    fn db_maps_to_graph_height_and_clamps() {
        let top = 10.0f32;
        let bottom = 110.0f32;
        assert!((level_db_to_y(0.0, top, bottom) - top).abs() < 1e-4);
        assert!((level_db_to_y(-60.0, top, bottom) - bottom).abs() < 1e-4);
        assert!((level_db_to_y(-30.0, top, bottom) - 60.0).abs() < 1e-4);
        assert_eq!(level_db_to_y(12.0, top, bottom), top);
        assert_eq!(level_db_to_y(-120.0, top, bottom), bottom);
        assert_eq!(level_db_to_y(f32::NEG_INFINITY, top, bottom), bottom);
    }

    #[test]
    fn loudness_history_keeps_the_newest_256_points_in_order() {
        let mut history = LoudnessHistory::new();
        for index in 0..300u32 {
            history.push(HistoryPoint {
                input_db: index as f32,
                output_db: index as f32 - 1.0,
                reduction_db: 1.0,
            });
        }
        assert_eq!(history.count(), HISTORY_LEN);
        assert_eq!(history.get(0).unwrap().input_db, 44.0);
        assert_eq!(history.newest().unwrap().input_db, 299.0);
        assert!(history.get(HISTORY_LEN).is_none());
    }

    #[test]
    fn display_ballistics_are_time_based_and_asymmetric() {
        let attack_step = follow(
            0.0,
            12.0,
            LEVEL_ATTACK_SECONDS,
            LEVEL_RELEASE_SECONDS,
            0.033,
        );
        let release_step = follow(
            12.0,
            0.0,
            LEVEL_ATTACK_SECONDS,
            LEVEL_RELEASE_SECONDS,
            0.033,
        );
        let attack_change = attack_step;
        let release_change = 12.0 - release_step;
        assert!(attack_change > 4.0);
        assert!(release_change < attack_change);
        let faster = follow(
            0.0,
            12.0,
            LEVEL_ATTACK_SECONDS,
            LEVEL_RELEASE_SECONDS,
            0.016,
        );
        assert!(faster < attack_step);
    }

    #[test]
    fn loudness_history_samples_by_time_not_frame_count() {
        let point = HistoryPoint {
            input_db: -20.0,
            output_db: -25.0,
            reduction_db: 5.0,
        };
        let mut history = LoudnessHistory::new();
        history.sample(0.0, point);
        assert_eq!(history.count(), 1);
        for _ in 0..5 {
            history.sample(0.01, point);
        }
        assert_eq!(history.count(), 1);
        history.sample(0.034, point);
        assert_eq!(history.count(), 2);
        history.sample(0.035, point);
        assert_eq!(history.count(), 2);
        history.sample(1.0, point);
        assert_eq!(history.count(), 6);
        history.sample(1.01, point);
        assert_eq!(history.count(), 6);
    }

    #[test]
    fn loudness_points_sanitize_analysis_values() {
        let snapshot = AnalysisSnapshot {
            sample_rate: 48_000.0,
            input_peak_db: 12.0,
            output_peak_db: f32::NAN,
            reduction_db: 100.0,
            noise_floor_db: -120.0,
        };
        let point = HistoryPoint::from_snapshot(&snapshot);
        assert_eq!(point.input_db, 0.0);
        assert_eq!(point.output_db, -120.0);
        assert_eq!(point.reduction_db, 60.0);
    }

    #[test]
    fn gate_transfer_curve_matches_ratio_and_range() {
        assert!((gate_transfer_db(-20.0, -40.0, 10.0, 40.0) + 20.0).abs() < 1e-6);
        assert!((gate_transfer_db(-50.0, -40.0, 2.0, 40.0) + 60.0).abs() < 1e-4);
        assert!((gate_transfer_db(-80.0, -20.0, 100.0, 40.0) + 120.0).abs() < 1e-3);
    }

    #[test]
    fn background_modes_follow_the_reference_groups() {
        for kind in [
            BuiltinKind::Eq,
            BuiltinKind::NoiseRemoval,
            BuiltinKind::PitchShift,
        ] {
            assert_eq!(background_mode(kind), BackgroundMode::Spectrum);
        }
        for kind in [BuiltinKind::NoiseGate, BuiltinKind::DynamicGate] {
            assert_eq!(background_mode(kind), BackgroundMode::Transfer);
        }
        for kind in [
            BuiltinKind::Gain,
            BuiltinKind::Compressor,
            BuiltinKind::Limiter,
            BuiltinKind::Reverb,
            BuiltinKind::Delay,
            BuiltinKind::Echo,
        ] {
            assert_eq!(background_mode(kind), BackgroundMode::Loudness);
        }
    }

    #[test]
    fn eq_magnitude_is_flat_at_defaults_and_follows_the_bands() {
        let sample_rate = 48_000.0f32;
        let params = ParamSet::new(BuiltinKind::Eq);
        for freq in [30.0f32, 100.0, 1_000.0, 10_000.0] {
            assert!(eq_magnitude_db(&params, freq, sample_rate).abs() < 1e-3);
        }

        let set = |key: &str, value: f32| {
            params.set(params.index_of(key).unwrap(), value);
        };

        set("bass_db", 12.0);
        let bass = eq_magnitude_db(&params, 30.0, sample_rate);
        assert!(bass > 6.0, "bass shelf response {bass}");
        set("bass_db", 0.0);

        set("treble_db", -12.0);
        let treble = eq_magnitude_db(&params, 18_000.0, sample_rate);
        assert!(treble < -6.0, "treble shelf response {treble}");

        set("low_cut_hz", 200.0);
        let cut = eq_magnitude_db(&params, 50.0, sample_rate);
        assert!(cut < -12.0, "low cut response {cut}");
        set("low_cut_hz", eq::LOW_CUT_MIN_HZ);

        set("high_cut_hz", 2_000.0);
        let high_cut = eq_magnitude_db(&params, 10_000.0, sample_rate);
        assert!(high_cut < -12.0, "high cut response {high_cut}");
        set("high_cut_hz", eq::HIGH_CUT_MAX_HZ);

        set("output_db", 6.0);
        let shifted = eq_magnitude_db(&params, 18_000.0, sample_rate);
        assert!((shifted - (treble + 6.0)).abs() < 0.05);
    }
}
