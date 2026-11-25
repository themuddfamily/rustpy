//! Disk revisions shared with semantic analysis. A Keep decision acknowledges
//! the displayed revision, never a later read of the same path.
use super::*;
use rustpy_core::vfs::{ReadLimits, Vfs};
use std::sync::atomic::Ordering;
impl IdeApp {
    pub(super) fn file_vfs(&self) -> Arc<Vfs> {
        self.analysis_handle()
            .map(|a| a.vfs.clone())
            .unwrap_or_else(|| self.vfs.clone())
    }
    pub(super) fn editor_read_limits(&self) -> ReadLimits {
        ReadLimits {
            max_file_bytes: 256 * 1024 * 1024,
            max_cached_bytes: 32 * 1024 * 1024,
            max_cached_files: 128,
        }
    }
    pub(super) fn normal_editor_read_limits(&self) -> ReadLimits {
        ReadLimits {
            max_file_bytes: background::LARGE_FILE_GATE_BYTES as usize,
            ..self.editor_read_limits()
        }
    }
    pub(super) fn acknowledge_disk_revision(&mut self, path: &Path) {
        if let Some(observed) = self.observed_revisions.remove(path) {
            self.disk_revisions.insert(path.to_path_buf(), observed);
        }
        if !self.external_changes.iter().any(|p| p == path) {
            self.external_changes.push(path.to_path_buf());
        }
    }
    pub(super) fn sync_external_file_watches(&mut self) {
        use notify::Watcher;
        let mut wanted: std::collections::HashSet<PathBuf> = self
            .tabs
            .iter()
            .filter_map(|t| t.state.path.as_ref())
            .filter(|p| !p.starts_with("<typeshed>"))
            .filter_map(|p| p.parent())
            .filter(|p| {
                !self
                    .project
                    .as_ref()
                    .is_some_and(|project| p.starts_with(&project.root))
                    && !self
                        .project_index
                        .watched_roots
                        .iter()
                        .any(|root| p.starts_with(root))
            })
            .map(Path::to_path_buf)
            .collect();
        wanted.extend(
            self.native_skeleton_paths()
                .iter()
                .filter_map(|path| path.parent().map(Path::to_path_buf)),
        );
        if let Some(parent) = self.template_watch_parent().filter(|parent| {
            !self
                .project
                .as_ref()
                .is_some_and(|project| parent.starts_with(&project.root))
                && !self
                    .project_index
                    .watched_roots
                    .iter()
                    .any(|root| parent.starts_with(root))
        }) {
            wanted.insert(parent);
        }
        if let Some(watcher) = &mut self.fs_watcher {
            for parent in self.external_watches.difference(&wanted) {
                if !self.project_index.watched_roots.contains(parent)
                    && !self
                        .project
                        .as_ref()
                        .is_some_and(|project| project.root == *parent)
                {
                    let _ = watcher.unwatch(parent);
                }
            }
            self.external_watches.retain(|p| wanted.contains(p));
            for parent in wanted
                .difference(&self.external_watches)
                .cloned()
                .collect::<Vec<_>>()
            {
                match watcher.watch(&parent, notify::RecursiveMode::NonRecursive) {
                    Ok(()) => { self.external_watches.insert(parent); },
                    Err(error) => log::warn!("cannot watch external editor {}: {error}; focus and manual refresh reconcile it", parent.display()),
                }
            }
        }
    }
    pub(super) fn queue_open_file_reconciliation(&mut self) {
        self.sync_external_file_watches();
        for path in self
            .tabs
            .iter()
            .filter_map(|t| t.state.path.as_ref())
            .filter(|p| !p.starts_with("<typeshed>"))
        {
            if !self.external_changes.contains(path) {
                self.external_changes.push(path.clone());
            }
        }
    }
    pub(super) fn start_fs_watcher(&mut self, root: &Path) {
        use notify::Watcher;
        self.file_session = self.file_session.wrapping_add(1);
        self.fs_watcher = None;
        self.fs_events = None;
        self.fs_dirty_since = None;
        self.external_changes.clear();
        if matches!(self.dialog, Dialog::ExternalChange(_)) {
            self.dialog = Dialog::None;
        }
        self.observed_revisions.clear();
        self.disk_revisions.clear();
        self.external_watches.clear();
        self.project_index.forget_watches();
        let (tx, rx) = crossbeam_channel::bounded(1024);
        self.fs_overflow = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let overflow = self.fs_overflow.clone();
        match notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            if !forwards_watcher_event(&ev) {
                return;
            }
            if tx.try_send(ev).is_err() {
                overflow.store(true, Ordering::Release);
            }
        }) {
            Ok(mut watcher) => match watcher.watch(root, notify::RecursiveMode::Recursive) {
                Ok(()) => {
                    crate::performance_trace::event(
                        "watcher_ready",
                        serde_json::json!({"recursive":true}),
                    );
                    self.fs_watcher = Some(watcher);
                    self.fs_events = Some(rx);
                }
                Err(e) => self.note_unwatched(root, &e.to_string()),
            },
            Err(e) => self.note_unwatched(root, &e.to_string()),
        }
        self.sync_external_file_watches();
    }
    pub(super) fn poll_fs_events(&mut self, now: f64) {
        let project_safe_mode = self.project_safe_mode();
        let mut touched = Vec::new();
        let mut history_events = Vec::new();
        let mut open_paths = Vec::new();
        let mut rescan = self.fs_overflow.swap(false, Ordering::AcqRel);
        let mut packages_changed = false;
        let mut templates_changed = false;
        let mut native_changed = false;
        if let Some(rx) = &self.fs_events {
            // A fixed drain budget keeps the UI responsive during event storms.
            for _ in 0..1024 {
                let Ok(event) = rx.try_recv() else { break };
                match event {
                    Err(error) => {
                        self.code_vision.note_rescan();
                        log::warn!("file watcher: {error}");
                        rescan = true;
                    }
                    Ok(event) => {
                        // Normally dropped by the callback already.
                        if matches!(event.kind, notify::EventKind::Access(_)) {
                            continue;
                        }
                        crate::performance_trace::event(
                            "watcher_event",
                            serde_json::json!({"paths":event.paths,"rescan":event.need_rescan()}),
                        );
                        let history_kind = match event.kind {
                            notify::EventKind::Create(_) => {
                                Some(crate::history_store::EventKind::Created)
                            }
                            notify::EventKind::Remove(_) => {
                                Some(crate::history_store::EventKind::Deleted)
                            }
                            notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
                                if event.paths.len() >= 2 =>
                            {
                                Some(crate::history_store::EventKind::Rename {
                                    from: event.paths[0].clone(),
                                })
                            }
                            notify::EventKind::Modify(_) => {
                                Some(crate::history_store::EventKind::Changed)
                            }
                            _ => None,
                        };
                        self.code_vision.note_fs_event(&event);
                        rescan |= event.need_rescan();
                        self.fs_dirty_since = Some(now);
                        self.project_index.note_project_file(&event.paths);
                        self.project_index.note_fs_event(&event.kind);
                        for (deployment_index, path) in event.paths.into_iter().enumerate() {
                            templates_changed |= self.is_template_store_event(&path);
                            native_changed |= self.is_native_skeleton_event(&path);
                            open_paths.push(rustpy_core::vfs::normalize_path(&path));
                            if path.components().any(|c| {
                                matches!(
                                    c.as_os_str().to_str(),
                                    Some(
                                        ".git"
                                            | "__pycache__"
                                            | ".rustpy"
                                            | "target"
                                            | ".venv"
                                            | "venv"
                                    )
                                )
                            }) {
                                packages_changed |= code_insight::is_installed_package_entry(&path);
                            } else {
                                if let Some(kind) = &history_kind {
                                    history_events.push(crate::history_store::HistoryEvent {
                                        path: path.clone(),
                                        kind: kind.clone(),
                                        time: chrono::Local::now().timestamp_millis(),
                                        label: "File system change".into(),
                                    });
                                }
                                if !project_safe_mode {
                                    if let Some(project) = &self.project {
                                        self.deploy.on_external_change(
                                            &project.root,
                                            &path,
                                            matches!(event.kind, notify::EventKind::Remove(_))
                                                || (deployment_index == 0
                                                    && matches!(
                                                        event.kind,
                                                        notify::EventKind::Modify(
                                                            notify::event::ModifyKind::Name(_)
                                                        )
                                                    )),
                                        );
                                    }
                                    self.test_panel.on_source_change(&path);
                                }
                                touched.push(rustpy_core::vfs::normalize_path(&path));
                            }
                        }
                    }
                }
            }
        }
        if native_changed || rescan {
            self.cancel_native_skeletons();
        }
        if templates_changed || rescan {
            self.project_template_store_changed();
        }
        if packages_changed {
            self.packages_changed();
        }
        if rescan {
            self.code_vision.note_rescan();
            self.reload_analysis_from_disk();
            self.queue_open_file_reconciliation();
            self.refresh_project_files();
            self.fs_dirty_since = Some(now);
        }
        for path in &touched {
            self.queue_file_watchers(path, true);
        }
        for path in &touched {
            self.web_preview_saved(path);
        }
        if !touched.is_empty() {
            self.note_history_disk_paths(&touched);
            self.invalidate_analysis_paths(&touched);
        }
        if !history_events.is_empty() {
            let root = self.project.as_ref().map(|project| project.root.as_path());
            if let Err(error) = crate::history_store::record_events(
                &crate::history_store::base_dir(),
                root,
                &history_events,
            ) {
                self.set_status(format!("Local History: {error}"));
            }
        }
        if !open_paths.is_empty() {
            for tab_path in self.tabs.iter().filter_map(|t| t.state.path.as_ref()) {
                if open_paths
                    .iter()
                    .any(|p| tab_path == p || tab_path.starts_with(p))
                    && !self.external_changes.contains(tab_path)
                {
                    self.external_changes.push(tab_path.clone());
                }
            }
            self.fs_dirty_since = Some(now);
        }
        if self.fs_dirty_since.is_some_and(|since| now - since > 0.3) {
            self.fs_dirty_since = None;
            if let Some(project) = &mut self.project {
                project.tree.invalidate_all();
                project.git_refreshed = -100.0;
            }
            self.symbol_indexes.files_changed();
            self.after_fs_changes();
            self.reconcile_external_changes();
        }
    }
    pub(super) fn refresh_notebook_from_document(&mut self, i: usize) {
        if self.tabs[i].notebook.is_some() {
            if let Ok(model) = self.tabs[i]
                .state
                .doc
                .text()
                .parse::<rustpy_notebook::Notebook>()
            {
                let version = self.tabs[i].state.doc.version();
                if let Some(nb) = self.tabs[i].notebook.as_mut() {
                    nb.replace_model(model);
                    nb.set_dirty(false);
                    nb.set_synced_version(Some(version));
                }
            }
        }
    }
    pub(super) fn reconcile_external_changes(&mut self) {
        let paths = std::mem::take(&mut self.external_changes);
        for path in paths {
            let Some(i) = self.tab_of(&path) else {
                continue;
            };
            if self.is_large_file_placeholder(i) || self.tabs[i].image.is_some() {
                continue;
            }
            if self.reload_large_file(i, background::IfModified::Ask) {
                continue;
            }
            let snapshot = self
                .file_vfs()
                .read(&path, self.normal_editor_read_limits());
            let revision = match &snapshot {
                Ok(s) => Some(s.revision),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    self.set_status(format!("Cannot read {}: {e}", path.display()));
                    continue;
                }
            };
            if self.disk_revisions.get(&path) == Some(&revision) {
                continue;
            }
            // Missing files require an explicit choice even for clean buffers.
            if revision.is_none() || self.tabs[i].has_unsaved_changes() {
                if matches!(self.dialog, Dialog::None) {
                    self.observed_revisions.insert(path.clone(), revision);
                    self.dialog = Dialog::ExternalChange(path);
                } else if !self.external_changes.contains(&path) {
                    self.external_changes.push(path);
                }
            } else if let Ok(snapshot) = snapshot {
                let selected = match self.encoding_override(&path) {
                    Ok(selected) => selected,
                    Err(error) => {
                        self.set_status(error);
                        continue;
                    }
                };
                let position = self.tabs[i].view.session_position(&self.tabs[i].state);
                if let Err(error) = self.tabs[i]
                    .state
                    .reload_from_bytes_with_encoding(&snapshot.bytes, selected)
                {
                    self.set_status(format!("Cannot reload {}: {error}", path.display()));
                    continue;
                }
                let tab = &mut self.tabs[i];
                let carets = tab.state.carets.clone();
                tab.view
                    .restore_session_position(&mut tab.state, position, true);
                tab.state.set_carets(carets);
                self.refresh_notebook_from_document(i);
                self.disk_revisions.insert(path.clone(), revision);
                self.bind_file_permissions(i);
                self.set_status(format!("Reloaded {} (changed on disk)", path.display()));
            }
        }
    }
    pub(super) fn reload_tab_from_disk(&mut self, i: usize) {
        if self.reload_large_file(i, background::IfModified::Discard) {
            return;
        }
        let Some(path) = self.tabs.get(i).and_then(|t| t.state.path.clone()) else {
            return;
        };
        match self
            .file_vfs()
            .read(&path, self.normal_editor_read_limits())
        {
            Ok(snapshot) => {
                let selected = match self.encoding_override(&path) {
                    Ok(selected) => selected,
                    Err(error) => {
                        self.set_status(error);
                        return;
                    }
                };
                let position = self.tabs[i].view.session_position(&self.tabs[i].state);
                if let Err(error) = self.tabs[i]
                    .state
                    .reload_from_bytes_with_encoding(&snapshot.bytes, selected)
                {
                    self.set_status(format!("Cannot reload {}: {error}", path.display()));
                    return;
                }
                let tab = &mut self.tabs[i];
                let carets = tab.state.carets.clone();
                tab.view
                    .restore_session_position(&mut tab.state, position, true);
                tab.state.set_carets(carets);
                self.refresh_notebook_from_document(i);
                self.disk_revisions
                    .insert(path.clone(), Some(snapshot.revision));
                self.bind_file_permissions(i);
                self.set_status(format!("Reloaded {}", path.display()));
            }
            Err(error) => self.set_status(format!(
                "Cannot reload {}: {error}. Keep the editor version to recreate a missing file.",
                path.display()
            )),
        }
    }
}

/// Whether a watcher callback passes `event` on to the UI. Opening, reading
/// and closing files (the IDE's own indexing reads included) changes nothing,
/// and notify's inotify backend cannot be told not to report them, so they
/// are dropped here before they cost channel capacity, the per-frame drain
/// budget or performance-trace rows.
fn forwards_watcher_event(event: &notify::Result<notify::Event>) -> bool {
    !matches!(event, Ok(event) if matches!(event.kind, notify::EventKind::Access(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_drops_access_events_before_queueing() {
        use notify::event::{AccessKind, AccessMode, CreateKind, DataChange, ModifyKind};
        let event = |kind| Ok(notify::Event::new(kind).add_path(PathBuf::from("/p/a.py")));
        for access in [
            AccessKind::Read,
            AccessKind::Open(AccessMode::Any),
            AccessKind::Close(AccessMode::Read),
            AccessKind::Close(AccessMode::Write),
            AccessKind::Any,
        ] {
            assert!(!forwards_watcher_event(&event(notify::EventKind::Access(
                access
            ))));
        }
        assert!(forwards_watcher_event(&event(notify::EventKind::Create(
            CreateKind::File
        ))));
        assert!(forwards_watcher_event(&event(notify::EventKind::Modify(
            ModifyKind::Data(DataChange::Content)
        ))));
        assert!(forwards_watcher_event(&Err(notify::Error::generic(
            "overflow"
        ))));
    }

    #[test]
    fn external_change_prompts_without_publishing_pending_notebook_model() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("current");
        std::fs::create_dir_all(&current).unwrap();
        let path = current.join("pending.ipynb");
        let original = rustpy_notebook::new_notebook().to_string();
        std::fs::write(&path, &original).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(&current);
        let mut app = IdeApp::new(settings, Some(current.clone()));
        assert!(app.tabs.is_empty());
        app.open_file(&path);
        assert_eq!(app.tabs.len(), 1);
        let doc_version = app.tabs[0].state.doc.version();
        let history = app.tabs[0].state.doc.current_history();
        let nb = app.tabs[0].notebook.as_mut().unwrap();
        nb.set_cell_source(0, "print('pending unsynced cell')".into());
        nb.set_dirty(true);
        let synced = nb.synced_version();
        assert!(!app.tabs[0].state.doc.is_modified());
        let external = rustpy_notebook::new_notebook().to_string() + "\n";
        std::fs::write(&path, &external).unwrap();
        let baseline = app.disk_revisions[&path];
        app.external_changes.push(path.clone());
        app.reconcile_external_changes();
        assert!(matches!(&app.dialog, Dialog::ExternalChange(p) if p == &path));
        assert_eq!(app.disk_revisions[&path], baseline);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].state.doc.text(), original);
        assert_eq!(app.tabs[0].state.doc.version(), doc_version);
        assert_eq!(app.tabs[0].state.doc.current_history(), history);
        let nb = app.tabs[0].notebook.as_ref().unwrap();
        assert!(nb.is_dirty());
        assert_eq!(nb.synced_version(), synced);
        assert_eq!(nb.model().cells[0].source, "print('pending unsynced cell')");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), external);
        assert_eq!(app.project.as_ref().unwrap().root, current);
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "rustpy-file-sync-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(dunce::canonicalize(&p).unwrap())
        }
        fn app(&self) -> IdeApp {
            // An explicit fresh project prevents CWD/recent-project workspace restoration.
            // Fixture files are siblings of this root, retaining real external-parent watches.
            let root = self.0.join("project");
            std::fs::create_dir_all(root.join(".rustpy")).unwrap();
            let mut settings = Settings::default();
            settings.trust_project(&root);
            let app = IdeApp::new(settings, Some(root.clone()));
            assert!(
                !app.safe_mode,
                "Run fixtures must reach the save failure, not safe mode"
            );
            assert!(matches!(app.dialog, Dialog::None));
            assert_eq!(app.project.as_ref().unwrap().root, root);
            assert!(
                app.tabs.is_empty(),
                "fresh fixture project must not restore ambient tabs"
            );
            app
        }
        fn assert_tabs_owned(&self, app: &IdeApp) {
            for tab in &app.tabs {
                let path = tab
                    .state
                    .path
                    .as_ref()
                    .expect("fixture tabs must have paths");
                assert!(
                    path.starts_with(&self.0),
                    "fixture must never edit/save ambient tab {}",
                    path.display()
                );
            }
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn synthetic_events(
        app: &mut IdeApp,
        capacity: usize,
    ) -> crossbeam_channel::Sender<notify::Result<notify::Event>> {
        // Drop/join the real watcher before disconnecting its receiver; its callback
        // treats disconnected channels as lost events and legitimately requests a rescan.
        app.fs_watcher.take();
        app.fs_events.take();
        app.fs_overflow = Arc::new(std::sync::atomic::AtomicBool::new(false));
        app.fs_dirty_since = None;
        app.external_changes.clear();
        let (tx, rx) = crossbeam_channel::bounded(capacity);
        app.fs_events = Some(rx);
        tx
    }
    fn changed(app: &mut IdeApp) {
        app.queue_open_file_reconciliation();
        app.reconcile_external_changes();
    }
    fn edit(app: &mut IdeApp) {
        let fixture = app.project.as_ref().unwrap().root.parent().unwrap();
        assert!(app.tabs.iter().all(|tab| tab
            .state
            .path
            .as_ref()
            .is_some_and(|path| path.starts_with(fixture))));
        app.tabs[0].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "# editor\n".into())],
            "test edit",
        );
    }
    #[test]
    fn python_save_rejects_editorconfig_utf16_but_cython_reload_is_generic() {
        let dir = Scratch::new();
        std::fs::write(
            dir.0.join(".editorconfig"),
            "root=true\n[*]\ncharset=utf-16le\n",
        )
        .unwrap();
        // On a case-insensitive file system `m.PY` is the `m.py` already opened.
        std::fs::write(dir.0.join("Case.probe"), b"").unwrap();
        let case_insensitive = dir.0.join("case.probe").exists();
        for extension in ["py", "pyi", "pyw", "PY"] {
            if extension == "PY" && case_insensitive {
                continue;
            }
            let path = dir.0.join(format!("m.{extension}"));
            let original = b"x=1\n";
            std::fs::write(&path, original).unwrap();
            let mut app = dir.app();
            app.open_file(&path);
            dir.assert_tabs_owned(&app);
            edit(&mut app);
            let baseline = app.disk_revisions[&path];
            assert!(!app.save_all());
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert!(app.tabs[0].state.doc.is_modified());
            assert_eq!(app.disk_revisions[&path], baseline);
            let cfg = RunConfig {
                kind: RunConfigKind::Program {
                    program: "sh".into(),
                    args: vec![
                        "-c".into(),
                        format!("touch {}", dir.0.join("ran").display()),
                    ],
                },
                ..Default::default()
            };
            app.launch_config(cfg, crate::run_context::RunMode::Run);
            assert!(app.run_panel.sessions.is_empty() && !dir.0.join("ran").exists());
        }
        let path = dir.0.join("m.pyx");
        let bytes = rustpy_text::Encoding::Utf16Le.encode("x=1\r\n").unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let mut app = dir.app();
        app.open_file(&path);
        dir.assert_tabs_owned(&app);
        assert_eq!(app.tabs.len(), 1);
        let text = app.tabs[0].state.doc.text();
        app.reload_tab_from_disk(0);
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert_eq!(
            app.tabs[0].state.doc.encoding(),
            rustpy_text::Encoding::Utf16Le
        );
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(message, _)| message.starts_with("Reloaded")));
    }

    #[test]
    fn ordinary_open_edit_save_preserves_supported_format_and_raw_revision() {
        let dir = Scratch::new();
        for encoding in [
            rustpy_text::Encoding::Latin1,
            rustpy_text::Encoding::Utf8Bom,
            rustpy_text::Encoding::Utf16Le,
            rustpy_text::Encoding::Utf16Be,
        ] {
            let python = matches!(
                encoding,
                rustpy_text::Encoding::Latin1 | rustpy_text::Encoding::Utf8Bom
            );
            let path = dir.0.join(if python { "m.py" } else { "m.txt" });
            let text = if encoding == rustpy_text::Encoding::Latin1 {
                "# coding: latin-1\r\nx='é'\r\n"
            } else {
                "x='é'\r\n"
            };
            std::fs::write(&path, encoding.encode(text).unwrap()).unwrap();
            let mut app = dir.app();
            app.open_file(&path);
            dir.assert_tabs_owned(&app);
            assert_eq!(app.tabs.len(), 1);
            assert_eq!(app.tabs[0].state.doc.encoding(), encoding);
            let at = app.tabs[0].state.doc.text().find('é').unwrap() as u32;
            app.tabs[0].state.apply_text_edits(
                vec![(
                    TextRange::new(TextSize::new(at), TextSize::new(at + 2)),
                    "à".into(),
                )],
                "Unicode edit",
            );
            assert!(app.save_tab(0), "{:?}", app.status);
            let saved = encoding.encode(&text.replace('é', "à")).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), saved);
            assert_eq!(
                app.disk_revisions[&path],
                Some(rustpy_core::vfs::ContentRevision::of(&saved))
            );
            assert!(!app.tabs[0].state.doc.is_modified());
        }
    }

    #[test]
    fn encoding_failures_preserve_tab_baseline_history_and_disk() {
        let dir = Scratch::new();
        let path = dir.0.join("m.py");
        let original = b"# coding: latin-1\r\nx=1\r\n";
        std::fs::write(&path, original).unwrap();
        let mut app = dir.app();
        app.open_file(&path);
        dir.assert_tabs_owned(&app);
        let baseline = app.disk_revisions[&path];
        let end = app.tabs[0].state.doc.len();
        app.tabs[0].state.apply_text_edits(
            vec![(TextRange::empty(end), "# 😀\n".into())],
            "Unrepresentable edit",
        );
        let text = app.tabs[0].state.doc.text();
        let version = app.tabs[0].state.doc.version();
        let carets = app.tabs[0].state.carets.clone();
        assert!(!app.save_all());
        let cfg = RunConfig {
            kind: RunConfigKind::Program {
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    format!("touch {}", dir.0.join("ran").display()),
                ],
            },
            ..Default::default()
        };
        app.launch_config(cfg, crate::run_context::RunMode::Run);
        assert!(app.run_panel.sessions.is_empty() && !dir.0.join("ran").exists());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert!(app.tabs[0].state.doc.is_modified());
        assert_eq!(app.disk_revisions[&path], baseline);
        app.tabs[0].state.doc.set_read_only(true);
        std::fs::write(&path, b"# coding: utf-8\n\xff").unwrap();
        app.reload_tab_from_disk(0);
        assert_eq!(app.tabs[0].state.doc.text(), text);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert_eq!(app.tabs[0].state.carets, carets);
        assert!(app.tabs[0].state.doc.is_read_only() && app.tabs[0].state.doc.can_undo());
        assert_eq!(app.disk_revisions[&path], baseline);
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(message, _)| message.contains("UTF-8")));
        let rejected = dir.0.join("bad.py");
        std::fs::write(&rejected, b"\xff\xfea").unwrap();
        app.open_file(&rejected);
        dir.assert_tabs_owned(&app);
        assert_eq!(app.tabs.len(), 1);
        assert!(!app.disk_revisions.contains_key(&rejected));
    }

    #[test]
    fn same_metadata_edits_and_raw_format_changes_reload() {
        let dir = Scratch::new();
        let path = dir.0.join("m.py");
        std::fs::write(&path, b"x=1\n").unwrap();
        let mut app = dir.app();
        app.open_file(&path);
        dir.assert_tabs_owned(&app);
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, b"x=2\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        changed(&mut app);
        assert_eq!(app.tabs[0].state.doc.text(), "x=2\n");
        std::fs::write(&path, b"\xef\xbb\xbfx=2\r\n").unwrap();
        changed(&mut app);
        assert_eq!(
            app.tabs[0].state.doc.encoding(),
            rustpy_text::Encoding::Utf8Bom
        );
        assert_eq!(
            app.tabs[0].state.doc.line_ending(),
            rustpy_text::LineEnding::CrLf
        );
    }
    #[test]
    fn keep_acknowledges_displayed_revision_and_save_conflicts_preserve_both() {
        let dir = Scratch::new();
        let path = dir.0.join("m.py");
        std::fs::write(&path, b"x=1\n").unwrap();
        let mut app = dir.app();
        app.open_file(&path);
        dir.assert_tabs_owned(&app);
        edit(&mut app);
        let editor = app.tabs[0].state.doc.text();
        std::fs::write(&path, b"x=2\n").unwrap();
        changed(&mut app);
        let displayed = app.observed_revisions[&path];
        std::fs::write(&path, b"x=3\n").unwrap();
        changed(&mut app);
        assert_eq!(app.observed_revisions[&path], displayed);
        app.acknowledge_disk_revision(&path);
        app.dialog = Dialog::None;
        assert!(!app.save_all());
        assert_eq!(app.tabs[0].state.doc.text(), editor);
        assert_eq!(std::fs::read(&path).unwrap(), b"x=3\n");
        app.acknowledge_disk_revision(&path);
        app.dialog = Dialog::None;
        assert!(app.save_all());
        assert!(!app.tabs[0].state.doc.is_modified());
        assert!(String::from_utf8(std::fs::read(&path).unwrap())
            .unwrap()
            .contains("# editor"));
    }
    #[test]
    fn missing_clean_files_require_choice_and_keep_allows_recreation() {
        let dir = Scratch::new();
        let path = dir.0.join("m.py");
        std::fs::write(&path, b"x=1\n").unwrap();
        let mut app = dir.app();
        app.open_file(&path);
        dir.assert_tabs_owned(&app);
        std::fs::remove_file(&path).unwrap();
        changed(&mut app);
        assert!(matches!(app.dialog, Dialog::ExternalChange(_)));
        assert!(!app.save_tab(0));
        assert!(!path.exists());
        app.acknowledge_disk_revision(&path);
        app.dialog = Dialog::None;
        assert!(app.save_all());
        assert_eq!(std::fs::read(&path).unwrap(), b"x=1\n");
    }
    #[test]
    fn watcher_tracks_excluded_tabs_descendants_and_trailing_debounce() {
        let dir = Scratch::new();
        let folder = dir.0.join(".venv/pkg");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("m.py");
        std::fs::write(&path, b"x=1\n").unwrap();
        let mut app = dir.app();
        app.open_file(&path);
        dir.assert_tabs_owned(&app);
        let old_overflow = app.fs_overflow.clone();
        let tx = synthetic_events(&mut app, 4);
        assert!(app.fs_watcher.is_none());
        assert!(!Arc::ptr_eq(&old_overflow, &app.fs_overflow));
        // A late old callback must not spuriously bypass the synthetic debounce.
        old_overflow.store(true, Ordering::Release);
        assert!(!app.fs_overflow.load(Ordering::Acquire));
        std::fs::write(&path, b"x=2\n").unwrap();
        tx.send(Ok(notify::Event::new(notify::EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))
        .add_path(path.clone())))
            .unwrap();
        app.poll_fs_events(1.0);
        app.poll_fs_events(1.2);
        assert_eq!(app.tabs[0].state.doc.text(), "x=1\n");
        tx.send(Ok(notify::Event::new(notify::EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))
        .add_path(path.clone())))
            .unwrap();
        app.poll_fs_events(1.2);
        app.poll_fs_events(1.4);
        assert_eq!(app.tabs[0].state.doc.text(), "x=1\n");
        app.poll_fs_events(1.6);
        assert_eq!(app.tabs[0].state.doc.text(), "x=2\n");
        app.tabs[0].state.doc.set_read_only(false);
        edit(&mut app);
        app.tabs[0].state.doc.set_read_only(true);
        std::fs::remove_dir_all(&folder).unwrap();
        tx.send(Ok(notify::Event::new(notify::EventKind::Remove(
            notify::event::RemoveKind::Folder,
        ))
        .add_path(folder)))
            .unwrap();
        app.poll_fs_events(2.0);
        app.poll_fs_events(2.4);
        assert!(matches!(app.dialog, Dialog::ExternalChange(_)));
    }
    #[test]
    fn overflow_and_watcher_error_rescan_open_tabs() {
        let dir = Scratch::new();
        let path = dir.0.join("m.py");
        std::fs::write(&path, b"x=1\n").unwrap();
        let mut app = dir.app();
        app.open_file(&path);
        dir.assert_tabs_owned(&app);
        std::fs::write(&path, b"x=2\n").unwrap();
        app.fs_overflow.store(true, Ordering::Release);
        app.poll_fs_events(1.0);
        app.poll_fs_events(1.4);
        assert_eq!(app.tabs[0].state.doc.text(), "x=2\n");
        let tx = synthetic_events(&mut app, 1);
        std::fs::write(&path, b"x=3\n").unwrap();
        tx.send(Err(notify::Error::generic("lost events"))).unwrap();
        app.poll_fs_events(2.0);
        app.poll_fs_events(2.4);
        assert_eq!(app.tabs[0].state.doc.text(), "x=3\n");
    }
    #[test]
    fn external_parent_watch_survives_content_root_attach_and_detach() {
        let dir = Scratch::new();
        let root = dir.0.join("project");
        let external = dir.0.join("external");
        std::fs::create_dir_all(root.join(".rustpy")).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        let path = external.join("m.py");
        std::fs::write(&path, b"x=1\n").unwrap();
        let mut app = dir.app();
        app.open_file(&path);
        dir.assert_tabs_owned(&app);
        assert!(app.external_watches.contains(&external));
        let pump = |app: &mut IdeApp, expected: &str| {
            let start = std::time::Instant::now();
            while app.tabs[0].state.doc.text() != expected {
                assert!(
                    start.elapsed() < std::time::Duration::from_secs(10),
                    "external parent event was lost"
                );
                app.poll_fs_events(start.elapsed().as_secs_f64());
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        std::fs::write(&path, b"x=2\n").unwrap();
        pump(&mut app, "x=2\n");
        std::fs::write(
            root.join(".rustpy/project.json"),
            r#"{"content_roots":["../external"]}"#,
        )
        .unwrap();
        app.refresh_project_files();
        assert!(app.project_index.watched_roots.contains(&external));
        assert!(!app.external_watches.contains(&external));
        std::fs::write(&path, b"x=3\n").unwrap();
        pump(&mut app, "x=3\n");
        std::fs::write(root.join(".rustpy/project.json"), r#"{"content_roots":[]}"#).unwrap();
        app.refresh_project_files();
        assert!(!app.project_index.watched_roots.contains(&external));
        assert!(app.external_watches.contains(&external));
        std::fs::write(&path, b"x=4\n").unwrap();
        pump(&mut app, "x=4\n");
    }
    #[test]
    fn regained_focus_and_manual_refresh_reconcile_missed_editor_events() {
        let dir = Scratch::new();
        let path = dir.0.join("m.py");
        std::fs::write(&path, b"x=1\n").unwrap();
        let mut app = dir.app();
        app.open_file(&path);
        dir.assert_tabs_owned(&app);
        let ctx = egui::Context::default();
        let frame = |app: &mut IdeApp, focused: bool| {
            let mut viewports = egui::ViewportIdMap::default();
            viewports.insert(
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    focused: Some(focused),
                    ..Default::default()
                },
            );
            ctx.run_ui(
                egui::RawInput {
                    viewports,
                    ..Default::default()
                },
                |ui| app.poll_project_index(ui.ctx()),
            )
            .drop_without_applying_deltas();
        };
        frame(&mut app, false);
        std::fs::write(&path, b"x=2\n").unwrap();
        frame(&mut app, true);
        assert_eq!(app.tabs[0].state.doc.text(), "x=2\n");
        std::fs::write(&path, b"x=3\n").unwrap();
        app.refresh_project_files();
        assert_eq!(app.tabs[0].state.doc.text(), "x=3\n");
    }

    #[test]
    fn failed_save_blocks_run_and_preserves_editor() {
        let dir = Scratch::new();
        let path = dir.0.join("m.py");
        std::fs::write(&path, b"x=1\n").unwrap();
        let mut app = dir.app();
        app.open_file(&path);
        dir.assert_tabs_owned(&app);
        edit(&mut app);
        std::fs::write(&path, b"x=2\n").unwrap();
        let cfg = RunConfig {
            kind: RunConfigKind::Program {
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    format!("touch {}", dir.0.join("ran").display()),
                ],
            },
            ..Default::default()
        };
        app.launch_config(cfg, crate::run_context::RunMode::Run);
        assert!(app.run_panel.sessions.is_empty());
        assert!(!dir.0.join("ran").exists());
        assert!(app.tabs[0].state.doc.is_modified());
        assert_eq!(std::fs::read(&path).unwrap(), b"x=2\n");
    }
}
