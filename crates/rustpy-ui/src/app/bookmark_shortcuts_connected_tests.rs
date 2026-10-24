//! Actual default-keymap numbered bookmark routes and keyboard ownership.
use super::*;
use rustpy_settings::BookmarkKind;
fn fixture() -> (tempfile::TempDir, IdeApp, egui::Context, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bookmarks.py");
    std::fs::write(&path, "zero = 'π🙂'\none = 1\ntwo = 2\nthree = 3").unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.02, vec![]);
    (root, app, ctx, path)
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
            app.dialogs(ui.ctx());
            let keyboard = app.editor_keyboard();
            egui::Panel::left("bookmark-fixture-project").show(ui, |ui| {
                if let Some(project) = app.project.as_mut() {
                    let _ = project.tree.ui(ui, &app.theme, &app.keymap, keyboard);
                }
            });
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    )
    .drop_without_applying_deltas();
}
fn digit(key: egui::Key) -> Vec<egui::Event> {
    vec![
        egui::Event::ModifiersChanged(egui::Modifiers::CTRL),
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        },
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        },
        egui::Event::ModifiersChanged(egui::Modifiers::NONE),
    ]
}
fn bookmark(path: &Path, line: usize, mnemonic: char, kind: BookmarkKind) -> Bookmark {
    Bookmark {
        path: path.to_path_buf(),
        line,
        mnemonic: Some(mnemonic),
        description: "π🙂 target".into(),
        kind,
        list: "Default".into(),
    }
}
fn line(app: &IdeApp) -> usize {
    let state = &app.tabs[app.active.unwrap()].state;
    state.doc.line_of(state.primary().head())
}
#[test]
fn actual_ctrl_zero_one_nine_distinguish_first_second_line_and_file_same_path() {
    let (_root, mut app, ctx, path) = fixture();
    app.bookmarks = vec![
        bookmark(&path, 1, '0', BookmarkKind::Line),
        bookmark(&path, 2, '1', BookmarkKind::Line),
        bookmark(&path, 0, '9', BookmarkKind::File),
    ];
    let i = app.active.unwrap();
    app.tabs[i].state.go_to_line(3, Some(2));
    let original = app.tabs[i].state.doc.text();
    let version = app.tabs[i].state.doc.version();
    frame(&mut app, &ctx, 0.04, digit(egui::Key::Num0));
    assert_eq!(
        line(&app),
        0,
        "Ctrl0 must navigate first-line bookmark rather than reset zoom"
    );
    frame(&mut app, &ctx, 0.06, digit(egui::Key::Num1));
    assert_eq!(line(&app), 1);
    app.tabs[i].state.go_to_line(3, Some(2));
    let caret = app.tabs[i].state.primary();
    frame(&mut app, &ctx, 0.08, digit(egui::Key::Num9));
    assert_eq!(
        app.tabs[i].state.primary(),
        caret,
        "file bookmark has no invented line-zero target"
    );
    assert_eq!(app.tabs[i].state.doc.text(), original);
    assert_eq!(app.tabs[i].state.doc.version(), version);
    assert!(!app.tabs[i].state.doc.can_undo());
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}
#[test]
fn actual_absent_digit_reports_without_movement_and_readonly_bookmark_still_navigates() {
    let (_root, mut app, ctx, path) = fixture();
    let i = app.active.unwrap();
    app.tabs[i].state.go_to_line(3, Some(1));
    let caret = app.tabs[i].state.primary();
    frame(&mut app, &ctx, 0.04, digit(egui::Key::Num9));
    assert_eq!(app.tabs[i].state.primary(), caret);
    assert!(app.status.as_ref().is_some_and(
        |(message, _)| message.contains('9') && message.to_lowercase().contains("bookmark")
    ));
    app.bookmarks
        .push(bookmark(&path, 2, '1', BookmarkKind::Line));
    app.tabs[i].state.doc.set_read_only(true);
    let source = app.tabs[i].state.doc.text();
    frame(&mut app, &ctx, 0.06, digit(egui::Key::Num1));
    assert_eq!(line(&app), 1);
    assert!(app.tabs[i].state.doc.is_read_only());
    assert!(!app.tabs[i].state.take_read_only_refusal());
    assert_eq!(app.tabs[i].state.doc.text(), source);
    assert!(!app.tabs[i].state.doc.can_undo());
}
#[test]
fn actual_numbered_keys_do_not_steal_modal_find_tree_or_global_popup_ownership() {
    for owner in ["modal", "find", "tree", "popup"] {
        let (_root, mut app, ctx, path) = fixture();
        app.bookmarks
            .push(bookmark(&path, 1, '0', BookmarkKind::Line));
        let i = app.active.unwrap();
        app.tabs[i].state.go_to_line(3, Some(2));
        let caret = app.tabs[i].state.primary();
        match owner {
            "modal" => app.dialog = Dialog::GoToLine(String::new()),
            "find" => app.handle_action(Action::Find, &ctx),
            "tree" => {
                app.project.as_mut().unwrap().tree.request_focus();
                app.focus_editor = false;
            }
            "popup" => app.everywhere.visible = true,
            _ => unreachable!(),
        }
        frame(&mut app, &ctx, 0.04, vec![]);
        frame(&mut app, &ctx, 0.06, digit(egui::Key::Num0));
        assert_eq!(app.tabs[i].state.primary(), caret, "{owner} owns Ctrl0");
        assert!(!app.tabs[i].state.doc.can_undo());
    }
}
