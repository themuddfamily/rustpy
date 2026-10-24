//! Patch operations used by the commit workspace. Checks precede mutations.
use crate::GitRepo;
use anyhow::{bail, Context};
use std::path::{Component, Path};
/// One patch-conflict input: path, base, current contents, patched contents.
pub type PatchMergeInput = (
    std::path::PathBuf,
    Option<String>,
    Option<String>,
    Option<String>,
);
impl GitRepo {
    pub fn workspace_layers(&self) -> anyhow::Result<Vec<crate::StatusEntry>> {
        let mut result = Vec::new();
        for entry in self.status()? {
            let status = self
                .repo
                .status_file(entry.path.strip_prefix(self.root())?)
                .unwrap_or(git2::Status::CURRENT);
            if entry.staged {
                result.push(entry.clone());
            }
            if !entry.staged
                || status.is_wt_modified()
                || status.is_wt_deleted()
                || status.is_wt_renamed()
                || status.is_wt_typechange()
                || status.is_wt_new()
            {
                let mut unstaged = entry;
                if unstaged.staged && unstaged.status == crate::FileStatus::Renamed {
                    unstaged.status = crate::FileStatus::Modified;
                }
                unstaged.staged = false;
                result.push(unstaged);
            }
        }
        Ok(result)
    }
    pub fn workspace_export(&self, paths: &[std::path::PathBuf]) -> anyhow::Result<String> {
        if paths.is_empty() {
            return Ok(String::new());
        }
        let tree = self
            .repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_tree().ok());
        let mut options = git2::DiffOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true)
            .show_binary(true);
        for path in paths {
            options.pathspec(
                path.strip_prefix(self.root())
                    .context("Patch file outside repository")?,
            );
        }
        for entry in self.status()? {
            if paths.contains(&entry.path) {
                if let Some(old) = entry.old_path {
                    options.pathspec(old.strip_prefix(self.root())?);
                }
            }
        }
        let mut diff = self
            .repo
            .diff_tree_to_workdir_with_index(tree.as_ref(), Some(&mut options))?;
        let mut find = git2::DiffFindOptions::new();
        find.renames(true);
        diff.find_similar(Some(&mut find))?;
        let mut bytes = Vec::new();
        diff.print(git2::DiffFormat::Patch, |_, _, line| {
            if matches!(line.origin(), '+' | '-' | ' ') {
                bytes.push(line.origin() as u8);
            }
            bytes.extend_from_slice(line.content());
            true
        })?;
        if bytes.len() > 8 * 1024 * 1024 {
            bail!("Patch exceeds 8 MiB; select fewer changes");
        }
        Ok(String::from_utf8(bytes)?)
    }
    pub fn workspace_patch_merge(
        &self,
        base: &str,
        text: &str,
    ) -> anyhow::Result<Vec<PatchMergeInput>> {
        let tree = self.repo.revparse_single(base)?.peel_to_tree()?;
        let diff = git2::Diff::from_buffer(text.as_bytes())?;
        self.workspace_validate_patch(&diff)?;
        let index = self.repo.apply_to_tree(&tree, &diff, None)?;
        let mut result = Vec::new();
        for delta in diff.deltas() {
            let relative = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .context("Patch has no path")?;
            let path = self.root().join(relative);
            let original = tree
                .get_path(relative)
                .ok()
                .map(|entry| {
                    self.repo.find_blob(entry.id()).and_then(|blob| {
                        String::from_utf8(blob.content().to_vec()).map_err(|_| {
                            git2::Error::from_str("Binary conflict requires external resolution")
                        })
                    })
                })
                .transpose()?;
            let theirs = index
                .get_path(relative, 0)
                .map(|entry| {
                    self.repo.find_blob(entry.id).and_then(|blob| {
                        String::from_utf8(blob.content().to_vec()).map_err(|_| {
                            git2::Error::from_str("Binary conflict requires external resolution")
                        })
                    })
                })
                .transpose()?;
            let ours = if path.exists() {
                Some(std::fs::read_to_string(&path)?)
            } else {
                None
            };
            result.push((path, original, ours, theirs));
        }
        Ok(result)
    }
    fn workspace_validate_patch(&self, diff: &git2::Diff<'_>) -> anyhow::Result<()> {
        for delta in diff.deltas() {
            for file in [delta.old_file(), delta.new_file()] {
                if let Some(path) = file.path() {
                    if path.is_absolute()
                        || path
                            .components()
                            .any(|component| !matches!(component, Component::Normal(_)))
                    {
                        bail!("Patch path escapes repository");
                    }
                    let canonical_root = self.root().canonicalize()?;
                    let mut ancestor = self.root().to_path_buf();
                    for component in path.components() {
                        ancestor.push(component);
                        if let Ok(metadata) = std::fs::symlink_metadata(&ancestor) {
                            if metadata.file_type().is_symlink()
                                && !ancestor.canonicalize()?.starts_with(&canonical_root)
                            {
                                bail!("Patch follows a symlink outside repository");
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub fn workspace_index_text(&self, path: &Path) -> anyhow::Result<String> {
        let relative = path
            .strip_prefix(self.root())
            .context("File outside repository")?;
        let index = self.repo.index()?;
        let entry = index
            .get_path(relative, 0)
            .context("File is not in the index")?;
        Ok(String::from_utf8(
            self.repo.find_blob(entry.id)?.content().to_vec(),
        )?)
    }
    /// Update only the captured staged version; unstaged edits and other entries stay intact.
    pub fn workspace_replace_index_text(
        &self,
        path: &Path,
        expected: &str,
        replacement: &str,
    ) -> anyhow::Result<()> {
        let relative = path
            .strip_prefix(self.root())
            .context("File outside repository")?;
        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            bail!("Invalid staged file path");
        }
        let mut index = self.repo.index()?;
        index.read(true)?;
        let entry = index
            .get_path(relative, 0)
            .context("File is not in the index")?;
        if self.repo.find_blob(entry.id)?.content() != expected.as_bytes() {
            bail!(
                "Staged file changed while preparing commit: {}",
                path.display()
            );
        }
        if expected != replacement {
            index.add_frombuffer(&entry, replacement.as_bytes())?;
            index.write()?;
        }
        Ok(())
    }
    pub fn workspace_revision(&self) -> String {
        self.repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .map(|id| id.to_string())
            .unwrap_or_default()
    }
    pub fn workspace_patch(&self, staged: bool) -> anyhow::Result<String> {
        let mut options = git2::DiffOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        let tree = self
            .repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_tree().ok());
        let diff = if staged {
            self.repo
                .diff_tree_to_index(tree.as_ref(), None, Some(&mut options))?
        } else {
            self.repo.diff_index_to_workdir(None, Some(&mut options))?
        };
        let mut bytes = Vec::new();
        diff.print(git2::DiffFormat::Patch, |_, _, line| {
            if matches!(line.origin(), '+' | '-' | ' ') {
                bytes.push(line.origin() as u8);
            }
            bytes.extend_from_slice(line.content());
            true
        })?;
        Ok(String::from_utf8(bytes)?)
    }
    /// Libgit2 rejects mismatched context. No path may escape this worktree.
    pub fn workspace_apply_patch(
        &self,
        text: &str,
        index: bool,
        check: bool,
    ) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(self.root(), "Apply patch")?;
        let diff = git2::Diff::from_buffer(text.as_bytes()).context("Invalid unified patch")?;
        self.workspace_validate_patch(&diff)?;
        let mut options = git2::ApplyOptions::new();
        options.check(check);
        self.repo.apply(
            &diff,
            if index {
                git2::ApplyLocation::Index
            } else {
                git2::ApplyLocation::WorkDir
            },
            Some(&mut options),
        )?;
        Ok(())
    }
    pub fn workspace_stage(&self, path: &Path) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(self.root(), "Stage change")?;
        let relative = path
            .strip_prefix(self.root())
            .context("File outside repository")?;
        let mut index = self.repo.index()?;
        if path.exists() {
            index.add_path(relative)?;
        } else {
            index.remove_path(relative)?;
        }
        index.write()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staged_cleanup_preserves_unstaged_text_and_unrelated_entries() {
        let directory = tempfile::tempdir().unwrap();
        let repo = GitRepo::init(directory.path()).unwrap();
        let path = directory.path().join("notebook.ipynb");
        let other = directory.path().join("other.py");
        std::fs::write(&path, "staged notebook outputs").unwrap();
        std::fs::write(&other, "other staged text").unwrap();
        let mut index = repo.repo.index().unwrap();
        index.add_path(Path::new("notebook.ipynb")).unwrap();
        index.add_path(Path::new("other.py")).unwrap();
        index.write().unwrap();
        std::fs::write(&path, "unstaged cell edit").unwrap();
        repo.workspace_replace_index_text(
            &path,
            "staged notebook outputs",
            "staged notebook cleared",
        )
        .unwrap();
        assert_eq!(
            repo.workspace_index_text(&path).unwrap(),
            "staged notebook cleared"
        );
        assert_eq!(
            repo.workspace_index_text(&other).unwrap(),
            "other staged text"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "unstaged cell edit"
        );
        assert!(repo
            .workspace_replace_index_text(&path, "staged notebook outputs", "stale replacement")
            .is_err());
        assert_eq!(
            repo.workspace_index_text(&path).unwrap(),
            "staged notebook cleared"
        );
    }
    #[test]
    fn index_patch_does_not_change_worktree_and_rejects_stale_context() {
        let directory = tempfile::tempdir().unwrap();
        let repo = GitRepo::init(directory.path()).unwrap();
        let path = directory.path().join("a.py");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let mut index = repo.repo.index().unwrap();
        index.add_path(Path::new("a.py")).unwrap();
        index.write().unwrap();
        let patch="diff --git a/a.py b/a.py\n--- a/a.py\n+++ b/a.py\n@@ -1,2 +1,2 @@\n one\n-two\n+changed\n";
        repo.workspace_apply_patch(patch, true, true).unwrap();
        assert_eq!(repo.workspace_index_text(&path).unwrap(), "one\ntwo\n");
        repo.workspace_apply_patch(patch, true, false).unwrap();
        assert_eq!(repo.workspace_index_text(&path).unwrap(), "one\nchanged\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        assert!(repo.workspace_apply_patch(patch, true, true).is_err());
    }
    #[test]
    fn patch_traversal_and_absolute_paths_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let repo = GitRepo::init(directory.path()).unwrap();
        for path in ["../escape.py", "/absolute.py"] {
            let patch=format!("diff --git a/{path} b/{path}\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+escaped\n");
            assert!(repo.workspace_apply_patch(&patch, false, true).is_err());
        }
    }
}
