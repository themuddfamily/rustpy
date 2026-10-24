//! Inferred return types of library functions, shared across inference sessions.
//! Library, virtual-environment and bundled sources do not change while project
//! files are edited, so their unannotated function bodies are inferred once and
//! reused by inspections, completion and hover. Entries are keyed by the defining
//! file's content and dropped when any library source, the interpreter, run-time
//! types or module resolution change (see `Analysis::library_changed`).
use super::*;
use rustpy_core::vfs::ContentRevision;
use std::path::Path;

/// Upper bound of retained entries; the least recently used half is dropped beyond it.
const CAPACITY: usize = 50_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Version {
    /// Disk contents, stable across eviction and reload of the same text.
    Content(ContentRevision),
    /// Editor or virtual text: this exact `FileInfo`.
    Exact(usize),
}

fn version(file: &Arc<FileInfo>) -> Version {
    match file.disk_revision {
        Some(revision) if !file.is_open => Version::Content(revision),
        _ => Version::Exact(Arc::as_ptr(file) as usize),
    }
}

#[derive(Default)]
pub(crate) struct LibraryReturns {
    entries: FxHashMap<(FileId, TextRange), (Version, Type, u64)>,
    tick: u64,
    /// Changes whenever entries are dropped for a library change; results computed
    /// across a change are not stored.
    epoch: u64,
    hits: u64,
    stores: u64,
}

impl LibraryReturns {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.epoch += 1;
    }
    /// `(hits, stores, retained entries)` since the database was created.
    pub(crate) fn stats(&self) -> (u64, u64, usize) {
        (self.hits, self.stores, self.entries.len())
    }
}

/// Whether `path` is a library source: bundled stubs, interpreter search paths and
/// installed-package directories (also inside an in-project virtual environment).
pub(crate) fn library_path(db: &Analysis, path: &Path) -> bool {
    path.starts_with("<typeshed>")
        || db
            .resolver
            .search_paths
            .iter()
            .any(|root| db.resolver.root_containing(path, root).is_some())
        || path.components().any(|part| {
            matches!(
                part.as_os_str().to_str(),
                Some("site-packages" | "dist-packages")
            )
        })
}

impl Inferer<'_> {
    /// The shared return type of library function `f`, or `None` with the epoch to
    /// store a freshly computed one under.
    pub(super) fn cached_library_return(
        &self,
        file: &Arc<FileInfo>,
        f: &ast::FuncDef,
    ) -> Result<Type, Option<u64>> {
        if file.is_stub || !library_path(self.db, &file.path) {
            return Err(None);
        }
        let mut cache = self.db.library_returns.lock();
        cache.tick += 1;
        let tick = cache.tick;
        let wanted = version(file);
        let epoch = cache.epoch;
        let found = match cache.entries.get_mut(&(file.id, f.text_range())) {
            Some((version, ty, used)) if *version == wanted => {
                *used = tick;
                Some(ty.clone())
            }
            _ => None,
        };
        match found {
            Some(ty) => {
                cache.hits += 1;
                Ok(ty)
            }
            None => Err(Some(epoch)),
        }
    }

    pub(super) fn store_library_return(
        &self,
        file: &Arc<FileInfo>,
        f: &ast::FuncDef,
        epoch: u64,
        ty: &Type,
    ) {
        if self.budget_spent() || !self.session_independent(ty, &mut 512) {
            return;
        }
        let mut cache = self.db.library_returns.lock();
        if cache.epoch != epoch {
            return;
        }
        if cache.entries.len() >= CAPACITY {
            let mut ticks: Vec<u64> = cache.entries.values().map(|(_, _, used)| *used).collect();
            let middle = ticks.len() / 2;
            let (_, cut, _) = ticks.select_nth_unstable(middle);
            let cut = *cut;
            cache.entries.retain(|_, (_, _, used)| *used > cut);
        }
        cache.tick += 1;
        let tick = cache.tick;
        cache.stores += 1;
        cache
            .entries
            .insert((file.id, f.text_range()), (version(file), ty.clone(), tick));
    }

    /// Whether `ty` means the same in every inference session: it names no synthetic
    /// value owned by this session (`functools.partial` results).
    fn session_independent(&self, ty: &Type, budget: &mut usize) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let signature = |s: &Signature, budget: &mut usize| {
            self.session_independent(&s.ret, budget)
                && s.params
                    .iter()
                    .all(|p| self.session_independent(&p.ty, budget))
        };
        match ty {
            Type::Class(class, args) | Type::Instance(class, args) => {
                !self.is_functools_value(&Type::Instance(class.clone(), Vec::new()))
                    && args.iter().all(|t| self.session_independent(t, budget))
            }
            Type::SelfType(class) => {
                !self.is_functools_value(&Type::Instance(class.clone(), Vec::new()))
            }
            Type::Tuple(items) | Type::Union(items) | Type::TypePack(items) => {
                items.iter().all(|t| self.session_independent(t, budget))
            }
            Type::Unpack(inner) => self.session_independent(inner, budget),
            Type::Function(s) | Type::Callable(s) => signature(s, budget),
            Type::BoundMethod(s, receiver) => {
                signature(s, budget) && self.session_independent(receiver, budget)
            }
            Type::Overloaded(all) => all.iter().all(|s| signature(s, budget)),
            Type::Super { receiver, .. } => self.session_independent(receiver, budget),
            Type::ParameterList(params) => params
                .iter()
                .all(|p| self.session_independent(&p.ty, budget)),
            Type::Concatenate(items, _) => {
                items.iter().all(|t| self.session_independent(t, budget))
            }
            _ => true,
        }
    }
}
