//! Real App keyboard owners and painted Find/Replace previews, without live edits.
use super::*;
use crate::editor::{view, EditorState, Language};

struct Frame(Vec<(String, egui::Pos2)>);
impl Frame {
    fn point(&self, text: &str) -> egui::Pos2 {
        self.0
            .iter()
            .rev()
            .find(|(label, _)| label == text)
            .map(|(_, point)| *point)
            .unwrap_or_else(|| panic!("Actual painted control missing: {text}"))
    }
    fn contains(&self, text: &str) -> bool {
        self.0.iter().any(|(label, _)| label.contains(text))
    }
}
fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) -> Frame {
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            if app.find.visible {
                app.find_bar(ui);
            }
            egui::CentralPanel::default().show(ui, |ui| {
                let options = app.edit_options();
                let request_focus = std::mem::take(&mut app.focus_editor);
                let cfg = view::ViewConfig {
                    scheme: &app.scheme,
                    theme: &app.theme,
                    settings: &app.settings.editor,
                    opts: &options,
                    keymap: &app.keymap,
                    font_size: 14.0,
                    search_highlights: &app.find.matches,
                    request_focus,
                    run_lines: &[],
                    line_changes: &[],
                    breakpoints: &[],
                    exec_line: None,
                    inline_values: &[],
                    bookmarks: &[],
                    annotations: None,
                    code_vision: None,
                    gutter_markers: &[],
                    gutter_quick_fixes: &[],
                    coverage: &[],
                    keyboard: app.editor_keyboard(),
                };
                let tab = &mut app.tabs[app.active.unwrap()];
                let events = view::show(ui, &mut tab.state, &mut tab.view, &cfg);
                for event in events {
                    if let view::ViewEvent::Action(action) = event {
                        app.handle_action(action, ui.ctx());
                    }
                }
            });
        },
    );
    let painted = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((
                text.galley.text().to_owned(),
                text.pos + text.galley.size() / 2.0,
            )),
            _ => None,
        })
        .collect();
    output.drop_without_applying_deltas();
    Frame(painted)
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
fn click(app: &mut IdeApp, ctx: &egui::Context, time: f64, point: egui::Pos2) -> Frame {
    frame(
        app,
        ctx,
        time,
        vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    frame(
        app,
        ctx,
        time + 0.01,
        vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    )
}
fn fixture(source: &str, language: Language) -> (IdeApp, egui::Context) {
    let mut app = IdeApp::new(Settings::default(), None);
    app.push_tab(EditorState::from_text(source, language));
    app.find.history = Some(FindHistory::default());
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    (app, ctx)
}
fn open_replace(app: &mut IdeApp, ctx: &egui::Context) -> Frame {
    let result = frame(app, ctx, 0.1, key(egui::Key::R, egui::Modifiers::CTRL));
    assert!(app.find.visible && app.find.replace_mode);
    result
}
fn query(app: &mut IdeApp, ctx: &egui::Context, text: &str) -> Frame {
    frame(app, ctx, 0.2, key(egui::Key::A, egui::Modifiers::CTRL));
    frame(app, ctx, 0.3, vec![egui::Event::Text(text.into())])
}

#[test]
fn multiline_query_enter_is_text_and_control_enter_navigates_without_editor_newline() {
    let source = "a\nb a\nb";
    let (mut app, ctx) = fixture(source, Language::PlainText);
    let painted = frame(
        &mut app,
        &ctx,
        0.1,
        key(egui::Key::F, egui::Modifiers::CTRL),
    );
    click(&mut app, &ctx, 0.15, painted.point("Multiline"));
    assert!(app.find.multiline);
    query(&mut app, &ctx, "a");
    frame(
        &mut app,
        &ctx,
        0.4,
        key(egui::Key::Enter, egui::Modifiers::NONE),
    );
    frame(&mut app, &ctx, 0.5, vec![egui::Event::Text("b".into())]);
    assert_eq!(app.find.query, "a\nb");
    assert_eq!(app.tabs[0].state.doc.text(), source);
    frame(
        &mut app,
        &ctx,
        0.6,
        key(egui::Key::Enter, egui::Modifiers::CTRL),
    );
    assert_eq!(app.find.query, "a\nb");
    assert_eq!(
        app.tabs[0].state.primary().range().start(),
        TextSize::new(4)
    );
    assert!(app.find.query_focused);
    frame(
        &mut app,
        &ctx,
        0.7,
        key(egui::Key::Escape, egui::Modifiers::NONE),
    );
    assert!(!app.find.visible);
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert!(!app.tabs[0].state.doc.can_undo());
}

#[test]
fn multiline_replacement_previews_newline_without_edit_then_control_enter_applies_once() {
    let (mut app, ctx) = fixture("foo", Language::PlainText);
    let painted = open_replace(&mut app, &ctx);
    let painted = click(&mut app, &ctx, 0.15, painted.point("Multiline"));
    frame(
        &mut app,
        &ctx,
        0.18,
        key(egui::Key::Enter, egui::Modifiers::CTRL),
    );
    assert_eq!(
        app.tabs[0]
            .state
            .doc
            .slice(app.tabs[0].state.primary().range()),
        "foo"
    );
    click(&mut app, &ctx, 0.2, painted.point("Replace with"));
    let version = app.tabs[0].state.doc.version();
    frame(&mut app, &ctx, 0.3, vec![egui::Event::Text("π".into())]);
    frame(
        &mut app,
        &ctx,
        0.4,
        key(egui::Key::Enter, egui::Modifiers::NONE),
    );
    let painted = frame(&mut app, &ctx, 0.5, vec![egui::Event::Text("🙂".into())]);
    assert_eq!(app.find.replacement, "π\n🙂");
    assert!(painted.contains("foo → π\\n🙂"));
    assert_eq!(app.tabs[0].state.doc.text(), "foo");
    assert_eq!(app.tabs[0].state.doc.version(), version);
    assert!(!app.tabs[0].state.doc.can_undo());
    frame(
        &mut app,
        &ctx,
        0.6,
        key(egui::Key::Enter, egui::Modifiers::CTRL),
    );
    assert_eq!(app.tabs[0].state.doc.text(), "π\n🙂");
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), "foo");
    assert!(!app.tabs[0].state.doc.can_undo());
}

#[test]
fn regex_live_preview_tracks_replacement_and_exclusion_without_mutating_buffer() {
    let source = "snake_case snake_case";
    let (mut app, ctx) = fixture(source, Language::PlainText);
    app.find.regex = true;
    open_replace(&mut app, &ctx);
    let painted = query(&mut app, &ctx, r"(\w+)_(\w+)");
    click(&mut app, &ctx, 0.4, painted.point("Replace with"));
    let caret = app.tabs[0].state.carets.clone();
    let version = app.tabs[0].state.doc.version();
    let painted = frame(
        &mut app,
        &ctx,
        0.5,
        vec![egui::Event::Text(r"$1\u$2".into())],
    );
    assert!(painted.contains("snake_case → snakeCase"));
    assert_eq!(app.find.preview.as_ref().unwrap().rows.len(), 2);
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert_eq!(app.tabs[0].state.doc.version(), version);
    assert_eq!(app.tabs[0].state.carets, caret);
    assert!(!app.tabs[0].state.doc.can_undo());
    let painted = click(&mut app, &ctx, 0.6, painted.point("Exclude"));
    assert_eq!(app.find.excluded.len(), 1);
    frame(&mut app, &ctx, 0.7, vec![]);
    assert_eq!(app.find.preview.as_ref().unwrap().rows.len(), 1);
    click(&mut app, &ctx, 0.8, painted.point("Replace All"));
    assert_eq!(app.tabs[0].state.doc.text(), "snake_case snakeCase");
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), source);
}

#[test]
fn actual_context_selector_restricts_navigation_preview_and_replace_to_comments() {
    let source = "needle = 1\ntext = 'needle'\n# needle";
    let (mut app, ctx) = fixture(source, Language::Python);
    let painted = open_replace(&mut app, &ctx);
    click(
        &mut app,
        &ctx,
        0.15,
        painted.point(find::SearchFilter::Anywhere.label()),
    );
    // The popup first measures its contents before the next frame paints them.
    let painted = frame(&mut app, &ctx, 0.17, vec![]);
    click(
        &mut app,
        &ctx,
        0.18,
        painted.point(find::SearchFilter::Comments.label()),
    );
    assert_eq!(app.find.filter, find::SearchFilter::Comments);
    let painted = query(&mut app, &ctx, "needle");
    assert_eq!(app.find.matches.len(), 1);
    assert_eq!(
        app.tabs[0]
            .state
            .doc
            .slice(app.tabs[0].state.primary().range()),
        "needle"
    );
    assert_eq!(app.tabs[0].state.primary_position().line, 2);
    click(&mut app, &ctx, 0.4, painted.point("Replace with"));
    let painted = frame(
        &mut app,
        &ctx,
        0.5,
        vec![egui::Event::Text("changed".into())],
    );
    assert_eq!(app.find.preview.as_ref().unwrap().rows.len(), 1);
    click(&mut app, &ctx, 0.6, painted.point("Replace All"));
    assert_eq!(
        app.tabs[0].state.doc.text(),
        "needle = 1\ntext = 'needle'\n# changed"
    );
}

#[test]
fn readonly_replace_all_preserves_preview_and_reports_refusal_not_replaced_count() {
    let (mut app, ctx) = fixture("foo foo", Language::PlainText);
    let painted = open_replace(&mut app, &ctx);
    click(&mut app, &ctx, 0.2, painted.point("Replace with"));
    app.tabs[0].state.doc.set_read_only(true);
    let painted = frame(&mut app, &ctx, 0.3, vec![egui::Event::Text("bar".into())]);
    assert!(painted.contains("foo → bar"));
    click(&mut app, &ctx, 0.4, painted.point("Replace All"));
    assert_eq!(app.tabs[0].state.doc.text(), "foo foo");
    assert!(!app.tabs[0].state.doc.can_undo());
    assert_eq!(
        app.status.as_ref().unwrap().0,
        "Cannot replace: file is read-only"
    );
    assert!(app.tabs[0].state.read_only_refused);
}

#[test]
fn multiline_control_shift_enter_owns_query_navigation_before_global_dispatch() {
    let source = "foo()\nfoo()";
    let (mut app, ctx) = fixture(source, Language::Python);
    let painted = open_replace(&mut app, &ctx);
    click(&mut app, &ctx, 0.15, painted.point("Multiline"));
    query(&mut app, &ctx, "foo");
    frame(
        &mut app,
        &ctx,
        0.4,
        key(egui::Key::Enter, egui::Modifiers::CTRL),
    );
    assert_eq!(
        app.tabs[0].state.primary().range().start(),
        TextSize::new(6)
    );
    let version = app.tabs[0].state.doc.version();
    frame(
        &mut app,
        &ctx,
        0.6,
        key(
            egui::Key::Enter,
            egui::Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
        ),
    );
    assert_eq!(
        app.tabs[0].state.primary().range().start(),
        TextSize::new(0)
    );
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert_eq!(app.tabs[0].state.doc.version(), version);
    assert!(!app.tabs[0].state.doc.can_undo());
    assert!(app.find.query_focused);
}

#[test]
fn multiline_control_shift_enter_replaces_all_once_before_global_dispatch() {
    let (mut app, ctx) = fixture("foo foo", Language::PlainText);
    let painted = open_replace(&mut app, &ctx);
    let painted = click(&mut app, &ctx, 0.15, painted.point("Multiline"));
    click(&mut app, &ctx, 0.2, painted.point("Replace with"));
    frame(&mut app, &ctx, 0.3, vec![egui::Event::Text("bar".into())]);
    frame(
        &mut app,
        &ctx,
        0.4,
        key(
            egui::Key::Enter,
            egui::Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
        ),
    );
    assert_eq!(app.tabs[0].state.doc.text(), "bar bar");
    assert!(app.find.replacement_focused);
    app.tabs[0].state.undo();
    assert_eq!(app.tabs[0].state.doc.text(), "foo foo");
    assert!(!app.tabs[0].state.doc.can_undo());
}

#[test]
fn oversized_live_preview_reports_omission_without_claiming_no_occurrences() {
    let (mut app, ctx) = fixture("foo", Language::PlainText);
    let painted = open_replace(&mut app, &ctx);
    click(&mut app, &ctx, 0.2, painted.point("Replace with"));
    let version = app.tabs[0].state.doc.version();
    let painted = frame(
        &mut app,
        &ctx,
        0.3,
        vec![egui::Event::Text("x".repeat(256 * 1024 + 1))],
    );
    let preview = app.find.preview.as_ref().unwrap();
    assert!(preview.rows.is_empty() && preview.has_more);
    assert!(painted.contains("Preview omitted due to size limit"));
    assert!(!painted.contains("No eligible occurrences"));
    assert_eq!(app.tabs[0].state.doc.text(), "foo");
    assert_eq!(app.tabs[0].state.doc.version(), version);
    assert!(!app.tabs[0].state.doc.can_undo());
}
