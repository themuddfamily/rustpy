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

/// Whether `dir` is tagged as a cache by a `CACHEDIR.TAG` file (Cache Directory
/// Tagging Specification): Cargo target directories under any name, and the
/// caches of pip and other tools. Build output is never project source.
fn is_cache_dir(dir: &Path) -> bool {
    use std::io::Read;
    const SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
    let Ok(file) = std::fs::File::open(dir.join("CACHEDIR.TAG")) else {
        return false;
    };
    let mut head = Vec::with_capacity(SIGNATURE.len());
    file.take(SIGNATURE.len() as u64)
        .read_to_end(&mut head)
        .is_ok_and(|_| head == SIGNATURE)
}

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
                && !(e.file_type().is_some_and(|t| t.is_dir()) && is_cache_dir(e.path()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_cache_directories_are_not_project_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (path, text) in [
            ("main.py", "x = 1\n"),
            // A Cargo target directory under a name no rule knows.
            (
                "target-win/CACHEDIR.TAG",
                "Signature: 8a477f597d28d172789f06886806bc55\n# cache\n",
            ),
            ("target-win/debug/build/gen.py", "y = 2\n"),
            // Only the exact signature marks a cache.
            ("notes/CACHEDIR.TAG", "not a cache tag\n"),
            ("notes/kept.py", "z = 3\n"),
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let files: Vec<_> = project_files(root)
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "py"))
            .collect();
        assert_eq!(files, [root.join("main.py"), root.join("notes/kept.py")]);
    }
}
