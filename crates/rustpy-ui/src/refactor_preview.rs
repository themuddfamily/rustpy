//! Refactoring Preview (roadmap §2.8 "Refactoring infrastructure: usage
//! search, conflict detection with preview dialog ('Refactoring Preview'
//! tree with usages to be changed, non-code usages, conflicts), 'Do
//! Refactor'"): the places a refactoring would change, grouped (code
//! usages, dynamic usages, comments and strings, conflicts) and by file,
//! each with a checkbox. Rename fills it; Change Signature and Move can
//! reuse it by building [`PreviewGroup`]s from their own changes.

use std::path::{Path, PathBuf};

use egui::text::LayoutJob;
use egui::{Color32, Sense, TextFormat};
use rustpy_core::TextRange;

use crate::theme::UiTheme;

/// One change (or, in a conflicts group, one problem) at a place in a file.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewItem {
    pub path: PathBuf,
    pub range: TextRange,
    /// 0-based line of `range`.
    pub line: usize,
    /// Byte offset of `range` in `line_text`.
    pub column: usize,
    pub line_text: String,
    pub checked: bool,
    /// A short tag (`def`, `write`) or, for a conflict, the problem.
    pub note: String,
}

/// What a group of items is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GroupKind {
    /// Changes the refactoring makes; included by default.
    Code,
    /// Usages matched by name only (receiver of unknown type): they may
    /// belong to another symbol, so they start excluded.
    Dynamic,
    /// Occurrences in comments and string literals.
    NonCode,
    /// Problems the refactoring would cause; listed, never applied.
    Conflicts,
}

#[derive(Clone, Debug)]
pub struct PreviewGroup {
    pub kind: GroupKind,
    pub title: String,
    /// Explains the group (tooltip of its title).
    pub hint: String,
    pub items: Vec<PreviewItem>,
}

impl PreviewGroup {
    /// Whether the items can be included or excluded.
    pub fn checkable(&self) -> bool {
        self.kind != GroupKind::Conflicts
    }
}

/// The preview tree: groups → files → lines.
#[derive(Clone, Debug, Default)]
pub struct RefactorPreview {
    pub groups: Vec<PreviewGroup>,
}

/// `Some(true)` when every item is checked, `Some(false)` when none is, `None` when mixed.
fn check_state<'a>(items: impl Iterator<Item = &'a PreviewItem>) -> Option<bool> {
    let (mut on, mut off) = (false, false);
    for i in items {
        if i.checked {
            on = true;
        } else {
            off = true;
        }
    }
    match (on, off) {
        (true, true) => None,
        (true, false) => Some(true),
        _ => Some(false),
    }
}

impl RefactorPreview {
    /// Adds a group (empty groups are left out).
    pub fn add_group(
        &mut self,
        kind: GroupKind,
        title: impl Into<String>,
        hint: impl Into<String>,
        items: Vec<PreviewItem>,
    ) {
        if !items.is_empty() {
            self.groups.push(PreviewGroup {
                kind,
                title: title.into(),
                hint: hint.into(),
                items,
            });
        }
    }

    /// The included changes, in group order.
    pub fn checked(&self) -> impl Iterator<Item = &PreviewItem> {
        self.groups
            .iter()
            .filter(|g| g.checkable())
            .flat_map(|g| g.items.iter())
            .filter(|i| i.checked)
    }

    pub fn checked_count(&self) -> usize {
        self.checked().count()
    }

    /// Number of files the included changes touch.
    pub fn checked_files(&self) -> usize {
        let mut paths: Vec<&Path> = self.checked().map(|i| i.path.as_path()).collect();
        paths.sort();
        paths.dedup();
        paths.len()
    }

    /// Number of listed problems.
    pub fn conflict_count(&self) -> usize {
        self.groups
            .iter()
            .filter(|g| g.kind == GroupKind::Conflicts)
            .map(|g| g.items.len())
            .sum()
    }

    /// Includes or excludes every item of group `group` (of `path` only, when given).
    pub fn set_checked(&mut self, group: usize, path: Option<&Path>, on: bool) {
        if let Some(g) = self.groups.get_mut(group).filter(|g| g.checkable()) {
            for i in g
                .items
                .iter_mut()
                .filter(|i| path.is_none_or(|p| i.path == p))
            {
                i.checked = on;
            }
        }
    }

    /// Draws the tree; returns the place the user clicked, to show in the editor.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        root: &Path,
        id: egui::Id,
    ) -> Option<(PathBuf, TextRange)> {
        let mut clicked: Option<(PathBuf, TextRange)> = None;
        let mut toggles: Vec<(usize, Option<PathBuf>, bool)> = Vec::new();
        for (gi, group) in self.groups.iter_mut().enumerate() {
            let checkable = group.checkable();
            let title_color = match group.kind {
                GroupKind::Conflicts => theme.error,
                GroupKind::Dynamic => theme.warning,
                _ => theme.text,
            };
            let state = check_state(group.items.iter());
            let header = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id.with(("group", gi)),
                true,
            );
            header
                .show_header(ui, |ui| {
                    if checkable {
                        let mut on = state == Some(true);
                        if ui
                            .add(
                                egui::Checkbox::without_text(&mut on)
                                    .indeterminate(state.is_none()),
                            )
                            .clicked()
                        {
                            toggles.push((gi, None, on));
                        }
                    }
                    ui.label(
                        egui::RichText::new(format!("{} ({})", group.title, group.items.len()))
                            .strong()
                            .color(title_color),
                    )
                    .on_hover_text(&group.hint);
                })
                .body(|ui| {
                    let mut files: Vec<PathBuf> = Vec::new();
                    for item in &group.items {
                        if !files.contains(&item.path) {
                            files.push(item.path.clone());
                        }
                    }
                    for path in files {
                        let rel = path
                            .strip_prefix(root)
                            .unwrap_or(&path)
                            .display()
                            .to_string();
                        let count = group.items.iter().filter(|i| i.path == path).count();
                        let state = check_state(group.items.iter().filter(|i| i.path == path));
                        let file_header =
                            egui::collapsing_header::CollapsingState::load_with_default_open(
                                ui.ctx(),
                                id.with(("file", gi, &path)),
                                true,
                            );
                        file_header
                            .show_header(ui, |ui| {
                                if checkable {
                                    let mut on = state == Some(true);
                                    if ui
                                        .add(
                                            egui::Checkbox::without_text(&mut on)
                                                .indeterminate(state.is_none()),
                                        )
                                        .clicked()
                                    {
                                        toggles.push((gi, Some(path.clone()), on));
                                    }
                                }
                                ui.label(egui::RichText::new(&rel).color(theme.text));
                                ui.label(
                                    egui::RichText::new(format!("{count}"))
                                        .color(theme.text_dim)
                                        .small(),
                                );
                            })
                            .body(|ui| {
                                for item in group.items.iter_mut().filter(|i| i.path == path) {
                                    ui.horizontal(|ui| {
                                        if checkable {
                                            ui.add(egui::Checkbox::without_text(&mut item.checked));
                                        }
                                        ui.label(
                                            egui::RichText::new(format!("{:>4}", item.line + 1))
                                                .monospace()
                                                .color(theme.text_dim),
                                        );
                                        if checkable && !item.note.is_empty() {
                                            ui.label(
                                                egui::RichText::new(&item.note)
                                                    .small()
                                                    .color(theme.text_dim),
                                            );
                                        }
                                        let dim = checkable && !item.checked;
                                        let job = line_job(
                                            ui,
                                            theme,
                                            &item.line_text,
                                            item.column,
                                            usize::from(item.range.len()),
                                            dim,
                                        );
                                        let r = ui
                                            .add(egui::Label::new(job).sense(Sense::click()))
                                            .on_hover_text("Show in the editor");
                                        if r.clicked() {
                                            clicked = Some((item.path.clone(), item.range));
                                        }
                                    });
                                    if !checkable && !item.note.is_empty() {
                                        ui.horizontal(|ui| {
                                            ui.add_space(40.0);
                                            ui.label(
                                                egui::RichText::new(&item.note).color(theme.error),
                                            );
                                        });
                                    }
                                }
                            });
                    }
                });
        }
        for (g, path, on) in toggles {
            self.set_checked(g, path.as_deref(), on);
        }
        clicked
    }
}

/// Longest line shown in full; longer ones are cut around the occurrence.
const MAX_LINE: usize = 140;

/// `text` with the `len` bytes at `column` highlighted, leading whitespace
/// dropped and long lines shortened around the occurrence.
pub fn line_job(
    ui: &egui::Ui,
    theme: &UiTheme,
    text: &str,
    column: usize,
    len: usize,
    dim: bool,
) -> LayoutJob {
    let (before, matched, after) = split_line(text, column, len);
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let color = if dim { theme.text_disabled } else { theme.text };
    let bg = if dim {
        Color32::TRANSPARENT
    } else {
        theme.selection_bg.gamma_multiply(0.45)
    };
    let mut job = LayoutJob::default();
    job.append(
        &before,
        0.0,
        TextFormat {
            font_id: font.clone(),
            color,
            ..Default::default()
        },
    );
    job.append(
        &matched,
        0.0,
        TextFormat {
            font_id: font.clone(),
            color: if dim { color } else { Color32::WHITE },
            background: bg,
            ..Default::default()
        },
    );
    job.append(
        &after,
        0.0,
        TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job
}

/// The parts before, at and after the occurrence, as shown in the tree.
pub fn split_line(text: &str, column: usize, len: usize) -> (String, String, String) {
    let floor = |mut i: usize| {
        i = i.min(text.len());
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let start = floor(column);
    let end = floor(column.saturating_add(len)).max(start);
    let lead = text.len() - text.trim_start().len();
    let mut from = lead.min(start);
    let mut to = text.trim_end().len().max(end);
    if to - from > MAX_LINE {
        from = floor(start.saturating_sub(MAX_LINE / 3)).max(from);
        to = floor((end + MAX_LINE * 2 / 3).min(to)).max(end);
    }
    let before = format!(
        "{}{}",
        if from > lead { "…" } else { "" },
        &text[from..start]
    );
    let after = format!(
        "{}{}",
        &text[end..to],
        if to < text.trim_end().len() {
            "…"
        } else {
            ""
        }
    );
    (before, text[start..end].to_string(), after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_core::TextSize;

    fn item(path: &str, line: usize, checked: bool) -> PreviewItem {
        PreviewItem {
            path: PathBuf::from(path),
            range: TextRange::new(TextSize::new(0), TextSize::new(3)),
            line,
            column: 0,
            line_text: "run()".into(),
            checked,
            note: String::new(),
        }
    }

    #[test]
    fn groups_files_and_checks() {
        let mut p = RefactorPreview::default();
        p.add_group(
            GroupKind::Code,
            "Code",
            "",
            vec![item("/p/a.py", 0, true), item("/p/b.py", 3, true)],
        );
        p.add_group(
            GroupKind::Dynamic,
            "Dynamic",
            "",
            vec![item("/p/a.py", 5, false)],
        );
        p.add_group(GroupKind::NonCode, "Comments", "", Vec::new());
        p.add_group(
            GroupKind::Conflicts,
            "Conflicts",
            "",
            vec![PreviewItem {
                note: "clash".into(),
                ..item("/p/c.py", 1, true)
            }],
        );
        assert_eq!(p.groups.len(), 3, "empty groups are left out");
        assert_eq!(
            (p.checked_count(), p.checked_files(), p.conflict_count()),
            (2, 2, 1),
            "conflicts are never applied"
        );
        p.set_checked(1, None, true);
        assert_eq!(p.checked_count(), 3);
        p.set_checked(0, Some(Path::new("/p/a.py")), false);
        assert_eq!(p.checked().map(|i| i.line).collect::<Vec<_>>(), vec![3, 5]);
        assert_eq!(check_state(p.groups[0].items.iter()), None);
        p.set_checked(2, None, false);
        assert_eq!(p.checked_count(), 2, "a conflicts group cannot be toggled");
    }

    #[test]
    fn lines_are_cut_around_the_occurrence() {
        assert_eq!(
            split_line("    job.run()  ", 8, 3),
            ("job.".to_string(), "run".to_string(), "()".to_string())
        );
        let long = format!("x = {}target{}", "a".repeat(200), "b".repeat(200));
        let col = long.find("target").unwrap();
        let (before, matched, after) = split_line(&long, col, 6);
        assert_eq!(matched, "target");
        assert!(
            before.starts_with('…') && after.ends_with('…'),
            "{before} / {after}"
        );
        assert!(before.len() + after.len() < MAX_LINE + 10);
        // Stale columns (out of range, inside a character) do not panic.
        assert_eq!(split_line("é", 1, 5).1, "é");
        assert_eq!(
            split_line("abc", 10, 2),
            ("abc".to_string(), String::new(), String::new())
        );
    }
}
