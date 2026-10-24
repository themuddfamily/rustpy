//! Built-in network Git operations: libgit2 transport backed by libssh2.
//! No ssh executable is used on this path. Credential prompts use the IDE bridge.
use crate::{GitEnv, RepositoryReservation};
use git2::{
    CertificateCheckStatus, Cred, CredentialType, FetchOptions, PushOptions, RemoteCallbacks,
    Repository,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, LazyLock, Mutex,
    },
};

static SESSION_HOSTS: LazyLock<Mutex<BTreeSet<(String, String)>>> = LazyLock::new(Mutex::default);
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NetworkPlan {
    Submodules {
        init: bool,
        recursive: bool,
    },
    Clone {
        url: String,
        target: PathBuf,
        depth: i32,
        branch: Option<String>,
    },
    Fetch {
        remotes: Vec<String>,
        specs: Vec<String>,
        depth: i32,
        prune: bool,
        tags: bool,
    },
    Push {
        remote: String,
        specs: Vec<String>,
        lease: bool,
        force: bool,
        set_upstream: bool,
        tags: bool,
        follow_tags: bool,
    },
    Pull {
        remote: String,
        upstream: String,
        rebase: bool,
        autostash: bool,
    },
}
pub(crate) struct Control {
    pub cancel: Arc<AtomicBool>,
    pub completion: Receiver<i32>,
}
fn ssh_url(url: &str) -> bool {
    url.starts_with("ssh://")
        || url.starts_with("git+ssh://")
        || (url.contains(':')
            && url
                .split(':')
                .next()
                .is_some_and(|prefix| !prefix.contains(['/', '\\']))
            && !url.contains("://")
            && !Path::new(url).is_absolute()
            && !url.as_bytes().get(1).is_some_and(|b| *b == b':'))
}
/// Push URLs override fetch URLs only when present; decoding errors are not
/// missing URLs and must never redirect authentication to another authority.
fn transport_url<'a>(remote: &'a git2::Remote<'_>, push: bool) -> Result<&'a str, git2::Error> {
    if push {
        if let Some(url) = remote.pushurl()? {
            return Ok(url);
        }
    }
    remote.url()
}
fn depth(args: &[String]) -> i32 {
    if args.iter().any(|arg| arg == "--unshallow") {
        return i32::MAX;
    }
    args.iter()
        .find_map(|arg| {
            arg.strip_prefix("--depth=")
                .and_then(|s| s.parse::<i32>().ok())
        })
        .unwrap_or(0)
        .max(0)
}
fn positional(args: &[String]) -> Vec<String> {
    args.iter()
        .skip(1)
        .filter(|s| !s.starts_with('-'))
        .cloned()
        .collect()
}
/// Returns None for local commands and non-SSH remotes. Never silently substitutes
/// native SSH when Built-in was selected for an SSH transfer.
pub(crate) fn plan(cwd: &Path, args: &[String]) -> anyhow::Result<Option<NetworkPlan>> {
    if crate::policy::current().ssh_transport != "builtin" {
        return Ok(None);
    }
    selected_plan(cwd, args)
}
fn selected_plan(cwd: &Path, args: &[String]) -> anyhow::Result<Option<NetworkPlan>> {
    let Some(operation) = args.first().map(String::as_str) else {
        return Ok(None);
    };
    if operation == "clone" {
        let mut values = Vec::new();
        let mut branch = None;
        let mut skip = false;
        let mut unsupported = Vec::new();
        for (i, arg) in args.iter().enumerate().skip(1) {
            if skip {
                skip = false;
                continue;
            }
            if arg == "--branch" || arg == "-b" {
                branch = args.get(i + 1).cloned();
                skip = true;
            } else if !arg.starts_with('-') {
                values.push(arg.clone());
            } else if arg != "--progress" && !arg.starts_with("--depth=") {
                unsupported.push(arg.clone());
            }
        }
        if values.first().is_none_or(|url| !ssh_url(url)) {
            return Ok(None);
        }
        anyhow::ensure!(
            unsupported.is_empty(),
            "Unsupported built-in clone option(s): {}; select Native SSH for this command",
            unsupported.join(", ")
        );
        anyhow::ensure!(
            values.len() == 2,
            "Built-in SSH clone requires a URL and target directory"
        );
        let target = PathBuf::from(&values[1]);
        return Ok(Some(NetworkPlan::Clone {
            url: values[0].clone(),
            target: if target.is_absolute() {
                target
            } else {
                cwd.join(target)
            },
            depth: depth(args),
            branch,
        }));
    }
    if operation == "submodule" && args.get(1).is_some_and(|arg| arg == "update") {
        anyhow::ensure!(
            args.iter()
                .skip(2)
                .all(|arg| matches!(arg.as_str(), "--init" | "--recursive" | "--progress")),
            "Unsupported built-in submodule update option; select Native SSH for this command"
        );
        return Ok(Some(NetworkPlan::Submodules {
            init: args.iter().any(|arg| arg == "--init"),
            recursive: args.iter().any(|arg| arg == "--recursive"),
        }));
    }
    if !matches!(operation, "fetch" | "push" | "pull") {
        return Ok(None);
    }
    let repo = Repository::discover(cwd)?;
    let values = positional(args);
    if operation == "push" && values.is_empty() {
        let handle = crate::GitRepo::discover(cwd)
            .ok_or_else(|| anyhow::anyhow!("Repository is not available"))?;
        let request = handle
            .push_target()
            .as_ref()
            .and_then(crate::PushRequest::from_target)
            .ok_or_else(|| anyhow::anyhow!("Configure a push remote and target branch first"))?;
        let mut normalized = request.args();
        let additional = args
            .iter()
            .skip(1)
            .filter(|arg| !normalized.contains(arg))
            .cloned()
            .collect::<Vec<_>>();
        normalized.extend(additional);
        return selected_plan(cwd, &normalized);
    }
    let selected = values.first().cloned().unwrap_or_else(|| "origin".into());
    let remote_ssh = |name: &str, push: bool| -> Result<bool, git2::Error> {
        let Ok(remote) = repo.find_remote(name) else {
            return Ok(false);
        };
        Ok(ssh_url(transport_url(&remote, push)?))
    };
    match operation {
        "fetch" => {
            let remotes = if args.iter().any(|s| s == "--all") {
                let names = repo.remotes()?;
                let mut remotes = Vec::new();
                for name in &names {
                    if let Some(name) = name? {
                        remotes.push(name.to_owned());
                    }
                }
                remotes
            } else {
                vec![selected]
            };
            let mut uses_ssh = false;
            for name in &remotes {
                uses_ssh |= remote_ssh(name, false)?;
            }
            if !uses_ssh {
                return Ok(None);
            }
            for arg in args.iter().skip(1).filter(|s| s.starts_with('-')) {
                anyhow::ensure!(
                    matches!(
                        arg.as_str(),
                        "--all"
                            | "--prune"
                            | "--progress"
                            | "--tags"
                            | "--no-tags"
                            | "--unshallow"
                            | "--"
                    ) || arg.starts_with("--depth="),
                    "Unsupported built-in fetch option {arg}; select Native SSH for this command"
                );
            }
            Ok(Some(NetworkPlan::Fetch {
                remotes,
                specs: values.into_iter().skip(1).collect(),
                depth: depth(args),
                prune: args.iter().any(|s| s == "--prune"),
                tags: args.iter().any(|s| s == "--tags"),
            }))
        }
        "push" => {
            if !remote_ssh(&selected, true)? {
                return Ok(None);
            }
            let hook = repo
                .config()?
                .get_path("core.hooksPath")
                .ok()
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        cwd.join(path)
                    }
                })
                .unwrap_or_else(|| repo.path().join("hooks"))
                .join("pre-push");
            anyhow::ensure!(
                args.iter().any(|arg| arg == "--no-verify") || !hook.is_file(),
                "This repository has a pre-push hook. Select Native SSH to run it, or explicitly disable push hooks for this operation"
            );
            for arg in args.iter().skip(1).filter(|s| s.starts_with('-')) {
                anyhow::ensure!(
                    matches!(
                        arg.as_str(),
                        "--porcelain"
                            | "--no-verify"
                            | "--progress"
                            | "--set-upstream"
                            | "-u"
                            | "--force-with-lease"
                            | "--force"
                            | "-f"
                            | "--tags"
                            | "--follow-tags"
                            | "--delete"
                            | "--"
                    ),
                    "Unsupported built-in push option {arg}; select Native SSH for this command"
                );
            }
            let delete = args.iter().any(|s| s == "--delete");
            let specs = values
                .into_iter()
                .skip(1)
                .map(|spec| {
                    if delete {
                        format!(
                            ":{}",
                            if spec.starts_with("refs/") {
                                spec
                            } else {
                                format!("refs/heads/{spec}")
                            }
                        )
                    } else if !spec.contains(':') && !spec.starts_with('+') {
                        let reference = if spec.starts_with("refs/") {
                            spec
                        } else {
                            format!("refs/heads/{spec}")
                        };
                        format!("{reference}:{reference}")
                    } else {
                        spec
                    }
                })
                .collect();
            Ok(Some(NetworkPlan::Push {
                remote: selected,
                specs,
                lease: args.iter().any(|s| s == "--force-with-lease"),
                force: args.iter().any(|s| matches!(s.as_str(), "--force" | "-f")),
                set_upstream: args
                    .iter()
                    .any(|s| matches!(s.as_str(), "--set-upstream" | "-u")),
                tags: args.iter().any(|s| s == "--tags"),
                follow_tags: args.iter().any(|s| s == "--follow-tags"),
            }))
        }
        "pull" => {
            let head = repo.head()?;
            anyhow::ensure!(head.is_branch(), "Cannot update detached HEAD");
            let branch = head.shorthand()?.to_owned();
            let config = repo.config()?;
            let remote = config.get_string(&format!("branch.{branch}.remote"))?;
            if !remote_ssh(&remote, false)? {
                return Ok(None);
            }
            anyhow::ensure!(
                values.is_empty(),
                "Built-in pull currently uses the configured upstream; use Update Project or select Native SSH for explicit pull targets"
            );
            let merge = config.get_string(&format!("branch.{branch}.merge"))?;
            let upstream = format!(
                "{}/{}",
                remote,
                merge.strip_prefix("refs/heads/").unwrap_or(&merge)
            );
            Ok(Some(NetworkPlan::Pull {
                remote,
                upstream,
                rebase: args.iter().any(|s| s == "--rebase"),
                autostash: args.iter().any(|s| s == "--autostash"),
            }))
        }
        _ => Ok(None),
    }
}
fn cancelled(cancel: &AtomicBool) -> Result<(), git2::Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(git2::Error::from_str("Operation cancelled"))
    } else {
        Ok(())
    }
}
fn prompt(
    env: &GitEnv,
    cancel: &AtomicBool,
    hint: &str,
    text: &str,
) -> Result<String, git2::Error> {
    cancelled(cancel)?;
    crate::askpass::ask_cancellable(env, cancel, hint, text)
        .map_err(|e| git2::Error::from_str(&e.to_string()))?
        .ok_or_else(|| git2::Error::from_str("Authentication cancelled"))
}
/// libssh2 extracts the public key before authenticating when no public-key
/// file is supplied. An encrypted key without its passphrase can therefore fail
/// with a file error, which libgit2 does not retry through its credential callback.
/// Inspect only the bounded, non-secret envelope; libssh2 still validates the key.
fn private_key_is_encrypted(key: &Path) -> Result<bool, git2::Error> {
    use std::io::Read;
    let mut prefix = Vec::new();
    std::fs::File::open(key)
        .and_then(|file| file.take(4096).read_to_end(&mut prefix))
        .map_err(|_| git2::Error::from_str("Cannot read the selected SSH private key"))?;
    encrypted_key_envelope(&prefix)
}
fn encrypted_key_envelope(prefix: &[u8]) -> Result<bool, git2::Error> {
    use base64::Engine;
    let invalid = || git2::Error::from_str("Invalid or unsupported SSH private-key envelope");
    let text = std::str::from_utf8(prefix).map_err(|_| invalid())?;
    let mut lines = text.lines();
    // Both libssh2's PEM reader and OpenSSL skip preamble lines.
    let header = lines
        .find(|line| line.starts_with("-----BEGIN "))
        .ok_or_else(invalid)?
        .trim_end();
    if header == "-----BEGIN ENCRYPTED PRIVATE KEY-----" {
        return Ok(true);
    }
    if header == "-----BEGIN OPENSSH PRIVATE KEY-----" {
        // openssh-key-v1 NUL, then an SSH length-prefixed cipher name. The
        // cipher name is public envelope metadata, preceding all key material.
        let encoded: Vec<u8> = lines
            .flat_map(str::bytes)
            .filter(|byte| !byte.is_ascii_whitespace())
            .take(128)
            .collect();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&encoded[..encoded.len() / 4 * 4])
            .map_err(|_| invalid())?;
        let rest = decoded
            .strip_prefix(b"openssh-key-v1\0")
            .ok_or_else(invalid)?;
        let length = u32::from_be_bytes(
            rest.get(..4)
                .ok_or_else(invalid)?
                .try_into()
                .map_err(|_| invalid())?,
        ) as usize;
        if !(1..=64).contains(&length) {
            return Err(invalid());
        }
        let cipher = rest.get(4..4 + length).ok_or_else(invalid)?;
        if !cipher.iter().all(|byte| {
            byte.is_ascii_alphanumeric() || *byte == b'-' || *byte == b'@' || *byte == b'.'
        }) {
            return Err(invalid());
        }
        return Ok(cipher != b"none");
    }
    if matches!(
        header,
        "-----BEGIN RSA PRIVATE KEY-----"
            | "-----BEGIN DSA PRIVATE KEY-----"
            | "-----BEGIN EC PRIVATE KEY-----"
            | "-----BEGIN PRIVATE KEY-----"
    ) {
        return Ok(lines.take_while(|line| !line.is_empty()).any(|line| {
            line.strip_prefix("Proc-Type:")
                .is_some_and(|value| value.trim() == "4,ENCRYPTED")
        }));
    }
    Err(invalid())
}
fn private_key_credential(
    username: &str,
    key: &Path,
    tried_plain_key: &mut bool,
    request_passphrase: impl FnOnce() -> Result<String, git2::Error>,
) -> Result<Cred, git2::Error> {
    let encrypted = private_key_is_encrypted(key)?;
    let needs_passphrase = encrypted || *tried_plain_key;
    *tried_plain_key = true;
    if needs_passphrase {
        let passphrase = request_passphrase()?;
        Cred::ssh_key(username, None, key, Some(&passphrase))
    } else {
        Cred::ssh_key(username, None, key, None)
    }
}
fn callbacks<'a>(
    repo: Option<&'a Repository>,
    endpoint: Option<String>,
    env: &'a GitEnv,
    cancel: &'a AtomicBool,
    tx: &'a Sender<crate::job::Chunk>,
) -> RemoteCallbacks<'a> {
    let mut callbacks = RemoteCallbacks::new();
    let policy = crate::policy::current();
    let mut attempts = 0;
    let mut tried_agent = false;
    let mut tried_helper = false;
    let mut tried_plain_key = false;
    callbacks.credentials(move |url,username,allowed| {
        cancelled(cancel)?;attempts+=1;
        if attempts>8 {return Err(git2::Error::from_str("Authentication failed after the available credentials were tried"));}
        let username=username.unwrap_or("git");
        if allowed.contains(CredentialType::USERNAME) {return Cred::username(&prompt(env,cancel,"",&format!("Username for {url}:"))?);}
        if allowed.contains(CredentialType::SSH_KEY) {
            if policy.ssh_key.is_empty() && !tried_agent {tried_agent=true;return Cred::ssh_key_from_agent(username);}
            let key=if !policy.ssh_key.is_empty(){PathBuf::from(&policy.ssh_key)}else{
                let home=std::env::var_os("HOME").or_else(||std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
                ["id_ed25519","id_ecdsa","id_rsa"].into_iter().map(|name|home.join(".ssh").join(name)).find(|path|path.is_file()).ok_or_else(||git2::Error::from_str("No SSH key was found; select a private key in Git settings or load one into the SSH agent"))?
            };
            return private_key_credential(username, &key, &mut tried_plain_key, || {
                prompt(env,cancel,"",&format!("Passphrase for SSH key '{}':",key.display()))
            });
        }
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            if policy.credential_helper && !tried_helper {tried_helper=true;
                if let Some(repo)=repo {if let Ok(config)=repo.config(){if let Ok(credential)=Cred::credential_helper(&config,url,Some(username)){return Ok(credential);}}}
            }
            let user=if username=="git" {prompt(env,cancel,"",&format!("Username for {url}:"))?}else{username.to_string()};
            let password=prompt(env,cancel,"",&format!("Password or HTTPS token for {user} at {url}:"))?;
            return Cred::userpass_plaintext(&user,&password);
        }
        if allowed.contains(CredentialType::DEFAULT){Cred::default()}else{Err(git2::Error::from_str("The server requested an unsupported credential type"))}
    });
    callbacks.certificate_check(move |cert,_host| {
        cancelled(cancel)?;
        let Some(key)=cert.as_hostkey() else {return Ok(CertificateCheckStatus::CertificatePassthrough)};
        let fingerprint=key.hash_sha256().map(|hash|hash.iter().map(|b|format!("{b:02x}")).collect::<String>()).ok_or_else(||git2::Error::from_str("The SSH server did not supply a SHA-256 host-key fingerprint"))?;
        let host = endpoint.as_deref().ok_or_else(|| git2::Error::from_str(
            "Cannot determine the SSH host/port authority; use an absolute SSH URL or select OpenSSH transport"
        ))?;
        let raw = key.hostkey().ok_or_else(|| git2::Error::from_str("SSH server supplied no raw host key"))?;
        let algorithm = key.hostkey_type().ok_or_else(|| git2::Error::from_str("SSH server supplied no host key algorithm"))?;
        let decision = crate::known_hosts::check(host, algorithm.name(), raw)
            .map_err(|error| git2::Error::from_str(&error))?;
        match decision {
            crate::known_hosts::Decision::Known => return Ok(CertificateCheckStatus::CertificateOk),
            crate::known_hosts::Decision::Changed | crate::known_hosts::Decision::Revoked =>
                return Err(git2::Error::from_str("SSH host key changed or was revoked. Connection refused; verify and update known_hosts independently.")),
            crate::known_hosts::Decision::Unknown => {}
        }
        let identity=(host.to_string(),fingerprint.clone());
        {
            let hosts = SESSION_HOSTS.lock().map_err(|_| git2::Error::from_str("SSH host trust is unavailable"))?;
            if hosts.contains(&identity) { return Ok(CertificateCheckStatus::CertificateOk); }
            if hosts.iter().any(|(saved, _)| saved == host) {
                return Err(git2::Error::from_str("SSH host key changed during this session. Connection refused."));
            }
        }
        let answer=prompt(env,cancel,"",&format!("Unknown SSH host '{host}'.\nSHA256 (hex): {fingerprint}\nVerify this fingerprint independently. Trust this key for this session (yes/no)?"))?;
        if answer.trim()!="yes" {return Err(git2::Error::from_str("SSH host key was not trusted"));}
        let mut hosts = SESSION_HOSTS.lock().map_err(|_| git2::Error::from_str("SSH host trust is unavailable"))?;
        // A concurrent first-use prompt cannot replace an already accepted key.
        if hosts.iter().any(|(saved, hash)| saved == host && hash != &fingerprint) {
            return Err(git2::Error::from_str("SSH host key changed while awaiting confirmation"));
        }
        hosts.insert(identity);
        Ok(CertificateCheckStatus::CertificateOk)
    });
    callbacks.transfer_progress(move |progress| {
        let _ = tx.send(crate::job::Chunk::Progress(format!(
            "Receiving objects: {}/{} ({} bytes)",
            progress.received_objects(),
            progress.total_objects(),
            progress.received_bytes()
        )));
        !cancel.load(Ordering::Relaxed)
    });
    callbacks.sideband_progress(move |bytes| {
        let _ = tx.send(crate::job::Chunk::Line(
            String::from_utf8_lossy(bytes).trim().to_string(),
        ));
        !cancel.load(Ordering::Relaxed)
    });
    callbacks.push_transfer_progress(move |current, total, bytes| {
        let _ = tx.send(crate::job::Chunk::Progress(format!(
            "Sending objects: {current}/{total} ({bytes} bytes)"
        )));
    });
    callbacks.push_update_reference(move |name, status| {
        cancelled(cancel)?;
        if let Some(reason) = status {
            let _ = tx.send(crate::job::Chunk::Line(format!(
                "!\t{name}\t[rejected] {reason}"
            )));
            return Err(git2::Error::from_str(reason));
        }
        let _ = tx.send(crate::job::Chunk::Line(format!(" \t{name}\t[updated]")));
        Ok(())
    });
    callbacks
}
fn update_submodules(
    repo: &Repository,
    init: bool,
    recursive: bool,
    level: usize,
    env: &GitEnv,
    cancel: &AtomicBool,
    tx: &Sender<crate::job::Chunk>,
) -> anyhow::Result<()> {
    anyhow::ensure!(level < 32, "Submodule nesting exceeds the supported depth");
    for mut module in repo.submodules()? {
        cancelled(cancel)?;
        let _ = tx.send(crate::job::Chunk::Line(format!(
            "Updating submodule {} with built-in transport",
            module.path().display()
        )));
        let url = module.url()?.map(str::to_owned);
        let mut fetch = FetchOptions::new();
        fetch.remote_callbacks(callbacks(
            Some(repo),
            crate::known_hosts::endpoint(url.as_deref()),
            env,
            cancel,
            tx,
        ));
        if let Some(header) = url.as_deref().and_then(crate::policy::github_header) {
            fetch.custom_headers(&[header.as_str()]);
        }
        let mut options = git2::SubmoduleUpdateOptions::new();
        options.fetch(fetch);
        module.update(init, Some(&mut options))?;
        cancelled(cancel)?;
        if recursive {
            update_submodules(&module.open()?, init, recursive, level + 1, env, cancel, tx)?;
        }
    }
    Ok(())
}
struct FetchPolicy {
    depth: i32,
    prune: bool,
    tags: bool,
}
fn fetch(
    repo: &Repository,
    remote: &str,
    specs: &[String],
    policy: FetchPolicy,
    env: &GitEnv,
    cancel: &AtomicBool,
    tx: &Sender<crate::job::Chunk>,
) -> anyhow::Result<()> {
    cancelled(cancel)?;
    let _ = tx.send(crate::job::Chunk::Line(format!(
        "Fetching {remote} with built-in libgit2 transport"
    )));
    let mut remote = repo.find_remote(remote)?;
    let url = remote.url()?.to_owned();
    let endpoint = crate::known_hosts::endpoint(Some(&url));
    let mut options = FetchOptions::new();
    options
        .remote_callbacks(callbacks(Some(repo), endpoint, env, cancel, tx))
        .depth(policy.depth);
    if policy.prune {
        options.prune(git2::FetchPrune::On);
    }
    if policy.tags {
        options.download_tags(git2::AutotagOption::All);
    }
    if let Some(header) = crate::policy::github_header(&url) {
        options.custom_headers(&[header.as_str()]);
    }
    remote.fetch(specs, Some(&mut options), None)?;
    Ok(())
}
fn execute(
    root: &Path,
    plan: NetworkPlan,
    env: &GitEnv,
    cancel: &AtomicBool,
    tx: &Sender<crate::job::Chunk>,
) -> anyhow::Result<()> {
    if let NetworkPlan::Clone {
        url,
        target,
        depth,
        branch,
    } = plan
    {
        let mut options = FetchOptions::new();
        options
            .remote_callbacks(callbacks(
                None,
                crate::known_hosts::endpoint(Some(&url)),
                env,
                cancel,
                tx,
            ))
            .depth(depth);
        let mut builder = git2::build::RepoBuilder::new();
        builder.fetch_options(options);
        if let Some(branch) = branch {
            builder.branch(&branch);
        }
        builder.clone(&url, &target)?;
        let _ = tx.send(crate::job::Chunk::Line(format!(
            "Cloned into {}",
            target.display()
        )));
        return Ok(());
    }
    let repo = Repository::discover(root)?;
    match plan {
        NetworkPlan::Submodules { init, recursive } => {
            update_submodules(&repo, init, recursive, 0, env, cancel, tx)?;
        }
        NetworkPlan::Fetch {
            remotes,
            specs,
            depth,
            prune,
            tags,
        } => {
            for remote in remotes {
                fetch(
                    &repo,
                    &remote,
                    &specs,
                    FetchPolicy { depth, prune, tags },
                    env,
                    cancel,
                    tx,
                )?;
            }
        }
        NetworkPlan::Pull {
            remote,
            upstream,
            rebase,
            autostash,
        } => {
            fetch(
                &repo,
                &remote,
                &[],
                FetchPolicy {
                    depth: 0,
                    prune: true,
                    tags: false,
                },
                env,
                cancel,
                tx,
            )?;
            cancelled(cancel)?;
            let handle = crate::GitRepo::discover(root)
                .ok_or_else(|| anyhow::anyhow!("Repository disappeared"))?;
            let result = handle.integrate(
                &upstream,
                if rebase {
                    crate::UpdateMethod::Rebase
                } else {
                    crate::UpdateMethod::Merge
                },
                autostash,
            )?;
            let _ = tx.send(crate::job::Chunk::Line(format!(
                "Integrated {} incoming commit(s); {} conflict(s). {}",
                result.incoming,
                result.conflicts.len(),
                result.detail
            )));
            anyhow::ensure!(
                result.conflicts.is_empty(),
                "Update stopped with conflicts; resolve them in Local Changes"
            );
        }
        NetworkPlan::Push {
            remote: remote_name,
            mut specs,
            lease,
            force,
            set_upstream,
            tags,
            follow_tags,
        } => {
            let mut remote = repo.find_remote(&remote_name)?;
            let endpoint = crate::known_hosts::endpoint(Some(transport_url(&remote, true)?));
            let mut expected = BTreeMap::new();
            if specs.is_empty() && !tags {
                anyhow::bail!("Select an explicit branch or tag to push with built-in SSH");
            }
            if tags || follow_tags {
                let existing = {
                    let connection = remote.connect_auth(
                        git2::Direction::Push,
                        Some(callbacks(Some(&repo), endpoint.clone(), env, cancel, tx)),
                        None,
                    )?;
                    connection
                        .list()?
                        .iter()
                        .map(|head| head.name().to_string())
                        .collect::<BTreeSet<_>>()
                };
                for reference in repo.references_glob("refs/tags/*")? {
                    let reference = reference?;
                    let name = reference.name()?;
                    if tags {
                        specs.push(format!("{name}:{name}"));
                    } else if !existing.contains(name)
                        && reference
                            .target()
                            .and_then(|oid| repo.find_tag(oid).ok())
                            .is_some()
                    {
                        if let Ok(tag) = reference.peel_to_commit() {
                            let reachable = specs
                                .iter()
                                .filter_map(|spec| spec.split(':').next())
                                .filter_map(|source| repo.revparse_single(source).ok())
                                .filter_map(|object| object.peel_to_commit().ok())
                                .any(|head| {
                                    head.id() == tag.id()
                                        || repo
                                            .graph_descendant_of(head.id(), tag.id())
                                            .unwrap_or(false)
                                });
                            if reachable {
                                specs.push(format!("{name}:{name}"));
                            }
                        }
                    }
                }
            }
            if force || lease {
                for spec in &mut specs {
                    let (_, destination) = spec
                        .trim_start_matches('+')
                        .split_once(':')
                        .ok_or_else(|| {
                            anyhow::anyhow!("An explicit destination ref is required")
                        })?;
                    if let Some(branch) = destination.strip_prefix("refs/heads/") {
                        anyhow::ensure!(
                            !crate::policy::protected(
                                branch,
                                &crate::policy::current().protected_branches
                            ),
                            "Cannot force-push protected branch {branch}"
                        );
                    }
                    if lease {
                        let branch = destination.strip_prefix("refs/heads/").ok_or_else(|| {
                            anyhow::anyhow!("Force-with-lease is only supported for branch refs")
                        })?;
                        let tracking = format!("refs/remotes/{remote_name}/{branch}");
                        expected.insert(
                            destination.to_string(),
                            repo.refname_to_id(&tracking)
                                .unwrap_or(git2::Oid::ZERO_SHA1),
                        );
                    }
                    if !spec.starts_with('+') {
                        *spec = format!("+{spec}");
                    }
                }
            }
            let mut cb = callbacks(Some(&repo), endpoint.clone(), env, cancel, tx);
            cb.push_negotiation(move |updates| {cancelled(cancel)?;for update in updates {if let Some(expected)=expected.get(update.dst_refname()?) {if update.src()!=*expected {return Err(git2::Error::from_str("[rejected] stale force-with-lease: remote branch changed; fetch and review before pushing"));}}}Ok(())});
            let mut options = PushOptions::new();
            options.remote_callbacks(cb);
            if let Err(error) = remote.push(&specs, Some(&mut options)) {
                if error.code() == git2::ErrorCode::NotFastForward {
                    anyhow::bail!("[rejected] non-fast-forward: {error}");
                }
                return Err(error.into());
            }
            if set_upstream {
                let mut config = repo.config()?;
                for spec in specs {
                    let (source, destination) = spec
                        .trim_start_matches('+')
                        .split_once(':')
                        .unwrap_or((&spec, &spec));
                    if let (Some(local), Some(remote_branch)) = (
                        source.strip_prefix("refs/heads/"),
                        destination.strip_prefix("refs/heads/"),
                    ) {
                        config.set_str(&format!("branch.{local}.remote"), &remote_name)?;
                        config.set_str(&format!("branch.{local}.merge"), destination)?;
                        if let Ok(oid) = repo.refname_to_id(source) {
                            repo.reference(
                                &format!("refs/remotes/{remote_name}/{remote_branch}"),
                                oid,
                                true,
                                "RustPy push tracking",
                            )?;
                        }
                    }
                }
            }
        }
        NetworkPlan::Clone { .. } => unreachable!(),
    }
    Ok(())
}
pub(crate) fn start(
    root: PathBuf,
    plan: NetworkPlan,
    env: GitEnv,
    reservation: RepositoryReservation,
    output: Sender<crate::job::Chunk>,
) -> Control {
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let (tx, completion) = mpsc::channel();
    std::thread::spawn(move || {
        let result = reservation.run(|| execute(&root, plan, &env, &worker_cancel, &output));
        let code = match result {
            Ok(()) => 0,
            Err(error) => {
                let _ = output.send(crate::job::Chunk::Line(format!("error: {error}")));
                if worker_cancel.load(Ordering::Relaxed) {
                    130
                } else {
                    1
                }
            }
        };
        drop(reservation);
        let _ = tx.send(code);
    });
    Control { cancel, completion }
}

/// Synchronous transport entry for already-owned background GitRepo workflows.
/// The caller's reservation remains held throughout fetch/integrate cleanup.
pub(crate) fn run_inline(
    root: &Path,
    args: &[String],
    env: &GitEnv,
) -> Option<anyhow::Result<String>> {
    let selected = match plan(root, args) {
        Ok(Some(plan)) => plan,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    Some(execute_capture(
        root,
        selected,
        env,
        &AtomicBool::new(false),
    ))
}
fn execute_capture(
    root: &Path,
    selected: NetworkPlan,
    env: &GitEnv,
    cancel: &AtomicBool,
) -> anyhow::Result<String> {
    let (tx, rx) = mpsc::channel();
    let collector = std::thread::spawn(move || {
        let mut lines = std::collections::VecDeque::new();
        while let Ok(chunk) = rx.recv() {
            if let crate::job::Chunk::Line(line) = chunk {
                lines.push_back(line);
                while lines.len() > 2_000 {
                    lines.pop_front();
                }
            }
        }
        lines.into_iter().collect::<Vec<_>>().join("\n")
    });
    let result = execute(root, selected, env, cancel, &tx);
    drop(tx);
    let output = collector.join().unwrap_or_default();
    result.map(|()| output)
}
pub(crate) fn run_cancellable(
    root: &Path,
    args: &[String],
    env: &GitEnv,
    token: &rustpy_core::CancellationToken,
) -> Option<Result<String, rustpy_core::TaskError>> {
    let selected = match plan(root, args) {
        Ok(Some(plan)) => plan,
        Ok(None) => return None,
        Err(e) => return Some(Err(rustpy_core::TaskError::Failed(e.to_string()))),
    };
    let cancel = Arc::new(AtomicBool::new(token.is_cancelled()));
    let finished = Arc::new(AtomicBool::new(false));
    let (watch_cancel, watch_finished, watch_token) =
        (cancel.clone(), finished.clone(), token.clone());
    let monitor = std::thread::spawn(move || {
        while !watch_finished.load(Ordering::Relaxed) {
            if watch_token.is_cancelled() {
                watch_cancel.store(true, Ordering::Relaxed);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
    let result = execute_capture(root, selected, env, &cancel);
    finished.store(true, Ordering::Relaxed);
    let _ = monitor.join();
    Some(if token.is_cancelled() {
        Err(rustpy_core::TaskError::Cancelled)
    } else {
        result.map_err(|e| rustpy_core::TaskError::Failed(e.to_string()))
    })
}
#[cfg(test)]
mod tests {
    // Envelope-only fixtures intentionally contain no usable private key. These
    // tests exercise the decision before Cred/libssh2 receives the key, rather
    // than assuming libgit2 retries a failed private-key extraction.
    fn openssh_envelope(cipher: &str) -> String {
        use base64::Engine;
        let mut bytes = b"openssh-key-v1\0".to_vec();
        bytes.extend_from_slice(&(cipher.len() as u32).to_be_bytes());
        bytes.extend_from_slice(cipher.as_bytes());
        bytes.resize(192, 0);
        format!(
            "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }
    #[test]
    fn encrypted_envelopes_prompt_before_first_credential_without_public_key_hint() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("selected-key");
        let envelopes = [
            "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,0123456789ABCDEF\n\nopaque\n".into(),
            "-----BEGIN EC PRIVATE KEY-----\r\nProc-Type: 4,ENCRYPTED\r\n\r\nopaque\r\n".into(),
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nopaque\n".into(),
            openssh_envelope("aes256-ctr"),
            openssh_envelope("aes256-gcm@openssh.com"),
        ];
        for envelope in envelopes {
            std::fs::write(&key, envelope).unwrap();
            let mut tried_plain = false;
            let mut prompts = 0;
            let credential = super::private_key_credential("git", &key, &mut tried_plain, || {
                prompts += 1;
                Ok("fixture-only-passphrase".into())
            })
            .unwrap();
            assert_eq!(
                prompts, 1,
                "first credential must already carry the passphrase"
            );
            assert!(tried_plain);
            assert!(credential.has_username());
            assert!(!key.with_extension("pub").exists());
        }
    }
    #[test]
    fn unencrypted_keys_need_no_initial_prompt_and_encrypted_prompt_can_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("selected-key");
        for envelope in [
            "-----BEGIN RSA PRIVATE KEY-----\nopaque\n".into(),
            "-----BEGIN PRIVATE KEY-----\nopaque\n".into(),
            "Exported SSH key\n\n-----BEGIN EC PRIVATE KEY-----\nopaque\n".into(),
            format!("Exported SSH key\n{}", openssh_envelope("none")),
            openssh_envelope("none"),
        ] {
            std::fs::write(&key, envelope).unwrap();
            let mut tried_plain = false;
            super::private_key_credential("git", &key, &mut tried_plain, || {
                panic!("unencrypted first attempt must not prompt")
            })
            .unwrap();
            let error = super::private_key_credential("git", &key, &mut tried_plain, || {
                Err(git2::Error::from_str("Authentication cancelled"))
            })
            .err()
            .unwrap();
            assert_eq!(error.message(), "Authentication cancelled");
        }
        std::fs::write(&key, openssh_envelope("aes256-ctr")).unwrap();
        let error = super::private_key_credential("git", &key, &mut false, || {
            Err(git2::Error::from_str("Operation cancelled"))
        })
        .err()
        .unwrap();
        assert_eq!(error.message(), "Operation cancelled");
    }
    #[test]
    fn key_envelope_inspection_is_bounded_and_does_not_echo_key_data() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("selected-key");
        let mut envelope = openssh_envelope("none").into_bytes();
        envelope.resize(4096, b'\n');
        envelope.extend_from_slice(&[0xff; 4096]);
        std::fs::write(&key, envelope).unwrap();
        assert!(!super::private_key_is_encrypted(&key).unwrap());
        for malformed in [
            "private-secret-material".to_string(),
            "-----BEGIN OPENSSH PRIVATE KEY-----\n!!!!private-secret-material\n".into(),
        ] {
            std::fs::write(&key, malformed).unwrap();
            let mut prompted = false;
            let error = super::private_key_credential("git", &key, &mut false, || {
                prompted = true;
                Ok("secret".into())
            })
            .err()
            .unwrap();
            assert!(!prompted);
            assert_eq!(
                error.message(),
                "Invalid or unsupported SSH private-key envelope"
            );
        }
        assert!(super::private_key_is_encrypted(&dir.path().join("missing")).is_err());
    }
    #[test]
    fn parses_real_fetch_depth_and_force_lease_push_targets() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        repo.remote("origin", "ssh://git@example.test/team/repo.git")
            .unwrap();
        let args = ["fetch", "--all", "--prune", "--depth=25"].map(str::to_string);
        assert!(matches!(
            super::selected_plan(dir.path(), &args).unwrap(),
            Some(super::NetworkPlan::Fetch {
                depth: 25,
                prune: true,
                ..
            })
        ));
        let args = [
            "push",
            "--force-with-lease",
            "--set-upstream",
            "origin",
            "refs/heads/topic:refs/heads/topic",
        ]
        .map(str::to_string);
        assert!(matches!(
            super::selected_plan(dir.path(), &args).unwrap(),
            Some(super::NetworkPlan::Push {
                lease: true,
                set_upstream: true,
                ..
            })
        ));
    }
    #[test]
    fn ssh_url_classification_does_not_mistake_windows_or_https_paths_for_ssh() {
        assert!(super::ssh_url("git@host:team/repo.git"));
        assert!(super::ssh_url("ssh://host/team/repo.git"));
        assert!(!super::ssh_url("https://host/repo.git"));
        assert!(!super::ssh_url("C:\\projects\\repo"));
        assert!(!super::ssh_url("/tmp/repo.git"));
        assert!(!super::ssh_url("./relative/path:repo.git"));
    }
    #[test]
    fn selected_push_authority_is_independent_of_fetch_authority() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        repo.remote("origin", "https://fetch.example.test/team/repo.git")
            .unwrap();
        repo.remote_set_pushurl(
            "origin",
            Some("ssh://git@push.example.test:2222/team/repo.git"),
        )
        .unwrap();
        let remote = repo.find_remote("origin").unwrap();
        assert_eq!(
            super::transport_url(&remote, false).unwrap(),
            "https://fetch.example.test/team/repo.git"
        );
        assert_eq!(
            crate::known_hosts::endpoint(Some(super::transport_url(&remote, true).unwrap()))
                .as_deref(),
            Some("[push.example.test]:2222")
        );
        let args = ["push", "origin", "refs/heads/topic:refs/heads/topic"].map(str::to_owned);
        assert!(matches!(
            super::selected_plan(dir.path(), &args).unwrap(),
            Some(super::NetworkPlan::Push { .. })
        ));
        drop(remote);
        repo.remote_set_pushurl("origin", None).unwrap();
        let remote = repo.find_remote("origin").unwrap();
        assert_eq!(
            super::transport_url(&remote, true).unwrap(),
            super::transport_url(&remote, false).unwrap()
        );
    }
    #[test]
    fn invalid_push_url_encoding_never_falls_back_to_fetch_host() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        repo.remote("origin", "ssh://git@fetch.example.test/repo.git")
            .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(repo.path().join("config"))
            .unwrap()
            .write_all(b"\n[remote \"origin\"]\n pushurl = ssh://git@wrong.example.test/\xff\n")
            .unwrap();
        let reopened = git2::Repository::open(dir.path()).unwrap();
        let remote = reopened.find_remote("origin").unwrap();
        assert!(super::transport_url(&remote, true).is_err());
        assert!(super::transport_url(&remote, false).is_ok());
    }
    #[test]
    fn detached_head_pull_does_not_interpret_head_as_a_branch_name() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Fixture", "fixture@example.test").unwrap();
        let oid = repo
            .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        repo.set_head_detached(oid).unwrap();
        let error = super::selected_plan(dir.path(), &["pull".into()]).unwrap_err();
        assert!(error.to_string().contains("detached HEAD"));
    }
}
