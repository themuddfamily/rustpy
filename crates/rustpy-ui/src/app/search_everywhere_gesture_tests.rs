//! Double Shift must be a clean gesture, not ordinary shifted typing. These
//! fixture-only tests exercise the real IDE shortcut handler and editor input.
//! Event times are explicit; there are no sleeps or shortened gesture windows.

use super::*;
use crate::editor::view::{self, ViewConfig};

const ORIGINAL: &str = "answer = 41\n";
const MARKER: &str = "# RUSTPY_NATIVE_DIRTY_CANCEL_PENDING_20261012";

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
                    self.app.global_shortcuts(ui);
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
        assert!(matches!(self.app.dialog, Dialog::None));
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
fn uppercase_marker_reaches_real_editor_without_opening_search_everywhere() {
    let mut fixture = Fixture::new();
    for (index, character) in MARKER.chars().enumerate() {
        let time = 1.0 + index as f64 * 0.04;
        let (key, shifted) = match character {
            '#' => (egui::Key::Num3, true),
            '_' => (egui::Key::Minus, true),
            ' ' => (egui::Key::Space, false),
            character => (
                egui::Key::from_name(&character.to_string()).unwrap(),
                character.is_ascii_uppercase(),
            ),
        };
        let modifiers = if shifted {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        };
        // Each uppercase character is an independent physical Shift chord,
        // with its letter key and Text event between Shift press and release.
        // Frame boundaries are deliberate; the old frame-wide `any` detector
        // must not hide the second press by collapsing a whole string into one.
        if shifted {
            fixture.shift_down(time, egui::Key::ShiftLeft);
        }
        fixture.frame(
            time + 0.004,
            vec![
                egui::Event::ModifiersChanged(modifiers),
                key_event(key, true, modifiers),
                egui::Event::Text(character.to_string()),
            ],
        );
        fixture.frame(time + 0.008, vec![key_event(key, false, modifiers)]);
        if shifted {
            fixture.shift_up(time + 0.012, egui::Key::ShiftLeft);
        }
    }
    fixture.assert_search_closed();
    assert_eq!(
        fixture.text(),
        format!("{MARKER}{ORIGINAL}"),
        "the complete literal uppercase marker must reach the real document"
    );
}

#[test]
fn clean_double_shift_within_original_350ms_window_opens_search() {
    for second in [egui::Key::ShiftLeft, egui::Key::ShiftRight] {
        let mut fixture = Fixture::new();
        fixture.tap_shift(1.0, egui::Key::ShiftLeft);
        fixture.assert_search_closed();
        fixture.tap_shift(1.349, second);
        assert!(
            fixture.app.everywhere.visible,
            "two clean Shift taps 349ms apart must open Search Everywhere"
        );
        assert_eq!(fixture.app.everywhere.query, "");
        assert_eq!(fixture.text(), ORIGINAL);
    }
}

#[test]
fn intervening_nonshift_key_invalidates_double_shift_candidate() {
    let mut fixture = Fixture::new();
    fixture.tap_shift(1.0, egui::Key::ShiftLeft);
    fixture.frame(
        1.10,
        vec![key_event(
            egui::Key::ArrowRight,
            true,
            egui::Modifiers::NONE,
        )],
    );
    fixture.frame(
        1.11,
        vec![key_event(
            egui::Key::ArrowRight,
            false,
            egui::Modifiers::NONE,
        )],
    );
    fixture.tap_shift(1.20, egui::Key::ShiftLeft);
    fixture.assert_search_closed();
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn intervening_text_invalidates_double_shift_candidate_and_is_inserted() {
    let mut fixture = Fixture::new();
    fixture.tap_shift(1.0, egui::Key::ShiftLeft);
    fixture.frame(1.10, vec![egui::Event::Text("x".into())]);
    assert_eq!(
        fixture.text(),
        format!("x{ORIGINAL}"),
        "real editor must consume intervening text"
    );
    fixture.tap_shift(1.20, egui::Key::ShiftLeft);
    fixture.assert_search_closed();
    assert_eq!(fixture.text(), format!("x{ORIGINAL}"));
}

#[test]
fn intervening_real_editor_pointer_click_invalidates_double_shift_candidate() {
    let mut fixture = Fixture::new();
    fixture.tap_shift(1.0, egui::Key::ShiftLeft);
    let pos = fixture.app.tabs[fixture.app.active.unwrap()]
        .view
        .caret_screen_rect
        .unwrap()
        .center();
    for (time, pressed) in [(1.10, true), (1.11, false)] {
        fixture.frame(
            time,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    fixture.tap_shift(1.20, egui::Key::ShiftLeft);
    fixture.assert_search_closed();
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn double_shift_at_or_after_original_350ms_deadline_stays_closed() {
    for second_time in [1.350, 1.351] {
        let mut fixture = Fixture::new();
        fixture.tap_shift(1.0, egui::Key::ShiftLeft);
        fixture.tap_shift(second_time, egui::Key::ShiftLeft);
        fixture.assert_search_closed();
        assert_eq!(fixture.text(), ORIGINAL);
    }
}
