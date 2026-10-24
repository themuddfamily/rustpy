//! Push and remotes (roadmap §4.1 "Push (dialog with commits preview …,
//! target remote/branch editing, push tags, force push with lease with
//! protected-branch check, push to new remote branch tracking)", "Manage
//! Remotes").

use anyhow::{anyhow, bail};
use git2::BranchType;

use crate::{CommitInfo, GitRepo};

/// Where Push sends the current branch by default: where a plain `git
/// push` would send it (see [`GitRepo::push_target`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushTarget {
    pub local_branch: String,
    /// The push remote: `branch.<name>.pushRemote`, else
    /// `remote.pushDefault`, else the upstream's remote, else the only
    /// remote, `origin` or the first remote; `None` when the repository has
    /// no remote.
    pub remote: Option<String>,
    /// The branch on the remote.
    pub remote_branch: String,
    /// The remote branch the local branch tracks, as `(remote, branch)`.
    pub upstream: Option<(String, String)>,
    /// Why the push must be confirmed in the Push dialog instead of running
    /// at once (Push in the Git tool window, Commit and Push): `git push`
    /// would refuse it (the upstream branch has another name, `push.default
    /// = nothing`, …), or it would go to a branch named differently from
    /// the local one.
    pub confirm: Option<String>,
}

impl PushTarget {
    /// The local branch tracks a remote branch.
    pub fn has_upstream(&self) -> bool {
        self.upstream.is_some()
    }

    /// Whether a push may go straight to this target, without the dialog.
    pub fn is_direct(&self) -> bool {
        self.remote.is_some() && self.confirm.is_none()
    }
}

/// The git configuration [`resolve_push_target`] reads for one branch.
#[derive(Clone, Debug, Default)]
struct PushConfig {
    remotes: Vec<String>,
    /// `branch.<name>.remote` and `branch.<name>.merge` (a full ref name).
    upstream: Option<(String, String)>,
    /// `branch.<name>.pushRemote`.
    branch_push_remote: Option<String>,
    /// `remote.pushDefault`.
    push_default_remote: Option<String>,
    /// `push.default` (`simple` when unset).
    push_default: Option<String>,
    /// The `remote.<name>.push` refspecs of each remote that has some.
    push_refspecs: Vec<(String, Vec<String>)>,
}

/// The ref a configured push refspec (`remote.<name>.push`) sends the
/// local branch `branch` to, when it pushes that branch at all:
/// `refs/heads/*:refs/heads/me/*`, `HEAD:refs/for/main`, `main` (the same
/// name) or `:` (matching branches).
fn push_refspec_dest(spec: &str, branch: &str) -> Option<String> {
    let spec = spec.trim();
    let spec = spec.strip_prefix('+').unwrap_or(spec);
    if spec.starts_with('^') {
        return None;
    }
    let local = format!("refs/heads/{branch}");
    let full = |s: &str| {
        if s.starts_with("refs/") {
            s.to_string()
        } else {
            format!("refs/heads/{s}")
        }
    };
    let (src, dst) = spec.split_once(':').unwrap_or((spec, ""));
    if src.is_empty() {
        // `:` pushes the branches both sides have under the same name;
        // `:dst` deletes `dst`.
        return dst.is_empty().then_some(local);
    }
    let src = if matches!(src, "HEAD" | "@") {
        local.clone()
    } else {
        full(src)
    };
    let dst = if dst.is_empty() {
        src.clone()
    } else {
        full(dst)
    };
    match (src.split_once('*'), dst.split_once('*')) {
        (Some((src_pre, src_post)), Some((dst_pre, dst_post))) => {
            let middle = local.strip_prefix(src_pre)?.strip_suffix(src_post)?;
            Some(format!("{dst_pre}{middle}{dst_post}"))
        }
        (None, None) => (src == local).then_some(dst),
        _ => None,
    }
}

/// Where a plain `git push` sends `local`, decided as git decides it: the
/// push remote (`branch.<name>.pushRemote`, `remote.pushDefault`, the
/// upstream's remote, the only remote, `origin`), then that remote's `push`
/// refspecs or `push.default` (`simple`, the default: the upstream branch
/// on the upstream's remote, which must have the local name; `upstream`;
/// `current`; `matching`; `nothing`). A branch without an upstream goes to
/// the same name (and tracks it after the push). When git would refuse the
/// push, or the push would go to another name, `confirm` says why.
fn resolve_push_target(local: &str, cfg: &PushConfig) -> PushTarget {
    let is_remote = |r: &str| cfg.remotes.iter().any(|x| x == r);
    let upstream = cfg
        .upstream
        .as_ref()
        .filter(|(r, _)| is_remote(r))
        .and_then(|(r, merge)| {
            merge
                .strip_prefix("refs/heads/")
                .map(|b| (r.clone(), b.to_string()))
        });
    let fallback =
        upstream
            .as_ref()
            .map(|(r, _)| r.clone())
            .or_else(|| match cfg.remotes.as_slice() {
                [only] => Some(only.clone()),
                all => all.iter().find(|r| *r == "origin").or(all.first()).cloned(),
            });
    let mut confirm: Option<String> = None;
    let configured = cfg
        .branch_push_remote
        .as_ref()
        .map(|r| (format!("branch.{local}.pushRemote"), r))
        .or_else(|| {
            cfg.push_default_remote
                .as_ref()
                .map(|r| ("remote.pushDefault".to_string(), r))
        });
    let remote = match configured {
        Some((_, r)) if is_remote(r) => Some(r.clone()),
        Some((key, r)) => {
            confirm = Some(format!("{key} names '{r}', which is not a remote of this repository: choose the remote to push to"));
            fallback
        }
        // Several remotes, none of them origin: git has no push destination,
        // so the first one is only proposed.
        None if upstream.is_none()
            && cfg.remotes.len() > 1
            && !cfg.remotes.iter().any(|r| r == "origin") =>
        {
            confirm = Some(format!("{local} has no upstream and no remote is the default: choose the remote to push to"));
            fallback
        }
        None => fallback,
    };
    let Some(remote) = remote else {
        return PushTarget {
            local_branch: local.to_string(),
            remote: None,
            remote_branch: local.to_string(),
            upstream,
            confirm,
        };
    };
    let specs = cfg
        .push_refspecs
        .iter()
        .find(|(r, specs)| *r == remote && !specs.is_empty())
        .map(|(_, specs)| specs);
    let (remote_branch, why) = match specs {
        Some(specs) => match specs.iter().find_map(|s| push_refspec_dest(s, local)) {
            Some(dest) => match dest.strip_prefix("refs/heads/") {
                Some(b) => (b.to_string(), None),
                None => (local.to_string(), Some(format!("remote.{remote}.push sends {local} to {dest}, which is not a branch: choose the branch to push to"))),
            },
            None => (local.to_string(), Some(format!("remote.{remote}.push does not push {local}: choose where to push it"))),
        },
        None => match cfg.push_default.as_deref().map(str::to_ascii_lowercase).as_deref().unwrap_or("simple") {
            "nothing" => (local.to_string(), Some("push.default is 'nothing': choose where to push".to_string())),
            "current" | "matching" => (local.to_string(), None),
            "upstream" | "tracking" => match &upstream {
                Some((r, b)) if *r == remote => (b.clone(), None),
                Some((r, b)) => (local.to_string(), Some(format!("push.default is 'upstream', but {local} tracks {r}/{b}, not a branch of {remote}: choose where to push"))),
                None => (local.to_string(), None),
            },
            // `simple`: the upstream branch must have the local name; a
            // push to another remote goes to the local name.
            _ => match &upstream {
                Some((r, b)) if *r == remote && b != local => (local.to_string(), Some(format!("{local} tracks {r}/{b}, which has another name, so git push would refuse: choose the remote branch ({b} to update it, {local} for a branch of its own)"))),
                _ => (local.to_string(), None),
            },
        },
    };
    let confirm = confirm.or(why).or_else(|| {
        (remote_branch != local).then(|| {
            format!("{local} is pushed to {remote}/{remote_branch}, a branch with another name")
        })
    });
    PushTarget {
        local_branch: local.to_string(),
        remote: Some(remote),
        remote_branch,
        upstream,
        confirm,
    }
}

/// "Push tags" in the Push dialog.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum PushTags {
    #[default]
    None,
    /// Annotated tags on the pushed commits (`--follow-tags`).
    CurrentBranch,
    /// Every local tag (`--tags`).
    All,
}

impl PushTags {
    pub fn label(self) -> &'static str {
        match self {
            PushTags::None => "None",
            PushTags::CurrentBranch => "Current Branch",
            PushTags::All => "All",
        }
    }
}

/// One push, as chosen in the Push dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRequest {
    pub local_branch: String,
    pub remote: String,
    pub remote_branch: String,
    /// Make the pushed branch the upstream of the local one (`-u`).
    pub set_upstream: bool,
    pub force_with_lease: bool,
    pub tags: PushTags,
}

impl PushRequest {
    /// The default push of `target` (`None` without a remote).
    pub fn from_target(target: &PushTarget) -> Option<PushRequest> {
        Some(PushRequest {
            local_branch: target.local_branch.clone(),
            remote: target.remote.clone()?,
            remote_branch: target.remote_branch.clone(),
            set_upstream: !target.has_upstream(),
            force_with_lease: false,
            tags: PushTags::None,
        })
    }

    /// `git push` arguments. The refspec names both branches, so the push
    /// works without an upstream and whatever `push.default` says.
    pub fn args(&self) -> Vec<String> {
        let mut a = vec![
            "push".to_string(),
            "--porcelain".into(),
            "--progress".into(),
        ];
        if self.set_upstream {
            a.push("--set-upstream".into());
        }
        if self.force_with_lease {
            a.push("--force-with-lease".into());
        }
        match self.tags {
            PushTags::None => {}
            PushTags::CurrentBranch => a.push("--follow-tags".into()),
            PushTags::All => a.push("--tags".into()),
        }
        a.push(self.remote.clone());
        a.push(format!(
            "refs/heads/{}:refs/heads/{}",
            self.local_branch, self.remote_branch
        ));
        a
    }

    /// `origin/main`, for messages.
    pub fn target_label(&self) -> String {
        format!("{}/{}", self.remote, self.remote_branch)
    }
}

/// Branches force pushes are refused for ("Protected branches" default).
pub fn is_protected_branch(name: &str) -> bool {
    crate::policy::protected(name, &crate::policy::current().protected_branches)
}

/// Whether `name` can name a remote (`origin`, `upstream`, `my-fork`).
pub fn is_valid_remote_name(name: &str) -> bool {
    !name.trim().is_empty() && name == name.trim() && git2::Remote::is_valid_name(name)
}

/// A remote URL must not be empty or contain spaces.
fn check_remote_url(url: &str) -> anyhow::Result<()> {
    if url.is_empty() {
        bail!("the URL is empty");
    }
    if url.chars().any(char::is_whitespace) {
        bail!("the URL contains spaces");
    }
    Ok(())
}

/// Whether `name` can name a branch on the remote.
pub fn is_valid_branch_name(name: &str) -> bool {
    !name.is_empty() && git2::Reference::is_valid_name(&format!("refs/heads/{name}"))
}

/// A ref line of `git push --porcelain`: `<flag>\t<from>:<to>\t<summary>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushedRef {
    /// ` ` fast-forward, `+` forced, `-` deleted, `*` new, `!` rejected, `=` up to date.
    pub flag: char,
    pub from: String,
    pub to: String,
    pub summary: String,
}

impl PushedRef {
    pub fn rejected(&self) -> bool {
        self.flag == '!'
    }
}

/// The ref lines of `git push --porcelain` output.
pub fn parse_push_porcelain(output: &str) -> Vec<PushedRef> {
    let mut out = Vec::new();
    for line in output.lines() {
        let mut chars = line.chars();
        let (Some(flag), Some('\t')) = (chars.next(), chars.next()) else {
            continue;
        };
        if !" +-*!=".contains(flag) {
            continue;
        }
        let rest = &line[2..];
        let (refs, summary) = rest.split_once('\t').unwrap_or((rest, ""));
        let (from, to) = refs.split_once(':').unwrap_or(("", refs));
        out.push(PushedRef {
            flag,
            from: from.to_string(),
            to: to.to_string(),
            summary: summary.trim().to_string(),
        });
    }
    out
}

/// One-line result of a push for the status bar.
pub fn push_result_message(
    req: &PushRequest,
    success: bool,
    output: &str,
    commits: Option<usize>,
) -> String {
    let refs = parse_push_porcelain(output);
    let target = req.target_label();
    let mine = refs
        .iter()
        .find(|r| r.to == format!("refs/heads/{}", req.remote_branch));
    if let Some(r) = mine.filter(|r| r.rejected()) {
        let hint = if r.summary.contains("fetch first") || r.summary.contains("non-fast-forward") {
            ": the remote has commits you do not have. Update the project, then push again"
        } else if r.summary.contains("stale info") {
            ": the remote branch changed since the last fetch"
        } else {
            ""
        };
        return format!(
            "Push to {target} rejected ({}){hint}",
            rejection_reason(&r.summary)
        );
    }
    if !success {
        // `fatal:` explains more than the closing "error: failed to push some refs".
        let last = |prefix: &str| {
            output
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| l.starts_with(prefix))
        };
        let line = last("fatal:")
            .or_else(|| last("ERROR:"))
            .or_else(|| last("error:"))
            .or_else(|| {
                output
                    .lines()
                    .rev()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && *l != "Done")
            })
            .unwrap_or("git push failed");
        return format!(
            "Push to {target} failed: {}",
            crate::job::describe_failure(line)
        );
    }
    match mine.map(|r| r.flag) {
        Some('=') => format!("Everything up to date on {target}"),
        Some('*') => format!(
            "Pushed {} to new branch {target}{}",
            commit_count(commits),
            if req.set_upstream {
                " (now tracked)"
            } else {
                ""
            }
        ),
        Some('+') => format!("Force-pushed {} to {target}", commit_count(commits)),
        _ => format!("Pushed {} to {target}", commit_count(commits)),
    }
}

/// `fetch first` from `[rejected] (fetch first)`.
fn rejection_reason(summary: &str) -> String {
    match (summary.find('('), summary.rfind(')')) {
        (Some(a), Some(b)) if b > a => summary[a + 1..b].to_string(),
        _ => summary.trim_matches(['[', ']']).to_string(),
    }
}

fn commit_count(n: Option<usize>) -> String {
    match n {
        Some(1) => "1 commit".into(),
        Some(n) => format!("{n} commits"),
        None => "commits".into(),
    }
}

impl GitRepo {
    /// The current branch's default push target: where a plain `git push`
    /// would send it (`branch.<name>.pushRemote`, `remote.pushDefault`,
    /// `remote.<name>.push` and `push.default` are honoured); `None` for a
    /// detached HEAD or a branch without commits.
    pub fn push_target(&self) -> Option<PushTarget> {
        let head = self.repo.head().ok()?;
        if !head.is_branch() {
            return None;
        }
        let local_branch = head.shorthand().ok()?.to_string();
        Some(resolve_push_target(
            &local_branch,
            &self.push_config(&local_branch),
        ))
    }

    fn push_config(&self, branch: &str) -> PushConfig {
        let refname = format!("refs/heads/{branch}");
        let upstream_remote = self
            .repo
            .branch_upstream_remote(&refname)
            .ok()
            .and_then(|b| b.as_str().ok().map(str::to_owned));
        let upstream_merge = self
            .repo
            .branch_upstream_merge(&refname)
            .ok()
            .and_then(|b| b.as_str().ok().map(str::to_owned));
        let config = self.repo.config().ok();
        let get = |key: &str| {
            config
                .as_ref()
                .and_then(|c| c.get_string(key).ok())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let remotes = self.remotes();
        let push_refspecs = remotes
            .iter()
            .filter_map(|r| {
                let specs: Vec<String> = self
                    .repo
                    .find_remote(r)
                    .ok()?
                    .push_refspecs()
                    .ok()?
                    .iter()
                    .flatten()
                    .flatten()
                    .map(str::to_owned)
                    .collect();
                (!specs.is_empty()).then(|| (r.clone(), specs))
            })
            .collect();
        PushConfig {
            upstream: upstream_remote.zip(upstream_merge),
            branch_push_remote: get(&format!("branch.{branch}.pushRemote")),
            push_default_remote: get("remote.pushDefault"),
            push_default: get("push.default"),
            push_refspecs,
            remotes,
        }
    }

    /// Whether `remote` has `branch` (as far as the last fetch knows).
    pub fn remote_branch_exists(&self, remote: &str, branch: &str) -> bool {
        self.repo
            .find_branch(&format!("{remote}/{branch}"), BranchType::Remote)
            .is_ok()
    }

    /// Commits a push of `local_branch` to `remote`/`remote_branch` sends:
    /// those not on the remote-tracking branch, or, for a new branch, not on
    /// any branch of that remote. Newest first, at most `limit`; the flag
    /// says whether more were left out.
    pub fn outgoing_commits(
        &self,
        local_branch: &str,
        remote: &str,
        remote_branch: &str,
        limit: usize,
    ) -> anyhow::Result<(Vec<CommitInfo>, bool)> {
        let tip = self
            .repo
            .find_branch(local_branch, BranchType::Local)
            .map_err(|_| anyhow!("no local branch '{local_branch}'"))?
            .get()
            .peel_to_commit()?
            .id();
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        walk.push(tip)?;
        match self
            .repo
            .find_branch(&format!("{remote}/{remote_branch}"), BranchType::Remote)
            .ok()
            .and_then(|b| b.get().target())
        {
            Some(oid) => walk.hide(oid)?,
            None => {
                let prefix = format!("refs/remotes/{remote}/");
                for r in self.repo.references()?.flatten() {
                    if r.name().ok().is_some_and(|n| n.starts_with(&prefix)) {
                        if let Some(oid) = r.resolve().ok().and_then(|r| r.target()) {
                            walk.hide(oid)?;
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        let mut more = false;
        for oid in walk {
            let oid = oid?;
            if out.len() == limit {
                more = true;
                break;
            }
            let c = self.repo.find_commit(oid)?;
            let id = oid.to_string();
            out.push(CommitInfo {
                short_id: id[..7.min(id.len())].to_string(),
                id,
                summary: c.summary().ok().flatten().unwrap_or("").to_string(),
                message: c.message().unwrap_or("").to_string(),
                author: c.author().name().unwrap_or("").to_string(),
                email: c.author().email().unwrap_or("").to_string(),
                time: c.time().seconds(),
                parents: c.parent_ids().map(|p| p.to_string()).collect(),
                refs: Vec::new(),
            });
        }
        Ok((out, more))
    }

    /// Changes the URL of a remote, as `git remote set-url` does: a
    /// separate push URL (`remote.<name>.pushurl`) is kept.
    pub fn set_remote_url(&self, name: &str, url: &str) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git set remote url")?;
        let url = url.trim();
        check_remote_url(url)?;
        self.repo
            .find_remote(name)
            .map_err(|_| anyhow!("no remote '{name}'"))?;
        self.repo.remote_set_url(name, url)?;
        Ok(())
    }

    /// The separate push URL of a remote (`remote.<name>.pushurl`), if any.
    pub fn remote_push_url(&self, name: &str) -> Option<String> {
        self.repo
            .find_remote(name)
            .ok()?
            .pushurl()
            .ok()
            .flatten()
            .map(str::to_owned)
    }

    /// Manage Remotes › Edit › Save: renames the remote `original` to `name`
    /// and points it at `url`, changing only what was edited (a separate
    /// push URL is kept). Returns whether anything changed.
    pub fn update_remote(&self, original: &str, name: &str, url: &str) -> anyhow::Result<bool> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git update remote")?;
        let (name, url) = (name.trim(), url.trim());
        let current = self
            .repo
            .find_remote(original)
            .map_err(|_| anyhow!("no remote '{original}'"))?
            .url()
            .ok()
            .map(str::to_owned);
        // Checked before the rename, so a bad URL changes nothing.
        check_remote_url(url)?;
        let renamed = name != original;
        if renamed {
            self.rename_remote(original, name)?;
        }
        let moved = current.as_deref() != Some(url);
        if moved {
            self.set_remote_url(name, url)?;
        }
        Ok(renamed || moved)
    }

    /// Renames a remote (its remote-tracking branches and the upstream
    /// settings of local branches follow).
    pub fn rename_remote(&self, old: &str, new: &str) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git rename remote")?;
        if old == new {
            return Ok(());
        }
        if !is_valid_remote_name(new) {
            bail!("'{new}' is not a valid remote name");
        }
        if self.remotes().iter().any(|r| r == new) {
            bail!("a remote named '{new}' already exists");
        }
        let problems = self.repo.remote_rename(old, new)?;
        for p in problems.iter() {
            log::warn!("rename remote {old} -> {new}: could not update the refspec {p:?}");
        }
        Ok(())
    }

    /// Adds a remote after checking the name and URL.
    pub fn add_remote_checked(&self, name: &str, url: &str) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git add remote checked")?;
        let (name, url) = (name.trim(), url.trim());
        if !is_valid_remote_name(name) {
            bail!("'{name}' is not a valid remote name");
        }
        check_remote_url(url)?;
        if self.remotes().iter().any(|r| r == name) {
            bail!("a remote named '{name}' already exists");
        }
        self.add_remote(name, url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn has_git() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok()
    }

    /// A work tree with one commit on `main` and an empty bare `origin`.
    fn setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (bare, work) = (dir.path().join("origin.git"), dir.path().join("work"));
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&bare, &["init", "--bare", "-q", "-b", "main"]);
        git(&work, &["init", "-q", "-b", "main"]);
        std::fs::write(work.join("a.txt"), "1").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "first"]);
        (dir, bare, work)
    }

    fn commit(work: &Path, file: &str, msg: &str) {
        std::fs::write(work.join(file), msg).unwrap();
        git(work, &["add", "."]);
        git(work, &["commit", "-q", "-m", msg]);
    }

    fn run(work: &Path, req: &PushRequest) -> (bool, String) {
        let mut job = crate::GitJob::spawn(work, &req.args(), &crate::GitEnv::default()).unwrap();
        assert!(job.wait(std::time::Duration::from_secs(60)));
        (job.succeeded(), job.output())
    }

    /// Where a plain `git push` would send the current branch: `(remote
    /// URL, destination ref)`, or `None` when git refuses to push.
    fn git_would_push(work: &Path) -> Option<(String, String)> {
        let out = std::process::Command::new("git")
            .args(["push", "--dry-run", "--porcelain"])
            .current_dir(work)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git runs");
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let url = text
            .lines()
            .find_map(|l| l.strip_prefix("To "))?
            .trim()
            .to_string();
        Some((url, parse_push_porcelain(&text).into_iter().next()?.to))
    }

    /// Review: `git checkout -b feature origin/main` makes origin/main the
    /// upstream; the quick push sent the feature commit to origin/main.
    #[test]
    fn a_branch_tracking_another_name_is_not_pushed_there_without_the_dialog() {
        if !has_git() {
            return;
        }
        let (_dir, bare, work) = setup();
        git(&work, &["config", "push.default", "simple"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&work, &["push", "-q", "-u", "origin", "main"]);
        git(
            &work,
            &["checkout", "-q", "-b", "feature", "--track", "origin/main"],
        );
        commit(&work, "f.txt", "feature work");
        let target = || GitRepo::discover(&work).unwrap().push_target().unwrap();
        let t = target();
        assert_eq!(t.upstream, Some(("origin".to_string(), "main".to_string())));
        // Not straight to main: the dialog confirms, proposing the local name.
        assert!(!t.is_direct(), "{t:?}");
        assert!(
            t.confirm.as_deref().is_some_and(
                |c| c.starts_with("feature tracks origin/main, which has another name")
            ),
            "{t:?}"
        );
        assert_eq!(
            (t.remote.as_deref(), t.remote_branch.as_str()),
            (Some("origin"), "feature")
        );
        assert_eq!(git_would_push(&work), None, "plain git push refuses it too");
        // Pushing the proposed target creates origin/feature; main is untouched.
        let req = PushRequest::from_target(&t).unwrap();
        assert_eq!(
            req.args().last().map(String::as_str),
            Some("refs/heads/feature:refs/heads/feature")
        );
        let (ok, output) = run(&work, &req);
        assert!(ok, "{output}");
        assert!(!git(&bare, &["log", "--format=%s", "main"]).contains("feature work"));
        assert!(git(&bare, &["log", "--format=%s", "feature"]).contains("feature work"));
        // push.default = upstream asks for the upstream branch, as git does;
        // still confirmed, since it has another name.
        git(&work, &["config", "push.default", "upstream"]);
        let t = target();
        assert_eq!(t.remote_branch, "main");
        assert!(!t.is_direct(), "{t:?}");
        assert_eq!(
            git_would_push(&work).map(|(_, to)| to).as_deref(),
            Some("refs/heads/main")
        );
        // A branch tracking its own name is pushed at once.
        git(&work, &["config", "push.default", "simple"]);
        git(&work, &["checkout", "-q", "main"]);
        assert!(target().is_direct(), "{:?}", target());
    }

    /// Review: `remote.pushDefault`, `branch.<name>.pushRemote`,
    /// `remote.<name>.push` and `push.default` were ignored.
    #[test]
    fn push_remotes_refspecs_and_push_default_are_honoured() {
        if !has_git() {
            return;
        }
        let (dir, bare, work) = setup();
        let fork = dir.path().join("fork.git");
        std::fs::create_dir_all(&fork).unwrap();
        git(&fork, &["init", "--bare", "-q", "-b", "main"]);
        git(&work, &["config", "push.default", "simple"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&work, &["remote", "add", "fork", fork.to_str().unwrap()]);
        git(&work, &["push", "-q", "-u", "origin", "main"]);
        commit(&work, "b.txt", "second");
        let target = || GitRepo::discover(&work).unwrap().push_target().unwrap();
        // remote.pushDefault: pushes go to the fork, the upstream stays origin/main.
        git(&work, &["config", "remote.pushDefault", "fork"]);
        let t = target();
        assert_eq!(
            (t.remote.as_deref(), t.remote_branch.as_str(), t.is_direct()),
            (Some("fork"), "main", true),
            "{t:?}"
        );
        assert_eq!(t.upstream, Some(("origin".to_string(), "main".to_string())));
        assert_eq!(
            git_would_push(&work),
            Some((fork.display().to_string(), "refs/heads/main".to_string()))
        );
        let req = PushRequest::from_target(&t).unwrap();
        assert!(!req.set_upstream);
        let (ok, output) = run(&work, &req);
        assert!(ok, "{output}");
        assert!(git(&fork, &["log", "--format=%s", "main"]).contains("second"));
        assert!(!git(&bare, &["log", "--format=%s", "main"]).contains("second"));
        // branch.<name>.pushRemote wins over remote.pushDefault.
        git(&work, &["config", "branch.main.pushRemote", "origin"]);
        assert_eq!(target().remote.as_deref(), Some("origin"));
        assert_eq!(
            git_would_push(&work).map(|(url, _)| url),
            Some(bare.display().to_string())
        );
        git(&work, &["config", "--unset", "branch.main.pushRemote"]);
        // remote.<name>.push maps the branch (to another name: confirmed).
        git(
            &work,
            &[
                "config",
                "remote.fork.push",
                "refs/heads/*:refs/heads/mirror/*",
            ],
        );
        let t = target();
        assert_eq!(
            (t.remote.as_deref(), t.remote_branch.as_str(), t.is_direct()),
            (Some("fork"), "mirror/main", false),
            "{t:?}"
        );
        assert_eq!(
            git_would_push(&work).map(|(_, to)| to).as_deref(),
            Some("refs/heads/mirror/main")
        );
        git(&work, &["config", "--unset", "remote.fork.push"]);
        // push.default = nothing: git pushes nothing without a refspec.
        git(&work, &["config", "push.default", "nothing"]);
        assert!(!target().is_direct());
        assert_eq!(git_would_push(&work), None);
        // A push remote that does not exist is not replaced silently.
        git(&work, &["config", "push.default", "simple"]);
        git(&work, &["config", "remote.pushDefault", "gone"]);
        let t = target();
        assert!(
            !t.is_direct()
                && t.confirm
                    .as_deref()
                    .is_some_and(|c| c.contains("remote.pushDefault names 'gone'")),
            "{t:?}"
        );
    }

    #[test]
    fn push_refspecs_map_the_local_branch() {
        assert_eq!(
            push_refspec_dest("refs/heads/*:refs/heads/me/*", "f").as_deref(),
            Some("refs/heads/me/f")
        );
        assert_eq!(
            push_refspec_dest("+refs/heads/main:refs/heads/trunk", "main").as_deref(),
            Some("refs/heads/trunk")
        );
        assert_eq!(
            push_refspec_dest("refs/heads/main:refs/heads/trunk", "dev"),
            None
        );
        assert_eq!(
            push_refspec_dest("HEAD:refs/for/main", "dev").as_deref(),
            Some("refs/for/main")
        );
        assert_eq!(
            push_refspec_dest("dev", "dev").as_deref(),
            Some("refs/heads/dev")
        );
        assert_eq!(
            push_refspec_dest("HEAD", "dev").as_deref(),
            Some("refs/heads/dev")
        );
        assert_eq!(
            push_refspec_dest("dev:review", "dev").as_deref(),
            Some("refs/heads/review")
        );
        assert_eq!(
            push_refspec_dest(":", "dev").as_deref(),
            Some("refs/heads/dev")
        );
        assert_eq!(push_refspec_dest(":gone", "dev"), None, "a deletion");
        assert_eq!(
            push_refspec_dest("^refs/heads/dev", "dev"),
            None,
            "a negative refspec"
        );
    }

    #[test]
    fn push_targets_follow_the_configuration() {
        let remotes = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let up = |r: &str, b: &str| Some((r.to_string(), format!("refs/heads/{b}")));
        // No remote: nothing to push to.
        let t = resolve_push_target("main", &PushConfig::default());
        assert!(t.remote.is_none() && !t.is_direct());
        // Without an upstream: the only remote, else origin, under the local name.
        let t = resolve_push_target(
            "dev",
            &PushConfig {
                remotes: remotes(&["upstream"]),
                ..Default::default()
            },
        );
        assert_eq!(
            (t.remote.as_deref(), t.remote_branch.as_str(), t.is_direct()),
            (Some("upstream"), "dev", true)
        );
        let t = resolve_push_target(
            "dev",
            &PushConfig {
                remotes: remotes(&["fork", "origin"]),
                ..Default::default()
            },
        );
        assert_eq!((t.remote.as_deref(), t.is_direct()), (Some("origin"), true));
        // Several remotes, none of them origin: git has no push destination,
        // so the first one is only proposed in the dialog.
        let t = resolve_push_target(
            "dev",
            &PushConfig {
                remotes: remotes(&["fork", "upstream"]),
                ..Default::default()
            },
        );
        assert_eq!(t.remote.as_deref(), Some("fork"));
        assert!(
            !t.is_direct()
                && t.confirm
                    .as_deref()
                    .unwrap()
                    .contains("no remote is the default"),
            "{t:?}"
        );
        let t = resolve_push_target(
            "dev",
            &PushConfig {
                remotes: remotes(&["fork", "upstream"]),
                push_default_remote: Some("upstream".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            (t.remote.as_deref(), t.is_direct()),
            (Some("upstream"), true)
        );
        // An upstream on a removed remote is no upstream.
        let t = resolve_push_target(
            "dev",
            &PushConfig {
                remotes: remotes(&["origin"]),
                upstream: up("old", "dev"),
                ..Default::default()
            },
        );
        assert_eq!((t.upstream, t.remote.as_deref()), (None, Some("origin")));
        // current / matching: the local name, whatever the upstream is called.
        for mode in ["current", "matching", "Current"] {
            let t = resolve_push_target(
                "dev",
                &PushConfig {
                    remotes: remotes(&["origin"]),
                    upstream: up("origin", "main"),
                    push_default: Some(mode.into()),
                    ..Default::default()
                },
            );
            assert_eq!(
                (t.remote_branch.as_str(), t.is_direct()),
                ("dev", true),
                "{mode}"
            );
        }
        // simple, triangular (pushing to another remote than the upstream's): the local name.
        let t = resolve_push_target(
            "dev",
            &PushConfig {
                remotes: remotes(&["origin", "fork"]),
                upstream: up("origin", "main"),
                push_default_remote: Some("fork".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            (t.remote.as_deref(), t.remote_branch.as_str(), t.is_direct()),
            (Some("fork"), "dev", true)
        );
        // upstream, triangular: git refuses.
        let t = resolve_push_target(
            "dev",
            &PushConfig {
                remotes: remotes(&["origin", "fork"]),
                upstream: up("origin", "main"),
                push_default_remote: Some("fork".into()),
                push_default: Some("upstream".into()),
                ..Default::default()
            },
        );
        assert!(
            !t.is_direct()
                && t.confirm
                    .as_deref()
                    .unwrap()
                    .contains("not a branch of fork"),
            "{t:?}"
        );
        // A push refspec to something that is not a branch (Gerrit).
        let t = resolve_push_target(
            "dev",
            &PushConfig {
                remotes: remotes(&["origin"]),
                push_refspecs: vec![("origin".into(), vec!["HEAD:refs/for/main".into()])],
                ..Default::default()
            },
        );
        assert!(
            !t.is_direct()
                && t.confirm
                    .as_deref()
                    .unwrap()
                    .contains("refs/for/main, which is not a branch"),
            "{t:?}"
        );
    }

    /// Review: Manage Remotes › Edit › Save overwrote a separate push URL
    /// with the fetch URL, even when only the name changed.
    #[test]
    fn saving_a_remote_keeps_a_separate_push_url() {
        let dir = tempfile::tempdir().unwrap();
        let repo = GitRepo::init(dir.path()).unwrap();
        repo.add_remote_checked("origin", "https://github.com/me/repo.git")
            .unwrap();
        repo.repo
            .remote_set_pushurl("origin", Some("git@github.com:me/repo.git"))
            .unwrap();
        let fetch_url = |name: &str| {
            repo.remotes_with_urls()
                .unwrap()
                .into_iter()
                .find(|(n, _)| n == name)
                .map(|(_, u)| u)
        };
        // Saved unchanged: nothing is written.
        assert!(!repo
            .update_remote("origin", "origin", "https://github.com/me/repo.git")
            .unwrap());
        assert_eq!(
            repo.remote_push_url("origin").as_deref(),
            Some("git@github.com:me/repo.git")
        );
        // Only the name changed: both URLs are kept.
        assert!(repo
            .update_remote("origin", "upstream", " https://github.com/me/repo.git ")
            .unwrap());
        assert_eq!(repo.remotes(), vec!["upstream".to_string()]);
        assert_eq!(
            fetch_url("upstream").as_deref(),
            Some("https://github.com/me/repo.git")
        );
        assert_eq!(
            repo.remote_push_url("upstream").as_deref(),
            Some("git@github.com:me/repo.git")
        );
        // A new URL is the fetch URL, as `git remote set-url` sets it.
        assert!(repo
            .update_remote("upstream", "upstream", "https://example.com/repo.git")
            .unwrap());
        assert_eq!(
            fetch_url("upstream").as_deref(),
            Some("https://example.com/repo.git")
        );
        assert_eq!(
            repo.remote_push_url("upstream").as_deref(),
            Some("git@github.com:me/repo.git")
        );
        // A bad URL changes nothing, not even the name.
        assert!(repo.update_remote("upstream", "origin", "a b").is_err());
        assert_eq!(repo.remotes(), vec!["upstream".to_string()]);
    }

    #[test]
    fn first_push_to_a_new_remote_sets_the_upstream_and_clears_outgoing() {
        if !has_git() {
            return;
        }
        let (_dir, bare, work) = setup();
        let repo = GitRepo::discover(&work).unwrap();
        // No remote yet: nowhere to push.
        let t = repo.push_target().unwrap();
        assert_eq!(t.remote, None);
        assert!(PushRequest::from_target(&t).is_none());
        repo.add_remote_checked("origin", bare.to_str().unwrap())
            .unwrap();
        let t = repo.push_target().unwrap();
        assert_eq!(
            t,
            PushTarget {
                local_branch: "main".into(),
                remote: Some("origin".into()),
                remote_branch: "main".into(),
                upstream: None,
                confirm: None
            }
        );
        assert!(t.is_direct());
        commit(&work, "b.txt", "second");
        let (out, more) = repo
            .outgoing_commits("main", "origin", "main", 100)
            .unwrap();
        assert_eq!(
            out.iter().map(|c| c.summary.as_str()).collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        assert!(!more);
        let (out, more) = repo.outgoing_commits("main", "origin", "main", 1).unwrap();
        assert_eq!((out.len(), more), (1, true));
        assert!(!repo.remote_branch_exists("origin", "main"));
        let req = PushRequest::from_target(&t).unwrap();
        assert!(req.set_upstream);
        assert_eq!(
            req.args(),
            vec![
                "push",
                "--porcelain",
                "--progress",
                "--set-upstream",
                "origin",
                "refs/heads/main:refs/heads/main"
            ]
        );
        let (ok, output) = run(&work, &req);
        assert!(ok, "{output}");
        assert_eq!(
            push_result_message(&req, ok, &output, Some(2)),
            "Pushed 2 commits to new branch origin/main (now tracked)"
        );
        let repo = GitRepo::discover(&work).unwrap();
        assert_eq!(repo.upstream_of_current().as_deref(), Some("origin/main"));
        let t = repo.push_target().unwrap();
        assert!(t.has_upstream() && t.is_direct());
        assert!(repo.remote_branch_exists("origin", "main"));
        assert!(repo
            .outgoing_commits("main", "origin", "main", 100)
            .unwrap()
            .0
            .is_empty());
        // Pushing again changes nothing.
        let req = PushRequest::from_target(&t).unwrap();
        assert!(!req.set_upstream);
        let (ok, output) = run(&work, &req);
        assert!(ok);
        assert_eq!(
            push_result_message(&req, ok, &output, Some(0)),
            "Everything up to date on origin/main"
        );
    }

    #[test]
    fn feature_branches_push_under_another_name_and_rejections_are_reported() {
        if !has_git() {
            return;
        }
        let (dir, bare, work) = setup();
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&work, &["push", "-q", "-u", "origin", "main"]);
        let repo = GitRepo::discover(&work).unwrap();
        repo.create_branch("feature", true).unwrap();
        commit(&work, "f.txt", "feature work");
        let repo = GitRepo::discover(&work).unwrap();
        let t = repo.push_target().unwrap();
        assert_eq!(
            (t.local_branch.as_str(), t.has_upstream()),
            ("feature", false)
        );
        // A new branch: only the commit that no remote branch has is outgoing.
        let (out, _) = repo
            .outgoing_commits("feature", "origin", "feature", 100)
            .unwrap();
        assert_eq!(out.len(), 1);
        let mut req = PushRequest::from_target(&t).unwrap();
        req.remote_branch = "review/feature".into();
        let (ok, output) = run(&work, &req);
        assert!(ok, "{output}");
        assert!(git(&bare, &["branch", "--list"]).contains("review/feature"));
        assert_eq!(
            GitRepo::discover(&work)
                .unwrap()
                .upstream_of_current()
                .as_deref(),
            Some("origin/review/feature")
        );
        // Someone else pushes to main; our main diverges: rejected.
        let other = dir.path().join("other");
        git(
            dir.path(),
            &[
                "clone",
                "-q",
                bare.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        commit(&other, "o.txt", "theirs");
        git(&other, &["push", "-q", "origin", "main"]);
        git(&work, &["checkout", "-q", "main"]);
        commit(&work, "m.txt", "ours");
        let repo = GitRepo::discover(&work).unwrap();
        let req = PushRequest::from_target(&repo.push_target().unwrap()).unwrap();
        let (ok, output) = run(&work, &req);
        assert!(!ok);
        let msg = push_result_message(&req, ok, &output, Some(1));
        assert!(
            msg.starts_with("Push to origin/main rejected (fetch first): "),
            "{msg}\n{output}"
        );
        // --force-with-lease against a stale tracking ref is refused too.
        let mut forced = req.clone();
        forced.force_with_lease = true;
        let (ok, output) = run(&work, &forced);
        assert!(!ok, "{output}");
        assert_eq!(push_result_message(&forced, ok, &output, None), "Push to origin/main rejected (stale info): the remote branch changed since the last fetch");
    }

    #[test]
    fn porcelain_lines_are_parsed() {
        let out = "To /tmp/origin.git\n*\trefs/heads/main:refs/heads/main\t[new branch]\n!\trefs/heads/x:refs/heads/x\t[rejected] (fetch first)\nDone\n";
        let refs = parse_push_porcelain(out);
        assert_eq!(refs.len(), 2);
        assert_eq!(
            refs[0],
            PushedRef {
                flag: '*',
                from: "refs/heads/main".into(),
                to: "refs/heads/main".into(),
                summary: "[new branch]".into()
            }
        );
        assert!(refs[1].rejected());
        assert_eq!(refs[1].summary, "[rejected] (fetch first)");
    }

    #[test]
    fn remote_names_and_urls_are_managed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = GitRepo::init(dir.path()).unwrap();
        assert!(repo.add_remote_checked("bad name", "x").is_err());
        assert!(repo.add_remote_checked("origin", "  ").is_err());
        assert!(repo
            .add_remote_checked("origin", "git@github.com:me/repo.git")
            .is_ok());
        assert!(repo
            .add_remote_checked("origin", "x")
            .unwrap_err()
            .to_string()
            .contains("already exists"));
        repo.set_remote_url("origin", "https://github.com/me/repo.git")
            .unwrap();
        assert_eq!(
            repo.remotes_with_urls().unwrap(),
            vec![(
                "origin".to_string(),
                "https://github.com/me/repo.git".to_string()
            )]
        );
        assert!(repo.set_remote_url("nope", "x").is_err());
        repo.rename_remote("origin", "upstream").unwrap();
        assert_eq!(repo.remotes(), vec!["upstream".to_string()]);
        assert!(repo.rename_remote("upstream", "a b").is_err());
        assert!(
            is_valid_branch_name("feature/x")
                && !is_valid_branch_name("a..b")
                && !is_valid_branch_name("")
        );
        assert!(
            is_protected_branch("main")
                && is_protected_branch("master")
                && !is_protected_branch("dev")
        );
    }
}
