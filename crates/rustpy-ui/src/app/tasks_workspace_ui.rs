//! Tracker preferences, work items, close-task transaction and commit integration.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_core::{TaskHandle, TaskManager};
use rustpy_settings::tasks_workspace::{self as model, Tracker, TrackerKind, WorkItem};
#[derive(Default)]
pub(super) struct Workspace {
    pub visible: bool,
    pub root: Option<PathBuf>,
    pub selected: usize,
    pub secret: String,
    pub notice: String,
    pub minutes: u64,
    pub description: String,
    pub close: bool,
    pub commit: bool,
    pub merge: bool,
    show_closed: bool,
    pending_close: Option<(PathBuf, String, bool)>,
    inflight: Option<WorklogOwner>,
    job: Option<TaskHandle<Result<Outcome, String>>>,
}
enum Outcome {
    Credential,
    Sent { receipt: String },
}
struct WorklogOwner {
    request: u64,
    root: PathBuf,
    session: u64,
    analysis: std::sync::Weak<Analysis>,
    tracker: Tracker,
    task: String,
    remote: String,
    item: WorkItem,
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel();
        }
    }
}
impl IdeApp {
    pub(super) fn task_operation_pending(&self) -> bool {
        self.task_workspace.job.is_some()
    }
    pub(super) fn task_operation_stamp(&self) -> String {
        format!(
            "{:?}",
            (
                self.task_workspace.root.as_ref(),
                self.task_workspace.job.as_ref().map(|job| job.id()),
                self.task_workspace.inflight.as_ref().map(|owner| (
                    &owner.task,
                    owner.item.id,
                    owner.request
                )),
                self.tasks
                    .as_ref()
                    .and_then(|store| serde_json::to_string(store).ok())
            )
        )
    }
    pub(super) fn stop_task_operation(&mut self) {
        let request = self.task_workspace.job.as_ref().map(TaskHandle::id);
        if let Some(job) = self.task_workspace.job.take() {
            job.cancel();
        }
        if let Some(owner) = self
            .task_workspace
            .inflight
            .take()
            .filter(|owner| Some(owner.request) == request)
        {
            self.clear_worklog_sending(&owner);
        }
        self.task_workspace.pending_close = None;
    }
    pub(super) fn checkpoint_task_context(&mut self) -> bool {
        self.flush_task_time();
        let context = self.current_context();
        if let Some(task) = self
            .tasks
            .as_mut()
            .and_then(|store| store.active_task_mut())
        {
            task.context = context;
        }
        match (&self.project, &self.tasks) {
            (Some(project), Some(store)) => match store.save(&project.root) {
                Ok(()) => true,
                Err(error) => {
                    self.set_status(format!("Cannot save task context: {error}"));
                    false
                }
            },
            _ => true,
        }
    }
    pub(super) fn cancel_task_workspace(&mut self) {
        self.checkpoint_task_context();
        self.stop_task_operation();
        self.task_workspace = Workspace::default();
    }
    pub(super) fn task_status_controls(&mut self, ui: &mut egui::Ui) {
        let tracking = self
            .tasks
            .as_ref()
            .is_some_and(|store| store.preferences.tracking);
        if ui
            .small_button(if tracking {
                "Pause time"
            } else {
                "Resume time"
            })
            .clicked()
        {
            self.flush_task_time();
            if let Some(store) = self.tasks.as_mut() {
                store.preferences.tracking = !tracking;
            }
            self.save_tasks();
        }
        if ui.small_button("Work items").clicked() {
            self.task_workspace_open(false);
        }
    }
    pub(super) fn task_workspace_open(&mut self, close: bool) {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            return;
        };
        if self.task_workspace.root.as_ref() != Some(&root) {
            self.task_workspace = Workspace::default();
            self.task_workspace.root = Some(root);
        }
        if close
            && self
                .tasks
                .as_ref()
                .and_then(|s| s.active_task())
                .is_none_or(|task| task.id == rustpy_settings::tasks::DEFAULT_TASK_ID)
        {
            self.set_status("The default task cannot be closed");
            return;
        }
        self.task_workspace.visible = true;
        self.task_workspace.close = close;
    }
    pub(super) fn take_task_commit_intent(&mut self) -> Option<(PathBuf, String, bool)> {
        self.task_workspace.pending_close.take()
    }
    pub(super) fn task_commit_completed(&mut self, intent: Option<(PathBuf, String, bool)>) {
        if let Some((root, id, merge)) = intent {
            if self.project.as_ref().is_some_and(|p| p.root == root)
                && self
                    .tasks
                    .as_ref()
                    .is_some_and(|s| s.active.as_deref() == Some(&id))
            {
                self.finish_task_close(merge);
            } else {
                self.set_status("Commit finished; task ownership changed, so no task was closed");
            }
        }
    }
    fn finish_task_close(&mut self, merge: bool) {
        if !self.save_all() {
            return;
        }
        let branch = self
            .tasks
            .as_ref()
            .and_then(|s| s.active_task())
            .and_then(|t| t.branch.clone().zip(t.base_branch.clone()));
        if merge {
            let Some((task, base)) = branch else {
                self.set_status("This task has no recorded task/base branch pair");
                return;
            };
            if self.git_busy().is_some() {
                self.set_status("Wait for the current Git operation before closing this task");
                return;
            }
            let result = self
                .project
                .as_ref()
                .and_then(|p| p.git.as_ref())
                .map(|git| {
                    anyhow::ensure!(
                        git.status()?.is_empty(),
                        "Commit or shelve working changes before merging a task"
                    );
                    anyhow::ensure!(
                        git.current_branch().as_deref() == Some(task.as_str()),
                        "Task branch is not checked out"
                    );
                    git.checkout_branch(&base)?;
                    let outcome = git.merge_branch(&task)?;
                    anyhow::ensure!(
                        outcome.conflicts.is_empty(),
                        "Task merge has conflicts; resolve them before closing"
                    );
                    Ok::<_, anyhow::Error>(())
                });
            match result {
                Some(Ok(())) => self.reload_changed_files(),
                Some(Err(error)) => {
                    self.set_status(format!("Task remains open: {error}"));
                    return;
                }
                None => {
                    self.set_status("Task repository is unavailable");
                    return;
                }
            }
        }
        self.close_active_task_now();
        self.task_workspace.visible = false;
    }
    pub(super) fn task_commit_ui(&mut self, ui: &mut egui::Ui) {
        let Some(store) = self.tasks.as_ref() else {
            return;
        };
        let suggestions: Vec<_> = store
            .tasks
            .iter()
            .filter(|t| !t.closed && t.id != rustpy_settings::tasks::DEFAULT_TASK_ID)
            .take(30)
            .map(|t| {
                (
                    t.issue_key.as_deref().unwrap_or(&t.id).to_owned(),
                    t.summary.clone(),
                )
            })
            .collect();
        let patterns = store.preferences.issue_navigation.clone();
        let known: Vec<_> = store
            .tasks
            .iter()
            .filter_map(|t| {
                Some((
                    t.issue_key.as_deref().unwrap_or(&t.id).to_owned(),
                    t.url.clone()?,
                ))
            })
            .collect();
        crate::task_trackers::set_commit_tasks(
            ui.ctx(),
            crate::task_trackers::CommitTasks {
                suggestions: suggestions.clone(),
                known: known.clone(),
                patterns: patterns.clone(),
            },
        );
        ui.horizontal_wrapped(|ui| {
            ui.menu_button("Insert task ID", |ui| {
                for (id, title) in &suggestions {
                    if ui.button(format!("{id} {title}")).clicked() {
                        if !self.git_panel.message.is_empty()
                            && !self.git_panel.message.ends_with(char::is_whitespace)
                        {
                            self.git_panel.message.push(' ');
                        }
                        self.git_panel.message.push_str(id);
                        ui.close();
                    }
                }
            });
            if ui.button("Format active task commit").clicked() {
                if let Some(task) = store.active_task() {
                    self.git_panel.message = model::expand(
                        &store.preferences.commit_pattern,
                        task.issue_key.as_deref().unwrap_or(&task.id),
                        &task.summary,
                    );
                }
            }
        });
        crate::task_trackers::issue_navigation(ui, &self.git_panel.message);
    }

    pub(super) fn draw_tasks_workspace(&mut self, ctx: &egui::Context) {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            self.task_workspace = Workspace::default();
            return;
        };
        if self
            .task_workspace
            .root
            .as_ref()
            .is_some_and(|owned| owned != &root)
        {
            self.task_workspace = Workspace::default();
        }
        if let Some((request, result)) = self
            .task_workspace
            .job
            .as_mut()
            .and_then(|job| job.try_result().map(|result| (job.id(), result)))
        {
            self.task_workspace.job = None;
            let owner = self
                .task_workspace
                .inflight
                .take()
                .filter(|owner| owner.request == request);
            let publish = owner
                .as_ref()
                .is_some_and(|owner| self.worklog_owner_current(owner));
            if let Some(owner) = &owner {
                self.clear_worklog_sending(owner);
            }

            match result {
                Ok(Ok(Outcome::Credential))=>self.task_workspace.notice="Credential saved in Password Safe".into(),
                Ok(Ok(Outcome::Sent{receipt}))=>{
                    if let Some(owner) = owner.filter(|_| publish) {
                        if let Some(work)=self.tasks.as_mut().and_then(|s|s.tasks.iter_mut().find(|t|t.id==owner.task)).and_then(|t|t.work_items.iter_mut().find(|w|w.id==owner.item.id)){work.sent=true;work.receipt=Some(receipt);self.save_tasks();}
                        self.task_workspace.notice="Work item accepted by tracker".into();
                    } else {
                        self.task_workspace.notice="Tracker accepted the request, but task, tracker or project authority changed; local work item was not modified. Check the tracker before resending.".into();
                    }
                },
                Ok(Err(error))=>self.task_workspace.notice=format!("{error}. No automatic retry; check the tracker before resending a timed-out request."),
                Err(error)=>self.task_workspace.notice=error.to_string(),
            }
        }
        if !self.task_workspace.visible || self.coordinated_quit_active() {
            return;
        }
        let Some(mut store) = self.tasks.clone() else {
            return;
        };
        let mut visible = true;
        let mut save_secret = None;
        let mut send = None;
        let mut close = false;
        let mut commit = false;
        let mut switch = None;
        egui::Window::new(if self.task_workspace.close{"Close Active Task"}else{"Tasks, Trackers & Context Settings"}).open(&mut visible).default_size(Vec2::new(740.,650.)).show_modal(ctx,|ui|{
            egui::ScrollArea::vertical().max_height(600.).show(ui,|ui|{
            let state=&mut self.task_workspace;
            if state.close {
                if let Some(task)=store.active_task(){ui.heading(format!("{} {}",task.issue_key.as_deref().unwrap_or(&task.id),task.summary));}
                ui.checkbox(&mut state.commit,"Commit selected changes before closing");
                ui.checkbox(&mut state.merge,"Merge task branch into recorded base branch");
                ui.label("Commit uses the checked files and existing commit checks. Conflicts or failed checks leave the task open.");
                if ui.button(if state.commit{"Commit and close task"}else{"Close task"}).clicked(){close=true;commit=state.commit;}
            }else{
                ui.heading("Tasks");ui.checkbox(&mut state.show_closed,"Show closed tasks");
                for task in store.tasks.iter().filter(|task|state.show_closed||!task.closed).take(100){
                    ui.horizontal(|ui|{ui.label(format!("{} {}{}",task.issue_key.as_deref().unwrap_or(&task.id),task.summary,if task.closed{" (closed)"}else{""}));if ui.button("Switch / reopen").clicked(){switch=Some(task.id.clone());}
if let Some(url)=&task.url{ui.hyperlink_to("Open issue",url);}});
                }
                ui.separator();
                ui.checkbox(&mut store.preferences.create_changelist,"Create/activate a changelist per task");
                ui.checkbox(&mut store.preferences.create_branch,"Create a branch by default");
                ui.label("Changelist pattern");ui.text_edit_singleline(&mut store.preferences.changelist_pattern);
                ui.label("Branch pattern");ui.text_edit_singleline(&mut store.preferences.branch_pattern);
                ui.label("Commit message pattern (${id}, ${summary})");ui.text_edit_singleline(&mut store.preferences.commit_pattern);
                ui.checkbox(&mut store.preferences.capture_tools,"Save/restore tool window layout with context");
                ui.checkbox(&mut store.preferences.capture_breakpoints,"Save/restore line breakpoints with context (off by default)");
                ui.checkbox(&mut store.preferences.tracking,"Track active task time (pause/resume)");
                ui.collapsing("Issue navigation regular expressions",|ui|{
                    let mut remove=None;for(index,pattern)in store.preferences.issue_navigation.iter_mut().enumerate(){ui.horizontal(|ui|{ui.text_edit_singleline(&mut pattern.pattern);ui.label("→");ui.text_edit_singleline(&mut pattern.replacement);if ui.small_button("Remove").clicked(){remove=Some(index);}});}
                    if let Some(index)=remove{store.preferences.issue_navigation.remove(index);}
                    if ui.button("Add navigation pattern").clicked(){store.preferences.issue_navigation.push(model::IssueNavigation{pattern:"([A-Z]+-[0-9]+)".into(),replacement:"https://tracker.example/browse/$1".into()});}
                });
                ui.separator();
                ui.horizontal(|ui|{ui.heading("Tracker servers");if ui.button("Add server").clicked(){let id=format!("tracker-{}-{}",std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d|d.as_nanos()).unwrap_or_default(),std::process::id());store.preferences.trackers.push(Tracker{id,name:"New tracker".into(),..Default::default()});state.selected=store.preferences.trackers.len()-1;}});
                let mut remove=false;
                let previous_server=state.selected;
                egui::ComboBox::from_id_salt("task-server").selected_text(store.preferences.trackers.get(state.selected).map(|s|s.name.as_str()).unwrap_or("No server")).show_ui(ui,|ui|{for(index,server)in store.preferences.trackers.iter().enumerate(){ui.selectable_value(&mut state.selected,index,&server.name);}}).combo_named(crate::i18n::tr_static("Tracker server"));
                if previous_server!=state.selected{state.secret.clear();}
                if let Some(server)=store.preferences.trackers.get_mut(state.selected){
                    ui.text_edit_singleline(&mut server.name);
                    egui::ComboBox::from_id_salt("task-provider").selected_text(server.kind.label()).show_ui(ui,|ui|{for kind in TrackerKind::ALL{ui.selectable_value(&mut server.kind,*kind,kind.label());}}).combo_named(crate::i18n::tr_static("Tracker type"));
                    ui.label("Server URL (installation root)");ui.text_edit_singleline(&mut server.url);
                    ui.label("Repository owner/name, project ID, or Asana workspace GID");ui.text_edit_singleline(&mut server.project);
                    ui.label("Search template (${query}); empty uses provider default");ui.text_edit_singleline(&mut server.search);
                    if matches!(server.kind,TrackerKind::GitHub|TrackerKind::GitLab){
                        egui::ComboBox::from_id_salt("task-account").selected_text(server.account_id.as_deref().unwrap_or("Select shared account")).show_ui(ui,|ui|{for account in &self.settings.github.accounts{ui.selectable_value(&mut server.account_id,Some(account.id.clone()),format!("{} — {}",account.login,account.server));}}).combo_named(crate::i18n::tr_static("Tracker account"));
                        ui.label("Accounts are authenticated in GitHub/GitLab settings.");
                    }else{
                        if matches!(server.kind,TrackerKind::Jira|TrackerKind::Trac){ui.label("Username (Basic authentication; token/password below)");ui.text_edit_singleline(&mut server.username);}
                        if server.kind==TrackerKind::Trello{ui.label("Trello developer API key");ui.text_edit_singleline(&mut server.api_key);}
                        ui.add(egui::TextEdit::singleline(&mut state.secret).password(true).hint_text("API token/password (never stored in tasks.json)"));
                        if ui.add_enabled(state.job.is_none(),egui::Button::new("Save credential")).clicked(){save_secret=Some((server.secret_key(),std::mem::take(&mut state.secret)));}
                    }
                    if server.kind==TrackerKind::Trac{ui.label("Requires Trac XmlRpcPlugin with JSON-RPC enabled at login/jsonrpc.");}
                    if server.kind==TrackerKind::Pivotal{ui.label("Pivotal Tracker's hosted service retired in 2025. This adapter requires a reachable compatible API/archive deployment.");}
                    ui.collapsing("Generic REST mapping / custom worklog endpoint",|ui|{
                        if server.kind==TrackerKind::Generic{
                            for(label,field)in [("Search path",&mut server.generic.search_path),("Items JSON pointer",&mut server.generic.items_pointer),("ID pointer",&mut server.generic.id_pointer),("Summary pointer",&mut server.generic.title_pointer),("URL pointer",&mut server.generic.url_pointer),("Browser URL pattern",&mut server.generic.browser_pattern),("Auth header",&mut server.generic.auth_header),("Auth prefix",&mut server.generic.auth_prefix)]{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(field);}
                        }
                        ui.label("Optional worklog POST path (${id}, ${project})");ui.text_edit_singleline(&mut server.generic.worklog_path);
                        ui.label("JSON body (${id_json}, ${seconds}, ${description_json}, ${date})");ui.text_edit_multiline(&mut server.generic.worklog_json);
                    });
                    remove=ui.button("Remove server").clicked();
                }
                if remove{store.preferences.trackers.remove(state.selected);state.selected=state.selected.saturating_sub(1);state.secret.clear();}
                ui.separator();
                if let Some(task)=store.active_task_mut(){
                    ui.heading(format!("Work items — {}",task.summary));
                    ui.label("YouTrack and Asana round sent durations up to whole minutes. Timed-out writes are never retried automatically.");
                    if let Some(url)=&task.url{ui.hyperlink_to("Open issue in browser",url);}
                    ui.horizontal(|ui|{ui.add(egui::DragValue::new(&mut state.minutes).range(1..=525600).suffix(" min"));ui.text_edit_singleline(&mut state.description);if ui.button("Add work item").clicked(){store.next_work_item=store.next_work_item.saturating_add(1);let id=store.next_work_item;if let Some(task)=store.active_task_mut(){task.time_spent=task.time_spent.saturating_add(state.minutes.max(1)*60);task.work_items.push(WorkItem{id,started:rustpy_settings::tasks::now_secs().saturating_sub(state.minutes.max(1)*60),seconds:state.minutes.max(1)*60,description:state.description.clone(),..Default::default()});}}});
                }
                if let Some(task)=store.active_task(){
                    for item in task.work_items.iter().rev().take(100){ui.horizontal(|ui|{ui.label(format!("{} — {} {}",rustpy_settings::tasks::format_duration(item.seconds),item.description,if item.sent{"(sent)"}else{"(local)"}));if ui.add_enabled(!item.sent&&state.job.is_none()&&task.tracker.is_some(),egui::Button::new("Send time to tracker")).clicked(){send=Some((task.id.clone(),task.tracker.clone().unwrap_or_default(),task.remote_id.clone().unwrap_or_default(),item.clone()));}});}
                }
            }
            if !state.notice.is_empty(){ui.label(&state.notice);}
            if let Some(job)=&state.job{ui.label("Tracker operation in progress…");if ui.button("Cancel tracker operation").clicked(){job.cancel();}}
            });
        });
        self.task_workspace.visible = visible;
        self.task_workspace.root = Some(root.clone());
        if self.tasks.as_ref() != Some(&store) {
            self.tasks = Some(store);
            self.save_tasks();
        }
        if let Some((key, secret)) = save_secret {
            self.task_workspace.job = Some(
                TaskManager::global()
                    .task("Saving tracker credential")
                    .spawn(move |_| {
                        Ok(rustpy_secrets::set(&key, &secret)
                            .map(|_| Outcome::Credential)
                            .map_err(|e| e.to_string()))
                    }),
            );
        }
        if let Some((task, tracker, id, item)) = send {
            if self.project_safe_mode() {
                self.task_workspace.notice =
                    "Trust the project before sending task work items".into();
                return;
            }
            let server = self
                .tasks
                .as_ref()
                .and_then(|s| s.preferences.trackers.iter().find(|s| s.id == tracker))
                .cloned();
            if let Some(server) = server {
                let mut owner = WorklogOwner {
                    request: 0,
                    root: root.clone(),
                    session: self.file_session,
                    analysis: Arc::downgrade(&self.project.as_ref().unwrap().analysis),
                    tracker: server.clone(),
                    task: task.clone(),
                    remote: id.clone(),
                    item: item.clone(),
                };
                owner.item.sending = false;
                owner.item.sending_owner = None;
                let accounts = self.settings.github.clone();
                let job =
                    TaskManager::global()
                        .task("Sending task work item")
                        .spawn(move |progress| {
                            Ok(crate::task_trackers::send_time(
                                &server,
                                &id,
                                &item,
                                &accounts,
                                progress.token(),
                            )
                            .map(|receipt| Outcome::Sent { receipt }))
                        });
                owner.request = job.id();
                if let Some(work) = self
                    .tasks
                    .as_mut()
                    .and_then(|store| store.tasks.iter_mut().find(|task| task.id == owner.task))
                    .and_then(|task| {
                        task.work_items
                            .iter_mut()
                            .find(|item| item.id == owner.item.id)
                    })
                {
                    work.sending = true;
                    work.sending_owner = Some(owner.request);
                }
                self.task_workspace.inflight = Some(owner);
                self.task_workspace.job = Some(job);
            }
        }
        if let Some(id) = switch {
            self.flush_task_time();
            self.switch_task(&id, false, true);
        }
        if close {
            if commit {
                if let Some(task) = self.tasks.as_ref().and_then(|s| s.active_task()) {
                    self.git_panel.message = model::expand(
                        &self.tasks.as_ref().unwrap().preferences.commit_pattern,
                        task.issue_key.as_deref().unwrap_or(&task.id),
                        &task.summary,
                    );
                    self.task_workspace.pending_close =
                        Some((root, task.id.clone(), self.task_workspace.merge));
                    self.bottom = Some(BottomTool::Git);
                    self.commit_from_git_panel(false, ctx.input(|i| i.time));
                    self.task_workspace.pending_close = None;
                }
            } else {
                self.finish_task_close(self.task_workspace.merge);
            }
        }
    }
    fn worklog_owner_current(&self, owner: &WorklogOwner) -> bool {
        !self.project_safe_mode()
            && self.worklog_request_current(owner)
            && self.tasks.as_ref().is_some_and(|store| {
                store.preferences.trackers.contains(&owner.tracker)
                    && store
                        .tasks
                        .iter()
                        .find(|task| task.id == owner.task)
                        .is_some_and(|task| {
                            task.tracker.as_deref() == Some(&owner.tracker.id)
                                && task.remote_id.as_deref() == Some(&owner.remote)
                                && task
                                    .work_items
                                    .iter()
                                    .find(|item| item.id == owner.item.id)
                                    .is_some_and(|item| {
                                        let mut current = item.clone();
                                        current.sending = false;
                                        current.sending_owner = None;
                                        current == owner.item
                                    })
                        })
            })
    }
    fn worklog_request_current(&self, owner: &WorklogOwner) -> bool {
        self.file_session == owner.session
            && self.project.as_ref().is_some_and(|project| {
                project.root == owner.root
                    && std::sync::Weak::ptr_eq(&owner.analysis, &Arc::downgrade(&project.analysis))
            })
            && self.tasks.as_ref().is_some_and(|store| {
                store
                    .tasks
                    .iter()
                    .find(|task| task.id == owner.task)
                    .is_some_and(|task| {
                        task.tracker.as_deref() == Some(owner.tracker.id.as_str())
                            && task.remote_id.as_deref() == Some(owner.remote.as_str())
                            && task.work_items.iter().any(|item| {
                                item.id == owner.item.id
                                    && item.sending_owner == Some(owner.request)
                            })
                    })
            })
    }
    fn clear_worklog_sending(&mut self, owner: &WorklogOwner) {
        // Trust/tracker edits reject the receipt, but must still retire the
        // original request's spinner. Reused IDs never confer request ownership.
        if self.worklog_request_current(owner) {
            if let Some(work) = self
                .tasks
                .as_mut()
                .and_then(|store| store.tasks.iter_mut().find(|task| task.id == owner.task))
                .and_then(|task| {
                    task.work_items
                        .iter_mut()
                        .find(|item| item.id == owner.item.id)
                })
            {
                work.sending = false;
                work.sending_owner = None;
            }
        }
    }
}

#[cfg(test)]
mod acceptance_tests;
