//! Narrow captured model producers; no database connection or framework import is initiated.
use super::*;
impl IdeApp {
    pub(super) fn django_model_diagram(&mut self) {
        for index in 0..self.tabs.len() {
            if self.tabs[index].state.language == Language::Python {
                let _ = self.analysis_file(index);
            }
        }
        let paths = self
            .project_files()
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "py"))
            .collect();
        let Some(project) = &self.project else {
            return;
        };
        if !project.tree.model().frameworks.enabled(&project.root) {
            self.set_status(
                "Enable the canonical Django project facet before capturing Django model diagrams",
            );
            return;
        }
        let root = project.root.clone();
        let analysis = project.analysis.clone();
        self.diagram_panel.request_django(root, analysis, paths);
        self.bottom = Some(BottomTool::Diagram);
    }
    pub(super) fn database_er_diagram(&mut self) {
        match self.db_panel.database_diagram_snapshot() {
            Ok((source, objects)) => {
                if let Some(project) = &self.project {
                    self.diagram_panel.project_context(&project.root);
                }
                self.diagram_panel.set_er(source, objects);
                self.bottom = Some(BottomTool::Diagram);
            }
            Err(error) => self.set_status(error),
        }
    }
    pub(super) fn consume_diagram_request(&mut self) {
        match self.diagram_panel.take_request() {
            Some(0) => self.show_diagram(),
            Some(1) => self.django_model_diagram(),
            Some(2) => self.database_er_diagram(),
            _ => {}
        }
    }
    pub(super) fn diagram_navigate(&mut self, path: PathBuf, range: TextRange) {
        let Some(project) = &self.project else {
            return;
        };
        let current = self
            .tabs
            .iter()
            .find(|tab| tab.state.path.as_deref() == Some(&path))
            .map(|tab| tab.state.doc.text())
            .or_else(|| {
                project
                    .analysis
                    .file_for_path(&path)
                    .map(|file| file.text.to_string())
            });
        if current
            .as_deref()
            .is_none_or(|text| !self.diagram_panel.navigation_current(&path, text))
        {
            self.set_status("Diagram source changed; refresh before navigating the captured range");
            return;
        }
        self.navigate_to(&path, range);
    }
}
