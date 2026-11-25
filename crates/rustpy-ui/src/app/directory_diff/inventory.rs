//! Bounded, cancellation-aware directory inventory. No-follow traversal relative to held
//! directories prevents link replacement from redirecting a scan outside its captured roots.
use std::{
    collections::BTreeMap,
    fs::{File, Metadata},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
pub(super) trait CancelCheck {
    fn cancelled(&self) -> bool;
}
impl CancelCheck for AtomicBool {
    fn cancelled(&self) -> bool {
        self.load(Ordering::Relaxed)
    }
}
impl CancelCheck for rustpy_core::CancellationToken {
    fn cancelled(&self) -> bool {
        self.is_cancelled()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Difference {
    SameFile,
    SameDirectory,
    Added,
    Removed,
    Changed,
    TypeConflict,
    SymlinkSkipped,
    Error(String),
}
#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub relative: PathBuf,
    pub difference: Difference,
    pub paired_files: bool,
}
#[derive(Clone, Debug)]
pub(super) struct Report {
    pub left: PathBuf,
    pub right: PathBuf,
    pub entries: Vec<Entry>,
    pub complete: bool,
}
#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub entries: usize,
    pub bytes: u64,
    pub depth: usize,
    pub duration: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: 4096,
            bytes: 64 * 1024 * 1024,
            depth: 32,
            duration: Duration::from_secs(3),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    size: u64,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    modified: (i64, i64),
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(windows)]
    created: u64,
    #[cfg(windows)]
    modified: u64,
}
fn stamp(m: &Metadata) -> Stamp {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Stamp {
            size: m.len(),
            dev: m.dev(),
            ino: m.ino(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        Stamp {
            size: m.len(),
            created: m.creation_time(),
            modified: m.last_write_time(),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Stamp { size: m.len() }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Link,
    Other,
}
struct Item {
    kind: Kind,
    stamp: Stamp,
}
struct Budget<'a> {
    limits: Limits,
    started: Instant,
    cancel: &'a dyn CancelCheck,
    entries: usize,
    bytes: u64,
}
impl Budget<'_> {
    fn check(&self) -> Result<(), String> {
        if self.cancel.cancelled() {
            Err("Cancelled; inventory incomplete".into())
        } else if self.started.elapsed() >= self.limits.duration {
            Err("Time limit; inventory incomplete".into())
        } else {
            Ok(())
        }
    }
}
/// A directory (or, as an operand, a file) held open while the scan resolves names beneath it.
///
/// Unix opens and inspects children relative to the held descriptor (`openat`/`fstatat` with
/// no-follow flags), so replacing a component with a link cannot redirect the scan. Windows keeps
/// each held directory open without delete sharing, so it cannot be renamed or replaced while
/// path-based reads run beneath it, and opens every child as a reparse point itself rather than
/// its target; links and junctions are reported, never followed.
struct Handle {
    file: File,
    #[cfg(windows)]
    path: PathBuf,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Handle {
    const FLAGS: i32 = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    fn flags(directory: bool) -> i32 {
        // Non-blocking so a FIFO swapped in for a file cannot stall the scan.
        Self::FLAGS
            | if directory {
                libc::O_DIRECTORY
            } else {
                libc::O_NONBLOCK
            }
    }
    fn root(path: &Path) -> io::Result<Self> {
        use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(Self::flags(true))
            .open(path)?;
        Ok(Self { file })
    }
    fn name(name: &std::ffi::OsStr) -> io::Result<std::ffi::CString> {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid file name"))
    }
    fn open(&self, name: &std::ffi::OsStr, directory: bool) -> io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let name = Self::name(name)?;
        // SAFETY: the held descriptor and the C string outlive the call; a successful
        // descriptor is owned exactly once by the returned File.
        let fd =
            unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), Self::flags(directory)) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            file: unsafe { File::from_raw_fd(fd) },
        })
    }
    fn errno() -> *mut libc::c_int {
        // SAFETY: both return the calling thread's errno location.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::__errno_location()
        }
        #[cfg(target_os = "macos")]
        unsafe {
            libc::__error()
        }
    }
    fn names(&self) -> io::Result<Vec<std::ffi::OsString>> {
        use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
        // fdopendir takes ownership of its descriptor; give it a duplicate.
        let fd = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(error);
        }
        // The duplicate shares the file offset; always list from the start.
        unsafe { libc::rewinddir(stream) };
        let mut names = Vec::new();
        let result = loop {
            // readdir reports both the end and an error with null; only errno tells them apart.
            unsafe { *Self::errno() = 0 };
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                let code = unsafe { *Self::errno() };
                break if code == 0 {
                    Ok(())
                } else {
                    Err(io::Error::from_raw_os_error(code))
                };
            }
            // SAFETY: readdir returns a NUL-terminated name valid until the next call.
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
            let name = name.to_bytes();
            if name != b"." && name != b".." {
                names.push(std::ffi::OsStr::from_bytes(name).to_os_string());
            }
        };
        unsafe { libc::closedir(stream) };
        result.map(|()| names)
    }
    #[allow(clippy::unnecessary_cast)] // stat field widths differ between Unix targets
    fn child(&self, name: &std::ffi::OsStr) -> io::Result<(Kind, Stamp)> {
        use std::os::fd::AsRawFd;
        let name = Self::name(name)?;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid descriptor, C string and output buffer.
        if unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let kind = match st.st_mode & libc::S_IFMT {
            libc::S_IFLNK => Kind::Link,
            libc::S_IFDIR => Kind::Directory,
            libc::S_IFREG => Kind::File,
            _ => Kind::Other,
        };
        // The same widening std's MetadataExt applies, so it matches `stamp`.
        let stamp = Stamp {
            size: st.st_size as u64,
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
            modified: (st.st_mtime as i64, st.st_mtime_nsec as i64),
            changed: (st.st_ctime as i64, st.st_ctime_nsec as i64),
        };
        Ok((kind, stamp))
    }
}
#[cfg(windows)]
impl Handle {
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    fn root(path: &Path) -> io::Result<Self> {
        Self::at(path.to_path_buf(), true)
    }
    fn at(path: PathBuf, directory: bool) -> io::Result<Self> {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        // Read and write sharing only: without FILE_SHARE_DELETE the object cannot be
        // renamed or deleted while it is held.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .custom_flags(
                Self::FILE_FLAG_OPEN_REPARSE_POINT
                    | if directory {
                        Self::FILE_FLAG_BACKUP_SEMANTICS
                    } else {
                        0
                    },
            )
            .open(&path)?;
        let metadata = file.metadata()?;
        if metadata.file_attributes() & Self::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other("Link or reparse point is not followed"));
        }
        if directory && !metadata.is_dir() {
            return Err(io::Error::other("Not a directory"));
        }
        Ok(Self { file, path })
    }
    fn open(&self, name: &std::ffi::OsStr, directory: bool) -> io::Result<Self> {
        Self::at(self.path.join(name), directory)
    }
    fn names(&self) -> io::Result<Vec<std::ffi::OsString>> {
        std::fs::read_dir(&self.path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect()
    }
    fn child(&self, name: &std::ffi::OsStr) -> io::Result<(Kind, Stamp)> {
        let m = std::fs::symlink_metadata(self.path.join(name))?;
        // Symbolic links and junctions are links; any other reparse point is opened as
        // itself and refused by `at`, so it is never followed either.
        let kind = if m.file_type().is_symlink() {
            Kind::Link
        } else if m.is_dir() {
            Kind::Directory
        } else if m.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        Ok((kind, stamp(&m)))
    }
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl Handle {
    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "No-follow directory inventory is not available on this system",
        )
    }
    fn root(_: &Path) -> io::Result<Self> {
        Err(Self::unsupported())
    }
    fn open(&self, _: &std::ffi::OsStr, _: bool) -> io::Result<Self> {
        Err(Self::unsupported())
    }
    fn names(&self) -> io::Result<Vec<std::ffi::OsString>> {
        Err(Self::unsupported())
    }
    fn child(&self, _: &std::ffi::OsStr) -> io::Result<(Kind, Stamp)> {
        Err(Self::unsupported())
    }
}
impl Handle {
    fn stamp(&self) -> Result<Stamp, String> {
        Ok(stamp(&self.file.metadata().map_err(|e| e.to_string())?))
    }
}
fn walk(
    dir: &Handle,
    relative: &Path,
    depth: usize,
    map: &mut BTreeMap<PathBuf, Item>,
    budget: &mut Budget<'_>,
) -> Result<(), String> {
    budget.check()?;
    if depth > budget.limits.depth {
        return Err("Depth limit; inventory incomplete".into());
    }
    let before = dir.stamp()?;
    let names = dir.names().map_err(|e| e.to_string())?;
    for name in names {
        budget.check()?;
        if budget.entries >= budget.limits.entries {
            return Err("Entry limit; inventory incomplete".into());
        }
        budget.entries += 1;
        let rel = relative.join(&name);
        let (kind, item_stamp) = dir
            .child(&name)
            .map_err(|e| format!("{}: {e}", rel.display()))?;
        map.insert(
            rel.clone(),
            Item {
                kind,
                stamp: item_stamp,
            },
        );
        if kind == Kind::Directory {
            let child = dir
                .open(&name, true)
                .map_err(|e| format!("{}: {e}", rel.display()))?;
            if child.stamp()? != item_stamp {
                return Err("Directory changed during acquisition".into());
            }
            walk(&child, &rel, depth + 1, map, budget)?;
        }
    }
    if dir.stamp()? != before {
        return Err("Directory entries changed during inventory".into());
    }
    Ok(())
}
/// Open every ancestor relative to an already-held directory, never following links. The
/// ancestors stay held until the operand itself is open.
fn operand(root: &Handle, relative: &Path) -> Result<Handle, String> {
    let parts: Vec<_> = relative.components().collect();
    let mut chain: Vec<Handle> = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        let std::path::Component::Normal(name) = part else {
            return Err("Invalid relative operand".into());
        };
        let next = chain
            .last()
            .unwrap_or(root)
            .open(name, i + 1 < parts.len())
            .map_err(|e| e.to_string())?;
        chain.push(next);
    }
    chain.pop().ok_or_else(|| "Invalid relative operand".into())
}
fn chunk(file: &mut File, output: &mut [u8], budget: &Budget<'_>) -> Result<usize, String> {
    let mut count = 0;
    while count < output.len() {
        budget.check()?;
        match file.read(&mut output[count..]) {
            Ok(0) => break,
            Ok(read) => count += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(count)
}

fn compare(
    left: &File,
    right: &File,
    a: Stamp,
    b: Stamp,
    budget: &mut Budget<'_>,
) -> Result<bool, String> {
    compare_with(left, right, a, b, budget, || {})
}
fn compare_with(
    left: &File,
    right: &File,
    a: Stamp,
    b: Stamp,
    budget: &mut Budget<'_>,
    mut after_read: impl FnMut(),
) -> Result<bool, String> {
    let mut left = left.try_clone().map_err(|e| e.to_string())?;
    let mut right = right.try_clone().map_err(|e| e.to_string())?;
    if !left.metadata().map_err(|e| e.to_string())?.is_file()
        || !right.metadata().map_err(|e| e.to_string())?.is_file()
        || stamp(&left.metadata().map_err(|e| e.to_string())?) != a
        || stamp(&right.metadata().map_err(|e| e.to_string())?) != b
    {
        return Err("Operand changed before comparison".into());
    }
    let mut equal = true;
    let mut x = [0u8; 32768];
    let mut y = [0u8; 32768];
    loop {
        budget.check()?;
        if budget.limits.bytes.saturating_sub(budget.bytes) < 65536 {
            return Err("Byte limit; content unverified".into());
        }
        let nx = chunk(&mut left, &mut x, budget)?;
        let ny = chunk(&mut right, &mut y, budget)?;
        budget.bytes += (nx + ny) as u64;
        after_read();
        if nx != ny || x[..nx] != y[..ny] {
            equal = false;
            break;
        }
        if nx == 0 {
            break;
        }
    }
    if stamp(&left.metadata().map_err(|e| e.to_string())?) != a
        || stamp(&right.metadata().map_err(|e| e.to_string())?) != b
    {
        return Err("Operand changed during comparison".into());
    }
    Ok(equal)
}
pub(super) fn scan(
    left: PathBuf,
    right: PathBuf,
    limits: Limits,
    cancel: &dyn CancelCheck,
) -> Report {
    let mut report = Report {
        left: left.clone(),
        right: right.clone(),
        entries: vec![],
        complete: true,
    };
    let roots = (Handle::root(&left), Handle::root(&right));
    let (l, r) = match roots {
        (Ok(l), Ok(r)) => (l, r),
        (left, right) => {
            report.complete = false;
            report.entries.push(Entry {
                relative: PathBuf::new(),
                difference: Difference::Error(format!(
                    "Cannot open captured roots as no-follow directories; left: {}; right: {}",
                    left.err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "opened".into()),
                    right
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "opened".into()),
                )),
                paired_files: false,
            });
            return report;
        }
    };
    let mut budget = Budget {
        limits,
        started: Instant::now(),
        cancel,
        entries: 0,
        bytes: 0,
    };
    let mut lm = BTreeMap::new();
    let mut rm = BTreeMap::new();
    for (root, map, label) in [(&l, &mut lm, "Left"), (&r, &mut rm, "Right")] {
        if let Err(e) = walk(root, Path::new(""), 0, map, &mut budget) {
            report.complete = false;
            report.entries.push(Entry {
                relative: PathBuf::new(),
                difference: Difference::Error(format!("{label}: {e}")),
                paired_files: false,
            });
        }
    }
    let keys: std::collections::BTreeSet<_> = lm.keys().chain(rm.keys()).cloned().collect();
    let inventory_complete = report.complete;
    for relative in keys {
        let a = lm.get(&relative);
        let b = rm.get(&relative);
        let paired_files =
            a.is_some_and(|x| x.kind == Kind::File) && b.is_some_and(|x| x.kind == Kind::File);
        let difference = match (a, b) {
            (Some(a), Some(b)) if a.kind != b.kind => Difference::TypeConflict,
            (Some(a), Some(b)) if a.kind == Kind::File => {
                match operand(&l, &relative).and_then(|lf| {
                    operand(&r, &relative)
                        .and_then(|rf| compare(&lf.file, &rf.file, a.stamp, b.stamp, &mut budget))
                }) {
                    Ok(true) => Difference::SameFile,
                    Ok(false) => Difference::Changed,
                    Err(e) => {
                        report.complete = false;
                        Difference::Error(e)
                    }
                }
            }
            (Some(a), Some(_)) if a.kind == Kind::Directory => Difference::SameDirectory,
            (Some(a), Some(_)) if a.kind == Kind::Link => Difference::SymlinkSkipped,
            (Some(_), Some(_)) => {
                report.complete = false;
                Difference::Error("Special file is not compared".into())
            }
            (None, Some(_)) if inventory_complete => Difference::Added,
            (Some(_), None) if inventory_complete => Difference::Removed,
            _ => Difference::Error("Presence unknown: sibling inventory incomplete".into()),
        };
        report.entries.push(Entry {
            relative,
            difference,
            paired_files,
        });
    }
    report
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests {
    use super::*;
    use std::fs;
    fn run(left: &Path, right: &Path) -> Report {
        scan(
            left.into(),
            right.into(),
            Limits::default(),
            &AtomicBool::new(false),
        )
    }
    fn status<'a>(report: &'a Report, path: &str) -> &'a Difference {
        &report
            .entries
            .iter()
            .find(|x| x.relative == Path::new(path))
            .unwrap()
            .difference
    }
    #[test]
    fn recursive_unicode_empty_and_presence_are_distinguished() {
        let l = tempfile::tempdir().unwrap();
        let r = tempfile::tempdir().unwrap();
        for root in [l.path(), r.path()] {
            fs::create_dir(root.join("empty")).unwrap();
            fs::create_dir(root.join("λ🙂")).unwrap();
            fs::write(root.join("λ🙂/same"), b"same").unwrap();
        }
        fs::write(l.path().join("λ🙂/changed"), b"before").unwrap();
        fs::write(r.path().join("λ🙂/changed"), b"after").unwrap();
        fs::write(l.path().join("removed"), b"r").unwrap();
        fs::write(r.path().join("added"), b"a").unwrap();
        let report = run(l.path(), r.path());
        assert!(report.complete);
        assert_eq!(status(&report, "empty"), &Difference::SameDirectory);
        assert_eq!(status(&report, "λ🙂/same"), &Difference::SameFile);
        assert_eq!(status(&report, "λ🙂/changed"), &Difference::Changed);
        assert_eq!(status(&report, "removed"), &Difference::Removed);
        assert_eq!(status(&report, "added"), &Difference::Added);
    }
    /// Creates a directory link at `link`; on Windows also tries a symbolic link, which
    /// needs a privilege the account may lack, so a junction is always made as well.
    fn links(root: &Path, target: &Path) -> Vec<&'static str> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, root.join("link")).unwrap();
            vec!["link"]
        }
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(root.join("junction"))
                .arg(target)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "mklink /J failed");
            let mut made = vec!["junction"];
            if std::os::windows::fs::symlink_dir(target, root.join("link")).is_ok() {
                made.push("link");
            }
            made
        }
    }
    #[test]
    fn type_conflicts_and_symlinks_are_never_followed_or_called_equal() {
        let l = tempfile::tempdir().unwrap();
        let r = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"secret").unwrap();
        fs::write(l.path().join("kind"), b"file").unwrap();
        fs::create_dir(r.path().join("kind")).unwrap();
        let made = links(l.path(), outside.path());
        assert_eq!(links(r.path(), outside.path()), made);
        let report = run(l.path(), r.path());
        assert!(report.complete, "{:?}", report.entries);
        assert_eq!(status(&report, "kind"), &Difference::TypeConflict);
        for link in made {
            assert_eq!(status(&report, link), &Difference::SymlinkSkipped);
            assert!(!report
                .entries
                .iter()
                .any(|x| x.relative.starts_with(Path::new(link).join("secret"))));
        }
        // A linked root is refused rather than followed.
        let holder = tempfile::tempdir().unwrap();
        for link in links(holder.path(), l.path()) {
            let report = run(&holder.path().join(link), r.path());
            assert!(!report.complete);
            assert!(!report
                .entries
                .iter()
                .any(|x| x.relative == Path::new("kind")));
        }
    }
    #[test]
    fn missing_root_cancellation_and_limits_are_incomplete_not_equal() {
        let l = tempfile::tempdir().unwrap();
        let r = tempfile::tempdir().unwrap();
        fs::write(l.path().join("a"), b"a").unwrap();
        fs::write(r.path().join("a"), b"a").unwrap();
        assert!(!run(l.path(), &r.path().join("missing")).complete);
        let cancelled = scan(
            l.path().into(),
            r.path().into(),
            Limits::default(),
            &AtomicBool::new(true),
        );
        assert!(!cancelled.complete);
        assert!(cancelled.entries.iter().any(
            |x| matches!(&x.difference,Difference::Error(message) if message.contains("Cancelled"))
        ));
        let limited = scan(
            l.path().into(),
            r.path().into(),
            Limits {
                entries: 0,
                ..Limits::default()
            },
            &AtomicBool::new(false),
        );
        assert!(!limited.complete);
        assert!(!limited.entries.iter().any(|x| matches!(
            x.difference,
            Difference::Added | Difference::Removed | Difference::SameFile
        )));
        let bytes = scan(
            l.path().into(),
            r.path().into(),
            Limits {
                bytes: 1,
                ..Limits::default()
            },
            &AtomicBool::new(false),
        );
        assert!(!bytes.complete);
        assert!(matches!(status(&bytes, "a"), Difference::Error(_)));
    }
    #[test]
    fn changed_during_stream_is_error_even_when_first_chunks_match() {
        let l = tempfile::NamedTempFile::new().unwrap();
        let r = tempfile::NamedTempFile::new().unwrap();
        fs::write(l.path(), vec![b'x'; 65536]).unwrap();
        fs::write(r.path(), vec![b'x'; 65536]).unwrap();
        let lf = File::open(l.path()).unwrap();
        let rf = File::open(r.path()).unwrap();
        let a = stamp(&lf.metadata().unwrap());
        let b = stamp(&rf.metadata().unwrap());
        let cancel = AtomicBool::new(false);
        let mut budget = Budget {
            limits: Limits::default(),
            started: Instant::now(),
            cancel: &cancel,
            entries: 0,
            bytes: 0,
        };
        let mut changed = false;
        let result = compare_with(&lf, &rf, a, b, &mut budget, || {
            if !changed {
                fs::write(r.path(), b"replaced content").unwrap();
                changed = true;
            }
        });
        assert!(result.unwrap_err().contains("changed during"));
    }
    #[cfg(unix)]
    #[test]
    fn unopenable_special_operand_is_explicit_error_not_same() {
        let l = tempfile::tempdir().unwrap();
        let r = tempfile::tempdir().unwrap();
        for root in [l.path(), r.path()] {
            let path = root.join("pipe");
            let c =
                std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()))
                    .unwrap();
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        }
        let report = run(l.path(), r.path());
        assert!(!report.complete);
        assert!(matches!(status(&report, "pipe"), Difference::Error(_)));
    }
    #[cfg(unix)]
    #[test]
    fn unreadable_directory_is_reported_when_permission_is_enforced() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        } // Root bypasses mode bits; permission denial cannot be manufactured.
        let l = tempfile::tempdir().unwrap();
        let r = tempfile::tempdir().unwrap();
        let secret = l.path().join("secret");
        fs::create_dir(&secret).unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o0)).unwrap();
        let report = run(l.path(), r.path());
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!report.complete);
        assert!(report
            .entries
            .iter()
            .any(|x| matches!(x.difference, Difference::Error(_))));
    }
}

#[cfg(all(test, not(any(target_os = "linux", target_os = "macos", windows))))]
mod unsupported_platform_tests {
    use super::*;
    use std::fs;
    #[test]
    fn existing_regular_roots_remain_explicitly_unsupported_not_empty_equal() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        fs::write(left.path().join("π🙂"), b"same").unwrap();
        fs::write(right.path().join("π🙂"), b"same").unwrap();
        let result = scan(
            left.path().into(),
            right.path().into(),
            Limits::default(),
            &AtomicBool::new(false),
        );
        assert!(!result.complete);
        assert_eq!(result.entries.len(), 1);
        assert!(!result.entries[0].paired_files);
        assert!(
            matches!(&result.entries[0].difference, Difference::Error(message) if message.contains("not available on this system"))
        );
    }
}
