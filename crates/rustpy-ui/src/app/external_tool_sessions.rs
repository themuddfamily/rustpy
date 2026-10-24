//! Per-launch ownership for output filters, rerun and one post-execution VFS synchronization.
use super::*;
use crate::external_tool_workspace::{self as plans, PlanError};
use crate::external_tools::{InvocationError, MacroContext};
#[derive(Default)]
pub(super) struct Workspace {
    sessions: std::collections::HashMap<u64, Launch>,
}
struct Launch {
    root: PathBuf,
    project: std::sync::Weak<Analysis>,
    tool: rustpy_settings::ExternalTool,
    file: Option<PathBuf>,
    synchronized: bool,
    ssh_auth: Option<u64>,
}
#[derive(Clone)]
pub(super) struct Owner {
    root: Option<PathBuf>,
    project_session: u64,
    document: Option<(rustpy_text::DocumentId, u64)>,
}
impl IdeApp {
    pub(super) fn tool_owner(&self, file: Option<&Path>) -> Owner {
        Owner {
            root: self.project.as_ref().map(|p| p.root.clone()),
            project_session: self.file_session,
            document: self
                .tabs
                .iter()
                .find(|tab| tab.state.path.as_deref() == file)
                .map(|tab| (tab.state.doc.identity(), tab.state.doc.version())),
        }
    }
    pub(super) fn tool_owner_current(&self, owner: &Owner) -> bool {
        owner.root == self.project.as_ref().map(|p| p.root.clone())
            && owner.project_session == self.file_session
            && owner.document.is_none_or(|(id, version)| {
                self.tabs
                    .iter()
                    .any(|tab| tab.state.doc.identity() == id && tab.state.doc.version() == version)
            })
    }
    pub(super) fn run_tool_context(
        &mut self,
        tool: rustpy_settings::ExternalTool,
        context: MacroContext,
        owner: Owner,
    ) {
        let project_safe_mode = self.project_safe_mode();
        if !self.tool_owner_current(&owner) {
            self.set_status("External tool context changed; invoke the tool again");
            return;
        }
        match plans::plan(&tool, &context) {
            Err(PlanError::Invocation(InvocationError::NeedsPrompt)) => {
                self.dialog = Dialog::ToolPrompt(Box::new(super::tools_ui::ToolPromptState {
                    tool,
                    value: String::new(),
                    context,
                    owner,
                }));
            }
            Err(error) => self.set_status(format!("Cannot run external tool: {error}")),
            Ok(mut plan) => {
                if project_safe_mode {
                    self.set_status("Trust the project before running external tools");
                    return;
                }
                let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
                    return;
                };
                self.poll_external_tool_sessions();
                if !tool.options.allow_parallel
                    && self
                        .external_tool_sessions
                        .sessions
                        .iter()
                        .any(|(id, launch)| {
                            launch.root == root
                                && launch.tool.name == tool.name
                                && self
                                    .run_panel
                                    .sessions
                                    .iter()
                                    .any(|session| session.id == *id && session.is_running())
                        })
                {
                    self.set_status(format!("External tool '{}' is already running", tool.name));
                    return;
                }
                let ssh_auth = if tool.options.ssh.is_some() {
                    let (auth_id, environment) = self.git_panel.git_jobs.new_job();
                    match plans::plan_with_ssh_confirmation(&tool, &context, &environment.vars) {
                        Ok(authorized) => {
                            plan = authorized;
                            Some(auth_id)
                        }
                        Err(error) => {
                            self.git_panel.git_jobs.job_finished(
                                auth_id,
                                false,
                                "SSH confirmation unavailable",
                            );
                            self.set_status(error.to_string());
                            return;
                        }
                    }
                } else {
                    None
                };
                let id = self.next_session_id;
                self.next_session_id += 1;
                let session = RunSession::spawn(id, &plan.config, None, &root);
                let failed = matches!(session.status, rustpy_run::SessionStatus::Failed(_));
                let previous = self.run_panel.active;
                self.run_panel.add_session(session);
                self.run_panel.set_external_tool_links(id, plan.links);
                if !tool.show_console {
                    self.run_panel.background(id);
                    self.run_panel.active = previous.filter(|id| {
                        self.run_panel
                            .sessions
                            .iter()
                            .any(|session| session.id == *id)
                    });
                }
                self.external_tool_sessions.sessions.insert(
                    id,
                    Launch {
                        root,
                        project: std::sync::Arc::downgrade(
                            &self.project.as_ref().unwrap().analysis,
                        ),
                        tool: tool.clone(),
                        file: context.file,
                        synchronized: false,
                        ssh_auth,
                    },
                );
                if tool.show_console {
                    self.bottom = Some(BottomTool::Run);
                } else if failed {
                    self.set_status(format!(
                        "External tool '{}' could not start; see Run output",
                        tool.name
                    ));
                }
            }
        }
    }
    pub(super) fn rerun_external_tool(&mut self, id: u64) -> bool {
        let Some(launch) = self.external_tool_sessions.sessions.get(&id) else {
            return false;
        };
        if !self.project.as_ref().is_some_and(|p| {
            p.root == launch.root
                && std::sync::Weak::ptr_eq(&launch.project, &std::sync::Arc::downgrade(&p.analysis))
        }) {
            self.set_status("External tool belongs to a previous project context");
            return true;
        }
        let (tool, file) = (launch.tool.clone(), launch.file.clone());
        self.run_panel.replace_running(id);
        self.run_tool(tool, file, None);
        true
    }
    pub(super) fn poll_external_tool_sessions(&mut self) {
        let mut refresh = false;
        for (id, launch) in &mut self.external_tool_sessions.sessions {
            if launch.synchronized {
                continue;
            }
            let running = self
                .run_panel
                .sessions
                .iter()
                .any(|session| session.id == *id && session.is_running());
            if !running {
                if let Some(auth_id) = launch.ssh_auth.take() {
                    let success = self
                        .run_panel
                        .sessions
                        .iter()
                        .find(|session| session.id == *id)
                        .is_some_and(|session| {
                            matches!(session.status, rustpy_run::SessionStatus::Exited(0))
                        });
                    self.git_panel.git_jobs.job_finished(
                        auth_id,
                        success,
                        "SSH external tool finished",
                    );
                }
                launch.synchronized = true;
                if launch.tool.options.synchronize
                    && self.project.as_ref().is_some_and(|p| {
                        p.root == launch.root
                            && std::sync::Weak::ptr_eq(
                                &launch.project,
                                &std::sync::Arc::downgrade(&p.analysis),
                            )
                    })
                {
                    refresh = true;
                }
            }
        }
        if refresh {
            self.reload_changed_files();
        }
        let live = self
            .run_panel
            .sessions
            .iter()
            .map(|session| session.id)
            .collect::<std::collections::HashSet<_>>();
        self.external_tool_sessions
            .sessions
            .retain(|id, launch| live.contains(id) || !launch.synchronized);
    }
}
