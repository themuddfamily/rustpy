//! Focused Editor subpages. These edit the existing authoritative settings,
//! not a second copy of editor state; Apply/Cancel retain normal snapshots.
use super::settings_ui::SettingsPage;
use super::*;
use crate::notifications::NotificationKind;
use rustpy_settings::editor_pages::{Association, EncodingRule, InjectionRule};
fn lines(ui: &mut egui::Ui, label: &str, values: &mut Vec<String>) {
    ui.label(label);
    let mut text = values.join("\n");
    if ui.text_edit_multiline(&mut text).changed() {
        *values = text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .take(256)
            .map(str::to_owned)
            .collect();
    }
}
fn toggle(ui: &mut egui::Ui, disabled: &mut Vec<String>, id: &str, label: &str) {
    let mut enabled = !disabled.iter().any(|s| s == id);
    if ui.checkbox(&mut enabled, label).changed() {
        disabled.retain(|s| s != id);
        if !enabled {
            disabled.push(id.into());
        }
    }
}
impl IdeApp {
    pub(super) fn editor_general_additional_options(&mut self, ui: &mut egui::Ui) {
        let prefs = &mut self.settings.editor.preferences;
        ui.checkbox(&mut prefs.drag_and_drop, "Drag-and-drop selected text");
        ui.horizontal(|ui| {
            ui.label("Soft wrap file masks (; separated)");
            ui.text_edit_singleline(&mut prefs.soft_wrap_masks);
        });
        ui.checkbox(
            &mut prefs.soft_wrap_indent,
            "Use original line indentation for soft wraps",
        );
        ui.checkbox(
            &mut prefs.show_soft_wrap_indent,
            "Show soft wrap continuation marker",
        );
        ui.checkbox(
            &mut prefs.reformat_notifications,
            "Show notification after reformat / optimize imports",
        );
        ui.horizontal(|ui| {
            ui.label("Maximum lines with full code insight");
            ui.add(egui::DragValue::new(&mut prefs.max_editor_lines).range(1000..=1_000_000));
        });
        ui.checkbox(
            &mut self
                .settings
                .save_synchronization
                .synchronize_on_frame_activation,
            "Auto-reload files on frame activation",
        );
        ui.checkbox(
            &mut self
                .settings
                .save_synchronization
                .synchronize_on_tab_activation,
            "Auto-reload files on tab activation",
        );
    }

    pub(super) fn editor_preferences_page(&mut self, ui: &mut egui::Ui, page: SettingsPage) {
        match page {
            SettingsPage::EditorAppearance => {
                let e = &mut self.settings.editor;
                ui.checkbox(&mut e.show_line_numbers, "Show line numbers");
                ui.checkbox(
                    &mut e.relative_line_numbers,
                    "Relative / hybrid numbers (absolute on caret line)",
                );
                ui.checkbox(&mut e.highlight_current_line, "Highlight caret row");
                ui.checkbox(&mut e.show_right_margin, "Show right margin");
                ui.checkbox(&mut e.show_indent_guides, "Show indent guides");
                ui.checkbox(&mut e.show_method_separators, "Show method separators");
                ui.checkbox(&mut e.show_whitespaces, "Show whitespaces");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut e.show_leading_whitespaces, "Leading");
                    ui.checkbox(&mut e.show_inner_whitespaces, "Inner");
                    ui.checkbox(&mut e.show_trailing_whitespaces, "Trailing");
                });
                ui.checkbox(&mut e.show_line_separators, "Show line separators");
                ui.checkbox(&mut e.show_intention_bulb, "Show intention bulb");
                ui.checkbox(&mut e.show_css_color_preview, "Show CSS color preview");
                ui.checkbox(
                    &mut e.preferences.css_color_background,
                    "Show CSS color preview as source background",
                );
                ui.checkbox(
                    &mut e.preferences.scrollbar_code_preview,
                    "Show code preview on scrollbar hover",
                );
                ui.checkbox(&mut e.render_doc_comments, "Render documentation comments");
                ui.checkbox(
                    &mut e.preferences.caret_when_unfocused,
                    "Show caret when editor is not focused",
                );
                ui.checkbox(
                    &mut e.preferences.diagnostic_tooltips,
                    "Show diagnostic tooltip on hover",
                );
                ui.horizontal(|ui| {
                    ui.label("Error stripe minimum mark height (px)");
                    ui.add(
                        egui::DragValue::new(&mut e.preferences.error_stripe_min_height)
                            .range(1.0..=20.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Autoreparse / background analysis delay (ms)");
                    ui.add(
                        egui::DragValue::new(&mut e.preferences.reparse_delay_ms).range(0..=5000),
                    );
                });
            }
            SettingsPage::AutoImport => {
                let e = &mut self.settings.editor.preferences;
                ui.checkbox(
                    &mut self.settings.code_style.show_import_popup,
                    "Show import popup for classes, static methods and functions",
                );
                ui.checkbox(
                    &mut self.settings.code_style.add_unambiguous_imports_on_fly,
                    "Add unambiguous imports on the fly",
                );
                ui.checkbox(
                    &mut self.settings.code_style.optimize_imports_on_fly,
                    "Optimize imports on the fly",
                );
                ui.checkbox(
                    &mut e.prefer_from_imports,
                    "Prefer from module import name (otherwise import module and qualify)",
                );
                lines(
                    ui,
                    "Exclude from import and completion (qualified prefixes or wildcard masks)",
                    &mut e.excluded_imports,
                );
            }
            SettingsPage::Breadcrumbs => {
                let e = &mut self.settings.editor;
                ui.checkbox(&mut e.show_breadcrumbs, "Show breadcrumbs");
                ui.checkbox(
                    &mut e.breadcrumbs_at_top,
                    "Place above the editor (otherwise below)",
                );
                for language in [
                    "Python",
                    "JavaScript",
                    "TypeScript",
                    "HTML",
                    "XML",
                    "JSON",
                    "YAML",
                    "TOML",
                    "Markdown",
                    "Shell",
                ] {
                    toggle(
                        ui,
                        &mut e.preferences.breadcrumbs_disabled,
                        language,
                        language,
                    );
                }
            }
            SettingsPage::SmartKeys => {
                ui.label("Shared with Editor › General; these settings affect typing, paste and navigation immediately.");
                let e = &mut self.settings.editor;
                ui.checkbox(
                    &mut e.auto_close_brackets,
                    "Insert pair bracket, including )",
                );
                ui.checkbox(&mut e.auto_close_quotes, "Insert pair quote");
                ui.checkbox(
                    &mut e.surround_selection_on_typing,
                    "Surround selection on typing quote or brace",
                );
                ui.checkbox(&mut e.auto_indent, "Indent on Enter");
                ui.checkbox(&mut e.smart_home_end, "Smart Home/End");
                ui.checkbox(&mut e.smart_backspace, "Smart Backspace");
                ui.checkbox(
                    &mut e.jump_outside_brackets_with_tab,
                    "Jump outside closing bracket with Tab",
                );
                ui.checkbox(&mut e.camel_humps_words, "Use CamelHumps words");
                ui.checkbox(
                    &mut e.preferences.double_click_camel_humps,
                    "Honor CamelHumps on double click",
                );
                ui.checkbox(
                    &mut e.preferences.drag_and_drop,
                    "Enable drag-and-drop of selected text",
                );
                ui.checkbox(
                    &mut e.preferences.rich_text_copy,
                    "Include syntax-colored HTML when copying",
                );
                egui::ComboBox::from_label("Reformat on paste")
                    .selected_text(format!("{:?}", e.reformat_on_paste))
                    .show_ui(ui, |ui| {
                        for (value, label) in [
                            (rustpy_settings::ReformatOnPaste::None, "None"),
                            (
                                rustpy_settings::ReformatOnPaste::IndentBlock,
                                "Indent block",
                            ),
                            (rustpy_settings::ReformatOnPaste::Reformat, "Reformat"),
                        ] {
                            ui.selectable_value(
                                &mut e.reformat_on_paste,
                                value,
                                crate::i18n::tr_static(label),
                            );
                        }
                    });
                ui.checkbox(
                    &mut e.insert_self_in_methods,
                    "Python: insert self / cls in method declarations",
                );
                ui.checkbox(
                    &mut e.insert_docstring_stub,
                    "Python: insert documentation stub on Enter",
                );
                ui.checkbox(
                    &mut e.insert_type_placeholders_in_docstring_stub,
                    "Insert type placeholders",
                );
                ui.checkbox(
                    &mut e.insert_rtype_in_docstring_stub,
                    "Insert return-type placeholder",
                );
                ui.checkbox(
                    &mut e.auto_close_triple_quotes,
                    "Insert closing triple quote",
                );
            }
            SettingsPage::EditorTabs => {
                let e = &mut self.settings.editor;
                super::tab_policy_settings::show(ui, &mut e.tabs);
                ui.add(egui::Slider::new(&mut e.tab_limit, 1..=100).text("Tab limit"));
                ui.checkbox(
                    &mut e.mark_modified_tabs_with_asterisk,
                    "Mark modified tabs with asterisk",
                );
                ui.checkbox(
                    &mut e.open_in_preview_tab,
                    "Open navigation targets in preview tabs",
                );
            }
            SettingsPage::EditorConsole => self.editor_console_preferences(ui),
            SettingsPage::GutterIcons => {
                let disabled = &mut self.settings.editor.preferences.gutter_disabled;
                for (id, label) in [
                    ("run", "Run / test configurations"),
                    ("breakpoints", "Breakpoints"),
                    ("folding", "Code folding"),
                    ("vcs", "VCS changes"),
                    ("bookmarks", "Bookmarks"),
                    ("super", "Super method"),
                    ("implementations", "Implementations"),
                    ("quickfix", "Quick fixes"),
                    ("review", "Review comments"),
                    ("colors", "CSS color previews"),
                ] {
                    toggle(ui, disabled, id, label);
                }
            }
            SettingsPage::PostfixCompletion => {
                let e = &mut self.settings.editor.preferences;
                ui.checkbox(&mut e.postfix_enabled, "Enable postfix completion");
                for template in rustpy_templates::postfix_templates() {
                    toggle(
                        ui,
                        &mut e.postfix_disabled,
                        template.key,
                        &format!("{} — {}", template.key, template.description),
                    );
                }
            }
            SettingsPage::FileTypes => {
                let e = &mut self.settings.editor.preferences;
                ui.collapsing("Registered built-in patterns", |ui| {
                    for sample in [
                        "file.py",
                        "file.pyi",
                        "file.md",
                        "file.json",
                        "file.json5",
                        "file.jsonl",
                        "file.toml",
                        "file.yaml",
                        "file.ini",
                        "file.sh",
                        "file.html",
                        "file.xml",
                        "file.css",
                        "file.scss",
                        "file.js",
                        "file.ts",
                        "file.jsx",
                        "file.tsx",
                        "file.http",
                        "file.sql",
                        "file.rst",
                        "file.pyx",
                        "file.jinja",
                        "file.feature",
                        "file.csv",
                        "file.properties",
                        "file.diff",
                        "Dockerfile",
                        "Makefile",
                        "requirements.txt",
                        ".gitignore",
                        ".dockerignore",
                        ".env",
                    ] {
                        ui.label(format!(
                            "{sample} — {}",
                            Language::from_path(Path::new(sample)).display_name()
                        ));
                    }
                });
                ui.label("Built-in file types remain available. Later custom associations override earlier ones; unknown language names use plain text with the configured keywords.");
                ui.horizontal(|ui| {
                    ui.label("Ignored files and folders (; separated masks)");
                    ui.text_edit_singleline(&mut e.ignored_patterns);
                });
                let mut remove = None;
                for (index, a) in e.associations.iter_mut().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.collapsing(format!("{} — {}", a.name, a.patterns), |ui| {
                            ui.text_edit_singleline(&mut a.name);
                            ui.horizontal(|ui| {
                                ui.label("Patterns");
                                ui.text_edit_singleline(&mut a.patterns);
                            });
                            ui.horizontal(|ui| {
                                ui.label("Language / built-in extension");
                                ui.text_edit_singleline(&mut a.language);
                            });
                            ui.horizontal(|ui| {
                                ui.label("Hash-bang mask (including #!)");
                                ui.text_edit_singleline(&mut a.shebang);
                            });
                            ui.checkbox(
                                &mut a.external_application,
                                "Open matching files in associated system application",
                            );
                            lines(
                                ui,
                                "Custom keyword highlighting (one per line)",
                                &mut a.keywords,
                            );
                            if ui.button("Remove").clicked() {
                                remove = Some(index);
                            }
                        });
                    });
                }
                if let Some(index) = remove {
                    e.associations.remove(index);
                }
                if e.associations.len() < 256
                    && ui.button("Add association / custom file type").clicked()
                {
                    e.associations.push(Association {
                        name: "Custom".into(),
                        patterns: "*.custom".into(),
                        language: "txt".into(),
                        ..Default::default()
                    });
                }
            }
            SettingsPage::FileEncodings => self.editor_encoding_preferences(ui),
            SettingsPage::Copyright => self.copyright_profiles_panel(ui),
            SettingsPage::Duplicates => {
                let e = &mut self.settings.editor.preferences;
                ui.label("Locate Duplicates uses Python syntax statements and the following comparison policy.");
                ui.add(
                    egui::Slider::new(&mut e.duplicate_min_size, 8..=8192)
                        .text("Minimum statement size (bytes)"),
                );
                ui.checkbox(&mut e.duplicate_anonymize_variables, "Anonymize variables");
                ui.checkbox(
                    &mut e.duplicate_anonymize_functions,
                    "Anonymize function / class names",
                );
                ui.checkbox(&mut e.duplicate_anonymize_literals, "Anonymize literals");
                ui.checkbox(
                    &mut e.duplicate_anonymize_fields,
                    "Anonymize fields / attributes",
                );
            }
            SettingsPage::Images => {
                let e = &mut self.settings.editor.preferences;
                ui.checkbox(
                    &mut e.image_checkerboard,
                    "Show transparency chessboard by default",
                );
                ui.checkbox(&mut e.image_grid, "Show pixel grid by default");
                ui.add(egui::Slider::new(&mut e.image_zoom, 0.1..=8.0).text("Initial zoom"));
                ui.horizontal(|ui| {
                    ui.label("External image editor executable (empty: system application)");
                    ui.text_edit_singleline(&mut e.image_editor);
                });
                ui.label("Image toolbar Open externally uses this command with the current image path as one argument.");
            }
            SettingsPage::LanguageInjections => {
                let e = &mut self.settings.editor.preferences;
                ui.checkbox(&mut e.injections_enabled, "Enable language injections");
                for language in [
                    "python",
                    "regex",
                    "sql",
                    "html",
                    "json",
                    "yaml",
                    "markdown",
                    "file reference",
                ] {
                    toggle(ui, &mut e.injections_disabled, language, language);
                }
                ui.label("Rules match a Python string's literal content prefix and the containing file mask. Explicit language comments and temporary injections remain source-authoritative.");
                let mut remove = None;
                for (index, rule) in e.injection_rules.iter_mut().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut rule.enabled, "Enabled");
                            ui.text_edit_singleline(&mut rule.language);
                            ui.text_edit_singleline(&mut rule.file_masks);
                            ui.text_edit_singleline(&mut rule.prefix);
                            if ui.button("Remove").clicked() {
                                remove = Some(index);
                            }
                        });
                    });
                }
                if let Some(index) = remove {
                    e.injection_rules.remove(index);
                }
                if e.injection_rules.len() < 128 && ui.button("Add injection rule").clicked() {
                    e.injection_rules.push(InjectionRule {
                        language: "sql".into(),
                        file_masks: "*.py".into(),
                        prefix: "SELECT ".into(),
                        enabled: true,
                    });
                }
            }
            SettingsPage::CodeVision => {
                ui.checkbox(
                    &mut self.settings.editor.code_vision_usages,
                    "Show declaration usages",
                );
                ui.checkbox(
                    &mut self.settings.editor.code_vision_author,
                    "Show Git author",
                );
                ui.checkbox(
                    &mut self.settings.editor.preferences.code_vision_inheritors,
                    "Show implementations / inheritors link",
                );
                ui.label("Code Vision placement");
                for (value, label) in [
                    ("top", "Above declaration"),
                    ("inline", "Inline at declaration end"),
                    ("hidden", "Hidden"),
                ] {
                    ui.selectable_value(
                        &mut self.settings.editor.preferences.code_vision_position,
                        value.into(),
                        crate::i18n::tr_static(label),
                    );
                }
            }
            SettingsPage::StickyLines => {
                ui.checkbox(
                    &mut self.settings.editor.show_sticky_lines,
                    "Show sticky declaration lines",
                );
                ui.add(
                    egui::Slider::new(&mut self.settings.editor.sticky_lines_limit, 1..=20)
                        .text("Maximum sticky lines"),
                );
            }
            SettingsPage::Emmet => {
                self.web_development_settings(ui);
            }
            _ => {}
        }
    }
    fn editor_console_preferences(&mut self, ui: &mut egui::Ui) {
        let e = &mut self.settings.run_console;
        ui.checkbox(&mut e.soft_wrap, "Use soft wraps in Run console");
        ui.checkbox(&mut e.scroll_to_end, "Scroll to end when output is added");
        ui.add(egui::Slider::new(&mut e.font_size, 8.0..=40.0).text("Console font size"));
        ui.horizontal(|ui| {
            ui.label("Console font family (empty: editor font)");
            ui.text_edit_singleline(&mut e.font_family);
        });
        let mut custom = e.ansi_palette.len() == 16;
        if ui.checkbox(&mut custom, "Override ANSI colors").changed() {
            e.ansi_palette = if custom {
                (0..16)
                    .map(|index| {
                        crate::console_view::ansi_color(index, self.theme.dark).to_array()[..3]
                            .try_into()
                            .unwrap()
                    })
                    .collect()
            } else {
                Vec::new()
            };
        }
        for (index, color) in e.ansi_palette.iter_mut().take(16).enumerate() {
            ui.horizontal(|ui| {
                ui.color_edit_button_srgb(color);
                ui.label(format!("ANSI {index}"));
            });
        }
        ui.add(egui::Slider::new(&mut e.buffer_kib, 1..=262144).text("Cyclic buffer (KiB)"));
        ui.checkbox(&mut e.fold_output, "Enable console folding");
        lines(ui, "Fold lines containing", &mut e.fold_contains);
        lines(
            ui,
            "Console hyperlink regular expressions",
            &mut e.link_filters,
        );
        ui.separator();
        ui.checkbox(
            &mut self.settings.diff_merge.soft_wrap,
            "Use soft wraps in diff viewers",
        );
        ui.checkbox(
            &mut self.settings.python_console.soft_wrap,
            "Use soft wraps in Python Console",
        );
        ui.label("Python Console command history and interpreter options:");
        ui.label("Command history limit");
        ui.add(
            egui::DragValue::new(&mut self.settings.python_console.history_limit)
                .range(1..=100_000),
        );
    }
    fn editor_encoding_preferences(&mut self, ui: &mut egui::Ui) {
        let project = self.project.as_ref().map(|p| p.root.clone());
        let e = &mut self.settings.editor.preferences;
        ui.label("Per-file Convert/Reload overrides take precedence; directory rules choose the longest matching path. Empty default preserves BOM/cookie detection. Changes affect subsequent loads; they never reinterpret an open dirty buffer.");
        ui.horizontal(|ui| {
            ui.label("Global default encoding key");
            ui.text_edit_singleline(&mut e.encoding_default);
        });
        ui.checkbox(&mut e.new_utf8_bom, "Create UTF-8 files with BOM");
        ui.horizontal(|ui| {
            ui.label("Line separator for new files");
            for separator in ["preserve", "LF", "CRLF", "CR"] {
                ui.selectable_value(
                    &mut e.new_line_separator,
                    separator.into(),
                    crate::i18n::tr_static(separator),
                );
            }
        });
        for rule in &mut e.encoding_rules {
            ui.horizontal(|ui| {
                ui.label(if rule.project.is_some() {
                    "Project"
                } else {
                    "Global"
                });
                let mut path = rule.path.to_string_lossy().into_owned();
                if ui.text_edit_singleline(&mut path).changed() {
                    rule.path = path.into();
                }
                ui.text_edit_singleline(&mut rule.encoding);
            });
        }
        if ui.button("Add global directory encoding").clicked() {
            e.encoding_rules.push(EncodingRule {
                project: None,
                path: PathBuf::new(),
                encoding: "utf-8".into(),
            });
        }
        if let Some(project) = project {
            if ui.button("Add project/directory encoding").clicked() {
                e.encoding_rules.push(EncodingRule {
                    project: Some(project),
                    path: PathBuf::new(),
                    encoding: "utf-8".into(),
                });
            }
        }
        e.encoding_rules.retain(|r| r.encoding != "remove");
        ui.label("Set a rule's encoding to remove to delete it. Supported keys: utf-8, utf-8-bom, latin1, utf-16le, utf-16be (status-bar encoding menu provides explicit Convert/Reload).");
    }
}

impl IdeApp {
    pub(super) fn sync_editor_preferences(&mut self) {
        let preferences = self.settings.editor.preferences.clone();
        for tab in &mut self.tabs {
            if tab.state.preferences == preferences {
                continue;
            }
            let path = tab.state.path.as_deref().unwrap_or(Path::new("untitled"));
            let first = tab.state.doc.line_text(0);
            let old = tab.state.preferences.association(path, &first).is_some();
            let association = preferences.association(path, &first);
            let language = association.map(|a| configured_language(&a.language));
            if let Some(language) = language {
                tab.state.set_language(language);
            } else if old {
                tab.state.set_language(Language::from_path(path));
            }
            tab.state.preferences = preferences.clone();
            tab.view.wrap_cache = None;
            tab.state.invalidate_highlight();
        }
        let rebuild = self
            .project
            .as_ref()
            .is_some_and(|project| project.tree.ignored_patterns != preferences.ignored_patterns);
        if let Some(project) = &mut self.project {
            project.tree.ignored_patterns = preferences.ignored_patterns;
        }
        if rebuild {
            self.rebuild_file_index();
        }
    }
    pub(super) fn open_associated_editor_file(
        &mut self,
        path: &Path,
        image: bool,
        first_line: &str,
    ) -> bool {
        if self
            .tabs
            .iter()
            .any(|tab| tab.state.path.as_deref() == Some(path) && tab.has_unsaved_changes())
        {
            return false;
        }
        let p = &self.settings.editor.preferences;
        let custom = if image {
            p.image_editor.clone()
        } else {
            String::new()
        };
        if !image
            && !p
                .association(path, first_line)
                .is_some_and(|a| a.external_application)
        {
            return false;
        }
        let program = if !custom.trim().is_empty() {
            custom.trim().to_owned()
        } else if cfg!(target_os = "windows") {
            "explorer.exe".into()
        } else if cfg!(target_os = "macos") {
            "open".into()
        } else {
            "xdg-open".into()
        };
        let tool = rustpy_settings::ExternalTool {
            name: "Open in associated application".into(),
            program,
            arguments: "$FilePath$".into(),
            show_console: false,
            ..Default::default()
        };
        self.run_tool(tool, Some(path.to_path_buf()), None);
        true
    }
}

impl IdeApp {
    pub(super) fn publish_editor_rich_copy(&mut self, index: usize) {
        let state = &mut self.tabs[index].state;
        let text = state.doc.text();
        let range = if state.has_selection() {
            state.primary().range()
        } else {
            let line = state.doc.line_of(state.primary().head());
            state.doc.line_range_with_newline(line)
        };
        if state.copy_text() != text[usize::from(range.start())..usize::from(range.end())] {
            return;
        }
        let spans = state.highlight_spans().to_vec();
        let html = crate::editor::rich_copy::to_html(
            &text,
            usize::from(range.start()),
            usize::from(range.end()),
            &spans,
            &self.scheme,
            &self.settings.editor.font_family,
            self.settings.editor.font_size,
        );
        let _ = crate::editor::rich_copy::set_clipboard_html(
            &html,
            &text[usize::from(range.start())..usize::from(range.end())],
        );
    }
    pub(super) fn editor_format_notification(&mut self, label: &str) {
        if self.settings.editor.preferences.reformat_notifications {
            self.notify(
                NotificationKind::Info,
                "Editor",
                label,
                "Completed",
                Vec::new(),
            );
        }
    }
}

impl IdeApp {
    pub(super) fn additional_inlay_preferences(&mut self, ui: &mut egui::Ui) {
        let prefs = &mut self.settings.editor.preferences;
        lines(
            ui,
            "Parameter-name hint blacklist (label masks; or function(parameter) masks)",
            &mut prefs.inlay_parameter_blacklist,
        );
        for language in ["Python", "JavaScript", "TypeScript", "SQL"] {
            toggle(
                ui,
                &mut prefs.inlay_disabled_languages,
                language,
                &format!("Enable hints for {language}"),
            );
        }
    }
}

impl IdeApp {
    pub(super) fn color_scheme_font_options(&mut self, ui: &mut egui::Ui) {
        let name = self.settings.editor.color_scheme.clone();
        let default_font = rustpy_settings::editor_pages::SchemeFont {
            enabled: false,
            family: self.settings.editor.font_family.clone(),
            size: self.settings.editor.font_size,
        };
        let font = self
            .settings
            .editor
            .preferences
            .scheme_fonts
            .entry(name.clone())
            .or_insert(default_font);
        ui.collapsing("Color scheme font override", |ui| {
            ui.checkbox(
                &mut font.enabled,
                format!("Override editor font for {name}"),
            );
            ui.add_enabled_ui(font.enabled, |ui| {
                ui.text_edit_singleline(&mut font.family);
                ui.add(egui::Slider::new(&mut font.size, 6.0..=60.0).text("Scheme font size"));
            });
        });
    }
    pub(super) fn editor_color_section(&mut self, ui: &mut egui::Ui, section: &str) {
        ui.heading(section);
        if section == "Console Font & Colors" {
            self.editor_console_preferences(ui);
        }
        self.page_color_scheme(ui);
    }
    pub(super) fn editor_folding_preferences(&mut self, ui: &mut egui::Ui) {
        let e = &mut self.settings.editor;
        ui.checkbox(&mut e.code_folding, "Enable code folding");
        ui.checkbox(
            &mut e.fold_imports_by_default,
            "Collapse imports by default",
        );
        ui.checkbox(
            &mut e.fold_docstrings_by_default,
            "Collapse documentation strings by default",
        );
    }
}

fn configured_language(name: &str) -> Language {
    match name.to_ascii_lowercase().as_str() {
        "python" => Language::Python,
        "text" | "plain text" => Language::PlainText,
        "markdown" => Language::Markdown,
        "javascript" | "typescript" => Language::JavaScript,
        "shell" => Language::Shell,
        "sql" => Language::Sql,
        "html" | "xml" => Language::Html,
        "css" => Language::Css,
        "json" => Language::Json,
        "yaml" => Language::Yaml,
        "toml" => Language::Toml,
        _ => Language::from_path(Path::new(&format!(
            "custom.{}",
            name.trim_start_matches('.')
        ))),
    }
}
impl IdeApp {
    pub(super) fn scheme_profile_actions(&mut self, ui: &mut egui::Ui) {
        let duplicate = ui.button("Duplicate scheme").clicked();
        let rename = ui
            .add_enabled(
                !ColorScheme::builtin_names().contains(&self.scheme.name.as_str()),
                egui::Button::new("Rename scheme"),
            )
            .clicked();
        if !duplicate && !rename {
            return;
        }
        let current = self.scheme.name.clone();
        let name = if self.settings_ui.scheme_save_name.trim().is_empty() {
            format!("{current} Copy")
        } else {
            self.settings_ui.scheme_save_name.trim().to_owned()
        };
        if name.len() > 128
            || name.is_empty()
            || name.contains(['/', '\\', '\0'])
            || name == "."
            || name == ".."
        {
            self.set_status("Use a nonempty scheme name without path separators");
            return;
        }
        let directory = Settings::config_dir().join("colors");
        let target = directory.join(format!("{name}.json"));
        if target.exists() {
            self.set_status("A scheme with that name already exists");
            return;
        }
        let mut scheme = self.scheme.clone();
        scheme.name = name.clone();
        let result = (|| -> Result<(), String> {
            std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
            let bytes = serde_json::to_vec_pretty(&scheme).map_err(|e| e.to_string())?;
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| e.to_string())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            if rename {
                std::fs::remove_file(directory.join(format!("{current}.json")))
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                if let Some(font) = self
                    .settings
                    .editor
                    .preferences
                    .scheme_fonts
                    .get(&current)
                    .cloned()
                {
                    self.settings
                        .editor
                        .preferences
                        .scheme_fonts
                        .insert(name.clone(), font);
                    if rename {
                        self.settings
                            .editor
                            .preferences
                            .scheme_fonts
                            .remove(&current);
                    }
                }
                self.settings.editor.color_scheme = name;
                self.scheme = scheme;
                self.set_status(if rename {
                    "Scheme renamed"
                } else {
                    "Scheme duplicated"
                });
            }
            Err(error) => self.set_status(format!("Scheme operation failed: {error}")),
        }
    }
}

#[cfg(test)]
mod connected_fixtures {
    use super::*;
    #[test]
    fn live_association_changes_language_and_keywords_without_editing_source() {
        let mut app = IdeApp::new(Settings::default(), None);
        let mut state = EditorState::from_text("special = 1", Language::PlainText);
        state.path = Some("/project/file.custom".into());
        let before = state.doc.version();
        app.push_tab(state);
        app.settings
            .editor
            .preferences
            .associations
            .push(Association {
                name: "Custom".into(),
                patterns: "*.custom".into(),
                language: "python".into(),
                keywords: vec!["special".into()],
                ..Default::default()
            });
        app.sync_editor_preferences();
        let index = app.active.unwrap();
        assert_eq!(app.tabs[index].state.language, Language::Python);
        assert_eq!(app.tabs[index].state.doc.version(), before);
        assert!(app.tabs[index]
            .state
            .highlight_spans()
            .iter()
            .any(|span| span.tag == rustpy_highlight::HighlightTag::Keyword
                && span.range == TextRange::new(0.into(), 7.into())));
        app.settings.editor.preferences.associations.clear();
        app.sync_editor_preferences();
        assert_eq!(app.tabs[index].state.language, Language::Other);
        assert_eq!(app.tabs[index].state.doc.text(), "special = 1");
    }
    #[test]
    fn persistent_injection_rule_and_language_toggle_reach_real_fragment_authority() {
        let mut app = IdeApp::new(Settings::default(), None);
        let mut state =
            EditorState::from_text("query = \"SELECT id FROM people\"\n", Language::Python);
        state.path = Some("/project/test.py".into());
        state.set_carets(vec![crate::editor::Caret::at(15.into())]);
        app.push_tab(state);
        app.settings
            .editor
            .preferences
            .injection_rules
            .push(InjectionRule {
                language: "sql".into(),
                file_masks: "*.py".into(),
                prefix: "SELECT ".into(),
                enabled: true,
            });
        app.sync_editor_preferences();
        let index = app.active.unwrap();
        assert_eq!(
            app.tabs[index].state.current_injection().unwrap().language,
            crate::editor::language_injections::Language::Sql
        );
        app.settings
            .editor
            .preferences
            .injections_disabled
            .push("sql".into());
        app.sync_editor_preferences();
        assert!(app.tabs[index].state.current_injection().is_none());
    }
    #[test]
    fn disabled_postfix_cannot_modify_actual_editor_and_perfile_codec_wins() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("value.if", Language::Python));
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(8.into())]);
        app.settings.editor.preferences.postfix_enabled = false;
        app.apply_postfix("if", 5);
        assert_eq!(app.tabs[index].state.doc.text(), "value.if");
        app.settings.editor.preferences.encoding_default = "latin1".into();
        assert_eq!(
            app.encoding_override(Path::new("/project/a.txt")).unwrap(),
            Some(rustpy_text::Encoding::Latin1)
        );
        app.encoding_overrides
            .insert("/project/a.txt".into(), "utf-8".into());
        assert_eq!(
            app.encoding_override(Path::new("/project/a.txt")).unwrap(),
            Some(rustpy_text::Encoding::Utf8)
        );
    }
    #[test]
    fn settings_controls_are_actual_named_subpages() {
        let mut app = IdeApp::new(Settings::default(), None);
        let context = egui::Context::default();
        crate::theme::install_fonts(&context);
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            app.editor_preferences_page(ui, SettingsPage::AutoImport)
        });
        output.drop_without_applying_deltas();
        assert!(app.settings.editor.preferences.prefer_from_imports);
        let mut new = app.settings.clone();
        new.editor.preferences.prefer_from_imports = false;
        assert!(super::super::settings_ui::page_modified(
            SettingsPage::AutoImport,
            &app.settings,
            &new
        ));
    }
}
