//! Shared editor-session restoration for project reopen and task contexts.
use super::*;
use crate::editor::session::SessionPosition;

impl IdeApp {
    pub(super) fn update_detached_bounds(
        &mut self,
        inner: Option<egui::Rect>,
        outer: Option<egui::Rect>,
    ) {
        if let Some(inner) = inner {
            let origin = outer.unwrap_or(inner).min;
            self.detached_bounds = Some(rustpy_settings::LogicalScreenBounds {
                x: origin.x,
                y: origin.y,
                width: inner.width(),
                height: inner.height(),
            });
        }
    }
    pub(super) fn detached_viewport_id(&self, index: usize) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("detached", self.tabs[index].view.id))
    }
    pub(super) fn editor_split_panel_id(&self, stacked: bool) -> egui::Id {
        egui::Id::new(("editor_split", self.editor_layout_generation, stacked))
    }
    pub(super) fn seed_editor_split_panel(&mut self, ctx: &egui::Context, id: egui::Id) {
        if self.editor_layout_seed_pending {
            ctx.data_mut(|data| data.remove::<egui::containers::panel::PanelState>(id));
            self.editor_layout_seed_pending = false;
        }
    }
    pub(super) fn capture_editor_layout(&self) -> rustpy_settings::EditorLayout {
        let mut layout = rustpy_settings::EditorLayout {
            instances: self
                .tabs
                .iter()
                .filter_map(|tab| {
                    tab.state
                        .path
                        .clone()
                        .map(|path| rustpy_settings::EditorInstanceState {
                            file: tab
                                .view
                                .session_position(&tab.state)
                                .into_open_file_state(path, tab.pinned),
                            group: tab.group,
                        })
                })
                .collect(),
            active_group: self
                .active
                .and_then(|index| self.tabs.get(index))
                .map(|tab| tab.group)
                .unwrap_or(0),
            detached_group: self
                .detached
                .and_then(|index| self.tabs.get(index))
                .map(|tab| tab.group)
                .unwrap_or(0),
            groups: self
                .tabs
                .iter()
                .filter_map(|tab| {
                    tab.state
                        .path
                        .clone()
                        .map(|path| rustpy_settings::EditorGroupMember {
                            path,
                            group: tab.group,
                        })
                })
                .collect(),
            split: self.split.map(|split| match split {
                SplitDir::Horizontal => rustpy_settings::EditorSplit::SideBySide,
                SplitDir::Vertical => rustpy_settings::EditorSplit::Stacked,
            }),
            split_fraction: self.split_fraction,
            group_active: std::array::from_fn(|group| {
                self.group_active_tab(group as u8)
                    .and_then(|index| self.tabs[index].state.path.clone())
            }),
            detached: self
                .detached
                .and_then(|index| self.tabs.get(index))
                .and_then(|tab| tab.state.path.clone())
                .map(|path| rustpy_settings::DetachedEditorState {
                    path,
                    bounds: self.detached_bounds,
                }),
        };
        let files = self
            .tabs
            .iter()
            .filter_map(|tab| {
                tab.state.path.clone().map(|path| {
                    tab.view
                        .session_position(&tab.state)
                        .into_open_file_state(path, tab.pinned)
                })
            })
            .collect::<Vec<_>>();
        let active = self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| tab.state.path.as_deref());
        layout.normalize(&files, active);
        layout
    }
    pub(super) fn restore_editor_layout(&mut self, saved: Option<&rustpy_settings::EditorLayout>) {
        self.editor_layout_generation = self.editor_layout_generation.wrapping_add(1);
        self.editor_layout_seed_pending = true;
        let mut layout = saved.cloned().unwrap_or_default();
        let files = self
            .tabs
            .iter()
            .filter_map(|tab| {
                tab.state.path.clone().map(|path| {
                    tab.view
                        .session_position(&tab.state)
                        .into_open_file_state(path, tab.pinned)
                })
            })
            .collect::<Vec<_>>();
        layout.normalize(&files, None);
        if !layout.instances.is_empty() {
            // Files were loaded once by legacy restoration. Duplicate only their editor instances.
            for instance in &layout.instances {
                let Some(source) = self.tabs.iter().position(|tab| {
                    tab.state.path.as_deref() == Some(instance.file.path.as_path())
                }) else {
                    continue;
                };
                // Assign the first existing instance to the first saved group before cloning peers.
                let first = layout
                    .instances
                    .iter()
                    .find(|entry| entry.file.path == instance.file.path)
                    .unwrap();
                if instance == first {
                    self.tabs[source].group = instance.group;
                }
                if let Some(index) = self.open_document_in_group(source, instance.group) {
                    let loaded = !self.is_large_file_placeholder(index);
                    let tab = &mut self.tabs[index];
                    tab.view.restore_session_position(
                        &mut tab.state,
                        SessionPosition::from(&instance.file),
                        loaded,
                    );
                    tab.pinned = instance.file.pinned;
                }
            }
            // The schema stores editor-instance order, not just legacy file order.
            self.tabs.sort_by_key(|tab| {
                layout
                    .instances
                    .iter()
                    .position(|instance| {
                        tab.state.path.as_deref() == Some(instance.file.path.as_path())
                            && tab.group == instance.group
                    })
                    .unwrap_or(usize::MAX)
            });
        }
        for tab in &mut self.tabs {
            if layout.instances.iter().any(|instance| {
                instance.file.path.as_path() == tab.state.path.as_deref().unwrap_or(Path::new(""))
                    && instance.group == tab.group
            }) {
                continue;
            }
            tab.group = layout
                .groups
                .iter()
                .find(|member| tab.state.path.as_deref() == Some(member.path.as_path()))
                .map(|member| member.group)
                .unwrap_or(0);
        }
        self.split = layout.split.map(|split| match split {
            rustpy_settings::EditorSplit::SideBySide => SplitDir::Horizontal,
            rustpy_settings::EditorSplit::Stacked => SplitDir::Vertical,
        });
        self.split_fraction = layout.split_fraction;
        self.group_active = std::array::from_fn(|group| {
            layout.group_active[group]
                .as_deref()
                .and_then(|path| {
                    self.tabs.iter().find(|tab| {
                        tab.group as usize == group && tab.state.path.as_deref() == Some(path)
                    })
                })
                .map(|tab| tab.view.id)
        });
        if let Some(path) = saved
            .and_then(|layout| layout.group_active[layout.active_group.min(1) as usize].as_deref())
        {
            if let Some(index) = self.tabs.iter().position(|tab| {
                tab.group == layout.active_group && tab.state.path.as_deref() == Some(path)
            }) {
                self.active = Some(index);
            }
        }
        self.detached = layout.detached.as_ref().and_then(|detached| {
            self.tabs.iter().position(|tab| {
                tab.group == layout.detached_group
                    && tab.state.path.as_deref() == Some(detached.path.as_path())
            })
        });
        self.detached_bounds = layout.detached.and_then(|detached| detached.bounds);
        self.detached_bounds_pending = self.detached.is_some();
    }

    /// Navigation proceeds only in the requested editor after a successful open.
    pub(super) fn opened_editor_index(&self, path: &Path) -> Option<usize> {
        let normalized = if path.starts_with("<typeshed>") {
            path.to_path_buf()
        } else {
            rustpy_core::vfs::normalize_path(path)
        };
        self.active.filter(|&i| {
            self.tabs
                .get(i)
                .is_some_and(|tab| tab.state.path.as_deref() == Some(normalized.as_path()))
        })
    }

    pub(super) fn restore_editor_session(&mut self, file: &rustpy_settings::OpenFileState) {
        self.open_file(&file.path);
        let Some(index) = self.opened_editor_index(&file.path) else {
            return;
        };
        let loaded = !self.is_large_file_placeholder(index);
        let tab = &mut self.tabs[index];
        tab.view
            .restore_session_position(&mut tab.state, SessionPosition::from(file), loaded);
        tab.pinned = file.pinned;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(broken: bool) -> (tempfile::TempDir, IdeApp, rustpy_settings::OpenFileState) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.py");
        let text = (0..100)
            .map(|n| format!("value_{n} = {n}\n"))
            .collect::<String>();
        std::fs::write(&path, text).unwrap();
        let saved = rustpy_settings::OpenFileState {
            path: path.clone(),
            caret_line: 3,
            caret_column: 2,
            scroll_line: 45,
            pinned: true,
        };
        let mut workspace = Workspace {
            open_files: vec![saved.clone()],
            active_file: Some(path),
            ..Default::default()
        };
        if broken {
            let path = dir.path().join("broken.py");
            std::fs::write(&path, b"# coding: utf-8\nvalue = '\xff'\n").unwrap();
            workspace.open_files.push(rustpy_settings::OpenFileState {
                path,
                caret_line: 99,
                caret_column: 9,
                scroll_line: 88,
                pinned: false,
            });
        }
        workspace.save(dir.path()).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        let app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        (dir, app, saved)
    }

    #[test]
    fn project_and_task_sessions_keep_independent_scroll_before_layout() {
        let (dir, mut app, saved) = fixture(false);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.current_context().open_files, vec![saved.clone()]);
        app.save_workspace();
        assert_eq!(Workspace::load(dir.path()).open_files, vec![saved.clone()]);
        let context = app.current_context();
        app.restore_context(context, true);
        assert_eq!(app.current_context().open_files, vec![saved]);
    }

    #[test]
    fn rejected_session_file_does_not_replace_previous_caret_scroll_or_pin() {
        let (_dir, app, saved) = fixture(true);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.current_context().open_files, vec![saved]);
    }

    #[test]
    fn go_to_line_on_empty_placeholder_cancels_saved_positions_before_completion() {
        let (_dir, mut app, saved) = fixture(false);
        let tab = &mut app.tabs[0];
        tab.state = EditorState::from_text("", Language::Python);
        tab.state.path = Some(saved.path.clone());
        tab.view
            .restore_session_position(&mut tab.state, SessionPosition::from(&saved), false);
        app.dialog = Dialog::GoToLine("10".into());
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| app.dialogs(ui.ctx()))
            .drop_without_applying_deltas();
        assert!(matches!(app.dialog, Dialog::None));
        let tab = &mut app.tabs[0];
        tab.state = EditorState::from_text(
            &(0..100)
                .map(|n| format!("value_{n} = {n}\n"))
                .collect::<String>(),
            Language::Python,
        );
        tab.state.path = Some(saved.path);
        tab.view.complete_session_load(&mut tab.state);
        assert_eq!(
            tab.state.primary_position(),
            rustpy_core::Position::new(0, 0)
        );
        assert_eq!(tab.view.session_position(&tab.state).scroll_line, 0);
    }

    #[test]
    fn rejected_navigation_target_preserves_previous_saved_session() {
        let (dir, mut app, saved) = fixture(false);
        let rejected = dir.path().join("rejected.py");
        std::fs::write(&rejected, b"# coding: utf-8\nvalue = '\xff'\n").unwrap();
        app.goto_file_line(&rejected, 90);
        assert_eq!(app.current_context().open_files, vec![saved]);
    }
}

#[cfg(test)]
mod workspace_persistence_tests {
    use super::*;
    fn app(root: &Path) -> IdeApp {
        let mut settings = Settings::default();
        settings.trust_project(root);
        let app = IdeApp::new(settings, Some(root.to_path_buf()));
        assert_eq!(app.project.as_ref().unwrap().root, root);
        app
    }
    #[test]
    fn corrupt_workspace_is_reported_and_never_overwritten_by_actual_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = Workspace::path_for(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let corrupt = b"{\"open_files\": [ broken owned fixture";
        std::fs::write(&path, corrupt).unwrap();
        let mut app = app(dir.path());
        app.save_workspace();
        app.close_project();
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
        assert!(
            app.status
                .as_ref()
                .is_some_and(|(message, _)| message.contains("Workspace persistence failed")),
            "workspace error must remain visible"
        );
    }
    #[test]
    fn stale_window_close_preserves_another_windows_codec_choice() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("owned.txt");
        std::fs::write(&file, b"owned").unwrap();
        Workspace {
            file_encodings: vec![rustpy_settings::FileEncodingOverride {
                path: file.clone(),
                encoding: "latin1".into(),
            }],
            ..Default::default()
        }
        .save(dir.path())
        .unwrap();
        let mut stale = app(dir.path());
        let mut writer = app(dir.path());
        writer.publish_encoding_override(&file, rustpy_text::Encoding::Utf8);
        writer.persist_encoding_overrides();
        stale.close_project();
        let saved = Workspace::load(dir.path());
        assert_eq!(
            saved
                .file_encodings
                .iter()
                .find(|entry| entry.path == file)
                .unwrap()
                .encoding,
            rustpy_text::Encoding::Utf8.key()
        );
    }
    #[test]
    fn pending_codec_deletion_and_interpreter_and_tasks_survive_failed_write_then_retry() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("owned.txt");
        Workspace {
            file_encodings: vec![rustpy_settings::FileEncodingOverride {
                path: file.clone(),
                encoding: "utf-8".into(),
            }],
            ..Default::default()
        }
        .save(dir.path())
        .unwrap();
        let path = Workspace::path_for(dir.path());
        let original = std::fs::read(&path).unwrap();
        let mut app = app(dir.path());
        std::fs::write(&path, b"owned invalid bytes").unwrap();
        app.encoding_overrides.remove(&file);
        app.persist_encoding_overrides();
        let selected = dir.path().join("external-python");
        app.record_interpreter_choice(&selected);
        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(vec![Interpreter {
            path: selected.clone(),
            version: "owned fixture".into(),
            prefix: dir.path().to_path_buf(),
            sys_path: Vec::new(),
            is_venv: false,
            platform: "linux".into(),
            kind: "recorded".into(),
        }])
        .unwrap();
        app.project.as_mut().unwrap().interpreter_rx = Some(rx);
        app.poll_interpreters();
        assert_eq!(
            app.project
                .as_ref()
                .unwrap()
                .interpreter
                .as_ref()
                .unwrap()
                .path,
            selected
        );
        app.set_startup_task("Owned run", true);
        assert_eq!(
            app.effective_workspace().interpreter,
            Some(selected.clone())
        );
        assert!(app
            .effective_workspace()
            .startup_tasks
            .contains(&"Owned run".to_owned()));
        assert!(!app.encoding_overrides.contains_key(&file));
        assert_eq!(std::fs::read(&path).unwrap(), b"owned invalid bytes");
        assert!(app
            .notifications
            .items()
            .iter()
            .any(|item| item.body.contains("workspace.json")));
        std::fs::write(&path, original).unwrap();
        app.persist_encoding_overrides();
        let saved = Workspace::try_load(dir.path()).unwrap();
        assert!(saved.file_encodings.is_empty());
        assert_eq!(saved.interpreter, Some(selected));
        assert_eq!(saved.startup_tasks, vec!["Owned run"]);
    }
    #[test]
    fn failed_workspace_switch_keeps_project_and_writer_lease_until_explicit_discard() {
        let dir = tempfile::tempdir().unwrap();
        let next = tempfile::tempdir().unwrap();
        Workspace::default().save(dir.path()).unwrap();
        let path = Workspace::path_for(dir.path());
        let mut app = app(dir.path());
        assert!(
            app.workspace_client
                .as_ref()
                .unwrap()
                .session_lease_identity()
                .is_some(),
            "initial session ownership: {:?}",
            app.workspace_client.as_ref().unwrap().error
        );
        let initial_lease = app
            .workspace_client
            .as_ref()
            .unwrap()
            .session_lease_identity()
            .unwrap();
        #[cfg(unix)]
        let initial_lock_identity = {
            use std::os::unix::fs::MetadataExt;
            let metadata =
                std::fs::metadata(dir.path().join(".rustpy/workspace.session.lock")).unwrap();
            (metadata.dev(), metadata.ino())
        };
        std::fs::write(&path, b"damaged after opening").unwrap();
        app.set_startup_task("Pending task", true);
        app.open_project(next.path());
        assert_eq!(app.project.as_ref().unwrap().root, dir.path());
        assert!(matches!(app.dialog, Dialog::WorkspacePersistence(_)));
        assert_eq!(
            app.workspace_client
                .as_ref()
                .unwrap()
                .session_lease_identity()
                .unwrap(),
            initial_lease,
            "failed switch must retain the same lease object"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata =
                std::fs::metadata(dir.path().join(".rustpy/workspace.session.lock")).unwrap();
            assert_eq!((metadata.dev(), metadata.ino()), initial_lock_identity);
        }
        assert!(rustpy_settings::WorkspaceStore::new(dir.path())
            .try_acquire_session()
            .unwrap()
            .is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"damaged after opening");
        assert!(app
            .effective_workspace()
            .startup_tasks
            .contains(&"Pending task".to_owned()));
    }
    #[test]
    fn cancelling_unsaved_project_switch_retains_session_writer_lease() {
        let dir = tempfile::tempdir().unwrap();
        let next = tempfile::tempdir().unwrap();
        let path = dir.path().join("owned.txt");
        std::fs::write(&path, "owned").unwrap();
        let mut app = app(dir.path());
        app.open_file(&path);
        app.tabs[0].state.apply_text_edits(
            vec![(
                rustpy_core::TextRange::new(TextSize::new(0), TextSize::new(0)),
                "dirty ".into(),
            )],
            "Owned dirty edit",
        );
        app.tabs[0].state.doc.set_read_only(true);
        app.open_project(next.path());
        assert_eq!(app.project.as_ref().unwrap().root, dir.path());
        assert!(matches!(app.dialog, Dialog::OpenProject(_)));
        assert!(rustpy_settings::WorkspaceStore::new(dir.path())
            .try_acquire_session()
            .unwrap()
            .is_none());
        assert!(app.tabs[0].state.doc.text().starts_with("dirty "));
    }
    #[test]
    fn secondary_window_cannot_replace_primary_editor_session_but_can_save_choice() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("owned.txt");
        std::fs::write(&file, "owned").unwrap();
        let mut primary = app(dir.path());
        primary.open_file(&file);
        primary.save_workspace();
        let mut secondary = app(dir.path());
        secondary.tabs.clear();
        secondary.active = None;
        secondary.set_startup_task("Secondary choice", true);
        secondary.close_project();
        let saved = Workspace::try_load(dir.path()).unwrap();
        assert_eq!(saved.open_files.len(), 1);
        assert_eq!(saved.open_files[0].path, file);
        assert!(saved.startup_tasks.contains(&"Secondary choice".to_owned()));
        primary.close_project();
        assert!(
            primary.project.is_none(),
            "primary close was refused: {:?}",
            primary
                .workspace_client
                .as_ref()
                .and_then(|client| client.error.as_ref())
        );
        assert!(rustpy_settings::WorkspaceStore::new(dir.path())
            .try_acquire_session()
            .unwrap()
            .is_some());
    }

    #[test]
    fn explicit_choice_on_provisional_workspace_blocks_close_and_preserves_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = Workspace::path_for(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"damaged startup").unwrap();
        let mut app = app(dir.path());
        app.set_startup_task("Explicit intent", true);
        app.close_project();
        assert_eq!(app.project.as_ref().unwrap().root, dir.path());
        assert!(matches!(app.dialog, Dialog::WorkspacePersistence(_)));
        assert!(app
            .effective_workspace()
            .startup_tasks
            .contains(&"Explicit intent".to_owned()));
        assert_eq!(std::fs::read(&path).unwrap(), b"damaged startup");
    }
}

#[cfg(test)]
mod workspace_pending_reversion_tests {
    use super::*;
    #[test]
    fn failed_choices_reverted_to_acknowledged_values_are_not_later_published() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("owned.txt");
        let mut ws = Workspace {
            file_encodings: vec![rustpy_settings::FileEncodingOverride {
                path: file.clone(),
                encoding: "utf-8".into(),
            }],
            bookmarks: vec![rustpy_settings::BookmarkState {
                kind: rustpy_settings::BookmarkKind::Line,
                list: String::new(),
                path: file.clone(),
                line: 1,
                mnemonic: None,
                description: "original".into(),
            }],
            ..Default::default()
        };
        let mut bp = crate::breakpoints::BreakpointStore::default();
        bp.set(
            &file,
            1,
            crate::breakpoints::LineBp {
                condition: "original".into(),
                ..Default::default()
            },
        );
        bp.write_to(&mut ws);
        ws.save(dir.path()).unwrap();
        let path = Workspace::path_for(dir.path());
        let original = std::fs::read(&path).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        let mut app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        std::fs::write(&path, b"damaged after open").unwrap();
        app.publish_encoding_override(&file, rustpy_text::Encoding::Latin1);
        app.bookmarks[0].description = "failed".into();
        app.breakpoints.set(
            &file,
            1,
            crate::breakpoints::LineBp {
                condition: "failed".into(),
                ..Default::default()
            },
        );
        assert!(!app.save_workspace());
        app.publish_encoding_override(&file, rustpy_text::Encoding::Utf8);
        app.bookmarks[0].description = "original".into();
        app.breakpoints.set(
            &file,
            1,
            crate::breakpoints::LineBp {
                condition: "original".into(),
                ..Default::default()
            },
        );
        std::fs::write(&path, original).unwrap();
        assert!(app.save_workspace());
        let saved = Workspace::try_load(dir.path()).unwrap();
        assert_eq!(saved.file_encodings[0].encoding, "utf-8");
        assert_eq!(saved.bookmarks[0].description, "original");
        assert_eq!(saved.line_breakpoints[0].condition, "original");
    }
    #[test]
    fn codec_acknowledgement_does_not_rebase_unacknowledged_breakpoint_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("owned.txt");
        let mut ws = Workspace::default();
        let mut bp = crate::breakpoints::BreakpointStore::default();
        bp.set(
            &file,
            1,
            crate::breakpoints::LineBp {
                condition: "initial".into(),
                ..Default::default()
            },
        );
        bp.write_to(&mut ws);
        ws.save(dir.path()).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        let mut app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        app.breakpoints.set(
            &file,
            1,
            crate::breakpoints::LineBp {
                condition: "local".into(),
                ..Default::default()
            },
        );
        let store = rustpy_settings::WorkspaceStore::new(dir.path());
        let baseline = store.load().unwrap();
        let mut remote = baseline.workspace().clone();
        bp.set(
            &file,
            1,
            crate::breakpoints::LineBp {
                condition: "remote".into(),
                ..Default::default()
            },
        );
        bp.write_to(&mut remote);
        store
            .commit(
                &baseline,
                &rustpy_settings::WorkspacePatch {
                    breakpoints: Some(rustpy_settings::WorkspaceBreakpoints::from_workspace(
                        &remote,
                    )),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        assert!(!app.save_workspace());
        app.publish_encoding_override(&file, rustpy_text::Encoding::Latin1);
        app.persist_encoding_overrides();
        let partial = Workspace::try_load(dir.path()).unwrap();
        assert_eq!(
            partial.file_encodings[0].encoding,
            rustpy_text::Encoding::Latin1.key()
        );
        assert_eq!(partial.line_breakpoints[0].condition, "remote");
        assert!(!app.save_workspace());
        assert_eq!(
            Workspace::try_load(dir.path()).unwrap().line_breakpoints[0].condition,
            "remote"
        );
        assert_eq!(app.breakpoints.get(&file, 1).unwrap().condition, "local");
    }
}

#[cfg(test)]
mod workspace_discard_lifecycle_tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, IdeApp, PathBuf, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        Workspace::default().save(dir.path()).unwrap();
        let path = Workspace::path_for(dir.path());
        let original = std::fs::read(&path).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        settings.confirm_exit = false;
        let app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        (dir, app, path, original)
    }
    #[test]
    fn accepted_exit_discard_survives_actual_on_exit_without_rewriting_damaged_workspace() {
        let (_dir, mut app, path, _original) = fixture();
        std::fs::write(&path, b"damaged after opening").unwrap();
        app.set_startup_task("Discarded", true);
        app.continue_exit();
        assert!(matches!(app.dialog, Dialog::WorkspacePersistence(_)));
        assert!(!app.exit_requested);
        app.answer_workspace_persistence(workspace_ui::WorkspaceTransition::Exit, true);
        assert!(app.exit_requested);
        eframe::App::on_exit(&mut app, None);
        assert_eq!(std::fs::read(path).unwrap(), b"damaged after opening");
    }
    #[test]
    fn discarded_switch_refusal_does_not_disable_later_intent_persistence() {
        let (dir, mut app, path, original) = fixture();
        let next = tempfile::tempdir().unwrap();
        let file = dir.path().join("owned.txt");
        std::fs::write(&file, "owned").unwrap();
        app.open_file(&file);
        std::fs::write(&path, b"damaged after opening").unwrap();
        app.set_startup_task("Discarded", true);
        assert!(
            !app.workspace_transition(workspace_ui::WorkspaceTransition::Switch(
                next.path().to_path_buf()
            ))
        );
        app.tabs[0].state.apply_text_edits(
            vec![(
                TextRange::new(TextSize::new(0), TextSize::new(0)),
                "dirty ".into(),
            )],
            "Later source edit",
        );
        app.tabs[0].state.doc.set_read_only(true);
        app.answer_workspace_persistence(
            workspace_ui::WorkspaceTransition::Switch(next.path().to_path_buf()),
            true,
        );
        assert_eq!(app.project.as_ref().unwrap().root, dir.path());
        assert!(matches!(app.dialog, Dialog::OpenProject(_)));
        std::fs::write(&path, original).unwrap();
        app.publish_encoding_override(&file, rustpy_text::Encoding::Latin1);
        app.persist_encoding_overrides();
        app.answer_workspace_persistence(workspace_ui::WorkspaceTransition::Exit, false);
        let saved = Workspace::try_load(dir.path()).unwrap();
        assert_eq!(
            saved.file_encodings[0].encoding,
            rustpy_text::Encoding::Latin1.key()
        );
        assert!(!saved.startup_tasks.contains(&"Discarded".to_owned()));
        assert!(!app.exit_requested);
    }
}

#[cfg(test)]
mod notebook_workspace_exit_tests {
    use super::*;
    #[test]
    fn dirty_notebook_clean_document_requires_exit_prompt_and_cancel_preserves_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owned.ipynb");
        let bytes = rustpy_notebook::new_notebook().to_string().into_bytes();
        std::fs::write(&path, &bytes).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        settings.confirm_exit = false;
        let mut app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        app.open_file(&path);
        let version = app.tabs[0].state.doc.version();
        let text = app.tabs[0].state.doc.text();
        let notebook = app.tabs[0].notebook.as_mut().unwrap();
        notebook.set_cell_source(0, "print('owned pending model')  \n".into());
        notebook.set_dirty(true);
        assert!(!app.tabs[0].state.doc.is_modified());
        assert!(
            app.intercept_close(&egui::Context::default()),
            "actual close must be cancelled to ask about the dirty notebook model"
        );
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmClose {
                tab: 0,
                then_exit: true
            }
        ));
        app.answer_confirm_close(0, true, layout::CloseAnswer::Cancel);
        assert!(!app.exit_requested);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert!(app.tabs[0].notebook.as_ref().unwrap().is_dirty());
        assert_eq!(
            app.tabs[0].notebook.as_ref().unwrap().model().cells[0].source,
            "print('owned pending model')  \n"
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

#[cfg(test)]
mod workspace_recovery_tests {
    use super::*;
    #[test]
    fn provisional_task_toggle_reversion_allows_clean_close_without_writing() {
        let root = tempfile::tempdir().unwrap();
        let path = Workspace::path_for(root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"damaged").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.set_startup_task("Owned task", true);
        app.set_startup_task("Owned task", false);
        app.close_project();
        assert!(app.project.is_none());
        assert_eq!(std::fs::read(path).unwrap(), b"damaged");
    }
    #[test]
    fn recovered_saved_codec_is_used_for_actual_open_without_losing_pending_choice() {
        let root = tempfile::tempdir().unwrap();
        let path = Workspace::path_for(root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"damaged").unwrap();
        let file = root.path().join("owned.txt");
        std::fs::write(&file, [0xE9]).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.set_startup_task("Pending task", true);
        let repaired = Workspace {
            file_encodings: vec![rustpy_settings::FileEncodingOverride {
                path: file.clone(),
                encoding: "latin1".into(),
            }],
            ..Default::default()
        };
        std::fs::write(&path, serde_json::to_vec_pretty(&repaired).unwrap()).unwrap();
        app.recover_workspace_baseline().unwrap();
        app.open_file(&file);
        let index = app.opened_editor_index(&file).unwrap();
        assert_eq!(app.tabs[index].state.doc.text(), "é");
        assert_eq!(
            app.tabs[index].state.doc.encoding(),
            rustpy_text::Encoding::Latin1
        );
        assert!(app
            .effective_workspace()
            .startup_tasks
            .contains(&"Pending task".to_owned()));
        assert!(app.save_workspace());
        assert_eq!(
            Workspace::try_load(root.path()).unwrap().file_encodings,
            repaired.file_encodings
        );
    }
}

#[cfg(test)]
mod editor_layout_tests {
    use super::*;
    #[test]
    fn reattachment_preserves_all_carets_selections_and_goal_columns() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selected.txt");
        std::fs::write(&path, "first selected line\nsecond selected line\n").unwrap();
        let mut app = owned_app(root.path());
        app.open_file(&path);
        let mut primary = crate::editor::Caret::new(TextSize::new(1), TextSize::new(6));
        primary.goal_column = Some(13);
        let mut secondary = crate::editor::Caret::new(TextSize::new(24), TextSize::new(21));
        secondary.goal_column = Some(9);
        app.tabs[0].state.set_carets(vec![primary, secondary]);
        let carets = app.tabs[0].state.carets.clone();
        let version = app.tabs[0].state.doc.version();
        let text = app.tabs[0].state.doc.text();
        let view_id = app.tabs[0].view.id;
        let last_op = format!("{:?}", app.tabs[0].state.last_op);
        app.detached = Some(0);
        app.reattach_editor();
        assert_eq!(app.tabs[0].state.carets, carets);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert_eq!(app.tabs[0].view.id, view_id);
        assert_eq!(format!("{:?}", app.tabs[0].state.last_op), last_op);
        assert!(app.detached.is_none());
    }
    #[test]
    fn reattaching_unloaded_large_file_retains_pending_session_positions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("large.txt");
        let line = format!("{}\n", "x".repeat(1023));
        std::fs::write(&path, line.repeat(21 * 1024)).unwrap();
        let mut app = owned_app(root.path());
        app.open_file(&path);
        assert!(app.is_large_file_placeholder(0));
        let position = SessionPosition {
            caret: rustpy_text::Position::new(600, 9),
            scroll_line: 580,
        };
        let view_id = app.tabs[0].view.id;
        {
            let tab = &mut app.tabs[0];
            tab.view
                .restore_session_position(&mut tab.state, position, false);
        }
        app.detached = Some(0);
        app.reattach_editor();
        assert!(app.detached.is_none());
        assert!(app.is_large_file_placeholder(0));
        assert_eq!(app.tabs[0].view.id, view_id);
        assert_eq!(
            app.tabs[0].view.session_position(&app.tabs[0].state),
            position
        );
        assert!(app.save_workspace());
        let saved = Workspace::try_load(root.path()).unwrap();
        assert_eq!(saved.open_files[0].caret_line, 600);
        assert_eq!(saved.open_files[0].caret_column, 9);
        assert_eq!(saved.open_files[0].scroll_line, 580);
    }
    #[test]
    fn detached_editor_root_group_does_not_overwrite_restored_scroll() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("detached.txt");
        std::fs::write(
            &path,
            (0..200)
                .map(|n| format!("line {n:04}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let mut app = owned_app(root.path());
        app.open_file(&path);
        app.detached = Some(0);
        let position = SessionPosition {
            caret: rustpy_text::Position::new(36, 3),
            scroll_line: 28,
        };
        {
            let tab = &mut app.tabs[0];
            tab.view
                .restore_session_position(&mut tab.state, position, true);
        }
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(720.0, 430.0),
            )),
            ..Default::default()
        };
        for _ in 0..3 {
            ctx.run_ui(input(), |ui| app.detached_window(ui.ctx()))
                .drop_without_applying_deltas();
            assert_eq!(
                app.tabs[0].view.session_position(&app.tabs[0].state),
                position
            );
            ctx.run_ui(input(), |ui| app.group_area(ui, 0))
                .drop_without_applying_deltas();
            assert_eq!(
                app.tabs[0].view.session_position(&app.tabs[0].state),
                position,
                "the root group must not render over the detached editor's viewport position"
            );
        }
        assert!(app.save_workspace());
        let saved = Workspace::try_load(root.path()).unwrap();
        assert_eq!(saved.open_files[0].scroll_line, 28);
        assert_eq!(saved.open_files[0].caret_line, 36);
        assert_eq!(saved.editor_layout.unwrap().detached.unwrap().path, path);
        app.reattach_editor();
        for _ in 0..3 {
            ctx.run_ui(input(), |ui| app.group_area(ui, 0))
                .drop_without_applying_deltas();
            assert_eq!(
                app.tabs[0].view.session_position(&app.tabs[0].state),
                position
            );
        }
        assert!(app.detached.is_none());
        assert!(app.save_workspace());
        let saved = Workspace::try_load(root.path()).unwrap();
        assert_eq!(saved.open_files[0].scroll_line, 28);
        assert!(saved.editor_layout.unwrap().detached.is_none());
    }
    fn owned_app(root: &Path) -> IdeApp {
        let mut settings = Settings::default();
        settings.trust_project(root);
        IdeApp::new(settings, Some(root.to_path_buf()))
    }
    fn files(root: &Path) -> Vec<PathBuf> {
        ["one.txt", "two.txt", "three.txt"]
            .iter()
            .map(|name| {
                let path = root.join(name);
                std::fs::write(
                    &path,
                    (0..100).map(|n| format!("row_{n}\n")).collect::<String>(),
                )
                .unwrap();
                path
            })
            .collect()
    }
    #[test]
    fn actual_save_and_fresh_app_restore_order_groups_focus_ratio_and_detached_bounds() {
        for split in [SplitDir::Horizontal, SplitDir::Vertical] {
            let root = tempfile::tempdir().unwrap();
            let paths = files(root.path());
            let mut app = owned_app(root.path());
            for path in &paths {
                app.open_file(path);
            }
            app.tabs[0].pinned = true;
            app.tabs[1].group = 1;
            app.tabs[2].group = 1;
            app.split = Some(split);
            app.split_fraction = 0.37;
            app.group_active = [Some(app.tabs[0].view.id), Some(app.tabs[1].view.id)];
            app.active = Some(0);
            app.detached = Some(2);
            app.detached_bounds = Some(rustpy_settings::LogicalScreenBounds {
                x: -100.0,
                y: 50.0,
                width: 800.0,
                height: 500.0,
            });
            let saved = rustpy_settings::OpenFileState {
                path: paths[1].clone(),
                caret_line: 3,
                caret_column: 2,
                scroll_line: 45,
                pinned: false,
            };
            app.restore_editor_session(&saved);
            app.active = Some(0);
            let expected = app.capture_editor_layout();
            assert!(app.save_workspace());
            app.close_project();
            assert!(app.project.is_none());
            let restored = owned_app(root.path());
            assert_eq!(
                restored
                    .tabs
                    .iter()
                    .map(|tab| tab.state.path.clone().unwrap())
                    .collect::<Vec<_>>(),
                paths
            );
            assert!(restored.tabs[0].pinned);
            assert_eq!(restored.capture_editor_layout(), expected);
            assert_eq!(restored.active, Some(0));
            assert_eq!(restored.group_active_tab(1), Some(1));
            assert_eq!(
                restored.tabs[1]
                    .view
                    .session_position(&restored.tabs[1].state),
                SessionPosition::from(&saved)
            );
        }
    }
    #[test]
    fn actual_split_layout_seeds_workspace_ratio_over_stale_egui_panel_state() {
        let root = tempfile::tempdir().unwrap();
        let paths = files(root.path());
        let mut app = owned_app(root.path());
        app.open_file(&paths[0]);
        app.open_file(&paths[1]);
        app.tabs[1].group = 1;
        app.split = Some(SplitDir::Horizontal);
        app.split_fraction = 0.37;
        let layout = app.capture_editor_layout();
        app.restore_editor_layout(Some(&layout));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let id = app.editor_split_panel_id(false);
        ctx.data_mut(|data| {
            data.insert_persisted(
                id,
                egui::containers::panel::PanelState {
                    outer_rect: egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 800.0),
                    ),
                },
            )
        });
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| app.editor_area(ui),
        )
        .drop_without_applying_deltas();
        assert!(
            (app.split_fraction - 0.37).abs() < 0.05,
            "rendered ratio {}",
            app.split_fraction
        );
        assert!(!app.editor_layout_seed_pending);
    }
    #[test]
    fn detached_capture_persists_outer_origin_and_inner_size_without_decoration_drift() {
        let root = tempfile::tempdir().unwrap();
        let paths = files(root.path());
        let mut app = owned_app(root.path());
        app.open_file(&paths[0]);
        app.detached = Some(0);
        let outer = egui::Rect::from_min_size(egui::pos2(-100.0, 40.0), egui::vec2(816.0, 538.0));
        let inner = egui::Rect::from_min_size(egui::pos2(-92.0, 70.0), egui::vec2(800.0, 500.0));
        app.update_detached_bounds(Some(inner), Some(outer));
        assert_eq!(
            app.detached_bounds,
            Some(rustpy_settings::LogicalScreenBounds {
                x: -100.0,
                y: 40.0,
                width: 800.0,
                height: 500.0
            })
        );
        assert!(app.save_workspace());
        assert_eq!(
            Workspace::try_load(root.path())
                .unwrap()
                .editor_layout
                .unwrap()
                .detached
                .unwrap()
                .bounds,
            app.detached_bounds
        );
        app.update_detached_bounds(None, None);
        assert_eq!(app.detached_bounds.unwrap().x, -100.0);
        app.update_detached_bounds(Some(inner), None);
        assert_eq!(app.detached_bounds.unwrap().x, -92.0);
        assert_eq!(app.detached_bounds.unwrap().width, 800.0);
    }
    #[test]
    fn old_session_and_missing_split_member_restore_valid_files_without_empty_group() {
        let root = tempfile::tempdir().unwrap();
        let paths = files(root.path());
        let mut app = owned_app(root.path());
        app.open_file(&paths[0]);
        app.open_file(&paths[1]);
        app.split = Some(SplitDir::Horizontal);
        app.tabs[1].group = 1;
        app.detached = Some(1);
        assert!(app.save_workspace());
        app.close_project();
        std::fs::remove_file(&paths[1]).unwrap();
        let mut restored = owned_app(root.path());
        assert_eq!(restored.tabs.len(), 1);
        assert!(restored.split.is_none());
        assert!(restored.detached.is_none());
        assert_eq!(restored.tabs[0].group, 0);
        restored.close_project();
        let path = Workspace::path_for(root.path());
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("editor_layout");
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let old = owned_app(root.path());
        assert_eq!(old.tabs.len(), 1);
        assert!(old.split.is_none());
        assert_eq!(old.split_fraction, 0.5);
    }
    #[test]
    fn copied_project_layout_resolves_into_new_root_and_detached_id_survives_reorder() {
        let original = tempfile::tempdir().unwrap();
        let paths = files(original.path());
        let mut app = owned_app(original.path());
        for path in &paths {
            app.open_file(path);
        }
        app.split = Some(SplitDir::Vertical);
        app.tabs[1].group = 1;
        app.detached = Some(2);
        assert!(app.save_workspace());
        app.close_project();
        let copy = tempfile::tempdir().unwrap();
        for path in &paths {
            std::fs::copy(path, copy.path().join(path.file_name().unwrap())).unwrap();
        }
        let target = Workspace::path_for(copy.path());
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(Workspace::path_for(original.path()), target).unwrap();
        let mut restored = owned_app(copy.path());
        assert!(restored.tabs.iter().all(|tab| tab
            .state
            .path
            .as_ref()
            .unwrap()
            .starts_with(copy.path())));
        let detached = restored.detached.unwrap();
        let id = restored.detached_viewport_id(detached);
        let detached_path = restored.tabs[detached].state.path.clone();
        restored.tabs[detached].pinned = true;
        restored.sort_pinned_first();
        assert_eq!(
            restored.tabs[restored.detached.unwrap()].state.path,
            detached_path
        );
        assert_eq!(
            restored.detached_viewport_id(restored.detached.unwrap()),
            id
        );
    }
    #[test]
    fn large_placeholder_keeps_saved_layout_and_positions_through_delayed_completion() {
        let root = tempfile::tempdir().unwrap();
        let paths = files(root.path());
        let large = root.path().join("large.txt");
        let file = std::fs::File::create(&large).unwrap();
        file.set_len(super::super::background::LARGE_FILE_GATE_BYTES + 1)
            .unwrap();
        let saved = rustpy_settings::OpenFileState {
            path: large.clone(),
            caret_line: 3,
            caret_column: 2,
            scroll_line: 45,
            pinned: true,
        };
        let workspace = Workspace {
            open_files: vec![
                rustpy_settings::OpenFileState {
                    path: paths[0].clone(),
                    caret_line: 0,
                    caret_column: 0,
                    scroll_line: 0,
                    pinned: false,
                },
                saved.clone(),
            ],
            active_file: Some(large.clone()),
            editor_layout: Some(rustpy_settings::EditorLayout {
                groups: vec![rustpy_settings::EditorGroupMember {
                    path: large.clone(),
                    group: 1,
                }],
                split: Some(rustpy_settings::EditorSplit::SideBySide),
                split_fraction: 0.62,
                ..Default::default()
            }),
            ..Default::default()
        };
        workspace.save(root.path()).unwrap();
        let mut app = owned_app(root.path());
        let index = app
            .tabs
            .iter()
            .position(|tab| tab.state.path.as_deref() == Some(large.as_path()))
            .unwrap();
        assert!(app.is_large_file_placeholder(index));
        assert_eq!(app.tabs[index].group, 1);
        assert_eq!(app.split_fraction, 0.62);
        let id = app.tabs[index].view.id;
        app.tabs[index].state = EditorState::from_text(
            &(0..100).map(|n| format!("row_{n}\n")).collect::<String>(),
            Language::PlainText,
        );
        app.tabs[index].state.path = Some(large);
        let tab = &mut app.tabs[index];
        tab.view.complete_session_load(&mut tab.state);
        assert_eq!(tab.view.id, id);
        assert_eq!(tab.group, 1);
        assert_eq!(
            tab.view.session_position(&tab.state),
            SessionPosition::from(&saved)
        );
    }
}
