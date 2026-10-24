//! Generate .gitignore: pick bundled templates (several at once), preview the
//! patterns that would be added and merge them into `<dir>/.gitignore`
//! (roadmap "VCS detection & mapping"). Opened from the Git panel's Local
//! Changes and from Project View › New › .gitignore File.

use super::*;
use crate::a11y::DialogWindow;
use rustpy_vcs::gitignore_templates::{self, TEMPLATES};
use std::collections::BTreeSet;

pub(super) struct GitignoreTemplatesState {
    /// Folder whose `.gitignore` is created or extended.
    pub(super) dir: PathBuf,
    /// Selected template names, in `TEMPLATES` order when applied.
    pub(super) selected: BTreeSet<&'static str>,
    pub(super) filter: String,
}

impl GitignoreTemplatesState {
    pub(super) fn new(dir: PathBuf) -> Self {
        // Python projects start with the Python template ticked.
        Self {
            dir,
            selected: BTreeSet::from(["Python"]),
            filter: String::new(),
        }
    }

    /// The selected names in picker order.
    fn chosen(&self) -> Vec<&'static str> {
        TEMPLATES
            .iter()
            .map(|t| t.name)
            .filter(|name| self.selected.contains(name))
            .collect()
    }
}

impl IdeApp {
    /// Opens the template picker for the `.gitignore` in `dir`.
    pub(super) fn open_gitignore_templates(&mut self, dir: PathBuf) {
        self.dialog = Dialog::GitignoreTemplates(Box::new(GitignoreTemplatesState::new(dir)));
    }

    pub(super) fn gitignore_templates_dialog(
        &mut self,
        ctx: &egui::Context,
        mut st: Box<GitignoreTemplatesState>,
    ) {
        let theme = self.theme.clone();
        let path = st.dir.join(".gitignore");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let chosen = st.chosen();
        let preview = gitignore_templates::merge_templates(
            &existing,
            chosen
                .iter()
                .filter_map(|n| gitignore_templates::template(n)),
        );
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let mut open = !esc;
        let mut generate = false;
        let mut cancel = false;
        egui::Window::new("Generate .gitignore")
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 100.0))
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label(
                    egui::RichText::new(path.display().to_string())
                        .color(theme.text_dim)
                        .small(),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut st.filter)
                        .desired_width(f32::INFINITY)
                        .hint_text("Filter templates"),
                );
                ui.columns(2, |cols| {
                    egui::ScrollArea::vertical()
                        .id_salt("gitignore_templates_list")
                        .max_height(280.0)
                        .show(&mut cols[0], |ui| {
                            let filter = st.filter.to_lowercase();
                            for t in TEMPLATES.iter().filter(|t| {
                                filter.is_empty() || t.name.to_lowercase().contains(&filter)
                            }) {
                                let mut on = st.selected.contains(t.name);
                                if ui
                                    .checkbox(&mut on, t.name)
                                    .on_hover_text(t.description)
                                    .changed()
                                {
                                    if on {
                                        st.selected.insert(t.name);
                                    } else {
                                        st.selected.remove(t.name);
                                    }
                                }
                            }
                        });
                    let ui = &mut cols[1];
                    ui.label(format!(
                        "{} new pattern(s){}",
                        preview.added_count(),
                        if existing.is_empty() {
                            String::new()
                        } else {
                            "; existing lines are kept and not repeated".to_string()
                        }
                    ));
                    egui::ScrollArea::vertical()
                        .id_salt("gitignore_templates_preview")
                        .max_height(260.0)
                        .show(ui, |ui| {
                            if preview.added.is_empty() {
                                ui.label(
                                    egui::RichText::new(if chosen.is_empty() {
                                        "Select one or more templates"
                                    } else {
                                        "All selected patterns are already ignored"
                                    })
                                    .color(theme.text_dim),
                                );
                            }
                            for (name, patterns) in &preview.added {
                                ui.label(
                                    egui::RichText::new(format!("# {name}"))
                                        .monospace()
                                        .color(theme.text_dim),
                                );
                                for pattern in patterns {
                                    ui.monospace(pattern);
                                }
                            }
                        });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(preview.added_count() > 0, egui::Button::new("Generate"))
                        .clicked()
                    {
                        generate = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if generate {
            match gitignore_templates::apply_templates(&st.dir, &chosen) {
                Ok(merged) => {
                    self.set_status(format!(
                        "Added {} pattern(s) to {}",
                        merged.added_count(),
                        path.display()
                    ));
                    if let Some(p) = &mut self.project {
                        p.tree.invalidate(&st.dir);
                        p.git_refreshed = -100.0;
                    }
                    self.git_panel.last_refresh = -100.0;
                    self.open_file(&path);
                }
                Err(e) => self.set_status(format!("Cannot update .gitignore: {e}")),
            }
        } else if open && !cancel {
            self.dialog = Dialog::GitignoreTemplates(st);
        }
    }
}
