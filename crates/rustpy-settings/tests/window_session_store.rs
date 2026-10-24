use rustpy_settings::window_session_store::*;
use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn store() -> (tempfile::TempDir, WindowSessionStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WindowSessionStore::new(dir.path());
    (dir, store)
}
fn accepted(store: &WindowSessionStore, project: Option<&Path>) -> WindowMutation {
    store
        .accept_window(&store.load().unwrap(), WindowOwnerId::new(), project)
        .unwrap()
}
fn checkpoint(store: &WindowSessionStore) -> WindowSessionSnapshot {
    accepted(store, Some(Path::new("missing-project")));
    store
        .quit_application(store.prepare_quit(&store.load().unwrap()).unwrap())
        .unwrap()
}
fn bytes(store: &WindowSessionStore) -> Vec<u8> {
    fs::read(store.path()).unwrap()
}

#[test]
fn missing_corrupt_and_nonregular_are_distinct_and_preserved() {
    let (_dir, store) = store();
    assert!(store.load().unwrap().revision().is_none());
    assert!(!store.path().exists());
    for raw in [b"broken".as_slice(), b"[]", br#"{"generation":"wrong"}"#] {
        fs::write(store.path(), raw).unwrap();
        assert!(matches!(
            store.load(),
            Err(WindowSessionError::Invalid { .. })
        ));
        assert_eq!(bytes(&store), raw);
    }
    fs::remove_file(store.path()).unwrap();
    fs::create_dir(store.path()).unwrap();
    assert!(store.load().is_err());
    assert!(store.path().is_dir());
}

#[test]
fn stale_baseline_accepts_disjoint_owners_but_tombstone_rejects_callbacks() {
    let (dir, store) = store();
    let second = WindowSessionStore::new(dir.path());
    let baseline = store.load().unwrap();
    let a = store
        .accept_window(&baseline, WindowOwnerId::new(), Some(Path::new("a")))
        .unwrap();
    let b = second
        .accept_window(&baseline, WindowOwnerId::new(), Some(Path::new("a")))
        .unwrap();
    let live = b.snapshot.live_windows();
    assert_eq!(live.len(), 2);
    assert_eq!(live[0].token, a.token);
    assert_eq!(live[1].token, b.token);
    let changed = store.update_project(&a.token, None).unwrap();
    assert!(changed.snapshot.live_windows()[0].project.is_none());
    assert!(matches!(
        store.update_project(&a.token, None),
        Err(WindowSessionError::StaleOwner)
    ));
    store.close_window(&changed.token).unwrap();
    let closed = bytes(&store);
    assert!(matches!(
        store.update_project(&changed.token, Some(Path::new("late"))),
        Err(WindowSessionError::ClosedOwner)
    ));
    assert!(matches!(
        store.accept_window(&store.load().unwrap(), a.token.owner().clone(), None),
        Err(WindowSessionError::DuplicateOwner)
    ));
    assert_eq!(bytes(&store), closed);
}

#[test]
fn quit_is_whole_cohort_guarded_and_late_close_cannot_erase_checkpoint() {
    let (_dir, store) = store();
    let first = accepted(&store, Some(Path::new("missing-a")));
    let plan = store.prepare_quit(&first.snapshot).unwrap();
    let second = accepted(&store, None);
    assert!(matches!(
        store.quit_application(plan),
        Err(WindowSessionError::CohortChanged)
    ));
    let plan = store.prepare_quit(&second.snapshot).unwrap();
    let updated = store
        .update_project(&first.token, Some(Path::new("missing-b")))
        .unwrap();
    assert!(matches!(
        store.quit_application(plan),
        Err(WindowSessionError::CohortChanged)
    ));
    let before = bytes(&store);
    drop(store.prepare_quit(&updated.snapshot).unwrap()); // caller guard declined
    assert_eq!(bytes(&store), before);
    let saved = store
        .quit_application(store.prepare_quit(&updated.snapshot).unwrap())
        .unwrap();
    let checkpoint = saved.checkpoint().unwrap().clone();
    assert_eq!(checkpoint.windows.len(), 2);
    assert_eq!(checkpoint.windows[0].owner, first.token.owner().clone());
    assert_eq!(
        checkpoint.windows[0].project_status(),
        Some(WindowProjectStatus::Missing)
    );
    assert!(saved.live_windows().is_empty());
    assert!(matches!(
        store.close_window(&second.token),
        Err(WindowSessionError::StaleGeneration { .. })
    ));
    let welcome = accepted(&store, None);
    let target = store
        .update_project(&welcome.token, Some(Path::new("explicit-target")))
        .unwrap();
    let cleared = store.update_project(&target.token, None).unwrap();
    store.close_window(&cleared.token).unwrap();
    assert_eq!(store.load().unwrap().checkpoint(), Some(&checkpoint));
}

#[test]
fn restore_claim_is_exclusive_readonly_owner_bound_and_generation_guarded() {
    let (dir, store) = store();
    let saved = checkpoint(&store);
    let generation = saved.checkpoint().unwrap().generation;
    let before = bytes(&store);
    let second = WindowSessionStore::new(dir.path());
    let mut claim = store.try_claim_restore(generation).unwrap().unwrap();
    assert!(second.try_claim_restore(generation).unwrap().is_none());
    assert_eq!(bytes(&store), before);
    assert!(matches!(
        second.begin_restore(&mut claim),
        Err(WindowSessionError::WrongConfiguration)
    ));
    let restored = store.begin_restore(&mut claim).unwrap();
    assert!(restored.generation().value() > saved.generation().value());
    assert_eq!(restored.checkpoint(), saved.checkpoint());
    assert!(matches!(
        store.begin_restore(&mut claim),
        Err(WindowSessionError::RestoreAlreadyBegun)
    ));
    drop(claim);
    assert!(second.try_claim_restore(generation).unwrap().is_some());
    let next = checkpoint(&store);
    assert_ne!(generation, next.checkpoint().unwrap().generation);
    assert!(matches!(
        store.try_claim_restore(generation),
        Err(WindowSessionError::StaleGeneration { .. })
    ));
}

#[test]
fn restore_does_not_implicitly_clear_live_and_rejects_new_cohort() {
    let (_dir, store) = store();
    let saved = checkpoint(&store);
    let live = accepted(&store, Some(Path::new("actual-live")));
    let mut claim = store
        .try_claim_restore(saved.checkpoint().unwrap().generation)
        .unwrap()
        .unwrap();
    assert_eq!(
        store.load().unwrap().live_windows(),
        live.snapshot.live_windows()
    );
    let new = accepted(&store, None);
    let before = bytes(&store);
    assert!(matches!(
        store.begin_restore(&mut claim),
        Err(WindowSessionError::CohortChanged)
    ));
    assert_eq!(bytes(&store), before);
    assert_eq!(
        store.load().unwrap().live_windows(),
        new.snapshot.live_windows()
    );
}

#[test]
fn unknown_fields_survive_narrow_owner_and_checkpoint_updates() {
    let (_dir, store) = store();
    let a = accepted(&store, Some(Path::new("a")));
    let mut raw: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    raw["future"] = json!({"nested": [1,2]});
    raw["live"][a.token.owner().as_str()]["future_owner"] = json!("keep");
    fs::write(store.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
    let a = store.update_project(&a.token, None).unwrap();
    let raw: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    assert_eq!(raw["future"], json!({"nested": [1,2]}));
    assert_eq!(
        raw["live"][a.token.owner().as_str()]["future_owner"],
        "keep"
    );
    let saved = store
        .quit_application(store.prepare_quit(&a.snapshot).unwrap())
        .unwrap();
    let mut raw: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    raw["checkpoint"]["future_checkpoint"] = json!(true);
    raw["checkpoint"]["windows"][0]["future_window"] = json!(42);
    fs::write(store.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
    let b = accepted(&store, Some(Path::new("b")));
    store.close_window(&b.token).unwrap();
    let mut claim = store
        .try_claim_restore(saved.checkpoint().unwrap().generation)
        .unwrap()
        .unwrap();
    store.begin_restore(&mut claim).unwrap();
    let raw: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    assert_eq!(raw["checkpoint"]["future_checkpoint"], true);
    assert_eq!(raw["checkpoint"]["windows"][0]["future_window"], 42);
}

fn wait(path: &Path) {
    let end = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < end,
            "timeout waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn child(dir: &Path, mode: &str, tag: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "process_worker", "--nocapture"])
        .env("RUSTPY_WINDOW_TEST_DIR", dir)
        .env("RUSTPY_WINDOW_TEST_MODE", mode)
        .env("RUSTPY_WINDOW_TEST_TAG", tag)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}
fn finish(mut child: std::process::Child) {
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    println!("child stdout: {}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn process_worker() {
    let Ok(dir) = std::env::var("RUSTPY_WINDOW_TEST_DIR") else {
        return;
    };
    let dir = Path::new(&dir);
    let mode = std::env::var("RUSTPY_WINDOW_TEST_MODE").unwrap();
    let tag = std::env::var("RUSTPY_WINDOW_TEST_TAG").unwrap();
    let store = WindowSessionStore::new(dir);
    match mode.as_str() {
        "accept" => {
            let baseline = store.load().unwrap();
            fs::write(dir.join(format!("ready-{tag}")), b"ready").unwrap();
            wait(&dir.join("go"));
            let result = store
                .accept_window(&baseline, WindowOwnerId::new(), Some(Path::new(&tag)))
                .unwrap();
            println!(
                "pid={} owner={} generation={}",
                std::process::id(),
                result.token.owner().as_str(),
                result.token.generation().value()
            );
        }
        "late" => {
            let token: WindowToken =
                serde_json::from_slice(&fs::read(dir.join("token")).unwrap()).unwrap();
            fs::write(dir.join("ready-late"), b"ready").unwrap();
            wait(&dir.join("go-late"));
            assert!(matches!(
                store.update_project(&token, None),
                Err(WindowSessionError::ClosedOwner)
            ));
        }
        "generation" => {
            let token: WindowToken =
                serde_json::from_slice(&fs::read(dir.join("token-generation")).unwrap()).unwrap();
            fs::write(dir.join("ready-generation"), b"ready").unwrap();
            wait(&dir.join("go-generation"));
            assert!(matches!(
                store.close_window(&token),
                Err(WindowSessionError::StaleGeneration { .. })
            ));
        }
        "claim" => {
            let generation = store.load().unwrap().checkpoint().unwrap().generation;
            let claim = store.try_claim_restore(generation).unwrap().unwrap();
            fs::write(dir.join("ready-claim"), b"ready").unwrap();
            wait(&dir.join("go-claim"));
            drop(claim);
        }
        "reader" => {
            fs::write(dir.join("ready-reader"), b"ready").unwrap();
            let end = Instant::now() + Duration::from_secs(10);
            let mut reads = 0;
            while !dir.join("done-reader").exists() {
                assert!(Instant::now() < end);
                let snap = store.load().unwrap();
                assert_eq!(snap.live_windows().len(), 1);
                reads += 1;
            }
            assert!(reads > 0);
            println!(
                "atomic reader pid={} valid_reads={reads}",
                std::process::id()
            );
        }
        _ => panic!("unknown child mode"),
    }
}
#[test]
fn actual_processes_preserve_disjoint_owners_and_tombstones() {
    let (dir, store) = store();
    let a = child(dir.path(), "accept", "a");
    let b = child(dir.path(), "accept", "b");
    wait(&dir.path().join("ready-a"));
    wait(&dir.path().join("ready-b"));
    fs::write(dir.path().join("go"), b"go").unwrap();
    finish(a);
    finish(b);
    assert_eq!(store.load().unwrap().live_windows().len(), 2);
    let token = store.load().unwrap().live_windows()[0].token.clone();
    fs::write(
        dir.path().join("token"),
        serde_json::to_vec(&token).unwrap(),
    )
    .unwrap();
    let late = child(dir.path(), "late", "late");
    wait(&dir.path().join("ready-late"));
    store.close_window(&token).unwrap();
    let closed = bytes(&store);
    fs::write(dir.path().join("go-late"), b"go").unwrap();
    finish(late);
    assert_eq!(bytes(&store), closed);
}
#[test]
#[cfg(unix)] // Windows file-sharing and socket semantics differ; see known issues
fn actual_process_claim_isolation_and_atomic_readers() {
    let (dir, store) = store();
    let saved = checkpoint(&store);
    let generation = saved.checkpoint().unwrap().generation;
    let claimant = child(dir.path(), "claim", "claim");
    wait(&dir.path().join("ready-claim"));
    assert!(store.try_claim_restore(generation).unwrap().is_none());
    let live = accepted(&store, None);
    fs::write(dir.path().join("go-claim"), b"go").unwrap();
    finish(claimant);
    assert!(store.try_claim_restore(generation).unwrap().is_some());
    let reader = child(dir.path(), "reader", "reader");
    wait(&dir.path().join("ready-reader"));
    let mut token = live.token;
    for n in 0..32 {
        token = store
            .update_project(&token, Some(Path::new(&format!("project-{n}"))))
            .unwrap()
            .token;
    }
    fs::write(dir.path().join("done-reader"), b"done").unwrap();
    finish(reader);
}

#[test]
fn exact_revision_rejects_net_zero_cohort_and_same_generation_checkpoint_changes() {
    let (_dir, store) = store();
    let first = accepted(&store, None);
    let plan = store.prepare_quit(&first.snapshot).unwrap();
    let temporary = accepted(&store, None);
    store.close_window(&temporary.token).unwrap();
    assert_eq!(
        store.load().unwrap().live_windows(),
        first.snapshot.live_windows()
    );
    assert!(matches!(
        store.quit_application(plan),
        Err(WindowSessionError::CohortChanged)
    ));
    let saved = store
        .quit_application(store.prepare_quit(&store.load().unwrap()).unwrap())
        .unwrap();
    let mut claim = store
        .try_claim_restore(saved.checkpoint().unwrap().generation)
        .unwrap()
        .unwrap();
    let mut raw: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    raw["checkpoint"]["windows"][0]["project"] = json!("different-missing-root");
    fs::write(store.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
    let changed = bytes(&store);
    assert!(matches!(
        store.begin_restore(&mut claim),
        Err(WindowSessionError::CohortChanged)
    ));
    assert_eq!(bytes(&store), changed);
}

#[test]
fn actual_process_old_generation_callback_cannot_erase_quit_checkpoint() {
    let (dir, store) = store();
    let live = accepted(&store, Some(Path::new("missing")));
    fs::write(
        dir.path().join("token-generation"),
        serde_json::to_vec(&live.token).unwrap(),
    )
    .unwrap();
    let old = child(dir.path(), "generation", "generation");
    wait(&dir.path().join("ready-generation"));
    let saved = store
        .quit_application(store.prepare_quit(&live.snapshot).unwrap())
        .unwrap();
    let before = bytes(&store);
    fs::write(dir.path().join("go-generation"), b"go").unwrap();
    finish(old);
    assert_eq!(bytes(&store), before);
    assert_eq!(store.load().unwrap().checkpoint(), saved.checkpoint());
}

#[test]
#[cfg(unix)] // Unix fixture paths, tools or process semantics
fn inaccessible_configuration_ancestor_is_error_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("ordinary-file");
    fs::write(&parent, b"retain").unwrap();
    let store = WindowSessionStore::new(parent.join("config"));
    assert!(matches!(store.load(), Err(WindowSessionError::Io { .. })));
    assert_eq!(fs::read(&parent).unwrap(), b"retain");
}

fn assert_required_state_refusal(
    store: &WindowSessionStore,
    baseline: &WindowSessionSnapshot,
    raw: Value,
) {
    let corrupt = serde_json::to_vec(&raw).unwrap();
    fs::write(store.path(), &corrupt).unwrap();
    assert!(
        matches!(store.load(), Err(WindowSessionError::Invalid { .. })),
        "incomplete existing state must never default"
    );
    assert!(store
        .accept_window(baseline, WindowOwnerId::new(), None)
        .is_err());
    assert_eq!(bytes(store), corrupt);
}
#[test]
fn required_state_existing_empty_object_refuses_default_and_publication() {
    let (_dir, store) = store();
    let baseline = checkpoint(&store);
    assert_required_state_refusal(&store, &baseline, json!({}));
}
#[test]
fn required_state_partial_checkpoint_refuses_lost_restart_data() {
    let (_dir, store) = store();
    let baseline = checkpoint(&store);
    let mut raw: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    raw.as_object_mut().unwrap().remove("checkpoint");
    assert_required_state_refusal(&store, &baseline, raw);
}
#[test]
fn required_state_partial_owner_refuses_implicit_welcome_window() {
    let (_dir, store) = store();
    let baseline = accepted(&store, Some(Path::new("missing-owned-project")));
    let mut raw: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    raw["live"][baseline.token.owner().as_str()]
        .as_object_mut()
        .unwrap()
        .remove("project");
    assert_required_state_refusal(&store, &baseline.snapshot, raw);
}

#[test]
fn required_state_all_known_fields_are_required_but_explicit_null_is_valid() {
    let (_dir, store) = store();
    checkpoint(&store);
    let live = accepted(&store, None);
    let valid: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    assert!(valid["live"][live.token.owner().as_str()]["project"].is_null());
    for field in [
        "format_version",
        "generation",
        "next_order",
        "live",
        "checkpoint",
    ] {
        let mut raw = valid.clone();
        raw.as_object_mut().unwrap().remove(field);
        assert_required_state_refusal(&store, &live.snapshot, raw);
    }
    for field in ["revision", "order", "project", "closed"] {
        let mut raw = valid.clone();
        raw["live"][live.token.owner().as_str()]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert_required_state_refusal(&store, &live.snapshot, raw);
    }
    for field in ["generation", "windows"] {
        let mut raw = valid.clone();
        raw["checkpoint"].as_object_mut().unwrap().remove(field);
        assert_required_state_refusal(&store, &live.snapshot, raw);
    }
    for field in ["owner", "project"] {
        let mut raw = valid.clone();
        raw["checkpoint"]["windows"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert_required_state_refusal(&store, &live.snapshot, raw);
    }
    fs::write(store.path(), serde_json::to_vec(&valid).unwrap()).unwrap();
    assert_eq!(
        store.load().unwrap().live_windows(),
        live.snapshot.live_windows()
    );
}
