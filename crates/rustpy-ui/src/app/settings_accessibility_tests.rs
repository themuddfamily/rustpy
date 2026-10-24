//! The real Settings dialog in the AccessKit tree: every combo box has a name
//! (Orca read the Appearance combos as just "combo box"), and Tab / Shift+Tab
//! stay inside the dialog instead of wandering into the main window.
use super::*;
use egui::accesskit::{NodeId, Role, TreeUpdate};

struct Fixture {
    app: IdeApp,
    ctx: egui::Context,
    _root: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("main.py");
    std::fs::write(&path, "value = 1\n").unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&path);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut f = Fixture {
        app,
        ctx,
        _root: root,
    };
    frame(&mut f, vec![]);
    f.app.dialog = Dialog::Settings;
    frame(&mut f, vec![]);
    frame(&mut f, vec![]);
    f
}

/// One frame of the main window (editor and tool windows) with the dialogs on top.
fn frame(f: &mut Fixture, events: Vec<egui::Event>) -> TreeUpdate {
    let output = f.ctx.run_ui(
        egui::RawInput {
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300.0, 900.0))),
            ..Default::default()
        },
        |ui| {
            f.app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| {
                f.app.editor_area(ui);
                f.app.bottom_panel(ui);
            });
            f.app.dialogs(ui.ctx());
            f.app.dialog_policy_windows(ui.ctx());
            f.app.interpreter_management_frame(ui.ctx());
        },
    );
    let update = output.platform_output.accesskit_update.clone().unwrap();
    output.drop_without_applying_deltas();
    update
}

fn tab(shift: bool) -> Vec<egui::Event> {
    let modifiers = if shift {
        egui::Modifiers::SHIFT
    } else {
        egui::Modifiers::NONE
    };
    vec![
        egui::Event::ModifiersChanged(modifiers),
        egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: Some(egui::Key::Tab),
            pressed: true,
            repeat: false,
            modifiers,
        },
        egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: Some(egui::Key::Tab),
            pressed: false,
            repeat: false,
            modifiers,
        },
    ]
}

/// The name a screen reader reads for `node`: its label, or the text of the
/// nodes it is labelled by.
fn accessible_name(update: &TreeUpdate, node: &egui::accesskit::Node) -> String {
    if let Some(label) = node.label().filter(|l| !l.trim().is_empty()) {
        return label.to_string();
    }
    let by_id = |id: &NodeId| update.nodes.iter().find(|(n, _)| n == id).map(|(_, n)| n);
    node.labelled_by()
        .iter()
        .filter_map(by_id)
        .filter_map(|label| label.label().or(label.value()).map(str::to_string))
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// `(name, value)` of every combo box in the tree.
fn combo_boxes(update: &TreeUpdate) -> Vec<(String, String)> {
    update
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == Role::ComboBox)
        .map(|(_, n)| {
            (
                accessible_name(update, n),
                n.value().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[test]
fn every_settings_combo_box_has_an_accessible_name() {
    let mut f = fixture();
    let mut unnamed = Vec::new();
    let mut seen = 0;
    for (_, pages) in TREE {
        for (title, page) in *pages {
            f.app.settings_ui.page = *page;
            f.app.dialog = Dialog::Settings;
            frame(&mut f, vec![]);
            f.app.dialog = Dialog::Settings;
            let update = frame(&mut f, vec![]);
            for (name, value) in combo_boxes(&update) {
                seen += 1;
                if name.is_empty() {
                    unnamed.push(format!("{title}: combo box showing {value:?}"));
                }
            }
        }
    }
    assert!(seen > 20, "the Settings pages show combo boxes ({seen})");
    assert!(
        unnamed.is_empty(),
        "Settings combo boxes without an accessible name:\n{}",
        unnamed.join("\n")
    );
}

#[test]
fn appearance_combo_boxes_are_named_by_their_labels() {
    let mut f = fixture();
    f.app.settings_ui.page = SettingsPage::Appearance;
    frame(&mut f, vec![]);
    let names: Vec<String> = combo_boxes(&frame(&mut f, vec![]))
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(
        names.len() >= 2,
        "Appearance shows its combo boxes: {names:?}"
    );
    assert!(
        names.iter().all(|n| !n.is_empty()),
        "every Appearance combo box is named: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.contains("Language")),
        "the UI language combo box is named after its label: {names:?}"
    );
}

/// The focused widget is on the Settings window's layer.
fn focus_in_settings(f: &Fixture) -> Option<egui::Id> {
    let settings = egui::LayerId::new(egui::Order::Middle, egui::Id::new("Settings"));
    let focused = f.ctx.memory(|m| m.focused())?;
    let layer = f.ctx.read_response(focused).map(|r| r.layer_id);
    assert_eq!(
        layer,
        Some(settings),
        "Tab moved the keyboard focus out of the modal Settings dialog"
    );
    Some(focused)
}

#[test]
fn tab_and_shift_tab_cycle_inside_the_settings_dialog() {
    let mut f = fixture();
    // Few pages in the tree, so the cycle wraps around within the test.
    f.app.settings_ui.search = "appearance".into();
    f.app.settings_ui.page = SettingsPage::Appearance;
    frame(&mut f, vec![]);
    frame(&mut f, vec![]);
    for shift in [false, true] {
        let mut order = Vec::new();
        let mut wrapped = false;
        for _ in 0..250 {
            frame(&mut f, tab(shift));
            frame(&mut f, vec![]);
            assert!(
                matches!(f.app.dialog, Dialog::Settings),
                "Tab keeps Settings open"
            );
            let Some(id) = focus_in_settings(&f) else {
                continue;
            };
            if order.first() == Some(&id) && order.len() > 3 {
                wrapped = true;
                break;
            }
            if order.last() != Some(&id) {
                order.push(id);
            }
        }
        assert!(
            wrapped,
            "{} cycles back to the first Settings control ({} visited)",
            if shift { "Shift+Tab" } else { "Tab" },
            order.len()
        );
    }
}

/// A screen reader reads a combo box's current value with its name ("Language,
/// combo box, English"): every Settings combo box carries the text it shows as
/// its AccessKit value.
#[test]
fn settings_combo_boxes_expose_their_current_value() {
    let mut f = fixture();
    f.app.settings.appearance.language = "en".into();
    f.app.settings_ui.page = SettingsPage::Appearance;
    frame(&mut f, vec![]);
    let combos = combo_boxes(&frame(&mut f, vec![]));
    let language = combos
        .iter()
        .find(|(name, _)| name.contains("Language"))
        .unwrap_or_else(|| panic!("the Language combo box is in the tree: {combos:?}"));
    assert_eq!(
        language.1, "English",
        "the UI language combo box speaks its value"
    );
    let mut missing = Vec::new();
    for (_, pages) in TREE {
        for (title, page) in *pages {
            f.app.settings_ui.page = *page;
            f.app.dialog = Dialog::Settings;
            frame(&mut f, vec![]);
            f.app.dialog = Dialog::Settings;
            for (name, value) in combo_boxes(&frame(&mut f, vec![])) {
                if value.trim().is_empty() {
                    missing.push(format!("{title}: {name}"));
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "Settings combo boxes without a current value:\n{}",
        missing.join("\n")
    );
}

/// Opens `dialog`, presses Tab (or Shift+Tab) repeatedly and checks that the
/// keyboard focus never leaves the dialog's window: the dialog is the modal
/// layer, Tab reaches its controls, and closing it releases the trap.
fn assert_tab_stays_in_dialog(f: &mut Fixture, name: &str, open: impl Fn(&mut IdeApp)) {
    for shift in [false, true] {
        open(&mut f.app);
        frame(f, vec![]);
        frame(f, vec![]);
        let layer = f
            .ctx
            .memory(|m| m.top_modal_layer())
            .unwrap_or_else(|| panic!("{name} is shown as a modal dialog"));
        let mut visited = std::collections::HashSet::new();
        for _ in 0..40 {
            frame(f, tab(shift));
            frame(f, vec![]);
            let Some(focused) = f.ctx.memory(|m| m.focused()) else {
                continue;
            };
            let focused_layer = f.ctx.read_response(focused).map(|r| r.layer_id);
            assert_eq!(
                focused_layer,
                Some(layer),
                "{} moved the keyboard focus out of the modal {name} dialog",
                if shift { "Shift+Tab" } else { "Tab" }
            );
            visited.insert(focused);
        }
        assert!(
            !visited.is_empty(),
            "{name}: Tab reaches the dialog's controls"
        );
        f.app.dialog = Dialog::None;
        f.app.dialog_workspace.maintenance = false;
        f.app.interpreter_management.open = false;
        frame(f, vec![]);
        frame(f, vec![]);
        assert_eq!(
            f.ctx.memory(|m| m.top_modal_layer()),
            None,
            "{name}: closing the dialog releases the focus trap"
        );
    }
}

/// Opens one dialog of the app.
type Opener = Box<dyn Fn(&mut IdeApp)>;

/// A representative set of the modal dialogs, each with focusable controls:
/// Tab and Shift+Tab stay inside the dialog instead of reaching the editor or
/// the tool windows behind it.
#[test]
fn tab_and_shift_tab_stay_inside_modal_dialogs() {
    let mut f = fixture();
    f.app.dialog = Dialog::None;
    frame(&mut f, vec![]);
    let root = f._root.path().to_path_buf();
    let dialogs: Vec<(&str, Opener)> = vec![
        (
            "Go to Line",
            Box::new(|app| app.dialog = Dialog::GoToLine(String::new())),
        ),
        (
            "Unsaved Changes",
            Box::new(|app| {
                app.dialog = Dialog::ConfirmClose {
                    tab: 0,
                    then_exit: false,
                }
            }),
        ),
        (
            "Rename file",
            Box::new(move |app| {
                app.dialog = Dialog::RenamePath {
                    path: root.join("main.py"),
                    name: "main.py".into(),
                }
            }),
        ),
        (
            "Notice",
            Box::new(|app| {
                app.dialog = Dialog::Notice {
                    title: "Notice".into(),
                    text: "Something happened".into(),
                    then: None,
                }
            }),
        ),
        ("About", Box::new(|app| app.dialog = Dialog::About)),
        (
            "Save Context",
            Box::new(|app| app.dialog = Dialog::SaveContext(String::new())),
        ),
        ("Settings", Box::new(|app| app.dialog = Dialog::Settings)),
        (
            "Invalidate Caches",
            Box::new(|app| app.dialog_workspace.maintenance = true),
        ),
        (
            "Add Interpreter",
            Box::new(|app| app.interpreter_management.open = true),
        ),
    ];
    for (name, open) in dialogs {
        assert_tab_stays_in_dialog(&mut f, name, open);
    }
}
