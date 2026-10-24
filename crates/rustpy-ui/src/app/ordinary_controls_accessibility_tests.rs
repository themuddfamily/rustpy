//! Finite ordinary-control traversal, using actual App panels and owned files.
//! No kernel, coverage provider, network database or OS screen reader is started.
use super::*;
struct Fixture {
    app: IdeApp,
    ctx: egui::Context,
    root: tempfile::TempDir,
}
fn fixture(notebook: bool) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(if notebook { "π.ipynb" } else { "π.py" });
    if notebook {
        let mut model = rustpy_notebook::Notebook::empty();
        model.insert_cell(0, rustpy_notebook::Cell::markdown("# π title\n"));
        model.save(&path).unwrap();
    } else {
        std::fs::write(&path, "π = 1\n").unwrap();
    }
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&path);
    app.theme = UiTheme::high_contrast();
    app.scheme = rustpy_highlight::ColorScheme::high_contrast();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    Fixture { app, ctx, root }
}
fn key(key: egui::Key) -> Vec<egui::Event> {
    vec![
        egui::Event::ModifiersChanged(egui::Modifiers::NONE),
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
fn frame(f: &mut Fixture, events: Vec<egui::Event>, notebook: bool) -> egui::accesskit::TreeUpdate {
    let output = f.ctx.run_ui(
        egui::RawInput {
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300.0, 850.0))),
            ..Default::default()
        },
        |ui| {
            f.app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| {
                if notebook {
                    f.app.notebook_area(ui, f.app.active.unwrap());
                } else {
                    f.app.bottom_panel(ui);
                }
            });
            f.app.dialogs(ui.ctx());
        },
    );
    let update = output.platform_output.accesskit_update.clone().unwrap();
    output.drop_without_applying_deltas();
    update
}
fn tab_to(f: &mut Fixture, notebook: bool, target: &str) -> egui::Id {
    for _ in 0..100 {
        let update = frame(f, key(egui::Key::Tab), notebook);
        let focused = f.ctx.memory(|memory| memory.focused());
        if update.nodes.iter().any(|(id, node)| {
            (node.label().is_some_and(|label| label.starts_with(target))
                || node.labelled_by().iter().any(|label_id| {
                    update.nodes.iter().any(|(id, label)| {
                        id == label_id && label.value().is_some_and(|value| value == target)
                    })
                }))
                && focused.is_some_and(|focus| focus.accesskit_id() == *id)
        }) {
            return focused.unwrap();
        }
    }
    panic!("real Tab reaches {target}");
}
#[test]
fn actual_rendered_markdown_tab_enter_edits_and_foreign_owner_cannot_reenter() {
    let mut f = fixture(true);
    let original = std::fs::read(f.root.path().join("π.ipynb")).unwrap();
    let index = f.app.active.unwrap();
    frame(&mut f, vec![], true);
    frame(&mut f, vec![], true);
    tab_to(&mut f, true, "Markdown cell 1. Enter edits.");
    frame(&mut f, key(egui::Key::Enter), true);
    frame(&mut f, vec![], true);
    let input = f.app.tabs[index]
        .notebook
        .as_ref()
        .unwrap()
        .cell_editor_id(&f.root.path().join("π.ipynb"), 0);
    assert!(
        f.ctx.memory(|memory| memory.has_focus(input)),
        "Enter transfers to the actual cell TextEdit"
    );
    assert_eq!(
        f.app.tabs[index].notebook.as_ref().unwrap().model().cells[0].source,
        "# π title\n"
    );
    frame(&mut f, vec![egui::Event::Text("😀".into())], true);
    assert!(
        f.app.tabs[index].notebook.as_ref().unwrap().model().cells[0]
            .source
            .contains('😀'),
        "real text reaches edit mode"
    );
    frame(&mut f, key(egui::Key::Escape), true);
    frame(&mut f, vec![], true);
    tab_to(&mut f, true, "Markdown cell 1. Enter edits.");
    let before = f.app.tabs[index].notebook.as_ref().unwrap().model().cells[0]
        .source
        .clone();
    f.app.handle_action(Action::Settings, &f.ctx);
    frame(&mut f, key(egui::Key::Enter), true);
    assert!(
        !f.ctx.memory(|memory| memory.has_focus(input)),
        "Settings retains keyboard authority"
    );
    assert_eq!(
        f.app.tabs[index].notebook.as_ref().unwrap().model().cells[0].source,
        before
    );
    assert_eq!(
        std::fs::read(f.root.path().join("π.ipynb")).unwrap(),
        original
    );
}
#[test]
fn actual_coverage_import_and_cancel_are_reachable_without_starting_provider() {
    let mut f = fixture(false);
    f.app.bottom = Some(BottomTool::Coverage);
    frame(&mut f, vec![], false);
    frame(&mut f, vec![], false);
    tab_to(&mut f, false, "Import…");
    frame(&mut f, key(egui::Key::Enter), false);
    let update = frame(&mut f, vec![], false);
    assert!(update
        .nodes
        .iter()
        .any(|(_, node)| node.value() == Some("Import coverage from:")));
    tab_to(&mut f, false, "Cancel");
    frame(&mut f, key(egui::Key::Enter), false);
    let update = frame(&mut f, vec![], false);
    assert!(!update
        .nodes
        .iter()
        .any(|(_, node)| node.value() == Some("Import coverage from:")));
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("π.py")).unwrap(),
        "π = 1\n"
    );
    assert!(!f.app.tabs[f.app.active.unwrap()].state.doc.can_undo());
}
#[test]
fn actual_database_path_field_and_add_source_are_keyboard_reachable_with_owned_sqlite() {
    let mut f = fixture(false);
    f.app.bottom = Some(BottomTool::Database);
    frame(&mut f, vec![], false);
    frame(&mut f, vec![], false);
    tab_to(&mut f, false, "+ Data source / Properties");
    frame(&mut f, key(egui::Key::Enter), false);
    frame(&mut f, vec![], false);
    tab_to(&mut f, false, "Name");
    // Replace the actual default name using the keyboard.
    let select_all = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    frame(
        &mut f,
        vec![
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers: select_all,
            },
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: false,
                repeat: false,
                modifiers: select_all,
            },
        ],
        false,
    );
    frame(&mut f, vec![egui::Event::Text("π".into())], false);
    tab_to(&mut f, false, "File");
    frame(&mut f, vec![egui::Event::Text("π.sqlite".into())], false);
    tab_to(&mut f, false, "Save properties");
    frame(&mut f, key(egui::Key::Enter), false);
    tab_to(&mut f, false, "Connect / New console");
    frame(&mut f, key(egui::Key::Enter), false);
    // The retained SQLite worker opens the owned file asynchronously.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !f.root.path().join("π.sqlite").exists() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
        frame(&mut f, vec![], false);
    }
    assert!(
        f.root.path().join("π.sqlite").exists(),
        "actual local SQLite open happened"
    );
    assert!(f.app.db_panel.database_has_owners());
    assert!(f
        .app
        .db_panel
        .database_owners()
        .iter()
        .any(|(_, name, _, _)| name.contains('π')));
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("π.py")).unwrap(),
        "π = 1\n"
    );
    assert!(!f.app.tabs[f.app.active.unwrap()].state.doc.can_undo());
}

#[test]
fn actual_editing_notebook_cell_rejects_text_and_paste_under_settings() {
    let mut f = fixture(true);
    let disk = std::fs::read(f.root.path().join("π.ipynb")).unwrap();
    frame(&mut f, vec![], true);
    frame(&mut f, vec![], true);
    tab_to(&mut f, true, "Markdown cell 1. Enter edits.");
    frame(&mut f, key(egui::Key::Enter), true);
    frame(&mut f, vec![], true);
    let input = f.app.tabs[f.app.active.unwrap()]
        .notebook
        .as_ref()
        .unwrap()
        .cell_editor_id(&f.root.path().join("π.ipynb"), 0);
    assert!(f.ctx.memory(|memory| memory.has_focus(input)));
    let index = f.app.active.unwrap();
    let before = f.app.tabs[index].notebook.as_ref().unwrap().model().cells[0]
        .source
        .clone();
    let version = f.app.tabs[index].state.doc.version();
    let undo = f.app.tabs[index].state.doc.can_undo();
    f.app.handle_action(Action::Settings, &f.ctx);
    frame(
        &mut f,
        vec![
            egui::Event::Text("blocked😀".into()),
            egui::Event::Paste("blockedπ".into()),
        ],
        true,
    );
    assert_eq!(
        f.app.tabs[index].notebook.as_ref().unwrap().model().cells[0].source,
        before
    );
    assert_eq!(f.app.tabs[index].state.doc.version(), version);
    assert_eq!(f.app.tabs[index].state.doc.can_undo(), undo);
    assert_eq!(std::fs::read(f.root.path().join("π.ipynb")).unwrap(), disk);
}
