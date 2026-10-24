//! Real editor events and checked Save consume imported per-file code style.
use super::*;

fn fixture(source: &str, config: &str) -> (tempfile::TempDir, IdeApp, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.py");
    std::fs::write(&path, source).unwrap();
    std::fs::write(root.path().join(".editorconfig"), config).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    settings.editor.detect_indents = false;
    settings.editor.reformat_on_paste = rustpy_settings::ReformatOnPaste::Reformat;
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    (root, app, path)
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
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    )
    .drop_without_applying_deltas();
}
#[test]
fn imported_style_real_paste_reformat_and_save_share_one_policy() {
    let source = "pass\n";
    let (_root, mut app, path) = fixture(source,
        "root=true\n[*.py]\nindent_size=2\nmax_line_length=off\nij_python_blank_lines_around_top_level_classes_functions=1\nend_of_line=crlf\ninsert_final_newline=true\n");
    app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
        TextSize::new(0),
        TextSize::new(4),
    )]);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(
        &mut app,
        &ctx,
        0.1,
        vec![egui::Event::Paste(
            "def a():\n    return 1\ndef b():\n    return 2".into(),
        )],
    );
    let expected = "def a():\n  return 1\n\ndef b():\n  return 2\n";
    assert_eq!(app.tabs[0].state.doc.text(), expected);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert!(app.tabs[0].state.redo());
    assert_eq!(app.tabs[0].state.doc.text(), expected);
    app.settings.code_style.format_on_save = true;
    assert!(app.save_tab(0));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        expected.replace('\n', "\r\n").as_bytes()
    );
}
#[test]
fn imported_margin_off_keeps_long_pasted_calls_on_one_line() {
    let source = "pass\n";
    let (_root, mut app, path) = fixture(source, "root=true\n[*.py]\nmax_line_length=off\n");
    app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
        TextSize::new(0),
        TextSize::new(4),
    )]);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    let pasted =
        "result = some_function(first_argument_that_is_long, second_argument_that_is_long)";
    frame(&mut app, &ctx, 0.1, vec![egui::Event::Paste(pasted.into())]);
    assert_eq!(app.tabs[0].state.doc.text(), format!("{pasted}\n"));
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[test]
fn imported_formatter_disable_keeps_paste_and_save_text_but_standard_save_still_applies() {
    let source = "pass\n";
    let (_root, mut app, path) = fixture(source,
        "root=true\n[*]\nij_formatter_enabled=false\nindent_size=2\nend_of_line=cr\ntrim_trailing_whitespace=true\ninsert_final_newline=true\n");
    app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
        TextSize::new(0),
        TextSize::new(4),
    )]);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(
        &mut app,
        &ctx,
        0.1,
        vec![egui::Event::Paste("value=1+2  ".into())],
    );
    assert_eq!(app.tabs[0].state.doc.text(), "value=1+2  \n");
    app.settings.editor.keep_trailing_spaces_on_caret_line = false;
    app.settings.code_style.format_on_save = true;
    assert!(app.save_tab(0));
    assert_eq!(std::fs::read(&path).unwrap(), b"value=1+2\r");
    let text = app.tabs[0].state.doc.text();
    app.tabs[0].state.doc.set_read_only(true);
    frame(&mut app, &ctx, 0.2, vec![egui::Event::Paste("BAD".into())]);
    assert_eq!(app.tabs[0].state.doc.text(), text);
}
#[test]
fn actual_enter_imports_bom_root_tab_and_margin_off_without_overriding_file_choice() {
    let source = "if True:";
    let (_root, mut app, path) = fixture(source,
        "\u{feff}root=true\n[*]\nindent_style=space\nindent_size=TAB\ntab_width=3\nmax_line_length=OFF\nij_wrap_on_typing=true\n");
    app.tabs[0].state.move_doc_end(false);
    let opts = app.edit_options_for(0);
    assert_eq!(opts.indent_size, 3);
    assert_eq!(opts.right_margin, 0);
    assert!(!opts.wrap_on_typing);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
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
    assert_eq!(app.tabs[0].state.doc.text(), "if True:\n   ");
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[test]
fn actual_enter_tab_size_intent_without_config_tab_width_uses_editor_width() {
    let source = "if True:";
    let (_root, mut app, path) = fixture(
        source,
        "root=true\n[*]\nindent_style=space\nindent_size=TAB\n",
    );
    app.settings.editor.tab_size = 8;
    app.settings.editor.indent_size = 4;
    app.tabs[0].state.move_doc_end(false);
    let opts = app.edit_options_for(0);
    assert_eq!((opts.tab_size, opts.indent_size), (8, 8));
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
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
    assert_eq!(app.tabs[0].state.doc.text(), "if True:\n        ");
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[test]
fn actual_enter_distinguishes_configured_continuation_from_suite_indent() {
    for (source, spaces) in [
        ("def f(", 6),
        ("async def f(", 6),
        ("value = call(", 2),
        ("value = [", 2),
        ("if True:", 2),
    ] {
        let (_root, mut app, path) = fixture(
            source,
            "root=true\n[*]\nindent_size=2\nij_continuation_indent_size=6\n",
        );
        app.tabs[0].state.move_doc_end(false);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
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
        let expected = format!("{source}\n{}", " ".repeat(spaces));
        assert_eq!(app.tabs[0].state.doc.text(), expected, "{source}");
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert!(app.tabs[0].state.redo());
        assert_eq!(app.tabs[0].state.doc.text(), expected);
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
}
#[test]
fn actual_paste_and_save_import_continuation_formatting_in_one_undo_transaction() {
    let source = "pass\n";
    let (_root, mut app, path) = fixture(
        source,
        "root=true\n[*]\nindent_size=2\nij_continuation_indent_size=6\n",
    );
    app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
        TextSize::new(0),
        TextSize::new(4),
    )]);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(
        &mut app,
        &ctx,
        0.1,
        vec![egui::Event::Paste(
            "def f(alpha,beta,):\n    return alpha".into(),
        )],
    );
    let expected = "def f(\n      alpha,\n      beta,\n):\n  return alpha\n";
    assert_eq!(app.tabs[0].state.doc.text(), expected);
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert!(app.tabs[0].state.redo());
    assert_eq!(app.tabs[0].state.doc.text(), expected);
    app.settings.code_style.format_on_save = true;
    assert!(app.save_tab(0));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    app.tabs[0].state.doc.set_read_only(true);
    frame(&mut app, &ctx, 0.2, vec![egui::Event::Paste("BAD".into())]);
    assert_eq!(app.tabs[0].state.doc.text(), expected);
}

#[test]
fn actual_enter_and_tab_keep_smart_alignment_spaces_and_one_undo_per_command() {
    let source = "def f():\n\treturn λ(";
    for smart in [false, true] {
        let config=format!("root=true\n[*]\nindent_style=tab\nindent_size=4\ntab_width=4\nij_smart_tabs={smart}\nij_continuation_indent_size=4\nij_python_use_continuation_indent_for_arguments=true\n");
        let (_root, mut app, path) = fixture(source, &config);
        app.tabs[0].state.move_doc_end(false);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        frame(&mut app, &ctx, 0.0, vec![]);
        let key = |key| {
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]
        };
        frame(&mut app, &ctx, 0.1, key(egui::Key::Enter));
        let prefix = if smart { "\t    " } else { "\t\t" };
        let after_enter = format!("{source}\n{prefix}");
        assert_eq!(app.tabs[0].state.doc.text(), after_enter);
        frame(&mut app, &ctx, 0.2, key(egui::Key::Tab));
        let after_tab = format!("{after_enter}{}", if smart { "    " } else { "\t" });
        assert_eq!(app.tabs[0].state.doc.text(), after_tab);
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), after_enter);
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert!(app.tabs[0].state.redo());
        assert_eq!(app.tabs[0].state.doc.text(), after_enter);
        assert!(app.tabs[0].state.redo());
        assert_eq!(app.tabs[0].state.doc.text(), after_tab);
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
}
#[test]
fn actual_paste_and_save_use_tabbed_builtin_formatting_with_smart_alignment() {
    let source = "pass\n";
    let (_root,mut app,path)=fixture(source,"root=true\n[*]\nindent_style=tab\nindent_size=4\ntab_width=4\nij_smart_tabs=true\nij_continuation_indent_size=4\nij_python_use_continuation_indent_for_arguments=true\n");
    app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
        TextSize::new(0),
        TextSize::new(4),
    )]);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(
        &mut app,
        &ctx,
        0.1,
        vec![egui::Event::Paste(
            "def f():\n    return call(alpha,beta,)".into(),
        )],
    );
    let expected = "def f():\n\treturn call(\n\t    alpha,\n\t    beta,\n\t)\n";
    assert_eq!(app.tabs[0].state.doc.text(), expected);
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert!(app.tabs[0].state.redo());
    assert_eq!(app.tabs[0].state.doc.text(), expected);
    app.settings.code_style.format_on_save = true;
    assert!(app.save_tab(0));
    assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
    app.tabs[0].state.doc.set_read_only(true);
    frame(&mut app, &ctx, 0.2, vec![egui::Event::Paste("BAD".into())]);
    assert_eq!(app.tabs[0].state.doc.text(), expected);
}

#[test]
fn actual_reformat_and_save_honor_imported_comment_tags_and_one_undo() {
    let source = "before=1\n# KEEP π\nuntidy = [ 1,2 ]\n# RESUME π\nafter=2\n";
    for enabled in [false, true] {
        let config=format!("root=true\n[*]\nij_formatter_tags_enabled={enabled}\nij_formatter_off_tag=KEEP π\nij_formatter_on_tag=RESUME π\n");
        let (_root, mut app, path) = fixture(source, &config);
        app.settings.editor.strip_trailing_spaces_on_save = rustpy_settings::StripTrailing::None;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        app.tabs[0].state.move_doc_end(false);
        frame(&mut app, &ctx, 0.0, vec![]);
        frame(
            &mut app,
            &ctx,
            0.1,
            vec![
                egui::Event::ModifiersChanged(egui::Modifiers {
                    ctrl: true,
                    alt: true,
                    ..Default::default()
                }),
                egui::Event::Key {
                    key: egui::Key::L,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        alt: true,
                        ..Default::default()
                    },
                },
            ],
        );
        let protected = if enabled {
            "untidy = [ 1,2 ]"
        } else {
            "untidy = [1, 2]"
        };
        let expected = format!("before = 1\n# KEEP π\n{protected}\n# RESUME π\nafter = 2\n");
        assert_eq!(app.tabs[0].state.doc.text(), expected);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert!(app.tabs[0].state.redo());
        assert_eq!(app.tabs[0].state.doc.text(), expected);
        app.settings.code_style.format_on_save = true;
        assert!(app.save_tab(0));
        assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
    }
}
#[test]
fn actual_paste_keeps_configured_protected_fragment_and_refuses_readonly() {
    let source = "pass\n";
    let (_root,mut app,path)=fixture(source,"root=true\n[*]\nij_formatter_tags_enabled=true\nij_formatter_off_tag=KEEP\nij_formatter_on_tag=RESUME\n");
    app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
        TextSize::new(0),
        TextSize::new(4),
    )]);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(
        &mut app,
        &ctx,
        0.1,
        vec![egui::Event::Paste(
            "before=1\n# KEEP\nuntidy = [ 1,2 ]\n# RESUME\nafter=2".into(),
        )],
    );
    let expected = "before = 1\n# KEEP\nuntidy = [ 1,2 ]\n# RESUME\nafter = 2\n";
    assert_eq!(app.tabs[0].state.doc.text(), expected);
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert!(app.tabs[0].state.redo());
    assert_eq!(app.tabs[0].state.doc.text(), expected);
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    app.tabs[0].state.doc.set_read_only(true);
    frame(&mut app, &ctx, 0.2, vec![egui::Event::Paste("BAD".into())]);
    assert_eq!(app.tabs[0].state.doc.text(), expected);
}
#[test]
fn external_formatter_refuses_configured_regions_before_spawn_and_preserves_dirty_source() {
    let source = "# KEEP\nvalue= 1\n# RESUME\n";
    let (_root,mut app,path)=fixture(source,"root=true\n[*]\nij_formatter_tags_enabled=true\nij_formatter_off_tag=KEEP\nij_formatter_on_tag=RESUME\n");
    app.settings.code_style.formatter = "black".into();
    app.settings.code_style.format_on_save = true;
    let opts = app.edit_options_for(0);
    app.tabs[0].state.insert_text("# dirty\n", &opts);
    let before = app.tabs[0].state.doc.text();
    let version = app.tabs[0].state.doc.version();
    let carets = app.tabs[0].state.carets.clone();
    let error = app
        .prepare_format_edits(0, &app.tabs[0].state.doc, &carets)
        .unwrap_err();
    assert!(
        error.to_string().contains("require the builtin formatter"),
        "{error}"
    );
    assert!(!app.save_tab(0));
    assert_eq!(app.tabs[0].state.doc.text(), before);
    assert_eq!(app.tabs[0].state.doc.version(), version);
    assert_eq!(app.tabs[0].state.carets, carets);
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}
