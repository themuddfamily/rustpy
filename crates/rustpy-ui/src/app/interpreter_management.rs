//! Add/show-all interpreter workflows, with all environment tools off the UI thread.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_project::environment_manager::{
    self as manager, Event, Manager, Registry, Request, Task,
};

pub(super) struct State {
    skeletons: Option<SkeletonBatch>,
    pub(super) open: bool,
    manager: Manager,
    existing: bool,
    executable: String,
    base: String,
    location: String,
    version: String,
    name: String,
    shared: bool,
    inherit: bool,
    install: bool,
    path_input: String,
    selected: Option<PathBuf>,
    registry: Registry,
    task: Option<(PathBuf, Task)>,
    message: Option<String>,
    prompted: std::collections::HashSet<PathBuf>,
}
const LOCAL_PYTHON: &str = if cfg!(windows) { "python" } else { "python3" };
impl Default for State {
    fn default() -> Self {
        Self {
            skeletons: None,
            open: false,
            manager: Manager::Virtualenv,
            existing: false,
            // On Windows `python3` is usually the Microsoft Store alias.
            executable: LOCAL_PYTHON.into(),
            base: LOCAL_PYTHON.into(),
            location: String::new(),
            version: "3.12".into(),
            name: String::new(),
            shared: false,
            inherit: false,
            install: true,
            path_input: String::new(),
            selected: None,
            registry: Registry::default(),
            task: None,
            message: None,
            prompted: Default::default(),
        }
    }
}
fn registry_path() -> PathBuf {
    manager::registry_path()
}
impl IdeApp {
    pub(super) fn open_interpreter_management(&mut self) {
        if self.settings_ui.snapshot.is_some() {
            self.pending_dialog = Some(Dialog::None);
        }
        let Some(project) = self.project.as_ref() else {
            self.set_status("Open a project first");
            return;
        };
        self.interpreter_management.registry = Registry::load(&registry_path());
        for interpreter in &project.interpreters {
            // Discovery is a project association, not an instruction to execute it.
            if !self
                .interpreter_management
                .registry
                .hidden_paths
                .contains(&interpreter.path)
                && !self
                    .interpreter_management
                    .registry
                    .entries
                    .iter()
                    .any(|e| e.interpreter.path == interpreter.path)
            {
                self.interpreter_management.registry.remember(
                    interpreter.clone(),
                    &project.root,
                    "",
                    false,
                );
            }
        }
        let state = &mut self.interpreter_management;
        state.open = true;
        state.location = project.root.join(".venv").display().to_string();
        if let Some(interpreter) = &project.interpreter {
            state.base = interpreter.path.display().to_string();
            state.selected = Some(interpreter.path.clone());
            if let Some(entry) = state
                .registry
                .entries
                .iter()
                .find(|entry| entry.interpreter.path == interpreter.path)
            {
                state.name = entry.name.clone();
                state.shared = entry.shared;
            }
        }
        if let Some((suggested, _)) = manager::suggested_manager(&project.root) {
            state.manager = suggested;
            state.executable =
                super::testing_ui::manager_executable(&self.settings.testing, suggested);
        }
    }
    /// Selection also maintains the reusable, named SDK catalogue. Module SDK
    /// assignment remains in Project Structure; run configs keep their override.
    pub(super) fn remember_managed_interpreter(&mut self, interpreter: &mut Interpreter) {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            return;
        };
        let mut registry = Registry::load(&registry_path());
        registry.remember(interpreter.clone(), &root, "", false);
        registry.apply_paths(interpreter);
        if let Err(error) = registry.save(&registry_path()) {
            self.set_status(format!("Cannot save interpreter catalogue: {error}"));
        }
        self.interpreter_management.registry = registry;
    }
    fn start_managed_interpreter(&mut self, reload: bool) {
        let project_safe_mode = self.project_safe_mode();
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            return;
        };
        if self.interpreter_management.task.is_some() {
            return;
        }
        let state = &mut self.interpreter_management;
        let request = if reload {
            let Some(path) = state.selected.clone() else {
                return;
            };
            Request {
                root: root.clone(),
                manager: Manager::System,
                existing: true,
                executable: path.clone(),
                base: path.clone(),
                location: path,
                python_version: String::new(),
                inherit_site_packages: false,
                install: false,
            }
        } else {
            Request {
                root: root.clone(),
                manager: state.manager,
                existing: state.existing,
                executable: PathBuf::from(state.executable.trim()),
                base: PathBuf::from(state.base.trim()),
                location: PathBuf::from(state.location.trim()),
                python_version: state.version.trim().into(),
                inherit_site_packages: state.inherit,
                install: state.install,
            }
        };
        match Task::start(request, !project_safe_mode) {
            Ok(task) => {
                state.task = Some((root, task));
                state.message = Some("Preparing environment…".into());
            }
            Err(error) => state.message = Some(error),
        }
    }
    pub(super) fn interpreter_management_frame(&mut self, ctx: &egui::Context) {
        let project_safe_mode = self.project_safe_mode();
        self.poll_interpreter_skeleton_batch(ctx);
        let event = self
            .interpreter_management
            .task
            .as_ref()
            .and_then(|(_, task)| task.poll());
        if let Some(event) = event {
            match event {
                Event::Progress(message) => self.interpreter_management.message = Some(message),
                Event::Finished(result) => {
                    let (root, _task) = self.interpreter_management.task.take().unwrap();
                    match result {
                        Ok(mut interpreter) => {
                            let state = &mut self.interpreter_management;
                            state.registry.remember(
                                interpreter.clone(),
                                &root,
                                &state.name,
                                state.shared,
                            );
                            state.registry.apply_paths(&mut interpreter);
                            state.selected = Some(interpreter.path.clone());
                            let saved = state.registry.save(&registry_path());
                            state.message =
                                Some(saved.err().unwrap_or_else(|| "Interpreter ready".into()));
                            if self.project.as_ref().is_some_and(|p| p.root == root) {
                                if let Some(project) = &mut self.project {
                                    project.interpreters.retain(|i| i.path != interpreter.path);
                                    project.interpreters.insert(0, interpreter.clone());
                                }
                                self.select_interpreter(interpreter);
                            } else {
                                self.set_status(format!(
                                    "Interpreter prepared for {}",
                                    root.display()
                                ));
                            }
                        }
                        Err(error) => self.interpreter_management.message = Some(error),
                    }
                }
            }
        }
        if self.interpreter_management.task.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            self.interpreter_management.open = false;
            return;
        };
        if self.interpreter_management.prompted.insert(root.clone()) && !root.join(".venv").exists()
        {
            if let Some((_, manifest)) = manager::suggested_manager(&root) {
                self.notify(
                    crate::notifications::NotificationKind::Info,
                    "Python Interpreter",
                    "Configure the project environment",
                    format!(
                        "Create an environment from {manifest} in Python Interpreter settings."
                    ),
                    vec![(
                        "Configure environment…".into(),
                        crate::notifications::NotificationAction::OpenSettings("interpreter"),
                    )],
                );
            }
        }
        if !self.interpreter_management.open {
            return;
        }
        let mut state = std::mem::take(&mut self.interpreter_management);
        let mut open = true;
        let mut create = false;
        let mut reload = false;
        let mut apply = None;
        let mut remote = None;
        let mut rebuild = false;
        let mut generate_all = false;
        let mut rebuild_all = false;
        let mut save = false;
        egui::Window::new("Add Interpreter / Show All Interpreters").id(egui::Id::new("interpreter-management"))
            .default_width(720.0).open(&mut open).show_modal(ctx,|ui| {
            ui.heading("Previously configured interpreters");
            egui::ScrollArea::vertical().id_salt("configured-interpreters").max_height(170.0).show(ui,|ui| {
                for entry in &state.registry.entries {
                    if !entry.shared && !entry.projects.contains(&root){continue;}
                    let selected=state.selected.as_ref()==Some(&entry.interpreter.path);
                    let response=ui.selectable_label(selected,format!("{} — {}",entry.name,entry.interpreter.path.display()));
                    response.clone().on_hover_text(format!("Projects: {}",entry.projects.iter().map(|p|p.display().to_string()).collect::<Vec<_>>().join(", ")));
                    if response.clicked(){state.selected=Some(entry.interpreter.path.clone());state.name=entry.name.clone();state.shared=entry.shared;}
                    if response.double_clicked(){apply=Some(entry.interpreter.clone());}
                }
            });
            if let Some(index)=state.registry.entries.iter().position(|e|Some(&e.interpreter.path)==state.selected.as_ref()) {
                ui.horizontal(|ui| {
                    ui.label("Interpreter name");if ui.text_edit_singleline(&mut state.name).changed(){state.registry.entries[index].name=state.name.clone();save=true;}
                    if ui.checkbox(&mut state.shared,"Available to all projects").changed(){state.registry.entries[index].shared=state.shared;save=true;}
                });
                ui.label("Interpreter paths (custom additions/removals are retained after reload)");
                let paths=state.registry.entries[index].interpreter.sys_path.clone();
                let mut remove=None;
                egui::ScrollArea::vertical().id_salt("interpreter-paths").max_height(110.0).show(ui,|ui| {
                    for path in paths.iter().chain(&state.registry.entries[index].paths) {
                        if state.registry.entries[index].removed_paths.contains(path){continue;}
                        ui.horizontal(|ui|{ui.label(path.display().to_string());if ui.small_button("Remove").clicked(){remove=Some(path.clone());}});
                    }
                });
                if let Some(path)=remove {state.registry.entries[index].paths.retain(|p|p!=&path);state.registry.entries[index].removed_paths.push(path);save=true;}
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut state.path_input);
                    if ui.button("Add path").clicked(){let path=PathBuf::from(state.path_input.trim());if path.is_absolute()&&path.is_dir(){state.registry.entries[index].removed_paths.retain(|p|p!=&path);if !state.registry.entries[index].paths.contains(&path){state.registry.entries[index].paths.push(path);}save=true;state.path_input.clear();}else{state.message=Some("Choose an existing absolute directory".into());}}
                });
                ui.horizontal(|ui| {
                    if ui.button("Use for project").clicked(){apply=Some(state.registry.entries[index].interpreter.clone());}
                    if ui.add_enabled(state.task.is_none(),egui::Button::new("Reload interpreter paths")).clicked(){reload=true;}
                    if ui.button("Rebuild skeletons for selected binary module").clicked(){rebuild=true;}
                    if ui.button("Remove from configured list").clicked(){let removed=state.registry.entries.remove(index);state.registry.hidden_paths.push(removed.interpreter.path);state.selected=None;save=true;}
                });
                ui.small("Module interpreter: Project Structure → Modules. Run interpreter: Run → Edit Configurations → Interpreter.");
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.add_enabled(state.skeletons.is_none()&&!project_safe_mode,egui::Button::new("Generate skeletons for binary modules")).clicked(){generate_all=true;}
                if ui.add_enabled(state.skeletons.is_none()&&!project_safe_mode,egui::Button::new("Rebuild skeletons")).clicked(){rebuild_all=true;}
                if let Some(batch)=&state.skeletons {ui.label(format!("Skeletons: {} / {}",batch.done,batch.total));if ui.button("Cancel skeletons").clicked(){batch.cancel.cancel();}}
            });
            ui.heading("Add New Interpreter");
            let previous=state.manager;
            egui::ComboBox::from_label("Environment manager").selected_text(state.manager.name()).show_ui(ui,|ui|{for manager in Manager::ALL{ui.selectable_value(&mut state.manager,manager,manager.name());}});
            if state.manager!=previous{state.executable=super::testing_ui::manager_executable(&self.settings.testing,state.manager);state.location=if state.manager==Manager::System{state.base.clone()}else{root.join(".venv").display().to_string()};}
            if state.manager!=Manager::System{ui.checkbox(&mut state.existing,"Use an existing environment");}
            egui::Grid::new("environment-create-fields").num_columns(2).show(ui,|ui| {
                ui.label(if state.manager==Manager::Conda{"Conda executable"}else{"Manager executable"});ui.text_edit_singleline(&mut state.executable);ui.end_row();
                if !state.existing&&state.manager!=Manager::System{ui.label("Base interpreter");ui.text_edit_singleline(&mut state.base);ui.end_row();}
                if state.existing||matches!(state.manager,Manager::Virtualenv|Manager::Conda|Manager::Uv|Manager::System){ui.label(if state.manager==Manager::System{"Python executable"}else{"Environment location"});ui.text_edit_singleline(&mut state.location);ui.end_row();}
                if state.manager==Manager::Conda&&!state.existing{ui.label("Python version");ui.text_edit_singleline(&mut state.version);ui.end_row();}
                ui.label("Display name");ui.text_edit_singleline(&mut state.name);ui.end_row();
            });
            if !state.existing&&matches!(state.manager,Manager::Virtualenv|Manager::Uv){ui.checkbox(&mut state.inherit,"Inherit global site-packages");}
            ui.checkbox(&mut state.shared,"Make available to all projects");
            if !state.existing&&state.manager!=Manager::System {ui.checkbox(&mut state.install,"Install/sync dependencies from project manifest or lock file");}
            if matches!(state.manager,Manager::Poetry|Manager::Pipenv|Manager::Hatch|Manager::Pdm)&&!state.existing {
                ui.small("The manager chooses its environment location. Hatch creates the default environment and installs its configured dependencies.");
            }
            if let Some((_,manifest))=manager::suggested_manager(&root){ui.label(format!("Project manifest detected: {manifest}"));}
            ui.horizontal(|ui| {
                if ui.add_enabled(state.task.is_none()&&!project_safe_mode,egui::Button::new(if state.existing||state.manager==Manager::System{"Add interpreter"}else{"Create environment"})).clicked(){create=true;}
                if state.task.is_some() {crate::a11y::spinner(ui);if ui.button("Cancel task").clicked(){if let Some((_,task))=&state.task{task.cancel();}}}
            });
            ui.horizontal_wrapped(|ui|{for (kind,label) in [("ssh","SSH"),("docker","Docker"),("compose","Docker Compose"),("wsl","WSL"),("vagrant","Vagrant")] {if ui.button(crate::i18n::tr_static(label)).clicked(){remote=Some(kind);}}});
            if project_safe_mode{ui.label("Trust the project to run environment managers and interpreter probes.");}
            if let Some(message)=&state.message{ui.label(message);}
        });
        state.open = open;
        if save {
            if let Err(error) = state.registry.save(&registry_path()) {
                state.message = Some(error);
            }
        }
        self.interpreter_management = state;
        if let Some(interpreter) = apply {
            self.interpreter_management.selected = Some(interpreter.path);
            self.start_managed_interpreter(true);
        }
        if generate_all || rebuild_all {
            self.start_interpreter_skeleton_batch(rebuild_all);
        }
        if create || reload {
            self.start_managed_interpreter(reload);
        }
        if rebuild {
            self.interpreter_management.open = false;
            self.generate_native_stub();
        }
        if let Some(kind) = remote {
            self.interpreter_management.open = false;
            self.dialog = Dialog::RemoteInterpreter {
                kind: kind.into(),
                target: String::new(),
                python: "python3".into(),
                remote_root: String::new(),
                error: None,
            };
        }
    }
    pub(super) fn interpreter_configuration_banner(&mut self, ui: &mut egui::Ui) {
        if self
            .project
            .as_ref()
            .is_some_and(|p| p.interpreter.is_none() && p.interpreter_rx.is_none())
        {
            ui.horizontal(|ui| {
                ui.colored_label(self.theme.warning, "Python interpreter not configured");
                if ui.button("Add New Interpreter…").clicked() {
                    self.open_interpreter_management();
                }
            });
        }
    }
}

struct SkeletonBatch {
    root: PathBuf,
    interpreter: PathBuf,
    analysis: Arc<Analysis>,
    previous: Option<PathBuf>,
    current: Option<PathBuf>,
    targets: std::collections::VecDeque<PathBuf>,
    scan: Option<crossbeam_channel::Receiver<Result<Vec<PathBuf>, String>>>,
    cancel: rustpy_core::CancellationToken,
    done: usize,
    failed: usize,
    total: usize,
}
impl Drop for SkeletonBatch {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl IdeApp {
    fn start_interpreter_skeleton_batch(&mut self, rebuild: bool) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode
            || self.native_skeletons_pending()
            || self.interpreter_management.skeletons.is_some()
        {
            self.set_status("Finish the current skeleton task first");
            return;
        }
        let Some(project) = self.project.as_ref() else {
            return;
        };
        if project.interpreter_rx.is_some() {
            self.set_status("Wait for interpreter discovery before generating skeletons");
            return;
        }
        let Some(interpreter) = project.interpreter.clone() else {
            self.notify_no_interpreter();
            return;
        };
        let cancel = rustpy_core::CancellationToken::new();
        let token = cancel.clone();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let python = interpreter.path.clone();
        let root = project.root.clone();
        let ignored_root = root.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let python = std::fs::canonicalize(python).map_err(|e| e.to_string())?;
                if rebuild {
                    rustpy_project::native_skeleton::clear_interpreter_cache(&python, &token)
                        .map_err(|e| e.to_string())?;
                }
                let mut pending = interpreter
                    .sys_path
                    .into_iter()
                    .filter(|p| p.is_dir() && p != &ignored_root)
                    .map(|p| (p, 0))
                    .collect::<Vec<_>>();
                let mut visited = std::collections::HashSet::new();
                let mut targets = Vec::new();
                let mut examined = 0;
                while let Some((directory, depth)) = pending.pop() {
                    token.check().map_err(|e| e.to_string())?;
                    if depth > 16 {
                        continue;
                    }
                    let Ok(directory) = std::fs::canonicalize(directory) else {
                        continue;
                    };
                    if !visited.insert(directory.clone()) {
                        continue;
                    }
                    let Ok(entries) = std::fs::read_dir(directory) else {
                        continue;
                    };
                    for entry in entries.flatten() {
                        token.check().map_err(|e| e.to_string())?;
                        examined += 1;
                        if examined > 100_000 {
                            return Err("Interpreter library scan exceeded 100,000 entries; generate individual selected modules instead".into());
                        }
                        let Ok(kind) = entry.file_type() else {
                            continue;
                        };
                        if kind.is_symlink() {
                            continue;
                        }
                        if kind.is_dir() {
                            if entry.file_name() != "__pycache__" {
                                pending.push((entry.path(), depth + 1));
                            }
                        } else if kind.is_file()
                            && entry
                                .path()
                                .extension()
                                .is_some_and(|e| e == "so" || e == "pyd")
                        {
                            targets.push(entry.path());
                            if targets.len() > 4096 {
                                return Err("Interpreter has more than 4096 native images; generate individual selected modules instead".into());
                            }
                        }
                    }
                }
                targets.sort();
                targets.dedup();
                Ok(targets)
            })();
            let _ = tx.send(result);
        });
        self.interpreter_management.skeletons = Some(SkeletonBatch {
            root,
            interpreter: project.interpreter.as_ref().unwrap().path.clone(),
            analysis: project.analysis.clone(),
            previous: project.tree.selected.clone(),
            current: None,
            targets: Default::default(),
            scan: Some(rx),
            cancel,
            done: 0,
            failed: 0,
            total: 0,
        });
        self.set_status("Scanning interpreter binary modules…");
    }
    fn poll_interpreter_skeleton_batch(&mut self, ctx: &egui::Context) {
        let project_safe_mode = self.project_safe_mode();
        let Some(mut batch) = self.interpreter_management.skeletons.take() else {
            return;
        };
        let valid = self.project.as_ref().is_some_and(|p| {
            p.root == batch.root
                && p.interpreter
                    .as_ref()
                    .is_some_and(|i| i.path == batch.interpreter)
                && Arc::ptr_eq(&p.analysis, &batch.analysis)
                && batch
                    .current
                    .as_ref()
                    .is_none_or(|current| p.tree.selected.as_ref() == Some(current))
        });
        if project_safe_mode || batch.cancel.is_cancelled() || !valid {
            self.cancel_native_skeletons();
            if let Some(p) = self.project.as_mut().filter(|p| p.root == batch.root) {
                if batch
                    .current
                    .as_ref()
                    .is_some_and(|current| p.tree.selected.as_ref() == Some(current))
                {
                    p.tree.selected = batch.previous.take();
                }
            }
            self.set_status("Interpreter skeleton generation cancelled");
            return;
        }
        if let Some(rx) = &batch.scan {
            match rx.try_recv() {
                Ok(Ok(paths)) => {
                    batch.total = paths.len();
                    batch.targets = paths.into();
                    batch.scan = None;
                }
                Ok(Err(error)) => {
                    self.set_status(error);
                    return;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.set_status("Interpreter library scan stopped");
                    return;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
        } else if !self.native_skeletons_pending() {
            if let Some(current) = batch.current.take() {
                batch.done += 1;
                if !self.native_skeleton_paths().contains(&current) {
                    batch.failed += 1;
                }
            }
            if let Some(path) = batch.targets.pop_front() {
                if let Some(project) = self.project.as_mut() {
                    project.tree.selected = Some(path.clone());
                }
                batch.current = Some(path);
                let manager_open = self.interpreter_management.open;
                self.interpreter_management.open = false;
                self.generate_native_stub();
                self.interpreter_management.open = manager_open;
            } else {
                if let Some(project) = self.project.as_mut() {
                    project.tree.selected = batch.previous.take();
                }
                self.set_status(format!(
                    "Skeleton generation finished: {} binary images processed, {} unavailable",
                    batch.done, batch.failed
                ));
                return;
            }
        }
        self.interpreter_management.skeletons = Some(batch);
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    pub(super) fn interpreter_management_stamps(&self) -> Vec<String> {
        self.interpreter_management
            .task
            .iter()
            .map(|(_, task)| task.quit_guard_state())
            .chain(self.interpreter_management.skeletons.iter().map(|batch| {
                format!(
                    "skeleton-batch:{}:{}:{}",
                    batch.root.display(),
                    batch.done,
                    batch.cancel.is_cancelled()
                )
            }))
            .collect()
    }
}

impl IdeApp {
    pub(super) fn interpreter_management_active(&self) -> bool {
        self.interpreter_management.open
    }
    pub(super) fn cancel_interpreter_management(&mut self) {
        if let Some((_, task)) = self.interpreter_management.task.take() {
            task.cancel();
        }
        if let Some(batch) = self.interpreter_management.skeletons.take() {
            batch.cancel.cancel();
        }
        self.interpreter_management.open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manager_window_is_available_without_starting_processes() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(directory.path().into()));
        app.open_interpreter_management();
        assert!(app.interpreter_management.open);
        assert!(app.interpreter_management.task.is_none());
        assert_eq!(
            app.interpreter_management.location,
            directory.path().join(".venv").display().to_string()
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "interpreter_management/configured_tests.rs"]
mod configured_tests;
