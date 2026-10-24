//! Project roots and exclusions shared by enumeration consumers.

use std::path::{Path, PathBuf};

/// What a project-wide walk covers: the project root plus extra content roots,
/// minus VCS/cache directories and the directories marked Excluded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectScope {
    pub root: PathBuf,
    /// Content roots besides `root` (attached folders); nested ones are walked once.
    pub content_roots: Vec<PathBuf>,
    /// Absolute directories whose files are left out of search, navigation and refactoring.
    pub excluded: Vec<PathBuf>,
    pub ignored_patterns: String,
}

impl ProjectScope {
    /// The walk scope of an already loaded persisted project structure.
    pub fn from_model(root: impl Into<PathBuf>, model: &crate::ProjectModel) -> Self {
        Self::new(root)
            .with_content_roots(model.content_roots.iter().cloned())
            .with_excluded(model.excluded.iter().cloned())
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ..Default::default()
        }
    }

    pub fn with_excluded(mut self, excluded: impl IntoIterator<Item = PathBuf>) -> Self {
        self.excluded = excluded.into_iter().collect();
        self.excluded.sort();
        self
    }

    pub fn with_content_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        self.content_roots = roots.into_iter().collect();
        self.content_roots.sort();
        self
    }

    /// Whether `path` is an excluded directory or inside one (component-wise).
    pub fn is_excluded(&self, path: &Path) -> bool {
        self.excluded.iter().any(|e| path.starts_with(e))
            || path.components().any(|part| {
                rustpy_settings::editor_pages::matches(
                    &self.ignored_patterns,
                    Path::new(part.as_os_str()),
                )
            })
            || rustpy_settings::editor_pages::matches(&self.ignored_patterns, path)
    }

    /// Whether `path` is under a walked root and not excluded.
    pub fn contains(&self, path: &Path) -> bool {
        self.walk_roots().iter().any(|r| path.starts_with(r)) && !self.is_excluded(path)
    }

    /// The directories to walk: the root, then the content roots that are not
    /// inside another root.
    pub fn walk_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        for r in std::iter::once(&self.root).chain(self.content_roots.iter()) {
            if r.as_os_str().is_empty() || roots.iter().any(|seen| r.starts_with(seen)) {
                continue;
            }
            roots.retain(|seen| !seen.starts_with(r));
            roots.push(r.clone());
        }
        roots
    }

    /// Drops the excluded files from a list built before a folder was excluded.
    pub fn filter(&self, files: &[PathBuf]) -> Vec<PathBuf> {
        if self.excluded.is_empty() && self.ignored_patterns.is_empty() {
            return files.to_vec();
        }
        files
            .iter()
            .filter(|p| !self.is_excluded(p))
            .cloned()
            .collect()
    }
}
