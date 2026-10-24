//! Actual action/keyboard/View/layout/settings integration for documentation.
use super::*;
use crate::editor::Caret;

fn fixture() -> (tempfile::TempDir, IdeApp, egui::Context, String) {
    let (root, mut app, source) = quick_documentation_ui::tests::fixture();
    app.settings.completion.show_as_you_type = false;
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    app.focus_editor = true;
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.02, vec![]);
    (root, app, ctx, source)
}
fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 650.0))),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
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
#[test]
fn actual_ctrl_q_escape_pin_and_dock_action_preserve_document_and_keyboard_ownership() {
    let (_root, mut app, ctx, source) = fixture();
    let i = app.active.unwrap();
    let version = app.tabs[i].state.doc.version();
    frame(
        &mut app,
        &ctx,
        0.1,
        key(egui::Key::Q, egui::Modifiers::CTRL),
    );
    assert!(app.quick_doc.as_ref().unwrap().signature.contains("first"));
    app.documentation.pinned = true;
    app.tabs[i]
        .state
        .set_carets(vec![Caret::at(TextSize::new(0))]);
    frame(&mut app, &ctx, 0.2, vec![]);
    assert!(app.quick_doc.is_some(), "pin survives caret movement");
    frame(
        &mut app,
        &ctx,
        0.3,
        key(egui::Key::Escape, egui::Modifiers::NONE),
    );
    assert!(app.quick_doc.is_none());
    app.handle_action(Action::ToggleDocumentationView, &ctx);
    assert_eq!(app.bottom, Some(BottomTool::Documentation));
    app.handle_action(Action::QuickDocumentation, &ctx);
    assert!(
        app.quick_doc.is_none(),
        "dock must not claim popup keyboard modality"
    );
    assert_eq!(app.bottom, Some(BottomTool::Documentation));
    ctx.run_ui(Default::default(), |ui| {
        ui.memory_mut(|memory| memory.request_focus(app.tabs[i].view.id));
        assert!(app.parameter_info_keyboard(ui));
    })
    .drop_without_applying_deltas();
    assert_eq!(app.tabs[i].state.doc.text(), source);
    assert_eq!(app.tabs[i].state.doc.version(), version);
    assert_eq!(app.tabs[i].state.doc.undo_label().as_deref(), None);
}
#[test]
fn actual_view_pointer_hover_obeys_delay_disabled_and_modal_owner() {
    let (_root, mut app, ctx, source) = fixture();
    let i = app.active.unwrap();
    let point = app.tabs[i].view.caret_screen_rect.unwrap().center() + Vec2::new(3.0, 0.0);
    frame(&mut app, &ctx, 0.1, vec![egui::Event::PointerMoved(point)]);
    assert!(
        app.tabs[i].view.hovered_offset.is_some(),
        "actual View must resolve pointer"
    );
    assert!(app.quick_doc.is_none());
    frame(&mut app, &ctx, 0.4, vec![]);
    assert!(app.quick_doc.is_none());
    frame(&mut app, &ctx, 0.71, vec![]);
    assert!(app.quick_doc.is_some());
    app.quick_doc = None;
    app.settings.editor.documentation_on_hover = false;
    frame(&mut app, &ctx, 1.5, vec![]);
    assert!(app.quick_doc.is_none());
    app.settings.editor.documentation_on_hover = true;
    app.dialog = Dialog::GoToLine(String::new());
    frame(&mut app, &ctx, 2.5, vec![]);
    assert!(app.quick_doc.is_none());
    app.dialog = Dialog::None;
    frame(&mut app, &ctx, 2.6, vec![]);
    assert!(
        app.quick_doc.is_none(),
        "suppression must reset continuous delay"
    );
    assert_eq!(app.tabs[i].state.doc.text(), source);
}
#[test]
fn documentation_layout_and_general_settings_roundtrip_with_legacy_defaults() {
    let (_root, mut app, _ctx, _source) = fixture();
    app.bottom = Some(BottomTool::Documentation);
    let layout = app.tool_window_layout();
    assert_eq!(layout.bottom_tab, "Documentation");
    app.bottom = None;
    app.restore_tool_window_layout(&layout);
    assert_eq!(app.bottom, Some(BottomTool::Documentation));
    assert!(app.documentation.docked);
    let defaults = Settings::default();
    let legacy: rustpy_settings::EditorSettings = serde_json::from_str("{}").unwrap();
    assert!(legacy.documentation_on_hover);
    assert_eq!(legacy.documentation_delay_ms, 600);
    let mut changed = defaults.clone();
    changed.editor.documentation_on_hover = false;
    changed.editor.documentation_delay_ms = 900;
    assert!(settings_ui::page_modified(
        settings_ui::SettingsPage::EditorGeneral,
        &defaults,
        &changed
    ));
    assert!(!settings_ui::page_modified(
        settings_ui::SettingsPage::Todo,
        &defaults,
        &changed
    ));
    let decoded: Settings =
        serde_json::from_str(&serde_json::to_string(&changed).unwrap()).unwrap();
    assert!(!decoded.editor.documentation_on_hover);
    assert_eq!(decoded.editor.documentation_delay_ms, 900);
}

#[test]
fn pinned_ctrl_q_does_not_consume_search_everywhere_escape_or_lose_documentation() {
    let (root, mut app, ctx, source) = fixture();
    app.handle_action(Action::QuickDocumentation, &ctx);
    app.documentation.pinned = true;
    let signature = app.quick_doc.as_ref().unwrap().signature.clone();
    app.handle_action(Action::SearchEverywhere, &ctx);
    assert!(app.everywhere.visible);
    ctx.run_ui(
        egui::RawInput {
            time: Some(0.1),
            focused: true,
            events: key(egui::Key::Escape, egui::Modifiers::NONE),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 650.0))),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            assert!(
                ui.input(|input| input.key_pressed(egui::Key::Escape)),
                "documentation must leave Escape for the actual owner"
            );
            let events = app
                .everywhere
                .ui(ui.ctx(), &app.theme, root.path(), &app.keymap);
            for event in events {
                app.handle_nav_event(event, ui.ctx());
            }
        },
    )
    .drop_without_applying_deltas();
    assert!(
        !app.everywhere.visible,
        "actual Search Everywhere must receive and close on Escape"
    );
    assert_eq!(app.quick_doc.as_ref().unwrap().signature, signature);
    assert!(app.documentation.pinned);
    assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), source);
}

#[test]
fn pinned_ctrl_q_leaves_escape_for_actual_find_bar_and_preserves_content() {
    let (_root, mut app, ctx, source) = fixture();
    app.handle_action(Action::QuickDocumentation, &ctx);
    app.documentation.pinned = true;
    let signature = app.quick_doc.as_ref().unwrap().signature.clone();
    let i = app.active.unwrap();
    let version = app.tabs[i].state.doc.version();
    app.handle_action(Action::Find, &ctx);
    assert!(app.find.visible);
    frame(&mut app, &ctx, 0.1, vec![]);
    frame(
        &mut app,
        &ctx,
        0.2,
        key(egui::Key::Escape, egui::Modifiers::NONE),
    );
    assert!(
        !app.find.visible,
        "actual editor find_bar must receive Escape and close"
    );
    assert_eq!(app.quick_doc.as_ref().unwrap().signature, signature);
    assert!(app.documentation.pinned);
    assert_eq!(app.tabs[i].state.doc.text(), source);
    assert_eq!(app.tabs[i].state.doc.version(), version);
    assert_eq!(app.tabs[i].state.doc.undo_label().as_deref(), None);
}
