//! Local History storage (independent of VCS): snapshots of file contents under
//! `<config>/local_history/<project-hash>/<file-hash>/<unix-ms>_<label>.txt`.
//!
//! Each file directory also holds `meta.json` with the file's path, so a folder's
//! history (including files deleted since) can be listed and restored. Snapshots
//! are taken from editors (save, reformat, refactorings) and, with
//! [`snapshot_paths`], from disk before destructive operations: Git rollback,
//! reset --hard, stash, checkout, pull, merge abort, and deletes.

use std::path::{Path, PathBuf};

use rustpy_settings::Settings;
use serde::{Deserialize, Serialize};
mod timeline;
pub use timeline::*;

static POLICY: std::sync::RwLock<rustpy_settings::local_history::LocalHistorySettings> =
    std::sync::RwLock::new(rustpy_settings::local_history::LocalHistorySettings {
        enabled: true,
        retention_days: 7,
        max_size_mb: 512,
        snapshots_per_file: KEEP as u16,
    });
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub fn set_policy(policy: &rustpy_settings::local_history::LocalHistorySettings) {
    if let Ok(mut current) = POLICY.write() {
        if *current != *policy {
            *current = policy.clone();
        }
    }
}
fn policy() -> rustpy_settings::local_history::LocalHistorySettings {
    POLICY
        .read()
        .map(|policy| policy.clone())
        .unwrap_or_default()
}

/// Snapshots kept per file.
const KEEP: usize = 100;
/// Files larger than this are not snapshotted from disk.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Upper bounds for one [`snapshot_paths`] call (a huge folder delete stays fast).
const MAX_FILES_PER_CALL: usize = 5000;
const MAX_BYTES_PER_CALL: usize = 64 * 1024 * 1024;
/// Directories never snapshotted (also when deleted themselves); virtual
/// environments are recognized by their `pyvenv.cfg` / `conda-meta`, not by name.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".p4root",
    "site-packages",
    "node_modules",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
];

fn is_skipped_dir(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| SKIP_DIRS.contains(&n))
        || p.join("pyvenv.cfg").is_file()
        || p.join("conda-meta").is_dir()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub file: PathBuf,
    pub label: String,
    /// Unix time in milliseconds.
    pub time: i64,
}

/// A file with Local History below a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryFile {
    pub path: PathBuf,
    /// False when the file was deleted (or moved) since.
    pub exists: bool,
    /// Directory holding its snapshots.
    pub dir: PathBuf,
    pub newest: Option<HistoryEntry>,
}

#[derive(Serialize, Deserialize)]
struct Meta {
    path: PathBuf,
    /// The last snapshot was taken because the file was about to be deleted.
    #[serde(default)]
    deleted: bool,
}

/// `<system>/local_history` (the system directory is the configuration
/// directory unless `--system-dir` / `RUSTPY_SYSTEM_DIR` moves it).
pub fn base_dir() -> PathBuf {
    Settings::system_dir().join("local_history")
}

fn hash_path(p: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Snapshot directory of one project (`no-project` for files outside a project).
pub fn project_dir(base: &Path, root: Option<&Path>) -> PathBuf {
    base.join(root.map(hash_path).unwrap_or_else(|| "no-project".into()))
}

/// Snapshot directory of `path`.
pub fn file_dir(base: &Path, root: Option<&Path>, path: &Path) -> PathBuf {
    project_dir(base, root).join(hash_path(path))
}

/// Snapshots in `dir`, newest first.
pub fn list_entries(dir: &Path) -> Vec<HistoryEntry> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("txt") {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some((ts, label)) = stem.split_once('_') else {
                continue;
            };
            let Ok(time) = ts.parse::<i64>() else {
                continue;
            };
            out.push(HistoryEntry {
                file: p.clone(),
                label: label.replace('-', " "),
                time,
            });
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.time));
    out
}

/// Stores `text` as the newest snapshot of `path` (skipped when it equals the
/// newest one); keeps the newest [`KEEP`]. Returns whether a snapshot was written.
pub fn snapshot_text(
    base: &Path,
    root: Option<&Path>,
    path: &Path,
    text: &str,
    label: &str,
) -> bool {
    store(base, root, path, text, label, false)
}

fn store(
    base: &Path,
    root: Option<&Path>,
    path: &Path,
    text: &str,
    label: &str,
    deleted: bool,
) -> bool {
    store_with_policy(
        base,
        root,
        path,
        text,
        label,
        SnapshotMode {
            deleted,
            force: false,
        },
        &policy(),
    )
}
struct SnapshotMode {
    deleted: bool,
    force: bool,
}
fn store_with_policy(
    base: &Path,
    root: Option<&Path>,
    path: &Path,
    text: &str,
    label: &str,
    mode: SnapshotMode,
    policy: &rustpy_settings::local_history::LocalHistorySettings,
) -> bool {
    if !policy.enabled || text.len() as u64 > MAX_FILE_BYTES {
        return false;
    }
    let Ok(_guard) = WRITE_LOCK.lock() else {
        return false;
    };
    let dir = file_dir(base, root, path);
    let entries = list_entries(&dir);
    let unchanged = entries.first().is_some_and(|newest| {
        std::fs::read_to_string(&newest.file)
            .map(|t| t == text)
            .unwrap_or(false)
    });
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    // Written every time: it records the path (for folder history) and a pending delete.
    let meta = Meta {
        path: path.to_path_buf(),
        deleted: mode.deleted,
    };
    if let Ok(json) = serde_json::to_string(&meta) {
        let _ = std::fs::write(dir.join("meta.json"), json);
    }
    if unchanged && !mode.force {
        return false;
    }
    let safe_label: String = label
        .chars()
        .take(40)
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let mut ts = chrono::Local::now().timestamp_millis();
    if let Some(newest) = entries.first() {
        ts = ts.max(newest.time + 1);
    }
    let snapshot = dir.join(format!("{ts}_{safe_label}.txt"));
    let staging = snapshot.with_extension("staging");
    if std::fs::write(&staging, text)
        .and_then(|()| std::fs::rename(&staging, &snapshot))
        .is_err()
    {
        let _ = std::fs::remove_file(staging);
        return false;
    }
    let keep = usize::from(policy.snapshots_per_file.clamp(1, 1000));
    for old in entries.iter().skip(keep - 1) {
        let _ = std::fs::remove_file(&old.file);
    }
    true
}

/// A named checkpoint remains visible even when the text has not changed.
pub fn label_text(base: &Path, root: Option<&Path>, path: &Path, text: &str, label: &str) -> bool {
    store_with_policy(
        base,
        root,
        path,
        text,
        label,
        SnapshotMode {
            deleted: false,
            force: true,
        },
        &policy(),
    )
}

/// Snapshots the current disk content of `paths`, descending into directories
/// (skipping VCS, virtualenv and cache folders, binary and very large files).
/// `deleting` marks the snapshots as taken before a delete. Returns the number of
/// snapshots written.
pub fn snapshot_paths(
    base: &Path,
    root: Option<&Path>,
    paths: &[PathBuf],
    label: &str,
    deleting: bool,
) -> usize {
    let mut files = Vec::new();
    for p in paths {
        collect_files(p, &mut files);
        if files.len() >= MAX_FILES_PER_CALL {
            files.truncate(MAX_FILES_PER_CALL);
            log::warn!("Local History: snapshot limited to {MAX_FILES_PER_CALL} files ({label})");
            break;
        }
    }
    let (mut n, mut bytes) = (0, 0usize);
    for f in files {
        if let Some(text) = read_text(&f) {
            bytes += text.len();
            if bytes > MAX_BYTES_PER_CALL {
                log::warn!(
                    "Local History: snapshot limited to {} MB ({label})",
                    MAX_BYTES_PER_CALL / (1024 * 1024)
                );
                break;
            }
            if store(base, root, &f, &text, label, deleting) {
                n += 1;
            }
        }
    }
    n
}

fn collect_files(p: &Path, out: &mut Vec<PathBuf>) {
    if out.len() >= MAX_FILES_PER_CALL {
        return;
    }
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return;
    };
    if meta.is_file() {
        out.push(p.to_path_buf());
    } else if meta.is_dir() {
        if is_skipped_dir(p) {
            return;
        }
        let mut children: Vec<PathBuf> = std::fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        children.sort();
        for c in children {
            collect_files(&c, out);
        }
    }
}

/// Whether Local History can keep `p`'s content: a UTF-8 text file of at most 2 MB.
pub fn can_snapshot(p: &Path) -> bool {
    read_text(p).is_some()
}

/// Text content of a regular file, or None for binary, huge or unreadable files.
fn read_text(p: &Path) -> Option<String> {
    let meta = std::fs::metadata(p).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(p).ok()?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Files with Local History at or below `dir` (the file itself when `dir` is a
/// file), including deleted ones: deleted first, then by path.
pub fn files_under(base: &Path, root: Option<&Path>, dir: &Path) -> Vec<HistoryFile> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(project_dir(base, root)) else {
        return out;
    };
    for e in rd.flatten() {
        let d = e.path();
        let Some(meta) = std::fs::read_to_string(d.join("meta.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Meta>(&t).ok())
        else {
            continue;
        };
        if !meta.path.starts_with(dir) {
            continue;
        }
        let newest = list_entries(&d).into_iter().next();
        if newest.is_none() {
            continue;
        }
        out.push(HistoryFile {
            exists: meta.path.is_file(),
            path: meta.path,
            dir: d,
            newest,
        });
    }
    out.sort_by(|a, b| a.exists.cmp(&b.exists).then_with(|| a.path.cmp(&b.path)));
    out
}

/// Writes snapshot `entry` back to `path` (re-creating missing folders), after
/// snapshotting what is there now.
pub fn restore(
    base: &Path,
    root: Option<&Path>,
    path: &Path,
    entry: &HistoryEntry,
) -> std::io::Result<()> {
    let text = std::fs::read_to_string(&entry.file)?;
    if let Some(current) = read_text(path) {
        snapshot_text(base, root, path, &current, "Before restore");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rustpy-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn snapshot_meta_roundtrip_and_dedup() {
        let t = tmp("meta");
        let base = t.join("history");
        let root = t.join("proj");
        let file = root.join("pkg/mod.py");
        assert!(snapshot_text(&base, Some(&root), &file, "v1\n", "Save"));
        assert!(
            !snapshot_text(&base, Some(&root), &file, "v1\n", "Save"),
            "same text is not stored twice"
        );
        assert!(snapshot_text(
            &base,
            Some(&root),
            &file,
            "v2\n",
            "Before Reformat"
        ));
        let entries = list_entries(&file_dir(&base, Some(&root), &file));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "Before Reformat");
        assert_eq!(std::fs::read_to_string(&entries[1].file).unwrap(), "v1\n");
        assert!(entries[0].time > entries[1].time);
        let files = files_under(&base, Some(&root), &root.join("pkg"));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, file);
        assert!(!files[0].exists);
        assert!(files_under(&base, Some(&root), &root.join("other")).is_empty());
        std::fs::remove_dir_all(t).ok();
    }

    #[test]
    fn deleted_folder_can_be_recovered() {
        let t = tmp("folder");
        let base = t.join("history");
        let root = t.join("proj");
        std::fs::create_dir_all(root.join("app/sub")).unwrap();
        std::fs::create_dir_all(root.join("app/__pycache__")).unwrap();
        std::fs::write(root.join("app/a.py"), "a = 1\n").unwrap();
        std::fs::write(root.join("app/sub/b.py"), "b = 2\n").unwrap();
        std::fs::write(root.join("app/__pycache__/a.pyc"), "cache").unwrap();
        std::fs::write(root.join("app/blob.bin"), [0u8, 1, 2, 3]).unwrap();
        let n = snapshot_paths(
            &base,
            Some(&root),
            &[root.join("app")],
            "Before delete",
            true,
        );
        assert_eq!(n, 2, "text files only, caches skipped");
        // Deleting a virtualenv (recognized by pyvenv.cfg) does not copy it either…
        std::fs::create_dir_all(root.join(".venv/lib")).unwrap();
        std::fs::write(root.join(".venv/pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        std::fs::write(root.join(".venv/lib/x.py"), "x = 1\n").unwrap();
        assert_eq!(
            snapshot_paths(
                &base,
                Some(&root),
                &[root.join(".venv")],
                "Before delete",
                true
            ),
            0
        );
        // …but a source package that is merely named like one is kept.
        std::fs::create_dir_all(root.join("env")).unwrap();
        std::fs::write(root.join("env/settings.py"), "DEBUG = True\n").unwrap();
        assert_eq!(
            snapshot_paths(
                &base,
                Some(&root),
                &[root.join("env")],
                "Before delete",
                true
            ),
            1
        );
        std::fs::remove_dir_all(root.join("app")).unwrap();
        let files = files_under(&base, Some(&root), &root.join("app"));
        assert_eq!(
            files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
            vec![root.join("app/a.py"), root.join("app/sub/b.py")]
        );
        assert!(files.iter().all(|f| !f.exists));
        let entry = files[1].newest.clone().unwrap();
        assert_eq!(entry.label, "Before delete");
        restore(&base, Some(&root), &files[1].path, &entry).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("app/sub/b.py")).unwrap(),
            "b = 2\n"
        );
        std::fs::remove_dir_all(t).ok();
    }

    #[test]
    fn restore_keeps_the_overwritten_content() {
        let t = tmp("restore");
        let base = t.join("history");
        let root = t.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let f = root.join("m.py");
        std::fs::write(&f, "old\n").unwrap();
        snapshot_paths(
            &base,
            Some(&root),
            std::slice::from_ref(&f),
            "Before rollback",
            false,
        );
        std::fs::write(&f, "new\n").unwrap();
        let old = list_entries(&file_dir(&base, Some(&root), &f)).remove(0);
        restore(&base, Some(&root), &f, &old).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "old\n");
        let entries = list_entries(&file_dir(&base, Some(&root), &f));
        assert_eq!(entries[0].label, "Before restore");
        assert_eq!(std::fs::read_to_string(&entries[0].file).unwrap(), "new\n");
        std::fs::remove_dir_all(t).ok();
    }
}
