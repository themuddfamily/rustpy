//! Real task domains and current unsaved operands, without synthetic worker results.
use super::super::*;
use super::{job::DirectoryJob, DirectoryDiff};
use rustpy_core::{CancellationToken, TaskManager};

fn drain(job: &mut DirectoryJob) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !job.drained() {
        assert!(
            std::time::Instant::now() < deadline,
            "Owned directory task did not acknowledge retirement"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
#[test]
fn separate_app_task_domains_cancel_only_their_own_scan_and_acknowledge_retirement() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("π.py"), "before\n").unwrap();
    std::fs::write(right.path().join("π.py"), "after\n").unwrap();
    let manager = TaskManager::new();
    let cancelled_owner = CancellationToken::new();
    let retained_owner = CancellationToken::new();
    cancelled_owner.cancel();
    let mut cancelled = DirectoryJob::spawn(
        &manager,
        &cancelled_owner,
        left.path().into(),
        right.path().into(),
    );
    let mut retained = DirectoryJob::spawn(
        &manager,
        &retained_owner,
        left.path().into(),
        right.path().into(),
    );
    drain(&mut cancelled);
    drain(&mut retained);
    assert!(!cancelled.take_result().unwrap().unwrap().complete);
    assert!(retained.take_result().unwrap().unwrap().complete);
    assert!(!retained_owner.is_cancelled());
    assert!(manager.running_all().is_empty());
}
#[test]
fn closing_inventory_transfers_exact_cancelled_job_for_managed_drain() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let manager = TaskManager::new();
    let owner = CancellationToken::new();
    let mut dialog =
        DirectoryDiff::with_domain(&manager, &owner, left.path().into(), right.path().into());
    let before = dialog.quit_stamp();
    let mut job = dialog
        .take_job_for_drain()
        .expect("actual retained directory task");
    assert!(job.quit_stamp().ends_with(":true"));
    assert_ne!(dialog.quit_stamp(), before);
    drop(dialog);
    drain(&mut job);
    assert!(!owner.is_cancelled());
    assert!(manager.running_all().is_empty());
}
#[test]
fn directory_file_pair_uses_existing_unsaved_editor_capture_and_preserves_disk_and_history() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let lp = left.path().join("π.py");
    let rp = right.path().join("π.py");
    std::fs::write(&lp, "disk left\n").unwrap();
    std::fs::write(&rp, "disk right\n").unwrap();
    let mut app = IdeApp::new(Settings::default(), None);
    app.open_file(&rp);
    let index = app.active.unwrap();
    app.tabs[index]
        .state
        .insert_text("# live π🙂\n", &crate::editor::EditOptions::default());
    let identity = app.tabs[index].state.doc.identity();
    let version = app.tabs[index].state.doc.version();
    let text = app.tabs[index].state.doc.text();
    let source = app.capture_comparison(Some(&rp), false).unwrap();
    assert_eq!(source.text, text);
    app.compare_captured_file(&source, &lp).unwrap();
    assert!(matches!(&app.dialog, Dialog::Diff(state) if state.diff.changes > 0));
    assert_eq!(app.tabs[index].state.doc.identity(), identity);
    assert_eq!(app.tabs[index].state.doc.version(), version);
    assert_eq!(app.tabs[index].state.doc.text(), text);
    assert!(app.tabs[index].state.doc.can_undo());
    assert_eq!(std::fs::read(&rp).unwrap(), b"disk right\n");
    assert_eq!(std::fs::read(&lp).unwrap(), b"disk left\n");
}
