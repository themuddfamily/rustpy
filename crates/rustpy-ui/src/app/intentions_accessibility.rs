//! Names and selection announcements for the existing context-actions popup.
use super::*;

#[derive(Default)]
pub(super) struct IntentionsAccessibility {
    pub area: Option<egui::Id>,
}

pub(super) fn row(response: &egui::Response, title: &str, selected: bool) {
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, title)
    });
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.clear_toggled();
        node.set_selected(selected);
    });
}

pub(super) fn announce(ctx: &egui::Context, owner: egui::Id, fixes: &[QuickFix], selected: usize) {
    let current = fixes
        .get(selected)
        .map(|fix| format!("Action {} of {}: {}", selected + 1, fixes.len(), fix.title))
        .unwrap_or_else(|| "No available actions".into());
    crate::a11y::announce(
        ctx,
        owner,
        &format!(
            "Context actions. {current}. Up and Down to select; Enter to apply; Escape to close"
        ),
    );
}

impl IdeApp {
    // The manual Area has no text field of its own: the editor retains focus.
    // A competing owner must get its key before the stale intentions handler.
    pub(super) fn intentions_accessibility_owner(&self, ctx: &egui::Context) -> bool {
        let focus_owned = ctx.memory(|memory| memory.focused()).is_none_or(|focus| {
            self.active
                .and_then(|i| self.tabs.get(i))
                .is_some_and(|tab| focus == tab.view.id)
                || ctx.read_response(focus).is_some_and(|response| {
                    Some(response.layer_id.id) == self.intentions_accessibility.area
                })
        });
        focus_owned
            && self.editor_accessibility.owner.is_none()
            && matches!(self.dialog, Dialog::None)
            && self.choice.is_none()
            && !self.everywhere.visible
            && !self.find.visible
            && !self.find_in_files.visible
            && !self.nav.popup_visible()
            && !self.git_panel.branch_popup
            && self.inplace_rename.is_none()
            && !self.completion.visible
            && self.import_completion.is_none()
            && !self.intention_menu.dialog_active()
            && !self.emmet_ui.active()
            && !self.injection_ui.active()
            && !self.generate_methods.active()
            && self.layout.exit_prompt.is_none()
            && !self.coordinated_quit_active()
            && !egui::Popup::is_any_open(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Caret;

    fn frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> egui::accesskit::TreeUpdate {
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 720.0))),
                ..Default::default()
            },
            |ui| {
                app.global_shortcuts(ui);
                egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
                app.dialogs(ui.ctx());
            },
        );
        let update = output.platform_output.accesskit_update.clone().unwrap();
        output.drop_without_applying_deltas();
        update
    }
    fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers,
            },
        ]
    }
    fn fixture() -> (tempfile::TempDir, IdeApp, egui::Context) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("π.py");
        std::fs::write(&path, "import os\nπ = 1\n").unwrap();
        let mut settings = Settings::default();
        settings.completion.show_as_you_type = false;
        settings.inspections.spelling = false;
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        app.open_file(&path);
        app.sync_open_tabs_now();
        let inputs = app.analysis_inputs();
        let (db, file) = app.analysis_file(0).unwrap();
        let result = analysis_jobs::analyze_file(
            &db,
            &file,
            &inputs,
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
        app.tabs[0].state.extra_diagnostics = result.diagnostics;
        app.tabs[0].state.quick_fixes = result.quick_fixes;
        app.tabs[0].state.semantic_version = Some(app.tabs[0].state.doc.version());
        app.tabs[0]
            .state
            .set_carets(vec![Caret::at(TextSize::new(7))]);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        frame(&mut app, &ctx, 0.0, vec![]);
        frame(&mut app, &ctx, 0.1, vec![]);
        frame(
            &mut app,
            &ctx,
            0.2,
            key(egui::Key::Enter, egui::Modifiers::ALT),
        );
        assert!(
            app.intentions
                .as_ref()
                .is_some_and(|(fixes, _)| fixes.len() > 1),
            "actual AltEnter builds current analysis actions"
        );
        frame(&mut app, &ctx, 0.3, vec![]);
        (root, app, ctx)
    }
    #[test]
    fn actual_context_actions_key_selection_publishes_title_without_editing() {
        let (_root, mut app, ctx) = fixture();
        let text = app.tabs[0].state.doc.text();
        let version = app.tabs[0].state.doc.version();
        let update = frame(
            &mut app,
            &ctx,
            0.4,
            key(egui::Key::ArrowDown, egui::Modifiers::NONE),
        );
        let (fixes, selected) = app.intentions.as_ref().unwrap();
        assert_eq!(*selected, 1);
        let title = &fixes[*selected].title;
        let (id, status) = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Status
                    && node
                        .label()
                        .is_some_and(|s| s.starts_with("Context actions."))
            })
            .expect("real context-action announcement");
        assert!(status.label().unwrap().contains(title));
        assert!(status.label().unwrap().contains("Action 2 of"));
        assert!(update
            .nodes
            .iter()
            .any(|(_, node)| node.children().contains(id)));
        assert!(update
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some(title.as_str())
                && node.is_selected() == Some(true)));
        frame(
            &mut app,
            &ctx,
            0.5,
            key(egui::Key::Escape, egui::Modifiers::NONE),
        );
        assert!(app.intentions.is_none());
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert!(!app.tabs[0].state.doc.can_undo());
    }
    #[test]
    fn stale_context_actions_leave_escape_to_the_actual_find_owner() {
        let (_root, mut app, ctx) = fixture();
        let text = app.tabs[0].state.doc.text();
        app.handle_action(Action::Find, &ctx);
        assert!(app.find.visible);
        assert!(
            app.intentions.is_some(),
            "actual competing-owner transition"
        );
        frame(&mut app, &ctx, 0.4, vec![]);
        let update = frame(
            &mut app,
            &ctx,
            0.5,
            key(egui::Key::Escape, egui::Modifiers::NONE),
        );
        assert!(app.intentions.is_none());
        assert!(!app.find.visible, "actual Find handler receives its Escape");
        assert!(!update
            .nodes
            .iter()
            .any(|(_, node)| node.role() == egui::accesskit::Role::Status
                && node
                    .label()
                    .is_some_and(|s| s.starts_with("Context actions."))));
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert!(!app.tabs[0].state.doc.can_undo());
    }
}
