//! Sync planning ("Sync with Deployed") and batch transfers ("Upload to…", "Download from…").
//!
//! [`plan_sync`] compares a local tree with a recursive remote listing by size and
//! modification time and proposes one [`SyncAction`] per differing file:
//!
//! | situation | `Upload` | `Download` | `Both` |
//! |---|---|---|---|
//! | only local | Upload | Skip | Upload |
//! | only remote | Skip | Download | Download |
//! | local newer | Upload | Conflict | Upload |
//! | remote newer | Conflict | Download | Download |
//! | same time (or unknown), other size | Upload | Download | Conflict |
//! | same size and time (or unknown time) | — | — | — |
//! | file on one side, directory on the other | Conflict | Conflict | Conflict |
//!
//! [`SyncAction::DeleteRemote`] is never proposed; the UI may offer it for remote-only files.
//! Times within [`SyncOptions::mtime_tolerance_secs`] are equal, and coarse remote times
//! (FTP `LIST` shows minutes, or just the day for old files) match any local time inside that
//! minute/day. Directories themselves are not items (they are created as needed).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::config::default_excluded;
use crate::config::Server;
use crate::glob::ExcludeSet;
use crate::paths::{
    join_remote, local_path, mapping_local_root, mapping_remote_root, normalize_local, push_rel,
    rel_string, remote_parent, remote_path,
};
use crate::{mtime_secs, RemoteEntry, Transport};

/// Which side is the source of truth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    /// Local → remote.
    #[default]
    Upload,
    /// Remote → local.
    Download,
    /// Newer side wins.
    Both,
}

/// What to do with one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncAction {
    Upload,
    Download,
    /// The planner cannot decide (or would overwrite a newer file); the user chooses.
    Conflict,
    /// Delete the remote file. Never chosen automatically.
    DeleteRemote,
    /// Nothing to do; listed so the user can change it (e.g. a remote-only file while uploading).
    Skip,
}

/// One entry of a sync plan. `None` sizes/times mean the file does not exist on that side
/// (or the time is unknown).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncItem {
    /// `/`-separated path relative to the compared root (the project root for
    /// [`plan_server_sync`]).
    pub rel_path: String,
    pub action: SyncAction,
    pub local_mtime: Option<i64>,
    pub remote_mtime: Option<i64>,
    pub local_size: Option<u64>,
    pub remote_size: Option<u64>,
}

impl SyncItem {
    pub fn exists_locally(&self) -> bool {
        self.local_size.is_some()
    }

    pub fn exists_remotely(&self) -> bool {
        self.remote_size.is_some()
    }
}

/// Comparison settings for [`plan_sync_with`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncOptions {
    /// Modification times at most this many seconds apart are considered equal.
    pub mtime_tolerance_secs: i64,
    /// Remote times may be truncated to the minute or day (FTP); a truncated remote
    /// time matching the local time's minute/day counts as equal.
    pub coarse_remote_times: bool,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            mtime_tolerance_secs: 2,
            coarse_remote_times: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Info {
    is_dir: bool,
    size: u64,
    mtime: Option<i64>,
    /// A symlinked local directory (not descended into).
    linked: bool,
}

fn join_rel(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

/// Files and directories below `root` (relative paths), skipping `skip`ped paths and not
/// following symlinked directories.
fn scan_local(root: &Path, skip: &dyn Fn(&str) -> bool) -> BTreeMap<String, Info> {
    let mut out = BTreeMap::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                if !rel.is_empty() || dir.exists() {
                    log::warn!("deployment: cannot read {}: {e}", dir.display());
                }
                continue;
            }
        };
        for de in rd.filter_map(Result::ok) {
            let child = join_rel(&rel, &de.file_name().to_string_lossy());
            if skip(&child) {
                continue;
            }
            let path = de.path();
            let Ok(lmeta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let linked = lmeta.file_type().is_symlink();
            let Ok(meta) = (if linked {
                std::fs::metadata(&path)
            } else {
                Ok(lmeta)
            }) else {
                continue;
            };
            if meta.is_dir() {
                out.insert(
                    child.clone(),
                    Info {
                        is_dir: true,
                        size: 0,
                        mtime: None,
                        linked,
                    },
                );
                if !linked {
                    stack.push((path, child));
                }
            } else if meta.is_file() {
                out.insert(
                    child,
                    Info {
                        is_dir: false,
                        size: meta.len(),
                        mtime: mtime_secs(&meta),
                        linked: false,
                    },
                );
            }
        }
    }
    out
}

/// `./a//b` → `a/b` (remote names are POSIX paths).
fn clean_rel(name: &str) -> String {
    name.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect::<Vec<_>>()
        .join("/")
}

fn remote_map(entries: &[RemoteEntry], skip: &dyn Fn(&str) -> bool) -> BTreeMap<String, Info> {
    entries
        .iter()
        .filter_map(|e| {
            let rel = clean_rel(&e.name);
            (!rel.is_empty() && !skip(&rel)).then_some((
                rel,
                Info {
                    is_dir: e.is_dir,
                    size: e.size,
                    mtime: e.mtime,
                    linked: false,
                },
            ))
        })
        .collect()
}

/// Equal modification times, allowing for `tolerance` and coarse (minute/day) remote times.
fn same_time(local: i64, remote: i64, tolerance: i64, coarse: bool) -> bool {
    if (local - remote).abs() <= tolerance {
        return true;
    }
    if !coarse {
        return false;
    }
    (remote.rem_euclid(86_400) == 0 && local.div_euclid(86_400) * 86_400 == remote)
        || (remote.rem_euclid(60) == 0 && local.div_euclid(60) * 60 == remote)
}

fn file_action(
    l: &Info,
    r: &Info,
    direction: SyncDirection,
    opts: &SyncOptions,
) -> Option<SyncAction> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let order = match (l.mtime, r.mtime) {
        (Some(a), Some(b))
            if same_time(a, b, opts.mtime_tolerance_secs, opts.coarse_remote_times) =>
        {
            None
        }
        (Some(a), Some(b)) => Some(a.cmp(&b)),
        _ => None,
    };
    Some(match (order, direction) {
        (None | Some(Equal), _) if l.size == r.size => return None,
        (None | Some(Equal), SyncDirection::Upload) => SyncAction::Upload,
        (None | Some(Equal), SyncDirection::Download) => SyncAction::Download,
        (None | Some(Equal), SyncDirection::Both) => SyncAction::Conflict,
        (Some(Greater), SyncDirection::Download) => SyncAction::Conflict,
        (Some(Greater), _) => SyncAction::Upload,
        (Some(Less), SyncDirection::Upload) => SyncAction::Conflict,
        (Some(Less), _) => SyncAction::Download,
    })
}

fn compare(
    local: &BTreeMap<String, Info>,
    remote: &BTreeMap<String, Info>,
    direction: SyncDirection,
    opts: &SyncOptions,
) -> Vec<SyncItem> {
    let linked: Vec<String> = local
        .iter()
        .filter(|(_, i)| i.linked)
        .map(|(k, _)| format!("{k}/"))
        .collect();
    let keys: BTreeSet<&String> = local.keys().chain(remote.keys()).collect();
    let mut items = Vec::new();
    for key in keys {
        let (l, r) = (local.get(key), remote.get(key));
        let action = match (l, r) {
            (Some(l), Some(r)) if l.is_dir && r.is_dir => continue,
            (Some(l), Some(r)) if l.is_dir != r.is_dir => SyncAction::Conflict,
            (Some(l), Some(r)) => match file_action(l, r, direction, opts) {
                Some(a) => a,
                None => continue,
            },
            (Some(l), None) if l.is_dir => continue,
            (Some(_), None) if direction == SyncDirection::Download => SyncAction::Skip,
            (Some(_), None) => SyncAction::Upload,
            (None, Some(r)) if r.is_dir || linked.iter().any(|p| key.starts_with(p.as_str())) => {
                continue
            }
            (None, Some(_)) if direction == SyncDirection::Upload => SyncAction::Skip,
            (None, Some(_)) => SyncAction::Download,
            (None, None) => continue,
        };
        items.push(SyncItem {
            rel_path: key.clone(),
            action,
            local_mtime: l.and_then(|i| i.mtime),
            remote_mtime: r.and_then(|i| i.mtime),
            local_size: l.map(|i| i.size),
            remote_size: r.map(|i| i.size),
        });
    }
    items
}

/// Compares the files below `local_root` with a recursive listing of the corresponding remote
/// directory (as returned by [`Transport::list_recursive`], names relative to it). `excluded`
/// patterns are matched against paths relative to `local_root`. Items are sorted by path.
pub fn plan_sync(
    local_root: &Path,
    excluded: &[String],
    remote_entries_recursive: &[RemoteEntry],
    direction: SyncDirection,
) -> Vec<SyncItem> {
    plan_sync_with(
        local_root,
        excluded,
        remote_entries_recursive,
        direction,
        &SyncOptions::default(),
    )
}

/// [`plan_sync`] with explicit [`SyncOptions`].
pub fn plan_sync_with(
    local_root: &Path,
    excluded: &[String],
    remote_entries_recursive: &[RemoteEntry],
    direction: SyncDirection,
    options: &SyncOptions,
) -> Vec<SyncItem> {
    let set = ExcludeSet::new(excluded);
    let skip = |rel: &str| set.is_excluded(rel);
    let local = scan_local(local_root, &skip);
    let remote = remote_map(remote_entries_recursive, &skip);
    compare(&local, &remote, direction, options)
}

/// Plans a sync of the whole project with `server`: every mapping is listed on the server
/// and compared with its local directory (paths covered by a deeper mapping are left to
/// that mapping). `rel_path`s are relative to `project_root`; `excluded` patterns too.
/// Mappings outside the project are skipped.
pub fn plan_server_sync(
    transport: &dyn Transport,
    server: &Server,
    project_root: &Path,
    excluded: &[String],
    direction: SyncDirection,
) -> anyhow::Result<Vec<SyncItem>> {
    let project = normalize_local(project_root);
    let set = ExcludeSet::new(excluded);
    let remote_prune: Vec<String> = set.name_patterns().to_vec();
    let opts = SyncOptions {
        coarse_remote_times: transport.coarse_times(),
        ..SyncOptions::default()
    };
    let roots: Vec<(PathBuf, String)> = server
        .effective_mappings()
        .iter()
        .map(|m| {
            (
                mapping_local_root(&project, m),
                mapping_remote_root(server, m),
            )
        })
        .collect();
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for (local_root, remote_root) in &roots {
        if !seen.insert(local_root.clone()) {
            continue;
        }
        let Some(prefix) = rel_string(&project, local_root) else {
            log::warn!(
                "deployment: mapping {} is outside the project; not synced",
                local_root.display()
            );
            continue;
        };
        let nested: Vec<String> = roots
            .iter()
            .filter(|(other, _)| other != local_root && other.starts_with(local_root))
            .filter_map(|(other, _)| rel_string(local_root, other))
            .collect();
        let skip = |rel: &str| {
            nested.iter().any(|n| {
                rel == n || (rel.starts_with(n.as_str()) && rel[n.len()..].starts_with('/'))
            }) || set.is_excluded(&join_rel(&prefix, rel))
        };
        let local = scan_local(local_root, &skip);
        let entries = transport.list_recursive(remote_root, &remote_prune)?;
        // A remote file belongs to this mapping only if the inverse mapping takes it back
        // here: files under a deeper mapping's remote root (e.g. /www/public for
        // frontend/dist inside "" -> /www) are left to that mapping.
        let remote_skip = |rel: &str| {
            skip(rel)
                || local_path(server, &project, &join_remote(remote_root, rel))
                    .map(|p| normalize_local(&p))
                    != Some(normalize_local(&push_rel(local_root, rel)))
        };
        let remote = remote_map(&entries, &remote_skip);
        items.extend(
            compare(&local, &remote, direction, &opts)
                .into_iter()
                .map(|mut item| {
                    item.rel_path = join_rel(&prefix, &item.rel_path);
                    item
                }),
        );
    }
    items.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(items)
}

/// Files under `paths` (absolute or relative to `project_root`): directories are expanded
/// recursively (sorted, symlinked directories not followed); paths matching `excluded`
/// (relative to the project) are dropped. Duplicates are removed.
pub fn collect_files(project_root: &Path, paths: &[PathBuf], excluded: &[String]) -> Vec<PathBuf> {
    collect(project_root, paths, &ExcludeSet::new(excluded), true)
}

fn collect(
    project_root: &Path,
    paths: &[PathBuf],
    set: &ExcludeSet,
    filter_explicit: bool,
) -> Vec<PathBuf> {
    let project = normalize_local(project_root);
    let excluded = |p: &Path| rel_string(&project, p).is_some_and(|rel| set.is_excluded(&rel));
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        if crate::transfer::check().is_err() {
            break;
        }
        let abs = normalize_local(&project.join(p));
        if !abs.is_dir() {
            if !(filter_explicit && excluded(&abs)) && seen.insert(abs.clone()) {
                out.push(abs);
            }
            continue;
        }
        let mut stack = vec![abs];
        while let Some(dir) = stack.pop() {
            if crate::transfer::check().is_err() {
                return out;
            }
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut children: Vec<PathBuf> =
                rd.filter_map(Result::ok).map(|de| de.path()).collect();
            children.sort();
            let mut subdirs = Vec::new();
            for child in children {
                if excluded(&child) {
                    continue;
                }
                let is_link = std::fs::symlink_metadata(&child)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false);
                if child.is_dir() {
                    if !is_link {
                        subdirs.push(child);
                    }
                } else if child.is_file() && seen.insert(child.clone()) {
                    out.push(child);
                }
            }
            stack.extend(subdirs.into_iter().rev());
        }
    }
    out
}

fn display_rel(project: &Path, file: &Path) -> String {
    rel_string(project, file)
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| file.display().to_string())
}

/// Creates the parent directory of `remote` once per batch (remembering created directories).
fn ensure_remote_parent(
    transport: &dyn Transport,
    remote: &str,
    made: &mut HashSet<String>,
) -> anyhow::Result<()> {
    if transport.creates_parent_dirs() {
        return Ok(());
    }
    let Some(parent) = remote_parent(remote) else {
        return Ok(());
    };
    if parent == "/" || parent == "." || made.contains(&parent) {
        return Ok(());
    }
    transport.mkdirs(&parent)?;
    let mut dir = Some(parent);
    while let Some(d) = dir {
        if d == "/" || d == "." {
            break;
        }
        dir = remote_parent(&d);
        made.insert(d);
    }
    Ok(())
}

fn not_mapped(file: &Path, server: &Server) -> anyhow::Error {
    anyhow!(
        "{} has no deployment mapping on server '{}'",
        file.display(),
        server.name
    )
}

/// Uploads `files` (absolute or relative to `project_root`) to their mapped remote paths,
/// creating remote directories as needed. Directories are uploaded recursively, skipping
/// [`DEFAULT_EXCLUDED`](crate::DEFAULT_EXCLUDED) (use [`collect_files`] first to apply the
/// project's patterns). `on_progress(index, total, path)` is called before each file. Returns
/// one result per file; one failure does not stop the batch.
pub fn upload_paths(
    transport: &dyn Transport,
    server: &Server,
    project_root: &Path,
    files: &[PathBuf],
    mut on_progress: impl FnMut(usize, usize, &str),
) -> Vec<(PathBuf, anyhow::Result<()>)> {
    upload_paths_while(
        transport,
        server,
        project_root,
        files,
        |index, total, path| {
            on_progress(index, total, path);
            true
        },
    )
}

/// Stops before the next transfer when the progress callback returns false.
/// An in-flight transport operation retains its own network timeout.
pub fn upload_paths_while(
    transport: &dyn Transport,
    server: &Server,
    project_root: &Path,
    files: &[PathBuf],
    mut on_progress: impl FnMut(usize, usize, &str) -> bool,
) -> Vec<(PathBuf, anyhow::Result<()>)> {
    let project = normalize_local(project_root);
    let mut patterns = crate::DeploymentConfig::load_or_default(project_root).excluded;
    patterns.extend(server.options.excluded.clone());
    let files = collect(&project, files, &ExcludeSet::new(&patterns), false);
    let total = files.len();
    let mut made = HashSet::new();
    let mut results = Vec::with_capacity(total);
    for (i, file) in files.into_iter().enumerate() {
        if let Err(error) = crate::transfer::check() {
            results.push((file, Err(error)));
            break;
        }
        if !on_progress(i, total, &display_rel(&project, &file)) {
            break;
        }
        let result = match remote_path(server, &project, &file) {
            Some(remote) => ensure_remote_parent(transport, &remote, &mut made)
                .and_then(|()| transport.upload(&file, &remote)),
            None => Err(not_mapped(&file, server)),
        };
        if let Err(e) = &result {
            log::warn!(
                "deployment: upload of {} to {} failed: {e:#}",
                file.display(),
                server.name
            );
        }
        results.push((file, result));
    }
    results
}

/// Downloads the mapped remote counterparts of `paths` (absolute or relative to
/// `project_root`). An existing local directory is downloaded recursively from the server,
/// skipping `excluded` paths (relative to the project) and files belonging to a deeper
/// mapping. `on_progress(index, total, path)` is called before each file.
pub fn download_paths(
    transport: &dyn Transport,
    server: &Server,
    project_root: &Path,
    paths: &[PathBuf],
    excluded: &[String],
    mut on_progress: impl FnMut(usize, usize, &str),
) -> Vec<(PathBuf, anyhow::Result<()>)> {
    let project = normalize_local(project_root);
    let set = ExcludeSet::new(excluded);
    let prune: Vec<String> = set.name_patterns().to_vec();
    let mut jobs: Vec<(PathBuf, anyhow::Result<String>)> = Vec::new();
    for p in paths {
        let local = normalize_local(&project.join(p));
        let Some(remote) = remote_path(server, &project, &local) else {
            jobs.push((local.clone(), Err(not_mapped(&local, server))));
            continue;
        };
        if !local.is_dir() {
            jobs.push((local, Ok(remote)));
            continue;
        }
        match transport.list_recursive(&remote, &prune) {
            Ok(entries) => {
                for e in entries.into_iter().filter(|e| !e.is_dir) {
                    let target = push_rel(&local, &e.name);
                    let remote_file = join_remote(&remote, &e.name);
                    let maps_back = local_path(server, &project, &remote_file)
                        .map(|p| normalize_local(&p))
                        == Some(normalize_local(&target));
                    let skip = rel_string(&project, &target)
                        .is_some_and(|rel| set.is_excluded(&rel))
                        || remote_path(server, &project, &target).as_deref()
                            != Some(remote_file.as_str())
                        || !maps_back;
                    if !skip {
                        jobs.push((target, Ok(remote_file)));
                    }
                }
            }
            Err(e) => jobs.push((local, Err(e))),
        }
    }
    let total = jobs.len();
    let mut results = Vec::with_capacity(total);
    for (i, (local, remote)) in jobs.into_iter().enumerate() {
        if let Err(error) = crate::transfer::check() {
            results.push((local, Err(error)));
            break;
        }
        on_progress(i, total, &display_rel(&project, &local));
        let result = remote.and_then(|r| transport.download(&r, &local));
        results.push((local, result));
    }
    results
}

/// Carries out the `Upload`, `Download` and `DeleteRemote` items of a plan whose `rel_path`s
/// are relative to `project_root` (as produced by [`plan_server_sync`]); `Conflict` and
/// `Skip` items are left alone. `on_progress(index, total, rel_path)` is called before each.
pub fn execute_sync(
    transport: &dyn Transport,
    server: &Server,
    project_root: &Path,
    items: &[SyncItem],
    mut on_progress: impl FnMut(usize, usize, &str),
) -> Vec<(PathBuf, anyhow::Result<()>)> {
    let project = normalize_local(project_root);
    let todo: Vec<&SyncItem> = items
        .iter()
        .filter(|i| {
            matches!(
                i.action,
                SyncAction::Upload | SyncAction::Download | SyncAction::DeleteRemote
            )
        })
        .collect();
    let total = todo.len();
    let mut made = HashSet::new();
    let mut results = Vec::with_capacity(total);
    for (i, item) in todo.into_iter().enumerate() {
        if let Err(error) = crate::transfer::check() {
            results.push((push_rel(&project, &item.rel_path), Err(error)));
            break;
        }
        on_progress(i, total, &item.rel_path);
        let local = push_rel(&project, &item.rel_path);
        let result = match remote_path(server, &project, &local) {
            None => Err(not_mapped(&local, server)),
            Some(remote) => match item.action {
                SyncAction::Upload => ensure_remote_parent(transport, &remote, &mut made)
                    .and_then(|()| transport.upload(&local, &remote)),
                SyncAction::Download => transport.download(&remote, &local),
                _ => transport.delete(&remote),
            },
        };
        results.push((local, result));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{set_mtime, TempDir};
    use crate::LocalTransport;
    use std::sync::Mutex;

    const T: i64 = 1_700_000_000;

    fn file(name: &str, size: u64, mtime: i64) -> RemoteEntry {
        RemoteEntry::file(name, size, Some(mtime))
    }

    fn actions(items: &[SyncItem]) -> Vec<(&str, SyncAction)> {
        items
            .iter()
            .map(|i| (i.rel_path.as_str(), i.action))
            .collect()
    }

    /// A local tree: (path, contents, mtime).
    fn tree(files: &[(&str, &str, i64)]) -> TempDir {
        let dir = TempDir::new("sync");
        for (rel, contents, mtime) in files {
            let p = dir.write(rel, contents);
            set_mtime(&p, *mtime);
        }
        dir
    }

    #[test]
    fn decision_table() {
        let local = tree(&[
            ("same.py", "1234", T),
            ("local_only.py", "x", T),
            ("local_newer.py", "12", T + 100),
            ("remote_newer.py", "12", T),
            ("size.py", "123", T),
            ("unknown.py", "12", T),
            ("unknown_size.py", "123", T),
            ("tolerance.py", "12", T + 1),
        ]);
        let remote = vec![
            file("same.py", 4, T),
            file("remote_only.py", 1, T),
            file("local_newer.py", 2, T),
            file("remote_newer.py", 2, T + 100),
            file("size.py", 5, T),
            RemoteEntry::file("unknown.py", 2, None),
            RemoteEntry::file("unknown_size.py", 9, None),
            file("tolerance.py", 2, T),
        ];
        use SyncAction::*;
        let up = plan_sync(local.path(), &[], &remote, SyncDirection::Upload);
        assert_eq!(
            actions(&up),
            [
                ("local_newer.py", Upload),
                ("local_only.py", Upload),
                ("remote_newer.py", Conflict),
                ("remote_only.py", Skip),
                ("size.py", Upload),
                ("unknown_size.py", Upload)
            ]
        );
        let down = plan_sync(local.path(), &[], &remote, SyncDirection::Download);
        assert_eq!(
            actions(&down),
            [
                ("local_newer.py", Conflict),
                ("local_only.py", Skip),
                ("remote_newer.py", Download),
                ("remote_only.py", Download),
                ("size.py", Download),
                ("unknown_size.py", Download)
            ]
        );
        let both = plan_sync(local.path(), &[], &remote, SyncDirection::Both);
        assert_eq!(
            actions(&both),
            [
                ("local_newer.py", Upload),
                ("local_only.py", Upload),
                ("remote_newer.py", Download),
                ("remote_only.py", Download),
                ("size.py", Conflict),
                ("unknown_size.py", Conflict)
            ]
        );

        let item = both.iter().find(|i| i.rel_path == "local_only.py").unwrap();
        assert_eq!(
            (
                item.local_size,
                item.remote_size,
                item.local_mtime,
                item.remote_mtime
            ),
            (Some(1), None, Some(T), None)
        );
        assert!(item.exists_locally() && !item.exists_remotely());
        let item = both
            .iter()
            .find(|i| i.rel_path == "remote_newer.py")
            .unwrap();
        assert_eq!(
            (
                item.local_size,
                item.remote_size,
                item.local_mtime,
                item.remote_mtime
            ),
            (Some(2), Some(2), Some(T), Some(T + 100))
        );

        let strict = plan_sync_with(
            local.path(),
            &[],
            &remote,
            SyncDirection::Upload,
            &SyncOptions {
                mtime_tolerance_secs: 0,
                ..SyncOptions::default()
            },
        );
        assert!(strict
            .iter()
            .any(|i| i.rel_path == "tolerance.py" && i.action == Upload));
    }

    #[test]
    fn coarse_ftp_times() {
        // LIST shows 10:30 for a file modified at 10:30:42, and only the date for old files.
        let at = crate::listing::unix_from_civil(2024, 1, 15, 10, 30, 42);
        let old = crate::listing::unix_from_civil(2021, 6, 1, 13, 5, 0);
        let local = tree(&[
            ("minute.py", "12", at),
            ("day.py", "12", old),
            ("changed.py", "12", at),
        ]);
        let remote = vec![
            file(
                "minute.py",
                2,
                crate::listing::unix_from_civil(2024, 1, 15, 10, 30, 0),
            ),
            file(
                "day.py",
                2,
                crate::listing::unix_from_civil(2021, 6, 1, 0, 0, 0),
            ),
            file(
                "changed.py",
                2,
                crate::listing::unix_from_civil(2024, 1, 15, 10, 31, 0),
            ),
        ];
        let coarse = SyncOptions {
            coarse_remote_times: true,
            ..SyncOptions::default()
        };
        assert_eq!(
            actions(&plan_sync_with(
                local.path(),
                &[],
                &remote,
                SyncDirection::Both,
                &coarse
            )),
            [("changed.py", SyncAction::Download)]
        );
        // Transports with exact times (SFTP, rsync, local) do not get the truncation leeway.
        let precise = plan_sync(local.path(), &[], &remote, SyncDirection::Both);
        assert!(
            precise.iter().any(|i| i.rel_path == "minute.py"),
            "{precise:?}"
        );
    }

    #[test]
    fn exclusions_dirs_and_type_conflicts() {
        let local = tree(&[
            ("pkg/mod.py", "1", T),
            ("pkg/__pycache__/mod.pyc", "1", T),
            (".git/HEAD", "1", T),
            ("x.pyc", "1", T),
            ("sub dir/a b.py", "1", T),
            ("clash", "1", T),
            ("dir_clash/inner.py", "1", T),
        ]);
        let remote = vec![
            RemoteEntry::dir("pkg"),
            file("pkg/mod.py", 1, T),
            RemoteEntry::dir(".git"),
            file(".git/config", 3, T),
            file("remote.pyc", 1, T),
            RemoteEntry::dir("clash"),
            file("dir_clash", 1, T),
            RemoteEntry::dir("empty_remote_dir"),
            file("./sub dir//a b.py", 1, T),
        ];
        let plan = plan_sync(
            local.path(),
            &default_excluded(),
            &remote,
            SyncDirection::Both,
        );
        assert_eq!(
            actions(&plan),
            [
                ("clash", SyncAction::Conflict),
                ("dir_clash", SyncAction::Conflict),
                ("dir_clash/inner.py", SyncAction::Upload)
            ]
        );
        // Without exclusions everything shows up.
        let plan = plan_sync(local.path(), &[], &remote, SyncDirection::Upload);
        assert!(plan.iter().any(|i| i.rel_path == ".git/HEAD"));
        assert!(plan
            .iter()
            .any(|i| i.rel_path == "remote.pyc" && i.action == SyncAction::Skip));
        // A missing local root plans downloads of everything.
        let missing = plan_sync(
            &local.path().join("nope"),
            &[],
            &[file("a.py", 1, T)],
            SyncDirection::Both,
        );
        assert_eq!(actions(&missing), [("a.py", SyncAction::Download)]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_dirs_are_not_followed() {
        let local = tree(&[("real/a.py", "1", T)]);
        std::os::unix::fs::symlink(local.path().join("real"), local.path().join("link")).unwrap();
        let remote = vec![
            RemoteEntry::dir("link"),
            file("link/a.py", 1, T),
            file("real/a.py", 1, T),
        ];
        assert!(plan_sync(local.path(), &[], &remote, SyncDirection::Both).is_empty());
    }

    /// Delegates to a [`LocalTransport`] but behaves like scp: the parent directory must exist.
    struct StrictParents {
        inner: LocalTransport,
        mkdirs: Mutex<Vec<String>>,
    }

    impl Transport for StrictParents {
        fn upload(&self, local: &Path, remote: &str) -> anyhow::Result<()> {
            let parent = self.inner.resolve(&remote_parent(remote).unwrap())?;
            anyhow::ensure!(parent.is_dir(), "no such directory: {}", parent.display());
            self.inner.upload(local, remote)
        }
        fn download(&self, remote: &str, local: &Path) -> anyhow::Result<()> {
            self.inner.download(remote, local)
        }
        fn list(&self, dir: &str) -> anyhow::Result<Vec<RemoteEntry>> {
            self.inner.list(dir)
        }
        fn delete(&self, remote: &str) -> anyhow::Result<()> {
            self.inner.delete(remote)
        }
        fn mkdirs(&self, dir: &str) -> anyhow::Result<()> {
            self.mkdirs.lock().unwrap().push(dir.to_string());
            self.inner.mkdirs(dir)
        }
        fn test_connection(&self) -> anyhow::Result<String> {
            self.inner.test_connection()
        }
        fn list_recursive(
            &self,
            dir: &str,
            excluded: &[String],
        ) -> anyhow::Result<Vec<RemoteEntry>> {
            self.inner.list_recursive(dir, excluded)
        }
        fn describe(&self) -> String {
            "strict".into()
        }
    }

    #[test]
    #[cfg(unix)] // local fixture paths with characters invalid on Windows
    fn upload_creates_remote_dirs_once() {
        let project = tree(&[
            ("pkg/a.py", "a", T),
            ("pkg/b.py", "b", T),
            ("pkg/sub/c.py", "c", T),
            ("pkg/__pycache__/a.pyc", "x", T),
            ("top.py", "t", T),
            ("tests/t.py", "t", T),
        ]);
        let remote = TempDir::new("sync-remote");
        let t = StrictParents {
            inner: LocalTransport::new(remote.path()),
            mkdirs: Mutex::new(Vec::new()),
        };
        let server = Server::local("m", remote.path())
            .with_mapping("", "/www")
            .with_mapping("tests", "/qa/tests");
        let mut progress = Vec::new();
        let results = upload_paths(
            &t,
            &server,
            project.path(),
            &[
                PathBuf::from("pkg"),
                PathBuf::from("top.py"),
                project.path().join("tests/t.py"),
                PathBuf::from("/elsewhere/x.py"),
            ],
            |i, n, p| progress.push((i, n, p.to_string())),
        );
        let summary: Vec<(String, bool)> = results
            .iter()
            .map(|(p, r)| (display_rel(&normalize_local(project.path()), p), r.is_ok()))
            .collect();
        assert_eq!(
            summary,
            [
                ("pkg/a.py".to_string(), true),
                ("pkg/b.py".to_string(), true),
                ("pkg/sub/c.py".to_string(), true),
                ("top.py".to_string(), true),
                ("tests/t.py".to_string(), true),
                ("/elsewhere/x.py".to_string(), false)
            ]
        );
        assert!(
            format!("{:#}", results[5].1.as_ref().unwrap_err()).contains("no deployment mapping")
        );
        assert_eq!(
            progress
                .iter()
                .map(|(i, n, _)| (*i, *n))
                .collect::<Vec<_>>(),
            (0..6).map(|i| (i, 6)).collect::<Vec<_>>()
        );
        assert_eq!(progress[0].2, "pkg/a.py");
        assert_eq!(
            *t.mkdirs.lock().unwrap(),
            ["/www/pkg", "/www/pkg/sub", "/qa/tests"],
            "one mkdirs per new directory; /www is known from /www/pkg"
        );
        assert_eq!(
            std::fs::read_to_string(remote.path().join("www/pkg/sub/c.py")).unwrap(),
            "c"
        );
        assert_eq!(
            std::fs::read_to_string(remote.path().join("qa/tests/t.py")).unwrap(),
            "t"
        );
        assert!(
            !remote.path().join("www/pkg/__pycache__").exists(),
            "default exclusions apply to directories"
        );
    }

    #[test]
    fn collect_files_applies_exclusions() {
        let project = tree(&[
            ("a.py", "", T),
            ("b/c.py", "", T),
            ("b/d.log", "", T),
            ("b/.git/x", "", T),
            ("b/e/f.py", "", T),
        ]);
        let files = collect_files(
            project.path(),
            &[
                PathBuf::from("b"),
                PathBuf::from("a.py"),
                PathBuf::from("b/c.py"),
                PathBuf::from("b/d.log"),
            ],
            &["*.log".to_string(), ".git".to_string()],
        );
        let rels: Vec<String> = files
            .iter()
            .map(|f| display_rel(&normalize_local(project.path()), f))
            .collect();
        assert_eq!(rels, ["b/c.py", "b/e/f.py", "a.py"]);
    }

    #[test]
    fn server_sync_round_trip_with_nested_mappings() {
        let project = tree(&[
            ("app.py", "app v2", T + 50),
            ("static/site.css", "css", T),
            ("static/img/logo.svg", "svg", T),
            ("notes.pyc", "x", T),
            ("same.py", "same", T),
        ]);
        let remote = TempDir::new("sync-server");
        let t = LocalTransport::new(remote.path());
        let server = Server::local("m", remote.path())
            .with_mapping("", "/app")
            .with_mapping("static", "/public");
        // Remote state: an older app.py, an extra file, an unrelated copy of static under /app.
        for (rel, contents, mtime) in [
            ("app/app.py", "app v1", T),
            ("app/extra.py", "e", T),
            ("app/static/stale.css", "old", T),
            ("app/same.py", "same", T),
            ("public/img/logo.svg", "svg", T),
        ] {
            set_mtime(&remote.write(rel, contents), mtime);
        }
        let plan = plan_server_sync(
            &t,
            &server,
            project.path(),
            &default_excluded(),
            SyncDirection::Upload,
        )
        .unwrap();
        use SyncAction::*;
        assert_eq!(
            actions(&plan),
            [
                ("app.py", Upload),
                ("extra.py", Skip),
                ("static/site.css", Upload)
            ],
            "static/stale.css under /app belongs to the deeper mapping"
        );

        let mut plan = plan;
        plan.iter_mut()
            .find(|i| i.rel_path == "extra.py")
            .unwrap()
            .action = DeleteRemote;
        let mut seen = Vec::new();
        let results = execute_sync(&t, &server, project.path(), &plan, |i, n, p| {
            seen.push(format!("{i}/{n} {p}"))
        });
        assert!(results.iter().all(|(_, r)| r.is_ok()), "{results:?}");
        assert_eq!(seen, ["0/3 app.py", "1/3 extra.py", "2/3 static/site.css"]);
        assert_eq!(
            std::fs::read_to_string(remote.path().join("app/app.py")).unwrap(),
            "app v2"
        );
        assert!(!remote.path().join("app/extra.py").exists());
        assert_eq!(
            std::fs::read_to_string(remote.path().join("public/site.css")).unwrap(),
            "css"
        );
        assert!(
            plan_server_sync(
                &t,
                &server,
                project.path(),
                &default_excluded(),
                SyncDirection::Both
            )
            .unwrap()
            .is_empty(),
            "in sync afterwards"
        );

        // Downloads: a remote change comes back, files of the deeper mapping come from /public.
        set_mtime(&remote.write("public/img/logo.svg", "svg v2"), T + 500);
        set_mtime(&remote.write("public/new.css", "new"), T + 500);
        let plan = plan_server_sync(
            &t,
            &server,
            project.path(),
            &default_excluded(),
            SyncDirection::Download,
        )
        .unwrap();
        assert_eq!(
            actions(&plan),
            [
                ("static/img/logo.svg", Download),
                ("static/new.css", Download)
            ]
        );
        let results = execute_sync(&t, &server, project.path(), &plan, |_, _, _| {});
        assert!(results.iter().all(|(_, r)| r.is_ok()));
        assert_eq!(
            std::fs::read_to_string(project.path().join("static/img/logo.svg")).unwrap(),
            "svg v2"
        );
        assert_eq!(
            mtime_secs(&std::fs::metadata(project.path().join("static/new.css")).unwrap()),
            Some(T + 500)
        );
    }

    #[test]
    fn deeper_remote_root_inside_an_outer_mapping_is_not_mirrored_back() {
        let project = tree(&[("app.py", "app", T), ("frontend/dist/site.css", "css", T)]);
        let remote = TempDir::new("sync-nested-remote");
        let t = LocalTransport::new(remote.path());
        let server = Server::local("m", remote.path())
            .with_mapping("", "/www")
            .with_mapping("frontend/dist", "/www/public");
        let plan = plan_server_sync(
            &t,
            &server,
            project.path(),
            &default_excluded(),
            SyncDirection::Upload,
        )
        .unwrap();
        let results = execute_sync(&t, &server, project.path(), &plan, |_, _, _| {});
        assert!(results.iter().all(|(_, r)| r.is_ok()), "{results:?}");
        assert_eq!(
            std::fs::read_to_string(remote.path().join("www/public/site.css")).unwrap(),
            "css"
        );
        for direction in [
            SyncDirection::Both,
            SyncDirection::Download,
            SyncDirection::Upload,
        ] {
            let plan =
                plan_server_sync(&t, &server, project.path(), &default_excluded(), direction)
                    .unwrap();
            assert!(
                plan.iter().all(|i| i.rel_path != "public/site.css"),
                "{direction:?}: {plan:?}"
            );
        }
        let results = download_paths(
            &t,
            &server,
            project.path(),
            &[PathBuf::from(".")],
            &default_excluded(),
            |_, _, _| {},
        );
        assert!(
            results.iter().all(|(p, _)| !p.ends_with("public/site.css")),
            "{results:?}"
        );
        assert!(
            !project.path().join("public").exists(),
            "no stray copy of the deeper mapping"
        );
    }

    #[test]
    fn download_paths_expands_directories() {
        let project = tree(&[("keep.txt", "local", T)]);
        std::fs::create_dir_all(project.path().join("pkg")).unwrap();
        let remote = TempDir::new("sync-download");
        for (rel, contents) in [
            ("srv/pkg/a.py", "a"),
            ("srv/pkg/sub/b.py", "b"),
            ("srv/pkg/x.log", "log"),
            ("srv/pkg/__pycache__/a.pyc", "c"),
            ("srv/single.py", "s"),
        ] {
            remote.write(rel, contents);
        }
        let t = LocalTransport::new(remote.path());
        let server = Server::local("m", remote.path()).with_mapping("", "/srv");
        let mut calls = 0;
        let results = download_paths(
            &t,
            &server,
            project.path(),
            &[
                PathBuf::from("pkg"),
                PathBuf::from("single.py"),
                PathBuf::from("missing.py"),
            ],
            &["*.log".to_string(), "__pycache__".to_string()],
            |_, n, _| {
                calls += 1;
                assert_eq!(n, 4);
            },
        );
        assert_eq!(calls, 4);
        let ok: Vec<(String, bool)> = results
            .iter()
            .map(|(p, r)| (display_rel(&normalize_local(project.path()), p), r.is_ok()))
            .collect();
        assert_eq!(
            ok,
            [
                ("pkg/a.py".to_string(), true),
                ("pkg/sub/b.py".to_string(), true),
                ("single.py".to_string(), true),
                ("missing.py".to_string(), false)
            ]
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("pkg/sub/b.py")).unwrap(),
            "b"
        );
        assert!(!project.path().join("pkg/x.log").exists());
        assert!(
            !project.path().join("missing.py").exists(),
            "failed downloads leave nothing behind"
        );
    }
}
