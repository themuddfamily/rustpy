use rustpy_settings::window_session_store::*;
use serde_json::{json, Value};
use std::{fs, path::Path};

fn store() -> (tempfile::TempDir, WindowSessionStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WindowSessionStore::new(dir.path());
    (dir, store)
}
fn accept(store: &WindowSessionStore, owner: WindowOwnerId, project: Option<&Path>) {
    store
        .accept_window(&store.load().unwrap(), owner, project)
        .unwrap();
}
fn binding(snapshot: &WindowSessionSnapshot, tx: &str) -> GuardedQuitBinding {
    GuardedQuitBinding {
        transaction_id: tx.into(),
        cohort: snapshot
            .live_windows()
            .into_iter()
            .enumerate()
            .map(|(i, live)| GuardedQuitWindow {
                owner: live.token.owner().clone(),
                generation: live.token.generation(),
                owner_revision: live.token.revision(),
                window_id: format!("endpoint-{i}"),
                project: live.project,
                guard: QuitUiGuard {
                    nonce: format!("guard-{i}"),
                    state: "actual-state-digest".into(),
                },
            })
            .collect(),
    }
}
fn bytes(store: &WindowSessionStore) -> Vec<u8> {
    fs::read(store.path()).unwrap()
}
fn commit(
    store: &WindowSessionStore,
    snapshot: &WindowSessionSnapshot,
    binding: GuardedQuitBinding,
) -> Result<GuardedQuitCertificate, WindowSessionError> {
    store.commit_guarded_quit(store.prepare_quit(snapshot).unwrap(), binding)
}

#[test]
fn one_checkpoint_binds_complete_welcome_project_identity_and_guards() {
    let (_dir, store) = store();
    accept(&store, WindowOwnerId::new(), None);
    accept(&store, WindowOwnerId::new(), Some(Path::new("项目 🦀")));
    let before = store.load().unwrap();
    let binding = binding(&before, "quit-a");
    let certificate = commit(&store, &before, binding.clone()).unwrap();
    let after = store.load().unwrap();
    assert!(after.live_windows().is_empty());
    assert_eq!(after.generation().value(), before.generation().value() + 1);
    assert_eq!(after.checkpoint().unwrap().windows.len(), 2);
    assert!(certificate.cohort[0].project.is_none());
    assert_eq!(certificate.checkpoint_revision.len(), 64);
    for local in &binding.cohort {
        let verified = store.verify_guarded_quit(&certificate, local).unwrap();
        assert_eq!(verified.certificate(), &certificate);
        assert_eq!(verified.configuration(), store.path().parent().unwrap());
    }
    let raw: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    assert_eq!(
        raw["checkpoint"]["guarded_quit"],
        serde_json::to_value(&certificate).unwrap()
    );
}

#[test]
fn complete_owner_revision_project_and_unique_endpoint_required_before_writing() {
    let (_dir, store) = store();
    accept(&store, WindowOwnerId::new(), None);
    accept(&store, WindowOwnerId::new(), None);
    let before = store.load().unwrap();
    let good = binding(&before, "quit-a");
    let original = bytes(&store);
    let mut variants = Vec::new();
    let mut b = good.clone();
    b.cohort.pop();
    variants.push(b);
    let mut b = good.clone();
    b.cohort.swap(0, 1);
    variants.push(b);
    let mut b = good.clone();
    b.cohort[0].owner = WindowOwnerId::new();
    variants.push(b);
    let mut b = good.clone();
    b.cohort[0].owner_revision += 1;
    variants.push(b);
    let mut b = good.clone();
    b.cohort[0].project = Some(Path::new("other").into());
    variants.push(b);
    let mut b = good.clone();
    b.cohort[1].window_id = b.cohort[0].window_id.clone();
    variants.push(b);
    let mut b = good.clone();
    b.cohort[0].guard.nonce.clear();
    variants.push(b);
    let mut b = good.clone();
    b.cohort[0].guard.state = "x".repeat(129);
    variants.push(b);
    let mut b = good;
    b.transaction_id = "bad\ntransaction".into();
    variants.push(b);
    for b in variants {
        assert!(commit(&store, &before, b).is_err());
        assert_eq!(bytes(&store), original);
    }
}

#[test]
fn exact_duplicate_is_byte_identical_conflicting_reuse_and_foreign_guard_fail() {
    let (_dir, store) = store();
    accept(&store, WindowOwnerId::new(), None);
    let before = store.load().unwrap();
    let binding = binding(&before, "quit-a");
    let certificate = commit(&store, &before, binding.clone()).unwrap();
    let original = bytes(&store);
    assert_eq!(
        commit(&store, &before, binding.clone()).unwrap(),
        certificate
    );
    assert_eq!(bytes(&store), original);
    let mut changed = binding.clone();
    changed.cohort[0].guard.state = "changed".into();
    assert!(commit(&store, &before, changed).is_err());
    let mut other = certificate.clone();
    other.transaction_id = "forged".into();
    assert!(store
        .verify_guarded_quit(&other, &binding.cohort[0])
        .is_err());
    let mut local = binding.cohort[0].clone();
    local.window_id = "other-endpoint".into();
    assert!(store.verify_guarded_quit(&certificate, &local).is_err());
    let mut local = binding.cohort[0].clone();
    local.guard.nonce = "old-approval".into();
    assert!(store.verify_guarded_quit(&certificate, &local).is_err());
    assert_eq!(bytes(&store), original);
}

#[test]
fn stale_plan_net_zero_owner_mutation_and_raw_external_change_are_rejected() {
    let (_dir, store) = store();
    accept(&store, WindowOwnerId::new(), None);
    let before = store.load().unwrap();
    let b = binding(&before, "quit-a");
    let token = before.live_windows()[0].token.clone();
    let changed = store
        .update_project(&token, Some(Path::new("temp")))
        .unwrap();
    store.update_project(&changed.token, None).unwrap();
    let original = bytes(&store);
    assert!(commit(&store, &before, b).is_err());
    assert_eq!(bytes(&store), original);
    let fresh = store.load().unwrap();
    let mut raw: Value = serde_json::from_slice(&original).unwrap();
    raw["future"] = json!({"changed": true});
    fs::write(store.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
    let original = bytes(&store);
    assert!(commit(&store, &fresh, binding(&fresh, "quit-b")).is_err());
    assert_eq!(bytes(&store), original);
}

#[test]
fn legacy_checkpoint_cannot_authorize_commit_and_missing_read_creates_nothing() {
    let (dir, store) = store();
    let owner = WindowOwnerId::new();
    let fake: GuardedQuitCertificate = serde_json::from_value(json!({
        "transaction_id":"fake", "checkpoint_generation":1, "checkpoint_revision":"fake",
        "cohort":[{"owner":owner,"generation":0,"owner_revision":0,"window_id":"endpoint",
            "project":null,"guard":{"nonce":"n","state":"s"}}]
    }))
    .unwrap();
    assert!(store.verify_guarded_quit(&fake, &fake.cohort[0]).is_err());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    accept(&store, WindowOwnerId::new(), None);
    store
        .quit_application(store.prepare_quit(&store.load().unwrap()).unwrap())
        .unwrap();
    let original = bytes(&store);
    assert!(store.verify_guarded_quit(&fake, &fake.cohort[0]).is_err());
    assert_eq!(bytes(&store), original);
}

#[test]
fn restored_generation_and_new_live_membership_reject_late_commit() {
    let (_dir, store) = store();
    accept(&store, WindowOwnerId::new(), None);
    let before = store.load().unwrap();
    let b = binding(&before, "quit-a");
    let certificate = commit(&store, &before, b.clone()).unwrap();
    let mut claim = store
        .try_claim_restore(certificate.checkpoint_generation)
        .unwrap()
        .unwrap();
    store.begin_restore(&mut claim).unwrap();
    assert!(store
        .verify_guarded_quit(&certificate, &b.cohort[0])
        .is_err());
    assert!(commit(&store, &before, b).is_err());
    // Restore preserves the restart checkpoint and its receipt as historical
    // data; its old certificate never authorizes the new runtime round.
    assert_eq!(
        store.load().unwrap().checkpoint().unwrap().generation,
        certificate.checkpoint_generation
    );
}

#[test]
fn same_generation_new_membership_rejects_old_certificate_and_duplicate() {
    let (_dir, store) = store();
    accept(&store, WindowOwnerId::new(), None);
    let before = store.load().unwrap();
    let b = binding(&before, "quit-a");
    let certificate = commit(&store, &before, b.clone()).unwrap();
    accept(&store, WindowOwnerId::new(), None);
    assert!(store
        .verify_guarded_quit(&certificate, &b.cohort[0])
        .is_err());
    assert!(commit(&store, &before, b).is_err());
}

#[test]
fn unknown_root_checkpoint_cohort_and_guard_fields_survive_exact_retry() {
    let (_dir, store) = store();
    let owner = WindowOwnerId::new();
    accept(&store, owner.clone(), None);
    let before = store.load().unwrap();
    let b = binding(&before, "quit-a");
    let certificate = commit(&store, &before, b.clone()).unwrap();
    let mut raw: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    raw["future"] = json!({"root":"opaque"});
    raw["checkpoint"]["future"] = json!({"checkpoint":"opaque"});
    raw["checkpoint"]["windows"][0]["future"] = json!({"window":"opaque"});
    raw["checkpoint"]["guarded_quit"]["future"] = json!({"receipt":"opaque"});
    raw["checkpoint"]["guarded_quit"]["cohort"][0]["future"] = json!({"owner":"opaque"});
    raw["checkpoint"]["guarded_quit"]["cohort"][0]["guard"]["future"] = json!({"guard":"opaque"});
    fs::write(store.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
    let original = bytes(&store);
    assert_eq!(commit(&store, &before, b).unwrap(), certificate);
    assert_eq!(bytes(&store), original);
    // A new transaction for the same logical owner overlays only known keys.
    accept(&store, owner, None);
    let next = store.load().unwrap();
    commit(&store, &next, binding(&next, "quit-b")).unwrap();
    let now: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    assert_eq!(now["future"], raw["future"]);
    for pointer in [
        "/checkpoint/future",
        "/checkpoint/windows/0/future",
        "/checkpoint/guarded_quit/future",
        "/checkpoint/guarded_quit/cohort/0/future",
        "/checkpoint/guarded_quit/cohort/0/guard/future",
    ] {
        assert_eq!(now.pointer(pointer), raw.pointer(pointer));
    }
}

#[test]
fn every_required_receipt_key_including_explicit_welcome_null_is_checked() {
    let (_dir, store) = store();
    accept(&store, WindowOwnerId::new(), None);
    let before = store.load().unwrap();
    commit(&store, &before, binding(&before, "quit-a")).unwrap();
    let original: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    for (pointer, key) in [
        ("/checkpoint/guarded_quit", "transaction_id"),
        ("/checkpoint/guarded_quit", "checkpoint_generation"),
        ("/checkpoint/guarded_quit", "checkpoint_revision"),
        ("/checkpoint/guarded_quit", "cohort"),
        ("/checkpoint/guarded_quit/cohort/0", "owner"),
        ("/checkpoint/guarded_quit/cohort/0", "generation"),
        ("/checkpoint/guarded_quit/cohort/0", "owner_revision"),
        ("/checkpoint/guarded_quit/cohort/0", "window_id"),
        ("/checkpoint/guarded_quit/cohort/0", "project"),
        ("/checkpoint/guarded_quit/cohort/0", "guard"),
        ("/checkpoint/guarded_quit/cohort/0/guard", "nonce"),
        ("/checkpoint/guarded_quit/cohort/0/guard", "state"),
    ] {
        let mut raw = original.clone();
        raw.pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        let broken = serde_json::to_vec(&raw).unwrap();
        fs::write(store.path(), &broken).unwrap();
        assert!(
            matches!(store.load(), Err(WindowSessionError::Invalid { .. })),
            "missing {pointer}/{key}"
        );
        assert_eq!(bytes(&store), broken);
    }
}

#[test]
fn digest_and_checkpoint_tampering_fail_without_repair_or_creation() {
    let (_dir, store) = store();
    accept(&store, WindowOwnerId::new(), None);
    let before = store.load().unwrap();
    commit(&store, &before, binding(&before, "quit-a")).unwrap();
    let original: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    for (pointer, value) in [
        (
            "/checkpoint/guarded_quit/checkpoint_revision",
            json!("0".repeat(64)),
        ),
        (
            "/checkpoint/guarded_quit/cohort/0/guard/state",
            json!("forged"),
        ),
        ("/checkpoint/windows/0/project", json!("other")),
    ] {
        let mut raw = original.clone();
        *raw.pointer_mut(pointer).unwrap() = value;
        let broken = serde_json::to_vec(&raw).unwrap();
        fs::write(store.path(), &broken).unwrap();
        assert!(matches!(
            store.load(),
            Err(WindowSessionError::Invalid { .. })
        ));
        assert_eq!(bytes(&store), broken);
    }
}

#[test]
fn deterministic_digest_ignores_root_bytes_and_old_quit_stays_legacy() {
    let (_d1, a) = store();
    let (_d2, b) = store();
    let owner = WindowOwnerId::new();
    accept(&a, owner.clone(), None);
    accept(&b, owner.clone(), None);
    let mut raw: Value = serde_json::from_slice(&bytes(&b)).unwrap();
    raw["future"] = json!([1, 2, 3]);
    fs::write(b.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
    let aa = a.load().unwrap();
    let bb = b.load().unwrap();
    let ca = commit(&a, &aa, binding(&aa, "same-tx")).unwrap();
    let cb = commit(&b, &bb, binding(&bb, "same-tx")).unwrap();
    assert_eq!(ca, cb);
    accept(&a, owner, None);
    a.quit_application(a.prepare_quit(&a.load().unwrap()).unwrap())
        .unwrap();
    let raw: Value = serde_json::from_slice(&bytes(&a)).unwrap();
    assert!(raw["checkpoint"].get("guarded_quit").is_none());
    assert!(a.verify_guarded_quit(&ca, &ca.cohort[0]).is_err());
}

#[test]
fn postcommit_net_zero_membership_mutation_cannot_reauthorize_retired_cohort() {
    let (_dir, store) = store();
    accept(&store, WindowOwnerId::new(), None);
    let before = store.load().unwrap();
    let b = binding(&before, "quit-a");
    let certificate = commit(&store, &before, b.clone()).unwrap();
    let new_owner = store
        .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
        .unwrap();
    store.close_window(&new_owner.token).unwrap();
    assert!(store.load().unwrap().live_windows().is_empty());
    assert!(store
        .verify_guarded_quit(&certificate, &b.cohort[0])
        .is_err());
    assert!(commit(&store, &before, b).is_err());
}

#[test]
fn checked_snapshot_accessor_loads_exact_certificate_and_clone_retains_it() {
    let (_dir, store) = store();
    assert!(store.load().unwrap().guarded_quit_certificate().is_none());
    accept(&store, WindowOwnerId::new(), None);
    let before = store.load().unwrap();
    assert!(before.guarded_quit_certificate().is_none());
    let certificate = commit(&store, &before, binding(&before, "checked-receipt")).unwrap();
    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.guarded_quit_certificate(), Some(&certificate));
    assert_eq!(
        snapshot.clone().guarded_quit_certificate(),
        Some(&certificate)
    );
    assert!(before.guarded_quit_certificate().is_none());
}

#[test]
fn snapshot_receipt_tracks_persisted_checkpoint_through_runtime_and_legacy_replacement() {
    let (_dir, store) = store();
    accept(&store, WindowOwnerId::new(), None);
    let before = store.load().unwrap();
    let certificate = commit(&store, &before, binding(&before, "checked-receipt")).unwrap();
    let mut claim = store
        .try_claim_restore(certificate.checkpoint_generation)
        .unwrap()
        .unwrap();
    let restored = store.begin_restore(&mut claim).unwrap();
    assert_eq!(restored.guarded_quit_certificate(), Some(&certificate));
    assert!(restored.generation().value() > certificate.checkpoint_generation.value());
    let accepted = store
        .accept_window(&restored, WindowOwnerId::new(), None)
        .unwrap();
    assert_eq!(
        accepted.snapshot.guarded_quit_certificate(),
        Some(&certificate)
    );
    let changed = store
        .update_project(&accepted.token, Some(Path::new("project")))
        .unwrap();
    assert_eq!(
        changed.snapshot.guarded_quit_certificate(),
        Some(&certificate)
    );
    let closed = store.close_window(&changed.token).unwrap();
    assert_eq!(closed.guarded_quit_certificate(), Some(&certificate));
    let legacy = store
        .quit_application(store.prepare_quit(&closed).unwrap())
        .unwrap();
    assert!(legacy.guarded_quit_certificate().is_none());
    assert!(store.load().unwrap().guarded_quit_certificate().is_none());
}

#[test]
fn invalid_persisted_receipt_cannot_be_exposed_through_snapshot_accessor() {
    let (_dir, store) = store();
    accept(&store, WindowOwnerId::new(), None);
    let before = store.load().unwrap();
    commit(&store, &before, binding(&before, "checked-receipt")).unwrap();
    let mut raw: Value = serde_json::from_slice(&bytes(&store)).unwrap();
    raw["checkpoint"]["guarded_quit"]["cohort"][0]
        .as_object_mut()
        .unwrap()
        .remove("project");
    let damaged = serde_json::to_vec(&raw).unwrap();
    fs::write(store.path(), &damaged).unwrap();
    assert!(matches!(
        store.load(),
        Err(WindowSessionError::Invalid { .. })
    ));
    assert_eq!(bytes(&store), damaged);
}
