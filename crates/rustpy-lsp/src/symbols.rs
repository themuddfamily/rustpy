//! Document symbols (the Structure view as a tree) and the project symbol
//! index behind workspace symbols (Go to Symbol), auto-import completion and
//! the "Add import" quick fixes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lsp_types::{DocumentSymbol, Range, SymbolInformation, SymbolKind, Uri};
use parking_lot::Mutex;

use crate::line_index::{Encoding, LineIndex};
use crate::syntax::{self, OutlineItem, OutlineKind};

pub fn symbol_kind(k: OutlineKind) -> SymbolKind {
    match k {
        OutlineKind::Class => SymbolKind::CLASS,
        OutlineKind::Function => SymbolKind::FUNCTION,
        OutlineKind::Method => SymbolKind::METHOD,
        OutlineKind::Constructor => SymbolKind::CONSTRUCTOR,
        OutlineKind::Property => SymbolKind::PROPERTY,
        OutlineKind::Variable => SymbolKind::VARIABLE,
        OutlineKind::Constant => SymbolKind::CONSTANT,
        OutlineKind::Field => SymbolKind::FIELD,
    }
}

/// The outline as LSP document symbols (hierarchical).
#[allow(deprecated)]
pub fn document_symbols(
    items: &[OutlineItem],
    index: &LineIndex,
    enc: Encoding,
) -> Vec<DocumentSymbol> {
    items
        .iter()
        .map(|i| {
            let range = index.range(i.range, enc);
            let mut selection = index.range(i.name_range, enc);
            // The selection range must lie inside the range.
            if selection.start < range.start || selection.end > range.end {
                selection = Range::new(range.start, range.start);
            }
            DocumentSymbol {
                name: i.name.clone(),
                detail: (!i.detail.is_empty()).then(|| i.detail.clone()),
                kind: symbol_kind(i.kind),
                tags: None,
                deprecated: None,
                range,
                selection_range: selection,
                children: (!i.children.is_empty())
                    .then(|| document_symbols(&i.children, index, enc)),
            }
        })
        .collect()
}

/// The outline flattened for clients without hierarchical symbol support.
#[allow(deprecated)]
pub fn flat_symbols(
    items: &[OutlineItem],
    index: &LineIndex,
    enc: Encoding,
    uri: &Uri,
    container: Option<&str>,
    out: &mut Vec<SymbolInformation>,
) {
    for i in items {
        out.push(SymbolInformation {
            name: i.name.clone(),
            kind: symbol_kind(i.kind),
            tags: None,
            deprecated: None,
            location: lsp_types::Location::new(uri.clone(), index.range(i.name_range, enc)),
            container_name: container.map(str::to_string),
        });
        flat_symbols(&i.children, index, enc, uri, Some(&i.name), out);
    }
}

/// One indexed definition.
#[derive(Clone, Debug)]
pub struct IndexedSymbol {
    pub name: String,
    pub kind: OutlineKind,
    pub container: Option<String>,
    /// The name's range in the negotiated position encoding.
    pub range: Range,
    pub top_level: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Stamp {
    Disk {
        revision: rustpy_core::vfs::ContentRevision,
    },
    Open {
        len: usize,
        hash: u64,
    },
}

#[derive(Clone)]
struct Entry {
    stamp: Stamp,
    module: Option<String>,
    symbols: Arc<Vec<IndexedSymbol>>,
}

/// Definitions of every project file, refreshed lazily when files change.
pub struct SymbolIndex {
    enc: Encoding,
    entries: Mutex<HashMap<PathBuf, Entry>>,
    /// `(name, module)` of top-level definitions, rebuilt by [`SymbolIndex::refresh`].
    top_level: Mutex<Arc<Vec<(String, String, OutlineKind)>>>,
}

impl SymbolIndex {
    /// Fork disk symbols and overlay this request generation's buffers.
    pub fn with_open(
        &self,
        docs: &HashMap<PathBuf, crate::docs::Doc>,
        files: &crate::project::ProjectFiles,
        analysis: &rustpy_semantic::Analysis,
    ) -> Self {
        let view = Self {
            enc: self.enc,
            entries: Mutex::new(self.entries.lock().clone()),
            top_level: Mutex::new(self.top_level.lock().clone()),
        };
        for doc in docs
            .values()
            .filter(|d| d.is_python && files.contains(&d.path))
        {
            let text = doc.text();
            view.entries.lock().insert(
                doc.path.clone(),
                Entry {
                    stamp: Stamp::Open {
                        len: text.len(),
                        hash: hash(text),
                    },
                    module: analysis.resolver.module_name_of(&doc.path),
                    symbols: Arc::new(index_text(text, self.enc)),
                },
            );
        }
        view.rebuild_top();
        view
    }
    pub fn new(enc: Encoding) -> Self {
        Self {
            enc,
            entries: Mutex::new(HashMap::new()),
            top_level: Mutex::new(Arc::new(Vec::new())),
        }
    }

    /// Brings the index up to date with `files` (open documents' text wins
    /// over the disk). Returns whether the top-level definitions changed, or
    /// `None` when `cancelled` stopped it early.
    pub fn refresh(
        &self,
        files: &[PathBuf],
        open: &dyn Fn(&Path) -> Option<Arc<str>>,
        module_of: &dyn Fn(&Path) -> Option<String>,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<bool> {
        let mut seen: Vec<&PathBuf> = Vec::with_capacity(files.len());
        for (n, path) in files.iter().enumerate() {
            if n.is_multiple_of(64) && cancelled() {
                return None;
            }
            seen.push(path);
            let (stamp, text): (Stamp, Arc<str>) = match open(path) {
                Some(text) => (
                    Stamp::Open {
                        len: text.len(),
                        hash: hash(&text),
                    },
                    text,
                ),
                None => {
                    let limits = rustpy_core::vfs::ReadLimits {
                        max_file_bytes: 4 * 1024 * 1024,
                        ..Default::default()
                    };
                    let snapshot = match rustpy_core::vfs::Vfs::new().read(path, limits) {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            log::warn!("Cannot index {}: {error}", path.display());
                            self.entries.lock().remove(path);
                            continue;
                        }
                    };
                    let stamp = Stamp::Disk {
                        revision: snapshot.revision,
                    };
                    if self
                        .entries
                        .lock()
                        .get(path)
                        .is_some_and(|entry| entry.stamp == stamp)
                    {
                        continue;
                    }
                    let text = match rustpy_text::Encoding::decode_python_source(&snapshot.bytes) {
                        Ok(text) => text,
                        Err(error) => {
                            log::warn!("Cannot index {}: {error}", path.display());
                            self.entries.lock().remove(path);
                            continue;
                        }
                    };
                    (stamp, Arc::from(text))
                }
            };
            if self
                .entries
                .lock()
                .get(path)
                .is_some_and(|entry| entry.stamp == stamp)
            {
                continue;
            }
            let symbols = Arc::new(index_text(&text, self.enc));
            self.entries.lock().insert(
                path.clone(),
                Entry {
                    stamp,
                    module: module_of(path),
                    symbols,
                },
            );
        }
        let mut entries = self.entries.lock();
        let keep: std::collections::HashSet<&PathBuf> = seen.into_iter().collect();
        entries.retain(|p, _| keep.contains(p));
        drop(entries);
        Some(self.rebuild_top())
    }

    fn rebuild_top(&self) -> bool {
        let entries = self.entries.lock();
        let mut top: Vec<(String, String, OutlineKind)> = Vec::new();
        for e in entries.values() {
            let Some(m) = &e.module else { continue };
            top.extend(
                e.symbols
                    .iter()
                    .filter(|s| s.top_level)
                    .map(|s| (s.name.clone(), m.clone(), s.kind)),
            );
        }
        top.sort();
        top.dedup();
        let mut current = self.top_level.lock();
        let changed = **current != top;
        *current = Arc::new(top);
        changed
    }

    /// `(name, module)` of the project's public top-level classes, functions
    /// and variables whose names pass `keep`.
    pub fn project_symbols(&self, keep: impl Fn(&str) -> bool) -> Vec<(String, String)> {
        let top = self.top_level.lock().clone();
        top.iter()
            .filter(|(n, _, _)| !n.starts_with('_') && keep(n))
            .map(|(n, m, _)| (n.clone(), m.clone()))
            .collect()
    }

    pub fn top_level(&self) -> Arc<Vec<(String, String, OutlineKind)>> {
        self.top_level.lock().clone()
    }

    /// Symbols whose names match `query` (fuzzy, as Go to Symbol matches),
    /// best first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<(PathBuf, IndexedSymbol)> {
        let entries = self.entries.lock();
        let mut scored: Vec<(i32, &PathBuf, &IndexedSymbol)> = Vec::new();
        for (path, e) in entries.iter() {
            for s in e.symbols.iter() {
                if let Some(score) = rustpy_search::fuzzy_score(&s.name, query) {
                    // Classes and functions before variables, public before private.
                    let bonus = match s.kind {
                        OutlineKind::Class => 3,
                        OutlineKind::Function
                        | OutlineKind::Method
                        | OutlineKind::Constructor
                        | OutlineKind::Property => 2,
                        _ => 0,
                    } - if s.name.starts_with('_') { 5 } else { 0 };
                    scored.push((score * 4 + bonus, path, s));
                }
            }
        }
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.2.name.cmp(&b.2.name))
                .then_with(|| a.1.cmp(b.1))
                .then_with(|| a.2.range.start.line.cmp(&b.2.range.start.line))
        });
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|(_, p, s)| (p.clone(), s.clone()))
            .collect()
    }
}

fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// The definitions of one file.
pub fn index_text(text: &str, enc: Encoding) -> Vec<IndexedSymbol> {
    let parse = rustpy_syntax::parse(text);
    let items = syntax::outline(&parse);
    let index = LineIndex::new(text);
    let mut out = Vec::new();
    fn walk(
        items: &[OutlineItem],
        container: Option<&str>,
        index: &LineIndex,
        enc: Encoding,
        out: &mut Vec<IndexedSymbol>,
    ) {
        for i in items {
            out.push(IndexedSymbol {
                name: i.name.clone(),
                kind: i.kind,
                container: container.map(str::to_string),
                range: index.range(i.name_range, enc),
                top_level: container.is_none(),
            });
            walk(&i.children, Some(&i.name), index, enc, out);
        }
    }
    walk(&items, None, &index, enc, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_disk_symbols_validate_bytes_even_with_restored_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.py");
        let original = b"# coding: utf-8\ndef old(): pass\n";
        std::fs::write(&path, original).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let idx = SymbolIndex::new(Encoding::Utf16);
        let files = vec![path.clone()];
        let module = |_: &Path| Some("m".to_owned());
        idx.refresh(&files, &|_| None, &module, &|| false).unwrap();
        assert_eq!(idx.search("old", 10)[0].1.name, "old");
        let mut malformed = original.to_vec();
        let at = original.windows(3).position(|s| s == b"old").unwrap();
        malformed[at] = 0xff;
        std::fs::write(&path, malformed).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        idx.refresh(&files, &|_| None, &module, &|| false).unwrap();
        assert!(idx.search("old", 10).is_empty());
        std::fs::write(&path, original).unwrap();
        idx.refresh(&files, &|_| None, &module, &|| false).unwrap();
        assert!(!idx.search("old", 10).is_empty());
        std::fs::remove_file(&path).unwrap();
        idx.refresh(&files, &|_| None, &module, &|| false).unwrap();
        assert!(idx.search("old", 10).is_empty());
    }

    #[test]
    fn closed_symbols_decode_cookie_and_drop_malformed_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.py");
        std::fs::write(&path, b"# coding: latin-1\ndef caf\xe9():\n    pass\n").unwrap();
        let idx = SymbolIndex::new(Encoding::Utf16);
        let files = vec![path.clone()];
        let module = |_: &Path| Some("m".to_owned());
        idx.refresh(&files, &|_| None, &module, &|| false).unwrap();
        assert_eq!(idx.search("café", 10)[0].1.name, "café");
        std::fs::write(&path, b"# coding: utf-8\n\xff").unwrap();
        idx.refresh(&files, &|_| None, &module, &|| false).unwrap();
        assert!(idx.search("café", 10).is_empty());
        let open: Arc<str> = Arc::from("def editor():\n    pass\n");
        idx.refresh(&files, &|_| Some(open.clone()), &module, &|| false)
            .unwrap();
        assert_eq!(idx.search("editor", 10)[0].1.name, "editor");
    }

    #[test]
    fn index_finds_definitions_and_refreshes() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.py");
        let b = dir.path().join("b.py");
        std::fs::write(
            &a,
            "class Greeter:\n    def greet(self):\n        pass\n\nMAX_SIZE = 3\n",
        )
        .unwrap();
        std::fs::write(&b, "def make_greeter():\n    pass\n").unwrap();
        let idx = SymbolIndex::new(Encoding::Utf16);
        let files = vec![a.clone(), b.clone()];
        let module = |p: &Path| p.file_stem().map(|s| s.to_string_lossy().into_owned());
        assert_eq!(
            idx.refresh(&files, &|_| None, &module, &|| false),
            Some(true)
        );
        assert_eq!(
            idx.refresh(&files, &|_| None, &module, &|| false),
            Some(false),
            "nothing changed"
        );
        let hits = idx.search("greet", 10);
        let names: Vec<&str> = hits.iter().map(|(_, s)| s.name.as_str()).collect();
        assert_eq!(names[0], "greet");
        assert!(names.contains(&"Greeter") && names.contains(&"make_greeter"));
        let greet = hits.iter().find(|(_, s)| s.name == "greet").unwrap();
        assert_eq!(greet.1.container.as_deref(), Some("Greeter"));
        assert_eq!(greet.1.range.start, lsp_types::Position::new(1, 8));
        // CamelHumps.
        assert_eq!(idx.search("MS", 10)[0].1.name, "MAX_SIZE");
        let mut syms = idx.project_symbols(|_| true);
        syms.sort();
        assert_eq!(
            syms,
            vec![
                ("Greeter".into(), "a".into()),
                ("MAX_SIZE".into(), "a".into()),
                ("make_greeter".into(), "b".into())
            ]
        );
        // Open text wins over the disk; deleted files drop out.
        let open_b: Arc<str> = Arc::from("def build():\n    pass\n");
        assert_eq!(
            idx.refresh(
                std::slice::from_ref(&b),
                &|p| (p == b).then(|| open_b.clone()),
                &module,
                &|| false
            ),
            Some(true)
        );
        assert_eq!(
            idx.refresh(&files, &|_| None, &module, &|| true),
            None,
            "cancelled"
        );
        assert!(idx
            .search("greet", 10)
            .iter()
            .all(|(_, s)| s.name != "Greeter"));
        assert_eq!(idx.search("build", 10)[0].1.name, "build");
    }
}
