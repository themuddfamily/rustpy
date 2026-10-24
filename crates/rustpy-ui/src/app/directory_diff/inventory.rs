//! Bounded, cancellation-aware directory inventory. Linux descriptor-relative traversal
//! prevents symlink replacement from redirecting a scan outside its captured roots.
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata, OpenOptions},
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
    #[cfg(not(unix))]
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
#[cfg(target_os = "linux")]
fn open(path: &Path, directory: bool) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(
            libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | if directory {
                    libc::O_DIRECTORY
                } else {
                    libc::O_NONBLOCK
                },
        )
        .open(path)
}
#[cfg(not(target_os = "linux"))]
fn open(_: &Path, _: bool) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Descriptor-relative no-follow inventory is currently available on Linux",
    ))
}
#[cfg(target_os = "linux")]
fn held_path(file: &File) -> PathBuf {
    use std::os::fd::AsRawFd;
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}
#[cfg(not(target_os = "linux"))]
fn held_path(_: &File) -> PathBuf {
    unreachable!()
}
fn walk(
    dir: &File,
    relative: &Path,
    depth: usize,
    map: &mut BTreeMap<PathBuf, Item>,
    budget: &mut Budget<'_>,
) -> Result<(), String> {
    budget.check()?;
    if depth > budget.limits.depth {
        return Err("Depth limit; inventory incomplete".into());
    }
    let before = stamp(&dir.metadata().map_err(|e| e.to_string())?);
    let entries = fs::read_dir(held_path(dir)).map_err(|e| e.to_string())?;
    for entry in entries {
        budget.check()?;
        if budget.entries >= budget.limits.entries {
            return Err("Entry limit; inventory incomplete".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        budget.entries += 1;
        let name = entry.file_name();
        let rel = relative.join(&name);
        let path = held_path(dir).join(name);
        let m = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", rel.display()))?;
        let kind = if m.file_type().is_symlink() {
            Kind::Link
        } else if m.is_dir() {
            Kind::Directory
        } else if m.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        map.insert(
            rel.clone(),
            Item {
                kind,
                stamp: stamp(&m),
            },
        );
        if kind == Kind::Directory {
            let child = open(&path, true).map_err(|e| format!("{}: {e}", rel.display()))?;
            if stamp(&child.metadata().map_err(|e| e.to_string())?) != stamp(&m) {
                return Err("Directory changed during acquisition".into());
            }
            walk(&child, &rel, depth + 1, map, budget)?;
        }
    }
    if stamp(&dir.metadata().map_err(|e| e.to_string())?) != before {
        return Err("Directory entries changed during inventory".into());
    }
    Ok(())
}
/// Open every ancestor relative to an already-held directory, never following links.
fn operand(root: &File, relative: &Path) -> Result<File, String> {
    let mut parent = root.try_clone().map_err(|e| e.to_string())?;
    let parts: Vec<_> = relative.components().collect();
    for (i, part) in parts.iter().enumerate() {
        let std::path::Component::Normal(name) = part else {
            return Err("Invalid relative operand".into());
        };
        parent =
            open(&held_path(&parent).join(name), i + 1 < parts.len()).map_err(|e| e.to_string())?;
    }
    Ok(parent)
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
    let roots = (open(&left, true), open(&right, true));
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
                        .and_then(|rf| compare(&lf, &rf, a.stamp, b.stamp, &mut budget))
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
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
    #[test]
    fn type_conflicts_and_symlinks_are_never_followed_or_called_equal() {
        use std::os::unix::fs::symlink;
        let l = tempfile::tempdir().unwrap();
        let r = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"secret").unwrap();
        fs::write(l.path().join("kind"), b"file").unwrap();
        fs::create_dir(r.path().join("kind")).unwrap();
        for root in [l.path(), r.path()] {
            symlink(outside.path(), root.join("link")).unwrap();
        }
        let report = run(l.path(), r.path());
        assert_eq!(status(&report, "kind"), &Difference::TypeConflict);
        assert_eq!(status(&report, "link"), &Difference::SymlinkSkipped);
        assert!(!report
            .entries
            .iter()
            .any(|x| x.relative.starts_with("link/secret")));
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

#[cfg(all(test, not(target_os = "linux")))]
mod unsupported_platform_tests {
    use super::*;
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
            matches!(&result.entries[0].difference, Difference::Error(message) if message.contains("currently available on Linux"))
        );
    }
}
