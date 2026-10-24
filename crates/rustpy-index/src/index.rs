//! The in-memory index: roots, files and symbols in compact tables (one string
//! arena, fixed-size records), plus the lookup tables built over them when an
//! index is created or loaded.

use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::extract::{FileSymbols, SymbolKind, FLAG_EXPLICIT_REEXPORT, STORED_FLAGS};
use crate::hash::ContentHash;

/// What an index covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexKind {
    /// The interpreter's library directories (stdlib, site-packages).
    #[default]
    Libraries,
    /// The files of a project.
    Project,
    /// A shared index bundle: only its content hashes and symbols are used.
    Bundle,
}

impl IndexKind {
    pub fn label(self) -> &'static str {
        match self {
            IndexKind::Libraries => "Interpreter libraries",
            IndexKind::Project => "Project",
            IndexKind::Bundle => "Shared bundle",
        }
    }
}

/// Where a root comes from (ranks results: project, then stdlib, then packages).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RootKind {
    Project,
    Stdlib,
    /// `site-packages` / `dist-packages`.
    Site,
    Other,
}

impl RootKind {
    pub fn code(self) -> u8 {
        match self {
            RootKind::Project => 0,
            RootKind::Stdlib => 1,
            RootKind::Site => 2,
            RootKind::Other => 3,
        }
    }

    pub fn from_code(code: u8) -> Option<RootKind> {
        [
            RootKind::Project,
            RootKind::Stdlib,
            RootKind::Site,
            RootKind::Other,
        ]
        .get(code as usize)
        .copied()
    }

    pub fn label(self) -> &'static str {
        match self {
            RootKind::Project => "project",
            RootKind::Stdlib => "stdlib",
            RootKind::Site => "site-packages",
            RootKind::Other => "library",
        }
    }

    /// Guesses the kind of an interpreter search path: the directory with
    /// `os.py` and the `encodings` package is the standard library (a project
    /// on `sys.path` with its own `os.py` is not), `site-packages` /
    /// `dist-packages` hold installed packages.
    pub fn classify(path: &Path) -> RootKind {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "site-packages" || name == "dist-packages" {
            RootKind::Site
        } else if path.join("os.py").is_file()
            && path.join("encodings").join("__init__.py").is_file()
        {
            RootKind::Stdlib
        } else {
            RootKind::Other
        }
    }
}

/// Facts about an index, stored uncompressed at the start of its file so that
/// listing indexes does not need to load them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexMeta {
    pub kind: IndexKind,
    /// What the index is for, as shown in Settings: `Python 3.12.3 (venv)`, a project name.
    pub label: String,
    /// The interpreter executable or project root it belongs to.
    pub owner: String,
    pub python_version: String,
    /// Unix seconds.
    pub created: u64,
    pub updated: u64,
    /// Counts, filled in by [`SymbolIndex`] constructors.
    pub files: u64,
    pub symbols: u64,
}

/// An indexed directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub path: PathBuf,
    pub kind: RootKind,
}

/// Strings in one arena.
#[derive(Clone, Debug, Default)]
pub(crate) struct Strings {
    arena: String,
    spans: Vec<(u32, u32)>,
}

impl Strings {
    pub(crate) fn get(&self, id: u32) -> &str {
        match self.spans.get(id as usize) {
            Some(&(start, len)) => &self.arena[start as usize..(start + len) as usize],
            None => "",
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.spans.len()
    }

    pub(crate) fn push(&mut self, s: &str) -> u32 {
        let id = self.spans.len() as u32;
        self.spans.push((self.arena.len() as u32, s.len() as u32));
        self.arena.push_str(s);
        id
    }

    fn shrink_to_fit(&mut self) {
        self.arena.shrink_to_fit();
        self.spans.shrink_to_fit();
    }

    pub(crate) fn reserve(&mut self, count: usize, bytes: usize) {
        self.spans.reserve(count);
        self.arena.reserve(bytes);
    }
}

/// "No symbol" in [`SymRec::parent`].
pub(crate) const NONE: u32 = u32::MAX;

/// A binary extension module (`.so` / `.pyd`): not read, no symbols.
pub(crate) const FILE_EXTENSION: u8 = 1;
pub(crate) const FILE_HAS_ALL: u8 = 1 << 1;
pub(crate) const FILE_ALL_EXACT: u8 = 1 << 2;
pub(crate) const FILE_STORED_FLAGS: u8 = FILE_EXTENSION | FILE_HAS_ALL | FILE_ALL_EXACT;

#[derive(Clone, Debug)]
pub(crate) struct FileRec {
    pub root: u32,
    /// Path relative to the root, with `/` separators.
    pub rel: u32,
    /// Dotted module name (empty when the path is not importable).
    pub module: u32,
    pub size: u64,
    /// Modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    pub hash: ContentHash,
    pub flags: u8,
    /// `__all__` names: a range of `SymbolIndex::all_names`.
    pub all_start: u32,
    pub all_len: u32,
    /// A range of `SymbolIndex::symbols`.
    pub sym_start: u32,
    pub sym_len: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SymRec {
    pub name: u32,
    pub detail: u32,
    pub origin: u32,
    pub file: u32,
    pub line: u32,
    /// Global index of the enclosing class, or [`NONE`].
    pub parent: u32,
    pub kind: SymbolKind,
    pub flags: u8,
    /// Declared bases of a class joined with `,` (0: none).
    pub bases: u32,
}

/// In memory only: the symbol is part of its module's importable API.
pub(crate) const FLAG_EXPORTED: u8 = 1 << 7;

/// A distinct symbol name and the symbols that have it.
#[derive(Clone, Debug)]
struct NameRow {
    name: u32,
    /// [`char_mask`] of the name.
    mask: u64,
    /// Bit `kind.code()` set for every kind among the symbols.
    kinds: u8,
    start: u32,
    len: u32,
}

/// Bits of the characters in `s` (ASCII letters case-insensitively): a name can
/// only match a query whose bits it has all of.
pub(crate) fn char_mask(s: &str) -> u64 {
    let mut m = 0u64;
    for b in s.bytes() {
        m |= match b {
            b'a'..=b'z' => 1 << (b - b'a'),
            b'A'..=b'Z' => 1 << (b - b'A'),
            b'0'..=b'9' => 1 << (26 + b - b'0'),
            b'_' => 1 << 36,
            0x80.. => 1 << 37,
            _ => 1 << 38,
        };
    }
    m
}

fn kind_bit(kind: SymbolKind) -> u8 {
    1 << kind.code()
}

fn is_dunder(name: &str) -> bool {
    name.len() > 4 && name.starts_with("__") && name.ends_with("__")
}

/// Whether the file at `rel` (relative to its root) is an import hook that a
/// build backend writes into site-packages for an editable install, named
/// after the user's project: setuptools' `__editable___<project>_<version>_finder.py`,
/// editables' (hatchling, pdm) `_editable_impl_<project>.py`, meson-python's
/// `_<project>_editable_loader.py` and scikit-build-core's `_<project>_editable.py`.
/// Bundles leave them out (a file left out is parsed where the bundle is used).
pub(crate) fn is_editable_install_hook(rel: &str) -> bool {
    if rel.starts_with("__editable__") {
        return true;
    }
    let Some(stem) = rel.strip_suffix(".py").filter(|s| !s.contains('/')) else {
        return false;
    };
    stem.starts_with("_editable_impl_")
        || (stem.starts_with('_')
            && (stem.ends_with("_editable_loader") || stem.ends_with("_editable")))
}

/// Whether any component of a dotted module name is private (`_core`, `pkg._impl`).
fn is_private_module(module: &str) -> bool {
    module
        .split('.')
        .any(|c| c.starts_with('_') && !is_dunder(c))
}

/// Whether `name` starts with `lower_prefix` ignoring case (ASCII without allocating).
fn starts_with_lower(name: &str, lower_prefix: &str) -> bool {
    if name.is_ascii() && lower_prefix.is_ascii() {
        name.len() >= lower_prefix.len()
            && name.as_bytes()[..lower_prefix.len()].eq_ignore_ascii_case(lower_prefix.as_bytes())
    } else {
        name.to_lowercase().starts_with(lower_prefix)
    }
}

/// Options of [`SymbolIndex::search`].
#[derive(Clone, Debug)]
pub struct SearchOptions {
    /// Go to Class: classes only.
    pub classes_only: bool,
    /// Maximum number of hits.
    pub limit: usize,
    /// Maximum number of hits with the same name (`__init__`, `logger`, …).
    pub per_name: usize,
    /// Hits in these files are left out: the files of the project index
    /// when searching the library index (an editable install puts the
    /// project's sources on `sys.path`).
    pub exclude_files: Option<Arc<FxHashSet<PathBuf>>>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            classes_only: false,
            limit: 60,
            per_name: 8,
            exclude_files: None,
        }
    }
}

/// A search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolHit {
    pub name: String,
    pub kind: SymbolKind,
    pub flags: u8,
    pub detail: String,
    /// Enclosing classes, outermost first, joined with `.` (empty at module level).
    pub container: String,
    pub module: String,
    pub path: PathBuf,
    /// 0-based.
    pub line: u32,
    pub score: i32,
    pub root_kind: RootKind,
}

/// A class recorded in an index with its declared bases ([`crate::extract::declared_bases`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassBases {
    pub name: String,
    /// 0-based line of the class name.
    pub line: u32,
    pub bases: Vec<String>,
}

/// What [`SymbolIndex::class_bases`] records for one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileBases {
    pub path: PathBuf,
    /// Content hash of the indexed text: the record is only valid for it.
    pub hash: ContentHash,
    pub classes: Vec<ClassBases>,
    /// Module-level `from m import origin as alias`: `(alias, origin)`.
    pub imports: Vec<(String, String)>,
    /// Module-level variables (possible class aliases: `Base = Impl`).
    pub variables: Vec<String>,
}

impl FileBases {
    /// The same record for freshly extracted symbols (a file whose text
    /// differs from the indexed content).
    pub fn from_symbols(path: PathBuf, hash: ContentHash, symbols: &FileSymbols) -> FileBases {
        let mut file = FileBases {
            path,
            hash,
            classes: Vec::new(),
            imports: Vec::new(),
            variables: Vec::new(),
        };
        for s in &symbols.symbols {
            match s.kind {
                SymbolKind::Class => file.classes.push(ClassBases {
                    name: s.name.clone(),
                    line: s.line,
                    bases: s.bases.clone(),
                }),
                SymbolKind::Import if s.parent.is_none() => {
                    if !s.origin.is_empty() && s.origin != s.name {
                        file.imports.push((s.name.clone(), s.origin.clone()));
                    }
                }
                SymbolKind::Variable | SymbolKind::Constant if s.parent.is_none() => {
                    file.variables.push(s.name.clone());
                }
                _ => {}
            }
        }
        file
    }
}

/// A name that can be imported: `from <module> import <name>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportCandidate {
    pub module: String,
    pub name: String,
    /// The kind of the definition (never [`SymbolKind::Import`]: re-exports are
    /// followed to what they import; [`SymbolKind::Variable`] when unknown).
    pub kind: SymbolKind,
}

/// A loaded or freshly built index. Cheap to query from any thread.
#[derive(Clone, Debug)]
pub struct SymbolIndex {
    pub(crate) meta: IndexMeta,
    pub(crate) strings: Strings,
    pub(crate) roots: Vec<Root>,
    pub(crate) files: Vec<FileRec>,
    pub(crate) all_names: Vec<u32>,
    pub(crate) symbols: Vec<SymRec>,
    names: Vec<NameRow>,
    by_name: Vec<u32>,
    by_module: FxHashMap<Box<str>, u32>,
    /// Stub files (`.pyi`) by module: consulted first when following imports.
    stub_by_module: FxHashMap<Box<str>, u32>,
    /// File -> files whose module does `from <its module> import *`.
    star_importers: FxHashMap<u32, Vec<u32>>,
    /// Per file: number of its top-level package (groups import candidates).
    file_top: Vec<u32>,
    /// Per symbol: for exported imports, the kind of what they import
    /// (`SymbolKind::code()` or a `KIND_*` value).
    def_kind: Vec<u8>,
}

/// [`SymbolIndex::def_kind`]: the import binds a module.
const KIND_MODULE: u8 = 0xFD;
/// The import could not be followed to a definition.
const KIND_UNRESOLVED: u8 = 0xFE;
/// Not an exported import.
const KIND_NONE: u8 = 0xFF;

/// How good a module is to import a name from (lower is better): public
/// before private, shallow before deep, followed to a definition before not,
/// project before stdlib before packages, short before long.
type Rank = (bool, usize, bool, RootKind, usize);

/// Star imports followed from a module offering a name (`from .core import *`
/// in `pkg/__init__.py`, and a package star-importing that).
const STAR_DEPTH: u32 = 3;
/// Work spent following star imports for one query at most (one unit per
/// importing module looked at, plus the length of its `__all__` when that is
/// searched): candidates are computed on the UI thread, and star imports can
/// form dense graphs. Real environments need a small fraction of it.
const STAR_BUDGET: usize = 1_000_000;
/// Other top-level packages one definition is offered from at most through
/// star imports (`from numpy import *` in `pylab`). Within its own package
/// re-exports are always followed; across packages, a tree of modules
/// star-importing each other would otherwise turn every import followed
/// into an offer, about a million per query (a definition is offered from its
/// canonical module, not from every module that re-exports it).
const MAX_STAR_OFFERS: usize = 8;

/// The bookkeeping of one candidate query.
struct Offers {
    /// (name row, top-level package) -> best offer.
    best: FxHashMap<(u32, u32), Offer>,
    /// Star-import work left for this query (see [`STAR_BUDGET`]).
    budget: usize,
}

impl Offers {
    fn new() -> Offers {
        Offers {
            best: FxHashMap::default(),
            budget: STAR_BUDGET,
        }
    }
}

#[derive(Clone, Copy)]
struct Offer {
    rank: Rank,
    file: u32,
    kind: SymbolKind,
    prefix: bool,
}

/// Module-level symbols sorted by `(file, name id, symbol id)`.
struct TopLevel(Vec<(u32, u32, u32)>);

impl TopLevel {
    /// The first module-level symbol named `name` in `file`.
    fn get(&self, file: u32, name: u32) -> Option<u32> {
        let i = self.0.partition_point(|&(f, n, _)| (f, n) < (file, name));
        self.0
            .get(i)
            .filter(|&&(f, n, _)| (f, n) == (file, name))
            .map(|&(_, _, id)| id)
    }
}

/// Assembles an index file by file (see `build::refresh`).
pub(crate) struct IndexBuilder {
    strings: Strings,
    interned: FxHashMap<Box<str>, u32>,
    roots: Vec<Root>,
    files: Vec<FileRec>,
    all_names: Vec<u32>,
    symbols: Vec<SymRec>,
}

/// Where a file came from, for [`IndexBuilder::add_file`].
pub(crate) struct FileStamp<'a> {
    pub root: u32,
    pub rel: &'a str,
    pub module: &'a str,
    pub size: u64,
    pub mtime_ns: i64,
    pub hash: ContentHash,
}

impl IndexBuilder {
    pub(crate) fn new(roots: Vec<Root>) -> IndexBuilder {
        let mut b = IndexBuilder {
            strings: Strings::default(),
            interned: FxHashMap::default(),
            roots,
            files: Vec::new(),
            all_names: Vec::new(),
            symbols: Vec::new(),
        };
        b.intern("");
        b
    }

    pub(crate) fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.interned.get(s) {
            return id;
        }
        let id = self.strings.push(s);
        self.interned.insert(s.into(), id);
        id
    }

    fn push_file(&mut self, st: &FileStamp<'_>, flags: u8) -> u32 {
        let rel = self.intern(st.rel);
        let module = self.intern(st.module);
        let idx = self.files.len() as u32;
        self.files.push(FileRec {
            root: st.root,
            rel,
            module,
            size: st.size,
            mtime_ns: st.mtime_ns,
            hash: st.hash,
            flags,
            all_start: self.all_names.len() as u32,
            all_len: 0,
            sym_start: self.symbols.len() as u32,
            sym_len: 0,
        });
        idx
    }

    /// Adds a file with freshly extracted symbols (`None`: an extension module or a file not read).
    pub(crate) fn add_file(
        &mut self,
        st: &FileStamp<'_>,
        symbols: Option<&FileSymbols>,
        extension: bool,
    ) {
        let mut flags = if extension { FILE_EXTENSION } else { 0 };
        if let Some(all) = symbols.and_then(|s| s.dunder_all.as_ref()) {
            flags |= FILE_HAS_ALL | if all.exact { FILE_ALL_EXACT } else { 0 };
        }
        let file = self.push_file(st, flags);
        let Some(fs) = symbols else { return };
        if let Some(all) = &fs.dunder_all {
            for n in &all.names {
                let id = self.intern(n);
                self.all_names.push(id);
            }
        }
        let base = self.symbols.len() as u32;
        for s in &fs.symbols {
            let name = self.intern(&s.name);
            let detail = self.intern(&s.detail);
            let origin = self.intern(&s.origin);
            let bases = self.intern(&s.bases.join(","));
            let parent = s
                .parent
                .filter(|p| (*p as usize) < fs.symbols.len())
                .map_or(NONE, |p| base + p);
            self.symbols.push(SymRec {
                name,
                detail,
                origin,
                file,
                line: s.line,
                parent,
                kind: s.kind,
                flags: s.flags & STORED_FLAGS,
                bases,
            });
        }
        let f = &mut self.files[file as usize];
        f.all_len = self.all_names.len() as u32 - f.all_start;
        f.sym_len = self.symbols.len() as u32 - f.sym_start;
    }

    /// Adds a file whose symbols are those of file `from_file` of `from`
    /// (same content: an unchanged file, a copy, a shared bundle entry).
    pub(crate) fn add_copy(&mut self, st: &FileStamp<'_>, from: &SymbolIndex, from_file: u32) {
        let src = &from.files[from_file as usize];
        let file = self.push_file(st, src.flags & FILE_STORED_FLAGS);
        for &n in &from.all_names[src.all_start as usize..(src.all_start + src.all_len) as usize] {
            let id = self.intern(from.strings.get(n));
            self.all_names.push(id);
        }
        let base = self.symbols.len() as u32;
        for s in &from.symbols[src.sym_start as usize..(src.sym_start + src.sym_len) as usize] {
            let name = self.intern(from.strings.get(s.name));
            let detail = self.intern(from.strings.get(s.detail));
            let origin = self.intern(from.strings.get(s.origin));
            let bases = self.intern(from.strings.get(s.bases));
            let parent = if s.parent == NONE {
                NONE
            } else {
                base + (s.parent - src.sym_start)
            };
            self.symbols.push(SymRec {
                name,
                detail,
                origin,
                file,
                line: s.line,
                parent,
                kind: s.kind,
                flags: s.flags & STORED_FLAGS,
                bases,
            });
        }
        let f = &mut self.files[file as usize];
        f.all_len = self.all_names.len() as u32 - f.all_start;
        f.sym_len = self.symbols.len() as u32 - f.sym_start;
    }

    pub(crate) fn finish(self, meta: IndexMeta) -> SymbolIndex {
        SymbolIndex::from_parts(
            meta,
            self.strings,
            self.roots,
            self.files,
            self.all_names,
            self.symbols,
        )
    }
}

impl SymbolIndex {
    /// An index with nothing in it.
    pub fn empty(meta: IndexMeta) -> SymbolIndex {
        IndexBuilder::new(Vec::new()).finish(meta)
    }

    /// Builds the lookup tables over decoded or assembled tables.
    pub(crate) fn from_parts(
        mut meta: IndexMeta,
        mut strings: Strings,
        roots: Vec<Root>,
        mut files: Vec<FileRec>,
        mut all_names: Vec<u32>,
        mut symbols: Vec<SymRec>,
    ) -> SymbolIndex {
        meta.files = files.len() as u64;
        meta.symbols = symbols.len() as u64;
        // An assembled index grew its tables by doubling: up to half of them
        // would be spare capacity, kept for as long as the index lives.
        strings.shrink_to_fit();
        files.shrink_to_fit();
        all_names.shrink_to_fit();
        symbols.shrink_to_fit();
        let mut index = SymbolIndex {
            meta,
            strings,
            roots,
            files,
            all_names,
            symbols,
            names: Vec::new(),
            by_name: Vec::new(),
            by_module: FxHashMap::default(),
            stub_by_module: FxHashMap::default(),
            star_importers: FxHashMap::default(),
            file_top: Vec::new(),
            def_kind: Vec::new(),
        };
        index.derive();
        index
    }

    fn derive(&mut self) {
        // Modules: the first file wins (roots are in `sys.path` order, and
        // `x.py` sorts before `x.pyi`).
        let mut tops: FxHashMap<&str, u32> = FxHashMap::default();
        let mut file_top = Vec::with_capacity(self.files.len());
        for (i, f) in self.files.iter().enumerate() {
            let module = self.strings.get(f.module);
            if !module.is_empty() {
                if self.strings.get(f.rel).ends_with(".pyi")
                    && !self.stub_by_module.contains_key(module)
                {
                    self.stub_by_module.insert(module.into(), i as u32);
                }
                if !self.by_module.contains_key(module) {
                    self.by_module.insert(module.into(), i as u32);
                }
            }
            let top = module.split('.').next().unwrap_or("");
            let next = tops.len() as u32;
            file_top.push(*tops.entry(top).or_insert(next));
        }
        self.file_top = file_top;
        self.mark_exports();
        // Names: symbols grouped by name id, in symbol order within a name.
        // (Sorted rather than counted per string id: tables the size of the
        // string table would let a hostile bundle of tiny strings take far
        // more memory than its body.)
        let star = self
            .symbols
            .iter()
            .find(|s| self.strings.get(s.name) == "*")
            .map(|s| s.name);
        let mut by_name: Vec<u32> = (0..self.symbols.len() as u32)
            .filter(|&i| Some(self.symbols[i as usize].name) != star)
            .collect();
        by_name.sort_unstable_by_key(|&i| (self.symbols[i as usize].name, i));
        let distinct = by_name
            .windows(2)
            .filter(|w| self.symbols[w[0] as usize].name != self.symbols[w[1] as usize].name)
            .count()
            + usize::from(!by_name.is_empty());
        self.names = Vec::with_capacity(distinct);
        let mut start = 0;
        while start < by_name.len() {
            let name = self.symbols[by_name[start] as usize].name;
            let mut end = start;
            let mut kinds = 0;
            while end < by_name.len() && self.symbols[by_name[end] as usize].name == name {
                kinds |= kind_bit(self.symbols[by_name[end] as usize].kind);
                end += 1;
            }
            self.names.push(NameRow {
                name,
                mask: char_mask(self.strings.get(name)),
                kinds,
                start: start as u32,
                len: (end - start) as u32,
            });
            start = end;
        }
        by_name.shrink_to_fit();
        self.by_name = by_name;
        // Star imports, for re-exports through `from .core import *`.
        for s in &self.symbols {
            if Some(s.name) != star || s.parent != NONE {
                continue;
            }
            let file = s.file;
            if let Some(target) = self.resolve_import_module(file, self.strings.get(s.detail)) {
                if let Some(&t) = self.by_module.get(target.as_str()) {
                    if t != file {
                        self.star_importers.entry(t).or_default().push(file);
                    }
                }
            }
        }
        self.resolve_import_kinds();
    }

    /// Follows every exported import to what it imports (see `def_kind`).
    fn resolve_import_kinds(&mut self) {
        // Module-level symbols by (file, name), sorted (12 bytes each: less
        // than a hash map, decoding stays within a small multiple of the body).
        let mut top: Vec<(u32, u32, u32)> =
            Vec::with_capacity(self.symbols.iter().filter(|s| s.parent == NONE).count());
        for (i, s) in self.symbols.iter().enumerate() {
            if s.parent == NONE {
                top.push((s.file, s.name, i as u32));
            }
        }
        top.sort_unstable();
        let top = TopLevel(top);
        let mut kinds = vec![KIND_NONE; self.symbols.len()];
        for (i, s) in self.symbols.iter().enumerate() {
            if s.kind == SymbolKind::Import && s.flags & FLAG_EXPORTED != 0 {
                kinds[i] = self.follow_import(i as u32, &top, 0);
            }
        }
        self.def_kind = kinds;
    }

    /// The kind of what import `id` binds: the imported definition's kind,
    /// following re-exports (stubs first, as they declare more precisely).
    fn follow_import(&self, id: u32, top: &TopLevel, depth: u32) -> u8 {
        let s = &self.symbols[id as usize];
        if s.kind != SymbolKind::Import {
            return s.kind.code();
        }
        let origin = self.strings.get(s.origin);
        if origin.is_empty() || origin == "*" {
            return KIND_MODULE;
        }
        let Some(target) = self.resolve_import_module(s.file, self.strings.get(s.detail)) else {
            return KIND_UNRESOLVED;
        };
        if depth < 6 {
            for map in [&self.stub_by_module, &self.by_module] {
                if let Some(t) = map
                    .get(target.as_str())
                    .and_then(|tf| top.get(*tf, s.origin))
                {
                    if t != id {
                        return self.follow_import(t, top, depth + 1);
                    }
                }
            }
        }
        // `from . import submodule` imports a module.
        if self
            .by_module
            .contains_key(format!("{target}.{origin}").as_str())
        {
            return KIND_MODULE;
        }
        KIND_UNRESOLVED
    }

    /// Sets [`FLAG_EXPORTED`] on module-level symbols that are part of their
    /// module's importable API: public definitions (restricted to `__all__`
    /// when it is known exactly) and re-exported imports (listed in
    /// `__all__`, imported `as` the same name, or imported from within the
    /// package by a package `__init__.py`).
    fn mark_exports(&mut self) {
        for fi in 0..self.files.len() {
            let f = self.files[fi].clone();
            let module = self.strings.get(f.module);
            if module.is_empty() || f.sym_len == 0 {
                continue;
            }
            let top = module.split('.').next().unwrap_or("").to_string();
            let rel = self.strings.get(f.rel);
            let init_py = rel == "__init__.py" || rel.ends_with("/__init__.py");
            let exact = f.flags & FILE_ALL_EXACT != 0;
            let all: FxHashSet<&str> = self.all_names
                [f.all_start as usize..(f.all_start + f.all_len) as usize]
                .iter()
                .map(|&n| self.strings.get(n))
                .collect();
            let mut exported: Vec<u32> = Vec::new();
            for si in f.sym_start..f.sym_start + f.sym_len {
                let s = &self.symbols[si as usize];
                if s.parent != NONE {
                    continue;
                }
                let name = self.strings.get(s.name);
                if name.starts_with('_') || name == "*" {
                    continue;
                }
                let yes = if s.kind == SymbolKind::Import {
                    let from = self.strings.get(s.detail);
                    let in_package =
                        from.starts_with('.') || from.split('.').next() == Some(top.as_str());
                    all.contains(name)
                        || s.flags & FLAG_EXPLICIT_REEXPORT != 0
                        || (init_py
                            && !exact
                            && in_package
                            && !self.strings.get(s.origin).is_empty())
                } else {
                    !exact || all.contains(name)
                };
                if yes {
                    exported.push(si);
                }
            }
            for si in exported {
                self.symbols[si as usize].flags |= FLAG_EXPORTED;
            }
        }
    }

    /// The absolute module named by an import's `from` part (`.core`, `..x`,
    /// `pkg.mod`) as seen from `file`.
    fn resolve_import_module(&self, file: u32, from: &str) -> Option<String> {
        let level = from.chars().take_while(|c| *c == '.').count();
        if level == 0 {
            return Some(from.to_string());
        }
        let f = &self.files[file as usize];
        let module = self.strings.get(f.module);
        if module.is_empty() {
            return None;
        }
        let rel = self.strings.get(f.rel);
        let is_init = rel == "__init__.py"
            || rel == "__init__.pyi"
            || rel.ends_with("/__init__.py")
            || rel.ends_with("/__init__.pyi");
        let mut parts: Vec<&str> = module.split('.').collect();
        if !is_init {
            parts.pop();
        }
        for _ in 1..level {
            parts.pop()?;
        }
        let rest = &from[level..];
        if !rest.is_empty() {
            parts.push(rest);
        }
        let out = parts.join(".");
        (!out.is_empty()).then_some(out)
    }

    pub fn meta(&self) -> &IndexMeta {
        &self.meta
    }

    pub fn set_meta(&mut self, meta: IndexMeta) {
        let (files, symbols) = (self.meta.files, self.meta.symbols);
        self.meta = IndexMeta {
            files,
            symbols,
            ..meta
        };
    }

    pub fn roots(&self) -> &[Root] {
        &self.roots
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    /// Number of distinct module names.
    pub fn module_count(&self) -> usize {
        self.by_module.len()
    }

    /// Absolute path of file `i`.
    pub(crate) fn file_path(&self, i: u32) -> PathBuf {
        let f = &self.files[i as usize];
        let mut p = self
            .roots
            .get(f.root as usize)
            .map(|r| r.path.clone())
            .unwrap_or_default();
        for part in self.strings.get(f.rel).split('/') {
            p.push(part);
        }
        p
    }

    /// The file implementing `module`, if indexed.
    pub fn module_path(&self, module: &str) -> Option<PathBuf> {
        self.by_module.get(module).map(|&i| self.file_path(i))
    }

    /// Absolute paths of all files (see [`SearchOptions::exclude_files`]).
    pub fn file_paths(&self) -> FxHashSet<PathBuf> {
        (0..self.files.len() as u32)
            .map(|i| self.file_path(i))
            .collect()
    }

    /// Go to File over the index: source files whose name `score` accepts
    /// (higher is better), best first. Binary extension modules are left out.
    pub fn search_files(
        &self,
        score: &dyn Fn(&str) -> Option<i32>,
        limit: usize,
    ) -> Vec<(PathBuf, i32)> {
        let mut scored: Vec<(i32, u32)> = Vec::new();
        for (i, f) in self.files.iter().enumerate() {
            if f.flags & FILE_EXTENSION != 0 {
                continue;
            }
            let rel = self.strings.get(f.rel);
            let name = rel.rsplit('/').next().unwrap_or(rel);
            if let Some(s) = score(name) {
                scored.push((s, i as u32));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|(s, i)| (self.file_path(i), s))
            .collect()
    }

    /// `(path, module, symbol count)` of every file (diagnostics, tests).
    pub fn files(&self) -> impl Iterator<Item = (PathBuf, &str, usize)> + '_ {
        (0..self.files.len() as u32).map(move |i| {
            let f = &self.files[i as usize];
            (
                self.file_path(i),
                self.strings.get(f.module),
                f.sym_len as usize,
            )
        })
    }

    /// Every declaration (imports excluded), in file order, with score 0.
    pub fn declarations(&self) -> Vec<SymbolHit> {
        (0..self.symbols.len() as u32)
            .filter(|&i| self.symbols[i as usize].kind.is_definition())
            .map(|i| self.hit(i, 0))
            .collect()
    }

    /// Module-level declarations of the given kinds, in file order, with
    /// score 0. Symbols are filtered before hits are built (each hit owns
    /// its strings and path), and each file's path is built once.
    pub fn top_level_declarations(&self, kinds: &[SymbolKind]) -> Vec<SymbolHit> {
        let mut out = Vec::new();
        let mut path: Option<(u32, PathBuf)> = None;
        for s in &self.symbols {
            if s.parent != NONE || !kinds.contains(&s.kind) {
                continue;
            }
            let f = &self.files[s.file as usize];
            let file_path = match &path {
                Some((file, p)) if *file == s.file => p.clone(),
                _ => {
                    let p = self.file_path(s.file);
                    path = Some((s.file, p.clone()));
                    p
                }
            };
            out.push(SymbolHit {
                name: self.strings.get(s.name).to_string(),
                kind: s.kind,
                flags: s.flags & STORED_FLAGS,
                detail: self.strings.get(s.detail).to_string(),
                container: String::new(),
                module: self.strings.get(f.module).to_string(),
                path: file_path,
                line: s.line,
                score: 0,
                root_kind: self
                    .roots
                    .get(f.root as usize)
                    .map_or(RootKind::Other, |r| r.kind),
            });
        }
        out
    }

    /// Ids of the module-level declarations of the given kinds, in file
    /// order: what [`SymbolIndex::top_level_declarations`] returns, as four
    /// bytes per symbol to keep and resolve with [`SymbolIndex::declaration`]
    /// when needed.
    pub fn top_level_declaration_ids(&self, kinds: &[SymbolKind]) -> Vec<u32> {
        (0..self.symbols.len() as u32)
            .filter(|&i| {
                let s = &self.symbols[i as usize];
                s.parent == NONE && kinds.contains(&s.kind)
            })
            .collect()
    }

    /// Symbol `id` as a hit with score 0 (`None` when there is no such symbol).
    pub fn declaration(&self, id: u32) -> Option<SymbolHit> {
        ((id as usize) < self.symbols.len()).then(|| self.hit(id, 0))
    }

    /// Per file, the classes with their declared bases and the module-level
    /// names that may alias a class (hierarchy queries: which files can
    /// declare a subclass of a given class without resolving every base).
    pub fn class_bases(&self) -> Vec<FileBases> {
        let mut out = Vec::new();
        for (i, f) in self.files.iter().enumerate() {
            if f.flags & FILE_EXTENSION != 0 {
                continue;
            }
            let mut file = FileBases {
                path: self.file_path(i as u32),
                hash: f.hash,
                classes: Vec::new(),
                imports: Vec::new(),
                variables: Vec::new(),
            };
            for s in &self.symbols[f.sym_start as usize..(f.sym_start + f.sym_len) as usize] {
                let name = self.strings.get(s.name);
                match s.kind {
                    SymbolKind::Class => {
                        let bases = self.strings.get(s.bases);
                        file.classes.push(ClassBases {
                            name: name.to_string(),
                            line: s.line,
                            bases: if bases.is_empty() {
                                Vec::new()
                            } else {
                                bases.split(',').map(str::to_string).collect()
                            },
                        });
                    }
                    SymbolKind::Import if s.parent == NONE => {
                        let origin = self.strings.get(s.origin);
                        if !origin.is_empty() && origin != name {
                            file.imports.push((name.to_string(), origin.to_string()));
                        }
                    }
                    SymbolKind::Variable | SymbolKind::Constant if s.parent == NONE => {
                        file.variables.push(name.to_string());
                    }
                    _ => {}
                }
            }
            out.push(file);
        }
        out
    }

    /// Symbols declared in the file at `path` (tests, diagnostics).
    pub fn symbols_of(&self, path: &Path) -> Vec<(String, SymbolKind, u32)> {
        let Some(i) = (0..self.files.len() as u32).find(|&i| self.file_path(i) == path) else {
            return Vec::new();
        };
        let f = &self.files[i as usize];
        self.symbols[f.sym_start as usize..(f.sym_start + f.sym_len) as usize]
            .iter()
            .map(|s| (self.strings.get(s.name).to_string(), s.kind, s.line))
            .collect()
    }

    fn container(&self, mut parent: u32) -> String {
        let mut parts: Vec<&str> = Vec::new();
        while parent != NONE && parts.len() < 16 {
            let p = &self.symbols[parent as usize];
            parts.push(self.strings.get(p.name));
            parent = p.parent;
        }
        parts.reverse();
        parts.join(".")
    }

    fn hit(&self, id: u32, score: i32) -> SymbolHit {
        let s = &self.symbols[id as usize];
        let f = &self.files[s.file as usize];
        SymbolHit {
            name: self.strings.get(s.name).to_string(),
            kind: s.kind,
            flags: s.flags & STORED_FLAGS,
            detail: self.strings.get(s.detail).to_string(),
            container: self.container(s.parent),
            module: self.strings.get(f.module).to_string(),
            path: self.file_path(s.file),
            line: s.line,
            score,
            root_kind: self
                .roots
                .get(f.root as usize)
                .map_or(RootKind::Other, |r| r.kind),
        }
    }

    /// Orders symbols sharing a name: project before stdlib before packages,
    /// public modules before private ones, shallow before deep, sources before stubs.
    fn rank_key(&self, id: u32) -> (RootKind, bool, usize, bool, u32) {
        let s = &self.symbols[id as usize];
        let f = &self.files[s.file as usize];
        let module = self.strings.get(f.module);
        let stub = self.strings.get(f.rel).ends_with(".pyi");
        (
            self.roots
                .get(f.root as usize)
                .map_or(RootKind::Other, |r| r.kind),
            is_private_module(module),
            module.matches('.').count(),
            stub,
            s.file,
        )
    }

    /// Go to Symbol / Go to Class: definitions whose name matches `query`
    /// according to `score` (higher is better, `None` = no match), best
    /// first. `Class.meth` and `module.name` queries match the part after
    /// the last dot against names and the rest against the container or
    /// module. Dunder methods only match queries starting with `__`.
    pub fn search(
        &self,
        query: &str,
        opts: &SearchOptions,
        score: &dyn Fn(&str, &str) -> Option<i32>,
    ) -> Vec<SymbolHit> {
        let query = query.trim();
        let (qualifier, q) = match query.rsplit_once('.') {
            Some((a, b)) if !b.is_empty() => (Some(a.trim_matches('.').to_lowercase()), b),
            _ => (None, query.trim_matches('.')),
        };
        if q.is_empty() || opts.limit == 0 {
            return Vec::new();
        }
        let qmask = char_mask(q);
        let wanted: u8 = if opts.classes_only {
            kind_bit(SymbolKind::Class)
        } else {
            !kind_bit(SymbolKind::Import)
        };
        let dunder_query = q.starts_with("__");
        let mut scored: Vec<(i32, u32)> = Vec::new();
        for (i, row) in self.names.iter().enumerate() {
            if row.mask & qmask != qmask || row.kinds & wanted == 0 {
                continue;
            }
            let name = self.strings.get(row.name);
            if !dunder_query && is_dunder(name) {
                continue;
            }
            if let Some(s) = score(name, q) {
                scored.push((s, i as u32));
            }
        }
        let order = |a: &(i32, u32), b: &(i32, u32)| {
            let (na, nb) = (
                self.strings.get(self.names[a.1 as usize].name),
                self.strings.get(self.names[b.1 as usize].name),
            );
            b.0.cmp(&a.0)
                .then(na.len().cmp(&nb.len()))
                .then_with(|| na.cmp(nb))
        };
        // Only the best names can make it into the result.
        let keep = (opts.limit * 8).max(400);
        if scored.len() > keep {
            scored.select_nth_unstable_by(keep, order);
            scored.truncate(keep);
        }
        scored.sort_unstable_by(order);
        let mut hits: Vec<SymbolHit> = Vec::new();
        let mut seen: FxHashSet<(String, String, String)> = FxHashSet::default();
        'names: for (s, r) in scored {
            let row = &self.names[r as usize];
            let mut ids: Vec<u32> = self.by_name
                [row.start as usize..(row.start + row.len) as usize]
                .iter()
                .copied()
                .filter(|&id| wanted & kind_bit(self.symbols[id as usize].kind) != 0)
                .collect();
            ids.sort_by_cached_key(|&id| self.rank_key(id));
            let mut taken = 0;
            for id in ids {
                let sym = &self.symbols[id as usize];
                if let Some(excluded) = &opts.exclude_files {
                    if excluded.contains(&self.file_path(sym.file)) {
                        continue;
                    }
                }
                let hit = self.hit(id, s);
                if let Some(qual) = &qualifier {
                    if !hit.container.to_lowercase().contains(qual.as_str())
                        && !hit.module.to_lowercase().contains(qual.as_str())
                    {
                        continue;
                    }
                }
                // A stub next to its source declares the same thing again.
                if !hit.module.is_empty()
                    && !seen.insert((hit.module.clone(), hit.container.clone(), hit.name.clone()))
                {
                    continue;
                }
                hits.push(hit);
                taken += 1;
                if hits.len() >= opts.limit {
                    break 'names;
                }
                if taken >= opts.per_name {
                    break;
                }
            }
        }
        hits
    }

    /// Second-press completion: importable names matching the typed prefix.
    /// `lower_prefix` is the prefix in lower case: names starting with it are
    /// collected first; only when there are fewer than `limit` of them are
    /// other names that `matches` accepts added (names lacking any of the
    /// prefix's characters are skipped without calling it). A name is offered
    /// from the best module that exports it ([`Rank`]), once per top-level
    /// package (`pandas.DataFrame`, not also `pandas.core.frame.DataFrame`),
    /// and from private modules only when no public one has it.
    pub fn import_candidates(
        &self,
        lower_prefix: &str,
        matches: &dyn Fn(&str) -> bool,
        limit: usize,
    ) -> Vec<ImportCandidate> {
        self.import_candidates_kind(lower_prefix, matches, limit, None)
    }
    /// Public class exports for basic completion, filtered before ranking/capping.
    pub fn class_import_candidates(
        &self,
        lower_prefix: &str,
        matches: &dyn Fn(&str) -> bool,
        limit: usize,
    ) -> Vec<ImportCandidate> {
        self.import_candidates_kind(lower_prefix, matches, limit, Some(SymbolKind::Class))
    }
    fn import_candidates_kind(
        &self,
        lower_prefix: &str,
        matches: &dyn Fn(&str) -> bool,
        limit: usize,
        wanted: Option<SymbolKind>,
    ) -> Vec<ImportCandidate> {
        if lower_prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        let qmask = char_mask(lower_prefix);
        let mut offers = Offers::new();
        for pass in 0..2 {
            for (r, row) in self.names.iter().enumerate() {
                if row.mask & qmask != qmask || row.kinds & !kind_bit(SymbolKind::Method) == 0 {
                    continue;
                }
                let name = self.strings.get(row.name);
                if name.starts_with('_') {
                    continue;
                }
                let prefix = starts_with_lower(name, lower_prefix);
                if (pass == 0) != prefix || (!prefix && !matches(name)) {
                    continue;
                }
                self.offer_row_kind(r as u32, prefix, &mut offers, wanted);
            }
            if offers.best.len() >= limit {
                break;
            }
        }
        self.finish_offers(offers.best, limit)
    }

    /// Modules to import exactly `name` from (the "Import" quick fix for an
    /// unresolved reference), best first: one per top-level package.
    pub fn exact_import_candidates(&self, name: &str, limit: usize) -> Vec<ImportCandidate> {
        if name.is_empty() || name.starts_with('_') || limit == 0 {
            return Vec::new();
        }
        let mask = char_mask(name);
        let mut offers = Offers::new();
        if let Some(r) = self
            .names
            .iter()
            .position(|row| row.mask == mask && self.strings.get(row.name) == name)
        {
            self.offer_row(r as u32, true, &mut offers);
        }
        self.finish_offers(offers.best, limit)
    }

    /// Offers every exported module-level symbol of name row `row`.
    fn offer_row(&self, row: u32, prefix: bool, offers: &mut Offers) {
        self.offer_row_kind(row, prefix, offers, None)
    }
    fn offer_row_kind(
        &self,
        row: u32,
        prefix: bool,
        offers: &mut Offers,
        wanted: Option<SymbolKind>,
    ) {
        let r = &self.names[row as usize];
        for &id in &self.by_name[r.start as usize..(r.start + r.len) as usize] {
            let s = &self.symbols[id as usize];
            if s.flags & FLAG_EXPORTED == 0 || s.parent != NONE {
                continue;
            }
            let (kind, resolved) = match s.kind {
                SymbolKind::Import => {
                    match self.def_kind.get(id as usize).copied().unwrap_or(KIND_NONE) {
                        KIND_UNRESOLVED => (SymbolKind::Variable, false),
                        code => match SymbolKind::from_code(code) {
                            Some(k) => (k, true),
                            None => continue,
                        },
                    }
                }
                k => (k, true),
            };
            if wanted.is_some_and(|wanted| wanted != kind) {
                continue;
            }
            self.offer(s.file, row, kind, resolved, prefix, offers);
        }
    }

    /// Orders the offers (prefix matches, then [`Rank`], then name) and drops
    /// private modules for names that a public module offers too. Names are
    /// lower-cased once per name rather than per comparison, and only the
    /// best `limit` offers are sorted (a broad query has tens of thousands).
    fn finish_offers(
        &self,
        best: FxHashMap<(u32, u32), Offer>,
        limit: usize,
    ) -> Vec<ImportCandidate> {
        let public: FxHashSet<u32> = best
            .iter()
            .filter(|(_, o)| !o.rank.0)
            .map(|((r, _), _)| *r)
            .collect();
        let mut out: Vec<(Offer, u32)> = best
            .into_iter()
            .filter(|((r, _), o)| !o.rank.0 || !public.contains(r))
            .map(|((r, _), o)| (o, r))
            .collect();
        let name_of = |r: u32| self.strings.get(self.names[r as usize].name);
        let module_of = |o: &Offer| self.strings.get(self.files[o.file as usize].module);
        let mut lower: FxHashMap<u32, String> = FxHashMap::default();
        for (_, r) in &out {
            lower
                .entry(*r)
                .or_insert_with(|| name_of(*r).to_lowercase());
        }
        // `Foo` before `foo` in one module, then the row: the order is total.
        let order = |(a, ra): &(Offer, u32), (b, rb): &(Offer, u32)| {
            b.prefix
                .cmp(&a.prefix)
                .then(a.rank.cmp(&b.rank))
                .then_with(|| lower[ra].cmp(&lower[rb]))
                .then_with(|| module_of(a).cmp(module_of(b)))
                .then_with(|| name_of(*ra).cmp(name_of(*rb)))
                .then(ra.cmp(rb))
        };
        if out.len() > limit {
            out.select_nth_unstable_by(limit, order);
            out.truncate(limit);
        }
        out.sort_unstable_by(order);
        out.into_iter()
            .map(|(o, r)| ImportCandidate {
                module: module_of(&o).to_string(),
                name: name_of(r).to_string(),
                kind: o.kind,
            })
            .collect()
    }

    /// Records name row `row` as importable from the module of `file`, and
    /// from the modules that star-import it (breadth first, each module
    /// once, at most [`STAR_DEPTH`] imports away, from at most
    /// [`MAX_STAR_OFFERS`] other packages and within the query's budget:
    /// dense star-import graphs cost no more than their size).
    fn offer(
        &self,
        file: u32,
        row: u32,
        kind: SymbolKind,
        resolved: bool,
        prefix: bool,
        offers: &mut Offers,
    ) {
        let name = self.names[row as usize].name;
        let mut level = vec![file];
        let mut visited: FxHashSet<u32> = FxHashSet::default();
        // Packages the definition was newly offered from through star imports.
        let mut spread = 0;
        for depth in 0..=STAR_DEPTH {
            let mut next = Vec::new();
            for &f in &level {
                let external = self.file_top[f as usize] != self.file_top[file as usize];
                // The external offer cap must not stop traversal: a better
                // public re-export in the defining package may still be queued.
                let added = if external
                    && spread >= MAX_STAR_OFFERS
                    && !offers.best.contains_key(&(row, self.file_top[f as usize]))
                {
                    false
                } else {
                    let Some(added) =
                        self.offer_one(f, row, kind, resolved, prefix, &mut offers.best)
                    else {
                        continue;
                    };
                    added
                };
                if depth > 0 && external && added {
                    spread += 1;
                }
                if depth == STAR_DEPTH {
                    continue;
                }
                let Some(importers) = self.star_importers.get(&f) else {
                    continue;
                };
                for &p in importers {
                    let pf = &self.files[p as usize];
                    let exact = pf.flags & FILE_ALL_EXACT != 0;
                    let cost = 1 + if exact { pf.all_len as usize } else { 0 };
                    if offers.budget < cost {
                        offers.budget = 0;
                        break;
                    }
                    offers.budget -= cost;
                    let listed = || {
                        self.all_names[pf.all_start as usize..(pf.all_start + pf.all_len) as usize]
                            .contains(&name)
                    };
                    if p != file && (!exact || listed()) && visited.insert(p) {
                        next.push(p);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            level = next;
        }
    }

    /// Records name row `row` as importable from the module of `file`:
    /// `Some(true)` when no module of that top-level package offered it
    /// before, `None` when that module cannot be imported from.
    fn offer_one(
        &self,
        file: u32,
        row: u32,
        kind: SymbolKind,
        resolved: bool,
        prefix: bool,
        best: &mut FxHashMap<(u32, u32), Offer>,
    ) -> Option<bool> {
        let f = &self.files[file as usize];
        let module = self.strings.get(f.module);
        if module.is_empty() || module == "__main__" || module.ends_with(".__main__") {
            return None;
        }
        let rank: Rank = (
            is_private_module(module),
            module.matches('.').count(),
            !resolved,
            self.roots
                .get(f.root as usize)
                .map_or(RootKind::Other, |r| r.kind),
            module.len(),
        );
        let offer = Offer {
            rank,
            file,
            kind,
            prefix,
        };
        match best.entry((row, self.file_top[file as usize])) {
            Entry::Occupied(mut e) => {
                if rank < e.get().rank {
                    e.insert(offer);
                }
                Some(false)
            }
            Entry::Vacant(e) => {
                e.insert(offer);
                Some(true)
            }
        }
    }

    /// Content hash -> file, for files with recorded symbols (reuse when building).
    pub(crate) fn files_by_hash(&self) -> FxHashMap<ContentHash, u32> {
        let mut map = FxHashMap::default();
        for (i, f) in self.files.iter().enumerate() {
            if !f.hash.is_none() && f.flags & FILE_EXTENSION == 0 {
                map.entry(f.hash).or_insert(i as u32);
            }
        }
        map
    }

    /// A copy restricted to files below roots of the given kinds, as a shared
    /// bundle: roots are renamed (`<stdlib>`, `<site-packages>`) so that no
    /// local path leaves the machine, and the editable-install hooks in
    /// site-packages, which name the user's own projects, are left out (see
    /// [`is_editable_install_hook`]).
    pub fn to_bundle(&self, kinds: &[RootKind], meta: IndexMeta) -> SymbolIndex {
        let mut roots: Vec<Root> = Vec::new();
        let mut map: Vec<Option<u32>> = vec![None; self.roots.len()];
        for (i, r) in self.roots.iter().enumerate() {
            if kinds.contains(&r.kind) {
                map[i] = Some(roots.len() as u32);
                roots.push(Root {
                    path: PathBuf::from(format!("<{}-{}>", r.kind.label(), roots.len())),
                    kind: r.kind,
                });
            }
        }
        let mut b = IndexBuilder::new(roots);
        for (i, f) in self.files.iter().enumerate() {
            let Some(root) = map.get(f.root as usize).copied().flatten() else {
                continue;
            };
            if f.hash.is_none()
                || f.flags & FILE_EXTENSION != 0
                || is_editable_install_hook(self.strings.get(f.rel))
            {
                continue;
            }
            let st = FileStamp {
                root,
                rel: self.strings.get(f.rel),
                module: self.strings.get(f.module),
                size: f.size,
                mtime_ns: 0,
                hash: f.hash,
            };
            b.add_copy(&st, self, i as u32);
        }
        b.finish(IndexMeta {
            kind: IndexKind::Bundle,
            ..meta
        })
    }

    /// Whether two indexes record the same files and symbols (tests, round trips).
    pub fn same_content(&self, other: &SymbolIndex) -> bool {
        if self.roots != other.roots
            || self.files.len() != other.files.len()
            || self.symbols.len() != other.symbols.len()
        {
            return false;
        }
        let file_eq = self.files.iter().zip(&other.files).all(|(a, b)| {
            a.root == b.root
                && self.strings.get(a.rel) == other.strings.get(b.rel)
                && self.strings.get(a.module) == other.strings.get(b.module)
                && (
                    a.size,
                    a.mtime_ns,
                    a.hash,
                    a.flags & FILE_STORED_FLAGS,
                    a.sym_start,
                    a.sym_len,
                    a.all_len,
                ) == (
                    b.size,
                    b.mtime_ns,
                    b.hash,
                    b.flags & FILE_STORED_FLAGS,
                    b.sym_start,
                    b.sym_len,
                    b.all_len,
                )
                && self.all_names[a.all_start as usize..(a.all_start + a.all_len) as usize]
                    .iter()
                    .zip(&other.all_names[b.all_start as usize..(b.all_start + b.all_len) as usize])
                    .all(|(x, y)| self.strings.get(*x) == other.strings.get(*y))
        });
        let sym_eq = self.symbols.iter().zip(&other.symbols).all(|(a, b)| {
            self.strings.get(a.name) == other.strings.get(b.name)
                && self.strings.get(a.detail) == other.strings.get(b.detail)
                && self.strings.get(a.origin) == other.strings.get(b.origin)
                && self.strings.get(a.bases) == other.strings.get(b.bases)
                && (a.file, a.line, a.parent, a.kind, a.flags)
                    == (b.file, b.line, b.parent, b.kind, b.flags)
        });
        file_eq && sym_eq
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::extract::extract;

    /// Builds an index from `(root, rel path, source)` files under `/r<root>`.
    pub(crate) fn index_of(roots: &[RootKind], files: &[(u32, &str, &str)]) -> SymbolIndex {
        let roots: Vec<Root> = roots
            .iter()
            .enumerate()
            .map(|(i, k)| Root {
                path: PathBuf::from(format!("/r{i}")),
                kind: *k,
            })
            .collect();
        let mut b = IndexBuilder::new(roots);
        for (root, rel, src) in files {
            let module = crate::build::module_name(rel);
            let st = FileStamp {
                root: *root,
                rel,
                module: &module,
                size: src.len() as u64,
                mtime_ns: 1,
                hash: ContentHash::of(src.as_bytes()),
            };
            b.add_file(&st, Some(&extract(src)), false);
        }
        b.finish(IndexMeta::default())
    }

    /// Simple scorer for tests: case-insensitive prefix (100) or substring (50).
    pub(crate) fn score(name: &str, q: &str) -> Option<i32> {
        let (n, q) = (name.to_lowercase(), q.to_lowercase());
        if n == q {
            Some(200)
        } else if n.starts_with(&q) {
            Some(100 - n.len() as i32)
        } else if n.contains(&q) {
            Some(50 - n.len() as i32)
        } else {
            None
        }
    }

    fn names(hits: &[SymbolHit]) -> Vec<String> {
        hits.iter()
            .map(|h| {
                if h.container.is_empty() {
                    format!("{}:{}", h.module, h.name)
                } else {
                    format!("{}:{}.{}", h.module, h.container, h.name)
                }
            })
            .collect()
    }

    #[test]
    fn search_ranks_and_filters() {
        let idx = index_of(
            &[RootKind::Stdlib, RootKind::Site],
            &[
                (0, "json/decoder.py", "class JSONDecoder(object):\n    def decode(self, s):\n        pass\n    def __init__(self):\n        pass\n\ndef decode_hook(x):\n    pass\n"),
                (0, "json/__init__.py", "from .decoder import JSONDecoder\n__all__ = ['JSONDecoder', 'loads']\ndef loads(s):\n    pass\n"),
                (1, "pkg/_impl.py", "class Decoder:\n    pass\n"),
                (1, "pkg/api.py", "class Decoder:\n    pass\n"),
            ],
        );
        let all = idx.search("decod", &SearchOptions::default(), &score);
        // Better matches first (prefix before substring, shorter first); per name, public modules first.
        assert_eq!(
            names(&all),
            [
                "json.decoder:JSONDecoder.decode",
                "pkg.api:Decoder",
                "pkg._impl:Decoder",
                "json.decoder:decode_hook",
                "json.decoder:JSONDecoder"
            ]
        );
        assert!(
            !all.iter().any(|h| h.kind == SymbolKind::Import),
            "imports are not declarations"
        );
        // Same name: the public module before the private one.
        let pos = |n: &str| names(&all).iter().position(|x| x == n).unwrap();
        assert!(pos("pkg.api:Decoder") < pos("pkg._impl:Decoder"));
        let classes = idx.search(
            "decoder",
            &SearchOptions {
                classes_only: true,
                ..Default::default()
            },
            &score,
        );
        assert!(classes.iter().all(|h| h.kind == SymbolKind::Class));
        assert_eq!(classes.len(), 3);
        // Dunder methods only for dunder queries.
        assert!(idx
            .search("init", &SearchOptions::default(), &score)
            .is_empty());
        assert_eq!(
            names(&idx.search("__init__", &SearchOptions::default(), &score)),
            vec!["json.decoder:JSONDecoder.__init__".to_string()]
        );
        // Qualified queries.
        assert_eq!(
            names(&idx.search("JSONDecoder.dec", &SearchOptions::default(), &score)),
            vec!["json.decoder:JSONDecoder.decode".to_string()]
        );
        assert_eq!(
            names(&idx.search("api.Decoder", &SearchOptions::default(), &score)),
            vec!["pkg.api:Decoder".to_string()]
        );
        // Limits.
        assert_eq!(
            idx.search(
                "decod",
                &SearchOptions {
                    limit: 2,
                    ..Default::default()
                },
                &score
            )
            .len(),
            2
        );
        let hit = &idx.search(
            "JSONDecoder",
            &SearchOptions {
                classes_only: true,
                ..Default::default()
            },
            &score,
        )[0];
        assert_eq!(
            (
                hit.path.clone(),
                hit.line,
                hit.root_kind,
                hit.detail.as_str()
            ),
            (
                PathBuf::from("/r0/json/decoder.py"),
                0,
                RootKind::Stdlib,
                "(object)"
            )
        );
        // Leaving out files (those of the project index).
        let excluded: FxHashSet<PathBuf> = [
            PathBuf::from("/r1/pkg/api.py"),
            PathBuf::from("/r1/pkg/_impl.py"),
        ]
        .into_iter()
        .collect();
        let skipped = idx.search(
            "Decoder",
            &SearchOptions {
                classes_only: true,
                exclude_files: Some(Arc::new(excluded)),
                ..Default::default()
            },
            &score,
        );
        assert_eq!(
            names(&skipped),
            vec!["json.decoder:JSONDecoder".to_string()]
        );
        assert_eq!(idx.file_paths().len(), 4);
        assert!(idx.file_paths().contains(Path::new("/r0/json/decoder.py")));
        // Files by name.
        let files = idx.search_files(&|name: &str| score(name, "dec"), 10);
        assert_eq!(
            files.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            vec![PathBuf::from("/r0/json/decoder.py")]
        );
        assert_eq!(
            idx.search_files(&|name: &str| score(name, "py"), 2).len(),
            2
        );
    }

    #[test]
    fn stubs_next_to_sources_are_not_listed_twice() {
        let idx = index_of(
            &[RootKind::Site],
            &[
                (0, "m.py", "def f(x):\n    pass\n"),
                (0, "m.pyi", "def f(x: int) -> int: ...\n"),
            ],
        );
        let hits = idx.search("f", &SearchOptions::default(), &score);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("m.py"));
    }

    #[test]
    fn import_candidates_prefer_public_reexports() {
        let idx = index_of(
            &[RootKind::Site],
            &[
                (0, "frames/__init__.py", "from frames.core.frame import DataFrame\nfrom .core.series import Series\nfrom .core import helpers\n__version__ = '1'\n"),
                (0, "frames/core/__init__.py", ""),
                (0, "frames/core/frame.py", "class DataFrame:\n    def head(self):\n        pass\n"),
                (0, "frames/core/series.py", "class Series:\n    pass\n"),
                (0, "frames/core/helpers.py", "def data_helper():\n    pass\n"),
                (0, "other/__init__.py", "from ._frame import *\n"),
                (0, "other/_frame.py", "__all__ = ['DataFrame']\nclass DataFrame:\n    pass\nclass Hidden:\n    pass\n"),
                (0, "_private_only.py", "def datafy():\n    pass\n"),
                (0, "loose.py", "from os import path as path_mod\nfrom frames import DataFrame\nimport json as json\n"),
            ],
        );
        let lower = "data";
        let found = idx.import_candidates(lower, &|n: &str| n.to_lowercase().contains(lower), 50);
        let pairs: Vec<(String, String, SymbolKind)> = found
            .iter()
            .map(|c| (c.module.clone(), c.name.clone(), c.kind))
            .collect();
        assert!(
            pairs.contains(&("frames".into(), "DataFrame".into(), SymbolKind::Class)),
            "{pairs:?}"
        );
        assert!(
            !pairs
                .iter()
                .any(|(m, n, _)| m == "frames.core.frame" && n == "DataFrame"),
            "the shallowest public module wins: {pairs:?}"
        );
        assert!(
            pairs.contains(&("other".into(), "DataFrame".into(), SymbolKind::Class)),
            "re-exported through a star import: {pairs:?}"
        );
        assert!(pairs.contains(&(
            "frames.core.helpers".into(),
            "data_helper".into(),
            SymbolKind::Function
        )));
        assert!(
            pairs.contains(&(
                "_private_only".into(),
                "datafy".into(),
                SymbolKind::Function
            )),
            "a private module when nothing public has the name"
        );
        assert!(
            !pairs.iter().any(|(m, _, _)| m == "loose"),
            "plain imports in a module are not re-exports: {pairs:?}"
        );
        // Exact prefix matches come first.
        assert!(
            found
                .iter()
                .take_while(|c| c.name.to_lowercase().starts_with(lower))
                .count()
                >= 3
        );
        let hidden =
            idx.import_candidates("hidden", &|n: &str| n.to_lowercase().contains("hidden"), 50);
        assert!(hidden.is_empty(), "not in its module's exact __all__, so not re-exported by the star import either: {hidden:?}");
        // `from . import helpers` re-exports a module, which is not a name candidate.
        assert!(idx
            .import_candidates("helpers", &|n: &str| n == "helpers", 50)
            .is_empty());
        let series =
            idx.import_candidates("ser", &|n: &str| n.to_lowercase().starts_with("ser"), 50);
        assert_eq!(
            series,
            vec![ImportCandidate {
                module: "frames".into(),
                name: "Series".into(),
                kind: SymbolKind::Class
            }]
        );
        // Explicit re-exports and `__all__` in plain modules.
        let idx2 = index_of(
            &[RootKind::Site],
            &[
                (
                    0,
                    "a.py",
                    "from b import Thing as Thing\nfrom c import Other\n",
                ),
                (0, "b.py", "class Thing: pass\n"),
                (0, "c.py", "Other = 1\n"),
                (0, "d.py", "__all__ = ['Other']\nfrom c import Other\n"),
            ],
        );
        let got = idx2.import_candidates("t", &|n: &str| n.to_lowercase().contains('t'), 50);
        let pairs: Vec<(String, String)> = got
            .iter()
            .map(|c| (c.module.clone(), c.name.clone()))
            .collect();
        assert!(pairs.contains(&("a".into(), "Thing".into())), "{pairs:?}");
        assert!(pairs.contains(&("d".into(), "Other".into())), "{pairs:?}");
        assert!(!pairs.contains(&("a".into(), "Other".into())), "{pairs:?}");
    }

    #[test]
    fn exact_import_candidates_for_quick_fixes() {
        let idx = index_of(
            &[RootKind::Stdlib, RootKind::Site],
            &[
                (0, "pathlib/__init__.py", "from ._local import Path\n__all__ = ['Path', 'PurePath']\nclass PurePath: pass\n"),
                (0, "pathlib/_local.py", "class Path: pass\nclass PathLike: pass\n"),
                (1, "fsx/__init__.py", "from .core import Path as Path\n"),
                (1, "fsx/core.py", "class Path: pass\n"),
                (1, "other.py", "def Paths(): pass\nclass Thing:\n    Path = 1\n"),
            ],
        );
        let got: Vec<(String, SymbolKind)> = idx
            .exact_import_candidates("Path", 10)
            .into_iter()
            .map(|c| (c.module, c.kind))
            .collect();
        // Exactly that name (not `PathLike`, `Paths` or the class attribute), the
        // shallowest public module per package, stdlib before site-packages.
        assert_eq!(
            got,
            vec![
                ("pathlib".to_string(), SymbolKind::Class),
                ("fsx".to_string(), SymbolKind::Class)
            ]
        );
        assert!(idx.exact_import_candidates("Missing", 10).is_empty());
        assert!(idx.exact_import_candidates("_local", 10).is_empty());
        assert_eq!(idx.exact_import_candidates("Path", 1).len(), 1);
    }

    #[test]
    fn every_star_import_of_a_package_reexports() {
        let idx = index_of(
            &[RootKind::Stdlib],
            &[
                (0, "aio/__init__.py", "from .base import *\nfrom .events import *\nfrom .unix import *\n"),
                (0, "aio/base.py", "__all__ = ['BaseLoop']\nclass BaseLoop: pass\n"),
                (0, "aio/events.py", "__all__ = ['EventPolicy']\nclass EventPolicy: pass\n"),
                (0, "aio/unix.py", "__all__ = ['DefaultPolicy']\nclass _UnixPolicy: pass\nDefaultPolicy = _UnixPolicy\n"),
            ],
        );
        for name in ["BaseLoop", "EventPolicy", "DefaultPolicy"] {
            let got = idx.exact_import_candidates(name, 5);
            assert_eq!(
                got.iter().map(|c| c.module.as_str()).collect::<Vec<_>>(),
                vec!["aio"],
                "{name}"
            );
        }
    }

    /// `count` modules of package `pkg`, each star-importing all the others
    /// and defining `defs` functions `alpha_<module>_<n>`.
    fn star_web(count: usize, defs: usize) -> SymbolIndex {
        let sources: Vec<(String, String)> = (0..count)
            .map(|i| {
                let mut src: String = (0..count)
                    .filter(|k| *k != i)
                    .map(|k| format!("from .m{k} import *\n"))
                    .collect();
                for n in 0..defs {
                    src.push_str(&format!("def alpha_{i}_{n}(): pass\n"));
                }
                (format!("pkg/m{i}.py"), src)
            })
            .collect();
        let mut files: Vec<(u32, &str, &str)> = vec![(0, "pkg/__init__.py", "")];
        files.extend(
            sources
                .iter()
                .map(|(rel, src)| (0, rel.as_str(), src.as_str())),
        );
        index_of(&[RootKind::Site], &files)
    }

    #[test]
    fn dense_star_imports_cost_their_size_not_a_power_of_it() {
        // 40 modules star-importing each other: following every path three
        // imports deep took 14 s for one query (on the UI thread).
        let idx = star_web(40, 25);
        let started = std::time::Instant::now();
        let found = idx.import_candidates("alpha", &|n: &str| n.starts_with("alpha"), 2000);
        assert_eq!(found.len(), 1000);
        // One offer per name and package: every module re-exports every
        // name, the shortest module name wins (the defining one on ties).
        let module_of = |name: &str| {
            found
                .iter()
                .find(|c| c.name == name)
                .map(|c| (c.module.clone(), c.kind))
        };
        assert_eq!(
            module_of("alpha_7_3"),
            Some(("pkg.m7".to_string(), SymbolKind::Function))
        );
        assert_eq!(
            module_of("alpha_17_3"),
            Some(("pkg.m0".to_string(), SymbolKind::Function))
        );
        assert_eq!(idx.exact_import_candidates("alpha_39_24", 5).len(), 1);
        // A much denser web: the work per query is bounded.
        let dense = star_web(300, 4);
        let found = dense.import_candidates("alpha", &|n: &str| n.starts_with("alpha"), 2000);
        assert_eq!(
            found.len(),
            1200,
            "every name is still offered (at least from its own module)"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    /// Module `root` defining `count` functions `alpha_<n>`, star-imported by
    /// a tree of plain modules (each a top-level module of its own), `fan`
    /// importers per module, three levels deep.
    fn star_tree(count: usize, fan: usize) -> SymbolIndex {
        let mut sources: Vec<(String, String)> = vec![(
            "root.py".to_string(),
            (0..count)
                .map(|n| format!("def alpha_{n}(): pass\n"))
                .collect(),
        )];
        let mut level = vec!["root".to_string()];
        for depth in 1..=3 {
            let mut next = Vec::new();
            for parent in &level {
                for _ in 0..fan {
                    let module = format!("t{depth}_{}", next.len());
                    sources.push((format!("{module}.py"), format!("from {parent} import *\n")));
                    next.push(module);
                }
            }
            level = next;
        }
        let files: Vec<(u32, &str, &str)> = sources
            .iter()
            .map(|(rel, src)| (0, rel.as_str(), src.as_str()))
            .collect();
        index_of(&[RootKind::Site], &files)
    }

    #[test]
    fn capped_star_fanout_keeps_public_reexport_rank_improvements() {
        let mut sources = vec![(
            "pkg/_impl.py".to_string(),
            "class Alpha: pass\n".to_string(),
        )];
        for i in 0..8 {
            sources.push((format!("a{i}.py"), "from pkg._impl import *\n".into()));
        }
        // A public defining-package export queued after all external offers.
        sources.push(("pkg/__init__.py".into(), "from ._impl import *\n".into()));
        let files: Vec<_> = sources
            .iter()
            .map(|(path, text)| (0, path.as_str(), text.as_str()))
            .collect();
        let index = index_of(&[RootKind::Site], &files);
        assert!(index
            .exact_import_candidates("Alpha", 100)
            .iter()
            .any(|c| c.module == "pkg"));
        // Rank improvements in an already offered external package must
        // survive the cap as well (the private bridge itself is filtered).
        sources[1] = ("x/_bridge.py".into(), "from pkg._impl import *\n".into());
        sources.push(("x/__init__.py".into(), "from ._bridge import *\n".into()));
        let files: Vec<_> = sources
            .iter()
            .map(|(path, text)| (0, path.as_str(), text.as_str()))
            .collect();
        let index = index_of(&[RootKind::Site], &files);
        let modules: Vec<_> = index
            .exact_import_candidates("Alpha", 100)
            .into_iter()
            .map(|c| c.module)
            .collect();
        assert!(
            modules.contains(&"pkg".to_string()) && modules.contains(&"x".to_string()),
            "{modules:?}"
        );
        assert!(modules.len() <= 1 + MAX_STAR_OFFERS);
    }

    #[test]
    fn star_import_trees_offer_a_name_from_a_few_modules() {
        // 1,110 modules re-export one module's names through star imports,
        // each a new offer: a query collected about a million offers and
        // took seconds (on the UI thread, for one Ctrl+Space).
        let idx = star_tree(1000, 10);
        let started = std::time::Instant::now();
        let found = idx.import_candidates("alpha", &|n: &str| n.starts_with("alpha"), 200);
        assert_eq!(found.len(), 200);
        let exact = idx.exact_import_candidates("alpha_7", 5000);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        // The defining module first, then a few of the modules re-exporting it.
        assert_eq!(
            exact.iter().map(|c| c.module.as_str()).collect::<Vec<_>>(),
            ["root", "t1_0", "t1_1", "t1_2", "t1_3", "t1_4", "t1_5", "t1_6", "t1_7"]
        );
        assert!(exact
            .iter()
            .all(|c| c.name == "alpha_7" && c.kind == SymbolKind::Function));
        // Re-exports within one package do not count: a package `__init__`
        // star-importing its modules still offers their names.
        let idx = index_of(
            &[RootKind::Site],
            &[
                (
                    0,
                    "pkg/__init__.py",
                    "from ._a import *\nfrom ._b import *\n",
                ),
                (0, "pkg/_a.py", "from ._b import *\ndef alpha_one(): pass\n"),
                (0, "pkg/_b.py", "def alpha_two(): pass\n"),
            ],
        );
        let modules = |name: &str| {
            idx.exact_import_candidates(name, 10)
                .into_iter()
                .map(|c| c.module)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            (modules("alpha_one"), modules("alpha_two")),
            (vec!["pkg".to_string()], vec!["pkg".to_string()])
        );
    }

    #[test]
    fn top_level_declarations_leave_out_members_and_other_kinds() {
        let idx = index_of(&[RootKind::Project], &[(0, "app/models.py", "import os\nLIMIT = 3\nvalue = 1\nclass Order(Base):\n    TOTAL = 1\n    def total(self): pass\ndef helper(): pass\n"), (0, "main.py", "def main(): pass\n")]);
        let got: Vec<(String, SymbolKind, String, PathBuf, u32)> = idx
            .top_level_declarations(&[
                SymbolKind::Class,
                SymbolKind::Function,
                SymbolKind::Constant,
            ])
            .into_iter()
            .map(|h| (h.name, h.kind, h.detail, h.path, h.line))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "LIMIT".to_string(),
                    SymbolKind::Constant,
                    String::new(),
                    PathBuf::from("/r0/app/models.py"),
                    1
                ),
                (
                    "Order".to_string(),
                    SymbolKind::Class,
                    "(Base)".to_string(),
                    PathBuf::from("/r0/app/models.py"),
                    3
                ),
                (
                    "helper".to_string(),
                    SymbolKind::Function,
                    "()".to_string(),
                    PathBuf::from("/r0/app/models.py"),
                    6
                ),
                (
                    "main".to_string(),
                    SymbolKind::Function,
                    "()".to_string(),
                    PathBuf::from("/r0/main.py"),
                    0
                ),
            ]
        );
        // The same as filtering every declaration.
        let filtered: Vec<SymbolHit> = idx
            .declarations()
            .into_iter()
            .filter(|d| {
                d.container.is_empty()
                    && matches!(
                        d.kind,
                        SymbolKind::Class | SymbolKind::Function | SymbolKind::Constant
                    )
            })
            .collect();
        assert_eq!(
            idx.top_level_declarations(&[
                SymbolKind::Class,
                SymbolKind::Function,
                SymbolKind::Constant
            ]),
            filtered
        );
    }

    #[test]
    fn top_level_declaration_ids_resolve_to_the_same_hits() {
        let idx = index_of(&[RootKind::Project], &[(0, "app/models.py", "import os\nLIMIT = 3\nclass Order(Base):\n    def total(self): pass\ndef helper(): pass\n"), (0, "main.py", "def main(): pass\n")]);
        let kinds = [
            SymbolKind::Class,
            SymbolKind::Function,
            SymbolKind::Constant,
        ];
        let ids = idx.top_level_declaration_ids(&kinds);
        let resolved: Vec<SymbolHit> = ids
            .iter()
            .map(|&id| idx.declaration(id).expect("listed id"))
            .collect();
        assert_eq!(resolved, idx.top_level_declarations(&kinds));
        assert_eq!(resolved.len(), 4);
        assert_eq!(idx.declaration(idx.symbol_count() as u32), None);
    }

    #[test]
    fn only_a_real_standard_library_is_classified_as_one() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib/python3.12");
        std::fs::create_dir_all(lib.join("encodings")).unwrap();
        std::fs::write(lib.join("os.py"), "").unwrap();
        std::fs::write(lib.join("encodings/__init__.py"), "").unwrap();
        assert_eq!(RootKind::classify(&lib), RootKind::Stdlib);
        // A project on sys.path that happens to have an os.py.
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("os.py"), "").unwrap();
        assert_eq!(RootKind::classify(&proj), RootKind::Other);
        assert_eq!(
            RootKind::classify(&dir.path().join("venv/lib/python3.12/site-packages")),
            RootKind::Site
        );
    }

    #[test]
    fn exact_dunder_all_limits_exports() {
        let idx = index_of(
            &[RootKind::Site],
            &[(
                0,
                "m.py",
                "__all__ = ['public_one']\ndef public_one(): pass\ndef public_two(): pass\n",
            )],
        );
        let got = idx.import_candidates("public", &|n: &str| n.starts_with("public"), 10);
        assert_eq!(
            got.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["public_one"]
        );
        // Go to Symbol still finds both.
        assert_eq!(
            idx.search("public", &SearchOptions::default(), &score)
                .len(),
            2
        );
    }

    #[test]
    fn bundles_drop_local_paths_and_other_roots() {
        let idx = index_of(
            &[RootKind::Stdlib, RootKind::Site],
            &[
                (0, "os.py", "def getcwd(): pass\n"),
                (1, "pkg.py", "X = 1\n"),
            ],
        );
        let bundle = idx.to_bundle(
            &[RootKind::Stdlib],
            IndexMeta {
                label: "stdlib".into(),
                ..Default::default()
            },
        );
        assert_eq!(bundle.meta().kind, IndexKind::Bundle);
        assert_eq!(bundle.file_count(), 1);
        assert_eq!(bundle.roots()[0].path, PathBuf::from("<stdlib-0>"));
        assert_eq!(bundle.files_by_hash().len(), 1);
        assert_eq!(bundle.meta().symbols, 1);
    }

    #[test]
    fn bundles_carry_no_trace_of_the_users_own_projects() {
        // sys.path of an interpreter with editable installs: the project's
        // `src` (another root) and the build backends' import hooks in
        // site-packages (setuptools, editables for hatchling and pdm,
        // meson-python, scikit-build-core), named after the projects.
        let idx = index_of(
            &[RootKind::Stdlib, RootKind::Site, RootKind::Other],
            &[
                (0, "os.py", "def getcwd(): pass\n"),
                (1, "pkg/__init__.py", "def helper(): pass\n"),
                (1, "__editable___acme_billing_0_1_finder.py", "MAPPING: dict[str, str] = {'acme_billing': '/home/me/acme/src/acme_billing'}\nclass _EditableFinder:\n    pass\ndef install():\n    pass\n"),
                (1, "_editable_impl_acme_ledger.py", "from editables.redirector import RedirectingFinder as F\nF.install()\nF.map_module('acme_ledger', '/home/me/acme/ledger/__init__.py')\n"),
                (1, "_acme_native_editable_loader.py", "class MesonpyMetaFinder:\n    pass\ndef install(names, path): pass\ninstall({'acme_native'}, '/home/me/acme/build/cp312')\n"),
                (1, "_acme_fast_editable.py", "class ScikitBuildRedirectingFinder:\n    pass\ndef install(known_source_files): pass\ninstall({'acme_fast': '/home/me/acme/fast/__init__.py'})\n"),
                (2, "acme_billing/__init__.py", "def connect(host='db.internal.acme.example', password='hunter2'):\n    pass\n"),
            ],
        );
        assert_eq!(
            idx.exact_import_candidates("connect", 5).len(),
            1,
            "the library index itself has the project"
        );
        let bundle = idx.to_bundle(
            &[RootKind::Stdlib, RootKind::Site],
            IndexMeta {
                label: "Python 3.12 libraries".into(),
                ..Default::default()
            },
        );
        assert_eq!(
            bundle
                .files()
                .map(|(_, m, _)| m.to_string())
                .collect::<Vec<_>>(),
            vec!["os", "pkg"]
        );
        for name in [
            "connect",
            "install",
            "MAPPING",
            "_EditableFinder",
            "F",
            "MesonpyMetaFinder",
            "ScikitBuildRedirectingFinder",
        ] {
            assert!(
                bundle
                    .search(name, &SearchOptions::default(), &score)
                    .is_empty(),
                "{name}"
            );
        }
        // Not in the bytes written either (header and decompressed body).
        let bytes = crate::format::encode(&bundle);
        let meta_len = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        let mut raw = bytes[..20 + meta_len].to_vec();
        std::io::Read::read_to_end(
            &mut flate2::read::ZlibDecoder::new(&bytes[20 + meta_len..]),
            &mut raw,
        )
        .unwrap();
        let text = String::from_utf8_lossy(&raw);
        for secret in ["acme", "hunter2", "/home/me", "/r0", "/r1", "/r2"] {
            assert!(!text.contains(secret), "{secret:?} in the bundle");
        }
        assert!(text.contains("getcwd") && text.contains("helper"));
    }

    #[test]
    fn editable_install_hooks_are_recognized() {
        for hook in [
            "__editable___acme_0_1_finder.py",
            "_editable_impl_acme.py",
            "_acme_editable_loader.py",
            "_acme_editable.py",
        ] {
            assert!(is_editable_install_hook(hook), "{hook}");
        }
        for module in [
            "acme_editable.py",
            "_acme_editor.py",
            "pkg/_acme_editable.py",
            "_editable_impl_acme.pyi",
            "editables/redirector.py",
            "_distutils_hack/__init__.py",
        ] {
            assert!(!is_editable_install_hook(module), "{module}");
        }
    }

    #[test]
    fn masks() {
        assert_eq!(char_mask("abc") & char_mask("ABC"), char_mask("abc"));
        assert_eq!(char_mask("get_value") & char_mask("gv"), char_mask("gv"));
        assert_ne!(char_mask("abc") & char_mask("x"), char_mask("x"));
        assert_ne!(char_mask("größe") & (1 << 37), 0);
        assert!(
            is_private_module("pkg._impl")
                && is_private_module("_ssl")
                && !is_private_module("pkg.__main__")
                && !is_private_module("json.decoder")
        );
    }
}

#[cfg(test)]
mod basic_class_import_tests {
    use super::*;
    #[test]
    fn class_only_candidate_selection_preserves_reexports_before_applying_cap() {
        let source = (0..250)
            .map(|number| format!("def Thing{number}(): pass\n"))
            .collect::<String>()
            + "class ThingWanted: pass\n";
        let index = super::tests::index_of(
            &[RootKind::Site],
            &[
                (0, "package/__init__.py", "from ._impl import ThingWanted\n"),
                (0, "package/_impl.py", &source),
            ],
        );
        let found = index.class_import_candidates(
            "thing",
            &|name| name.to_lowercase().contains("thing"),
            1,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, SymbolKind::Class);
        assert_eq!(found[0].name, "ThingWanted");
        assert_eq!(found[0].module, "package");
    }
}
