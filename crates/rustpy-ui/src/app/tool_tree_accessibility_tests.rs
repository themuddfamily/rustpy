//! Real semantic hierarchy and unittest data through actual App key owners.
use super::*;
use crate::editor::Caret;
use std::time::{Duration, Instant};
struct Fixture {
    app: IdeApp,
    ctx: egui::Context,
    root: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.app.test_panel.stop();
    }
}
fn fixture(source: &str) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("π.py");
    std::fs::write(&path, source).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&path);
    app.theme = UiTheme::high_contrast();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    Fixture { app, ctx, root }
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
fn frame(f: &mut Fixture, events: Vec<egui::Event>) -> egui::accesskit::TreeUpdate {
    f.app.test_panel.poll();
    let time = f.ctx.input(|input| input.time) + 0.1;
    let output = f.ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 800.0))),
            ..Default::default()
        },
        |ui| {
            f.app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| f.app.bottom_panel(ui));
            f.app.dialogs(ui.ctx());
        },
    );
    let update = output.platform_output.accesskit_update.clone().unwrap();
    output.drop_without_applying_deltas();
    update
}
fn tab_to(f: &mut Fixture, prefix: &str) -> egui::accesskit::NodeId {
    for _ in 0..80 {
        let update = frame(f, key(egui::Key::Tab, egui::Modifiers::NONE));
        if let Some((id, _)) = update.nodes.iter().find(|(id, node)| {
            node.role() == egui::accesskit::Role::TreeItem
                && node.label().is_some_and(|text| text.starts_with(prefix))
                && f.ctx
                    .memory(|memory| memory.focused())
                    .is_some_and(|focus| focus.accesskit_id() == *id)
        }) {
            return *id;
        }
    }
    panic!("actual Tab reaches named runtime tree row {prefix}");
}
#[test]
fn actual_semantic_hierarchy_keyboard_expansion_navigation_and_modal_owner() {
    let source = "class Base: pass\nclass Derived(Base): pass\n";
    let mut f = fixture(source);
    f.app.tabs[0].state.set_carets(vec![Caret::at(TextSize::new(
        source.find("Derived").unwrap() as u32,
    ))]);
    f.app.show_hierarchy(HierarchyKind::Supertypes);
    assert_eq!(f.app.bottom, Some(BottomTool::Hierarchy));
    assert!(
        !f.app.hierarchy_panel.roots[0].children.is_empty(),
        "actual semantic base class"
    );
    frame(&mut f, vec![]);
    frame(&mut f, vec![]);
    let root = tab_to(&mut f, "Derived.");
    frame(&mut f, key(egui::Key::ArrowLeft, egui::Modifiers::NONE));
    let collapsed = frame(&mut f, vec![]);
    assert!(collapsed
        .nodes
        .iter()
        .any(|(id, node)| *id == root && node.is_expanded() == Some(false)));
    frame(&mut f, key(egui::Key::ArrowRight, egui::Modifiers::NONE));
    let expanded = frame(&mut f, vec![]);
    assert!(expanded
        .nodes
        .iter()
        .any(|(id, node)| *id == root && node.is_expanded() == Some(true)));
    tab_to(&mut f, "Base.");
    frame(&mut f, key(egui::Key::Enter, egui::Modifiers::NONE));
    assert_eq!(
        f.app.tabs[0].state.primary().range(),
        TextRange::new(
            TextSize::new(source.find("Base").unwrap() as u32),
            TextSize::new(source.find("Base").unwrap() as u32 + 4)
        )
    );
    tab_to(&mut f, "Derived.");
    let before = f.app.hierarchy_panel.collapsed.clone();
    f.app.handle_action(Action::Settings, &f.ctx);
    frame(&mut f, key(egui::Key::ArrowLeft, egui::Modifiers::NONE));
    assert_eq!(
        f.app.hierarchy_panel.collapsed, before,
        "Settings owns arrows before hierarchy paint"
    );
    assert_eq!(f.app.tabs[0].state.doc.text(), source);
    assert!(!f.app.tabs[0].state.doc.can_undo());
}
#[test]
fn actual_unittest_tree_keyboard_expansion_and_run_debug_source_menu() {
    let Some(python) = ["python3", "python"]
        .into_iter()
        .find_map(|name| which::which(name).ok())
    else {
        return;
    };
    let mut f = fixture("π = 1\n");
    std::fs::write(f.root.path().join("test_unicode.py"),
        "import unittest\nclass TestUnicode(unittest.TestCase):\n    def test_ok(self):\n        self.assertEqual('π😀', 'π😀')\n").unwrap();
    f.app
        .test_panel
        .start(
            rustpy_test::Framework::Unittest,
            &python,
            f.root.path(),
            vec![],
            vec![],
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(15);
    while f.app.test_panel.is_running() {
        f.app.test_panel.poll();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    f.app.test_panel.poll();
    f.app.bottom = Some(BottomTool::Tests);
    frame(&mut f, vec![]);
    frame(&mut f, vec![]);
    let row = tab_to(&mut f, "TestUnicode.");
    frame(&mut f, key(egui::Key::ArrowLeft, egui::Modifiers::NONE));
    let collapsed = frame(&mut f, vec![]);
    assert!(collapsed
        .nodes
        .iter()
        .any(|(id, node)| *id == row && node.is_expanded() == Some(false)));
    frame(&mut f, key(egui::Key::ArrowRight, egui::Modifiers::NONE));
    let expanded = frame(&mut f, vec![]);
    assert!(expanded
        .nodes
        .iter()
        .any(|(id, node)| *id == row && node.is_expanded() == Some(true)));
    frame(&mut f, key(egui::Key::F10, egui::Modifiers::SHIFT));
    let menu = frame(&mut f, vec![]);
    assert!(egui::Popup::is_any_open(&f.ctx));
    for label in [
        "▶ Run 'TestUnicode'",
        "🐞 Debug 'TestUnicode'",
        "Jump to Source",
    ] {
        assert!(
            menu.nodes
                .iter()
                .any(|(_, node)| node.label() == Some(label)),
            "{label}"
        );
    }
    frame(&mut f, key(egui::Key::Escape, egui::Modifiers::NONE));
    assert!(!egui::Popup::is_any_open(&f.ctx));
    assert_eq!(f.app.tabs[0].state.doc.text(), "π = 1\n");
    assert!(!f.app.tabs[0].state.doc.can_undo());
}
