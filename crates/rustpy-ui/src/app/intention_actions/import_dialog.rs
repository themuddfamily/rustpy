//! Stateful import choices retain the initiating document/version/caret.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_analysis::intentions::import_actions::{self, ImportRequest};

pub(super) struct ImportDialog {
    owner: Owner,
    request: ImportRequest,
    value: String,
    interpreter: String,
    versions: Vec<String>,
    versions_rx: Option<crossbeam_channel::Receiver<Vec<String>>>,
}
impl IdeApp {
    pub(super) fn begin_import_request(&mut self, request: ImportRequest) {
        let Some(owner) = self.intention_owner() else {
            return;
        };
        if let ImportRequest::Create { path, package } = &request {
            if let Err(error) = self.create_import_target(path, *package) {
                self.set_status(error)
            }
            return;
        }
        let mut versions_rx = None;
        if let ImportRequest::Install { distribution } = &request {
            let (tx, rx) = crossbeam_channel::bounded(1);
            let name = distribution.clone();
            std::thread::spawn(move || {
                let versions = rustpy_packages::pypi_versions(&name).unwrap_or_default();
                let _ = tx.send(versions);
            });
            versions_rx = Some(rx);
        }
        let value = match &request {
            ImportRequest::Alias { binding } => self
                .active
                .and_then(|i| self.analysis_file(i))
                .map(|(_, file)| file.scopes.binding(*binding).name.to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
        let interpreter = self
            .project
            .as_ref()
            .and_then(|p| p.interpreter.as_ref())
            .map(|i| i.path.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.intention_menu.import = Some(ImportDialog {
            owner,
            request,
            value,
            interpreter,
            versions: Vec::new(),
            versions_rx,
        });
    }
    fn create_import_target(
        &mut self,
        path: &std::path::Path,
        package: bool,
    ) -> Result<(), String> {
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .ok_or("Open a project before creating a module")?;
        if !path.starts_with(&root)
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("Module destination is outside the project".into());
        }
        let parent = path.parent().ok_or("Module has no parent directory")?;
        let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
        // Validate every existing ancestor BEFORE any filesystem mutation. A
        // project-relative spelling must not traverse a symlink outside it.
        let mut current = root.clone();
        for component in parent
            .strip_prefix(&root)
            .map_err(|_| "Module destination is outside the project")?
            .components()
        {
            current.push(component);
            match std::fs::symlink_metadata(&current) {
                Ok(_) => {
                    let resolved = current.canonicalize().map_err(|error| error.to_string())?;
                    if !resolved.starts_with(&canonical_root) || !resolved.is_dir() {
                        return Err(
                            "Module parent resolves outside the project or is not a directory"
                                .into(),
                        );
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
            if current.with_extension("py").is_file() {
                return Err("A module file already occupies a parent package name".into());
            }
        }
        // Existing namespace packages remain namespaces. Only the explicitly
        // requested package receives an __init__.py, at `path` itself.
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
        if let Some(project) = &self.project {
            project.analysis.invalidate_modules();
        }
        self.open_file(path);
        self.set_status(if package {
            "Package created"
        } else {
            "Module created"
        });
        Ok(())
    }
    pub(super) fn import_request_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.intention_menu.import.take() else {
            return;
        };
        if !self.intention_owner_current(&dialog.owner) {
            self.set_status("Import target changed; reopen context actions");
            return;
        }
        if let Some(rx) = &dialog.versions_rx {
            if let Ok(versions) = rx.try_recv() {
                dialog.versions = versions;
                dialog.versions_rx = None;
            }
        }
        let mut apply = false;
        let mut cancel = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let install = matches!(dialog.request, ImportRequest::Install { .. });
        egui::Window::new(if install {
            "Install missing import"
        } else {
            "Import as alias"
        })
        .collapsible(false)
        .resizable(false)
        .show_modal(ctx, |ui| {
            match &dialog.request {
                ImportRequest::Alias { .. } => {
                    ui.label("Alias (references to this binding will be updated)");
                    ui.text_edit_singleline(&mut dialog.value);
                }
                ImportRequest::Install { distribution } => {
                    ui.label(format!("Package: {distribution}"));
                    let interpreter_label = ui.label("Python interpreter");
                    egui::ComboBox::from_id_salt("import-interpreter")
                        .selected_text(&dialog.interpreter)
                        .show_ui(ui, |ui| {
                            if let Some(project) = &self.project {
                                for interpreter in &project.interpreters {
                                    let path = interpreter.path.to_string_lossy().into_owned();
                                    ui.selectable_value(
                                        &mut dialog.interpreter,
                                        path.clone(),
                                        path,
                                    );
                                }
                            }
                        })
                        .combo_labelled(&interpreter_label);
                    ui.text_edit_singleline(&mut dialog.interpreter);
                    let version_label = ui.label("Version (empty installs latest)");
                    egui::ComboBox::from_id_salt("import-version")
                        .selected_text(if dialog.value.is_empty() {
                            "Latest"
                        } else {
                            &dialog.value
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut dialog.value, String::new(), "Latest");
                            for version in &dialog.versions {
                                ui.selectable_value(&mut dialog.value, version.clone(), version);
                            }
                        })
                        .combo_labelled(&version_label);
                    ui.text_edit_singleline(&mut dialog.value);
                    if dialog.versions_rx.is_some() {
                        crate::a11y::spinner(ui);
                        ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    }
                }
                _ => {}
            }
            ui.horizontal(|ui| {
                apply = ui
                    .button(if install { "Install" } else { "Apply" })
                    .clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
        if cancel {
            self.focus_editor = true;
            return;
        }
        if !apply {
            self.intention_menu.import = Some(dialog);
            return;
        }
        match dialog.request {
            ImportRequest::Alias { binding } => {
                if let Some((_, file)) = self.active.and_then(|i| self.analysis_file(i)) {
                    if let Some(intention) =
                        import_actions::alias_import(&file, binding, dialog.value.trim())
                    {
                        if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
                            tab.state
                                .apply_text_edits(intention.edits, &intention.title);
                        }
                        self.set_status(intention.title);
                    } else {
                        self.set_status("Alias is invalid, conflicts with an existing binding, or needs a dotted-import conversion");
                    }
                }
            }
            ImportRequest::Install { distribution } => {
                if self.safe_mode {
                    self.set_status("Trust the project before installing packages");
                    return;
                }
                if self.packages_panel.is_running() {
                    self.set_status("A package operation is already running");
                    return;
                }
                let version = dialog.value.trim();
                if !version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_' | '!'))
                {
                    self.set_status("Invalid package version");
                    return;
                }
                self.packages_panel.interpreter = Some(PathBuf::from(dialog.interpreter));
                let spec = if version.is_empty() {
                    distribution
                } else {
                    format!("{distribution}=={version}")
                };
                if let Some(error) = self.packages_panel.install(&spec) {
                    self.set_status(error)
                } else {
                    self.bottom = Some(BottomTool::Packages);
                    self.set_status(format!("Installing {spec}…"));
                }
            }
            _ => {}
        }
        self.focus_editor = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_semantic::scope::BindingKind;
    fn app(source: &str) -> (IdeApp, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.py");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        app.open_file(&path);
        (app, root)
    }
    #[test]
    fn typed_alias_request_opens_bound_document_dialog_and_expires_after_edit() {
        let (mut app, _root) = app("import os\nprint(os.name)\n");
        let (_, file) = app.analysis_file(app.active.unwrap()).unwrap();
        let id = file
            .scopes
            .bindings
            .iter()
            .enumerate()
            .find(|(_, b)| matches!(b.kind, BindingKind::Import(_)))
            .map(|(i, _)| rustpy_semantic::scope::BindingId(i as u32))
            .unwrap();
        app.begin_import_request(ImportRequest::Alias { binding: id });
        let dialog = app.intention_menu.import.as_ref().unwrap();
        assert_eq!(dialog.value, "os");
        assert!(app.intention_menu.dialog_active());
        let owner = dialog.owner.clone();
        assert!(app.intention_owner_current(&owner));
        let index = app.active.unwrap();
        app.tabs[index].state.apply_text_edits(
            vec![(
                rustpy_core::TextRange::empty(TextSize::new(0)),
                "# edit\n".into(),
            )],
            "Edit",
        );
        assert!(!app.intention_owner_current(&owner));
    }
    #[test]
    fn create_module_and_package_requests_create_parents_without_overwriting() {
        let (mut app, root) = app("import generated.tools\n");
        let path = root.path().join("generated/tools.py");
        app.begin_import_request(ImportRequest::Create {
            path: path.clone(),
            package: false,
        });
        assert!(path.exists());
        assert!(!root.path().join("generated/__init__.py").exists());
        std::fs::write(&path, "value=1\n").unwrap();
        assert!(app.create_import_target(&path, false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "value=1\n");
        let package = root.path().join("other/sub/__init__.py");
        app.begin_import_request(ImportRequest::Create {
            path: package.clone(),
            package: true,
        });
        assert!(package.exists());
        assert!(!root.path().join("other/__init__.py").exists());
        assert!(app
            .create_import_target(&root.path().join("../outside.py"), false)
            .is_err());
    }
    #[test]
    fn creating_module_preserves_an_existing_namespace_parent() {
        let (mut app, root) = app("import namespace.added\n");
        let namespace = root.path().join("namespace");
        std::fs::create_dir(&namespace).unwrap();
        std::fs::write(namespace.join("existing.py"), "sentinel = 1\n").unwrap();
        app.create_import_target(&namespace.join("added.py"), false)
            .unwrap();
        assert!(namespace.join("added.py").exists());
        assert!(!namespace.join("__init__.py").exists());
        assert_eq!(
            std::fs::read_to_string(namespace.join("existing.py")).unwrap(),
            "sentinel = 1\n"
        );
    }
    #[cfg(unix)]
    #[test]
    fn outside_symlink_parent_is_refused_before_creating_any_files() {
        let (mut app, root) = app("import linked.added\n");
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
        for package in [false, true] {
            let target = if package {
                root.path().join("linked/new_package/__init__.py")
            } else {
                root.path().join("linked/new_package/added.py")
            };
            assert!(app.create_import_target(&target, package).is_err());
            assert_eq!(
                std::fs::read_dir(outside.path()).unwrap().count(),
                0,
                "no mutation outside project"
            );
        }
    }
}
