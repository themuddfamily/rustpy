//! Explicit encoding conversion and disk reinterpretation. Conversion writes
//! validated current text before publishing edits; reload never writes.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_text::{DocumentId, Encoding, SourcePolicy};

#[derive(Clone, Debug)]
pub(super) struct EncodingReloadRequest {
    pub path: PathBuf,
    pub encoding: Encoding,
    pub tab_id: egui::Id,
    pub document: DocumentId,
    pub version: u64,
    pub session: u64,
    pub generation: u64,
    pub revision: Option<rustpy_core::vfs::ContentRevision>,
    pub convert: bool,
}

impl IdeApp {
    pub(super) fn encoding_widget(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.active.filter(|&i| i < self.tabs.len()) else {
            return;
        };
        let encoding = self.tabs[index].state.doc.encoding();
        let mut action = None;
        ui.menu_button(encoding.label(), |ui| {
            ui.label("Convert saves the current editor text; Reload reinterprets disk bytes without writing.");
            for target in [Encoding::Utf8, Encoding::Utf8Bom, Encoding::Latin1, Encoding::Utf16Le, Encoding::Utf16Be, Encoding::Utf16LeNoBom, Encoding::Utf16BeNoBom] {
                ui.horizontal(|ui| {
                    ui.label(target.label());
                    if ui.button("Convert").clicked() { action = Some((target, true)); ui.close(); }
                    if ui.button("Reload").clicked() { action = Some((target, false)); ui.close(); }
                });
            }
        }).response.on_hover_text("File encoding: Convert or Reload");
        if let Some((target, convert)) = action {
            if convert {
                self.convert_encoding(index, target);
            } else {
                self.request_encoding_reload(index, target);
            }
        }
    }
    pub(super) fn encoding_override(&self, path: &Path) -> Result<Option<Encoding>, String> {
        self.encoding_overrides
            .get(path)
            .map(String::as_str)
            .or_else(|| {
                self.settings
                    .editor
                    .preferences
                    .encoding(path, self.project.as_ref().map(|p| p.root.as_path()))
            })
            .map(|key| {
                Encoding::from_key(key)
                    .ok_or_else(|| format!("Unknown saved encoding {key:?} for {}", path.display()))
            })
            .transpose()
    }
    pub(super) fn publish_encoding_override(&mut self, path: &Path, encoding: Encoding) {
        if self
            .encoding_overrides
            .get(path)
            .is_some_and(|key| key == encoding.key())
        {
            return;
        }
        self.encoding_overrides
            .insert(path.to_path_buf(), encoding.key().to_owned());
        self.encoding_generation = self.encoding_generation.wrapping_add(1);
    }
    pub(super) fn convert_encoding(&mut self, i: usize, encoding: Encoding) {
        self.perform_encoding_conversion(i, encoding, false);
    }
    fn perform_encoding_conversion(&mut self, i: usize, encoding: Encoding, confirmed: bool) {
        let Some(tab) = self.tabs.get(i) else { return };
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        if tab.image.is_some()
            || tab.notebook.is_some()
            || tab.state.doc.is_read_only()
            || path.starts_with("<typeshed>")
            || self.is_large_file_placeholder(i)
        {
            self.set_status("This editor cannot convert its file encoding");
            return;
        }
        let plan = match tab
            .state
            .doc
            .prepare_encoding_conversion(encoding, SourcePolicy::for_path(&path))
        {
            Ok(plan) => plan,
            Err(error) => {
                self.set_status(format!("Cannot convert {}: {error}", path.display()));
                return;
            }
        };
        if plan.affects_shebang() && !confirmed {
            let Some(revision) = self.disk_revisions.get(&path).copied().flatten() else {
                self.set_status("Save this file before adding a BOM before its shebang");
                return;
            };
            self.dialog = Dialog::EncodingReload(EncodingReloadRequest {
                path,
                encoding,
                tab_id: tab.view.id,
                document: tab.state.doc.identity(),
                version: tab.state.doc.version(),
                session: self.file_session,
                generation: self.encoding_generation,
                revision: Some(revision),
                convert: true,
            });
            return;
        }
        let before = tab.state.selections();
        let carets: Vec<_> = tab
            .state
            .carets
            .iter()
            .map(|caret| {
                crate::editor::Caret::new(
                    crate::editor::state::map_offset(plan.edits(), caret.sel.anchor, false),
                    crate::editor::state::map_offset(plan.edits(), caret.sel.head, false),
                )
            })
            .collect();
        let after: Vec<_> = carets.iter().map(|caret| caret.sel).collect();
        let baseline = self.disk_revisions.get(&path).copied().flatten();
        let _operation =
            match rustpy_vcs::RepositoryOperation::enter(&path, "Converting file encoding") {
                Ok(operation) => operation,
                Err(error) => {
                    self.set_status(format!("Cannot convert {}: {error}", path.display()));
                    return;
                }
            };
        let revision = match self.checked_editor_write(&path, plan.bytes(), baseline) {
            Ok(revision) => revision,
            Err(error) => {
                self.set_status(format!("Cannot convert {}: {error}", path.display()));
                return;
            }
        };
        // The UI thread cannot change the document between this write and commit.
        self.snapshot(i, "Before encoding conversion");
        let tab = &mut self.tabs[i];
        if let Err(error) = tab
            .state
            .doc
            .commit_encoding_conversion(plan, &before, &after)
        {
            self.set_status(format!(
                "Cannot publish conversion of {}: {error}",
                path.display()
            ));
            return;
        }
        tab.state.set_carets(carets);
        tab.state.doc.mark_saved();
        self.disk_revisions.insert(path.clone(), Some(revision));
        self.bind_file_permissions(i);
        self.publish_encoding_override(&path, encoding);
        self.reconcile_document_markers();
        self.run_file_watchers(&path);
        if let Some(root) = self.project.as_ref().map(|p| p.root.clone()) {
            self.deploy.on_saved(&root, &path, true);
        }
        if let Some(project) = &mut self.project {
            project.git_refreshed = -100.0;
        }
        self.save_workspace();
        self.set_status(format!(
            "Converted {} to {}",
            path.display(),
            encoding.label()
        ));
    }
    pub(super) fn request_encoding_reload(&mut self, i: usize, encoding: Encoding) {
        let Some(tab) = self.tabs.get(i) else { return };
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        if tab.image.is_some() || tab.notebook.is_some() || path.starts_with("<typeshed>") {
            self.set_status("This editor cannot reload its file encoding");
            return;
        }
        let request = EncodingReloadRequest {
            path,
            encoding,
            tab_id: tab.view.id,
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            session: self.file_session,
            generation: self.encoding_generation,
            revision: None,
            convert: false,
        };
        if std::fs::metadata(&request.path)
            .is_ok_and(|metadata| metadata.len() > background::LARGE_FILE_GATE_BYTES)
        {
            self.prepare_large_encoding_reload(request);
            return;
        }
        match self
            .file_vfs()
            .read(&request.path, self.normal_editor_read_limits())
        {
            Ok(snapshot) => {
                let mut request = request;
                request.revision = Some(snapshot.revision);
                self.finish_encoding_reload_request(request);
            }
            Err(error) => {
                self.set_status(format!("Cannot reload {}: {error}", request.path.display()))
            }
        }
    }
    pub(super) fn finish_encoding_reload_request(&mut self, request: EncodingReloadRequest) {
        let Some(index) = self.encoding_request_index(&request) else {
            return;
        };
        if self.tabs[index].state.doc.is_modified() {
            self.dialog = Dialog::EncodingReload(request);
        } else {
            self.confirm_encoding_reload(request);
        }
    }

    pub(super) fn encoding_request_index(&self, request: &EncodingReloadRequest) -> Option<usize> {
        if request.session != self.file_session || request.generation != self.encoding_generation {
            return None;
        }
        self.tabs.iter().position(|tab| {
            tab.view.id == request.tab_id
                && tab.state.path.as_deref() == Some(request.path.as_path())
                && tab.state.doc.identity() == request.document
                && tab.state.doc.version() == request.version
        })
    }
    pub(super) fn confirm_encoding_reload(&mut self, request: EncodingReloadRequest) {
        if self.encoding_request_index(&request).is_none() {
            self.set_status("Encoding reload cancelled: the editor changed");
            return;
        }
        if request.convert {
            if self.disk_revisions.get(&request.path).copied().flatten() != request.revision {
                self.set_status("Encoding conversion cancelled: the disk baseline changed");
                return;
            }
            let index = self.encoding_request_index(&request).unwrap();
            self.perform_encoding_conversion(index, request.encoding, true);
        } else {
            self.start_encoding_reload(request);
        }
    }
    pub(super) fn encoding_reload_dialog(
        &mut self,
        ctx: &egui::Context,
        request: EncodingReloadRequest,
    ) {
        let (mut discard, mut cancel) = (false, false);
        let mut open = true;
        egui::Window::new("Reload File Encoding").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            if request.convert { ui.label("Adding a UTF-8 BOM before #! can prevent direct script execution. Convert and save the current editor text anyway?"); }
            else { ui.label(format!("Reload {} as {}? Unsaved editor changes will be discarded.", request.path.display(), request.encoding.label())); }
            ui.horizontal(|ui| { discard = ui.button(if request.convert { "Convert and save" } else { "Discard changes and reload" }).clicked(); cancel = ui.button("Cancel").clicked(); });
        });
        if discard {
            self.confirm_encoding_reload(request);
        } else if open && !cancel {
            self.dialog = Dialog::EncodingReload(request);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str, bytes: &[u8]) -> (tempfile::TempDir, IdeApp, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
        assert_eq!(app.project.as_ref().unwrap().root, directory.path());
        assert!(app.tabs.is_empty());
        app.open_file(&path);
        assert_eq!(app.tabs[0].state.path.as_deref(), Some(path.as_path()));
        (directory, app, path)
    }
    fn reload_request(app: &mut IdeApp, encoding: Encoding) -> EncodingReloadRequest {
        let end = app.tabs[0].state.doc.len();
        app.tabs[0].state.apply_text_edits(
            vec![(TextRange::empty(end), " dirty".into())],
            "Owned dirty edit",
        );
        app.request_encoding_reload(0, encoding);
        let Dialog::EncodingReload(request) = std::mem::replace(&mut app.dialog, Dialog::None)
        else {
            panic!("dirty reload confirmation")
        };
        request
    }
    #[test]
    fn convert_writes_current_dirty_text_and_one_atomic_cookie_format_undo() {
        let (_directory, mut app, path) =
            fixture("m.py", b"#!/usr/bin/python3\n# coding: latin-1\nx='\xe9'\n");
        let end = app.tabs[0].state.doc.len();
        app.tabs[0].state.apply_text_edits(
            vec![(TextRange::empty(end), "# current 😀\n".into())],
            "Current unsaved text",
        );
        let before = app.tabs[0].state.doc.text();
        let carets = app.tabs[0].state.carets.clone();
        app.convert_encoding(0, Encoding::Utf8);
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Utf8);
        assert!(!app.tabs[0].state.doc.is_modified());
        let bytes = std::fs::read(&path).unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .starts_with("#!/usr/bin/python3\n# coding: utf-8\n"));
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("# current 😀"));
        assert_eq!(
            app.disk_revisions[&path],
            Some(rustpy_core::vfs::ContentRevision::of(&bytes))
        );
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), before);
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Latin1);
        assert_eq!(app.tabs[0].state.carets, carets);
        assert!(app.tabs[0].state.doc.is_modified());
        assert!(app.tabs[0].state.redo());
        assert_eq!(
            app.tabs[0]
                .state
                .doc
                .to_bytes_with_policy(SourcePolicy::Python)
                .unwrap(),
            bytes
        );
        assert!(!app.tabs[0].state.doc.is_modified());
    }
    #[test]
    fn ordinary_save_after_undo_encoding_conversion_decodes_actual_disk_baseline() {
        let (_directory, mut app, path) =
            fixture("m.py", "#!/usr/bin/python3\r\nx='é'\r\n".as_bytes());
        assert_eq!(
            app.settings.editor.strip_trailing_spaces_on_save,
            rustpy_settings::StripTrailing::ModifiedLines
        );
        let end = app.tabs[0].state.doc.len();
        app.tabs[0].state.apply_text_edits(
            vec![(TextRange::empty(end), "# dirty é\n".into())],
            "Unsaved current text",
        );
        let before_conversion = app.tabs[0].state.doc.text();
        app.convert_encoding(0, Encoding::Latin1);
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Latin1);
        assert!(std::fs::read(&path)
            .unwrap()
            .windows(b"coding: latin-1".len())
            .any(|window| window == b"coding: latin-1"));
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), before_conversion);
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Utf8);
        assert!(!app.tabs[0].state.doc.text().contains("coding:"));
        assert!(app.tabs[0].state.doc.is_modified());
        assert!(
            app.save_tab(0),
            "ordinary Save after Undo: {:?}",
            app.status
        );
        let expected = "#!/usr/bin/python3\r\nx='é'\r\n# dirty é\r\n".as_bytes();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Utf8);
        assert_eq!(app.encoding_override(&path).unwrap(), Some(Encoding::Utf8));
        assert!(!app.tabs[0].state.doc.is_modified());
        assert_eq!(
            app.disk_revisions[&path],
            Some(rustpy_core::vfs::ContentRevision::of(expected))
        );
    }
    #[test]
    fn generic_save_after_conversion_undo_keeps_unchanged_latin1_trailing_spaces() {
        let (_directory, mut app, path) = fixture("m.txt", b"caf\xe9=1  \r\nx=1  \r\n# end\r\n");
        let end = app.tabs[0].state.doc.len();
        app.tabs[0].state.apply_text_edits(
            vec![(TextRange::empty(end), "# dirty é\n".into())],
            "Unsaved current text",
        );
        let before_conversion = app.tabs[0].state.doc.text();
        app.convert_encoding(0, Encoding::Utf8);
        assert_eq!(app.encoding_override(&path).unwrap(), Some(Encoding::Utf8));
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Latin1);
        assert_eq!(app.tabs[0].state.doc.text(), before_conversion);
        assert!(app.save_tab(0), "generic Save after Undo: {:?}", app.status);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"caf\xe9=1  \r\nx=1  \r\n# end\r\n# dirty \xe9\r\n"
        );
        assert_eq!(app.tabs[0].state.doc.text(), before_conversion);
        assert_eq!(
            app.encoding_override(&path).unwrap(),
            Some(Encoding::Latin1)
        );
        assert!(!app.tabs[0].state.doc.is_modified());
    }
    #[test]
    fn conversion_encode_and_revision_failures_preserve_all_editor_state() {
        let (_directory, mut app, path) = fixture("m.txt", "x=😀\n".as_bytes());
        let text = app.tabs[0].state.doc.text();
        let version = app.tabs[0].state.doc.version();
        let history = app.tabs[0].state.doc.undo_history();
        let carets = app.tabs[0].state.carets.clone();
        let revision = app.disk_revisions[&path];
        let bytes = std::fs::read(&path).unwrap();
        app.convert_encoding(0, Encoding::Latin1);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert_eq!(app.tabs[0].state.doc.undo_history(), history);
        assert_eq!(app.tabs[0].state.carets, carets);
        assert_eq!(app.disk_revisions[&path], revision);
        assert!(app.encoding_overrides.is_empty());
        std::fs::write(&path, b"external\n").unwrap();
        app.convert_encoding(0, Encoding::Utf8Bom);
        assert_eq!(std::fs::read(&path).unwrap(), b"external\n");
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Utf8);
        assert_eq!(app.tabs[0].state.doc.undo_history(), history);
        assert_eq!(app.disk_revisions[&path], revision);
    }
    #[test]
    fn bom_shebang_conversion_requires_confirmation_and_cancel_is_inert() {
        let (_directory, mut app, path) = fixture("m.py", b"#!/usr/bin/python3\nx=1\n");
        let bytes = std::fs::read(&path).unwrap();
        let version = app.tabs[0].state.doc.version();
        app.convert_encoding(0, Encoding::Utf8Bom);
        let Dialog::EncodingReload(request) = std::mem::replace(&mut app.dialog, Dialog::None)
        else {
            panic!("BOM warning")
        };
        assert!(request.convert);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        // A stale confirmation cannot apply to a changed editor.
        app.tabs[0].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "# changed\n".into())],
            "Changed meanwhile",
        );
        app.confirm_encoding_reload(request);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    #[test]
    fn selected_reload_never_writes_and_retains_choice_through_watcher_and_reopen() {
        let (_directory, mut app, path) = fixture("m.txt", "é\nsecond\n".as_bytes());
        app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
            TextSize::new(3),
            TextSize::new(9),
        )]);
        app.bookmarks.push(Bookmark {
            path: path.clone(),
            line: 2,
            mnemonic: Some('a'),
            description: "owned bookmark".into(),
            kind: rustpy_settings::BookmarkKind::Line,
            list: String::new(),
        });
        app.reconcile_document_markers();
        let request = reload_request(&mut app, Encoding::Latin1);
        let bytes = std::fs::read(&path).unwrap();
        let snapshot = app
            .file_vfs()
            .read(&path, app.editor_read_limits())
            .unwrap();
        let document =
            Document::from_bytes_with_policy(&bytes, SourcePolicy::Generic, Some(Encoding::Latin1))
                .unwrap();
        app.apply_encoding_reload(request, Ok((document, snapshot)));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(app.tabs[0].state.doc.text().starts_with("Ã©\n"));
        assert!(!app.tabs[0].state.doc.is_modified());
        assert!(!app.tabs[0].state.undo());
        app.reconcile_document_markers();
        assert_eq!(app.bookmarks[0].line, 2);
        assert_eq!(
            app.tabs[0]
                .state
                .doc
                .offset_to_position(app.tabs[0].state.carets[0].sel.anchor),
            rustpy_core::Position::new(1, 0)
        );
        std::fs::write(&path, "é\nchanged\n".as_bytes()).unwrap();
        app.external_changes.push(path.clone());
        app.reconcile_external_changes();
        assert!(app.tabs[0].state.doc.text().starts_with("Ã©\n"));
        app.close_tab(0);
        app.open_file(&path);
        assert!(app.tabs[0].state.doc.text().starts_with("Ã©\n"));
        let workspace = Workspace::load(app.project.as_ref().unwrap().root.as_path());
        assert!(workspace
            .file_encodings
            .iter()
            .any(|entry| entry.path == path && entry.encoding == Encoding::Latin1.key()));
    }
    #[test]
    fn reload_errors_cancel_and_stale_success_or_error_preserve_state() {
        let (_directory, mut app, path) = fixture("m.txt", b"original\n");
        let request = reload_request(&mut app, Encoding::Utf16LeNoBom);
        let text = app.tabs[0].state.doc.text();
        let history = app.tabs[0].state.doc.undo_history();
        let revision = app.disk_revisions[&path];
        let bytes = std::fs::read(&path).unwrap();
        app.apply_encoding_reload(request.clone(), Err(rustpy_core::TaskError::Cancelled));
        app.apply_encoding_reload(
            request.clone(),
            Err(rustpy_core::TaskError::Failed("malformed fixture".into())),
        );
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert_eq!(app.tabs[0].state.doc.undo_history(), history);
        assert_eq!(app.disk_revisions[&path], revision);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        app.encoding_generation = app.encoding_generation.wrapping_add(1);
        let status = app.status.clone();
        let snapshot = app
            .file_vfs()
            .read(&path, app.editor_read_limits())
            .unwrap();
        app.apply_encoding_reload(request.clone(), Ok((Document::new("stale"), snapshot)));
        app.apply_encoding_reload(
            request,
            Err(rustpy_core::TaskError::Failed("stale error".into())),
        );
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert_eq!(app.status, status);
        assert!(app.encoding_overrides.is_empty());
    }
    #[test]
    fn actual_background_reload_rejects_raced_disk_and_accepts_bomless_utf16() {
        let (_directory, mut app, path) = fixture("m.txt", b"abcd");
        let request = reload_request(&mut app, Encoding::Latin1);
        std::fs::write(&path, b"raced disk").unwrap();
        app.confirm_encoding_reload(request);
        let context = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !app
            .status
            .as_ref()
            .is_some_and(|(message, _)| message.contains("changed after reload"))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "reload worker result: {:?}",
                app.status
            );
            app.poll_background(&context);
            std::thread::yield_now();
        }
        assert!(app.tabs[0].state.doc.text().contains(" dirty"));
        assert!(app.encoding_overrides.is_empty());
        let bytes = Encoding::Utf16BeNoBom
            .encode_with_policy("é\n", SourcePolicy::Generic)
            .unwrap();
        std::fs::write(&path, &bytes).unwrap();
        app.disk_revisions.insert(
            path.clone(),
            Some(rustpy_core::vfs::ContentRevision::of(&bytes)),
        );
        let request = reload_request(&mut app, Encoding::Utf16BeNoBom);
        app.confirm_encoding_reload(request);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.tabs[0].state.doc.encoding() != Encoding::Utf16BeNoBom {
            assert!(
                std::time::Instant::now() < deadline,
                "selected worker result: {:?}",
                app.status
            );
            app.poll_background(&context);
            std::thread::yield_now();
        }
        assert_eq!(app.tabs[0].state.doc.text(), "é\n");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Utf16BeNoBom);
    }
    #[test]
    fn reload_accepts_current_external_bytes_but_rejects_changes_after_confirmation() {
        let (_directory, mut app, path) = fixture("m.txt", b"old disk");
        std::fs::write(&path, "é latest disk".as_bytes()).unwrap();
        let request = reload_request(&mut app, Encoding::Latin1);
        let snapshot = app
            .file_vfs()
            .read(&path, app.editor_read_limits())
            .unwrap();
        assert_eq!(request.revision, Some(snapshot.revision));
        assert_ne!(request.revision, app.disk_revisions[&path]);
        let document = Document::from_bytes_with_policy(
            &snapshot.bytes,
            SourcePolicy::Generic,
            Some(Encoding::Latin1),
        )
        .unwrap();
        app.apply_encoding_reload(request, Ok((document, snapshot)));
        assert_eq!(app.tabs[0].state.doc.text(), "Ã© latest disk");
        assert_eq!(std::fs::read(&path).unwrap(), "é latest disk".as_bytes());
        let request = reload_request(&mut app, Encoding::Utf8);
        let text = app.tabs[0].state.doc.text();
        std::fs::write(&path, b"later race").unwrap();
        let snapshot = app
            .file_vfs()
            .read(&path, app.editor_read_limits())
            .unwrap();
        app.apply_encoding_reload(request, Ok((Document::new("later race"), snapshot)));
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert!(app.tabs[0].state.doc.is_modified());
    }
    #[test]
    fn explicit_reload_loads_large_placeholder_with_selected_codec_off_ui_thread() {
        let mut bytes = vec![b'x'; background::LARGE_FILE_GATE_BYTES as usize + 1];
        bytes[..7].copy_from_slice("é\na\nb\n".as_bytes());
        let (_directory, mut app, path) = fixture("m.txt", &bytes);
        assert!(app.is_large_file_placeholder(0));
        app.request_encoding_reload(0, Encoding::Latin1);
        let context = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while app.is_large_file_placeholder(0) {
            assert!(
                std::time::Instant::now() < deadline,
                "large reload result: {:?}",
                app.status
            );
            app.poll_background(&context);
            std::thread::yield_now();
        }
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Latin1);
        assert!(app.tabs[0].state.doc.line_text(0).starts_with("Ã©"));
        assert!(!app.tabs[0].state.doc.is_read_only());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    #[test]
    fn successful_editorconfig_save_updates_explicit_choice_before_reopen() {
        let (directory, mut app, path) = fixture("m.txt", "é\n".as_bytes());
        std::fs::write(
            directory.path().join(".editorconfig"),
            "root=true\n[*]\ncharset=utf-8\n",
        )
        .unwrap();
        rustpy_settings::editorconfig::invalidate_cache();
        let request = reload_request(&mut app, Encoding::Latin1);
        let snapshot = app
            .file_vfs()
            .read(&path, app.editor_read_limits())
            .unwrap();
        let document = Document::from_bytes_with_policy(
            &snapshot.bytes,
            SourcePolicy::Generic,
            Some(Encoding::Latin1),
        )
        .unwrap();
        app.apply_encoding_reload(request, Ok((document, snapshot)));
        let text = app.tabs[0].state.doc.text();
        assert_eq!(text, "Ã©\n");
        assert_eq!(
            app.encoding_override(&path).unwrap(),
            Some(Encoding::Latin1)
        );
        assert!(app.save_tab(0));
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Utf8);
        assert_eq!(app.encoding_override(&path).unwrap(), Some(Encoding::Utf8));
        app.close_tab(0);
        app.open_file(&path);
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Utf8);
        assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
    }
    #[test]
    fn failed_editorconfig_save_does_not_publish_new_override() {
        let (directory, mut app, path) = fixture("m.txt", "😀\n".as_bytes());
        app.publish_encoding_override(&path, Encoding::Utf8);
        std::fs::write(
            directory.path().join(".editorconfig"),
            "root=true\n[*]\ncharset=latin1\n",
        )
        .unwrap();
        rustpy_settings::editorconfig::invalidate_cache();
        let generation = app.encoding_generation;
        let bytes = std::fs::read(&path).unwrap();
        let version = app.tabs[0].state.doc.version();
        assert!(!app.save_tab(0));
        assert_eq!(app.encoding_override(&path).unwrap(), Some(Encoding::Utf8));
        assert_eq!(app.encoding_generation, generation);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(app.tabs[0].state.doc.version(), version);
    }
    #[test]
    fn unknown_saved_choice_and_readonly_conversion_are_visible_and_inert() {
        let (_directory, mut app, path) = fixture("m.txt", b"original\n");
        app.encoding_overrides
            .insert(path.clone(), "future-codec".into());
        assert!(app.encoding_override(&path).is_err());
        let version = app.tabs[0].state.doc.version();
        app.reload_tab_from_disk(0);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        app.tabs[0].state.doc.set_read_only(true);
        app.convert_encoding(0, Encoding::Utf8Bom);
        assert_eq!(std::fs::read(&path).unwrap(), b"original\n");
        assert_eq!(app.encoding_overrides[&path], "future-codec");
    }
}
