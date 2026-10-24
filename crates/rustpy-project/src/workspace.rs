//! Loaded project roots for ordered, possibly multi-folder workspaces.

use crate::{ProjectModel, ProjectScope};
use rustpy_settings::Workspace;
use std::path::{Path, PathBuf};

/// A workspace's validated persisted roots, shared by IDE, CLI and LSP services.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    /// Canonical workspace folders, in client order, with aliases deduplicated.
    pub folders: Vec<PathBuf>,
    pub content_roots: Vec<PathBuf>,
    pub source_roots: Vec<PathBuf>,
    pub test_roots: Vec<PathBuf>,
    pub excluded: Vec<PathBuf>,
    pub ide_interpreter: Option<PathBuf>,
    /// Invalid attached roots omitted by the persisted project-model validator.
    pub warnings: Vec<String>,
    loaded: bool,
}

impl WorkspaceSnapshot {
    /// Read shared project.json or migrate legacy workspace roots without writes.
    /// An unreadable or malformed shared file is an error, never a fallback.
    pub fn try_load(folders: Vec<PathBuf>) -> Result<Self, String> {
        let mut snapshot = Self::unavailable(folders);
        for (index, folder) in snapshot.folders.iter().enumerate() {
            let (model, warnings) = ProjectModel::try_load_reporting(folder)?;
            if index == 0 {
                snapshot.ide_interpreter = model.sdk.clone().or(Workspace::try_load(folder)
                    .map_err(|error| error.to_string())?
                    .interpreter);
            }
            extend_unique(&mut snapshot.content_roots, model.content_roots);
            extend_unique(&mut snapshot.source_roots, model.source_roots);
            extend_unique(&mut snapshot.test_roots, model.test_roots);
            extend_unique(&mut snapshot.excluded, model.excluded);
            snapshot.warnings.extend(warnings);
        }
        snapshot.loaded = true;
        Ok(snapshot)
    }

    /// Retain workspace identities after a load failure, with no disk-walk or
    /// import roots. Clients can keep open documents and retry when files change.
    pub fn unavailable(folders: Vec<PathBuf>) -> Self {
        let mut snapshot = Self::default();
        for folder in folders {
            let root = folder.canonicalize().unwrap_or(folder);
            if !snapshot.folders.contains(&root) {
                snapshot.folders.push(root);
            }
        }
        snapshot
    }

    pub fn primary(&self) -> Option<&Path> {
        self.folders.first().map(PathBuf::as_path)
    }

    /// Content roots in workspace order, then attached, source and test roots.
    pub fn roots(&self) -> Vec<PathBuf> {
        if !self.loaded {
            return Vec::new();
        }
        let mut roots = self.folders.clone();
        extend_unique(&mut roots, self.content_roots.iter().cloned());
        extend_unique(&mut roots, self.source_roots.iter().cloned());
        extend_unique(&mut roots, self.test_roots.iter().cloned());
        roots
    }

    pub fn scope(&self) -> ProjectScope {
        if !self.loaded {
            return ProjectScope::default();
        }
        ProjectScope::new(self.primary().unwrap_or(Path::new("")))
            .with_content_roots(
                self.folders
                    .iter()
                    .skip(1)
                    .chain(&self.content_roots)
                    .cloned(),
            )
            .with_excluded(self.excluded.iter().cloned())
    }
}

fn extend_unique(paths: &mut Vec<PathBuf>, additions: impl IntoIterator<Item = PathBuf>) {
    for path in additions {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
}
