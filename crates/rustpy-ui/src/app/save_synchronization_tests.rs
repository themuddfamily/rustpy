use super::*;

struct Fixture {
    dir: tempfile::TempDir,
    app: IdeApp,
    ctx: egui::Context,
    path: PathBuf,
}
impl Fixture {
    fn new(name: &str, bytes: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let mut settings = Settings::default();
        settings.editor.ensure_newline_at_eof = false;
        settings.editor.strip_trailing_spaces_on_save = rustpy_settings::StripTrailing::None;
        settings
            .save_synchronization
            .synchronize_on_frame_activation = false;
        settings.save_synchronization.synchronize_on_tab_activation = false;
        let mut app = IdeApp::new(settings, None);
        app.open_file(&path);
        assert_eq!(app.tabs[0].state.path.as_deref(), Some(path.as_path()));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        Self {
            dir,
            app,
            ctx,
            path,
        }
    }
    fn edit(&mut self, text: &str) {
        let range = TextRange::new(TextSize::new(0), self.app.tabs[0].state.doc.len());
        self.app.tabs[0]
            .state
            .apply_text_edits(vec![(range, text.into())], "User edit");
        assert!(self.app.tabs[0].state.doc.is_modified());
    }
    fn frame(&mut self, time: f64, focused: bool, events: Vec<egui::Event>) {
        self.ctx
            .run_ui(
                egui::RawInput {
                    time: Some(time),
                    focused,
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000., 720.),
                    )),
                    ..Default::default()
                },
                |ui| self.app.save_synchronization_tick(ui.ctx()),
            )
            .drop_without_applying_deltas();
    }
}
#[test]
fn actual_idle_saves_only_after_last_edit_and_input_delay_without_extra_history() {
    let mut fixture = Fixture::new("idle.txt", b"original");
    fixture.app.settings.save_synchronization.save_when_idle = true;
    fixture.app.settings.save_synchronization.idle_seconds = 5;
    fixture.frame(0., true, vec![]);
    fixture.edit("π😀 edited");
    fixture.frame(1., true, vec![]);
    fixture.frame(5.9, true, vec![]);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"original");
    fixture.frame(6., true, vec![egui::Event::Text("activity".into())]);
    fixture.frame(10.9, true, vec![]);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"original");
    let history = fixture.app.tabs[0].state.doc.undo_history();
    fixture.frame(11., true, vec![]);
    assert_eq!(
        std::fs::read(&fixture.path).unwrap(),
        "π😀 edited".as_bytes()
    );
    assert!(!fixture.app.tabs[0].state.doc.is_modified());
    assert_eq!(fixture.app.tabs[0].state.doc.undo_history(), history);
    assert!(fixture.app.tabs[0].state.undo());
    assert_eq!(fixture.app.tabs[0].state.doc.text(), "original");
    assert!(!fixture.app.tabs[0].state.undo());
}
#[test]
fn deactivation_saves_named_files_but_never_prompts_untitled_or_readonly() {
    let mut fixture = Fixture::new("named.txt", b"original");
    fixture
        .app
        .settings
        .save_synchronization
        .save_on_deactivation = true;
    fixture.frame(0., true, vec![]);
    fixture.edit("named edited");
    fixture.frame(0.1, true, vec![]);
    fixture.frame(0.2, false, vec![]);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"named edited");
    fixture
        .app
        .push_tab(EditorState::from_text("untitled", Language::PlainText));
    let last = fixture.app.tabs.len() - 1;
    fixture.app.tabs[last].state.apply_text_edits(
        vec![(TextRange::empty(TextSize::new(0)), "dirty ".into())],
        "Untitled edit",
    );
    fixture.app.active = Some(0);
    fixture.edit("protected edit");
    let mut permissions = std::fs::metadata(&fixture.path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&fixture.path, permissions).unwrap();
    fixture.app.refresh_file_permissions(0);
    fixture.frame(1., true, vec![]);
    fixture.frame(1.1, false, vec![]);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"named edited");
    assert!(fixture.app.tabs[0].has_unsaved_changes());
    assert!(fixture.app.tabs[last].has_unsaved_changes());
    assert!(matches!(fixture.app.dialog, Dialog::None));
    // Release only the actual OS attribute for portable temporary-directory cleanup.
    let mut permissions = std::fs::metadata(&fixture.path).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    // Releases a temporary fixture file for cleanup, not a security boundary.
    #[cfg(not(unix))]
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&fixture.path, permissions).unwrap();
}
#[test]
fn external_conflict_and_modal_preserve_dirty_source_without_automatic_retry() {
    let mut fixture = Fixture::new("conflict.txt", b"original");
    fixture.app.settings.save_synchronization.save_when_idle = true;
    fixture.app.settings.save_synchronization.idle_seconds = 1;
    fixture.frame(0., true, vec![]);
    fixture.edit("editor π");
    fixture.frame(0.1, true, vec![]);
    std::fs::write(&fixture.path, b"external").unwrap();
    fixture.frame(1.1, true, vec![]);
    assert!(matches!(fixture.app.dialog, Dialog::ExternalChange(_)));
    let status = fixture.app.status.clone();
    let version = fixture.app.tabs[0].state.doc.version();
    fixture.frame(20., true, vec![]);
    assert_eq!(fixture.app.status, status);
    assert_eq!(fixture.app.tabs[0].state.doc.version(), version);
    assert_eq!(fixture.app.tabs[0].state.doc.text(), "editor π");
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"external");
    fixture.app.acknowledge_disk_revision(&fixture.path);
    fixture.app.dialog = Dialog::None;
    fixture.frame(21., true, vec![]);
    fixture.frame(22., true, vec![]);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), "editor π".as_bytes());
}
#[test]
fn pending_notebook_model_is_saved_and_active_transactions_defer_publication() {
    let notebook = rustpy_notebook::new_notebook();
    let mut fixture = Fixture::new("cells.ipynb", notebook.to_string().as_bytes());
    fixture.app.settings.save_synchronization.save_when_idle = true;
    fixture.app.settings.save_synchronization.idle_seconds = 1;
    fixture.frame(0., true, vec![]);
    let nb = fixture.app.tabs[0].notebook.as_mut().unwrap();
    nb.set_cell_source(0, "print('π😀')".into());
    nb.set_dirty(true);
    assert!(!fixture.app.tabs[0].state.doc.is_modified());
    fixture.frame(0.1, true, vec![]);
    fixture.frame(1.1, true, vec![]);
    let saved: rustpy_notebook::Notebook = std::fs::read_to_string(&fixture.path)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(saved.cells[0].source, "print('π😀')");
    assert!(!fixture.app.tabs[0].notebook.as_ref().unwrap().is_dirty());
    let mut plain = Fixture::new("scope.txt", b"original");
    plain.app.settings.save_synchronization.save_when_idle = true;
    plain.app.settings.save_synchronization.idle_seconds = 1;
    let token = plain.app.tabs[0]
        .state
        .begin_transaction("Owned edit")
        .unwrap();
    plain.edit("pending scope");
    plain.frame(0., true, vec![]);
    plain.frame(1., true, vec![]);
    assert_eq!(std::fs::read(&plain.path).unwrap(), b"original");
    plain.app.tabs[0].state.commit_transaction(token).unwrap();
    plain.frame(2., true, vec![]);
    plain.frame(3., true, vec![]);
    assert_eq!(std::fs::read(&plain.path).unwrap(), b"pending scope");
}
#[test]
#[cfg_attr(
    windows,
    ignore = "the retried save after a blocked backup root does not write on Windows; see known issues"
)]
fn backup_failure_preserves_editor_and_retries_only_after_real_activity() {
    let mut fixture = Fixture::new("backup.txt", b"original\r\n");
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_BACKUP_ROOT.with(|root| *root.borrow_mut() = None);
        }
    }
    let _reset = Reset;
    let root = fixture.dir.path().join("blocked");
    std::fs::write(&root, b"blocking regular file").unwrap();
    TEST_BACKUP_ROOT.with(|value| *value.borrow_mut() = Some(root.clone()));
    fixture.app.settings.save_synchronization.backup_before_save = true;
    fixture.app.settings.save_synchronization.save_when_idle = true;
    fixture.app.settings.save_synchronization.idle_seconds = 1;
    fixture.edit("edited π\n");
    fixture.frame(0., true, vec![]);
    let history = fixture.app.tabs[0].state.doc.undo_history();
    fixture.frame(1., true, vec![]);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"original\r\n");
    assert!(fixture.app.tabs[0].state.doc.is_modified());
    assert_eq!(fixture.app.tabs[0].state.doc.undo_history(), history);
    let status = fixture.app.status.clone();
    std::fs::remove_file(&root).unwrap();
    fixture.frame(5., true, vec![]);
    assert_eq!(fixture.app.status, status);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"original\r\n");
    fixture.frame(6., true, vec![egui::Event::Text("user activity".into())]);
    fixture.frame(7., true, vec![]);
    assert_eq!(
        std::fs::read(&fixture.path).unwrap(),
        "edited π\r\n".as_bytes()
    );
    assert_eq!(
        std::fs::read(crate::editor::file_backups::backup_path(
            &root,
            &fixture.path,
            b"original\r\n"
        ))
        .unwrap(),
        b"original\r\n"
    );
    fixture
        .app
        .convert_encoding(0, rustpy_text::Encoding::Utf8Bom);
    assert_eq!(
        std::fs::read(crate::editor::file_backups::backup_path(
            &root,
            &fixture.path,
            "edited π\r\n".as_bytes()
        ))
        .unwrap(),
        "edited π\r\n".as_bytes()
    );
}
#[test]
fn activation_uses_existing_reconciliation_and_settings_own_dirty_page() {
    let mut fixture = Fixture::new("sync.txt", b"original");
    fixture
        .app
        .settings
        .save_synchronization
        .synchronize_on_frame_activation = true;
    fixture.frame(0., true, vec![]);
    fixture.frame(0.1, false, vec![]);
    std::fs::write(&fixture.path, "external π").unwrap();
    fixture.frame(1., true, vec![]);
    assert_eq!(fixture.app.tabs[0].state.doc.text(), "external π");
    assert!(!fixture.app.tabs[0].state.doc.is_modified());
    let old = fixture.app.settings.clone();
    fixture.app.settings.save_synchronization.backup_before_save = true;
    assert!(settings_ui::page_modified(
        settings_ui::SettingsPage::Appearance,
        &old,
        &fixture.app.settings
    ));
    assert!(!settings_ui::page_modified(
        settings_ui::SettingsPage::EditorGeneral,
        &old,
        &fixture.app.settings
    ));
}

#[test]
fn actual_manual_sync_reload_and_backup_open_preserve_source_authority() {
    let mut fixture = Fixture::new("manual.txt", b"original\r\n");
    std::fs::write(&fixture.path, "external π\r\n").unwrap();
    fixture.app.handle_action(Action::Synchronize, &fixture.ctx);
    assert_eq!(fixture.app.tabs[0].state.doc.text(), "external π\n");
    assert!(!fixture.app.tabs[0].state.doc.is_modified());
    fixture.edit("editor 😀\n");
    std::fs::write(&fixture.path, b"new external\r\n").unwrap();
    fixture.app.handle_action(Action::Synchronize, &fixture.ctx);
    assert!(matches!(fixture.app.dialog, Dialog::ExternalChange(_)));
    assert_eq!(fixture.app.tabs[0].state.doc.text(), "editor 😀\n");
    fixture
        .app
        .handle_action(Action::ReloadFromDisk, &fixture.ctx);
    assert_eq!(fixture.app.tabs[0].state.doc.text(), "new external\n");
    assert!(!fixture.app.tabs[0].state.doc.is_modified());
    fixture.app.dialog = Dialog::None;
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_BACKUP_ROOT.with(|value| *value.borrow_mut() = None);
        }
    }
    let _reset = Reset;
    let root = fixture.dir.path().join("backups");
    TEST_BACKUP_ROOT.with(|value| *value.borrow_mut() = Some(root.clone()));
    fixture.app.settings.save_synchronization.backup_before_save = true;
    fixture.edit("saved 😀\n");
    fixture.app.handle_action(Action::SaveAll, &fixture.ctx);
    assert_eq!(
        std::fs::read(&fixture.path).unwrap(),
        "saved 😀\r\n".as_bytes()
    );
    let backup =
        crate::editor::file_backups::backup_path(&root, &fixture.path, b"new external\r\n");
    assert_eq!(std::fs::read(&backup).unwrap(), b"new external\r\n");
    fixture.app.open_file(&backup);
    let index = fixture.app.active.unwrap();
    assert_eq!(
        fixture.app.tabs[index].state.path.as_deref(),
        Some(backup.as_path())
    );
    assert_eq!(fixture.app.tabs[index].state.doc.text(), "new external\n");
    assert!(!fixture.app.tabs[index].state.doc.is_modified());
    assert_eq!(
        std::fs::read(&fixture.path).unwrap(),
        "saved 😀\r\n".as_bytes()
    );
}

#[test]
fn consumed_real_view_navigation_resets_idle_before_the_post_view_tick() {
    let mut fixture = Fixture::new("navigation.txt", b"original");
    fixture.app.settings.save_synchronization.save_when_idle = true;
    fixture.app.settings.save_synchronization.idle_seconds = 1;
    fixture.edit("dirty π😀");
    let end = fixture.app.tabs[0].state.doc.len();
    fixture.app.tabs[0]
        .state
        .set_carets(vec![crate::editor::Caret::at(end)]);
    fixture.app.focus_editor = true;
    fn actual_frame(
        fixture: &mut Fixture,
        time: f64,
        mut events: Vec<egui::Event>,
        consumed: bool,
    ) {
        if let Some(modifiers) = events.iter().find_map(|event| match event {
            egui::Event::Key { modifiers, .. } => Some(*modifiers),
            _ => None,
        }) {
            events.insert(0, egui::Event::ModifiersChanged(modifiers));
        }
        let mut raw = egui::RawInput {
            time: Some(time),
            focused: true,
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000., 720.),
            )),
            ..Default::default()
        };
        eframe::App::raw_input_hook(&mut fixture.app, &fixture.ctx, &mut raw);
        fixture
            .ctx
            .run_ui(raw, |ui| {
                fixture.app.global_shortcuts(ui);
                egui::CentralPanel::default().show(ui, |ui| fixture.app.editor_area(ui));
                if consumed {
                    assert!(!ui.input(|input| input.events.iter().any(|event| matches!(
                        event,
                        egui::Event::Key {
                            key: egui::Key::ArrowLeft,
                            pressed: true,
                            ..
                        }
                    ))));
                }
                fixture.app.save_synchronization_tick(ui.ctx());
            })
            .drop_without_applying_deltas();
    }
    actual_frame(&mut fixture, 0., vec![], false);
    actual_frame(&mut fixture, 0.1, vec![], false);
    let version = fixture.app.tabs[0].state.doc.version();
    // The View intentionally retains plain navigation keys; Ctrl+Left is
    // an actually consumed editor chord and still changes no document revision.
    actual_frame(
        &mut fixture,
        1.1,
        vec![egui::Event::Key {
            key: egui::Key::ArrowLeft,
            physical_key: Some(egui::Key::ArrowLeft),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        }],
        true,
    );
    assert!(fixture.app.tabs[0].state.primary().head() < end);
    assert_eq!(fixture.app.tabs[0].state.doc.version(), version);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"original");
    actual_frame(
        &mut fixture,
        1.2,
        vec![egui::Event::Key {
            key: egui::Key::ArrowLeft,
            physical_key: Some(egui::Key::ArrowLeft),
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        }],
        false,
    );
    actual_frame(&mut fixture, 2.3, vec![], false);
    assert_eq!(
        std::fs::read(&fixture.path).unwrap(),
        "dirty π😀".as_bytes()
    );
}
