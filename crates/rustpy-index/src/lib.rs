//! RustPy IDE — persistent symbol indexes (roadmap §2.2 "Project-wide index",
//! §7 "Shared indexes").
//!
//! An index records, for every Python file below a set of roots (the
//! interpreter's `sys.path` directories, or a project), its size,
//! modification time and content hash together with the symbols declared in
//! it ([`extract`]). Indexes are kept on disk ([`store`]), so a restart only
//! compares file stamps and re-parses what changed ([`build::refresh`]).
//!
//! Extraction depends only on a file's content, so an index can also serve as
//! a *shared index* (a bundle): exported for the standard library on one
//! machine, it lets another RustPy skip parsing every file whose content hash
//! it contains, wherever that file lives.
//!
//! Queries ([`SymbolIndex::search`], [`SymbolIndex::import_candidates`]) back
//! Go to Symbol / Class with "Include non-project items", Search Everywhere and
//! the second-press auto-import completion.

pub mod build;
pub mod extract;
pub mod format;
pub mod hash;
pub mod index;
pub mod store;

pub use build::{
    refresh, walk_library_roots, BuildOptions, Progress, RefreshStats, RootSpec, SourceFile,
};
pub use extract::{declared_bases, extract, DunderAll, FileSymbols, Symbol, SymbolKind};
pub use hash::ContentHash;
pub use index::{
    ClassBases, FileBases, ImportCandidate, IndexKind, IndexMeta, RootKind, SearchOptions,
    SymbolHit, SymbolIndex,
};
pub use store::{IndexError, IndexStore, StagedIndex, StoredIndex};
