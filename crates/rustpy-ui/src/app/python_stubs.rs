//! User stub directories and the Python Stubs library.
use super::*;

impl IdeApp {
    pub(super) fn page_python_stubs(&mut self, ui: &mut egui::Ui) {
        ui.heading("Python Stubs");
        ui.label("Custom stub folders take priority over installed and bundled stubs. Changes take effect when you apply the settings.");
        let mut remove = None;
        let mut move_up = None;
        for (index, root) in self.settings.python_stub_roots.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(root.display().to_string()).truncate())
                    .on_hover_text(root.display().to_string());
                if ui
                    .add_enabled(index > 0, egui::Button::new("Move up"))
                    .clicked()
                {
                    move_up = Some(index);
                }
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = move_up {
            self.settings.python_stub_roots.swap(index, index - 1);
        }
        if let Some(index) = remove {
            self.settings.python_stub_roots.remove(index);
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.settings_ui.stub_root_input)
                    .hint_text("Absolute path to a stub folder")
                    .desired_width(320.0),
            );
            if ui.button("Add folder").clicked() {
                let raw = self.settings_ui.stub_root_input.trim();
                let path = raw
                    .strip_prefix("~/")
                    .and_then(|tail| dirs::home_dir().map(|home| home.join(tail)))
                    .unwrap_or_else(|| PathBuf::from(raw));
                if !path.is_absolute() || !path.is_dir() {
                    self.settings_ui.message = Some("Choose an existing absolute directory".into());
                } else {
                    let path = dunce::canonicalize(&path).unwrap_or(path);
                    if !self.settings.python_stub_roots.contains(&path) {
                        self.settings.python_stub_roots.push(path);
                    }
                    self.settings_ui.stub_root_input.clear();
                    self.settings_ui.message = None;
                }
            }
        });
        ui.separator();
        ui.label("Bundled typeshed provides standard-library and third-party stubs. Browse them in the Project view's Python Stubs library.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_stubs_persist_override_runtime_and_rebuild_when_removed() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let stubs = temp.path().join("stubs");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&stubs).unwrap();
        std::fs::write(project.join("sample.py"), "def answer() -> int: return 1\n").unwrap();
        std::fs::write(stubs.join("sample.pyi"), "def answer() -> str: ...\n").unwrap();
        let settings = Settings {
            python_stub_roots: vec![stubs.clone()],
            ..Settings::default()
        };
        let settings: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        let mut app = IdeApp::new(settings, Some(project.clone()));
        app.project.as_mut().unwrap().interpreter = None;
        app.apply_interpreter_paths();
        let analysis = &app.project.as_ref().unwrap().analysis;
        assert_eq!(
            analysis.module("sample").unwrap().path,
            stubs.join("sample.pyi")
        );
        app.open_file(&stubs.join("sample.pyi"));
        assert!(app.tabs[app.active.unwrap()].state.doc.is_read_only());
        app.settings.python_stub_roots.clear();
        app.apply_interpreter_paths();
        assert_eq!(
            app.project
                .as_ref()
                .unwrap()
                .analysis
                .module("sample")
                .unwrap()
                .path,
            project.join("sample.py")
        );
        assert!(app
            .project
            .as_ref()
            .unwrap()
            .tree
            .python_stub_roots
            .is_empty());
    }

    #[test]
    fn bundled_library_browses_opens_and_refuses_tree_mutation() {
        use crate::project_tree::{TreeAction, TreeKey};
        let temp = tempfile::tempdir().unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(temp.path().into()));
        app.apply_interpreter_paths();
        let builtin = PathBuf::from("<typeshed>/builtins.pyi");
        let tree = &mut app.project.as_mut().unwrap().tree;
        // Use the actual navigation route: Python Stubs is nested below the
        // External Libraries group, whose collapsed parent hides its children.
        tree.reveal(&builtin);
        for parent in ["<external-libraries>", "<python-stubs>", "<typeshed>"] {
            assert!(tree.expanded.contains(Path::new(parent)));
        }
        assert!(tree.visible_rows().iter().any(|row| row.path == builtin));
        assert_eq!(tree.selected.as_deref(), Some(builtin.as_path()));
        assert!(tree.key(TreeKey::Delete).is_none());
        assert!(tree.key(TreeKey::Rename).is_none());
        assert!(
            matches!(tree.key(TreeKey::Open), Some(TreeAction::OpenFileDoubleClick(path)) if path == builtin)
        );
        app.open_file(&builtin);
        let editor = &app.tabs[app.active.unwrap()].state;
        assert_eq!(editor.path.as_deref(), Some(builtin.as_path()));
        assert!(editor.doc.is_read_only());
        assert!(editor.doc.text().contains("class int"));
    }
}
