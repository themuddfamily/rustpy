//! Automatic document checkpoints, named labels and selection history.
use super::*;
use crate::a11y::DialogWindow;
use crate::history_store::{self, EventKind, HistoryEvent};
use rustpy_core::{TaskHandle, TaskManager};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct Runtime {
    seen: HashMap<rustpy_text::DocumentId, (u64, PathBuf)>,
    label: Option<(PathBuf, String)>,
    export: Option<(String, String, Option<String>)>,
    maintenance: Option<TaskHandle<history_store::Pruned>>,
    last_maintenance: Option<Instant>,
    disk_queue: std::collections::HashSet<(Option<PathBuf>, PathBuf)>,
    disk_job: Option<TaskHandle<usize>>,
}
impl Runtime {
    pub(super) fn modal_active(&self) -> bool {
        self.label.is_some() || self.export.is_some()
    }
}
pub(super) struct Selection {
    pub range: TextRange,
    pub document: rustpy_text::DocumentId,
    pub version: u64,
    pub source: String,
    pub current: String,
    pub first_line: usize,
    pub last_line: usize,
}

impl IdeApp {
    pub(super) fn checkpoint_execution_history(&self, label: &str) {
        let root = self.project.as_ref().map(|project| project.root.as_path());
        let base = history_store::base_dir();
        let mut files = std::collections::HashSet::new();
        let mut bytes = 0;
        for tab in &self.tabs {
            let Some(path) = tab
                .state
                .path
                .as_ref()
                .filter(|path| !path.starts_with("<typeshed>"))
            else {
                continue;
            };
            if !files.insert(path.clone()) {
                continue;
            }
            let text = tab.state.doc.text();
            bytes += text.len();
            if bytes > 64 * 1024 * 1024 {
                break;
            }
            history_store::label_text(&base, root, path, &text, label);
        }
    }
    pub(super) fn note_history_disk_paths(&mut self, paths: &[PathBuf]) {
        for path in paths {
            if self.local_history_runtime.disk_queue.len() >= 5000 {
                break;
            }
            self.local_history_runtime.disk_queue.insert((
                self.project.as_ref().map(|project| project.root.clone()),
                path.clone(),
            ));
        }
    }
    pub(super) fn note_saved_history(&self, path: &Path) {
        let root = self.project.as_ref().map(|project| project.root.as_path());
        let _ = history_store::record_events(
            &history_store::base_dir(),
            root,
            &[HistoryEvent {
                path: path.into(),
                kind: EventKind::Saved,
                time: chrono::Local::now().timestamp_millis(),
                label: "Save".into(),
            }],
        );
    }
    pub(super) fn observe_local_history(&mut self, ctx: &egui::Context) {
        history_store::set_policy(&self.settings.local_history);
        if !self.settings.local_history.enabled {
            return;
        }
        let root = self.project.as_ref().map(|project| project.root.clone());
        let base = history_store::base_dir();
        if let Some(job) = &mut self.local_history_runtime.disk_job {
            if job.try_result().is_some() {
                self.local_history_runtime.disk_job = None;
            }
        }
        if self.local_history_runtime.disk_job.is_none()
            && !self.local_history_runtime.disk_queue.is_empty()
        {
            let root = self
                .local_history_runtime
                .disk_queue
                .iter()
                .next()
                .and_then(|(root, _)| root.clone());
            let paths = self
                .local_history_runtime
                .disk_queue
                .iter()
                .filter(|(owner, _)| *owner == root)
                .map(|(_, path)| path.clone())
                .collect::<Vec<_>>();
            self.local_history_runtime
                .disk_queue
                .retain(|(owner, _)| *owner != root);
            let base = base.clone();
            self.local_history_runtime.disk_job = Some(
                TaskManager::global()
                    .task("Local History disk changes")
                    .hidden()
                    .spawn(move |progress| {
                        progress.check()?;
                        Ok(history_store::snapshot_paths(
                            &base,
                            root.as_deref(),
                            &paths,
                            "External change",
                            false,
                        ))
                    }),
            );
        }
        let live = self
            .tabs
            .iter()
            .map(|tab| tab.state.doc.identity())
            .collect::<std::collections::HashSet<_>>();
        self.local_history_runtime
            .seen
            .retain(|document, _| live.contains(document));
        let mut events = Vec::new();
        for tab in &self.tabs {
            let Some(path) = tab
                .state
                .path
                .as_ref()
                .filter(|path| !path.starts_with("<typeshed>"))
            else {
                continue;
            };
            if tab.state.doc.is_read_only()
                || tab.state.doc.transaction_depth() > 0
                || u32::from(tab.state.doc.len()) as u64 > 2 * 1024 * 1024
            {
                continue;
            }
            let id = tab.state.doc.identity();
            let version = tab.state.doc.version();
            let previous = self
                .local_history_runtime
                .seen
                .insert(id, (version, path.clone()));
            if previous
                .as_ref()
                .is_some_and(|(old, old_path)| *old == version && old_path == path)
            {
                continue;
            }
            let kind = match &previous {
                Some((_, old)) if old != path => EventKind::Rename { from: old.clone() },
                Some(_) => EventKind::Changed,
                None => EventKind::Opened,
            };
            let label = if previous.is_none() {
                "Opened"
            } else if tab.state.doc.is_modified() {
                "Editor change"
            } else {
                "External change"
            };
            history_store::snapshot_text(
                &base,
                root.as_deref(),
                path,
                &tab.state.doc.text(),
                label,
            );
            events.push(HistoryEvent {
                path: path.clone(),
                kind,
                time: chrono::Local::now().timestamp_millis(),
                label: label.into(),
            });
        }
        if let Err(error) = history_store::record_events(&base, root.as_deref(), &events) {
            self.set_status(format!("Local History: {error}"));
        }
        if let Some(task) = &mut self.local_history_runtime.maintenance {
            if let Some(result) = task.try_result() {
                self.local_history_runtime.maintenance = None;
                if let Err(error) = result {
                    self.set_status(format!("Local History cleanup: {error}"));
                }
            }
        }
        if self.local_history_runtime.maintenance.is_none()
            && self
                .local_history_runtime
                .last_maintenance
                .is_none_or(|last| last.elapsed() > Duration::from_secs(30))
        {
            self.local_history_runtime.last_maintenance = Some(Instant::now());
            let policy = self.settings.local_history.clone();
            self.local_history_runtime.maintenance = Some(
                TaskManager::global()
                    .task("Local History retention")
                    .hidden()
                    .spawn(move |progress| {
                        progress.check()?;
                        history_store::prune(
                            &base,
                            &policy,
                            chrono::Local::now().timestamp_millis(),
                        )
                        .map_err(|error| rustpy_core::TaskError::Failed(error.to_string()))
                    }),
            );
        }
        if self.local_history_runtime.maintenance.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
    pub(super) fn begin_history_label(&mut self, path: PathBuf) {
        self.local_history_runtime.label = Some((path, String::new()));
    }
    pub(super) fn begin_history_patch(&mut self, path: &Path, entry: &history_store::HistoryEntry) {
        let Ok(old) = std::fs::read_to_string(&entry.file) else {
            self.set_status("The history snapshot is unavailable");
            return;
        };
        let current = self
            .tabs
            .iter()
            .find(|tab| tab.state.path.as_deref() == Some(path))
            .map(|tab| tab.state.doc.text())
            .unwrap_or_else(|| std::fs::read_to_string(path).unwrap_or_default());
        let relative = self
            .project
            .as_ref()
            .and_then(|project| path.strip_prefix(&project.root).ok())
            .unwrap_or(path);
        let text = history_store::create_patch(relative, &old, &current);
        self.local_history_runtime.export = Some((
            path.with_extension("patch").display().to_string(),
            text,
            None,
        ));
    }
    pub(super) fn history_auxiliary_dialogs(&mut self, ctx: &egui::Context) {
        if let Some((path, mut label)) = self.local_history_runtime.label.take() {
            let mut open = true;
            let mut apply = false;
            egui::Window::new("Put Local History Label")
                .id(egui::Id::new("history-label"))
                .open(&mut open)
                .show_modal(ctx, |ui| {
                    ui.label(path.display().to_string());
                    ui.add(egui::TextEdit::singleline(&mut label).hint_text("Checkpoint name"));
                    if ui
                        .add_enabled(!label.trim().is_empty(), egui::Button::new("Put Label"))
                        .clicked()
                    {
                        apply = true;
                    }
                });
            if apply {
                let root = self.project.as_ref().map(|project| project.root.clone());
                let base = history_store::base_dir();
                let mut count = if path.is_dir() {
                    history_store::label_paths(
                        &base,
                        root.as_deref(),
                        std::slice::from_ref(&path),
                        label.trim(),
                    )
                } else {
                    0
                };
                for tab in &self.tabs {
                    if let Some(file) = tab
                        .state
                        .path
                        .as_ref()
                        .filter(|file| *file == &path || file.starts_with(&path))
                    {
                        if history_store::label_text(
                            &base,
                            root.as_deref(),
                            file,
                            &tab.state.doc.text(),
                            label.trim(),
                        ) {
                            count += 1;
                        }
                    }
                }
                if count == 0 && path.is_file() {
                    count = history_store::label_paths(
                        &base,
                        root.as_deref(),
                        std::slice::from_ref(&path),
                        label.trim(),
                    );
                }
                let _ = history_store::record_events(
                    &base,
                    root.as_deref(),
                    &[HistoryEvent {
                        path: path.clone(),
                        kind: EventKind::Label,
                        time: chrono::Local::now().timestamp_millis(),
                        label: label.clone(),
                    }],
                );
                self.set_status(format!("Local History label saved for {count} file(s)"));
                open = false;
            }
            if open {
                self.local_history_runtime.label = Some((path, label));
            }
        }
        if let Some((mut path, text, mut error)) = self.local_history_runtime.export.take() {
            let mut open = true;
            let mut save = false;
            egui::Window::new("Export Local History Patch")
                .id(egui::Id::new("history-patch"))
                .open(&mut open)
                .show_modal(ctx, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut path).desired_width(480.0));
                    if let Some(error) = &error {
                        ui.colored_label(self.theme.warning, error);
                    }
                    if ui
                        .add_enabled(!path.trim().is_empty(), egui::Button::new("Save patch"))
                        .clicked()
                    {
                        save = true;
                    }
                    if ui.button("Copy patch").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                });
            if save {
                match std::fs::write(path.trim(), &text) {
                    Ok(()) => {
                        self.set_status("Local History patch exported");
                        open = false;
                    }
                    Err(failure) => error = Some(failure.to_string()),
                }
            }
            if open {
                self.local_history_runtime.export = Some((path, text, error));
            }
        }
    }
    pub(super) fn open_selection_history(&mut self) {
        let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) else {
            return;
        };
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        let selected = tab.state.primary().range();
        if selected.is_empty() {
            self.set_status("Select text to view its Local History");
            return;
        }
        let first = tab.state.doc.line_of(selected.start());
        let last = tab
            .state
            .doc
            .line_of(TextSize::new(u32::from(selected.end()).saturating_sub(1)));
        let range = TextRange::new(
            tab.state.doc.line_range(first).start(),
            tab.state.doc.line_range_with_newline(last).end(),
        );
        let selection = Selection {
            range,
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            source: tab.state.doc.text(),
            current: tab.state.doc.slice(range),
            first_line: first,
            last_line: last,
        };
        self.open_path_history(&path);
        match std::mem::replace(&mut self.dialog, Dialog::None) {
            Dialog::LocalHistory(mut state) => {
                state.selection = Some(selection);
                self.select_history_entry(&mut state, 0);
                self.dialog = Dialog::LocalHistory(state);
            }
            other => self.dialog = other,
        }
    }
    pub(super) fn restore_history_selection(
        &mut self,
        path: &Path,
        entry: &history_store::HistoryEntry,
        selection: &Selection,
    ) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| {
            tab.state.path.as_deref() == Some(path)
                && tab.state.doc.identity() == selection.document
                && tab.state.doc.version() == selection.version
        }) else {
            self.set_status("The document changed. Reopen selection history before reverting.");
            return false;
        };
        let Ok(old) = std::fs::read_to_string(&entry.file) else {
            self.set_status("The history snapshot is unavailable");
            return false;
        };
        let text = history_store::selection_text_at(
            &old,
            &selection.source,
            selection.first_line,
            selection.last_line,
        );
        self.snapshot(index, "Before selection revert");
        let caret = crate::editor::Caret::at(selection.range.start());
        self.tabs[index].state.apply(
            vec![rustpy_text::Edit::replace(selection.range, text)],
            vec![caret],
            rustpy_text::EditGroup::Separate,
            Some("Revert selection from Local History"),
        );
        self.set_status("Selection reverted from Local History");
        true
    }
    pub(super) fn local_history_settings(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Local History (independent of Git)", |ui| {
            let settings = &mut self.settings.local_history;
            ui.checkbox(&mut settings.enabled, "Keep Local History");
            ui.add(egui::Slider::new(&mut settings.retention_days, 1..=365).text("Retention days"));
            ui.add(
                egui::Slider::new(&mut settings.max_size_mb, 1..=16_384)
                    .logarithmic(true)
                    .text("Maximum snapshot storage (MiB)"),
            );
            ui.add(
                egui::Slider::new(&mut settings.snapshots_per_file, 1..=1000)
                    .text("Snapshots per file"),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_revert_is_undoable_and_refuses_an_edited_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("main.py");
        let current = "head\nvalue = 2\ntail\n";
        std::fs::write(&path, current).unwrap();
        let snapshot = directory.path().join("old.txt");
        std::fs::write(&snapshot, "head\nvalue = 1\ntail\n").unwrap();
        let entry = history_store::HistoryEntry {
            file: snapshot,
            label: "Earlier".into(),
            time: 1,
        };
        let mut app = IdeApp::new(Settings::default(), Some(directory.path().into()));
        app.open_file(&path);
        let document = &app.tabs[0].state.doc;
        let selection = Selection {
            range: document.line_range_with_newline(1),
            document: document.identity(),
            version: document.version(),
            source: current.into(),
            current: document.line_text(1),
            first_line: 1,
            last_line: 1,
        };
        assert!(app.restore_history_selection(&path, &entry, &selection));
        assert_eq!(app.tabs[0].state.doc.text(), "head\nvalue = 1\ntail\n");
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), current);
        assert!(!app.restore_history_selection(&path, &entry, &selection));
        assert_eq!(app.tabs[0].state.doc.text(), current);
    }
}
