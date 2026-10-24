//! Root registers this inside app::directory_diff; Source-only integration fixtures.
use super::super::*;
use super::DirectoryDiff;
use rustpy_core::TaskManager;

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, IdeApp) {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("π🙂.py"), b"left\n").unwrap();
    std::fs::write(right.path().join("π🙂.py"), b"right\n").unwrap();
    let app = IdeApp::new(Settings::default(), None);
    (left, right, app)
}
fn id(stamp: &str) -> &str {
    stamp.split(':').nth(1).unwrap()
}
fn drain(app: &mut IdeApp, ctx: &egui::Context) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !app.directory_diff_draining.is_empty() {
        app.poll_directory_comparison(ctx);
        assert!(
            std::time::Instant::now() < deadline,
            "actual directory result/manager retirement not acknowledged: {:?}",
            app.directory_comparison_stamps()
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
#[test]
fn arbitrary_dialog_replacement_transfers_exact_job_and_observes_real_retirement() {
    let (left, right, mut app) = fixture();
    let manager = TaskManager::new();
    app.directory_comparison = Some(Box::new(DirectoryDiff::with_domain(
        &manager,
        &app.directory_diff_domain,
        left.path().into(),
        right.path().into(),
    )));
    app.dialog = Dialog::DirectoryComparison;
    let stamp = app.directory_comparison.as_ref().unwrap().quit_stamp();
    app.dialog = Dialog::None;
    // Exercise the same transfer leaf as stable-boundary replacement polling.
    app.transfer_directory_comparison();
    assert!(app.directory_comparison.is_none());
    assert_eq!(app.directory_diff_draining.len(), 1);
    assert_eq!(id(&app.directory_diff_draining[0].quit_stamp()), id(&stamp));
    assert!(app.directory_diff_draining[0]
        .quit_stamp()
        .ends_with(":true"));
    drain(&mut app, &egui::Context::default());
    assert!(!app.directory_comparisons_pending());
    assert!(manager.running_all().is_empty());
    assert_eq!(
        std::fs::read(left.path().join("π🙂.py")).unwrap(),
        b"left\n"
    );
}
#[test]
fn transient_dialog_paint_none_retains_job_without_cancellation() {
    let (left, right, mut app) = fixture();
    app.start_directory_comparison(left.path().into(), right.path().into());
    let before = app.directory_comparison.as_ref().unwrap().quit_stamp();
    let dialog = std::mem::replace(&mut app.dialog, Dialog::None);
    // Mirrors dialogs() ownership interval: no stable-boundary poll occurs here.
    assert_eq!(
        id(&app.directory_comparison.as_ref().unwrap().quit_stamp()),
        id(&before)
    );
    assert!(app.directory_diff_draining.is_empty());
    assert!(app
        .directory_comparison
        .as_ref()
        .unwrap()
        .quit_stamp()
        .ends_with(":false"));
    app.dialog = dialog;
    app.poll_directory_comparison(&egui::Context::default());
    assert!(app.directory_comparison.is_some());
    app.cancel_directory_comparisons();
    drain(&mut app, &egui::Context::default());
}
#[test]
fn actual_quit_consent_abort_retains_owned_job_then_project_cancel_drains() {
    let (left, right, mut app) = fixture();
    app.start_directory_comparison(left.path().into(), right.path().into());
    let before = app.directory_comparison.as_ref().unwrap().quit_stamp();
    assert!(matches!(
        app.prepare_managed_coordinated_quit("directory-owner"),
        window_lifecycle::QuitProgress::PendingProcesses { .. }
    ));
    app.poll_directory_comparison(&egui::Context::default());
    assert!(app.directory_diff_draining.is_empty());
    assert_eq!(
        id(&app.directory_comparison.as_ref().unwrap().quit_stamp()),
        id(&before)
    );
    assert!(app
        .directory_comparison
        .as_ref()
        .unwrap()
        .quit_stamp()
        .ends_with(":false"));
    assert!(matches!(
        app.answer_coordinated_quit("directory-owner", window_lifecycle::QuitAnswer::Cancel),
        window_lifecycle::QuitProgress::Rejected { .. }
    ));
    assert!(app.coordinated_quit_cancel_requested("directory-owner"));
    assert!(app.coordinated_quit_active());
    assert_eq!(
        id(&app.directory_comparison.as_ref().unwrap().quit_stamp()),
        id(&before)
    );
    assert!(app
        .directory_comparison
        .as_ref()
        .unwrap()
        .quit_stamp()
        .ends_with(":false"));
    assert!(app.directory_diff_draining.is_empty());
    assert_eq!(
        app.abort_coordinated_quit("directory-owner"),
        window_lifecycle::QuitProgress::Aborted
    );
    assert!(
        matches!(app.dialog, Dialog::DirectoryComparison),
        "Abort must restore the retained comparison surface"
    );
    let old_domain = app.directory_diff_domain.clone();
    app.cancel_project_tasks();
    assert!(old_domain.is_cancelled());
    assert!(!app.directory_diff_domain.is_cancelled());
    assert!(app.directory_comparison.is_none());
    drain(&mut app, &egui::Context::default());
}
#[test]
fn stable_replacement_poll_cannot_keep_unowned_active_comparison() {
    let (left, right, mut app) = fixture();
    app.start_directory_comparison(left.path().into(), right.path().into());
    let before = app.directory_comparison.as_ref().unwrap().quit_stamp();
    app.dialog = Dialog::None;
    app.poll_directory_comparison(&egui::Context::default());
    assert!(app.directory_comparison.is_none());
    // A fast worker may already be acknowledged/retired in this same poll.
    for job in &app.directory_diff_draining {
        assert_eq!(id(&job.quit_stamp()), id(&before));
    }
    drain(&mut app, &egui::Context::default());
    assert!(!app.directory_comparisons_pending());
}

#[test]
fn committed_quit_keeps_exact_cancelled_job_until_actual_acknowledgement() {
    let (left, right, mut app) = fixture();
    app.start_directory_comparison(left.path().into(), right.path().into());
    let before = app.directory_comparison.as_ref().unwrap().quit_stamp();
    // Wait for actual manager retirement WITHOUT polling/consuming the result ACK.
    // This stabilizes the running flag before consent; missing ACK remains pending.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app
        .directory_comparison
        .as_ref()
        .unwrap()
        .quit_stamp()
        .split(':')
        .nth(2)
        != Some("false")
    {
        assert!(
            std::time::Instant::now() < deadline,
            "Real directory worker did not retire"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        app.directory_comparisons_pending(),
        "Unconsumed result ACK must retain process ownership"
    );
    assert!(matches!(
        app.prepare_managed_coordinated_quit("directory-commit"),
        window_lifecycle::QuitProgress::PendingProcesses { .. }
    ));
    let window_lifecycle::QuitProgress::Prepared { guard } =
        app.answer_coordinated_quit("directory-commit", window_lifecycle::QuitAnswer::Continue)
    else {
        panic!("Stable retired worker with pending ACK did not prepare actual consent");
    };
    app.commit_coordinated_quit("directory-commit", &guard, |_, _| Ok(()))
        .unwrap();
    assert!(app.directory_comparison.is_none());
    assert_eq!(app.directory_diff_draining.len(), 1);
    assert_eq!(
        id(&app.directory_diff_draining[0].quit_stamp()),
        id(&before)
    );
    assert!(app.directory_diff_draining[0]
        .quit_stamp()
        .ends_with(":true"));
    drain(&mut app, &egui::Context::default());
    assert!(!app.directory_comparisons_pending());
}
