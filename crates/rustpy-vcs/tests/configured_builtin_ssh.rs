//! ROOT-only genuine libgit2/libssh2 acceptance against a disposable sshd.
//! No provider is provisioned/launched here. ROOT prepares an encrypted RSA
//! PEM identity, a private foreground sshd accepting only that public key, and
//! fresh case directory/marker using row163/builtin-ssh-provider-preparation.
//! No HOME/global known_hosts/config changes. Unknown-host refusal and explicit
//! session trust are live; persistent known/changed-file classification remains
//! covered separately by known_hosts fixtures, not claimed by this test.
#![cfg(target_os = "linux")]

use base64::Engine as _;
use git2::{Oid, Repository, Signature};
use rustpy_vcs::{askpass::AskpassServer, policy, GitEnv, GitRepo};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread::JoinHandle,
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(30);
fn required(name: &str) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} required"));
    assert!(!value.is_empty() && !value.contains(['\r', '\n', '\0']));
    value
}
fn file(name: &str, root: &Path) -> PathBuf {
    let value = PathBuf::from(required(name));
    assert!(value.is_absolute() && value.is_file());
    let value = value.canonicalize().unwrap();
    assert!(
        value.starts_with(root),
        "fixture identity must be privately owned"
    );
    value
}
struct Policy {
    previous: rustpy_settings::VcsPreferences,
    git_program: String,
}
impl Drop for Policy {
    fn drop(&mut self) {
        policy::configure(&self.previous);
        rustpy_vcs::set_git_program(&self.git_program);
    }
}
// Sanitize before an error crosses the worker channel, so panic formatting
// cannot expose the session passphrase, askpass bearer token or private key path.
fn sanitized(mut message: String, secrets: &[String]) -> String {
    if message.contains("PRIVATE KEY-----") {
        return "provider error contained private-key material; detail redacted".into();
    }
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        message = message.replace(secret, "<redacted>");
        let escaped = secret.escape_debug().to_string();
        if escaped != *secret {
            message = message.replace(&escaped, "<redacted>");
        }
    }
    message
}
struct Worker {
    thread: Option<JoinHandle<()>>,
    result: Receiver<Result<String, String>>,
}
impl Worker {
    fn start(root: PathBuf, args: Vec<String>, environment: GitEnv, secrets: Vec<String>) -> Self {
        let (tx, result) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            // This public API dispatches to the same selected built-in network
            // planner/callbacks used by GitJob; no direct test remote callbacks.
            let result = GitRepo::run_git_in_with_environment(&root, &args, &environment)
                .map_err(|error| sanitized(format!("{error:#}"), &secrets));
            let _ = tx.send(result);
        });
        Self {
            thread: Some(thread),
            result,
        }
    }
    fn join(&mut self) {
        assert!(
            self.thread.take().unwrap().join().is_ok(),
            "native Git worker panicked"
        );
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            // The prompt owner below cancels pending requests first on unwind;
            // retain the actual library worker until it physically returns.
            let _ = thread.join();
        }
    }
}
#[derive(Default)]
struct Prompts {
    hosts: usize,
    passphrases: usize,
}
struct Operation<'a> {
    worker: Worker,
    server: &'a AskpassServer,
}
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        while self
            .worker
            .thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
        {
            while let Some(prompt) = self.server.try_recv() {
                prompt.cancel();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
struct PromptAuthority<'a> {
    server: &'a AskpassServer,
    host: &'a str,
    fingerprint: &'a str,
    key: &'a Path,
    passphrase: &'a str,
}
fn operation(
    root: &Path,
    args: &[String],
    job: u64,
    authority: &PromptAuthority<'_>,
    trust: bool,
    expected_success: bool,
) -> Prompts {
    let PromptAuthority {
        server,
        host,
        fingerprint,
        key,
        passphrase,
    } = authority;
    let environment = GitEnv {
        vars: server.env(&root.join("not-an-askpass-executable"), job),
    };
    let mut secrets = vec![
        (*passphrase).to_string(),
        key.to_string_lossy().into_owned(),
    ];
    if let Ok(token) = std::env::var("RUSTPY_BUILTIN_SSH_TOKEN") {
        secrets.push(token);
    }
    secrets.extend(
        environment
            .vars
            .iter()
            .filter(|(name, _)| name == rustpy_vcs::askpass::TOKEN_ENV)
            .map(|(_, value)| value.clone()),
    );
    let mut operation = Operation {
        worker: Worker::start(root.to_path_buf(), args.to_vec(), environment, secrets),
        server,
    };
    let mut observed = Prompts::default();
    let deadline = Instant::now() + WAIT;
    loop {
        while let Some(prompt) = server.try_recv() {
            assert_eq!(
                prompt.job, job,
                "prompt belongs to another captured operation"
            );
            if prompt.prompt.starts_with("Unknown SSH host '") {
                observed.hosts += 1;
                assert!(prompt
                    .prompt
                    .contains(&format!("Unknown SSH host '{host}'")));
                assert!(
                    prompt
                        .prompt
                        .contains(&format!("SHA256 (hex): {fingerprint}")),
                    "prompt must contain the actual independently captured host key"
                );
                prompt.answer(if trust { "yes" } else { "no" });
            } else if prompt.prompt.starts_with("Passphrase for SSH key '") {
                observed.passphrases += 1;
                assert!(trust && prompt.prompt.contains(key.to_str().unwrap()));
                prompt.answer(passphrase);
            } else {
                prompt.cancel();
                panic!("unexpected credential prompt; no unverified answer supplied");
            }
        }
        match operation.worker.result.try_recv() {
            Ok(result) => {
                operation.worker.join();
                assert_eq!(
                    result.is_ok(),
                    expected_success,
                    "actual built-in SSH job={job} operation={} hosts={} passphrase_prompts={} error={}",
                    args.first().map(String::as_str).unwrap_or("<missing>"),
                    observed.hosts,
                    observed.passphrases,
                    result.as_ref().err().map(String::as_str).unwrap_or("<none>"),
                );
                return observed;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("native operation exited without result")
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        assert!(
            Instant::now() < deadline,
            "actual built-in SSH operation deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn commit(repo: &Repository, parent: Option<Oid>, text: &[u8]) -> Oid {
    let blob = repo.blob(text).unwrap();
    let mut builder = repo.treebuilder(None).unwrap();
    builder.insert("value.txt", blob, 0o100644).unwrap();
    let tree_id = builder.write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = Signature::now("Owned transport fixture", "owned@example.test").unwrap();
    let parent = parent.map(|id| repo.find_commit(id).unwrap());
    repo.commit(
        Some("refs/heads/main"),
        &signature,
        &signature,
        "Owned transport value",
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
#[ignore = "explicit ROOT-owned real loopback sshd, encrypted key and bare Git remote only"]
fn configured_builtin_ssh_refuses_unknown_then_clones_fetches_pushes_with_encrypted_key() {
    assert_eq!(required("RUSTPY_BUILTIN_SSH_ADMITTED"), "yes");
    let root = PathBuf::from(required("RUSTPY_BUILTIN_SSH_ROOT"));
    assert!(root.is_absolute() && root.is_dir());
    let root = root.canonicalize().unwrap();
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("owner.json")).unwrap()).unwrap();
    assert_eq!(marker["root"], root.to_str().unwrap());
    assert_eq!(marker["token"], required("RUSTPY_BUILTIN_SSH_TOKEN"));
    let key = file("RUSTPY_BUILTIN_SSH_KEY", &root);
    let public_host = file("RUSTPY_BUILTIN_SSH_HOST_PUBLIC_KEY", &root);
    let text = std::fs::read_to_string(public_host).unwrap();
    let fields: Vec<_> = text.split_whitespace().collect();
    assert!(fields.len() >= 2);
    let raw_key = base64::engine::general_purpose::STANDARD
        .decode(fields[1])
        .unwrap();
    let fingerprint = rustpy_net::x509::sha256(&raw_key)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let passphrase = required("RUSTPY_BUILTIN_SSH_PASSPHRASE");
    let port: u16 = required("RUSTPY_BUILTIN_SSH_PORT").parse().unwrap();
    assert!(port > 1024);
    let host = format!("[127.0.0.1]:{port}");
    let bare_path = root.join("remote.git");
    assert!(
        !bare_path.exists(),
        "must not overwrite any existing repository"
    );
    let bare = Repository::init_bare(&bare_path).unwrap();
    bare.set_head("refs/heads/main").unwrap();
    let first = commit(&bare, None, b"42\n");
    let url = format!("ssh://root@127.0.0.1:{port}{}", bare_path.to_str().unwrap());
    let previous = policy::current();
    let _policy = Policy {
        previous: previous.clone(),
        git_program: rustpy_vcs::git_program(),
    };
    let mut configured = previous;
    configured.ssh_transport = "builtin".into();
    configured.ssh_key = key.to_str().unwrap().into();
    configured.credential_helper = false;
    configured.sign_commits = false;
    configured.ssh_executable = root
        .join("no-native-ssh-executable")
        .to_str()
        .unwrap()
        .into();
    policy::configure(&configured);
    rustpy_vcs::set_git_program(root.join("no-git-cli-executable").to_str().unwrap());
    let server = AskpassServer::start().unwrap();
    let authority = PromptAuthority {
        server: &server,
        host: &host,
        fingerprint: &fingerprint,
        key: &key,
        passphrase: &passphrase,
    };
    let rejected_path = root.join("rejected-clone");
    let rejected = operation(
        &root,
        &[
            "clone".into(),
            url.clone(),
            rejected_path.to_str().unwrap().into(),
        ],
        1,
        &authority,
        false,
        false,
    );
    assert_eq!(rejected.hosts, 1);
    assert_eq!(rejected.passphrases, 0);
    assert!(!rejected_path.join("value.txt").exists());
    let clone_path = root.join("accepted-clone");
    let accepted = operation(
        &root,
        &["clone".into(), url, clone_path.to_str().unwrap().into()],
        2,
        &authority,
        true,
        true,
    );
    assert_eq!(accepted.hosts, 1);
    assert!(
        accepted.passphrases >= 1,
        "encrypted private key must use real credential callback"
    );
    assert_eq!(
        std::fs::read(clone_path.join("value.txt")).unwrap(),
        b"42\n"
    );
    let repo = GitRepo::discover(&clone_path).expect("actual public GitRepo clone");
    assert_eq!(repo.current_branch().as_deref(), Some("main"));
    let second = commit(&bare, Some(first), b"43\n");
    let fetched = operation(
        &clone_path,
        &["fetch".into(), "origin".into()],
        3,
        &authority,
        true,
        true,
    );
    assert_eq!(
        fetched.hosts, 0,
        "session host trust is reused, not silently approved again"
    );
    assert!(fetched.passphrases >= 1);
    let cloned = Repository::open(&clone_path).unwrap();
    assert_eq!(
        cloned.refname_to_id("refs/remotes/origin/main").unwrap(),
        second
    );
    let second_object = cloned.find_object(second, None).unwrap();
    cloned
        .reset(&second_object, git2::ResetType::Hard, None)
        .unwrap();
    let third = commit(&cloned, Some(second), b"44\n");
    let pushed = operation(
        &clone_path,
        &[
            "push".into(),
            "--no-verify".into(),
            "origin".into(),
            "refs/heads/main:refs/heads/main".into(),
        ],
        4,
        &authority,
        true,
        true,
    );
    assert_eq!(pushed.hosts, 0);
    assert!(pushed.passphrases >= 1);
    assert_eq!(bare.refname_to_id("refs/heads/main").unwrap(), third);
    let tree = bare.find_commit(third).unwrap().tree().unwrap();
    let entry = tree.get_name("value.txt").unwrap();
    assert_eq!(bare.find_blob(entry.id()).unwrap().content(), b"44\n");
    assert!(server.try_recv().is_none());
    assert!(
        rustpy_vcs::running_operation(&clone_path).is_none(),
        "physical worker retirement releases its reservation"
    );
    // No CLI/helper executable exists: successful transfers cannot be an
    // OpenSSH/Git CLI substitution for the selected library transport.
    assert!(!Path::new(&policy::current().ssh_executable).exists());
    assert!(!Path::new(&rustpy_vcs::git_program()).exists());
}
