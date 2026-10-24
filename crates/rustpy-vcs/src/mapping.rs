//! Project directory mappings. Detection never follows symlinks or enters Git object stores.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    #[serde(default)]
    pub provider: crate::provider::Provider,
    pub path: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub linked_worktree: bool,
    pub submodule: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DirectoryMappings {
    /// Non-Git providers have their own operation adapters; never route them into GitRepo.
    pub providers: BTreeMap<PathBuf, crate::provider::Provider>,
    pub roots: BTreeSet<PathBuf>,
    pub ignored: BTreeSet<PathBuf>,
    pub selected: Option<PathBuf>,
    pub all_roots: bool,
}
impl DirectoryMappings {
    pub fn register_provider(&mut self, root: PathBuf, provider: crate::provider::Provider) {
        let root = dunce::canonicalize(&root).unwrap_or(root);
        if provider == crate::provider::Provider::Git {
            self.providers.remove(&root);
            self.register(root);
        } else {
            self.roots.remove(&root);
            self.ignored.remove(&root);
            self.providers.insert(root, provider);
        }
    }
    pub fn provider_for(&self, path: &Path) -> Option<(&Path, crate::provider::Provider)> {
        self.roots
            .iter()
            .map(|root| (root.as_path(), crate::provider::Provider::Git))
            .chain(
                self.providers
                    .iter()
                    .map(|(root, provider)| (root.as_path(), *provider)),
            )
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
    }
    pub fn load(project: &Path) -> Self {
        let mut mappings: Self = std::fs::read(project.join(".rustpy/vcs-mappings.json"))
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default();
        let resolve = |path: PathBuf| {
            if path.is_absolute() {
                path
            } else {
                project.join(path)
            }
        };
        mappings.roots = mappings.roots.into_iter().map(resolve).collect();
        mappings.ignored = mappings.ignored.into_iter().map(resolve).collect();
        mappings.providers = mappings
            .providers
            .into_iter()
            .map(|(root, provider)| (resolve(root), provider))
            .collect();
        mappings.selected = mappings.selected.map(resolve);
        mappings
    }
    pub fn save(&self, project: &Path) -> anyhow::Result<()> {
        let dir = project.join(".rustpy");
        std::fs::create_dir_all(&dir)?;
        let pending = dir.join("vcs-mappings.json.pending");
        let relative = |path: PathBuf| {
            path.strip_prefix(project)
                .map(Path::to_path_buf)
                .unwrap_or(path)
        };
        let mut stored = self.clone();
        stored.roots = stored.roots.into_iter().map(relative).collect();
        stored.ignored = stored.ignored.into_iter().map(relative).collect();
        stored.providers = stored
            .providers
            .into_iter()
            .map(|(root, provider)| (relative(root), provider))
            .collect();
        stored.selected = stored.selected.map(relative);
        std::fs::write(&pending, serde_json::to_vec_pretty(&stored)?)?;
        std::fs::rename(pending, dir.join("vcs-mappings.json"))?;
        Ok(())
    }
    pub fn root_for(&self, path: &Path) -> Option<&PathBuf> {
        let (root, provider) = self.provider_for(path)?;
        if provider != crate::provider::Provider::Git {
            return None;
        }
        self.roots.get(root)
    }
    pub fn operation_roots(&self) -> Vec<PathBuf> {
        if self.all_roots {
            self.roots.iter().cloned().collect()
        } else {
            self.selected.clone().into_iter().collect()
        }
    }
    pub fn register(&mut self, root: PathBuf) {
        self.ignored.remove(&root);
        self.roots.insert(root.clone());
        self.selected.get_or_insert(root);
    }
}
#[derive(Clone, Debug)]
pub struct DetectionLimits {
    pub directories: usize,
    pub depth: usize,
    pub duration: Duration,
}
impl Default for DetectionLimits {
    fn default() -> Self {
        Self {
            directories: 20_000,
            depth: 12,
            duration: Duration::from_secs(3),
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Detection {
    pub roots: Vec<Root>,
    pub errors: Vec<(PathBuf, String)>,
    pub truncated: bool,
}
pub struct Detector {
    pub receiver: Receiver<Detection>,
    cancel: Arc<AtomicBool>,
}
impl Drop for Detector {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
impl Detector {
    pub fn start(project: PathBuf, limits: DetectionLimits) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (tx, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = detect(&project, limits, &worker_cancel);
            let _ = tx.send(result);
        });
        Self { receiver, cancel }
    }
}
fn inspect(path: &Path) -> Result<Root, git2::Error> {
    let repo = git2::Repository::open(path)?;
    let root = repo.workdir().unwrap_or(path).to_path_buf();
    let git_dir = repo.path().to_path_buf();
    let common_dir = repo.commondir().to_path_buf();
    let submodule = git_dir.components().any(|c| c.as_os_str() == "modules");
    Ok(Root {
        provider: crate::provider::Provider::Git,
        path: root,
        linked_worktree: git_dir != common_dir,
        git_dir,
        common_dir,
        submodule,
    })
}
pub fn detect(project: &Path, limits: DetectionLimits, cancel: &AtomicBool) -> Detection {
    let start = Instant::now();
    let mut result = Detection::default();
    let mut seen = BTreeSet::new();
    for ancestor in project.ancestors() {
        if cancel.load(Ordering::Relaxed) || start.elapsed() >= limits.duration {
            result.truncated = true;
            return result;
        }
        for provider in [
            crate::provider::Provider::Mercurial,
            crate::provider::Provider::Subversion,
            crate::provider::Provider::Perforce,
            crate::provider::Provider::Tfs,
        ] {
            if provider.detect(ancestor) && seen.insert((ancestor.to_path_buf(), provider)) {
                result.roots.push(Root {
                    provider,
                    path: ancestor.to_path_buf(),
                    git_dir: PathBuf::new(),
                    common_dir: PathBuf::new(),
                    linked_worktree: false,
                    submodule: false,
                });
            }
        }
    }
    // Discover an ancestor root too, without recursively traversing its siblings.
    if let Ok(repo) = git2::Repository::discover(project) {
        if let Some(root) = repo.workdir() {
            if let Ok(found) = inspect(root) {
                seen.insert((found.path.clone(), found.provider));
                result.roots.push(found);
            }
        }
    }
    let mut pending = vec![(project.to_path_buf(), 0)];
    let mut visited = 0;
    let mut entries_seen = 0usize;
    'scan: while let Some((dir, depth)) = pending.pop() {
        if cancel.load(Ordering::Relaxed)
            || visited >= limits.directories
            || start.elapsed() >= limits.duration
        {
            result.truncated = true;
            break;
        }
        visited += 1;
        for provider in [
            crate::provider::Provider::Mercurial,
            crate::provider::Provider::Subversion,
            crate::provider::Provider::Perforce,
            crate::provider::Provider::Tfs,
        ] {
            if provider.detect(&dir) && seen.insert((dir.clone(), provider)) {
                result.roots.push(Root {
                    provider,
                    path: dir.clone(),
                    git_dir: PathBuf::new(),
                    common_dir: PathBuf::new(),
                    linked_worktree: false,
                    submodule: false,
                });
            }
        }
        if dir.join(".git").exists() {
            match inspect(&dir) {
                Ok(root) if seen.insert((root.path.clone(), root.provider)) => {
                    result.roots.push(root)
                }
                Ok(_) => {}
                Err(error) => result.errors.push((dir.clone(), error.to_string())),
            }
            if let Ok(repo) = git2::Repository::open(&dir) {
                if let Ok(submodules) = repo.submodules() {
                    for submodule in submodules.into_iter().take(limits.directories) {
                        let path = dir.join(submodule.path());
                        if path.is_dir() {
                            pending.push((path, depth + 1));
                        }
                    }
                }
            }
        }
        if depth >= limits.depth {
            result.truncated = true;
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            entries_seen = entries_seen.saturating_add(1);
            if entries_seen > limits.directories.saturating_mul(16)
                || start.elapsed() >= limits.duration
                || cancel.load(Ordering::Relaxed)
            {
                result.truncated = true;
                break 'scan;
            }
            if entry
                .file_type()
                .is_ok_and(|t| t.is_dir() && !t.is_symlink())
            {
                let name = entry.file_name();
                if !matches!(
                    name.to_str(),
                    Some(
                        ".git"
                            | ".hg"
                            | ".svn"
                            | ".p4root"
                            | "$tf"
                            | ".venv"
                            | "venv"
                            | "node_modules"
                            | "target"
                            | ".mypy_cache"
                            | ".tox"
                            | "__pycache__"
                    )
                ) {
                    pending.push((entry.path(), depth + 1));
                }
            }
        }
    }
    result.roots.sort_by(|a, b| a.path.cmp(&b.path));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_relative_mappings_survive_a_project_move() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        let moved = dir.path().join("moved");
        std::fs::create_dir_all(original.join("hg/.hg")).unwrap();
        let mut mappings = DirectoryMappings::default();
        mappings.register_provider(original.join("hg"), crate::provider::Provider::Mercurial);
        mappings.save(&original).unwrap();
        std::fs::rename(&original, &moved).unwrap();
        let restored = DirectoryMappings::load(&moved);
        assert_eq!(
            restored
                .provider_for(&moved.join("hg/file.py"))
                .map(|(_, provider)| provider),
            Some(crate::provider::Provider::Mercurial)
        );
    }
    #[test]
    fn provider_mappings_override_enclosing_git_without_becoming_git_roots() {
        use crate::provider::Provider;
        let mut mappings = DirectoryMappings::default();
        mappings.register(PathBuf::from("/project"));
        mappings.register_provider(PathBuf::from("/project/vendor/hg"), Provider::Mercurial);
        assert!(mappings
            .root_for(Path::new("/project/vendor/hg/file.py"))
            .is_none());
        assert_eq!(
            mappings
                .provider_for(Path::new("/project/vendor/hg/file.py"))
                .map(|(_, p)| p),
            Some(Provider::Mercurial)
        );
        let decoded: DirectoryMappings =
            serde_json::from_slice(&serde_json::to_vec(&mappings).unwrap()).unwrap();
        assert_eq!(
            decoded.providers.get(Path::new("/project/vendor/hg")),
            Some(&Provider::Mercurial)
        );
    }
    #[test]
    fn detects_non_git_nested_working_copies_without_spawning_executables() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("hg/.hg")).unwrap();
        std::fs::create_dir_all(dir.path().join("svn/.svn")).unwrap();
        std::fs::write(dir.path().join("svn/.svn/wc.db"), b"fixture marker").unwrap();
        let detection = detect(
            dir.path(),
            DetectionLimits::default(),
            &AtomicBool::new(false),
        );
        assert!(detection
            .roots
            .iter()
            .any(|r| r.provider == crate::provider::Provider::Mercurial));
        assert!(detection
            .roots
            .iter()
            .any(|r| r.provider == crate::provider::Provider::Subversion));
    }
    #[test]
    fn recognizes_linked_worktree_without_scanning_git_objects() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Fixture", "fixture@example.test").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        let linked = dir.path().join("linked");
        repo.worktree("linked", &linked, None).unwrap();
        let detection = detect(
            dir.path(),
            DetectionLimits::default(),
            &AtomicBool::new(false),
        );
        let worktree = detection.roots.iter().find(|r| r.path == linked).unwrap();
        assert!(worktree.linked_worktree);
        assert_eq!(
            dunce::canonicalize(&worktree.common_dir).unwrap(),
            dunce::canonicalize(repo.commondir()).unwrap()
        );
        assert_eq!(detection.roots.len(), 2);
    }
    #[test]
    fn routes_nested_files_and_preserves_explicit_ignored_roots() {
        let mut mappings = DirectoryMappings::default();
        mappings.register(PathBuf::from("/project"));
        mappings.register(PathBuf::from("/project/vendor/nested"));
        mappings.ignored.insert(PathBuf::from("/project/private"));
        assert_eq!(
            mappings.root_for(Path::new("/project/vendor/nested/file.py")),
            Some(&PathBuf::from("/project/vendor/nested"))
        );
        let encoded = serde_json::to_vec(&mappings).unwrap();
        let decoded: DirectoryMappings = serde_json::from_slice(&encoded).unwrap();
        assert!(decoded.ignored.contains(Path::new("/project/private")));
    }
    #[test]
    fn detects_nested_repositories_with_a_hard_budget() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let nested = dir.path().join("nested");
        git2::Repository::init(&nested).unwrap();
        let result = detect(
            dir.path(),
            DetectionLimits::default(),
            &AtomicBool::new(false),
        );
        assert_eq!(result.roots.len(), 2);
        let limited = detect(
            dir.path(),
            DetectionLimits {
                directories: 0,
                ..Default::default()
            },
            &AtomicBool::new(false),
        );
        assert!(limited.truncated);
    }
}
