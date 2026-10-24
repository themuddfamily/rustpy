//! Window-local history adapters. Uses the existing NavHistory and recent-file popup.
use super::*;
use crate::nav_history::{NavContext, NavPoint};
use rustpy_text::DocumentId;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct HistoryRuntime {
    seen: HashMap<DocumentId, (u64, usize)>,
    edit_cursor: usize,
}
impl IdeApp {
    pub(super) fn navigation_history_options(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Navigation history", |ui| {
            let settings = &mut self.settings.navigation_history;
            ui.checkbox(
                &mut settings.restore_tool_windows,
                "Restore tool windows with Back / Forward",
            );
            ui.add(
                egui::Slider::new(&mut settings.jump_lines, 1..=200)
                    .text("Lines moved to record a navigation jump"),
            );
            ui.add(
                egui::Slider::new(&mut settings.recent_limit, 10..=100)
                    .text("Recent files / locations shown"),
            );
        });
    }
    pub(super) fn capture_navigation_context(&self) -> NavContext {
        let mut layout = self.tool_window_layout();
        // Persisted layout deliberately describes what view modes hid; history
        // captures what is actually visible at this navigation place instead.
        layout.project_visible = self.layout.left_visible;
        layout.bottom_visible = self.bottom.is_some();
        if let Some(bottom) = self.bottom {
            layout.bottom_tab = bottom.title().into();
        }
        NavContext {
            layout: serde_json::to_string(&layout).unwrap_or_default(),
            identity: format!(
                "{:?}:{}:{:?}:{}:{}",
                self.left_tool,
                self.layout.left_visible,
                self.bottom,
                self.floating_bottom,
                self.bottom_maximized
            ),
        }
    }
    pub(super) fn navigation_point(&self) -> NavPoint {
        self.caret_point().unwrap_or_else(|| {
            let mut point = NavPoint::new(PathBuf::new(), 0, 0);
            point.context = Some(self.capture_navigation_context());
            point
        })
    }
    pub(super) fn restore_navigation_point(&mut self, point: NavPoint) {
        if !point.path.as_os_str().is_empty() {
            self.open_at(&point.path, point.line, point.col);
        } else {
            self.active = None;
            self.group_active = [None, None];
        }
        if self.settings.navigation_history.restore_tool_windows {
            if let Some(context) = point.context {
                if context.layout.len() <= 64 * 1024 {
                    if let Ok(layout) =
                        serde_json::from_str::<rustpy_settings::ToolWindowLayout>(&context.layout)
                    {
                        self.restore_navigation_tool_window_layout(&layout);
                    }
                }
            }
        }
        self.focus_editor = true;
    }
    /// Track edits by document identity, including inactive editors. Merely switching
    /// between files with different version counters must never manufacture an edit.
    pub(super) fn observe_navigation_edits(&mut self) {
        self.nav.history.jump_lines = self.settings.navigation_history.jump_lines.clamp(1, 200);
        let live: HashSet<_> = self
            .tabs
            .iter()
            .map(|tab| tab.state.doc.identity())
            .collect();
        self.nav.details.seen.retain(|id, _| live.contains(id));
        let mut order: Vec<_> = (0..self.tabs.len()).collect();
        order.sort_by_key(|index| Some(*index) == self.active);
        for index in order {
            let tab = &self.tabs[index];
            let Some(path) = tab.state.path.clone() else {
                continue;
            };
            let id = tab.state.doc.identity();
            let version = tab.state.doc.version();
            let lines = tab.state.doc.line_count();
            let previous = self.nav.details.seen.insert(id, (version, lines));
            let changed = match previous {
                Some((old, _)) => old != version,
                None => tab.has_unsaved_changes(),
            };
            if !changed || tab.state.doc.is_read_only() {
                continue;
            }
            if let Some((old_version, old_lines)) = previous {
                let delta = lines as i64 - old_lines as i64;
                if delta != 0 {
                    let first = tab
                        .state
                        .doc
                        .changes_since(old_version)
                        .and_then(|changes| {
                            changes
                                .iter()
                                .flat_map(|edits| edits.iter())
                                .map(|edit| edit.range.start())
                                .min()
                        })
                        .map(|offset| tab.state.doc.line_of(offset.min(tab.state.doc.len())))
                        .unwrap_or(0);
                    self.nav
                        .history
                        .shift_lines(&path, first.saturating_sub(1), delta);
                }
            }
            let position = tab.state.primary_position();
            let mut point = NavPoint::new(
                path.clone(),
                position.line as usize,
                position.column as usize,
            );
            if Some(index) == self.active {
                point.context = Some(self.capture_navigation_context());
            }
            self.nav.history.note_edit(point);
            self.nav.details.edit_cursor = 0;
            if tab.has_unsaved_changes() {
                self.tabs[index].preview = false;
            }
        }
    }
    pub(super) fn go_to_last_edit_location(&mut self) {
        self.observe_navigation_edits();
        let current = self.caret_point();
        let places = self.nav.history.changed_places();
        while let Some(point) = places.get(self.nav.details.edit_cursor).cloned() {
            self.nav.details.edit_cursor += 1;
            if current.as_ref().is_some_and(|at| at.same_line(&point)) {
                continue;
            }
            if point.path.exists()
                || self
                    .tabs
                    .iter()
                    .any(|tab| tab.state.path.as_ref() == Some(&point.path))
            {
                self.mark_nav_jump();
                self.restore_navigation_point(point);
                return;
            }
        }
        self.set_status("No earlier edit location");
    }
    /// Edited is session edit order; changed additionally includes current working
    /// tree changes and unsaved editors, even if not visited in this session.
    pub(super) fn open_history_file_list(&mut self, edited_only: bool) {
        self.observe_navigation_edits();
        let mut paths: Vec<_> = self
            .nav
            .history
            .changed_places()
            .into_iter()
            .map(|point| point.path)
            .collect();
        let edited: HashSet<_> = paths.iter().cloned().collect();
        if !edited_only {
            let mut changed: Vec<_> = self
                .git_panel
                .entries
                .iter()
                .filter(|entry| {
                    !matches!(
                        entry.status,
                        rustpy_vcs::FileStatus::Ignored | rustpy_vcs::FileStatus::Unmodified
                    )
                })
                .take(512)
                .map(|entry| entry.path.clone())
                .collect();
            changed.extend(
                self.tabs
                    .iter()
                    .filter(|tab| tab.has_unsaved_changes())
                    .filter_map(|tab| tab.state.path.clone()),
            );
            changed.sort_by_key(|path| {
                std::cmp::Reverse(
                    std::fs::metadata(path)
                        .and_then(|metadata| metadata.modified())
                        .ok(),
                )
            });
            paths.extend(changed);
        }
        let mut seen = HashSet::new();
        paths.retain(|path| seen.insert(path.clone()));
        let current = self.caret_point().map(|point| point.path);
        let files = paths
            .into_iter()
            .filter(|path| {
                path.exists()
                    || self
                        .tabs
                        .iter()
                        .any(|tab| tab.state.path.as_ref() == Some(path))
            })
            .take(self.settings.navigation_history.recent_limit.clamp(10, 100))
            .map(|path| self.file_entry(&path, current.as_deref(), &edited))
            .collect();
        self.nav.recent_files.open(files, Vec::new(), edited_only);
        if !edited_only {
            self.nav.recent_files.set_title("Recently Changed Files");
        }
        self.focus_editor = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn opened() -> (IdeApp, tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a.py");
        let b = root.path().join("b.py");
        std::fs::write(&a, "one\ntwo\nthree\n").unwrap();
        std::fs::write(&b, "other\n").unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        app.open_file(&a);
        app.observe_navigation();
        (app, root, a, b)
    }
    #[test]
    fn actual_back_forward_restore_tool_windows_at_same_file_and_caret() {
        let (mut app, _root, _a, _b) = opened();
        app.left_tool = LeftTool::Project;
        app.layout.left_visible = true;
        app.bottom = None;
        app.floating_bottom = false;
        app.observe_navigation();
        let version = app.tabs[app.active.unwrap()].state.doc.version();
        app.left_tool = LeftTool::Structure;
        app.bottom = Some(BottomTool::Problems);
        app.floating_bottom = true;
        app.observe_navigation();
        app.handle_action(Action::NavigateBack, &egui::Context::default());
        assert_eq!(app.left_tool, LeftTool::Project);
        assert_eq!(app.bottom, None);
        assert!(!app.floating_bottom);
        app.handle_action(Action::NavigateForward, &egui::Context::default());
        assert_eq!(app.left_tool, LeftTool::Structure);
        assert_eq!(app.bottom, Some(BottomTool::Problems));
        assert!(app.floating_bottom);
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.version(), version);
        assert!(app.nav.history.changed_places().is_empty());
    }
    #[test]
    fn real_inactive_edits_populate_named_lists_without_inventing_edits_on_tab_switch() {
        let (mut app, _root, a, b) = opened();
        let a_index = app.active.unwrap();
        app.open_file(&b);
        app.observe_navigation();
        assert!(app.nav.history.changed_places().is_empty());
        assert!(app.tabs[a_index].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "edited\n".into())],
            "Inactive edit"
        ));
        app.observe_navigation();
        app.handle_action(Action::RecentlyEditedFiles, &egui::Context::default());
        assert_eq!(app.nav.recent_files.title(), "Recently Edited Files");
        assert_eq!(app.nav.recent_files.shown_paths(), vec![a.clone()]);
        // Cached real VCS status adds a working-tree file without pretending it was edited in this editor.
        app.git_panel.entries = vec![rustpy_vcs::StatusEntry {
            path: b.clone(),
            status: rustpy_vcs::FileStatus::Modified,
            staged: false,
            old_path: None,
        }];
        app.handle_action(Action::RecentlyChangedFiles, &egui::Context::default());
        assert_eq!(app.nav.recent_files.title(), "Recently Changed Files");
        assert_eq!(app.nav.recent_files.shown_paths().len(), 2);
        app.nav.recent_files.toggle_edited();
        assert_eq!(app.nav.recent_files.shown_paths(), vec![a.clone()]);
        app.handle_action(Action::RecentlyEditedFiles, &egui::Context::default());
        assert_eq!(app.nav.recent_files.shown_paths(), vec![a]);
        assert!(!app.nav.recent_files.shown_paths().contains(&b));
    }
    #[test]
    fn last_edit_action_uses_per_document_places_and_cycles_without_reediting() {
        let (mut app, _root, a, b) = opened();
        let a_index = app.active.unwrap();
        app.tabs[a_index].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "α\n".into())],
            "Edit a",
        );
        let a_position = app.tabs[a_index].state.primary_position();
        app.observe_navigation();
        app.open_file(&b);
        app.observe_navigation();
        let b_index = app.active.unwrap();
        app.tabs[b_index].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "β\n".into())],
            "Edit b",
        );
        let b_position = app.tabs[b_index].state.primary_position();
        app.observe_navigation();
        app.tabs[b_index]
            .state
            .go_to_line(if b_position.line == 0 { 2 } else { 1 }, None);
        let context = egui::Context::default();
        app.handle_action(Action::LastEditLocation, &context);
        assert_eq!(app.tabs[app.active.unwrap()].state.path.as_ref(), Some(&b));
        assert_eq!(
            app.tabs[app.active.unwrap()].state.primary_position(),
            b_position
        );
        app.handle_action(Action::LastEditLocation, &context);
        assert_eq!(app.tabs[app.active.unwrap()].state.path.as_ref(), Some(&a));
        assert_eq!(
            app.tabs[app.active.unwrap()].state.primary_position(),
            a_position
        );
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "one\ntwo\nthree\n");
    }
    #[test]
    fn preferences_control_restoration_and_are_owned_by_editor_general() {
        let (mut app, _root, _a, _b) = opened();
        let before = app.settings.clone();
        app.settings.navigation_history.restore_tool_windows = false;
        assert!(settings_ui::page_modified(
            settings_ui::SettingsPage::EditorGeneral,
            &before,
            &app.settings
        ));
        assert!(!settings_ui::page_modified(
            settings_ui::SettingsPage::EditorFont,
            &before,
            &app.settings
        ));
        app.bottom = None;
        app.observe_navigation();
        app.bottom = Some(BottomTool::Git);
        app.observe_navigation();
        app.nav_history_step(true);
        assert_eq!(app.bottom, Some(BottomTool::Git));
        let encoded = serde_json::to_string(&app.settings.navigation_history).unwrap();
        let restored: rustpy_settings::navigation_history::NavigationHistorySettings =
            serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored, app.settings.navigation_history);
    }
}
