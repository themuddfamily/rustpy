//! Captured, serial watcher dispatch. All child ownership remains in RunPanel.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::external_tools::{self, Invocation};
use rustpy_settings::{file_watchers::Level, FileWatcher};
use std::{
    collections::BTreeMap,
    hash::Hasher,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Job {
    root: PathBuf,
    path: PathBuf,
    watcher: FileWatcher,
    invocation: Invocation,
    outputs: Vec<PathBuf>,
    source: u64,
    automatic: bool,
    due: Instant,
}
struct Active {
    id: u64,
    job: Job,
    superseded: bool,
}
#[derive(Default)]
pub(super) struct Runtime {
    root: Option<PathBuf>,
    pub project: Vec<FileWatcher>,
    pending: BTreeMap<(String, PathBuf), Job>,
    active: Option<Active>,
    /// Exact content identities suppress own-save and generated-output events,
    /// without suppressing later genuine external edits to the same path.
    observed: BTreeMap<PathBuf, u64>,
    edits: BTreeMap<PathBuf, (String, Instant)>,
    before_paths: Vec<PathBuf>,
    message: String,
}
fn signature(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut hash = std::hash::DefaultHasher::new();
    let mut buffer = [0u8; 16384];
    loop {
        let n = file.read(&mut buffer).ok()?;
        if n == 0 {
            break;
        }
        hash.write(&buffer[..n]);
    }
    Some(hash.finish())
}
fn output_identities(paths: &[PathBuf]) -> Vec<(PathBuf, u64)> {
    let mut pending = paths.to_vec();
    let mut identities = Vec::new();
    let mut visited = 0usize;
    while let Some(path) = pending.pop() {
        visited += 1;
        if visited > 20_000 {
            break;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                pending.extend(entries.filter_map(Result::ok).map(|e| e.path()));
            }
        } else if let Some(source) = signature(&path) {
            identities.push((path, source));
        }
    }
    identities
}
fn key(w: &FileWatcher, path: &Path) -> (String, PathBuf) {
    (
        serde_json::to_string(w).unwrap_or_else(|_| format!("{:?}:{}", w.level, w.name)),
        path.to_path_buf(),
    )
}
fn file_type_matches(w: &FileWatcher, path: &Path, language: &str) -> bool {
    let kind = w.file_type.trim();
    if kind.is_empty() || kind.eq_ignore_ascii_case("Any") {
        return true;
    }
    if kind.eq_ignore_ascii_case(language) {
        return true;
    }
    let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
    matches!(
        (kind.to_ascii_lowercase().as_str(), ext),
        ("scss", "scss")
            | ("less", "less")
            | ("typescript", "ts" | "tsx")
            | ("javascript", "js" | "jsx" | "mjs" | "cjs")
    )
}
fn project_path(root: &Path) -> PathBuf {
    root.join(".rustpy/fileWatchers.json")
}
impl IdeApp {
    fn watcher_definitions(&self) -> Vec<FileWatcher> {
        self.settings
            .file_watchers
            .iter()
            .chain(&self.watcher_runtime.project)
            .cloned()
            .collect()
    }
    fn watcher_scope_context(&self, path: &Path) -> rustpy_settings::scopes::ScopeContext {
        let mut context = self.capture_scope_context();
        // The exact event/save operand can precede the asynchronous file-index
        // rebuild. Its current scope membership is already knowable without
        // admitting excluded/ignored files or walking the whole project.
        if self
            .project_scope()
            .is_some_and(|scope| scope.contains(path))
        {
            let files = Arc::make_mut(&mut context.project_files);
            if let Err(index) = files.binary_search_by(|entry| entry.as_path().cmp(path)) {
                files.insert(index, path.to_path_buf());
            }
        }
        context
    }
    fn watcher_matches(
        &self,
        w: &FileWatcher,
        path: &Path,
        context: &rustpy_settings::scopes::ScopeContext,
    ) -> bool {
        let language = self
            .tabs
            .iter()
            .find(|t| t.state.path.as_deref() == Some(path))
            .map(|t| t.state.language_label())
            .unwrap_or_else(|| {
                crate::editor::Language::from_path(path)
                    .display_name()
                    .into()
            });
        let name = path.file_name().and_then(|p| p.to_str()).unwrap_or("");
        w.enabled
            && file_type_matches(w, path, &language)
            && rustpy_settings::editorconfig::glob_match(&w.pattern, name)
            && self
                .settings
                .scopes
                .matches(&w.scope, path, context)
                .unwrap_or(false)
    }
    pub(super) fn load_file_watchers(&mut self) {
        let root = self.project.as_ref().map(|p| p.root.clone());
        if self.watcher_runtime.root == root {
            return;
        }
        self.cancel_file_watchers();
        self.watcher_runtime.root = root.clone();
        if let Some(root) = root {
            match std::fs::read(project_path(&root)) {
                Ok(bytes) => match serde_json::from_slice::<Vec<FileWatcher>>(&bytes) {
                    Ok(mut entries) => {
                        for w in &mut entries {
                            w.level = Level::Project;
                        }
                        self.watcher_runtime.project = entries;
                    }
                    Err(e) => {
                        self.watcher_runtime.message = format!("Cannot read project watchers: {e}")
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    self.watcher_runtime.message = format!("Cannot read project watchers: {e}")
                }
            }
        }
    }
    pub(super) fn named_watcher_invocation(
        &self,
        name: &str,
        file: Option<&Path>,
    ) -> Result<Invocation, String> {
        let project = self.project.as_ref().ok_or("Open a project first")?;
        let disk_project: Vec<FileWatcher> =
            if self.watcher_runtime.root.as_ref() == Some(&project.root) {
                self.watcher_runtime.project.clone()
            } else {
                match std::fs::read(project_path(&project.root)) {
                    Ok(bytes) => serde_json::from_slice(&bytes)
                        .map_err(|e| format!("Cannot read project watchers: {e}"))?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                    Err(e) => return Err(e.to_string()),
                }
            };
        let watcher = disk_project
            .iter()
            .chain(&self.settings.file_watchers)
            .find(|w| w.enabled && w.name == name)
            .ok_or_else(|| format!("Enabled file watcher '{name}' does not exist"))?;
        let file = file.ok_or("A file watcher prerequisite needs an active saved file")?;
        if !self.watcher_matches(watcher, file, &self.capture_scope_context()) {
            return Err(format!(
                "File does not match watcher '{name}' type, mask or scope"
            ));
        }
        let mut macros = self.macro_context(Some(file));
        if [
            &watcher.program,
            &watcher.arguments,
            &watcher.working_dir,
            &watcher.output_paths,
        ]
        .iter()
        .any(|s| s.contains("$ClipboardContent$"))
        {
            macros.clipboard = self.clipboard_text();
        }
        external_tools::build_invocation(
            &watcher.program,
            &watcher.arguments,
            &watcher.working_dir,
            &macros,
            |p| p.is_file(),
        )
        .map_err(|e| format!("File watcher {name}: {e:?}"))
    }
    pub(super) fn prepare_watcher_prerequisites(
        &mut self,
        plan: &[rustpy_project::run_workflow::Planned],
    ) -> Result<(), String> {
        use rustpy_project::run_workflow::{Planned, Task};
        let names = plan
            .iter()
            .filter_map(|p| match p {
                Planned::Task(Task::FileWatcher { name }) => Some(name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if names.is_empty() {
            return Ok(());
        }
        self.load_file_watchers();
        if self.watcher_runtime.active.is_some() {
            return Err("Wait for the running file watcher or stop it before launching watcher prerequisites".into());
        }
        let file = self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|t| t.state.path.clone())
            .ok_or("File watcher prerequisites need an active saved file")?;
        let root = self
            .project
            .as_ref()
            .ok_or("Open a project first")?
            .root
            .clone();
        let macros = self.macro_context(Some(&file));
        let definitions = self.watcher_definitions();
        let mut paths = vec![file];
        for name in &names {
            let watcher = definitions
                .iter()
                .rev()
                .find(|w| w.enabled && w.name == *name)
                .ok_or_else(|| format!("Enabled watcher '{name}' does not exist"))?;
            let invocation = self.named_watcher_invocation(name, macros.file.as_deref())?;
            let cwd = invocation
                .working_dir
                .map(|p| if p.is_absolute() { p } else { root.join(p) })
                .unwrap_or_else(|| root.clone());
            for output in watcher
                .output_paths
                .lines()
                .filter(|s| !s.trim().is_empty())
            {
                let path = PathBuf::from(external_tools::expand_macros(output.trim(), &macros));
                paths.push(rustpy_core::vfs::normalize_path(&if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                }));
            }
        }
        self.watcher_runtime
            .pending
            .retain(|_, job| !names.contains(&job.watcher.name.as_str()));
        self.watcher_runtime.before_paths.extend(paths);
        Ok(())
    }
    pub(super) fn stop_file_watchers(&mut self) {
        if let Some(active) = &mut self.watcher_runtime.active {
            active.superseded = true;
            self.run_panel.request_stop(active.id);
        }
        self.watcher_runtime.pending.clear();
        self.watcher_runtime.before_paths.clear();
        self.watcher_runtime.edits.clear();
    }
    pub(super) fn cancel_file_watchers(&mut self) {
        self.stop_file_watchers();
        self.watcher_runtime = Runtime::default();
    }
    pub(super) fn file_watcher_pending_labels(&self) -> Vec<String> {
        self.watcher_runtime
            .pending
            .values()
            .map(|j| {
                format!(
                    "Pending file watcher: {} ({})",
                    j.watcher.name,
                    j.path.display()
                )
            })
            .collect()
    }
    pub(super) fn file_watcher_lifecycle_stamp(&self) -> String {
        format!(
            "{:?}|{:?}|{:?}|{:?}",
            self.watcher_runtime.root,
            self.watcher_runtime.before_paths,
            self.watcher_runtime.active.as_ref().map(|a| a.id),
            self.watcher_runtime
                .pending
                .values()
                .map(|j| (&j.root, &j.path, &j.invocation, &j.outputs, j.source))
                .collect::<Vec<_>>()
        )
    }
    pub(super) fn queue_file_watchers(&mut self, path: &Path, external: bool) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            return;
        }
        self.load_file_watchers();
        let Some(root) = self.watcher_runtime.root.clone() else {
            return;
        };
        let path = rustpy_core::vfs::normalize_path(path);
        if !path.starts_with(&root) || !path.is_file() {
            return;
        }
        // Save actions own their mask/automatic-save policy; external events
        // additionally require the watcher's explicit external-change option.
        if !self.settings.save_actions.run_file_watchers || !self.save_actions_apply(&path) {
            return;
        }
        let affected_active = self.watcher_runtime.active.as_ref().is_some_and(|a| {
            a.job.path == path
                || a.job
                    .outputs
                    .iter()
                    .any(|p| path == *p || path.starts_with(p))
        });
        if affected_active {
            if external {
                return;
            }
            if let Some(active) = &mut self.watcher_runtime.active {
                active.superseded = true;
                self.run_panel.request_stop(active.id);
            }
        }
        if self
            .watcher_runtime
            .before_paths
            .iter()
            .any(|p| path == *p || path.starts_with(p))
        {
            return;
        }
        let Some(source) = signature(&path) else {
            return;
        };
        if self.watcher_runtime.observed.get(&path) == Some(&source) {
            return;
        }
        if external
            && self
                .tabs
                .iter()
                .any(|t| t.state.path.as_deref() == Some(path.as_path()) && t.has_unsaved_changes())
        {
            return;
        }
        let context = self.watcher_scope_context(&path);
        for watcher in self.watcher_definitions() {
            if external && !watcher.external_changes
                || !self.watcher_matches(&watcher, &path, &context)
            {
                continue;
            }
            let mut macros = self.macro_context(Some(&path));
            if [
                &watcher.program,
                &watcher.arguments,
                &watcher.working_dir,
                &watcher.output_paths,
            ]
            .iter()
            .any(|s| s.contains("$ClipboardContent$"))
            {
                macros.clipboard = self.clipboard_text();
            }
            let identity = key(&watcher, &path);
            if self
                .watcher_runtime
                .pending
                .get(&identity)
                .is_some_and(|j| j.source == source)
            {
                continue;
            }
            match external_tools::build_invocation(
                &watcher.program,
                &watcher.arguments,
                &watcher.working_dir,
                &macros,
                |p| p.is_file(),
            ) {
                Ok(mut invocation) => {
                    let cwd = invocation
                        .working_dir
                        .take()
                        .map(|p| if p.is_absolute() { p } else { root.join(p) })
                        .unwrap_or_else(|| root.clone());
                    invocation.working_dir = Some(cwd.clone());
                    let outputs = watcher
                        .output_paths
                        .lines()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| PathBuf::from(external_tools::expand_macros(s.trim(), &macros)))
                        .map(|p| {
                            rustpy_core::vfs::normalize_path(&if p.is_absolute() {
                                p
                            } else {
                                cwd.join(p)
                            })
                        })
                        .collect();
                    let due =
                        Instant::now() + Duration::from_millis(watcher.debounce_ms.min(60_000));
                    self.watcher_runtime.pending.insert(
                        identity,
                        Job {
                            root: root.clone(),
                            path: path.clone(),
                            watcher,
                            invocation,
                            outputs,
                            source,
                            automatic: self.save_synchronization.automatic,
                            due,
                        },
                    );
                }
                Err(error) => self.set_status(format!("File watcher {}: {error:?}", watcher.name)),
            }
        }
    }
    pub(super) fn file_watchers_tick(&mut self, ctx: &egui::Context) {
        let project_safe_mode = self.project_safe_mode();
        self.load_file_watchers();
        if project_safe_mode {
            self.stop_file_watchers();
            return;
        }
        if let Some(active) = self.watcher_runtime.active.take() {
            let editor_changed = self
                .tabs
                .iter()
                .find(|t| t.state.path.as_deref() == Some(active.job.path.as_path()))
                .is_some_and(|t| t.has_unsaved_changes());
            let running = self
                .run_panel
                .sessions
                .iter()
                .find(|s| s.id == active.id)
                .is_some_and(|s| s.is_running());
            if running {
                if editor_changed {
                    self.run_panel.request_stop(active.id);
                }
                self.watcher_runtime.active = Some(active);
                self.watcher_auto_save();
                ctx.request_repaint_after(Duration::from_millis(100));
                return;
            }
            if let Some(session) = self.run_panel.sessions.iter().find(|s| s.id == active.id) {
                if !matches!(session.status, rustpy_run::SessionStatus::Exited(0)) {
                    self.set_status(format!(
                        "File watcher {} finished: {:?} (see Run console)",
                        active.job.watcher.name, session.status
                    ));
                }
            }
            if self
                .project
                .as_ref()
                .is_some_and(|p| p.root == active.job.root)
                && !editor_changed
                && !active.superseded
            {
                let mut paths = active.job.outputs.clone();
                paths.push(active.job.path.clone());
                for (path, source) in output_identities(&paths) {
                    self.watcher_runtime.observed.insert(path, source);
                }
                // A formatter earlier in the same serialized batch may change
                // the input of later watchers. Recapture only that known output.
                if let Some(source) = signature(&active.job.path) {
                    for job in self
                        .watcher_runtime
                        .pending
                        .values_mut()
                        .filter(|j| j.root == active.job.root && j.path == active.job.path)
                    {
                        job.source = source;
                    }
                }
                self.invalidate_analysis_paths(&paths);
                self.queue_open_file_reconciliation();
                self.refresh_project_files();
                if let Some(p) = &mut self.project {
                    p.tree.invalidate_all();
                }
            }
        }
        if !self.watcher_runtime.before_paths.is_empty() {
            if self.run_workflows_pending() {
                ctx.request_repaint_after(Duration::from_millis(100));
                return;
            }
            let paths = std::mem::take(&mut self.watcher_runtime.before_paths);
            for (path, source) in output_identities(&paths) {
                self.watcher_runtime.observed.insert(path, source);
            }
            self.queue_open_file_reconciliation();
            self.refresh_project_files();
        }
        if self.run_workflows_pending() {
            return;
        }
        self.watcher_auto_save();
        if self
            .run_panel
            .sessions
            .iter()
            .any(|session| session.name.starts_with("File watcher:") && session.is_running())
        {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        let now = Instant::now();
        let next = self
            .watcher_runtime
            .pending
            .iter()
            .filter(|(_, j)| j.due <= now)
            .min_by_key(|(_, j)| j.due)
            .map(|(key, _)| key.clone());
        if let Some(key) = next {
            let job = self.watcher_runtime.pending.remove(&key).unwrap();
            let context = self.watcher_scope_context(&job.path);
            let valid = self.project.as_ref().is_some_and(|p| p.root == job.root)
                && self.watcher_definitions().contains(&job.watcher)
                && self.settings.save_actions.run_file_watchers
                && (!job.automatic || self.settings.save_actions.on_auto_save)
                && self.save_actions_apply(&job.path)
                && self.watcher_matches(&job.watcher, &job.path, &context)
                && signature(&job.path) == Some(job.source)
                && !self.tabs.iter().any(|t| {
                    t.state.path.as_deref() == Some(job.path.as_path()) && t.has_unsaved_changes()
                });
            if valid {
                let cfg = RunConfig {
                    name: format!("File watcher: {}", job.watcher.name),
                    kind: RunConfigKind::Program {
                        program: job.invocation.program.clone(),
                        args: job.invocation.args.clone(),
                    },
                    working_dir: job.invocation.working_dir.clone(),
                    temporary: true,
                    add_content_roots: false,
                    add_source_roots: false,
                    ..Default::default()
                };
                let id = self.next_session_id;
                self.next_session_id += 1;
                self.run_panel
                    .add_session(RunSession::spawn(id, &cfg, None, &job.root));
                if job.watcher.show_console {
                    self.bottom = Some(BottomTool::Run);
                }
                self.watcher_runtime.active = Some(Active {
                    id,
                    job,
                    superseded: false,
                });
            }
        }
        if !self.watcher_runtime.pending.is_empty()
            || self.watcher_runtime.active.is_some()
            || !self.watcher_runtime.edits.is_empty()
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
    fn watcher_auto_save(&mut self) {
        let context = self.capture_scope_context();
        let definitions = self.watcher_definitions();
        let now = Instant::now();
        let mut save = Vec::new();
        for (index, tab) in self.tabs.iter().enumerate() {
            let Some(path) = tab.state.path.as_ref() else {
                continue;
            };
            if !tab.has_unsaved_changes()
                || tab.state.doc.is_read_only()
                || tab.state.doc.transaction_depth() != 0
                || tab.notebook.as_ref().is_some_and(|n| n.is_busy())
            {
                self.watcher_runtime.edits.remove(path);
                continue;
            }
            if !self.save_actions_apply(path)
                || !self.settings.save_actions.on_auto_save
                || !self.settings.save_actions.run_file_watchers
            {
                continue;
            }
            let Some(delay) = definitions
                .iter()
                .filter(|w| w.auto_save && self.watcher_matches(w, path, &context))
                .map(|w| w.debounce_ms)
                .min()
            else {
                continue;
            };
            let version = format!("{:?}", tab.state.doc.version());
            let entry = self
                .watcher_runtime
                .edits
                .entry(path.clone())
                .or_insert((version.clone(), now));
            if entry.0 != version {
                *entry = (version, now);
            }
            if now.duration_since(entry.1) >= Duration::from_millis(delay.clamp(100, 60_000)) {
                save.push(index);
            }
        }
        if !save.is_empty() {
            let automatic = self.save_synchronization.automatic;
            self.save_synchronization.automatic = true;
            for index in save {
                let path = self.tabs[index].state.path.clone();
                if self.refresh_file_permissions(index) {
                    self.save_tab(index);
                }
                if let Some(path) = path {
                    self.watcher_runtime.edits.remove(&path);
                }
            }
            self.save_synchronization.automatic = automatic;
        }
    }
}

fn watcher_form(ui: &mut egui::Ui, w: &mut FileWatcher, names: &[String], id: &str) -> bool {
    let before = w.clone();
    ui.horizontal(|ui| {
        ui.checkbox(&mut w.enabled, "Enabled");
        ui.label("Name");
        ui.text_edit_singleline(&mut w.name);
    });
    ui.horizontal(|ui| {
        let level_label = ui.label("Level");
        egui::ComboBox::from_id_salt((id, "level"))
            .selected_text(format!("{:?}", w.level))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut w.level, Level::Project, "Project");
                ui.selectable_value(&mut w.level, Level::Global, "Global");
            })
            .combo_labelled(&level_label);
    });
    ui.horizontal(|ui| {
        ui.label("File type");
        ui.text_edit_singleline(&mut w.file_type).on_hover_text(
            "Any, Python, JavaScript, TypeScript, SCSS, Less, or an editor language name",
        );
        ui.label("Filename mask");
        ui.text_edit_singleline(&mut w.pattern);
    });
    ui.horizontal(|ui| {
        ui.label("Scope");
        super::scope_ui::scope_selector(ui, &mut w.scope, names, id);
    });
    if ui.small_button("Custom scope pattern").clicked() {
        w.scope = rustpy_settings::scopes::ScopeSelection::Custom("file:*".into());
    }
    if let rustpy_settings::scopes::ScopeSelection::Custom(pattern) = &mut w.scope {
        ui.text_edit_singleline(pattern);
    }
    for (label, value) in [
        ("Program", &mut w.program),
        ("Arguments", &mut w.arguments),
        ("Working directory", &mut w.working_dir),
    ] {
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr_static(label));
            ui.text_edit_singleline(value);
        });
    }
    ui.label("Output paths to refresh (one per line; macros supported)");
    ui.text_edit_multiline(&mut w.output_paths);
    ui.horizontal(|ui| {
        ui.checkbox(&mut w.auto_save, "Auto-save edited files to trigger");
        ui.checkbox(&mut w.external_changes, "Trigger on external changes");
        ui.checkbox(&mut w.show_console, "Show console");
    });
    ui.horizontal(|ui| {
        ui.label("Debounce (ms)");
        ui.add(egui::DragValue::new(&mut w.debounce_ms).range(0..=60_000));
    });
    egui::CollapsingHeader::new("Available macros")
        .id_salt((id, "macros"))
        .show(ui, |ui| {
            for (name, description) in external_tools::MACROS {
                ui.label(format!("${name}$ — {description}"));
            }
        });
    before != *w
}
impl IdeApp {
    pub(super) fn save_project_watchers(&self) -> anyhow::Result<()> {
        let Some(root) = self.watcher_runtime.root.as_ref() else {
            if !self.watcher_runtime.project.is_empty() {
                anyhow::bail!("Open a project before adding project-level watchers");
            }
            return Ok(());
        };
        for w in self
            .settings
            .file_watchers
            .iter()
            .chain(&self.watcher_runtime.project)
        {
            self.settings
                .scopes
                .compile(&w.scope)
                .map_err(anyhow::Error::msg)?;
            if w.name.trim().is_empty() {
                anyhow::bail!("Every watcher needs a name");
            }
        }
        let path = project_path(root);
        std::fs::create_dir_all(path.parent().unwrap())?;
        use std::io::Write;
        let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        temporary.write_all(&serde_json::to_vec_pretty(&self.watcher_runtime.project)?)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&path).map_err(|e| e.error)?;
        Ok(())
    }
    pub(super) fn file_watchers_settings(&mut self, ui: &mut egui::Ui) {
        self.load_file_watchers();
        ui.heading("File Watchers");
        ui.label("Watchers run directly without a shell, after a matching changed save or an enabled external-change trigger. Project watchers are stored in .rustpy/fileWatchers.json; global watchers use IDE settings.");
        ui.label("Auto-save respects Actions on Save masks and its automatic-save/watcher switches. Safe Mode disables execution.");
        let names = self
            .settings
            .scopes
            .named
            .iter()
            .map(|scope| scope.name.clone())
            .collect::<Vec<_>>();
        ui.horizontal_wrapped(|ui| {
            ui.label("Add template:");
            for name in FileWatcher::TEMPLATES {
                if ui.button(*name).clicked() {
                    self.settings
                        .file_watchers
                        .push(FileWatcher::template(name));
                }
            }
        });
        let mut moved_project = Vec::new();
        let mut moved_global = Vec::new();
        let mut remove = None;
        ui.heading("Global");
        for (i, w) in self.settings.file_watchers.iter_mut().enumerate() {
            egui::CollapsingHeader::new(format!(
                "{}{}",
                if w.enabled { "" } else { "Disabled: " },
                w.name
            ))
            .id_salt(("watcher-global", i))
            .show(ui, |ui| {
                watcher_form(ui, w, &names, &format!("global-watcher-{i}"));
                if ui.button("Remove watcher").clicked() {
                    remove = Some(i);
                }
            });
            if w.level == Level::Project {
                moved_project.push(i);
            }
        }
        if let Some(index) = remove {
            self.settings.file_watchers.remove(index);
            moved_project.clear();
        }
        for index in moved_project.into_iter().rev() {
            self.watcher_runtime
                .project
                .push(self.settings.file_watchers.remove(index));
        }
        remove = None;
        ui.heading("Project");
        for (i, w) in self.watcher_runtime.project.iter_mut().enumerate() {
            egui::CollapsingHeader::new(&w.name)
                .id_salt(("watcher-project", i))
                .show(ui, |ui| {
                    watcher_form(ui, w, &names, &format!("project-watcher-{i}"));
                    if ui.button("Remove project watcher").clicked() {
                        remove = Some(i);
                    }
                });
            if w.level == Level::Global {
                moved_global.push(i);
            }
        }
        if let Some(index) = remove {
            self.watcher_runtime.project.remove(index);
            moved_global.clear();
        }
        for index in moved_global.into_iter().rev() {
            self.settings
                .file_watchers
                .push(self.watcher_runtime.project.remove(index));
        }
        ui.label("Apply saves watcher definitions at both levels; Cancel restores changes.");
        if !self.watcher_runtime.message.is_empty() {
            ui.label(&self.watcher_runtime.message);
        }
        if let Some(active) = &self.watcher_runtime.active {
            ui.label(format!(
                "Running: {} — {}",
                active.job.watcher.name,
                active.job.path.display()
            ));
        }
        ui.label(format!(
            "{} queued watcher invocation(s)",
            self.watcher_runtime.pending.len()
        ));
        if ui
            .button("Stop watcher and discard queued invocations")
            .clicked()
        {
            self.stop_file_watchers();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_type_and_mask_are_independent_restrictions() {
        let w = FileWatcher::template("SCSS");
        assert!(file_type_matches(
            &w,
            Path::new("/project/style.scss"),
            "CSS"
        ));
        assert!(!file_type_matches(
            &w,
            Path::new("/project/style.css"),
            "CSS"
        ));
    }
    #[test]
    fn captured_macros_keep_one_argv_for_paths_with_spaces() {
        let w = FileWatcher::template("SCSS");
        let ctx = external_tools::MacroContext {
            file: Some(PathBuf::from("/project with spaces/styles/main.scss")),
            project_root: Some(PathBuf::from("/project with spaces")),
            ..Default::default()
        };
        let inv = external_tools::build_invocation(
            &w.program,
            &w.arguments,
            &w.working_dir,
            &ctx,
            |_| false,
        )
        .unwrap();
        assert_eq!(
            inv.args,
            vec![
                "/project with spaces/styles/main.scss",
                "/project with spaces/styles/main.css"
            ]
        );
    }
    #[test]
    fn same_name_global_and_project_have_distinct_coalescing_keys() {
        let mut w = FileWatcher::template("Black");
        let global = key(&w, Path::new("/p/a.py"));
        w.level = Level::Project;
        assert_ne!(global, key(&w, Path::new("/p/a.py")));
    }
    fn queue_fixture() -> (tempfile::TempDir, IdeApp, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source with spaces.py");
        std::fs::write(&path, "first = 1\n").unwrap();
        let mut settings = Settings::default();
        // Execution admission checks the canonical trust store and captured
        // directory identity, not merely the cached safe-mode UI flag.
        assert!(settings.trust_project(dir.path()));
        let mut app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        assert!(!app.project_safe_mode());
        let mut watcher = FileWatcher::template("Black");
        watcher.scope = rustpy_settings::scopes::ScopeSelection::Predefined(
            rustpy_settings::scopes::PredefinedScope::AllPlaces,
        );
        watcher.program = "nonexistent-test-watcher-do-not-spawn".into();
        app.settings.file_watchers.push(watcher);
        (dir, app, path)
    }
    #[test]
    fn saved_project_file_before_index_publication_queues_without_admitting_exclusions() {
        let (dir, mut app, path) = queue_fixture();
        app.settings.file_watchers[0].scope = rustpy_settings::scopes::ScopeSelection::Predefined(
            rustpy_settings::scopes::PredefinedScope::Project,
        );
        app.settings.file_watchers[0].debounce_ms = 0;
        app.project_index.files.clear();
        assert!(!app.capture_scope_context().is_project(&path));
        app.queue_file_watchers(&path, false);
        assert_eq!(app.file_watcher_pending_labels().len(), 1);
        assert_eq!(
            app.watcher_runtime.pending.values().next().unwrap().path,
            path
        );
        app.file_watchers_tick(&egui::Context::default());
        assert!(app.file_watcher_pending_labels().is_empty());
        assert_eq!(
            app.run_panel.sessions.len(),
            1,
            "dispatch must revalidate the same exact project file even before index publication"
        );
        assert!(app.run_panel.sessions[0]
            .command_line
            .contains("nonexistent-test-watcher-do-not-spawn"));
        assert!(!app.run_panel.sessions[0].is_running());
        app.stop_file_watchers();
        let dispatched = app.run_panel.sessions.len();

        let excluded = dir.path().join("excluded");
        std::fs::create_dir(&excluded).unwrap();
        let excluded_file = excluded.join("excluded.py");
        std::fs::write(&excluded_file, "must_not_run = True\n").unwrap();
        app.project.as_mut().unwrap().tree.excluded.insert(excluded);
        app.queue_file_watchers(&excluded_file, false);
        assert!(app.file_watcher_pending_labels().is_empty());
        let ignored = dir.path().join("ignored.py");
        std::fs::write(&ignored, "must_not_run = True\n").unwrap();
        app.settings.editor.preferences.ignored_patterns = "ignored.py".into();
        app.queue_file_watchers(&ignored, false);
        assert!(app.file_watcher_pending_labels().is_empty());
        assert_eq!(app.run_panel.sessions.len(), dispatched);
    }
    #[test]
    fn repeated_notifications_coalesce_and_new_source_replaces_capture_without_spawning() {
        let (_dir, mut app, path) = queue_fixture();
        app.queue_file_watchers(&path, false);
        app.queue_file_watchers(&path, false);
        assert_eq!(app.watcher_runtime.pending.len(), 1);
        let original = app.watcher_runtime.pending.values().next().unwrap().source;
        assert_eq!(
            app.watcher_runtime
                .pending
                .values()
                .next()
                .unwrap()
                .invocation
                .args
                .last(),
            Some(&path.to_string_lossy().into_owned())
        );
        std::fs::write(&path, "second = 2\n").unwrap();
        app.queue_file_watchers(&path, false);
        assert_eq!(app.watcher_runtime.pending.len(), 1);
        assert_ne!(
            app.watcher_runtime.pending.values().next().unwrap().source,
            original
        );
        assert!(app.run_panel.sessions.is_empty());
    }
    #[test]
    fn external_opt_in_safe_mode_and_project_cancellation_guard_dispatch() {
        let (_dir, mut app, path) = queue_fixture();
        app.queue_file_watchers(&path, true);
        assert!(app.watcher_runtime.pending.is_empty());
        app.queue_file_watchers(&path, false);
        assert_eq!(app.watcher_runtime.pending.len(), 1);
        app.cancel_file_watchers();
        assert!(app.watcher_runtime.pending.is_empty());
        app.safe_mode = true;
        app.queue_file_watchers(&path, false);
        assert!(app.watcher_runtime.pending.is_empty());
        assert!(app.run_panel.sessions.is_empty());
    }
    #[test]
    fn revoked_project_trust_cancels_captured_watchers_despite_cached_ui_flag() {
        let (_dir, mut app, path) = queue_fixture();
        app.queue_file_watchers(&path, false);
        assert_eq!(app.watcher_runtime.pending.len(), 1);
        app.settings.trusted_projects.clear();
        assert!(!app.safe_mode, "cached UI admission has not refreshed");
        assert!(app.project_safe_mode());
        app.file_watchers_tick(&egui::Context::default());
        assert!(app.watcher_runtime.pending.is_empty());
        app.queue_file_watchers(&path, false);
        assert!(app.watcher_runtime.pending.is_empty());
        assert!(app.run_panel.sessions.is_empty());
    }
    #[test]
    fn project_level_round_trip_is_separate_from_global_and_shared_scopes_restrict_capture() {
        let (_dir, mut app, path) = queue_fixture();
        app.load_file_watchers();
        let mut watcher = app.settings.file_watchers[0].clone();
        watcher.name = "Project compiler".into();
        watcher.level = Level::Project;
        watcher.scope =
            rustpy_settings::scopes::ScopeSelection::Custom("file:*never-match*".into());
        app.watcher_runtime.project.push(watcher.clone());
        app.save_project_watchers().unwrap();
        app.cancel_file_watchers();
        app.load_file_watchers();
        assert_eq!(app.watcher_runtime.project, vec![watcher]);
        assert_eq!(app.settings.file_watchers.len(), 1);
        app.queue_file_watchers(&path, false);
        assert_eq!(app.watcher_runtime.pending.len(), 1);
        assert_eq!(
            app.watcher_runtime
                .pending
                .values()
                .next()
                .unwrap()
                .watcher
                .level,
            Level::Global
        );
    }
}
