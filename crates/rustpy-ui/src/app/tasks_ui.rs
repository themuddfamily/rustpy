//! Tools → Tasks & Contexts: tasks with their own editor context, issue
//! import from GitHub, named contexts and time tracking.

use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_settings::tasks::{self, EditorContext, TaskStore, DEFAULT_TASK_ID};

type IssueList = Vec<(String, String, String)>;

pub(super) struct OpenTaskState {
    pub root: PathBuf,
    pub session: u64,
    pub analysis: std::sync::Weak<Analysis>,
    pub server: usize,
    pub metadata: Vec<crate::task_trackers::Issue>,
    pub search_owner: Option<rustpy_settings::tasks_workspace::Tracker>,
    pub query: String,
    pub selected: usize,
    pub create_branch: bool,
    pub clear_context: bool,
    /// `(id "#42", title, url)` from the issue tracker.
    pub issues: IssueList,
    pub issues_rx:
        Option<rustpy_core::TaskHandle<Result<Vec<crate::task_trackers::Issue>, String>>>,
    pub issues_status: Option<String>,
    pub has_git: bool,
}

/// `owner/repo` of a GitHub remote URL (https, ssh or scp-like syntax).
#[cfg(test)]
fn github_repo(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = rest.splitn(2, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner, repo))
}

/// Parses the GitHub issues API response (pull requests are skipped).
#[cfg(test)]
fn parse_github_issues(json: &str) -> Result<IssueList, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let arr = v.as_array().ok_or_else(|| {
        v["message"]
            .as_str()
            .unwrap_or("unexpected response")
            .to_string()
    })?;
    Ok(arr
        .iter()
        .filter(|i| i.get("pull_request").is_none())
        .filter_map(|i| {
            Some((
                format!("#{}", i["number"].as_u64()?),
                i["title"].as_str()?.to_string(),
                i["html_url"].as_str().unwrap_or_default().to_string(),
            ))
        })
        .collect())
}

impl Drop for OpenTaskState {
    fn drop(&mut self) {
        if let Some(job) = &self.issues_rx {
            job.cancel();
        }
    }
}

impl IdeApp {
    pub(super) fn task_store(&mut self) -> Option<&mut TaskStore> {
        let root = self.project.as_ref()?.root.clone();
        if self.tasks.is_none() {
            self.tasks = Some(TaskStore::load(&root));
        }
        self.tasks.as_mut()
    }

    pub(super) fn flush_task_time(&mut self) {
        let seconds = self.task_time_acc.floor() as u64;
        self.task_time_acc -= seconds as f64;
        if let Some(store) = &mut self.tasks {
            if seconds > 0 {
                store.add_time(seconds);
            }
        }
        self.save_tasks();
    }

    pub(super) fn save_tasks(&self) {
        if let (Some(p), Some(t)) = (&self.project, &self.tasks) {
            if let Err(e) = t.save(&p.root) {
                log::warn!("cannot save tasks: {e}");
            }
        }
    }

    pub(super) fn current_context(&self) -> EditorContext {
        let mut workspace = rustpy_settings::Workspace::default();
        self.breakpoints.write_to(&mut workspace);
        let preferences = self
            .tasks
            .as_ref()
            .map(|s| s.preferences.clone())
            .unwrap_or_default();
        EditorContext {
            tool_windows: preferences.capture_tools.then(|| self.tool_window_layout()),
            editor_layout: Some(self.capture_editor_layout()),
            breakpoints: preferences
                .capture_breakpoints
                .then_some(workspace.line_breakpoints),
            open_files: self.editor_session_files(),
            active_file: self
                .active
                .and_then(|i| self.tabs.get(i))
                .and_then(|t| t.state.path.clone()),
        }
    }

    /// Saves modified files, closes the tabs (when `replace`) and opens `ctx`.
    pub(super) fn restore_context(&mut self, ctx: EditorContext, replace: bool) {
        if !self.save_all() {
            return;
        }
        if replace {
            while !self.tabs.is_empty() {
                self.close_tab(self.tabs.len() - 1);
            }
        }
        for f in &ctx.open_files {
            if f.path.is_file() {
                self.restore_editor_session(f);
            }
        }
        if replace {
            self.restore_editor_layout(ctx.editor_layout.as_ref());
        }
        if let Some(layout) = ctx.tool_windows {
            self.restore_tool_window_layout(&layout);
        }
        if let Some(points) = ctx.breakpoints {
            let old_paths: Vec<_> = self
                .breakpoints
                .all()
                .into_iter()
                .map(|(path, _, _)| path)
                .collect();
            let mut workspace = rustpy_settings::Workspace::default();
            self.breakpoints.write_to(&mut workspace);
            workspace.line_breakpoints = points;
            workspace.breakpoints.clear();
            self.breakpoints = crate::breakpoints::BreakpointStore::from_workspace(&workspace);
            let paths: std::collections::HashSet<_> = old_paths
                .into_iter()
                .chain(self.breakpoints.all().into_iter().map(|(path, _, _)| path))
                .collect();
            for path in paths {
                self.sync_breakpoints(&path);
            }
            self.save_workspace();
        }
        if let Some(active) = ctx.active_file {
            if let Some(i) = self
                .tabs
                .iter()
                .position(|t| t.state.path.as_deref() == Some(active.as_path()))
            {
                self.active = Some(i);
            }
        }
        self.focus_editor = true;
    }

    pub(super) fn open_task_dialog(&mut self) {
        if self.project.is_none() {
            self.set_status("Open a project to use tasks");
            return;
        }
        let root = self.project.as_ref().unwrap().root.clone();
        let preferences = self
            .task_store()
            .map(|s| s.preferences.clone())
            .unwrap_or_default();
        let has_git = self.project.as_ref().is_some_and(|p| p.git.is_some());
        let st = OpenTaskState {
            root,
            session: self.file_session,
            analysis: Arc::downgrade(&self.project.as_ref().unwrap().analysis),
            server: 0,
            metadata: Vec::new(),
            search_owner: None,
            query: String::new(),
            selected: 0,
            create_branch: preferences.create_branch,
            clear_context: true,
            issues: Vec::new(),
            issues_rx: None,
            issues_status: Some(
                "Select a configured tracker and Search, or create a local task".into(),
            ),
            has_git,
        };
        self.dialog = Dialog::OpenTask(Box::new(st));
    }

    /// Switches to task `id` (stores the current context into the active task).
    pub(super) fn switch_task(&mut self, id: &str, create_branch: bool, clear_context: bool) {
        if !self.save_all() {
            return;
        }
        self.flush_task_time();
        let base_branch = self
            .project
            .as_ref()
            .and_then(|p| p.git.as_ref())
            .and_then(|g| g.current_branch());
        let current = self.current_context();
        let previous = self.tasks.clone();
        let Some(store) = self.task_store() else {
            return;
        };
        let Some(target) = store.switch_to(id, current) else {
            return;
        };
        let (summary, task_id, mut branch) = store
            .active_task()
            .map(|t| (t.summary.clone(), t.id.clone(), t.branch.clone()))
            .unwrap_or_default();
        let preferences = store.preferences.clone();
        let issue_key = store
            .active_task()
            .and_then(|t| t.issue_key.clone())
            .unwrap_or(task_id.clone());
        if create_branch && branch.is_none() {
            let suggested = rustpy_settings::tasks_workspace::branch(
                &preferences.branch_pattern,
                &issue_key,
                &summary,
            );
            if suggested.is_empty() {
                self.tasks = previous;
                self.set_status("Task unchanged: branch pattern produces an empty name");
                return;
            }
            branch = Some(suggested);
        }
        if let Some(running) = branch.as_ref().and(self.git_busy()).map(str::to_owned) {
            self.tasks = previous;
            self.save_tasks();
            self.set_status(format!("Task unchanged: {running}"));
            return;
        }
        if let Some(mut b) = branch.clone() {
            let is_new = self
                .tasks
                .as_ref()
                .and_then(|store| store.active_task())
                .is_some_and(|task| task.branch.is_none());
            if is_new {
                let base = b.clone();
                let mut suffix = 2;
                while self
                    .project
                    .as_ref()
                    .and_then(|p| p.git.as_ref())
                    .is_some_and(|git| {
                        git.branches()
                            .iter()
                            .any(|branch| branch.name == b && !branch.is_remote)
                    })
                    || self.tasks.as_ref().is_some_and(|store| {
                        store
                            .tasks
                            .iter()
                            .any(|task| task.id != id && task.branch.as_deref() == Some(&b))
                    })
                {
                    b = format!("{base}-{suffix}");
                    suffix += 1;
                    if suffix > 1024 {
                        self.tasks = previous;
                        self.set_status("No unused branch name is available for this task");
                        return;
                    }
                }
            }
            let result = self
                .project
                .as_ref()
                .and_then(|p| p.git.as_ref().map(|g| (g, &p.root)))
                .map(|(g, root)| {
                    crate::git_panel::snapshot_working_tree(g, root, "Before checkout", false);
                    if g.branches().iter().any(|x| x.name == b && !x.is_remote) {
                        g.checkout_branch(&b)
                    } else {
                        g.create_branch(&b, true)
                    }
                });
            match result {
                Some(Ok(())) => {
                    if let Some(t) = self.task_store().and_then(|s| s.active_task_mut()) {
                        t.branch = Some(b.clone());
                        if t.base_branch.is_none() {
                            t.base_branch = base_branch.clone().filter(|base| base != &b);
                        }
                    }
                    self.reload_changed_files();
                }
                Some(Err(e)) => {
                    self.tasks = previous;
                    self.save_tasks();
                    self.set_status(format!("Task unchanged: cannot switch to branch {b}: {e}"));
                    return;
                }
                None => {
                    self.tasks = previous;
                    self.save_tasks();
                    self.set_status("Task unchanged: repository unavailable");
                    return;
                }
            }
        }
        if preferences.create_changelist {
            if let Some(root) = self
                .project
                .as_ref()
                .and_then(|p| p.git.as_ref())
                .map(|g| g.root().to_path_buf())
            {
                let name = rustpy_settings::tasks_workspace::expand(
                    &preferences.changelist_pattern,
                    &issue_key,
                    &summary,
                );
                self.git_panel
                    .workspace_task_named(&root, &task_id, &summary, &name);
            }
        }
        if clear_context || !target.open_files.is_empty() {
            self.restore_context(target, clear_context);
        }
        self.save_tasks();
        self.set_status(format!("Switched to task: {summary}"));
    }

    pub(super) fn open_switch_task_popup(&mut self) {
        let Some(store) = self.task_store() else {
            return;
        };
        let active = store.active.clone();
        let rows: Vec<(String, String)> = store
            .recent_open()
            .iter()
            .map(|t| {
                let mark = if Some(&t.id) == active.as_ref() {
                    " (active)"
                } else {
                    ""
                };
                let key = if t.id == DEFAULT_TASK_ID || t.id.starts_with("local-") {
                    "task".to_string()
                } else {
                    t.id.clone()
                };
                (
                    key,
                    format!(
                        "{}{} · {}",
                        t.summary,
                        mark,
                        tasks::format_duration(t.time_spent)
                    ),
                )
            })
            .collect();
        let ids: Vec<String> = store.recent_open().iter().map(|t| t.id.clone()).collect();
        self.task_choice_ids = ids;
        self.choice = Some(ChoicePopup::new(
            ChoiceKind::SwitchTask,
            "Switch to Task".into(),
            rows,
        ));
    }

    pub(super) fn open_load_context_popup(&mut self) {
        let Some(store) = self.task_store() else {
            return;
        };
        let mut rows = Vec::new();
        let mut ids = Vec::new();
        for c in &store.contexts {
            rows.push((
                "context".to_string(),
                format!("{} · {} file(s)", c.name, c.context.open_files.len()),
            ));
            ids.push(format!("ctx:{}", c.name));
        }
        for t in store
            .tasks
            .iter()
            .filter(|t| !t.context.open_files.is_empty())
        {
            rows.push((
                "task".to_string(),
                format!("{} · {} file(s)", t.summary, t.context.open_files.len()),
            ));
            ids.push(format!("task:{}", t.id));
        }
        if rows.is_empty() {
            self.set_status("No saved contexts (Tools › Tasks & Contexts › Save Context)");
            return;
        }
        self.task_choice_ids = ids;
        self.choice = Some(ChoicePopup::new(
            ChoiceKind::LoadContext,
            "Load Context".into(),
            rows,
        ));
    }

    /// Handles a pick from the Switch Task / Load Context popups.
    pub(super) fn apply_task_choice(&mut self, kind: ChoiceKind, idx: usize) {
        let Some(id) = self.task_choice_ids.get(idx).cloned() else {
            return;
        };
        match kind {
            ChoiceKind::SwitchTask => self.switch_task(&id, false, true),
            ChoiceKind::LoadContext => {
                let ctx = self.task_store().and_then(|s| {
                    if let Some(name) = id.strip_prefix("ctx:") {
                        s.contexts
                            .iter()
                            .find(|c| c.name == name)
                            .map(|c| c.context.clone())
                    } else {
                        id.strip_prefix("task:")
                            .and_then(|t| s.get(t))
                            .map(|t| t.context.clone())
                    }
                });
                if let Some(ctx) = ctx {
                    self.restore_context(ctx, true);
                }
            }
            _ => {}
        }
    }

    pub(super) fn close_active_task(&mut self) {
        self.task_workspace_open(true);
    }

    pub(super) fn close_active_task_now(&mut self) {
        if !self.save_all() {
            return;
        }
        self.flush_task_time();
        let current = self.current_context();
        let Some(store) = self.task_store() else {
            return;
        };
        let name = store
            .active_task()
            .map(|t| t.summary.clone())
            .unwrap_or_default();
        match store.close_active(current) {
            Some(ctx) => {
                self.restore_context(ctx, true);
                self.save_tasks();
                self.set_status(format!("Closed task: {name}"));
            }
            None => self.set_status("The default task cannot be closed"),
        }
    }

    /// Accumulates active time for the current task (window focused and the
    /// user active within the last five minutes).
    pub(super) fn tasks_tick(&mut self, ctx: &egui::Context) {
        let (now, focused, active_input) = ctx.input(|i| {
            (
                i.time,
                i.focused,
                !i.events.is_empty() || i.pointer.is_moving(),
            )
        });
        if active_input {
            self.last_user_input = now;
        }
        let dt = (now - self.last_task_tick).clamp(0.0, 1.0);
        self.last_task_tick = now;
        self.draw_tasks_workspace(ctx);
        if !self.coordinated_quit_active()
            && focused
            && now - self.last_user_input < 300.0
            && self.tasks.as_ref().is_some_and(|s| s.preferences.tracking)
        {
            self.task_time_acc += dt;
            if self.task_time_acc >= 30.0 {
                let secs = self.task_time_acc.floor();
                self.task_time_acc -= secs;
                if let Some(s) = self.tasks.as_mut() {
                    s.add_time(secs as u64);
                }
                self.save_tasks();
            }
        }
    }

    pub(super) fn draw_open_task(&mut self, ctx: &egui::Context, mut st: Box<OpenTaskState>) {
        if self.file_session != st.session
            || !self.project.as_ref().is_some_and(|p| {
                p.root == st.root
                    && std::sync::Weak::ptr_eq(&st.analysis, &Arc::downgrade(&p.analysis))
            })
        {
            return;
        }
        let search_current = !self.project_safe_mode()
            && st.search_owner.as_ref().is_some_and(|owner| {
                self.tasks
                    .as_ref()
                    .is_some_and(|store| store.preferences.trackers.get(st.server) == Some(owner))
            });
        if st.search_owner.is_some() && !search_current {
            st.issues.clear();
            st.metadata.clear();
            st.search_owner = None;
            st.issues_status = Some(
                "Tracker or project authority changed; trust the project and search again".into(),
            );
        }
        let theme = self.theme.clone();
        if let Some(rx) = &mut st.issues_rx {
            if let Some(result) = rx.try_result() {
                let res = result
                    .map_err(|error| error.to_string())
                    .and_then(|value| value);
                let res = if search_current {
                    res
                } else {
                    Err(
                        "Tracker or project authority changed; trust the project and search again"
                            .into(),
                    )
                };

                match res {
                    Ok(list) => {
                        st.issues_status = Some(format!("{} open issue(s)", list.len()));
                        st.issues = list
                            .iter()
                            .map(|issue| {
                                (
                                    format!("{}::{}", issue.tracker, issue.id),
                                    issue.summary.clone(),
                                    issue.url.clone(),
                                )
                            })
                            .collect();
                        st.metadata = list;
                    }
                    Err(e) => st.issues_status = Some(e),
                }
                st.issues_rx = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }
        // Rows: (task id or "" for new, label, url)
        let q = st.query.trim().to_lowercase();
        let mut rows: Vec<(String, String, Option<String>)> = Vec::new();
        if !q.is_empty() {
            rows.push((
                String::new(),
                format!("Create new task '{}'", st.query.trim()),
                None,
            ));
        }
        if let Some(store) = self.task_store() {
            for t in store.recent_open() {
                if q.is_empty()
                    || t.summary.to_lowercase().contains(&q)
                    || t.id.to_lowercase().contains(&q)
                {
                    rows.push((
                        t.id.clone(),
                        format!(
                            "{} {}  ({})",
                            t.issue_key.as_deref().unwrap_or(&t.id),
                            t.summary,
                            tasks::format_duration(t.time_spent)
                        ),
                        t.url.clone(),
                    ));
                }
            }
            for (id, title, url) in &st.issues {
                if store.get(id).is_some_and(|t| !t.closed) {
                    continue;
                }
                if q.is_empty()
                    || title.to_lowercase().contains(&q)
                    || id.to_lowercase().contains(&q)
                {
                    rows.push((
                        format!("issue:{id}"),
                        format!(
                            "{} {title}",
                            st.metadata
                                .iter()
                                .find(|item| format!("{}::{}", item.tracker, item.id) == *id)
                                .map(|item| item.id.as_str())
                                .unwrap_or(id)
                        ),
                        Some(url.clone()),
                    ));
                }
            }
        }
        st.selected = st.selected.min(rows.len().saturating_sub(1));
        let servers = self
            .tasks
            .as_ref()
            .map(|s| s.preferences.trackers.clone())
            .unwrap_or_default();
        st.server = st.server.min(servers.len().saturating_sub(1));
        let mut search = false;
        let mut chosen: Option<usize> = None;
        let mut open = true;
        let mut cancel = false;
        egui::Window::new("Open Task")
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 90.0))
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("open-task-server")
                        .selected_text(
                            servers
                                .get(st.server)
                                .map(|s| s.name.as_str())
                                .unwrap_or("Local tasks"),
                        )
                        .show_ui(ui, |ui| {
                            for (index, server) in servers.iter().enumerate() {
                                ui.selectable_value(&mut st.server, index, &server.name);
                            }
                        })
                        .combo_named(crate::i18n::tr_static("Task server"));
                    if ui
                        .add_enabled(!servers.is_empty(), egui::Button::new("Search tracker"))
                        .clicked()
                    {
                        search = true;
                    }
                    if ui.button("Configure Servers…").clicked() {
                        self.task_workspace_open(false);
                    }
                });
                let r = ui.add(
                    egui::TextEdit::singleline(&mut st.query)
                        .hint_text("Task name or issue (e.g. #42)")
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
                if r.changed() {
                    st.selected = 0;
                }
                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .show(ui, |ui| {
                        for (i, (id, label, url)) in rows.iter().enumerate() {
                            let text = egui::RichText::new(label).color(if id.is_empty() {
                                theme.accent
                            } else {
                                theme.text
                            });
                            let resp = ui.selectable_label(i == st.selected, text);
                            let resp = match url {
                                Some(u) if !u.is_empty() => resp.on_hover_text(u),
                                _ => resp,
                            };
                            if resp.clicked() {
                                chosen = Some(i);
                            }
                        }
                    });
                if let Some(s) = &st.issues_status {
                    ui.label(egui::RichText::new(s).color(theme.text_dim).small());
                }
                ui.horizontal(|ui| {
                    ui.add_enabled(
                        st.has_git,
                        egui::Checkbox::new(&mut st.create_branch, "Create/checkout branch"),
                    );
                    ui.checkbox(&mut st.clear_context, "Clear context (close open files)");
                });
                ui.input(|i| {
                    if i.key_pressed(egui::Key::ArrowDown) {
                        st.selected = (st.selected + 1).min(rows.len().saturating_sub(1));
                    }
                    if i.key_pressed(egui::Key::ArrowUp) {
                        st.selected = st.selected.saturating_sub(1);
                    }
                    if i.key_pressed(egui::Key::Enter) && !rows.is_empty() {
                        chosen = Some(st.selected);
                    }
                    if i.key_pressed(egui::Key::Escape) {
                        cancel = true;
                    }
                });
            });
        if search {
            if let Some(old) = st.issues_rx.take() {
                old.cancel();
            }
            st.issues.clear();
            st.metadata.clear();
            if self.project_safe_mode() {
                st.issues_status =
                    Some("Trust the project before searching its task tracker".into());
            } else if let Some(server) = servers.get(st.server).cloned() {
                st.search_owner = Some(server.clone());
                let query = st.query.clone();
                let accounts = self.settings.github.clone();
                st.issues_rx = Some(
                    rustpy_core::TaskManager::global()
                        .task(format!("Searching {} tasks", server.name))
                        .spawn(move |progress| {
                            Ok(crate::task_trackers::search(
                                &server,
                                &query,
                                &accounts,
                                progress.token(),
                            ))
                        }),
                );
                st.issues_status = Some("Searching authenticated tracker…".into());
            }
        }
        if let Some(i) = chosen {
            let (id, _, _url) = rows[i].clone();
            let summary = st.query.trim().to_string();
            let id = if id.is_empty() {
                self.task_store()
                    .map(|s| s.create_local(&summary))
                    .unwrap_or_default()
            } else if let Some(issue) = id.strip_prefix("issue:") {
                if !search_current {
                    self.set_status("Tracker or project authority changed; search again before opening an issue");
                    return;
                }
                let title = st
                    .issues
                    .iter()
                    .find(|(k, _, _)| k == issue)
                    .map(|(_, t, _)| t.clone())
                    .unwrap_or_default();
                if let Some(metadata) = st
                    .metadata
                    .iter()
                    .find(|item| format!("{}::{}", item.tracker, item.id) == issue)
                {
                    self.task_store()
                        .map(|store| {
                            store.upsert_remote(
                                &metadata.tracker,
                                &metadata.id,
                                &metadata.remote_id,
                                &title,
                                &metadata.url,
                            )
                        })
                        .unwrap_or_default()
                } else {
                    self.set_status("Tracker result is no longer owned by this search");
                    return;
                }
            } else {
                id
            };
            self.switch_task(&id, st.create_branch, st.clear_context);
            return;
        }
        if open && !cancel {
            self.dialog = Dialog::OpenTask(st);
        } else {
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_remote_urls() {
        assert_eq!(
            github_repo("https://github.com/rustpy/rustpy.git"),
            Some(("rustpy".into(), "rustpy".into()))
        );
        assert_eq!(
            github_repo("git@github.com:owner/repo.git"),
            Some(("owner".into(), "repo".into()))
        );
        assert_eq!(
            github_repo("ssh://git@github.com/owner/repo"),
            Some(("owner".into(), "repo".into()))
        );
        assert_eq!(github_repo("https://gitlab.com/owner/repo.git"), None);
        assert_eq!(github_repo("https://github.com/owner"), None);
    }

    #[test]
    fn issues_json_skips_pull_requests() {
        let json = r#"[{"number":1,"title":"Bug","html_url":"https://github.com/o/r/issues/1"},{"number":2,"title":"PR","html_url":"u","pull_request":{}}]"#;
        let list = parse_github_issues(json).unwrap();
        assert_eq!(
            list,
            vec![(
                "#1".to_string(),
                "Bug".to_string(),
                "https://github.com/o/r/issues/1".to_string()
            )]
        );
        assert_eq!(
            parse_github_issues(r#"{"message":"Not Found"}"#),
            Err("Not Found".to_string())
        );
    }
}
