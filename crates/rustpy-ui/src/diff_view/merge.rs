//! Three-way line merge with explicit per-conflict choices. No repository writes.
use super::model::Model;
use crate::theme::UiTheme;
use rustpy_settings::diff_merge::{DiffMergeSettings, Ignore};
use std::ops::Range;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Left,
    Right,
    Base,
    Both,
}
#[derive(Clone, Debug)]
pub struct Chunk {
    pub base: String,
    pub left: String,
    pub right: String,
    pub conflict: bool,
    pub choice: Option<Choice>,
}
#[derive(Clone)]
struct Edit {
    range: Range<usize>,
    text: String,
    left: bool,
}
pub struct MergeViewer {
    pub chunks: Vec<Chunk>,
    pub result: String,
    pub options: DiffMergeSettings,
    manual: bool,
    selected: usize,
    scroll: f32,
    preview: Option<(usize, super::SideBySide)>,
}
fn edited(base: &[String], range: Range<usize>, edits: &[Edit], left: bool) -> String {
    let mut lines = base[range.clone()].to_vec();
    for edit in edits.iter().filter(|edit| edit.left == left).rev() {
        lines.splice(
            edit.range.start - range.start..edit.range.end - range.start,
            [edit.text.clone()],
        );
    }
    lines.concat()
}
fn render_chunk(chunk: &Chunk) -> String {
    match chunk.choice {
        Some(Choice::Left) => chunk.left.clone(),
        Some(Choice::Right) => chunk.right.clone(),
        Some(Choice::Base) => chunk.base.clone(),
        Some(Choice::Both) => format!("{}{}", chunk.left, chunk.right),
        None => format!(
            "<<<<<<< theirs\n{}{}||||||| base\n{}{}=======\n{}{}>>>>>>> yours\n",
            chunk.left,
            if chunk.left.ends_with('\n') { "" } else { "\n" },
            chunk.base,
            if chunk.base.ends_with('\n') { "" } else { "\n" },
            chunk.right,
            if chunk.right.ends_with('\n') {
                ""
            } else {
                "\n"
            }
        ),
    }
}
impl MergeViewer {
    pub fn new(base: &str, theirs: &str, ours: &str, options: DiffMergeSettings) -> Self {
        let left = Model::new(base, theirs, Ignore::None, true);
        let right = Model::new(base, ours, Ignore::None, true);
        let mut edits: Vec<_> = left
            .hunks
            .iter()
            .map(|hunk| Edit {
                range: hunk.left.clone(),
                text: left.right[hunk.right.clone()].concat(),
                left: true,
            })
            .chain(right.hunks.iter().map(|hunk| Edit {
                range: hunk.left.clone(),
                text: right.right[hunk.right.clone()].concat(),
                left: false,
            }))
            .collect();
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let lines: Vec<String> = base.split_inclusive('\n').map(str::to_owned).collect();
        let mut chunks = Vec::new();
        let mut cursor = 0;
        let mut index = 0;
        while index < edits.len() {
            let start = edits[index].range.start;
            let mut end = edits[index].range.end;
            let first = index;
            index += 1;
            while index < edits.len()
                && (edits[index].range.start < end
                    || (edits[index].range.start == start && start == end))
            {
                end = end.max(edits[index].range.end);
                index += 1;
            }
            if start > cursor {
                let same = lines[cursor..start].concat();
                chunks.push(Chunk {
                    base: same.clone(),
                    left: same.clone(),
                    right: same,
                    conflict: false,
                    choice: Some(Choice::Base),
                });
            }
            let base = lines[start..end].concat();
            let left = edited(&lines, start..end, &edits[first..index], true);
            let right = edited(&lines, start..end, &edits[first..index], false);
            let conflict = left != right && left != base && right != base;
            let choice = if left == right {
                Some(Choice::Left)
            } else if !conflict && options.auto_nonconflicting && options.enable_nonconflicting {
                Some(if left == base {
                    Choice::Right
                } else {
                    Choice::Left
                })
            } else {
                None
            };
            chunks.push(Chunk {
                base,
                left,
                right,
                conflict,
                choice,
            });
            cursor = end;
        }
        if cursor < lines.len() {
            let same = lines[cursor..].concat();
            chunks.push(Chunk {
                base: same.clone(),
                left: same.clone(),
                right: same,
                conflict: false,
                choice: Some(Choice::Base),
            });
        }
        let mut viewer = Self {
            chunks,
            result: String::new(),
            options,
            manual: false,
            selected: 0,
            scroll: 0.0,
            preview: None,
        };
        viewer.rebuild();
        viewer
    }
    pub fn resolve(&mut self, index: usize, choice: Choice) {
        if let Some(chunk) = self.chunks.get_mut(index) {
            if self.manual {
                if chunk.choice.is_some() {
                    return;
                }
                let before = render_chunk(chunk);
                if !self.result.contains(&before) {
                    return;
                }
                chunk.choice = Some(choice);
                self.result = self.result.replacen(&before, &render_chunk(chunk), 1);
            } else {
                chunk.choice = Some(choice);
                self.rebuild();
            }
        }
    }
    pub fn apply_nonconflicting(&mut self) {
        if !self.options.enable_nonconflicting || self.manual {
            return;
        }
        for chunk in &mut self.chunks {
            if !chunk.conflict {
                chunk.choice = Some(if chunk.left == chunk.base {
                    Choice::Right
                } else {
                    Choice::Left
                });
            }
        }
        self.manual = false;
        self.rebuild();
    }
    pub fn set_result(&mut self, text: String) {
        self.result = text;
        self.manual = true;
    }
    pub fn unresolved(&self) -> usize {
        if self.manual {
            let markers = self
                .result
                .lines()
                .filter(|line| {
                    line.starts_with("<<<<<<< ")
                        || line.starts_with("||||||| ")
                        || *line == "======="
                        || line.starts_with(">>>>>>> ")
                })
                .count();
            return markers.div_ceil(4);
        }
        self.chunks
            .iter()
            .filter(|chunk| chunk.choice.is_none())
            .count()
    }
    fn rebuild(&mut self) {
        self.result = self.chunks.iter().map(render_chunk).collect();
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) -> bool {
        let mut apply = false;
        ui.horizontal_wrapped(|ui| {
            if ui.button("Accept all left (theirs)").clicked() {
                for chunk in &mut self.chunks {
                    chunk.choice = Some(Choice::Left);
                }
                self.manual = false;
                self.rebuild();
            }
            if ui.button("Accept all right (yours)").clicked() {
                for chunk in &mut self.chunks {
                    chunk.choice = Some(Choice::Right);
                }
                self.manual = false;
                self.rebuild();
            }
            if ui
                .add_enabled(
                    self.options.enable_nonconflicting && !self.manual,
                    egui::Button::new("Apply all non-conflicting / Magic Resolve"),
                )
                .clicked()
            {
                self.apply_nonconflicting();
            }
            ui.label(format!("{} unresolved conflicts", self.unresolved()));
        });
        let conflicts: Vec<_> = self
            .chunks
            .iter()
            .enumerate()
            .filter(|(_, chunk)| chunk.choice.is_none() || chunk.conflict)
            .map(|(index, _)| index)
            .collect();
        ui.horizontal(|ui| {
            if ui.button("Previous conflict (Shift+F7)").clicked()
                || ui.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::F7))
            {
                self.selected = self.selected.saturating_sub(1);
            }
            if ui.button("Next conflict (F7)").clicked()
                || ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::F7))
            {
                self.selected = (self.selected + 1).min(conflicts.len().saturating_sub(1));
            }
            ui.checkbox(&mut self.options.synchronize, "Synchronize scrolling");
        });
        if let Some(index) = conflicts.get(self.selected).copied() {
            let chunk = self.chunks[index].clone();
            ui.label(format!(
                "Conflict {} / {}",
                self.selected + 1,
                conflicts.len()
            ));
            ui.columns(3, |columns| {
                for (column, (title, text)) in columns.iter_mut().zip([
                    ("Left · theirs", chunk.left.as_str()),
                    ("Base", chunk.base.as_str()),
                    ("Right · yours", chunk.right.as_str()),
                ]) {
                    column.label(title);
                    let mut area = egui::ScrollArea::both()
                        .id_salt(("merge-side", title))
                        .max_height(180.0);
                    if self.options.synchronize {
                        area = area.vertical_scroll_offset(self.scroll);
                    }
                    let response = area.show(column, |ui| {
                        ui.monospace(text);
                    });
                    if response.inner_rect.contains(
                        column
                            .input(|input| input.pointer.hover_pos())
                            .unwrap_or(egui::Pos2::ZERO),
                    ) && column.input(|input| input.is_scrolling())
                    {
                        self.scroll = response.state.offset.y;
                    }
                }
            });
            if !self
                .preview
                .as_ref()
                .is_some_and(|(selected, _)| *selected == index)
            {
                self.preview = Some((index, super::SideBySide::new(&chunk.left, &chunk.right)));
            }
            egui::CollapsingHeader::new(
                "Highlight conflict changes (word / character / whitespace)",
            )
            .show(ui, |ui| {
                if let Some((_, diff)) = &self.preview {
                    diff.ui(ui, theme, egui::FontId::monospace(12.0), "Theirs", "Yours");
                }
            });
            ui.horizontal(|ui| {
                for (label, choice) in [
                    ("Accept left", Choice::Left),
                    ("Accept right", Choice::Right),
                    ("Accept base", Choice::Base),
                    ("Accept both", Choice::Both),
                ] {
                    if ui.button(crate::i18n::tr_static(label)).clicked() {
                        self.resolve(index, choice);
                    }
                }
            });
        }
        ui.label("Result (editable)");
        if ui
            .add(
                egui::TextEdit::multiline(&mut self.result)
                    .code_editor()
                    .desired_rows(14)
                    .desired_width(f32::INFINITY),
            )
            .changed()
        {
            self.manual = true;
        }
        if self.unresolved() == 0 {
            ui.colored_label(theme.success, "All conflicts resolved");
        }
        if ui
            .add_enabled(
                self.unresolved() == 0,
                egui::Button::new("Apply resolved result"),
            )
            .clicked()
        {
            apply = true;
        }
        apply
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_edits_merge_and_overlapping_edits_need_choice() {
        let mut merged = MergeViewer::new(
            "a\nb\nc\n",
            "A\nb\nc\n",
            "a\nb\nC\n",
            DiffMergeSettings::default(),
        );
        assert_eq!(merged.result, "A\nb\nC\n");
        assert_eq!(merged.unresolved(), 0);
        merged = MergeViewer::new("a\n", "L\n", "R\n", DiffMergeSettings::default());
        assert_eq!(merged.unresolved(), 1);
        merged.resolve(0, Choice::Right);
        assert_eq!(merged.result, "R\n");
        assert_eq!(merged.unresolved(), 0);
    }
    #[test]
    fn insertion_at_same_position_conflicts_and_identical_edits_do_not() {
        assert_eq!(
            MergeViewer::new("", "L\n", "R\n", DiffMergeSettings::default()).unresolved(),
            1
        );
        assert_eq!(
            MergeViewer::new("x\n", "y\n", "y\n", DiffMergeSettings::default()).result,
            "y\n"
        );
    }
}

#[cfg(test)]
mod fixture_tests {
    use super::*;
    #[test]
    fn independent_fixture_edits_are_kept() {
        let base = include_str!("../fixtures/diff_merge/base.txt");
        let left = include_str!("../fixtures/diff_merge/theirs.txt");
        let right = include_str!("../fixtures/diff_merge/ours.txt");
        let merge = MergeViewer::new(base, left, right, DiffMergeSettings::default());
        assert_eq!(merge.unresolved(), 0);
        assert!(merge.result.contains("THEIRS"));
        assert!(merge.result.contains("OURS"));
        assert!(!merge.result.contains("left edit"));
    }
}

#[cfg(test)]
mod manual_tests {
    use super::*;
    #[test]
    fn partial_marker_deletion_does_not_mark_conflict_resolved() {
        let mut viewer = MergeViewer::new("a\n", "left\n", "right\n", DiffMergeSettings::default());
        viewer.set_result("left\n=======\nright\n>>>>>>> yours\n".into());
        assert!(viewer.unresolved() > 0);
        viewer.set_result("manually resolved\n".into());
        assert_eq!(viewer.unresolved(), 0);
    }
}

#[cfg(test)]
mod preservation_tests {
    use super::*;
    #[test]
    fn choices_preserve_manual_text_outside_intact_conflicts() {
        let mut merge = MergeViewer::new("a\n", "left\n", "right\n", DiffMergeSettings::default());
        merge.set_result(format!("manual prefix\n{}", merge.result));
        merge.resolve(0, Choice::Right);
        assert_eq!(merge.result, "manual prefix\nright\n");
    }
}
