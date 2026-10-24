//! Baseline causal assertions use only the preexisting App::open_preview API.
use super::*;
fn files() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("α.py");
    let b = dir.path().join("β.py");
    std::fs::write(&a, "π = 1\n").unwrap();
    std::fs::write(&b, "β = 2\n").unwrap();
    (dir, a, b)
}
#[test]
fn opening_preview_preserves_pinned_predecessor() {
    let (_dir, a, b) = files();
    let mut app = IdeApp::new(Settings::default(), None);
    app.open_preview(&a);
    let i = app.active.unwrap();
    let id = app.tabs[i].view.id;
    let version = app.tabs[i].state.doc.version();
    app.tabs[i].pinned = true;
    app.open_preview(&b);
    let old = app
        .tabs
        .iter()
        .find(|t| t.view.id == id)
        .expect("pinned preview must remain open");
    assert_eq!(old.state.doc.text(), "π = 1\n");
    assert_eq!(old.state.doc.version(), version);
    assert!(old.pinned);
    assert!(!old.state.doc.can_undo());
    assert_eq!(app.tabs[app.active.unwrap()].state.path.as_ref(), Some(&b));
}
#[test]
fn opening_preview_replaces_only_its_destination_editor_group() {
    let (_dir, a, b) = files();
    let mut app = IdeApp::new(Settings::default(), None);
    app.open_preview(&a);
    let id = app.tabs[app.active.unwrap()].view.id;
    app.push_tab(EditorState::from_text(
        "other group 🙂",
        Language::PlainText,
    ));
    let i = app.active.unwrap();
    app.tabs[i].group = 1;
    app.open_preview(&b);
    let original = app
        .tabs
        .iter()
        .find(|t| t.view.id == id)
        .expect("other group preview must remain open");
    assert_eq!(original.group, 0);
    assert!(original.preview);
    assert_eq!(original.state.doc.text(), "π = 1\n");
    let current = &app.tabs[app.active.unwrap()];
    assert_eq!(current.group, 1);
    assert_eq!(current.state.path.as_ref(), Some(&b));
    assert!(current.preview);
}
#[test]
fn failed_preview_open_preserves_original_editor_identity_history_and_selection() {
    let (dir, a, _) = files();
    let mut app = IdeApp::new(Settings::default(), None);
    app.open_preview(&a);
    let i = app.active.unwrap();
    let id = app.tabs[i].view.id;
    let version = app.tabs[i].state.doc.version();
    let carets = app.tabs[i].state.carets.clone();
    app.open_preview(&dir.path().join("missing.py"));
    assert_eq!(
        app.tabs.len(),
        1,
        "failed open must not consume old preview"
    );
    assert_eq!(app.tabs[app.active.unwrap()].view.id, id);
    assert_eq!(app.tabs[0].state.doc.text(), "π = 1\n");
    assert_eq!(app.tabs[0].state.doc.version(), version);
    assert_eq!(app.tabs[0].state.carets, carets);
    assert!(app.tabs[0].preview);
    assert!(!app.tabs[0].state.doc.can_undo());
    assert_eq!(std::fs::read_to_string(a).unwrap(), "π = 1\n");
}
