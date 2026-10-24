//! Package manager controls and project dependency workflows.
use super::*;
use crate::a11y::AccessibleCombo;
use rustpy_packages::project::{self, Manager, Operation, Repository};
impl PackagesPanel {
    pub fn open_workflow(&mut self, dependencies: bool, build: bool) -> Option<String> {
        self.show_dependencies = dependencies;
        self.show_preferences = !dependencies;
        if build {
            self.project_operation(Operation::Build)
        } else {
            None
        }
    }
    pub fn set_project_context(
        &mut self,
        root: Option<PathBuf>,
        interpreters: Vec<PathBuf>,
        selected: Option<PathBuf>,
        trusted: bool,
    ) {
        if self.project_root != root && self.is_running() {
            return;
        }
        self.project_trusted = trusted;
        self.known_interpreters = interpreters;
        if let Some(path) = selected.as_ref() {
            if !self.known_interpreters.contains(path) {
                self.known_interpreters.insert(0, path.clone());
            }
        }
        if self.project_root != root {
            self.project_root = root;
            self.preferences = self
                .project_root
                .as_deref()
                .map(Preferences::load)
                .unwrap_or_default();
            self.install_options = self
                .preferences
                .options
                .iter()
                .map(|option| format!("{option:?}"))
                .collect::<Vec<_>>()
                .join(" ");
            self.lock_watcher = LockWatcher::default();
            self.lock_changes.clear();
            self.lock_notice_pending = false;
            self.lock_poll = None;
            self.catalogs.clear();
            self.switch_interpreter(selected.clone());
        } else if self.interpreter.is_none() || self.project_default_interpreter != selected {
            self.switch_interpreter(selected.clone());
        }
        self.project_default_interpreter = selected;
    }
    fn switch_interpreter(&mut self, target: Option<PathBuf>) {
        if self.interpreter == target {
            return;
        }
        if self.is_running() {
            self.error =
                Some("Stop the running package operation before switching interpreters".into());
            return;
        }
        if let Some(previous) = self.loaded_for.take() {
            self.catalogs.insert(previous, self.installed.clone());
        }
        self.interpreter = target.clone();
        self.installed = target
            .as_ref()
            .and_then(|p| self.catalogs.get(p))
            .cloned()
            .unwrap_or_default();
        self.loaded_for = if self.installed.is_empty() {
            None
        } else {
            target
        };
        self.installed_rx = None;
        self.outdated_rx = None;
        self.info_rx = None;
        self.vulns_rx = None;
        self.outdated.clear();
        self.info = None;
        self.selected = None;
        self.vulns.clear();
        self.error = None;
        self.externally_managed = false;
    }
    pub(super) fn project_operation(&mut self, operation: Operation) -> Option<String> {
        if !self.project_trusted && self.project_root.is_some() {
            return Some("Trust the project before running package operations".into());
        }
        if self.is_running() {
            return Some("A package operation is already running".into());
        }
        let Some(interpreter) = self.interpreter.clone() else {
            return Some("No Python interpreter configured".into());
        };
        let root = self
            .project_root
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        let mut prefs = self.preferences.clone();
        match split_options(&self.install_options) {
            Ok(options) => prefs.options = options,
            Err(error) => return Some(error),
        }
        if self.allow_break_system
            && matches!(prefs.manager, Manager::Pip | Manager::Uv | Manager::Auto)
        {
            prefs
                .options
                .push(rustpy_packages::BREAK_SYSTEM_PACKAGES.into());
        }
        self.start_task(
            project::command_plan(&root, &interpreter, &prefs, &operation)
                .and_then(|plan| plan.start()),
        )
    }
    pub(super) fn search_packages(&mut self, query: &str) {
        let query = query.to_owned();
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            let _ =
                tx.send(rustpy_packages::pypi::search_catalog(&query).map_err(|e| e.to_string()));
        });
        self.search_rx = Some(rx);
        self.search_results.clear();
        self.pypi_error = None;
    }
    pub(super) fn poll_project_workflows(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.search_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(items) => self.search_results = items,
                    Err(error) => self.pypi_error = Some(error),
                }
                self.search_rx = None;
                changed = true;
            }
        }
        if self
            .lock_poll
            .is_none_or(|last| last.elapsed() > std::time::Duration::from_secs(2))
        {
            self.lock_poll = Some(std::time::Instant::now());
            if let Some(root) = &self.project_root {
                let updates = self.lock_watcher.poll(root);
                if !updates.is_empty() {
                    self.lock_changes = updates;
                    self.lock_notice_pending = true;
                    changed = true;
                }
            }
        }
        changed
    }
    pub fn take_lock_notification(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.lock_notice_pending) {
            return None;
        }
        Some(
            self.lock_changes
                .iter()
                .filter_map(|p| p.file_name())
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
    fn persist_preferences(&mut self) {
        match split_options(&self.install_options) {
            Ok(options) => self.preferences.options = options,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        }
        if let Some(root) = &self.project_root {
            if let Err(error) = self.preferences.save(root) {
                self.error = Some(error.to_string());
            }
        }
    }
    pub(super) fn workflows_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        events: &mut Vec<PackagesEvent>,
    ) {
        if self.search_rx.is_some() {
            crate::a11y::spinner(ui);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        ui.horizontal_wrapped(|ui| {
            for path in self.known_interpreters.clone() {
                let active = self.interpreter.as_ref() == Some(&path);
                if ui
                    .selectable_label(active, path.display().to_string())
                    .clicked()
                    && !active
                {
                    self.switch_interpreter(Some(path));
                }
            }
        });
        if !self.lock_changes.is_empty() {
            ui.group(|ui| {
                ui.label(format!(
                    "Dependency files changed: {}",
                    self.lock_changes
                        .iter()
                        .filter_map(|p| p.file_name())
                        .map(|s| s.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.is_running() && self.project_trusted,
                            egui::Button::new("Install / sync dependencies"),
                        )
                        .clicked()
                    {
                        if let Some(error) = self.project_operation(Operation::Sync) {
                            events.push(PackagesEvent::Status(error));
                        } else {
                            self.lock_changes.clear();
                        }
                    }
                    if ui.button("Dismiss").clicked() {
                        self.lock_changes.clear();
                    }
                });
            });
        }
        ui.horizontal(|ui| {
            ui.toggle_value(&mut self.show_preferences, "Package settings");
            ui.toggle_value(&mut self.show_dependencies, "Dependencies");
            if ui
                .add_enabled(
                    !self.is_running() && self.project_trusted,
                    egui::Button::new("Install project dependencies"),
                )
                .clicked()
            {
                if let Some(error) = self.project_operation(Operation::Sync) {
                    events.push(PackagesEvent::Status(error));
                }
            }
        });
        if self.show_preferences {
            self.preferences_ui(ui, events);
        }
        if self.show_dependencies {
            self.dependencies_ui(ui, theme, events);
        }
        if !self.search_results.is_empty() {
            let mut lookup = None;
            ui.collapsing("PyPI search results", |ui| {
                for package in &self.search_results {
                    if ui
                        .selectable_label(
                            false,
                            format!("{} {} — {}", package.name, package.version, package.summary),
                        )
                        .clicked()
                    {
                        lookup = Some(package.name.clone());
                    }
                }
            });
            if let Some(name) = lookup {
                self.lookup_pypi(&name);
            }
        }
    }
    fn dependencies_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        events: &mut Vec<PackagesEvent>,
    ) {
        let Some(root) = self.project_root.clone() else {
            return;
        };
        let manifest = project::read_manifest(&root);
        ui.group(|ui| {
            ui.heading("Dependencies");
            for warning in manifest.warnings {
                ui.colored_label(theme.warning, warning);
            }
            let missing = rustpy_packages::unsatisfied(&manifest.dependencies, &self.installed);
            for requirement in manifest.dependencies {
                let issue = missing.iter().find(|(r, _)| r.raw == requirement.raw);
                ui.horizontal(|ui| {
                    ui.label(&requirement.raw);
                    if let Some((_, installed)) = issue {
                        ui.colored_label(
                            theme.warning,
                            installed
                                .as_ref()
                                .map(|version| {
                                    format!("Installed {version} does not satisfy requirement")
                                })
                                .unwrap_or_else(|| "Not installed".into()),
                        );
                        if ui
                            .add_enabled(
                                !self.is_running() && self.project_trusted,
                                egui::Button::new("Install requirement"),
                            )
                            .clicked()
                        {
                            if let Some(error) = self.install(&requirement.raw) {
                                events.push(PackagesEvent::Status(error));
                            }
                        }
                    }
                });
            }
            if ui.button("Check vulnerabilities (OSV)").clicked() {
                self.check_vulnerabilities();
            }
            for package in &self.installed {
                if let Some(advisories) = self
                    .vulns
                    .get(&rustpy_packages::normalize_name(&package.name))
                {
                    for advisory in advisories {
                        ui.horizontal(|ui| {
                            ui.colored_label(
                                theme.error,
                                format!("{} {}: {}", package.name, package.version, advisory.id),
                            );
                            ui.label(&advisory.summary);
                            if let Some(url) = &advisory.reference {
                                ui.hyperlink_to("Advisory", url);
                            }
                        });
                    }
                }
            }
        });
    }
    /// The same interpreter-bound installed table and controls in Settings.
    pub fn settings_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: egui::FontId,
    ) -> Vec<PackagesEvent> {
        ui.heading("Installed Packages");
        self.show_preferences = true;
        ui.push_id("settings_packages", |ui| self.ui(ui, theme, mono))
            .inner
    }
}

/// Shell-like option tokenization, used only to form argv (never a shell).
fn split_options(source: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escape = false;
    for ch in source.chars() {
        if escape {
            current.push(ch);
            escape = false;
        } else if ch == '\\' && quote != Some('\'') {
            escape = true;
        } else if let Some(q) = quote {
            if ch == q {
                quote = None
            } else {
                current.push(ch)
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if escape || quote.is_some() {
        return Err("Unclosed quote or escape in package options".into());
    }
    if !current.is_empty() {
        out.push(current)
    }
    Ok(out)
}

impl PackagesPanel {
    fn preferences_ui(&mut self, ui: &mut egui::Ui, events: &mut Vec<PackagesEvent>) {
        let previous_manager = self.preferences.manager;
        ui.group(|ui| {
            ui.horizontal(|ui| {
                let manager_label = ui.label("Package manager:");
                egui::ComboBox::from_id_salt("package_manager")
                    .selected_text(self.preferences.manager.label())
                    .show_ui(ui, |ui| {
                        for manager in Manager::ALL {
                            ui.selectable_value(&mut self.preferences.manager, manager, manager.label());
                        }
                    })
                    .combo_labelled(&manager_label);
                let mut conda = self.preferences.manager == Manager::Conda;
                if ui.checkbox(&mut conda, "Use Conda package manager").changed() {
                    self.preferences.manager = if conda { Manager::Conda } else { Manager::Auto };
                }
            });
            let manager_name = self.preferences.manager.label().to_ascii_lowercase();
            if !matches!(self.preferences.manager, Manager::Pip | Manager::Auto) {
                let mut executable = self.preferences.executables.get(&manager_name)
                    .map(|path| path.display().to_string()).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label("Manager executable:");
                    if ui.text_edit_singleline(&mut executable).changed() {
                        if executable.trim().is_empty() {
                            self.preferences.executables.remove(&manager_name);
                        } else {
                            self.preferences.executables.insert(manager_name.clone(), executable.into());
                        }
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.preferences.prereleases, "Show early releases / --pre");
                ui.checkbox(&mut self.preferences.user, "Install to user site (--user, pip)");
            });
            ui.horizontal(|ui| {
                ui.label("Install options:");
                ui.add(egui::TextEdit::singleline(&mut self.install_options).hint_text("--no-deps --timeout 30"));
            });
            ui.collapsing("Repositories and credentials", |ui| {
                let mut remove = None;
                for (index, repository) in self.preferences.repositories.iter_mut().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Extra index URL");
                            ui.text_edit_singleline(&mut repository.url);
                            if ui.small_button("Remove").clicked() { remove = Some(index); }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Trusted host");
                            ui.text_edit_singleline(&mut repository.trusted_host);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Username");
                            ui.text_edit_singleline(&mut repository.username);
                            ui.label("Password environment variable");
                            ui.text_edit_singleline(&mut repository.password_environment);
                        });
                    });
                }
                if let Some(index) = remove { self.preferences.repositories.remove(index); }
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.repository_new).hint_text("https://packages.example/simple"));
                    if ui.button("Add repository").clicked() && !self.repository_new.trim().is_empty() {
                        self.preferences.repositories.push(Repository {
                            url: std::mem::take(&mut self.repository_new), ..Default::default()
                        });
                    }
                });
                ui.label("Credentials are read from the named environment variable at execution and omitted from command previews.");
            });
            ui.collapsing("Conda channels", |ui| {
                let mut remove = None;
                for (index, channel) in self.preferences.conda_channels.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(channel);
                        if ui.small_button("Remove").clicked() { remove = Some(index); }
                    });
                }
                if let Some(index) = remove { self.preferences.conda_channels.remove(index); }
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.conda_channel_new);
                    if ui.button("Add channel").clicked() && !self.conda_channel_new.trim().is_empty() {
                        self.preferences.conda_channels.push(std::mem::take(&mut self.conda_channel_new));
                    }
                });
            });
            if ui.button("Save package settings").clicked() { self.persist_preferences(); }
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.bulk).hint_text("requests>=2 flask pandas"));
                for (label, kind) in [("Install (+)", 0), ("Uninstall (−)", 1), ("Upgrade (^)", 2)] {
                    if ui.add_enabled(!self.is_running() && self.project_trusted, egui::Button::new(label)).clicked() {
                        match split_options(&self.bulk) {
                            Ok(specs) if !specs.is_empty() => {
                                let operation = match kind {
                                    0 => Operation::Install(specs), 1 => Operation::Uninstall(specs), _ => Operation::Upgrade(specs)
                                };
                                if let Some(error) = self.project_operation(operation) { events.push(PackagesEvent::Status(error)); }
                            }
                            _ => events.push(PackagesEvent::Status("Enter one or more package specifications".into()))
                        }
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.install_file).hint_text("requirements.txt / package.whl / archive.tar.gz"));
                if ui.add_enabled(!self.is_running() && self.project_trusted, egui::Button::new("Install from file")).clicked() {
                    let path = PathBuf::from(self.install_file.trim());
                    let path = if path.is_absolute() { path } else { self.project_root.clone().unwrap_or_default().join(path) };
                    if !path.is_file() {
                        events.push(PackagesEvent::Status("Select an existing package or requirements file".into()));
                    } else if let Some(error) = self.project_operation(Operation::InstallFile(path)) {
                        events.push(PackagesEvent::Status(error));
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.is_running() && self.project_trusted, egui::Button::new("Build pyproject distribution")).clicked() {
                    if let Some(error) = self.project_operation(Operation::Build) { events.push(PackagesEvent::Status(error)); }
                }
                egui::ComboBox::from_id_salt("setup_task")
                    .selected_text(if self.setup_task.is_empty() { "setup.py task" } else { &self.setup_task })
                    .show_ui(ui, |ui| {
                        for task in project::SETUP_COMMANDS { ui.selectable_value(&mut self.setup_task, task.to_string(), *task); }
                    })
                    .combo_named(crate::i18n::tr_static("setup.py task"));
                if ui.add_enabled(!self.setup_task.is_empty() && !self.is_running() && self.project_trusted, egui::Button::new("Run setup.py task")).clicked() {
                    if let Some(error) = self.project_operation(Operation::Setup(self.setup_task.clone())) { events.push(PackagesEvent::Status(error)); }
                }
                if ui.add_enabled(self.project_trusted, egui::Button::new("Create setup.py")).clicked() {
                    if let Some(root) = &self.project_root {
                        match project::create_setup(root) {
                            Ok(path) => events.push(PackagesEvent::Status(format!("Created {}", path.display()))),
                            Err(error) => events.push(PackagesEvent::Status(error.to_string()))
                        }
                    }
                }
            });
        });
        if self.preferences.manager != previous_manager {
            self.refresh();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interpreter_tabs_never_reuse_another_interpreters_inspection_catalog() {
        let root = tempfile::tempdir().unwrap();
        let a = PathBuf::from("/a/bin/python");
        let b = PathBuf::from("/b/bin/python");
        let mut panel = PackagesPanel::default();
        panel.set_project_context(
            Some(root.path().into()),
            vec![a.clone(), b.clone()],
            Some(a.clone()),
            true,
        );
        panel.testing_installed_catalog(
            a.clone(),
            vec![InstalledPackage {
                name: "only-a".into(),
                version: "1".into(),
                location: None,
                editable: false,
                summary: None,
            }],
        );
        panel.switch_interpreter(Some(b.clone()));
        assert!(panel.inspection_snapshot(Some(&a)).0.is_none());
        assert!(panel.installed.is_empty());
        panel.switch_interpreter(Some(a.clone()));
        assert_eq!(panel.installed[0].name, "only-a");
        panel.set_project_context(
            Some(root.path().into()),
            vec![a, b.clone()],
            Some(b.clone()),
            true,
        );
        assert_eq!(panel.interpreter, Some(b));
    }
    #[test]
    fn changed_lock_notifies_once_and_remains_available_for_explicit_sync() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("poetry.lock");
        std::fs::write(&lock, "old").unwrap();
        let mut panel = PackagesPanel::default();
        panel.set_project_context(Some(root.path().into()), Vec::new(), None, true);
        panel.poll_project_workflows();
        assert!(panel.take_lock_notification().is_none());
        std::fs::write(&lock, "new").unwrap();
        panel.lock_poll = None;
        panel.poll_project_workflows();
        assert_eq!(
            panel.take_lock_notification().as_deref(),
            Some("poetry.lock")
        );
        assert!(panel.take_lock_notification().is_none());
        assert_eq!(panel.lock_changes, vec![lock]);
    }
    #[test]
    fn quoted_bulk_specifiers_remain_single_arguments_and_unclosed_quotes_fail() {
        assert_eq!(
            split_options("'requests>=2, <3' flask --timeout 30").unwrap(),
            vec!["requests>=2, <3", "flask", "--timeout", "30"]
        );
        assert!(split_options("'unterminated").is_err());
    }
}
