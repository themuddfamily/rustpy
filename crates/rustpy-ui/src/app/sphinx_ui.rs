//! Integrated documentation tasks and reStructuredText editor lookup.
use super::*;
#[cfg(test)]
#[path = "sphinx_task_execution.rs"]
mod task_execution;
impl IdeApp {
    pub(super) fn sphinx_settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Sphinx");
        let prefs = &mut self.settings.sphinx;
        fn path(ui: &mut egui::Ui, label: &str, value: &mut PathBuf, hint: &str) {
            ui.horizontal(|ui| {
                ui.label(label);
                let mut text = value.to_string_lossy().into_owned();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut text)
                            .hint_text(hint)
                            .desired_width(300.0),
                    )
                    .changed()
                {
                    *value = text.into();
                }
            });
        }
        path(
            ui,
            "Working directory",
            &mut prefs.working_directory,
            "auto: docs/, doc/, project root",
        );
        path(
            ui,
            "Source directory",
            &mut prefs.source_directory,
            "relative to working directory",
        );
        path(
            ui,
            "Configuration directory",
            &mut prefs.configuration_directory,
            "directory containing conf.py",
        );
        path(
            ui,
            "Output directory",
            &mut prefs.output_directory,
            "_build/html",
        );
        ui.horizontal(|ui| {
            ui.label("Builder");
            ui.text_edit_singleline(&mut prefs.builder);
        });
        ui.checkbox(
            &mut prefs.warnings_as_errors,
            "Treat Sphinx warnings as errors (-W)",
        );
        ui.checkbox(
            &mut prefs.fresh_environment,
            "Rebuild the Sphinx environment (-E)",
        );
        ui.horizontal(|ui| {
            ui.label("Additional Sphinx arguments");
            ui.text_edit_singleline(&mut prefs.additional_arguments);
        });
        ui.label("Build Sphinx Docs creates a reusable Run configuration using the selected Python interpreter. Docutils exports the current saved .rst file; the Run console supports cancellation and displays diagnostics.");
        ui.label("reStructuredText supports directive/role completion and a local split preview. Trusted Sphinx projects add their installed extension registry and reference targets. Use Open Generated Documentation for the full Docutils HTML preview.");
    }
    pub(super) fn build_sphinx_docs(&mut self) {
        let Some(project) = &self.project else {
            self.set_status("Open a project before building Sphinx documentation");
            return;
        };
        match rustpy_project::sphinx::build_configuration(&project.root, &self.settings.sphinx) {
            Ok(config) => self.run_config(config),
            Err(error) => self.set_status(error),
        }
    }
    fn docutils_output(&self) -> Option<PathBuf> {
        let project = self.project.as_ref()?;
        let source = self
            .active
            .and_then(|index| self.tabs.get(index))?
            .state
            .path
            .as_ref()?;
        let relative = source
            .strip_prefix(&project.root)
            .ok()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(source.file_name().unwrap_or_default()));
        Some(
            project
                .root
                .join(".rustpy/documentation-preview")
                .join(relative)
                .with_extension("html"),
        )
    }
    pub(super) fn run_docutils_task(&mut self) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            self.set_status("Trust this project before running Docutils");
            return;
        }
        let Some(index) = self.active else { return };
        if self.tabs[index].state.language != Language::Rst {
            self.set_status("Open a reStructuredText file to run Docutils");
            return;
        }
        if !self.save_all() {
            return;
        }
        let Some(project) = &self.project else { return };
        let Some(interpreter) = project.interpreter.as_ref() else {
            self.set_status("Configure a Python interpreter with Docutils installed");
            return;
        };
        let Some(source) = self.tabs[index].state.path.as_ref() else {
            return;
        };
        let Some(output) = self.docutils_output() else {
            return;
        };
        if let Some(parent) = output.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                self.set_status(format!("Cannot create documentation output: {error}"));
                return;
            }
        }
        match rustpy_project::sphinx::docutils_configuration(source, &output, &interpreter.path) {
            Ok(config) => self.run_config(config),
            Err(error) => self.set_status(error),
        }
    }
    pub(super) fn open_documentation_preview(&mut self) {
        if self.project_safe_mode() {
            self.set_status("Trust the project before opening executable generated documentation");
            return;
        }
        let source = self.active.and_then(|index| self.tabs.get(index));
        if source.is_some_and(|tab| tab.state.doc.is_modified()) {
            self.set_status("Save and rebuild this document before opening its generated preview");
            return;
        }
        let sphinx = self.project.as_ref().and_then(|project| {
            source
                .and_then(|tab| tab.state.path.as_deref())
                .and_then(|path| {
                    rustpy_project::sphinx::preview_path(&project.root, &self.settings.sphinx, path)
                })
        });
        let path = self
            .docutils_output()
            .into_iter()
            .chain(sphinx)
            .filter(|path| path.is_file())
            .max_by_key(|path| {
                path.metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
            });
        let Some(path) = path else {
            self.set_status(
                "Run Docutils Task or Build Sphinx Docs first to generate an HTML preview",
            );
            return;
        };
        let configured = self.settings.browser.trim();
        let program = if configured.is_empty() {
            if cfg!(target_os = "macos") {
                "open"
            } else if cfg!(windows) {
                "explorer.exe"
            } else {
                "xdg-open"
            }
        } else {
            configured
        };
        let mut command = std::process::Command::new(program);
        match command.arg(&path).spawn() {
            Ok(_) => self.set_status(format!(
                "Opened generated documentation: {}",
                path.display()
            )),
            Err(error) => self.set_status(format!("Cannot open documentation preview: {error}")),
        }
    }
    pub(super) fn open_rst_completion(&mut self, explicit: bool) -> bool {
        let project_safe_mode = self.project_safe_mode();
        let Some(index) = self.active else {
            return false;
        };
        let tab = &self.tabs[index];
        if tab.state.language != Language::Rst {
            return false;
        }
        let text = tab.state.doc.text();
        let caret = tab.state.primary().head();
        let registry = self
            .project
            .as_ref()
            .and_then(|project| {
                let python = project.interpreter.as_ref()?;
                let root = self
                    .settings
                    .sphinx
                    .working_directory(&project.root)
                    .unwrap_or_else(|| project.root.clone());
                Some(rustpy_project::sphinx::registry(
                    &root,
                    &python.path,
                    project.analysis.revision_excluding(Path::new("<sphinx>")),
                    !project_safe_mode && !self.tabs.iter().any(|tab| tab.state.doc.is_modified()),
                    &self.settings.sphinx,
                ))
            })
            .unwrap_or_default();
        let completion = rustpy_syntax::rst_editing::complete(
            &text,
            usize::from(caret),
            &registry.directives,
            &registry.roles,
            &registry.references,
        );
        self.cancel_basic_completion();
        self.import_completion = None;
        self.completion_ui.paths = true;
        self.completion_ui.reset_help();
        let items: Vec<_> = completion
            .as_ref()
            .map(|completion| {
                completion
                    .names
                    .iter()
                    .map(|name| CompletionItem {
                        label: name.clone().into(),
                        kind: CompletionKind::Keyword,
                        detail: "reStructuredText / Sphinx".into(),
                        insert: format!("{name}{}", completion.suffix),
                        caret_offset: None,
                        priority: 0,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.completion = CompletionPopup {
            visible: !items.is_empty(),
            imports: vec![None; items.len()],
            items,
            selected: if self.settings.completion.preselect_first {
                0
            } else {
                usize::MAX
            },
            prefix_start: completion.map_or(caret, |completion| {
                TextSize::new(completion.range.start as u32)
            }),
            doc_version: self.tabs[index].state.doc.version(),
            caret,
            explicit,
            rect: None,
        };
        self.refresh_completion_order();
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rst_lookup_is_available_without_project_execution() {
        let mut app = IdeApp::new(rustpy_settings::Settings::default(), None);
        app.push_tab(crate::editor::EditorState::from_text(
            ".. not",
            Language::Rst,
        ));
        let index = app.active.unwrap();
        app.tabs[index].state = crate::editor::EditorState::from_text(".. not", Language::Rst);
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(6))]);
        assert!(app.open_rst_completion(true));
        assert!(app
            .completion
            .items
            .iter()
            .any(|item| item.label == "note" && item.insert == "note:: "));
    }
    #[test]
    fn rst_preview_paints_heading_instead_of_source_adornment() {
        let mut app = IdeApp::new(rustpy_settings::Settings::default(), None);
        app.push_tab(crate::editor::EditorState::from_text(
            "Heading\n=======\n\n.. note:: Body\n",
            Language::Rst,
        ));
        let index = app.active.unwrap();
        // The first workspace frame initializes its mode from preferences.
        app.settings.markdown.layout = rustpy_settings::markdown::Layout::Preview;
        let context = egui::Context::default();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    assert!(app.markdown_preview(ui, index));
                });
            },
        );
        let labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        output.drop_without_applying_deltas();
        assert!(labels.contains("Heading"), "{labels}");
        assert!(labels.contains("note"), "{labels}");
        assert!(!labels.contains("======="), "{labels}");
    }
}
