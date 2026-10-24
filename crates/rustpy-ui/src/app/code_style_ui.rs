//! Code Style scheme management and shared formatter preferences.
use super::*;
use crate::a11y::AccessibleCombo;
use rustpy_settings::code_style_schemes::Scheme;
#[derive(Default)]
pub(super) struct State {
    pub name: String,
    pub path: String,
    pub language: String,
}
impl IdeApp {
    pub(super) fn code_style_excludes(&self, path: &Path) -> Result<bool, String> {
        if self.settings.code_style.do_not_format_scopes.is_empty() {
            return Ok(self.settings.code_style.excluded(
                path,
                self.project.as_ref().map(|project| project.root.as_path()),
            ));
        }
        self.settings
            .formatting_excluded(path, &self.capture_scope_context())
    }

    /// Returns true for the Python page; other pages edit their own indentation.
    pub(super) fn code_style_header(&mut self, ui: &mut egui::Ui) -> bool {
        if ui.button("EditorConfig preview…").clicked() {
            self.pending_dialog = Some(Dialog::EditorConfig(Box::new(
                self.editorconfig_preview_state(),
            )));
        }
        let mut selected = self.settings.code_style_schemes.selected.clone();
        ui.horizontal(|ui| {
            let scheme_label = ui.label("Scheme");
            egui::ComboBox::from_id_salt("code_style_scheme")
                .selected_text(&selected)
                .show_ui(ui, |ui| {
                    for name in ["Project", "Default"].into_iter().chain(
                        self.settings
                            .code_style_schemes
                            .named
                            .keys()
                            .filter(|n| !matches!(n.as_str(), "Project" | "Default"))
                            .map(String::as_str),
                    ) {
                        ui.selectable_value(&mut selected, name.into(), name);
                    }
                })
                .combo_labelled(&scheme_label);
        });
        if selected != self.settings.code_style_schemes.selected {
            if let Err(e) = self.settings.select_code_style(&selected) {
                self.settings_ui.message = Some(e)
            }
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.settings_ui.code_style.name)
                    .hint_text("Named scheme"),
            );
            let name = self.settings_ui.code_style.name.trim().to_owned();
            if ui.button("Copy").clicked() {
                if let Err(e) = self.settings.copy_code_style(&name) {
                    self.settings_ui.message = Some(e)
                }
            }
            if ui.button("Rename").clicked() {
                if let Err(e) = self.settings.rename_code_style(&name) {
                    self.settings_ui.message = Some(e)
                }
            }
            ui.menu_button("Set from", |ui| {
                for name in ["PEP 8", "Black-compatible", "Google"] {
                    if ui.button(crate::i18n::tr_static(name)).clicked() {
                        Scheme::preset(name).unwrap().apply(&mut self.settings);
                        ui.close();
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.settings_ui.code_style.path)
                    .hint_text("Scheme JSON or .editorconfig path"),
            );
            if ui.button("Import").clicked() {
                let path = PathBuf::from(self.settings_ui.code_style.path.trim());
                let result = std::fs::metadata(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|metadata| {
                        if metadata.len() > 1024 * 1024 {
                            Err("Scheme exceeds 1 MiB".into())
                        } else {
                            std::fs::read_to_string(&path).map_err(|e| e.to_string())
                        }
                    })
                    .and_then(|text| {
                        if path.file_name().is_some_and(|name| name == ".editorconfig")
                            || path.extension().is_some_and(|ext| ext == "editorconfig")
                        {
                            let mut scheme = Scheme::capture(&self.settings);
                            scheme.import_editorconfig(&text)?;
                            Ok(scheme)
                        } else {
                            Scheme::import_json(&text)
                        }
                    });
                match result {
                    Ok(scheme) => scheme.apply(&mut self.settings),
                    Err(e) => self.settings_ui.message = Some(e),
                }
            }
            for editorconfig in [false, true] {
                if ui
                    .button(if editorconfig {
                        "Export → EditorConfig"
                    } else {
                        "Export JSON"
                    })
                    .clicked()
                {
                    let path = PathBuf::from(self.settings_ui.code_style.path.trim());
                    let scheme = Scheme::capture(&self.settings);
                    let result = if path.as_os_str().is_empty() {
                        Err("Choose an export file path".into())
                    } else {
                        let text = if editorconfig {
                            Ok(scheme.editorconfig())
                        } else {
                            scheme.export_json()
                        };
                        text.and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()))
                    };
                    self.settings_ui.message =
                        Some(result.map_or_else(|e| e, |_| format!("Exported {}", path.display())));
                }
            }
        });
        ui.collapsing("Reformat scope and protection", |ui| {
            let cs = &mut self.settings.code_style;
            egui::ComboBox::from_id_salt("format_scope")
                .selected_text(&cs.reformat_scope)
                .show_ui(ui, |ui| {
                    for (value, label) in [
                        ("all", "Whole file"),
                        ("changed", "Only changed lines (since saved)"),
                        ("vcs", "VCS changed text (HEAD)"),
                    ] {
                        ui.selectable_value(
                            &mut cs.reformat_scope,
                            value.into(),
                            crate::i18n::tr_static(label),
                        );
                    }
                })
                .combo_named(crate::i18n::tr_static("Reformat scope"));
            ui.checkbox(&mut cs.formatter_markers, "Formatter control markers");
            ui.horizontal(|ui| {
                ui.label("Off");
                ui.text_edit_singleline(&mut cs.formatter_off);
                ui.label("On");
                ui.text_edit_singleline(&mut cs.formatter_on);
            });
            ui.label("Do not format (filename/project-relative glob patterns; one per line)");
            let mut masks = cs.do_not_format.join("\n");
            if ui.text_edit_multiline(&mut masks).changed() {
                cs.do_not_format = masks
                    .lines()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .take(256)
                    .map(String::from)
                    .collect()
            }
            ui.label("Do not format named scopes");
            for scope in &self.settings.scopes.named {
                let mut enabled = cs.do_not_format_scopes.contains(&scope.name);
                if ui.checkbox(&mut enabled, &scope.name).changed() {
                    if enabled {
                        cs.do_not_format_scopes.push(scope.name.clone())
                    } else {
                        cs.do_not_format_scopes.retain(|name| name != &scope.name)
                    }
                }
            }
            ui.label("Keep when reformatting");
            ui.checkbox(
                &mut cs.keep_line_breaks,
                "Existing line breaks inside expressions",
            );
            ui.checkbox(&mut cs.keep_simple_blocks, "Simple blocks in one line");
            ui.checkbox(
                &mut cs.keep_first_column_comments,
                "Comments in first column",
            );
        });
        if self.settings_ui.code_style.language.is_empty() {
            self.settings_ui.code_style.language = "Python".into()
        }
        ui.horizontal(|ui| {
            for language in [
                "Python",
                "JSON",
                "YAML",
                "HTML",
                "CSS",
                "JavaScript",
                "Other",
            ] {
                ui.selectable_value(
                    &mut self.settings_ui.code_style.language,
                    language.into(),
                    crate::i18n::tr_static(language),
                );
            }
        });
        let language = self.settings_ui.code_style.language.clone();
        if language == "Python" {
            return true;
        }
        let style = self
            .settings
            .code_style
            .languages
            .entry(language.clone())
            .or_default();
        ui.heading(format!("{language} › Tabs and Indents"));
        ui.checkbox(&mut style.tabs, "Use tab character");
        ui.add(egui::Slider::new(&mut style.indent, 1..=16).text("Indent"));
        ui.add(egui::Slider::new(&mut style.tab_size, 1..=16).text("Tab width"));
        ui.add(egui::Slider::new(&mut style.margin, 40..=200).text("Right margin"));
        ui.label("Per-language editing, paste and formatting indentation; file detection and EditorConfig take precedence.");
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(source: &str) -> (tempfile::TempDir, IdeApp, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("code.py");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.editor.detect_indents = false;
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        app.open_file(&path);
        (root, app, path)
    }
    #[test]
    fn code_style_markers_and_exclusions_reach_actual_reformat() {
        let (_root, mut app, _) = fixture("# @formatter:off\na=  1\n# @formatter:on\nb=2\n");
        app.settings.code_style.formatter_markers = true;
        app.reformat_code();
        let text = app.tabs[0].state.doc.text();
        assert!(text.contains("a=  1"));
        assert!(text.contains("b = 2"));
        app.tabs[0]
            .state
            .doc
            .set_text("other=3\n", &[], &[])
            .unwrap();
        app.settings.code_style.do_not_format = vec!["*.py".into()];
        app.reformat_code();
        assert_eq!(app.tabs[0].state.doc.text(), "other=3\n")
    }
    #[test]
    fn only_changed_lines_uses_saved_document_not_whole_file() {
        let (_root, mut app, _) = fixture("a=1\nb=2\n");
        app.tabs[0]
            .state
            .doc
            .set_text("a=1\nb=3\n", &[], &[])
            .unwrap();
        app.settings.code_style.reformat_scope = "changed".into();
        app.reformat_code();
        assert_eq!(app.tabs[0].state.doc.text(), "a=1\nb = 3\n")
    }
    #[test]
    fn project_scheme_round_trip_drives_indentation_and_named_scheme_copy() {
        let root = tempfile::tempdir().unwrap();
        let mut scheme = Scheme::default();
        scheme.indent.indent = 2;
        scheme.detect_indents = false;
        rustpy_settings::code_style_schemes::save_project(root.path(), &scheme).unwrap();
        let path = root.path().join("code.py");
        std::fs::write(&path, "if True:\n  print(1)\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(root.path().into()));
        app.open_file(&path);
        assert_eq!(app.edit_options_for(0).indent_size, 2);
        app.settings.copy_code_style("Team").unwrap();
        app.settings.editor.indent_size = 8;
        app.settings.select_code_style("Project").unwrap();
        assert_eq!(app.settings.editor.indent_size, 2);
        app.settings.select_code_style("Team").unwrap();
        assert_eq!(app.settings.editor.indent_size, 8)
    }
}
