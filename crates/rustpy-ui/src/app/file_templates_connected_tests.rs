//! Real new-file dialog, opened document, and existing linked-field keyboard owner.
use super::*;
fn key(key: egui::Key) -> Vec<egui::Event> {
    vec![
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}
fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000., 720.),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
            app.dialogs(ui.ctx());
        },
    )
    .drop_without_applying_deltas();
}
fn fixture(template: FileTemplate) -> (tempfile::TempDir, IdeApp, egui::Context) {
    let root = tempfile::tempdir().unwrap();
    ft::save_store(
        &ft::project_store_path(root.path()),
        &TemplateStore {
            templates: vec![template],
            includes: vec![],
        },
    )
    .unwrap();
    let mut settings = Settings::default();
    settings.completion.show_as_you_type = false;
    let app = IdeApp::new(settings, Some(root.path().to_owned()));
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    (root, app, ctx)
}
#[test]
fn real_new_dialog_opens_saved_live_content_and_links_unicode_fields_until_end() {
    let mut template = FileTemplate::new(
        "Linked file fixture",
        "py",
        "module",
        "# ${NAME}\n$字段$ = '$字段$'\n$END$",
        "Linked fields",
    );
    template.live_templates = true;
    let (root, mut app, ctx) = fixture(template);
    app.open_new_file_dialog(root.path().to_owned(), "Linked file fixture".into());
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.1, key(egui::Key::Enter));
    let index = app.active.unwrap();
    assert_eq!(app.tabs[index].state.doc.text(), "# module\n = ''\n");
    assert_eq!(
        std::fs::read(root.path().join("module.py")).unwrap(),
        b"# module\n = ''\n"
    );
    assert!(!app.tabs[index].state.doc.is_modified());
    assert!(app.template_session.is_some());
    frame(&mut app, &ctx, 0.15, vec![]);
    assert!(app.tabs[index].view.focused);
    let before = app.tabs[index].state.doc.version();
    frame(&mut app, &ctx, 0.2, vec![egui::Event::Text("π".into())]);
    frame(&mut app, &ctx, 0.3, vec![]);
    assert_eq!(app.tabs[index].state.doc.text(), "# module\nπ = ''\n");
    assert!(app.tabs[index].state.doc.version() > before);
    frame(&mut app, &ctx, 0.4, key(egui::Key::Tab));
    assert!(app.template_session.is_none());
    assert_eq!(app.tabs[index].state.doc.text(), "# module\nπ = 'π'\n");
    assert_eq!(
        app.tabs[index].state.primary().head(),
        app.tabs[index].state.doc.len()
    );
    // Creation itself did not add a synthetic insertion undo step.
    assert_eq!(
        std::fs::read(root.path().join("module.py")).unwrap(),
        b"# module\n = ''\n"
    );
}
#[test]
fn project_live_flag_roundtrips_and_disabled_tokens_remain_literal() {
    let template = FileTemplate::new(
        "Literal fixture",
        "txt",
        "literal",
        "$FIELD$ $END$ ${NAME}",
        "Description persisted",
    );
    let (root, mut app, ctx) = fixture(template.clone());
    let loaded = ft::load_store(&ft::project_store_path(root.path()));
    assert_eq!(loaded.templates[0], template);
    assert!(!loaded.templates[0].live_templates);
    app.open_new_file_dialog(root.path().to_owned(), template.name.clone());
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.1, key(egui::Key::Enter));
    assert_eq!(
        app.tabs[app.active.unwrap()].state.doc.text(),
        "$FIELD$ $END$ literal"
    );
    assert!(app.template_session.is_none());
}
#[test]
fn nested_editorconfig_controls_new_file_formatter_and_project_override() {
    let mut template = FileTemplate::new(
        "Python File",
        "py",
        "module",
        "def f():\n        return 1\n",
        "Project replacement",
    );
    template.reformat = true;
    let (root, mut app, _ctx) = fixture(template);
    let nested = root.path().join("pkg");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(
        root.path().join(".editorconfig"),
        "root=true\n[*.py]\nindent_size=2\nmax_line_length=72\n",
    )
    .unwrap();
    std::fs::write(nested.join(".editorconfig"), "[*.py]\nindent_size=3\n").unwrap();
    let created = app
        .create_from_template(&nested, "Python File", "local")
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(created.path).unwrap(),
        "def f():\n   return 1\n"
    );
    assert_eq!(
        app.template_set()
            .template("Python File")
            .unwrap()
            .description,
        "Project replacement"
    );
    // The entire project's store, including the live flag, remains authoritative.
    app.settings.editor.indent_size = 8;
    let created = app
        .create_from_template(root.path(), "Python File", "root_file")
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(created.path).unwrap(),
        "def f():\n  return 1\n"
    );
}
#[test]
fn a_racing_creator_keeps_exact_bytes_and_failed_dialog_keeps_existing_editor() {
    let root = tempfile::tempdir().unwrap();
    let set = TemplateSet::merged(&TemplateStore::default(), None);
    let path = root.path().join("race.py");
    let result = create_prepared_file(
        FileTemplateRequest {
            set: &set,
            dir: root.path(),
            template: "Python File",
            typed: "race",
        },
        |_| Ok(BTreeMap::new()),
        |path, _| {
            std::fs::write(path, "external π").unwrap();
            Ok("ours".into())
        },
        |_| rustpy_format::FormatOptions::pep8(),
        |_, text| Ok(text.as_bytes().to_vec()),
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), "external π".as_bytes());
    let template = FileTemplate::new("Failed fixture", "py", "race", "x=1\n", "");
    let (project, mut app, ctx) = fixture(template);
    app.open_file(&path);
    let retained_template = rustpy_templates::LiveTemplate::new(
        "retained",
        "",
        "keep $OLD$",
        &[rustpy_templates::TemplateContext::Any],
        "test",
    );
    app.expand_template(&retained_template, TextRange::empty(TextSize::new(0)), None);
    let retained_vars = app.template_session.as_ref().unwrap().vars.clone();
    let retained_owner = app.template_session.as_ref().unwrap().tab_id;
    let index = app.active.unwrap();
    let id = app.tabs[index].state.doc.identity();
    let version = app.tabs[index].state.doc.version();
    std::fs::write(project.path().join("race.py"), "retained").unwrap();
    app.open_new_file_dialog(project.path().to_owned(), "Failed fixture".into());
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.1, key(egui::Key::Enter));
    assert!(matches!(app.dialog, Dialog::NewFromTemplate(_)));
    assert_eq!(app.active, Some(index));
    assert_eq!(app.tabs[index].state.doc.identity(), id);
    assert_eq!(app.tabs[index].state.doc.version(), version);
    assert_eq!(
        std::fs::read(project.path().join("race.py")).unwrap(),
        b"retained"
    );
    assert_eq!(app.template_session.as_ref().unwrap().vars, retained_vars);
    assert_eq!(
        app.template_session.as_ref().unwrap().tab_id,
        retained_owner
    );
}

#[test]
fn real_project_settings_checkbox_and_save_publish_live_flag_with_description() {
    let template = FileTemplate::new(
        "Settings fixture",
        "txt",
        "settings",
        "$FIELD$",
        "Unicode описание",
    );
    let (root, mut app, ctx) = fixture(template);
    app.settings_ui.file_templates.scope = Scope::Project;
    app.settings_ui.file_templates.selected = Some("Settings fixture".into());
    fn page(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400., 1000.),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| app.page_file_templates(ui));
            },
        )
    }
    fn rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.galley.rect.translate(text.pos.to_vec2()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing painted {label}"))
    }
    fn click(app: &mut IdeApp, ctx: &egui::Context, time: f64, point: egui::Pos2) {
        page(
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
        )
        .drop_without_applying_deltas();
        page(
            app,
            ctx,
            time + 0.01,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        )
        .drop_without_applying_deltas();
    }
    let output = page(&mut app, &ctx, 0., vec![]);
    let toggle = rect(&output, "Enable live templates").center();
    output.drop_without_applying_deltas();
    click(&mut app, &ctx, 0.1, toggle);
    assert!(
        app.settings_ui
            .file_templates
            .draft
            .as_ref()
            .unwrap()
            .0
            .live_templates
    );
    let output = page(&mut app, &ctx, 0.2, vec![]);
    let save = rect(&output, "Save").center();
    output.drop_without_applying_deltas();
    click(&mut app, &ctx, 0.3, save);
    let loaded = ft::load_store(&ft::project_store_path(root.path()));
    assert!(loaded.templates[0].live_templates);
    assert_eq!(loaded.templates[0].description, "Unicode описание");
}

// Native transport frames retain the complete InputPlayer input, including
// focused/modifiers/time. Existing event-only fixtures keep their old helper.
fn transport_frame(app: &mut IdeApp, ctx: &egui::Context, raw: egui::RawInput) {
    ctx.run_ui(raw, |ui| {
        app.global_shortcuts(ui);
        egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        app.dialogs(ui.ctx());
    })
    .drop_without_applying_deltas();
}

fn transport_input(time: f64) -> egui::RawInput {
    egui::RawInput {
        time: Some(time),
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1000., 720.),
        )),
        ..Default::default()
    }
}

#[test]
fn native_transport_live_end_after_expression_uses_nested_style_and_separate_final_edit() {
    let mut template = FileTemplate::new(
        "Python File",
        "py",
        "created",
        "def ${NAME}():\n        value = \"$FIELD$\"\n        return \"$FIELD$\"$END$\n",
        "Native live-end regression",
    );
    template.live_templates = true;
    template.reformat = true;
    let (root, mut app, ctx) = fixture(template);
    std::fs::create_dir(root.path().join("pkg")).unwrap();
    std::fs::write(
        root.path().join(".editorconfig"),
        "root=true\n[*.py]\nindent_size=2\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("pkg/.editorconfig"),
        "[*.py]\nindent_size=3\n",
    )
    .unwrap();
    app.open_new_file_dialog(root.path().to_owned(), "Python File".into());
    let mut transport = crate::input_script::InputPlayer::new(
        crate::input_script::parse("key A ctrl\ntypefast pkg/created\nkey Enter\n").unwrap(),
    );
    frame(&mut app, &ctx, 0.0, vec![]);
    let mut time = 0.1;
    while transport.is_active() {
        let mut raw = transport_input(time);
        transport.feed(&mut raw);
        transport_frame(&mut app, &ctx, raw);
        time += 0.1;
    }
    let index = app.active.unwrap();
    let empty = "def created():\n   value = \"\"\n   return \"\"\n";
    assert_eq!(app.tabs[index].state.doc.text(), empty);
    assert_eq!(
        std::fs::read_to_string(root.path().join("pkg/created.py")).unwrap(),
        empty
    );
    frame(&mut app, &ctx, time, vec![]);
    let mut transport = crate::input_script::InputPlayer::new(
        crate::input_script::parse("typefast π\nkey Tab\ntypefast + 1\n").unwrap(),
    );
    while transport.is_active() {
        time += 0.1;
        let mut raw = transport_input(time);
        transport.feed(&mut raw);
        transport_frame(&mut app, &ctx, raw);
    }
    let linked = "def created():\n   value = \"π\"\n   return \"π\"\n";
    let final_text = "def created():\n   value = \"π\"\n   return \"π\"+ 1\n";
    assert_eq!(app.tabs[index].state.doc.text(), final_text);
    assert!(app.template_session.is_none());
    // Undo/Redo are editor keyboard actions, not IdeApp::handle_action arms.
    // Use the same fresh native script transport as the accepted runner.
    let mut transport =
        crate::input_script::InputPlayer::new(crate::input_script::parse("key Z ctrl\n").unwrap());
    while transport.is_active() {
        time += 0.1;
        let mut raw = transport_input(time);
        transport.feed(&mut raw);
        transport_frame(&mut app, &ctx, raw);
    }
    assert_eq!(app.tabs[index].state.doc.text(), linked);
    let mut transport = crate::input_script::InputPlayer::new(
        crate::input_script::parse("key Z ctrl shift\n").unwrap(),
    );
    while transport.is_active() {
        time += 0.1;
        let mut raw = transport_input(time);
        transport.feed(&mut raw);
        transport_frame(&mut app, &ctx, raw);
    }
    assert_eq!(app.tabs[index].state.doc.text(), final_text);
    assert_eq!(
        std::fs::read_to_string(root.path().join("pkg/created.py")).unwrap(),
        empty
    );
}

#[test]
fn live_end_formatting_after_number_and_within_unicode_literal_has_exact_caret() {
    for (body, expected, end) in [
        (
            "def ${NAME}():\n        return 42$END$\n",
            "def created():\n   return 42\n",
            "def created():\n   return 42".len(),
        ),
        (
            "def ${NAME}():\n        return 'π$END$😀'\n",
            "def created():\n   return 'π😀'\n",
            "def created():\n   return 'π".len(),
        ),
    ] {
        let mut template = FileTemplate::new("Python File", "py", "created", body, "");
        template.live_templates = true;
        template.reformat = true;
        let (root, mut app, ctx) = fixture(template);
        std::fs::write(
            root.path().join(".editorconfig"),
            "root=true\n[*.py]\nindent_size=3\n",
        )
        .unwrap();
        app.open_new_file_dialog(root.path().to_owned(), "Python File".into());
        frame(&mut app, &ctx, 0.0, vec![]);
        frame(&mut app, &ctx, 0.1, key(egui::Key::Enter));
        let tab = &app.tabs[app.active.unwrap()];
        assert_eq!(tab.state.doc.text(), expected);
        assert_eq!(u32::from(tab.state.primary().head()) as usize, end);
        assert!(app.template_session.is_none());
        assert!(!tab.state.doc.can_undo());
    }
}
