use super::*;
use rustpy_settings::documentation::DocstringPreferences;

impl IdeApp {
    pub(super) fn documentation_preferences(&self) -> DocstringPreferences {
        self.settings.documentation.effective(
            self.project.as_ref().map(|project| project.root.as_path()),
            &self.settings.docstring_format,
        )
    }

    pub(super) fn apply_documentation_policy(&self) {
        if let Some(project) = &self.project {
            project
                .analysis
                .set_analyze_doc_comments(self.documentation_preferences().analyze_doc_comments);
        }
    }

    pub(super) fn detect_project_docstring_format(
        &self,
    ) -> rustpy_semantic::docstrings::DocstringFormat {
        use rustpy_semantic::docstrings::{detect, DocstringFormat as Format};
        if let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) {
            let selected = detect(&tab.state.doc.text());
            if selected != Format::Plain {
                return selected;
            }
        }
        let mut votes = [0usize; 5];
        let mut bytes = 0usize;
        let mut observe = |text: &str| {
            bytes = bytes.saturating_add(text.len());
            if bytes <= 2 * 1024 * 1024 {
                votes[detect(text) as usize] += 1;
            }
        };
        if let Some(project) = &self.project {
            // Already-loaded project sources keep this keystroke consumer free
            // of directory walks and interpreter/library docstring influence.
            if let Some(mut files) = project.analysis.files_with_limit(128) {
                files.sort_by(|left, right| left.path.cmp(&right.path));
                for file in files
                    .iter()
                    .filter(|file| file.path.starts_with(&project.root))
                {
                    observe(&file.text);
                }
            }
        }
        [
            Format::ReStructuredText,
            Format::Google,
            Format::NumPy,
            Format::Epytext,
        ]
        .into_iter()
        .max_by_key(|format| votes[*format as usize])
        .filter(|format| votes[*format as usize] != 0)
        .unwrap_or(Format::ReStructuredText)
    }

    pub(super) fn documentation_settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Docstrings");
        let root = self.project.as_ref().map(|project| project.root.clone());
        let mut use_project = root
            .as_ref()
            .is_some_and(|root| self.settings.documentation.projects.contains_key(root));
        if let Some(root) = &root {
            if ui
                .checkbox(&mut use_project, "Use project documentation settings")
                .changed()
            {
                if use_project {
                    let preferences = self.documentation_preferences();
                    self.settings
                        .documentation
                        .projects
                        .insert(root.clone(), preferences);
                } else {
                    self.settings.documentation.projects.remove(root);
                }
            }
        }
        let mut preferences = if use_project {
            self.documentation_preferences()
        } else {
            self.settings
                .documentation
                .effective(None, &self.settings.docstring_format)
        };
        ui.horizontal_wrapped(|ui| {
            ui.label("Docstring format");
            for (label, value) in [
                ("Auto-detect", "auto"),
                ("Plain", "plain"),
                ("reStructuredText", "rest"),
                ("Epytext", "epytext"),
                ("Google", "google"),
                ("NumPy", "numpy"),
            ] {
                if ui
                    .selectable_label(
                        preferences.format_key() == value,
                        crate::i18n::tr_static(label),
                    )
                    .clicked()
                {
                    preferences.format = value.into();
                }
            }
        });
        ui.checkbox(&mut preferences.analyze_doc_comments, "Analyze DOC comments")
            .on_hover_text("Use structured docstring types and check documentation contracts. Explicit Python annotations and type comments continue to apply.");
        if use_project {
            if let Some(root) = root {
                self.settings
                    .documentation
                    .projects
                    .insert(root, preferences);
            }
        } else {
            self.settings.docstring_format = preferences.format;
            self.settings.documentation.analyze_doc_comments = preferences.analyze_doc_comments;
        }
        ui.label(egui::RichText::new("Used by Generate Docstring, the triple-quote Enter stub, inference and docstring inspections.").color(self.theme.text_dim).small());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_docstring_style_and_doc_analysis_policy_apply_to_real_consumers() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let path = root.join("module.py");
        let source = "def f(value):\n    return value\n";
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings {
            docstring_format: "rest".into(),
            ..Default::default()
        };
        settings.documentation.projects.insert(
            root.clone(),
            DocstringPreferences {
                format: "numpy".into(),
                analyze_doc_comments: false,
            },
        );
        let mut app = IdeApp::new(settings, Some(root));
        app.open_file(&path);
        app.tabs[0].state.go_to_line(1, Some(5));
        app.handle_action(Action::GenerateDocstring, &egui::Context::default());
        assert!(app.tabs[0].state.doc.text().contains("Parameters\n"));
        assert!(!app
            .project
            .as_ref()
            .unwrap()
            .analysis
            .analyze_doc_comments());
        assert_eq!(app.settings.docstring_format, "rest");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        app.settings.documentation.projects.clear();
        app.apply_documentation_policy();
        assert!(app
            .project
            .as_ref()
            .unwrap()
            .analysis
            .analyze_doc_comments());
        assert_eq!(
            app.docstring_format(),
            rustpy_semantic::docstrings::DocstringFormat::ReStructuredText
        );
    }
}
