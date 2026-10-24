//! Captured Run/Debug/Coverage/Profile and configuration edits from source menus.
use super::*;
use crate::run_context::{self, ContextAction, EditorRequest, PathRequest, ProjectOwner};
impl IdeApp {
    pub(super) fn context_run_owner(&self) -> ProjectOwner {
        ProjectOwner {
            root: self
                .project
                .as_ref()
                .map(|p| p.root.clone())
                .unwrap_or_default(),
            session: self.file_session,
        }
    }
    pub(super) fn bind_context_run_owner(&self, ctx: &egui::Context, index: usize) {
        if let Some(tab) = self.tabs.get(index) {
            ctx.data_mut(|data| {
                data.insert_temp(
                    run_context::owner_key(tab.view.id),
                    self.context_run_owner(),
                )
            });
        }
    }
    pub(super) fn editor_context_request(&mut self, index: usize, request: EditorRequest) {
        let valid = self.tabs.get(index).is_some_and(|tab| {
            request.is_current(
                &self.context_run_owner(),
                tab.state.doc.identity(),
                tab.state.doc.version(),
                tab.state.path.as_deref(),
            )
        });
        if !valid {
            self.set_status("Run context changed; reopen its menu");
            return;
        }
        self.dispatch_context_configuration(request.configuration, request.action);
    }
    pub(super) fn project_context_request(&mut self, request: PathRequest) {
        if !request.is_current(&self.context_run_owner()) {
            self.set_status("Run target changed; reopen its menu");
            return;
        }
        // An open editor is the authoritative source for a Project-view file.
        // Its unsaved test/symbol changes must not launch a stale disk target.
        let configuration = self
            .tabs
            .iter()
            .find(|tab| tab.state.path.as_ref() == Some(&request.path))
            .map(|tab| {
                run_context::file_config(&request.owner.root, &request.path, &tab.state.doc.text())
            })
            .unwrap_or(request.configuration);
        self.dispatch_context_configuration(configuration, request.action);
    }
    pub(super) fn dispatch_context_configuration(
        &mut self,
        configuration: RunConfig,
        action: ContextAction,
    ) {
        if let ContextAction::Launch(mode) = action {
            self.launch_context(configuration, mode);
            return;
        }
        let Some(project) = &mut self.project else {
            self.set_status("Open a project to create a run configuration");
            return;
        };
        let configuration = run_context::configuration_for_edit(
            &project.run_configs,
            configuration,
            &project.root,
            action == ContextAction::Create,
        );
        let mut updated = project.run_configs.clone();
        updated.upsert(configuration);
        if let Err(error) = updated.save(&project.root) {
            self.set_status(format!("Cannot save run configuration: {error}"));
            return;
        }
        project.run_configs = updated;
        self.dialog = Dialog::RunConfigurations;
    }
    pub(super) fn focused_context_action(&mut self, ctx: &egui::Context, action: ContextAction) {
        if ProjectTree::has_focus(ctx) {
            let Some(path) = self
                .project
                .as_ref()
                .and_then(|project| project.tree.selected.clone())
            else {
                return;
            };
            if !path.is_dir()
                && !path
                    .extension()
                    .is_some_and(|extension| extension == "py" || extension == "pyw")
            {
                self.set_status("Select a Python file or test directory");
                return;
            }
            let Some(stamp) = run_context::PathStamp::read(&path) else {
                self.set_status("The selected run target is unavailable");
                return;
            };
            let owner = self.context_run_owner();
            let configuration = run_context::config_for_path(&owner.root, &path);
            self.project_context_request(PathRequest {
                owner,
                path,
                stamp,
                configuration,
                action,
            });
            return;
        }
        self.editor_context_action(action);
    }
    pub(super) fn editor_context_action(&mut self, action: ContextAction) {
        let Some(index) = self.active else {
            return;
        };
        let Some(configuration) = self.context_cfg(true) else {
            self.set_status("Open a Python file to run its context");
            return;
        };
        let tab = &self.tabs[index];
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        let request = EditorRequest {
            owner: self.context_run_owner(),
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            path,
            configuration,
            action,
        };
        self.editor_context_request(index, request);
    }
}
