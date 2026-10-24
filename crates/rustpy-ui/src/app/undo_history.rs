//! Retained editor undo branches and selection restore.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_text::HistoryId;

pub(super) struct UndoHistoryState {
    tab: usize,
    selected: HistoryId,
}

impl IdeApp {
    pub(super) fn open_undo_history(&mut self) {
        let Some(tab) = self.active.filter(|&i| i < self.tabs.len()) else {
            return;
        };
        let doc = &self.tabs[tab].state.doc;
        if doc.is_read_only() || doc.transaction_depth() > 0 {
            self.set_status("Undo history needs a writable editor with no unfinished command");
            return;
        }
        self.dialog = Dialog::UndoHistory(UndoHistoryState {
            tab,
            selected: doc.current_history(),
        });
    }

    pub(super) fn undo_history_dialog(&mut self, ctx: &egui::Context, mut state: UndoHistoryState) {
        let Some(tab) = self.tabs.get(state.tab) else {
            return;
        };
        let entries = tab.state.doc.undo_history();
        if !entries.iter().any(|e| e.id == state.selected) {
            self.set_status("This undo state is no longer available; reopen Undo History");
            return;
        }
        let allowed = !tab.state.doc.is_read_only() && tab.state.doc.transaction_depth() == 0;
        let mut open = true;
        let mut restore = false;
        let mut cancel = false;
        egui::Window::new("Undo History")
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .default_size([520.0, 380.0])
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label("Choose a state to return to. Other branches stay available.");
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        for (number, entry) in entries.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.add_space(16.0 * entry.depth.min(20) as f32);
                                let label =
                                    entry.label.as_deref().unwrap_or(if entry.parent.is_none() {
                                        "Initial state"
                                    } else {
                                        "Edit"
                                    });
                                let current = if entry.is_current { " (current)" } else { "" };
                                if ui
                                    .selectable_label(
                                        state.selected == entry.id,
                                        format!("{number}. {label}{current}"),
                                    )
                                    .clicked()
                                {
                                    state.selected = entry.id;
                                }
                            });
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    restore = ui
                        .add_enabled(allowed, egui::Button::new("Restore"))
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if restore {
            match self.tabs[state.tab].state.select_history(state.selected) {
                Ok(()) => {
                    self.active = Some(state.tab);
                    self.focus_editor = true;
                    self.set_status("Undo state restored");
                }
                Err(error) => self.set_status(format!("Cannot restore undo state: {error}")),
            }
        } else if open && !cancel && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.dialog = Dialog::UndoHistory(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::state::LastOp;
    use rustpy_text::EditGroup;

    fn fixture() -> (tempfile::TempDir, IdeApp) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.py");
        std::fs::write(&path, "value=1\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        let mut app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        assert!(app.tabs.is_empty());
        app.open_file(&path);
        (dir, app)
    }

    #[test]
    fn picker_exposes_alternate_branches_and_restores_editor_state() {
        let (_dir, mut app) = fixture();
        let state = &mut app.tabs[0].state;
        state.apply_text_edits(
            vec![(
                TextRange::new(TextSize::new(6), TextSize::new(7)),
                "2".into(),
            )],
            "Second",
        );
        let second = state.doc.current_history();
        state.undo();
        state.apply_text_edits(
            vec![(
                TextRange::new(TextSize::new(6), TextSize::new(7)),
                "3".into(),
            )],
            "Third",
        );
        let third = state.doc.current_history();
        state.auto_closers.push(TextSize::new(0));
        state.last_op = LastOp::Typing;
        state.select_history(second).unwrap();
        assert_eq!(state.doc.text(), "value=2\n");
        assert!(state.auto_closers.is_empty());
        assert_eq!(state.last_op, LastOp::Other);
        state.select_history(third).unwrap();
        assert_eq!(state.doc.text(), "value=3\n");
        app.open_undo_history();
        assert!(matches!(app.dialog, Dialog::UndoHistory(_)));
        assert_eq!(app.tabs[0].state.doc.undo_history().len(), 3);
    }

    #[test]
    fn cleanup_nested_commands_form_one_undo_unit() {
        let (_dir, mut app) = fixture();
        let state = &mut app.tabs[0].state;
        state.apply(
            vec![rustpy_text::Edit::insert(TextSize::new(0), "import os\n")],
            state.carets.clone(),
            EditGroup::Separate,
            Some("Setup"),
        );
        let before = state.doc.text();
        let selections = state.selections();
        let count = state.doc.undo_history().len();
        app.code_cleanup();
        let state = &mut app.tabs[0].state;
        let after = state.doc.text();
        assert_ne!(after, before);
        assert!(!after.contains("import os"));
        assert!(after.contains("value = 1"));
        assert_eq!(state.doc.transaction_depth(), 0);
        assert_eq!(state.doc.undo_history().len(), count + 1);
        assert_eq!(state.doc.undo_label().as_deref(), Some("Code Cleanup"));
        assert!(state.undo());
        assert_eq!(state.doc.text(), before);
        assert_eq!(state.selections(), selections);
        assert!(state.redo());
        assert_eq!(state.doc.text(), after);
    }
}
