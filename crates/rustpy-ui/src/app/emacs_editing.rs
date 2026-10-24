//! Application route for the shared Emacs kill ring; ordinary clipboard actions stay separate.
use super::*;
use crate::editor::emacs::Kill;

impl IdeApp {
    pub(super) fn is_emacs_action(action: Action) -> bool {
        matches!(
            action,
            Action::EmacsKillLine
                | Action::EmacsKillWordForward
                | Action::EmacsKillWordBackward
                | Action::EmacsKillRegion
                | Action::EmacsCopyRegion
                | Action::EmacsYank
                | Action::EmacsYankPop
                | Action::EmacsTranspose
        )
    }

    pub(super) fn emacs_keyboard_owned(&self) -> bool {
        self.editor_keyboard()
            && !self.find_in_files.visible
            && !self.nav.popup_visible()
            && !self.completion.visible
            && self.intentions.is_none()
            && !self.coordinated_quit_active()
            && !self.coordinated_quit_committed
    }

    /// Observe input before widgets consume it, and compare the actual active view.
    /// Release/modifier/motion events are not commands; all other unrelated input
    /// breaks command adjacency even if movement later returns to equal carets.
    pub(super) fn emacs_observe_frame(&mut self, ui: &egui::Ui) {
        let owner = self
            .active
            .and_then(|index| self.tabs.get(index))
            .map(|tab| (tab.state.doc.identity(), tab.view.id));
        let focused = owner.is_some_and(|(_, view)| ui.memory(|memory| memory.has_focus(view)));
        self.emacs_frame_interrupted = owner != self.emacs_owner
            || !focused
            || !self.emacs_keyboard_owned()
            || !ui.input(|input| input.focused)
            || ui.input(|input| {
                input.events.iter().any(|event| match event {
                    egui::Event::Key {
                        pressed: true,
                        key,
                        physical_key,
                        modifiers,
                        ..
                    } => !self
                        .keymap
                        .lookup_key(*key, *physical_key, *modifiers)
                        .is_some_and(Self::is_emacs_action),
                    egui::Event::Key { pressed: false, .. }
                    | egui::Event::ModifiersChanged(_)
                    | egui::Event::PointerMoved(_) => false,
                    egui::Event::Screenshot { .. } => false,
                    _ => true,
                })
            });
        if self.emacs_frame_interrupted {
            self.emacs_editing.interrupt();
        }
        self.emacs_owner = owner;
    }

    /// Called only after the action dispatcher has established active-editor ownership.
    /// Returns whether this is an Emacs action, including a refused/no-op command.
    pub(super) fn emacs_action(&mut self, action: Action, ctx: &egui::Context) -> bool {
        let command = match action {
            Action::EmacsKillLine => Some(Kill::LineForward),
            Action::EmacsKillWordForward => Some(Kill::WordForward),
            Action::EmacsKillWordBackward => Some(Kill::WordBackward),
            Action::EmacsKillRegion => Some(Kill::Region),
            Action::EmacsCopyRegion
            | Action::EmacsYank
            | Action::EmacsYankPop
            | Action::EmacsTranspose => None,
            _ => return false,
        };
        if !self.emacs_keyboard_owned() {
            self.emacs_editing.interrupt();
            return true;
        }
        let Some(index) = self.active.filter(|&index| index < self.tabs.len()) else {
            self.emacs_editing.interrupt();
            return true;
        };
        self.emacs_owner = Some((
            self.tabs[index].state.doc.identity(),
            self.tabs[index].view.id,
        ));
        if action != Action::EmacsCopyRegion {
            self.refresh_file_permissions(index);
        }
        let options = self.edit_options_for(index);
        let publish = if let Some(command) = command {
            self.emacs_editing
                .kill(&mut self.tabs[index].state, command, &options)
        } else {
            match action {
                Action::EmacsCopyRegion => self.emacs_editing.copy_region(&self.tabs[index].state),
                Action::EmacsYank => {
                    self.emacs_editing.yank(&mut self.tabs[index].state);
                    false
                }
                Action::EmacsYankPop => {
                    self.emacs_editing.yank_pop(&mut self.tabs[index].state);
                    false
                }
                Action::EmacsTranspose => {
                    self.emacs_editing.transpose(&mut self.tabs[index].state);
                    false
                }
                _ => unreachable!(),
            }
        };
        if publish {
            if let Some(text) = self.emacs_editing.latest_text() {
                ctx.copy_text(text.clone());
                self.remember_copied(text);
            }
        }
        self.focus_editor = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (tempfile::TempDir, IdeApp) {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        std::fs::write(&first, "alpha beta\n").unwrap();
        std::fs::write(&second, "target\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
        app.open_file(&first);
        app.open_file(&second);
        app.active = Some(0);
        (directory, app)
    }

    #[test]
    fn kills_publish_clipboard_and_yank_ring_crosses_documents_without_disk_edits() {
        let (directory, mut app) = app();
        let context = egui::Context::default();
        assert!(app.emacs_action(Action::EmacsKillLine, &context));
        assert_eq!(app.tabs[0].state.doc.text(), "\n");
        assert_eq!(
            app.clipboard_history.first().map(String::as_str),
            Some("alpha beta")
        );
        assert!(
            context.output_mut(|output| output.commands.iter().any(|command| {
                matches!(command, egui::OutputCommand::CopyText(text) if text == "alpha beta")
            }))
        );
        app.active = Some(1);
        assert!(app.emacs_action(Action::EmacsYank, &context));
        assert_eq!(app.tabs[1].state.doc.text(), "alpha betatarget\n");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("first.txt")).unwrap(),
            "alpha beta\n"
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("second.txt")).unwrap(),
            "target\n"
        );
    }

    #[test]
    fn modal_and_missing_active_owner_never_replay_a_yank() {
        let (_directory, mut app) = app();
        let context = egui::Context::default();
        app.emacs_action(Action::EmacsKillLine, &context);
        app.active = Some(1);
        let before = app.tabs[1].state.doc.text();
        app.dialog = Dialog::GoToLine(String::new());
        assert!(app.emacs_action(Action::EmacsYank, &context));
        assert_eq!(app.tabs[1].state.doc.text(), before);
        app.dialog = Dialog::None;
        app.active = None;
        assert!(app.emacs_action(Action::EmacsYank, &context));
        assert_eq!(app.tabs[1].state.doc.text(), before);
    }
}
