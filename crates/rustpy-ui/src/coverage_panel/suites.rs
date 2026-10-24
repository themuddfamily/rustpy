use super::*;
impl CoveragePanel {
    pub fn set_root(&mut self, root: Option<PathBuf>) -> bool {
        if self.root == root {
            return false;
        }
        self.stop();
        self.run = None;
        self.import_rx = None;
        self.html_rx = None;
        self.import_path = None;
        self.log.clear();
        self.export_path.clear();
        self.html_index = None;
        self.root = root;
        self.suites = self
            .root
            .as_ref()
            .and_then(|root| std::fs::read(root.join(".rustpy/coverage/suites.json")).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        self.rebuild_report();
        self.modified_files.clear();
        true
    }
    pub(super) fn persist_suites(&self) {
        if let Some(root) = &self.root {
            let path = root.join(".rustpy/coverage/suites.json");
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(bytes) = serde_json::to_vec_pretty(&self.suites) {
                let _ = std::fs::write(path, bytes);
            }
        }
    }
    pub(super) fn accept_report(&mut self, name: String, mut report: CoverageReport) {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        if let (Some(root), Some(data)) = (&self.root, &report.data_file) {
            let destination = root
                .join(".rustpy/coverage/suites")
                .join(format!("{id}.coverage"));
            if std::fs::create_dir_all(destination.parent().unwrap()).is_ok()
                && std::fs::copy(data, &destination).is_ok()
            {
                report.data_file = Some(destination);
            }
        }
        if !self.merge_new && !self.settings.sum_tests {
            for suite in &mut self.suites {
                suite.active = false;
            }
        }
        self.suites.push(CoverageSuite {
            id,
            name,
            active: true,
            report,
        });
        self.rebuild_report();
        self.persist_suites();
    }
    pub(super) fn rebuild_report(&mut self) {
        let active: Vec<_> = self
            .suites
            .iter()
            .filter(|suite| suite.active)
            .map(|suite| suite.report.clone())
            .collect();
        self.report = match active.len() {
            0 => None,
            1 => active.into_iter().next(),
            _ => Some(rustpy_run::coverage_suites::merge(active)),
        };
    }
    pub fn project_percentages(&self) -> std::collections::HashMap<PathBuf, f32> {
        let mut counts = std::collections::HashMap::<PathBuf, (usize, usize)>::new();
        if !self.settings.show_project_view || self.hidden {
            return Default::default();
        }
        if let Some(report) = &self.report {
            for file in &report.files {
                let covered = file.executed.len() + file.covered_branches;
                let total = file.statements() + file.num_branches;
                counts.insert(file.path.clone(), (covered, total));
                let mut parent = file.path.parent();
                while let Some(path) = parent {
                    if self
                        .root
                        .as_ref()
                        .is_some_and(|root| !path.starts_with(root))
                    {
                        break;
                    }
                    let entry = counts.entry(path.into()).or_default();
                    entry.0 += covered;
                    entry.1 += total;
                    parent = path.parent();
                }
            }
        }
        counts
            .into_iter()
            .map(|(path, (covered, total))| (path, percent_of(covered, total)))
            .collect()
    }
    pub(super) fn suites_ui(
        &mut self,
        ui: &mut egui::Ui,
        root: &Path,
        events: &mut Vec<CoverageEvent>,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.only_modified, "Show only modified files");
            ui.checkbox(&mut self.settings.hide_complete, "Hide 100% files");
            ui.checkbox(
                &mut self.merge_new,
                "Merge new suites (clear to replace active)",
            );
        });
        let mut changed = false;
        let mut remove = None;
        ui.collapsing(format!("Coverage suites ({})", self.suites.len()), |ui| {
            for suite in &mut self.suites {
                ui.push_id(suite.id, |ui| {
                    ui.horizontal(|ui| {
                        changed |= ui.checkbox(&mut suite.active, &suite.name).changed();
                        ui.label(format!(
                            "{:.1}% · {} files",
                            suite.report.total_percent,
                            suite.report.files.len()
                        ));
                        if ui.small_button("Show only").clicked() {
                            remove = Some((suite.id, false));
                        }
                        if ui.small_button("Remove").clicked() {
                            remove = Some((suite.id, true));
                        }
                    });
                });
            }
        });
        if let Some((id, delete)) = remove {
            if delete {
                self.suites.retain(|suite| suite.id != id);
            } else {
                for suite in &mut self.suites {
                    suite.active = suite.id == id;
                }
                self.hidden = false;
            }
            changed = true;
        }
        if changed {
            self.rebuild_report();
            self.persist_suites();
        }
        ui.horizontal(|ui| {
            if self.export_path.is_empty() {
                self.export_path = root.join("coverage-export.json").display().to_string();
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.export_path)
                    .hint_text("Export report path (.json)"),
            );
            if ui
                .add_enabled(self.report.is_some(), egui::Button::new("Export report"))
                .clicked()
            {
                let path = PathBuf::from(self.export_path.trim());
                let path = if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                };
                if let Some(report) = &self.report {
                    match rustpy_run::coverage_suites::export_json(report, &path) {
                        Ok(()) => events.push(CoverageEvent::Status(format!(
                            "Exported {}",
                            path.display()
                        ))),
                        Err(error) => events.push(CoverageEvent::Status(error.to_string())),
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacing_and_merging_suites_preserves_history_and_gutter_union() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("app.py");
        let report = |lines| CoverageReport {
            files: vec![rustpy_run::FileCoverage {
                path: path.clone(),
                executed: lines,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut panel = CoveragePanel::default();
        panel.set_root(Some(root.path().into()));
        panel.accept_report("first".into(), report(vec![1]));
        panel.accept_report("second".into(), report(vec![2]));
        assert_eq!(panel.suites.len(), 2);
        assert!(!panel.suites[0].active);
        assert_eq!(panel.lines_for(&path), vec![(1, LineCoverage::Covered)]);
        panel.merge_new = true;
        panel.accept_report("third".into(), report(vec![3]));
        assert_eq!(
            panel.lines_for(&path),
            vec![(1, LineCoverage::Covered), (2, LineCoverage::Covered)]
        );
        let mut restored = CoveragePanel::default();
        restored.set_root(Some(root.path().into()));
        assert_eq!(restored.suites.len(), 3);
        assert_eq!(restored.lines_for(&path), panel.lines_for(&path));
        restored.hidden = true;
        assert!(restored.project_percentages().is_empty());
        assert!(restored.lines_for(&path).is_empty());
    }
}
