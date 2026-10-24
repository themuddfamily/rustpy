//! Pending notebook models count as unsaved before automatic document sync.
use super::*;

fn pending_notebook() -> (tempfile::TempDir, IdeApp, String) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pending.ipynb");
    std::fs::write(&path, rustpy_notebook::new_notebook().to_string()).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(directory.path());
    let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
    assert!(app.tabs.is_empty());
    app.open_file(&path);
    let notebook = app.tabs[0].notebook.as_mut().unwrap();
    notebook.set_cell_source(0, "print('pending unsynced cell')".into());
    notebook.set_dirty(true);
    let model = notebook.text();
    assert!(!app.tabs[0].state.doc.is_modified());
    assert!(app.tabs[0].has_unsaved_changes());
    (directory, app, model)
}

#[test]
fn closing_unsynced_notebook_prompts_and_retains_pending_model() {
    let (_directory, mut app, model) = pending_notebook();
    let text = app.tabs[0].state.doc.text();
    let version = app.tabs[0].state.doc.version();
    app.request_close_tab(0);
    assert!(matches!(
        app.dialog,
        Dialog::ConfirmClose {
            tab: 0,
            then_exit: false
        }
    ));
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].state.doc.text(), text);
    assert_eq!(app.tabs[0].state.doc.version(), version);
    assert_eq!(app.tabs[0].notebook.as_ref().unwrap().text(), model);
    assert!(app.tabs[0].notebook.as_ref().unwrap().is_dirty());
}

#[test]
fn close_all_keeps_unsynced_notebook_model() {
    let (_directory, mut app, model) = pending_notebook();
    app.handle_action(Action::CloseAllEditors, &egui::Context::default());
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].notebook.as_ref().unwrap().text(), model);
    assert!(app.tabs[0].has_unsaved_changes());
    assert!(app.status.as_ref().unwrap().0.contains("unsaved changes"));
}
