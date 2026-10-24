//! VCS change markers in the editor gutter (roadmap §4.1 Diff & merge viewers):
//! clicking a marker opens a popup with the previous text and Rollback Lines,
//! Show Diff, Copy and Previous/Next; Next/Previous Change
//! (Ctrl+Alt+Shift+Down/Up) and Git › Show Diff.

use super::*;
use crate::diff_view::SideBySide;
use rustpy_vcs::{ChangeKind, LineChange};

impl IdeApp {
    /// Change markers of tab `i` against HEAD, recomputed when the text changed.
    fn tab_line_changes(&mut self, i: usize) -> Vec<LineChange> {
        let Some(tab) = self.tabs.get(i) else {
            return Vec::new();
        };
        let Some(path) = tab.state.path.clone() else {
            return Vec::new();
        };
        let version = tab.state.doc.version();
        if let Some((p, v, changes)) = &self.line_changes {
            if *p == path && *v == version {
                return changes.clone();
            }
        }
        let Some(git) = self.project.as_ref().and_then(|p| p.git.as_ref()) else {
            return Vec::new();
        };
        let changes = git.line_changes(&path, &tab.state.doc.text());
        self.line_changes = Some((path, version, changes.clone()));
        changes
    }

    /// A gutter change marker was clicked: show its popup at `anchor`.
    pub(super) fn open_change_popup(&mut self, i: usize, line: usize, anchor: Option<Pos2>) {
        if let Some(tab) = self.tabs.get_mut(i) {
            tab.view.change_popup = Some((line, anchor));
        }
    }

    /// Next/Previous Change: moves the caret to the next or previous changed
    /// block (the change popup follows when open).
    pub(super) fn goto_change(&mut self, forward: bool) {
        let Some(i) = self.active.filter(|&i| i < self.tabs.len()) else {
            return;
        };
        let changes = self.tab_line_changes(i);
        if changes.is_empty() {
            self.set_status("No changes against HEAD in this file");
            return;
        }
        let st = &self.tabs[i].state;
        let line = st.doc.line_of(st.primary().head());
        let line_count = st.doc.line_count();
        match rustpy_vcs::adjacent_change(&changes, line, forward, line_count) {
            Some(idx) => self.move_to_change(i, changes[idx].marker_line(line_count)),
            None => self.set_status(if forward {
                "No more changes below"
            } else {
                "No more changes above"
            }),
        }
    }

    fn move_to_change(&mut self, i: usize, line: usize) {
        let tab = &mut self.tabs[i];
        tab.view.cancel_session_restore();
        tab.state.go_to_line(line + 1, None);
        tab.state.scroll_to_caret = true;
        if tab.view.change_popup.is_some() {
            tab.view.change_popup = Some((line, None));
        }
        self.focus_editor = true;
    }

    /// Git › Show Diff: the active file against HEAD, side by side.
    pub(super) fn show_diff_with_head(&mut self) {
        let Some(i) = self.active.filter(|&i| i < self.tabs.len()) else {
            return;
        };
        let Some(path) = self.tabs[i].state.path.clone() else {
            self.set_status("Show Diff needs a saved file");
            return;
        };
        let Some(git) = self.project.as_ref().and_then(|p| p.git.as_ref()) else {
            self.set_status("The project is not under Git");
            return;
        };
        // A renamed file is compared with its HEAD copy under the old name.
        let head_path = git
            .status()
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.path == path)
            .and_then(|e| e.old_path)
            .unwrap_or_else(|| path.clone());
        let Some(head) = git.head_text(&head_path) else {
            self.set_status(format!(
                "{} is not in HEAD (new or unversioned file)",
                path.display()
            ));
            return;
        };
        let text = self.tabs[i].state.doc.text();
        let diff = SideBySide::new(&head.replace("\r\n", "\n"), &text)
            .with_edit_target(path.clone(), text.clone());
        let title = self.tabs[i].state.title();
        self.dialog = Dialog::Diff(Box::new(DiffDialog {
            title: format!("Show Diff: {title}"),
            left_title: "HEAD".into(),
            right_title: "Your version".into(),
            diff,
        }));
    }

    /// Rollback Lines: replaces the change's lines with the HEAD text as one undoable edit.
    fn rollback_change(&mut self, i: usize, change: &LineChange) {
        let tab = &mut self.tabs[i];
        let text = tab.state.doc.text();
        let (range, old) = rustpy_vcs::rollback_range(&text, change);
        let range = TextRange::new(
            TextSize::new(range.start as u32),
            TextSize::new(range.end as u32),
        );
        if tab
            .state
            .apply_text_edits(vec![(range, old)], "Rollback Lines")
        {
            tab.view.change_popup = None;
            let n = change.count.max(change.removed);
            self.set_status(format!("Rolled back {n} line(s)"));
        }
    }

    /// Draws the change popup of tab `i` (if open) next to its marker.
    pub(super) fn change_popup_ui(&mut self, ui: &mut egui::Ui, i: usize) {
        let Some((line, anchor)) = self.tabs.get(i).and_then(|t| t.view.change_popup) else {
            return;
        };
        let changes = self.tab_line_changes(i);
        let line_count = self.tabs[i].state.doc.line_count();
        let Some(idx) = rustpy_vcs::change_at_line(&changes, line, line_count) else {
            self.tabs[i].view.change_popup = None;
            return;
        };
        let change = changes[idx].clone();
        let Some(pos) =
            anchor.or_else(|| self.tabs[i].view.caret_screen_rect.map(|r| r.left_bottom()))
        else {
            return;
        };
        let ctx = ui.ctx().clone();
        let theme = self.theme.clone();
        let mono = egui::FontId::new(
            self.settings.editor.font_size.max(10.0),
            crate::theme::mono_family(false, false),
        );
        let current: Vec<String> = (change.start..change.start + change.count)
            .map(|l| self.tabs[i].state.doc.line_text(l))
            .collect();
        let has_prev = rustpy_vcs::adjacent_change(
            &changes,
            change.marker_line(line_count),
            false,
            line_count,
        );
        let has_next =
            rustpy_vcs::adjacent_change(&changes, change.marker_line(line_count), true, line_count);
        enum Act {
            Prev(usize),
            Next(usize),
            Rollback,
            Include(bool, bool),
            MoveToList(Option<usize>),
            ShowDiff,
            Copy,
            Close,
        }
        let mut act: Option<Act> = None;
        let (del_bg, ins_bg) = if theme.dark {
            (
                Color32::from_rgb(0x4B, 0x2B, 0x2B),
                Color32::from_rgb(0x29, 0x45, 0x2B),
            )
        } else {
            (
                Color32::from_rgb(0xFF, 0xD8, 0xD8),
                Color32::from_rgb(0xD8, 0xF5, 0xD8),
            )
        };
        let area = egui::Area::new(egui::Id::new(("vcs_change_popup", self.tabs[i].view.id)))
            .order(egui::Order::Foreground)
            .fixed_pos(pos + Vec2::new(4.0, 4.0))
            .constrain(true)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(640.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(has_prev.is_some(), egui::Button::new("⬆").small())
                            .on_hover_text("Previous Change (Ctrl+Alt+Shift+Up)")
                            .clicked()
                        {
                            act = has_prev.map(Act::Prev);
                        }
                        if ui
                            .add_enabled(has_next.is_some(), egui::Button::new("⬇").small())
                            .on_hover_text("Next Change (Ctrl+Alt+Shift+Down)")
                            .clicked()
                        {
                            act = has_next.map(Act::Next);
                        }
                        if ui
                            .small_button("Rollback")
                            .on_hover_text("Rollback Lines: restore the HEAD text")
                            .clicked()
                        {
                            act = Some(Act::Rollback);
                        }
                        if ui.small_button("Include hunk").clicked() {
                            act = Some(Act::Include(true, false));
                        }
                        if ui.small_button("Exclude hunk").clicked() {
                            act = Some(Act::Include(false, false));
                        }
                        if ui.small_button("Include line").clicked() {
                            act = Some(Act::Include(true, true));
                        }
                        if ui.small_button("Exclude line").clicked() {
                            act = Some(Act::Include(false, true));
                        }
                        if ui.small_button("Move to active changelist").clicked() {
                            act = Some(Act::MoveToList(None));
                        }
                        ui.menu_button("Move to changelist", |ui| {
                            for (index, list) in
                                self.git_panel.commit_workspace.lists.iter().enumerate()
                            {
                                if ui.button(&list.name).clicked() {
                                    act = Some(Act::MoveToList(Some(index)));
                                    ui.close();
                                }
                            }
                        });
                        if ui.small_button("Show Diff").clicked() {
                            act = Some(Act::ShowDiff);
                        }
                        if ui
                            .add_enabled(
                                !change.old_text.is_empty(),
                                egui::Button::new("Copy").small(),
                            )
                            .on_hover_text("Copy the HEAD text")
                            .clicked()
                        {
                            act = Some(Act::Copy);
                        }
                        let what = match change.kind {
                            ChangeKind::Added => format!("{} added line(s)", change.count),
                            ChangeKind::Deleted => format!("{} deleted line(s)", change.removed),
                            ChangeKind::Modified => {
                                format!("{} line(s) modified", change.count.max(change.removed))
                            }
                        };
                        ui.label(egui::RichText::new(what).color(theme.text_dim).small());
                        if ui.small_button("×").clicked() {
                            act = Some(Act::Close);
                        }
                    });
                    ui.separator();
                    // HEAD lines then current lines, each capped (Show Diff has the rest).
                    const MAX_ROWS: usize = 15;
                    let removed: Vec<&str> = if change.old_text.is_empty() {
                        Vec::new()
                    } else {
                        change
                            .old_text
                            .strip_suffix('\n')
                            .unwrap_or(&change.old_text)
                            .split('\n')
                            .collect()
                    };
                    for (rows, prefix, bg) in [
                        (
                            removed
                                .iter()
                                .map(|l| l.trim_end_matches('\r'))
                                .collect::<Vec<_>>(),
                            "-",
                            del_bg,
                        ),
                        (current.iter().map(String::as_str).collect(), "+", ins_bg),
                    ] {
                        for l in rows.iter().take(MAX_ROWS) {
                            ui.label(
                                egui::RichText::new(format!("{prefix} {l}"))
                                    .font(mono.clone())
                                    .color(theme.text)
                                    .background_color(bg),
                            );
                        }
                        if rows.len() > MAX_ROWS {
                            ui.label(
                                egui::RichText::new(format!(
                                    "  … {} more line(s)",
                                    rows.len() - MAX_ROWS
                                ))
                                .color(theme.text_dim)
                                .small(),
                            );
                        }
                    }
                });
            });
        let clicked_outside = ctx.input(|inp| inp.pointer.any_pressed())
            && ctx
                .input(|inp| inp.pointer.interact_pos())
                .is_some_and(|p| !area.response.rect.contains(p));
        if act.is_none() && (clicked_outside || ctx.input(|inp| inp.key_pressed(egui::Key::Escape)))
        {
            act = Some(Act::Close);
        }
        match act {
            Some(Act::Prev(k)) | Some(Act::Next(k)) => {
                self.move_to_change(i, changes[k].marker_line(line_count))
            }
            Some(Act::Rollback) => self.rollback_change(i, &change),
            Some(Act::Include(include, single)) => {
                self.commit_workspace_gutter(i, line, include, single)
            }
            Some(Act::MoveToList(target)) => {
                if let Some(target) =
                    target.filter(|index| *index < self.git_panel.commit_workspace.lists.len())
                {
                    self.git_panel.commit_workspace.active = target;
                }
                self.commit_workspace_move_gutter(i);
            }
            Some(Act::ShowDiff) => {
                self.tabs[i].view.change_popup = None;
                self.show_diff_with_head();
            }
            Some(Act::Copy) => {
                ctx.copy_text(change.old_text.replace("\r\n", "\n"));
                self.set_status("Copied the HEAD text of the change");
            }
            Some(Act::Close) => self.tabs[i].view.change_popup = None,
            None => {}
        }
    }
}
