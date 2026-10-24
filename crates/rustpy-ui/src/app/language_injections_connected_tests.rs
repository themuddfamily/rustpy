//! Connected picker, source paint and separate EditorView fragment routes.
use super::language_injections_ui::InjectionUi;
use super::*;
use crate::editor::language_injections::Language as InjectedLanguage;
use rustpy_highlight::HighlightTag;
fn fixture(source: &str) -> (tempfile::TempDir, IdeApp, egui::Context, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("host.py");
    std::fs::write(&path, source).unwrap();
    let mut settings = Settings::default();
    settings.completion.show_as_you_type = false;
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]).drop_without_applying_deltas();
    frame(&mut app, &ctx, 0.02, vec![]).drop_without_applying_deltas();
    (root, app, ctx, path)
}
fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            app.dialogs(ui.ctx());
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    )
}
fn text_rect(output: &egui::FullOutput, label: &str) -> Rect {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(t) if t.galley.text() == label => {
                Some(Rect::from_min_size(t.pos, t.galley.size()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing rendered {label}"))
}
fn click(app: &mut IdeApp, ctx: &egui::Context, time: f64, rect: Rect) {
    for (pressed, at) in [(true, time), (false, time + 0.02)] {
        frame(
            app,
            ctx,
            at,
            vec![
                egui::Event::PointerMoved(rect.center()),
                egui::Event::PointerButton {
                    pos: rect.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .drop_without_applying_deltas();
    }
}
fn select_all() -> Vec<egui::Event> {
    vec![
        egui::Event::ModifiersChanged(egui::Modifiers::CTRL),
        egui::Event::Key {
            key: egui::Key::A,
            physical_key: Some(egui::Key::A),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        },
        egui::Event::Key {
            key: egui::Key::A,
            physical_key: Some(egui::Key::A),
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        },
        egui::Event::ModifiersChanged(egui::Modifiers::NONE),
    ]
}
#[test]
fn actual_picker_action_changes_source_highlight_without_document_or_history_edits() {
    let source = "payload = 'SELECT 1'";
    let (_root, mut app, ctx, path) = fixture(source);
    let i = app.active.unwrap();
    app.tabs[i].state.go_to_line(1, Some(12));
    let version = app.tabs[i].state.doc.version();
    app.handle_action(Action::InjectLanguage, &ctx);
    let out = frame(&mut app, &ctx, 0.04, vec![]);
    out.drop_without_applying_deltas();
    let out = frame(&mut app, &ctx, 0.06, vec![]);
    let sql = text_rect(&out, "sql");
    out.drop_without_applying_deltas();
    click(&mut app, &ctx, 0.08, sql);
    assert!(!app.injection_ui.active());
    assert_eq!(
        app.tabs[i].state.current_injection().unwrap().language,
        InjectedLanguage::Sql
    );
    let spans = app.tabs[i].state.highlight_spans().to_vec();
    assert!(spans.iter().any(
        |s| s.tag == HighlightTag::Keyword && app.tabs[i].state.doc.slice(s.range) == "SELECT"
    ));
    let rgb = app.scheme.style(HighlightTag::Keyword).fg.unwrap();
    let color = Color32::from_rgb(rgb.0, rgb.1, rgb.2);
    let output = frame(&mut app, &ctx, 0.14, vec![]);
    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.sections.iter().any(|section| section.format.color == color && text.galley.job.text[section.byte_range.start.0..section.byte_range.end.0].contains("SELECT")))), "actual View must paint injected SQL keyword style");
    output.drop_without_applying_deltas();
    assert_eq!(app.tabs[i].state.doc.version(), version);
    assert_eq!(app.tabs[i].state.doc.text(), source);
    assert!(!app.tabs[i].state.doc.can_undo());
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    assert!(app.injected_intention_action("Remove temporary language injection"));
    assert!(app.tabs[i].state.current_injection().is_none());
}
#[test]
fn actual_separate_editor_paste_apply_is_one_host_undo_and_cancel_is_inert() {
    let source = "# language=json\npayload = '{}'\nafter = 1";
    let (_root, mut app, ctx, path) = fixture(source);
    let i = app.active.unwrap();
    app.tabs[i].state.go_to_line(2, Some(12));
    app.handle_action(Action::EditInjectedFragment, &ctx);
    frame(&mut app, &ctx, 0.04, vec![]).drop_without_applying_deltas();
    frame(&mut app, &ctx, 0.06, vec![]).drop_without_applying_deltas();
    frame(&mut app, &ctx, 0.08, select_all()).drop_without_applying_deltas();
    frame(
        &mut app,
        &ctx,
        0.10,
        vec![egui::Event::Paste("{\"π🙂\": 2}".into())],
    )
    .drop_without_applying_deltas();
    assert_eq!(
        app.tabs[i].state.doc.text(),
        source,
        "fragment typing must not reach host"
    );
    let out = frame(&mut app, &ctx, 0.12, vec![]);
    let apply = text_rect(&out, "Apply");
    out.drop_without_applying_deltas();
    click(&mut app, &ctx, 0.14, apply);
    assert!(!app.injection_ui.active());
    let changed = app.tabs[i].state.doc.text();
    let parsed = rustpy_syntax::parse(&changed);
    let literal = parsed
        .syntax_node()
        .descendants()
        .find_map(rustpy_syntax::ast::StringExpr::cast)
        .unwrap();
    assert_eq!(literal.value(), "{\"π🙂\": 2}");
    assert!(changed.ends_with("after = 1"));
    assert!(app.tabs[i].state.undo());
    assert_eq!(app.tabs[i].state.doc.text(), source);
    assert!(!app.tabs[i].state.doc.can_undo());
    assert!(app.tabs[i].state.redo());
    assert_eq!(app.tabs[i].state.doc.text(), changed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    app.tabs[i].state.go_to_line(2, Some(12));
    app.handle_action(Action::EditInjectedFragment, &ctx);
    let before = app.tabs[i].state.doc.text();
    let version = app.tabs[i].state.doc.version();
    let out = frame(&mut app, &ctx, 0.18, vec![]);
    out.drop_without_applying_deltas();
    let out = frame(&mut app, &ctx, 0.20, vec![]);
    let cancel = text_rect(&out, "Cancel");
    out.drop_without_applying_deltas();
    click(&mut app, &ctx, 0.22, cancel);
    assert_eq!(app.tabs[i].state.doc.text(), before);
    assert_eq!(app.tabs[i].state.doc.version(), version);
}
#[test]
fn actual_stale_picker_readonly_fragment_and_file_reference_use_captured_host() {
    let source = "path = 'target π.py'";
    let (root, mut app, ctx, path) = fixture(source);
    let i = app.active.unwrap();
    app.tabs[i].state.go_to_line(1, Some(10));
    app.handle_action(Action::InjectLanguage, &ctx);
    app.tabs[i]
        .state
        .insert_text("x", &crate::editor::EditOptions::default());
    frame(&mut app, &ctx, 0.04, vec![]).drop_without_applying_deltas();
    assert!(!app.injection_ui.active());
    assert!(app
        .status
        .as_ref()
        .is_some_and(|(s, _)| s.contains("changed")));
    assert!(app.tabs[i].state.undo());
    app.tabs[i].state.go_to_line(1, Some(10));
    app.tabs[i]
        .state
        .inject_language(InjectedLanguage::FileReference)
        .unwrap();
    let target = root.path().join("target π.py");
    std::fs::write(&target, "target = 1").unwrap();
    app.handle_action(Action::GoToInjectedReference, &ctx);
    assert_eq!(
        app.tabs[app.active.unwrap()].state.path.as_deref(),
        Some(target.as_path())
    );
    app.open_file(&path);
    let i = app.active.unwrap();
    app.tabs[i].state.go_to_line(1, Some(10));
    app.tabs[i]
        .state
        .inject_language(InjectedLanguage::Json)
        .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).unwrap();
    app.handle_action(Action::EditInjectedFragment, &ctx);
    frame(&mut app, &ctx, 0.06, vec![]).drop_without_applying_deltas();
    assert!(
        matches!(&app.injection_ui,InjectionUi::Fragment {editor,..}if editor.doc.is_read_only())
    );
    assert_eq!(app.tabs[i].state.doc.text(), source);
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[test]
fn picker_refuses_caret_retarget_without_any_source_edit() {
    let source = "first = 'SELECT 1'\nsecond = 'SELECT 2'";
    let (_root, mut app, ctx, _) = fixture(source);
    let i = app.active.unwrap();
    app.tabs[i].state.go_to_line(1, Some(10));
    let version = app.tabs[i].state.doc.version();
    app.handle_action(Action::InjectLanguage, &ctx);
    frame(&mut app, &ctx, 0.04, vec![]).drop_without_applying_deltas();
    app.tabs[i].state.go_to_line(2, Some(11));
    frame(&mut app, &ctx, 0.06, vec![]).drop_without_applying_deltas();
    assert!(!app.injection_ui.active());
    assert!(app.tabs[i].state.current_injection().is_none());
    assert_eq!(app.tabs[i].state.doc.version(), version);
    assert_eq!(app.tabs[i].state.doc.text(), source);
    assert!(!app.tabs[i].state.doc.can_undo());
}

#[test]
fn actual_fragment_context_menu_paste_targets_separate_editor_only() {
    let source = "# language=json\npayload = '{}'";
    let (_root, mut app, ctx, _) = fixture(source);
    let i = app.active.unwrap();
    app.tabs[i].state.go_to_line(2, Some(12));
    app.handle_action(Action::EditInjectedFragment, &ctx);
    frame(&mut app, &ctx, 0.04, vec![]).drop_without_applying_deltas();
    let out = frame(&mut app, &ctx, 0.06, vec![]);
    let pos = out
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == "{}" => Some(t.pos + Vec2::new(3.0, 3.0)),
            _ => None,
        })
        .expect("actual separate editor text must be rendered");
    out.drop_without_applying_deltas();
    app.clipboard_history = vec!["{\"context\": 3}".into()];
    for (time, pressed) in [(0.08, true), (0.10, false)] {
        frame(
            &mut app,
            &ctx,
            time,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .drop_without_applying_deltas();
    }
    let out = frame(&mut app, &ctx, 0.12, vec![]);
    let paste = text_rect(&out, Action::Paste.text());
    out.drop_without_applying_deltas();
    click(&mut app, &ctx, 0.14, paste);
    assert_eq!(app.tabs[i].state.doc.text(), source);
    let InjectionUi::Fragment { editor, .. } = &app.injection_ui else {
        panic!("fragment remains open")
    };
    assert!(
        editor.doc.text().contains("\"context\": 3"),
        "actual returned context Paste must reach fragment"
    );
    assert!(!app.tabs[i].state.doc.can_undo());
}
