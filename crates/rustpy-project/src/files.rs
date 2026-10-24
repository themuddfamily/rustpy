//! Project file enumeration respecting roots, ignore rules and exclusions.

use crate::scope::ProjectScope;
use rustpy_core::ProgressReporter;
use std::path::{Path, PathBuf};

const SKIP_DIRS: &[&str] = &[
    "__pycache__",
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "venv",
    ".venv",
    "env",
    ".env",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    "build",
    "dist",
    "target",
    ".idea",
    ".rustpy",
];

/// Lists project files (respecting .gitignore and skipping caches).
pub fn project_files(root: &Path) -> Vec<PathBuf> {
    project_files_in(&ProjectScope::new(root))
}

/// [`project_files`] without the contents of `excluded` directories (folders
/// marked *Excluded* in the Project view).
pub fn project_files_excluding(root: &Path, excluded: &[PathBuf]) -> Vec<PathBuf> {
    project_files_in(&ProjectScope::new(root).with_excluded(excluded.iter().cloned()))
}

/// Lists the files of `scope`, sorted: `.gitignore` is respected, caches and
/// VCS directories are skipped and excluded directories are not entered.
pub fn project_files_in(scope: &ProjectScope) -> Vec<PathBuf> {
    project_files_in_with_cancel(scope, &|| false)
}

/// Enumerates the scope until cancelled; returns the sorted files collected so far.
pub fn project_files_in_with_cancel(scope: &ProjectScope, stop: &dyn Fn() -> bool) -> Vec<PathBuf> {
    let roots = scope.walk_roots();
    let Some((first, rest)) = roots.split_first() else {
        return Vec::new();
    };
    let mut builder = ignore::WalkBuilder::new(first);
    for r in rest {
        builder.add(r);
    }
    let excluded = scope.excluded.clone();
    let ignored = scope.ignored_patterns.clone();
    builder
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .follow_links(false)
        .max_depth(Some(24))
        .filter_entry(move |e| {
            let skipped = e
                .path()
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| SKIP_DIRS.contains(&n));
            !skipped
                && !excluded.iter().any(|x| e.path().starts_with(x))
                && !rustpy_settings::editor_pages::matches(&ignored, e.path())
        });
    let mut files = Vec::new();
    for entry in builder.build().flatten() {
        if stop() {
            break;
        }
        if entry.file_type().is_some_and(|t| t.is_file()) {
            files.push(entry.into_path());
        }
    }
    files.sort();
    files.dedup();
    files
}

/// [`project_files_excluding`] for background tasks: stops early (with the
/// files found so far) once `progress` is cancelled.
pub fn project_files_with_progress(
    root: &Path,
    excluded: &[PathBuf],
    progress: &ProgressReporter,
) -> Vec<PathBuf> {
    project_files_in_with_cancel(
        &ProjectScope::new(root).with_excluded(excluded.iter().cloned()),
        &|| progress.is_cancelled(),
    )
}
