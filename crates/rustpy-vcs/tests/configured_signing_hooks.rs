//! Configured local SSH signing and actual commit-hook acceptance.
//! Keys, allowed signers, hooks and repositories belong only to this fixture.
#![cfg(unix)]
use rustpy_vcs::{GitEnv, GitRepo};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

struct RestorePolicy(rustpy_settings::VcsPreferences);
impl Drop for RestorePolicy {
    fn drop(&mut self) {
        rustpy_vcs::policy::configure(&self.0);
    }
}
fn git(root: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(rustpy_vcs::git_program())
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("SSH_AUTH_SOCK")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}
fn checked_git(root: &Path, args: &[&str]) -> String {
    let output = git(root, args);
    assert!(
        output.status.success(),
        "Git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn hook(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
fn verify(root: &Path, id: &str) {
    let commit = checked_git(root, &["cat-file", "commit", id]);
    assert!(commit.contains("-----BEGIN SSH SIGNATURE-----"));
    let verification = git(root, &["verify-commit", id]);
    assert!(
        verification.status.success(),
        "actual SSH signature verification failed: {}",
        String::from_utf8_lossy(&verification.stderr)
    );
    let output = String::from_utf8_lossy(&verification.stderr);
    assert!(
        output.contains("Good") && output.contains("fixture@example.test"),
        "verified fixture principal missing: {output}"
    );
}

#[test]
#[ignore = "requires explicitly admitted absolute ssh-keygen; creates ephemeral local signing keys only"]
fn configured_ssh_signed_commits_execute_hooks_and_rejections_preserve_index_and_worktree() {
    let keygen = PathBuf::from(
        std::env::var_os("RUSTPY_TEST_SSH_KEYGEN").expect("explicit ssh-keygen admission required"),
    );
    assert!(keygen.is_absolute() && keygen.is_file());
    let parent = PathBuf::from(
        std::env::var_os("RUSTPY_TEST_GIT_DISPOSABLE_ROOT")
            .expect("explicit isolated disposable root required"),
    );
    assert!(parent.is_absolute() && parent.is_dir());
    let fixture = tempfile::Builder::new()
        .prefix("git-signing-")
        .tempdir_in(parent)
        .unwrap();
    let root = fixture.path();
    let native = git2::Repository::init(root).unwrap();
    let control = root.join(".git");
    let key = control.join("ephemeral signing key");
    let generated = std::process::Command::new(&keygen)
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "fixture@example.test",
            "-f",
        ])
        .arg(&key)
        .env_remove("SSH_AUTH_SOCK")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "admitted ssh-keygen could not create owned key: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let public = std::fs::read_to_string(key.with_file_name("ephemeral signing key.pub")).unwrap();
    assert!(public.starts_with("ssh-ed25519 "));
    let signers = control.join("allowed signers");
    std::fs::write(
        &signers,
        format!("fixture@example.test namespaces=\"git\" {public}"),
    )
    .unwrap();
    let hooks = control.join("owned-hooks");
    std::fs::create_dir(&hooks).unwrap();
    let mut config = native.config().unwrap();
    for (name, value) in [
        ("user.name", "Owned Signing Fixture"),
        ("user.email", "fixture@example.test"),
        ("core.hooksPath", hooks.to_str().unwrap()),
        ("gpg.ssh.program", keygen.to_str().unwrap()),
        ("gpg.ssh.allowedSignersFile", signers.to_str().unwrap()),
    ] {
        config.set_str(name, value).unwrap();
    }
    hook(&hooks.join("pre-commit"), "#!/bin/sh\nset -eu\ncontrol=$(git rev-parse --git-dir)\nprintf 'pre-commit\\n' >> \"$control/hook-events\"\nprintf 'rustpy-precommit-executed\\n' >&2\nif test -f \"$control/reject-precommit\"; then printf 'rustpy-precommit-rejected\\n' >&2; exit 1; fi\n");
    hook(&hooks.join("commit-msg"), "#!/bin/sh\nset -eu\ncontrol=$(git rev-parse --git-dir)\nprintf 'commit-msg\\n' >> \"$control/hook-events\"\nif grep -q 'reject fixture' \"$1\"; then printf 'rustpy-commit-msg-rejected\\n' >&2; exit 1; fi\nprintf 'rustpy-commit-msg-executed\\n' >&2\n");
    let _restore = RestorePolicy(rustpy_vcs::policy::current());
    rustpy_vcs::policy::configure(&rustpy_settings::VcsPreferences {
        sign_commits: true,
        signing_format: "ssh".into(),
        signing_key: key.to_str().unwrap().into(),
        staging_area: true,
        warn_crlf: false,
        ..Default::default()
    });
    let env = GitEnv {
        vars: vec![
            ("GIT_CONFIG_COUNT".into(), "0".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("SSH_AUTH_SOCK".into(), String::new()),
        ],
    };
    let repo = GitRepo::discover(root).unwrap();
    let file = root.join("owned.py");
    std::fs::write(&file, "value = 'initial'\n").unwrap();
    let stage_owned = || {
        let mut index = native.index().unwrap();
        index.add_path(Path::new("owned.py")).unwrap();
        index.write().unwrap();
    };
    stage_owned();
    let first = repo
        .commit_with_policy(&[], "initial signed fixture", false, &env)
        .unwrap();
    verify(root, &first);
    assert_eq!(
        std::fs::read_to_string(control.join("hook-events")).unwrap(),
        "pre-commit\ncommit-msg\n"
    );
    std::fs::write(&file, "value = 'staged'\n").unwrap();
    stage_owned();
    std::fs::write(&file, "value = 'unstaged retained'\n").unwrap();
    let extra = root.join("untracked.bin");
    std::fs::write(&extra, [0, 255, 17]).unwrap();
    let staged = checked_git(root, &["diff", "--cached", "--binary"]);
    let index = checked_git(root, &["ls-files", "--stage"]);
    let worktree = std::fs::read(&file).unwrap();
    let assert_preserved = || {
        assert_eq!(checked_git(root, &["rev-parse", "HEAD"]).trim(), first);
        assert_eq!(checked_git(root, &["diff", "--cached", "--binary"]), staged);
        assert_eq!(checked_git(root, &["ls-files", "--stage"]), index);
        assert_eq!(std::fs::read(&file).unwrap(), worktree);
        assert_eq!(std::fs::read(&extra).unwrap(), [0, 255, 17]);
    };
    std::fs::write(control.join("reject-precommit"), "owned rejection").unwrap();
    let error = repo
        .commit_with_policy(&[], "precheck denial", false, &env)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("rustpy-precommit-rejected"),
        "actual hook output missing: {error}"
    );
    assert_preserved();
    std::fs::remove_file(control.join("reject-precommit")).unwrap();
    let error = repo
        .commit_with_policy(&[], "reject fixture", false, &env)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("rustpy-commit-msg-rejected"),
        "actual commit-msg output missing: {error}"
    );
    assert_preserved();
    let second = repo
        .commit_with_policy(&[], "accepted signed fixture", false, &env)
        .unwrap();
    assert_ne!(second, first);
    verify(root, &second);
    assert_eq!(
        checked_git(root, &["show", "HEAD:owned.py"]),
        "value = 'staged'\n"
    );
    assert_eq!(std::fs::read(&file).unwrap(), worktree);
    assert!(checked_git(root, &["diff", "--cached", "--binary"]).is_empty());
    assert_eq!(
        std::fs::read_to_string(control.join("hook-events")).unwrap(),
        "pre-commit\ncommit-msg\npre-commit\npre-commit\ncommit-msg\npre-commit\ncommit-msg\n"
    );
}
