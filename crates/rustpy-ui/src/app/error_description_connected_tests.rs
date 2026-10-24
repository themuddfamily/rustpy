//! Default keymap dispatch through the actual App and editor view.
use super::*;
use rustpy_core::{Diagnostic, Severity};

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
fn frame(app: &mut IdeApp, ctx: &egui::Context, events: Vec<egui::Event>) -> Vec<String> {
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 800.0),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
            app.error_description_ui(ui.ctx());
        },
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_string()),
            _ => None,
        })
        .collect();
    output.drop_without_applying_deltas();
    labels
}
#[test]
fn default_shortcuts_show_description_and_navigate_configured_severity_without_edits() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("errors.py");
    let original = "π = 1\nvalue = 2\nother = 3\n";
    std::fs::write(&path, original).unwrap();
    let mut app = IdeApp::new(Settings::default(), None);
    app.open_file(&path);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, vec![]);
    assert!(app.tabs[0].view.focused);
    let range = |start, end| TextRange::new(TextSize::new(start), TextSize::new(end));
    app.tabs[0].state.extra_diagnostics = vec![
        Diagnostic::warning(range(17, 22), "later", "later warning"),
        Diagnostic::error(range(7, 12), "error", "connected actual error"),
        Diagnostic::new(range(4, 5), Severity::WeakWarning, "weak", "weak warning"),
        Diagnostic::new(range(0, 2), Severity::Info, "info", "information"),
    ];
    app.tabs[0].state.semantic_version = Some(app.tabs[0].state.doc.version());
    let position = app.tabs[0].state.doc.offset_to_position(TextSize::new(0));
    app.tabs[0].state.move_primary_to(position);
    frame(&mut app, &ctx, key(egui::Key::F2, egui::Modifiers::NONE));
    assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(4));
    frame(&mut app, &ctx, key(egui::Key::F2, egui::Modifiers::NONE));
    assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(7));
    let ctrl = egui::Modifiers {
        ctrl: true,
        command: true,
        ..egui::Modifiers::NONE
    };
    frame(&mut app, &ctx, key(egui::Key::F1, ctrl));
    let labels = frame(&mut app, &ctx, vec![]);
    assert!(labels.iter().any(|label| label == "Error Description"));
    assert!(labels.iter().any(|label| label == "connected actual error"));
    frame(
        &mut app,
        &ctx,
        key(egui::Key::Escape, egui::Modifiers::NONE),
    );
    frame(&mut app, &ctx, vec![]);
    frame(&mut app, &ctx, key(egui::Key::F2, egui::Modifiers::SHIFT));
    assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(4));
    app.settings.editor.error_navigation_errors_only = true;
    frame(&mut app, &ctx, key(egui::Key::F2, egui::Modifiers::NONE));
    assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(7));
    frame(&mut app, &ctx, key(egui::Key::F2, egui::Modifiers::SHIFT));
    assert_eq!(app.tabs[0].state.primary().head(), TextSize::new(7));
    assert_eq!(app.tabs[0].state.doc.text(), original);
    assert!(!app.tabs[0].state.doc.can_undo());
    assert!(!app.tabs[0].state.doc.is_modified());
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}
#[test]
fn error_navigation_setting_defaults_roundtrips_and_belongs_to_general_page() {
    let old: Settings = serde_json::from_str(r#"{"editor":{}}"#).unwrap();
    assert!(!old.editor.error_navigation_errors_only);
    let mut new = old.clone();
    new.editor.error_navigation_errors_only = true;
    let restored: Settings = serde_json::from_slice(&serde_json::to_vec(&new).unwrap()).unwrap();
    assert!(restored.editor.error_navigation_errors_only);
    assert!(settings_ui::page_modified(
        settings_ui::SettingsPage::EditorGeneral,
        &old,
        &new
    ));
    assert!(!settings_ui::page_modified(
        settings_ui::SettingsPage::Inspections,
        &old,
        &new
    ));
    assert!(!settings_ui::page_modified(
        settings_ui::SettingsPage::Todo,
        &old,
        &new
    ));
}
