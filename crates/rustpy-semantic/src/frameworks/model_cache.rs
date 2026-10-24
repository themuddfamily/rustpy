//! Project model discovery, computed once per source generation and project file set.
//! Library, virtual-environment and stub sources are never scanned for models: their
//! facts come from the declared bases of project classes only.
use super::*;
use rustc_hash::{FxHashMap, FxHashSet};
use rustpy_core::FileId;
use smol_str::SmolStr;
use std::path::PathBuf;

/// Project files above this count are not scanned as a whole; callers fall back to
/// the current file and its imports and stay conservative.
const MAX_PROJECT_FILES: usize = 4096;
const FRAMEWORK_PACKAGES: [&str; 5] = [
    "django",
    "sqlalchemy",
    "flask_sqlalchemy",
    "pydantic",
    "sqlmodel",
];

#[derive(Default)]
pub(crate) struct ModelCache {
    /// Resolver roots and search paths the library classification was made against.
    environment: Vec<PathBuf>,
    library: FxHashMap<FileId, bool>,
    generation: u64,
    project: Option<Arc<ProjectModels>>,
    known: FxHashMap<FileId, (usize, Arc<Vec<Model>>)>,
}

pub(crate) struct ProjectModels {
    /// `(id, identity)` of every loaded project source, sorted by id.
    ids: Vec<(FileId, usize)>,
    /// `None` when the project has too many loaded files to scan.
    pub(crate) files: Option<Vec<Arc<FileInfo>>>,
    pub(crate) models: Vec<Model>,
    /// Some project source adds model members at run time.
    pub(crate) dynamic: bool,
    /// Some project source registers custom lookups.
    pub(crate) custom_lookups: bool,
}

thread_local! {
    /// Set while project models are being computed: nested framework queries made by
    /// inference during the scan see only direct facts instead of recursing.
    static SCANNING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn identity(file: &Arc<FileInfo>) -> usize {
    Arc::as_ptr(file) as usize
}

fn environment(db: &Analysis) -> Vec<PathBuf> {
    db.resolver
        .roots
        .iter()
        .chain(db.resolver.search_paths.iter())
        .cloned()
        .collect()
}

fn classify(cache: &mut ModelCache, db: &Analysis, file: &FileInfo) -> bool {
    *cache
        .library
        .entry(file.id)
        .or_insert_with(|| file.is_stub || crate::usages::is_library_file(db, file))
}

fn sync_environment(cache: &mut ModelCache, db: &Analysis) {
    let environment = environment(db);
    if cache.environment != environment || cache.library.len() > 200_000 {
        cache.environment = environment;
        cache.library.clear();
        cache.known.clear();
        cache.project = None;
    }
}

/// Whether `file` is a library, virtual-environment or stub source (never a model source).
pub(crate) fn is_library(db: &Analysis, file: &FileInfo) -> bool {
    let mut cache = db.framework_models.lock();
    sync_environment(&mut cache, db);
    classify(&mut cache, db, file)
}

/// Models of every loaded project source, reused while the source generation and the
/// set of loaded project files are unchanged.
pub(crate) fn project(db: &Analysis) -> Arc<ProjectModels> {
    let generation = db.source_generation();
    let all = db.all_files();
    let mut sources: Vec<Arc<FileInfo>> = {
        let mut cache = db.framework_models.lock();
        sync_environment(&mut cache, db);
        all.into_iter()
            .filter(|file| !classify(&mut cache, db, file))
            .collect()
    };
    sources.sort_by_key(|file| file.id.0);
    let ids: Vec<(FileId, usize)> = sources.iter().map(|f| (f.id, identity(f))).collect();
    {
        let cache = db.framework_models.lock();
        if let Some(project) = &cache.project {
            if cache.generation == generation && project.ids == ids {
                return project.clone();
            }
        }
    }
    let computed = Arc::new(scan(db, ids, sources));
    // Inference cut short by a cancelled job: incomplete, so not cached.
    if crate::infer::ambient_cancelled() {
        return computed;
    }
    let mut cache = db.framework_models.lock();
    cache.known.clear();
    cache.generation = generation;
    cache.project = Some(computed.clone());
    computed
}

fn scan(db: &Analysis, ids: Vec<(FileId, usize)>, sources: Vec<Arc<FileInfo>>) -> ProjectModels {
    let dynamic = sources.iter().any(|source| {
        source.text.contains("add_to_class") || source.text.contains("contribute_to_class")
    });
    let custom_lookups = sources
        .iter()
        .any(|source| source.text.contains("register_lookup"));
    if sources.len() > MAX_PROJECT_FILES {
        return ProjectModels {
            ids,
            files: None,
            models: Vec::new(),
            dynamic,
            custom_lookups,
        };
    }
    let models = guarded(|| project_models(db, &sources)).unwrap_or_default();
    ProjectModels {
        ids,
        files: Some(sources),
        models,
        dynamic,
        custom_lookups,
    }
}

fn guarded<T>(work: impl FnOnce() -> T) -> Option<T> {
    if SCANNING.with(|active| active.replace(true)) {
        return None;
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SCANNING.with(|active| active.set(false));
        }
    }
    let _reset = Reset;
    Some(work())
}

/// Direct models from every source, then inherited families only for sources whose
/// classes name a known model class or a framework import as a base.
fn project_models(db: &Analysis, sources: &[Arc<FileInfo>]) -> Vec<Model> {
    let mut per_file: Vec<Vec<Model>> = sources.iter().map(models).collect();
    let mut names: FxHashSet<SmolStr> = per_file
        .iter()
        .flatten()
        .map(|model| model.class.name.clone())
        .collect();
    let mut scanned = vec![false; sources.len()];
    loop {
        let mut changed = false;
        for (index, source) in sources.iter().enumerate() {
            if scanned[index] || !inherits_candidate(source, &names) {
                continue;
            }
            scanned[index] = true;
            let found = models_in_analysis(db, source);
            for model in &found {
                changed |= names.insert(model.class.name.clone());
            }
            per_file[index] = found;
        }
        if !changed {
            break;
        }
    }
    per_file.into_iter().flatten().take(4096).collect()
}

/// A class whose family is not direct but whose base may lead to a model: a known model
/// class name (directly or through an import alias) or a name imported from a framework.
fn inherits_candidate(file: &Arc<FileInfo>, names: &FxHashSet<SmolStr>) -> bool {
    if file.text.len() > 2 * 1024 * 1024 || !file.text.contains("class ") {
        return false;
    }
    let scope = file.scopes.module_scope();
    file.parse
        .syntax_node()
        .descendants()
        .take(20000)
        .filter_map(ast::ClassDef::cast)
        .take(256)
        .any(|class| {
            family(file, &class).is_none()
                && class.bases().iter().any(|base| {
                    let text = base.syntax().text().to_string();
                    let text = text.split('[').next().unwrap_or_default().trim();
                    let last = text.rsplit('.').next().unwrap_or(text);
                    if names.contains(last) {
                        return true;
                    }
                    let head = text.split('.').next().unwrap_or(text);
                    file.scopes
                        .resolve(scope, head)
                        .into_iter()
                        .flatten()
                        .any(|id| match &file.scopes.binding(*id).kind {
                            BindingKind::Import(info) => {
                                info.name.as_ref().is_some_and(|name| names.contains(name))
                                    || FRAMEWORK_PACKAGES.iter().any(|package| {
                                        info.module == *package
                                            || info.module.starts_with(&format!("{package}."))
                                    })
                            }
                            _ => false,
                        })
                })
        })
}

static EMPTY: std::sync::OnceLock<Arc<Vec<Model>>> = std::sync::OnceLock::new();

/// Models visible from `file`: every project model, plus models of `file` and of its
/// project imports when they are not part of the scanned project. Library sources
/// have none.
pub(crate) fn known(db: &Analysis, file: &Arc<FileInfo>) -> Arc<Vec<Model>> {
    let empty = || EMPTY.get_or_init(Default::default).clone();
    if is_library(db, file) {
        return empty();
    }
    if SCANNING.with(|active| active.get()) {
        return Arc::new(models(file));
    }
    let project = project(db);
    {
        let cache = db.framework_models.lock();
        if let Some((owner, models)) = cache.known.get(&file.id) {
            if *owner == identity(file)
                && cache
                    .project
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &project))
            {
                return models.clone();
            }
        }
    }
    let scanned =
        |id: FileId| project.files.is_some() && project.ids.iter().any(|(other, _)| *other == id);
    let mut out = project.models.clone();
    let mut extra = Vec::new();
    if !project
        .ids
        .iter()
        .any(|(id, owner)| *id == file.id && *owner == identity(file))
        || project.files.is_none()
    {
        extra.push(file.clone());
    }
    for binding in file.scopes.bindings.iter().take(2048) {
        if extra.len() >= 256 {
            break;
        }
        if let BindingKind::Import(info) = &binding.kind {
            if let Some(imported) = db.module_from(file, &info.module) {
                if !scanned(imported.id)
                    && !extra.iter().any(|other| other.id == imported.id)
                    && !is_library(db, &imported)
                {
                    extra.push(imported);
                }
            }
        }
    }
    for source in &extra {
        out.retain(|model| model.class.file != source.id);
        out.extend(guarded(|| models_in_analysis(db, source)).unwrap_or_else(|| models(source)));
    }
    out.truncate(4096);
    let out = Arc::new(out);
    if crate::infer::ambient_cancelled() {
        return out;
    }
    let mut cache = db.framework_models.lock();
    if cache
        .project
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, &project))
    {
        if cache.known.len() >= 1024 {
            cache.known.clear();
        }
        cache.known.insert(file.id, (identity(file), out.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::ModuleResolver;

    #[test]
    fn project_models_are_cached_per_generation_and_skip_library_sources() {
        let directory = tempfile::TempDir::with_prefix("framework-model-cache").unwrap();
        let root = directory.path();
        let site = root.join(".venv/lib/python3.12/site-packages");
        std::fs::create_dir_all(site.join("lib_models")).unwrap();
        std::fs::write(root.join(".venv/pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        let library = site.join("lib_models/__init__.py");
        std::fs::write(
            &library,
            "from django.db import models\nclass Hidden(models.Model):\n    name = models.CharField()\nmodels.Model.add_to_class('x', 1)\n",
        )
        .unwrap();
        let db = Analysis::new(ModuleResolver::new(
            vec![root.to_path_buf()],
            vec![site.clone()],
            (3, 12),
        ));
        let hidden = db.file_for_path(&library).unwrap();
        let id = db.set_file_text(
            &root.join("app.py"),
            "from django.db import models\nclass Book(models.Model):\n    title = models.CharField()\nclass Novel(Book):\n    pass\n",
        );
        let file = db.file(id).unwrap();
        assert!(known(&db, &hidden).is_empty());
        let first = known(&db, &file);
        let names: Vec<_> = first.iter().map(|m| m.class.name.as_str()).collect();
        assert_eq!(names, ["Book", "Novel"]);
        assert!(Arc::ptr_eq(&first, &known(&db, &file)));
        let project = project(&db);
        assert!(!project.dynamic, "library add_to_class must not count");
        assert_eq!(project.files.as_ref().map(Vec::len), Some(1));
        db.set_file_text(
            &root.join("app.py"),
            "from django.db import models\nclass Book(models.Model):\n    title = models.CharField()\n",
        );
        let file = db.file(id).unwrap();
        let names: Vec<_> = known(&db, &file)
            .iter()
            .map(|m| m.class.name.to_string())
            .collect();
        assert_eq!(names, ["Book"]);
    }
    #[cfg(unix)]
    #[test]
    fn symlinked_project_virtualenv_stays_library_code() {
        let directory = tempfile::TempDir::with_prefix("framework-model-symlink").unwrap();
        let base = directory.path();
        let root = base.join("proj");
        let venv = base.join("venv");
        let real_site = venv.join("lib/python3.12/site-packages");
        std::fs::create_dir_all(real_site.join("lib_models")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        std::os::unix::fs::symlink("../venv", root.join(".venv")).unwrap();
        std::fs::write(
            real_site.join("lib_models/__init__.py"),
            "from django.db import models\nclass Hidden(models.Model):\n    name = models.CharField()\n",
        )
        .unwrap();
        // The interpreter reports its search path through the link, as
        // `proj/.venv/bin/python -c 'import sys; print(sys.path)'` does.
        let linked_site = root.join(".venv/lib/python3.12/site-packages");
        for search_paths in [vec![linked_site.clone()], Vec::new()] {
            let db = Analysis::new(ModuleResolver::new(
                vec![root.clone()],
                search_paths,
                (3, 12),
            ));
            let hidden = db
                .file_for_path(&linked_site.join("lib_models/__init__.py"))
                .unwrap();
            assert!(
                crate::usages::is_library_file(&db, &hidden),
                "{}",
                hidden.path.display()
            );
            assert_eq!(db.resolver.project_root_of(&hidden.path), None);
            assert!(known(&db, &hidden).is_empty());
            let id = db.set_file_text(
                &root.join("app.py"),
                "from django.db import models\nclass Book(models.Model):\n    title = models.CharField()\n",
            );
            let app = db.file(id).unwrap();
            assert!(!crate::usages::is_library_file(&db, &app));
            let names: Vec<_> = known(&db, &app)
                .iter()
                .map(|m| m.class.name.to_string())
                .collect();
            assert_eq!(names, ["Book"]);
        }
    }
}
