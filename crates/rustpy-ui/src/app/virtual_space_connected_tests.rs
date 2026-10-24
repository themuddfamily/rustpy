use super::*;
use crate::editor::view::{self, ViewConfig};
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

#[test]
fn real_tab_after_virtual_abbreviation_materializes_gap_instead_of_expanding() {
    let mut app = IdeApp::new(Settings::default(), None);
    app.settings.editor.allow_caret_after_end_of_line = true;
    app.templates = templates::builtin_templates();
    app.push_tab(EditorState::from_text("iter", Language::Python));
    app.tabs[0].state.set_virtual_space_policy(true, true);
    app.tabs[0]
        .state
        .set_virtual_carets(vec![crate::editor::state::virtual_space::VirtualCaret {
            anchor: rustpy_core::Position::new(0, 8),
            head: rustpy_core::Position::new(0, 8),
        }]);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    assert!(app.tabs[0].view.focused);
    assert_eq!(app.tabs[0].state.doc.text(), "iter");
    frame(&mut app, &ctx, 0.1, plain(egui::Key::Tab));
    assert_eq!(app.tabs[0].state.doc.text(), "iter        ");
    assert!(app.template_session.is_none());
    assert_eq!(
        app.tabs[0].state.virtual_caret(0).head,
        rustpy_core::Position::new(0, 12)
    );
    assert!(app.dispatch_editor_action(Action::Undo));
    assert_eq!(app.tabs[0].state.doc.text(), "iter");
    assert_eq!(
        app.tabs[0].state.virtual_caret(0).head,
        rustpy_core::Position::new(0, 8)
    );
    assert!(!app.tabs[0].state.doc.can_undo());
}

fn virtual_fixture(
    source: &str,
    positions: Vec<crate::editor::state::virtual_space::VirtualCaret>,
) -> (IdeApp, egui::Context) {
    let mut app = IdeApp::new(Settings::default(), None);
    app.settings.editor.allow_caret_after_end_of_line = true;
    app.templates = templates::builtin_templates();
    app.push_tab(EditorState::from_text(source, Language::Python));
    app.tabs[0].state.set_virtual_space_policy(true, true);
    app.tabs[0].state.set_virtual_carets(positions);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    (app, ctx)
}
fn position(line: u32, column: u32) -> crate::editor::state::virtual_space::VirtualCaret {
    let position = rustpy_core::Position::new(line, column);
    crate::editor::state::virtual_space::VirtualCaret {
        anchor: position,
        head: position,
    }
}
fn completion(app: &mut IdeApp, kind: CompletionKind, label: &str) {
    app.completion.items = vec![CompletionItem {
        label: label.into(),
        kind,
        detail: String::new(),
        insert: if kind == CompletionKind::Function {
            format!("{label}()")
        } else {
            label.into()
        },
        caret_offset: (kind == CompletionKind::Function).then_some(label.len() + 1),
        priority: 100,
    }];
    app.completion.selected = 0;
    app.completion.imports = vec![None];
    app.completion.prefix_start = TextSize::new(0); // stale physical prefix must never authorize replacement before the gap.
    app.completion.doc_version = app.tabs[0].state.doc.version();
    app.completion.caret = app.tabs[0].state.primary().head();
    app.completion.visible = true;
}
#[test]
fn real_completion_tab_inserts_at_each_virtual_position_and_undo_restores_authority() {
    let source = "αβ\nx";
    let original = vec![position(0, 6), position(1, 5)];
    let (mut app, ctx) = virtual_fixture(source, original.clone());
    completion(&mut app, CompletionKind::Class, "thing");
    frame(&mut app, &ctx, 0.1, plain(egui::Key::Tab));
    assert_eq!(app.tabs[0].state.doc.text(), "αβ    thing\nx    thing");
    assert!(!app.completion.visible);
    assert!(app.dispatch_editor_action(Action::Undo));
    assert_eq!(app.tabs[0].state.doc.text(), source);
    for (i, expected) in original.iter().enumerate() {
        assert_eq!(&app.tabs[0].state.virtual_caret(i), expected);
    }
    assert!(!app.tabs[0].state.doc.can_undo());
    assert!(app.dispatch_editor_action(Action::Redo));
    assert_eq!(app.tabs[0].state.doc.text(), "αβ    thing\nx    thing");
}
#[test]
fn real_callable_completion_places_caret_inside_parentheses_after_virtual_gap() {
    let (mut app, ctx) = virtual_fixture("αβ", vec![position(0, 6)]);
    completion(&mut app, CompletionKind::Function, "call");
    frame(&mut app, &ctx, 0.1, plain(egui::Key::Tab));
    assert_eq!(app.tabs[0].state.doc.text(), "αβ    call()");
    assert_eq!(
        app.tabs[0].state.primary_position(),
        rustpy_core::Position::new(0, 11)
    );
    assert!(app.dispatch_editor_action(Action::Undo));
    assert_eq!(app.tabs[0].state.doc.text(), "αβ");
    assert_eq!(app.tabs[0].state.virtual_caret(0), position(0, 6));
}
#[test]
fn actual_insert_template_chooser_recomputes_stops_after_padding_and_finishes() {
    let (mut app, ctx) = virtual_fixture("", vec![position(0, 4)]);
    frame(
        &mut app,
        &ctx,
        0.1,
        key(
            egui::Key::J,
            egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        ),
    );
    assert!(matches!(
        app.choice.as_ref().map(|choice| choice.kind),
        Some(ChoiceKind::LiveTemplate)
    ));
    frame(&mut app, &ctx, 0.2, vec![egui::Event::Text("iter".into())]);
    frame(&mut app, &ctx, 0.3, plain(egui::Key::Enter));
    assert_eq!(
        app.tabs[0].state.doc.text(),
        "    for item in iterable:\n        "
    );
    assert_eq!(
        app.tabs[0]
            .state
            .doc
            .slice(app.tabs[0].state.primary().range()),
        "item"
    );
    assert_eq!(
        app.template_session.as_ref().unwrap().vars[0].1[0].start(),
        TextSize::new(8)
    );
    frame(&mut app, &ctx, 0.4, plain(egui::Key::Tab));
    assert_eq!(
        app.tabs[0]
            .state
            .doc
            .slice(app.tabs[0].state.primary().range()),
        "iterable"
    );
    frame(&mut app, &ctx, 0.5, plain(egui::Key::Tab));
    assert!(app.template_session.is_none());
    assert_eq!(
        app.tabs[0].state.primary_position(),
        rustpy_core::Position::new(1, 8)
    );
    assert!(app.dispatch_editor_action(Action::Undo));
    assert_eq!(app.tabs[0].state.doc.text(), "");
    assert_eq!(app.tabs[0].state.virtual_caret(0), position(0, 4));
    assert!(!app.tabs[0].state.doc.can_undo());
}
#[test]
fn readonly_virtual_completion_and_template_never_publish_padding_or_session() {
    for template in [false, true] {
        let (mut app, ctx) = virtual_fixture("", vec![position(0, 4)]);
        app.tabs[0].state.doc.set_read_only(true);
        completion(
            &mut app,
            if template {
                CompletionKind::Template
            } else {
                CompletionKind::Class
            },
            if template { "iter" } else { "thing" },
        );
        frame(&mut app, &ctx, 0.1, plain(egui::Key::Tab));
        assert_eq!(app.tabs[0].state.doc.text(), "");
        assert_eq!(app.tabs[0].state.virtual_caret(0), position(0, 4));
        assert!(app.template_session.is_none());
        assert!(!app.tabs[0].state.doc.can_undo());
    }
}

#[test]
fn real_virtual_autoimport_completion_maps_unicode_caret_and_undo_as_one_edit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.py");
    let original = "αβ";
    std::fs::write(&path, original.as_bytes()).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(directory.path());
    settings.editor.allow_caret_after_end_of_line = true;
    let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
    app.open_file(&path);
    assert_eq!(app.tabs.len(), 1);
    app.tabs[0].state.set_virtual_space_policy(true, true);
    app.tabs[0].state.set_virtual_carets(vec![position(0, 6)]);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    completion(&mut app, CompletionKind::Class, "Path");
    app.completion.imports = vec![Some(("pathlib".into(), "Path".into()))];

    frame(&mut app, &ctx, 0.1, plain(egui::Key::Tab));
    let expected = "from pathlib import Path\nαβ    Path";
    assert_eq!(app.tabs[0].state.doc.text(), expected);
    assert_eq!(
        app.tabs[0].state.primary_position(),
        rustpy_core::Position::new(1, 10)
    );
    assert_eq!(
        app.tabs[0]
            .state
            .doc
            .slice(app.tabs[0].state.doc.line_range(1)),
        "αβ    Path"
    );
    assert!(!app.completion.visible);
    assert!(app.import_completion.is_none());
    assert!(app.template_session.is_none());
    assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());

    assert!(app.dispatch_editor_action(Action::Undo));
    assert_eq!(app.tabs[0].state.doc.text(), original);
    assert_eq!(app.tabs[0].state.virtual_caret(0), position(0, 6));
    assert!(!app.tabs[0].state.doc.can_undo());
    assert!(app.template_session.is_none());
    assert!(app.dispatch_editor_action(Action::Redo));
    assert_eq!(app.tabs[0].state.doc.text(), expected);
    assert_eq!(
        app.tabs[0].state.primary_position(),
        rustpy_core::Position::new(1, 10)
    );
    assert!(app.template_session.is_none());
}
