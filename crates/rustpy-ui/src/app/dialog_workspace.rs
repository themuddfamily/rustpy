//! Shared dialog policies and concrete Settings/maintenance consumers.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_core::{TaskHandle, TaskManager};
use rustpy_settings::dialogs::{self, Preferences};
use serde_json::Value;
#[derive(Default)]
pub(super) struct State {
    search_cache: std::cell::RefCell<std::collections::HashMap<(String, String), bool>>,
    pub prefs: Preferences,
    pub loaded: bool,
    pub options: bool,
    pub maintenance: bool,
    pub repair: bool,
    pub registry: bool,
    pub inspector: bool,
    pub defaults: bool,
    maintenance_root: Option<PathBuf>,
    maintenance_identity: Option<rustpy_settings::trust::DirectoryIdentity>,
    /// Pass in which the maintenance dialog opened: the Enter that chose the
    /// action (e.g. in Find Action) must not also confirm the dialog.
    maintenance_opened: u64,
    pub help: Option<String>,
    pub project_base: Option<rustpy_settings::CodeStyleSettings>,
    project_observed: Option<(PathBuf, Value)>,
    filesystem: bool,
    history: bool,
    shared: bool,
    git_log: bool,
    remember: bool,
    job: Option<TaskHandle<String>>,
    reload: bool,
    reload_ready: bool,
    result: String,
    restart: Option<(PathBuf, Vec<String>, PathBuf)>,
}
impl State {
    fn load(&mut self) {
        if !self.loaded {
            self.prefs = Preferences::load();
            self.loaded = true;
        }
    }
    pub(super) fn size(&mut self, key: &str, fallback: [f32; 2]) -> [f32; 2] {
        self.load();
        self.prefs.size(key, fallback)
    }
    pub(super) fn remember_size(&mut self, key: &str, size: [f32; 2]) {
        self.prefs.remember_size(key, size);
        let _ = self.prefs.save();
    }
}
pub(super) fn highlighted(text: &str, query: &str, color: Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let query = query.trim().to_lowercase();
    let lower = text.to_lowercase();
    if !query.is_empty() {
        if let Some(start) = lower.find(&query) {
            if text.is_char_boundary(start) && text.is_char_boundary(start + query.len()) {
                for (part, strong) in [
                    (&text[..start], false),
                    (&text[start..start + query.len()], true),
                    (&text[start + query.len()..], false),
                ] {
                    job.append(
                        part,
                        0.,
                        egui::TextFormat {
                            color: if strong { color } else { Color32::PLACEHOLDER },
                            background: if strong {
                                color.gamma_multiply(0.18)
                            } else {
                                Color32::TRANSPARENT
                            },
                            ..Default::default()
                        },
                    );
                }
                return job;
            }
        }
    }
    job.append(text, 0., egui::TextFormat::default());
    job
}
impl IdeApp {
    pub(super) fn dialog_policy_action(&mut self, action: Action, ctx: &egui::Context) -> bool {
        self.dialog_workspace.load();
        match action {
            Action::DialogPreferences => self.dialog_workspace.options = true,
            Action::NewProjectSettings => self.dialog_workspace.defaults = true,
            Action::RepairIde => {
                self.capture_dialog_maintenance();
                self.dialog_workspace.maintenance_opened = ctx.cumulative_pass_nr();
                self.dialog_workspace.maintenance = true;
                self.dialog_workspace.repair = true;
                self.dialog_workspace.filesystem = true;
                self.dialog_workspace.git_log = true;
            }
            Action::AdvancedRegistry => self.dialog_workspace.registry = true,
            Action::UiInspector => {
                if self.dialog_workspace.prefs.developer_mode {
                    self.dialog_workspace.inspector = true;
                } else {
                    self.set_status(
                        "Enable developer mode in Registry before opening the UI Inspector",
                    );
                }
            }
            Action::RestartIde => {
                match std::env::current_exe().and_then(|exe| Ok((exe, std::env::current_dir()?))) {
                    Ok((exe, cwd)) => {
                        let mut args = vec!["--new-window".into()];
                        if let Some(project) = &self.project {
                            args.push(project.root.to_string_lossy().into_owned());
                        }
                        self.dialog_workspace.restart = Some((exe, args, cwd));
                        self.request_exit();
                    }
                    Err(error) => self.set_status(error.to_string()),
                }
            }
            Action::InvalidateCaches => {
                if self.dialog_workspace.prefs.answer("rebuild-caches") == Some(true) {
                    self.invalidate_caches_now();
                } else {
                    self.capture_dialog_maintenance();
                    self.dialog_workspace.maintenance_opened = ctx.cumulative_pass_nr();
                    self.dialog_workspace.maintenance = true;
                    self.dialog_workspace.filesystem = true;
                }
            }
            _ => return false,
        }
        let _ = ctx;
        true
    }
    pub(super) fn pending_dialog_restart(&self) -> bool {
        self.dialog_workspace.restart.is_some()
    }
    fn capture_dialog_maintenance(&mut self) {
        self.dialog_workspace.maintenance_root =
            self.project.as_ref().map(|project| project.root.clone());
        self.dialog_workspace.maintenance_identity = self
            .dialog_workspace
            .maintenance_root
            .as_ref()
            .and_then(|root| rustpy_settings::trust::DirectoryIdentity::capture(root).ok());
        self.dialog_workspace.history = false;
        self.dialog_workspace.shared = false;
        self.dialog_workspace.remember = false;
    }
    pub(super) fn dialog_workspace_progress(&self, ui: &mut egui::Ui) {
        if let Some(job) = &self.dialog_workspace.job {
            let (text, details, fraction) = job.progress();
            ui.label(format!("{} · {text} · {details}", job.title()));
            crate::progress_ui::progress_bar(ui, fraction, ui.available_width(), &self.theme);
            if ui.button("Cancel background operation").clicked() {
                job.cancel();
            }
        }
        if !self.dialog_workspace.result.is_empty() {
            ui.label(&self.dialog_workspace.result);
        }
    }
    pub(super) fn dialog_restart_stamp(&self) -> String {
        format!("{:?}", self.dialog_workspace.restart)
    }
    pub(super) fn cancel_dialog_restart(&mut self) {
        self.dialog_workspace.restart = None;
    }
    pub(super) fn finish_dialog_restart(&mut self) {
        if !(self.exit_requested || self.coordinated_quit_committed) {
            return;
        }
        if let Some((exe, args, cwd)) = self.dialog_workspace.restart.take() {
            match rustpy_cli::instance::spawn_detached(&exe, &args, &cwd) {
                Ok(mut child) => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                Err(error) => {
                    log::error!("Cannot restart RustPy: {error}");
                    self.dialog_workspace.restart = Some((exe, args, cwd));
                }
            }
        }
    }
    pub(super) fn dialog_workspace_pending(&self) -> Option<(u64, String)> {
        self.dialog_workspace
            .job
            .as_ref()
            .map(|job| (job.id(), job.title().to_string()))
    }
    pub(super) fn cancel_dialog_workspace_job(&self) {
        if let Some(job) = &self.dialog_workspace.job {
            job.cancel();
        }
    }
    pub(super) fn poll_dialog_workspace_job(&mut self, ctx: &egui::Context) {
        self.poll_dialog_workspace_retirement(ctx);
        if self.dialog_workspace.reload_ready {
            self.reload_settings(ctx);
            self.dialog_workspace.reload_ready = false;
            self.dialog_workspace.reload = false;
            self.dialog_workspace.project_base = None;
            if let Some(root) = self.project.as_ref().map(|project| project.root.clone()) {
                let _ = self.settings.load_project_code_style(&root);
                let _ = self.settings.scopes.load_project(&root);
                self.load_dialog_project_preferences(&root);
            }
        }
    }
    /// Frozen quit IO: consume only already-issued worker results. Raw settings
    /// refresh prevents final persistence from reverting an accepted import;
    /// runtime application waits until ordinary UI resumes.
    pub(super) fn poll_dialog_workspace_retirement(&mut self, ctx: &egui::Context) {
        let result = self
            .dialog_workspace
            .job
            .as_mut()
            .and_then(TaskHandle::try_result);
        if let Some(result) = result {
            self.dialog_workspace.job = None;
            self.dialog_workspace.result = match result {
                Ok(message) => message,
                Err(error) => format!("{error:?}"),
            };
            if self.dialog_workspace.reload {
                self.dialog_workspace.prefs = Preferences::load();
                self.settings = Settings::load();
                self.dialog_workspace.reload_ready = true;
            }
        }
        if self.dialog_workspace.job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
    pub(super) fn start_settings_archive(
        &mut self,
        mode: u8,
        path: &str,
    ) -> Result<String, String> {
        if self.dialog_workspace.job.is_some() {
            return Err("Wait for or cancel the current settings operation".into());
        }
        let path = PathBuf::from(path.trim());
        if mode < 2 && (path.as_os_str().is_empty() || !path.is_absolute()) {
            return Err("Choose an absolute settings archive path".into());
        }
        let config = Settings::config_dir();
        let config = if config.is_absolute() {
            config
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(config)
        };
        let settings = self.settings.clone();
        self.dialog_workspace.reload = mode != 0;
        self.dialog_workspace.job = Some(
            TaskManager::global()
                .task("Manage IDE settings")
                .exclusive("settings-archive")
                .spawn(move |progress| {
                    let result = (|| -> Result<String, String> {
                        progress.set_text(path.display().to_string());
                        let cancel = || progress.is_cancelled();
                        if path
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
                        {
                            return if mode == 0 {
                                crate::settings_archive::export(&config, &path, &settings, &cancel)
                            } else {
                                crate::settings_archive::import(&config, &path, &cancel)
                            };
                        }
                        progress.check().map_err(|e| e.to_string())?;
                        match mode {
                            0 => rustpy_settings::bundle::export_bundle(&config, &path)
                                .map(|files| format!("Exported {} files", files.len())),
                            1 => rustpy_settings::bundle::import_bundle(&config, &path)
                                .map(|files| format!("Imported {} files", files.len())),
                            _ => {
                                rustpy_settings::bundle::reset_to_defaults(&config).map(|backup| {
                                    format!("Defaults restored; backup {}", backup.display())
                                })
                            }
                        }
                        .map_err(|e| e.to_string())
                    })();
                    progress.check()?;
                    result.map_err(rustpy_core::TaskError::Failed)
                }),
        );
        Ok("Running in Background Tasks; cancellation is available there".into())
    }
    pub(super) fn sync_dialog_project_preferences(&mut self) -> Result<(), String> {
        self.dialog_workspace.load();
        if self.dialog_workspace.prefs.sync_project_settings {
            if let Some(project) = &self.project {
                dialogs::save_project(&project.root, &self.settings).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    pub(super) fn load_dialog_project_preferences(&mut self, root: &Path) {
        self.dialog_workspace.project_base = Some(self.settings.code_style.clone());
        if let Err(error) = dialogs::load_project(root, &mut self.settings) {
            self.set_status(format!("Project preferences: {error}"));
        }
    }
    pub(super) fn restore_dialog_project_base(&mut self) {
        if let Some(base) = self.dialog_workspace.project_base.take() {
            self.settings.code_style = base;
        }
    }
    pub(super) fn seed_new_project_dialog_defaults(&mut self, root: &Path) {
        let result = (|| -> Result<(), String> {
            dialogs::seed_project(root).map_err(|e| e.to_string())?;
            let path = Settings::config_dir().join("new-project-run-templates.json");
            if path.is_file() && !rustpy_project::run_workflow::Templates::path(root).exists() {
                let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
                if bytes.len() > 4 * 1024 * 1024 {
                    return Err("New project run templates exceed 4 MiB".into());
                }
                let document: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                let mut value = document
                    .get("templates")
                    .cloned()
                    .unwrap_or_else(|| document.clone());
                if let Some(source) = document["source_root"].as_str() {
                    if source.is_empty() || document["version"] != 1 {
                        return Err("Unsupported new-project run template origin".into());
                    }
                    rebase_template_paths(&mut value, source, &root.to_string_lossy());
                }
                let templates: rustpy_project::run_workflow::Templates =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                templates.save(root)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.set_status(format!("New project defaults: {error}"));
        }
    }
    pub(super) fn validate_dialog_settings(&mut self) -> Result<(), String> {
        if let Some(path) = &self.settings.default_project_directory {
            self.settings.default_project_directory =
                Some(rustpy_settings::project_directory::validate(path)?);
        }
        for (label, value) in [
            ("Editor font size", self.settings.editor.font_size),
            ("IDE font size", self.settings.appearance.ui_font_size),
        ] {
            if !value.is_finite() || !(5.0..=128.0).contains(&value) {
                return Err(format!("{label} must be between 5 and 128 points"));
            }
        }
        if self.settings.code_style.quote_style != "preserve"
            && self.settings.code_style.quote_style != "single"
            && self.settings.code_style.quote_style != "double"
        {
            return Err("Code Style quote style must be preserve, single or double".into());
        }
        self.settings.path_variables.validate()?;
        Ok(())
    }
    /// Rebuilds file system, analysis and symbol caches without asking (the
    /// remembered "do not ask again" answer) and records the invalidation.
    pub(super) fn invalidate_caches_now(&mut self) {
        let analysis_replaced = self.rebuild_dialog_caches();
        record_caches_invalidated(analysis_replaced, &["filesystem"]);
    }
    /// Returns whether a project's analysis was replaced.
    fn rebuild_dialog_caches(&mut self) -> bool {
        self.vfs.clear_cached_snapshots();
        let root = self.project.as_ref().map(|project| project.root.clone());
        if let Some(root) = &root {
            if let Some(project) = &mut self.project {
                project.analysis = Arc::new(Analysis::for_project(root));
                project.tree.invalidate_all();
            }
            for tab in &mut self.tabs {
                tab.state.semantic_version = None;
                tab.state.synced_version = None;
            }
            self.apply_interpreter_paths();
            self.symbol_indexes.rebuild_project();
            self.symbol_indexes.rebuild_library();
            self.reindex();
            self.refresh_project_files();
        }
        self.set_status("Filesystem/analysis caches invalidated; indexes rebuilding");
        root.is_some()
    }
    pub(super) fn settings_page_reset(&mut self, defaults: bool, ctx: &egui::Context) {
        let target = if defaults {
            Settings::shipped_defaults()
        } else {
            match &self.settings_ui.snapshot {
                Some(snapshot) => snapshot.settings.clone(),
                None => return,
            }
        };
        let mut current = serde_json::to_value(&self.settings).unwrap_or_default();
        let source = serde_json::to_value(&target).unwrap_or_default();
        for path in dialogs::leaves(&source) {
            let Some(value) = dialogs::at(&source, &path) else {
                continue;
            };
            if dialogs::at(&current, &path) == Some(value) {
                continue;
            }
            let mut candidate = current.clone();
            if !dialogs::replace(&mut candidate, &path, value.clone()) {
                continue;
            }
            let Ok(settings) = serde_json::from_value::<Settings>(candidate.clone()) else {
                continue;
            };
            if settings_ui::page_modified(self.settings_ui.page, &self.settings, &settings) {
                self.settings = settings;
                current = candidate;
            }
        }
        self.theme = UiTheme::for_name(&self.settings.appearance.theme);
        self.scheme = settings_ui::scheme_by_name(&self.settings.editor.color_scheme)
            .unwrap_or_else(ColorScheme::dark);
        self.keymap = Keymap::preset(&self.settings.keymap);
        self.shortcuts_applied = false;
        self.apply_ui_font_size(ctx);
    }
    pub(super) fn settings_option_matches(
        &self,
        page: settings_ui::SettingsPage,
        filter: &str,
    ) -> bool {
        let key = (page.title().to_owned(), filter.to_owned());
        if let Some(answer) = self.dialog_workspace.search_cache.borrow().get(&key) {
            return *answer;
        }
        let answer = self.settings_option_matches_uncached(page, filter);
        let mut cache = self.dialog_workspace.search_cache.borrow_mut();
        if cache.len() > 4096 {
            cache.clear();
        }
        cache.insert(key, answer);
        answer
    }
    fn settings_option_matches_uncached(
        &self,
        page: settings_ui::SettingsPage,
        filter: &str,
    ) -> bool {
        let words = filter.split_whitespace().collect::<Vec<_>>();
        if words.is_empty() {
            return true;
        }
        let default = Settings::default();
        let current = serde_json::to_value(&self.settings).unwrap_or_default();
        let defaults = serde_json::to_value(&default).unwrap_or_default();
        for path in dialogs::leaves(&current) {
            let label = path.join(" ").replace('_', " ");
            if !words.iter().all(|word| label.to_lowercase().contains(word)) {
                continue;
            }
            let mut probe = current.clone();
            let existing = dialogs::at(&current, &path).cloned().unwrap_or_default();
            let replacement = match existing {
                Value::Bool(value) => Value::Bool(!value),
                Value::String(value) => Value::String(format!("{value}_probe")),
                Value::Number(_) => Value::from(12345),
                _ => dialogs::at(&defaults, &path)
                    .cloned()
                    .unwrap_or(Value::Null),
            };
            if !dialogs::replace(&mut probe, &path, replacement) {
                continue;
            }
            if let Ok(candidate) = serde_json::from_value::<Settings>(probe) {
                if settings_ui::page_modified(page, &self.settings, &candidate) {
                    return true;
                }
            }
        }
        false
    }
    pub(super) fn settings_page_shell(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let title = self.settings_ui.page.title();
        ui.horizontal_wrapped(|ui|{for(part,index)in title.split('›').zip(0..){if index>0{ui.label("›");}ui.label(part.trim());}let project=title.starts_with("Project")||self.settings_ui.page==settings_ui::SettingsPage::CodeStyle;ui.label(egui::RichText::new(if project{"Project"}else{"IDE"}).small().color(self.theme.text_dim));if ui.small_button("Help").clicked(){self.dialog_workspace.help=Some(format!("# {title}\n\nChanges preview in the current window. Apply saves them, OK saves and closes, Cancel restores the opening snapshot, Reset reverts this page's unapplied changes, and Reset to Defaults restores its shipped values.\n\nProject pages use the captured project. IDE preferences apply to all projects. Search accepts page names and serialized option names (spaces replace underscores). New Projects Settings seeds formatting preferences for subsequently created projects. Project trust, credentials and executable choices never come from project-preferences.json.\n\nUse Help → Help Topics for the bundled guide, or Help → Online Help for the project documentation."));}
if ui.small_button("Reset").clicked(){self.settings_page_reset(false,ctx);}
if ui.small_button("Reset to Defaults").clicked(){self.settings_page_reset(true,ctx);}
if ui.small_button("Dialog options…").clicked(){self.dialog_workspace.options=true;}});
        if !self.settings_ui.search.trim().is_empty() {
            ui.label(highlighted(
                &format!("Matching settings: {}", self.settings_ui.search),
                &self.settings_ui.search,
                self.theme.accent,
            ));
        }
    }
    pub(super) fn dialog_workspace_tick(&mut self, ctx: &egui::Context) {
        self.dialog_workspace.load();
        self.poll_dialog_workspace_job(ctx);
        if self.dialog_workspace.prefs.sync_project_settings
            && !matches!(self.dialog, Dialog::Settings)
        {
            if let Some(project) = &self.project {
                let root = project.root.clone();
                let value = dialogs::project_values(&self.settings);
                if self.dialog_workspace.project_observed.as_ref()
                    != Some(&(root.clone(), value.clone()))
                {
                    self.dialog_workspace.project_observed = Some((root.clone(), value));
                    if let Err(error) = dialogs::save_project(&root, &self.settings) {
                        self.set_status(format!("Cannot sync project preferences: {error}"));
                    }
                }
            }
        }
        self.dialog_policy_windows(ctx);
    }
    pub(super) fn dialog_policy_windows(&mut self, ctx: &egui::Context) {
        let mut open = self.dialog_workspace.options;
        if open {
            egui::Window::new("Popup and Dialog Options").open(&mut open).show_modal(ctx,|ui|{ui.checkbox(&mut self.dialog_workspace.prefs.sync_project_settings,"Sync shareable formatting settings to .rustpy/project-preferences.json on Apply");ui.add(egui::Slider::new(&mut self.dialog_workspace.prefs.popup_rows,6..=30).text("Chooser visible rows"));if ui.button("Reset remembered confirmation choices").clicked(){self.dialog_workspace.prefs.confirmations.clear();}
if ui.button("Reset remembered dialog sizes").clicked(){self.dialog_workspace.prefs.sizes.clear();}ui.label("Settings search sections (unchecked sections are excluded)");for group in ["Appearance & Behavior","Keymap","Editor","Project","Version Control","Build, Execution, Deployment","Tools","Languages & Frameworks","Plugins"]{let mut visible=!self.dialog_workspace.prefs.search_sections.contains(group);if ui.checkbox(&mut visible,crate::i18n::tr_static(group)).changed(){if visible{self.dialog_workspace.prefs.search_sections.remove(group);}else{self.dialog_workspace.prefs.search_sections.insert(group.into());}}}
if ui.button("Save dialog preferences").clicked(){self.dialog_workspace.result=self.dialog_workspace.prefs.save().map(|_|"Dialog preferences saved".into()).unwrap_or_else(|e|e.to_string());}});
        }
        self.dialog_workspace.options = open;
        let mut open = self.dialog_workspace.registry;
        if open {
            egui::Window::new("Registry — advanced local flags")
                .open(&mut open)
                .show_modal(ctx, |ui| {
                    ui.checkbox(
                        &mut self.dialog_workspace.prefs.developer_mode,
                        "Developer mode: enable Internal Actions / UI Inspector",
                    );
                    ui.checkbox(
                        &mut self.dialog_workspace.prefs.show_widget_ids,
                        "Show actual panel IDs and rectangles in UI Inspector",
                    );
                    ui.add(
                        egui::Slider::new(&mut self.dialog_workspace.prefs.popup_rows, 6..=30)
                            .text("popup.visible.rows"),
                    );
                    if ui.button("Save").clicked() {
                        let _ = self.dialog_workspace.prefs.save();
                    }
                });
        }
        self.dialog_workspace.registry = open;
        let mut open = self.dialog_workspace.inspector;
        if open && self.dialog_workspace.prefs.developer_mode {
            egui::Window::new("UI Inspector")
                .open(&mut open)
                .default_size([640., 480.])
                .show_modeless(ctx, |ui| {
                    if self.dialog_workspace.prefs.show_widget_ids {
                        for id in [
                            "project_panel",
                            "bottom_tools",
                            "main_toolbar",
                            "menu_bar",
                            "status_bar",
                        ] {
                            if let Some(state) = egui::PanelState::load(ctx, egui::Id::new(id)) {
                                ui.monospace(format!("{id}: {:?}", state.outer_rect));
                            }
                        }
                    }
                    ctx.inspection_ui(ui);
                });
        }
        self.dialog_workspace.inspector = open;
        let mut open = self.dialog_workspace.defaults;
        if open {
            egui::Window::new("Settings for New Projects").open(&mut open).default_size([560.,400.]).show_modal(ctx,|ui|{
                ui.label("New projects inherit pure formatting preferences. Trust, credentials, interpreter discovery and startup execution are unchanged.");
                ui.monospace(serde_json::to_string_pretty(&dialogs::project_values(&self.settings)).unwrap_or_default());
                if ui.button("Use current formatting for new projects").clicked(){self.dialog_workspace.result=dialogs::save_defaults(&self.settings).map(|_|"New-project defaults saved".into()).unwrap_or_else(|e|e.to_string());}
                if ui.button("Reset new-project defaults").clicked(){self.dialog_workspace.result=dialogs::save_defaults(&Settings::default()).map(|_|"New-project defaults reset".into()).unwrap_or_else(|e|e.to_string());}
                if ui.button("Edit this project's Run Configuration Templates…").clicked(){let root=self.project.as_ref().map(|project| project.root.clone()).unwrap_or_default();ctx.data_mut(|data|data.insert_temp(egui::Id::new(("run-configuration-templates",&root)),true));self.dialog=Dialog::RunConfigurations;}
                if ui.add_enabled(self.project.is_some(),egui::Button::new("Use this project's run templates for new projects")).clicked(){let root=self.project.as_ref().map(|project| project.root.clone()).unwrap_or_default();let templates=rustpy_project::run_workflow::Templates::load(&root);let value=serde_json::json!({"version":1,"source_root":root,"templates":templates});self.dialog_workspace.result=dialogs::save(&Settings::config_dir().join("new-project-run-templates.json"),&value).map(|_|"Run templates saved; project paths will follow each newly created root".into()).unwrap_or_else(|e|e.to_string());}
                ui.label(&self.dialog_workspace.result);
            });
        }
        self.dialog_workspace.defaults = open;
        let mut open = self.dialog_workspace.maintenance;
        let mut apply = false;
        if open {
            // Modal: Enter confirms the primary action and
            // Escape cancels, unless the Enter is the one that opened it.
            let confirmable = ctx.cumulative_pass_nr() > self.dialog_workspace.maintenance_opened;
            if confirmable
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                open = false;
            }
            let enter = open
                && confirmable
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            egui::Window::new(if self.dialog_workspace.repair{"Repair IDE"}else{"Invalidate Caches"}).open(&mut open).show_modal(ctx,|ui|{ui.label("Choose caches to rebuild for the captured current project. Source files and settings are retained. Clearing Local History permanently removes its snapshots for this project.");ui.checkbox(&mut self.dialog_workspace.filesystem,"File system, analysis and symbol caches");ui.checkbox(&mut self.dialog_workspace.history,"Delete this project's Local History");ui.checkbox(&mut self.dialog_workspace.shared,"Delete downloaded shared indexes (bundled indexes retained)");ui.checkbox(&mut self.dialog_workspace.git_log,"Clear this window's Git log caches");if !self.dialog_workspace.history&&!self.dialog_workspace.shared{ui.checkbox(&mut self.dialog_workspace.remember,"Do not ask again for cache rebuilds");}let idle=self.dialog_workspace.job.is_none();apply=ui.add_enabled(idle,egui::Button::new("Apply selected repairs").fill(ui.visuals().selection.bg_fill)).on_hover_text("Enter").clicked()||(enter&&idle);ui.label(&self.dialog_workspace.result);});
        }
        self.dialog_workspace.maintenance = open;
        if apply {
            let current = self.project.as_ref().map(|project| project.root.clone());
            let identity = current
                .as_ref()
                .and_then(|root| rustpy_settings::trust::DirectoryIdentity::capture(root).ok());
            if current != self.dialog_workspace.maintenance_root
                || identity != self.dialog_workspace.maintenance_identity
            {
                self.dialog_workspace.result =
                    "Project changed; reopen maintenance for the intended project".into();
                return;
            }
            if self.dialog_workspace.shared && self.shared_index_download_owner().is_some() {
                self.dialog_workspace.result =
                    "Stop the shared-index download before clearing its cache".into();
                return;
            }
            let mut caches = vec![];
            let mut analysis_replaced = false;
            if self.dialog_workspace.filesystem {
                analysis_replaced = self.rebuild_dialog_caches();
                caches.push("filesystem");
            }
            if self.dialog_workspace.git_log {
                self.git_panel.cancel_log_project();
                caches.push("git_log");
            }
            let mut paths = vec![];
            if self.dialog_workspace.history {
                paths.push(rustpy_vcs::local_history::project_dir(
                    &rustpy_vcs::local_history::base_dir(),
                    self.project.as_ref().map(|p| p.root.as_path()),
                ));
            }
            if self.dialog_workspace.shared {
                paths.push(self.symbol_indexes.store().shared_dir());
            }
            if self.dialog_workspace.history {
                caches.push("local_history");
            }
            if self.dialog_workspace.shared {
                caches.push("shared_indexes");
            }
            if !caches.is_empty() {
                record_caches_invalidated(analysis_replaced, &caches);
            }
            if self.dialog_workspace.remember
                && !self.dialog_workspace.history
                && !self.dialog_workspace.shared
            {
                self.dialog_workspace.prefs.remember("rebuild-caches", true);
                let _ = self.dialog_workspace.prefs.save();
            }
            if !paths.is_empty() {
                self.dialog_workspace.job = Some(
                    TaskManager::global()
                        .task("Clear selected IDE caches")
                        .spawn(move |progress| {
                            let result = (|| -> Result<String, String> {
                                let mut count = 0;
                                for path in paths {
                                    progress.check().map_err(|e| e.to_string())?;
                                    progress.set_text(path.display().to_string());
                                    count += remove_cache(&path, progress.token())?;
                                }
                                Ok(format!("Removed {count} cache entries"))
                            })();
                            progress.check()?;
                            result.map_err(rustpy_core::TaskError::Failed)
                        }),
                );
            }
            self.dialog_workspace.maintenance = false;
            self.dialog_workspace.repair = false;
        }
        if let Some(text) = self.dialog_workspace.help.clone() {
            let mut open = true;
            egui::Window::new("Settings Help")
                .open(&mut open)
                .default_size([600., 400.])
                .show_modal(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        crate::markdown_view::render_markdown(ui, &self.theme, &text, 14.);
                    });
                });
            if !open {
                self.dialog_workspace.help = None;
            }
        }
    }
}
/// The `caches_invalidated` performance-trace row: which caches the
/// Invalidate Caches / Repair IDE apply path cleared.
fn record_caches_invalidated(analysis_replaced: bool, caches: &[&str]) {
    crate::performance_trace::event(
        "caches_invalidated",
        serde_json::json!({
            "analysis_replaced": analysis_replaced,
            "index_rebuild_requested": analysis_replaced,
            "caches": caches,
        }),
    );
}
fn rebase_template_paths(value: &mut Value, source: &str, target: &str) {
    match value {
        Value::String(text) => {
            if text == source {
                *text = target.into();
            } else if let Some(suffix) = text
                .strip_prefix(source)
                .filter(|suffix| suffix.starts_with('/') || suffix.starts_with('\\'))
            {
                *text = format!("{target}{}", suffix.replace('\\', "/"));
            }
        }
        Value::Array(values) => {
            for value in values {
                rebase_template_paths(value, source, target)
            }
        }
        Value::Object(values) => {
            let old = std::mem::take(values);
            for (key, mut value) in old {
                let mut key_value = Value::String(key);
                rebase_template_paths(&mut key_value, source, target);
                rebase_template_paths(&mut value, source, target);
                values.insert(key_value.as_str().unwrap().into(), value);
            }
        }
        _ => {}
    }
}
fn remove_cache(path: &Path, cancel: &rustpy_core::CancellationToken) -> Result<usize, String> {
    remove_cache_inner(path, cancel, 0, &mut 200_000)
}
fn remove_cache_inner(
    path: &Path,
    cancel: &rustpy_core::CancellationToken,
    depth: usize,
    remaining: &mut usize,
) -> Result<usize, String> {
    cancel.check().map_err(|e| e.to_string())?;
    if depth > 64 || *remaining == 0 {
        return Err("Cache cleanup limit reached; remaining files were retained".into());
    }
    *remaining -= 1;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.to_string()),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let mut count = 1;
        for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
            count += remove_cache_inner(
                &entry.map_err(|e| e.to_string())?.path(),
                cancel,
                depth + 1,
                remaining,
            )?;
        }
        std::fs::remove_dir(path).map_err(|e| e.to_string())?;
        Ok(count)
    } else {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
        Ok(1)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn highlighted_unicode_boundaries_remain_valid() {
        let job = highlighted("Editor café", "café", Color32::WHITE);
        assert_eq!(job.text, "Editor café");
        assert!(job.sections.len() > 1);
    }
    #[test]
    fn popup_search_uses_current_ordered_word_start_policy() {
        assert!(super::super::choice_popup::choice_matches(
            "Python Interpreter",
            "",
            "pyin"
        ));
        assert!(!super::super::choice_popup::choice_matches(
            "Python Interpreter",
            "",
            "unrelated"
        ));
    }
    fn maintenance_frame(app: &mut IdeApp, ctx: &egui::Context, events: Vec<egui::Event>) {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 720.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.dialog_workspace_tick(ui.ctx()),
        )
        .drop_without_applying_deltas();
    }
    fn key(key: egui::Key) -> Vec<egui::Event> {
        [true, false]
            .map(|pressed| egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .to_vec()
    }
    fn maintenance_fixture() -> (IdeApp, egui::Context) {
        let mut app = IdeApp::new(Settings::default(), None);
        // Fresh in-memory preferences: no remembered "do not ask" answer.
        app.dialog_workspace.loaded = true;
        let ctx = egui::Context::default();
        maintenance_frame(&mut app, &ctx, vec![]);
        crate::performance_trace::take_recorded();
        (app, ctx)
    }
    fn caches_invalidated() -> Vec<Value> {
        crate::performance_trace::take_recorded()
            .into_iter()
            .filter(|(kind, _)| kind == "caches_invalidated")
            .map(|(_, data)| data)
            .collect()
    }
    #[test]
    fn invalidate_caches_dialog_enter_applies_selection_and_traces_it() {
        let (mut app, ctx) = maintenance_fixture();
        let enter = key(egui::Key::Enter);
        app.handle_action(Action::InvalidateCaches, &ctx);
        assert!(app.dialog_workspace.maintenance);
        assert!(
            !app.editor_controls_available(),
            "the dialog owns the keyboard"
        );
        // The Enter that chose the action in the same pass does not confirm.
        maintenance_frame(&mut app, &ctx, enter.clone());
        assert!(app.dialog_workspace.maintenance);
        assert!(caches_invalidated().is_empty());
        maintenance_frame(&mut app, &ctx, enter);
        assert!(
            !app.dialog_workspace.maintenance,
            "Enter applies and closes"
        );
        let events = caches_invalidated();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["caches"], serde_json::json!(["filesystem"]));
        assert_eq!(events[0]["analysis_replaced"], false, "no project is open");
        assert!(app.editor_controls_available());
    }
    #[test]
    fn repair_ide_apply_traces_every_selected_cache_and_escape_cancels() {
        let (mut app, ctx) = maintenance_fixture();
        app.handle_action(Action::RepairIde, &ctx);
        maintenance_frame(&mut app, &ctx, vec![]);
        maintenance_frame(&mut app, &ctx, key(egui::Key::Enter));
        assert_eq!(
            caches_invalidated()[0]["caches"],
            serde_json::json!(["filesystem", "git_log"])
        );
        app.handle_action(Action::InvalidateCaches, &ctx);
        maintenance_frame(&mut app, &ctx, vec![]);
        maintenance_frame(&mut app, &ctx, key(egui::Key::Escape));
        assert!(!app.dialog_workspace.maintenance);
        assert!(
            caches_invalidated().is_empty(),
            "cancel invalidates nothing"
        );
    }
    #[test]
    fn remembered_rebuild_answer_invalidates_directly_and_traces_it() {
        let (mut app, ctx) = maintenance_fixture();
        app.dialog_workspace.prefs.remember("rebuild-caches", true);
        app.handle_action(Action::InvalidateCaches, &ctx);
        assert!(!app.dialog_workspace.maintenance);
        assert_eq!(
            caches_invalidated()[0]["caches"],
            serde_json::json!(["filesystem"])
        );
    }
    #[test]
    fn new_project_run_templates_rebase_only_captured_root_paths() {
        let mut value = serde_json::json!({"working_dir":"/old/project/src","env":{"OTHER":"/old/project-extra"},"mapping":{"/old/project":"/remote"}});
        rebase_template_paths(&mut value, "/old/project", "/new/project");
        assert_eq!(value["working_dir"], "/new/project/src");
        assert_eq!(value["env"]["OTHER"], "/old/project-extra");
        assert_eq!(value["mapping"]["/new/project"], "/remote");
    }
}
