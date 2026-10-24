//! Git policy shared by CLI jobs and local commit operations.
use crate::{GitEnv, GitRepo};
use rustpy_settings::VcsPreferences;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{LazyLock, RwLock},
};
static POLICY: LazyLock<RwLock<VcsPreferences>> =
    LazyLock::new(|| RwLock::new(VcsPreferences::default()));
static GITHUB_AUTH: LazyLock<RwLock<BTreeMap<String, String>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));
pub fn github_authorization(header: String) {
    let _ = github_authorization_for("https://github.com", header);
}
fn https_origin(value: &str) -> anyhow::Result<String> {
    let url = url::Url::parse(value)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "Use an HTTPS origin without credentials, path, query or fragment"
    );
    Ok(url.origin().ascii_serialization())
}
/// Installs or removes a session-only Git authentication header for one exact
/// HTTPS origin (including a non-default enterprise port). Never persisted.
pub fn github_authorization_for(origin: &str, header: String) -> anyhow::Result<()> {
    let origin = https_origin(origin)?;
    anyhow::ensure!(
        !header.contains(['\r', '\n', '\0'])
            && (header.is_empty()
                || header
                    .split_once(':')
                    .is_some_and(|(name, value)| name.eq_ignore_ascii_case("authorization")
                        && !value.trim().is_empty())),
        "An Authorization header must contain a single nonempty value"
    );
    let mut headers = GITHUB_AUTH
        .write()
        .map_err(|_| anyhow::anyhow!("Git authorization state is unavailable"))?;
    if header.is_empty() {
        headers.remove(&origin);
    } else {
        headers.insert(origin, header);
    }
    Ok(())
}
pub(crate) fn github_header(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    GITHUB_AUTH
        .read()
        .ok()?
        .get(&url.origin().ascii_serialization())
        .cloned()
}
pub fn configure(value: &VcsPreferences) {
    if let Ok(mut policy) = POLICY.write() {
        *policy = value.clone();
    }
}
pub fn current() -> VcsPreferences {
    POLICY.read().map(|p| p.clone()).unwrap_or_default()
}
pub fn apply(command: &mut std::process::Command) {
    if let Ok(headers) = GITHUB_AUTH.read() {
        if !headers.is_empty() {
            command.env("GIT_CONFIG_COUNT", headers.len().to_string());
            for (index, (origin, header)) in headers.iter().enumerate() {
                command
                    .env(
                        format!("GIT_CONFIG_KEY_{index}"),
                        format!("http.{origin}/.extraheader"),
                    )
                    .env(format!("GIT_CONFIG_VALUE_{index}"), header);
            }
        }
    }
    let policy = current();
    if matches!(policy.autocrlf.as_str(), "true" | "false" | "input") {
        command.args(["-c", &format!("core.autocrlf={}", policy.autocrlf)]);
    }
    if !policy.credential_helper {
        command.args(["-c", "credential.helper="]);
    }
    if policy.sign_commits {
        command.args([
            "-c",
            "commit.gpgsign=true",
            "-c",
            &format!("gpg.format={}", policy.signing_format),
        ]);
        if !policy.signing_key.is_empty() {
            command.args(["-c", &format!("user.signingkey={}", policy.signing_key)]);
        }
    }
    {
        // Shell-quote each path: GIT_SSH_COMMAND is interpreted by Git's shell.
        fn quote(s: &str) -> String {
            format!("'{}'", s.replace('\'', "'\\''"))
        }
        let executable = if policy.ssh_executable.is_empty() {
            "ssh"
        } else {
            &policy.ssh_executable
        };
        // Do not inherit core.sshCommand or a permissive StrictHostKeyChecking
        // setting from a repository. OpenSSH asks through the owned bridge.
        let mut ssh = format!("{} -o StrictHostKeyChecking=ask", quote(executable));
        if !policy.ssh_key.is_empty() {
            ssh.push_str(&format!(
                " -i {} -o IdentitiesOnly=yes",
                quote(&policy.ssh_key)
            ));
        }
        command.env("GIT_SSH_COMMAND", ssh);
    }
}
pub fn protected(branch: &str, patterns: &str) -> bool {
    patterns
        .split([',', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .any(|p| wildcard(p, branch))
}
fn wildcard(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut i, mut j, mut star, mut back) = (0, 0, None, 0);
    while j < t.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            back = j;
        } else if let Some(s) = star {
            back += 1;
            j = back;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}
impl GitRepo {
    pub fn ignored_files(&self) -> anyhow::Result<Vec<PathBuf>> {
        let mut options = git2::StatusOptions::new();
        options
            .include_ignored(true)
            .recurse_ignored_dirs(false)
            .include_untracked(false);
        Ok(self
            .repo
            .statuses(Some(&mut options))?
            .iter()
            .filter(|e| e.status().is_ignored())
            .filter_map(|e| e.path().ok().map(|p| self.root.join(p)))
            .collect())
    }
    pub fn ignore_sources(&self) -> Vec<PathBuf> {
        let mut paths = vec![
            self.root.join(".gitignore"),
            self.repo.path().join("info/exclude"),
            self.root.join(".ignore"),
        ];
        if let Ok(path) = self
            .repo
            .config()
            .and_then(|c| c.get_path("core.excludesfile"))
        {
            paths.push(path);
        }
        paths
    }
    pub fn stage(&self, path: &Path) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git stage")?;
        let rel = path.strip_prefix(&self.root)?;
        Self::run_git_in(
            &self.root,
            &[
                "add".into(),
                "-A".into(),
                "--".into(),
                rel.to_string_lossy().into_owned(),
            ],
        )?;
        Ok(())
    }
    pub fn commit_with_policy(
        &self,
        paths: &[PathBuf],
        message: &str,
        amend: bool,
        env: &GitEnv,
    ) -> anyhow::Result<String> {
        let policy = current();
        let staging_area = policy.staging_area
            || env
                .vars
                .iter()
                .any(|(key, value)| key == "RUSTPY_COMMIT_INDEX" && value == "1");
        if policy.warn_detached && self.repo.head_detached().unwrap_or(false) {
            anyhow::bail!("Detached HEAD: create or check out a branch, or disable the detached HEAD warning in Git settings");
        }
        if staging_area && policy.warn_crlf {
            let index = self.repo.index()?;
            let head = self.repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            for entry in index.iter() {
                let name = String::from_utf8_lossy(&entry.path);
                if head
                    .as_ref()
                    .and_then(|tree| tree.get_path(Path::new(name.as_ref())).ok())
                    .is_some_and(|old| old.id() == entry.id)
                {
                    continue;
                }
                if let Ok(blob) = self.repo.find_blob(entry.id) {
                    let bytes = blob.content();
                    if !bytes.contains(&0) && bytes.windows(2).any(|pair| pair == b"\r\n") {
                        anyhow::bail!("The staged version of {} contains CRLF. Restage with core.autocrlf=input/true, or disable the CRLF warning before committing the index",String::from_utf8_lossy(&entry.path));
                    }
                }
            }
        }
        if !staging_area && policy.warn_crlf {
            for path in paths {
                let Ok(bytes) = std::fs::read(path) else {
                    continue;
                };
                if bytes.contains(&0) || !bytes.windows(2).any(|s| s == b"\r\n") {
                    continue;
                }
                if policy.fix_crlf {
                    let normalized: Vec<u8> = bytes
                        .iter()
                        .enumerate()
                        .filter_map(|(i, b)| {
                            (*b != b'\r' || bytes.get(i + 1) != Some(&b'\n')).then_some(*b)
                        })
                        .collect();
                    std::fs::write(path, normalized)?;
                } else {
                    anyhow::bail!("{} contains CRLF line separators; enable Fix CRLF or disable the warning in Git settings", path.display());
                }
            }
        }
        // The CLI runs hooks, configured GPG/SSH signing, autocrlf filters and commit-msg validation.
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git commit")?;
        let mut args = vec!["commit".into(), "-m".into(), message.into()];
        // Hooks must not try to launch an editor without a terminal.
        if amend {
            args.push("--amend".into());
        }
        if policy.sign_off {
            args.push("--signoff".into());
        }
        if !staging_area && self.repo.state() == git2::RepositoryState::Clean {
            // Stage selected untracked files first; --only keeps unrelated index entries out.
            for path in paths {
                let rel = path.strip_prefix(&self.root)?;
                if !path.exists() && self.repo.index()?.get_path(rel, 0).is_none() {
                    continue;
                }
                let output = crate::git_command(
                    &self.root,
                    &[
                        "add".into(),
                        "--".into(),
                        rel.to_string_lossy().into_owned(),
                    ],
                    env,
                )
                .output()?;
                if !output.status.success() {
                    anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
                }
            }
            args.push("--only".into());
            args.push("--".into());
            args.extend(
                paths
                    .iter()
                    .filter_map(|p| p.strip_prefix(&self.root).ok())
                    .map(|p| p.to_string_lossy().into_owned()),
            );
        }
        let result = crate::git_command(&self.root, &args, env)
            .env("GIT_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .output()?;
        if !result.status.success() {
            anyhow::bail!(
                "{}",
                crate::describe_failure(&String::from_utf8_lossy(&result.stderr))
            );
        }
        Ok(git2::Repository::open(&self.root)?
            .head()?
            .target()
            .ok_or_else(|| anyhow::anyhow!("Git did not create a commit"))?
            .to_string())
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn account_origins_are_https_and_cannot_contain_credentials_or_paths() {
        assert_eq!(
            super::https_origin("https://git.example.test:8443/").unwrap(),
            "https://git.example.test:8443"
        );
        assert!(super::https_origin("https://git.example.test/account/path").is_err());
        assert!(super::https_origin("https://user:secret@git.example.test").is_err());
        assert!(super::https_origin("http://git.example.test").is_err());
    }
    #[test]
    fn protected_patterns_are_anchored() {
        assert!(super::protected("release/2026", "main,release/*"));
        assert!(!super::protected("feature/main", "main,release/*"));
    }
}
