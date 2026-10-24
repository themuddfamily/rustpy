//! Editor-group operations retain a single text/history authority per document.
use super::*;

impl IdeApp {
    pub(super) fn sync_shared_editors(&mut self) {
        let changes = self
            .tabs
            .iter()
            .filter_map(|tab| {
                tab.state
                    .shared_file_change()
                    .map(|metadata| (tab.state.doc.share(), metadata))
            })
            .collect::<Vec<_>>();
        let file_changed = !changes.is_empty();
        for (document, (old_path, path, language)) in changes {
            if old_path != path {
                if let (Some(old), Some(new)) = (&old_path, &path) {
                    if let Some(revision) = self.disk_revisions.remove(old) {
                        self.disk_revisions.entry(new.clone()).or_insert(revision);
                    }
                    if let Some(encoding) = self.encoding_overrides.remove(old) {
                        self.encoding_overrides
                            .entry(new.clone())
                            .or_insert(encoding);
                        self.encoding_generation = self.encoding_generation.wrapping_add(1);
                    }
                    if let Some(revision) = self.observed_revisions.remove(old) {
                        self.observed_revisions
                            .entry(new.clone())
                            .or_insert(revision);
                    }
                }
            }
            for tab in &mut self.tabs {
                if tab.state.doc.shares_authority(&document) {
                    tab.state.path = path.clone();
                    tab.state.set_language(language);
                }
            }
        }
        for tab in &mut self.tabs {
            tab.state.sync_shared_view();
        }
        if file_changed {
            self.sync_external_file_watches();
            self.prune_file_permissions();
        }
    }

    /// Legacy file sessions contain one record per file; layout stores each instance separately.
    pub(super) fn editor_session_files(&self) -> Vec<rustpy_settings::OpenFileState> {
        let mut seen = std::collections::HashSet::new();
        self.tabs
            .iter()
            .filter_map(|tab| {
                let path = tab.state.path.as_ref()?;
                if !seen.insert(path.clone()) {
                    return None;
                }
                let instance = self
                    .active
                    .and_then(|index| self.tabs.get(index))
                    .filter(|active| active.state.doc.shares_authority(&tab.state.doc))
                    .unwrap_or(tab);
                Some(
                    instance
                        .view
                        .session_position(&instance.state)
                        .into_open_file_state(path.clone(), instance.pinned),
                )
            })
            .collect()
    }

    pub(super) fn has_other_document_view(&self, index: usize) -> bool {
        self.tabs.get(index).is_some_and(|tab| {
            self.tabs.iter().enumerate().any(|(other, candidate)| {
                other != index && tab.state.doc.shares_authority(&candidate.state.doc)
            })
        })
    }

    /// Clone the editor instance, never its text, save baseline or history.
    /// A destination already showing this document is activated rather than duplicated.
    pub(super) fn open_document_in_group(&mut self, source: usize, group: u8) -> Option<usize> {
        if group > 1 || source >= self.tabs.len() {
            return None;
        }
        if let Some(index) = self.tabs.iter().position(|tab| {
            tab.group == group && tab.state.doc.shares_authority(&self.tabs[source].state.doc)
        }) {
            self.active = Some(index);
            self.group_active[group as usize] = Some(self.tabs[index].view.id);
            self.focus_editor = true;
            return Some(index);
        }
        self.tabs[source].preview = false;
        let state = self.tabs[source].state.split_view();
        let id = egui::Id::new(("editor", self.next_tab_id));
        self.next_tab_id += 1;
        let image = self.tabs[source].image.clone();
        let svg = self.tabs[source]
            .svg
            .as_ref()
            .map(crate::editor::svg_preview::SvgPane::split_view);
        let notebook = self.tabs[source]
            .notebook
            .as_ref()
            .map(|notebook| Box::new(notebook.split_view()));
        self.tabs.push(Tab {
            state,
            view: EditorView::new(id),
            pinned: self.tabs[source].pinned,
            group,
            preview: false,
            image,
            svg,
            notebook,
        });
        let index = self.tabs.len() - 1;
        self.active = Some(index);
        self.group_active[group as usize] = Some(id);
        self.focus_editor = true;
        Some(index)
    }

    pub(super) fn split_shared_editor(&mut self, dir: SplitDir) {
        let Some(source) = self.active else { return };
        let group = self.tabs[source].group;
        if self.open_document_in_group(source, group ^ 1).is_some() {
            self.split = Some(dir);
        }
    }

    /// Transfer this instance; leave its independent carets and view intact.
    pub(super) fn split_and_move_editor(&mut self, dir: SplitDir) {
        let Some(index) = self.active else { return };
        let group = self.tabs[index].group;
        if !self
            .tabs
            .iter()
            .enumerate()
            .any(|(other, tab)| other != index && tab.group == group)
        {
            self.set_status("The only editor in a group cannot be moved out of that group");
            return;
        }
        self.move_editor_instance(index, group ^ 1);
        self.split = Some(dir);
    }

    fn move_editor_instance(&mut self, index: usize, group: u8) {
        let moving = self.tabs[index].view.id;
        if let Some(destination) = self
            .tabs
            .iter()
            .enumerate()
            .find(|(other, tab)| {
                *other != index
                    && tab.group == group
                    && tab.state.doc.shares_authority(&self.tabs[index].state.doc)
            })
            .map(|(other, _)| other)
        {
            self.close_tab(destination);
        }
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.view.id == moving)
            .expect("moving instance retained");
        self.tabs[index].group = group;
        self.active = Some(index);
        self.group_active[group as usize] = Some(moving);
        self.focus_editor = true;
    }

    pub(super) fn move_to_opposite_group(&mut self) {
        let Some(index) = self.active else { return };
        if self.split.is_none() {
            self.split_and_move_editor(SplitDir::Horizontal);
        } else {
            self.move_editor_instance(index, self.tabs[index].group ^ 1);
            self.sync_groups();
        }
    }

    pub(super) fn open_in_opposite_group(&mut self) {
        let Some(source) = self.active else { return };
        if self
            .open_document_in_group(source, self.tabs[source].group ^ 1)
            .is_some()
        {
            self.split.get_or_insert(SplitDir::Horizontal);
        }
    }

    /// Open a project file on the right without relocating another editor instance.
    pub fn open_in_right_split(&mut self, path: &Path) {
        let before = self.active;
        let previous_count = self.tabs.len();
        let normalized = rustpy_core::vfs::normalize_path(path);
        // A split-open must retain the source group even with a one-tab limit.
        // Ordinary open_file still owns decoding, validation and viewer selection.
        let limit = self.settings.editor.tab_limit;
        self.settings.editor.tab_limit = limit.max(self.tabs.len().saturating_add(1));
        self.open_file(&normalized);
        self.settings.editor.tab_limit = limit;
        let opened = self
            .active
            .filter(|index| self.tabs[*index].state.path.as_deref() == Some(normalized.as_path()));
        let Some(source) = opened else {
            self.active = before;
            return;
        };
        // A newly opened file need not acquire an unnecessary left-hand instance.
        let original = before.filter(|index| *index != source && self.tabs[*index].group == 0);
        if self.tabs.len() > previous_count
            && original.is_some()
            && !self.tabs.iter().enumerate().any(|(index, tab)| {
                index != source && tab.state.doc.shares_authority(&self.tabs[source].state.doc)
            })
        {
            self.tabs[source].group = 1;
            self.group_active[1] = Some(self.tabs[source].view.id);
            self.split = Some(SplitDir::Horizontal);
        } else if self.open_document_in_group(source, 1).is_some() {
            self.split = Some(SplitDir::Horizontal);
        }
    }

    /// Collapse duplicate instances while retaining the focused instance's caret/cache.
    pub(super) fn unsplit_editors(&mut self) {
        let active_id = self
            .active
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.view.id);
        let mut index = self.tabs.len();
        while index > 0 {
            index -= 1;
            let retain = self
                .tabs
                .iter()
                .enumerate()
                .find(|(other, tab)| {
                    *other != index
                        && tab.state.doc.shares_authority(&self.tabs[index].state.doc)
                        && (Some(tab.view.id) == active_id
                            || (Some(self.tabs[index].view.id) != active_id && *other < index))
                })
                .map(|(other, _)| other);
            if retain.is_some() {
                // This is an instance removal, not a document close or Reopen Closed entry.
                self.tabs.remove(index);
                self.active = self.active.and_then(|active| {
                    if active == index {
                        None
                    } else {
                        Some(active - usize::from(active > index))
                    }
                });
                self.detached = self.detached.and_then(|detached| {
                    if detached == index {
                        None
                    } else {
                        Some(detached - usize::from(detached > index))
                    }
                });
            }
        }
        if let Some(id) = active_id {
            self.active = self.tabs.iter().position(|tab| tab.view.id == id);
        }
        self.split = None;
        for tab in &mut self.tabs {
            tab.group = 0;
        }
        self.group_active = [
            self.active
                .and_then(|index| self.tabs.get(index))
                .map(|tab| tab.view.id),
            None,
        ];
        self.focus_editor = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditOptions, Language};

    #[test]
    fn sole_document_split_shares_edits_history_and_retains_active_instance_on_unsplit() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("value = 'π🙂'\n", Language::Python));
        app.split_shared_editor(SplitDir::Horizontal);
        assert_eq!(app.tabs.len(), 2);
        assert!(app.tabs[0]
            .state
            .doc
            .shares_authority(&app.tabs[1].state.doc));
        assert_ne!(app.tabs[0].view.id, app.tabs[1].view.id);
        assert_eq!(app.tabs[0].group, 0);
        assert_eq!(app.tabs[1].group, 1);
        app.tabs[1]
            .state
            .insert_text("# peer\n", &EditOptions::default());
        assert_eq!(app.tabs[0].state.doc.text(), "# peer\nvalue = 'π🙂'\n");
        app.tabs[0].state.undo();
        assert_eq!(app.tabs[1].state.doc.text(), "value = 'π🙂'\n");
        let retained = app.tabs[1].view.id;
        app.unsplit_editors();
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].view.id, retained);
        assert!(app.tabs[0].state.doc.can_redo());
    }

    #[test]
    fn opposite_reuses_instance_and_move_transfers_instead_of_copying() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("a = 1\n", Language::Python));
        app.open_in_opposite_group();
        let second = app.tabs[1].view.id;
        app.active = Some(0);
        app.open_in_opposite_group();
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[app.active.unwrap()].view.id, second);
        app.push_tab(EditorState::from_text("b = 2\n", Language::Python));
        let moving = app.tabs[2].view.id;
        app.split_and_move_editor(SplitDir::Vertical);
        assert_eq!(app.tabs.len(), 3);
        assert_eq!(app.tabs[2].view.id, moving);
        assert_eq!(app.tabs[2].group, 0);
    }
    #[test]
    fn actual_split_actions_share_saved_baseline_and_restore_independent_session_positions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("shared.py");
        std::fs::write(&path, "α = 1\nsecond = 2\nthird = 3\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings.clone(), Some(root.path().to_path_buf()));
        app.open_file(&path);
        let ctx = egui::Context::default();
        app.handle_action(Action::SplitVertically, &ctx);
        assert_eq!(app.tabs.len(), 2);
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(0))]);
        app.tabs[1].state.go_to_line(2, None);
        app.tabs[1]
            .state
            .insert_text("# π🙂\n", &EditOptions::default());
        assert_eq!(app.tabs[0].state.doc.text(), app.tabs[1].state.doc.text());
        assert!(app.tabs[0].state.doc.is_modified());
        app.active = Some(0); // Saving the inactive instance must serialize the shared edited authority.
        assert!(app.save_tab(0));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            app.tabs[1].state.doc.to_bytes().unwrap()
        );
        assert!(!app.tabs[1].state.doc.is_modified());
        app.tabs[0].state.go_to_line(1, None);
        app.tabs[1].state.go_to_line(3, None);
        app.active = Some(1);
        let layout = app.capture_editor_layout();
        assert!(app.save_workspace());
        let persisted = Workspace::load(root.path());
        assert_eq!(persisted.open_files.len(), 1);
        assert_eq!(persisted.editor_layout.as_ref(), Some(&layout));
        assert_eq!(layout.instances.len(), 2);
        assert_ne!(
            layout.instances[0].file.caret_line,
            layout.instances[1].file.caret_line
        );
        let restored = IdeApp::new(settings, Some(root.path().to_path_buf()));
        assert_eq!(restored.tabs.len(), 2);
        assert!(restored.tabs[0]
            .state
            .doc
            .shares_authority(&restored.tabs[1].state.doc));
        assert_eq!(restored.capture_editor_layout(), layout);
        assert_eq!(restored.tabs[restored.active.unwrap()].group, 1);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            restored.tabs[0].state.doc.to_bytes().unwrap()
        );
    }
    #[test]
    fn closing_one_dirty_instance_retains_history_revision_and_subsequent_save() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("shared.py");
        std::fs::write(&path, "value = 'π🙂'\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&path);
        app.split_shared_editor(SplitDir::Horizontal);
        app.tabs[1]
            .state
            .insert_text("# shared\n", &EditOptions::default());
        let identity = app.tabs[1].state.doc.identity();
        let revision = app.disk_revisions[&path];
        app.request_close_tab(0);
        assert_eq!(app.tabs.len(), 1);
        assert!(matches!(app.dialog, Dialog::None));
        assert_eq!(app.tabs[0].state.doc.identity(), identity);
        assert!(app.tabs[0].state.doc.can_undo());
        assert_eq!(app.disk_revisions[&path], revision);
        assert!(app.save_tab(0));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            "# shared\nvalue = 'π🙂'\n".as_bytes()
        );
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), "value = 'π🙂'\n");
    }
    fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 720.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                app.global_shortcuts(ui);
                egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
            },
        );
        output.drop_without_applying_deltas();
    }

    #[test]
    fn real_group_frames_route_unicode_typing_and_undo_to_one_shared_authority() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("value = 1\n", Language::Python));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        frame(&mut app, &ctx, 0.0, vec![]);
        app.handle_action(Action::SplitHorizontally, &ctx);
        frame(&mut app, &ctx, 0.1, vec![]);
        assert_eq!(app.tabs.len(), 2);
        assert!(app.tabs[1].view.focused);
        frame(&mut app, &ctx, 0.2, vec![egui::Event::Text("π🙂".into())]);
        assert_eq!(app.tabs[0].state.doc.text(), "π🙂value = 1\n");
        assert_eq!(app.tabs[1].state.doc.text(), app.tabs[0].state.doc.text());
        let undo = egui::Event::Key {
            key: egui::Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        };
        frame(&mut app, &ctx, 0.3, vec![undo]);
        assert_eq!(app.tabs[0].state.doc.text(), "value = 1\n");
        assert!(app.tabs[1].state.doc.can_redo());
        app.handle_action(Action::NextSplitter, &ctx);
        assert_eq!(app.active, Some(0));
        app.handle_action(Action::ChangeSplitOrientation, &ctx);
        assert!(matches!(app.split, Some(SplitDir::Horizontal)));
        app.handle_action(Action::Unsplit, &ctx);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active, Some(0));
        assert!(app.tabs[0].state.doc.can_redo());
    }
    #[test]
    fn renamed_shared_file_preserves_all_instance_paths_languages_and_save_baseline() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("shared.py");
        let new = root.path().join("shared.txt");
        std::fs::write(&old, "value = 'π🙂'\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&old);
        let encoding = app.tabs[0].state.doc.encoding();
        app.publish_encoding_override(&old, encoding);
        app.split_shared_editor(SplitDir::Horizontal);
        std::fs::rename(&old, &new).unwrap();
        app.tabs[1].state.path = Some(new.clone());
        app.tabs[1].state.set_language(Language::PlainText);
        app.sync_shared_editors();
        assert!(!app.encoding_overrides.contains_key(&old));
        assert!(app.encoding_overrides.contains_key(&new));
        assert!(app
            .tabs
            .iter()
            .all(|tab| tab.state.path.as_deref() == Some(new.as_path())
                && tab.state.language == Language::PlainText));
        assert!(app
            .tabs
            .iter()
            .all(|tab| tab.state.shared_file_change().is_none()));
        app.tabs[1]
            .state
            .insert_text("# peer\n", &EditOptions::default());
        assert!(app.save_tab(0));
        assert_eq!(
            std::fs::read(&new).unwrap(),
            "# peer\nvalue = 'π🙂'\n".as_bytes()
        );
        assert!(!old.exists());
        assert!(!app.tabs[1].state.doc.is_modified());
    }
    #[test]
    fn right_split_open_preserves_existing_source_view_and_failed_open_identity() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a.py");
        let b = root.path().join("b.py");
        std::fs::write(&a, "a = 'π🙂'\n").unwrap();
        std::fs::write(&b, "b = 2\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&a);
        let original = app.tabs[0].view.id;
        app.tabs[0]
            .state
            .insert_text("# dirty\n", &EditOptions::default());
        app.open_in_right_split(&b);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[0].view.id, original);
        assert_eq!(app.tabs[0].group, 0);
        assert_eq!(app.tabs[1].group, 1);
        assert_eq!(app.tabs[1].state.path.as_deref(), Some(b.as_path()));
        assert_eq!(app.tabs[0].state.doc.text(), "# dirty\na = 'π🙂'\n");
        let active = app.active;
        app.open_in_right_split(&root.path().join("missing.py"));
        assert_eq!(app.active, active);
        assert_eq!(app.tabs.len(), 2);
        app.open_in_right_split(&a);
        assert_eq!(app.tabs.len(), 3);
        assert!(app.tabs[0]
            .state
            .doc
            .shares_authority(&app.tabs[2].state.doc));
        assert_eq!(app.tabs[0].view.id, original);
        assert_eq!(std::fs::read(&a).unwrap(), "a = 'π🙂'\n".as_bytes());
    }
    #[test]
    fn notebook_split_keeps_one_model_and_shared_saved_document() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("shared.ipynb");
        std::fs::write(&path, rustpy_notebook::new_notebook().to_string()).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.open_file(&path);
        app.split_shared_editor(SplitDir::Horizontal);
        assert_eq!(app.tabs.len(), 2);
        assert!(app.tabs[0]
            .state
            .doc
            .shares_authority(&app.tabs[1].state.doc));
        assert!(app.tabs[0]
            .notebook
            .as_ref()
            .unwrap()
            .shares_authority(app.tabs[1].notebook.as_ref().unwrap()));
        app.tabs[1]
            .notebook
            .as_mut()
            .unwrap()
            .set_cell_source(0, "print('π🙂')".into());
        assert_eq!(
            app.tabs[0].notebook.as_ref().unwrap().model().cells[0].source,
            "print('π🙂')"
        );
        IdeApp::sync_notebook_doc(&mut app.tabs[1]);
        assert_eq!(app.tabs[0].state.doc.text(), app.tabs[1].state.doc.text());
        app.save_tab(0);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            app.tabs[1].state.doc.text()
        );
        app.request_close_tab(0);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(
            app.tabs[0].notebook.as_ref().unwrap().model().cells[0].source,
            "print('π🙂')"
        );
        app.tabs[0].state.undo();
        assert!(!app.tabs[0].state.doc.text().contains("print('π🙂')"));
    }

    #[test]
    fn right_open_with_one_tab_limit_keeps_original_instance() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a.py");
        let b = root.path().join("b.py");
        std::fs::write(&a, "a = 1\n").unwrap();
        std::fs::write(&b, "b = 2\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.settings.editor.tab_limit = 1;
        app.open_file(&a);
        let original = app.tabs[0].view.id;
        app.open_in_right_split(&b);
        assert_eq!(app.settings.editor.tab_limit, 1);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[0].view.id, original);
        assert_eq!(app.tabs[0].group, 0);
        assert_eq!(app.tabs[1].group, 1);
    }
    #[test]
    fn real_notebook_frames_edit_only_active_instance_and_share_unicode_model() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cells.ipynb");
        std::fs::write(&path, rustpy_notebook::new_notebook().to_string()).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.open_file(&path);
        app.split_shared_editor(SplitDir::Horizontal);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        frame(&mut app, &ctx, 0.0, vec![]);
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut app, &ctx, 0.1, vec![enter]);
        frame(&mut app, &ctx, 0.2, vec![egui::Event::Text("π🙂".into())]);
        assert_eq!(
            app.tabs[1].notebook.as_ref().unwrap().model().cells[0].source,
            "π🙂"
        );
        assert_eq!(
            app.tabs[0].notebook.as_ref().unwrap().model().cells[0].source,
            "π🙂"
        );
        frame(&mut app, &ctx, 0.8, vec![]);
        assert_eq!(app.tabs[0].state.doc.text(), app.tabs[1].state.doc.text());
        assert!(app.tabs[0].state.doc.can_undo());
        assert_eq!(app.active, Some(1));
        app.request_close_tab(0);
        assert_eq!(app.tabs.len(), 1);
        assert!(app.save_tab(0));
        assert!(std::fs::read_to_string(&path).unwrap().contains("π🙂"));
    }

    #[test]
    fn real_image_frames_render_both_instances_with_independent_zoom() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("", Language::PlainText));
        app.tabs[0].state.doc.set_read_only(true);
        app.tabs[0].image = Some(ImageTab {
            rgba: vec![255, 0, 0, 255],
            size: [1, 1],
            texture: None,
            zoom: 1.0,
            checkerboard: true,
            pixel_grid: false,
        });
        app.split_shared_editor(SplitDir::Horizontal);
        app.tabs[1].image.as_mut().unwrap().zoom = 2.0;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        frame(&mut app, &ctx, 0.0, vec![]);
        assert!(app
            .tabs
            .iter()
            .all(|tab| tab.image.as_ref().unwrap().texture.is_some()));
        assert_eq!(app.tabs[0].image.as_ref().unwrap().zoom, 1.0);
        assert_eq!(app.tabs[1].image.as_ref().unwrap().zoom, 2.0);
        assert!(app.tabs[0]
            .state
            .doc
            .shares_authority(&app.tabs[1].state.doc));
        app.tabs[1]
            .state
            .insert_text("refused", &EditOptions::default());
        assert_eq!(app.tabs[0].state.doc.text(), "");
        app.request_close_tab(0);
        assert_eq!(app.tabs.len(), 1);
        assert!(app.tabs[0].state.doc.is_read_only());
    }
    #[test]
    fn actual_project_menu_right_split_preserves_source_and_routes_exact_clicked_target() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a.py");
        let b = root.path().join("b_β.py");
        std::fs::write(&a, "source = 'π🙂'\n").unwrap();
        std::fs::write(&b, "target = 2\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&a);
        app.tabs[0]
            .state
            .insert_text("# dirty\n", &EditOptions::default());
        let original = app.tabs[0].view.id;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut render = |events| {
            let mut action = None;
            let output = ctx.run_ui(
                egui::RawInput {
                    focused: true,
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 700.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let keyboard = app.editor_keyboard();
                    action = app.project.as_mut().unwrap().tree.ui(
                        ui,
                        &app.theme,
                        &app.keymap,
                        keyboard,
                    );
                },
            );
            (output, action)
        };
        render(vec![]).0.drop_without_applying_deltas();
        let output = render(vec![]).0;
        let file_point = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "b_β.py" => {
                    Some(text.pos + text.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .expect("actual painted target filename");
        output.drop_without_applying_deltas();
        for pressed in [true, false] {
            render(vec![
                egui::Event::PointerMoved(file_point),
                egui::Event::PointerButton {
                    pos: file_point,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ])
            .0
            .drop_without_applying_deltas();
        }
        let output = render(vec![]).0;
        assert!(egui::Popup::is_any_open(&ctx));
        let menu_point = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "Open in Right Split" => {
                    Some(text.pos + text.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .expect("actual bound ProjectTree right-split menu item");
        output.drop_without_applying_deltas();
        let mut intent = None;
        for pressed in [true, false] {
            let (output, action) = render(vec![
                egui::Event::PointerMoved(menu_point),
                egui::Event::PointerButton {
                    pos: menu_point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            output.drop_without_applying_deltas();
            if action.is_some() {
                intent = action;
            }
        }
        assert_eq!(intent, Some(TreeAction::OpenInRightSplit(b.clone())));
        let Some(TreeAction::OpenInRightSplit(path)) = intent else {
            unreachable!()
        };
        app.open_in_right_split(&path);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[0].view.id, original);
        assert_eq!(app.tabs[0].state.doc.text(), "# dirty\nsource = 'π🙂'\n");
        assert_eq!(app.tabs[1].state.path.as_deref(), Some(b.as_path()));
        assert_eq!(app.tabs[1].group, 1);
        assert_eq!(std::fs::read(&a).unwrap(), "source = 'π🙂'\n".as_bytes());
    }
}
