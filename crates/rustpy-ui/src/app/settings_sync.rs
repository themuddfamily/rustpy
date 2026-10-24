//! Explicit Settings Sync / Settings Repository operations. No background fetch/push.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_core::tasks::{TaskError, TaskHandle, TaskManager};
use rustpy_settings::settings_sync::{
    self as sync, Choice, Plugin, Preferences, Preview, Snapshot, Storage,
};
#[derive(Clone)]
struct Captured {
    origin: PathBuf,
    config: PathBuf,
    repository: PathBuf,
    file: PathBuf,
    base: PathBuf,
    preferences: Preferences,
}
impl Captured {
    fn new(origin: PathBuf, preferences: Preferences) -> Result<Self, String> {
        let path = PathBuf::from(preferences.path.trim());
        if path.as_os_str().is_empty() {
            return Err("Choose a folder, settings file or dedicated Git checkout".into());
        }
        let path = if path.is_absolute() {
            path
        } else {
            origin.join(path)
        };
        let repository = if preferences.storage == Storage::File {
            path.parent().unwrap_or(&origin).to_path_buf()
        } else {
            path.clone()
        };
        let file = if preferences.storage == Storage::File {
            path
        } else {
            repository.join("rustpy-settings.json")
        };
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        file.hash(&mut hasher);
        preferences.remote.hash(&mut hasher);
        preferences.branch.hash(&mut hasher);
        let base = Settings::system_dir()
            .join("settings-sync/baselines")
            .join(format!("{:016x}.json", hasher.finish()));
        Ok(Self {
            origin,
            config: Settings::config_dir(),
            repository,
            file,
            base,
            preferences,
        })
    }
}
struct PendingPreview {
    target: Captured,
    preview: Preview,
    remote: Snapshot,
}
impl PendingPreview {
    fn publish_snapshot(&self) -> anyhow::Result<Snapshot> {
        let merged = self.preview.merged()?;
        let mut output = self.remote.clone();
        let selected = sync::selected(&output, &self.target.preferences.selection);
        for key in selected.entries.keys() {
            output.entries.remove(key);
        }
        output.entries.extend(merged.entries);
        if self.target.preferences.selection.plugins {
            output.plugins = merged.plugins;
        }
        output.validate()?;
        Ok(output)
    }
}
enum Work {
    PreparedCommit(PendingPublish, bool),
    Preview(PendingPreview),
    Applied(Snapshot, Captured, Snapshot, Vec<String>, Vec<Plugin>),
    Written(Captured, Snapshot, String, bool),
}
#[derive(Clone)]
enum Stage {
    Init(Captured),
    Remote(Captured),
    Clone(Captured),
    Fetch(Captured),
    Read(Captured),
    PublishFetch(PendingPublish),
    PublishRead(PendingPublish),
    Status(PendingPublish),
    LocalFiles(PendingPublish),
    RemoteFiles(PendingPublish),
    InitialStatus(PendingPublish),
    InitialFiles(PendingPublish),
    RetryStatus(PendingPublish),
    Merge(PendingPublish),
    Add(PendingPublish),
    Commit(PendingPublish),
    Push(Captured, Snapshot),
}
#[derive(Clone)]
struct PendingPublish {
    target: Captured,
    snapshot: Snapshot,
    remote: Snapshot,
}
#[derive(Default)]
pub(super) struct State {
    open: bool,
    preferences: Option<Preferences>,
    origin: Option<PathBuf>,
    git: crate::git_jobs::GitJobs,
    stage: Option<Stage>,
    worker: Option<TaskHandle<Work>>,
    preview: Option<PendingPreview>,
    publish: Option<PendingPublish>,
    message: String,
    export: String,
    import: String,
    missing: Vec<Plugin>,
    stop_requested: bool,
}
impl Drop for State {
    fn drop(&mut self) {
        self.git.cancel();
        if let Some(worker) = &self.worker {
            worker.cancel();
        }
    }
}
impl IdeApp {
    pub(super) fn open_settings_sync(&mut self, repository: bool) {
        if matches!(self.dialog, Dialog::Settings) {
            self.set_status("Close the Settings dialog before synchronizing settings");
            return;
        }
        let origin = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| Settings::config_dir()));
        self.settings_sync.origin = Some(origin.clone());
        let preferences = self
            .settings_sync
            .preferences
            .get_or_insert_with(Preferences::load);
        if repository {
            preferences.storage = Storage::Git;
        }
        self.settings_sync.export = origin
            .join("rustpy-settings-sync.json")
            .to_string_lossy()
            .into_owned();
        self.settings_sync.open = true;
    }
    pub(crate) fn settings_sync_pending(&self) -> Vec<(u64, String, String)> {
        let mut jobs = Vec::new();
        if let Some(running) = &self.settings_sync.git.running {
            jobs.push((
                running.id * 2 + 1,
                "Settings Repository".into(),
                format!("{}:{}", running.root.display(), running.job.command_line),
            ));
        }
        if let Some(worker) = &self.settings_sync.worker {
            jobs.push((
                worker.id() * 2,
                "Settings Sync".into(),
                format!("worker:{}", worker.id()),
            ));
        }
        jobs
    }
    pub(crate) fn stop_settings_sync(&mut self, id: u64) -> bool {
        if let Some(job) = &self.settings_sync.git.running {
            if id == job.id * 2 + 1 {
                self.settings_sync.stop_requested = true;
                self.settings_sync.git.cancel();
                return true;
            }
        }
        if let Some(worker) = &self.settings_sync.worker {
            if id == worker.id() * 2 {
                self.settings_sync.stop_requested = true;
                worker.cancel();
                return true;
            }
        }
        false
    }
    pub(super) fn stop_all_settings_sync(&mut self) {
        for (id, _, _) in self.settings_sync_pending() {
            self.stop_settings_sync(id);
        }
    }
    pub(crate) fn settings_sync_busy(&self) -> bool {
        self.settings_sync.worker.is_some() || self.settings_sync.git.is_busy()
    }
    pub(crate) fn poll_settings_sync_retirement(&mut self, ctx: &egui::Context) {
        self.settings_sync_tick(ctx, false);
    }
    pub(super) fn settings_sync_tick(&mut self, ctx: &egui::Context, draw: bool) {
        let _ = self.settings_sync.git.poll_prompts();
        if draw {
            self.settings_sync.git.prompt_window(ctx, &self.theme);
        }
        if let Some(finished) = self.settings_sync.git.poll() {
            let stage = self.settings_sync.stage.take();
            if finished.stopped || self.settings_sync.stop_requested {
                self.settings_sync.stop_requested = false;
                self.settings_sync.message = "Settings repository operation cancelled".into();
            } else if let Some(stage) = stage {
                self.settings_sync_git_finished(stage, finished.success, finished.output);
            }
        }
        let result = self
            .settings_sync
            .worker
            .as_mut()
            .and_then(|worker| worker.try_result());
        if let Some(result) = result {
            self.settings_sync.worker = None;
            let cancelled = std::mem::take(&mut self.settings_sync.stop_requested);
            match result {
                Ok(Work::PreparedCommit(_, _)) | Ok(Work::Preview(_)) if cancelled => {
                    self.settings_sync.message =
                        "Settings Sync cancelled; no next Git operation was started".into()
                }
                Ok(Work::PreparedCommit(publish, unchanged)) => {
                    let target = publish.target.clone();
                    if unchanged {
                        self.sync_git(
                            &target,
                            Stage::Push(target.clone(), publish.snapshot),
                            vec![
                                "push".into(),
                                target.preferences.remote.clone(),
                                format!("HEAD:refs/heads/{}", target.preferences.branch),
                            ],
                            target.repository.clone(),
                        );
                    } else {
                        self.sync_git(
                            &target,
                            Stage::Add(publish),
                            vec!["add".into(), "--".into(), "rustpy-settings.json".into()],
                            target.repository.clone(),
                        );
                    }
                }
                Ok(Work::Preview(preview)) => {
                    self.settings_sync.message = format!(
                        "{} differing entries; resolve conflicts before applying or publishing",
                        preview.preview.changes.len()
                    );
                    self.settings_sync.preview = Some(preview);
                }
                Ok(Work::Applied(snapshot, target, baseline, files, previous_plugins)) => {
                    self.settings_sync.missing.clear();
                    let mut plugin_errors = Vec::new();
                    crate::plugin_host::update(|registry| {
                        for previous in &previous_plugins {
                            if !snapshot
                                .plugins
                                .iter()
                                .any(|plugin| plugin.id == previous.id)
                                && registry.get(&previous.id).is_some()
                            {
                                if let Err(error) = registry.set_enabled(&previous.id, false) {
                                    plugin_errors.push(error.to_string());
                                }
                            }
                        }
                        for plugin in &snapshot.plugins {
                            if registry.get(&plugin.id).is_some() {
                                if registry
                                    .get(&plugin.id)
                                    .is_some_and(|entry| entry.manifest.version != plugin.version)
                                {
                                    self.settings_sync.missing.push(plugin.clone());
                                }
                                if let Err(error) = registry.set_enabled(&plugin.id, plugin.enabled)
                                {
                                    plugin_errors.push(error.to_string());
                                }
                            } else {
                                self.settings_sync.missing.push(plugin.clone());
                            }
                        }
                    });
                    if let Err(error) = baseline.write(&target.base) {
                        self.settings_sync.message = format!(
                            "Settings applied, but baseline could not be recorded: {error}"
                        );
                    } else {
                        self.settings_sync.message=format!("Applied selected settings and {} text files. Machine-local settings preserved.",files.len());
                    }
                    let mut runtime = self.settings.clone();
                    self.reload_settings(ctx);
                    sync::copy_selected_preferences(
                        &mut runtime,
                        &self.settings,
                        &target.preferences.selection,
                    );
                    self.settings = runtime;
                    if let Some(snapshot) = &mut self.settings_ui.snapshot {
                        sync::copy_selected_preferences(
                            &mut snapshot.settings,
                            &self.settings,
                            &target.preferences.selection,
                        );
                    }
                    self.netsec_settings_reloaded();
                    if !plugin_errors.is_empty() {
                        self.settings_sync.message.push_str(&format!(
                            "\nPlugin choices could not all be applied: {}",
                            plugin_errors.join("; ")
                        ));
                    }
                }
                Ok(Work::Written(target, snapshot, label, record)) => {
                    self.settings_sync.message = label;
                    if let Err(error) = if record {
                        snapshot.write(&target.base)
                    } else {
                        Ok(())
                    } {
                        self.settings_sync.message = error.to_string();
                    }
                }
                Err(error) => self.settings_sync.message = error.to_string(),
            }
        }
        if self.settings_sync_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if draw && self.settings_sync.open {
            self.settings_sync_window(ctx);
        }
    }
    fn sync_plugins(&self) -> Vec<Plugin> {
        crate::plugin_host::registry()
            .plugins()
            .iter()
            .map(|plugin| Plugin {
                id: plugin.id.clone(),
                version: plugin.manifest.version.clone(),
                enabled: plugin.enabled,
            })
            .collect()
    }
    fn sync_preview(&mut self, target: Captured, remote: Snapshot) {
        if self.settings_sync_busy() {
            return;
        }
        let settings = self.settings.clone();
        let plugins = self.sync_plugins();
        self.settings_sync.worker = Some(TaskManager::global().spawn(
            "Preview Settings Sync",
            move |progress| {
                progress.set_details(target.origin.display().to_string());
                let local = sync::capture(
                    &settings,
                    &target.config,
                    &target.preferences.selection,
                    plugins,
                    &|| progress.is_cancelled(),
                )
                .map_err(|e| TaskError::Failed(e.to_string()))?;
                let base = Snapshot::load(&target.base).unwrap_or_default();
                let selected_remote = sync::selected(&remote, &target.preferences.selection);
                let preview = Preview::new(
                    &sync::selected(&base, &target.preferences.selection),
                    local,
                    selected_remote,
                )
                .map_err(|e| TaskError::Failed(e.to_string()))?;
                Ok(Work::Preview(PendingPreview {
                    target,
                    preview,
                    remote,
                }))
            },
        ));
    }
    fn sync_read_local(&mut self, target: Captured, path: PathBuf) {
        if self.settings_sync_busy() {
            return;
        }
        let settings = self.settings.clone();
        let plugins = self.sync_plugins();
        self.settings_sync.worker = Some(TaskManager::global().spawn(
            "Read Settings Sync source",
            move |progress| {
                progress.set_details(path.display().to_string());
                progress.check()?;
                let remote = read_snapshot(&path).map_err(TaskError::Failed)?;
                let local = sync::capture(
                    &settings,
                    &target.config,
                    &target.preferences.selection,
                    plugins,
                    &|| progress.is_cancelled(),
                )
                .map_err(|e| TaskError::Failed(e.to_string()))?;
                let base = Snapshot::load(&target.base).unwrap_or_default();
                let selected_remote = sync::selected(&remote, &target.preferences.selection);
                let preview = Preview::new(
                    &sync::selected(&base, &target.preferences.selection),
                    local,
                    selected_remote,
                )
                .map_err(|e| TaskError::Failed(e.to_string()))?;
                Ok(Work::Preview(PendingPreview {
                    target,
                    preview,
                    remote,
                }))
            },
        ));
    }
    fn sync_git(&mut self, target: &Captured, stage: Stage, args: Vec<String>, cwd: PathBuf) {
        if self.settings_sync.git.is_busy() {
            return;
        }
        let label = match &stage {
            Stage::Init(_) => "Initialize settings repository",
            Stage::Clone(_) => "Clone settings repository",
            Stage::Fetch(_) | Stage::PublishFetch(_) => "Fetch settings repository",
            Stage::Push(_, _) => "Push settings repository",
            _ => "Settings repository",
        };
        match self.settings_sync.git.start(
            crate::git_jobs::GitOp::BranchNetwork {
                root: cwd.clone(),
                label: label.into(),
                args: args.clone(),
            },
            &cwd,
            args,
        ) {
            Ok(()) => {
                self.settings_sync.stage = Some(stage);
                self.settings_sync.stop_requested = false;
                self.settings_sync.message = format!("{label}: {}", target.repository.display());
            }
            Err(error) => self.settings_sync.message = error,
        }
    }
    fn settings_sync_git_finished(&mut self, stage: Stage, success: bool, output: String) {
        if !success {
            self.settings_sync.message = format!(
                "Settings repository command failed. Local settings were not replaced.\n{output}"
            );
            return;
        }
        match stage{
  Stage::Clone(target)=>self.sync_preview_file_after_git(target),
  Stage::Init(target)=>{let args=vec!["remote".into(),"add".into(),"origin".into(),target.preferences.remote.clone()];self.sync_git(&target,Stage::Remote(target.clone()),args,target.repository.clone());},
  Stage::Remote(target)=>self.sync_preview(target,Snapshot::default()),
  Stage::Fetch(target)=>{let args=vec!["show".into(),"FETCH_HEAD:rustpy-settings.json".into()];self.sync_git(&target,Stage::Read(target.clone()),args,target.repository.clone());},
  Stage::Read(target)=>match Snapshot::parse(&output){Ok(snapshot)=>self.sync_preview(target,snapshot),Err(error)=>self.settings_sync.message=format!("The remote settings snapshot is invalid: {error}")},
  Stage::PublishFetch(publish)=>{let target=publish.target.clone();self.sync_git(&target,Stage::PublishRead(publish),vec!["show".into(),"FETCH_HEAD:rustpy-settings.json".into()],target.repository.clone());},
  Stage::PublishRead(publish)=>match Snapshot::parse(&output){Ok(remote)if equivalent(&remote,&publish.remote)=>{let target=publish.target.clone();self.sync_git(&target,Stage::Status(publish),vec!["status".into(),"--porcelain".into()],target.repository.clone());},Ok(_)=>self.settings_sync.message="Remote settings changed after the preview. Fetch and resolve the new preview before pushing.".into(),Err(error)=>self.settings_sync.message=error.to_string()},
  Stage::Status(publish)|Stage::InitialStatus(publish)=>{if !output.trim().is_empty(){self.settings_sync.message="The selected settings checkout has uncommitted files. Commit or discard those changes in Git before publishing; Settings Sync will not overwrite them.".into();return;}let initial=publish.remote.entries.is_empty()&&!publish.target.file.exists();let target=publish.target.clone();let stage=if initial{Stage::InitialFiles(publish)}else{Stage::LocalFiles(publish)};self.sync_git(&target,stage,vec!["ls-files".into()],target.repository.clone());},
  Stage::LocalFiles(publish)|Stage::InitialFiles(publish)=>{if !dedicated_files(&output){self.settings_sync.message="Choose a dedicated settings repository; this checkout tracks unrelated files.".into();return;}
if publish.remote.entries.is_empty()&&!publish.target.file.exists(){self.sync_write_commit(publish);}else{let target=publish.target.clone();self.sync_git(&target,Stage::RemoteFiles(publish),vec!["ls-tree".into(),"-r".into(),"--name-only".into(),"FETCH_HEAD".into()],target.repository.clone());}},
  Stage::RemoteFiles(publish)=>{if !dedicated_files(&output){self.settings_sync.message="The remote contains unrelated files; use a dedicated settings branch/repository.".into();return;}let target=publish.target.clone();self.sync_git(&target,Stage::Merge(publish),vec!["merge".into(),"--ff-only".into(),"FETCH_HEAD".into()],target.repository.clone());},
  Stage::RetryStatus(publish)=>{let target=publish.target.clone();if output.trim().is_empty(){self.sync_git(&target,Stage::Push(target.clone(),publish.snapshot),vec!["push".into(),target.preferences.remote.clone(),format!("HEAD:refs/heads/{}",target.preferences.branch)],target.repository.clone());}else if output.lines().all(|line|line.get(3..)==Some("rustpy-settings.json")){self.sync_git(&target,Stage::Add(publish),vec!["add".into(),"--".into(),"rustpy-settings.json".into()],target.repository.clone());}else{self.settings_sync.message="Unrelated checkout changes prevent retrying Settings Sync".into();}},
  Stage::Merge(publish)=>self.sync_write_commit(publish),
  Stage::Add(publish)=>{let target=publish.target.clone();self.sync_git(&target,Stage::Commit(publish),vec!["commit".into(),"--only".into(),"-m".into(),"Sync IDE settings".into(),"--".into(),"rustpy-settings.json".into()],target.repository.clone());},
  Stage::Commit(publish)=>{let target=publish.target.clone();self.sync_git(&target,Stage::Push(target.clone(),publish.snapshot),vec!["push".into(),target.preferences.remote.clone(),format!("HEAD:refs/heads/{}",target.preferences.branch)],target.repository.clone());},
  Stage::Push(target,snapshot)=>{self.settings_sync.publish=None;self.settings_sync.message="Settings pushed successfully".into();if let Err(error)=snapshot.write(&target.base){self.settings_sync.message=error.to_string();}}
 }
    }
    fn sync_preview_file_after_git(&mut self, target: Captured) {
        let file = target.file.clone();
        self.sync_read_local(target, file);
    }
    fn sync_write_commit(&mut self, publish: PendingPublish) {
        self.settings_sync.worker = Some(TaskManager::global().spawn(
            "Prepare Settings Repository commit",
            move |progress| {
                progress.set_details(publish.target.repository.display().to_string());
                progress.check()?;
                let current = read_snapshot(&publish.target.file).map_err(TaskError::Failed)?;
                let unchanged = equivalent(&current, &publish.snapshot);
                if !unchanged {
                    publish
                        .snapshot
                        .write(&publish.target.file)
                        .map_err(|error| TaskError::Failed(error.to_string()))?;
                }
                progress.check()?;
                Ok(Work::PreparedCommit(publish, unchanged))
            },
        ));
    }
    fn settings_sync_window(&mut self, ctx: &egui::Context) {
        let mut state = std::mem::take(&mut self.settings_sync);
        let mut open = true;
        let busy = state.worker.is_some() || state.git.is_busy();
        let preferences = state.preferences.get_or_insert_with(Preferences::load);
        let mut action = None;
        let mut install = None;
        egui::Window::new("Settings Sync / Settings Repository").open(&mut open).default_size([960.0,720.0]).show_modeless(ctx,|ui|{egui::ScrollArea::vertical().show(ui,|ui|{ui.label(format!("Paths are captured relative to {}",state.origin.as_ref().map(|p|p.display().to_string()).unwrap_or_default()));ui.horizontal(|ui|{for(label,storage)in[("Shared folder",Storage::Folder),("Settings file",Storage::File),("Git repository",Storage::Git)]{ui.selectable_value(&mut preferences.storage,storage,crate::i18n::tr_static(label));}});ui.horizontal(|ui|{ui.label("Selected path / checkout");ui.text_edit_singleline(&mut preferences.path);});if preferences.storage==Storage::Git{for(label,value)in[("Remote URL (credentials use the existing Git askpass)",&mut preferences.remote),("Branch",&mut preferences.branch)]{ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(value);});}}ui.horizontal_wrapped(|ui|{let selection=&mut preferences.selection;for(label,value)in[("UI / themes",&mut selection.appearance),("Editor / code style",&mut selection.editor),("Keymaps",&mut selection.keymaps),("Templates",&mut selection.templates),("Inspections",&mut selection.inspections),("Plugin choices",&mut selection.plugins)]{ui.checkbox(value,crate::i18n::tr_static(label));}});ui.label("Passwords, proxy/auth data, trust/recent projects, session state, machine paths, interpreter wrappers, executable plugin binaries and local diagnostic history stay on this computer. Plugin installation remains an explicit Marketplace action.");ui.add_enabled_ui(!busy,|ui|{ui.horizontal_wrapped(|ui|{for label in ["Save configuration","Preview selected source","Preview local export"]{if ui.button(crate::i18n::tr_static(label)).clicked(){action=Some(label.to_owned());}}
if preferences.storage==Storage::Git{for label in ["Clone repository","Initialize repository","Fetch and preview","Commit and push preview","Retry interrupted publish"]{if ui.button(crate::i18n::tr_static(label)).clicked(){action=Some(label.to_owned());}}}});});ui.horizontal(|ui|{ui.label("Export file");ui.text_edit_singleline(&mut state.export);if ui.add_enabled(!busy,egui::Button::new("Export snapshot")).clicked(){action=Some("Export snapshot".into());}
if ui.add_enabled(!busy,egui::Button::new("Export portable bundle")).clicked(){action=Some("Export portable bundle".into());}});ui.horizontal(|ui|{ui.label("Import file / bundle");ui.text_edit_singleline(&mut state.import);if ui.add_enabled(!busy,egui::Button::new("Preview import")).clicked(){action=Some("Preview import".into());}});
 if let Some(preview)=&mut state.preview{ui.separator();ui.strong(format!("Captured target: {}",preview.target.file.display()));ui.label("Each entry shows the current local and incoming value. Select a side for conflicts and deletions; nothing is imported or pushed by previewing.");for change in &mut preview.preview.changes{ui.collapsing(format!("{}{}",if change.conflict{"CONFLICT: "}else{""},change.key),|ui|{ui.horizontal(|ui|{if ui.selectable_label(change.resolved&&change.choice==Choice::Local,"Keep local").clicked(){change.choice=Choice::Local;change.resolved=true;}
if ui.selectable_label(change.resolved&&change.choice==Choice::Remote,"Use incoming").clicked(){change.choice=Choice::Remote;change.resolved=true;}});ui.columns(2,|columns|{columns[0].label("Local");columns[0].monospace(pretty(&change.local));columns[1].label("Incoming");columns[1].monospace(pretty(&change.remote));});});}ui.horizontal(|ui|{if ui.add_enabled(!busy,egui::Button::new("Apply preview to this IDE")).clicked(){action=Some("Apply preview".into());}
if preview.target.preferences.storage!=Storage::Git&&ui.add_enabled(!busy,egui::Button::new("Write merged preview to selected source")).clicked(){action=Some("Write preview".into());}});}
 for plugin in &state.missing{ui.horizontal(|ui|{ui.label(format!("Plugin installation/update needed: {} ({})",plugin.id,plugin.version));if ui.button("Find and install…").clicked(){install=Some(plugin.id.clone());}});}if busy{ui.horizontal(|ui|{crate::a11y::spinner(ui);if ui.button("Cancel active sync").clicked(){state.stop_requested=true;state.git.cancel();if let Some(worker)=&state.worker{worker.cancel();}}});}ui.label(&state.message);state.git.console_ui(ui,&self.theme);});});
        state.open = open;
        self.settings_sync = state;
        if let Some(query) = install {
            self.data_find_plugin(query);
        }
        if let Some(action) = action {
            self.perform_settings_sync(&action);
        }
    }
    fn perform_settings_sync(&mut self, action: &str) {
        let origin = self
            .settings_sync
            .origin
            .clone()
            .unwrap_or_else(Settings::config_dir);
        let preferences = self.settings_sync.preferences.clone().unwrap_or_default();
        let target = match Captured::new(origin, preferences) {
            Ok(target) => target,
            Err(error) => {
                self.settings_sync.message = error;
                return;
            }
        };
        if self.settings_sync_busy() {
            return;
        }
        if target.preferences.storage == Storage::Git {
            if let Err(error) = validate_git(&target) {
                self.settings_sync.message = error;
                return;
            }
        }
        match action {
            "Save configuration" => {
                self.settings_sync.message = target
                    .preferences
                    .save()
                    .map(|_| "Sync configuration saved locally".into())
                    .unwrap_or_else(|e| e.to_string());
            }
            "Preview selected source" => {
                let file = target.file.clone();
                self.sync_read_local(target, file);
            }
            "Preview local export" => self.sync_preview(target, Snapshot::default()),
            "Preview import" => {
                let path = resolve(&target.origin, &self.settings_sync.import);
                if !path.is_file() {
                    self.settings_sync.message =
                        "Choose an existing settings snapshot or portable bundle to import".into();
                    return;
                }
                self.sync_read_local(target, path);
            }
            "Fetch and preview" | "Clone repository" | "Initialize repository" => {
                if let Err(error) = validate_git(&target) {
                    self.settings_sync.message = error;
                    return;
                }
                if action == "Clone repository" {
                    let parent = target
                        .repository
                        .parent()
                        .unwrap_or(&target.origin)
                        .to_path_buf();
                    self.sync_git(
                        &target,
                        Stage::Clone(target.clone()),
                        vec![
                            "clone".into(),
                            "--single-branch".into(),
                            "--branch".into(),
                            target.preferences.branch.clone(),
                            "--".into(),
                            target.preferences.remote.clone(),
                            target.repository.to_string_lossy().into_owned(),
                        ],
                        parent,
                    );
                } else if action == "Initialize repository" {
                    if target.repository.is_dir()
                        && std::fs::read_dir(&target.repository)
                            .is_ok_and(|mut entries| entries.next().is_some())
                    {
                        self.settings_sync.message="Initialize Settings Repository in an empty folder; existing projects are not changed.".into();
                        return;
                    }
                    if target.repository.join(".git").exists() {
                        self.settings_sync.message =
                            "Repository already exists; choose Fetch and preview".into();
                        return;
                    }
                    if let Err(error) = std::fs::create_dir_all(&target.repository) {
                        self.settings_sync.message = error.to_string();
                        return;
                    }
                    self.sync_git(
                        &target,
                        Stage::Init(target.clone()),
                        vec![
                            "init".into(),
                            format!("--initial-branch={}", target.preferences.branch),
                        ],
                        target.repository.clone(),
                    );
                } else {
                    self.sync_git(
                        &target,
                        Stage::Fetch(target.clone()),
                        vec![
                            "fetch".into(),
                            "--prune".into(),
                            target.preferences.remote.clone(),
                            target.preferences.branch.clone(),
                        ],
                        target.repository.clone(),
                    );
                }
            }
            "Apply preview" => {
                let Some(preview) = self.settings_sync.preview.as_ref() else {
                    return;
                };
                let target = preview.target.clone();
                let expected = preview.preview.local.clone();
                let baseline = preview.remote.clone();
                let snapshot = match preview.preview.merged() {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.settings_sync.message = error.to_string();
                        return;
                    }
                };
                self.settings_sync.worker = Some(TaskManager::global().spawn(
                    "Apply selected Settings Sync preview",
                    move |progress| {
                        progress.set_details(target.config.display().to_string());
                        let files = sync::apply(&target.config, &snapshot, &expected, &|| {
                            progress.is_cancelled()
                        })
                        .map_err(|e| TaskError::Failed(e.to_string()))?;
                        Ok(Work::Applied(
                            snapshot,
                            target,
                            baseline,
                            files,
                            expected.plugins,
                        ))
                    },
                ));
            }
            "Commit and push preview" => {
                let Some(preview) = self.settings_sync.preview.as_ref() else {
                    self.settings_sync.message = "Fetch/preview settings before publishing".into();
                    return;
                };
                let target = preview.target.clone();
                if let Err(error) = validate_git(&target) {
                    self.settings_sync.message = error;
                    return;
                }
                let snapshot = match preview.publish_snapshot() {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.settings_sync.message = error.to_string();
                        return;
                    }
                };
                let publish = PendingPublish {
                    target: target.clone(),
                    snapshot,
                    remote: preview.remote.clone(),
                };
                self.settings_sync.publish = Some(publish.clone());
                if publish.remote.entries.is_empty() && !target.file.exists() {
                    self.sync_git(
                        &target,
                        Stage::InitialStatus(publish),
                        vec!["status".into(), "--porcelain".into()],
                        target.repository.clone(),
                    );
                } else {
                    self.sync_git(
                        &target,
                        Stage::PublishFetch(publish),
                        vec![
                            "fetch".into(),
                            target.preferences.remote.clone(),
                            target.preferences.branch.clone(),
                        ],
                        target.repository.clone(),
                    );
                }
            }
            "Retry interrupted publish" => {
                if let Some(publish) = self.settings_sync.publish.clone() {
                    let target = publish.target.clone();
                    self.sync_git(
                        &target,
                        Stage::RetryStatus(publish),
                        vec!["status".into(), "--porcelain".into()],
                        target.repository.clone(),
                    );
                } else {
                    self.settings_sync.message = "No interrupted publish to retry".into();
                }
            }
            "Write preview" | "Export snapshot" | "Export portable bundle" => {
                let Some(preview) = self.settings_sync.preview.as_ref() else {
                    self.settings_sync.message = "Preview local export or fetch first".into();
                    return;
                };
                let target = preview.target.clone();
                let snapshot = match if action == "Write preview" {
                    preview.publish_snapshot()
                } else {
                    preview.preview.merged()
                } {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.settings_sync.message = error.to_string();
                        return;
                    }
                };
                let output = if action == "Write preview" {
                    target.file.clone()
                } else {
                    resolve(&target.origin, &self.settings_sync.export)
                };
                let portable = action == "Export portable bundle";
                let record = action == "Write preview";
                let expected_remote = preview.remote.clone();
                self.settings_sync.worker = Some(TaskManager::global().spawn(
                    "Write Settings Sync export",
                    move |progress| {
                        progress.set_details(format!(
                            "{} → {}",
                            target.origin.display(),
                            output.display()
                        ));
                        progress.check()?;
                        if record {
                            let current = read_snapshot(&target.file).map_err(TaskError::Failed)?;
                            if !equivalent(&current, &expected_remote) {
                                return Err(TaskError::Failed(
                                    "The shared settings changed after preview; preview again"
                                        .into(),
                                ));
                            }
                        }
                        if portable {
                            sync::write_bundle(&snapshot, &output)
                                .map_err(|error| TaskError::Failed(error.to_string()))?;
                        } else {
                            snapshot
                                .write(&output)
                                .map_err(|e| TaskError::Failed(e.to_string()))?;
                        }
                        Ok(Work::Written(
                            target,
                            snapshot,
                            format!("Wrote {}", output.display()),
                            record,
                        ))
                    },
                ));
            }
            _ => {}
        }
    }
}
fn resolve(root: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path.trim());
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}
fn read_snapshot(path: &Path) -> Result<Snapshot, String> {
    if !path.exists() {
        return Ok(Snapshot::default());
    }
    if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > 32 * 1024 * 1024 {
        return Err("Settings file exceeds 32 MiB".into());
    }
    sync::from_bundle(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn equivalent(left: &Snapshot, right: &Snapshot) -> bool {
    left.entries == right.entries && left.plugins == right.plugins
}
fn pretty(value: &Option<serde_json::Value>) -> String {
    value
        .as_ref()
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
        .unwrap_or_else(|| "(deleted / absent)".into())
}
fn validate_git(target: &Captured) -> Result<(), String> {
    let branch = &target.preferences.branch;
    if branch.is_empty()
        || branch.starts_with('-')
        || branch.contains("..")
        || branch.ends_with('/')
        || branch.ends_with('.')
        || branch.contains("//")
        || branch.split('/').any(|part| part.ends_with(".lock"))
        || !branch
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
    {
        return Err("Choose a valid branch name".into());
    }
    let remote = &target.preferences.remote;
    if remote.is_empty() || remote.starts_with('-') || remote.chars().any(char::is_control) {
        return Err("Enter a Git remote URL or path".into());
    }
    if let Ok(url) = url::Url::parse(remote) {
        if url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || matches!(url.scheme(), "http" | "https") && !url.username().is_empty()
        {
            return Err("Keep credentials out of the URL; use Git's credential/SSH prompt".into());
        }
    }
    Ok(())
}
fn dedicated_files(output: &str) -> bool {
    output.lines().all(|name| {
        let name = name.trim();
        name == "rustpy-settings.json"
            || name == ".gitignore"
            || name == ".rustpy-settings-repository"
            || name.starts_with("README") && !name.contains('/')
            || name.starts_with("LICENSE") && !name.contains('/')
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target() -> Captured {
        Captured {
            origin: "/chosen/project".into(),
            config: "/chosen/config".into(),
            repository: "/chosen/settings-repo".into(),
            file: "/chosen/settings-repo/rustpy-settings.json".into(),
            base: "/chosen/baseline.json".into(),
            preferences: Preferences {
                storage: Storage::Git,
                path: "settings-repo".into(),
                remote: "git@example.com:settings.git".into(),
                branch: "settings/main".into(),
                ..Default::default()
            },
        }
    }
    #[test]
    fn partial_selection_keeps_unselected_remote_preferences() {
        let mut target = target();
        target.preferences.selection.editor = false;
        let mut remote = Snapshot::default();
        remote.entries.insert(
            "settings/editor".into(),
            serde_json::json!({"font_size":18}),
        );
        let mut local = Snapshot::default();
        local
            .entries
            .insert("settings/keymap".into(), serde_json::json!("Emacs"));
        let preview = Preview::new(
            &Snapshot::default(),
            local,
            sync::selected(&remote, &target.preferences.selection),
        )
        .unwrap();
        let pending = PendingPreview {
            target,
            preview,
            remote,
        };
        let merged = pending.publish_snapshot().unwrap();
        assert_eq!(merged.entries["settings/editor"]["font_size"], 18);
        assert_eq!(merged.entries["settings/keymap"], "Emacs");
    }
    #[test]
    fn git_credentials_do_not_belong_in_persisted_urls() {
        let mut target = target();
        assert!(validate_git(&target).is_ok());
        target.preferences.remote = "https://user:password@example.com/settings.git".into();
        assert!(validate_git(&target).is_err());
        target.preferences.remote = "https://example.com/settings.git?access_token=secret".into();
        assert!(validate_git(&target).is_err());
    }
    #[test]
    fn foreign_checkouts_are_not_treated_as_settings_repositories() {
        assert!(dedicated_files("README.md\nrustpy-settings.json\n"));
        assert!(!dedicated_files("src/main.rs\nCargo.toml\n"));
        assert!(!dedicated_files("README/private.py\n"));
    }
    #[test]
    fn captured_authority_does_not_follow_later_form_changes() {
        let captured = target();
        let mut form = captured.preferences.clone();
        form.path = "another-folder".into();
        form.remote = "git@other.example:other.git".into();
        assert_eq!(captured.repository, PathBuf::from("/chosen/settings-repo"));
        assert_eq!(captured.preferences.remote, "git@example.com:settings.git");
    }
}

#[cfg(test)]
#[path = "settings_sync/git_execution_tests.rs"]
mod git_execution_tests;
