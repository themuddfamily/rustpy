//! The project model behind the analysis, built the way the IDE builds it
//! (`IdeApp::open_project` / `apply_interpreter_paths`): workspace folders as
//! content roots plus their marked Sources Roots, the interpreter's search
//! paths, its version and `sys.platform`, and the project file list without
//! Excluded folders.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rustpy_semantic::module::ModuleResolver;
use rustpy_semantic::Analysis;
use rustpy_settings::Settings;

use crate::config::InterpreterChoice;

// Workspace loading and persisted-root policy are owned by rustpy-project.
pub use rustpy_project::WorkspaceSnapshot as ProjectModel;

/// What module resolution needs from an interpreter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterpreterInfo {
    /// `Python 3.12 (venv)` style label for messages.
    pub label: String,
    pub path: Option<PathBuf>,
    pub version: (u32, u32),
    pub platform: String,
    pub search_paths: Vec<PathBuf>,
}

impl InterpreterInfo {
    fn from_probe(i: &rustpy_project::Interpreter) -> Self {
        Self {
            label: i.label(),
            path: Some(i.path.clone()),
            version: i.major_minor(),
            platform: i.platform.clone(),
            search_paths: i.search_paths(),
        }
    }
}

/// A fresh analysis database for `model` (and the interpreter, when known).
pub fn build_analysis(model: &ProjectModel, interp: Option<&InterpreterInfo>) -> Analysis {
    let roots = model.roots();
    let resolver = match interp {
        Some(i) => {
            ModuleResolver::new(roots, i.search_paths.clone(), i.version).with_platform(&i.platform)
        }
        None => ModuleResolver::new(roots, Vec::new(), (3, 12)),
    };
    let settings = Settings::load();
    let analysis = Analysis::new(resolver.with_stub_roots(settings.python_stub_roots.clone()));
    analysis.set_analyze_doc_comments(
        settings
            .documentation
            .effective(model.primary(), &settings.docstring_format)
            .analyze_doc_comments,
    );
    analysis
}

/// Finds the interpreter for the project (slow: probing runs Python). The
/// project's own environments are probed only when the IDE trusts the
/// project (`trusted_projects`, Trusted Locations or `RUSTPY_TRUST_ALL`);
/// otherwise an in-project virtual environment is still used for its
/// `site-packages`, read from disk without running anything.
pub fn discover_interpreter(
    model: &ProjectModel,
    choice: &InterpreterChoice,
    settings: &Settings,
) -> Option<InterpreterInfo> {
    match choice {
        InterpreterChoice::Disabled => None,
        InterpreterChoice::Path(p) => match rustpy_project::interpreter::probe(p, "configured") {
            Some(i) => Some(InterpreterInfo::from_probe(&i)),
            None => {
                log::warn!("lsp: cannot run the configured interpreter {}", p.display());
                None
            }
        },
        InterpreterChoice::Discover => {
            let root = model.primary();
            let trusted = root.is_some_and(|r| is_trusted(settings, r));
            if !trusted {
                if let Some(venv) = root.and_then(static_venv) {
                    log::info!(
                        "lsp: project not trusted; using {} without running it",
                        venv.label
                    );
                    return Some(venv);
                }
            }
            let found = rustpy_project::discover_interpreters_with(root, trusted);
            let preferred = model
                .ide_interpreter
                .as_ref()
                .and_then(|p| found.iter().find(|i| &i.path == p));
            let chosen = preferred
                .or_else(|| found.iter().find(|i| i.kind.contains("venv")))
                .or_else(|| found.first());
            chosen.map(InterpreterInfo::from_probe)
        }
    }
}

/// Trust as the IDE decides it (never from anything inside the project).
pub fn is_trusted(settings: &Settings, root: &Path) -> bool {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    rustpy_settings::trust_all_from_env()
        || settings.is_project_trusted(&canonical)
        || settings.is_project_trusted(root)
}

/// An in-project virtual environment read from disk: `pyvenv.cfg` for the
/// version and its `site-packages` directory.
pub fn static_venv(root: &Path) -> Option<InterpreterInfo> {
    for name in [".venv", "venv", "env", ".env"] {
        let env = root.join(name);
        let Ok(cfg) = std::fs::read_to_string(env.join("pyvenv.cfg")) else {
            continue;
        };
        let version = cfg.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            matches!(k.trim(), "version" | "version_info").then(|| {
                let mut it = v.trim().split('.').filter_map(|p| p.parse::<u32>().ok());
                (it.next().unwrap_or(3), it.next().unwrap_or(12))
            })
        });
        let mut site = Vec::new();
        for lib in ["lib", "lib64", "Lib"] {
            let Ok(rd) = std::fs::read_dir(env.join(lib)) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.file_name().and_then(|n| n.to_str()) == Some("site-packages") {
                    site.push(p);
                } else if p.join("site-packages").is_dir() {
                    site.push(p.join("site-packages"));
                }
            }
        }
        site.sort();
        site.dedup();
        let version = version.or_else(|| {
            // `lib/python3.12/site-packages`
            site.iter().find_map(|s| {
                s.parent()?
                    .file_name()?
                    .to_str()?
                    .strip_prefix("python")?
                    .split_once('.')
                    .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            })
        });
        let version = version.unwrap_or((3, 12));
        return Some(InterpreterInfo {
            label: format!("Python {}.{} ({name}, not run)", version.0, version.1),
            path: None,
            version,
            platform: String::new(),
            search_paths: site,
        });
    }
    None
}

/// The project's Python files (`.py`, `.pyi`) without Excluded folders,
/// `.gitignore`d files and caches — the list Find Usages and Rename search.
/// Walking a large tree is slow, so the list is cached until files are
/// created or deleted (or for a short while when the client does not report that).
pub struct ProjectFiles {
    scope: rustpy_project::ProjectScope,
    cache: Mutex<Option<(Instant, Arc<Vec<PathBuf>>)>>,
}

const FILES_TTL: Duration = Duration::from_secs(10);

impl ProjectFiles {
    pub fn contains(&self, path: &Path) -> bool {
        self.scope.contains(path)
    }
    pub fn new(model: &ProjectModel) -> Self {
        Self {
            scope: model.scope(),
            cache: Mutex::new(None),
        }
    }

    pub fn get(&self) -> Arc<Vec<PathBuf>> {
        let mut cache = self.cache.lock();
        if let Some((at, files)) = &*cache {
            if at.elapsed() < FILES_TTL {
                return files.clone();
            }
        }
        let mut out: Vec<PathBuf> = Vec::new();
        for p in rustpy_project::files::project_files_in(&self.scope) {
            if matches!(p.extension().and_then(|e| e.to_str()), Some("py" | "pyi")) {
                out.push(p);
            }
        }
        out.sort();
        out.dedup();
        let files = Arc::new(out);
        *cache = Some((Instant::now(), files.clone()));
        files
    }

    /// Forgets the cached list (files were created, deleted or renamed).
    pub fn invalidate(&self) {
        *self.cache.lock() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_venv_reads_version_and_site_packages() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join(".venv");
        std::fs::create_dir_all(env.join("lib/python3.11/site-packages")).unwrap();
        std::fs::write(
            env.join("pyvenv.cfg"),
            "home = /usr/bin\nversion = 3.11.4\n",
        )
        .unwrap();
        let info = static_venv(dir.path()).unwrap();
        assert_eq!(info.version, (3, 11));
        assert_eq!(
            info.search_paths,
            vec![env.join("lib/python3.11/site-packages")]
        );
        assert!(info.path.is_none());
    }

    #[test]
    fn project_files_skip_excluded_and_non_python() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("main.py"), "").unwrap();
        std::fs::write(root.join("pkg/mod.pyi"), "").unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();
        std::fs::write(root.join("build/gen.py"), "").unwrap();
        let mut model = ProjectModel::try_load(vec![root.clone()]).unwrap();
        model.excluded.push(root.join("build"));
        let files = ProjectFiles::new(&model).get();
        assert_eq!(*files, vec![root.join("main.py"), root.join("pkg/mod.pyi")]);
    }
    #[test]
    fn saved_shared_structure_drives_lsp_imports_and_cached_enumeration() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        let shared = dir.path().join("shared");
        for path in [
            root.join("src/main.py"),
            root.join("tests/check.pyi"),
            root.join("vendor/hidden.py"),
            root.join("ignored.py"),
            root.join(".git/config"),
            shared.join("helper.py"),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        std::fs::write(root.join(".gitignore"), "ignored.py\n").unwrap();
        rustpy_project::ProjectModel {
            content_roots: vec![shared.clone()],
            source_roots: vec![root.join("src")],
            test_roots: vec![root.join("tests")],
            excluded: vec![root.join("vendor")],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let model = ProjectModel::try_load(vec![root.clone()]).unwrap();
        let analysis = build_analysis(&model, None);
        assert_eq!(
            analysis.resolver.roots,
            vec![
                root.clone(),
                shared.clone(),
                root.join("src"),
                root.join("tests")
            ]
        );
        let files = ProjectFiles::new(&model);
        let mut expected = vec![
            root.join("src/main.py"),
            root.join("tests/check.pyi"),
            shared.join("helper.py"),
        ];
        expected.sort();
        assert_eq!(*files.get(), expected);
        assert!(files.contains(&shared.join("helper.py")));
        assert!(!files.contains(&root.join("vendor/hidden.py")));
        let cached = files.get();
        assert!(Arc::ptr_eq(&files.get(), &cached));
        std::fs::write(shared.join("new.py"), "").unwrap();
        assert_eq!(
            *files.get(),
            expected,
            "the existing ten-second cache is retained"
        );
        files.invalidate();
        assert!(files.get().contains(&shared.join("new.py")));
    }
}
