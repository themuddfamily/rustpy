use super::*;
use crate::editor::Caret;

fn app(source: &str, extension: &str) -> (IdeApp, tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(format!("π.{extension}"));
    std::fs::write(&path, source).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    settings.inspections.spelling = false;
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    (app, root, path)
}
fn analyze(app: &mut IdeApp) {
    app.sync_open_tabs_now();
    let inputs = app.analysis_inputs();
    let i = app.active.unwrap();
    let (db, file) = app.analysis_file(i).unwrap();
    let result =
        analysis_jobs::analyze_file(&db, &file, &inputs, &rustpy_core::CancellationToken::new())
            .unwrap();
    let state = &mut app.tabs[i].state;
    state.extra_diagnostics = result.diagnostics;
    state.quick_fixes = result.quick_fixes;
    state.semantic_version = Some(state.doc.version());
}
fn choose(app: &mut IdeApp, prefix: &str) {
    let index = app
        .intentions
        .as_ref()
        .unwrap()
        .0
        .iter()
        .position(|fix| fix.title.starts_with(prefix))
        .unwrap_or_else(|| panic!("missing {prefix}"));
    app.apply_intention(index);
}
fn open(app: &mut IdeApp) {
    app.open_intentions();
    assert!(app.intentions.is_some());
}

#[test]
fn real_alt_enter_frame_groups_current_diagnostics_and_disable_enable_refreshes_results() {
    let (mut app, _root, path) = app("import os\nπ = missing_name\n", "py");
    analyze(&mut app);
    app.tabs[0]
        .state
        .set_carets(vec![Caret::at(TextSize::new(7))]);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.memory_mut(|memory| memory.request_focus(app.tabs[0].view.id));
    ctx.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::ALT,
            }],
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            let (fixes, selected) = app
                .intentions
                .as_ref()
                .expect("real Alt+Enter opens context actions")
                .clone();
            app.draw_intentions(
                ui,
                egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(1.0, 20.0)),
                &fixes,
                selected,
            );
        },
    )
    .drop_without_applying_deltas();
    assert!(app.intention_menu.groups.contains(&Group::QuickFixes));
    assert!(app.intention_menu.groups.contains(&Group::Suppressions));
    assert!(app
        .intention_menu
        .groups
        .contains(&Group::InspectionSettings));
    let fingerprint = app.analysis_inputs().fingerprint();
    choose(&mut app, "Disable inspection: PyUnusedImports");
    assert_ne!(fingerprint, app.analysis_inputs().fingerprint());
    analyze(&mut app);
    assert!(!app.tabs[0]
        .state
        .extra_diagnostics
        .iter()
        .any(|d| d.code == "PyUnusedImports"));
    open(&mut app);
    choose(&mut app, "Enable inspection: PyUnusedImports");
    analyze(&mut app);
    assert!(app.tabs[0]
        .state
        .extra_diagnostics
        .iter()
        .any(|d| d.code == "PyUnusedImports"));
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "import os\nπ = missing_name\n"
    );
    assert!(!app.tabs[0].state.doc.can_undo());
}

#[test]
fn profile_selection_is_typed_and_same_version_replaced_document_refuses_old_menu() {
    let (mut app, _root, _) = app("import os\n", "py");
    analyze(&mut app);
    // Inspection management is contextual: target the import binding rather
    // than relying on the file-opening caret or another diagnostic on its line.
    let import = app.tabs[0].state.doc.text().find("os").unwrap();
    app.tabs[0]
        .state
        .set_carets(vec![Caret::at(TextSize::new(import as u32))]);
    assert!(app.tabs[0]
        .state
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.code == "PyUnusedImports"
            && diagnostic
                .range
                .contains_inclusive(TextSize::new(import as u32))));
    open(&mut app);
    choose(&mut app, "Edit inspection profile setting: PyUnusedImports");
    assert!(matches!(app.dialog, Dialog::Settings));
    assert_eq!(app.settings_ui.page, settings_ui::SettingsPage::Inspections);
    assert_eq!(
        app.settings_ui.inspection_focus.as_deref(),
        Some("PyUnusedImports")
    );
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    // A new egui Window uses an invisible sizing pass before actual painting.
    ctx.run_ui(egui::RawInput::default(), |_ui| app.settings_window(&ctx))
        .drop_without_applying_deltas();
    let mut output = ctx.run_ui(egui::RawInput::default(), |_ui| app.settings_window(&ctx));
    // Assertions still inspect real shapes; failure must not drop unapplied textures.
    output.textures_delta.clear();
    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains("PyUnusedImports"))));
    assert!(!app.settings_ui.inspection_request_focus);
    assert!(ctx.memory(|memory| memory.focused().is_some()));
    output.drop_without_applying_deltas();
    app.dialog = Dialog::None;
    // Rendering Settings refreshes the effective inspection policy and expires
    // semantic results. Complete that analysis before capturing another menu.
    analyze(&mut app);
    open(&mut app);
    let version = app.tabs[0].state.doc.version();
    app.tabs[0].state.doc = rustpy_text::Document::new("import os\n");
    assert_eq!(app.tabs[0].state.doc.version(), version);
    choose(&mut app, "Disable inspection: PyUnusedImports");
    assert!(app.settings.inspections.disabled_inspections.is_empty());
}

#[test]
fn file_suppression_preserves_python_header_unicode_and_is_one_undo() {
    let source = "#!/usr/bin/env python3\n# coding: utf-8\nπ = missing_one\nclass A:\n    def f(self):\n        return missing_two\n";
    let (mut app, _root, path) = app(source, "py");
    analyze(&mut app);
    let offset = source.find("missing_two").unwrap();
    app.tabs[0]
        .state
        .set_carets(vec![Caret::at(TextSize::new(offset as u32))]);
    open(&mut app);
    choose(&mut app, "Suppress 'PyUnresolvedReferences' for file");
    let edited = app.tabs[0].state.doc.text();
    assert!(edited.starts_with(
        "#!/usr/bin/env python3\n# coding: utf-8\n# noinspection file: PyUnresolvedReferences\nπ"
    ));
    analyze(&mut app);
    assert!(!app.tabs[0]
        .state
        .extra_diagnostics
        .iter()
        .any(|d| d.code == "PyUnresolvedReferences"));
    app.tabs[0].state.doc.undo().unwrap();
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert!(!app.tabs[0].state.doc.can_undo());
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[test]
fn readonly_file_suppression_refuses_without_changing_history_or_source() {
    let (mut app, _root, _) = app("π = missing\n", "py");
    analyze(&mut app);
    app.tabs[0]
        .state
        .set_carets(vec![Caret::at(TextSize::new(5))]);
    open(&mut app);
    app.tabs[0].state.doc.set_read_only(true);
    choose(&mut app, "Suppress 'PyUnresolvedReferences' for file");
    assert_eq!(app.tabs[0].state.doc.text(), "π = missing\n");
    assert!(!app.tabs[0].state.doc.can_undo());
}

#[test]
fn exact_disabled_codes_reach_markdown_plain_and_commit_policy_without_text_edits() {
    for extension in ["md", "txt"] {
        let (mut app, _root, path) = app("the the answer happens alot", extension);
        app.last_edit_time = 0.0;
        app.sync_analysis(1.0);
        assert!(!app.tabs[0].state.extra_diagnostics.is_empty());
        app.settings.inspections.disabled_inspections = vec!["Grammar".into()];
        app.invalidate_inspection_profile();
        app.sync_analysis(2.0);
        assert!(app.tabs[0].state.extra_diagnostics.is_empty());
        app.configure_git_grammar();
        app.git_panel.message = "the the".into();
        assert!(app.git_panel.grammar_issues().unwrap().is_empty());
        app.settings.inspections.disabled_inspections.clear();
        app.invalidate_inspection_profile();
        app.sync_analysis(3.0);
        assert!(!app.tabs[0].state.extra_diagnostics.is_empty());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "the the answer happens alot"
        );
        assert!(!app.tabs[0].state.doc.can_undo());
    }
}

#[test]
fn unknown_inspection_refuses_before_starting_task_and_run_modal_cancel_preserves_editor() {
    let (mut app, _root, _) = app("import os\n", "py");
    app.run_selected_inspection("UnknownInspection".into(), true);
    assert!(app
        .status
        .as_ref()
        .unwrap()
        .0
        .contains("Unknown inspection"));
    assert!(!app.bg.problems.show);
    analyze(&mut app);
    // Inspection management is contextual: target the import binding rather
    // than relying on the file-opening caret or another diagnostic on its line.
    let import = app.tabs[0].state.doc.text().find("os").unwrap();
    app.tabs[0]
        .state
        .set_carets(vec![Caret::at(TextSize::new(import as u32))]);
    assert!(app.tabs[0]
        .state
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.code == "PyUnusedImports"
            && diagnostic
                .range
                .contains_inclusive(TextSize::new(import as u32))));
    open(&mut app);
    choose(&mut app, "Run inspection on…: PyUnusedImports");
    assert!(!app.editor_keyboard());
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        },
        |_ui| app.intention_run_dialog(&ctx),
    )
    .drop_without_applying_deltas();
    assert!(!app.intention_menu.dialog_active());
    assert_eq!(app.tabs[0].state.doc.text(), "import os\n");
    assert!(!app.tabs[0].state.doc.can_undo());
}
