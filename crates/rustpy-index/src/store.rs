//! Index files on disk.
//!
//! ```text
//! <index dir>/
//! ├── libraries/<hash of the interpreter path>.rpidx   one per interpreter
//! ├── projects/<hash of the project root>.rpidx         one per project
//! └── shared/<name>.rpidx                               imported shared bundles
//! ```
//!
//! Files are written to a temporary name and renamed into place, so a reader
//! (another RustPy window) sees either the old or the new index, never half of
//! one. Any file can be deleted at any time: the index is rebuilt.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::format::{self, FormatError};
use crate::hash::short_hex;
use crate::index::{IndexKind, IndexMeta, SymbolIndex};

/// File name extension of index files and bundles.
pub const EXTENSION: &str = "rpidx";

/// Temporary files of a save that never finished (the process was killed)
/// are removed once they are this old (a younger one may still be written).
const STALE_TEMP_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Removes `*.rpidx.tmp-*` files in `dir` older than `age`.
fn remove_stale_temp_files(dir: &Path, age: std::time::Duration) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let marker = format!(".{EXTENSION}.tmp-");
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().contains(&marker) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|d| d >= age);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Open without blocking on a FIFO substituted between metadata and open.
pub(crate) fn open_source(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

/// Why an index could not be loaded or saved.
#[derive(Debug)]
pub enum IndexError {
    Io(std::io::Error),
    Format(FormatError),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Io(e) => write!(f, "{e}"),
            IndexError::Format(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for IndexError {}

impl From<std::io::Error> for IndexError {
    fn from(e: std::io::Error) -> Self {
        IndexError::Io(e)
    }
}

impl From<FormatError> for IndexError {
    fn from(e: FormatError) -> Self {
        IndexError::Format(e)
    }
}

/// An index written by [`IndexStore::stage`], not yet in place. Dropping it
/// discards it.
#[derive(Debug)]
pub struct StagedIndex {
    tmp: Option<PathBuf>,
    path: PathBuf,
    size: u64,
}

impl StagedIndex {
    /// Puts the index in place (an atomic rename); returns its size.
    pub fn finish(mut self) -> Result<u64, IndexError> {
        let Some(tmp) = self.tmp.take() else {
            return Ok(self.size);
        };
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        if let Some(dir) = self.path.parent() {
            remove_stale_temp_files(dir, STALE_TEMP_AGE);
        }
        Ok(self.size)
    }

    /// Removes the written file: the index on disk stays as it was.
    pub fn discard(self) {}
}

impl Drop for StagedIndex {
    fn drop(&mut self) {
        if let Some(tmp) = self.tmp.take() {
            let _ = std::fs::remove_file(tmp);
        }
    }
}

/// An index file as listed by [`IndexStore::list`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredIndex {
    pub path: PathBuf,
    /// Bytes on disk.
    pub size: u64,
    pub meta: IndexMeta,
    /// Set when the header cannot be read (another version, damaged).
    pub error: Option<String>,
    /// A bundle shipped with RustPy (read-only, cannot be deleted from Settings).
    pub builtin: bool,
}

/// The index directory.
#[derive(Clone, Debug)]
pub struct IndexStore {
    dir: PathBuf,
    /// Read-only directories with bundles shipped next to the executable.
    builtin_dirs: Vec<PathBuf>,
}

impl IndexStore {
    pub fn new(dir: impl Into<PathBuf>) -> IndexStore {
        IndexStore {
            dir: dir.into(),
            builtin_dirs: Vec::new(),
        }
    }

    /// Also offers the bundles found in `dirs` (for example a `shared-indexes`
    /// directory installed next to the RustPy executable).
    pub fn with_builtin_dirs(mut self, dirs: Vec<PathBuf>) -> IndexStore {
        self.builtin_dirs = dirs;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the library index of the interpreter at `interpreter` lives.
    pub fn library_path(&self, interpreter: &Path) -> PathBuf {
        self.dir.join("libraries").join(format!(
            "{}.{EXTENSION}",
            short_hex(&interpreter.to_string_lossy())
        ))
    }

    /// Where the index of the project at `root` lives.
    pub fn project_path(&self, root: &Path) -> PathBuf {
        self.dir.join("projects").join(format!(
            "{}.{EXTENSION}",
            short_hex(&root.to_string_lossy())
        ))
    }

    pub fn shared_dir(&self) -> PathBuf {
        self.dir.join("shared")
    }

    /// Loads an index file. Bundles come from anywhere: a file that is not a
    /// regular file, or larger than any index can be ([`format::MAX_FILE`]),
    /// is refused without being read.
    pub fn load(path: &Path) -> Result<SymbolIndex, IndexError> {
        let file = open_source(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(FormatError::NotAnIndex.into());
        }
        let too_large = || IndexError::Format(FormatError::Damaged("file too large".into()));
        if meta.len() > format::MAX_FILE {
            return Err(too_large());
        }
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        // At most one byte past the limit, should the file grow meanwhile.
        file.take(format::MAX_FILE + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > format::MAX_FILE {
            return Err(too_large());
        }
        Ok(format::decode(&bytes)?)
    }

    /// Writes `index` to `path` atomically; returns the size written.
    pub fn save(path: &Path, index: &SymbolIndex) -> Result<u64, IndexError> {
        Self::stage(path, index)?.finish()
    }

    /// The slow part of [`Self::save`]: writes `index` next to `path`. The
    /// result is put in place by [`StagedIndex::finish`] (a rename) or
    /// dropped by [`StagedIndex::discard`], so a caller can decide at the last
    /// moment, under its own lock, whether to replace the file.
    pub fn stage(path: &Path, index: &SymbolIndex) -> Result<StagedIndex, IndexError> {
        let bytes = format::encode(index);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Unique per write: two jobs of one process may save the same index.
        static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("{EXTENSION}.tmp-{}-{n}", std::process::id()));
        let written = (|| {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(StagedIndex {
            tmp: Some(tmp),
            path: path.to_path_buf(),
            size: bytes.len() as u64,
        })
    }

    /// Reads the header of an index file.
    pub fn read_meta(path: &Path) -> Result<IndexMeta, IndexError> {
        let f = std::fs::File::open(path)?;
        Ok(format::read_meta(std::io::BufReader::new(f))?)
    }

    fn list_dir(dir: &Path, builtin: bool, out: &mut Vec<StoredIndex>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(EXTENSION) && p.is_file())
            .collect();
        paths.sort();
        for path in paths {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let (meta, error) = match Self::read_meta(&path) {
                Ok(m) => (m, None),
                Err(e) => (IndexMeta::default(), Some(e.to_string())),
            };
            out.push(StoredIndex {
                path,
                size,
                meta,
                error,
                builtin,
            });
        }
    }

    /// Every index file: library indexes, project indexes, then shared bundles
    /// (imported ones, then built-in ones).
    pub fn list(&self) -> Vec<StoredIndex> {
        let mut out = Vec::new();
        Self::list_dir(&self.dir.join("libraries"), false, &mut out);
        Self::list_dir(&self.dir.join("projects"), false, &mut out);
        Self::list_dir(&self.shared_dir(), false, &mut out);
        for d in &self.builtin_dirs {
            Self::list_dir(d, true, &mut out);
        }
        out
    }

    /// Total bytes of all index files (built-in bundles excluded).
    pub fn disk_usage(&self) -> u64 {
        self.list()
            .iter()
            .filter(|s| !s.builtin)
            .map(|s| s.size)
            .sum()
    }

    /// Deletes an index file. Only files inside the index directory are deleted.
    pub fn delete(&self, path: &Path) -> std::io::Result<()> {
        let escapes = path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        if !path.starts_with(&self.dir)
            || escapes
            || path.extension().and_then(|e| e.to_str()) != Some(EXTENSION)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "not an index in the index directory",
            ));
        }
        std::fs::remove_file(path)
    }

    /// Every shared bundle that can be loaded (imported and built-in), for use
    /// while building. Files that fail to load are skipped (and logged).
    pub fn load_bundles(&self) -> Vec<Arc<SymbolIndex>> {
        let mut out = Vec::new();
        let mut dirs = vec![self.shared_dir()];
        dirs.extend(self.builtin_dirs.iter().cloned());
        for d in dirs {
            let mut listed = Vec::new();
            Self::list_dir(&d, false, &mut listed);
            for s in listed {
                match Self::load(&s.path) {
                    Ok(idx) => out.push(Arc::new(idx)),
                    Err(e) => {
                        log::warn!("index: cannot use shared index {}: {e}", s.path.display())
                    }
                }
            }
        }
        out
    }

    /// Copies a bundle file into the shared directory after checking that it
    /// loads. A bundle with the same file name is replaced.
    pub fn import_bundle(&self, src: &Path) -> Result<StoredIndex, IndexError> {
        let idx = Self::load(src)?;
        if idx.file_count() == 0 {
            return Err(IndexError::Format(FormatError::Damaged(
                "the bundle has no files".into(),
            )));
        }
        let name = src
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("bundle");
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let dest = self.shared_dir().join(format!("{safe}.{EXTENSION}"));
        // Saved again rather than copied: the result is always in this version's format.
        let mut meta = idx.meta().clone();
        meta.kind = IndexKind::Bundle;
        let mut idx = idx;
        idx.set_meta(meta);
        let size = Self::save(&dest, &idx)?;
        Ok(StoredIndex {
            path: dest,
            size,
            meta: idx.meta().clone(),
            error: None,
            builtin: false,
        })
    }

    /// Writes `bundle` (see [`SymbolIndex::to_bundle`]) to `dest`.
    pub fn export_bundle(bundle: &SymbolIndex, dest: &Path) -> Result<u64, IndexError> {
        Self::save(dest, bundle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::tests::index_of;
    use crate::index::RootKind;

    #[test]
    fn save_load_list_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = IndexStore::new(dir.path().join("index"));
        let lib = store.library_path(Path::new("/usr/bin/python3"));
        assert!(lib.starts_with(dir.path().join("index/libraries")));
        assert_ne!(lib, store.library_path(Path::new("/proj/.venv/bin/python")));
        assert_eq!(
            lib,
            store.library_path(Path::new("/usr/bin/python3")),
            "stable names"
        );
        let mut idx = index_of(&[RootKind::Stdlib], &[(0, "os.py", "def getcwd(): pass\n")]);
        idx.set_meta(IndexMeta {
            label: "Python 3.12".into(),
            ..Default::default()
        });
        let size = IndexStore::save(&lib, &idx).unwrap();
        assert!(size > 20);
        let back = IndexStore::load(&lib).unwrap();
        assert!(back.same_content(&idx));
        let proj = store.project_path(Path::new("/proj"));
        let mut p = index_of(
            &[RootKind::Project],
            &[(0, "main.py", "def main(): pass\n")],
        );
        p.set_meta(IndexMeta {
            kind: IndexKind::Project,
            label: "proj".into(),
            ..Default::default()
        });
        IndexStore::save(&proj, &p).unwrap();
        std::fs::write(store.dir().join("libraries/garbage.rpidx"), b"not an index").unwrap();
        let listed = store.list();
        assert_eq!(listed.len(), 3);
        let good: Vec<&StoredIndex> = listed.iter().filter(|s| s.error.is_none()).collect();
        assert_eq!(
            good.iter()
                .map(|s| s.meta.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Python 3.12", "proj"]
        );
        assert!(listed.iter().any(|s| s.error.is_some()));
        assert_eq!(
            store.disk_usage(),
            listed.iter().map(|s| s.size).sum::<u64>()
        );
        // No temporary files are left behind, and those of an interrupted save are cleaned up.
        assert!(std::fs::read_dir(store.dir().join("libraries"))
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().contains(".tmp-")));
        let stale = store.dir().join("libraries/x.rpidx.tmp-1-0");
        std::fs::write(&stale, b"half").unwrap();
        remove_stale_temp_files(
            &store.dir().join("libraries"),
            std::time::Duration::from_secs(3600),
        );
        assert!(stale.exists(), "a recent one may still be written");
        remove_stale_temp_files(&store.dir().join("libraries"), std::time::Duration::ZERO);
        assert!(!stale.exists());
        store.delete(&lib).unwrap();
        assert!(!lib.exists());
        assert!(
            store.delete(Path::new("/etc/passwd")).is_err(),
            "only index files are deleted"
        );
        let outside = dir.path().join("outside.rpidx");
        std::fs::write(&outside, b"x").unwrap();
        assert!(
            store.delete(&store.dir().join("../outside.rpidx")).is_err(),
            "no way out of the index directory"
        );
        std::fs::write(store.dir().join("libraries/notes.txt"), b"x").unwrap();
        assert!(
            store
                .delete(&store.dir().join("libraries/notes.txt"))
                .is_err(),
            "index files only"
        );
        assert!(outside.exists());
    }

    #[test]
    fn bundles_import_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = IndexStore::new(dir.path().join("index"));
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
                label: "Python 3.12 stdlib".into(),
                python_version: "3.12.3".into(),
                ..Default::default()
            },
        );
        let exported = dir.path().join("python 3.12 stdlib.rpidx");
        IndexStore::export_bundle(&bundle, &exported).unwrap();
        let imported = store.import_bundle(&exported).unwrap();
        assert_eq!(
            imported.path,
            store.shared_dir().join("python-3.12-stdlib.rpidx")
        );
        assert_eq!(imported.meta.kind, IndexKind::Bundle);
        assert_eq!(imported.meta.files, 1);
        let loaded = store.load_bundles();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].meta().label, "Python 3.12 stdlib");
        // Built-in bundles are listed and loaded too, but never deleted.
        let builtin = dir.path().join("builtin");
        std::fs::create_dir_all(&builtin).unwrap();
        IndexStore::export_bundle(&bundle, &builtin.join("stdlib.rpidx")).unwrap();
        let store = store.with_builtin_dirs(vec![builtin.clone()]);
        assert_eq!(store.load_bundles().len(), 2);
        assert!(store.list().iter().any(|s| s.builtin));
        assert!(store.delete(&builtin.join("stdlib.rpidx")).is_err());
        // Not an index: refused.
        let junk = dir.path().join("junk.rpidx");
        std::fs::write(&junk, b"junk").unwrap();
        assert!(store.import_bundle(&junk).is_err());
        let empty = dir.path().join("empty.rpidx");
        IndexStore::export_bundle(&SymbolIndex::empty(IndexMeta::default()), &empty).unwrap();
        assert!(store.import_bundle(&empty).is_err());
    }
}
