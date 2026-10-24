//! New Scratch File (Ctrl+Alt+Shift+Insert) and scratch-file actions
//! (roadmap §1.6 "Scratch files").

use super::*;
use crate::a11y::DialogWindow;
use crate::scratches;

impl IdeApp {
    /// Creates a scratch file with extension `ext` and opens it.
    pub(super) fn create_scratch(&mut self, ext: &str) {
        let dir = scratches::scratch_dir();
        match scratches::create(&dir, ext) {
            Ok(path) => {
                self.open_file(&path);
                if let Some(p) = &mut self.project {
                    p.tree.invalidate(&dir);
                    p.tree.expanded.insert(dir.clone());
                }
                self.set_status(format!(
                    "Created {} in {}",
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    dir.display()
                ));
            }
            Err(e) => self.set_status(format!(
                "Cannot create a scratch file in {}: {e}",
                dir.display()
            )),
        }
    }

    /// New Scratch Buffer: reuse one of five numbered real files, never truncate.
    pub(super) fn create_scratch_buffer(&mut self) {
        self.create_scratch_buffer_in(&scratches::scratch_dir());
    }
    fn create_scratch_buffer_in(&mut self, dir: &Path) {
        match scratches::next_buffer(dir) {
            Ok(path) => {
                self.open_file(&path); // Existing tab retains unsaved contents/history.
                if let Some(project) = &mut self.project {
                    project.tree.invalidate(dir);
                    project.tree.expanded.insert(dir.to_path_buf());
                }
                self.set_status(format!(
                    "Scratch buffer {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            Err(error) => self.set_status(format!("Cannot open scratch buffer: {error}")),
        }
    }

    /// Scratch file › Change Language: renames the extension and re-highlights open editors.
    pub(super) fn change_scratch_language(&mut self, path: &Path, ext: &str) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.state.path.as_deref() == Some(path))
        {
            if self.tabs[i].state.doc.is_modified() {
                self.save_tab(i);
            }
        }
        match scratches::change_language(path, ext) {
            Ok(new_path) => {
                for t in &mut self.tabs {
                    if t.state.path.as_deref() == Some(path) {
                        t.state.path = Some(new_path.clone());
                        t.state.set_language(Language::from_path(&new_path));
                    }
                }
                let grammars: Vec<_> = self
                    .tabs
                    .iter()
                    .map(|tab| self.settings_ui.textmate.grammar_for_scratch(&tab.state))
                    .collect();
                for (tab, grammar) in self.tabs.iter_mut().zip(grammars) {
                    if tab.state.path.as_deref() == Some(new_path.as_path()) {
                        tab.state.set_textmate_grammar(grammar);
                    }
                }
                self.invalidate_analysis_paths(&[path.to_path_buf(), new_path.clone()]);
                if let Some(p) = &mut self.project {
                    p.tree.invalidate_all();
                }
                self.set_status(format!(
                    "{} is now {}",
                    new_path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    self.tabs
                        .iter()
                        .find(|tab| tab.state.path.as_deref() == Some(new_path.as_path()))
                        .map(|tab| tab.state.language_label())
                        .unwrap_or_else(|| scratches::language_of(&new_path).to_string())
                ));
            }
            Err(e) => self.set_status(format!("Cannot change the language: {e}")),
        }
    }

    /// The New Scratch File language picker: type to filter, arrows and Enter.
    pub(super) fn new_scratch_dialog(
        &mut self,
        ctx: &egui::Context,
        mut query: String,
        mut selected: usize,
    ) {
        let theme = self.theme.clone();
        let q = query.to_lowercase();
        let mut choices: Vec<(String, String)> = scratches::LANGUAGES
            .iter()
            .map(|(name, ext)| (name.to_string(), ext.to_string()))
            .collect();
        choices.extend(self.settings_ui.textmate.scratch_languages());
        choices.sort();
        choices.dedup();
        let langs: Vec<(&str, &str)> = choices
            .iter()
            .map(|(name, ext)| (name.as_str(), ext.as_str()))
            .filter(|(name, ext)| {
                q.is_empty() || name.to_lowercase().contains(&q) || ext.starts_with(&q)
            })
            .collect();
        selected = selected.min(langs.len().saturating_sub(1));
        let mut chosen: Option<&str> = None;
        let mut open = true;
        let (down, up, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if down && selected + 1 < langs.len() {
            selected += 1;
        }
        if up {
            selected = selected.saturating_sub(1);
        }
        if enter {
            chosen = langs.get(selected).map(|(_, ext)| *ext);
        }
        egui::Window::new("New Scratch File")
            .collapsible(false)
            .resizable(false)
            .default_width(300.0)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 120.0))
            .open(&mut open)
            .show_modal(ctx, |ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut query)
                        .desired_width(280.0)
                        .hint_text("Language"),
                );
                resp.request_focus();
                for (i, (name, ext)) in langs.iter().enumerate() {
                    let label = egui::RichText::new(if scratches::is_textmate_token(ext) {
                        name.to_string()
                    } else {
                        format!("{name}  (.{ext})")
                    })
                    .color(if i == selected {
                        theme.text
                    } else {
                        theme.text_dim
                    });
                    if ui.selectable_label(i == selected, label).clicked() {
                        chosen = Some(ext);
                    }
                }
                if langs.is_empty() {
                    ui.label(egui::RichText::new("No matching language").color(theme.text_dim));
                }
                ui.label(
                    egui::RichText::new(format!("Saved in {}", scratches::scratch_dir().display()))
                        .color(theme.text_dim)
                        .small(),
                );
            });
        if let Some(ext) = chosen {
            self.create_scratch(ext);
        } else if open && !esc {
            self.dialog = Dialog::NewScratch(query, selected);
        } else {
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_app_reuses_numbered_buffers_without_losing_unsaved_text_or_undo() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("scratches");
        let mut app = IdeApp::new(Settings::default(), None);
        app.create_scratch_buffer_in(&dir);
        assert_eq!(app.tabs.len(), 1);
        let first = app.tabs[0].view.id;
        app.tabs[0]
            .state
            .insert_text("π🙂 unsaved", &crate::editor::EditOptions::default());
        assert!(app.tabs[0].state.doc.is_modified());
        for _ in 1..scratches::BUFFER_COUNT {
            app.create_scratch_buffer_in(&dir);
        }
        assert_eq!(app.tabs.len(), scratches::BUFFER_COUNT);
        app.create_scratch_buffer_in(&dir);
        assert_eq!(app.tabs[app.active.unwrap()].view.id, first);
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text(),
            "π🙂 unsaved"
        );
        assert_eq!(std::fs::read(dir.join("buffer1.txt")).unwrap(), b"");
        assert!(app.tabs[app.active.unwrap()].state.undo());
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), "");
        assert_eq!(app.tabs.len(), scratches::BUFFER_COUNT);
    }
}
