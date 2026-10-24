//! Local History (snapshots independent of VCS), Compare with Clipboard,
//! Compare Files and the Markdown split preview.

use super::*;
use crate::a11y::DialogWindow;
use crate::diff_view::SideBySide;
use crate::history_store::{self, HistoryEntry, HistoryFile};

pub(super) struct LocalHistoryState {
    pub path: PathBuf,
    pub entries: Vec<HistoryEntry>,
    pub selected: usize,
    pub diff: Option<SideBySide>,
    pub selection: Option<super::local_history_workflows::Selection>,
}

pub(super) struct DiffDialog {
    pub title: String,
    pub left_title: String,
    pub right_title: String,
    pub diff: SideBySide,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum MarkdownMode {
    EditorOnly,
    #[default]
    Split,
    PreviewOnly,
}

/// Local History of a folder: every file below it with snapshots, deleted ones too.
pub(super) struct FolderHistoryState {
    pub dir: PathBuf,
    pub files: Vec<HistoryFile>,
    pub selected_file: Option<usize>,
    pub entries: Vec<HistoryEntry>,
    pub selected_entry: usize,
    pub diff: Option<SideBySide>,
    pub deleted_only: bool,
    pub events: Vec<history_store::HistoryEvent>,
}

fn format_time(ms: i64) -> String {
    rustpy_i18n::regional::timestamp_millis(ms)
}

impl IdeApp {
    fn history_root(&self) -> Option<PathBuf> {
        self.project.as_ref().map(|p| p.root.clone())
    }

    /// Stores the current text of tab `i` as a Local History snapshot (deduplicated against the newest one).
    pub(super) fn snapshot(&mut self, i: usize, label: &str) {
        let Some(tab) = self.tabs.get(i) else { return };
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        let text = tab.state.doc.text();
        self.snapshot_text(&path, &text, label);
    }

    /// Stores `text` as a Local History snapshot of `path` — also for files
    /// that are not open (before Replace in Files rewrites them on disk).
    pub(super) fn snapshot_text(&self, path: &Path, text: &str, label: &str) {
        if path.starts_with("<typeshed>") {
            return;
        }
        let root = self.history_root();
        history_store::snapshot_text(
            &history_store::base_dir(),
            root.as_deref(),
            path,
            text,
            label,
        );
    }

    /// Snapshots `paths` (folders recursively) before they are deleted: open
    /// editors with unsaved changes contribute their text, other files their disk content.
    pub(super) fn snapshot_before_delete(&mut self, paths: &[PathBuf]) -> usize {
        let root = self.history_root();
        let base = history_store::base_dir();
        let mut n =
            history_store::snapshot_paths(&base, root.as_deref(), paths, "Before delete", true);
        for tab in &self.tabs {
            let Some(p) = &tab.state.path else { continue };
            if tab.state.doc.is_modified()
                && paths.iter().any(|d| p.starts_with(d))
                && history_store::snapshot_text(
                    &base,
                    root.as_deref(),
                    p,
                    &tab.state.doc.text(),
                    "Unsaved before delete",
                )
            {
                n += 1;
            }
        }
        n
    }

    pub(super) fn open_local_history(&mut self) {
        let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) else {
            return;
        };
        let Some(path) = tab.state.path.clone() else {
            self.set_status("Local History is available for files on disk");
            return;
        };
        self.open_path_history(&path);
    }

    /// Local History › Show History for a file or (with deleted files) a folder.
    pub(super) fn open_path_history(&mut self, path: &Path) {
        let root = self.history_root();
        let base = history_store::base_dir();
        if path.is_dir() {
            let files = history_store::files_under(&base, root.as_deref(), path);
            let mut st = FolderHistoryState {
                dir: path.to_path_buf(),
                files,
                selected_file: None,
                entries: Vec::new(),
                selected_entry: 0,
                diff: None,
                deleted_only: false,
                events:history_store::read_events(&base,root.as_deref()).into_iter().filter(|event|event.path.starts_with(path)||matches!(&event.kind,history_store::EventKind::Rename{from}if from.starts_with(path))).collect(),
            };
            if !st.files.is_empty() {
                self.select_folder_file(&mut st, 0);
            }
            self.dialog = Dialog::FolderHistory(Box::new(st));
            return;
        }
        let entries =
            history_store::list_entries(&history_store::file_dir(&base, root.as_deref(), path));
        let mut st = LocalHistoryState {
            path: path.to_path_buf(),
            entries,
            selected: 0,
            diff: None,
            selection: None,
        };
        self.select_history_entry(&mut st, 0);
        self.dialog = Dialog::LocalHistory(Box::new(st));
    }

    /// Text of `path` now: the editor's when it is open, else the file on disk.
    fn current_text(&self, path: &Path) -> String {
        self.tabs
            .iter()
            .find(|t| t.state.path.as_deref() == Some(path))
            .map(|t| t.state.doc.text())
            .unwrap_or_else(|| std::fs::read_to_string(path).unwrap_or_default())
    }

    pub(super) fn select_history_entry(&self, st: &mut LocalHistoryState, idx: usize) {
        st.selected = idx;
        let Some(e) = st.entries.get(idx) else {
            st.diff = None;
            return;
        };
        let old = std::fs::read_to_string(&e.file).unwrap_or_default();
        let current = self.current_text(&st.path);
        st.diff = Some(if let Some(selection) = &st.selection {
            SideBySide::new(
                &history_store::selection_text_at(
                    &old,
                    &selection.source,
                    selection.first_line,
                    selection.last_line,
                ),
                &selection.current,
            )
        } else {
            SideBySide::new(&old, &current)
        });
    }

    fn select_folder_file(&self, st: &mut FolderHistoryState, idx: usize) {
        st.selected_file = Some(idx);
        st.entries = st
            .files
            .get(idx)
            .map(|f| history_store::list_entries(&f.dir))
            .unwrap_or_default();
        self.select_folder_entry(st, 0);
    }

    fn select_folder_entry(&self, st: &mut FolderHistoryState, idx: usize) {
        st.selected_entry = idx;
        let (Some(f), Some(e)) = (
            st.selected_file.and_then(|i| st.files.get(i)),
            st.entries.get(idx),
        ) else {
            st.diff = None;
            return;
        };
        let old = std::fs::read_to_string(&e.file).unwrap_or_default();
        let current = if f.path.is_file() {
            self.current_text(&f.path)
        } else {
            String::new()
        };
        st.diff = Some(SideBySide::new(&old, &current));
    }

    pub(super) fn folder_history_dialog(
        &mut self,
        ctx: &egui::Context,
        mut st: Box<FolderHistoryState>,
    ) {
        let theme = self.theme.clone();
        let mono = egui::FontId::new(
            self.settings.editor.font_size.max(10.0),
            crate::theme::mono_family(false, false),
        );
        let mut open = true;
        let mut pick_file: Option<usize> = None;
        let mut pick_entry: Option<usize> = None;
        let mut restore = false;
        let mut open_file = false;
        let mut esc = false;
        let title = format!(
            "Local History — {}",
            st.dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| st.dir.display().to_string())
        );
        egui::Window::new(title)
            .collapsible(false)
            .resizable(true)
            .default_size([1100.0, 620.0])
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                egui::Panel::left("fh_files")
                    .resizable(true)
                    .default_size(300.0)
                    .show(ui, |ui| {
                        ui.checkbox(&mut st.deleted_only, "Deleted files only");
                        if ui.button("Put Label…").clicked() {
                            self.begin_history_label(st.dir.clone());
                        }
                        ui.collapsing("Directory events", |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("folder-history-events")
                                .max_height(140.0)
                                .show(ui, |ui| {
                                    for event in st.events.iter().rev().take(200) {
                                        ui.label(format!(
                                            "{} — {:?}: {}",
                                            format_time(event.time),
                                            event.kind,
                                            event
                                                .path
                                                .strip_prefix(&st.dir)
                                                .unwrap_or(&event.path)
                                                .display()
                                        ));
                                    }
                                });
                        });
                        egui::ScrollArea::vertical()
                            .id_salt("fh_files_list")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if st.files.is_empty() {
                                    ui.label(
                                        egui::RichText::new("No Local History below this folder.")
                                            .color(theme.text_dim),
                                    );
                                }
                                for (i, f) in st.files.iter().enumerate() {
                                    if st.deleted_only && f.exists {
                                        continue;
                                    }
                                    let rel = f
                                        .path
                                        .strip_prefix(&st.dir)
                                        .unwrap_or(&f.path)
                                        .display()
                                        .to_string();
                                    let text = if f.exists {
                                        egui::RichText::new(rel)
                                    } else {
                                        egui::RichText::new(format!("{rel}  (deleted)"))
                                            .color(theme.error)
                                    };
                                    if ui
                                        .selectable_label(st.selected_file == Some(i), text)
                                        .clicked()
                                    {
                                        pick_file = Some(i);
                                    }
                                }
                            });
                    });
                egui::Panel::left("fh_entries")
                    .resizable(true)
                    .default_size(220.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("fh_entries_list")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for (i, e) in st.entries.iter().enumerate() {
                                    if ui
                                        .selectable_label(
                                            i == st.selected_entry,
                                            format!("{}\n{}", format_time(e.time), e.label),
                                        )
                                        .clicked()
                                    {
                                        pick_entry = Some(i);
                                    }
                                }
                            });
                    });
                let file = st.selected_file.and_then(|i| st.files.get(i));
                ui.horizontal(|ui| {
                    let label = if file.is_some_and(|f| !f.exists) {
                        "Restore deleted file"
                    } else {
                        "Revert to this snapshot"
                    };
                    if ui
                        .add_enabled(st.diff.is_some(), egui::Button::new(label))
                        .clicked()
                    {
                        restore = true;
                    }
                    if ui
                        .add_enabled(
                            file.is_some_and(|f| f.path.is_file()),
                            egui::Button::new("Open"),
                        )
                        .clicked()
                    {
                        open_file = true;
                    }
                    if ui
                        .add_enabled(
                            file.is_some() && !st.entries.is_empty(),
                            egui::Button::new("Create Patch…"),
                        )
                        .clicked()
                    {
                        if let (Some(file), Some(entry)) = (file, st.entries.get(st.selected_entry))
                        {
                            self.begin_history_patch(&file.path, entry);
                        }
                    }
                    if let Some(d) = &st.diff {
                        ui.label(
                            egui::RichText::new(format!("{} changed line(s)", d.changes))
                                .color(theme.text_dim),
                        );
                    }
                });
                ui.separator();
                if let (Some(d), Some(f)) = (&st.diff, file) {
                    let left = st
                        .entries
                        .get(st.selected_entry)
                        .map(|e| format!("Snapshot {} ({})", format_time(e.time), e.label))
                        .unwrap_or_default();
                    d.ui(
                        ui,
                        &theme,
                        mono.clone(),
                        &left,
                        if f.exists { "Current" } else { "Deleted" },
                    );
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    esc = true;
                }
            });
        if esc {
            open = false;
        }
        if let Some(i) = pick_file {
            self.select_folder_file(&mut st, i);
        }
        if let Some(i) = pick_entry {
            self.select_folder_entry(&mut st, i);
        }
        let file = st.selected_file.and_then(|i| st.files.get(i)).cloned();
        if restore {
            if let (Some(f), Some(e)) = (&file, st.entries.get(st.selected_entry).cloned()) {
                self.restore_from_history(&f.path, &e);
                st.files = history_store::files_under(
                    &history_store::base_dir(),
                    self.history_root().as_deref(),
                    &st.dir,
                );
                if let Some(i) = st.files.iter().position(|x| x.path == f.path) {
                    self.select_folder_file(&mut st, i);
                }
            }
        }
        if open_file {
            if let Some(f) = &file {
                self.open_file(&f.path);
                open = false;
            }
        }
        if open {
            self.dialog = Dialog::FolderHistory(st);
        } else {
            self.focus_editor = true;
        }
    }

    /// Puts snapshot `e` back as the content of `path`: through the editor when the
    /// file is open (undoable), else on disk (re-creating a deleted file).
    fn restore_from_history(&mut self, path: &Path, e: &HistoryEntry) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.state.path.as_deref() == Some(path))
        {
            let Ok(text) = std::fs::read_to_string(&e.file) else {
                return;
            };
            self.snapshot(i, "Before revert");
            let tab = &mut self.tabs[i];
            let whole = TextRange::new(TextSize::new(0), tab.state.doc.len());
            tab.state.apply(
                vec![rustpy_text::Edit::replace(whole, text)],
                vec![crate::editor::Caret::at(TextSize::new(0))],
                rustpy_text::EditGroup::Separate,
                Some("Revert from Local History"),
            );
            self.set_status("Reverted from Local History (save to write it to disk)");
            return;
        }
        let root = self.history_root();
        match history_store::restore(&history_store::base_dir(), root.as_deref(), path, e) {
            Ok(()) => {
                self.reload_changed_files();
                self.set_status(format!("Restored {} from Local History", path.display()));
            }
            Err(err) => self.set_status(format!("Cannot restore {}: {err}", path.display())),
        }
    }

    pub(super) fn local_history_dialog(
        &mut self,
        ctx: &egui::Context,
        mut st: Box<LocalHistoryState>,
    ) {
        let theme = self.theme.clone();
        let mono = egui::FontId::new(
            self.settings.editor.font_size.max(10.0),
            crate::theme::mono_family(false, false),
        );
        let mut open = true;
        let mut select: Option<usize> = None;
        let mut revert = false;
        let mut open_snapshot = false;
        let mut selection_history = false;
        let mut esc = false;
        let title = format!(
            "Local History — {}",
            st.path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        egui::Window::new(title).collapsible(false).resizable(true).default_size([1000.0, 600.0]).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            egui::Panel::left("lh_entries").resizable(true).default_size(260.0).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("lh_list").auto_shrink([false, false]).show(ui, |ui| {
                    if st.entries.is_empty() {
                        ui.label(egui::RichText::new("No snapshots yet. Snapshots are taken on save, before reformatting and before refactorings.").color(theme.text_dim));
                    }
                    for (i, e) in st.entries.iter().enumerate() {
                        let text = format!("{}\n{}", format_time(e.time), e.label);
                        if ui.selectable_label(i == st.selected, text).clicked() {
                            select = Some(i);
                        }
                    }
                });
            });
            ui.horizontal(|ui| {
                if ui.button("Put Label…").clicked(){self.begin_history_label(st.path.clone());}
                if st.selection.is_none()&&ui.button("Show selection history").clicked(){selection_history=true;}
                if st.selection.is_some(){ui.label("Selected lines");}
                if ui.add_enabled(st.diff.is_some(), egui::Button::new("Revert to this snapshot")).clicked() {
                    revert = true;
                }
                if ui.add_enabled(st.diff.is_some(),egui::Button::new("Open snapshot")).clicked(){open_snapshot=true;}
                if ui.add_enabled(st.diff.is_some(),egui::Button::new("Create Patch…")).clicked(){if let Some(entry)=st.entries.get(st.selected){self.begin_history_patch(&st.path,entry);}}
                if let Some(d) = &st.diff {
                    ui.label(egui::RichText::new(format!("{} changed line(s)", d.changes)).color(theme.text_dim));
                }
            });
            ui.separator();
            if let Some(d) = &st.diff {
                let left = st.entries.get(st.selected).map(|e| format!("Snapshot {} ({})", format_time(e.time), e.label)).unwrap_or_default();
                d.ui(ui, &theme, mono.clone(), &left, "Current");
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                esc = true;
            }
        });
        if let Some(i) = select {
            self.select_history_entry(&mut st, i);
        }
        if revert {
            if let Some(e) = st.entries.get(st.selected).cloned() {
                let path = st.path.clone();
                if let Some(selection) = &st.selection {
                    if self.restore_history_selection(&path, &e, selection) {
                        open = false;
                    }
                } else {
                    self.restore_from_history(&path, &e);
                    let root = self.history_root();
                    st.entries = history_store::list_entries(&history_store::file_dir(
                        &history_store::base_dir(),
                        root.as_deref(),
                        &path,
                    ));
                    let sel = st
                        .entries
                        .iter()
                        .position(|x| x.file == e.file)
                        .unwrap_or(0);
                    self.select_history_entry(&mut st, sel);
                }
            }
        }
        if open_snapshot {
            if let Some(entry) = st.entries.get(st.selected) {
                if let Ok(text) = std::fs::read_to_string(&entry.file) {
                    let path = crate::scratches::create(&crate::scratches::scratch_dir(), "txt");
                    match path {
                        Ok(path) => {
                            if std::fs::write(&path, text).is_ok() {
                                self.open_file(&path);
                                open = false;
                            }
                        }
                        Err(error) => {
                            self.set_status(format!("Cannot open history snapshot: {error}"))
                        }
                    }
                }
            }
        }
        if esc {
            open = false;
        }
        if selection_history {
            self.open_selection_history();
            return;
        }
        if open {
            self.dialog = Dialog::LocalHistory(st);
        } else {
            self.focus_editor = true;
        }
    }

    pub(super) fn compare_with_clipboard(&mut self, _ctx: &egui::Context) {
        if let Err(error) = self.compare_current_clipboard() {
            self.set_status(error);
        }
    }

    /// Context actions belong to every actual source editor, including prose.
    /// Python-only completion/parameter interactions must not gate this popup.
    pub(super) fn draw_source_intentions(&mut self, ui: &mut egui::Ui, i: usize) {
        if self.active != Some(i) {
            return;
        }
        let tab = &self.tabs[i];
        let here = Some((
            tab.view.id,
            tab.state.doc.version(),
            tab.state.primary().head(),
        ));
        if self.intentions.is_some() && self.intentions_at != here {
            self.intentions = None;
        }
        if let Some((fixes, selected)) = self.intentions.clone() {
            if let Some(rect) = self.tabs[i].view.caret_screen_rect {
                self.draw_intentions(ui, rect, &fixes, selected);
            }
        }
    }

    /// Markdown preview panel for the active tab; returns true when the editor should be hidden.
    pub(super) fn markdown_preview(&mut self, ui: &mut egui::Ui, i: usize) -> bool {
        self.markdown_editor_workspace(ui, i)
    }
}

#[cfg(test)]
#[path = "markdown_intentions_tests.rs"]
mod markdown_intentions_tests;
