//! The server's own file watcher, used when the client cannot watch files
//! for it (no dynamic `workspace/didChangeWatchedFiles` registration, as with
//! Neovim on Linux). Like the IDE's watcher it reports changes to Python
//! files, folders, `.editorconfig`, `pyvenv.cfg` and shared `.rustpy/project.json` / legacy `workspace.json`,
//! skipping VCS metadata, caches and virtual environments.

use std::path::{Path, PathBuf};

use lsp_types::FileChangeType;

/// Keeps watching while alive.
pub struct FsWatcher {
    _watcher: notify::RecommendedWatcher,
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "__pycache__",
    "target",
    "node_modules",
    ".venv",
    "venv",
    ".tox",
    ".nox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".idea",
];

/// Whether a change of `path` can matter to the analysis.
pub fn relevant(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    // A virtual environment appearing (or changing) changes the interpreter.
    if name == "pyvenv.cfg" {
        return true;
    }
    let mut in_rustpy_dir = false;
    for c in path.components() {
        let Some(name) = c.as_os_str().to_str() else {
            continue;
        };
        if SKIP_DIRS.contains(&name) {
            return false;
        }
        in_rustpy_dir |= name == ".rustpy";
    }
    if in_rustpy_dir {
        return matches!(name, "workspace.json" | "project.json");
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("py" | "pyi") => true,
        // Folders (package directories appearing or disappearing).
        None => !name.starts_with('.') || name == ".editorconfig",
        _ => false,
    }
}

/// Watches `folders` recursively; `on_change` receives each batch of relevant changes.
pub fn watch(
    folders: &[PathBuf],
    on_change: impl Fn(Vec<(PathBuf, FileChangeType)>) + Send + 'static,
) -> Option<FsWatcher> {
    use notify::event::{EventKind, ModifyKind};
    use notify::Watcher;
    let mut watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
        let Ok(ev) = ev else { return };
        let typ = match ev.kind {
            EventKind::Access(_) => return,
            EventKind::Create(_) => FileChangeType::CREATED,
            EventKind::Remove(_) => FileChangeType::DELETED,
            // A rename removes one path and creates another: the set of files changed.
            EventKind::Modify(ModifyKind::Name(_)) => FileChangeType::DELETED,
            _ => FileChangeType::CHANGED,
        };
        let changes: Vec<(PathBuf, FileChangeType)> = ev
            .paths
            .into_iter()
            .filter(|p| relevant(p))
            .map(|p| (p, typ))
            .collect();
        if !changes.is_empty() {
            on_change(changes);
        }
    })
    .map_err(|e| log::warn!("lsp: file watcher unavailable: {e}"))
    .ok()?;
    let mut watching = false;
    for f in folders {
        match watcher.watch(f, notify::RecursiveMode::Recursive) {
            Ok(()) => watching = true,
            Err(e) => log::warn!("lsp: cannot watch {}: {e}", f.display()),
        }
    }
    watching.then_some(FsWatcher { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_analysis_inputs_are_relevant() {
        assert!(relevant(Path::new("/p/pkg/mod.py")));
        assert!(relevant(Path::new("/p/pkg/mod.pyi")));
        assert!(relevant(Path::new("/p/newpkg")));
        assert!(relevant(Path::new("/p/.editorconfig")));
        assert!(relevant(Path::new("/p/.rustpy/workspace.json")));
        assert!(relevant(Path::new("/p/.rustpy/project.json")));
        assert!(relevant(Path::new("/p/env/pyvenv.cfg")));
        assert!(relevant(Path::new("/p/.venv/pyvenv.cfg")));
        assert!(!relevant(Path::new("/p/.rustpy/runConfigurations.json")));
        assert!(!relevant(Path::new("/p/.git/index")));
        assert!(!relevant(Path::new(
            "/p/pkg/__pycache__/mod.cpython-312.pyc"
        )));
        assert!(!relevant(Path::new(
            "/p/.venv/lib/python3.12/site-packages/x.py"
        )));
        assert!(!relevant(Path::new("/p/README.md")));
        assert!(!relevant(Path::new("/p/setup.cfg")));
    }
}
