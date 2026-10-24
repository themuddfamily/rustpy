//! Zen combines distraction-free layout and fullscreen without changing stored
//! preferences or font scale. Native commands target the current project viewport.
use super::*;

pub(super) struct State {
    pub snapshot: layout::ViewSnapshot,
    pub fullscreen: bool,
}
impl IdeApp {
    pub(super) fn toggle_zen_mode(&mut self, ctx: &egui::Context) {
        if self.layout.zen.is_some() {
            self.exit_zen_mode(ctx);
            return;
        }
        // Re-entry in the same frame must not consume a stale native restore.
        let fullscreen = self
            .layout
            .zen_native_restore
            .take()
            .unwrap_or_else(|| ctx.input(|input| input.viewport().fullscreen.unwrap_or(false)));
        self.layout.zen = Some(State {
            snapshot: self.capture_zen_layout(),
            fullscreen,
        });
        self.hide_for_zen();
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        self.set_status("Zen Mode: press Esc or use Exit Zen Mode to restore the previous view");
    }
    pub(super) fn exit_zen_mode(&mut self, ctx: &egui::Context) {
        let Some(state) = self.layout.zen.take() else {
            return;
        };
        self.restore_zen_layout(state.snapshot);
        self.layout.zen_native_restore = Some(state.fullscreen);
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(state.fullscreen));
        ctx.request_repaint();
    }
    pub(super) fn zen_mode_frame(&mut self, ctx: &egui::Context) {
        if let Some(fullscreen) = self.layout.zen_native_restore.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
        }
        if self.layout.zen.is_none() {
            return;
        }
        // Remains available even when menu/toolbar/status are disabled or Esc is
        // currently owned by a popup, selection or input-method interaction.
        let mut exit = false;
        egui::Area::new(egui::Id::new("zen-mode-exit"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    exit = ui.button(Action::ExitZenMode.text()).clicked();
                });
            });
        if exit {
            self.handle_action(Action::ExitZenMode, ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> IdeApp {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Default::default()
            },
            None,
        );
        app.push_tab(EditorState::from_text("value = 'λ'\n", Language::Python));
        app.bottom = Some(BottomTool::Run);
        app.zoom = 2.0;
        app
    }
    fn input(fullscreen: bool) -> egui::RawInput {
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .fullscreen = Some(fullscreen);
        input
    }
    fn commanded(output: &egui::FullOutput, fullscreen: bool) -> bool {
        output.viewport_output.values().any(|viewport| {
            viewport.commands.iter().any(|command|
            matches!(command,egui::ViewportCommand::Fullscreen(value) if *value==fullscreen))
        })
    }
    #[test]
    fn zen_dispatch_combines_fullscreen_and_distraction_free_then_restores_exact_view() {
        for initial_fullscreen in [false, true] {
            let mut app = app();
            let ctx = egui::Context::default();
            let original = serde_json::to_value(app.tool_window_layout()).unwrap();
            let preferences = serde_json::to_value(&app.settings).unwrap();
            let document = app.tabs[0].state.doc.text();
            let version = app.tabs[0].state.doc.version();
            let entered = ctx.run_ui(input(initial_fullscreen), |ui| {
                app.handle_action(Action::ToggleZenMode, ui.ctx())
            });
            assert!(commanded(&entered, true));
            entered.drop_without_applying_deltas();
            assert_eq!(app.layout.view_mode, layout::ViewMode::Zen);
            assert!(!app.shows_chrome());
            assert!(!app.shows_menu_bar());
            assert!(!app.layout.left_visible);
            assert!(app.bottom.is_none());
            assert_eq!(
                app.zoom, 2.0,
                "Zen does not apply Presentation font enlargement"
            );
            assert_eq!(
                serde_json::to_value(app.tool_window_layout()).unwrap(),
                original,
                "saving during Zen retains the pre-Zen layout"
            );
            let exited = ctx.run_ui(input(true), |ui| {
                app.handle_action(Action::ExitZenMode, ui.ctx())
            });
            assert!(commanded(&exited, initial_fullscreen));
            exited.drop_without_applying_deltas();
            assert_eq!(app.layout.view_mode, layout::ViewMode::Normal);
            assert_eq!(
                serde_json::to_value(app.tool_window_layout()).unwrap(),
                original
            );
            assert_eq!(serde_json::to_value(&app.settings).unwrap(), preferences);
            assert_eq!(app.tabs[0].state.doc.text(), document);
            assert_eq!(app.tabs[0].state.doc.version(), version);
        }
    }
    #[test]
    fn zen_restores_presentation_and_its_original_fullscreen_stash() {
        let mut app = app();
        let ctx = egui::Context::default();
        ctx.run_ui(input(false), |ui| {
            app.handle_action(Action::TogglePresentationMode, ui.ctx())
        })
        .drop_without_applying_deltas();
        let presentation_zoom = app.zoom;
        ctx.run_ui(input(true), |ui| {
            app.handle_action(Action::ToggleZenMode, ui.ctx())
        })
        .drop_without_applying_deltas();
        ctx.run_ui(input(true), |ui| {
            app.handle_action(Action::ExitZenMode, ui.ctx())
        })
        .drop_without_applying_deltas();
        assert_eq!(app.layout.view_mode, layout::ViewMode::Presentation);
        assert_eq!(app.zoom, presentation_zoom);
        let exit = ctx.run_ui(input(true), |ui| app.exit_view_mode(ui.ctx()));
        assert!(commanded(&exit, false));
        exit.drop_without_applying_deltas();
        assert_eq!(app.zoom, 2.0);
        assert_eq!(app.bottom, Some(BottomTool::Run));
    }
    #[test]
    fn switching_from_zen_to_presentation_finishes_with_fullscreen_enabled() {
        let mut app = app();
        let ctx = egui::Context::default();
        ctx.run_ui(input(false), |ui| {
            app.handle_action(Action::ToggleZenMode, ui.ctx())
        })
        .drop_without_applying_deltas();
        let output = ctx.run_ui(input(true), |ui| {
            app.handle_action(Action::TogglePresentationMode, ui.ctx())
        });
        let fullscreen = output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .filter_map(|command| {
                if let egui::ViewportCommand::Fullscreen(value) = command {
                    Some(*value)
                } else {
                    None
                }
            })
            .next_back();
        assert_eq!(fullscreen, Some(true));
        output.drop_without_applying_deltas();
        assert_eq!(app.layout.view_mode, layout::ViewMode::Presentation);
        let exit = ctx.run_ui(input(true), |ui| app.exit_view_mode(ui.ctx()));
        assert!(commanded(&exit, false));
        exit.drop_without_applying_deltas();
        assert_eq!(app.layout.view_mode, layout::ViewMode::Normal);
    }

    #[test]
    fn explicit_exit_is_rendered_even_with_every_chrome_preference_disabled() {
        let mut app = app();
        let ctx = egui::Context::default();
        app.settings.chrome.show_main_menu = false;
        app.settings.appearance.show_main_toolbar = false;
        app.settings.appearance.show_status_bar = false;
        ctx.run_ui(input(false), |ui| {
            app.handle_action(Action::ToggleZenMode, ui.ctx());
            app.zen_mode_frame(ui.ctx());
        })
        .drop_without_applying_deltas();
        let output = ctx.run_ui(input(true), |ui| app.zen_mode_frame(ui.ctx()));
        assert!(output.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text()=="Exit Zen Mode")));
        output.drop_without_applying_deltas();
    }
    #[test]
    fn applying_another_project_layout_discards_old_zen_visibility_but_restores_native_state() {
        let mut app = app();
        let ctx = egui::Context::default();
        ctx.run_ui(input(false), |ui| {
            app.handle_action(Action::ToggleZenMode, ui.ctx())
        })
        .drop_without_applying_deltas();
        let target = rustpy_settings::ToolWindowLayout {
            project_visible: false,
            bottom_visible: false,
            ..Default::default()
        };
        app.restore_tool_window_layout(&target);
        let output = ctx.run_ui(input(true), |ui| app.zen_mode_frame(ui.ctx()));
        assert!(commanded(&output, false));
        output.drop_without_applying_deltas();
        assert!(app.layout.zen.is_none());
        assert_eq!(app.layout.view_mode, layout::ViewMode::Normal);
        assert!(!app.layout.left_visible);
        assert!(app.bottom.is_none());
    }
}
