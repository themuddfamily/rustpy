//! Help › Diagnostic Tools › Debug Log Settings: per-module log categories,
//! saved in the diagnostics settings and applied to the running logger.
use super::*;
use crate::a11y::DialogWindow;

#[derive(Default)]
pub(super) struct State {
    open: bool,
    text: String,
    message: Option<String>,
    /// The categories field takes the keyboard on the first frame after opening.
    focus_text: bool,
}

const TEXT_ID: &str = "debug_log_settings_text";

/// Installs the saved categories at startup and after a settings reload; a
/// saved text that no longer parses leaves logging as `RUST_LOG` configures it.
pub(super) fn apply_saved(settings: &Settings) {
    if let Err(error) =
        rustpy_settings::debug_log::apply(&settings.diagnostics.debug_log_categories)
    {
        log::warn!("Ignoring saved debug log categories: {error}");
    }
}

impl IdeApp {
    pub(super) fn open_debug_log_settings(&mut self) {
        let state = &mut self.debug_log_settings;
        state.open = true;
        state.text = self.settings.diagnostics.debug_log_categories.clone();
        state.message = None;
        state.focus_text = true;
    }

    /// Validates, applies and saves the dialog text; `Err` keeps the dialog open.
    fn save_debug_log_settings(&mut self) -> Result<usize, String> {
        let text = self.debug_log_settings.text.trim().to_string();
        let count = rustpy_settings::debug_log::apply(&text)?;
        self.settings.diagnostics.debug_log_categories = text.clone();
        if let Some(snapshot) = &mut self.settings_ui.snapshot {
            snapshot.settings.diagnostics.debug_log_categories = text;
        }
        self.settings_to_persist()
            .save()
            .map_err(|error| format!("Cannot save debug log settings: {error}"))?;
        Ok(count)
    }

    pub(super) fn debug_log_settings_ui(&mut self, ctx: &egui::Context) {
        if !self.debug_log_settings.open {
            return;
        }
        let mut open = true;
        let (mut ok, mut cancel) = (false, false);
        let state = &mut self.debug_log_settings;
        egui::Window::new(crate::i18n::tr_static("Debug Log Settings"))
            .open(&mut open)
            .collapsible(false)
            .default_width(520.0)
            .show_modal(ctx, |ui| {
                ui.label(crate::i18n::tr_static(
                    "Enter debug categories, one per line: a module path logs at debug \
                     (rustpy_index), or add a level (rustpy_debug:trace, #rustpy_vcs:info). \
                     The categories take effect immediately and add to RUST_LOG.",
                ));
                let text = ui.add(
                    egui::TextEdit::multiline(&mut state.text)
                        .id(egui::Id::new(TEXT_ID))
                        .code_editor()
                        .desired_rows(10)
                        .desired_width(f32::INFINITY)
                        .hint_text("rustpy_index\nrustpy_debug:trace"),
                );
                if std::mem::take(&mut state.focus_text) {
                    text.request_focus();
                }
                if let Some(message) = &state.message {
                    ui.colored_label(ui.visuals().warn_fg_color, message);
                }
                ui.horizontal(|ui| {
                    ok = ui.button(crate::i18n::tr_static("OK")).clicked();
                    cancel = ui.button(crate::i18n::tr_static("Cancel")).clicked();
                    if ui
                        .button(crate::i18n::tr_static("Show Log in Files"))
                        .clicked()
                    {
                        self.pending_action = Some(Action::ShowLogInFiles);
                        ui.ctx().request_repaint();
                    }
                });
            });
        if ok {
            match self.save_debug_log_settings() {
                Ok(count) => {
                    self.debug_log_settings.open = false;
                    self.set_status(format!("Debug log settings applied ({count} categories)"));
                }
                Err(error) => self.debug_log_settings.message = Some(error),
            }
        } else if cancel || !open {
            self.debug_log_settings.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_focuses_the_categories_field() {
        let mut app = IdeApp::new(Settings::default(), None);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        app.open_debug_log_settings();
        for _ in 0..2 {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                app.debug_log_settings_ui(ui.ctx());
            })
            .drop_without_applying_deltas();
        }
        assert!(ctx.memory(|m| m.has_focus(egui::Id::new(TEXT_ID))));
        // Focus is requested once: the user may then move it elsewhere.
        assert!(!app.debug_log_settings.focus_text);
    }
}
