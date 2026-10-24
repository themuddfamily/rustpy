//! Application insertions plan from materialized ranges inside the owned edit
//! transaction. Template stops and completion side effects publish on success.
use super::*;
use crate::editor::{Caret, EditorState};

pub(super) type TemplatePlacement = (Vec<(String, Vec<TextRange>)>, TextSize);

pub(super) fn apply_expansion(
    state: &mut EditorState,
    exp: &templates::Expansion,
    replace: TextRange,
) -> Option<TemplatePlacement> {
    let base = usize::from(replace.start());
    let mut vars: Vec<(String, Vec<TextRange>)> = Vec::new();
    for (s, e, name) in &exp.tab_stops {
        let r = TextRange::new(
            TextSize::new((base + s) as u32),
            TextSize::new((base + e) as u32),
        );
        if let Some(v) = vars.iter_mut().find(|(n, _)| n == name) {
            v.1.push(r);
        } else {
            vars.push((name.clone(), vec![r]));
        }
    }
    let end = TextSize::new((base + exp.end_offset.unwrap_or(exp.text.len())) as u32);
    let caret = match vars.first().and_then(|v| v.1.first()) {
        Some(r) => Caret::new(r.start(), r.end()),
        None => Caret::at(end),
    };
    state
        .apply(
            vec![rustpy_text::Edit::replace(replace, exp.text.clone())],
            vec![caret],
            rustpy_text::EditGroup::Separate,
            Some("Live template"),
        )
        .then_some((vars, end))
}

impl IdeApp {
    /// True means the virtual path handled/refused the item; false preserves the
    /// existing physical-prefix path byte for byte.
    pub(super) fn accept_virtual_completion(
        &mut self,
        item: &CompletionItem,
        replace: bool,
    ) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        if !self.tabs[index].state.has_virtual_padding() {
            return false;
        }
        let naming = self.inplace_rename_in(index);
        if item.kind == CompletionKind::Template && !naming {
            if let Some(template) = self
                .templates
                .iter()
                .find(|t| t.abbreviation == item.label.as_str())
                .cloned()
            {
                let range = self.tabs[index].state.primary().range();
                self.expand_template(&template, range, None);
            }
            self.completion.visible = false;
            self.import_completion = None;
            return true;
        }
        if item.kind == CompletionKind::Postfix && !naming {
            // There is no expression at a phantom position; do not apply a
            // postfix transform to the unrelated expression before the gap.
            self.set_status("Postfix completion requires an expression at the caret");
            self.completion.visible = false;
            self.import_completion = None;
            return true;
        }
        let import = self
            .completion
            .imports
            .get(self.completion.selected)
            .cloned()
            .flatten()
            .filter(|_| !naming);
        let file = if import.is_some() {
            let Some((_, file)) = self.analysis_file(index) else {
                self.completion.visible = false;
                self.import_completion = None;
                return true;
            };
            Some(file)
        } else {
            None
        };
        let parens = self.settings.completion.insert_parentheses;
        let label = if import.is_some() {
            "Complete with import"
        } else {
            "Complete"
        };
        let success = self.tabs[index]
            .state
            .try_edit_in_virtual_space(label, |state| {
                if let (Some((module, name)), Some(file)) = (&import, &file) {
                    let candidate = file.with_candidate_text(state.doc.text());
                    let (range, text) =
                        rustpy_semantic::completion2::import_edit_for(&candidate, module, name);
                    if !text.is_empty() {
                        let edits = vec![rustpy_text::Edit::replace(range, text)];
                        let carets = state
                            .carets
                            .iter()
                            .map(|caret| {
                                Caret::new(
                                    crate::editor::state::map_offset(
                                        &edits,
                                        caret.sel.anchor,
                                        true,
                                    ),
                                    crate::editor::state::map_offset(&edits, caret.head(), true),
                                )
                            })
                            .collect();
                        if !state.apply(edits, carets, rustpy_text::EditGroup::Open, Some(label)) {
                            return false;
                        }
                    }
                }
                // Phantom columns do not authorize replacing the earlier physical
                // identifier. Recompute each insertion/selection after padding/import.
                let mut planned = Vec::new();
                for caret in &state.carets {
                    let mut range = caret.range();
                    if range.is_empty() && replace {
                        let mut end = range.end();
                        while let Some(c) = state
                            .doc
                            .char_at(end)
                            .filter(|c| rustpy_core::is_ident_continue(*c))
                        {
                            end += TextSize::of(c);
                        }
                        range = TextRange::new(range.start(), end);
                    }
                    let (text, offset) = if naming {
                        (item.label.to_string(), None)
                    } else {
                        features::insert_text_for(item, parens, state.doc.char_at(range.end()))
                    };
                    planned.push((range, text, offset));
                }
                let mut edits: Vec<_> = planned
                    .iter()
                    .map(|(range, text, _)| rustpy_text::Edit::replace(*range, text.clone()))
                    .collect();
                edits.sort_by_key(|e| e.range.start());
                let carets = planned
                    .iter()
                    .map(|(range, text, offset)| {
                        Caret::at(
                            crate::editor::state::map_offset(&edits, range.start(), false)
                                + TextSize::new(offset.unwrap_or(text.len()) as u32),
                        )
                    })
                    .collect();
                state.apply(edits, carets, rustpy_text::EditGroup::Separate, Some(label))
            });
        if success == Some(true) {
            *self
                .completion_stats
                .entry(item.label.to_string())
                .or_insert(0) += 1;
            save_completion_stats(&self.completion_stats);
            if let Some((module, name)) = import {
                self.set_status(if name.is_empty() {
                    format!("Added import {module}")
                } else {
                    format!("Added from {module} import {name}")
                });
            }
        }
        self.completion.visible = false;
        self.import_completion = None;
        true
    }
}
