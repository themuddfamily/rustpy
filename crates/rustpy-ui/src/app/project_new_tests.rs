use super::*;

fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, enter: bool) {
    let events = if enter {
        vec![
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    } else {
        vec![]
    };
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
        |ui| app.dialogs(ui.ctx()),
    )
    .drop_without_applying_deltas();
}

#[test]
fn generation_catalog_retains_project_overrides_disabled_defaults_and_origin_groups() {
    let config = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let mut disabled = FileTemplate::new("TypeScript File", "ts", "script", "disabled", "");
    disabled.enabled = false;
    let custom = FileTemplate::new("Custom π", "txt", "custom", "${NAME} π", "Project custom");
    ft::save_store(
        &ft::project_store_path(project.path()),
        &ft::TemplateStore {
            templates: vec![disabled, custom.clone()],
            includes: vec![],
        },
    )
    .unwrap();
    let set = generation_templates(config.path(), Some(project.path()));
    let groups = template_groups(&set);
    assert!(groups
        .iter()
        .any(|(group, names)| group == "Project Templates" && names.contains(&custom.name)));
    assert!(!groups
        .iter()
        .any(|(_, names)| names.contains(&"TypeScript File".into())));
    for name in [
        "Python Stub",
        "Jupyter Notebook",
        "HTTP Request",
        "HTML File",
        "CSS File",
        "JavaScript File",
        ".editorconfig",
    ] {
        assert!(set.new_menu().contains(&name.to_owned()), "missing {name}");
    }
    let notebook = set
        .templates
        .iter()
        .find(|(template, _)| template.name == "Jupyter Notebook")
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&notebook.0.body).unwrap();
    assert_eq!(json["nbformat"], 4);
    assert_eq!(json["cells"], serde_json::json!([]));
}

#[test]
fn project_new_delegates_real_template_dialog_unicode_creation_and_collision() {
    let root = tempfile::tempdir().unwrap();
    let template = FileTemplate::new("Custom π", "txt", "unicode", "${NAME}: π😀\n", "");
    ft::save_store(
        &ft::project_store_path(root.path()),
        &ft::TemplateStore {
            templates: vec![template.clone()],
            includes: vec![],
        },
    )
    .unwrap();
    let mut app = IdeApp::new(Settings::default(), Some(root.path().to_owned()));
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    app.project_new_template(root.path().to_owned(), template.name.clone());
    frame(&mut app, &ctx, 0., false);
    frame(&mut app, &ctx, 0.1, true);
    let target = root.path().join("unicode.txt");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "unicode: π😀\n");
    assert_eq!(
        app.tabs[app.active.unwrap()].state.path.as_ref(),
        Some(&target)
    );
    let identity = app.tabs[app.active.unwrap()].state.doc.identity();
    app.project_new_template(root.path().to_owned(), template.name);
    frame(&mut app, &ctx, 0.2, false);
    frame(&mut app, &ctx, 0.3, true);
    assert!(matches!(app.dialog, Dialog::NewFromTemplate(_)));
    assert_eq!(app.tabs[app.active.unwrap()].state.doc.identity(), identity);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "unicode: π😀\n");
    app.project_edit_templates();
    assert!(matches!(app.dialog, Dialog::Settings));
    assert_eq!(
        app.settings_ui.page,
        settings_ui::SettingsPage::FileTemplates
    );
    app.project_new_data_source(root.path());
    assert_eq!(app.bottom, Some(BottomTool::Database));
    assert_eq!(
        app.db_panel.new_source_path,
        root.path().join("database.sqlite").display().to_string()
    );
}

#[test]
fn exclusive_python_package_creation_preserves_existing_initializer_and_refuses_escape() {
    let root = tempfile::tempdir().unwrap();
    let initializer = create_python_package(root.path(), "πackage").unwrap();
    assert_eq!(std::fs::read(&initializer).unwrap(), b"");
    std::fs::write(&initializer, "preserved\n").unwrap();
    assert!(create_python_package(root.path(), "πackage").is_err());
    assert_eq!(
        std::fs::read_to_string(&initializer).unwrap(),
        "preserved\n"
    );
    assert!(create_python_package(root.path(), "../escape").is_err());
}

#[test]
#[cfg(unix)] // removes .rustpy while the app holds its lock files, which Windows refuses
fn actual_project_catalog_tracks_successful_save_and_queued_external_template_event() {
    let root = tempfile::tempdir().unwrap();
    let mut app = IdeApp::new(Settings::default(), Some(root.path().to_owned()));
    let names = |app: &IdeApp| {
        app.project
            .as_ref()
            .unwrap()
            .tree
            .new_template_groups_for_test()
            .iter()
            .flat_map(|(_, names)| names.iter().cloned())
            .collect::<Vec<_>>()
    };
    assert!(names(&app).contains(&"TypeScript File".to_owned()));
    let stored = ft::TemplateStore {
        templates: vec![FileTemplate::new(
            "Saved project",
            "txt",
            "saved",
            "saved",
            "",
        )],
        includes: vec![],
    };
    ft::save_store(&ft::project_store_path(root.path()), &stored).unwrap();
    app.refresh_project_new_if_changed();
    assert!(names(&app).contains(&"Saved project".to_owned()));
    // External writer deliberately bypasses save_store's process generation.
    let external = ft::TemplateStore {
        templates: vec![FileTemplate::new(
            "External π",
            "txt",
            "external",
            "external",
            "",
        )],
        includes: vec![],
    };
    let path = ft::project_store_path(root.path());
    std::fs::write(&path, serde_json::to_vec(&external).unwrap()).unwrap();
    let (tx, rx) = crossbeam_channel::bounded(4);
    app.fs_events = Some(rx);
    tx.send(Ok(notify::Event::new(notify::EventKind::Modify(
        notify::event::ModifyKind::Data(notify::event::DataChange::Content),
    ))
    .add_path(path)))
        .unwrap();
    app.poll_fs_events(1.0);
    assert!(app.fs_events.as_ref().unwrap().is_empty());
    assert!(names(&app).contains(&"External π".to_owned()));
    assert!(!names(&app).contains(&"Saved project".to_owned()));
    // A watcher may emit only the removed parent, which is no longer a directory.
    let parent = root.path().join(".rustpy");
    std::fs::remove_dir_all(&parent).unwrap();
    tx.send(Ok(notify::Event::new(notify::EventKind::Remove(
        notify::event::RemoveKind::Folder,
    ))
    .add_path(parent)))
        .unwrap();
    app.poll_fs_events(2.0);
    assert!(app.fs_events.as_ref().unwrap().is_empty());
    assert!(!names(&app).contains(&"External π".to_owned()));
    assert!(names(&app).contains(&"TypeScript File".to_owned()));
}

#[test]
fn actual_new_dialog_creates_typescript_notebook_and_http_with_matching_openers() {
    for (template, file, expected, language) in [
        ("TypeScript File", "script.ts", "// script\n\n", Language::JavaScript),
        ("Jupyter Notebook", "notebook.ipynb", "{\n \"cells\": [],\n \"metadata\": {},\n \"nbformat\": 4,\n \"nbformat_minor\": 5\n}\n", Language::Json),
        ("HTTP Request", "requests.http", "### Request\nGET https://example.com/\n", Language::Http),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(root.path().to_owned()));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        app.project_new_template(root.path().to_owned(), template.to_owned());
        let Dialog::NewFromTemplate(state) = &app.dialog else { panic!("template dialog not opened") };
        assert_eq!(state.template, template);
        frame(&mut app, &ctx, 0., false);
        frame(&mut app, &ctx, 0.1, true);
        let target = root.path().join(file);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), expected);
        let tab = &app.tabs[app.active.unwrap()];
        assert_eq!(tab.state.path.as_ref(), Some(&target));
        assert_eq!(tab.state.doc.text(), expected);
        assert_eq!(tab.state.language, language);
        assert!(!tab.has_unsaved_changes());
        if template == "Jupyter Notebook" {
            let json: serde_json::Value = serde_json::from_str(expected).unwrap();
            assert_eq!(json["nbformat"], 4);
            let notebook = tab.notebook.as_ref().expect("actual notebook opener, not JSON fallback");
            assert!(notebook.model().cells.is_empty());
            assert!(!notebook.is_dirty());
        } else {
            assert!(tab.notebook.is_none());
        }
    }
}

#[test]
fn default_http_section_header_is_literal_not_a_velocity_comment() {
    let config = tempfile::tempdir().unwrap();
    let set = generation_templates(config.path(), None);
    let template = set
        .templates
        .iter()
        .find(|(template, _)| template.name == "HTTP Request")
        .unwrap();
    assert_eq!(
        set.try_render(&template.0, &std::collections::BTreeMap::new())
            .unwrap(),
        "### Request\nGET https://example.com/\n"
    );
}
