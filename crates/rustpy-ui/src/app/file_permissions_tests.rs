//! Connected permission action/prompt/edit/save/reload regressions.
use super::*;

struct Fixture {
    directory: tempfile::TempDir,
    path: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(metadata) = std::fs::metadata(&self.path) {
            let mut permissions = metadata.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(permissions.mode() | 0o200);
            }
            #[cfg(not(unix))]
            {
                let readonly = false;
                permissions.set_readonly(readonly);
            }
            let _ = std::fs::set_permissions(&self.path, permissions);
        }
    }
}
fn disk_read_only(path: &Path, readonly: bool) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(if readonly {
            permissions.mode() & !0o222
        } else {
            permissions.mode() | 0o200
        });
    }
    #[cfg(not(unix))]
    permissions.set_readonly(readonly);
    std::fs::set_permissions(path, permissions).unwrap();
}
fn fixture(relative: &str, readonly: bool) -> (Fixture, IdeApp) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "value = 'π🙂'\n").unwrap();
    disk_read_only(&path, readonly);
    let mut settings = Settings::default();
    settings.trust_project(directory.path());
    let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
    app.open_file(&path);
    assert_eq!(app.active, Some(0));
    (Fixture { directory, path }, app)
}
fn answer(app: &mut IdeApp, confirm: bool) {
    let Dialog::MakeWritable { document, request } =
        std::mem::replace(&mut app.dialog, Dialog::None)
    else {
        panic!("expected bound make-writable prompt")
    };
    app.answer_make_writable(document, request, confirm);
}
fn editor_proof(
    app: &IdeApp,
) -> (
    String,
    u64,
    rustpy_text::DocumentId,
    rustpy_text::HistoryId,
    bool,
    Vec<rustpy_text::Selection>,
) {
    let state = &app.tabs[0].state;
    (
        state.doc.text(),
        state.doc.version(),
        state.doc.identity(),
        state.doc.current_history(),
        state.doc.is_modified(),
        state.selections(),
    )
}

#[test]
fn disk_readonly_open_action_cancel_confirm_edit_and_save_use_one_bound_flow() {
    let (fixture, mut app) = fixture("file.py", true);
    let ctx = egui::Context::default();
    assert!(app.tabs[0].state.doc.is_read_only());
    let before = editor_proof(&app);
    app.handle_action(Action::ToggleReadOnly, &ctx);
    answer(&mut app, false);
    assert_eq!(editor_proof(&app), before);
    assert!(app.tabs[0].state.doc.is_read_only());
    assert!(std::fs::metadata(&fixture.path)
        .unwrap()
        .permissions()
        .readonly());
    app.handle_action(Action::MakeWritable, &ctx);
    answer(&mut app, true);
    assert_eq!(editor_proof(&app), before);
    assert!(!app.tabs[0].state.doc.is_read_only());
    assert!(!std::fs::metadata(&fixture.path)
        .unwrap()
        .permissions()
        .readonly());
    app.play_macro(vec!["text:# edited\n".into()], &ctx);
    assert!(app.tabs[0].state.doc.is_modified());
    app.handle_action(Action::Save, &ctx);
    assert!(!app.tabs[0].state.doc.is_modified());
    assert_eq!(
        std::fs::read_to_string(&fixture.path).unwrap(),
        "# edited\nvalue = 'π🙂'\n"
    );
    assert!(
        app.refresh_file_permissions(0),
        "our atomic Save rebinds the new inode"
    );
    app.handle_action(Action::ToggleReadOnly, &ctx);
    assert!(app.tabs[0].state.doc.is_read_only());
    assert!(std::fs::metadata(&fixture.path)
        .unwrap()
        .permissions()
        .readonly());
}

#[test]
fn refused_real_edit_opens_prompt_and_cancel_keeps_text_history_and_disk() {
    let (fixture, mut app) = fixture("file.py", true);
    let before = editor_proof(&app);
    let bytes = std::fs::read(&fixture.path).unwrap();
    let opts = app.edit_options();
    app.tabs[0].state.insert_text("refused", &opts);
    assert!(app.tabs[0].state.read_only_refused);
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.after_editor(ui, 0));
    output.textures_delta.clear();
    assert!(matches!(app.dialog, Dialog::MakeWritable { .. }));
    answer(&mut app, false);
    assert_eq!(editor_proof(&app), before);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), bytes);
}

#[test]
fn external_attribute_refresh_guards_actual_dispatch_and_can_release_only_os_guard() {
    let (fixture, mut app) = fixture("file.py", false);
    let before = editor_proof(&app);
    disk_read_only(&fixture.path, true);
    assert!(app.dispatch_editor_action(Action::Delete));
    assert_eq!(editor_proof(&app), before);
    assert!(app.tabs[0].state.doc.is_read_only());
    disk_read_only(&fixture.path, false);
    assert!(app.refresh_file_permissions(0));
    assert!(!app.tabs[0].state.doc.is_read_only());
    app.tabs[0].state.doc.set_read_only(true); // independent explicit model guard
    assert!(app.refresh_file_permissions(0));
    assert!(app.refresh_file_permissions(0));
    assert!(app.tabs[0].state.doc.is_read_only());
    app.handle_action(Action::MakeWritable, &egui::Context::default());
    assert!(matches!(app.dialog, Dialog::None));
}

#[test]
fn stale_prompt_after_actual_reload_cannot_unlock_new_document() {
    let (fixture, mut app) = fixture("file.py", true);
    app.handle_action(Action::MakeWritable, &egui::Context::default());
    let old = app.tabs[0].state.doc.identity();
    let version = app.tabs[0].state.doc.version();
    let tab = app.tabs[0].view.id;
    let snapshot = app
        .file_vfs()
        .read(&fixture.path, app.editor_read_limits())
        .unwrap();
    app.apply_disk_reload(
        fixture.path.clone(),
        version,
        tab,
        app.file_session,
        background::IfModified::Discard,
        Ok((Document::from_bytes(&snapshot.bytes).unwrap(), snapshot)),
    );
    assert_ne!(app.tabs[0].state.doc.identity(), old);
    answer(&mut app, true);
    assert!(app.tabs[0].state.doc.is_read_only());
    assert!(std::fs::metadata(&fixture.path)
        .unwrap()
        .permissions()
        .readonly());
    assert!(app.status.as_ref().unwrap().0.contains("original document"));
}

#[test]
fn actual_async_reload_rebinds_permissions_and_rejects_stale_delivery() {
    let (fixture, mut app) = fixture("file.py", false);
    let version = app.tabs[0].state.doc.version();
    let tab = app.tabs[0].view.id;
    disk_read_only(&fixture.path, true);
    let snapshot = app
        .file_vfs()
        .read(&fixture.path, app.editor_read_limits())
        .unwrap();
    let result = || {
        Ok((
            Document::from_bytes(&snapshot.bytes).unwrap(),
            snapshot.clone(),
        ))
    };
    app.apply_disk_reload(
        fixture.path.clone(),
        version,
        tab,
        app.file_session.wrapping_add(1),
        background::IfModified::Discard,
        result(),
    );
    assert!(!app.tabs[0].state.doc.is_read_only());
    app.apply_disk_reload(
        fixture.path.clone(),
        version,
        tab,
        app.file_session,
        background::IfModified::Discard,
        result(),
    );
    assert!(app.tabs[0].state.doc.is_read_only());
    app.handle_action(Action::MakeWritable, &egui::Context::default());
    answer(&mut app, true);
    assert!(!app.tabs[0].state.doc.is_read_only());
}

#[test]
fn reader_sync_never_unlocks_os_guard_even_when_library_protection_is_disabled() {
    let (fixture, mut app) = fixture("site-packages/lib.py", true);
    assert!(crate::editor::doc_render::library_protected(
        &app.tabs[0].state,
        &app.settings.editor
    ));
    app.settings.editor.reader_mode.protect_library_sources = false;
    assert!(app.refresh_file_permissions(0));
    crate::editor::doc_render::sync(&mut app.tabs[0].state, &app.settings.editor);
    assert!(app.tabs[0].state.doc.is_read_only());
    app.handle_action(Action::MakeWritable, &egui::Context::default());
    answer(&mut app, true);
    assert!(!std::fs::metadata(&fixture.path)
        .unwrap()
        .permissions()
        .readonly());
    crate::editor::doc_render::sync(&mut app.tabs[0].state, &app.settings.editor);
    assert!(!app.tabs[0].state.doc.is_read_only());
}

#[test]
fn protected_library_requires_confirmation_and_remains_unlocked_after_refresh() {
    let (fixture, mut app) = fixture("site-packages/lib.py", false);
    assert!(app.tabs[0].state.doc.is_read_only());
    app.handle_action(Action::ToggleReadOnly, &egui::Context::default());
    assert!(!std::fs::metadata(&fixture.path)
        .unwrap()
        .permissions()
        .readonly());
    answer(&mut app, true);
    assert!(!app.tabs[0].state.doc.is_read_only());
    app.bind_file_permissions(0);
    crate::editor::doc_render::sync(&mut app.tabs[0].state, &app.settings.editor);
    assert!(!app.tabs[0].state.doc.is_read_only());
}

#[test]
fn binary_viewer_and_virtual_stub_actions_cannot_unlock_or_change_attributes() {
    let (fixture, mut app) = fixture("file.py", false);
    let binary = fixture.directory.path().join("file.bin");
    std::fs::write(&binary, b"binary\0bytes").unwrap();
    app.open_file(&binary);
    let binary_index = app.active.unwrap();
    assert!(app.tabs[binary_index].state.doc.is_read_only());
    app.handle_action(Action::MakeWritable, &egui::Context::default());
    assert!(matches!(app.dialog, Dialog::None));
    assert!(!std::fs::metadata(&binary).unwrap().permissions().readonly());
    assert!(app.tabs[binary_index].state.doc.is_read_only());
    let mut state = EditorState::new(
        Document::new("..."),
        Some(PathBuf::from("<typeshed>/virtual.pyi")),
    );
    state.doc.set_read_only(true);
    app.push_tab(state);
    app.handle_action(Action::ToggleReadOnly, &egui::Context::default());
    assert!(matches!(app.dialog, Dialog::None));
    assert!(app.tabs.last().unwrap().state.doc.is_read_only());
    assert_eq!(std::fs::read(&binary).unwrap(), b"binary\0bytes");
}

#[test]
fn failed_confirmation_retains_readonly_and_never_replays_refused_input() {
    let (fixture, mut app) = fixture("file.py", true);
    app.handle_action(Action::MakeWritable, &egui::Context::default());
    let before = editor_proof(&app);
    disk_read_only(&fixture.path, false); // changes the prompt's observed subject
    answer(&mut app, true);
    assert_eq!(editor_proof(&app), before);
    assert!(app.tabs[0].state.doc.is_read_only());
    assert!(app.status.as_ref().unwrap().0.contains("stale"));
    assert_eq!(
        std::fs::read_to_string(&fixture.path).unwrap(),
        "value = 'π🙂'\n"
    );
}

#[test]
fn actual_image_viewer_cannot_unlock_or_change_disk_attribute() {
    let (fixture, mut app) = fixture("file.py", false);
    let path = fixture.directory.path().join("picture.png");
    image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]))
        .save(&path)
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    app.open_file(&path);
    let index = app.active.unwrap();
    assert!(app.tabs[index].image.is_some());
    app.handle_action(Action::ToggleReadOnly, &egui::Context::default());
    assert!(matches!(app.dialog, Dialog::None));
    assert!(app.tabs[index].state.doc.is_read_only());
    assert!(!std::fs::metadata(&path).unwrap().permissions().readonly());
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn accepted_conversion_and_both_synchronous_reload_paths_rebind_permissions() {
    for explicit_reload in [false, true] {
        let (fixture, mut app) = fixture("file.py", false);
        app.convert_encoding(0, rustpy_text::Encoding::Utf8Bom);
        assert!(
            app.refresh_file_permissions(0),
            "accepted conversion: {:?}",
            app.status
        );
        assert!(app.tabs[0].state.undo());
        assert!(
            app.save_tab(0),
            "Save after conversion Undo: {:?}",
            app.status
        );
        let replacement = fixture.directory.path().join("replacement.py");
        std::fs::write(&replacement, "changed = 'é'\n").unwrap();
        disk_read_only(&replacement, true);
        std::fs::remove_file(&fixture.path).unwrap();
        std::fs::rename(&replacement, &fixture.path).unwrap();
        if explicit_reload {
            app.reload_tab_from_disk(0);
        } else {
            app.external_changes.push(fixture.path.clone());
            app.reconcile_external_changes();
        }
        assert_eq!(app.tabs[0].state.doc.text(), "changed = 'é'\n");
        assert!(
            app.refresh_file_permissions(0),
            "accepted reload: {:?}",
            app.status
        );
        assert!(app.tabs[0].state.doc.is_read_only());
        app.handle_action(Action::MakeWritable, &egui::Context::default());
        answer(&mut app, true);
        assert!(!app.tabs[0].state.doc.is_read_only());
    }
}

#[test]
fn acknowledged_missing_baseline_allows_recreation_but_not_external_replacement() {
    let (fixture, mut app) = fixture("file.py", false);
    let original = std::fs::read(&fixture.path).unwrap();
    std::fs::remove_file(&fixture.path).unwrap();
    assert!(
        !app.save_tab(0),
        "missing existing file still needs explicit Keep"
    );
    assert!(!fixture.path.exists());
    app.external_changes.push(fixture.path.clone());
    app.reconcile_external_changes();
    assert!(matches!(app.dialog, Dialog::ExternalChange(_)));
    app.acknowledge_disk_revision(&fixture.path);
    app.dialog = Dialog::None;
    assert!(
        app.save_tab(0),
        "acknowledged missing recreation: {:?}",
        app.status
    );
    assert_eq!(std::fs::read(&fixture.path).unwrap(), original);
    assert!(app.refresh_file_permissions(0));
    let replacement = fixture.directory.path().join("unaccepted.py");
    std::fs::write(&replacement, b"external = 1\n").unwrap();
    std::fs::remove_file(&fixture.path).unwrap();
    std::fs::rename(&replacement, &fixture.path).unwrap();
    assert!(!app.save_tab(0));
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"external = 1\n");
}
