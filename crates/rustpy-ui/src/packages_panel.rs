//! Python Packages tool window: installed packages, PyPI lookup, install /
//! upgrade / uninstall with streamed pip output, requirements support.

use crate::a11y::AccessibleCombo;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossbeam_channel::Receiver;
use egui::{Sense, Vec2};
use rustpy_packages::{InstalledPackage, PackageInfo, PipTask, PypiProject};

use crate::theme::UiTheme;
mod workflows;
use rustpy_packages::project::{LockWatcher, Preferences};

/// `(name, installed, latest)` rows from `pip list --outdated`.
type OutdatedResult = Result<Vec<(String, String, String)>, String>;

/// Installed entries and the interpreter's externally-managed (PEP 668) status.
type InstalledResult = Result<(Vec<InstalledPackage>, bool), String>;
/// Installed versions and outdated rows captured for the bound interpreter.
type PackageInspectionSnapshot = (Option<Vec<(String, String)>>, Vec<(String, String, String)>);

pub enum PackagesEvent {
    Status(String),
}

/// Result of a background OSV check: `(package, advisories)` per package.
type VulnResult = Result<Vec<(String, Vec<rustpy_packages::Vulnerability>)>, String>;

#[derive(Default)]
pub struct PackagesPanel {
    project_root: Option<PathBuf>,
    project_default_interpreter: Option<PathBuf>,
    lock_notice_pending: bool,
    known_interpreters: Vec<PathBuf>,
    preferences: Preferences,
    lock_watcher: LockWatcher,
    lock_poll: Option<std::time::Instant>,
    lock_changes: Vec<PathBuf>,
    show_preferences: bool,
    show_dependencies: bool,
    install_options: String,
    install_file: String,
    bulk: String,
    setup_task: String,
    repository_new: String,
    conda_channel_new: String,
    project_trusted: bool,
    catalogs: HashMap<PathBuf, Vec<InstalledPackage>>,
    search_results: Vec<rustpy_packages::PypiSummary>,
    search_rx: Option<Receiver<Result<Vec<rustpy_packages::PypiSummary>, String>>>,
    accessibility_blocked: bool,
    pub interpreter: Option<PathBuf>,
    installed: Vec<InstalledPackage>,
    installed_rx: Option<Receiver<InstalledResult>>,
    loaded_for: Option<PathBuf>,
    error: Option<String>,
    filter: String,
    selected: Option<String>,
    info: Option<PackageInfo>,
    info_rx: Option<Receiver<Result<PackageInfo, String>>>,
    outdated: HashMap<String, String>,
    outdated_rx: Option<Receiver<OutdatedResult>>,
    pypi: Option<PypiProject>,
    pypi_rx: Option<Receiver<Result<PypiProject, String>>>,
    pypi_error: Option<String>,
    install_version: Option<String>,
    task: Option<PipTask>,
    process_epoch: u64,
    log: Vec<String>,
    externally_managed: bool,
    pub allow_break_system: bool,
    /// Known vulnerabilities per normalized package name (OSV).
    vulns: HashMap<String, Vec<rustpy_packages::Vulnerability>>,
    vulns_rx: Option<Receiver<VulnResult>>,
    vulns_status: Option<String>,
    /// Run the vulnerability check as soon as the installed list is loaded.
    pub vuln_check_pending: bool,
    /// Set when a pip task finishes, to its exit code, for callers that started it
    /// with `adopt_task`.
    finished: Option<Option<i32>>,
    /// A pip task finished: installed packages may have changed (see
    /// `take_packages_changed`).
    packages_changed: bool,
    /// The last pip task that exited with an error: (what it did, last output lines).
    failure: Option<(String, String)>,
}

impl PackagesPanel {
    pub(crate) fn set_accessibility_owner(&mut self, available: bool) {
        self.accessibility_blocked = !available;
    }

    #[cfg(test)]
    pub(crate) fn testing_installed_catalog(
        &mut self,
        interpreter: PathBuf,
        items: Vec<InstalledPackage>,
    ) {
        self.interpreter = Some(interpreter.clone());
        self.loaded_for = Some(interpreter);
        self.installed = items;
    }

    /// Observational identity of the retained process owner; does not poll it.
    pub(crate) fn quit_process_state(&self) -> String {
        format!(
            "{:?}",
            (
                self.process_epoch,
                self.task.as_ref().map(|owner| owner as *const _)
            )
        )
    }
    pub(crate) fn stop_quit_process(&mut self) {
        if let Some(task) = &mut self.task {
            task.stop();
        }
    }

    pub fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|t| t.is_running())
    }

    /// A pip task that failed since the last call (for a notification).
    pub fn take_failure(&mut self) -> Option<(String, String)> {
        self.failure.take()
    }

    /// Immutable, interpreter-bound inspection inputs. Never probe or refresh here.
    pub(crate) fn inspection_snapshot(
        &self,
        interpreter: Option<&std::path::Path>,
    ) -> PackageInspectionSnapshot {
        if interpreter.is_none()
            || self.loaded_for.as_deref() != interpreter
            || self.installed_rx.is_some()
            || self.error.is_some()
        {
            return (None, Vec::new());
        }
        let mut installed = self
            .installed
            .iter()
            .map(|p| (p.name.clone(), p.version.clone()))
            .collect::<Vec<_>>();
        installed.sort();
        let mut advisories = Vec::new();
        for package in &self.installed {
            if let Some(vulnerabilities) = self
                .vulns
                .get(&rustpy_packages::normalize_name(&package.name))
            {
                for vulnerability in vulnerabilities {
                    if vulnerability.affects(&package.version) {
                        advisories.push((
                            package.name.clone(),
                            package.version.clone(),
                            vulnerability.id.clone(),
                        ));
                    }
                }
            }
        }
        advisories.sort();
        (Some(installed), advisories)
    }

    pub fn installed(&self) -> &[InstalledPackage] {
        &self.installed
    }

    pub fn refresh(&mut self) {
        let Some(interp) = self.interpreter.clone() else {
            return;
        };
        self.loaded_for = Some(interp.clone());
        self.error = None;
        let mut preferences = self.preferences.clone();
        preferences.manager = rustpy_packages::project::effective_manager(
            self.project_root.as_deref().unwrap_or(Path::new(".")),
            &interp,
            &preferences,
        );
        let conda = preferences.manager == rustpy_packages::project::Manager::Conda;
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            let items = if preferences.manager == rustpy_packages::project::Manager::Conda {
                rustpy_packages::project::conda_installed(&interp, &preferences)
            } else {
                rustpy_packages::list_installed(&interp)
            };
            let result = items
                .map(|items| (items, rustpy_packages::is_externally_managed(&interp)))
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.installed_rx = Some(rx);
        if conda {
            self.outdated.clear();
            self.outdated_rx = None;
            return;
        }
        let (tx2, rx2) = crossbeam_channel::unbounded();
        let interp2 = self.interpreter.clone().unwrap();
        std::thread::spawn(move || {
            let _ = tx2.send(rustpy_packages::outdated(&interp2).map_err(|e| e.to_string()));
        });
        self.outdated_rx = Some(rx2);
    }

    fn start_task(&mut self, task: anyhow::Result<PipTask>) -> Option<String> {
        match task {
            Ok(t) => {
                self.log.clear();
                self.log.push(format!("$ {}", t.command_line));
                self.process_epoch = crate::app::window_lifecycle::next_process_owner();
                self.task = Some(t);
                None
            }
            Err(e) => Some(e.to_string()),
        }
    }

    /// Shows a pip task started elsewhere (the New Project setup) in this tool
    /// window; `take_finished` reports its exit code.
    pub fn adopt_task(&mut self, task: PipTask) {
        self.finished = None;
        self.log.clear();
        self.log.push(format!("$ {}", task.command_line));
        self.process_epoch = crate::app::window_lifecycle::next_process_owner();
        self.task = Some(task);
    }

    /// The exit code of the pip task that finished since the last call.
    pub fn take_finished(&mut self) -> Option<Option<i32>> {
        self.finished.take()
    }

    /// Installs `spec` (`name` or `name==version`) in the background; the
    /// error message when pip cannot be started.
    pub fn upgrade_from_editor(&mut self, name: &str) -> Option<String> {
        self.project_operation(rustpy_packages::project::Operation::Upgrade(vec![
            name.into()
        ]))
    }
    pub fn install(&mut self, spec: &str) -> Option<String> {
        if self.project_root.is_some() {
            return self.project_operation(rustpy_packages::project::Operation::Install(vec![
                spec.into(),
            ]));
        }
        let Some(interp) = self.interpreter.clone() else {
            return Some("No Python interpreter configured for the project".into());
        };
        let args: Vec<String> = if self.allow_break_system {
            vec![rustpy_packages::BREAK_SYSTEM_PACKAGES.into()]
        } else {
            Vec::new()
        };
        self.start_task(rustpy_packages::install(&interp, spec, &args))
    }

    /// Whether a pip task (install, upgrade, uninstall) finished since the last
    /// call: imports that failed may resolve now, also for an interpreter
    /// outside the project, whose `site-packages` the file watcher does not see.
    pub fn take_packages_changed(&mut self) -> bool {
        std::mem::take(&mut self.packages_changed)
    }

    pub fn install_requirements(&mut self, path: &Path) -> Option<String> {
        if self.project_root.is_some() {
            return self.project_operation(rustpy_packages::project::Operation::InstallFile(
                path.into(),
            ));
        }
        let interp = self.interpreter.clone()?;
        let args: Vec<String> = if self.allow_break_system {
            vec![rustpy_packages::BREAK_SYSTEM_PACKAGES.into()]
        } else {
            Vec::new()
        };
        self.start_task(rustpy_packages::install_requirements_with(
            &interp, path, &args,
        ))
    }

    /// Drains background work; returns true when something changed.
    /// Queries OSV for the installed packages in the background.
    pub fn check_vulnerabilities(&mut self) {
        if self.vulns_rx.is_some() || self.installed.is_empty() {
            return;
        }
        let pkgs: Vec<(String, String)> = self
            .installed
            .iter()
            .map(|p| (p.name.clone(), p.version.clone()))
            .collect();
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            let _ =
                tx.send(rustpy_packages::check_vulnerabilities(&pkgs).map_err(|e| e.to_string()));
        });
        self.vulns_rx = Some(rx);
        self.vulns_status = Some(format!(
            "Checking {} package(s) against the OSV database…",
            self.installed.len()
        ));
    }

    /// Number of vulnerable packages after the last check.
    pub fn vulnerable_count(&self) -> usize {
        self.vulns.values().filter(|v| !v.is_empty()).count()
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        changed |= self.poll_project_workflows();
        if let Some(rx) = &self.vulns_rx {
            if let Ok(res) = rx.try_recv() {
                match res {
                    Ok(list) => {
                        self.vulns = list
                            .into_iter()
                            .map(|(n, v)| (rustpy_packages::normalize_name(&n), v))
                            .collect();
                        let n = self.vulnerable_count();
                        let total: usize = self.vulns.values().map(Vec::len).sum();
                        self.vulns_status = Some(if n == 0 {
                            "No known vulnerabilities".into()
                        } else {
                            format!(
                                "{total} known vulnerabilit{} in {n} package(s)",
                                if total == 1 { "y" } else { "ies" }
                            )
                        });
                    }
                    Err(e) => self.vulns_status = Some(format!("Vulnerability check failed: {e}")),
                }
                self.vulns_rx = None;
                changed = true;
            }
        }
        if let Some(rx) = &self.installed_rx {
            if let Ok(res) = rx.try_recv() {
                match res {
                    Ok((list, managed)) => {
                        self.installed = list;
                        self.externally_managed = managed;
                    }
                    Err(e) => self.error = Some(e),
                }
                self.installed_rx = None;
                changed = true;
                if self.vuln_check_pending {
                    self.vuln_check_pending = false;
                    self.check_vulnerabilities();
                }
            }
        }
        if let Some(rx) = &self.outdated_rx {
            if let Ok(res) = rx.try_recv() {
                if let Ok(list) = res {
                    self.outdated = list
                        .into_iter()
                        .map(|(n, _, latest)| (rustpy_packages::normalize_name(&n), latest))
                        .collect();
                }
                self.outdated_rx = None;
                changed = true;
            }
        }
        if let Some(rx) = &self.info_rx {
            if let Ok(res) = rx.try_recv() {
                self.info = res.ok();
                self.info_rx = None;
                changed = true;
            }
        }
        if let Some(rx) = &self.pypi_rx {
            if let Ok(res) = rx.try_recv() {
                match res {
                    Ok(p) => {
                        self.install_version = p
                            .versions
                            .iter()
                            .find(|version| {
                                self.preferences.prereleases || {
                                    let parsed = rustpy_packages::parse_version(version);
                                    parsed.pre.is_none() && parsed.dev.is_none()
                                }
                            })
                            .cloned();
                        self.pypi = Some(p);
                        self.pypi_error = None;
                    }
                    Err(e) => {
                        self.pypi = None;
                        self.pypi_error = Some(e);
                    }
                }
                self.pypi_rx = None;
                changed = true;
            }
        }
        if let Some(t) = &mut self.task {
            let lines = t.poll();
            if !lines.is_empty() {
                self.log.extend(lines);
                changed = true;
            }
            if !t.is_running() {
                let code = t.exit_code();
                self.finished = Some(code);
                self.packages_changed = true;
                let label = t.kind.label();
                self.log.push(match code {
                    Some(0) => format!("{label}: finished successfully"),
                    Some(c) => format!("{label}: exited with code {c}"),
                    None => format!("{label}: stopped"),
                });
                self.packages_changed |= code == Some(0);
                if rustpy_packages::is_externally_managed_error(&self.log) {
                    self.log.push("This interpreter is externally managed (PEP 668). Create a virtual environment, or enable 'Allow --break-system-packages' below.".into());
                }
                if code.is_some_and(|c| c != 0) {
                    let tail: Vec<&str> = self
                        .log
                        .iter()
                        .rev()
                        .filter(|l| !l.trim().is_empty())
                        .take(4)
                        .map(String::as_str)
                        .collect();
                    self.failure = Some((
                        label.to_string(),
                        tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
                    ));
                }
                self.task = None;
                self.refresh();
                changed = true;
            }
        }
        changed
    }

    fn select(&mut self, name: &str) {
        self.selected = Some(name.to_string());
        self.info = None;
        let Some(interp) = self.interpreter.clone() else {
            return;
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        let n = name.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(rustpy_packages::package_info(&interp, &n).map_err(|e| e.to_string()));
        });
        self.info_rx = Some(rx);
    }

    fn lookup_pypi(&mut self, name: &str) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let n = name.trim().to_string();
        std::thread::spawn(move || {
            let _ = tx.send(rustpy_packages::pypi_project(&n).map_err(|e| e.to_string()));
        });
        self.pypi_rx = Some(rx);
        self.pypi = None;
        self.pypi_error = None;
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: egui::FontId,
    ) -> Vec<PackagesEvent> {
        let mut events = Vec::new();
        self.workflows_ui(ui, theme, &mut events);
        let Some(interp) = self.interpreter.clone() else {
            ui.label(
                egui::RichText::new("No Python interpreter configured for this project.")
                    .color(theme.text_dim),
            );
            return events;
        };
        if self.loaded_for.as_deref() != Some(interp.as_path()) && self.installed_rx.is_none() {
            self.refresh();
        }
        if self.installed_rx.is_some()
            || self.outdated_rx.is_some()
            || self.info_rx.is_some()
            || self.pypi_rx.is_some()
            || self.is_running()
        {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search installed or type a PyPI package name…").desired_width(300.0));
            let enter = (r.lost_focus() || r.has_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("Look up on PyPI").clicked() || enter) && !self.filter.trim().is_empty() {
                let q = self.filter.trim().to_string();
                self.search_packages(&q);
            }
            if ui.button("⟳").on_hover_text("Refresh").clicked() {
                self.refresh();
            }
            if ui.add_enabled(self.vulns_rx.is_none() && !self.installed.is_empty(), egui::Button::new("Check vulnerabilities")).on_hover_text("Look up the installed versions in the OSV database (osv.dev)").clicked() {
                self.check_vulnerabilities();
            }

            if self.is_running() && ui.button(egui::RichText::new("■ Stop").color(theme.error)).clicked() {
                if let Some(t) = &mut self.task {
                    t.stop();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.externally_managed {
                    ui.checkbox(&mut self.allow_break_system, "Allow --break-system-packages").on_hover_text("This interpreter is externally managed (PEP 668). Prefer a virtual environment.");
                }
                let via = rustpy_packages::package_manager_cached(&interp).map(|m| format!(" · via {}", m.label())).unwrap_or_default();
                // Elided to the room left: a long interpreter path must not cover the buttons.
                ui.add(egui::Label::new(egui::RichText::new(format!("{}{via}", interp.display())).color(theme.text_dim).small()).truncate());
            });
        });
        if let Some(s) = &self.vulns_status {
            let bad = self.vulnerable_count() > 0 || s.starts_with("Vulnerability check failed");
            ui.label(
                egui::RichText::new(s)
                    .color(if bad { theme.error } else { theme.text_dim })
                    .small(),
            );
        }
        ui.separator();
        if let Some(e) = &self.error {
            ui.colored_label(theme.error, e);
        }
        // Output log at the bottom while / after running pip.
        if !self.log.is_empty() {
            egui::Panel::bottom("pip_log")
                .resizable(true)
                .default_size(120.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("pip_log_scroll")
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for l in &self.log {
                                ui.label(
                                    egui::RichText::new(l).font(mono.clone()).color(theme.text),
                                );
                            }
                        });
                });
        }
        // Installed list (left) and details (right).
        egui::Panel::left("pkg_list")
            .resizable(true)
            .default_size(420.0)
            .min_size(240.0)
            .show(ui, |ui| {
                let filter = self.filter.trim().to_lowercase();
                let mut select: Option<String> = None;
                let mut upgrade: Option<String> = None;
                let mut uninstall: Option<String> = None;
                egui::ScrollArea::vertical()
                    .id_salt("pkg_list_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.installed.is_empty() && self.installed_rx.is_some() {
                            ui.label(
                                egui::RichText::new("Loading installed packages…")
                                    .color(theme.text_dim),
                            );
                        }
                        let items: Vec<(String, String)> = self
                            .installed
                            .iter()
                            .filter(|p| {
                                filter.is_empty() || p.name.to_lowercase().contains(&filter)
                            })
                            .map(|p| (p.name.clone(), p.version.clone()))
                            .collect();
                        for (name, version) in items {
                            let latest = self
                                .outdated
                                .get(&rustpy_packages::normalize_name(&name))
                                .cloned();
                            let selected = self.selected.as_deref() == Some(name.as_str());
                            let (rect, _) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 22.0),
                                Sense::hover(),
                            );
                            let id = ui.id().with((
                                "installed-package",
                                &self.loaded_for,
                                &self.interpreter,
                                rustpy_packages::normalize_name(&name),
                                &version,
                            ));
                            let resp = ui.interact(rect, id, Sense::click());
                            resp.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::SelectableLabel,
                                    !self.accessibility_blocked,
                                    selected,
                                    format!("{name} {version}"),
                                )
                            });
                            if selected {
                                ui.painter().rect_filled(rect, 2.0, theme.selection_bg);
                            } else if resp.hovered() {
                                ui.painter().rect_filled(rect, 2.0, theme.hover_bg);
                            }
                            ui.painter().text(
                                egui::Pos2::new(rect.left() + 8.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                &name,
                                egui::FontId::proportional(13.0),
                                if selected && !theme.high_contrast {
                                    egui::Color32::WHITE
                                } else {
                                    theme.text
                                },
                            );
                            ui.painter().text(
                                egui::Pos2::new(rect.right() - 120.0, rect.center().y),
                                egui::Align2::RIGHT_CENTER,
                                &version,
                                egui::FontId::proportional(13.0),
                                theme.text_dim,
                            );
                            let nv = self
                                .vulns
                                .get(&rustpy_packages::normalize_name(&name))
                                .map(Vec::len)
                                .unwrap_or(0);
                            if nv > 0 {
                                let badge = egui::Rect::from_min_size(
                                    egui::Pos2::new(rect.right() - 196.0, rect.top() + 3.0),
                                    Vec2::new(28.0, 16.0),
                                );
                                ui.painter().rect_filled(badge, 8.0, theme.error);
                                ui.painter().text(
                                    badge.center(),
                                    egui::Align2::CENTER_CENTER,
                                    format!("!{nv}"),
                                    egui::FontId::proportional(11.0),
                                    egui::Color32::WHITE,
                                );
                            }
                            if let Some(l) = &latest {
                                let r = egui::Rect::from_min_size(
                                    egui::Pos2::new(rect.right() - 112.0, rect.top() + 2.0),
                                    Vec2::new(104.0, 18.0),
                                );
                                let b = ui.put(
                                    r,
                                    egui::Button::new(
                                        egui::RichText::new(format!("update {l}")).small(),
                                    ),
                                );
                                if b.on_hover_text("Upgrade to the latest version").clicked() {
                                    upgrade = Some(name.clone());
                                }
                            }
                            if !self.accessibility_blocked && resp.clicked() {
                                select = Some(name.clone());
                            }
                            crate::a11y::keyboard_context_menu_fallback(
                                &resp,
                                !self.accessibility_blocked,
                            )
                            .show(|ui| {
                                if ui.button("Upgrade").clicked() {
                                    upgrade = Some(name.clone());
                                    ui.close();
                                }
                                if ui.button("Uninstall").clicked() {
                                    uninstall = Some(name.clone());
                                    ui.close();
                                }
                            });
                        }
                    });
                if let Some(n) = select {
                    self.select(&n);
                }
                if let Some(n) = upgrade {
                    if let Some(e) = self
                        .project_operation(rustpy_packages::project::Operation::Upgrade(vec![n]))
                    {
                        events.push(PackagesEvent::Status(e));
                    }
                }
                if let Some(n) = uninstall {
                    if let Some(e) = self
                        .project_operation(rustpy_packages::project::Operation::Uninstall(vec![n]))
                    {
                        events.push(PackagesEvent::Status(e));
                    }
                }
            });
        egui::ScrollArea::vertical().id_salt("pkg_details").auto_shrink([false, false]).show(ui, |ui| {
            // PyPI lookup result.
            if self.pypi_rx.is_some() {
                ui.label(egui::RichText::new("Looking up on PyPI…").color(theme.text_dim));
            }
            if let Some(e) = &self.pypi_error {
                ui.colored_label(theme.error, format!("PyPI: {e}"));
            }
            let mut install: Option<String> = None;
            if let Some(p) = self.pypi.clone() {
                ui.label(egui::RichText::new(format!("{} (PyPI)", p.name)).strong().size(16.0));
                if !p.summary.is_empty() {
                    ui.label(&p.summary);
                }
                if !p.home_page.is_empty(){ui.hyperlink_to("Documentation / project homepage", &p.home_page);}
                ui.horizontal(|ui| {
                    let version_label = ui.label("Version:");
                    let cur = self.install_version.clone().unwrap_or_default();
                    egui::ComboBox::from_id_salt("pypi_version").selected_text(&cur).show_ui(ui, |ui| {
                        for v in p.versions.iter().filter(|v|self.preferences.prereleases || {let version=rustpy_packages::parse_version(v);version.pre.is_none()&&version.dev.is_none()}).take(100) {
                            if ui.selectable_label(cur == *v, v).clicked() {
                                self.install_version = Some(v.clone());
                            }
                        }
                    }).combo_labelled(&version_label);
                    if ui.add_enabled(!self.is_running(), egui::Button::new("Install package")).clicked() {
                        install = Some(match &self.install_version {
                            Some(v) => format!("{}=={}", p.name, v),
                            None => p.name.clone(),
                        });
                    }
                });
                for (k, v) in [("Author", &p.author), ("License", &p.license), ("Requires Python", &p.requires_python), ("Home page", &p.home_page)] {
                    if !v.is_empty() {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("{k}:")).color(theme.text_dim));
                            ui.label(v);
                        });
                    }
                }
                if !p.requires_dist.is_empty() {
                    ui.label(egui::RichText::new(format!("Requires: {}", p.requires_dist.iter().take(12).cloned().collect::<Vec<_>>().join(", "))).color(theme.text_dim).small());
                }
                ui.separator();
            }
            if let Some(spec) = install {
                if let Some(e) = self.install(&spec) {
                    events.push(PackagesEvent::Status(e));
                }
            }
            // Installed package details.
            let mut vuln_upgrade: Option<(String, String)> = None;
            match (&self.selected, &self.info) {
                (Some(name), Some(info)) => {
                    ui.label(egui::RichText::new(format!("{} {}", info.name, info.version)).strong().size(16.0));
                    if !info.summary.is_empty() {
                        ui.label(&info.summary);
                    }
                    if !info.home_page.is_empty(){ui.hyperlink_to("Documentation / project homepage", &info.home_page);}
                    for (k, v) in [("Author", &info.author), ("License", &info.license), ("Home page", &info.home_page)] {
                        if !v.is_empty() {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(format!("{k}:")).color(theme.text_dim));
                                ui.label(v);
                            });
                        }
                    }
                    if let Some(loc) = &info.location {
                        ui.label(egui::RichText::new(format!("Location: {}", loc.display())).color(theme.text_dim).small());
                    }
                    if !info.requires.is_empty() {
                        ui.label(egui::RichText::new(format!("Requires: {}", info.requires.join(", "))).color(theme.text_dim).small());
                    }
                    if !info.required_by.is_empty() {
                        ui.label(egui::RichText::new(format!("Required by: {}", info.required_by.join(", "))).color(theme.text_dim).small());
                    }
                    if let Some(vs) = self.vulns.get(&rustpy_packages::normalize_name(name)).filter(|v| !v.is_empty()) {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(format!("{} known vulnerabilit{}", vs.len(), if vs.len() == 1 { "y" } else { "ies" })).strong().color(theme.error));
                        if let Some(up) = rustpy_packages::suggested_upgrade(&info.version, vs) {
                            if ui.button(format!("Upgrade to {up} (fixes all listed)")).clicked() {
                                vuln_upgrade = Some((info.name.clone(), up));
                            }
                        }
                        let mut sorted: Vec<&rustpy_packages::Vulnerability> = vs.iter().collect();
                        sorted.sort_by_key(|v| std::cmp::Reverse(v.severity.as_deref().map(rustpy_packages::severity_rank).unwrap_or(0)));
                        for v in sorted {
                            egui::Frame::new().fill(theme.panel_bg_darker).corner_radius(4.0).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(&v.id).monospace().strong());
                                    if let Some(sev) = &v.severity {
                                        let c = if rustpy_packages::severity_rank(sev) >= 3 { theme.error } else { theme.warning };
                                        ui.label(egui::RichText::new(sev).color(c).small());
                                    }
                                });
                                if !v.summary.is_empty() {
                                    ui.label(&v.summary);
                                }
                                if !v.fixed_versions.is_empty() {
                                    ui.label(egui::RichText::new(format!("Fixed in: {}", v.fixed_versions.join(", "))).color(theme.text_dim).small());
                                }
                                if let Some(r) = &v.reference {
                                    ui.hyperlink_to(egui::RichText::new(r).small(), r);
                                }
                            });
                        }
                    }
                    let _ = name;
                }
                (Some(name), None) => {
                    ui.label(egui::RichText::new(format!("{name}: loading details…")).color(theme.text_dim));
                }
                _ => {
                    if self.pypi.is_none() {
                        ui.label(egui::RichText::new("Select an installed package to see its details, or look up a package on PyPI to install it.").color(theme.text_dim));
                    }
                }
            }
            if let Some((name, version)) = vuln_upgrade {
                if let Some(e) = self.install(&format!("{name}=={version}")) {
                    events.push(PackagesEvent::Status(e));
                }
            }
        });
        events
    }
}
