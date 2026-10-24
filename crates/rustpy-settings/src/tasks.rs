//! Tasks & Contexts (`.rustpy/tasks.json`): named work items with their own
//! editor context (open files) and tracked time, plus named saved contexts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{OpenFileState, PROJECT_DIR_NAME};

pub const DEFAULT_TASK_ID: &str = "default";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorContext {
    pub open_files: Vec<OpenFileState>,
    pub active_file: Option<PathBuf>,
    pub tool_windows: Option<crate::ToolWindowLayout>,
    pub editor_layout: Option<crate::EditorLayout>,
    pub breakpoints: Option<Vec<crate::LineBreakpointState>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Task {
    /// Stable id: "default", "local-<n>" or the issue key (e.g. "#42").
    pub id: String,
    pub summary: String,
    /// Issue URL for tasks opened from an issue tracker.
    pub url: Option<String>,
    /// Branch created/checked out for this task.
    pub branch: Option<String>,
    pub context: EditorContext,
    /// Unix seconds.
    pub created: u64,
    pub updated: u64,
    /// Time spent while the task was active (seconds).
    pub time_spent: u64,
    pub closed: bool,
    pub tracker: Option<String>,
    pub remote_id: Option<String>,
    pub issue_key: Option<String>,
    pub base_branch: Option<String>,
    pub work_items: Vec<crate::tasks_workspace::WorkItem>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NamedContext {
    pub name: String,
    pub context: EditorContext,
    pub saved: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskStore {
    pub tasks: Vec<Task>,
    pub active: Option<String>,
    pub contexts: Vec<NamedContext>,
    pub next_local: u32,
    pub preferences: crate::tasks_workspace::Preferences,
    pub next_work_item: u64,
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "1h 05m", "12m", "40s".
pub fn format_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

/// Branch name suggestion for a task: "42-fix-login-crash".
pub fn branch_name_for(id: &str, summary: &str) -> String {
    let mut slug = String::new();
    for c in summary.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    let prefix: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    match (
        prefix.is_empty() || id.starts_with("local-"),
        slug.is_empty(),
    ) {
        (true, true) => "task".into(),
        (true, false) => slug,
        (false, true) => prefix,
        (false, false) => format!("{prefix}-{slug}"),
    }
}

impl TaskStore {
    pub fn path_for(project: &Path) -> PathBuf {
        project.join(PROJECT_DIR_NAME).join("tasks.json")
    }

    pub fn load(project: &Path) -> TaskStore {
        let mut s: TaskStore = std::fs::read_to_string(Self::path_for(project))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        s.ensure_default();
        s
    }

    pub fn save(&self, project: &Path) -> anyhow::Result<()> {
        let path = Self::path_for(project);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    fn ensure_default(&mut self) {
        if !self.tasks.iter().any(|t| t.id == DEFAULT_TASK_ID) {
            let now = now_secs();
            self.tasks.insert(
                0,
                Task {
                    id: DEFAULT_TASK_ID.into(),
                    summary: "Default task".into(),
                    created: now,
                    updated: now,
                    ..Default::default()
                },
            );
        }
        if self
            .active
            .as_deref()
            .is_none_or(|a| !self.tasks.iter().any(|t| t.id == a))
        {
            self.active = Some(DEFAULT_TASK_ID.into());
        }
    }

    pub fn active_task(&self) -> Option<&Task> {
        self.active
            .as_deref()
            .and_then(|a| self.tasks.iter().find(|t| t.id == a))
    }

    pub fn active_task_mut(&mut self) -> Option<&mut Task> {
        let a = self.active.clone()?;
        self.tasks.iter_mut().find(|t| t.id == a)
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Creates a local task and returns its id.
    pub fn create_local(&mut self, summary: &str) -> String {
        self.next_local += 1;
        let id = format!("local-{}", self.next_local);
        let now = now_secs();
        self.tasks.push(Task {
            id: id.clone(),
            summary: summary.trim().to_string(),
            created: now,
            updated: now,
            ..Default::default()
        });
        id
    }

    /// Adds (or reopens) an issue-backed task.
    pub fn upsert_issue(&mut self, id: &str, summary: &str, url: Option<String>) {
        let now = now_secs();
        match self.tasks.iter_mut().find(|t| t.id == id) {
            Some(t) => {
                t.summary = summary.to_string();
                t.url = url;
                t.closed = false;
                t.updated = now;
            }
            None => self.tasks.push(Task {
                id: id.into(),
                summary: summary.into(),
                url,
                created: now,
                updated: now,
                ..Default::default()
            }),
        }
    }

    /// Server-scoped identity avoids collisions; matching legacy URL retains its context.
    pub fn upsert_remote(
        &mut self,
        server: &str,
        key: &str,
        remote: &str,
        summary: &str,
        url: &str,
    ) -> String {
        let id = self
            .tasks
            .iter()
            .find(|task| {
                task.tracker.is_none() && task.url.as_deref() == Some(url) && !url.is_empty()
            })
            .map(|task| task.id.clone())
            .unwrap_or_else(|| format!("{server}::{key}"));
        self.upsert_issue(&id, summary, Some(url.into()));
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            task.tracker = Some(server.into());
            task.remote_id = Some(remote.into());
            task.issue_key = Some(key.into());
        }
        id
    }

    /// Stores `current` into the active task and activates `id`; returns the
    /// context to restore.
    pub fn switch_to(&mut self, id: &str, current: EditorContext) -> Option<EditorContext> {
        if let Some(t) = self.active_task_mut() {
            t.context = current;
            t.updated = now_secs();
        }
        let target = self.tasks.iter_mut().find(|t| t.id == id)?;
        target.closed = false;
        target.updated = now_secs();
        let ctx = target.context.clone();
        self.active = Some(id.to_string());
        Some(ctx)
    }

    /// Marks the active task closed and falls back to the default task.
    pub fn close_active(&mut self, current: EditorContext) -> Option<EditorContext> {
        let id = self.active.clone()?;
        if id == DEFAULT_TASK_ID {
            return None;
        }
        let ctx = self.switch_to(DEFAULT_TASK_ID, current);
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.closed = true;
        }
        ctx
    }

    pub fn add_time(&mut self, secs: u64) {
        if secs == 0 {
            return;
        }
        let now = now_secs();
        let reuse = self
            .active_task()
            .and_then(|task| task.work_items.last())
            .is_some_and(|item| {
                !item.sent
                    && !item.sending
                    && item.description == "Tracked active time"
                    && item.started / 86400 == now / 86400
                    && item
                        .started
                        .saturating_add(item.seconds)
                        .abs_diff(now.saturating_sub(secs))
                        <= 5
            });
        if !reuse {
            self.next_work_item = self.next_work_item.saturating_add(1);
        }
        let id = self.next_work_item;
        if let Some(task) = self.active_task_mut() {
            task.time_spent = task.time_spent.saturating_add(secs);
            if reuse {
                if let Some(item) = task.work_items.last_mut() {
                    item.seconds = item.seconds.saturating_add(secs);
                }
            } else {
                task.work_items.push(crate::tasks_workspace::WorkItem {
                    id,
                    started: now.saturating_sub(secs),
                    seconds: secs,
                    description: "Tracked active time".into(),
                    ..Default::default()
                });
            }
        }
    }

    /// Open tasks, most recently updated first (the active one first).
    pub fn recent_open(&self) -> Vec<&Task> {
        let mut v: Vec<&Task> = self.tasks.iter().filter(|t| !t.closed).collect();
        v.sort_by(|a, b| {
            (Some(&b.id) == self.active.as_ref())
                .cmp(&(Some(&a.id) == self.active.as_ref()))
                .then(b.updated.cmp(&a.updated))
        });
        v
    }

    pub fn save_context(&mut self, name: &str, ctx: EditorContext) {
        let now = now_secs();
        self.contexts.retain(|c| c.name != name);
        self.contexts.insert(
            0,
            NamedContext {
                name: name.to_string(),
                context: ctx,
                saved: now,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(files: &[&str]) -> EditorContext {
        EditorContext {
            open_files: files
                .iter()
                .map(|f| OpenFileState {
                    path: PathBuf::from(f),
                    ..Default::default()
                })
                .collect(),
            active_file: files.first().map(PathBuf::from),
            ..Default::default()
        }
    }

    #[test]
    fn default_task_exists_and_switching_swaps_contexts() {
        let mut s = TaskStore::default();
        s.ensure_default();
        assert_eq!(s.active.as_deref(), Some(DEFAULT_TASK_ID));
        let id = s.create_local("Fix login crash");
        let restored = s.switch_to(&id, ctx(&["a.py", "b.py"])).unwrap();
        assert!(restored.open_files.is_empty());
        assert_eq!(s.get(DEFAULT_TASK_ID).unwrap().context.open_files.len(), 2);
        let back = s.switch_to(DEFAULT_TASK_ID, ctx(&["c.py"])).unwrap();
        assert_eq!(back.open_files.len(), 2);
        assert_eq!(
            s.get(&id).unwrap().context.open_files[0].path,
            PathBuf::from("c.py")
        );
    }

    #[test]
    fn closing_returns_to_default_and_hides_task() {
        let mut s = TaskStore::default();
        s.ensure_default();
        let id = s.create_local("Refactor parser");
        s.switch_to(&id, EditorContext::default());
        s.add_time(90);
        assert_eq!(s.get(&id).unwrap().time_spent, 90);
        s.close_active(ctx(&["x.py"]));
        assert_eq!(s.active.as_deref(), Some(DEFAULT_TASK_ID));
        assert!(s.recent_open().iter().all(|t| t.id != id));
        assert!(
            s.close_active(EditorContext::default()).is_none(),
            "the default task cannot be closed"
        );
    }

    #[test]
    fn sent_or_inflight_time_is_never_silently_extended() {
        let mut store = TaskStore::default();
        store.ensure_default();
        let task = store.create_local("Fix issue");
        store.switch_to(&task, EditorContext::default());
        store.add_time(30);
        let first = store.active_task().unwrap().work_items[0].id;
        store.active_task_mut().unwrap().work_items[0].sending = true;
        store.add_time(20);
        assert_eq!(store.active_task().unwrap().work_items.len(), 2);
        assert_eq!(store.active_task().unwrap().work_items[0].seconds, 30);
        store.active_task_mut().unwrap().work_items[0].sent = true;
        store.add_time(10);
        assert_eq!(store.active_task().unwrap().work_items[0].id, first);
        assert_eq!(store.active_task().unwrap().time_spent, 60);
        let json = serde_json::to_string(&store).unwrap();
        let loaded: TaskStore = serde_json::from_str(&json).unwrap();
        assert!(!loaded.active_task().unwrap().work_items[0].sending);
        assert!(loaded.active_task().unwrap().work_items[0].sent);
    }

    #[test]
    fn task_context_roundtrip_keeps_layout_and_opt_in_breakpoints() {
        let context = EditorContext {
            tool_windows: Some(crate::ToolWindowLayout::default()),
            editor_layout: Some(crate::EditorLayout::default()),
            breakpoints: Some(vec![]),
            ..Default::default()
        };
        let back: EditorContext =
            serde_json::from_str(&serde_json::to_string(&context).unwrap()).unwrap();
        assert_eq!(back, context);
        let legacy: EditorContext =
            serde_json::from_str(r#"{"open_files":[],"active_file":null}"#).unwrap();
        assert!(legacy.breakpoints.is_none());
        assert!(legacy.tool_windows.is_none());
    }

    #[test]
    fn tracker_identity_is_scoped_and_legacy_context_is_not_lost() {
        let mut store = TaskStore::default();
        store.ensure_default();
        store.upsert_issue("#7", "Old", Some("https://forge.example/r/issues/7".into()));
        store
            .tasks
            .iter_mut()
            .find(|task| task.id == "#7")
            .unwrap()
            .context = ctx(&["saved.py"]);
        assert_eq!(
            store.upsert_remote("one", "#7", "7", "New", "https://forge.example/r/issues/7"),
            "#7"
        );
        let another = store.upsert_remote(
            "two",
            "#7",
            "7",
            "Other",
            "https://other.example/r/issues/7",
        );
        assert_ne!(another, "#7");
        assert_eq!(
            store.get("#7").unwrap().context.open_files[0].path,
            PathBuf::from("saved.py")
        );
        assert_eq!(store.get("#7").unwrap().remote_id.as_deref(), Some("7"));
    }

    #[test]
    fn branch_names_and_durations() {
        assert_eq!(
            branch_name_for("#42", "Fix: login crash on Windows!"),
            "42-fix-login-crash-on-windows"
        );
        assert_eq!(branch_name_for("local-3", "Add tests"), "add-tests");
        assert_eq!(format_duration(3900), "1h 05m");
        assert_eq!(format_duration(125), "2m");
        assert_eq!(format_duration(7), "7s");
    }

    #[test]
    fn roundtrip_and_named_contexts() {
        let dir = std::env::temp_dir().join(format!("rustpy-tasks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = TaskStore::load(&dir);
        s.upsert_issue(
            "#7",
            "Crash on start",
            Some("https://github.com/o/r/issues/7".into()),
        );
        s.save_context("review", ctx(&["a.py"]));
        s.save_context("review", ctx(&["b.py"]));
        s.save(&dir).unwrap();
        let t = TaskStore::load(&dir);
        assert_eq!(t.contexts.len(), 1);
        assert_eq!(
            t.contexts[0].context.open_files[0].path,
            PathBuf::from("b.py")
        );
        assert_eq!(
            t.get("#7").unwrap().url.as_deref(),
            Some("https://github.com/o/r/issues/7")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
