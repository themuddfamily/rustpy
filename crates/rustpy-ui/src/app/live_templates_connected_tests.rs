//! Connected abbreviation / Surround With keyboard paths through the actual App.
use super::*;
use crate::editor::view::{self, ViewConfig};

fn fixture(source: &str) -> (IdeApp, egui::Context) {
    let mut app = IdeApp::new(Settings::default(), None);
    app.templates = templates::builtin_templates();
    app.push_tab(EditorState::from_text(source, Language::Python));
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    assert!(app.tabs[0].view.focused);
    (app, ctx)
}

fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
    ctx.run_ui(
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
            // Production ordering: keyboard owners run before editor input;
            // the real chooser paints after the editor and consumes its keys.
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| {
                let opts = app.edit_options();
                let request_focus = std::mem::take(&mut app.focus_editor);
                let cfg = ViewConfig {
                    scheme: &app.scheme,
                    theme: &app.theme,
                    settings: &app.settings.editor,
                    opts: &opts,
                    keymap: &app.keymap,
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
                    keyboard: app.editor_keyboard(),
                };
                let tab = &mut app.tabs[app.active.unwrap()];
                let actions = view::show(ui, &mut tab.state, &mut tab.view, &cfg);
                for action in actions {
                    if let view::ViewEvent::Action(action) = action {
                        app.handle_action(action, ui.ctx());
                    }
                }
            });
            app.draw_choice_popup(ui.ctx());
        },
    )
    .drop_without_applying_deltas();
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
fn plain(keycode: egui::Key) -> Vec<egui::Event> {
    key(keycode, egui::Modifiers::NONE)
}
fn selected(app: &IdeApp) -> String {
    app.tabs[0]
        .state
        .doc
        .slice(app.tabs[0].state.primary().range())
}

#[test]
fn typed_builtin_abbreviation_tab_traverses_variables_and_finishes_in_body() {
    let (mut app, ctx) = fixture("");
    frame(&mut app, &ctx, 0.1, vec![egui::Event::Text("iter".into())]);
    assert_eq!(app.tabs[0].state.doc.text(), "iter");
    assert!(app.template_session.is_none());
    frame(&mut app, &ctx, 0.2, plain(egui::Key::Tab));
    assert_eq!(app.tabs[0].state.doc.text(), "for item in iterable:\n    ");
    assert_eq!(selected(&app), "item");
    assert_eq!(app.template_session.as_ref().unwrap().idx, 0);
    frame(&mut app, &ctx, 0.3, vec![egui::Event::Text("entry".into())]);
    frame(&mut app, &ctx, 0.4, plain(egui::Key::Tab));
    assert_eq!(selected(&app), "iterable");
    assert_eq!(app.template_session.as_ref().unwrap().idx, 1);
    frame(
        &mut app,
        &ctx,
        0.5,
        vec![egui::Event::Text("values".into())],
    );
    frame(&mut app, &ctx, 0.6, plain(egui::Key::Tab));
    assert!(app.template_session.is_none());
    assert!(!app.tabs[0].state.primary().has_selection());
    assert_eq!(
        app.tabs[0].state.primary_position(),
        rustpy_core::Position::new(1, 4)
    );
    frame(
        &mut app,
        &ctx,
        0.7,
        vec![egui::Event::Text("print(entry)".into())],
    );
    assert_eq!(
        app.tabs[0].state.doc.text(),
        "for entry in values:\n    print(entry)"
    );
}

#[test]
fn template_tab_finishes_variable_before_accepting_visible_completion() {
    let (mut app, ctx) = fixture("");
    frame(&mut app, &ctx, 0.1, vec![egui::Event::Text("iter".into())]);
    frame(&mut app, &ctx, 0.2, plain(egui::Key::Tab));
    frame(&mut app, &ctx, 0.3, vec![egui::Event::Text("entry".into())]);
    frame(&mut app, &ctx, 0.4, plain(egui::Key::Tab));
    frame(
        &mut app,
        &ctx,
        0.5,
        vec![egui::Event::Text("values".into())],
    );
    app.completion.items = vec![CompletionItem {
        label: "dict_values".into(),
        kind: CompletionKind::Class,
        detail: String::new(),
        insert: "dict_values".into(),
        caret_offset: None,
        priority: 100,
    }];
    app.completion.selected = 0;
    app.completion.prefix_start =
        TextSize::new(app.tabs[0].state.doc.text().find("values").unwrap() as u32);
    app.completion.visible = true;
    frame(&mut app, &ctx, 0.6, plain(egui::Key::Tab));
    assert!(app.template_session.is_none());
    assert!(!app.completion.visible);
    assert_eq!(app.tabs[0].state.doc.text(), "for entry in values:\n    ");
    assert_eq!(
        app.tabs[0].state.primary_position(),
        rustpy_core::Position::new(1, 4)
    );
    frame(
        &mut app,
        &ctx,
        0.7,
        vec![egui::Event::Text("print(entry)".into())],
    );
    assert_eq!(
        app.tabs[0].state.doc.text(),
        "for entry in values:\n    print(entry)"
    );
}

fn surround(source: &str, abbreviation: &str) -> IdeApp {
    let (mut app, ctx) = fixture(source);
    let end = app.tabs[0].state.doc.len();
    app.tabs[0]
        .state
        .set_carets(vec![crate::editor::Caret::new(TextSize::new(0), end)]);
    let before = app.tabs[0].state.selections();
    frame(
        &mut app,
        &ctx,
        0.1,
        key(
            egui::Key::T,
            egui::Modifiers {
                ctrl: true,
                alt: true,
                ..Default::default()
            },
        ),
    );
    assert!(matches!(
        app.choice.as_ref().map(|choice| choice.kind),
        Some(ChoiceKind::Surround)
    ));
    assert_eq!(
        app.tabs[0].state.doc.text(),
        source,
        "opening chooser is observational"
    );
    frame(
        &mut app,
        &ctx,
        0.2,
        vec![egui::Event::Text(abbreviation.into())],
    );
    let choice = app.choice.as_ref().unwrap();
    assert_eq!(choice.items[choice.current().unwrap()].0, abbreviation);
    frame(&mut app, &ctx, 0.3, plain(egui::Key::Enter));
    assert!(app.choice.is_none());
    assert_eq!(
        app.tabs[0].state.doc.undo_label().as_deref(),
        Some("Live template")
    );
    let expanded = app.tabs[0].state.doc.text();
    assert!(app.dispatch_editor_action(Action::Undo));
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert_eq!(app.tabs[0].state.selections(), before);
    assert!(app.dispatch_editor_action(Action::Redo));
    assert_eq!(app.tabs[0].state.doc.text(), expanded);
    app
}

#[test]
fn real_surround_chooser_covers_statement_catalog_and_preserves_unicode_selection() {
    let source = "print('π🙂')\nkeep()";
    let body = "    print('π🙂')\n    keep()";
    for (abbreviation, expected) in [
        ("if", format!("if condition:\n{body}")),
        ("while", format!("while condition:\n{body}")),
        ("for", format!("for item in iterable:\n{body}")),
        ("with", format!("with expr as name:\n{body}")),
        ("try", format!("try:\n{body}\nexcept Exception:\n    ")),
        ("tryfinally", format!("try:\n{body}\nfinally:\n    ")),
        ("def", format!("def function():\n{body}")),
        ("class", format!("class ClassName:\n{body}")),
    ] {
        assert_eq!(
            surround(source, abbreviation).tabs[0].state.doc.text(),
            expected,
            "{abbreviation}"
        );
    }
}

#[test]
fn real_surround_chooser_covers_expression_parentheses_quotes_and_fstring() {
    let source = "π + value";
    for (abbreviation, expected) in [
        ("(", "(π + value)"),
        ("[", "[π + value]"),
        ("{", "{π + value}"),
        ("\"", "\"π + value\""),
        ("'", "'π + value'"),
        ("f\"", "f\"{π + value}\""),
    ] {
        assert_eq!(
            surround(source, abbreviation).tabs[0].state.doc.text(),
            expected,
            "{abbreviation}"
        );
    }
}

#[test]
fn surround_escape_cancels_without_mutating_text_selection_or_history() {
    let (mut app, ctx) = fixture("π + value");
    let end = app.tabs[0].state.doc.len();
    app.tabs[0]
        .state
        .set_carets(vec![crate::editor::Caret::new(TextSize::new(0), end)]);
    let before = app.tabs[0].state.selections();
    frame(
        &mut app,
        &ctx,
        0.1,
        key(
            egui::Key::T,
            egui::Modifiers {
                ctrl: true,
                alt: true,
                ..Default::default()
            },
        ),
    );
    assert!(app.choice.is_some());
    frame(&mut app, &ctx, 0.2, plain(egui::Key::Escape));
    assert!(app.choice.is_none());
    assert_eq!(app.tabs[0].state.doc.text(), "π + value");
    assert_eq!(app.tabs[0].state.selections(), before);
    assert!(!app.tabs[0].state.doc.can_undo());
}

#[test]
fn readonly_abbreviation_tab_never_publishes_a_template_session() {
    let (mut app, ctx) = fixture("iter");
    let end = app.tabs[0].state.doc.len();
    app.tabs[0]
        .state
        .set_carets(vec![crate::editor::Caret::at(end)]);
    app.tabs[0].state.doc.set_read_only(true);
    let before = app.tabs[0].state.selections();
    frame(&mut app, &ctx, 0.1, plain(egui::Key::Tab));
    assert_eq!(app.tabs[0].state.doc.text(), "iter");
    assert_eq!(app.tabs[0].state.selections(), before);
    assert!(!app.tabs[0].state.doc.can_undo());
    assert!(
        app.template_session.is_none(),
        "rejected expansion must not claim editable template variables"
    );
}
