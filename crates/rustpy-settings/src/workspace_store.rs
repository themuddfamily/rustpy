//! Checked per-user workspace persistence. Locks coordinate cooperating RustPy
//! processes; arbitrary external writers are detected before publication but do
//! not participate in the advisory lock's final-check/rename exclusion.
use crate::{
    BookmarkState, ExceptionBreakpointSettings, FileEncodingOverride, LineBreakpointState,
    OpenFileState, ToolWindowLayout, Workspace,
};
use rustpy_core::vfs::{normalize_path, ContentRevision};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug)]
pub enum WorkspaceError {
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    Invalid {
        path: PathBuf,
        reason: String,
    },
    Conflict(String),
    Busy,
    SessionRequired,
    WrongStore,
}
impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => write!(f, "cannot access {}: {error}", path.display()),
            Self::Invalid { path, reason } => write!(
                f,
                "{} is not valid: {reason}; left unchanged",
                path.display()
            ),
            Self::Conflict(field) => write!(f, "workspace changed concurrently: {field}"),
            Self::Busy => write!(f, "workspace is busy in another window"),
            Self::SessionRequired => {
                write!(f, "this window does not own the workspace editor session")
            }
            Self::WrongStore => write!(
                f,
                "workspace snapshot or session lease belongs to another project"
            ),
        }
    }
}
impl std::error::Error for WorkspaceError {}
fn io(path: &Path, error: std::io::Error) -> WorkspaceError {
    WorkspaceError::Io {
        path: path.into(),
        error,
    }
}

#[derive(Clone, Debug)]
pub struct WorkspaceSnapshot {
    project: PathBuf,
    workspace: Workspace,
    raw: Value,
    revision: Option<ContentRevision>,
}
impl WorkspaceSnapshot {
    pub fn run_configurations(&self) -> Option<&Value> {
        self.raw.get("run_configurations")
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }
    /// None distinguishes a missing workspace from an existing empty document.
    pub fn revision(&self) -> Option<ContentRevision> {
        self.revision
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSession {
    pub open_files: Vec<OpenFileState>,
    pub active_file: Option<PathBuf>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::deserialize_editor_layout"
    )]
    pub editor_layout: Option<crate::EditorLayout>,
    pub expanded_dirs: Vec<PathBuf>,
    pub tool_windows: ToolWindowLayout,
    pub recent_files: Vec<PathBuf>,
}
impl WorkspaceSession {
    pub fn from_workspace(ws: &Workspace) -> Self {
        Self {
            open_files: ws.open_files.clone(),
            active_file: ws.active_file.clone(),
            editor_layout: ws.editor_layout.clone(),
            expanded_dirs: ws.expanded_dirs.clone(),
            tool_windows: ws.tool_windows.clone(),
            recent_files: ws.recent_files.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceBreakpoints {
    pub breakpoints: Vec<(PathBuf, Vec<usize>)>,
    pub line_breakpoints: Vec<LineBreakpointState>,
    pub exception_breakpoints: ExceptionBreakpointSettings,
    pub breakpoints_muted: bool,
}
impl WorkspaceBreakpoints {
    pub fn from_workspace(ws: &Workspace) -> Self {
        Self {
            breakpoints: ws.breakpoints.clone(),
            line_breakpoints: ws.line_breakpoints.clone(),
            exception_breakpoints: ws.exception_breakpoints.clone(),
            breakpoints_muted: ws.breakpoints_muted,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceLegacyRoots {
    pub source_roots: Vec<PathBuf>,
    pub excluded_dirs: Vec<PathBuf>,
}
#[derive(Clone, Debug, Default)]
pub struct WorkspacePatch {
    /// Opaque project-crate run configurations, committed under this store's
    /// lock and three-way conflict checks alongside other workspace state.
    pub run_configurations: Option<Value>,
    pub interpreter: Option<Option<PathBuf>>,
    pub file_encodings: BTreeMap<PathBuf, Option<String>>,
    pub startup_tasks: BTreeMap<String, bool>,
    pub bookmarks: BTreeMap<(PathBuf, usize), Option<BookmarkState>>,
    pub bookmark_lists: Option<Vec<String>>,
    pub watches: Option<Vec<String>>,
    pub breakpoints: Option<WorkspaceBreakpoints>,
    pub legacy_roots: Option<WorkspaceLegacyRoots>,
    pub session: Option<WorkspaceSession>,
}
#[derive(Debug)]
pub struct WorkspaceSessionLease {
    project: PathBuf,
    owner: Arc<()>,
    _lock: File,
}
impl Drop for WorkspaceSessionLease {
    fn drop(&mut self) {
        os_lock::unlock(&self._lock);
    }
}

/// Closing one handle is insufficient when its open-file description is shared.
struct CommitLock(File);
impl std::ops::Deref for CommitLock {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}
impl Drop for CommitLock {
    fn drop(&mut self) {
        os_lock::unlock(&self.0);
    }
}
#[derive(Clone, Debug)]
/// Clones deliberately share one window identity; separately constructed stores
/// cannot borrow one another's session lease.
pub struct WorkspaceStore {
    project: PathBuf,
    owner: Arc<()>,
}
impl WorkspaceStore {
    pub fn new(project: impl AsRef<Path>) -> Self {
        Self {
            project: normalize_path(project.as_ref()),
            owner: Arc::new(()),
        }
    }
    pub fn project(&self) -> &Path {
        &self.project
    }
    pub fn load(&self) -> Result<WorkspaceSnapshot, WorkspaceError> {
        let path = Workspace::path_for(&self.project);
        let bytes = read_checked(&path)?;
        let (raw, revision) = match bytes {
            None => (Value::Object(Default::default()), None),
            Some(bytes) => {
                let raw: Value =
                    serde_json::from_slice(&bytes).map_err(|e| WorkspaceError::Invalid {
                        path: path.clone(),
                        reason: e.to_string(),
                    })?;
                if !raw.is_object() {
                    return Err(WorkspaceError::Invalid {
                        path,
                        reason: "expected a JSON object".into(),
                    });
                }
                (raw, Some(ContentRevision::of(&bytes)))
            }
        };
        let mut workspace: Workspace =
            serde_json::from_value(raw.clone()).map_err(|e| WorkspaceError::Invalid {
                path,
                reason: e.to_string(),
            })?;
        workspace.resolve_paths(&self.project);
        Ok(WorkspaceSnapshot {
            project: self.project.clone(),
            workspace,
            raw,
            revision,
        })
    }
    pub fn try_acquire_session(&self) -> Result<Option<WorkspaceSessionLease>, WorkspaceError> {
        Ok(
            lock_file(&self.project.join(".rustpy/workspace.session.lock"))?.map(|lock| {
                WorkspaceSessionLease {
                    project: self.project.clone(),
                    owner: Arc::clone(&self.owner),
                    _lock: lock,
                }
            }),
        )
    }
    /// Validate the retained session without writing or refreshing its baseline.
    pub fn validate_session(&self, lease: &WorkspaceSessionLease) -> Result<(), WorkspaceError> {
        if lease.project != self.project || !Arc::ptr_eq(&lease.owner, &self.owner) {
            return Err(WorkspaceError::WrongStore);
        }
        if !same_lock(
            &self.project.join(".rustpy/workspace.session.lock"),
            &lease._lock,
        )? {
            return Err(WorkspaceError::SessionRequired);
        }
        Ok(())
    }
    pub fn commit(
        &self,
        baseline: &WorkspaceSnapshot,
        patch: &WorkspacePatch,
        lease: Option<&WorkspaceSessionLease>,
    ) -> Result<WorkspaceSnapshot, WorkspaceError> {
        self.commit_inner(baseline, patch, lease, None, |_| Ok(()))
    }
    fn commit_inner(
        &self,
        baseline: &WorkspaceSnapshot,
        patch: &WorkspacePatch,
        lease: Option<&WorkspaceSessionLease>,
        compatibility_order: Option<&Workspace>,
        before_publish: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<WorkspaceSnapshot, WorkspaceError> {
        if baseline.project != self.project
            || lease
                .is_some_and(|l| l.project != self.project || !Arc::ptr_eq(&l.owner, &self.owner))
        {
            return Err(WorkspaceError::WrongStore);
        }
        if let Some(lease) = lease {
            if !same_lock(
                &self.project.join(".rustpy/workspace.session.lock"),
                &lease._lock,
            )? {
                return Err(WorkspaceError::SessionRequired);
            }
        }
        if patch.session.is_some() && lease.is_none() {
            return Err(WorkspaceError::SessionRequired);
        }
        let lock_path = self.project.join(".rustpy/workspace.lock");
        let deadline = Instant::now() + Duration::from_secs(2);
        let _lock = loop {
            if let Some(lock) = lock_file(&lock_path)? {
                break CommitLock(lock);
            }
            if Instant::now() >= deadline {
                return Err(WorkspaceError::Busy);
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if !same_lock(&lock_path, &_lock)? {
            return Err(WorkspaceError::Conflict(
                "workspace lock was replaced".into(),
            ));
        }
        let current = self.load()?;
        if baseline.revision.is_some() && current.revision.is_none() {
            return Err(WorkspaceError::Conflict("workspace was deleted".into()));
        }
        let mut ws = current.workspace.clone();
        let base = &baseline.workspace;
        let mut fields: Vec<&str> = Vec::new();
        if let Some(value) = &patch.run_configurations {
            merge(
                &baseline.raw["run_configurations"],
                &current.raw["run_configurations"],
                value,
                "run_configurations",
            )?;
            fields.push("run_configurations");
        }
        if let Some(value) = &patch.interpreter {
            // The selected executable path is SDK authority: two virtualenvs
            // can symlink to the same base Python but load different prefixes
            // and packages. File identity normalization would erase that choice.
            let value = value.as_ref().map(|path| {
                if path.is_absolute() {
                    path.clone()
                } else {
                    self.project.join(path)
                }
            });
            merge(&base.interpreter, &ws.interpreter, &value, "interpreter")?;
            ws.interpreter = value;
            fields.push("interpreter");
        }
        if !patch.file_encodings.is_empty() {
            let before = codecs(base);
            let mut now = codecs(&ws);
            for (path, value) in &patch.file_encodings {
                let path = self.key(path);
                merge(
                    &before.get(&path),
                    &now.get(&path),
                    &value.as_ref(),
                    "file encoding",
                )?;
                if let Some(value) = value {
                    now.insert(path, value.clone());
                } else {
                    now.remove(&path);
                }
            }
            ws.file_encodings = now
                .into_iter()
                .map(|(path, encoding)| FileEncodingOverride { path, encoding })
                .collect();
            fields.push("file_encodings");
        }
        if !patch.startup_tasks.is_empty() {
            for (name, present) in &patch.startup_tasks {
                merge(
                    &base.startup_tasks.contains(name),
                    &ws.startup_tasks.contains(name),
                    present,
                    "startup task",
                )?;
                if *present {
                    if !ws.startup_tasks.contains(name) {
                        ws.startup_tasks.push(name.clone());
                    }
                } else {
                    ws.startup_tasks.retain(|n| n != name);
                }
            }
            fields.push("startup_tasks");
        }
        if !patch.bookmarks.is_empty() {
            let before = bookmarks(base);
            let mut now = bookmarks(&ws);
            for ((path, line), value) in &patch.bookmarks {
                let key = (self.key(path), *line);
                let value = value.clone().map(|mut b| {
                    b.path = key.0.clone();
                    b.line = key.1;
                    b
                });
                merge(
                    &before.get(&key),
                    &now.get(&key),
                    &value.as_ref(),
                    "bookmark",
                )?;
                if let Some(value) = value {
                    now.insert(key, value);
                } else {
                    now.remove(&key);
                }
            }
            ws.bookmarks = now.into_values().collect();
            fields.push("bookmarks");
        }
        if let Some(value) = &patch.bookmark_lists {
            merge(
                &base.bookmark_lists,
                &ws.bookmark_lists,
                value,
                "bookmark_lists",
            )?;
            ws.bookmark_lists = value.clone();
            fields.push("bookmark_lists");
        }
        if let Some(value) = &patch.watches {
            merge(&base.watches, &ws.watches, value, "watches")?;
            ws.watches = value.clone();
            fields.push("watches");
        }
        if let Some(value) = &patch.breakpoints {
            merge(
                &WorkspaceBreakpoints::from_workspace(base),
                &WorkspaceBreakpoints::from_workspace(&ws),
                value,
                "breakpoints",
            )?;
            ws.breakpoints = value.breakpoints.clone();
            ws.line_breakpoints = value.line_breakpoints.clone();
            ws.exception_breakpoints = value.exception_breakpoints.clone();
            ws.breakpoints_muted = value.breakpoints_muted;
            fields.extend([
                "breakpoints",
                "line_breakpoints",
                "exception_breakpoints",
                "breakpoints_muted",
            ]);
        }
        if let Some(value) = &patch.legacy_roots {
            merge(
                &(base.source_roots.clone(), base.excluded_dirs.clone()),
                &(ws.source_roots.clone(), ws.excluded_dirs.clone()),
                &(value.source_roots.clone(), value.excluded_dirs.clone()),
                "legacy roots",
            )?;
            ws.source_roots = value.source_roots.clone();
            ws.excluded_dirs = value.excluded_dirs.clone();
            fields.extend(["source_roots", "excluded_dirs"]);
        }
        if let Some(value) = &patch.session {
            let mut desired = Workspace {
                open_files: value.open_files.clone(),
                active_file: value.active_file.clone(),
                editor_layout: value.editor_layout.clone(),
                expanded_dirs: value.expanded_dirs.clone(),
                tool_windows: value.tool_windows.clone(),
                recent_files: value.recent_files.clone(),
                ..Default::default()
            };
            desired.map_paths(|path| Some(self.key(path)));
            desired.normalize_editor_layout();
            let value = &WorkspaceSession::from_workspace(&desired);
            merge(
                &WorkspaceSession::from_workspace(base),
                &WorkspaceSession::from_workspace(&ws),
                value,
                "editor session",
            )?;
            ws.open_files = value.open_files.clone();
            ws.active_file = value.active_file.clone();
            ws.editor_layout = value.editor_layout.clone();
            ws.expanded_dirs = value.expanded_dirs.clone();
            ws.tool_windows = value.tool_windows.clone();
            ws.recent_files = value.recent_files.clone();
            fields.extend([
                "open_files",
                "active_file",
                "editor_layout",
                "expanded_dirs",
                "tool_windows",
                "recent_files",
            ]);
        }
        if fields.is_empty() {
            return Ok(current);
        }
        if let Some(order) = compatibility_order {
            ws.file_encodings = order.file_encodings.clone();
            ws.bookmarks = order.bookmarks.clone();
            ws.bookmark_lists = order.bookmark_lists.clone();
            ws.startup_tasks = order.startup_tasks.clone();
        }
        let mut stored = ws.clone();
        stored.map_paths(|p| Some(p.strip_prefix(&self.project).unwrap_or(p).into()));
        stored.root = Some(self.project.clone());
        let encoded = serde_json::to_value(stored).map_err(|e| WorkspaceError::Invalid {
            path: Workspace::path_for(&self.project),
            reason: e.to_string(),
        })?;
        let mut raw = current.raw.clone();
        normalize_record_paths(&mut raw, &self.project);
        // A copied legacy file can still contain old-root absolute paths. Once
        // root is updated, every known path must use the resolved current root;
        // otherwise an unrelated patch would reinterpret untouched old paths.
        for field in fields.into_iter().chain([
            "root",
            "open_files",
            "active_file",
            "editor_layout",
            "expanded_dirs",
            "interpreter",
            "breakpoints",
            "line_breakpoints",
            "bookmarks",
            "recent_files",
            "source_roots",
            "excluded_dirs",
            "file_encodings",
        ]) {
            if let Some(value) = encoded.get(field) {
                overlay(&mut raw[field], value, &self.project);
            } else {
                raw.as_object_mut().unwrap().remove(field);
            }
        }
        if let Some(value) = &patch.run_configurations {
            raw["run_configurations"] = value.clone();
        }
        let mut validated: Workspace =
            serde_json::from_value(raw.clone()).map_err(|e| WorkspaceError::Invalid {
                path: Workspace::path_for(&self.project),
                reason: e.to_string(),
            })?;
        validated.resolve_paths(&self.project);
        let bytes = serde_json::to_vec_pretty(&raw).map_err(|e| WorkspaceError::Invalid {
            path: Workspace::path_for(&self.project),
            reason: e.to_string(),
        })?;
        let path = Workspace::path_for(&self.project);
        if bytes.len() as u64 > MAX_BYTES {
            return Err(WorkspaceError::Invalid {
                path,
                reason: "prepared workspace exceeds 8 MiB".into(),
            });
        }
        if read_checked(&path)?.as_deref().map(ContentRevision::of) != current.revision {
            return Err(WorkspaceError::Conflict("external file replacement".into()));
        }
        atomic_write(&path, &bytes, |temp| {
            before_publish(temp).map_err(|e| io(&path, e))?;
            if !same_lock(&lock_path, &_lock)? {
                return Err(WorkspaceError::Conflict(
                    "workspace lock was replaced".into(),
                ));
            }
            if let Some(lease) = lease {
                if !same_lock(
                    &self.project.join(".rustpy/workspace.session.lock"),
                    &lease._lock,
                )? {
                    return Err(WorkspaceError::SessionRequired);
                }
            }
            if read_checked(&path)?.as_deref().map(ContentRevision::of) != current.revision {
                return Err(WorkspaceError::Conflict("external file replacement".into()));
            }
            Ok(())
        })?;
        Ok(WorkspaceSnapshot {
            project: self.project.clone(),
            workspace: validated,
            raw,
            revision: Some(ContentRevision::of(&bytes)),
        })
    }
    fn key(&self, path: &Path) -> PathBuf {
        normalize_path(&if path.is_absolute() {
            path.into()
        } else {
            self.project.join(path)
        })
    }
    pub(crate) fn replace_compat(&self, ws: &Workspace) -> Result<(), WorkspaceError> {
        let baseline = self.load()?;
        let lease = self
            .try_acquire_session()?
            .ok_or(WorkspaceError::SessionRequired)?;
        let mut patch = WorkspacePatch {
            bookmark_lists: Some(ws.bookmark_lists.clone()),
            interpreter: Some(ws.interpreter.clone()),
            watches: Some(ws.watches.clone()),
            breakpoints: Some(WorkspaceBreakpoints::from_workspace(ws)),
            legacy_roots: Some(WorkspaceLegacyRoots {
                source_roots: ws.source_roots.clone(),
                excluded_dirs: ws.excluded_dirs.clone(),
            }),
            session: Some(WorkspaceSession::from_workspace(ws)),
            ..Default::default()
        };
        let desired = codecs(ws);
        for path in codecs(baseline.workspace()).keys().chain(desired.keys()) {
            patch
                .file_encodings
                .insert(path.clone(), desired.get(path).cloned());
        }
        for name in baseline
            .workspace
            .startup_tasks
            .iter()
            .chain(&ws.startup_tasks)
        {
            patch
                .startup_tasks
                .insert(name.clone(), ws.startup_tasks.contains(name));
        }
        let desired = bookmarks(ws);
        for key in bookmarks(baseline.workspace()).keys().chain(desired.keys()) {
            patch
                .bookmarks
                .insert(key.clone(), desired.get(key).cloned());
        }
        self.commit_inner(&baseline, &patch, Some(&lease), Some(ws), |_| Ok(()))
            .map(|_| ())
    }
}
fn merge<T: PartialEq>(
    base: &T,
    current: &T,
    desired: &T,
    field: &str,
) -> Result<(), WorkspaceError> {
    if current != base && current != desired {
        Err(WorkspaceError::Conflict(field.into()))
    } else {
        Ok(())
    }
}
fn codecs(ws: &Workspace) -> BTreeMap<PathBuf, String> {
    ws.file_encodings
        .iter()
        .map(|e| (normalize_path(&e.path), e.encoding.clone()))
        .collect()
}
fn bookmarks(ws: &Workspace) -> BTreeMap<(PathBuf, usize), BookmarkState> {
    ws.bookmarks
        .iter()
        .map(|b| {
            let mut b = b.clone();
            b.path = normalize_path(&b.path);
            ((b.path.clone(), b.line), b)
        })
        .collect()
}

fn same_record_path(old: Option<&Value>, new: Option<&Value>, project: &Path) -> bool {
    let path = |value: Option<&Value>| {
        value.and_then(Value::as_str).map(|path| {
            normalize_path(&if Path::new(path).is_absolute() {
                PathBuf::from(path)
            } else {
                project.join(path)
            })
        })
    };
    match (path(old), path(new)) {
        (Some(old), Some(new)) => old == new,
        _ => false,
    }
}
// Preserve unknown nested fields and record fields when changing known values.
fn overlay(old: &mut Value, new: &Value, project: &Path) {
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            for (k, v) in new {
                overlay(old.entry(k.clone()).or_insert(Value::Null), v, project);
            }
        }
        (Value::Array(old), Value::Array(new)) => {
            let previous = std::mem::take(old);
            *old = new
                .iter()
                .map(|value| {
                    let mut result = previous
                        .iter()
                        .find(|v| {
                            (value.get("path").is_some()
                                && same_record_path(v.get("path"), value.get("path"), project)
                                && v.get("line") == value.get("line"))
                                || (value.get("group").is_some()
                                    && v.get("group") == value.get("group")
                                    && same_record_path(
                                        v.get("file").and_then(|file| file.get("path")),
                                        value.get("file").and_then(|file| file.get("path")),
                                        project,
                                    ))
                                || (value.get("name").is_some()
                                    && v.get("name") == value.get("name"))
                        })
                        .cloned()
                        .unwrap_or(Value::Null);
                    overlay(&mut result, value, project);
                    result
                })
                .collect();
        }
        (old, new) => *old = new.clone(),
    }
}
fn normalize_record_paths(raw: &mut Value, project: &Path) {
    let saved_root = raw.get("root").and_then(Value::as_str).map(PathBuf::from);
    let remap = |value: &mut Value| {
        if let Some(path) = value.as_str() {
            let path = PathBuf::from(path);
            let path = if path.is_relative() {
                project.join(path)
            } else if let Some(old) = &saved_root {
                path.strip_prefix(old)
                    .map(|rel| project.join(rel))
                    .unwrap_or(path)
            } else {
                path
            };
            let path = normalize_path(&path);
            *value = serde_json::to_value(path.strip_prefix(project).unwrap_or(&path)).unwrap();
        }
    };
    for field in [
        "file_encodings",
        "open_files",
        "line_breakpoints",
        "bookmarks",
    ] {
        if let Some(records) = raw.get_mut(field).and_then(Value::as_array_mut) {
            for record in records {
                if let Some(path) = record.get_mut("path") {
                    remap(path);
                }
            }
        }
    }
    if let Some(layout) = raw.get_mut("editor_layout").and_then(Value::as_object_mut) {
        if let Some(instances) = layout.get_mut("instances").and_then(Value::as_array_mut) {
            for instance in instances {
                if let Some(path) = instance
                    .get_mut("file")
                    .and_then(|file| file.get_mut("path"))
                {
                    remap(path);
                }
            }
        }
        if let Some(groups) = layout.get_mut("groups").and_then(Value::as_array_mut) {
            for member in groups {
                if let Some(path) = member.get_mut("path") {
                    remap(path);
                }
            }
        }
        if let Some(active) = layout.get_mut("group_active").and_then(Value::as_array_mut) {
            for path in active {
                remap(path);
            }
        }
        if let Some(path) = layout
            .get_mut("detached")
            .and_then(|value| value.get_mut("path"))
        {
            remap(path);
        }
    }
}

fn read_checked(path: &Path) -> Result<Option<Vec<u8>>, WorkspaceError> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(path, e)),
        Ok(meta) if !meta.is_file() => {
            return Err(WorkspaceError::Invalid {
                path: path.into(),
                reason: "not a regular file (symlinks are not replaced)".into(),
            })
        }
        Ok(_) => {}
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| io(path, e))?;
    if !file.metadata().map_err(|e| io(path, e))?.is_file() {
        return Err(WorkspaceError::Invalid {
            path: path.into(),
            reason: "not a regular file".into(),
        });
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io(path, e))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(WorkspaceError::Invalid {
            path: path.into(),
            reason: "workspace exceeds 8 MiB".into(),
        });
    }
    Ok(Some(bytes))
}
fn same_lock(path: &Path, file: &File) -> Result<bool, WorkspaceError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(io(path, e)),
    };
    if !metadata.is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owned = file.metadata().map_err(|e| io(path, e))?;
        Ok(metadata.dev() == owned.dev() && metadata.ino() == owned.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(true)
    } // Windows lock handles deny delete/replacement.
}
fn lock_file(path: &Path) -> Result<Option<File>, WorkspaceError> {
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| io(path, e))?;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.is_file() {
            return Err(WorkspaceError::Invalid {
                path: path.into(),
                reason: "lock is not a regular file".into(),
            });
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|e| io(path, e))?;
    if !file.metadata().map_err(|e| io(path, e))?.is_file() {
        return Err(WorkspaceError::Invalid {
            path: path.into(),
            reason: "lock is not a regular file".into(),
        });
    }
    if os_lock::try_lock(&file).map_err(|e| io(path, e))? {
        Ok(Some(file))
    } else {
        Ok(None)
    }
}
fn atomic_write(
    path: &Path,
    bytes: &[u8],
    before_publish: impl FnOnce(&Path) -> Result<(), WorkspaceError>,
) -> Result<(), WorkspaceError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().unwrap();
    let permissions = match std::fs::metadata(path) {
        Ok(meta) if meta.permissions().readonly() => {
            return Err(io(
                path,
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "workspace is read-only",
                ),
            ))
        }
        Ok(meta) => Some(meta.permissions()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io(path, e)),
    };
    let mut temp = None;
    for _ in 0..128 {
        let candidate = parent.join(format!(
            ".workspace.{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temp = Some((candidate, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(&candidate, e)),
        }
    }
    let (tmp, mut file) = temp.ok_or(WorkspaceError::Busy)?;
    let result = (|| {
        if let Some(permissions) = permissions {
            file.set_permissions(permissions).map_err(|e| io(path, e))?;
        }
        file.write_all(bytes).map_err(|e| io(path, e))?;
        file.sync_all().map_err(|e| io(path, e))?;
        drop(file);
        before_publish(&tmp)?;
        replace_file(&tmp, path).map_err(|e| io(path, e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}
#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain([0]).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: both pointers are valid null-terminated strings for the call.
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
mod os_lock {
    use std::os::unix::io::AsRawFd as _;

    /// Takes the exclusive lock without waiting: false when another holds it.
    pub fn try_lock(file: &std::fs::File) -> std::io::Result<bool> {
        try_lock_with(|| {
            // SAFETY: flock on a file descriptor this `File` owns.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        })
    }

    pub(super) fn try_lock_with(
        mut attempt: impl FnMut() -> std::io::Result<()>,
    ) -> std::io::Result<bool> {
        loop {
            match attempt() {
                Ok(()) => return Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) => return Err(e),
            }
        }
    }

    pub fn unlock(file: &std::fs::File) {
        loop {
            // SAFETY: flock on a file descriptor this `File` owns.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0
                || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                break;
            }
        }
    }

    #[allow(dead_code)]
    pub fn unsupported(e: &std::io::Error) -> bool {
        matches!(e.raw_os_error(), Some(code) if code == libc::ENOLCK || code == libc::EOPNOTSUPP || code == libc::ENOTSUP || code == libc::ENOSYS)
    }
}

#[cfg(windows)]
mod os_lock {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    /// `ERROR_LOCK_VIOLATION`: another handle holds the lock.
    const LOCK_VIOLATION: i32 = 33;

    pub fn try_lock(file: &std::fs::File) -> std::io::Result<bool> {
        // SAFETY: a zeroed OVERLAPPED locks from offset 0; the handle is owned by `file`.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
        if ok != 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(LOCK_VIOLATION) {
            Ok(false)
        } else {
            Err(e)
        }
    }

    pub fn unlock(file: &std::fs::File) {
        // SAFETY: as above.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        unsafe { UnlockFileEx(file.as_raw_handle(), 0, u32::MAX, u32::MAX, &mut overlapped) };
    }

    #[allow(dead_code)]
    pub fn unsupported(_e: &std::io::Error) -> bool {
        false
    }
}

#[cfg(not(any(unix, windows)))]
mod os_lock {
    pub fn try_lock(_file: &std::fs::File) -> std::io::Result<bool> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no file locks on this platform",
        ))
    }

    pub fn unlock(_file: &std::fs::File) {}

    #[allow(dead_code)]
    pub fn unsupported(_e: &std::io::Error) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn dropping_session_lease_releases_lock_while_duplicate_description_lives() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::new(dir.path());
        let lease = store.try_acquire_session().unwrap().unwrap();
        let duplicate = lease._lock.try_clone().unwrap();
        assert!(WorkspaceStore::new(dir.path())
            .try_acquire_session()
            .unwrap()
            .is_none());
        drop(lease);
        let successor = WorkspaceStore::new(dir.path())
            .try_acquire_session()
            .unwrap();
        assert!(successor.is_some(), "public lease lifetime must release the OS lock even when its open-file description is duplicated");
        drop(duplicate);
    }

    #[cfg(unix)]
    #[test]
    fn commit_guard_releases_duplicate_description_on_error_return() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commit.lock");
        let mut duplicate = None;
        let mut operation = || -> Result<(), WorkspaceError> {
            let guard = CommitLock(lock_file(&path)?.unwrap());
            duplicate = Some(guard.try_clone().unwrap());
            assert!(lock_file(&path)?.is_none());
            Err(WorkspaceError::Busy)
        };
        assert!(operation().is_err());
        let successor = lock_file(&path).unwrap().unwrap();
        os_lock::unlock(&successor);
        drop(duplicate);
    }

    #[cfg(unix)]
    #[test]
    fn interrupted_lock_attempt_retries_real_flock_and_preserves_busy_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("interrupted.lock");
        let file = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let mut attempts = 0;
        // Fault injection covers EINTR classification; the retry acquires a real OS lock.
        assert!(os_lock::try_lock_with(|| {
            attempts += 1;
            if attempts == 1 {
                Err(std::io::Error::from_raw_os_error(libc::EINTR))
            } else {
                assert!(os_lock::try_lock(&file)?);
                Ok(())
            }
        })
        .unwrap());
        assert_eq!(attempts, 2);
        assert!(
            lock_file(&path).unwrap().is_none(),
            "a separate real descriptor must observe contention"
        );
        let error = os_lock::try_lock_with(|| Err(std::io::Error::from_raw_os_error(libc::EBADF)))
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
        os_lock::unlock(&file);
        let successor = lock_file(&path).unwrap().unwrap();
        os_lock::unlock(&successor);
    }

    #[test]
    fn failed_atomic_publication_preserves_original_and_removes_private_temp() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::new(dir.path());
        let base = store.load().unwrap();
        let patch = WorkspacePatch {
            interpreter: Some(Some(dir.path().join("python"))),
            ..Default::default()
        };
        let base = store.commit(&base, &patch, None).unwrap();
        let original = std::fs::read(Workspace::path_for(dir.path())).unwrap();
        let patch = WorkspacePatch {
            interpreter: Some(None),
            ..Default::default()
        };
        assert!(store
            .commit_inner(&base, &patch, None, None, |temp| {
                assert!(temp.is_file());
                Err(std::io::Error::other("injected before replacement"))
            })
            .is_err());
        assert_eq!(
            std::fs::read(Workspace::path_for(dir.path())).unwrap(),
            original
        );
        assert_eq!(store.load().unwrap().workspace(), base.workspace());
        assert!(!std::fs::read_dir(dir.path().join(".rustpy"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "tmp")));
    }
    #[test]
    fn external_replacement_after_temp_write_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::new(dir.path());
        let base = store.load().unwrap();
        let patch = WorkspacePatch {
            interpreter: Some(Some(dir.path().join("python"))),
            ..Default::default()
        };
        let path = Workspace::path_for(dir.path());
        assert!(matches!(
            store.commit_inner(&base, &patch, None, None, |_| std::fs::write(
                &path,
                b"{\"external\":true}"
            )),
            Err(WorkspaceError::Conflict(_))
        ));
        assert_eq!(std::fs::read(path).unwrap(), b"{\"external\":true}");
    }

    #[cfg(unix)]
    #[test]
    fn interpreter_symlinks_preserve_distinct_sdk_authority_and_stale_guards() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let base_python = root.join("base-python");
        std::fs::write(&base_python, b"owned executable identity fixture").unwrap();
        for sdk in ["first-venv", "second-venv"] {
            let bin = root.join(sdk).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::os::unix::fs::symlink(&base_python, bin.join("python")).unwrap();
        }
        let first = root.join("first-venv/bin/python");
        let second = root.join("second-venv/bin/python");
        assert_eq!(
            first.canonicalize().unwrap(),
            second.canonicalize().unwrap()
        );
        let store = WorkspaceStore::new(&root);
        let initial = store.load().unwrap();
        let selected = store
            .commit(
                &initial,
                &WorkspacePatch {
                    interpreter: Some(Some(PathBuf::from("first-venv/bin/python"))),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        assert_eq!(selected.workspace().interpreter.as_ref(), Some(&first));
        assert_eq!(
            store.load().unwrap().workspace().interpreter.as_ref(),
            Some(&first)
        );

        let switched = store
            .commit(
                &selected,
                &WorkspacePatch {
                    interpreter: Some(Some(second.clone())),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        assert_eq!(switched.workspace().interpreter.as_ref(), Some(&second));
        // An unrelated shared-field publication keeps the exact selected SDK.
        let retained = store
            .commit(
                &selected,
                &WorkspacePatch {
                    startup_tasks: [("owned startup".into(), true)].into(),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        assert_eq!(retained.workspace().interpreter.as_ref(), Some(&second));
        assert_eq!(
            store.load().unwrap().workspace().interpreter.as_ref(),
            Some(&second)
        );
        let bytes = std::fs::read(Workspace::path_for(&root)).unwrap();
        assert!(matches!(
            store.commit(
                &selected,
                &WorkspacePatch {
                    interpreter: Some(Some(first)),
                    ..Default::default()
                },
                None,
            ),
            Err(WorkspaceError::Conflict(_))
        ));
        assert_eq!(std::fs::read(Workspace::path_for(&root)).unwrap(), bytes);
    }
}
