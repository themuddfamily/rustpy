//! Local reStructuredText helpers share the document transaction/Undo authority.
use super::*;
#[derive(Clone, Copy)]
enum Helper {
    Section(char, bool),
    Table,
}
impl IdeApp {
    pub(super) fn rst_actions_ui(&mut self, ui: &mut egui::Ui, index: usize) {
        if !self
            .tabs
            .get(index)
            .is_some_and(|tab| tab.state.language == Language::Rst)
        {
            return;
        }
        let mut helper = None;
        let mut action = None;
        ui.add_enabled_ui(self.editor_controls_available(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.menu_button("reStructuredText tools", |ui| {
                    for (name, character) in [
                        ("Section =", '='),
                        ("Subsection -", '-'),
                        ("Subsection ~", '~'),
                        ("Subsection ^", '^'),
                        ("Subsection \"", '"'),
                    ] {
                        if ui.button(crate::i18n::tr_static(name)).clicked() {
                            helper = Some(Helper::Section(character, false));
                            ui.close();
                        }
                    }
                    if ui.button("Overlined title").clicked() {
                        helper = Some(Helper::Section('=', true));
                        ui.close();
                    }
                    if ui
                        .button("Grid table from tab-separated selection / empty line")
                        .clicked()
                    {
                        helper = Some(Helper::Table);
                        ui.close();
                    }
                    ui.separator();
                    for (name, command) in [
                        ("Build Sphinx", Action::BuildSphinxDocs),
                        ("Run Docutils", Action::RunDocutilsTask),
                        ("Open generated HTML", Action::OpenDocumentationPreview),
                    ] {
                        if ui.button(crate::i18n::tr_static(name)).clicked() {
                            action = Some(command);
                            ui.close();
                        }
                    }
                });
            });
        });
        if let Some(helper) = helper {
            self.apply_rst_helper(index, helper);
        }
        if let Some(action) = action {
            self.handle_action(action, ui.ctx());
        }
    }
    fn apply_rst_helper(&mut self, index: usize, helper: Helper) {
        if !self.editor_controls_available() || !self.refresh_file_permissions(index) {
            return;
        }
        let Some(tab) = self
            .tabs
            .get(index)
            .filter(|tab| tab.state.language == Language::Rst)
        else {
            return;
        };
        let source = tab.state.doc.text();
        let selection = tab.state.primary().range();
        let start = usize::from(selection.start());
        let end = usize::from(selection.end());
        let planned = match helper {
            Helper::Section(adornment, overline) => rustpy_syntax::rst_editing::section_edit(
                &source,
                usize::from(tab.state.primary().head()),
                adornment,
                overline,
            ),
            Helper::Table => {
                let line_start = source[..start].rfind('\n').map_or(0, |offset| offset + 1);
                let line_end = source[end..]
                    .find('\n')
                    .map_or(source.len(), |offset| end + offset);
                if selection.is_empty() {
                    if !source[line_start..line_end].trim().is_empty() {
                        Err("Move to an empty line, or select complete tab-separated rows".into())
                    } else {
                        rustpy_syntax::rst_editing::grid_table(
                            "Column 1\tColumn 2\tColumn 3\n\t\t\n\t\t",
                            true,
                        )
                        .map(|table| {
                            rustpy_syntax::rst_editing::SourceEdit {
                                range: line_start..line_end,
                                replacement: table,
                            }
                        })
                    }
                } else if start != line_start
                    || (end < source.len()
                        && !source[..end].ends_with('\n')
                        && source.as_bytes()[end] != b'\n')
                {
                    Err("Select complete tab-separated lines to build a grid table".into())
                } else {
                    rustpy_syntax::rst_editing::grid_table(&source[start..end], true).map(|table| {
                        rustpy_syntax::rst_editing::SourceEdit {
                            range: start..end,
                            replacement: table,
                        }
                    })
                }
            }
        };
        match planned {
            Ok(edit) => {
                let range = TextRange::new(
                    TextSize::new(edit.range.start as u32),
                    TextSize::new(edit.range.end as u32),
                );
                self.tabs[index]
                    .state
                    .apply_text_edits(vec![(range, edit.replacement)], "reStructuredText helper");
                self.completion.visible = false;
            }
            Err(error) => self.set_status(error),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn section_helper_uses_one_undo_and_directive_option_lookup_uses_current_document() {
        let mut app = IdeApp::new(Settings::default(), None);
        let original = "界e\u{301}\n\nbody\n";
        app.push_tab(crate::editor::EditorState::from_text(
            original,
            Language::Rst,
        ));
        let index = app.active.unwrap();
        app.apply_rst_helper(index, Helper::Section('=', false));
        assert!(app.tabs[index]
            .state
            .doc
            .text()
            .starts_with("界e\u{301}\n===\n"));
        app.tabs[index].state.undo();
        assert_eq!(app.tabs[index].state.doc.text(), original);
        let source = ".. image:: a.png\n   :wi";
        app.tabs[index].state = crate::editor::EditorState::from_text(source, Language::Rst);
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(
                source.len() as u32
            ))]);
        assert!(app.open_rst_completion(true));
        assert!(app
            .completion
            .items
            .iter()
            .any(|item| item.label == "width" && item.insert == "width: "));
    }
}
