//! Branch inventory, comparison and explicit local branch mutations.
//! All mutations participate in the existing repository operation authority.
use crate::{BranchInfo, CommitInfo, FileDiff, GitRepo};
use anyhow::{anyhow, bail};
use git2::{BranchType, Oid};
#[derive(Clone, Debug)]
pub struct BranchRecord {
    pub branch: BranchInfo,
    pub oid: String,
    pub summary: String,
    pub author: String,
    pub time: i64,
    pub incoming: usize,
    pub outgoing: usize,
    pub unmerged: usize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MergeMode {
    #[default]
    FastForward,
    NoFastForward,
    FastForwardOnly,
    Squash,
}
#[derive(Clone, Debug)]
pub enum LocalAction {
    Create {
        name: String,
        start: String,
        checkout: bool,
        overwrite: bool,
    },
    CheckoutRevision(String),
    CheckoutRebase(String),
    ForceCheckout(String),
    ShelfCheckout(String),
    Unshelve(String),
    Merge {
        branch: String,
        mode: MergeMode,
        message: String,
    },
    TrackRemote(String),
    Rename {
        old: String,
        new: String,
    },
    Track {
        branch: String,
        upstream: Option<String>,
    },
    DefaultRemote(String),
    Delete {
        branch: String,
        force: bool,
    },
    Restore {
        branch: String,
        oid: String,
    },
    Tag {
        name: String,
        target: String,
        message: String,
    },
    DeleteTag(String),
}
impl LocalAction {
    pub fn changes_files(&self) -> bool {
        matches!(
            self,
            Self::Create { checkout: true, .. }
                | Self::CheckoutRevision(_)
                | Self::CheckoutRebase(_)
                | Self::ForceCheckout(_)
                | Self::ShelfCheckout(_)
                | Self::Unshelve(_)
                | Self::Merge { .. }
        )
    }
}
#[derive(Clone, Debug)]
pub struct BranchComparison {
    pub current: String,
    pub selected: String,
    pub only_current: Vec<CommitInfo>,
    pub only_selected: Vec<CommitInfo>,
    pub current_more: bool,
    pub selected_more: bool,
}
impl GitRepo {
    pub fn branch_inventory(&self) -> Vec<BranchRecord> {
        let head = self.repo.head().ok().and_then(|head| head.target());
        self.branches()
            .into_iter()
            .filter_map(|branch| {
                let reference = self
                    .repo
                    .find_branch(
                        &branch.name,
                        if branch.is_remote {
                            BranchType::Remote
                        } else {
                            BranchType::Local
                        },
                    )
                    .ok()?;
                let commit = reference.get().peel_to_commit().ok()?;
                let upstream = reference
                    .upstream()
                    .ok()
                    .and_then(|up| up.get().target())
                    .or(if branch.is_remote { head } else { None });
                let (outgoing, incoming) = upstream
                    .and_then(|up| self.repo.graph_ahead_behind(commit.id(), up).ok())
                    .unwrap_or_default();
                let (outgoing, incoming) = if branch.is_remote {
                    (incoming, outgoing)
                } else {
                    (outgoing, incoming)
                };
                let unmerged = head
                    .and_then(|head| self.repo.graph_ahead_behind(commit.id(), head).ok())
                    .map(|(ahead, _)| ahead)
                    .unwrap_or(0);
                let author = commit.author().name().unwrap_or_default().to_owned();
                Some(BranchRecord {
                    branch,
                    oid: commit.id().to_string(),
                    summary: commit.summary().ok().flatten().unwrap_or_default().into(),
                    author,
                    time: commit.time().seconds(),
                    incoming,
                    outgoing,
                    unmerged,
                })
            })
            .collect()
    }
    fn branch_unique_commits(
        &self,
        include: Oid,
        exclude: Oid,
        limit: usize,
    ) -> anyhow::Result<(Vec<CommitInfo>, bool)> {
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        walk.push(include)?;
        walk.hide(exclude)?;
        let mut out = Vec::new();
        let mut more = false;
        for oid in walk {
            if out.len() >= limit {
                more = true;
                break;
            }
            let commit = self.repo.find_commit(oid?)?;
            let id = commit.id().to_string();
            out.push(CommitInfo {
                id: id.clone(),
                short_id: id[..7].into(),
                summary: commit.summary().ok().flatten().unwrap_or_default().into(),
                message: commit.message().unwrap_or_default().into(),
                author: commit.author().name().unwrap_or_default().into(),
                email: commit.author().email().unwrap_or_default().into(),
                time: commit.time().seconds(),
                parents: commit.parent_ids().map(|id| id.to_string()).collect(),
                refs: Vec::new(),
            });
        }
        Ok((out, more))
    }
    pub fn compare_branch_commits(
        &self,
        selected: &str,
        limit: usize,
    ) -> anyhow::Result<BranchComparison> {
        let head = self.repo.head()?.peel_to_commit()?.id();
        let target = self
            .repo
            .revparse_single(&self.branch_spec(selected))?
            .peel_to_commit()?
            .id();
        let (only_current, current_more) =
            self.branch_unique_commits(head, target, limit.clamp(1, 2000))?;
        let (only_selected, selected_more) =
            self.branch_unique_commits(target, head, limit.clamp(1, 2000))?;
        Ok(BranchComparison {
            current: self.current_branch().unwrap_or_else(|| "HEAD".into()),
            selected: selected.into(),
            only_current,
            only_selected,
            current_more,
            selected_more,
        })
    }
    pub fn branch_working_tree_diff(&self, selected: &str) -> anyhow::Result<Vec<FileDiff>> {
        let target = self
            .repo
            .revparse_single(&self.branch_spec(selected))?
            .peel_to_tree()?;
        let mut options = git2::DiffOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        let mut diff = self
            .repo
            .diff_tree_to_workdir_with_index(Some(&target), Some(&mut options))?;
        let mut find = git2::DiffFindOptions::new();
        find.renames(true);
        let _ = diff.find_similar(Some(&mut find));
        Ok(self.file_diffs(&diff))
    }
    /// Return the deleted tip before mutation so a UI can offer lossless Restore.
    pub fn branch_tip(&self, name: &str) -> anyhow::Result<String> {
        Ok(self
            .repo
            .find_branch(name, BranchType::Local)?
            .get()
            .peel_to_commit()?
            .id()
            .to_string())
    }
    pub fn branch_local_action(&self, action: &LocalAction) -> anyhow::Result<String> {
        self.branch_local_action_cancellable(action, &rustpy_core::CancellationToken::new())
    }
    pub fn branch_local_action_cancellable(
        &self,
        action: &LocalAction,
        token: &rustpy_core::CancellationToken,
    ) -> anyhow::Result<String> {
        token.check()?;
        let run = |args: &[&str]| -> anyhow::Result<String> {
            let args: Vec<String> = args.iter().map(|value| value.to_string()).collect();
            Self::run_git_cancellable(&self.root, &args, token).map_err(|error| anyhow!("{error}"))
        };
        let _operation =
            crate::RepositoryOperation::enter(&self.root, "Branch workspace operation")?;
        match action {
            LocalAction::Create {
                name,
                start,
                checkout,
                overwrite,
            } => {
                if !crate::is_valid_branch_name(name) {
                    bail!("Invalid branch name '{name}'");
                }
                if self.current_branch().as_deref() == Some(name.as_str()) {
                    bail!("Cannot overwrite the current branch");
                }
                let commit = self
                    .repo
                    .revparse_single(&self.branch_spec(start))?
                    .peel_to_commit()?;
                let mut created = self.repo.branch(name, &commit, *overwrite)?;
                if self.repo.find_branch(start, BranchType::Remote).is_ok() {
                    created.set_upstream(Some(start))?;
                }
                drop(created);
                if *checkout {
                    self.checkout_branch(name)?;
                }
                Ok(format!("Created branch '{name}'"))
            }
            LocalAction::CheckoutRevision(revision) => {
                let object = self.repo.revparse_single(revision)?.peel_to_commit()?;
                self.repo.checkout_tree(
                    object.as_object(),
                    Some(git2::build::CheckoutBuilder::new().safe()),
                )?;
                self.repo.set_head_detached(object.id())?;
                Ok(format!("Checked out {revision} (detached HEAD)"))
            }
            LocalAction::ForceCheckout(branch) => {
                run(&["checkout", "--force", &self.branch_spec(branch)])?;
                // Full refs are deliberately resolved before checkout; attach local HEAD.
                if self.repo.find_branch(branch, BranchType::Local).is_ok() {
                    self.repo.set_head(&format!("refs/heads/{branch}"))?;
                }
                Ok(format!("Force checked out '{branch}'"))
            }
            LocalAction::ShelfCheckout(branch) => {
                let shelf = self.branch_shelve()?;
                if let Err(error) = self.checkout_branch(branch) {
                    let restored = shelf.as_ref().is_none_or(|reference| {
                        self.branch_unshelve(reference, &rustpy_core::CancellationToken::new())
                            .is_ok()
                    });
                    return Err(error.context(if restored {
                        "Checkout failed; local changes restored".to_string()
                    } else {
                        format!(
                            "Checkout failed; local changes remain in {}",
                            shelf.unwrap_or_default()
                        )
                    }));
                }
                Ok(match shelf {
                    Some(reference) => format!(
                        "Checked out '{branch}'; changes preserved in branch shelf {reference}"
                    ),
                    None => format!("Checked out '{branch}'"),
                })
            }
            LocalAction::Unshelve(reference) => {
                if !reference.starts_with("refs/rustpy/branch-shelves/") {
                    bail!("Not a branch shelf");
                }
                let mut saved = self.repo.find_reference(reference)?;
                let id = saved.peel_to_commit()?.id().to_string();
                run(&["stash", "apply", "--index", &id])?;
                saved.delete()?;
                Ok("Restored shelved files and index".into())
            }
            LocalAction::CheckoutRebase(branch) => {
                let current = self.repo.head()?.peel_to_commit()?.id().to_string();
                if self
                    .branches()
                    .iter()
                    .any(|entry| entry.name == *branch && entry.is_remote)
                {
                    self.checkout_remote_branch(branch)?;
                } else {
                    self.checkout_branch(branch)?;
                }
                run(&["rebase", "--autostash", "--", &current])?;
                Ok(format!(
                    "Checked out '{branch}' and rebased onto the previous current branch"
                ))
            }
            LocalAction::Merge {
                branch,
                mode,
                message,
            } => {
                let mut args = vec!["merge".to_string()];
                args.push(
                    match mode {
                        MergeMode::FastForward => "--ff",
                        MergeMode::NoFastForward => "--no-ff",
                        MergeMode::FastForwardOnly => "--ff-only",
                        MergeMode::Squash => "--squash",
                    }
                    .into(),
                );
                if !message.trim().is_empty() {
                    args.extend(["-m".into(), message.clone()]);
                } else {
                    args.push("--no-edit".into());
                }
                args.extend(["--".into(), self.branch_spec(branch)]);
                let refs: Vec<_> = args.iter().map(String::as_str).collect();
                run(&refs)?;
                Ok(if *mode == MergeMode::Squash {
                    "Squash changes staged; commit them in Local Changes".into()
                } else {
                    format!("Merged '{branch}'")
                })
            }
            LocalAction::TrackRemote(remote_ref) => {
                let (_, local) = self
                    .split_remote_ref(remote_ref)
                    .ok_or_else(|| anyhow!("Not a remote branch"))?;
                let remote = self.repo.find_branch(remote_ref, BranchType::Remote)?;
                if self.repo.find_branch(&local, BranchType::Local).is_err() {
                    let commit = remote.get().peel_to_commit()?;
                    self.repo.branch(&local, &commit, false)?;
                }
                self.repo
                    .find_branch(&local, BranchType::Local)?
                    .set_upstream(Some(remote_ref))?;
                Ok(format!("'{local}' now tracks '{remote_ref}'"))
            }
            LocalAction::Rename { old, new } => {
                if !crate::is_valid_branch_name(new) {
                    bail!("Invalid branch name");
                }
                self.rename_branch(old, new)?;
                Ok(format!("Renamed {old} to {new}"))
            }
            LocalAction::Track { branch, upstream } => {
                let mut local = self.repo.find_branch(branch, BranchType::Local)?;
                if let Some(upstream) = upstream {
                    self.repo
                        .find_branch(upstream, BranchType::Remote)
                        .map_err(|_| anyhow!("Choose an existing remote branch"))?;
                }
                local.set_upstream(upstream.as_deref())?;
                Ok(format!("Updated tracking for '{branch}'"))
            }
            LocalAction::DefaultRemote(remote) => {
                self.repo.find_remote(remote)?;
                self.repo.config()?.set_str("remote.pushDefault", remote)?;
                Ok(format!("Default push remote: {remote}"))
            }
            LocalAction::Delete { branch, force } => {
                let oid = self.branch_tip(branch)?;
                let backup = format!("refs/rustpy/deleted-branches/{oid}/{branch}");
                let existed = self.repo.find_reference(&backup).is_ok();
                self.repo
                    .reference(
                        &backup,
                        Oid::from_str(&oid)?,
                        false,
                        "RustPy deleted branch restore point",
                    )
                    .or_else(|error| {
                        if existed {
                            self.repo.find_reference(&backup)
                        } else {
                            Err(error)
                        }
                    })?;
                if let Err(error) = self.delete_branch(branch, *force) {
                    if !existed {
                        let _ = self
                            .repo
                            .find_reference(&backup)
                            .and_then(|mut reference| reference.delete());
                    }
                    return Err(error);
                }
                Ok(format!("Deleted branch '{branch}'"))
            }
            LocalAction::Restore { branch, oid } => {
                if !crate::is_valid_branch_name(branch) {
                    bail!("Invalid branch name");
                }
                let commit = self.repo.find_commit(Oid::from_str(oid)?)?;
                self.repo.branch(branch, &commit, false)?;
                let backup = format!("refs/rustpy/deleted-branches/{oid}/{branch}");
                let _ = self
                    .repo
                    .find_reference(&backup)
                    .and_then(|mut reference| reference.delete());
                Ok(format!("Restored branch '{branch}'"))
            }
            LocalAction::Tag {
                name,
                target,
                message,
            } => {
                if name.starts_with('-')
                    || !git2::Reference::is_valid_name(&format!("refs/tags/{name}"))
                {
                    bail!("Invalid tag name");
                }
                self.create_tag_at(
                    name,
                    &self.branch_spec(target),
                    (!message.is_empty()).then_some(message.as_str()),
                )?;
                Ok(format!("Created tag '{name}'"))
            }
            LocalAction::DeleteTag(name) => {
                self.delete_tag(name)?;
                Ok(format!("Deleted tag '{name}'"))
            }
        }
    }
    /// Persist a lossless index/worktree/untracked snapshot and clean the tree.
    /// The returned ref is an immutable, GC-rooted token, never a stash index.
    pub fn branch_shelve(&self) -> anyhow::Result<Option<String>> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Create branch shelf")?;
        if self.status()?.is_empty() {
            return Ok(None);
        }
        let mut handle = git2::Repository::open(self.repo.path())?;
        let id = handle.stash_save2(
            &self.signature()?,
            Some("RustPy lossless branch shelf"),
            Some(git2::StashFlags::INCLUDE_UNTRACKED),
        )?;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let reference = format!(
            "refs/rustpy/branch-shelves/{stamp}-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.repo
            .reference(&reference, id, false, "RustPy lossless shelf")?;
        if self
            .stash_list()?
            .first()
            .is_some_and(|entry| entry.id == id.to_string())
        {
            self.stash_drop(0)?;
        }
        Ok(Some(reference))
    }
    /// Delete a captured shelf only after a clean --index restoration.
    pub fn branch_unshelve(
        &self,
        reference: &str,
        token: &rustpy_core::CancellationToken,
    ) -> anyhow::Result<()> {
        self.branch_local_action_cancellable(&LocalAction::Unshelve(reference.into()), token)
            .map(|_| ())
    }
    pub fn deleted_branches(&self) -> Vec<(String, String)> {
        self.repo
            .references_glob("refs/rustpy/deleted-branches/*")
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let reference = entry.ok()?;
                let tail = reference
                    .name()
                    .ok()?
                    .strip_prefix("refs/rustpy/deleted-branches/")?;
                let (oid, branch) = tail.split_once('/')?;
                Some((branch.to_string(), oid.to_string()))
            })
            .collect()
    }
    pub fn branch_push_remote(&self, branch: &str) -> Option<String> {
        let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        let remotes = self.remotes();
        let config = self.repo.config().ok()?;
        config
            .get_string(&format!("branch.{branch}.pushRemote"))
            .ok()
            .or_else(|| config.get_string("remote.pushDefault").ok())
            .filter(|name| remotes.contains(name))
            .or_else(|| {
                self.branches()
                    .into_iter()
                    .find(|entry| !entry.is_remote && entry.name == branch)
                    .and_then(|entry| entry.upstream)
                    .and_then(|upstream| self.split_remote_ref(&upstream).map(|(remote, _)| remote))
            })
            .or_else(|| {
                remotes
                    .iter()
                    .find(|name| *name == "origin")
                    .or(remotes.first())
                    .cloned()
            })
    }
    pub fn branch_shelves(&self) -> Vec<(String, String)> {
        self.repo
            .references_glob("refs/rustpy/branch-shelves/*")
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let reference = entry.ok()?;
                Some((
                    reference.name().ok()?.to_string(),
                    reference
                        .peel_to_commit()
                        .ok()?
                        .summary()
                        .ok()
                        .flatten()
                        .unwrap_or_default()
                        .to_string(),
                ))
            })
            .collect()
    }
    /// Only construct a GitHub compare link; opening it remains an explicit UI action.
    pub fn branch_pull_request_url(&self, branch: &str) -> Option<String> {
        let urls = self.remotes_with_urls().ok()?;
        let (_, url) = urls
            .iter()
            .find(|(name, _)| name == "origin")
            .or(urls.first())?;
        let repository = url
            .strip_prefix("git@github.com:")
            .or_else(|| url.strip_prefix("https://github.com/"))?
            .trim_end_matches(".git")
            .trim_end_matches('/');
        if repository.split('/').count() != 2 {
            return None;
        }
        fn encode(value: &str) -> String {
            value
                .bytes()
                .map(|byte| {
                    if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                        (byte as char).to_string()
                    } else {
                        format!("%{byte:02X}")
                    }
                })
                .collect()
        }
        Some(format!(
            "https://github.com/{repository}/compare/{}?expand=1",
            encode(branch)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn repository() -> (tempfile::TempDir, GitRepo) {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Branch fixture").unwrap();
        config
            .set_str("user.email", "branch@example.invalid")
            .unwrap();
        drop(config);
        drop(repo);
        std::fs::write(directory.path().join("tracked.bin"), [0, 1, 2]).unwrap();
        let handle = GitRepo::discover(directory.path()).unwrap();
        commit(&handle, "base", &["tracked.bin"]);
        (directory, handle)
    }
    fn commit(repo: &GitRepo, message: &str, paths: &[&str]) -> Oid {
        let mut index = repo.repo.index().unwrap();
        for path in paths {
            index.add_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();
        let tree = repo.repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Branch fixture", "branch@example.invalid").unwrap();
        let parent = repo
            .repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<_> = parent.iter().collect();
        repo.repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &parents,
            )
            .unwrap()
    }
    #[test]
    fn two_way_lists_and_upstream_counts_follow_branch_tips() {
        let (directory, repo) = repository();
        repo.create_branch("feature/topic", true).unwrap();
        std::fs::write(directory.path().join("feature.py"), "feature\n").unwrap();
        let feature = commit(&repo, "feature only", &["feature.py"]);
        repo.checkout_branch("main").unwrap();
        std::fs::write(directory.path().join("main.py"), "main\n").unwrap();
        commit(&repo, "main only", &["main.py"]);
        repo.repo
            .remote("origin", "https://example.invalid/repository.git")
            .unwrap();
        repo.repo
            .reference(
                "refs/remotes/origin/feature/topic",
                feature,
                false,
                "fixture",
            )
            .unwrap();
        repo.branch_local_action(&LocalAction::Track {
            branch: "feature/topic".into(),
            upstream: Some("origin/feature/topic".into()),
        })
        .unwrap();
        let compare = repo.compare_branch_commits("feature/topic", 20).unwrap();
        assert_eq!(
            compare
                .only_current
                .iter()
                .map(|commit| commit.summary.as_str())
                .collect::<Vec<_>>(),
            ["main only"]
        );
        assert_eq!(
            compare
                .only_selected
                .iter()
                .map(|commit| commit.summary.as_str())
                .collect::<Vec<_>>(),
            ["feature only"]
        );
        let inventory = repo.branch_inventory();
        let topic = inventory
            .iter()
            .find(|entry| entry.branch.name == "feature/topic")
            .unwrap();
        assert_eq!(
            topic.branch.upstream.as_deref(),
            Some("origin/feature/topic")
        );
        assert_eq!((topic.incoming, topic.outgoing, topic.unmerged), (0, 0, 1));
        assert!(repo
            .branch_working_tree_diff("feature/topic")
            .unwrap()
            .iter()
            .any(|diff| diff.path.ends_with("main.py")));
    }
    #[test]
    fn unmerged_delete_requires_force_and_restore_survives_reopen() {
        let (directory, repo) = repository();
        repo.create_branch("topic", true).unwrap();
        std::fs::write(directory.path().join("topic.py"), "topic\n").unwrap();
        let tip = commit(&repo, "unmerged topic", &["topic.py"]).to_string();
        repo.checkout_branch("main").unwrap();
        assert!(repo
            .branch_local_action(&LocalAction::Delete {
                branch: "topic".into(),
                force: false
            })
            .is_err());
        assert!(repo.deleted_branches().is_empty());
        repo.branch_local_action(&LocalAction::Delete {
            branch: "topic".into(),
            force: true,
        })
        .unwrap();
        drop(repo);
        let reopened = GitRepo::discover(directory.path()).unwrap();
        assert_eq!(
            reopened.deleted_branches(),
            vec![("topic".into(), tip.clone())]
        );
        reopened
            .branch_local_action(&LocalAction::Restore {
                branch: "topic".into(),
                oid: tip.clone(),
            })
            .unwrap();
        assert_eq!(reopened.branch_tip("topic").unwrap(), tip);
        assert!(reopened.deleted_branches().is_empty());
    }
    #[test]
    fn shelf_checkout_preserves_binary_index_worktree_and_untracked_files() {
        let (directory, repo) = repository();
        repo.create_branch("feature", false).unwrap();
        let tracked = directory.path().join("tracked.bin");
        std::fs::write(&tracked, [0, 9, 2]).unwrap();
        let mut index = repo.repo.index().unwrap();
        index.add_path(Path::new("tracked.bin")).unwrap();
        index.write().unwrap();
        drop(index);
        std::fs::write(&tracked, [0, 9, 8]).unwrap();
        std::fs::write(directory.path().join("untracked.bin"), [255, 0, 7]).unwrap();
        repo.branch_local_action(&LocalAction::ShelfCheckout("feature".into()))
            .unwrap();
        assert!(repo.status().unwrap().is_empty());
        assert_eq!(std::fs::read(&tracked).unwrap(), [0, 1, 2]);
        let shelves = repo.branch_shelves();
        assert_eq!(shelves.len(), 1);
        repo.branch_local_action(&LocalAction::Unshelve(shelves[0].0.clone()))
            .unwrap();
        assert_eq!(std::fs::read(&tracked).unwrap(), [0, 9, 8]);
        assert_eq!(
            std::fs::read(directory.path().join("untracked.bin")).unwrap(),
            [255, 0, 7]
        );
        let mut index = repo.repo.index().unwrap();
        index.read(true).unwrap();
        let entry = index.get_path(Path::new("tracked.bin"), 0).unwrap();
        assert_eq!(repo.repo.find_blob(entry.id).unwrap().content(), [0, 9, 2]);
        assert!(repo.branch_shelves().is_empty());
    }
    #[test]
    fn cancelled_actions_and_invalid_names_leave_branch_tips_unchanged() {
        let (_directory, repo) = repository();
        let original = repo.branch_tip("main").unwrap();
        assert!(repo
            .branch_local_action(&LocalAction::Create {
                name: "bad..name".into(),
                start: "HEAD".into(),
                checkout: false,
                overwrite: false
            })
            .is_err());
        let token = rustpy_core::CancellationToken::new();
        token.cancel();
        assert!(repo
            .branch_local_action_cancellable(
                &LocalAction::Create {
                    name: "cancelled".into(),
                    start: "HEAD".into(),
                    checkout: false,
                    overwrite: false
                },
                &token
            )
            .is_err());
        assert_eq!(repo.branch_tip("main").unwrap(), original);
        assert!(!repo
            .branches()
            .iter()
            .any(|branch| branch.name == "cancelled"));
    }
}
