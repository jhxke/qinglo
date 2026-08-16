use egui::{RichText, ScrollArea, Ui};

use super::state::UiState;

pub fn render_settings_view(ui: &mut Ui, _state: &mut UiState) {
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.heading("系统设置");
            ui.separator();
            ui.add_space(20.0);
            ui.label(RichText::new("暂无设置项").weak());
        });
}
