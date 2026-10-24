//! Real disk-backed Python stub editing through App shortcuts and EditorView.
use super::*;

fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            focused: true,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 720.0),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| app.group_area(ui, 0));
        },
    )
    .drop_without_applying_deltas();
}

fn command(key: egui::Key, shift: bool) -> Vec<egui::Event> {
    let modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        shift,
        ..Default::default()
    };
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

#[test]
fn actual_disk_stub_text_save_undo_save_redo_save_is_one_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("types π.pyi");
    let original = "def π(value: str) -> str: ...\n";
    let inserted = "# π🙂 stub\n";
    let edited = format!("{inserted}{original}");
    std::fs::write(&path, original.as_bytes()).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(directory.path());
    settings.completion.show_as_you_type = false;
    settings.code_style.format_on_save = false;
    settings.code_style.optimize_imports_on_save = false;
    let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
    app.open_file(&path);
    let index = app.active.unwrap();
    assert_eq!(app.tabs[index].state.path.as_deref(), Some(path.as_path()));
    assert_eq!(app.tabs[index].state.language, Language::Python);
    assert!(!app.tabs[index].state.doc.is_read_only());
    assert!(app.tabs[index].image.is_none());
    assert!(app.tabs[index].notebook.is_none());
    assert!(!app.tabs[index].state.doc.can_undo());
    let identity = app.tabs[index].state.doc.identity();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.1, vec![]);
    assert!(app.tabs[index].view.focused);
    assert_eq!(
        app.tabs[index].state.primary().head(),
        rustpy_core::TextSize::new(0)
    );

    // One actual EditorView Text event, not a direct model edit.
    frame(
        &mut app,
        &ctx,
        0.2,
        vec![egui::Event::Text(inserted.into())],
    );
    assert_eq!(app.tabs[index].state.doc.text(), edited);
    assert!(app.tabs[index].state.doc.is_modified());
    assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
    frame(&mut app, &ctx, 0.3, command(egui::Key::S, false));
    assert_eq!(std::fs::read(&path).unwrap(), edited.as_bytes());
    assert!(!app.tabs[index].state.doc.is_modified());
    assert!(app.tabs[index].state.doc.can_undo());

    // Fresh-frame shortcut transport preserves App's per-frame action guard.
    frame(&mut app, &ctx, 0.4, command(egui::Key::Z, false));
    assert_eq!(app.tabs[index].state.doc.text(), original);
    assert!(
        !app.tabs[index].state.doc.can_undo(),
        "one Text event is one history transaction"
    );
    assert!(app.tabs[index].state.doc.can_redo());
    assert_eq!(std::fs::read(&path).unwrap(), edited.as_bytes());
    frame(&mut app, &ctx, 0.5, command(egui::Key::S, false));
    assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
    assert!(!app.tabs[index].state.doc.is_modified());
    assert!(app.tabs[index].state.doc.can_redo());

    frame(&mut app, &ctx, 0.6, command(egui::Key::Z, true));
    assert_eq!(app.tabs[index].state.doc.text(), edited);
    assert!(app.tabs[index].state.doc.can_undo());
    assert!(!app.tabs[index].state.doc.can_redo());
    assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
    frame(&mut app, &ctx, 0.7, command(egui::Key::S, false));
    assert_eq!(std::fs::read(&path).unwrap(), edited.as_bytes());
    assert!(!app.tabs[index].state.doc.is_modified());
    assert_eq!(app.tabs[index].state.doc.identity(), identity);
    assert_eq!(app.tabs[index].state.path.as_deref(), Some(path.as_path()));
    assert_eq!(app.tabs[index].state.language, Language::Python);
    assert!(!app.tabs[index].state.doc.is_read_only());
    assert!(matches!(app.dialog, Dialog::None));
}
