//! Actual App documentation and file-aware paste consumers.
use super::*;
fn app(source: &str, enabled: bool) -> (tempfile::TempDir, IdeApp) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.py");
    std::fs::write(&path, source).unwrap();
    let mut settings = Settings {
        docstring_format: "rest".into(),
        ..Default::default()
    };
    settings.editor.insert_type_placeholders_in_docstring_stub = enabled;
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    (root, app)
}
fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            app.dialogs(ui.ctx());
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    )
    .drop_without_applying_deltas();
}
#[test]
fn actual_generate_update_actions_propagate_optional_types_preserving_existing_docs() {
    for enabled in [false, true] {
        let source = "def f(value):\n    return value\n";
        let (root, mut app) = app(source, enabled);
        let ctx = egui::Context::default();
        app.tabs[0].state.go_to_line(1, Some(5));
        app.handle_action(Action::GenerateDocstring, &ctx);
        let text = app.tabs[0].state.doc.text();
        assert!(text.contains(":param value:"), "{text}");
        assert_eq!(text.contains(":type value:"), enabled, "{text}");
        assert_eq!(
            std::fs::read_to_string(root.path().join("fixture.py")).unwrap(),
            source
        );
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }
    let source="def f(value, extra):\n    \"\"\"Summary.\n\n    :param value: preserved\n    :type value: float\n    \"\"\"\n    return value\n";
    let (_root, mut app) = app(source, true);
    app.tabs[0].state.go_to_line(1, Some(5));
    app.handle_action(Action::GenerateDocstring, &egui::Context::default());
    let text = app.tabs[0].state.doc.text();
    assert!(text.contains(":param value: preserved"), "{text}");
    assert!(text.contains(":type value: float"), "{text}");
    assert!(text.contains(":param extra:"), "{text}");
    assert!(text.contains(":type extra:"), "{text}");
    assert_eq!(text.matches(":type value:").count(), 1);
}
#[test]
fn actual_enter_docstring_consumer_reads_type_flag_for_both_setting_values() {
    for enabled in [false, true] {
        let source = "def f(value):\n    \"\"\"";
        let (_root, mut app) = app(source, enabled);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        app.tabs[0].state.move_doc_end(false);
        frame(&mut app, &ctx, 0.0, vec![]);
        frame(
            &mut app,
            &ctx,
            0.1,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let text = app.tabs[0].state.doc.text();
        assert!(text.contains(":param value:"), "{text}");
        assert_eq!(text.contains(":type value:"), enabled, "{text}");
    }
}
#[test]
fn generate_docstring_reads_independent_return_type_option_and_preserves_disk() {
    let source = "def f(value: int) -> str:\n    return str(value)\n";
    let (root, mut app) = app(source, false);
    app.settings.editor.insert_rtype_in_docstring_stub = true;
    app.tabs[0].state.go_to_line(1, Some(5));
    app.handle_action(Action::GenerateDocstring, &egui::Context::default());
    let text = app.tabs[0].state.doc.text();
    assert!(text.contains(":rtype: str"), "{text}");
    assert!(!text.contains(":type value:"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("fixture.py")).unwrap(),
        source
    );
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), source);
}
#[test]
fn app_paste_uses_resolved_file_indent_and_code_style_before_single_apply() {
    let source = "def f():\n  pass\n";
    let (_root, mut app) = app(source, false);
    app.settings.editor.reformat_on_paste = rustpy_settings::ReformatOnPaste::Reformat;
    app.settings.code_style.quote_style = "double".into();
    app.tabs[0].state.indent_override = Some(rustpy_text::util::IndentStyle::Spaces(2));
    app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
        TextSize::new(11),
        TextSize::new(15),
    )]);
    let options = app.edit_options_for(0);
    assert_eq!(options.indent_size, 2);
    assert_eq!(options.paste_format_options.indent, 2);
    assert_eq!(
        options.paste_format_options.quote,
        rustpy_format::QuoteStyle::Double
    );
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(
        &mut app,
        &ctx,
        0.1,
        vec![egui::Event::Paste("value='x'".into())],
    );
    assert_eq!(app.tabs[0].state.doc.text(), "def f():\n  value = \"x\"\n");
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert!(app.tabs[0].state.redo());
    assert_eq!(app.tabs[0].state.doc.text(), "def f():\n  value = \"x\"\n");
}
