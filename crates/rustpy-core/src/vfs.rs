//! Shared identities and immutable byte snapshots. Metadata is never a content revision.
use crate::{FileId, FileIdAllocator};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FileKey {
    Real(PathBuf),
    Virtual(PathBuf),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContentRevision(pub [u8; 32]);
impl ContentRevision {
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub id: FileId,
    pub key: FileKey,
    pub bytes: Arc<[u8]>,
    pub revision: ContentRevision,
}
#[derive(Clone, Copy, Debug)]
pub struct ReadLimits {
    pub max_file_bytes: usize,
    pub max_cached_bytes: usize,
    pub max_cached_files: usize,
}
impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 64 * 1024 * 1024,
            max_cached_bytes: 64 * 1024 * 1024,
            max_cached_files: 5000,
        }
    }
}
#[derive(Default)]
struct State {
    identities: HashMap<FileKey, FileId>,
    snapshots: HashMap<FileId, Snapshot>,
    bytes: usize,
}
pub struct Vfs {
    ids: FileIdAllocator,
    state: Mutex<State>,
}
/// Canonicalize existing ancestors, including aliases, then normalize missing suffixes.
pub fn normalize_path(path: &Path) -> PathBuf {
    normalize_path_inner(path, 0)
}
fn normalize_path_inner(path: &Path, depth: usize) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    if depth < 32 {
        if let Ok(target) = std::fs::read_link(&absolute) {
            let target = if target.is_absolute() {
                target
            } else {
                absolute.parent().unwrap_or(Path::new("/")).join(target)
            };
            return normalize_path_inner(&target, depth + 1);
        }
    }
    let mut prefix = absolute.as_path();
    let mut suffix = Vec::new();
    while std::fs::canonicalize(prefix).is_err() {
        let Some(name) = prefix.file_name() else {
            break;
        };
        suffix.push(name.to_os_string());
        let Some(parent) = prefix.parent() else { break };
        prefix = parent;
    }
    let mut result = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    for name in suffix.into_iter().rev() {
        result.push(name);
    }
    let mut clean = PathBuf::new();
    for component in result.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other.as_os_str()),
        }
    }
    clean
}
/// Nonblocking open on Unix prevents a raced replacement by a FIFO from hanging
/// the editor. The descriptor, rather than a preceding path stat, is validated.
pub fn open_regular_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}
pub fn open_regular_file_for_write(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}
fn normalize_key(key: FileKey) -> FileKey {
    match key {
        FileKey::Real(path) => FileKey::Real(normalize_path(&path)),
        FileKey::Virtual(path) => {
            let mut clean = PathBuf::new();
            for component in path.components() {
                match component {
                    Component::CurDir => {}
                    Component::ParentDir => {
                        clean.pop();
                    }
                    other => clean.push(other.as_os_str()),
                }
            }
            FileKey::Virtual(clean)
        }
    }
}
impl Default for Vfs {
    fn default() -> Self {
        Self::new()
    }
}
impl Vfs {
    pub fn new() -> Self {
        Self {
            ids: FileIdAllocator::new(),
            state: Mutex::new(State::default()),
        }
    }
    pub fn real_key(path: &Path) -> FileKey {
        FileKey::Real(normalize_path(path))
    }
    pub fn id(&self, key: FileKey) -> FileId {
        let key = normalize_key(key);
        let mut state = self.state.lock();
        *state
            .identities
            .entry(key)
            .or_insert_with(|| self.ids.alloc())
    }
    pub fn read(&self, path: &Path, limits: ReadLimits) -> io::Result<Snapshot> {
        self.read_with(path, limits, || Ok(()))
    }
    pub fn read_with(
        &self,
        path: &Path,
        limits: ReadLimits,
        mut checkpoint: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Snapshot> {
        let key = Self::real_key(path);
        let FileKey::Real(ref path) = key else {
            unreachable!()
        };
        let mut file = open_regular_file(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        if meta.len() > limits.max_file_bytes as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file exceeds read limit",
            ));
        }
        let mut bytes = Vec::with_capacity(
            (meta.len() as usize)
                .min(limits.max_file_bytes)
                .min(64 * 1024),
        );
        let mut chunk = [0u8; 64 * 1024];
        loop {
            checkpoint()?;
            let n = file.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            if bytes.len().saturating_add(n) > limits.max_file_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "file grew beyond read limit",
                ));
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
        if bytes.len() > limits.max_file_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file grew beyond read limit",
            ));
        }
        self.publish(key, bytes.into(), limits)
    }
    pub fn publish(
        &self,
        key: FileKey,
        bytes: Arc<[u8]>,
        limits: ReadLimits,
    ) -> io::Result<Snapshot> {
        if bytes.len() > limits.max_file_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot exceeds read limit",
            ));
        }
        let key = normalize_key(key);
        let revision = ContentRevision::of(&bytes);
        let mut state = self.state.lock();
        let id = *state
            .identities
            .entry(key.clone())
            .or_insert_with(|| self.ids.alloc());
        let bytes = state
            .snapshots
            .get(&id)
            .filter(|old| old.revision == revision)
            .map(|old| old.bytes.clone())
            .unwrap_or(bytes);
        let snapshot = Snapshot {
            id,
            key,
            bytes,
            revision,
        };
        if let Some(old) = state.snapshots.remove(&id) {
            state.bytes -= old.bytes.len();
        }
        while !state.snapshots.is_empty()
            && (state.bytes.saturating_add(snapshot.bytes.len()) > limits.max_cached_bytes
                || state.snapshots.len() >= limits.max_cached_files)
        {
            let oldest = *state.snapshots.keys().next().unwrap();
            let old = state.snapshots.remove(&oldest).unwrap();
            state.bytes -= old.bytes.len();
        }
        if snapshot.bytes.len() <= limits.max_cached_bytes && limits.max_cached_files > 0 {
            state.bytes += snapshot.bytes.len();
            state.snapshots.insert(id, snapshot.clone());
        }
        Ok(snapshot)
    }
    pub fn cached_usage(&self) -> (usize, usize) {
        let state = self.state.lock();
        (state.snapshots.len(), state.bytes)
    }
    /// Drop cached disk bytes while preserving stable file IDs and live editors.
    pub fn clear_cached_snapshots(&self) {
        let mut state = self.state.lock();
        state.snapshots.clear();
        state.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clearing_disk_snapshots_preserves_file_identity_and_live_bytes() {
        let dir = Scratch::new();
        let path = dir.0.join("cache.py");
        std::fs::write(&path, b"value=1\n").unwrap();
        let vfs = Vfs::new();
        let before = vfs.read(&path, ReadLimits::default()).unwrap();
        assert_eq!(vfs.cached_usage().0, 1);
        vfs.clear_cached_snapshots();
        assert_eq!(vfs.cached_usage(), (0, 0));
        std::fs::write(&path, b"value=2\n").unwrap();
        let after = vfs.read(&path, ReadLimits::default()).unwrap();
        assert_eq!(before.id, after.id);
        assert_eq!(&*before.bytes, b"value=1\n");
        assert_eq!(&*after.bytes, b"value=2\n");
    }
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "rustpy-vfs-{}-{}",
                std::process::id(),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn raw_revisions_detect_restored_metadata_and_keep_snapshots_immutable() {
        let dir = Scratch::new();
        let path = dir.0.join("m.py");
        let vfs = Vfs::new();
        std::fs::write(&path, b"x=1\n").unwrap();
        let old = vfs.read(&path, ReadLimits::default()).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, b"x=2\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        let new = vfs.read(&path, ReadLimits::default()).unwrap();
        assert_ne!(old.revision, new.revision);
        assert_eq!(old.id, new.id);
        assert_eq!(&*old.bytes, b"x=1\n");
        assert_ne!(ContentRevision::of(b"x\n"), ContentRevision::of(b"x\r\n"));
        assert_ne!(
            ContentRevision::of(b"x\n"),
            ContentRevision::of(b"\xef\xbb\xbfx\n")
        );
    }
    #[test]
    fn identity_normalizes_aliases_and_keeps_virtual_keys_distinct() {
        let dir = Scratch::new();
        let path = dir.0.join("m.py");
        std::fs::write(&path, b"x").unwrap();
        let vfs = Vfs::new();
        let real = vfs.id(FileKey::Real(path.clone()));
        assert_eq!(real, vfs.id(FileKey::Real(dir.0.join("./m.py"))));
        assert_ne!(real, vfs.id(FileKey::Virtual(path.clone())));
        assert_eq!(
            vfs.id(FileKey::Virtual(PathBuf::from("<typeshed>/./x.pyi"))),
            vfs.id(FileKey::Virtual(PathBuf::from("<typeshed>/x.pyi")))
        );
        #[cfg(unix)]
        {
            let link = dir.0.join("alias.py");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert_eq!(real, vfs.read(&link, ReadLimits::default()).unwrap().id);
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"new").unwrap();
        assert_eq!(real, vfs.read(&path, ReadLimits::default()).unwrap().id);
    }
    #[cfg(unix)]
    #[test]
    fn fifo_reads_never_block() {
        let dir = Scratch::new();
        let path = dir.0.join("pipe");
        use std::os::unix::ffi::OsStrExt;
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert!(Vfs::new().read(&path, ReadLimits::default()).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }
    #[test]
    fn regular_files_read_limits_cache_limits_and_cancellation_are_enforced() {
        let dir = Scratch::new();
        let path = dir.0.join("m.py");
        std::fs::write(&path, b"12345").unwrap();
        let vfs = Vfs::new();
        let limits = ReadLimits {
            max_file_bytes: 4,
            max_cached_bytes: 4,
            max_cached_files: 1,
        };
        assert!(vfs.read(&path, limits).is_err());
        assert!(vfs.read(&dir.0, ReadLimits::default()).is_err());
        assert!(vfs
            .read_with(&path, ReadLimits::default(), || Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "cancel"
            )))
            .is_err());
        let old = vfs.read(&path, ReadLimits::default()).unwrap();
        let smaller = ReadLimits {
            max_file_bytes: 8,
            max_cached_bytes: 2,
            max_cached_files: 0,
        };
        vfs.read(&path, smaller).unwrap();
        assert_eq!(vfs.cached_usage(), (0, 0));
        assert_eq!(old.bytes.len(), 5);
    }
}
