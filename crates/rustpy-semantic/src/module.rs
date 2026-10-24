//! Module name resolution: project source roots, extra search paths
//! (site-packages), and the bundled typeshed stubs (roadmap §2.2 "Module index").
//!
//! Besides `.py`/`.pyi` files and packages this finds PEP 561 stub-only
//! packages (`requests-stubs/` shadows `requests/` on the search paths) and
//! compiled extension modules (`ujson.cpython-312-x86_64-linux-gnu.so`,
//! `etree.pyd`), which resolve to a permissive placeholder stub.

use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use rustc_hash::FxHashMap;

use crate::typeshed;

/// Where a module's source comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ModuleSource {
    /// A `.py` / `.pyi` file on disk. `is_package` marks `__init__` files.
    File { path: PathBuf, is_package: bool },
    /// A directory without `__init__.py` (PEP 420 namespace package).
    Namespace(PathBuf),
    /// A bundled typeshed stdlib stub, identified by dotted module name.
    Stub(String),
    /// A compiled extension module (`.so` / `.pyd`) without a stub: its
    /// members are unknown, so it is analysed through [`BINARY_MODULE_STUB`].
    Binary(PathBuf),
}

/// Source analysed for a compiled module that has no stub: every attribute
/// resolves (PEP 562 module `__getattr__`) to `Any`, so imports and member
/// accesses produce no false "unresolved" errors.
pub const BINARY_MODULE_STUB: &str = "# Compiled extension module: no stub or source is available.\nfrom typing import Any\n\ndef __getattr__(name: str) -> Any: ...\n";

impl ModuleSource {
    pub fn path(&self) -> Option<&Path> {
        match self {
            ModuleSource::File { path, .. } => Some(path),
            ModuleSource::Namespace(p) => Some(p),
            ModuleSource::Binary(p) => Some(p),
            ModuleSource::Stub(_) => None,
        }
    }
    pub fn is_package(&self) -> bool {
        match self {
            ModuleSource::File { is_package, .. } => *is_package,
            ModuleSource::Namespace(_) => true,
            ModuleSource::Stub(name) => typeshed::is_package(name),
            ModuleSource::Binary(_) => false,
        }
    }
    pub fn text(&self) -> Option<String> {
        match self {
            ModuleSource::File { path, .. } => std::fs::read_to_string(path).ok(),
            ModuleSource::Namespace(_) => Some(String::new()),
            ModuleSource::Stub(name) => typeshed::stub_for_module(name).map(str::to_owned),
            ModuleSource::Binary(_) => Some(BINARY_MODULE_STUB.to_string()),
        }
    }
}

#[derive(Clone, Debug)]
struct Resolution {
    source: Option<ModuleSource>,
    import_available: bool,
}

/// Resolves dotted module names to sources.
#[derive(Debug)]
pub struct ModuleResolver {
    scoped: Vec<(PathBuf, Box<ModuleResolver>)>,
    /// Project source roots (content roots + marked source roots), highest priority first.
    pub roots: Vec<PathBuf>,
    /// Interpreter search paths (site-packages, …).
    pub search_paths: Vec<PathBuf>,
    /// User Python Stubs library roots, before project/interpreter fallbacks.
    pub stub_roots: Vec<PathBuf>,
    excluded: Vec<PathBuf>,
    excluded_library_roots: Vec<PathBuf>,
    pub python_version: typeshed::PyVersion,
    /// `sys.platform` of the target interpreter (the host's by default).
    pub platform: String,
    cache: RwLock<FxHashMap<String, Resolution>>,
    frozen: bool,
    frozen_sources: FxHashMap<String, PathBuf>,
    missing_dependency: std::sync::atomic::AtomicBool,
    /// Compiled extension modules per directory: module name → file.
    binaries: RwLock<FxHashMap<PathBuf, std::sync::Arc<FxHashMap<String, PathBuf>>>>,
    /// Memoized `normalize_path` answers for exclusion checks (hot in module lookups);
    /// dropped with the resolution cache on every invalidation.
    normalized: RwLock<FxHashMap<PathBuf, PathBuf>>,
}

impl ModuleResolver {
    pub fn new(
        roots: Vec<PathBuf>,
        search_paths: Vec<PathBuf>,
        python_version: typeshed::PyVersion,
    ) -> Self {
        Self {
            scoped: Vec::new(),
            roots,
            search_paths,
            stub_roots: Vec::new(),
            excluded: Vec::new(),
            excluded_library_roots: Vec::new(),
            python_version,
            platform: crate::static_cond::host_platform().to_string(),
            cache: RwLock::new(FxHashMap::default()),
            frozen: false,
            frozen_sources: FxHashMap::default(),
            missing_dependency: std::sync::atomic::AtomicBool::new(false),
            binaries: RwLock::new(FxHashMap::default()),
            normalized: RwLock::new(FxHashMap::default()),
        }
    }

    /// Module environments share the source catalogue but retain independent
    /// interpreter paths, language levels and import caches.
    pub fn with_module(mut self, root: PathBuf, resolver: ModuleResolver) -> Self {
        self.scoped.push((root, Box::new(resolver)));
        self.scoped
            .sort_by_key(|(root, _)| std::cmp::Reverse(root.components().count()));
        self
    }

    pub fn module_root_for(&self, path: &Path) -> Option<&Path> {
        self.scoped
            .iter()
            .find(|(root, _)| path.starts_with(root))
            .map(|(root, resolver)| resolver.module_root_for(path).unwrap_or(root.as_path()))
    }

    pub fn for_path(&self, path: &Path) -> &ModuleResolver {
        self.scoped
            .iter()
            .find(|(root, _)| path.starts_with(root))
            .map(|(_, resolver)| resolver.for_path(path))
            .unwrap_or(self)
    }

    /// Captures resolution answers for the owned source catalogue. Frozen
    /// resolvers never inspect directories, including relative imports.
    pub(crate) fn frozen_catalogue(
        &self,
        catalogue: impl Iterator<Item = (String, PathBuf)>,
        token: &rustpy_core::CancellationToken,
    ) -> Result<Self, rustpy_core::Cancelled> {
        let mut frozen = Self::new(
            self.roots.clone(),
            self.search_paths.clone(),
            self.python_version,
        )
        .with_stub_roots(self.stub_roots.clone())
        .with_platform(&self.platform)
        .with_excluded(self.excluded.clone());
        frozen.excluded_library_roots = self.excluded_library_roots.clone();
        let mut answers = self.cache.read().clone();
        let catalogue: Vec<_> = catalogue.collect();
        for (root, resolver) in &self.scoped {
            frozen.scoped.push((
                root.clone(),
                Box::new(resolver.frozen_catalogue(catalogue.iter().cloned(), token)?),
            ));
        }
        let paths: std::collections::HashSet<_> =
            catalogue.iter().map(|(_, path)| path.clone()).collect();
        for (name, _) in catalogue {
            token.check()?;
            self.resolve(&name);
            if let Some(answer) = self.cache.read().get(&name).cloned() {
                answers.insert(name.clone(), answer);
            }
            if let Some(path) = self.resolve_source(&name) {
                let path = rustpy_core::vfs::normalize_path(&path);
                if paths.contains(&path) {
                    frozen.frozen_sources.insert(name, path);
                }
            }
        }
        for source in answers
            .values_mut()
            .filter_map(|answer| answer.source.as_mut())
        {
            match source {
                ModuleSource::File { path, .. }
                | ModuleSource::Namespace(path)
                | ModuleSource::Binary(path) => {
                    *path = rustpy_core::vfs::normalize_path(path);
                }
                ModuleSource::Stub(_) => {}
            }
        }
        *frozen.cache.write() = answers;
        // Capture the canonical spelling of every root for `root_containing`;
        // a frozen resolver never consults the filesystem afterwards.
        for root in frozen
            .roots
            .iter()
            .chain(&frozen.search_paths)
            .chain(&frozen.stub_roots)
        {
            frozen.normalized(root);
        }
        frozen.frozen = true;
        Ok(frozen)
    }

    pub(crate) fn snapshot_complete(&self) -> bool {
        self.scoped
            .iter()
            .all(|(_, resolver)| resolver.snapshot_complete())
            && !self
                .missing_dependency
                .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Attach user-provided Python Stubs libraries. Roots contain normal module
    /// paths (`pkg/__init__.pyi`), not distribution metadata directory names.
    pub fn with_stub_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        assert!(
            !self.frozen,
            "cannot change stub libraries of a frozen resolver"
        );
        self.stub_roots = roots
            .into_iter()
            .map(|p| rustpy_core::vfs::normalize_path(&p))
            .collect();
        self.stub_roots.dedup();
        self.invalidate();
        self
    }

    /// Targets the interpreter's `sys.platform` (an empty value keeps the host's).
    pub fn with_platform(mut self, platform: &str) -> Self {
        if !platform.is_empty() {
            self.platform = platform.to_string();
        }
        self
    }

    /// Excluded project folders affect imports/indexing, not explicit editor opens.
    pub fn with_excluded(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        assert!(
            !self.frozen,
            "cannot change exclusions of a frozen resolver"
        );
        self.excluded = paths
            .into_iter()
            .map(|p| rustpy_core::vfs::normalize_path(&p))
            .collect();
        self.excluded.sort();
        self.excluded.dedup();
        // Interpreter roots explicitly attached inside excluded project containers
        // retain their independent library role. Capture this before freezing.
        self.excluded_library_roots = self
            .search_paths
            .iter()
            .map(|p| rustpy_core::vfs::normalize_path(p))
            .collect();
        self.invalidate();
        self
    }

    /// Whether `path` lies under a project source root, an interpreter search
    /// path or a stub library root of the environment owning it. Modules found
    /// anywhere else were only reachable relative to the importing file.
    pub fn is_on_import_path(&self, path: &Path) -> bool {
        [self, self.for_path(path)].into_iter().any(|resolver| {
            resolver
                .roots
                .iter()
                .chain(&resolver.search_paths)
                .chain(&resolver.stub_roots)
                .any(|root| resolver.root_containing(path, root).is_some())
        })
    }

    /// The project root that owns `path`, unless an interpreter or stub root
    /// nested more deeply (a project-local virtualenv) owns it instead.
    pub fn project_root_of(&self, path: &Path) -> Option<&Path> {
        let resolver = self.for_path(path);
        let (project, depth) = resolver
            .roots
            .iter()
            .filter_map(|root| Some((root, resolver.root_containing(path, root)?)))
            .max_by_key(|(_, matched)| matched.components().count())
            .map(|(root, matched)| (root, matched.components().count()))?;
        let library = resolver
            .search_paths
            .iter()
            .chain(&resolver.stub_roots)
            .filter_map(|root| resolver.root_containing(path, root))
            .map(|matched| matched.components().count())
            .max();
        library
            .is_none_or(|library| depth >= library)
            .then_some(project.as_path())
    }

    /// The spelling of `root` (as configured, or canonical) that contains
    /// `path`. Analysis files are keyed by canonical path, so a project-local
    /// virtualenv reached through a symlink (`.venv -> ../venv`) only matches
    /// the interpreter search paths reported through the link once those are
    /// canonicalised as well.
    pub fn root_containing(&self, path: &Path, root: &Path) -> Option<PathBuf> {
        if root.as_os_str().is_empty() {
            return None;
        }
        if path.starts_with(root) {
            return Some(root.to_path_buf());
        }
        let canonical = if self.frozen {
            self.normalized.read().get(root).cloned()?
        } else {
            self.normalized(root)
        };
        path.starts_with(&canonical).then_some(canonical)
    }

    pub fn excluded_paths(&self) -> &[PathBuf] {
        &self.excluded
    }

    /// `normalize_path`, memoized until the next invalidation.
    pub(crate) fn normalized(&self, path: &Path) -> PathBuf {
        if let Some(known) = self.normalized.read().get(path) {
            return known.clone();
        }
        let normalized = rustpy_core::vfs::normalize_path(path);
        let mut memo = self.normalized.write();
        if memo.len() >= 65_536 {
            memo.clear();
        }
        memo.insert(path.to_path_buf(), normalized.clone());
        normalized
    }

    pub fn is_excluded(&self, path: &Path) -> bool {
        // Frozen inputs and exclusions were normalized during capture. Do not
        // let later filesystem/symlink changes alter that captured authority.
        if self.excluded.is_empty() {
            return false;
        }
        let path = if self.frozen {
            path.to_path_buf()
        } else {
            self.normalized(path)
        };
        self.excluded.iter().any(|excluded| {
            path.starts_with(excluded)
                && !self
                    .excluded_library_roots
                    .iter()
                    .any(|library| library.starts_with(excluded) && path.starts_with(library))
        })
    }

    /// The environment version/platform branches of stubs are evaluated in.
    pub fn static_env(&self) -> crate::static_cond::StaticEnv {
        crate::static_cond::StaticEnv::new(self.python_version, &self.platform)
    }

    pub fn invalidate(&self) {
        if self.frozen {
            return;
        }
        self.cache.write().clear();
        self.binaries.write().clear();
        self.normalized.write().clear();
        for (_, resolver) in &self.scoped {
            resolver.invalidate();
        }
    }

    /// Captured provider answer, without treating a known absence as a missing
    /// snapshot dependency before an open project buffer can supply it.
    pub(crate) fn cached_source(&self, name: &str) -> Option<Option<ModuleSource>> {
        self.cache
            .read()
            .get(name)
            .map(|answer| answer.source.clone())
    }

    /// Resolves an absolute dotted name such as `os.path` or `pkg.mod`.
    pub fn resolve(&self, name: &str) -> Option<ModuleSource> {
        if name.is_empty() {
            return None;
        }
        if let Some(hit) = self.cache.read().get(name) {
            if self.frozen && hit.source.is_none() {
                self.missing_dependency
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            return hit.source.clone();
        }
        if self.frozen {
            let source = (typeshed::stub_for_module(name).is_some()
                && typeshed::module_available(name, self.python_version))
            .then(|| ModuleSource::Stub(name.to_owned()));
            if source.is_none() {
                self.missing_dependency
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            return source;
        }
        let selected = self.resolve_uncached(name);
        let available = selected.as_ref().is_some_and(|source| match source {
            ModuleSource::Stub(module) => typeshed::is_stdlib(module),
            _ => true,
        });
        let source = self.prefer_vendored_types(name, selected);
        self.cache.write().insert(
            name.to_owned(),
            Resolution {
                source: source.clone(),
                import_available: available,
            },
        );
        source
    }

    /// Runtime availability captured alongside the selected type source. Bundled
    /// third-party types never imply that their runtime dependency is installed.
    pub fn import_available(&self, name: &str) -> bool {
        self.resolve(name);
        if let Some(answer) = self.cache.read().get(name) {
            return answer.import_available;
        }
        if self.frozen && !typeshed::is_stdlib(name) {
            self.missing_dependency
                .store(true, std::sync::atomic::Ordering::Release);
            return false;
        }
        typeshed::is_stdlib(name) && typeshed::module_available(name, self.python_version)
    }

    fn prefer_vendored_types(
        &self,
        name: &str,
        selected: Option<ModuleSource>,
    ) -> Option<ModuleSource> {
        if typeshed::is_stdlib(name)
            || typeshed::stub_for_module(name).is_none()
            || !typeshed::module_available(name, self.python_version)
        {
            return selected;
        }
        let Some(source) = selected.as_ref() else {
            return selected;
        };
        let Some(path) = source.path() else {
            return selected;
        };
        if self
            .stub_roots
            .iter()
            .any(|root| self.root_containing(path, root).is_some())
            || path.extension().is_some_and(|extension| extension == "pyi")
        {
            return selected;
        }
        // Use the most specific source root: an interpreter environment may be
        // nested inside a project, and must not inherit project-source priority.
        let project = self
            .roots
            .iter()
            .filter_map(|root| self.root_containing(path, root))
            .map(|root| root.components().count())
            .max();
        let library = self
            .search_paths
            .iter()
            .filter_map(|root| self.root_containing(path, root))
            .max_by_key(|root| root.components().count());
        let Some(root) = library else {
            return selected;
        };
        let root = root.as_path();
        if project.is_some_and(|depth| depth >= root.components().count()) {
            return selected;
        }
        let mut parent = if matches!(source, ModuleSource::Namespace(_)) {
            Some(path)
        } else {
            path.parent()
        };
        while let Some(directory) = parent {
            if directory == root || !directory.starts_with(root) {
                break;
            }
            if directory.join("py.typed").is_file() {
                return selected;
            }
            parent = directory.parent();
        }
        Some(ModuleSource::Stub(name.to_owned()))
    }

    fn resolve_uncached(&self, name: &str) -> Option<ModuleSource> {
        let parts: Vec<&str> = name.split('.').collect();
        if let Some((source, _)) = self.locate(&self.stub_roots, &parts) {
            return Some(source);
        }
        // Project files shadow the stdlib (like sys.path[0]), except for `builtins`/`typing` core modules.
        let protected = matches!(
            parts[0],
            "builtins" | "typing" | "typing_extensions" | "_typeshed" | "collections" | "abc"
        );
        if !protected {
            // A regular project parent owns its children even if a requested
            // child is absent. Namespace portions defer to later regular packages.
            if self
                .locate(&self.roots, &parts[..1])
                .is_some_and(|(source, _)| !matches!(source, ModuleSource::Namespace(_)))
            {
                return self.locate(&self.roots, &parts).map(|(source, _)| source);
            }
        }
        // The typed stdlib stubs win over the interpreter's untyped sources
        // (its sys.path includes the stdlib directory itself).
        if typeshed::is_stdlib(name) && typeshed::module_available(name, self.python_version) {
            return Some(ModuleSource::Stub(name.to_string()));
        }
        if !protected {
            // A complete stub package owns its whole subtree. Only explicitly
            // partial distributions merge missing modules with runtime sources;
            // namespace stub portions remain open for other distributions.
            let stubs_top = format!("{}-stubs", parts[0]);
            let mut stub_parts = parts.clone();
            stub_parts[0] = &stubs_top;
            for root in &self.search_paths {
                let package = root.join(&stubs_top);
                if self.is_excluded(&package) {
                    continue;
                }
                let selected = self.resolve_in_dir(root, &stub_parts);
                if matches!(selected, Some(ModuleSource::File { .. })) {
                    return selected;
                }
                let mut ancestor = package;
                let mut partial = false;
                for component in 0..stub_parts.len() {
                    if component > 0 {
                        ancestor.push(stub_parts[component]);
                    }
                    partial |= std::fs::read_to_string(ancestor.join("py.typed"))
                        .ok()
                        .is_some_and(|marker| marker.lines().any(|line| line.trim() == "partial"));
                    // Namespace portions may be distributed separately; a
                    // regular child package still owns its descendants.
                    if component + 1 < stub_parts.len()
                        && ancestor.join("__init__.pyi").is_file()
                        && !partial
                    {
                        return None;
                    }
                }
            }
        }
        let dirs: Vec<PathBuf> = self
            .roots
            .iter()
            .chain(self.search_paths.iter())
            .cloned()
            .collect();
        self.locate(&dirs, &parts)
            .map(|(source, _)| source)
            .or_else(|| {
                (typeshed::stub_for_module(name).is_some()
                    && typeshed::module_available(name, self.python_version))
                .then(|| ModuleSource::Stub(name.to_owned()))
            })
    }

    /// The Python source of `name` in the interpreter's search paths only
    /// (bypassing stubs), e.g. for documentation that stubs do not carry.
    pub fn resolve_source(&self, name: &str) -> Option<PathBuf> {
        if self.frozen {
            let source = self.frozen_sources.get(name).cloned();
            if source.is_none() {
                self.missing_dependency
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            return source;
        }
        let parts: Vec<&str> = name.split('.').collect();
        let (ModuleSource::File { path, .. }, _) = self.locate(&self.search_paths, &parts)? else {
            return None;
        };
        if path.extension().and_then(|e| e.to_str()) == Some("py") {
            return Some(path);
        }
        let py = path.with_extension("py");
        (py.is_file() && !self.is_excluded(&py)).then_some(py)
    }

    fn resolve_in_dir(&self, root: &Path, parts: &[&str]) -> Option<ModuleSource> {
        self.locate(&[root.to_path_buf()], parts)
            .map(|(source, _)| source)
    }

    /// Carry the selected parent's real search locations down every dotted
    /// component. Namespace portions accumulate; any regular package/module
    /// wins over those portions, with the original directory ordering retained.
    fn locate(&self, roots: &[PathBuf], parts: &[&str]) -> Option<(ModuleSource, Vec<PathBuf>)> {
        let mut locations: Vec<_> = roots
            .iter()
            .filter(|p| !self.is_excluded(p))
            .cloned()
            .collect();
        let mut result = None;
        for (index, part) in parts.iter().enumerate() {
            let last = index + 1 == parts.len();
            let mut namespaces = Vec::new();
            let mut concrete = None;
            for dir in &locations {
                match self.direct_child(dir, part) {
                    Some(ModuleSource::Namespace(path)) => namespaces.push(path),
                    Some(source) => {
                        concrete = Some(source);
                        break;
                    }
                    None => {}
                }
            }
            if let Some(source) = concrete {
                locations = match &source {
                    ModuleSource::File {
                        path,
                        is_package: true,
                    } => vec![path.parent()?.to_path_buf()],
                    _ if !last => return None,
                    _ => Vec::new(),
                };
                result = Some(source);
            } else {
                result = Some(ModuleSource::Namespace(namespaces.first()?.clone()));
                locations = namespaces;
            }
        }
        Some((result?, locations))
    }

    fn direct_child(&self, dir: &Path, part: &str) -> Option<ModuleSource> {
        let pkg_dir = dir.join(part);
        for (file, is_package) in [
            (pkg_dir.join("__init__.pyi"), true),
            (pkg_dir.join("__init__.py"), true),
            (dir.join(format!("{part}.pyi")), false),
            (dir.join(format!("{part}.py")), false),
        ] {
            if !self.is_excluded(&file) && file.is_file() {
                return Some(ModuleSource::File {
                    path: file,
                    is_package,
                });
            }
        }
        if let Some(bin) = self.binary_in(dir, part).filter(|p| !self.is_excluded(p)) {
            return Some(ModuleSource::Binary(bin));
        }
        (!self.is_excluded(&pkg_dir) && pkg_dir.is_dir())
            .then_some(ModuleSource::Namespace(pkg_dir))
    }

    /// The compiled extension module `name` in `dir` (`name.so`,
    /// `name.cpython-312-x86_64-linux-gnu.so`, `name.abi3.so`, `name.pyd`,
    /// `name.cp312-win_amd64.pyd`). Directory listings are cached until
    /// [`ModuleResolver::invalidate`].
    fn binary_in(&self, dir: &Path, name: &str) -> Option<PathBuf> {
        if let Some(found) = self.binaries.read().get(dir) {
            return found.get(name).cloned();
        }
        let mut found: FxHashMap<String, PathBuf> = FxHashMap::default();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let file_name = e.file_name().to_string_lossy().into_owned();
                if let Some(module) = extension_module_name(&file_name) {
                    let path = e.path();
                    if path.is_file() {
                        found.entry(module.to_string()).or_insert(path);
                    }
                }
            }
        }
        let hit = found.get(name).cloned();
        self.binaries
            .write()
            .insert(dir.to_path_buf(), std::sync::Arc::new(found));
        hit
    }

    /// Resolves `from <dots><module> import …` relative to `from_file`.
    /// `level` is the number of leading dots.
    pub fn resolve_relative(
        &self,
        from_file: &Path,
        level: usize,
        module: &str,
    ) -> Option<ModuleSource> {
        if self.is_excluded(from_file) {
            return None;
        }
        if level == 0 {
            return self.resolve(module);
        }
        if self.frozen || self.module_name_of(from_file).is_some() {
            let base = self.module_name_of(from_file)?;
            let mut parts: Vec<_> = base.split('.').collect();
            if from_file.file_stem().and_then(|s| s.to_str()) != Some("__init__") {
                parts.pop();
            }
            for _ in 1..level {
                parts.pop()?;
            }
            if !module.is_empty() {
                parts.extend(module.split('.'));
            }
            return self.resolve(&parts.join("."));
        }
        let dir = Self::relative_package_dir(from_file, level)?;
        if self.is_excluded(&dir) {
            return None;
        }
        if module.is_empty() {
            for init in ["__init__.pyi", "__init__.py"] {
                let p = dir.join(init);
                if p.is_file() {
                    return Some(ModuleSource::File {
                        path: p,
                        is_package: true,
                    });
                }
            }
            return Some(ModuleSource::Namespace(dir));
        }
        let parts: Vec<&str> = module.split('.').collect();
        self.resolve_in_dir(&dir, &parts)
    }

    /// The package directory `level` leading dots refer to from `from_file`:
    /// `.` is the directory of the file (for `__init__.py` the package
    /// itself), every further dot one directory up.
    pub fn relative_package_dir(from_file: &Path, level: usize) -> Option<PathBuf> {
        let mut dir = from_file.parent()?.to_path_buf();
        for _ in 1..level {
            dir = dir.parent()?.to_path_buf();
        }
        Some(dir)
    }

    /// Dotted module name of a file inside one of the roots, e.g. `pkg.mod`.
    ///
    /// Among the roots and search paths containing the file, the deepest one
    /// that yields a valid dotted name wins: with `src` marked as a source root
    /// `src/pkg/mod.py` is `pkg.mod` (not `src.pkg.mod`), and a library in a
    /// project-local `.venv` is named relative to its `site-packages` (not
    /// `.venv.lib.python3.12.site-packages.requests`). Files in PEP 561
    /// `<pkg>-stubs` directories are named after `<pkg>`. When no root gives
    /// a valid name, the first containing root is used as before.
    pub fn module_name_of(&self, file: &Path) -> Option<String> {
        if self.is_excluded(file) {
            return None;
        }
        let mut best: Option<(usize, Vec<String>)> = None;
        let mut fallback: Option<Vec<String>> = None;
        for root in self
            .stub_roots
            .iter()
            .chain(self.roots.iter())
            .chain(self.search_paths.iter())
        {
            // Library files are keyed by canonical path while search paths
            // may be spelled through a symlinked project virtualenv.
            let Some(root) = self.root_containing(file, root) else {
                continue;
            };
            let Ok(rel) = file.strip_prefix(&root) else {
                continue;
            };
            let mut parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if let Some(last) = parts.last_mut() {
                *last = module_file_stem(last);
            }
            if parts.last().map(String::as_str) == Some("__init__") {
                parts.pop();
            }
            if parts.is_empty() {
                continue;
            }
            if let Some(stubbed) = parts[0]
                .strip_suffix("-stubs")
                .filter(|s| is_identifier(s))
                .map(str::to_owned)
            {
                parts[0] = stubbed;
            }
            if parts.iter().all(|p| is_identifier(p)) {
                let depth = root.components().count();
                if best.as_ref().is_none_or(|(d, _)| depth > *d) {
                    best = Some((depth, parts));
                }
            } else if fallback.is_none() {
                fallback = Some(parts);
            }
        }
        best.map(|(_, p)| p).or(fallback).map(|p| p.join("."))
    }

    /// Names of importable submodules of `package` (for completion).
    pub fn submodules(&self, package: &str) -> Vec<String> {
        if self.frozen {
            let prefix = format!("{package}.");
            return self
                .cache
                .read()
                .iter()
                .filter_map(|(name, source)| {
                    source
                        .source
                        .as_ref()
                        .and_then(|_| name.strip_prefix(&prefix))
                })
                .filter(|name| !name.contains('.'))
                .map(str::to_owned)
                .collect();
        }
        let mut out = Vec::new();
        match self.resolve(package) {
            Some(ModuleSource::File {
                path,
                is_package: true,
            }) => {
                if let Some(dir) = path.parent() {
                    out.extend(list_dir_modules(dir));
                    // A stub-only package lists the runtime package's modules too.
                    if let Some(runtime) = dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|n| n.strip_suffix("-stubs"))
                        .and_then(|n| dir.parent().map(|p| p.join(n)))
                    {
                        out.extend(list_dir_modules(&runtime));
                    }
                }
            }
            Some(ModuleSource::File {
                is_package: false, ..
            })
            | Some(ModuleSource::Binary(_)) => {}
            Some(ModuleSource::Namespace(_)) => {
                let roots: Vec<_> = self
                    .stub_roots
                    .iter()
                    .chain(self.roots.iter())
                    .chain(self.search_paths.iter())
                    .cloned()
                    .collect();
                let parts: Vec<_> = package.split('.').collect();
                if let Some((_, locations)) = self.locate(&roots, &parts) {
                    for dir in locations {
                        out.extend(list_dir_modules(&dir));
                    }
                }
            }
            Some(ModuleSource::Stub(name)) => {
                let prefix = format!("{name}.");
                for m in typeshed::all_modules() {
                    if let Some(rest) = m.strip_prefix(&prefix) {
                        if !rest.contains('.') {
                            out.push(rest.to_string());
                        }
                    }
                }
            }
            None => {}
        }
        out.retain(|name| self.resolve(&format!("{package}.{name}")).is_some());
        out.sort();
        out.dedup();
        out
    }

    /// Top-level importable names: project modules, search-path modules and stdlib.
    pub fn top_level_modules(&self) -> Vec<String> {
        if self.frozen {
            return self
                .cache
                .read()
                .iter()
                .filter(|(name, source)| source.source.is_some() && !name.contains('.'))
                .map(|(name, _)| name.clone())
                .collect();
        }
        let mut out: Vec<String> = Vec::new();
        for root in self
            .stub_roots
            .iter()
            .chain(self.roots.iter())
            .chain(self.search_paths.iter())
        {
            out.extend(list_dir_modules(root));
        }
        out.extend(
            typeshed::top_level_modules()
                .filter(|m| {
                    !m.starts_with('_') && typeshed::module_available(m, self.python_version)
                })
                .map(str::to_owned),
        );
        out.retain(|name| self.resolve(name).is_some());
        out.sort();
        out.dedup();
        out
    }
}

/// Whether `s` is a Python identifier (as a module name component).
pub fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// The module name of a compiled extension module file name: `etree` for
/// `etree.cpython-312-x86_64-linux-gnu.so`, `etree.abi3.so`, `etree.so`,
/// `etree.pyd` or `etree.cp312-win_amd64.pyd`.
pub fn extension_module_name(file_name: &str) -> Option<&str> {
    let parts: Vec<&str> = file_name.split('.').collect();
    let ok = matches!(parts.len(), 2 | 3)
        && matches!(parts.last(), Some(&"so") | Some(&"pyd"))
        && is_identifier(parts[0]);
    ok.then_some(parts[0])
}

/// Module name of the last path component: the stem of `mod.py` / `mod.pyi`,
/// the module of a compiled `mod.cpython-312-x86_64-linux-gnu.so`.
fn module_file_stem(file_name: &str) -> String {
    if let Some(m) = extension_module_name(file_name) {
        return m.to_string();
    }
    Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name.to_string())
}

/// Importable module names in `dir`: `.py`/`.pyi` modules, packages and
/// namespace directories, compiled extension modules, and the packages of
/// PEP 561 `<name>-stubs` directories.
pub fn list_dir_modules(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if let Some(stubbed) = name.strip_suffix("-stubs").filter(|s| is_identifier(s)) {
                out.push(stubbed.to_string());
            } else if is_identifier(&name) && name != "__pycache__" {
                out.push(name);
            }
        } else if let Some(stem) = name
            .strip_suffix(".py")
            .or_else(|| name.strip_suffix(".pyi"))
            .or_else(|| extension_module_name(&name))
        {
            if stem != "__init__" && is_identifier(stem) {
                out.push(stem.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, ModuleResolver) {
        let dir = tempfile::TempDir::with_prefix("rustpy-mod").unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("pkg/sub")).unwrap();
        std::fs::write(root.join("main.py"), "").unwrap();
        std::fs::write(root.join("pkg/__init__.py"), "").unwrap();
        std::fs::write(root.join("pkg/util.py"), "").unwrap();
        std::fs::write(root.join("pkg/util.pyi"), "").unwrap();
        std::fs::write(root.join("pkg/sub/const.py"), "").unwrap();
        std::fs::create_dir_all(root.join("ns/inner")).unwrap();
        std::fs::write(root.join("ns/inner/x.py"), "").unwrap();
        let r = ModuleResolver::new(vec![root], vec![], (3, 12));
        (dir, r)
    }

    #[test]
    fn resolves_project_modules_and_stubs() {
        let (dir, r) = project();
        let root = dir.path();
        assert_eq!(
            r.resolve("main"),
            Some(ModuleSource::File {
                path: root.join("main.py"),
                is_package: false
            })
        );
        assert_eq!(
            r.resolve("pkg"),
            Some(ModuleSource::File {
                path: root.join("pkg/__init__.py"),
                is_package: true
            })
        );
        assert_eq!(
            r.resolve("pkg.util"),
            Some(ModuleSource::File {
                path: root.join("pkg/util.pyi"),
                is_package: false
            }),
            "stub beside source wins"
        );
        assert_eq!(
            r.resolve("pkg.sub.const")
                .and_then(|s| s.path().map(Path::to_path_buf)),
            Some(root.join("pkg/sub/const.py"))
        );
        assert!(matches!(
            r.resolve("ns.inner"),
            Some(ModuleSource::Namespace(_))
        ));
        assert_eq!(
            r.resolve("os.path"),
            Some(ModuleSource::Stub("os.path".into()))
        );
        assert_eq!(r.resolve("nope.nothing"), None);
        assert_eq!(
            r.module_name_of(&root.join("pkg/sub/const.py")).as_deref(),
            Some("pkg.sub.const")
        );
        assert_eq!(
            r.module_name_of(&root.join("pkg/__init__.py")).as_deref(),
            Some("pkg")
        );
        assert_eq!(
            r.resolve_relative(&root.join("pkg/sub/const.py"), 2, "util")
                .and_then(|s| s.path().map(Path::to_path_buf)),
            Some(root.join("pkg/util.pyi"))
        );
        assert_eq!(
            r.resolve_relative(&root.join("pkg/__init__.py"), 1, "util")
                .and_then(|s| s.path().map(Path::to_path_buf)),
            Some(root.join("pkg/util.pyi"))
        );
        assert!(r.submodules("pkg").contains(&"util".to_string()));
        assert!(r.submodules("os").contains(&"path".to_string()));
        assert!(r.top_level_modules().contains(&"json".to_string()));
        assert!(r.top_level_modules().contains(&"pkg".to_string()));
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn module_names_use_the_deepest_valid_root() {
        let dir = tempfile::TempDir::with_prefix("rustpy-mod").unwrap();
        let root = dir.path().to_path_buf();
        let src = root.join("src");
        let site = root.join(".venv/lib/python3.12/site-packages");
        for f in [
            "src/pkg/__init__.py",
            "src/pkg/mod.py",
            "main.py",
            ".venv/lib/python3.12/site-packages/requests/api.py",
            "my-scripts/tool.py",
        ] {
            touch(&root.join(f));
        }
        let r = ModuleResolver::new(vec![root.clone(), src.clone()], vec![site.clone()], (3, 12));
        // A Sources Root names its modules (not `src.pkg.mod`).
        assert_eq!(
            r.module_name_of(&src.join("pkg/mod.py")).as_deref(),
            Some("pkg.mod")
        );
        assert_eq!(
            r.module_name_of(&src.join("pkg/__init__.py")).as_deref(),
            Some("pkg")
        );
        assert_eq!(
            r.module_name_of(&root.join("main.py")).as_deref(),
            Some("main")
        );
        // A library in a project-local virtual environment.
        assert_eq!(
            r.module_name_of(&site.join("requests/api.py")).as_deref(),
            Some("requests.api")
        );
        // No valid name anywhere: the first root as before.
        assert_eq!(
            r.module_name_of(&root.join("my-scripts/tool.py"))
                .as_deref(),
            Some("my-scripts.tool")
        );
        // Without the search path, the venv file keeps its long name.
        let bare = ModuleResolver::new(vec![root.clone()], vec![], (3, 12));
        assert_eq!(
            bare.module_name_of(&site.join("requests/api.py"))
                .as_deref(),
            Some(".venv.lib.python3.12.site-packages.requests.api")
        );
        // Resolution goes through the source root.
        assert_eq!(
            r.resolve("pkg.mod")
                .and_then(|s| s.path().map(Path::to_path_buf)),
            Some(src.join("pkg/mod.py"))
        );
    }

    #[test]
    fn stub_only_packages_shadow_the_runtime_package() {
        let dir = tempfile::TempDir::with_prefix("rustpy-mod").unwrap();
        let site = dir.path().join("site-packages");
        for f in [
            "requests/__init__.py",
            "requests/api.py",
            "requests/adapters.py",
            "requests-stubs/__init__.pyi",
            "requests-stubs/api.pyi",
            "google-stubs/protobuf/__init__.pyi",
            "google/cloud/__init__.py",
        ] {
            touch(&site.join(f));
        }
        let r = ModuleResolver::new(
            vec![dir.path().join("project")],
            vec![site.clone()],
            (3, 12),
        );
        assert_eq!(
            r.resolve("requests"),
            Some(ModuleSource::File {
                path: site.join("requests-stubs/__init__.pyi"),
                is_package: true
            })
        );
        assert_eq!(
            r.resolve("requests.api"),
            Some(ModuleSource::File {
                path: site.join("requests-stubs/api.pyi"),
                is_package: false
            })
        );
        // Missing partial-stub modules use vendor types for an untyped package,
        // then prefer the runtime package when it declares inline typing.
        std::fs::write(site.join("requests-stubs/py.typed"), "partial\n").unwrap();
        r.invalidate();
        assert_eq!(
            r.resolve("requests.adapters"),
            Some(ModuleSource::Stub("requests.adapters".to_owned()))
        );
        assert!(r.import_available("requests.adapters"));
        std::fs::write(site.join("requests/py.typed"), "").unwrap();
        r.invalidate();
        assert_eq!(
            r.resolve("requests.adapters"),
            Some(ModuleSource::File {
                path: site.join("requests/adapters.py"),
                is_package: false
            })
        );
        // A stub namespace directory does not hide the runtime namespace package.
        assert_eq!(
            r.resolve("google"),
            Some(ModuleSource::Namespace(site.join("google")))
        );
        assert_eq!(
            r.resolve("google.protobuf"),
            Some(ModuleSource::File {
                path: site.join("google-stubs/protobuf/__init__.pyi"),
                is_package: true
            })
        );
        assert!(matches!(
            r.resolve("google.cloud"),
            Some(ModuleSource::File { .. })
        ));
        // Stub files are named after the runtime package.
        assert_eq!(
            r.module_name_of(&site.join("requests-stubs/api.pyi"))
                .as_deref(),
            Some("requests.api")
        );
        assert_eq!(
            r.module_name_of(&site.join("requests-stubs/__init__.pyi"))
                .as_deref(),
            Some("requests")
        );
        let subs = r.submodules("requests");
        assert!(
            subs.contains(&"api".to_string()) && subs.contains(&"adapters".to_string()),
            "{subs:?}"
        );
        let top = r.top_level_modules();
        assert!(
            top.contains(&"requests".to_string()) && !top.iter().any(|m| m.ends_with("-stubs")),
            "{top:?}"
        );
    }

    #[test]
    fn compiled_extension_modules_resolve() {
        let dir = tempfile::TempDir::with_prefix("rustpy-mod").unwrap();
        let site = dir.path().join("site-packages");
        for f in [
            "ujson.cpython-312-x86_64-linux-gnu.so",
            "lxml/__init__.py",
            "lxml/etree.cpython-312-x86_64-linux-gnu.so",
            "speedups.abi3.so",
            "winmod.cp312-win_amd64.pyd",
            "both.cpython-312-x86_64-linux-gnu.so",
            "both.py",
            "libfoo.so.1",
        ] {
            touch(&site.join(f));
        }
        let r = ModuleResolver::new(
            vec![dir.path().join("project")],
            vec![site.clone()],
            (3, 12),
        );
        assert_eq!(
            r.resolve("ujson"),
            Some(ModuleSource::Binary(
                site.join("ujson.cpython-312-x86_64-linux-gnu.so")
            ))
        );
        assert_eq!(
            r.resolve("lxml.etree"),
            Some(ModuleSource::Binary(
                site.join("lxml/etree.cpython-312-x86_64-linux-gnu.so")
            ))
        );
        assert_eq!(
            r.resolve("speedups"),
            Some(ModuleSource::Binary(site.join("speedups.abi3.so")))
        );
        assert_eq!(
            r.resolve("winmod"),
            Some(ModuleSource::Binary(
                site.join("winmod.cp312-win_amd64.pyd")
            ))
        );
        // Python source beside a compiled module is analysed instead.
        assert_eq!(
            r.resolve("both"),
            Some(ModuleSource::File {
                path: site.join("both.py"),
                is_package: false
            })
        );
        assert_eq!(r.resolve("libfoo"), None);
        assert_eq!(r.resolve("lxml.objectify"), None);
        assert_eq!(
            r.module_name_of(&site.join("lxml/etree.cpython-312-x86_64-linux-gnu.so"))
                .as_deref(),
            Some("lxml.etree")
        );
        assert!(r.submodules("lxml").contains(&"etree".to_string()));
        assert!(r.top_level_modules().contains(&"ujson".to_string()));
        assert_eq!(
            ModuleSource::Binary(PathBuf::from("x.so"))
                .text()
                .as_deref(),
            Some(BINARY_MODULE_STUB)
        );
        assert_eq!(
            extension_module_name("etree.cpython-312-darwin.so"),
            Some("etree")
        );
        assert_eq!(extension_module_name("a.b.c.so"), None);
        assert_eq!(extension_module_name("module.py"), None);
        // A module appearing later is found after invalidation.
        assert_eq!(r.resolve("late"), None);
        touch(&site.join("late.cpython-312-x86_64-linux-gnu.so"));
        assert_eq!(r.resolve("late"), None, "cached");
        r.invalidate();
        assert!(matches!(r.resolve("late"), Some(ModuleSource::Binary(_))));
    }
}

#[cfg(test)]
mod frozen_exclusion_policy_tests {
    use super::*;
    #[test]
    fn bundled_types_do_not_install_dependencies_or_invent_frozen_import_answers() {
        let root = tempfile::TempDir::with_prefix("bundled-import-availability").unwrap();
        let resolver = ModuleResolver::new(vec![root.path().to_path_buf()], vec![], (3, 12));
        assert!(matches!(
            resolver.resolve("requests"),
            Some(ModuleSource::Stub(_))
        ));
        assert!(!resolver.import_available("requests"));
        let frozen = resolver
            .frozen_catalogue(std::iter::empty(), &rustpy_core::CancellationToken::new())
            .unwrap();
        assert!(!frozen.import_available("requests"));
        assert!(frozen.import_available("json"));
        assert!(frozen.snapshot_complete());

        std::fs::write(root.path().join("requests.py"), "installed = True\n").unwrap();
        assert!(!resolver.import_available("requests"));
        resolver.invalidate();
        assert!(resolver.import_available("requests"));
        assert!(!frozen.import_available("requests"));
        assert!(
            frozen.snapshot_complete(),
            "captured absence is still complete"
        );

        assert!(
            frozen.resolve("yaml").is_some(),
            "immutable type data remains available"
        );
        assert!(!frozen.import_available("yaml"));
        assert!(
            !frozen.snapshot_complete(),
            "uncaptured dependency needs a fresh snapshot"
        );

        let installed = ModuleResolver::new(vec![], vec![root.path().to_path_buf()], (3, 12));
        assert!(matches!(
            installed.resolve("requests"),
            Some(ModuleSource::Stub(_))
        ));
        assert!(installed.import_available("requests"));
        let captured = installed
            .frozen_catalogue(std::iter::empty(), &rustpy_core::CancellationToken::new())
            .unwrap();
        std::fs::remove_file(root.path().join("requests.py")).unwrap();
        installed.invalidate();
        assert!(!installed.import_available("requests"));
        assert!(captured.import_available("requests"));
        assert!(
            captured.snapshot_complete(),
            "captured installation stays immutable"
        );
    }

    #[test]
    #[should_panic(expected = "cannot change exclusions of a frozen resolver")]
    fn frozen_policy_builder_refuses_change_before_cached_answers_can_be_reused() {
        let root = tempfile::TempDir::with_prefix("frozen-exclusions").unwrap();
        let file = root.path().join("visible.py");
        std::fs::write(&file, "value = 1\n").unwrap();
        let resolver = ModuleResolver::new(vec![root.path().to_path_buf()], vec![], (3, 12));
        assert!(resolver.resolve("visible").is_some());
        let frozen = resolver
            .frozen_catalogue(
                std::iter::once(("visible".to_owned(), file.clone())),
                &rustpy_core::CancellationToken::new(),
            )
            .unwrap();
        assert!(
            frozen.resolve("visible").is_some(),
            "actual frozen cached success exists before refusal"
        );
        let _ = frozen.with_excluded([file]);
    }
    #[cfg(unix)]
    #[test]
    fn frozen_exclusion_identity_does_not_follow_later_symlink_retarget() {
        let root = tempfile::TempDir::with_prefix("frozen-exclusions").unwrap();
        let excluded = root.path().join("excluded");
        let moved = root.path().join("original-moved");
        let other = root.path().join("other");
        std::fs::create_dir(&excluded).unwrap();
        std::fs::create_dir(&other).unwrap();
        let captured = excluded.join("hidden.py");
        std::fs::write(&captured, "value = 1\n").unwrap();
        std::fs::write(other.join("hidden.py"), "different = 2\n").unwrap();
        let resolver = ModuleResolver::new(vec![root.path().to_path_buf()], vec![], (3, 12))
            .with_excluded([excluded.clone()]);
        let frozen = resolver
            .frozen_catalogue(
                std::iter::once(("excluded.hidden".to_owned(), captured.clone())),
                &rustpy_core::CancellationToken::new(),
            )
            .unwrap();
        assert!(frozen.is_excluded(&captured));
        std::fs::rename(&excluded, &moved).unwrap();
        std::os::unix::fs::symlink(&other, &excluded).unwrap();
        assert_eq!(
            std::fs::read_to_string(&captured).unwrap(),
            "different = 2\n",
            "retarget is actual, not simulated"
        );
        assert!(
            frozen.is_excluded(&captured),
            "frozen exclusion compares captured path without live canonicalization"
        );
        assert!(frozen.module_name_of(&captured).is_none());
        assert!(frozen.resolve("excluded.hidden").is_none());
        assert!(frozen.resolve_relative(&captured, 1, "").is_none());
    }
}
