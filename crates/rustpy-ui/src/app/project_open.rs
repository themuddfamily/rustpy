//! Opening projects safely: trust is IDE-level only (Trust Project, Trusted
//! Locations), File › Open / Recent Projects / Get from VCS / a dropped folder
//! ask "This Window / New Window" while a project is open, and unsaved editors
//! are saved before the project changes (edits that cannot be saved are only
//! dropped after an explicit "Discard").

use super::*;
use crate::a11y::DialogWindow;
use rustpy_settings::OpenProjectIn;

/// A question asked before a project opens.
pub(super) struct OpenProjectState {
    pub root: PathBuf,
    /// Modified editors that could not be saved: asks whether to discard them.
    pub unsaved: Vec<String>,
    /// "Remember my decision" for the This Window / New Window question.
    pub remember: bool,
}

/// What a request to open `target` does.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum OpenDecision {
    /// No project is open, or the user chose this window.
    OpenHere,
    AlreadyOpen,
    NewWindow,
    Ask,
}

pub(super) fn decide_open(
    current: Option<&Path>,
    target: &Path,
    pref: OpenProjectIn,
) -> OpenDecision {
    match current {
        None => OpenDecision::OpenHere,
        Some(c) if c == target => OpenDecision::AlreadyOpen,
        Some(_) => match pref {
            OpenProjectIn::Ask => OpenDecision::Ask,
            OpenProjectIn::CurrentWindow => OpenDecision::OpenHere,
            OpenProjectIn::NewWindow => OpenDecision::NewWindow,
        },
    }
}

/// Starts another RustPy process on `root` (settings are merged safely between
/// processes, see `rustpy_settings::persist`). `--new-window` keeps it from
/// handing the folder back to this IDE (the single-instance launcher).
pub(super) fn spawn_new_window(root: &Path) -> std::io::Result<()> {
    if let Some(result) = crate::launch::native_project_windows::open_project(root) {
        return result.map_err(std::io::Error::other);
    }
    let exe = std::env::current_exe()?;
    let mut child = rustpy_cli::instance::spawn_detached(
        &exe,
        &["--new-window".into(), root.display().to_string()],
        &std::env::current_dir()?,
    )?;
    // Reap the child when it exits so it does not linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// What a trust probe of the open project was decided from.
type TrustProbeKey = (
    PathBuf,
    Option<rustpy_settings::trust::DirectoryIdentity>,
    Vec<PathBuf>,
    Vec<PathBuf>,
    bool,
);

/// The last filesystem trust probe of the open project. `project_safe_mode`
/// runs at many admissions per frame; the probe (canonicalize and stat of the
/// root and every trusted path) is repeated only when the trust settings or the
/// opened identity change, or after `TRUST_PROBE_TTL` (a directory replaced on
/// disk is noticed within that time).
#[derive(Default)]
pub(super) struct TrustProbe(std::sync::Mutex<Option<(TrustProbeKey, std::time::Instant, bool)>>);

const TRUST_PROBE_TTL: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(test)]
thread_local! {
    /// How often this thread probed the filesystem for trust (tests).
    pub(super) static TRUST_PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
impl TrustProbe {
    /// Forgets the last probe: the next admission rechecks the disk.
    pub(super) fn reset(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

impl IdeApp {
    /// Recheck at each execution admission; a cached UI flag is not authority.
    /// The filesystem probe behind it is shared by the admissions of a frame
    /// (see `TrustProbe`); any change of the trust settings reprobes at once.
    pub(super) fn project_safe_mode(&self) -> bool {
        if self.safe_mode {
            return true;
        }
        let Some(project) = self.project.as_ref() else {
            return false;
        };
        let trust_all = rustpy_settings::trust_all_from_env();
        let mut probe = self.trust_probe.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((key, at, unsafe_)) = probe.as_ref() {
            if at.elapsed() < TRUST_PROBE_TTL
                && key.0 == project.root
                && key.1 == self.project_trust_identity
                && key.2 == self.settings.trusted_projects
                && key.3 == self.settings.trusted_locations
                && key.4 == trust_all
            {
                return *unsafe_;
            }
        }
        #[cfg(test)]
        TRUST_PROBES.with(|probes| probes.set(probes.get() + 1));
        let unsafe_ = self
            .project_trust_identity
            .as_ref()
            .is_none_or(|identity| !identity.matches(&project.root))
            || !(trust_all || self.settings.is_project_trusted(&project.root));
        *probe = Some((
            (
                project.root.clone(),
                self.project_trust_identity.clone(),
                self.settings.trusted_projects.clone(),
                self.settings.trusted_locations.clone(),
                trust_all,
            ),
            std::time::Instant::now(),
            unsafe_,
        ));
        unsafe_
    }

    /// Earliest ordinary-frame hook. Never upgrades trust implicitly. Root's
    /// prepared-quit guard must return before calling this publication hook.
    pub(super) fn refresh_project_trust(&mut self) {
        if !self.safe_mode && self.project_safe_mode() {
            self.safe_mode = true;
            self.terminal_panel.set_auto_shell(false);
            self.stop_file_watchers();
            self.cancel_run_workflows();
            self.cancel_native_skeletons();
            self.cancel_interpreter_management();
            self.http_panel.stop_all();
            self.py_console.stop_quit_process();
            self.packages_panel.stop_quit_process();
            self.test_panel.stop();
            self.git_panel.git_jobs.cancel();
            self.stop_other_vcs_operations();
            for tab in &mut self.tabs {
                if let Some(notebook) = tab.notebook.as_mut() {
                    notebook.stop_quit_process();
                }
            }
            for index in (0..self.terminal_panel.tab_count()).rev() {
                self.terminal_panel.close(index);
            }
            let runs = self
                .run_panel
                .sessions
                .iter()
                .filter(|run| run.is_running())
                .map(|run| run.id)
                .collect::<Vec<_>>();
            for id in runs {
                self.run_panel.request_stop(id);
            }
            if let Some(debug) = self.debug.as_mut() {
                debug.stop();
            }
            for (debug, _) in &mut self.debug_sessions {
                debug.stop();
            }
            self.deploy.cancel_all();
            self.db_panel.stop_all_database();
            self.cancel_docker_services_project();
            self.stop_web_languages();
            self.cancel_native_ui();
            self.cancel_plugin_runtime();
            self.cancel_remote_development_project();
            self.stop_collaboration();
            self.stop_ai_workers();
            self.cancel_ai_inline();
            self.cancel_data_files_project();
            if let Some(project) = self.project.as_mut() {
                project.interpreter_rx = None;
            }
            self.set_status(
                "Project trust changed; execution is disabled until you trust this root again",
            );
        }
    }

    /// File › Open, Recent Projects, Get from VCS and dropped folders: opens `root`
    /// here, in a new window, or asks, per Settings › System Settings.
    pub(super) fn request_open_project(&mut self, root: &Path) {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let current = self.project.as_ref().map(|p| p.root.clone());
        match decide_open(current.as_deref(), &root, self.settings.open_project_in()) {
            OpenDecision::OpenHere => self.open_project(&root),
            OpenDecision::AlreadyOpen => {
                self.set_status(format!("{} is already open", root.display()))
            }
            OpenDecision::NewWindow => self.open_in_new_window(&root),
            OpenDecision::Ask => {
                self.dialog = Dialog::OpenProject(Box::new(OpenProjectState {
                    root,
                    unsaved: Vec::new(),
                    remember: false,
                }))
            }
        }
    }

    pub(super) fn open_in_new_window(&mut self, root: &Path) {
        // The new process reads settings.json: write this window's state first.
        let _ = self.settings.save();
        if !self.workspace_transition(workspace_ui::WorkspaceTransition::NewWindow(
            root.to_path_buf(),
        )) {
            return;
        }
        match spawn_new_window(root) {
            Ok(()) => self.set_status(format!("Opening {} in a new window", root.display())),
            Err(e) => self.set_status(format!("Cannot start a new window: {e}")),
        }
    }

    /// Saves modified editors before the project changes (or closes). Returns false, and asks whether to discard them, when some could not
    /// be saved (untitled scratch editors, write errors).
    pub(super) fn save_before_switching(&mut self, root: &Path) -> bool {
        if !self.tabs.iter().any(|t| t.has_unsaved_changes()) {
            return true;
        }
        self.save_all();
        let unsaved: Vec<String> = self
            .tabs
            .iter()
            .filter(|t| t.has_unsaved_changes())
            .map(|t| {
                t.state
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| t.state.title())
            })
            .collect();
        if unsaved.is_empty() {
            return true;
        }
        self.dialog = Dialog::OpenProject(Box::new(OpenProjectState {
            root: root.to_path_buf(),
            unsaved,
            remember: false,
        }));
        false
    }

    pub(super) fn open_project_dialog(
        &mut self,
        ctx: &egui::Context,
        mut st: Box<OpenProjectState>,
    ) {
        #[derive(PartialEq)]
        enum Choice {
            ThisWindow,
            NewWindow,
            Discard,
            Cancel,
        }
        let theme = self.theme.clone();
        let mut open = true;
        let mut choice: Option<Choice> = None;
        let name = st
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| st.root.display().to_string());
        let title = if st.unsaved.is_empty() {
            "Open Project"
        } else {
            "Unsaved Changes"
        };
        egui::Window::new(title).collapsible(false).resizable(false).default_width(500.0).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            if st.unsaved.is_empty() {
                ui.label(format!("Open '{name}' in this window or in a new window?"));
                ui.label(egui::RichText::new(st.root.display().to_string()).color(theme.text_dim).small());
                ui.add_space(4.0);
                ui.checkbox(&mut st.remember, "Remember my decision (Settings › Appearance & Behavior › Appearance)");
                ui.horizontal(|ui| {
                    if ui.button("This Window").clicked() {
                        choice = Some(Choice::ThisWindow);
                    }
                    if ui.button("New Window").clicked() {
                        choice = Some(Choice::NewWindow);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(Choice::Cancel);
                    }
                });
            } else {
                ui.label(format!("These files have changes that could not be saved. Discard the changes and open '{name}'?"));
                for f in &st.unsaved {
                    ui.label(egui::RichText::new(format!("• {f}")).color(theme.warning));
                }
                ui.horizontal(|ui| {
                    if ui.button("Discard Changes and Open").clicked() {
                        choice = Some(Choice::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(Choice::Cancel);
                    }
                });
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                choice = Some(Choice::Cancel);
            }
        });
        if st.remember && matches!(choice, Some(Choice::ThisWindow | Choice::NewWindow)) {
            self.settings
                .set_open_project_in(if choice == Some(Choice::ThisWindow) {
                    OpenProjectIn::CurrentWindow
                } else {
                    OpenProjectIn::NewWindow
                });
            let _ = self.settings.save();
        }
        match choice {
            Some(Choice::ThisWindow) => self.open_project(&st.root),
            Some(Choice::NewWindow) => self.open_in_new_window(&st.root),
            Some(Choice::Discard) => {
                // Only the editors the user saw listed are dropped; open_project closes the rest.
                self.tabs.retain(|t| !t.has_unsaved_changes());
                self.active = None;
                self.open_project(&st.root);
            }
            Some(Choice::Cancel) => self.focus_editor = true,
            None if open => self.dialog = Dialog::OpenProject(st),
            None => self.focus_editor = true,
        }
    }

    /// File › Trust Project.
    pub(super) fn trust_current_project(&mut self) {
        if let Some(root) = self.project.as_ref().map(|p| p.root.clone()) {
            self.trust_project_at(&root);
        }
    }

    /// Records `root` as trusted in the IDE settings. For the open project this
    /// leaves safe mode and rediscovers interpreters, now including its own venvs.
    pub(super) fn trust_project_at(&mut self, root: &Path) {
        let Ok(canonical) = rustpy_settings::trust::canonical_directory(root) else {
            self.set_status("Cannot trust a missing or non-directory project");
            return;
        };
        if canonical != root
            || self.project.as_ref().is_some_and(|project| {
                project.root == root
                    && self
                        .project_trust_identity
                        .as_ref()
                        .is_none_or(|identity| !identity.matches(root))
            })
        {
            self.set_status("Project root changed; reopen it before granting trust");
            return;
        }
        if self.settings.trust_project(root) {
            let _ = self.settings.save();
        }
        if !self.project.as_ref().is_some_and(|p| p.root == root) {
            return;
        }
        let was_safe = self.safe_mode;
        self.safe_mode = false;
        // A Terminal held back in safe mode may start its shell now.
        self.terminal_panel.set_auto_shell(true);
        if was_safe {
            self.rediscover_interpreters();
        }
        self.save_workspace();
        self.set_status("Project trusted");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_open_blocks_console_and_typed_launch_before_spawning() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(root.clone()));
        assert!(app.project_safe_mode());
        assert!(!app.ensure_py_console());
        let config = RunConfig {
            name: "must remain preview only".into(),
            kind: RunConfigKind::Program {
                program: root.join("must-not-execute"),
                args: vec![],
            },
            ..Default::default()
        };
        assert!(app.launch_configuration_type(&config, crate::run_context::RunMode::Run));
        assert!(app.run_panel.sessions.is_empty());
        assert!(!app.py_console.any_running());
    }

    #[test]
    fn revocation_is_effective_before_the_next_frame_refresh() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(root.clone()));
        app.settings.trust_project(&root);
        app.safe_mode = false;
        assert!(!app.project_safe_mode());
        app.settings.trusted_projects.clear();
        assert!(
            app.project_safe_mode(),
            "cached safe_mode=false cannot authorize execution"
        );
        assert!(!app.ensure_py_console());
        app.refresh_project_trust();
        assert!(app.safe_mode);
        assert!(!app.terminal_panel.auto_shell());
    }

    #[test]
    fn trust_admissions_share_one_probe_until_trust_settings_change() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(root.clone()));
        app.settings.trust_project(&root);
        app.safe_mode = false;
        let probes = || TRUST_PROBES.with(|probes| probes.get());
        let before = probes();
        for _ in 0..20 {
            assert!(!app.project_safe_mode());
        }
        assert_eq!(
            probes() - before,
            1,
            "admissions of a frame share one probe"
        );
        app.settings.trusted_projects.clear();
        assert!(app.project_safe_mode());
        assert_eq!(probes() - before, 2, "a trust change reprobes at once");
        app.trust_probe.reset();
        assert!(app.project_safe_mode());
        assert_eq!(probes() - before, 3);
    }

    #[test]
    fn switching_prompts_for_unsynced_notebook_after_save_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("current");
        std::fs::create_dir_all(&current).unwrap();
        let path = current.join("pending.ipynb");
        let original = rustpy_notebook::new_notebook().to_string();
        std::fs::write(&path, &original).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(&current);
        let mut app = IdeApp::new(settings, Some(current.clone()));
        assert!(app.tabs.is_empty());
        app.open_file(&path);
        assert_eq!(app.tabs.len(), 1);
        let doc_version = app.tabs[0].state.doc.version();
        let history = app.tabs[0].state.doc.current_history();
        let nb = app.tabs[0].notebook.as_mut().unwrap();
        nb.set_cell_source(0, "print('pending unsynced cell')".into());
        nb.set_dirty(true);
        let synced = nb.synced_version();
        assert!(!app.tabs[0].state.doc.is_modified());
        let external = rustpy_notebook::new_notebook().to_string() + "\n";
        std::fs::write(&path, &external).unwrap();
        let next = dir.path().join("next");
        std::fs::create_dir_all(&next).unwrap();
        assert!(!app.save_before_switching(&next));
        let Dialog::OpenProject(st) = &app.dialog else {
            panic!("unsaved changes must prompt")
        };
        assert_eq!(st.root, next);
        assert_eq!(st.unsaved, vec![path.display().to_string()]);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].state.doc.text(), original);
        assert_eq!(app.tabs[0].state.doc.version(), doc_version);
        assert_eq!(app.tabs[0].state.doc.current_history(), history);
        let nb = app.tabs[0].notebook.as_ref().unwrap();
        assert!(nb.is_dirty());
        assert_eq!(nb.synced_version(), synced);
        assert_eq!(nb.model().cells[0].source, "print('pending unsynced cell')");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), external);
        assert_eq!(app.project.as_ref().unwrap().root, current);
    }

    #[test]
    fn open_decision_follows_the_setting() {
        let a = Path::new("/p/a");
        let b = Path::new("/p/b");
        assert_eq!(
            decide_open(None, a, OpenProjectIn::NewWindow),
            OpenDecision::OpenHere,
            "nothing open: open here"
        );
        assert_eq!(
            decide_open(Some(a), a, OpenProjectIn::Ask),
            OpenDecision::AlreadyOpen
        );
        assert_eq!(
            decide_open(Some(a), b, OpenProjectIn::Ask),
            OpenDecision::Ask
        );
        assert_eq!(
            decide_open(Some(a), b, OpenProjectIn::CurrentWindow),
            OpenDecision::OpenHere
        );
        assert_eq!(
            decide_open(Some(a), b, OpenProjectIn::NewWindow),
            OpenDecision::NewWindow
        );
    }
    #[test]
    fn startup_terminal_waits_for_selected_interpreter_and_honors_activation() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        let (tx, rx) = crossbeam_channel::unbounded();
        let interpreter = rustpy_project::Interpreter {
            path: dir.path().join("external/bin/python"),
            version: "3.12.1".into(),
            prefix: dir.path().join("external"),
            sys_path: vec![],
            is_venv: true,
            platform: "linux".into(),
            kind: "workspace".into(),
        };
        Workspace {
            interpreter: Some(interpreter.path.clone()),
            ..Default::default()
        }
        .save(dir.path())
        .unwrap();
        let mut app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        app.project.as_mut().unwrap().interpreter_rx = Some(rx);
        app.terminal_on_start = Some("echo ready".into());
        assert!(app.take_startup_terminal(5.0).is_none());
        assert_eq!(app.terminal_panel.tab_count(), 0);
        tx.send(vec![interpreter.clone()]).unwrap();
        app.poll_interpreters();
        let StartupTerminalDispatch { cwd, env, command } = app.take_startup_terminal(5.0).unwrap();
        assert_eq!(cwd, dir.path());
        assert_eq!(command, "echo ready");
        assert_eq!(
            env,
            crate::terminal_panel::terminal_env(true, Some(&interpreter))
        );
        assert!(app.take_startup_terminal(5.0).is_none());
        app.settings.terminal.activate_virtualenv = false;
        app.terminal_on_start = Some("echo no-activation".into());
        assert!(app.take_startup_terminal(5.0).unwrap().env.is_empty());
    }

    #[test]
    fn startup_terminal_cancels_on_session_change_and_finishes_disconnected_discovery() {
        let initial = tempfile::tempdir().unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(initial.path().to_path_buf()));
        assert_eq!(app.project.as_ref().unwrap().root, initial.path());
        app.terminal_on_start = Some("old-project-command".into());
        assert!(app.take_startup_terminal(0.5).is_none());
        app.file_session = app.file_session.wrapping_add(1);
        assert!(app.take_startup_terminal(5.0).is_none());
        assert!(app.terminal_on_start.is_none());
        let dir = tempfile::tempdir().unwrap();
        app.open_project(dir.path());
        let (tx, rx) = crossbeam_channel::unbounded();
        app.project.as_mut().unwrap().interpreter_rx = Some(rx);
        drop(tx);
        app.poll_interpreters();
        assert!(!app.interpreter_discovery_pending());
        app.terminal_on_start = Some("cancel on project switch".into());
        let next = tempfile::tempdir().unwrap();
        app.open_project(next.path());
        assert!(app.terminal_on_start.is_none());
        app.terminal_on_start = Some("cancel on close".into());
        app.close_project();
        assert!(app.terminal_on_start.is_none());
    }

    #[test]
    fn startup_terminal_ignores_old_project_discovery_events() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(first.path().to_path_buf()));
        let (old_tx, old_rx) = crossbeam_channel::unbounded();
        app.project.as_mut().unwrap().interpreter_rx = Some(old_rx);
        app.terminal_on_start = Some("old command".into());
        assert!(app.take_startup_terminal(5.0).is_none());
        app.open_project(second.path());
        assert!(
            old_tx.send(Vec::new()).is_err(),
            "switch drops old discovery receiver"
        );
        let (_new_tx, new_rx) = crossbeam_channel::unbounded();
        app.project.as_mut().unwrap().interpreter_rx = Some(new_rx);
        app.poll_interpreters();
        assert!(
            app.interpreter_discovery_pending(),
            "old event cannot finish new discovery"
        );
        assert!(app.take_startup_terminal(5.0).is_none());
        assert_eq!(app.terminal_panel.tab_count(), 0);
    }
}
