//! The "Install package" quick fix for an unresolved import, and imports
//! resolving again once pip has changed the interpreter's packages.

use super::*;

impl IdeApp {
    pub(super) fn package_workflow_action(&mut self, action: Action) {
        self.configure_package_context();
        self.bottom = Some(BottomTool::Packages);
        if let Some(error) = self.packages_panel.open_workflow(
            action == Action::ShowPythonDependencies,
            action == Action::BuildPythonDistribution,
        ) {
            self.set_status(error);
        }
    }
    pub(super) fn configure_package_context(&mut self) {
        let project_safe_mode = self.project_safe_mode();
        let root = self.project.as_ref().map(|p| p.root.clone());
        let interpreters = self
            .project
            .as_ref()
            .map(|p| p.interpreters.iter().map(|i| i.path.clone()).collect())
            .unwrap_or_default();
        let selected = self
            .project
            .as_ref()
            .and_then(|p| p.interpreter.as_ref().map(|i| i.path.clone()));
        self.packages_panel
            .set_project_context(root, interpreters, selected, !project_safe_mode);
    }

    pub(super) fn package_settings(&mut self, ui: &mut egui::Ui) {
        self.configure_package_context();
        let theme = self.theme.clone();
        let mono = egui::FontId::new(
            self.settings.editor.font_size.max(10.0),
            crate::theme::mono_family(false, false),
        );
        if ui.button("Manage Python Packages").clicked() {
            self.bottom = Some(BottomTool::Packages);
        }
        for event in self.packages_panel.settings_ui(ui, &theme, mono) {
            match event {
                PackagesEvent::Status(message) => self.set_status(message),
            }
        }
    }
    /// Installs the distribution `dist` into the project interpreter, with the
    /// output in the Python Packages tool window.
    pub(super) fn install_package(&mut self, dist: &str) {
        let project_safe_mode = self.project_safe_mode();
        self.configure_package_context();
        // The interpreter may be a project-local environment.
        if project_safe_mode {
            self.set_status(
                "Safe mode: installing packages is disabled until the project is trusted",
            );
            return;
        }
        if self.packages_panel.is_running() {
            self.set_status("A package operation is already running");
            self.bottom = Some(BottomTool::Packages);
            return;
        }
        self.packages_panel.interpreter = self
            .project
            .as_ref()
            .and_then(|p| p.interpreter.as_ref().map(|i| i.path.clone()));
        match self.packages_panel.install(dist) {
            Some(e) => self.set_status(e),
            None => {
                self.set_status(format!("Installing '{dist}'…"));
                self.bottom = Some(BottomTool::Packages);
            }
        }
    }

    /// After a pip task: failed module lookups are forgotten and open files
    /// re-inspected, so "No module named …" clears without Invalidate Caches.
    pub(super) fn packages_changed(&mut self) {
        if let Some(p) = &self.project {
            rustpy_project::framework_catalog::invalidate(
                &p.root,
                p.interpreter.as_ref().map(|i| i.path.as_path()),
            );
            p.analysis.invalidate_modules();
        }
        for t in &mut self.tabs {
            t.state.semantic_version = None;
        }
    }
}

impl IdeApp {
    pub(super) fn add_package_requirement(&mut self, distribution: Option<&str>) {
        let Some(root) = self.project.as_ref().map(|project| project.root.clone()) else {
            return;
        };
        if distribution
            .is_some_and(|name| rustpy_packages::requirements_file::parse_named(name).is_err())
        {
            self.set_status("Invalid package requirement");
            return;
        }
        let path = root.join("requirements.txt");
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.state.path.as_deref() == Some(path.as_path()))
        {
            if let Some(distribution) = distribution {
                let source = self.tabs[index].state.doc.text();
                let newline = if source.contains("\r\n") {
                    "\r\n"
                } else {
                    "\n"
                };
                let addition = format!(
                    "{}{distribution}{newline}",
                    if source.is_empty() || source.ends_with('\n') {
                        ""
                    } else {
                        newline
                    }
                );
                let offset = self.tabs[index].state.doc.len();
                self.tabs[index].state.apply_text_edits(
                    vec![(TextRange::empty(offset), addition)],
                    "Add package requirement",
                );
            }
            self.open_file(&path);
            return;
        }
        let result = (|| -> std::io::Result<()> {
            use std::io::{Read, Write};
            let mut existing = String::new();
            if let Ok(file) = std::fs::File::open(&path) {
                file.take(262145).read_to_string(&mut existing)?;
                if existing.len() > 262144 {
                    return Err(std::io::Error::other(
                        "requirements file exceeds edit budget",
                    ));
                }
            }
            if let Some(distribution) = distribution {
                if rustpy_packages::parse_requirements(&existing)
                    .iter()
                    .any(|r| {
                        rustpy_packages::normalize_name(&r.name)
                            == rustpy_packages::normalize_name(distribution)
                    })
                {
                    return Ok(());
                }
                let newline = if existing.contains("\r\n") {
                    "\r\n"
                } else {
                    "\n"
                };
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                write!(
                    file,
                    "{}{distribution}{newline}",
                    if existing.is_empty() || existing.ends_with('\n') {
                        ""
                    } else {
                        newline
                    }
                )?;
            } else {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.open_file(&path);
            }
            Err(error) => self.set_status(format!("Cannot update requirements.txt: {error}")),
        }
    }
}
