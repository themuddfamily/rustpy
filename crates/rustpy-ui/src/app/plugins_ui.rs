//! Settings › Plugins: the installed plugins (enable/disable, uninstall,
//! update, details with contributions and problems), the Marketplace (the
//! catalog bundled with the IDE, which installs offline, or a catalog from a
//! URL; search; install) and Install Plugin from Disk (a plugin folder or a
//! `.rpplugin` bundle). Changes apply at once, without a restart
//! ([`IdeApp::apply_plugin_changes`]); File › New from Plugin Template and the
//! plugin tools of Settings › Tools › External Tools are drawn here too.

const BACKGROUND_PLUGIN_JOB: u64 = 1 << 62;

use rustpy_plugins::{
    Catalog, CatalogEntry, Contributions, InstallOutcome, InstalledPlugin, PluginError,
    PluginRegistry,
};

use super::*;
use crate::a11y::AccessibleCombo;
use crate::plugin_host;

/// Width of the plugin lists; the details of the selected plugin get the rest.
const LIST_WIDTH: f32 = 250.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PluginsTab {
    #[default]
    Installed,
    Marketplace,
}

/// A catalog and where it came from.
pub(super) struct LoadedCatalog {
    catalog: Catalog,
    /// The URL it was fetched from; `None` for the catalog bundled with the IDE.
    url: Option<String>,
    /// Why the configured URL could not be used (the bundled catalog is shown instead).
    error: Option<String>,
}

/// State of the Plugins page (kept while the IDE runs).
#[derive(Default)]
pub(super) struct PluginsPage {
    pub tab: PluginsTab,
    search: String,
    /// Selected installed plugin (id).
    selected: Option<String>,
    /// Selected Marketplace entry (id).
    selected_entry: Option<String>,
    selected_version: String,
    show_disk: bool,
    disk_path: String,
    /// Result of the last operation: (text, is an error).
    message: Option<(String, bool)>,
    catalog: Option<LoadedCatalog>,
    catalog_job: Option<rustpy_core::TaskHandle<LoadedCatalog>>,
    install_job: Option<InstallJob>,
    confirm_uninstall: Option<String>,
}

/// A plugin install running in the background: the UI never waits for a
/// download or for copying a plugin folder (a slow or odd file system).
struct InstallJob {
    /// What is being installed: the plugin's name (Marketplace) or the path (from disk).
    label: String,
    from_disk: bool,
    task: rustpy_core::TaskHandle<InstallOutcome>,
}

/// What the user asked for while the page was drawn (applied afterwards, with the whole app at hand).
enum Request {
    SetEnabled(String, bool),
    Uninstall(String),
    InstallFromDisk(String),
    Install(Box<CatalogEntry>),
    LoadCatalog,
    OpenScreenshot(PathBuf),
}

/// "1 color scheme, 3 live templates" — what a plugin adds.
fn contribution_summary(c: &Contributions) -> String {
    let count = |n: usize, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    let parts: Vec<String> = [
        count(c.color_schemes.len(), "color scheme", "color schemes"),
        count(
            c.all_live_templates().len(),
            "live template",
            "live templates",
        ),
        count(c.keymaps.len(), "keymap", "keymaps"),
        count(
            c.textmate_grammars.len(),
            "TextMate grammar",
            "TextMate grammars",
        ),
        count(c.external_tools.len(), "external tool", "external tools"),
        count(c.file_templates.len(), "file template", "file templates"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        "no contributions".into()
    } else {
        parts.join(", ")
    }
}

/// Whether an installed plugin matches the search text.
fn plugin_matches(p: &InstalledPlugin, needle: &str) -> bool {
    needle.is_empty()
        || [
            p.name(),
            p.id.as_str(),
            p.manifest.description.as_str(),
            p.manifest.vendor.as_str(),
        ]
        .iter()
        .any(|f| f.to_lowercase().contains(needle))
}

/// `~/x` → `<home>/x`; surrounding quotes (from a pasted path) are dropped.
fn disk_path(input: &str) -> PathBuf {
    let s = input.trim().trim_matches(|c| c == '"' || c == '\'');
    match s.strip_prefix("~/").zip(dirs::home_dir()) {
        Some((rest, home)) => home.join(rest),
        None => PathBuf::from(s),
    }
}

/// An error for the message line: every validation problem, not just the first.
fn error_text(e: &PluginError) -> String {
    match e {
        PluginError::Invalid { problems } if problems.len() > 1 => {
            format!("Invalid plugin:\n• {}", problems.join("\n• "))
        }
        other => other.to_string(),
    }
}

/// Re-applies `custom` shortcut overrides (`Settings::custom_shortcuts`) to `keymap`.
fn apply_shortcuts(keymap: &mut Keymap, custom: &[(String, Vec<String>)]) {
    for (id, chords) in custom {
        if let Some(action) = Keymap::action_by_id(id) {
            keymap.clear_action(action);
            for c in chords {
                if let Some(chord) = KeyChord::parse(c) {
                    keymap.bind(chord, action);
                }
            }
        }
    }
}

/// The editor scheme to use after the plugins changed, when the current one
/// (`name`) is a plugin's: its new version, or — when its plugin is gone —
/// the builtin scheme for the theme. `None` keeps builtin and user schemes.
pub(super) fn scheme_after_plugin_change(
    name: &str,
    dark: bool,
    is_user_scheme: bool,
    plugins: &Contributions,
) -> Option<ColorScheme> {
    if ColorScheme::by_name(name).is_some() || is_user_scheme {
        return None;
    }
    plugins
        .color_scheme(name)
        .cloned()
        .or_else(|| ColorScheme::by_name(if dark { "Dark" } else { "Light" }))
}

/// The keymap name to use after the plugins changed: a plugin keymap that is
/// gone falls back to Default. `None` keeps the name.
pub(super) fn keymap_after_plugin_change(name: &str, plugins: &Contributions) -> Option<String> {
    let builtin = name.is_empty() || Keymap::preset_names().contains(&name);
    (!builtin && plugins.keymap(name).is_none()).then(|| "Default".to_string())
}

impl IdeApp {
    /// Puts plugin changes into effect without a restart: live templates are
    /// rebuilt; the color scheme and the keymap, when a plugin provides them,
    /// are re-read (or fall back to the defaults when their plugin is gone).
    /// The Settings dialog's Cancel state is updated the same way. External
    /// tools and file templates are read from the plugins when menus open.
    /// Returns what the user should know (a scheme or keymap that is gone).
    pub(super) fn apply_plugin_changes(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        self.templates = super::load_templates();
        let plugins = plugin_host::contributions();
        let user_scheme = |name: &str| {
            Settings::config_dir()
                .join("colors")
                .join(format!("{name}.json"))
                .is_file()
        };
        if let Some(s) = scheme_after_plugin_change(
            &self.scheme.name,
            self.scheme.dark,
            user_scheme(&self.scheme.name),
            &plugins,
        ) {
            if s.name != self.scheme.name {
                notes.push(format!(
                    "the color scheme \"{}\" is no longer available, using {}",
                    self.scheme.name, s.name
                ));
            }
            self.settings.editor.color_scheme = s.name.clone();
            self.scheme = s;
        }
        let fallback = keymap_after_plugin_change(&self.settings.keymap, &plugins);
        if let Some(name) = &fallback {
            notes.push(format!(
                "the keymap \"{}\" is no longer available, using {name}",
                self.settings.keymap
            ));
            self.settings.keymap = name.clone();
        }
        if fallback.is_some() || plugin_host::is_plugin_keymap(&self.settings.keymap, &plugins) {
            self.apply_custom_shortcuts();
        }
        if let Some(snap) = self.settings_ui.snapshot.as_mut() {
            if let Some(s) = scheme_after_plugin_change(
                &snap.scheme.name,
                snap.scheme.dark,
                user_scheme(&snap.scheme.name),
                &plugins,
            ) {
                snap.settings.editor.color_scheme = s.name.clone();
                snap.scheme = s;
            }
            let fallback = keymap_after_plugin_change(&snap.settings.keymap, &plugins);
            if let Some(name) = &fallback {
                snap.settings.keymap = name.clone();
            }
            if fallback.is_some() || plugin_host::is_plugin_keymap(&snap.settings.keymap, &plugins)
            {
                snap.keymap = plugin_host::keymap(&snap.settings.keymap);
                apply_shortcuts(&mut snap.keymap, &snap.settings.custom_shortcuts);
            }
        }
        notes
    }

    pub(super) fn plugin_background_jobs(&self) -> Vec<(u64, String)> {
        let state = &self.settings_ui.plugins;
        let mut jobs = Vec::new();
        if let Some(task) = &state.catalog_job {
            jobs.push((task.id() | BACKGROUND_PLUGIN_JOB, task.title().into()));
        }
        if let Some(job) = &state.install_job {
            jobs.push((
                job.task.id() | BACKGROUND_PLUGIN_JOB,
                job.task.title().into(),
            ));
        }
        jobs
    }
    pub(super) fn stop_plugin_background_job(&mut self, id: u64) -> bool {
        let state = &mut self.settings_ui.plugins;
        if let Some(task) = state
            .catalog_job
            .as_ref()
            .filter(|task| task.id() | BACKGROUND_PLUGIN_JOB == id)
        {
            task.cancel();
            return true;
        }
        if let Some(job) = state
            .install_job
            .as_ref()
            .filter(|job| job.task.id() | BACKGROUND_PLUGIN_JOB == id)
        {
            job.task.cancel();
            return true;
        }
        false
    }
    /// Cancel admission/publication, then retain existing I/O until completion.
    pub(super) fn retire_plugin_background_jobs(&mut self, ctx: &egui::Context) -> bool {
        let state = &mut self.settings_ui.plugins;
        if let Some(task) = state.catalog_job.as_mut() {
            task.cancel();
            if task.try_result().is_some() {
                state.catalog_job = None;
            }
        }
        if let Some(job) = state.install_job.as_mut() {
            job.task.cancel();
            if job.task.try_result().is_some() {
                state.install_job = None;
            }
        }
        let pending = state.catalog_job.is_some() || state.install_job.is_some();
        if pending {
            ctx.request_repaint_after(std::time::Duration::from_millis(25));
        }
        pending
    }

    /// Finishes background plugin jobs (catalog download, installs). Called every frame.
    pub(super) fn poll_plugin_jobs(&mut self, ctx: &egui::Context) {
        let st = &mut self.settings_ui.plugins;
        if let Some(task) = &mut st.catalog_job {
            if let Some(result) = task.try_result() {
                if !task.is_cancelled() {
                    if let Ok(loaded) = result {
                        st.catalog = Some(loaded);
                    }
                }
                st.catalog_job = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        let finished = st
            .install_job
            .as_mut()
            .and_then(|job| job.task.try_result());
        if st.install_job.is_some() && finished.is_none() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let finished = finished.map(|result| result.map_err(|error| error.to_string()));
        let Some(result) = finished else { return };
        let Some(job) = self.settings_ui.plugins.install_job.take() else {
            return;
        };
        if job.task.is_cancelled() {
            return;
        }
        // The install wrote to disk through its own registry: read the plugins again.
        plugin_host::reload();
        let notes = self.apply_plugin_changes();
        let message = match result {
            Ok(outcome) => {
                let st = &mut self.settings_ui.plugins;
                st.selected = Some(outcome.id.clone());
                if job.from_disk {
                    st.show_disk = false;
                    st.disk_path.clear();
                    st.tab = PluginsTab::Installed;
                }
                let name = if job.from_disk {
                    plugin_host::plugin_name(&outcome.id)
                } else {
                    job.label
                };
                Ok(installed_text(&name, &outcome))
            }
            Err(e) => Err(format!("Cannot install {}: {e}", job.label)),
        };
        self.show_plugin_message(with_notes(message, &notes));
    }

    /// Reports the outcome of a plugin operation on the page and in the status bar.
    fn show_plugin_message(&mut self, message: Result<String, String>) {
        let (Ok(text) | Err(text)) = &message;
        self.set_status(text.clone());
        self.settings_ui.plugins.message = Some(match message {
            Ok(t) => (t, false),
            Err(t) => (t, true),
        });
    }

    /// Runs `install` on a background thread; [`Self::poll_plugin_jobs`] picks up the result.
    fn start_install(
        &mut self,
        label: String,
        from_disk: bool,
        install: impl FnOnce() -> Result<InstallOutcome, PluginError> + Send + 'static,
    ) {
        if self.settings_ui.plugins.install_job.is_some() {
            return;
        }
        let task = rustpy_core::TaskManager::global()
            .task(format!("Install plugin {label}"))
            .spawn(move |progress| {
                progress.token().check()?;
                // Atomic installation must finish or roll back before retirement.
                install().map_err(|error| rustpy_core::TaskError::Failed(error_text(&error)))
            });
        self.settings_ui.plugins.message = Some((format!("Installing {label}…"), false));
        self.settings_ui.plugins.install_job = Some(InstallJob {
            label,
            from_disk,
            task,
        });
    }

    pub(super) fn page_plugins(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("Executable host, bundled capabilities, publisher trust and IDE scripting…")
            .clicked()
        {
            self.plugin_runtime.open = true;
        }
        let theme = self.theme.clone();
        let registry = plugin_host::registry();
        let mut requests: Vec<Request> = Vec::new();
        if self.settings_ui.plugins.catalog.is_none()
            && self.settings_ui.plugins.catalog_job.is_none()
        {
            requests.push(Request::LoadCatalog);
        }
        {
            let st = &mut self.settings_ui.plugins;
            ui.horizontal(|ui| {
                let installed = format!("Installed ({})", registry.plugins().len());
                if ui
                    .selectable_label(
                        st.tab == PluginsTab::Installed,
                        egui::RichText::new(installed).strong(),
                    )
                    .clicked()
                {
                    st.tab = PluginsTab::Installed;
                }
                if ui
                    .selectable_label(
                        st.tab == PluginsTab::Marketplace,
                        egui::RichText::new("Marketplace").strong(),
                    )
                    .clicked()
                {
                    st.tab = PluginsTab::Marketplace;
                }
                ui.add_space(12.0);
                ui.add(
                    egui::TextEdit::singleline(&mut st.search)
                        .hint_text("🔍 Search plugins")
                        .desired_width(180.0),
                );
                if ui
                    .selectable_label(st.show_disk, "Install Plugin from Disk…")
                    .on_hover_text("A plugin folder (with plugin.json) or a .rpplugin bundle")
                    .clicked()
                {
                    st.show_disk = !st.show_disk;
                }
            });
            if st.show_disk {
                ui.horizontal(|ui| {
                    ui.label("Plugin folder or .rpplugin file:");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut st.disk_path)
                            .hint_text("~/plugins/my-plugin")
                            .desired_width(300.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let ready = !st.disk_path.trim().is_empty() && st.install_job.is_none();
                    if (ui
                        .add_enabled(ready, egui::Button::new("Install"))
                        .clicked()
                        || enter)
                        && ready
                    {
                        requests.push(Request::InstallFromDisk(st.disk_path.clone()));
                    }
                    if ui.button("Cancel").clicked() {
                        st.show_disk = false;
                    }
                });
            }
            if let Some((text, is_error)) = &st.message {
                ui.label(egui::RichText::new(text).color(if *is_error {
                    theme.error
                } else {
                    theme.success
                }));
            }
            ui.separator();
            match st.tab {
                PluginsTab::Installed => installed_tab(ui, st, &theme, &registry, &mut requests),
                PluginsTab::Marketplace => marketplace_tab(
                    ui,
                    st,
                    &theme,
                    &registry,
                    &mut self.settings.plugin_catalog_url,
                    &mut requests,
                ),
            }
        }
        for request in requests {
            self.plugin_request(request);
        }
    }

    fn plugin_request(&mut self, request: Request) {
        let message = match request {
            Request::SetEnabled(id, on) => {
                let name = plugin_host::plugin_name(&id);
                match plugin_host::update(|r| r.set_enabled(&id, on)) {
                    Ok(()) => Ok(format!(
                        "{name} {}",
                        if on { "enabled" } else { "disabled" }
                    )),
                    Err(e) => Err(error_text(&e)),
                }
            }
            Request::Uninstall(id) => {
                let name = plugin_host::plugin_name(&id);
                self.settings_ui.plugins.confirm_uninstall = None;
                match plugin_host::update(|r| r.uninstall(&id)) {
                    Ok(()) => {
                        self.settings_ui.plugins.selected = None;
                        Ok(format!("{name} uninstalled"))
                    }
                    Err(e) => Err(error_text(&e)),
                }
            }
            Request::InstallFromDisk(input) => {
                let path = disk_path(&input);
                let config_dir = plugin_host::config_dir();
                self.start_install(path.display().to_string(), true, move || {
                    PluginRegistry::load(&config_dir).install_from_path(&path)
                });
                return;
            }
            Request::Install(entry) => {
                let network_policy = rustpy_net::config();
                let config_dir = plugin_host::config_dir();
                self.start_install(entry.name.clone(), false, move || {
                    rustpy_plugins::download_and_install_with(&entry, &config_dir, &network_policy)
                });
                return;
            }
            Request::OpenScreenshot(path) => {
                self.open_file(&path);
                return;
            }
            Request::LoadCatalog => {
                self.load_plugin_catalog();
                return;
            }
        };
        let notes = self.apply_plugin_changes();
        self.show_plugin_message(with_notes(message, &notes));
    }

    /// Loads the Marketplace catalog: the bundled one at once, or the one at
    /// the configured URL in the background (the bundled one when that fails).
    fn load_plugin_catalog(&mut self) {
        let url = self.settings.plugin_catalog_url.trim().to_string();
        let st = &mut self.settings_ui.plugins;
        if url.is_empty() {
            st.catalog = Some(LoadedCatalog {
                catalog: Catalog::bundled(),
                url: None,
                error: None,
            });
            return;
        }
        if st.catalog_job.is_some() {
            return;
        }
        let network_policy = rustpy_net::config();
        let task = rustpy_core::TaskManager::global()
            .task("Plugin Marketplace catalog")
            .spawn(move |progress| {
                progress.token().check()?;
                let loaded = match Catalog::fetch_with(&url, &network_policy) {
                    Ok(catalog) => LoadedCatalog {
                        catalog,
                        url: Some(url),
                        error: None,
                    },
                    Err(e) => LoadedCatalog {
                        catalog: Catalog::bundled(),
                        url: None,
                        error: Some(format!("Cannot load the catalog: {e}")),
                    },
                };
                progress.token().check()?;
                Ok(loaded)
            });
        st.catalog_job = Some(task);
    }

    /// Plugin tools under Settings › Tools › External Tools (read-only; managed in Settings › Plugins).
    pub(super) fn plugin_external_tools_list(&mut self, ui: &mut egui::Ui) {
        let plugins = plugin_host::contributions();
        if plugins.external_tools.is_empty() {
            return;
        }
        ui.add_space(8.0);
        ui.label(egui::RichText::new("From plugins").strong());
        let registry = plugin_host::registry();
        for c in &plugins.external_tools {
            let command = registry
                .get(&c.plugin_id)
                .map(|p| declared_command(p, &c.tool))
                .unwrap_or_else(|| plugin_host::tool_command_line(&c.tool));
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{} › {}", c.tool.group, c.tool.name));
                ui.label(egui::RichText::new(command).monospace().small());
                ui.label(
                    egui::RichText::new(format!("({})", plugin_host::plugin_name(&c.plugin_id)))
                        .color(self.theme.text_dim)
                        .small(),
                );
            });
        }
        if ui.small_button("Manage in Settings › Plugins").clicked() {
            self.settings_ui.page = settings_ui::SettingsPage::Plugins;
        }
    }

    /// File › New from Plugin Template (only when plugins contribute file templates).
    pub(super) fn plugin_file_templates_menu(&mut self, ui: &mut egui::Ui) {
        let entries = plugin_host::file_template_menu_entries();
        if entries.is_empty() {
            return;
        }
        ui.menu_button("New from Plugin Template", |ui| {
            for (label, kind) in entries {
                if ui.button(label).clicked() {
                    let dir = self
                        .project
                        .as_ref()
                        .map(|p| {
                            p.tree
                                .selected
                                .clone()
                                .map(|s| {
                                    if s.is_dir() {
                                        s
                                    } else {
                                        s.parent().map(Path::to_path_buf).unwrap_or_default()
                                    }
                                })
                                .unwrap_or_else(|| p.root.clone())
                        })
                        .unwrap_or_else(|| PathBuf::from("."));
                    let name = plugin_host::default_file_name(kind).unwrap_or_default();
                    self.dialog = Dialog::NewFileKind { dir, kind, name };
                    ui.close();
                }
            }
        });
    }
}

fn installed_text(name: &str, outcome: &InstallOutcome) -> String {
    match &outcome.previous_version {
        Some(old) => format!("{name} updated from {old} to {}", outcome.version),
        None => format!("{name} {} installed", outcome.version),
    }
}

/// `message` followed by `notes` ("Demo Kit disabled; the color scheme … is no longer available").
fn with_notes(message: Result<String, String>, notes: &[String]) -> Result<String, String> {
    if notes.is_empty() {
        return message;
    }
    let add = |text: String| format!("{text}; {}", notes.join("; "));
    message.map(add).map_err(add)
}

/// The Installed tab: plugins on the left, the selected one's details on the right.
fn installed_tab(
    ui: &mut egui::Ui,
    st: &mut PluginsPage,
    theme: &UiTheme,
    registry: &PluginRegistry,
    requests: &mut Vec<Request>,
) {
    for e in registry.errors() {
        ui.label(egui::RichText::new(e).color(theme.error));
    }
    let needle = st.search.trim().to_lowercase();
    let plugins: Vec<&InstalledPlugin> = registry
        .plugins()
        .iter()
        .filter(|p| plugin_matches(p, &needle))
        .collect();
    let updates = st
        .catalog
        .as_ref()
        .map(|c| c.catalog.updates(registry))
        .unwrap_or_default();
    if st
        .selected
        .as_ref()
        .is_none_or(|id| registry.get(id).is_none())
    {
        st.selected = plugins.first().map(|p| p.id.clone());
    }
    if registry.plugins().is_empty() {
        ui.label(egui::RichText::new(format!("No plugins installed. Browse the Marketplace or install one from disk; plugins live in {}.", registry.plugins_dir().display())).color(theme.text_dim));
    }
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LIST_WIDTH);
            egui::ScrollArea::vertical()
                .id_salt("plugins_installed_list")
                .max_height(360.0)
                .show(ui, |ui| {
                    for p in &plugins {
                        ui.horizontal(|ui| {
                            let mut on = p.enabled;
                            if ui
                                .add(egui::Checkbox::without_text(&mut on))
                                .on_hover_text(if p.enabled {
                                    "Enabled: uncheck to disable"
                                } else {
                                    "Disabled: check to enable"
                                })
                                .changed()
                            {
                                requests.push(Request::SetEnabled(p.id.clone(), on));
                            }
                            let mut name = egui::RichText::new(shortened(p.name(), 28)).strong();
                            if !p.is_valid() {
                                name = name.color(theme.error);
                            } else if !p.enabled {
                                name = name.color(theme.text_dim);
                            }
                            if ui
                                .selectable_label(
                                    st.selected.as_deref() == Some(p.id.as_str()),
                                    name,
                                )
                                .clicked()
                            {
                                st.selected = Some(p.id.clone());
                            }
                            ui.label(
                                egui::RichText::new(&p.manifest.version)
                                    .color(theme.text_dim)
                                    .small(),
                            );
                            if updates.iter().any(|u| u.id == p.id) {
                                ui.label(egui::RichText::new("update").color(theme.accent).small());
                            }
                        });
                        let status = if !p.is_valid() {
                            egui::RichText::new(format!("{} problem(s)", p.errors.len()))
                                .color(theme.error)
                        } else if !p.enabled {
                            egui::RichText::new("disabled").color(theme.text_dim)
                        } else {
                            egui::RichText::new(contribution_summary(p.contributions()))
                                .color(theme.text_dim)
                        };
                        ui.horizontal(|ui| {
                            ui.add_space(26.0);
                            ui.add(egui::Label::new(status.small()).wrap());
                        });
                    }
                });
        });
        ui.separator();
        ui.vertical(|ui| {
            // Long texts (paths, commands) wrap instead of widening the dialog.
            ui.set_max_width(ui.available_width());
            if let Some(p) = st.selected.as_ref().and_then(|id| registry.get(id)) {
                let update = updates
                    .iter()
                    .find(|u| u.id == p.id)
                    .map(|u| u.entry.clone());
                plugin_details(ui, st, theme, p, update, requests);
            }
        });
    });
    ui.add_space(8.0);
    egui::CollapsingHeader::new(egui::RichText::new("Built-in features").strong())
        .id_salt("plugins_builtin")
        .default_open(false)
        .show(ui, |ui| {
            for (name, desc) in [
                (
                    "Python",
                    "Parser, type inference, inspections, refactorings",
                ),
                ("Git", "Version control (libgit2)"),
                ("Terminal", "Embedded terminal emulator"),
                ("Debugger", "debugpy / DAP client"),
                ("Test Runner", "pytest and unittest"),
                ("Live Templates", "Live, surround and postfix templates"),
                ("Markdown", "Editor with preview"),
                ("Database Tools", "SQLite"),
                ("HTTP Client", ".http request files"),
            ] {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(name).strong());
                    ui.label(egui::RichText::new(desc).color(theme.text_dim));
                });
            }
        });
}

/// Name, version, actions, description, contributions and problems of an installed plugin.
fn plugin_details(
    ui: &mut egui::Ui,
    st: &mut PluginsPage,
    theme: &UiTheme,
    p: &InstalledPlugin,
    update: Option<CatalogEntry>,
    requests: &mut Vec<Request>,
) {
    ui.heading(p.name());
    let mut facts = vec![p.id.clone(), format!("version {}", p.manifest.version)];
    if !p.manifest.vendor.trim().is_empty() {
        facts.push(p.manifest.vendor.clone());
    }
    ui.label(egui::RichText::new(facts.join(" · ")).color(theme.text_dim));
    if let Some(url) = &p.manifest.homepage {
        ui.hyperlink_to(egui::RichText::new(url).small(), url);
    }
    let busy = st.install_job.is_some();
    ui.horizontal(|ui| {
        let toggle = if p.enabled { "Disable" } else { "Enable" };
        if ui.button(toggle).clicked() {
            requests.push(Request::SetEnabled(p.id.clone(), !p.enabled));
        }
        if st.confirm_uninstall.as_deref() == Some(p.id.as_str()) {
            ui.label(egui::RichText::new(format!("Uninstall {}?", p.name())).color(theme.warning));
            if ui
                .add_enabled(!busy, egui::Button::new("Uninstall"))
                .clicked()
            {
                requests.push(Request::Uninstall(p.id.clone()));
            }
            if ui.button("Keep").clicked() {
                st.confirm_uninstall = None;
            }
        } else if ui
            .add_enabled(!busy, egui::Button::new("Uninstall…"))
            .on_disabled_hover_text("Wait for the install to finish")
            .clicked()
        {
            st.confirm_uninstall = Some(p.id.clone());
        }
        if let Some(entry) = update {
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(format!("Update to {}", entry.version)),
                )
                .clicked()
            {
                requests.push(Request::Install(Box::new(entry)));
            }
        }
    });
    for (index, screenshot) in p.manifest.screenshots.iter().enumerate() {
        if ui
            .button(format!("View screenshot {}", index + 1))
            .clicked()
        {
            if let Ok(path) = p.dir.join(screenshot).canonicalize() {
                if p.dir
                    .canonicalize()
                    .is_ok_and(|root| path.starts_with(root))
                {
                    requests.push(Request::OpenScreenshot(path));
                }
            }
        }
    }
    if !p.manifest.description.trim().is_empty() {
        ui.add_space(4.0);
        ui.label(&p.manifest.description);
    }
    for e in &p.errors {
        ui.label(egui::RichText::new(format!("✖ {e}")).color(theme.error));
    }
    let c = p.contributions();
    if !c.is_empty() {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(if p.is_active() {
                "Contributes"
            } else {
                "Contributes when enabled"
            })
            .strong(),
        );
        let rows = [
            (
                "Color schemes",
                c.color_schemes
                    .iter()
                    .map(|s| s.scheme.name.clone())
                    .collect::<Vec<_>>(),
            ),
            (
                "Live templates",
                c.all_live_templates()
                    .iter()
                    .map(|t| t.abbreviation.clone())
                    .collect(),
            ),
            (
                "Keymaps",
                c.keymaps.iter().map(|k| k.name.clone()).collect(),
            ),
            (
                "TextMate grammars",
                c.textmate_grammars
                    .iter()
                    .map(|g| format!("{} (.{})", g.language, g.extensions.join(", .")))
                    .collect(),
            ),
            (
                "External tools",
                c.external_tools
                    .iter()
                    .map(|t| format!("{} ({})", t.tool.name, declared_command(p, &t.tool)))
                    .collect(),
            ),
            (
                "File templates",
                c.file_templates
                    .iter()
                    .map(|t| format!("{} (.{})", t.name, t.extension))
                    .collect(),
            ),
        ];
        for (label, items) in rows.iter().filter(|(_, items)| !items.is_empty()) {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(format!("{label}:")).color(theme.text_dim));
                ui.label(items.join(", "));
            });
        }
    }
    let mut warnings = p.warnings.clone();
    for k in &c.keymaps {
        warnings.extend(
            plugin_host::keymap_from_contribution(k)
                .1
                .into_iter()
                .map(|w| format!("keymap {}: {w}", k.name)),
        );
    }
    for w in &warnings {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("⚠ {w}"))
                    .color(theme.warning)
                    .small(),
            )
            .wrap(),
        );
    }
    ui.add_space(4.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(p.dir.display().to_string())
                .color(theme.text_dim)
                .small(),
        )
        .wrap(),
    );
}

/// The command of a plugin tool as its manifest declares it (`$PluginDir$/bin/x
/// $FilePath$`), which reads better than the resolved one with the full
/// plugin path.
fn declared_command(p: &InstalledPlugin, tool: &rustpy_settings::ExternalTool) -> String {
    match p
        .manifest
        .contributes
        .external_tools
        .iter()
        .find(|d| d.name.trim() == tool.name)
    {
        Some(d) => format!("{} {}", d.program.trim(), d.arguments.trim())
            .trim_end()
            .to_string(),
        None => plugin_host::tool_command_line(tool),
    }
}

/// `text`, cut to `max` characters with an ellipsis.
fn shortened(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// The Marketplace tab: catalog source, entries on the left, the selected entry on the right.
fn marketplace_tab(
    ui: &mut egui::Ui,
    st: &mut PluginsPage,
    theme: &UiTheme,
    registry: &PluginRegistry,
    catalog_url: &mut String,
    requests: &mut Vec<Request>,
) {
    ui.horizontal(|ui| {
        ui.label("Catalog URL:");
        let r = ui.add(
            egui::TextEdit::singleline(catalog_url)
                .hint_text("empty: the catalog bundled with RustPy")
                .desired_width(300.0),
        );
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui
            .add_enabled(st.catalog_job.is_none(), egui::Button::new("Refresh"))
            .clicked()
            || enter)
            && st.catalog_job.is_none()
        {
            requests.push(Request::LoadCatalog);
        }
    });
    if st.catalog_job.is_some() {
        ui.horizontal(|ui| {
            crate::a11y::spinner(ui);
            ui.label("Loading the catalog…");
        });
        return;
    }
    let Some(loaded) = &st.catalog else { return };
    match (&loaded.error, &loaded.url) {
        (Some(e), _) => {
            ui.label(egui::RichText::new(e).color(theme.error));
            ui.label(
                egui::RichText::new("Showing the catalog bundled with RustPy.")
                    .color(theme.text_dim)
                    .small(),
            );
        }
        (None, Some(url)) => {
            ui.label(
                egui::RichText::new(format!(
                    "{} plugins from {url}",
                    loaded.catalog.listing().len()
                ))
                .color(theme.text_dim)
                .small(),
            );
        }
        (None, None) => {
            ui.label(
                egui::RichText::new(
                    "The catalog bundled with RustPy: its plugins install without a network.",
                )
                .color(theme.text_dim)
                .small(),
            );
        }
    }
    for problem in &loaded.catalog.problems {
        ui.label(
            egui::RichText::new(format!("⚠ skipped {problem}"))
                .color(theme.warning)
                .small(),
        );
    }
    let entries = loaded.catalog.search(&st.search);
    if entries.is_empty() {
        ui.label(egui::RichText::new("No plugins match.").color(theme.text_dim));
        return;
    }
    if st
        .selected_entry
        .as_ref()
        .is_none_or(|id| !entries.iter().any(|e| &e.id == id))
    {
        st.selected_entry = entries.first().map(|e| e.id.clone());
    }
    let busy = st.install_job.is_some();
    // (label, enabled) of an entry's install button.
    let action = |e: &CatalogEntry| -> (String, bool) {
        match registry.get(&e.id) {
            Some(p) => match (
                p.manifest.parsed_version(),
                rustpy_plugins::Version::parse(&e.version).ok(),
            ) {
                (Some(have), Some(offered)) if offered > have => {
                    (format!("Update to {}", e.version), e.is_compatible())
                }
                _ => ("Installed".into(), false),
            },
            None => ("Install".into(), e.is_compatible()),
        }
    };
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LIST_WIDTH);
            egui::ScrollArea::vertical()
                .id_salt("plugins_market_list")
                .max_height(360.0)
                .show(ui, |ui| {
                    for e in &entries {
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(
                                    st.selected_entry.as_deref() == Some(e.id.as_str()),
                                    egui::RichText::new(shortened(&e.name, 26)).strong(),
                                )
                                .clicked()
                            {
                                st.selected_entry = Some(e.id.clone());
                            }
                            ui.label(
                                egui::RichText::new(shortened(&e.version, 12))
                                    .color(theme.text_dim)
                                    .small(),
                            );
                            let (label, enabled) = action(e);
                            if ui
                                .add_enabled(
                                    enabled && !busy,
                                    egui::Button::new(egui::RichText::new(label).small()),
                                )
                                .clicked()
                            {
                                requests.push(Request::Install(Box::new((*e).clone())));
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(4.0);
                            let mut line = e.vendor.clone();
                            if !e.tags.is_empty() {
                                line = format!("{line} · {}", e.tags.join(", "));
                            }
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(line).color(theme.text_dim).small(),
                                )
                                .wrap(),
                            );
                        });
                    }
                });
        });
        ui.separator();
        ui.vertical(|ui| {
            // Long texts (URLs, checksums) wrap instead of widening the dialog.
            ui.set_max_width(ui.available_width());
            let Some(e) = st
                .selected_entry
                .as_ref()
                .and_then(|id| entries.iter().find(|e| &e.id == id))
            else {
                return;
            };
            let versions = loaded
                .catalog
                .plugins
                .iter()
                .filter(|entry| entry.id == e.id)
                .collect::<Vec<_>>();
            if !versions
                .iter()
                .any(|entry| entry.version == st.selected_version)
            {
                st.selected_version = e.version.clone();
            }
            egui::ComboBox::from_id_salt("marketplace-plugin-version")
                .selected_text(&st.selected_version)
                .show_ui(ui, |ui| {
                    for version in &versions {
                        ui.selectable_value(
                            &mut st.selected_version,
                            version.version.clone(),
                            format!(
                                "{}{}",
                                version.version,
                                if version.is_compatible() {
                                    ""
                                } else {
                                    " (incompatible)"
                                }
                            ),
                        );
                    }
                })
                .combo_named(crate::i18n::tr_static("Plugin version"));
            let e = versions
                .iter()
                .find(|entry| entry.version == st.selected_version)
                .copied()
                .unwrap_or(e);
            ui.heading(&e.name);
            for (index, screenshot) in e.screenshots.iter().enumerate() {
                ui.hyperlink_to(format!("View screenshot {}", index + 1), screenshot);
            }
            let mut facts = vec![e.id.clone(), format!("version {}", e.version)];
            if !e.vendor.trim().is_empty() {
                facts.push(e.vendor.clone());
            }
            ui.label(egui::RichText::new(facts.join(" · ")).color(theme.text_dim));
            if let Some(url) = &e.homepage {
                ui.hyperlink_to(egui::RichText::new(url).small(), url);
            }
            ui.horizontal(|ui| {
                let (label, enabled) = action(e);
                if ui
                    .add_enabled(enabled && !busy, egui::Button::new(label))
                    .clicked()
                {
                    requests.push(Request::Install(Box::new((*e).clone())));
                }
                if let Some(job) = &st.install_job {
                    crate::a11y::spinner(ui);
                    ui.label(format!("Installing {}…", job.label));
                }
            });
            if !e.is_compatible() {
                ui.label(
                    egui::RichText::new(format!(
                        "Requires RustPy {} or newer (this is {}).",
                        e.min_ide_version.as_deref().unwrap_or("?"),
                        rustpy_plugins::IDE_VERSION
                    ))
                    .color(theme.error),
                );
            } else if let Some(min) = &e.min_ide_version {
                ui.label(
                    egui::RichText::new(format!("Compatible: RustPy {min} or newer"))
                        .color(theme.text_dim)
                        .small(),
                );
            }
            if !e.description.trim().is_empty() {
                ui.add_space(4.0);
                ui.label(&e.description);
            }
            if !e.tags.is_empty() {
                ui.label(
                    egui::RichText::new(format!("Tags: {}", e.tags.join(", ")))
                        .color(theme.text_dim)
                        .small(),
                );
            }
            ui.add_space(4.0);
            let source = if rustpy_plugins::shipped_bundle(e).is_some() {
                "shipped with RustPy".to_string()
            } else {
                e.download_url.clone()
            };
            ui.label(
                egui::RichText::new(format!("Bundle: {source}\nSHA-256: {}", e.sha256))
                    .color(theme.text_dim)
                    .small(),
            );
        });
    });
}

impl super::IdeApp {
    pub(super) fn github_find_plugin(&mut self, provider: String) {
        self.settings_ui.plugins.tab = PluginsTab::Marketplace;
        self.settings_ui.plugins.search = provider;
        self.settings_ui.page = super::settings_ui::SettingsPage::Plugins;
        self.dialog = super::Dialog::Settings;
    }
}

impl super::IdeApp {
    pub(super) fn data_find_plugin(&mut self, query: String) {
        self.settings_ui.plugins.tab = PluginsTab::Marketplace;
        self.settings_ui.plugins.search = query;
        self.settings_ui.page = super::settings_ui::SettingsPage::Plugins;
        self.dialog = super::Dialog::Settings;
    }
}

impl super::IdeApp {
    pub(super) fn suggested_plugins_for_extension(&self, extension: &str) -> Vec<String> {
        let fallback = rustpy_plugins::Catalog::bundled();
        let catalog = self
            .settings_ui
            .plugins
            .catalog
            .as_ref()
            .map(|loaded| &loaded.catalog)
            .unwrap_or(&fallback);
        let registry = plugin_host::registry();
        catalog
            .listing()
            .into_iter()
            .filter(|entry| {
                entry.is_compatible()
                    && entry.file_extensions.iter().any(|candidate| {
                        candidate
                            .trim_start_matches('.')
                            .eq_ignore_ascii_case(extension)
                    })
                    && !registry
                        .get(&entry.id)
                        .is_some_and(|plugin| plugin.is_active())
            })
            .map(|entry| format!("{} ({})", entry.name, entry.id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_plugins::{ColorSchemeContribution, KeymapContribution};

    fn plugins_with(scheme: Option<&str>, keymap: Option<&str>) -> Contributions {
        let mut c = Contributions::default();
        if let Some(name) = scheme {
            let mut s = ColorScheme::light();
            s.name = name.into();
            c.color_schemes.push(ColorSchemeContribution {
                plugin_id: "com.x".into(),
                path: PathBuf::from("/p"),
                scheme: s,
            });
        }
        if let Some(name) = keymap {
            c.keymaps.push(KeymapContribution {
                plugin_id: "com.x".into(),
                path: PathBuf::from("/p"),
                name: name.into(),
                bindings: Default::default(),
            });
        }
        c
    }

    #[test]
    fn plugin_schemes_follow_their_plugin() {
        let with = plugins_with(Some("Paper"), None);
        let without = Contributions::default();
        // Builtin and user schemes are never touched.
        assert_eq!(
            scheme_after_plugin_change("Dark", true, false, &without),
            None
        );
        assert_eq!(
            scheme_after_plugin_change("Mine", true, true, &without),
            None
        );
        // A plugin scheme is re-read (it may have been updated)…
        assert_eq!(
            scheme_after_plugin_change("Paper", false, false, &with).map(|s| s.name),
            Some("Paper".into())
        );
        // …and replaced by the theme's builtin scheme when its plugin is gone.
        assert_eq!(
            scheme_after_plugin_change("Paper", false, false, &without).map(|s| s.name),
            Some("Light".into())
        );
        assert_eq!(
            scheme_after_plugin_change("Paper", true, false, &without).map(|s| s.name),
            Some("Dark".into())
        );
    }

    #[test]
    fn plugin_keymaps_fall_back_to_default_when_gone() {
        let with = plugins_with(None, Some("Plugin Navigation"));
        let without = Contributions::default();
        assert_eq!(keymap_after_plugin_change("Plugin Navigation", &with), None);
        assert_eq!(
            keymap_after_plugin_change("Plugin Navigation", &without).as_deref(),
            Some("Default")
        );
        for builtin in std::iter::once("").chain(Keymap::preset_names()) {
            assert_eq!(
                keymap_after_plugin_change(builtin, &without),
                None,
                "{builtin:?}"
            );
        }
    }

    #[test]
    fn summaries_and_paths() {
        assert_eq!(
            contribution_summary(&Contributions::default()),
            "no contributions"
        );
        let c = plugins_with(Some("Paper"), Some("Emacs"));
        assert_eq!(contribution_summary(&c), "1 color scheme, 1 keymap");
        assert_eq!(
            disk_path(" '/tmp/x y.rpplugin' "),
            PathBuf::from("/tmp/x y.rpplugin")
        );
        if let Some(home) = dirs::home_dir() {
            assert_eq!(disk_path("~/p"), home.join("p"));
        }
        let e = PluginError::Invalid {
            problems: vec!["a".into(), "b".into()],
        };
        assert_eq!(error_text(&e), "Invalid plugin:\n• a\n• b");
    }

    #[test]
    fn messages_and_labels() {
        let notes =
            vec!["the color scheme \"Paper\" is no longer available, using Dark".to_string()];
        assert_eq!(
            with_notes(Ok("Paper Kit disabled".into()), &notes),
            Ok(
                "Paper Kit disabled; the color scheme \"Paper\" is no longer available, using Dark"
                    .into()
            )
        );
        assert_eq!(with_notes(Err("Cannot".into()), &[]), Err("Cannot".into()));
        assert_eq!(shortened("Demo Kit", 28), "Demo Kit");
        assert_eq!(
            shortened("A very long plugin name indeed", 10),
            "A very lo…"
        );
        assert_eq!(shortened("äöü", 2), "ä…");
        let outcome = |previous: Option<&str>| InstallOutcome {
            id: "com.x".into(),
            version: "2.0".into(),
            previous_version: previous.map(str::to_string),
            dir: PathBuf::from("/p"),
        };
        assert_eq!(installed_text("X", &outcome(None)), "X 2.0 installed");
        assert_eq!(
            installed_text("X", &outcome(Some("1.0"))),
            "X updated from 1.0 to 2.0"
        );
    }
}
