use super::super::*;

use eframe::egui;

impl MicHostApp {
    pub(crate) fn show_builtin_editors(&mut self, ctx: &egui::Context) {
        if self.builtin_editors.is_empty() {
            return;
        }
        let mut keep = Vec::with_capacity(self.builtin_editors.len());
        for editor in std::mem::take(&mut self.builtin_editors) {
            if builtin_ui::show(ctx, &editor) {
                keep.push(editor);
            }
        }
        self.builtin_editors = keep;
        if !self.builtin_editors.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}
