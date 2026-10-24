//! Declaration author summaries from real Git blame of the current editor buffer.
use crate::GitRepo;
use rustpy_core::CancellationToken;
use std::{collections::BTreeMap, ops::Range, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contributor {
    pub name: String,
    pub committed_lines: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorLens {
    /// Zero-based, half-open source line range supplied by the syntax owner.
    pub lines: Range<usize>,
    /// Most contributing lines first; ties use author name for stable display.
    pub contributors: Vec<Contributor>,
    pub uncommitted_lines: usize,
}
impl GitRepo {
    /// Exact HEAD identity for stale-result validation; unborn HEAD is None.
    pub fn code_vision_head(&self) -> anyhow::Result<Option<String>> {
        match self.repo.head() {
            Ok(head) => Ok(Some(head.peel_to_commit()?.id().to_string())),
            Err(error)
                if matches!(
                    error.code(),
                    git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// The UI must call from a trusted background job and reject results when
    /// document/index/repository identity changes. No invented owner is emitted
    /// for uncommitted lines or an unnamed commit author.
    pub fn code_authors(
        &self,
        path: &Path,
        text: &str,
        declarations: &[Range<usize>],
        token: &CancellationToken,
    ) -> anyhow::Result<Vec<AuthorLens>> {
        token.check()?;
        anyhow::ensure!(
            text.len() <= 1024 * 1024 && declarations.len() <= 128,
            "code author budget exceeded"
        );
        let line_count = text.lines().count();
        anyhow::ensure!(
            declarations
                .iter()
                .all(|range| range.start < range.end && range.end <= line_count),
            "invalid declaration line range"
        );
        if declarations.is_empty() {
            return Ok(Vec::new());
        }
        // libgit2's blame call has no cancellation callback. Bound buffer size
        // and check cancellation immediately before/after that library call.
        let blame = self.blame_contents(path, text.as_bytes())?;
        token.check()?;
        let mut lenses = Vec::with_capacity(declarations.len());
        for lines in declarations {
            token.check()?;
            let mut counts = BTreeMap::<String, usize>::new();
            let mut uncommitted_lines = 0;
            for line in blame
                .iter()
                .filter(|line| lines.contains(&line.line.saturating_sub(1)))
            {
                token.check()?;
                if line.is_uncommitted {
                    uncommitted_lines += 1;
                } else if !line.author.is_empty() {
                    *counts.entry(line.author.clone()).or_default() += 1;
                }
            }
            let mut contributors: Vec<_> = counts
                .into_iter()
                .map(|(name, committed_lines)| Contributor {
                    name,
                    committed_lines,
                })
                .collect();
            contributors.sort_by(|a, b| {
                b.committed_lines
                    .cmp(&a.committed_lines)
                    .then(a.name.cmp(&b.name))
            });
            lenses.push(AuthorLens {
                lines: lines.clone(),
                contributors,
                uncommitted_lines,
            });
        }
        Ok(lenses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn repository() -> (tempfile::TempDir, GitRepo, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let repo = GitRepo::init(directory.path()).unwrap();
        let mut config = repo.repo.config().unwrap();
        config.set_str("user.name", "Original Author").unwrap();
        config.set_str("user.email", "first@example.test").unwrap();
        let path = directory.path().join("main.py");
        std::fs::write(&path, "def π():\n    value = 1\n    return value\n").unwrap();
        repo.commit(std::slice::from_ref(&path), "initial", false)
            .unwrap();
        (directory, repo, path)
    }
    #[test]
    fn actual_blame_preserves_committed_author_and_marks_unsaved_buffer_lines() {
        let (_directory, repo, path) = repository();
        let disk = std::fs::read_to_string(&path).unwrap();
        let text = "def π():\n    value = 2\n    return value\n";
        let lenses = repo
            .code_authors(
                &path,
                text,
                std::slice::from_ref(&(0..3)),
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(
            lenses[0].contributors,
            [Contributor {
                name: "Original Author".into(),
                committed_lines: 2
            }]
        );
        assert_eq!(lenses[0].uncommitted_lines, 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), disk);
    }
    #[test]
    fn real_multiple_authors_are_counted_and_untracked_files_have_no_invented_author() {
        let (directory, repo, path) = repository();
        let mut config = repo.repo.config().unwrap();
        config.set_str("user.name", "Second Author").unwrap();
        config.set_str("user.email", "second@example.test").unwrap();
        let text = "def π():\n    value = 2\n    return value\n";
        std::fs::write(&path, text).unwrap();
        repo.commit(std::slice::from_ref(&path), "change", false)
            .unwrap();
        let lenses = repo
            .code_authors(
                &path,
                text,
                std::slice::from_ref(&(0..3)),
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(
            lenses[0].contributors,
            [
                Contributor {
                    name: "Original Author".into(),
                    committed_lines: 2
                },
                Contributor {
                    name: "Second Author".into(),
                    committed_lines: 1
                }
            ]
        );
        let untracked = directory.path().join("new.py");
        let lenses = repo
            .code_authors(
                &untracked,
                "def new():\n    return 1\n",
                std::slice::from_ref(&(0..2)),
                &CancellationToken::new(),
            )
            .unwrap();
        assert!(lenses[0].contributors.is_empty());
        assert_eq!(lenses[0].uncommitted_lines, 2);
    }
    #[test]
    fn cancellation_invalid_ranges_and_outside_repository_are_errors() {
        let (_directory, repo, path) = repository();
        let token = CancellationToken::new();
        token.cancel();
        assert!(repo
            .code_authors(&path, "x\n", std::slice::from_ref(&(0..1)), &token)
            .is_err());
        assert!(repo
            .code_authors(
                &path,
                "x\n",
                std::slice::from_ref(&(0..2)),
                &CancellationToken::new()
            )
            .is_err());
        assert!(repo
            .code_authors(
                Path::new("/outside/main.py"),
                "x\n",
                std::slice::from_ref(&(0..1)),
                &CancellationToken::new()
            )
            .is_err());
    }
}
