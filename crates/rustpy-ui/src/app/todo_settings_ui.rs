//! Root mounts this panel in Settings and owns dirty-page registration.
use super::*;
impl IdeApp {
    pub(super) fn todo_settings(&mut self, ui: &mut egui::Ui) {
        let before = self.settings.todo.clone();
        ui.heading("TODO patterns");
        ui.label("Regex patterns apply to selected lexical contexts; case and color belong to each pattern.");
        ui.checkbox(
            &mut self.settings.todo.comments,
            "Show TODO in comments / prose",
        );
        ui.checkbox(&mut self.settings.todo.strings, "Show TODO in strings");
        ui.checkbox(
            &mut self.settings.todo.multiline,
            "Include indented continuation comment lines",
        );
        let mut remove = None;
        for (index, pattern) in self.settings.todo.patterns.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut pattern.regex);
                    ui.checkbox(&mut pattern.case_sensitive, "Case sensitive");
                    ui.color_edit_button_srgb(&mut pattern.color);
                    if ui.small_button("Remove").clicked() {
                        remove = Some(index)
                    }
                });
            });
        }
        if let Some(index) = remove {
            self.settings.todo.patterns.remove(index);
        }
        if ui
            .add_enabled(
                self.settings.todo.patterns.len() < 64,
                egui::Button::new("Add pattern"),
            )
            .clicked()
        {
            self.settings.todo.patterns.push(Default::default());
        }
        if let Err(error) = crate::todo_patterns::Patterns::compile(&self.settings.todo) {
            ui.colored_label(egui::Color32::RED, error);
            ui.label("Invalid patterns are not scanned; correct the expression to resume.");
        }
        if before != self.settings.todo {
            self.todo_refreshed = -100.0;
            self.todo_items.clear();
            self.todo_colors.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_settings_frame_exposes_case_color_continuation_and_invalid_regex() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.settings.todo.patterns[0].regex = "[".into();
        let before = app.settings.todo.clone();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| app.todo_settings(ui),
        );
        for label in [
            "TODO patterns",
            "Case sensitive",
            "Include indented continuation comment lines",
            "Add pattern",
        ] {
            assert!(output.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text()==label)),"missing actual settings control {label}");
        }
        assert!(output.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text().contains("Pattern 1:"))));
        assert_eq!(app.settings.todo, before);
        output.drop_without_applying_deltas();
    }
}
