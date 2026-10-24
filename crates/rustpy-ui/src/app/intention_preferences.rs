//! Intention preferences and previews use the same edits as Alt+Enter.
use super::*;
use rustpy_analysis::inspections::infrastructure;
use std::collections::BTreeSet;

fn preview_text(source: &str, fix: &QuickFix) -> Option<String> {
    (!fix.edits.is_empty())
        .then(|| refactor::try_apply_edits(source, &fix.edits).ok())
        .flatten()
}

impl IdeApp {
    pub(super) fn append_fix_all_intentions(&self, fixes: &mut Vec<QuickFix>) {
        let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) else {
            return;
        };
        let current = analysis_jobs::current_quick_fixes(&tab.state);
        let mut batches = Vec::new();
        for fix in fixes
            .iter()
            .filter(|fix| !fix.edits.is_empty() && !fix.is_suppression())
        {
            let problems: Vec<_> = current
                .iter()
                .filter_map(|(range, available)| {
                    let matching = available.iter().find(|candidate| {
                        candidate.title == fix.title
                            && !candidate.is_suppression()
                            && !candidate.edits.is_empty()
                    })?;
                    Some(inspections::Problem {
                        diagnostic: rustpy_core::Diagnostic::new(
                            *range,
                            rustpy_core::Severity::Info,
                            "IntentionBatch",
                            fix.title.clone(),
                        ),
                        fixes: vec![matching.clone()],
                    })
                })
                .collect();
            if problems.len() < 2 {
                continue;
            }
            let edits = infrastructure::batch_edits(&problems, None, Some(&fix.title), false);
            if edits.is_empty()
                || edits == fix.edits
                || refactor::try_apply_edits(&tab.state.doc.text(), &edits).is_err()
            {
                continue;
            }
            let title = format!("Fix all in file: {}", fix.title);
            if !self.settings.disabled_intentions.contains(&title)
                && !batches.iter().any(|batch: &QuickFix| batch.title == title)
            {
                batches.push(QuickFix { title, edits });
            }
        }
        fixes.extend(batches);
    }

    pub(super) fn intention_preview(&self, ui: &mut egui::Ui, fix: &QuickFix) {
        if !self.settings.editor.show_intention_preview {
            return;
        }
        let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) else {
            return;
        };
        let source = tab.state.doc.text();
        let Some(after) = preview_text(&source, fix) else {
            ui.label("This action opens a dialog or changes a setting.");
            return;
        };
        ui.separator();
        ui.set_min_width(500.0);
        ui.label(egui::RichText::new("Preview").strong());
        ui.allocate_ui(egui::vec2(ui.available_width(), 180.0), |ui| {
            crate::diff_view::SideBySide::new(&source, &after).ui(
                ui,
                &self.theme,
                egui::FontId::monospace(12.0),
                "Before",
                "After",
            );
        });
    }

    pub(super) fn intention_preferences_page(&mut self, ui: &mut egui::Ui, known: &[&str]) {
        ui.heading("Intention Actions");
        ui.checkbox(
            &mut self.settings.editor.show_intention_bulb,
            "Show intention bulb",
        );
        ui.checkbox(
            &mut self.settings.editor.show_intention_preview,
            "Show intention preview in popup",
        );
        ui.label("Uncheck an intention to hide it from Alt+Enter. Select its name to preview the actual change in the current editor context.");
        let mut names: BTreeSet<String> = known
            .iter()
            .map(|name| (*name).into())
            .chain(self.settings.disabled_intentions.iter().cloned())
            .collect();
        let mut available = Vec::new();
        let source = self.active.map(|index| {
            let state = &self.tabs[index].state;
            for (_, fixes) in analysis_jobs::current_quick_fixes(state) {
                for fix in fixes {
                    if !available
                        .iter()
                        .any(|other: &QuickFix| other.title == fix.title)
                    {
                        available.push(fix.clone());
                    }
                }
            }
            let caret = state.primary().head();
            let source = state.doc.text();
            if let Some((analysis, file)) = self.analysis_file(index) {
                available.extend(
                    rustpy_analysis::intentions::intentions_at(&analysis, &file, caret)
                        .into_iter()
                        .map(|intention| QuickFix {
                            title: intention.title,
                            edits: intention.edits,
                        }),
                );
            }
            source
        });
        names.extend(available.iter().map(|fix| fix.title.clone()));
        let selection_id = ui.id().with("intention-settings-selection");
        let mut selected = ui
            .ctx()
            .data(|data| data.get_temp::<String>(selection_id))
            .unwrap_or_else(|| {
                available
                    .first()
                    .map_or_else(String::new, |fix| fix.title.clone())
            });
        egui::ScrollArea::vertical()
            .id_salt("intention-preferences-list")
            .max_height(230.0)
            .show(ui, |ui| {
                for name in names {
                    ui.horizontal(|ui| {
                        let mut enabled = !self.settings.disabled_intentions.contains(&name);
                        if ui
                            .checkbox(&mut enabled, "")
                            .on_hover_text(format!("Enable {name}"))
                            .changed()
                        {
                            if enabled {
                                self.settings
                                    .disabled_intentions
                                    .retain(|title| title != &name);
                            } else {
                                self.settings.disabled_intentions.push(name.clone());
                            }
                        }
                        if ui.selectable_label(selected == name, &name).clicked() {
                            selected = name;
                        }
                    });
                }
            });
        ui.ctx()
            .data_mut(|data| data.insert_temp(selection_id, selected.clone()));
        ui.separator();
        ui.label(egui::RichText::new(&selected).strong());
        if let Some((before, after)) = source.as_deref().and_then(|before| {
            available
                .iter()
                .find(|fix| fix.title == selected)
                .and_then(|fix| preview_text(before, fix))
                .map(|after| (before, after))
        }) {
            ui.allocate_ui(egui::vec2(ui.available_width(), 220.0), |ui| {
                crate::diff_view::SideBySide::new(before, &after).ui(
                    ui,
                    &self.theme,
                    egui::FontId::monospace(12.0),
                    "Before",
                    "After",
                );
            });
        } else {
            ui.label("A preview is available when this action has text edits at the editor's current caret. Dialog actions collect their options when invoked.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> IdeApp {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("a = 1;\nb = 2;\n", Language::Python));
        let state = &mut app.tabs[0].state;
        state.semantic_version = Some(state.doc.version());
        state.quick_fixes = [5u32, 12]
            .into_iter()
            .map(|offset| {
                let range = TextRange::new(TextSize::new(offset), TextSize::new(offset + 1));
                (
                    range,
                    vec![QuickFix {
                        title: "Remove trailing semicolon".into(),
                        edits: vec![(range, String::new())],
                    }],
                )
            })
            .collect();
        app
    }
    #[test]
    fn batch_preview_and_apply_share_edits_with_one_undo_and_stale_guard() {
        let mut app = fixture();
        let mut fixes = vec![app.tabs[0].state.quick_fixes[0].1[0].clone()];
        app.append_fix_all_intentions(&mut fixes);
        let batch = fixes
            .iter()
            .find(|fix| fix.title.starts_with("Fix all in file:"))
            .unwrap();
        let before = app.tabs[0].state.doc.text();
        let after = preview_text(&before, batch).unwrap();
        assert_eq!(after, "a = 1\nb = 2\n");
        app.tabs[0]
            .state
            .apply_text_edits(batch.edits.clone(), &batch.title);
        assert_eq!(app.tabs[0].state.doc.text(), after);
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), before);
        let mut stale = vec![fixes[0].clone()];
        app.append_fix_all_intentions(&mut stale);
        assert_eq!(
            stale.len(),
            1,
            "undo advanced the version so cached fixes cannot batch"
        );
    }
    #[test]
    fn suppression_and_dialog_actions_never_become_batch_text_edits() {
        let app = fixture();
        let mut fixes = vec![QuickFix {
            title: "Open settings".into(),
            edits: Vec::new(),
        }];
        app.append_fix_all_intentions(&mut fixes);
        assert_eq!(fixes.len(), 1);
        assert!(preview_text("text", &fixes[0]).is_none());
        let invalid = QuickFix {
            title: "Invalid old range".into(),
            edits: vec![(
                TextRange::new(TextSize::new(0), TextSize::new(10)),
                String::new(),
            )],
        };
        assert!(preview_text("text", &invalid).is_none());
    }
    #[test]
    fn bulb_preference_hides_only_bulbs_and_preferences_round_trip() {
        let mut app = fixture();
        assert_eq!(app.gutter_view_inputs(0).1.len(), 2);
        app.settings.editor.show_intention_bulb = false;
        assert!(app.gutter_view_inputs(0).1.is_empty());
        assert_eq!(
            analysis_jobs::current_quick_fixes(&app.tabs[0].state).len(),
            2
        );
        app.settings.editor.show_intention_preview = false;
        let loaded: Settings =
            serde_json::from_str(&serde_json::to_string(&app.settings).unwrap()).unwrap();
        assert!(!loaded.editor.show_intention_bulb);
        assert!(!loaded.editor.show_intention_preview);
    }
}
