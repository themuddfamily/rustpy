//! Connected App Settings Repository operations against owned local Git only.
use super::*;
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new(rustpy_vcs::git_program())
        .args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
fn settle(app: &mut IdeApp, ctx: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while app.settings_sync_busy() {
        app.settings_sync_tick(ctx, false);
        assert!(
            Instant::now() < deadline,
            "Settings Sync did not retire: {}",
            app.settings_sync.message
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(app.settings_sync_pending().is_empty());
}
fn action(app: &mut IdeApp, config: &Path, ctx: &egui::Context, name: &str) {
    // Only the exact-selector child executes this test; no other test observes
    // these per-actor configuration switches. Workers retain captured paths.
    std::env::set_var("RUSTPY_CONFIG_DIR", config);
    app.perform_settings_sync(name);
    settle(app, ctx);
}
fn actor(config: &Path, checkout: &Path, remote: &Path) -> IdeApp {
    std::fs::create_dir(config).unwrap();
    std::env::set_var("RUSTPY_CONFIG_DIR", config);
    let settings = Settings {
        reopen_last_project: false,
        ..Default::default()
    };
    settings.save().unwrap();
    std::fs::write(config.join("private-session.txt"), "machine-only authority").unwrap();
    let mut app = IdeApp::new(settings, None);
    app.settings_sync.origin = Some(config.to_path_buf());
    app.settings_sync.preferences = Some(Preferences {
        storage: Storage::Git,
        path: checkout.to_str().unwrap().into(),
        remote: remote.to_str().unwrap().into(),
        branch: "main".into(),
        selection: sync::Selection {
            appearance: false,
            editor: false,
            keymaps: true,
            templates: false,
            inspections: false,
            plugins: false,
        },
    });
    app
}
fn configure_git(checkout: &Path) {
    for (key, value) in [
        ("user.name", "Owned Settings Sync"),
        ("user.email", "sync@example.test"),
        ("commit.gpgsign", "false"),
    ] {
        git(checkout, &["config", key, value]);
    }
    let hooks = checkout.join(".git/empty-hooks");
    std::fs::create_dir(&hooks).unwrap();
    git(
        checkout,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
}
fn change_keymap(app: &mut IdeApp, config: &Path, keymap: &str) {
    std::env::set_var("RUSTPY_CONFIG_DIR", config);
    app.settings.keymap = keymap.into();
    app.settings.save().unwrap();
}
fn published_keymap(remote: &Path) -> String {
    let text = git(remote, &["show", "refs/heads/main:rustpy-settings.json"]);
    let snapshot = Snapshot::parse(&text).unwrap();
    assert!(snapshot
        .entries
        .keys()
        .all(|key| !key.contains("private-session")));
    snapshot.entries["settings/keymap"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn push(app: &mut IdeApp, config: &Path, ctx: &egui::Context) {
    action(app, config, ctx, "Commit and push preview");
    assert_eq!(app.settings_sync.message, "Settings pushed successfully");
}

#[test]
fn actual_git_settings_sync_roundtrip_conflict_captured_authority_and_cancel() {
    const MARKER: &str = "RUSTPY_SETTINGS_GIT_ROUNDTRIP_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let root = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "app::settings_sync::git_execution_tests::actual_git_settings_sync_roundtrip_conflict_captured_authority_and_cancel", "--nocapture", "--test-threads=1"])
            .env(MARKER, root.path()).env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY")
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        for (key, name) in [
            ("RUSTPY_CONFIG_DIR", "initial-config"),
            ("RUSTPY_SYSTEM_DIR", "system"),
            ("RUSTPY_LOG_DIR", "logs"),
            ("XDG_CONFIG_HOME", "xdg-config"),
            ("XDG_CACHE_HOME", "xdg-cache"),
            ("XDG_DATA_HOME", "xdg-data"),
            ("XDG_STATE_HOME", "xdg-state"),
            ("XDG_RUNTIME_DIR", "xdg-runtime"),
        ] {
            let path = root.path().join(name);
            std::fs::create_dir(&path).unwrap();
            command.env(key, path);
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("isolated Settings Sync workflow timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = PathBuf::from(std::env::var_os(MARKER).unwrap());
    let remote = root.join("owned-origin.git");
    let trap = root.join("unselected-origin.git");
    git(&root, &["init", "--bare", remote.to_str().unwrap()]);
    git(&root, &["init", "--bare", trap.to_str().unwrap()]);
    let ca = root.join("config-a");
    let cb = root.join("config-b");
    let wa = root.join("checkout-a");
    let wb = root.join("checkout-b");
    let mut a = actor(&ca, &wa, &remote);
    let mut b = actor(&cb, &wb, &remote);
    let ctx = egui::Context::default();
    action(&mut a, &ca, &ctx, "Initialize repository");
    configure_git(&wa);
    assert!(
        a.settings_sync.preview.is_some(),
        "{}",
        a.settings_sync.message
    );
    let original = a.settings.keymap.clone();
    push(&mut a, &ca, &ctx);
    assert_eq!(published_keymap(&remote), original);
    action(&mut b, &cb, &ctx, "Clone repository");
    configure_git(&wb);
    action(&mut b, &cb, &ctx, "Apply preview");
    assert!(
        b.settings_sync
            .message
            .starts_with("Applied selected settings"),
        "{}",
        b.settings_sync.message
    );

    change_keymap(&mut a, &ca, "Emacs");
    action(&mut a, &ca, &ctx, "Fetch and preview");
    // Publishing follows the preview's captured repository, never later form edits.
    let preferences = a.settings_sync.preferences.clone().unwrap();
    let form = a.settings_sync.preferences.as_mut().unwrap();
    form.path = root.join("unselected-checkout").to_str().unwrap().into();
    form.remote = trap.to_str().unwrap().into();
    push(&mut a, &ca, &ctx);
    a.settings_sync.preferences = Some(preferences);
    assert!(!root.join("unselected-checkout").exists());
    assert!(git(&trap, &["for-each-ref"]).is_empty());
    assert_eq!(published_keymap(&remote), "Emacs");
    action(&mut b, &cb, &ctx, "Fetch and preview");
    action(&mut b, &cb, &ctx, "Apply preview");
    assert_eq!(b.settings.keymap, "Emacs");

    change_keymap(&mut b, &cb, "VS Code");
    action(&mut b, &cb, &ctx, "Fetch and preview");
    change_keymap(&mut a, &ca, &original);
    action(&mut a, &ca, &ctx, "Fetch and preview");
    push(&mut a, &ca, &ctx);
    let head = git(&remote, &["rev-parse", "refs/heads/main"]);
    let local = std::fs::read(cb.join("settings.json")).unwrap();
    action(&mut b, &cb, &ctx, "Commit and push preview");
    assert!(b
        .settings_sync
        .message
        .contains("Remote settings changed after the preview"));
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/main"]), head);
    assert_eq!(std::fs::read(cb.join("settings.json")).unwrap(), local);
    action(&mut b, &cb, &ctx, "Fetch and preview");
    let preview = b.settings_sync.preview.as_ref().unwrap();
    assert!(preview
        .preview
        .changes
        .iter()
        .any(|change| change.key == "settings/keymap" && change.conflict && !change.resolved));
    action(&mut b, &cb, &ctx, "Commit and push preview");
    assert!(b
        .settings_sync
        .message
        .contains("Resolve every marked conflict"));
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/main"]), head);
    for change in &mut b.settings_sync.preview.as_mut().unwrap().preview.changes {
        if change.conflict {
            change.choice = Choice::Local;
            change.resolved = true;
        }
    }
    push(&mut b, &cb, &ctx);
    assert_eq!(published_keymap(&remote), "VS Code");
    action(&mut a, &ca, &ctx, "Fetch and preview");
    action(&mut a, &ca, &ctx, "Apply preview");
    assert_eq!(a.settings.keymap, "VS Code");

    let settings = std::fs::read(ca.join("settings.json")).unwrap();
    let baseline = a
        .settings_sync
        .preview
        .as_ref()
        .unwrap()
        .target
        .base
        .clone();
    let baseline_bytes = std::fs::read(&baseline).unwrap();
    let head = git(&remote, &["rev-parse", "refs/heads/main"]);
    std::env::set_var("RUSTPY_CONFIG_DIR", &ca);
    a.perform_settings_sync("Fetch and preview");
    assert!(
        !a.settings_sync_pending().is_empty(),
        "actual Git owner required before Stop"
    );
    a.stop_all_settings_sync();
    settle(&mut a, &ctx);
    assert!(a.settings_sync.message.contains("cancelled"));
    assert_eq!(std::fs::read(ca.join("settings.json")).unwrap(), settings);
    assert_eq!(std::fs::read(&baseline).unwrap(), baseline_bytes);
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/main"]), head);
    for config in [&ca, &cb] {
        assert_eq!(
            std::fs::read_to_string(config.join("private-session.txt")).unwrap(),
            "machine-only authority"
        );
    }
}
