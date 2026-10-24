//! Settings and project decorations for the owned coverage runner/suites.
use super::*;
impl IdeApp {
    pub(super) fn configure_coverage(&mut self) {
        let changed = self
            .coverage_panel
            .set_root(self.project.as_ref().map(|project| project.root.clone()))
            || self.coverage_panel.settings != self.settings.coverage;
        self.coverage_panel.settings = self.settings.coverage.clone();
        self.coverage_panel.branch = self.settings.coverage.branch;
        self.coverage_panel.modified_files = self
            .project
            .as_ref()
            .map(|project| project.tree.vcs_status.keys().cloned().collect())
            .unwrap_or_default();
        for tab in &self.tabs {
            if tab.state.doc.is_modified() {
                if let Some(path) = &tab.state.path {
                    self.coverage_panel.modified_files.insert(path.clone());
                }
            }
        }
        if changed {
            self.coverage_project_badges();
        }
    }
    pub(super) fn coverage_project_badges(&mut self) {
        let percentages = self.coverage_panel.project_percentages();
        if let Some(project) = &mut self.project {
            project.tree.coverage_percent = percentages;
        }
    }
    pub(super) fn coverage_settings_page(&mut self, ui: &mut egui::Ui) {
        let settings = &mut self.settings.coverage;
        ui.heading("Python Coverage");
        ui.checkbox(
            &mut settings.activate_view,
            "Activate Coverage tool window after launch",
        );
        ui.checkbox(
            &mut settings.show_project_view,
            "Show coverage percentages and file colors in Project",
        );
        ui.checkbox(&mut settings.branch, "Branch coverage");
        ui.checkbox(&mut settings.bundled, "Use bundled coverage.py 7.6.1");
        ui.horizontal(|ui| {
            ui.label("Coverage executable (optional override):");
            ui.text_edit_singleline(&mut settings.executable);
        });
        ui.checkbox(
            &mut settings.pytest_cov,
            "Use pytest-cov for pytest runs (plugin must be installed)",
        );
        ui.checkbox(
            &mut settings.sum_tests,
            "Sum coverage of tests / merge new coverage suites",
        );
        ui.checkbox(
            &mut settings.collect_test_folders,
            "Collect coverage in test folders",
        );
        ui.checkbox(&mut settings.hide_complete, "Hide files with 100% coverage");
        let mut include = settings.include.join("\n");
        let mut exclude = settings.exclude.join("\n");
        ui.label("Include patterns (one per line; empty honors coverage configuration):");
        if ui.text_edit_multiline(&mut include).changed() {
            settings.include = include
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect();
        }
        ui.label("Exclude patterns (one per line):");
        if ui.text_edit_multiline(&mut exclude).changed() {
            settings.exclude = exclude
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect();
        }
        ui.label(".coveragerc and coverage sections in pyproject.toml, setup.cfg and tox.ini are read by coverage.py. Explicit include/exclude preferences apply to the next run.");
    }
}
