//! Database tool window (SQLite): data sources, tables, data grid, SQL console.

use std::path::{Path, PathBuf};

use rustpy_db::DataSource;

use crate::theme::UiTheme;
mod catalog;
mod console;
mod grid;
mod native;
mod plan;
mod properties;
mod workspace;

pub enum DbEvent {
    Status(String),
    Navigate(PathBuf, usize),
}

#[derive(Default)]
pub struct DbPanel {
    workspace: workspace::DatabaseWorkspace,
    execution_allowed: bool,
    pub sources: Vec<DataSource>,
    pub selected_source: Option<String>,
    pub selected_table: Option<String>,
    pub expanded: std::collections::HashSet<String>,
    pub sql: String,
    pub new_source_path: String,
    pub loaded_for: Option<PathBuf>,
    pub show_console: bool,
}

impl DbPanel {
    pub(crate) fn service_rows(&self) -> Vec<(String, DataSource, bool)> {
        self.workspace
            .model
            .sources
            .iter()
            .map(|source| {
                let ds = DataSource {
                    name: source.name.clone(),
                    kind: rustpy_db::DataSourceKind::External {
                        configuration: Box::new(source.clone()),
                    },
                };
                (
                    serde_json::to_string(source).unwrap_or_default(),
                    ds,
                    self.workspace
                        .owners
                        .iter()
                        .any(|c| c.owner.source == *source && c.owner.alive() && !c.owner.stopping)
                        || self.workspace.native.source_running(source),
                )
            })
            .collect()
    }
    pub(crate) fn select_service(&mut self, id: &str) -> bool {
        let Some(source) = self
            .workspace
            .model
            .sources
            .iter()
            .find(|s| serde_json::to_string(s).ok().as_deref() == Some(id))
            .cloned()
        else {
            return false;
        };
        self.workspace.selected = Some(source.id);
        self.selected_source = Some(source.name.clone());
        self.show_console = true;
        self.workspace.active = self.workspace.native.source_owner(&source);
        if let Some(c) = self
            .workspace
            .owners
            .iter()
            .find(|c| c.owner.source == source && c.owner.alive())
        {
            self.workspace.active = Some(c.owner.id);
        }
        true
    }
    pub(crate) fn connect_service(&mut self, id: &str) -> Result<(), String> {
        if !self.select_service(id) {
            return Err("Database source no longer exists".into());
        }
        let source = self
            .workspace
            .model
            .sources
            .iter()
            .find(|s| serde_json::to_string(s).ok().as_deref() == Some(id))
            .cloned()
            .unwrap();
        self.workspace.connect(source)
    }
    pub(crate) fn disconnect_service(&mut self, id: &str) {
        if let Some(source) = self
            .workspace
            .model
            .sources
            .iter()
            .find(|source| serde_json::to_string(source).ok().as_deref() == Some(id))
            .cloned()
        {
            self.workspace.native.stop_source(&source);
        }
        for console in &mut self.workspace.owners {
            if serde_json::to_string(&console.owner.source).ok().as_deref() == Some(id) {
                console.owner.stop();
            }
        }
    }
    pub fn load(&mut self, project: &Path) {
        self.workspace.load(project);
        self.workspace.admitted = self.execution_allowed;
        if self.loaded_for.as_deref() == Some(project) {
            return;
        }
        self.loaded_for = Some(project.to_path_buf());
        self.sources = rustpy_db::load_data_sources(project);
        self.selected_source = None;
        self.selected_table = None;
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        _theme: &UiTheme,
        _mono: egui::FontId,
        project: &Path,
    ) -> Vec<DbEvent> {
        self.workspace_ui(ui, project)
    }
}

#[cfg(test)]
mod services_tests {
    use super::*;
    #[test]
    fn edited_source_service_identity_cannot_select_previous_configuration() {
        let mut panel = DbPanel::default();
        panel
            .workspace
            .model
            .sources
            .push(rustpy_db::workspace::Source {
                id: 1,
                name: "same name".into(),
                file: "one.sqlite".into(),
                ..Default::default()
            });
        let first = panel.service_rows()[0].0.clone();
        panel.workspace.model.sources[0].file = "two.sqlite".into();
        let second = panel.service_rows()[0].0.clone();
        assert_ne!(first, second);
        assert!(!panel.select_service(&first));
        assert!(panel.select_service(&second));
        assert!(!panel.service_rows()[0].2);
    }
    #[test]
    fn safe_mode_refuses_console_admission_before_any_driver_or_file_access() {
        let mut panel = DbPanel::default();
        panel
            .workspace
            .model
            .sources
            .push(rustpy_db::workspace::Source {
                id: 1,
                file: "must-not-create.sqlite".into(),
                ..Default::default()
            });
        let id = panel.service_rows()[0].0.clone();
        assert!(panel.connect_service(&id).unwrap_err().contains("Trust"));
        assert!(panel.database_owners().is_empty());
    }
}
