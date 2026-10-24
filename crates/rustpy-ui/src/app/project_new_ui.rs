//! Project New shares the checked File Templates creation pipeline.
use super::*;
use rustpy_templates::file::{self as ft, FileTemplate, Origin, TemplateSet};

/// Missing defaults only: a disabled or overridden template remains authoritative.
pub(super) fn generation_templates(config: &Path, project: Option<&Path>) -> TemplateSet {
    generation_defaults(ft::load(config, project))
}

/// Also used by the template Settings scheme view so new builtins remain editable.
pub(super) fn generation_defaults(mut set: TemplateSet) -> TemplateSet {
    for template in [
        FileTemplate::new("Python Stub", "pyi", "module", "# ${NAME}\n", "Python type stub"),
        FileTemplate::new("TypeScript File", "ts", "script", "// ${NAME}\n\n", "TypeScript source"),
        FileTemplate::new("Jupyter Notebook", "ipynb", "notebook", "{\n \"cells\": [],\n \"metadata\": {},\n \"nbformat\": 4,\n \"nbformat_minor\": 5\n}\n", "Empty Jupyter notebook"),
        FileTemplate::new("HTTP Request", "http", "requests", &ft::escape_literal("### Request\nGET https://example.com/\n"), "HTTP request file"),
    ] {
        if !set.templates.iter().any(|(existing, _)| existing.name == template.name) {
            set.templates.push((template, Origin::Builtin));
        }
    }
    set
}

pub(super) fn template_groups(set: &TemplateSet) -> Vec<(String, Vec<String>)> {
    let mut groups = Vec::new();
    for (label, origins) in [
        ("Project Templates", vec![Origin::Project]),
        ("IDE Templates", vec![Origin::Custom, Origin::Modified]),
        ("Built-in Templates", vec![Origin::Builtin]),
    ] {
        let names: Vec<_> = set
            .templates
            .iter()
            .filter(|(template, origin)| {
                template.enabled && !template.name.trim().is_empty() && origins.contains(origin)
            })
            .map(|(template, _)| template.name.clone())
            .collect();
        if !names.is_empty() {
            groups.push((label.to_owned(), names));
        }
    }
    groups
}

/// Package creation must not truncate an existing initializer. Nested parents
/// must already exist; the new package itself is created exclusively.
pub(super) fn create_python_package(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind, Write};
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Enter a single package name",
        ));
    }
    let (tokens, diagnostics) = rustpy_syntax::tokenize(name);
    if !diagnostics.is_empty()
        || !tokens.iter().any(|token| {
            token.kind == rustpy_syntax::SyntaxKind::IDENT
                && u32::from(token.range.start()) == 0
                && u32::from(token.range.end()) as usize == name.len()
        })
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Enter a valid Python package identifier",
        ));
    }
    let package = dir.join(name);
    std::fs::create_dir(&package)?;
    let initializer = package.join("__init__.py");
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&initializer)?;
        file.write_all(b"")?;
        file.sync_all()?;
        Ok(initializer.clone())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir(&package);
    }
    result
}

impl IdeApp {
    /// Called on accepted project open and successful template settings changes,
    /// not inside tree painting. Existing enabled origins define template groups.
    pub(super) fn refresh_project_new_catalog(&mut self) {
        let set = generation_templates(
            &Settings::config_dir(),
            self.project.as_ref().map(|p| p.root.as_path()),
        );
        if let Some(project) = &mut self.project {
            project.tree.set_new_template_groups(template_groups(&set));
        }
        self.project_new_generation = ft::generation();
        self.sync_external_file_watches();
    }

    pub(super) fn refresh_project_new_if_changed(&mut self) {
        if self.project_new_generation != ft::generation() {
            self.refresh_project_new_catalog();
        }
    }

    pub(super) fn project_template_store_changed(&mut self) {
        self.invalidate_file_template_page();
        self.refresh_project_new_catalog();
    }

    pub(super) fn is_template_store_event(&self, path: &Path) -> bool {
        let path = rustpy_core::vfs::normalize_path(path);
        let mut stores = vec![ft::ide_store_path(&Settings::config_dir())];
        if let Some(project) = &self.project {
            stores.push(ft::project_store_path(&project.root));
        }
        stores.into_iter().any(|store| {
            let store = rustpy_core::vfs::normalize_path(&store);
            path == store || store.starts_with(&path)
        })
    }

    pub(super) fn template_watch_parent(&self) -> Option<PathBuf> {
        let store = ft::ide_store_path(&Settings::config_dir());
        store
            .parent()?
            .ancestors()
            .find(|path| path.is_dir())
            .map(Path::to_owned)
    }

    pub(super) fn project_new_template(&mut self, dir: PathBuf, name: String) {
        self.open_new_file_dialog(dir, name);
    }

    pub(super) fn project_edit_templates(&mut self) {
        self.settings_ui.page = settings_ui::SettingsPage::FileTemplates;
        self.dialog = Dialog::Settings;
    }

    pub(super) fn project_new_data_source(&mut self, dir: &Path) {
        self.bottom = Some(BottomTool::Database);
        self.db_panel.new_source_path = dir.join("database.sqlite").display().to_string();
    }
}

#[cfg(test)]
#[path = "project_new_tests.rs"]
mod tests;
