//! Actual dialog ownership must break a Double Shift candidate even when
//! no intervening key, text or pointer event independently invalidates it.

use super::*;
use crate::editor::view::{self, ViewConfig};

const ORIGINAL: &str = "answer = 41\n";

struct Fixture {
    app: IdeApp,
    ctx: egui::Context,
}

impl Fixture {
    fn new() -> Self {
        // An editor without open_project: no watchers, interpreter discovery,
        // settings writes, external programs or filesystem fixture are needed.
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text(ORIGINAL, Language::Python));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut fixture = Self { app, ctx };
        fixture.frame(0.0, vec![]);
        assert!(
            fixture.app.tabs[fixture.app.active.unwrap()].view.focused,
            "fixture must begin with the actual editor focused"
        );
        fixture
    }

    fn frame(&mut self, time: f64, events: Vec<egui::Event>) {
        self.ctx
            .run_ui(
                egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 720.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    // Keep the real ordering: shortcuts can take keyboard ownership
                    // before the actual editor decides whether to process its input.
                    // Match App::ui: coordinated consent owns the entire frame
                    // and returns before the ordinary shortcut dispatcher.
                    if self.app.coordinated_quit_active() {
                        self.app.coordinated_quit_guard_ui(ui.ctx());
                        return;
                    }
                    self.app.global_shortcuts(ui);
                    self.app.navigation_bar(ui);
                    self.app.dialogs(ui.ctx());
                    egui::CentralPanel::default().show(ui, |ui| {
                        let keyboard = self.app.editor_keyboard();
                        let request_focus = std::mem::take(&mut self.app.focus_editor);
                        let opts = EditOptions::default();
                        let cfg = ViewConfig {
                            scheme: &self.app.scheme,
                            theme: &self.app.theme,
                            settings: &self.app.settings.editor,
                            opts: &opts,
                            keymap: &self.app.keymap,
                            font_size: 14.0,
                            search_highlights: &[],
                            request_focus,
                            run_lines: &[],
                            line_changes: &[],
                            breakpoints: &[],
                            exec_line: None,
                            inline_values: &[],
                            bookmarks: &[],
                            gutter_markers: &[],
                            gutter_quick_fixes: &[],
                            code_vision: None,
                            annotations: None,
                            coverage: &[],
                            keyboard,
                        };
                        let tab = &mut self.app.tabs[self.app.active.unwrap()];
                        let actions = view::show(ui, &mut tab.state, &mut tab.view, &cfg);
                        for action in actions {
                            if let view::ViewEvent::Action(action) = action {
                                self.app.handle_action(action, ui.ctx());
                            }
                        }
                    });
                },
            )
            .drop_without_applying_deltas();
    }

    fn shift_down(&mut self, time: f64, key: egui::Key) {
        self.frame(
            time,
            vec![
                egui::Event::ModifiersChanged(egui::Modifiers::SHIFT),
                key_event(key, true, egui::Modifiers::SHIFT),
            ],
        );
    }

    fn shift_up(&mut self, time: f64, key: egui::Key) {
        self.frame(
            time,
            vec![
                key_event(key, false, egui::Modifiers::NONE),
                egui::Event::ModifiersChanged(egui::Modifiers::NONE),
            ],
        );
    }

    fn tap_shift(&mut self, time: f64, key: egui::Key) {
        self.shift_down(time, key);
        self.shift_up(time + 0.0005, key);
    }

    fn text(&self) -> String {
        self.app.tabs[self.app.active.unwrap()].state.doc.text()
    }

    fn assert_search_closed(&self) {
        assert!(
            !self.app.everywhere.visible,
            "ordinary intervening input must not be recognized as clean Double Shift"
        );
    }
}

fn key_event(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers,
    }
}

#[test]
fn rendered_about_owns_second_clean_shift_tap() {
    let mut fixture = Fixture::new();
    fixture.tap_shift(1.0, egui::Key::ShiftLeft);
    fixture.assert_search_closed();
    fixture.app.handle_action(Action::About, &fixture.ctx);
    fixture.frame(1.10, vec![]);
    assert!(matches!(fixture.app.dialog, Dialog::About));

    // No dismissal click or Escape precedes the decisive assertion: either
    // would independently invalidate the candidate and mask a broken guard.
    fixture.tap_shift(1.20, egui::Key::ShiftLeft);
    assert!(matches!(fixture.app.dialog, Dialog::About));
    fixture.assert_search_closed();
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn rendered_quit_consent_then_cancel_breaks_double_shift_candidate() {
    use super::window_lifecycle::QuitProgress;

    let mut fixture = Fixture::new();
    fixture.app.settings.confirm_exit = true;
    fixture.tap_shift(1.0, egui::Key::ShiftLeft);
    fixture.assert_search_closed();
    let transaction = "double-shift-consent-ownership";
    assert!(matches!(
        fixture.app.prepare_coordinated_quit(transaction),
        QuitProgress::PendingProcesses { .. }
    ));
    fixture.frame(1.10, vec![]);
    assert!(fixture.app.coordinated_quit_active());
    assert!(matches!(
        fixture.app.poll_coordinated_quit(transaction),
        QuitProgress::PendingProcesses { .. }
    ));

    // Cancel belongs to the actual guard-only frame: Escape never reaches
    // global_shortcuts, so it cannot independently invalidate the candidate.
    fixture.frame(
        1.15,
        vec![key_event(egui::Key::Escape, true, egui::Modifiers::NONE)],
    );
    assert!(!fixture.app.coordinated_quit_active());
    assert!(matches!(fixture.app.dialog, Dialog::None));
    fixture.assert_search_closed();

    // Both presses remain within the unchanged strict 350ms interval. Entering
    // the guard must have reset the first candidate before cancellation returned
    // ownership to the editor; otherwise this second completed tap opens Search.
    fixture.tap_shift(1.20, egui::Key::ShiftLeft);
    fixture.assert_search_closed();
    assert!(!fixture.app.coordinated_quit_active());
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn rendered_navigation_ownership_then_escape_breaks_double_shift_candidate() {
    for intervening_text in [false, true] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.py"), ORIGINAL).unwrap();
        let mut fixture = Fixture::new();
        fixture.app.project = Some(Project {
            root: root.path().to_path_buf(),
            tree: ProjectTree::new(root.path().to_path_buf()),
            analysis: Arc::new(Analysis::for_project(root.path())),
            run_configs: RunConfigStore::default(),
            interpreters: Vec::new(),
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        fixture.app.tabs[fixture.app.active.unwrap()].state.path =
            Some(root.path().join("main.py"));
        fixture.frame(0.5, vec![]);
        fixture.tap_shift(1.0, egui::Key::ShiftLeft);
        fixture.assert_search_closed();

        // Dispatch the registered action directly: an Alt+Home key event would
        // independently invalidate the first tap before ownership changes and
        // mask the navigation early-return regression this fixture targets.
        fixture
            .app
            .handle_action(Action::JumpToNavigationBar, &fixture.ctx);
        fixture.frame(1.10, vec![]);
        assert!(
            !fixture.app.tabs[fixture.app.active.unwrap()].view.focused,
            "the rendered navigator must actually take focus from the editor"
        );
        if intervening_text {
            fixture.frame(1.12, vec![egui::Event::Text("main".into())]);
            fixture.assert_search_closed();
            assert_eq!(fixture.text(), ORIGINAL);
        }
        // Escape is consumed by the navigator before gesture.update sees it.
        fixture.frame(
            1.15,
            vec![key_event(egui::Key::Escape, true, egui::Modifiers::NONE)],
        );
        fixture.assert_search_closed();
        assert!(
            fixture.app.tabs[fixture.app.active.unwrap()].view.focused,
            "Escape must return actual keyboard focus to the editor"
        );
        fixture.tap_shift(1.20, egui::Key::ShiftLeft);
        fixture.assert_search_closed();
        assert_eq!(fixture.text(), ORIGINAL);
        assert_eq!(
            std::fs::read_to_string(root.path().join("main.py")).unwrap(),
            ORIGINAL
        );
    }
}
