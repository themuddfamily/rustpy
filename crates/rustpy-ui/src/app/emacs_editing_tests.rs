//! Actual global dispatcher + real editor rendering with the bundled Emacs preset.
use super::*;
use crate::editor::view::{self, ViewConfig};

struct Fixture {
    app: IdeApp,
    ctx: egui::Context,
    field_visible: bool,
    field: String,
    time: f64,
}
impl Fixture {
    fn new() -> Self {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text(
            "alpha beta\ngamma delta\n",
            Language::PlainText,
        ));
        let preset: serde_json::Value = serde_json::from_str(include_str!(
            "../../../rustpy-plugins/examples/emacs-keymap/keymaps/emacs.json"
        ))
        .unwrap();
        let bindings: std::collections::BTreeMap<String, Vec<String>> =
            serde_json::from_value(preset["bindings"].clone()).unwrap();
        let (keymap, problems) = Keymap::from_bindings(
            "Emacs",
            bindings
                .iter()
                .map(|(id, shortcuts)| (id.as_str(), shortcuts.as_slice())),
        );
        assert!(problems.is_empty(), "{problems:?}");
        app.keymap = keymap;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut fixture = Self {
            app,
            ctx,
            field_visible: false,
            field: String::new(),
            time: 0.0,
        };
        fixture.frame(0.0, vec![]);
        fixture.frame(0.01, vec![]);
        assert!(fixture.app.tabs[0].view.focused);
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
                    if self.field_visible {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.field)
                                .id(egui::Id::new("emacs-owned-field")),
                        );
                    }
                    self.app.global_shortcuts(ui);
                    self.app.navigation_bar(ui);
                    self.app.dialogs(ui.ctx());
                    egui::CentralPanel::default().show(ui, |ui| {
                        if self.app.split.is_some() {
                            self.app.editor_area(ui);
                            return;
                        }
                        let Some(index) = self.app.active else {
                            return;
                        };
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
                        let tab = &mut self.app.tabs[index];
                        let actions = view::show(ui, &mut tab.state, &mut tab.view, &cfg);
                        for action in actions {
                            if let view::ViewEvent::Action(action) = action {
                                self.app.handle_action(action, ui.ctx());
                            }
                        }
                        if self.app.emacs_frame_interrupted {
                            self.app.emacs_editing.interrupt();
                        }
                    });
                },
            )
            .drop_without_applying_deltas();
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        self.time += 0.1;
        self.frame(
            self.time,
            vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            }],
        );
        self.time += 0.01;
        self.frame(
            self.time,
            vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
    }
    fn text(&self) -> String {
        self.app.tabs[0].state.doc.text()
    }
    fn prepare_two_entries_and_yank(&mut self) {
        self.key(egui::Key::K, egui::Modifiers::CTRL);
        assert_eq!(self.text(), "\ngamma delta\n");
        self.key(egui::Key::ArrowDown, egui::Modifiers::NONE);
        self.key(egui::Key::K, egui::Modifiers::CTRL);
        assert_eq!(self.app.emacs_editing.ring_len(), 2);
        assert_eq!(self.text(), "\n\n");
        self.key(egui::Key::Y, egui::Modifiers::CTRL);
        assert_eq!(self.text(), "\ngamma delta\n");
    }
}

#[test]
fn bundled_ctrl_k_ctrl_y_alt_y_dispatch_shared_ring_not_system_paste_or_history_dialog() {
    let mut fixture = Fixture::new();
    fixture.app.macro_recording = Some(Vec::new());
    fixture.prepare_two_entries_and_yank();
    fixture.key(egui::Key::Y, egui::Modifiers::ALT);
    assert_eq!(fixture.text(), "\nalpha beta\n");
    assert!(matches!(fixture.app.dialog, Dialog::None));
    let steps = fixture.app.macro_recording.as_ref().unwrap();
    for id in [
        "EditorEmacsKillLine",
        "EditorEmacsYank",
        "EditorEmacsYankPop",
    ] {
        assert!(steps.contains(&format!("action:{id}")));
    }
}

#[test]
fn movement_away_and_back_breaks_yank_pop_even_with_equal_final_carets() {
    let mut fixture = Fixture::new();
    fixture.prepare_two_entries_and_yank();
    let text = fixture.text();
    let carets = fixture.app.tabs[0].state.selections();
    fixture.key(egui::Key::ArrowLeft, egui::Modifiers::NONE);
    fixture.key(egui::Key::ArrowRight, egui::Modifiers::NONE);
    assert_eq!(fixture.app.tabs[0].state.selections(), carets);
    fixture.key(egui::Key::Y, egui::Modifiers::ALT);
    assert_eq!(fixture.text(), text);
}

#[test]
fn owned_text_field_modal_and_no_active_editor_refuse_emacs_chords() {
    let mut fixture = Fixture::new();
    fixture.prepare_two_entries_and_yank();
    let before = fixture.text();
    fixture.field_visible = true;
    fixture
        .ctx
        .memory_mut(|memory| memory.request_focus(egui::Id::new("emacs-owned-field")));
    fixture.frame(1.0, vec![]);
    fixture.key(egui::Key::K, egui::Modifiers::CTRL);
    fixture.key(egui::Key::Y, egui::Modifiers::CTRL);
    assert_eq!(fixture.text(), before);
    fixture.field_visible = false;
    fixture.app.dialog = Dialog::GoToLine(String::new());
    fixture.key(egui::Key::K, egui::Modifiers::CTRL);
    fixture.key(egui::Key::Y, egui::Modifiers::CTRL);
    assert_eq!(fixture.text(), before);
    fixture.app.dialog = Dialog::None;
    fixture.app.active = None;
    fixture.key(egui::Key::Y, egui::Modifiers::CTRL);
    assert_eq!(fixture.text(), before);
}

#[test]
fn real_split_groups_preserve_active_owner_kill_chain_and_yank_pop() {
    let mut fixture = Fixture::new();
    fixture.app.push_tab(EditorState::from_text(
        "passive pane\n",
        Language::PlainText,
    ));
    fixture.app.tabs[1].group = 1;
    fixture.app.active = Some(0);
    fixture.app.split = Some(SplitDir::Horizontal);
    fixture.app.group_active = [
        Some(fixture.app.tabs[0].view.id),
        Some(fixture.app.tabs[1].view.id),
    ];
    fixture.app.focus_editor = true;
    fixture.time = 1.0;
    fixture.frame(1.0, vec![]);
    fixture.frame(1.01, vec![]);
    assert_eq!(fixture.app.group_active_tab(0), Some(0));
    assert_eq!(fixture.app.group_active_tab(1), Some(1));
    assert!(fixture.app.tabs[0].view.focused);
    assert!(!fixture.app.tabs[1].view.focused);
    fixture.key(egui::Key::K, egui::Modifiers::CTRL);
    fixture.key(egui::Key::K, egui::Modifiers::CTRL);
    assert_eq!(fixture.text(), "gamma delta\n");
    assert_eq!(fixture.app.emacs_editing.ring_len(), 1);
    assert_eq!(
        fixture.app.emacs_editing.latest_text().as_deref(),
        Some("alpha beta\n")
    );
    fixture.key(egui::Key::Home, egui::Modifiers::NONE);
    fixture.key(egui::Key::K, egui::Modifiers::CTRL);
    assert_eq!(fixture.app.emacs_editing.ring_len(), 2);
    assert_eq!(fixture.text(), "\n");
    fixture.key(egui::Key::Y, egui::Modifiers::CTRL);
    assert_eq!(fixture.text(), "gamma delta\n");
    fixture.key(egui::Key::Y, egui::Modifiers::ALT);
    assert_eq!(fixture.text(), "alpha beta\n\n");
    assert_eq!(fixture.app.active, Some(0));
    assert!(fixture.app.tabs[0].view.focused);
    assert!(!fixture.app.tabs[1].view.focused);
    assert_eq!(fixture.app.tabs[1].state.doc.text(), "passive pane\n");
}
