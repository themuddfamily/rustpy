//! Validated operation requests and read-only revision inspection.
use crate::{BlameLine, CommitInfo, FileDiff, GitRepo};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Borrowed controls from the captured read-only history request.
pub struct HistoryControl<'a> {
    pub cancel: &'a std::sync::atomic::AtomicBool,
    pub progress: &'a std::sync::atomic::AtomicUsize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pull {
    pub remote: String,
    pub branch: String,
    pub rebase: bool,
    pub no_commit: bool,
    pub squash: bool,
    pub ff_only: bool,
    pub no_ff: bool,
    pub no_verify: bool,
    pub autostash: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Merge {
    pub branch: String,
    pub strategy: String,
    pub strategy_option: String,
    pub no_commit: bool,
    pub squash: bool,
    pub ff_only: bool,
    pub no_ff: bool,
    pub no_verify: bool,
    pub message: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rebase {
    pub onto: String,
    pub upstream: String,
    pub branch: String,
    pub preserve_merges: bool,
    pub autostash: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reset {
    Soft,
    Mixed,
    Hard,
    Keep,
}
impl Reset {
    pub fn flag(self) -> &'static str {
        match self {
            Self::Soft => "--soft",
            Self::Mixed => "--mixed",
            Self::Hard => "--hard",
            Self::Keep => "--keep",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Request {
    pub label: String,
    pub args: Vec<String>,
    pub mutates: bool,
    pub destructive: bool,
}
fn text(value: &str, what: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !value.is_empty() && !value.starts_with('-') && !value.contains(['\0', '\r', '\n']),
        "Invalid {what}"
    );
    Ok(value.into())
}
fn flags(args: &mut Vec<String>, items: &[(&str, bool)]) {
    for (flag, on) in items {
        if *on {
            args.push((*flag).into());
        }
    }
}
impl Request {
    pub fn update(rebase: bool, clean: usize, submodules: bool) -> anyhow::Result<Vec<Self>> {
        let mut requests = Vec::new();
        let mut shelf = None;
        if clean == 1 {
            let reference = format!(
                "refs/rustpy/update-shelves/{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
            );
            let git = quote(&crate::git_program().replace('\\', "/"));
            let script = format!(
                "set -e; if test -n \"$({git} status --porcelain)\"; then {git} stash push --include-untracked -m 'RustPy shelf before update'; oid=$({git} rev-parse --verify refs/stash); {git} update-ref {reference} \"$oid\"; echo 'Recovery shelf: {reference}'; fi"
            );
            requests.push(Self {
                label: "Shelve before update".into(),
                args: vec![
                    "-c".into(),
                    format!("alias.rustpy-shelve=!{script}"),
                    "rustpy-shelve".into(),
                ],
                mutates: true,
                destructive: false,
            });
            shelf = Some(reference);
        }
        let mut pull = Self::pull(&Pull {
            rebase,
            autostash: clean == 0,
            ..Default::default()
        })?;
        pull.label = "Update Project".into();
        requests.push(pull);
        if submodules {
            requests.push(Self::submodules());
        }
        if let Some(reference) = shelf {
            requests.push(Self::unshelve(&reference)?);
        }
        Ok(requests)
    }
    pub fn unshelve(reference: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            reference.starts_with("refs/rustpy/") && git2::Reference::is_valid_name(reference),
            "Invalid shelf reference"
        );
        let git = quote(&crate::git_program().replace('\\', "/"));
        Ok(Self {
            label: "Restore update shelf".into(),
            args: vec![
                "-c".into(),
                format!(
                    "alias.rustpy-unshelve=!if {git} show-ref --verify --quiet {}; then {git} stash apply --index {} && {git} update-ref -d {}; fi",
                    quote(reference),
                    quote(reference),
                    quote(reference)
                ),
                "rustpy-unshelve".into(),
            ],
            mutates: true,
            destructive: false,
        })
    }
    pub fn pull(options: &Pull) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !(options.ff_only && (options.no_ff || options.squash)),
            "Fast-forward only conflicts with no-ff/squash"
        );
        anyhow::ensure!(
            !(options.rebase && (options.no_commit || options.squash || options.no_ff)),
            "Merge options cannot be combined with rebase"
        );
        let mut args = vec!["pull".into(), "--progress".into(), "--no-edit".into()];
        flags(
            &mut args,
            &[
                ("--rebase", options.rebase),
                ("--no-rebase", !options.rebase),
                ("--no-commit", options.no_commit),
                ("--squash", options.squash),
                ("--ff-only", options.ff_only),
                ("--no-ff", options.no_ff),
                ("--no-verify", options.no_verify),
                ("--autostash", options.autostash),
            ],
        );
        if !options.remote.is_empty() {
            args.push(text(&options.remote, "remote")?);
            if !options.branch.is_empty() {
                args.push(text(&options.branch, "branch")?);
            }
        } else {
            anyhow::ensure!(options.branch.is_empty(), "Choose a remote for the branch");
        }
        Ok(Self {
            label: "Pull".into(),
            args,
            mutates: true,
            destructive: false,
        })
    }
    pub fn merge(options: &Merge) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !(options.ff_only && (options.no_ff || options.squash)),
            "Choose compatible fast-forward options"
        );
        let mut args = vec!["merge".into()];
        flags(
            &mut args,
            &[
                ("--no-commit", options.no_commit),
                ("--squash", options.squash),
                ("--ff-only", options.ff_only),
                ("--no-ff", options.no_ff),
                ("--no-verify", options.no_verify),
            ],
        );
        if !options.strategy.is_empty() {
            args.extend([
                "--strategy".into(),
                text(&options.strategy, "merge strategy")?,
            ]);
        }
        if !options.strategy_option.is_empty() {
            args.extend([
                "-X".into(),
                text(&options.strategy_option, "strategy option")?,
            ]);
        }
        if !options.message.is_empty() {
            args.extend(["-m".into(), options.message.clone()]);
        } else {
            args.push("--no-edit".into());
        }
        args.push(text(&options.branch, "branch")?);
        Ok(Self {
            label: "Merge".into(),
            args,
            mutates: true,
            destructive: false,
        })
    }
    pub fn rebase(options: &Rebase) -> anyhow::Result<Self> {
        let mut args = vec!["rebase".into()];
        flags(
            &mut args,
            &[
                ("--rebase-merges", options.preserve_merges),
                ("--autostash", options.autostash),
            ],
        );
        if !options.onto.is_empty() {
            args.extend(["--onto".into(), text(&options.onto, "onto revision")?]);
        }
        args.push(text(&options.upstream, "upstream revision")?);
        if !options.branch.is_empty() {
            args.push(text(&options.branch, "branch")?);
        }
        Ok(Self {
            label: "Rebase".into(),
            args,
            mutates: true,
            destructive: false,
        })
    }
    pub fn cherry_pick(
        id: &str,
        suffix: bool,
        no_commit: bool,
        mainline: Option<u32>,
    ) -> anyhow::Result<Self> {
        let mut args = vec!["cherry-pick".into()];
        flags(&mut args, &[("-x", suffix), ("--no-commit", no_commit)]);
        if let Some(parent) = mainline {
            anyhow::ensure!(parent > 0, "Mainline parents start at 1");
            args.extend(["--mainline".into(), parent.to_string()]);
        }
        args.push(text(id, "revision")?);
        Ok(Self {
            label: "Cherry-pick".into(),
            args,
            mutates: true,
            destructive: false,
        })
    }
    pub fn revert(id: &str, no_commit: bool, mainline: Option<u32>) -> anyhow::Result<Self> {
        let mut request = Self::cherry_pick(id, false, no_commit, mainline)?;
        request.args[0] = "revert".into();
        request.args.insert(1, "--no-edit".into());
        request.label = "Revert".into();
        Ok(request)
    }
    pub fn reset(id: &str, mode: Reset) -> anyhow::Result<Self> {
        Ok(Self {
            label: format!("Reset {}", mode.flag()),
            args: vec!["reset".into(), mode.flag().into(), text(id, "revision")?],
            mutates: true,
            destructive: mode == Reset::Hard,
        })
    }
    pub fn stash(message: &str, untracked: bool, keep_index: bool) -> Self {
        let mut args = vec!["stash".into(), "push".into()];
        flags(
            &mut args,
            &[
                ("--include-untracked", untracked),
                ("--keep-index", keep_index),
            ],
        );
        if !message.is_empty() {
            args.extend(["-m".into(), message.into()]);
        }
        Self {
            label: "Stash changes".into(),
            args,
            mutates: true,
            destructive: false,
        }
    }
    pub fn stash_action(action: &str, index: usize, branch: Option<&str>) -> anyhow::Result<Self> {
        anyhow::ensure!(
            ["apply", "pop", "drop", "branch"].contains(&action),
            "Invalid stash action"
        );
        let mut args = vec!["stash".into(), action.into()];
        if action == "branch" {
            args.push(text(branch.unwrap_or(""), "branch name")?);
        }
        args.push(format!("stash@{{{index}}}"));
        Ok(Self {
            label: format!("Stash {action}"),
            args,
            mutates: true,
            destructive: action == "drop",
        })
    }
    pub fn continuation(operation: &str, action: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            ["merge", "rebase", "cherry-pick", "revert"].contains(&operation),
            "Unsupported operation"
        );
        anyhow::ensure!(
            ["continue", "skip", "abort"].contains(&action),
            "Unsupported continuation"
        );
        anyhow::ensure!(
            operation != "merge" || action != "skip",
            "Merge cannot skip a commit"
        );
        Ok(Self {
            label: format!("{action} {operation}"),
            args: vec![
                "-c".into(),
                "core.editor=true".into(),
                operation.into(),
                format!("--{action}"),
            ],
            mutates: true,
            destructive: action == "abort",
        })
    }
    pub fn submodules() -> Self {
        Self {
            label: "Update submodules".into(),
            args: vec![
                "submodule".into(),
                "update".into(),
                "--init".into(),
                "--recursive".into(),
                "--progress".into(),
            ],
            mutates: true,
            destructive: false,
        }
    }
    pub fn paths(repo: &GitRepo, command: &str, paths: &[PathBuf]) -> anyhow::Result<Self> {
        anyhow::ensure!(
            ["add", "restore", "rm", "mergetool"].contains(&command),
            "Unsupported path operation"
        );
        let mut args = vec![command.into()];
        if command == "restore" {
            args.push("--worktree".into());
        }
        if command == "mergetool" {
            args.push("--no-prompt".into());
        }
        if command == "rm" && paths.iter().any(|path| path.is_dir()) {
            args.push("-r".into());
        }
        args.push("--".into());
        for path in paths {
            let relative = repo.operation_relative(path)?;
            args.push(if relative.as_os_str().is_empty() {
                ".".into()
            } else {
                relative.to_string_lossy().into_owned()
            });
        }
        anyhow::ensure!(!paths.is_empty(), "Select a path");
        Ok(Self {
            label: command.into(),
            args,
            mutates: true,
            destructive: command == "restore" || command == "rm",
        })
    }
    pub fn get_revision(repo: &GitRepo, revision: &str, path: &Path) -> anyhow::Result<Self> {
        let revision = repo.resolve_log_revision(revision)?;
        let relative = repo.operation_relative(path)?;
        let path = if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.to_string_lossy().into_owned()
        };
        Ok(Self {
            label: "Get repository version".into(),
            args: vec!["checkout".into(), revision, "--".into(), path],
            mutates: true,
            destructive: true,
        })
    }
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Attributes {
    pub filter: Option<String>,
    pub eol: Option<String>,
    pub text: Option<String>,
    pub working_tree_encoding: Option<String>,
    pub large: bool,
    pub lfs_pointer: bool,
}
#[derive(Clone, Debug)]
pub struct Submodule {
    pub name: String,
    pub path: PathBuf,
    pub head: Option<String>,
    pub dirty: bool,
    pub initialized: bool,
}
#[derive(Clone, Debug)]
pub struct HistoryRevision {
    pub commit: CommitInfo,
    pub path: PathBuf,
}
#[derive(Clone, Debug)]
pub struct RevisionEntry {
    pub path: PathBuf,
    pub directory: bool,
    pub oid: String,
    pub size: usize,
}
impl GitRepo {
    pub fn operation_parent_path(&self, path: &Path, revision: &str) -> anyhow::Result<PathBuf> {
        let relative = self.operation_relative(path)?;
        let commit = self.repo.revparse_single(revision)?.peel_to_commit()?;
        let tree = commit.tree()?;
        let parent = commit.parent(0)?.tree()?;
        let mut diff = self
            .repo
            .diff_tree_to_tree(Some(&parent), Some(&tree), None)?;
        let mut options = git2::DiffFindOptions::new();
        options.renames(true);
        diff.find_similar(Some(&mut options))?;
        let path = diff
            .deltas()
            .find(|delta| {
                delta.status() == git2::Delta::Renamed
                    && delta.new_file().path() == Some(relative.as_path())
            })
            .and_then(|delta| delta.old_file().path().map(Path::to_path_buf))
            .unwrap_or(relative);
        Ok(self.root.join(path))
    }
    pub fn operation_worktrees(&self) -> Vec<(String, PathBuf)> {
        self.repo
            .worktrees()
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| {
                        name.ok().flatten().and_then(|name| {
                            self.repo
                                .find_worktree(name)
                                .ok()
                                .map(|worktree| (name.to_string(), worktree.path().to_path_buf()))
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn operation_branch_rebase_default(&self) -> bool {
        let Some(branch) = self.current_branch() else {
            return false;
        };
        let Ok(config) = self.repo.config() else {
            return false;
        };
        config
            .get_bool(&format!("branch.{branch}.rebase"))
            .or_else(|_| config.get_bool("pull.rebase"))
            .unwrap_or(false)
    }
    pub fn accept_operation_side(&self, path: &Path, theirs: bool) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Accept conflict side")?;
        let relative = self.operation_relative(path)?;
        let mut index = self.repo.index()?;
        index.read(false)?;
        let conflict = index.conflict_get(&relative)?;
        let chosen = if theirs { conflict.their } else { conflict.our };
        if chosen.is_some() {
            let mut options = git2::build::CheckoutBuilder::new();
            options
                .force()
                .allow_conflicts(true)
                .path(&relative)
                .use_theirs(theirs)
                .use_ours(!theirs);
            self.repo
                .checkout_index(Some(&mut index), Some(&mut options))?;
            let _ = index.conflict_remove(&relative);
            index.add_path(&relative)?;
        } else {
            if let Ok(metadata) = std::fs::symlink_metadata(path) {
                if metadata.is_dir() {
                    std::fs::remove_dir(path)?;
                } else {
                    std::fs::remove_file(path)?;
                }
            }
            let _ = index.conflict_remove(&relative);
            if let Err(error) = index.remove_path(&relative) {
                if error.code() != git2::ErrorCode::NotFound {
                    return Err(error.into());
                }
            }
        }
        index.write()?;
        Ok(())
    }
    pub fn operation_revision_message(&self, id: &str) -> anyhow::Result<String> {
        Ok(self
            .repo
            .revparse_single(id)?
            .peel_to_commit()?
            .message()?
            .to_string())
    }
    pub fn operation_shelves(&self) -> Vec<(String, String)> {
        self.repo
            .references_glob("refs/rustpy/*shelves/*")
            .map(|refs| {
                refs.flatten()
                    .filter_map(|reference| {
                        let name = reference.name().ok()?.to_string();
                        let commit = reference.peel_to_commit().ok()?;
                        Some((
                            name,
                            commit.summary().ok().flatten().unwrap_or("").to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn operation_relative(&self, path: &Path) -> anyhow::Result<PathBuf> {
        let rel = if path.is_absolute() {
            path.strip_prefix(&self.root)?
        } else {
            path
        };
        anyhow::ensure!(
            !rel.components().any(|c| matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )),
            "Path escapes the repository"
        );
        Ok(rel.to_path_buf())
    }
    pub fn operation_attributes(&self, path: &Path) -> anyhow::Result<Attributes> {
        let rel = self.operation_relative(path)?;
        let get = |name| {
            let raw = self
                .repo
                .get_attr_bytes(&rel, name, git2::AttrCheckFlags::FILE_THEN_INDEX)
                .ok()
                .flatten();
            match git2::AttrValue::from_bytes(raw) {
                git2::AttrValue::True => Some("true".into()),
                git2::AttrValue::False => Some("false".into()),
                git2::AttrValue::String(value) => Some(value.to_string()),
                git2::AttrValue::Bytes(value) => Some(String::from_utf8_lossy(value).into_owned()),
                git2::AttrValue::Unspecified => None,
            }
        };
        let prefix = std::fs::File::open(self.root.join(&rel))
            .ok()
            .and_then(|mut file| {
                use std::io::Read;
                let mut bytes = [0u8; 256];
                let count = file.read(&mut bytes).ok()?;
                Some(String::from_utf8_lossy(&bytes[..count]).into_owned())
            })
            .unwrap_or_default();
        Ok(Attributes {
            filter: get("filter"),
            eol: get("eol"),
            text: get("text"),
            working_tree_encoding: get("working-tree-encoding"),
            large: std::fs::metadata(self.root.join(&rel))
                .map(|m| m.len() > 10 * 1024 * 1024)
                .unwrap_or(false),
            lfs_pointer: prefix.starts_with("version https://git-lfs.github.com/spec/v1\n"),
        })
    }
    pub fn operation_lock(&self) -> Option<PathBuf> {
        let lock = self.repo.path().join("index.lock");
        lock.exists().then_some(lock)
    }
    pub fn operation_submodules(&self) -> anyhow::Result<Vec<Submodule>> {
        self.repo
            .submodules()?
            .into_iter()
            .map(|module| {
                let name = module.name()?.to_string();
                let status = self
                    .repo
                    .submodule_status(&name, git2::SubmoduleIgnore::Unspecified)?;
                Ok(Submodule {
                    name,
                    path: self.root.join(module.path()),
                    head: module.head_id().map(|id| id.to_string()),
                    dirty: status.intersects(
                        git2::SubmoduleStatus::WD_MODIFIED
                            | git2::SubmoduleStatus::WD_INDEX_MODIFIED
                            | git2::SubmoduleStatus::WD_WD_MODIFIED
                            | git2::SubmoduleStatus::WD_UNTRACKED,
                    ),
                    initialized: module.open().is_ok(),
                })
            })
            .collect()
    }
    pub fn revision_entries(
        &self,
        revision: &str,
        directory: &Path,
    ) -> anyhow::Result<Vec<RevisionEntry>> {
        let directory = self.operation_relative(directory)?;
        let root = self.repo.revparse_single(revision)?.peel_to_tree()?;
        let tree = if directory.as_os_str().is_empty() {
            root
        } else {
            let id = root.get_path(&directory)?.id();
            self.repo.find_tree(id)?
        };
        let mut result = Vec::new();
        for entry in tree.iter() {
            let directory_entry = entry.kind() == Some(git2::ObjectType::Tree);
            result.push(RevisionEntry {
                path: directory.join(entry.name()?),
                directory: directory_entry,
                oid: entry.id().to_string(),
                size: if directory_entry {
                    0
                } else {
                    self.repo
                        .find_blob(entry.id())
                        .map(|b| b.size())
                        .unwrap_or(0)
                },
            });
        }
        result.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| a.path.cmp(&b.path))
        });
        Ok(result)
    }
    pub fn file_history_from(
        &self,
        path: &Path,
        revision: &str,
        follow_renames: bool,
        limit: usize,
        all_branches: bool,
    ) -> anyhow::Result<Vec<HistoryRevision>> {
        self.file_history_cancellable(
            path,
            revision,
            follow_renames,
            limit,
            all_branches,
            HistoryControl {
                cancel: &std::sync::atomic::AtomicBool::new(false),
                progress: &std::sync::atomic::AtomicUsize::new(0),
            },
        )
    }
    pub fn file_history_cancellable(
        &self,
        path: &Path,
        revision: &str,
        follow_renames: bool,
        limit: usize,
        all_branches: bool,
        control: HistoryControl<'_>,
    ) -> anyhow::Result<Vec<HistoryRevision>> {
        let HistoryControl { cancel, progress } = control;
        let mut path = self.operation_relative(path)?;
        let mut walk = self.repo.revwalk()?;
        walk.push(self.repo.revparse_single(revision)?.peel_to_commit()?.id())?;
        if all_branches {
            let _ = walk.push_glob("refs/heads/*");
            let _ = walk.push_glob("refs/remotes/*");
        }
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        let mut result = Vec::new();
        for id in walk {
            anyhow::ensure!(
                !cancel.load(std::sync::atomic::Ordering::Acquire),
                "History cancelled"
            );
            progress.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let c = self.repo.find_commit(id?)?;
            let tree = c.tree()?;
            let parent = c.parent(0).ok().and_then(|p| p.tree().ok());
            let mut options = git2::DiffOptions::new();
            if !path.as_os_str().is_empty() {
                options.pathspec(&path);
            }
            let changed = self
                .repo
                .diff_tree_to_tree(parent.as_ref(), Some(&tree), Some(&mut options))?
                .deltas()
                .len()
                > 0;
            if !changed {
                continue;
            }
            result.push(HistoryRevision {
                path: self.root.join(&path),
                commit: CommitInfo {
                    id: c.id().to_string(),
                    short_id: c.id().to_string()[..8].into(),
                    summary: c.summary().ok().flatten().unwrap_or("").into(),
                    message: c.message().unwrap_or("").into(),
                    author: c.author().name().unwrap_or("").into(),
                    email: c.author().email().unwrap_or("").into(),
                    time: c.time().seconds(),
                    parents: c.parent_ids().map(|p| p.to_string()).collect(),
                    refs: Vec::new(),
                },
            });
            if follow_renames {
                let mut diff = self
                    .repo
                    .diff_tree_to_tree(parent.as_ref(), Some(&tree), None)?;
                let mut options = git2::DiffFindOptions::new();
                options.renames(true);
                diff.find_similar(Some(&mut options))?;
                if let Some(old) = diff
                    .deltas()
                    .find(|delta| {
                        delta.status() == git2::Delta::Renamed
                            && delta.new_file().path() == Some(path.as_path())
                    })
                    .and_then(|delta| delta.old_file().path().map(Path::to_path_buf))
                {
                    path = old;
                }
            }
            if result.len() >= limit {
                break;
            }
        }
        Ok(result)
    }
    pub fn blame_revision(
        &self,
        path: &Path,
        revision: &str,
        ignore_whitespace: bool,
        selection: Option<(usize, usize)>,
    ) -> anyhow::Result<Vec<BlameLine>> {
        let path = self.operation_relative(path)?;
        let mut options = git2::BlameOptions::new();
        options
            .newest_commit(self.repo.revparse_single(revision)?.peel_to_commit()?.id())
            .ignore_whitespace(ignore_whitespace);
        if let Some((start, end)) = selection {
            anyhow::ensure!(
                start > 0 && end >= start,
                "Line ranges are one-based and inclusive"
            );
            options.min_line(start).max_line(end);
        }
        let blame = self.repo.blame_file(&path, Some(&mut options))?;
        let mut rows = Vec::new();
        for hunk in blame.iter() {
            let commit = self.repo.find_commit(hunk.final_commit_id())?;
            for offset in 0..hunk.lines_in_hunk() {
                rows.push(BlameLine {
                    line: hunk.final_start_line() + offset,
                    commit: commit.id().to_string(),
                    author: commit.author().name().unwrap_or("").into(),
                    time: commit.time().seconds(),
                    summary: commit.summary().ok().flatten().unwrap_or("").into(),
                    is_uncommitted: false,
                });
            }
        }
        Ok(rows)
    }
    pub fn selection_history(
        &self,
        path: &Path,
        revision: &str,
        selection: (usize, usize),
        ignore_whitespace: bool,
    ) -> anyhow::Result<Vec<CommitInfo>> {
        let rows = self.blame_revision(path, revision, ignore_whitespace, Some(selection))?;
        let mut seen = std::collections::HashSet::new();
        let mut commits = Vec::new();
        for row in rows {
            if !seen.insert(row.commit.clone()) {
                continue;
            }
            let c = self.repo.find_commit(git2::Oid::from_str(&row.commit)?)?;
            commits.push(CommitInfo {
                id: row.commit,
                short_id: c.id().to_string()[..8].into(),
                summary: row.summary,
                message: c.message().unwrap_or("").into(),
                author: row.author,
                email: c.author().email().unwrap_or("").into(),
                time: row.time,
                parents: c.parent_ids().map(|p| p.to_string()).collect(),
                refs: Vec::new(),
            });
        }
        commits.sort_by_key(|c| std::cmp::Reverse(c.time));
        Ok(commits)
    }
    pub fn compare_revision_paths(
        &self,
        old: &str,
        new: &str,
        path: Option<&Path>,
    ) -> anyhow::Result<Vec<FileDiff>> {
        let mut files = self.diff_commits(old, new)?;
        if let Some(path) = path {
            let path = self.root.join(self.operation_relative(path)?);
            files.retain(|file| {
                file.path.starts_with(&path)
                    || file
                        .old_path
                        .as_ref()
                        .is_some_and(|old| old.starts_with(&path))
            });
        }
        Ok(files)
    }
    pub fn mark_operation_resolved(&self, path: &Path) -> anyhow::Result<()> {
        let rel = self.operation_relative(path)?;
        if self.root.join(&rel).exists() {
            let text = std::fs::read(self.root.join(&rel))?;
            anyhow::ensure!(
                !text
                    .windows(7)
                    .any(|bytes| bytes == b"<<<<<<<" || bytes == b">>>>>>>"),
                "Conflict markers remain; edit or merge before marking resolved"
            );
            let _operation =
                crate::RepositoryOperation::enter(&self.root, "Mark conflict resolved")?;
            let mut index = self.repo.index()?;
            index.read(false)?;
            let _ = index.conflict_remove(&rel);
            index.add_path(&rel)?;
            index.write()?;
        } else {
            let _operation = crate::RepositoryOperation::enter(&self.root, "Resolve deleted file")?;
            let mut index = self.repo.index()?;
            index.read(false)?;
            let _ = index.conflict_remove(&rel);
            if let Err(error) = index.remove_path(&rel) {
                if error.code() != git2::ErrorCode::NotFound {
                    return Err(error.into());
                }
            }
            index.write()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pull_options_reject_conflicting_modes() {
        let options = Pull {
            remote: "origin".into(),
            branch: "main".into(),
            rebase: true,
            squash: true,
            ..Default::default()
        };
        assert!(Request::pull(&options).is_err());
    }
    #[test]
    fn arguments_preserve_messages_and_paths_as_single_tokens() {
        let request = Request::merge(&Merge {
            branch: "feature/x".into(),
            message: "line one\nline two".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            request.args,
            vec!["merge", "-m", "line one\nline two", "feature/x"]
        );
        assert!(Request::reset("--hard", Reset::Soft).is_err());
    }
    #[test]
    fn stash_branch_uses_exact_selected_stash() {
        assert_eq!(
            Request::stash_action("branch", 3, Some("restore/hotfix"))
                .unwrap()
                .args,
            vec!["stash", "branch", "restore/hotfix", "stash@{3}"]
        );
    }
    #[test]
    fn update_shelves_restore_exact_reference_and_keep_network_steps_supervised() {
        let steps = Request::update(true, 1, true).unwrap();
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[1].args[0], "pull");
        assert!(steps[1].args.iter().any(|arg| arg == "--rebase"));
        assert_eq!(steps[2].args[0], "submodule");
        let restore = steps[3].args.join(" ");
        assert!(restore.contains("refs/rustpy/update-shelves/"));
        assert!(restore.contains("apply --index"));
        assert!(!restore.contains("stash pop"));
        assert!(!restore.contains("stash@{0}"));
    }
    #[test]
    fn merge_pull_overrides_global_rebase_default() {
        let request = Request::pull(&Pull::default()).unwrap();
        assert!(request.args.contains(&"--no-rebase".into()));
        assert!(request.args.contains(&"--no-edit".into()));
    }
    #[test]
    fn attributes_and_relative_paths_keep_repository_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let _native = git2::Repository::init(directory.path()).unwrap();
        std::fs::write(
            directory.path().join(".gitattributes"),
            "*.bin filter=lfs -text\n*.py text eol=lf\n",
        )
        .unwrap();
        std::fs::write(
            directory.path().join("model.bin"),
            "version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 10\n",
        )
        .unwrap();
        let repo = GitRepo::discover(directory.path()).unwrap();
        let attrs = repo
            .operation_attributes(&directory.path().join("model.bin"))
            .unwrap();
        assert_eq!(attrs.filter.as_deref(), Some("lfs"));
        assert_eq!(attrs.text.as_deref(), Some("false"));
        assert!(attrs.lfs_pointer);
        assert!(repo.operation_relative(Path::new("../outside.py")).is_err());
        assert_eq!(
            repo.operation_relative(&directory.path().join("module space.py"))
                .unwrap(),
            PathBuf::from("module space.py")
        );
    }
}
