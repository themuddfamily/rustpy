//! Refactoring Undo confirmation captures the document and exact history state.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_text::{DocumentId, HistoryId};

pub(super) struct UndoRequest {
    document: DocumentId,
    history: HistoryId,
    version: u64,
    label: String,
    file: String,
    do_not_ask: bool,
}

impl IdeApp {
    pub(super) fn collect_refactoring_undo_request(&mut self) {
        if !matches!(self.dialog, Dialog::None)
            || self.bg.problems.infrastructure.dialog
            || self.intention_menu.dialog_active()
            || self.injection_ui.active()
        {
            return;
        }
        let mut collected = None;
        for tab in &mut self.tabs {
            let Some((history, version)) = tab.state.pending_refactoring_undo.take() else {
                continue;
            };
            let document = &tab.state.doc;
            if document.current_history() != history
                || document.version() != version
                || !document.undo_is_refactoring()
                || document.is_read_only()
            {
                continue;
            }
            collected = Some(UndoRequest {
                document: document.identity(),
                history,
                version,
                label: document
                    .undo_label()
                    .unwrap_or_else(|| "Refactoring".into()),
                file: tab.state.title(),
                do_not_ask: false,
            });
            break;
        }
        if let Some(request) = collected {
            if self.settings.refactoring.confirm_undo {
                self.dialog = Dialog::ConfirmRefactoringUndo(request);
            } else {
                self.apply_refactoring_undo(&request);
            }
        }
    }

    fn apply_refactoring_undo(&mut self, request: &UndoRequest) -> bool {
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.state.doc.identity() == request.document)
        else {
            return false;
        };
        self.refresh_file_permissions(index);
        let state = &mut self.tabs[index].state;
        if state.doc.current_history() != request.history
            || state.doc.version() != request.version
            || !state.doc.undo_is_refactoring()
            || state.doc.is_read_only()
        {
            return false;
        }
        if let Some(undone) = self.undo_refactoring_group(request.document, request.history) {
            return undone;
        }
        let state = &mut self.tabs[index].state;
        let undone = state.undo();
        if undone {
            self.active = Some(index);
            self.focus_editor = true;
        }
        undone
    }

    pub(super) fn refactoring_undo_dialog(
        &mut self,
        ctx: &egui::Context,
        mut request: UndoRequest,
    ) {
        let mut open = true;
        let mut undo = false;
        let mut cancel = false;
        egui::Window::new("Undo Refactoring")
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label(format!("Undo '{}' in {}?", request.label, request.file));
                ui.checkbox(&mut request.do_not_ask, "Do not ask again");
                ui.horizontal(|ui| {
                    undo = ui.button("Undo").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if undo {
            if self.apply_refactoring_undo(&request) {
                if request.do_not_ask {
                    self.settings.refactoring.confirm_undo = false;
                    if let Err(error) = self.settings.save() {
                        self.set_status(format!("Cannot save refactoring preference: {error}"));
                    }
                }
            } else {
                self.set_status("The document changed; invoke Undo again for its current state");
            }
        } else if open && !cancel && !ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.dialog = Dialog::ConfirmRefactoringUndo(request);
        } else {
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_text::{Document, Edit, EditGroup};

    fn fixture() -> IdeApp {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("old = 1\n", Language::Python));
        app.apply_refactoring(Ok(Refactoring {
            title: "Rename".into(),
            edits: vec![(
                TextRange::new(TextSize::new(0), TextSize::new(3)),
                "new".into(),
            )],
            rename_ranges: Vec::new(),
            name: "new".into(),
        }));
        assert!(app.tabs[0].state.doc.undo_is_refactoring());
        app
    }

    fn request(app: &mut IdeApp) -> UndoRequest {
        let options = app.edit_options_for(0);
        assert!(crate::editor::view::handle_editor_action(
            Action::Undo,
            &mut app.tabs[0].state,
            &options
        ));
        assert_eq!(app.tabs[0].state.doc.text(), "new = 1\n");
        app.collect_refactoring_undo_request();
        let Dialog::ConfirmRefactoringUndo(request) =
            std::mem::replace(&mut app.dialog, Dialog::None)
        else {
            panic!("refactoring confirmation")
        };
        request
    }

    #[test]
    fn actual_refactoring_undo_prompts_then_preserves_redo_and_shared_document_tag() {
        let mut app = fixture();
        let shared = app.tabs[0].state.doc.share();
        let request = request(&mut app);
        assert!(app.apply_refactoring_undo(&request));
        assert_eq!(app.tabs[0].state.doc.text(), "old = 1\n");
        assert!(!shared.undo_is_refactoring());
        assert!(app.tabs[0].state.redo());
        assert!(shared.undo_is_refactoring());
        assert_eq!(shared.text(), "new = 1\n");
    }

    #[test]
    fn cancelled_or_stale_confirmation_cannot_undo_later_typing() {
        let mut app = fixture();
        let cancelled = request(&mut app);
        assert_eq!(app.tabs[0].state.doc.text(), "new = 1\n");
        let carets = app.tabs[0].state.carets.clone();
        assert!(app.tabs[0].state.apply(
            vec![Edit::insert(TextSize::new(0), "# later\n")],
            carets,
            EditGroup::Separate,
            Some("Typing")
        ));
        let changed = app.tabs[0].state.doc.text();
        assert!(!app.apply_refactoring_undo(&cancelled));
        assert_eq!(app.tabs[0].state.doc.text(), changed);
        assert!(!app.tabs[0].state.doc.undo_is_refactoring());
    }

    #[test]
    fn preference_disables_prompt_and_restores_from_old_settings() {
        let mut app = fixture();
        app.settings.refactoring.confirm_undo = false;
        let options = app.edit_options_for(0);
        crate::editor::view::handle_editor_action(Action::Undo, &mut app.tabs[0].state, &options);
        assert_eq!(app.tabs[0].state.doc.text(), "old = 1\n");
        assert!(app.tabs[0].state.pending_refactoring_undo.is_none());
        let old: rustpy_settings::RefactoringSettings = serde_json::from_str("{}").unwrap();
        assert!(
            old.confirm_undo
                && old.prefer_from_imports
                && old.rename_in_place
                && old.search_in_comments
        );
        let loaded: Settings =
            serde_json::from_str(&serde_json::to_string(&app.settings).unwrap()).unwrap();
        assert!(!loaded.refactoring.confirm_undo);
    }

    #[test]
    fn tagging_refactoring_closes_typing_merge_and_reset_discards_the_tag() {
        let mut document = Document::new("a");
        document
            .edit(
                vec![Edit::insert(TextSize::new(1), "b")],
                &[],
                &[],
                EditGroup::Open,
                Some("Refactor"),
            )
            .unwrap();
        document.mark_refactoring();
        document
            .edit(
                vec![Edit::insert(TextSize::new(2), "c")],
                &[],
                &[],
                EditGroup::Merge,
                Some("Typing"),
            )
            .unwrap();
        assert!(!document.undo_is_refactoring());
        document.undo().unwrap();
        assert_eq!(document.text(), "ab");
        assert!(document.undo_is_refactoring());
        document.reset("replacement");
        assert!(!document.undo_is_refactoring());
    }
}
