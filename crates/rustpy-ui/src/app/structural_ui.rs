//! Structural Search & Replace dialog (Edit → Find → Search Structurally).

use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_search::structural::{self, PatternOptions};

pub(super) struct StructuralState {
    pub pattern: String,
    pub replacement: String,
    pub replace_mode: bool,
    pub results: Vec<(PathBuf, usize, String, TextRange)>,
    pub selected: usize,
    pub error: Option<String>,
    pub files_searched: usize,
    pub options: PatternOptions,
    pub filter_name: String,
    pub library_name: String,
    pub inspection: bool,
}

impl Default for StructuralState {
    fn default() -> Self {
        Self {
            pattern: "$obj$.$method$($args...$)".into(),
            replacement: String::new(),
            replace_mode: false,
            results: Vec::new(),
            selected: 0,
            error: None,
            files_searched: 0,
            options: PatternOptions::default(),
            filter_name: String::new(),
            library_name: String::new(),
            inspection: false,
        }
    }
}

impl IdeApp {
    /// Text of a project file, preferring the open editor's content.
    fn file_text(&self, path: &Path) -> Result<String, String> {
        if let Some(t) = self
            .tabs
            .iter()
            .find(|t| t.state.path.as_deref() == Some(path))
        {
            return Ok(t.state.doc.text());
        }
        let snapshot = rustpy_core::vfs::Vfs::new()
            .read(path, rustpy_core::vfs::ReadLimits::default())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        rustpy_text::Encoding::decode_python_source(&snapshot.bytes)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    pub(super) fn structural_search_now(&self, st: &mut StructuralState) {
        self.structural_search(st);
    }

    fn structural_search(&self, st: &mut StructuralState) {
        st.results.clear();
        st.error = None;
        st.selected = 0;
        let pattern = match structural::compile(&st.pattern, &st.options) {
            Ok(p) => p,
            Err(e) => {
                st.error = Some(e);
                return;
            }
        };
        let files: Vec<PathBuf> = self
            .project_files()
            .into_iter()
            .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("py" | "pyi")))
            .collect();
        st.files_searched = files.len();
        for path in files {
            let text = match self.file_text(&path) {
                Ok(text) => text,
                Err(error) => {
                    st.error = Some(error);
                    continue;
                }
            };
            let matches = if let Some(db) = self.analysis_handle() {
                if self
                    .tabs
                    .iter()
                    .any(|tab| tab.state.path.as_deref() == Some(path.as_path()))
                {
                    db.set_file_text(&path, &text);
                }
                db.file_for_path(&path)
                    .filter(|file| file.text.as_ref() == text)
                    .map(|file| structural::search_with_analysis(&pattern, &db, &file))
                    .unwrap_or_default()
            } else {
                structural::search_text(&pattern, &text)
            };
            for m in matches {
                let start = usize::from(m.range.start()).min(text.len());
                let line = text[..start].matches('\n').count();
                let line_text = text.lines().nth(line).unwrap_or("").trim().to_string();
                st.results.push((path.clone(), line, line_text, m.range));
                if st.results.len() >= 2000 {
                    return;
                }
            }
        }
    }

    fn structural_replace_all(&mut self, st: &mut StructuralState) -> usize {
        let Ok(pattern) = structural::compile(&st.pattern, &st.options) else {
            return 0;
        };
        st.error = None;
        let files: Vec<PathBuf> = st
            .results
            .iter()
            .map(|(p, _, _, _)| p.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut n = 0;
        for path in files {
            if let Some(i) = self
                .tabs
                .iter()
                .position(|t| t.state.path.as_deref() == Some(path.as_path()))
            {
                if self.tabs[i].state.doc.is_read_only() {
                    st.error = Some(format!("{} is read-only", path.display()));
                    continue;
                }
                let text = self.tabs[i].state.doc.text();
                let edits = if let Some(db) = self.analysis_handle() {
                    db.set_file_text(&path, &text);
                    db.file_for_path(&path)
                        .map(|file| {
                            structural::replace_with_analysis(&pattern, &st.replacement, &db, &file)
                        })
                        .unwrap_or_default()
                } else {
                    structural::replace_text(&pattern, &st.replacement, &text)
                };
                if edits.is_empty() {
                    continue;
                }
                n += edits.len();
                self.snapshot(i, "Before structural replace");
                self.tabs[i]
                    .state
                    .apply_text_edits(edits, "Structural replace");
            } else {
                match replace_closed_structural_filtered(
                    &path,
                    &pattern,
                    &st.replacement,
                    self.analysis_handle().as_deref(),
                ) {
                    Ok(count) => n += count,
                    Err(error) => st.error = Some(error),
                }
            }
        }
        n
    }

    pub(super) fn structural_dialog(&mut self, ctx: &egui::Context, mut st: Box<StructuralState>) {
        let theme = self.theme.clone();
        let mono = egui::FontId::new(
            self.settings.editor.font_size.max(10.0),
            crate::theme::mono_family(false, false),
        );
        let mut open = true;
        let mut esc = false;
        let mut do_search = false;
        let mut do_replace = false;
        let mut do_inspect = false;
        let mut navigate: Option<(PathBuf, TextRange)> = None;
        let library_path = structural_library_path();
        let library_result = structural::load_library(&library_path);
        let library_ok = library_result.is_ok();
        let mut library = library_result.clone().unwrap_or_default();
        if let Err(error) = library_result {
            st.error = Some(error);
        }
        let mut save_template = false;
        let mut delete_template = false;
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        egui::Window::new(if st.replace_mode { "Replace Structurally" } else { "Search Structurally" }).collapsible(false).resizable(true).default_size([900.0, 560.0]).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            ui.horizontal(|ui| {
                let template_label = ui.label("Template:");
                egui::ComboBox::from_id_salt("ssr_templates").selected_text("Choose…").width(260.0).show_ui(ui, |ui| {
                    for (name, pat) in structural::builtin_templates() {
                        if ui.selectable_label(false, format!("{name}  —  {pat}")).clicked() {
                            st.pattern = pat.to_string();
                            st.options = PatternOptions::default();
                            st.library_name = name.into();
                            st.inspection = false;
                        }
                    }
                }).combo_labelled(&template_label);
                egui::ComboBox::from_id_salt("ssr_saved_templates").selected_text("Saved templates…").show_ui(ui, |ui| {
                    for template in &library {
                        if ui.selectable_label(false, &template.name).clicked() {
                            st.pattern = template.pattern.clone();
                            st.replacement = template.replacement.clone();
                            st.replace_mode = template.replace;
                            st.options = template.options.clone();
                            st.library_name = template.name.clone();
                            st.inspection = template.inspection;
                        }
                    }
                }).combo_named(crate::i18n::tr_static("Saved templates"));
                ui.checkbox(&mut st.replace_mode, "Replace");
                ui.label(egui::RichText::new("Placeholders: $x$ matches one node, $xs...$ matches a list; repeated names must match the same text.").color(theme.text_dim).small());
            });
            ui.label("Search template:");
            ui.add(egui::TextEdit::multiline(&mut st.pattern).font(egui::TextStyle::Monospace).desired_rows(3).desired_width(f32::INFINITY));
            if st.replace_mode {
                ui.label("Replacement template:");
                ui.add(egui::TextEdit::multiline(&mut st.replacement).font(egui::TextStyle::Monospace).desired_rows(3).desired_width(f32::INFINITY));
            }
            ui.collapsing("Placeholder filters", |ui| {
                if let Ok(pattern) = structural::compile(&st.pattern, &PatternOptions::default()) {
                    let names = pattern.placeholder_names();
                    if !names.contains(&st.filter_name) { st.filter_name = names.first().cloned().unwrap_or_default(); }
                    egui::ComboBox::from_id_salt("ssr_filter_name").selected_text(&st.filter_name).show_ui(ui, |ui| {
                        for name in names { ui.selectable_value(&mut st.filter_name, name.clone(), name); }
                    }).combo_named(crate::i18n::tr_static("Placeholder"));
                    if ui.button("Clear all filters").clicked() { st.options = PatternOptions::default(); }
                    if !st.filter_name.is_empty() {
                        let constraint = st.options.constraints.entry(st.filter_name.clone()).or_default();
                        optional_filter(ui, "Text regex", &mut constraint.regex);
                        ui.horizontal(|ui| {
                            ui.label("Count:");
                            ui.add(egui::DragValue::new(&mut constraint.min).range(0..=4096));
                            ui.label("to");
                            ui.add(egui::DragValue::new(&mut constraint.max).range(0..=usize::MAX));
                        });
                        optional_filter(ui, "Inferred type regex", &mut constraint.type_regex);
                        optional_filter(ui, "Script", &mut constraint.script);
                        ui.small("Script: text.contains(\"name\") && count >= 1; ==, !=, starts_with, ends_with, &&, ||. No Python execution.");
                        ui.small("Type filters use inferred expression types; unknown types do not match.");
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Saved name:"); ui.text_edit_singleline(&mut st.library_name);
                ui.checkbox(&mut st.inspection, "Use as Structural search inspection");
                save_template = ui.add_enabled(library_ok, egui::Button::new("Save template")).clicked();
                delete_template = ui.add_enabled(library.iter().any(|t| t.name == st.library_name), egui::Button::new("Delete template")).clicked();
            });
            ui.horizontal(|ui| {
                if ui.button("Find").clicked() {
                    do_search = true;
                }
                if st.replace_mode && ui.add_enabled(!st.results.is_empty(), egui::Button::new("Replace All")).clicked() {
                    do_replace = true;
                }
                do_inspect = ui.add_enabled(library.iter().any(|rule| rule.inspection), egui::Button::new("Run saved inspections in project")).clicked();
                if let Some(e) = &st.error {
                    ui.colored_label(theme.error, e);
                } else if st.files_searched > 0 {
                    ui.label(egui::RichText::new(format!("{} match(es) in {} file(s) searched", st.results.len(), st.files_searched)).color(theme.text_dim));
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().id_salt("ssr_results").auto_shrink([false, false]).show(ui, |ui| {
                let mut last_path: Option<&PathBuf> = None;
                for (i, (path, line, text, range)) in st.results.iter().enumerate() {
                    if last_path != Some(path) {
                        ui.label(egui::RichText::new(path.strip_prefix(&root).unwrap_or(path).display().to_string()).color(theme.accent));
                        last_path = Some(path);
                    }
                    let resp = ui.selectable_label(st.selected == i, egui::RichText::new(format!("{:>5}  {text}", line + 1)).font(mono.clone()));
                    if resp.clicked() {
                        st.selected = i;
                    }
                    if resp.double_clicked() {
                        navigate = Some((path.clone(), *range));
                    }
                }
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                esc = true;
            }
        });
        if save_template || delete_template {
            library.retain(|t| t.name != st.library_name);
            if save_template {
                library.push(structural::Template {
                    name: st.library_name.clone(),
                    pattern: st.pattern.clone(),
                    replacement: st.replacement.clone(),
                    replace: st.replace_mode,
                    options: st.options.clone(),
                    inspection: st.inspection,
                });
            }
            if let Err(error) = structural::save_library(&library_path, &library) {
                st.error = Some(error);
            }
        }
        if do_search {
            self.structural_search(&mut st);
        }
        if do_replace {
            let n = self.structural_replace_all(&mut st);
            let error = st.error.take();
            self.set_status(match &error {
                Some(error) => format!("Replaced {n} occurrence(s); {error}"),
                None => format!("Replaced {n} occurrence(s)"),
            });
            if let Some(p) = &mut self.project {
                p.tree.invalidate_all();
            }
            self.reload_changed_files();
            self.structural_search(&mut st);
            if error.is_some() {
                st.error = error;
            }
        }
        if do_inspect {
            self.run_selected_inspection("StructuralSearchInspection".into(), true);
            return;
        }
        if let Some((path, range)) = navigate {
            self.navigate_to(&path, range);
            self.focus_editor = true;
            return;
        }
        if open && !esc {
            self.dialog = Dialog::Structural(st);
        } else {
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
fn replace_closed_structural(
    path: &Path,
    pattern: &str,
    replacement: &str,
) -> Result<usize, String> {
    let pattern = structural::compile(pattern, &PatternOptions::default())?;
    replace_closed_structural_filtered(path, &pattern, replacement, None)
}

fn replace_closed_structural_filtered(
    path: &Path,
    pattern: &structural::Pattern,
    replacement: &str,
    db: Option<&Analysis>,
) -> Result<usize, String> {
    super::refactor_ui::transform_closed_file(path, true, |text| {
        // Real Python source must satisfy the same policy as open/index/semantic readers.
        let edits = if let Some(db) = db {
            let file = db
                .file_for_path(path)
                .ok_or("Cannot analyze structural replacement source")?;
            if file.text.as_ref() != text {
                return Err("Source changed before structural replacement".into());
            }
            structural::replace_with_analysis(pattern, replacement, db, &file)
        } else {
            structural::replace_text(pattern, replacement, text)
        };
        let count = edits.len();
        Ok((structural::apply_edits(text, &edits), count))
    })
}

pub(super) fn structural_library_path() -> PathBuf {
    Settings::config_dir().join("structural-templates.json")
}

fn optional_filter(ui: &mut egui::Ui, label: &str, value: &mut Option<String>) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut text = value.clone().unwrap_or_default();
        if ui.text_edit_singleline(&mut text).changed() {
            *value = (!text.trim().is_empty()).then_some(text);
        }
    });
}

#[cfg(test)]
mod encoding_tests {
    use super::*;
    #[test]
    fn closed_structural_replace_preserves_format_and_counts_only_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.py");
        for encoding in [
            rustpy_text::Encoding::Latin1,
            rustpy_text::Encoding::Utf8Bom,
        ] {
            let text = if encoding == rustpy_text::Encoding::Latin1 {
                "# coding: latin-1\r\ns = 'Âª'\r\nx = 1\r\n"
            } else {
                "s = 'é'\r\nx = 1\r\n"
            };
            std::fs::write(&path, encoding.encode(text).unwrap()).unwrap();
            assert_eq!(
                replace_closed_structural(&path, "x = 1", "x = 2").unwrap(),
                1
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                encoding.encode(&text.replace("x = 1", "x = 2")).unwrap()
            );
            if encoding == rustpy_text::Encoding::Latin1 {
                let before = std::fs::read(&path).unwrap();
                assert!(replace_closed_structural(&path, "x = 2", "x = '😀'").is_err());
                assert_eq!(std::fs::read(&path).unwrap(), before);
            }
        }
    }
    #[test]
    fn structural_replace_all_reports_failed_closed_writes_without_counting_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.py");
        let original = b"# coding: latin-1\r\nx = 1\r\n";
        std::fs::write(&path, original).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        let mut state = StructuralState {
            pattern: "x = 1".into(),
            replacement: "x = '😀'".into(),
            ..Default::default()
        };
        state.results.push((
            path.clone(),
            1,
            "x = 1".into(),
            TextRange::empty(TextSize::new(0)),
        ));
        assert_eq!(app.structural_replace_all(&mut state), 0);
        assert!(state
            .error
            .as_deref()
            .unwrap()
            .contains("cannot be encoded as Latin-1"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        state.replacement = "x = 2".into();
        assert_eq!(app.structural_replace_all(&mut state), 1);
        assert!(state.error.is_none());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"# coding: latin-1\r\nx = 2\r\n"
        );
    }
}

#[cfg(test)]
mod connected_tests {
    use super::*;

    fn frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> Vec<(String, Rect)> {
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
                ..Default::default()
            },
            |ui| {
                app.dialogs(ui.ctx());
            },
        );
        let labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((
                    text.galley.text().to_owned(),
                    Rect::from_min_size(text.pos, text.galley.size()),
                )),
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
        labels
    }
    fn click(app: &mut IdeApp, ctx: &egui::Context, at: f64, rect: Rect) {
        for (dt, pressed) in [(0.0, true), (0.02, false)] {
            frame(
                app,
                ctx,
                at + dt,
                vec![
                    egui::Event::PointerMoved(rect.center()),
                    egui::Event::PointerButton {
                        pos: rect.center(),
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }
    #[test]
    fn structural_action_rendered_find_replace_uses_same_type_and_script_filters() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sample.py");
        let original = "print('π')\nprint(42)\n";
        std::fs::write(&path, original).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&path);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        app.handle_action(Action::ReplaceStructurally, &ctx);
        let Dialog::Structural(state) = &mut app.dialog else {
            panic!("actual action did not open structural dialog")
        };
        assert!(state.replace_mode);
        state.pattern = "print($x$)".into();
        state.replacement = "log($x$)".into();
        state.options.constraints.insert(
            "x".into(),
            structural::Constraint {
                type_regex: Some("^str$".into()),
                script: Some("text.contains(\"π\")".into()),
                ..Default::default()
            },
        );
        frame(&mut app, &ctx, 0.0, vec![]);
        let labels = frame(&mut app, &ctx, 0.02, vec![]);
        let find = labels
            .iter()
            .find(|(label, _)| label == "Find")
            .expect("rendered Find control")
            .1;
        click(&mut app, &ctx, 0.1, find);
        let Dialog::Structural(state) = &app.dialog else {
            panic!("dialog unexpectedly closed")
        };
        assert_eq!(state.results.len(), 1);
        assert_eq!(state.results[0].1, 0);
        let labels = frame(&mut app, &ctx, 0.2, vec![]);
        let replace = labels
            .iter()
            .find(|(label, _)| label == "Replace All")
            .expect("rendered Replace All control")
            .1;
        click(&mut app, &ctx, 0.3, replace);
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text(),
            "log('π')\nprint(42)\n"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        app.handle_action(Action::SearchStructurally, &ctx);
        assert!(matches!(app.dialog, Dialog::Structural(ref state) if !state.replace_mode));
    }

    #[test]
    fn read_only_structural_replacement_does_not_claim_or_apply_edits() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("read_only.py");
        std::fs::write(&path, "x = 1\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&path);
        let index = app.active.unwrap();
        app.tabs[index].state.doc.set_read_only(true);
        let mut state = StructuralState {
            pattern: "x = 1".into(),
            replacement: "x = 2".into(),
            ..Default::default()
        };
        app.structural_search(&mut state);
        assert_eq!(state.results.len(), 1);
        assert_eq!(app.structural_replace_all(&mut state), 0);
        assert!(state.error.as_deref().unwrap().contains("read-only"));
        assert_eq!(app.tabs[index].state.doc.text(), "x = 1\n");
    }
}
