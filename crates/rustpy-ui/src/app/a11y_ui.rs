//! Accessibility in the application (roadmap §1.6): the High Contrast theme.
//! (Alt+1 and the Project view's keyboard handling are in `app.rs` and
//! `project_tree.rs`.)

use super::*;

impl IdeApp {
    pub(super) fn apply_custom_ui_theme(
        &mut self,
        name: &str,
        theme: UiTheme,
        ctx: &egui::Context,
    ) {
        self.theme = theme;
        self.settings.appearance.theme = name.into();
        self.apply_ui_font_size(ctx);
        self.settings_ui.message =
            Some("Custom UI theme applied; editor color scheme is configured separately".into());
    }

    /// Selects UI theme `name` ("Dark", "Light" or "High Contrast"). High
    /// Contrast brings the High Contrast editor scheme; leaving it brings back
    /// the scheme last used with the new theme. Does not save.
    pub(super) fn set_ui_theme(&mut self, name: &str, ctx: &egui::Context) {
        if name == "High Contrast" {
            self.settings.menus.appearance.sync_with_os = false;
        }
        if name == self.settings.appearance.theme
            && name == self.theme.name()
            && !name.starts_with(rustpy_settings::ui_themes::PREFIX)
        {
            return;
        }
        if name.starts_with(rustpy_settings::ui_themes::PREFIX) {
            match crate::theme::custom::load(&Settings::config_dir(), name) {
                Ok(theme) => self.apply_custom_ui_theme(name, theme, ctx),
                Err(e) => {
                    self.settings_ui.message = Some(format!(
                        "Cannot apply UI theme; keeping current theme colors: {e}"
                    ))
                }
            }
            return;
        }
        if name != "High Contrast" {
            self.switch_theme(name != "Light", ctx);
            return;
        }
        // Remember the scheme in use, to restore it when High Contrast is left.
        if self.scheme.dark == self.theme.dark && !self.theme.high_contrast {
            if self.theme.dark {
                self.settings.appearance.dark_scheme = self.scheme.name.clone();
            } else {
                self.settings.appearance.light_scheme = self.scheme.name.clone();
            }
        }
        self.theme = UiTheme::high_contrast();
        self.settings.appearance.theme = "High Contrast".into();
        if let Some(s) = settings_ui::scheme_by_name("High Contrast") {
            self.scheme = s;
        }
        self.settings.editor.color_scheme = self.scheme.name.clone();
        self.apply_ui_font_size(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_selection_reload_and_cancel_preserve_editor_scheme_and_presets() {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Settings::default()
            },
            None,
        );
        let ctx = egui::Context::default();
        app.scheme.name = "My editor scheme".into();
        app.settings.editor.color_scheme = app.scheme.name.clone();
        app.settings_ui.snapshot = Some(Box::new(settings_ui::SettingsSnapshot {
            tool_bar_options: (
                app.layout.tools.saved.side_by_side,
                app.layout.tools.saved.group_tabs,
            ),
            project_watchers: Vec::new(),
            watcher_root: None,
            settings: app.settings.clone(),
            theme: app.theme.clone(),
            scheme: app.scheme.clone(),
            keymap: app.keymap.clone(),
            frame: app.frame_count,
        }));
        let mut def = rustpy_settings::ui_themes::Theme::parse(
            br##"{"name":"Night","base":"Light","colors":{"accent":"#123456"}}"##,
        )
        .unwrap();
        let id = "custom:night.json";
        app.apply_custom_ui_theme(
            id,
            crate::theme::custom::from_definition(id, &def).unwrap(),
            &ctx,
        );
        assert_eq!(app.settings.appearance.theme, id);
        assert!(
            !app.theme.dark && app.scheme.dark,
            "custom light chrome leaves dark editor scheme intact"
        );
        assert_eq!(app.scheme.name, "My editor scheme");
        def.colors.insert("accent".into(), "#abcdef".into());
        app.apply_custom_ui_theme(
            id,
            crate::theme::custom::from_definition(id, &def).unwrap(),
            &ctx,
        );
        assert_eq!(app.theme.accent, egui::Color32::from_rgb(171, 205, 239));
        assert_eq!(app.scheme.name, "My editor scheme");
        app.restore_settings_snapshot(&ctx);
        assert_eq!(app.theme.name(), "Dark");
        assert_eq!(app.settings.appearance.theme, "Dark");
        assert_eq!(app.scheme.name, "My editor scheme");
        app.set_ui_theme("High Contrast", &ctx);
        assert!(app.theme.high_contrast);
        assert_eq!(app.scheme.name, "High Contrast");
        app.set_ui_theme("Light", &ctx);
        assert!(!app.theme.dark && !app.theme.high_contrast);
    }

    #[test]
    fn broken_custom_selection_keeps_last_valid_theme_and_reports_error() {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Settings::default()
            },
            None,
        );
        let ctx = egui::Context::default();
        app.set_ui_theme("Light", &ctx);
        let before = app.theme.accent;
        app.set_ui_theme("custom:../invalid.json", &ctx);
        assert_eq!(app.settings.appearance.theme, "Light");
        assert_eq!(app.theme.accent, before);
        assert!(app
            .settings_ui
            .message
            .as_ref()
            .unwrap()
            .contains("Cannot apply UI theme"));
        // A broken startup selection falls back to Dark visually. Choosing Dark
        // must still replace the broken persistent identifier.
        app.settings.appearance.theme = "custom:missing.json".into();
        app.theme = UiTheme::dark();
        app.set_ui_theme("Dark", &ctx);
        assert_eq!(app.settings.appearance.theme, "Dark");
    }
}
