//! Explicit, local diagnostic bundles and panic reports. Only RustPy logs and
//! crash reports are collected: settings, environment variables and projects
//! are excluded. Nothing is uploaded. Logs can contain paths or server details;
//! users should inspect an archive before sharing it.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::{write::GzEncoder, Compression};
use serde::Serialize;

const LOG_BYTES: u64 = 2 * 1024 * 1024;
const CRASH_BYTES: u64 = 256 * 1024;
const TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const CRASH_FILES: usize = 20;
const SCAN_ENTRIES: usize = 256;
const MESSAGE_BYTES: usize = 8 * 1024;
const BACKTRACE_BYTES: usize = 128 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static RUNTIME_MODE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

#[derive(Clone, Debug, Serialize)]
pub struct CollectionIssue {
    pub name: String,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct BundleReport {
    pub path: PathBuf,
    /// Logs and crash reports, excluding the manifest itself.
    pub collected_files: usize,
    pub issues: Vec<CollectionIssue>,
}

#[derive(Serialize)]
struct IncludedFile {
    name: String,
    source_bytes: u64,
    included_bytes: u64,
    truncated: bool,
}

#[derive(Serialize)]
struct Manifest<'a> {
    schema_version: u32,
    version: &'static str,
    os: &'static str,
    arch: &'static str,
    process_id: u32,
    collected_unix_seconds: u64,
    runtime_mode: &'static str,
    log_tail_limit_bytes: u64,
    crash_limit_bytes: u64,
    total_limit_bytes: u64,
    files: &'a [IncludedFile],
    issues: &'a [CollectionIssue],
    excluded: &'static [&'static str],
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn unique_name(prefix: &str, extension: &str) -> String {
    format!(
        "{prefix}-{:039}-{:010}-{:020}.{}",
        timestamp_nanos(),
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
        extension
    )
}

pub fn default_destination(log_dir: &Path) -> PathBuf {
    log_dir
        .join("diagnostics")
        .join(unique_name("rustpy-diagnostics", "tar.gz"))
}

pub(crate) fn set_runtime_mode(mode: &'static str) {
    let _ = RUNTIME_MODE.set(mode);
}

fn issue(issues: &mut Vec<CollectionIssue>, name: &str, reason: impl ToString) {
    // The directory walk is also bounded, but keep manifest errors bounded
    // independently of unusual filesystem failures.
    if issues.len() < 64 {
        issues.push(CollectionIssue {
            name: name.into(),
            reason: truncate(&reason.to_string(), 1024),
        });
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.into();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &text[..end])
}

#[cfg(not(unix))]
fn regular_file(path: &Path) -> io::Result<File> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file (symlinks are excluded)",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Prevent a raced symlink or FIFO from following secrets or blocking.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: open the link itself, never its target.
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if file.metadata()?.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reparse points are excluded",
            ));
        }
    }
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

// Pin directory identity on Unix: leaf O_NOFOLLOW alone does not prevent
// replacing a parent directory with a link between enumeration and opening.
struct SourceDirectory {
    path: PathBuf,
    #[cfg(any(unix, windows))]
    handle: File,
}

impl SourceDirectory {
    fn open(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let handle = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(path)?;
            Ok(Self {
                path: path.into(),
                handle,
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            // Keep the directory open without FILE_SHARE_DELETE so it cannot
            // be renamed/replaced while path-based Windows reads run.
            let handle = OpenOptions::new()
                .read(true)
                .share_mode(3)
                .custom_flags(0x0200_0000 | 0x0020_0000)
                .open(path)?;
            let metadata = handle.metadata()?;
            if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
                return Err(io::Error::other("not a regular directory"));
            }
            Ok(Self {
                path: path.into(),
                handle,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(io::Error::other("not a regular directory"));
            }
            Ok(Self { path: path.into() })
        }
    }

    #[cfg(unix)]
    fn open_child(&self, name: &std::ffi::OsStr, flags: i32) -> io::Result<File> {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        };
        let name = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| io::Error::other("invalid filename"))?;
        // SAFETY: directory and C string remain live through openat. A
        // successful descriptor is transferred exactly once to File.
        let fd = unsafe {
            libc::openat(
                self.handle.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn child_directory(&self, name: &str) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                path: self.path.join(name),
                handle: self.open_child(name.as_ref(), libc::O_RDONLY | libc::O_DIRECTORY)?,
            })
        }
        #[cfg(not(unix))]
        {
            Self::open(&self.path.join(name))
        }
    }

    fn file(&self, name: &std::ffi::OsStr) -> io::Result<File> {
        #[cfg(unix)]
        {
            let file = self.open_child(name, libc::O_RDONLY)?;
            if !file.metadata()?.is_file() {
                return Err(io::Error::other("not a regular file"));
            }
            Ok(file)
        }
        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            let _pin = &self.handle;
            regular_file(&self.path.join(name))
        }
    }
}

fn append(builder: &mut tar::Builder<GzEncoder<File>>, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    builder.append_data(&mut header, name, bytes)
}

/// Collects bounded snapshots of the four log files and up to twenty recent
/// local crash reports. Per-file failures and truncation appear in report.json
/// and the returned issues. Output failures are errors; existing destinations
/// are never overwritten, and a failed archive leaves no partial destination.
pub fn collect(log_dir: &Path, output: Option<&Path>) -> io::Result<BundleReport> {
    log::logger().flush();
    let destination = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_destination(log_dir));
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    let file = temporary.reopen()?;
    let mut builder = tar::Builder::new(GzEncoder::new(file, Compression::fast()));
    let mut issues = Vec::new();
    let source = SourceDirectory::open(log_dir);
    if let Err(e) = &source {
        if e.kind() != io::ErrorKind::NotFound {
            issue(&mut issues, "logs", e);
        }
    }
    let mut candidates = Vec::new();
    for name in ["rustpy.log", "rustpy.1.log", "rustpy.2.log", "rustpy.3.log"] {
        candidates.push((format!("logs/{name}"), log_dir.join(name), LOG_BYTES, true));
    }
    let crash_source = source
        .as_ref()
        .ok()
        .map(|directory| directory.child_directory("crashes"));
    match crash_source.as_ref() {
        Some(Ok(directory)) => match std::fs::read_dir(&directory.path) {
            Ok(entries) => {
                let mut crashes = Vec::new();
                for (index, entry) in entries.take(SCAN_ENTRIES + 1).enumerate() {
                    if index == SCAN_ENTRIES {
                        issue(
                            &mut issues,
                            "crashes",
                            "directory scan limit reached; further entries omitted",
                        );
                        break;
                    }
                    match entry {
                        Ok(entry) => {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            if crash_name(&name) {
                                crashes.push((name, entry.path()));
                            }
                        }
                        Err(e) => issue(&mut issues, "crashes", e),
                    }
                }
                // Generated names use fixed-width timestamps, so descending
                // lexical order chooses the newest reports deterministically.
                crashes.sort_by(|a, b| b.0.cmp(&a.0));
                if crashes.len() > CRASH_FILES {
                    issue(
                        &mut issues,
                        "crashes",
                        format!("{} older reports omitted", crashes.len() - CRASH_FILES),
                    );
                }
                candidates.extend(
                    crashes
                        .into_iter()
                        .take(CRASH_FILES)
                        .map(|(name, path)| (format!("crashes/{name}"), path, CRASH_BYTES, false)),
                );
            }
            Err(e) => issue(&mut issues, "crashes", e),
        },
        Some(Err(e)) if e.kind() == io::ErrorKind::NotFound => {}
        Some(Err(e)) => issue(&mut issues, "crashes", e),
        None => {}
    }
    let mut files = Vec::new();
    let mut remaining = TOTAL_BYTES;
    for (name, path, limit, tail) in candidates {
        let directory = if tail {
            source.as_ref().ok()
        } else {
            crash_source.as_ref().and_then(|s| s.as_ref().ok())
        };
        let Some(directory) = directory else { continue };
        let mut file = match directory.file(path.file_name().unwrap()) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                issue(&mut issues, &name, e);
                continue;
            }
        };
        let source_bytes = match file.metadata() {
            Ok(m) => m.len(),
            Err(e) => {
                issue(&mut issues, &name, e);
                continue;
            }
        };
        if !tail && source_bytes > limit.min(remaining) {
            issue(
                &mut issues,
                &name,
                "crash report exceeds the size limit; omitted rather than truncating JSON",
            );
            continue;
        }
        let length = source_bytes.min(limit).min(remaining);
        let offset = if tail { source_bytes - length } else { 0 };
        let mut bytes = Vec::new();
        if let Err(e) = file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| file.take(length).read_to_end(&mut bytes))
        {
            issue(&mut issues, &name, e);
            continue;
        }
        if !tail && serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
            issue(&mut issues, &name, "invalid crash report JSON; omitted");
            continue;
        }
        let truncated = source_bytes > bytes.len() as u64;
        if truncated {
            issue(
                &mut issues,
                &name,
                format!(
                    "{} of {source_bytes} bytes included{}",
                    bytes.len(),
                    if tail { " (log tail)" } else { "" }
                ),
            );
        }
        append(&mut builder, &name, &bytes)?;
        remaining -= bytes.len() as u64;
        files.push(IncludedFile {
            name,
            source_bytes,
            included_bytes: bytes.len() as u64,
            truncated,
        });
    }
    if files.is_empty() {
        issue(&mut issues, "logs", "no log or crash files were available");
    }
    let manifest = Manifest {
        schema_version: 1,
        version: crate::help::VERSION,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        process_id: std::process::id(),
        collected_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        runtime_mode: RUNTIME_MODE.get().copied().unwrap_or("unknown"),
        log_tail_limit_bytes: LOG_BYTES,
        crash_limit_bytes: CRASH_BYTES,
        total_limit_bytes: TOTAL_BYTES,
        files: &files,
        issues: &issues,
        excluded: &[
            "environment variables",
            "settings",
            "credentials databases",
            "project files",
        ],
    };
    append(
        &mut builder,
        "report.json",
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    let encoder = builder.into_inner()?;
    let file = encoder.finish()?;
    file.sync_all()?;
    drop(file);
    temporary
        .persist_noclobber(&destination)
        .map_err(|e| e.error)?;
    Ok(BundleReport {
        path: destination,
        collected_files: files.len(),
        issues,
    })
}

fn crash_name(name: &str) -> bool {
    let Some(body) = name
        .strip_prefix("crash-")
        .and_then(|s| s.strip_suffix(".json"))
    else {
        return false;
    };
    let parts: Vec<_> = body.split('-').collect();
    parts.len() == 3
        && parts[0].len() == 39
        && parts[1].len() == 10
        && parts[2].len() == 20
        && parts.iter().all(|s| s.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Serialize)]
struct CrashReport {
    schema_version: u32,
    version: &'static str,
    os: &'static str,
    arch: &'static str,
    process_id: u32,
    runtime_mode: &'static str,
    crashed_unix_seconds: u64,
    thread: String,
    message: String,
    location: Option<String>,
    backtrace: String,
}

/// Installs once, before dispatching any launcher mode. The previous hook
/// continues to report to stderr; this hook never writes stdout or uploads.
/// It writes directly rather than calling the logger, which may be locked by
/// the panicking thread. Reporting failures do not replace the original panic.
pub fn install_panic_hook() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = write_crash_report(&rustpy_settings::Settings::log_dir(), info);
            previous(info);
        }));
    });
}

fn write_crash_report(log_dir: &Path, info: &std::panic::PanicHookInfo<'_>) -> io::Result<PathBuf> {
    let directory = log_dir.join("crashes");
    std::fs::create_dir_all(&directory)?;
    if std::fs::symlink_metadata(&directory)?
        .file_type()
        .is_symlink()
    {
        return Err(io::Error::other("crash directory must not be a symlink"));
    }
    let path = directory.join(unique_name("crash", "json"));
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    let thread = std::thread::current();
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    let mut report = CrashReport {
        schema_version: 1,
        version: crate::help::VERSION,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        process_id: std::process::id(),
        runtime_mode: RUNTIME_MODE.get().copied().unwrap_or("launcher"),
        crashed_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        thread: truncate(thread.name().unwrap_or("unnamed"), 256),
        message: truncate(message, MESSAGE_BYTES),
        location: info
            .location()
            .map(|l| truncate(&format!("{}:{}:{}", l.file(), l.line(), l.column()), 1024)),
        backtrace: truncate(
            &std::backtrace::Backtrace::force_capture().to_string(),
            BACKTRACE_BYTES,
        ),
    };
    let mut bytes = serde_json::to_vec_pretty(&report)?;
    // JSON escaping can expand control characters. Keep a complete JSON
    // report under the same cap even with unusual panic messages/paths.
    if bytes.len() as u64 > CRASH_BYTES {
        report.backtrace = truncate(&report.backtrace, 32 * 1024);
        bytes = serde_json::to_vec_pretty(&report)?;
    }
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(&path).map_err(|e| e.error)?;
    prune_crashes(&directory);
    Ok(path)
}

fn prune_crashes(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut reports: Vec<_> = entries
        .take(SCAN_ENTRIES)
        .flatten()
        .filter(|entry| {
            crash_name(&entry.file_name().to_string_lossy())
                && entry.file_type().is_ok_and(|kind| kind.is_file())
        })
        .collect();
    reports.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for entry in reports.into_iter().skip(CRASH_FILES) {
        let _ = std::fs::remove_file(entry.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn archive(path: &Path) -> BTreeMap<String, Vec<u8>> {
        let decoder = flate2::read::GzDecoder::new(File::open(path).unwrap());
        let mut archive = tar::Archive::new(decoder);
        archive
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                let name = entry.path().unwrap().to_string_lossy().into_owned();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                (name, bytes)
            })
            .collect()
    }

    fn crash_fixture(index: u64) -> String {
        format!("crash-{index:039}-{:010}-{:020}.json", 1, 1)
    }

    #[test]
    fn bundle_contains_real_logs_crashes_and_platform_manifest_without_settings() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("log");
        std::fs::create_dir_all(logs.join("crashes")).unwrap();
        std::fs::write(logs.join("rustpy.log"), "recent log context\n").unwrap();
        std::fs::write(logs.join("rustpy.1.log"), "older log context\n").unwrap();
        std::fs::write(logs.join("settings.json"), "TOP_SECRET_SETTINGS_VALUE").unwrap();
        let crash = crash_fixture(1);
        std::fs::write(
            logs.join("crashes").join(&crash),
            r#"{"message":"crash context"}"#,
        )
        .unwrap();
        let report = collect(&logs, Some(&dir.path().join("bundle.tar.gz"))).unwrap();
        assert_eq!(report.collected_files, 3);
        assert!(report.issues.is_empty());
        let contents = archive(&report.path);
        assert_eq!(contents["logs/rustpy.log"], b"recent log context\n");
        assert_eq!(contents["logs/rustpy.1.log"], b"older log context\n");
        assert_eq!(
            contents[&format!("crashes/{crash}")],
            br#"{"message":"crash context"}"#
        );
        assert!(contents.values().all(|bytes| !bytes
            .windows(b"TOP_SECRET_SETTINGS_VALUE".len())
            .any(|w| w == b"TOP_SECRET_SETTINGS_VALUE")));
        let manifest: serde_json::Value = serde_json::from_slice(&contents["report.json"]).unwrap();
        assert_eq!(manifest["version"], crate::help::VERSION);
        assert_eq!(manifest["os"], std::env::consts::OS);
        assert_eq!(manifest["arch"], std::env::consts::ARCH);
        assert_eq!(manifest["files"].as_array().unwrap().len(), 3);
        assert_eq!(manifest["issues"], serde_json::json!([]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(report.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn collection_bounds_log_tails_and_keeps_crash_json_complete() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("crashes")).unwrap();
        let mut log = vec![b'x'; LOG_BYTES as usize];
        log.extend(vec![b'y'; LOG_BYTES as usize]);
        std::fs::write(dir.path().join("rustpy.log"), &log).unwrap();
        std::fs::write(dir.path().join("rustpy.1.log"), b"old context").unwrap();
        std::fs::write(
            dir.path().join("crashes").join(crash_fixture(1)),
            vec![b'x'; CRASH_BYTES as usize + 1],
        )
        .unwrap();
        let report = collect(dir.path(), None).unwrap();
        let contents = archive(&report.path);
        assert_eq!(contents["logs/rustpy.log"], vec![b'y'; LOG_BYTES as usize]);
        assert_eq!(report.collected_files, 2);
        assert!(report.issues.iter().any(|i| i.reason.contains("log tail")));
        assert!(report
            .issues
            .iter()
            .any(|i| i.reason.contains("rather than truncating JSON")));
        let manifest: serde_json::Value = serde_json::from_slice(&contents["report.json"]).unwrap();
        assert_eq!(manifest["files"][0]["source_bytes"], log.len());
        assert_eq!(manifest["files"][0]["included_bytes"], LOG_BYTES);
        assert_eq!(manifest["files"][0]["truncated"], true);
    }

    #[test]
    fn collection_is_no_clobber_and_failed_output_leaves_no_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("existing.tar.gz");
        std::fs::write(&output, "keep this file").unwrap();
        assert_eq!(
            collect(&dir.path().join("missing"), Some(&output))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read_to_string(output).unwrap(), "keep this file");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "temporary archives are removed"
        );
        let report = collect(
            &dir.path().join("missing"),
            Some(&dir.path().join("empty.tar.gz")),
        )
        .unwrap();
        assert_eq!(report.collected_files, 0);
        assert!(report
            .issues
            .iter()
            .any(|i| i.reason.contains("no log or crash")));
        assert_eq!(
            archive(&report.path).keys().collect::<Vec<_>>(),
            [&"report.json".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_directories_and_fifos_are_omitted_without_reading_secrets_or_blocking() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(&secret, "TOP_SECRET_SYMLINK_VALUE").unwrap();
        symlink(&secret, dir.path().join("rustpy.log")).unwrap();
        std::fs::create_dir(dir.path().join("rustpy.1.log")).unwrap();
        let fifo =
            std::ffi::CString::new(dir.path().join("rustpy.2.log").as_os_str().as_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path, private fixture directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        symlink(dir.path(), dir.path().join("crashes")).unwrap();
        let report = collect(dir.path(), None).unwrap();
        assert_eq!(report.collected_files, 0);
        assert!(report.issues.len() >= 4);
        let contents = archive(&report.path);
        assert!(!contents["report.json"]
            .windows(b"TOP_SECRET_SYMLINK_VALUE".len())
            .any(|w| w == b"TOP_SECRET_SYMLINK_VALUE"));
    }

    #[cfg(unix)]
    #[test]
    fn pinned_directory_cannot_be_redirected_by_parent_replacement() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        let external = dir.path().join("external");
        std::fs::create_dir(&logs).unwrap();
        std::fs::create_dir(&external).unwrap();
        std::fs::write(logs.join("rustpy.log"), "owned context").unwrap();
        std::fs::write(external.join("rustpy.log"), "external secret").unwrap();
        let source = SourceDirectory::open(&logs).unwrap();
        std::fs::rename(&logs, dir.path().join("moved")).unwrap();
        symlink(&external, &logs).unwrap();
        let mut bytes = String::new();
        source
            .file("rustpy.log".as_ref())
            .unwrap()
            .read_to_string(&mut bytes)
            .unwrap();
        assert_eq!(bytes, "owned context");
        assert!(SourceDirectory::open(&logs).is_err());
    }

    #[test]
    fn corrupt_crash_json_is_omitted_with_an_honest_notice() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("crashes")).unwrap();
        std::fs::write(dir.path().join("crashes").join(crash_fixture(1)), "{broken").unwrap();
        let report = collect(dir.path(), None).unwrap();
        assert_eq!(report.collected_files, 0);
        assert!(report
            .issues
            .iter()
            .any(|i| i.reason.contains("invalid crash report JSON")));
        assert_eq!(archive(&report.path).len(), 1);
    }

    #[test]
    fn recent_crash_count_and_directory_scan_are_bounded_with_notices() {
        let dir = tempfile::tempdir().unwrap();
        let crashes = dir.path().join("crashes");
        std::fs::create_dir(&crashes).unwrap();
        for index in 1..=25 {
            std::fs::write(crashes.join(crash_fixture(index)), "{}").unwrap();
        }
        let report = collect(dir.path(), None).unwrap();
        assert_eq!(report.collected_files, CRASH_FILES);
        assert!(report
            .issues
            .iter()
            .any(|i| i.reason.contains("5 older reports")));
        let contents = archive(&report.path);
        assert!(contents.contains_key(&format!("crashes/{}", crash_fixture(25))));
        assert!(!contents.contains_key(&format!("crashes/{}", crash_fixture(1))));
        for index in 0..=SCAN_ENTRIES {
            std::fs::write(crashes.join(format!("unrelated-{index}")), "").unwrap();
        }
        let report = collect(dir.path(), None).unwrap();
        assert!(report
            .issues
            .iter()
            .any(|i| i.reason.contains("scan limit")));
    }

    #[test]
    fn panic_probe_child() {
        if std::env::var_os("RUSTPY_TEST_DIAGNOSTIC_PANIC").is_none() {
            return;
        }
        set_runtime_mode("lsp");
        install_panic_hook();
        std::io::stdout().write_all(b"protocol-marker\n").unwrap();
        panic!(
            "diagnostic panic regression {}",
            "\0".repeat(MESSAGE_BYTES * 2)
        );
    }

    #[test]
    fn panic_hook_writes_a_private_bounded_local_report_and_survives_report_failure() {
        let dir = tempfile::tempdir().unwrap();
        let crashes = dir.path().join("crashes");
        std::fs::create_dir(&crashes).unwrap();
        for index in 0..20 {
            std::fs::write(crashes.join(crash_fixture(index)), "{}").unwrap();
        }
        let child = |log_dir: &Path| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "diagnostics::tests::panic_probe_child",
                    "--nocapture",
                ])
                .env("RUSTPY_TEST_DIAGNOSTIC_PANIC", "1")
                .env("RUSTPY_LOG_DIR", log_dir)
                .output()
                .unwrap()
        };
        let result = child(dir.path());
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stdout).contains("protocol-marker"));
        assert!(
            !String::from_utf8_lossy(&result.stdout).contains("diagnostic panic regression"),
            "panic hook must not write to protocol stdout"
        );
        assert!(String::from_utf8_lossy(&result.stderr).contains("diagnostic panic regression"));
        let reports: Vec<_> = std::fs::read_dir(&crashes).unwrap().flatten().collect();
        assert_eq!(reports.len(), 20, "old reports are pruned");
        let latest = reports
            .into_iter()
            .max_by_key(|entry| entry.file_name())
            .unwrap()
            .path();
        let bytes = std::fs::read(&latest).unwrap();
        assert!(bytes.len() as u64 <= CRASH_BYTES);
        let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(report["runtime_mode"], "lsp");
        assert!(report["message"].as_str().unwrap().ends_with("[truncated]"));
        assert!(report["location"]
            .as_str()
            .unwrap()
            .contains("diagnostics.rs"));
        assert!(!report["backtrace"].as_str().unwrap().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(latest).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        #[cfg(unix)]
        {
            let linked_logs = dir.path().join("linked-logs");
            let external = dir.path().join("external");
            std::fs::create_dir(&linked_logs).unwrap();
            std::fs::create_dir(&external).unwrap();
            std::os::unix::fs::symlink(&external, linked_logs.join("crashes")).unwrap();
            let result = child(&linked_logs);
            assert!(!result.status.success());
            assert!(String::from_utf8_lossy(&result.stderr).contains("diagnostic panic regression"));
            assert_eq!(std::fs::read_dir(external).unwrap().count(), 0);
        }
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, "keep me").unwrap();
        let result = child(&blocked);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("diagnostic panic regression"));
        assert_eq!(std::fs::read_to_string(blocked).unwrap(), "keep me");
    }
}
