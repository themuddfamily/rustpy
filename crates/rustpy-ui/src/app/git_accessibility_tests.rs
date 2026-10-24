//! Real repository and actual Branches dialog keyboard menu ownership.
//! No destructive Git operation, network access or native screen reader is claimed.
use super::*;
struct Fixture {
    app: IdeApp,
    ctx: egui::Context,
    root: tempfile::TempDir,
    initial: String,
}
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = GitRepo::init(root.path()).unwrap();
    let file = root.path().join("π.py");
    std::fs::write(&file, "π = '😀'\n").unwrap();
    let initial = repo
        .commit(std::slice::from_ref(&file), "Unicode π initial", false)
        .unwrap();
    repo.create_branch("α-local", false).unwrap();
    repo.run_git(&["remote", "add", "origin", root.path().to_str().unwrap()])
        .unwrap();
    repo.run_git(&["update-ref", "refs/remotes/origin/β-remote", &initial])
        .unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&file);
    app.theme = UiTheme::high_contrast();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    Fixture {
        app,
        ctx,
        root,
        initial,
    }
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
fn frame(
    f: &mut Fixture,
    events: Vec<egui::Event>,
    retained_branch_paint: bool,
) -> egui::accesskit::TreeUpdate {
    let output = f.ctx.run_ui(
        egui::RawInput {
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 850.0))),
            ..Default::default()
        },
        |ui| {
            f.app.global_shortcuts(ui);
            if retained_branch_paint {
                // A stale queued paint must not reclaim input from Settings.
                f.app.branches_dialog(ui.ctx());
            } else {
                f.app.dialogs(ui.ctx());
            }
        },
    );
    let update = output.platform_output.accesskit_update.clone().unwrap();
    output.drop_without_applying_deltas();
    update
}
fn tab_to(f: &mut Fixture, target: &str) -> egui::Id {
    for _ in 0..100 {
        let update = frame(f, key(egui::Key::Tab, egui::Modifiers::NONE), false);
        let focused = f.ctx.memory(|memory| memory.focused());
        if update.nodes.iter().any(|(id, node)| {
            node.label().is_some_and(|label| label.trim() == target)
                && focused.is_some_and(|focused| focused.accesskit_id() == *id)
        }) {
            return focused.unwrap();
        }
    }
    panic!("real Tab reaches branch {target}");
}
fn open_menu(f: &mut Fixture) -> egui::accesskit::TreeUpdate {
    frame(
        f,
        key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
        false,
    );
    frame(f, vec![], false)
}
#[test]
fn actual_local_and_remote_branch_menus_are_keyboard_reachable_and_current_owner_guarded() {
    let mut f = fixture();
    f.app.handle_action(Action::GitBranches, &f.ctx);
    frame(&mut f, vec![], false);
    frame(&mut f, vec![], false);
    tab_to(&mut f, "α-local");
    let update = open_menu(&mut f);
    for label in [
        "Checkout",
        "New Branch from 'α-local'…",
        "Compare with current",
    ] {
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some(label)),
            "local menu exposes {label}"
        );
    }
    frame(&mut f, key(egui::Key::Escape, egui::Modifiers::NONE), false);
    assert!(
        matches!(f.app.dialog, Dialog::Branches),
        "Escape closes only the branch menu owner"
    );
    assert!(!egui::Popup::is_any_open(&f.ctx));
    tab_to(&mut f, "origin/β-remote");
    let update = open_menu(&mut f);
    for label in [
        "Checkout",
        "New Branch from 'origin/β-remote'…",
        "Compare with current",
    ] {
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some(label)),
            "remote menu exposes {label}"
        );
    }
    frame(&mut f, key(egui::Key::Escape, egui::Modifiers::NONE), false);
    assert!(
        matches!(f.app.dialog, Dialog::Branches),
        "Escape closes only the branch menu owner"
    );
    assert!(!egui::Popup::is_any_open(&f.ctx));
    tab_to(&mut f, "origin/β-remote");
    f.app.handle_action(Action::Settings, &f.ctx);
    frame(
        &mut f,
        key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
        true,
    );
    assert!(
        !egui::Popup::is_any_open(&f.ctx),
        "retained branch response cannot reclaim Settings input"
    );
    assert!(
        matches!(f.app.dialog, Dialog::Settings),
        "stale Branches paint retains the actual Settings owner"
    );
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("π.py")).unwrap(),
        "π = '😀'\n"
    );
    assert!(!f.app.tabs[f.app.active.unwrap()].state.doc.can_undo());
    assert_eq!(
        f.app
            .project
            .as_ref()
            .unwrap()
            .git
            .as_ref()
            .unwrap()
            .run_git(&["rev-parse", "HEAD"])
            .unwrap()
            .trim(),
        f.initial
    );
}
#[test]
fn actual_branch_replacement_does_not_inherit_focused_target() {
    let mut f = fixture();
    f.app.handle_action(Action::GitBranches, &f.ctx);
    frame(&mut f, vec![], false);
    frame(&mut f, vec![], false);
    tab_to(&mut f, "α-local");
    let repo = f.app.project.as_ref().unwrap().git.as_ref().unwrap();
    repo.run_git(&["branch", "-D", "α-local"]).unwrap();
    repo.create_branch("α-replacement", false).unwrap();
    let update = frame(&mut f, vec![], false);
    assert!(
        !update.nodes.iter().any(|(id, node)| node
            .label()
            .is_some_and(|label| label.trim() == "α-replacement")
            && f.ctx
                .memory(|memory| memory.focused())
                .is_some_and(|focus| focus.accesskit_id() == *id)),
        "same-position replacement needs explicit focus reacquisition"
    );
    frame(
        &mut f,
        key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
        false,
    );
    assert!(!egui::Popup::is_any_open(&f.ctx));
    tab_to(&mut f, "α-replacement");
    assert!(open_menu(&mut f)
        .nodes
        .iter()
        .any(|(_, node)| node.label() == Some("New Branch from 'α-replacement'…")));
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("π.py")).unwrap(),
        "π = '😀'\n"
    );
}

#[test]
fn actual_retained_git_revision_cannot_activate_behind_settings() {
    let mut f = fixture();
    f.app.bottom = Some(BottomTool::Git);
    f.app.git_panel.tab = crate::git_panel::GitTab::Log;
    fn paint(f: &mut Fixture, events: Vec<egui::Event>) -> egui::accesskit::TreeUpdate {
        let output = f.ctx.run_ui(
            egui::RawInput {
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 850.0))),
                ..Default::default()
            },
            |ui| {
                f.app.global_shortcuts(ui);
                f.app.bottom_panel(ui);
                f.app.dialogs(ui.ctx());
            },
        );
        let update = output.platform_output.accesskit_update.clone().unwrap();
        output.drop_without_applying_deltas();
        update
    }
    paint(&mut f, vec![]);
    paint(&mut f, vec![]);
    let mut found = false;
    for _ in 0..100 {
        let update = paint(&mut f, key(egui::Key::Tab, egui::Modifiers::NONE));
        found = update.nodes.iter().any(|(id, node)| {
            node.label()
                .is_some_and(|label| label.starts_with("Revision "))
                && f.ctx
                    .memory(|memory| memory.focused())
                    .is_some_and(|focus| focus.accesskit_id() == *id)
        });
        if found {
            break;
        }
    }
    assert!(found, "real Tab reaches the actual Git revision row");
    let selection = f.app.git_panel.selected_commit.clone();
    let checked = f.app.git_panel.checked.clone();
    f.app.handle_action(Action::Settings, &f.ctx);
    paint(&mut f, key(egui::Key::Enter, egui::Modifiers::NONE));
    assert_eq!(f.app.git_panel.selected_commit, selection);
    assert_eq!(f.app.git_panel.checked, checked);
    assert!(matches!(f.app.dialog, Dialog::Settings));
    paint(&mut f, key(egui::Key::Space, egui::Modifiers::NONE));
    assert_eq!(f.app.git_panel.selected_commit, selection);
    assert_eq!(f.app.git_panel.checked, checked);
    assert!(matches!(f.app.dialog, Dialog::Settings));
    assert!(!f.app.tabs[f.app.active.unwrap()].state.doc.can_undo());
}
