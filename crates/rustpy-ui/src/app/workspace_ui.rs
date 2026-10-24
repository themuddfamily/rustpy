//! Checked workspace lifetime and narrow persistence of this window's changes.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_settings::{
    WorkspaceBreakpoints, WorkspaceLegacyRoots, WorkspacePatch, WorkspaceSession,
    WorkspaceSessionLease, WorkspaceSnapshot, WorkspaceStore,
};
use std::collections::{BTreeMap, HashMap};

pub(super) struct WorkspaceClient {
    store: WorkspaceStore,
    baseline: Option<WorkspaceSnapshot>,
    shared_baseline: Option<WorkspaceSnapshot>,
    full_baseline: Option<WorkspaceSnapshot>,
    reference: Workspace,
    pub(super) pending: WorkspacePatch,
    lease: Option<WorkspaceSessionLease>,
    pub(super) error: Option<String>,
    discarded: bool,
}
#[derive(Clone)]
pub(super) enum WorkspaceTransition {
    Switch(PathBuf),
    Close,
    Exit,
    NewWindow(PathBuf),
}
impl WorkspaceClient {
    pub(super) fn open(root: &Path) -> Self {
        let store = WorkspaceStore::new(root);
        let (baseline, reference, mut error) = match store.load() {
            Ok(snapshot) => {
                let ws = snapshot.workspace().clone();
                (Some(snapshot), ws, None)
            }
            Err(error) => (None, Workspace::default(), Some(error.to_string())),
        };
        let lease = if baseline.is_some() {
            match store.try_acquire_session() {
                Ok(lease) => lease,
                Err(e) => {
                    error = Some(e.to_string());
                    None
                }
            }
        } else {
            None
        };
        Self {
            store,
            shared_baseline: baseline.clone(),
            full_baseline: baseline.clone(),
            baseline,
            reference,
            pending: WorkspacePatch::default(),
            lease,
            error,
            discarded: false,
        }
    }
    pub(super) fn open_checked(root: &Path) -> Result<Self, String> {
        let client = Self::open(root);
        if let Some(error) = &client.error {
            return Err(error.clone());
        }
        if client.baseline.is_none() || client.lease.is_none() {
            return Err("Another window owns this project's workspace session".into());
        }
        client.validate_checked()?;
        Ok(client)
    }

    pub(super) fn validate_checked(&self) -> Result<(), String> {
        let baseline = self
            .baseline
            .as_ref()
            .ok_or("Missing prepared workspace baseline")?;
        let lease = self
            .lease
            .as_ref()
            .ok_or("Missing prepared workspace ownership")?;
        self.store
            .validate_session(lease)
            .map_err(|error| error.to_string())?;
        let current = self.store.load().map_err(|error| error.to_string())?;
        if current.revision() != baseline.revision() {
            return Err("Workspace changed after startup preparation".into());
        }
        Ok(())
    }

    pub(super) fn quit_clean_provisional(&self) -> bool {
        self.baseline.is_none()
            && self.pending.file_encodings.is_empty()
            && self.pending.interpreter.is_none()
            && self.pending.startup_tasks.is_empty()
            && self.pending.bookmarks.is_empty()
            && self.pending.bookmark_lists.is_none()
            && self.pending.breakpoints.is_none()
            && self.pending.watches.is_none()
            && self.pending.legacy_roots.is_none()
    }
    pub(super) fn quit_guard_state(&self) -> String {
        format!(
            "{:?}",
            (
                self.store.project(),
                &self.baseline,
                &self.shared_baseline,
                &self.full_baseline,
                &self.reference,
                &self.pending,
                &self.lease,
                self.lease.as_ref().map(|lease| lease as *const _),
                &self.error,
                self.discarded
            )
        )
    }
    #[cfg(test)]
    pub(super) fn session_lease_identity(&self) -> Option<*const WorkspaceSessionLease> {
        self.lease.as_ref().map(|lease| lease as *const _)
    }
    pub(super) fn recover_baseline(&mut self) -> Result<(), String> {
        if self.baseline.is_none() {
            let snapshot = self.store.load().map_err(|e| e.to_string())?;
            self.reference.file_encodings = snapshot.workspace().file_encodings.clone();
            self.shared_baseline = Some(snapshot.clone());
            self.full_baseline = Some(snapshot.clone());
            self.baseline = Some(snapshot);
            self.lease = self
                .store
                .try_acquire_session()
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub(super) fn effective(&self) -> Workspace {
        let mut ws = self
            .baseline
            .as_ref()
            .map(|s| s.workspace().clone())
            .unwrap_or_else(|| self.reference.clone());
        if let Some(interpreter) = &self.pending.interpreter {
            ws.interpreter = interpreter.clone();
        }
        if let Some(names) = &self.pending.bookmark_lists {
            ws.bookmark_lists = names.clone();
        }
        if let Some(watches) = &self.pending.watches {
            ws.watches = watches.clone();
        }
        for (name, selected) in &self.pending.startup_tasks {
            if *selected {
                if !ws.startup_tasks.contains(name) {
                    ws.startup_tasks.push(name.clone());
                }
            } else {
                ws.startup_tasks.retain(|item| item != name);
            }
        }
        ws
    }
    pub(super) fn commit(&mut self, desired: &Workspace, session: bool) -> Result<(), String> {
        if self.discarded {
            self.reference = desired.clone();
            return Ok(());
        }
        // Only differences from this window's own acknowledged UI state are edits.
        let before: BTreeMap<_, _> = self
            .reference
            .file_encodings
            .iter()
            .map(|e| (e.path.clone(), e.encoding.clone()))
            .collect();
        let after: BTreeMap<_, _> = desired
            .file_encodings
            .iter()
            .map(|e| (e.path.clone(), e.encoding.clone()))
            .collect();
        self.pending.file_encodings.clear();
        for key in before.keys().chain(after.keys()) {
            if before.get(key) != after.get(key) {
                self.pending
                    .file_encodings
                    .insert(key.clone(), after.get(key).cloned());
            }
        }
        if session {
            self.pending.bookmarks.clear();
            self.pending.bookmark_lists = None;
            self.pending.breakpoints = None;
            self.pending.watches = None;
            self.pending.legacy_roots = None;
            self.pending.session = None;
            if desired.bookmark_lists != self.reference.bookmark_lists {
                self.pending.bookmark_lists = Some(desired.bookmark_lists.clone());
            }
            let before: BTreeMap<_, _> = self
                .reference
                .bookmarks
                .iter()
                .map(|b| ((b.path.clone(), b.line), b.clone()))
                .collect();
            let after: BTreeMap<_, _> = desired
                .bookmarks
                .iter()
                .map(|b| ((b.path.clone(), b.line), b.clone()))
                .collect();
            for key in before.keys().chain(after.keys()) {
                if before.get(key) != after.get(key) {
                    self.pending
                        .bookmarks
                        .insert(key.clone(), after.get(key).cloned());
                }
            }
            if WorkspaceBreakpoints::from_workspace(desired)
                != WorkspaceBreakpoints::from_workspace(&self.reference)
            {
                self.pending.breakpoints = Some(WorkspaceBreakpoints::from_workspace(desired));
            }
            if desired.watches != self.reference.watches {
                self.pending.watches = Some(desired.watches.clone());
            }
            if path_set(&desired.source_roots) != path_set(&self.reference.source_roots)
                || path_set(&desired.excluded_dirs) != path_set(&self.reference.excluded_dirs)
            {
                self.pending.legacy_roots = Some(WorkspaceLegacyRoots {
                    source_roots: desired.source_roots.clone(),
                    excluded_dirs: desired.excluded_dirs.clone(),
                });
            }
            if self.lease.is_some()
                && WorkspaceSession::from_workspace(desired)
                    != WorkspaceSession::from_workspace(&self.reference)
            {
                self.pending.session = Some(WorkspaceSession::from_workspace(desired));
            }
        }
        if self.baseline.is_none() {
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "Workspace has no writable baseline".into()));
        }
        let shared = WorkspacePatch {
            file_encodings: self.pending.file_encodings.clone(),
            interpreter: self.pending.interpreter.clone(),
            startup_tasks: self.pending.startup_tasks.clone(),
            ..Default::default()
        };
        if !shared.file_encodings.is_empty()
            || shared.interpreter.is_some()
            || !shared.startup_tasks.is_empty()
        {
            let snapshot = self
                .store
                .commit(self.shared_baseline.as_ref().unwrap(), &shared, None)
                .map_err(|e| e.to_string())?;
            self.shared_baseline = Some(snapshot.clone());
            self.baseline = Some(snapshot);
            self.reference.file_encodings = desired.file_encodings.clone();
            self.pending.file_encodings.clear();
            self.pending.interpreter = None;
            self.pending.startup_tasks.clear();
        }
        if session {
            let full = WorkspacePatch {
                bookmarks: self.pending.bookmarks.clone(),
                bookmark_lists: self.pending.bookmark_lists.clone(),
                breakpoints: self.pending.breakpoints.clone(),
                watches: self.pending.watches.clone(),
                legacy_roots: self.pending.legacy_roots.clone(),
                session: self.pending.session.clone(),
                ..Default::default()
            };
            if full.bookmark_lists.is_some()
                || !full.bookmarks.is_empty()
                || full.breakpoints.is_some()
                || full.watches.is_some()
                || full.legacy_roots.is_some()
                || full.session.is_some()
            {
                let snapshot = self
                    .store
                    .commit(
                        self.full_baseline.as_ref().unwrap(),
                        &full,
                        self.lease.as_ref(),
                    )
                    .map_err(|e| e.to_string())?;
                self.full_baseline = Some(snapshot.clone());
                self.baseline = Some(snapshot);
            }
            self.reference = desired.clone();
            self.pending.bookmarks.clear();
            self.pending.bookmark_lists = None;
            self.pending.breakpoints = None;
            self.pending.watches = None;
            self.pending.legacy_roots = None;
            self.pending.session = None;
        }
        self.error = None;
        Ok(())
    }
}
fn path_set(paths: &[PathBuf]) -> std::collections::BTreeSet<&PathBuf> {
    paths.iter().collect()
}
impl IdeApp {
    pub(super) fn recover_workspace_baseline(&mut self) -> Result<(), String> {
        let Some(client) = &mut self.workspace_client else {
            return Ok(());
        };
        let provisional = client.baseline.is_none();
        client.recover_baseline()?;
        if provisional {
            let mut recovered: HashMap<PathBuf, String> = client
                .reference
                .file_encodings
                .iter()
                .map(|record| (record.path.clone(), record.encoding.clone()))
                .collect();
            for (path, value) in &client.pending.file_encodings {
                if let Some(value) = value {
                    recovered.insert(path.clone(), value.clone());
                } else {
                    recovered.remove(path);
                }
            }
            if self.encoding_overrides != recovered {
                self.encoding_overrides = recovered;
                self.encoding_generation = self.encoding_generation.wrapping_add(1);
            }
        }
        Ok(())
    }
    pub(super) fn begin_workspace(&mut self, root: &Path) -> Workspace {
        self.install_workspace_client(WorkspaceClient::open(root))
    }
    pub(super) fn install_workspace_client(&mut self, mut client: WorkspaceClient) -> Workspace {
        let ws = client.effective();
        crate::breakpoints::BreakpointStore::from_workspace(&ws).write_to(&mut client.reference);
        self.workspace_client = Some(client);
        ws
    }
    pub(super) fn acknowledge_loaded_workspace_roots(&mut self) {
        if let Some(project) = &self.project {
            if rustpy_project::ProjectModel::exists(&project.root) {
                if let Some(client) = &mut self.workspace_client {
                    client.reference.source_roots.clear();
                    client.reference.excluded_dirs.clear();
                }
            }
        }
    }
    pub(super) fn effective_workspace(&self) -> Workspace {
        self.workspace_client
            .as_ref()
            .map(WorkspaceClient::effective)
            .unwrap_or_default()
    }
    pub(super) fn report_workspace_error(&mut self) {
        if let Some(error) = self.workspace_client.as_ref().and_then(|c| c.error.clone()) {
            self.notify_error("Project", "Workspace persistence failed", error);
        }
    }
    pub(super) fn commit_workspace(&mut self, desired: Workspace, session: bool) -> bool {
        let Some(client) = &mut self.workspace_client else {
            return true;
        };
        match client.commit(&desired, session) {
            Ok(()) => true,
            Err(error) => {
                client.error = Some(error);
                self.report_workspace_error();
                false
            }
        }
    }
    pub(super) fn record_interpreter_choice(&mut self, path: &Path) {
        if let Some(client) = &mut self.workspace_client {
            let acknowledged = client
                .shared_baseline
                .as_ref()
                .and_then(|s| s.workspace().interpreter.as_deref());
            client.pending.interpreter = if acknowledged == Some(path) {
                None
            } else {
                Some(Some(path.to_path_buf()))
            };
        }
        self.persist_encoding_overrides();
    }
    pub(super) fn set_startup_task(&mut self, name: &str, selected: bool) {
        if let Some(client) = &mut self.workspace_client {
            let acknowledged = client
                .shared_baseline
                .as_ref()
                .map(|snapshot| snapshot.workspace())
                .unwrap_or(&client.reference)
                .startup_tasks
                .iter()
                .any(|task| task == name);
            if acknowledged == selected {
                client.pending.startup_tasks.remove(name);
            } else {
                client
                    .pending
                    .startup_tasks
                    .insert(name.to_owned(), selected);
            }
        }
        self.persist_encoding_overrides();
    }
    pub(super) fn workspace_transition(&mut self, transition: WorkspaceTransition) -> bool {
        let saved = self.save_workspace();
        let provisional = self.workspace_client.as_ref().is_some_and(|c| {
            c.baseline.is_none()
                && c.pending.file_encodings.is_empty()
                && c.pending.interpreter.is_none()
                && c.pending.startup_tasks.is_empty()
                && c.pending.bookmarks.is_empty()
                && c.pending.bookmark_lists.is_none()
                && c.pending.breakpoints.is_none()
                && c.pending.watches.is_none()
                && c.pending.legacy_roots.is_none()
        });
        if !self.setup_workspace_choice_pending() && (saved || provisional) {
            return true;
        }
        self.dialog = Dialog::WorkspacePersistence(transition);
        false
    }
    pub(super) fn workspace_persistence_dialog(
        &mut self,
        ctx: &egui::Context,
        transition: WorkspaceTransition,
    ) {
        let error = self
            .workspace_client
            .as_ref()
            .and_then(|c| c.error.clone())
            .or_else(|| self.setup_workspace_choice_error())
            .unwrap_or_default();
        let mut answer = None;
        let mut cancelled = false;
        egui::Window::new("Workspace changes were not saved").collapsible(false).resizable(false).show_modal(ctx, |ui| {
            ui.label(error);
            ui.label("Retry saving, discard this window's workspace changes, or keep the project open.");
            ui.horizontal(|ui| {
                if ui.button("Retry").clicked() { answer = Some(false); }
                if ui.button("Discard Workspace Changes").clicked() { answer = Some(true); }
            });
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) { cancelled = true; }
        });
        if cancelled {
            self.focus_editor = true;
            return;
        }
        if let Some(discard) = answer {
            self.answer_workspace_persistence(transition, discard);
        } else {
            self.dialog = Dialog::WorkspacePersistence(transition);
        }
    }
    pub(super) fn answer_workspace_persistence(
        &mut self,
        transition: WorkspaceTransition,
        discard: bool,
    ) {
        if discard {
            if let Some(client) = &mut self.workspace_client {
                client.discarded = true;
                client.pending = WorkspacePatch::default();
            }
        }
        if discard {
            self.discard_setup_workspace_choice();
        }
        if !discard {
            if let Err(error) = self.recover_workspace_baseline() {
                if let Some(client) = &mut self.workspace_client {
                    client.error = Some(error);
                }
                self.report_workspace_error();
                self.dialog = Dialog::WorkspacePersistence(transition);
                return;
            }
        }
        if discard {
            self.save_workspace();
        }
        if discard || (self.retry_setup_workspace_choice() && self.save_workspace()) {
            let previous = self.actual_current_project_root().map(Path::to_path_buf);
            match transition {
                WorkspaceTransition::Switch(root) => self.open_project(&root),
                WorkspaceTransition::Close => self.close_project(),
                WorkspaceTransition::Exit => self.continue_exit(),
                WorkspaceTransition::NewWindow(root) => self.open_in_new_window(&root),
            }
            if discard
                && !self.exit_requested
                && self.actual_current_project_root() == previous.as_deref()
            {
                if let Some(client) = &mut self.workspace_client {
                    client.discarded = false;
                    let snapshot = client.store.load().ok();
                    client.baseline = snapshot.clone();
                    client.shared_baseline = snapshot.clone();
                    client.full_baseline = snapshot;
                }
            }
        } else {
            self.dialog = Dialog::WorkspacePersistence(transition);
        }
    }
}

#[cfg(test)]
mod bookmark_list_persistence_tests {
    use super::*;
    #[test]
    fn successful_empty_group_commit_clears_pending_and_conflict_keeps_intent() {
        let root = tempfile::tempdir().unwrap();
        let mut client = WorkspaceClient::open(root.path());
        let mut desired = client.reference.clone();
        desired.bookmark_lists = vec!["Default".into(), "Empty".into()];
        client.commit(&desired, true).unwrap();
        assert!(client.pending.bookmark_lists.is_none());
        assert_eq!(
            Workspace::load(root.path()).bookmark_lists,
            desired.bookmark_lists
        );
        let other = WorkspaceStore::new(root.path());
        let baseline = other.load().unwrap();
        other
            .commit(
                &baseline,
                &WorkspacePatch {
                    bookmark_lists: Some(vec!["Default".into(), "Other".into()]),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        desired.bookmark_lists = vec!["Default".into(), "My retained change".into()];
        assert!(client.commit(&desired, true).is_err());
        assert_eq!(
            client.pending.bookmark_lists.as_ref(),
            Some(&desired.bookmark_lists)
        );
        assert_eq!(client.effective().bookmark_lists, desired.bookmark_lists);
        assert_eq!(client.reference.bookmark_lists, ["Default", "Empty"]);
        assert_eq!(
            Workspace::load(root.path()).bookmark_lists,
            ["Default", "Other"]
        );
    }
}
