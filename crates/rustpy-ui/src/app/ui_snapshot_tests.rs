//! Golden UI snapshots of key IDE surfaces, rendered headlessly through the
//! real `IdeApp` drawing code (see `crate::ui_snapshot` and qa/README.md).
//! Goldens live in qa/goldens/ui; they are generated at the verification stage.
use super::settings_ui::SettingsPage;
use super::*;
use crate::theme::UiTheme;
use crate::ui_snapshot::Harness;

/// Status widgets whose content depends on the machine or on background
/// jobs (interpreter discovery, indexing, Git, update checks, memory).
const NONDETERMINISTIC_STATUS: &[&str] = &[
    "task",
    "changes",
    "git",
    "interpreter",
    "indexing",
    "progress",
    "notifications",
    "memory",
    "updates",
    "collaboration",
];

const PYTHON_SOURCE: &str = r##"import dataclasses
from typing import Iterable

# Totals are rounded to cents.
@dataclasses.dataclass
class Invoice:
    """A customer invoice."""

    number: int
    lines: list[float]

    def total(self, discount: float = 0.0) -> float:
        amount = sum(self.lines) * (1 - discount)
        return round(amount, 2)


def describe(invoices: Iterable[Invoice]) -> str:
    return ", ".join(f"#{i.number}: {i.total():.2f}" for i in invoices)
"##;

fn settings() -> Settings {
    let mut settings = Settings::default();
    settings.chrome.hidden_status_widgets = NONDETERMINISTIC_STATUS
        .iter()
        .map(|id| id.to_string())
        .collect();
    // External libraries come from the machine's interpreter.
    settings.editor.project_view.show_libraries = false;
    settings
}

/// A small fixed project in a temporary directory named `snapshot_project`.
struct ProjectFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn project_fixture() -> ProjectFixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("snapshot_project");
    std::fs::create_dir_all(root.join("pkg")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(root.join("main.py"), PYTHON_SOURCE).unwrap();
    std::fs::write(root.join("pkg/__init__.py"), "").unwrap();
    std::fs::write(root.join("pkg/util.py"), "def helper():\n    return 1\n").unwrap();
    std::fs::write(
        root.join("tests/test_util.py"),
        "from pkg.util import helper\n\n\ndef test_helper():\n    assert helper() == 1\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "# Snapshot project\n").unwrap();
    std::fs::write(root.join("requirements.txt"), "requests\n").unwrap();
    ProjectFixture { _dir: dir, root }
}

/// The IDE on the fixture project with `main.py` open, dark theme.
fn project_app(fixture: &ProjectFixture) -> IdeApp {
    let mut settings = settings();
    settings.trust_project(&fixture.root);
    let mut app = IdeApp::new(settings, Some(fixture.root.clone()));
    app.theme = UiTheme::dark();
    app.theme.bind_editor_scheme(&app.scheme);
    if let Some(project) = app.project.as_mut() {
        project.tree.scratches_dir = None;
        project.tree.library_roots.clear();
        project.tree.options.show_libraries = false;
        project.tree.expanded.insert(fixture.root.clone());
        project.tree.expanded.insert(fixture.root.join("pkg"));
    }
    app.open_file(&fixture.root.join("main.py"));
    app
}

/// The IDE without a project (nothing on disk is read or written).
fn bare_app() -> IdeApp {
    let mut app = IdeApp::new(settings(), None);
    app.theme = UiTheme::dark();
    app.theme.bind_editor_scheme(&app.scheme);
    app
}

fn harness(name: &str, size: Vec2, app: &IdeApp, fixture: Option<&ProjectFixture>) -> Harness {
    let mut harness = Harness::new(name, size, &app.theme);
    if let Some(fixture) = fixture {
        if let Ok(canonical) = dunce::canonicalize(&fixture.root) {
            harness.replace(canonical.to_string_lossy(), "$PROJECT");
        }
        harness.replace(fixture.root.to_string_lossy(), "$PROJECT");
    }
    harness
}

#[test]
fn status_bar_on_fixture_project() {
    let fixture = project_fixture();
    let mut app = project_app(&fixture);
    let mut harness = harness("status_bar", Vec2::new(1000.0, 120.0), &app, Some(&fixture));
    harness.assert_snapshot(|ui| {
        egui::Panel::bottom("snapshot-status-bar").show(ui, |ui| app.status_bar(ui));
    });
}

#[test]
fn project_view_on_fixture_project() {
    let fixture = project_fixture();
    let mut app = project_app(&fixture);
    let mut harness = harness(
        "project_view",
        Vec2::new(320.0, 400.0),
        &app,
        Some(&fixture),
    );
    harness.assert_snapshot(|ui| {
        let keyboard = app.editor_keyboard();
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(project) = app.project.as_mut() {
                let _ = project.tree.ui(ui, &app.theme, &app.keymap, keyboard);
            }
        });
    });
}

#[test]
fn editor_with_highlighted_python() {
    let mut app = bare_app();
    app.push_tab(EditorState::from_text(PYTHON_SOURCE, Language::Python));
    let mut harness = harness("editor_python", Vec2::new(900.0, 520.0), &app, None);
    harness.assert_snapshot(|ui| {
        egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
    });
}

#[test]
fn settings_editor_folding_page() {
    let mut app = bare_app();
    app.settings_ui.page = SettingsPage::EditorFolding;
    app.dialog = Dialog::Settings;
    let mut harness = harness(
        "settings_editor_folding",
        Vec2::new(1200.0, 860.0),
        &app,
        None,
    );
    harness.assert_snapshot(|ui| app.dialogs(ui.ctx()));
    assert!(
        matches!(app.dialog, Dialog::Settings),
        "the dialog stays open"
    );
}

#[test]
fn go_to_line_dialog() {
    let mut app = bare_app();
    app.push_tab(EditorState::from_text(PYTHON_SOURCE, Language::Python));
    app.dialog = Dialog::GoToLine("12:5".into());
    let mut harness = harness("dialog_go_to_line", Vec2::new(800.0, 500.0), &app, None);
    harness.assert_snapshot(|ui| app.dialogs(ui.ctx()));
    assert!(matches!(&app.dialog, Dialog::GoToLine(text) if text == "12:5"));
}
