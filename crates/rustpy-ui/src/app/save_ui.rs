//! Save actions and the status-bar line separator widget (roadmap §0.2, §1.6
//! "Line separators", EditorConfig on save).

use std::collections::HashSet;

use super::*;
use crate::editor::save_actions::{self, SavePolicy};
use rustpy_settings::StripTrailing;
use rustpy_text::LineEnding;

/// Plan only: the candidate and live editor remain unchanged.
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_save_cleanup(
    doc: &Document,
    path: &Path,
    language: Language,
    carets: &[crate::editor::Caret],
    settings: &rustpy_settings::EditorSettings,
    ec: &rustpy_settings::editorconfig::EditorConfigProps,
    baseline_bytes: Option<&[u8]>,
    baseline_encoding: Option<rustpy_text::Encoding>,
) -> anyhow::Result<(Vec<rustpy_text::Edit>, rustpy_text::Encoding, LineEnding)> {
    let policy = SavePolicy::resolve(
        settings.strip_trailing_spaces_on_save,
        settings.ensure_newline_at_eof,
        ec,
    );
    let disk = if policy.strip == StripTrailing::ModifiedLines {
        baseline_bytes
            .map(|bytes| {
                Document::from_bytes_with_policy(
                    bytes,
                    rustpy_text::SourcePolicy::for_path(path),
                    baseline_encoding,
                )
                .map(|doc| doc.text())
            })
            .transpose()?
    } else {
        None
    };
    let mut keep: HashSet<usize> = if settings.keep_trailing_spaces_on_caret_line {
        carets
            .iter()
            .map(|caret| doc.line_of(caret.head()))
            .collect()
    } else {
        HashSet::new()
    };
    if language == Language::Markdown {
        keep.extend((0..doc.line_count()).filter(|&line| doc.line_text(line).ends_with("  ")));
    }
    let strings = if policy.strip != StripTrailing::None && language.is_pythonic() {
        save_actions::python_string_ranges(&doc.text())
    } else {
        Vec::new()
    };
    Ok((
        save_actions::save_edits(doc, &policy, disk.as_deref(), &keep, &strings),
        policy.encoding.unwrap_or(doc.encoding()),
        policy.line_ending.unwrap_or(doc.line_ending()),
    ))
}

impl IdeApp {
    // Retained direct helper for the existing readonly/nested regression tests.
    // Production saves stage cleanup through `save_pipeline` before writing.
    #[cfg(test)]
    pub(super) fn apply_save_actions(
        &mut self,
        i: usize,
        baseline_bytes: Option<&[u8]>,
    ) -> anyhow::Result<()> {
        let Some(tab) = self.tabs.get(i) else {
            return Ok(());
        };
        let Some(path) = tab.state.path.clone() else {
            return Ok(());
        };
        if tab.state.doc.is_read_only() || tab.image.is_some() {
            return Ok(());
        }
        let ec = self.settings.editor.editorconfig_for(&path);
        let baseline_encoding = self.encoding_override(&path).map_err(anyhow::Error::msg)?;
        let (edits, encoding, ending) = prepare_save_cleanup(
            &tab.state.doc,
            &path,
            tab.state.language,
            &tab.state.carets,
            &self.settings.editor,
            &ec,
            baseline_bytes,
            baseline_encoding,
        )?;
        let plan = tab.state.doc.prepare_save(
            vec![edits],
            encoding,
            ending,
            rustpy_text::SourcePolicy::for_path(&path),
        )?;
        self.tabs[i].state.commit_prepared_save(plan)?;
        Ok(())
    }

    /// Status-bar line separator of the active editor. Mixed separators show a
    /// warning; choosing a separator converts every line (the file is marked
    /// modified until it is saved).
    pub(super) fn line_separator_widget(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.active.filter(|i| *i < self.tabs.len()) else {
            return;
        };
        let theme = self.theme.clone();
        let (le, mixed) = (
            self.tabs[i].state.doc.line_ending(),
            self.tabs[i].state.doc.has_mixed_line_endings(),
        );
        let (text, color) = if mixed {
            (format!("{} (mixed)", le.label()), theme.warning)
        } else {
            (le.label().to_string(), theme.text)
        };
        let mut chosen: Option<LineEnding> = None;
        let mut project_chosen: Option<LineEnding> = None;
        let resp = ui.menu_button(egui::RichText::new(text).color(color), |ui| {
            if mixed {
                ui.label(
                    egui::RichText::new("This file mixes line separators").color(theme.warning),
                );
                if ui
                    .button(format!("Convert Line Separators to {}", le.label()))
                    .clicked()
                {
                    chosen = Some(le);
                    ui.close();
                }
                ui.separator();
            }
            if self.project.is_some() {
                ui.menu_button("Change Project Line Separators", |ui| {
                    ui.label("Sets end_of_line in the project's .editorconfig for future saves.");
                    ui.label("Nested .editorconfig files retain precedence. Existing files are not rewritten now.");
                    for (label,value) in [("LF",LineEnding::Lf),("CRLF",LineEnding::CrLf),("CR",LineEnding::Cr)] {
                        if ui.button(crate::i18n::tr_static(label)).clicked() { project_chosen=Some(value); ui.close(); }
                    }
                });
                ui.separator();
            }
            ui.label(
                egui::RichText::new("Line Separator")
                    .color(theme.text_dim)
                    .small(),
            );
            for (label, v) in [
                ("LF - Unix and macOS (\\n)", LineEnding::Lf),
                ("CRLF - Windows (\\r\\n)", LineEnding::CrLf),
                ("CR - Classic Mac OS (\\r)", LineEnding::Cr),
            ] {
                if ui.selectable_label(le == v && !mixed, crate::i18n::tr_static(label)).clicked() {
                    chosen = Some(v);
                    ui.close();
                }
            }
        });
        let hover = if mixed {
            format!(
                "Line separators are mixed: saving converts every line to {}. Click to choose another separator.",
                le.label()
            )
        } else {
            "Line separator: click to convert".to_string()
        };
        resp.response.on_hover_text(hover);
        if let Some(ending) = project_chosen {
            self.change_project_line_separators(ending);
        }
        if let Some(v) = chosen {
            let tab = &mut self.tabs[i];
            if tab.state.doc.is_read_only() || tab.image.is_some() {
                self.set_status("The file is read-only: its line separators cannot be changed");
                return;
            }
            tab.state.doc.set_line_ending(v);
            let msg = if tab.state.doc.is_modified() {
                format!(
                    "Line separators: {} (save the file to convert it)",
                    v.label()
                )
            } else {
                format!("Line separators: {}", v.label())
            };
            self.set_status(msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_text::Encoding;

    fn fixture(
        text: &str,
        encoding: Encoding,
        editorconfig: &str,
    ) -> (tempfile::TempDir, IdeApp, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.py");
        std::fs::write(&path, encoding.encode(text).unwrap()).unwrap();
        std::fs::write(dir.path().join(".editorconfig"), editorconfig).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        settings.editor.strip_trailing_spaces_on_save = StripTrailing::All;
        settings.editor.ensure_newline_at_eof = true;
        let mut app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        assert_eq!(app.project.as_ref().unwrap().root, dir.path());
        assert!(app.tabs.is_empty());
        app.open_file(&path);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].state.path.as_deref(), Some(path.as_path()));
        (dir, app, path)
    }

    #[test]
    fn failed_save_preflight_preserves_live_text_carets_history_format_and_disk() {
        let (_dir, mut app, path) = fixture(
            "# coding: latin-1\r\nx=1  \r\n# end",
            Encoding::Latin1,
            "root=true\n[*]\nend_of_line=lf\ncharset=latin1\n",
        );
        let end = app.tabs[0].state.doc.len();
        app.tabs[0].state.apply_text_edits(
            vec![(TextRange::empty(end), "😀".into())],
            "Unrepresentable edit",
        );
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(0))]);
        let doc = &app.tabs[0].state.doc;
        let text = doc.text();
        let version = doc.version();
        let history = doc.undo_history();
        let current = doc.current_history();
        let format = (
            doc.encoding(),
            doc.line_ending(),
            doc.has_mixed_line_endings(),
        );
        let carets = app.tabs[0].state.carets.clone();
        let disk = std::fs::read(&path).unwrap();
        let revision = app.disk_revisions[&path];
        assert!(!app.save_tab(0));
        let doc = &app.tabs[0].state.doc;
        assert_eq!(doc.text(), text);
        assert_eq!(doc.version(), version);
        assert_eq!(doc.undo_history(), history);
        assert_eq!(doc.current_history(), current);
        assert_eq!(
            (
                doc.encoding(),
                doc.line_ending(),
                doc.has_mixed_line_endings()
            ),
            format
        );
        assert_eq!(app.tabs[0].state.carets, carets);
        assert!(doc.is_modified());
        assert_eq!(std::fs::read(&path).unwrap(), disk);
        assert_eq!(app.disk_revisions[&path], revision);
    }
    #[test]
    fn successful_save_preflight_preserves_multicaret_and_one_undo_cleanup() {
        let original = "a=1  \nb=2  \n# end";
        let (_dir, mut app, path) = fixture(
            original,
            Encoding::Utf8,
            "root=true\n[*]\nend_of_line=crlf\ncharset=utf-8-bom\n",
        );
        let end = app.tabs[0].state.doc.len();
        app.tabs[0].state.set_carets(vec![
            crate::editor::Caret::at(TextSize::new(0)),
            crate::editor::Caret::at(end),
        ]);
        assert!(app.save_tab(0));
        let cleaned = "a=1  \nb=2\n# end\n";
        assert_eq!(app.tabs[0].state.doc.text(), cleaned);
        assert_eq!(app.tabs[0].state.carets.len(), 2);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            Encoding::Utf8Bom
                .encode(&LineEnding::CrLf.apply(cleaned))
                .unwrap()
        );
        assert!(!app.tabs[0].state.doc.is_modified());
        let history = app.tabs[0].state.doc.undo_history();
        let version = app.tabs[0].state.doc.version();
        assert!(app.save_tab(0));
        assert_eq!(app.tabs[0].state.doc.undo_history(), history);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert_eq!(
            app.tabs[0].state.doc.undo_label().as_deref(),
            Some("Save Actions")
        );
        assert!(app.tabs[0].state.doc.undo().is_some());
        assert_eq!(app.tabs[0].state.doc.text(), original);
        assert!(
            !app.tabs[0].state.doc.can_undo(),
            "all cleanup text edits form one undo unit"
        );
    }

    #[test]
    fn save_preflight_rejects_python_utf16_before_any_live_format_or_text_change() {
        let (_dir, mut app, path) = fixture(
            "a=1  \nb=2  ",
            Encoding::Utf8,
            "root=true\n[*]\ncharset=utf-16le\nend_of_line=crlf\n",
        );
        let text = app.tabs[0].state.doc.text();
        let history = app.tabs[0].state.doc.undo_history();
        let version = app.tabs[0].state.doc.version();
        let bytes = std::fs::read(&path).unwrap();
        assert!(!app.save_tab(0));
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert_eq!(app.tabs[0].state.doc.undo_history(), history);
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Utf8);
        assert_eq!(app.tabs[0].state.doc.line_ending(), LineEnding::Lf);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn save_preflight_keeps_readonly_and_nested_undo_boundaries() {
        let (_dir, mut app, path) = fixture("a=1  \nb=2  ", Encoding::Utf8, "root=true\n[*]\n");
        let disk = std::fs::read(&path).unwrap();
        app.tabs[0].state.doc.set_read_only(true);
        let version = app.tabs[0].state.doc.version();
        app.apply_save_actions(0, Some(&disk)).unwrap();
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert_eq!(app.tabs[0].state.doc.text(), "a=1  \nb=2  ");
        app.tabs[0].state.doc.set_read_only(false);
        let before: Vec<_> = app.tabs[0].state.carets.iter().map(|c| c.sel).collect();
        let outer = app.tabs[0]
            .state
            .doc
            .begin_transaction(&before, Some("Outer command"))
            .unwrap();
        app.apply_save_actions(0, Some(&disk)).unwrap();
        assert_eq!(app.tabs[0].state.doc.transaction_depth(), 1);
        let after: Vec<_> = app.tabs[0].state.carets.iter().map(|c| c.sel).collect();
        app.tabs[0]
            .state
            .doc
            .commit_transaction(outer, &after)
            .unwrap();
        assert_eq!(
            app.tabs[0].state.doc.undo_label().as_deref(),
            Some("Outer command")
        );
        app.tabs[0].state.doc.undo().unwrap();
        assert_eq!(app.tabs[0].state.doc.text(), "a=1  \nb=2  ");
        assert!(!app.tabs[0].state.doc.can_undo());
    }
}
