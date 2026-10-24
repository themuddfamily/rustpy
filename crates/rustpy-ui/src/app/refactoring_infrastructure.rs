//! Shared, captured preview for refactorings outside Rename's semantic planner.
use super::*;
use crate::a11y::DialogWindow;
use crate::refactor_preview::{GroupKind, PreviewItem, RefactorPreview};
use rustpy_refactor::refactor2::{FileOp, MultiFileRefactoring};
use std::collections::BTreeMap;

struct Baseline {
    text: Option<String>,
    document: Option<(rustpy_text::DocumentId, u64)>,
    bytes: Option<Vec<u8>>,
    metadata: Option<(bool, u64, Option<std::time::SystemTime>)>,
}
pub(super) struct PreviewState {
    title: String,
    ops: Vec<FileOp>,
    baselines: BTreeMap<PathBuf, Baseline>,
    tree: RefactorPreview,
    single: Option<(egui::Id, Refactoring)>,
    error: Option<String>,
}
fn disk_bytes(path: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    if !path.exists() || path.is_dir() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("Refactoring preview file exceeds 64 MiB".into());
    }
    Ok(Some(bytes))
}
fn metadata(path: &Path) -> Option<(bool, u64, Option<std::time::SystemTime>)> {
    std::fs::symlink_metadata(path)
        .ok()
        .map(|m| (m.is_dir(), m.len(), m.modified().ok()))
}
impl IdeApp {
    pub(super) fn preview_multi(&mut self, m: MultiFileRefactoring) {
        self.preview_file_ops(
            m.changes
                .into_iter()
                .map(|(p, e)| FileOp::Edit(p, e))
                .collect(),
            &m.title,
        );
    }
    pub(super) fn preview_file_ops(&mut self, ops: Vec<FileOp>, title: &str) {
        self.open_refactoring_preview(ops, title, None);
    }
    pub(super) fn preview_single_refactoring(&mut self, r: Refactoring) {
        let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) else {
            return;
        };
        let Some(path) = tab.state.path.clone() else {
            self.apply_refactoring_now(Ok(r));
            return;
        };
        let title = r.title.clone();
        self.open_refactoring_preview(
            vec![FileOp::Edit(path, r.edits.clone())],
            &title,
            Some((tab.view.id, r)),
        );
    }
    fn open_refactoring_preview(
        &mut self,
        ops: Vec<FileOp>,
        title: &str,
        single: Option<(egui::Id, Refactoring)>,
    ) {
        let mut baselines = BTreeMap::new();
        let mut code = Vec::new();
        let mut noncode = Vec::new();
        let mut conflicts = Vec::new();
        for path in refactor_ui::file_op_paths(&ops) {
            if baselines.contains_key(&path) {
                continue;
            }
            let tab = self
                .tabs
                .iter()
                .find(|t| t.state.path.as_ref() == Some(&path));
            let bytes = match disk_bytes(&path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    self.set_status(e);
                    return;
                }
            };
            let text = tab.map(|t| t.state.doc.text()).or_else(|| {
                bytes
                    .as_ref()
                    .and_then(|b| rustpy_text::Encoding::decode(b).ok().map(|(s, _)| s))
            });
            let document = tab.map(|t| (t.state.doc.identity(), t.state.doc.version()));
            let readonly = tab.is_some_and(|t| t.state.doc.is_read_only())
                || std::fs::metadata(&path).is_ok_and(|m| m.permissions().readonly());
            if readonly {
                conflicts.push(PreviewItem {
                    path: path.clone(),
                    range: TextRange::empty(TextSize::new(0)),
                    line: 0,
                    column: 0,
                    line_text: String::new(),
                    checked: false,
                    note: "Read-only target".into(),
                });
            }
            baselines.insert(
                path.clone(),
                Baseline {
                    text,
                    document,
                    bytes,
                    metadata: metadata(&path),
                },
            );
        }
        for op in &ops {
            match op {
                FileOp::Edit(path, edits) => {
                    let Some(text) = baselines.get(path).and_then(|b| b.text.as_ref()) else {
                        self.set_status("Cannot read refactoring target");
                        return;
                    };
                    let syntax = rustpy_syntax::parse(text).syntax_node();
                    let noncode_ranges: Vec<_> = syntax
                        .descendants_with_tokens()
                        .filter_map(|n| n.into_token())
                        .filter(|t| {
                            matches!(
                                t.kind(),
                                rustpy_syntax::SyntaxKind::COMMENT
                                    | rustpy_syntax::SyntaxKind::STRING
                                    | rustpy_syntax::SyntaxKind::FSTRING_MIDDLE
                            )
                        })
                        .map(|t| t.text_range())
                        .collect();
                    let mut previous = None;
                    let mut sorted: Vec<_> = edits.iter().collect();
                    sorted.sort_by_key(|(r, _)| r.start());
                    for (range, replacement) in sorted {
                        let start = usize::from(range.start());
                        let end = usize::from(range.end());
                        let valid = start <= end
                            && end <= text.len()
                            && text.is_char_boundary(start)
                            && text.is_char_boundary(end)
                            && previous.is_none_or(|end| end <= range.start());
                        let safe_start = start.min(text.len());
                        let (line, column, line_text) = if text.is_char_boundary(safe_start) {
                            line_info(text, safe_start)
                        } else {
                            (0, 0, String::new())
                        };
                        let item = PreviewItem {
                            path: path.clone(),
                            range: *range,
                            line,
                            column,
                            line_text,
                            checked: valid,
                            note: if valid {
                                format!("→ {replacement}")
                            } else {
                                "Invalid or overlapping edit".into()
                            },
                        };
                        if !valid {
                            conflicts.push(item)
                        } else if noncode_ranges.iter().any(|r| r.contains_range(*range)) {
                            noncode.push(item)
                        } else {
                            code.push(item)
                        }
                        previous = Some(range.end());
                    }
                }
                FileOp::Create(path, _) | FileOp::Move(_, path) if path.exists() => {
                    conflicts.push(PreviewItem {
                        path: path.clone(),
                        range: TextRange::empty(TextSize::new(0)),
                        line: 0,
                        column: 0,
                        line_text: String::new(),
                        checked: false,
                        note: "Destination already exists".into(),
                    })
                }
                _ => {}
            }
        }
        let mut tree = RefactorPreview::default();
        tree.add_group(GroupKind::Code, "Usages to be changed", "Code edits", code);
        tree.add_group(
            GroupKind::NonCode,
            "Non-code usages",
            "Comments and string literals",
            noncode,
        );
        tree.add_group(
            GroupKind::Conflicts,
            "Conflicts",
            "Resolve these before applying",
            conflicts,
        );
        self.dialog = Dialog::RefactoringPreview(Box::new(PreviewState {
            title: title.into(),
            ops,
            baselines,
            tree,
            single,
            error: None,
        }));
    }
    fn refactoring_baselines_current(&self, state: &PreviewState) -> Result<(), String> {
        for (path, baseline) in &state.baselines {
            if let Some((document, version)) = baseline.document {
                let Some(tab) = self
                    .tabs
                    .iter()
                    .find(|t| t.state.doc.identity() == document)
                else {
                    return Err("A refactoring editor was closed".into());
                };
                if tab.state.path.as_ref() != Some(path)
                    || tab.state.doc.version() != version
                    || tab.state.doc.is_read_only()
                {
                    return Err(
                        "A refactoring editor changed or became read-only; rebuild the preview"
                            .into(),
                    );
                }
            } else if metadata(path) != baseline.metadata || disk_bytes(path)? != baseline.bytes {
                return Err(format!("{} changed; rebuild the preview", path.display()));
            }
        }
        Ok(())
    }
    pub(super) fn refactoring_preview_dialog(
        &mut self,
        ctx: &egui::Context,
        mut state: Box<PreviewState>,
    ) {
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let mut navigate = None;
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        egui::Window::new("Refactoring Preview")
            .default_size([800.0, 500.0])
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.heading(&state.title);
                for op in &state.ops {
                    match op {
                        FileOp::Create(p, _) => {
                            ui.label(format!("Create {}", p.display()));
                        }
                        FileOp::Move(a, b) => {
                            ui.label(format!("Move {} → {}", a.display(), b.display()));
                        }
                        FileOp::Delete(p) => {
                            ui.label(format!("Delete {}", p.display()));
                        }
                        _ => {}
                    }
                }
                egui::ScrollArea::vertical()
                    .max_height(350.0)
                    .show(ui, |ui| {
                        navigate = state.tree.ui(
                            ui,
                            &self.theme,
                            &root,
                            egui::Id::new("common_refactoring_preview"),
                        );
                    });
                if let Some(error) = &state.error {
                    ui.colored_label(self.theme.error, error);
                }
                ui.horizontal(|ui| {
                    apply = ui
                        .add_enabled(
                            state.tree.conflict_count() == 0,
                            egui::Button::new("Do Refactor"),
                        )
                        .clicked();
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if let Some((path, range)) = navigate {
            self.navigate_to(&path, range);
        }
        if apply {
            if let Err(error) = self.refactoring_baselines_current(&state) {
                state.error = Some(error);
                self.dialog = Dialog::RefactoringPreview(state);
                return;
            }
            let selected: std::collections::HashSet<_> = state
                .tree
                .checked()
                .map(|i| (i.path.clone(), i.range))
                .collect();
            let all = state.ops.iter().all(|op| match op {
                FileOp::Edit(path, edits) => edits
                    .iter()
                    .all(|(range, _)| selected.contains(&(path.clone(), *range))),
                _ => true,
            });
            if all {
                if let Some((view, refactoring)) = state.single.take() {
                    if let Some(i) = self.tabs.iter().position(|t| t.view.id == view) {
                        self.active = Some(i);
                        self.apply_refactoring_now(Ok(refactoring));
                        return;
                    }
                }
            }
            let ops = state
                .ops
                .into_iter()
                .filter_map(|op| match op {
                    FileOp::Edit(path, mut edits) => {
                        edits.retain(|(r, _)| selected.contains(&(path.clone(), *r)));
                        (!edits.is_empty()).then_some(FileOp::Edit(path, edits))
                    }
                    other => Some(other),
                })
                .collect();
            self.apply_file_ops_failures(ops, &state.title);
        } else if open && !cancel && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.dialog = Dialog::RefactoringPreview(state);
        } else {
            self.focus_editor = true;
        }
    }
}

fn line_info(text: &str, start: usize) -> (usize, usize, String) {
    let line = text[..start].bytes().filter(|b| *b == b'\n').count();
    let first = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    (line, start - first, text[first..end].into())
}

pub(super) struct UndoGroup {
    documents: Vec<(
        rustpy_text::Document,
        rustpy_text::HistoryId,
        rustpy_text::HistoryId,
    )>,
    disk: Vec<(PathBuf, Vec<u8>, Vec<u8>)>,
    filesystem: Option<FilesystemUndo>,
    paths: Vec<(rustpy_text::DocumentId, Option<PathBuf>, Option<PathBuf>)>,
    title: String,
}
impl IdeApp {
    pub(super) fn capture_refactoring_group(&self, ops: &[FileOp]) -> Option<UndoGroup> {
        let filesystem = if ops.iter().any(|op| !matches!(op, FileOp::Edit(_, _))) {
            Some(FilesystemUndo::capture(ops).ok()?)
        } else {
            None
        };
        let mut documents = Vec::new();
        let mut disk = Vec::new();
        let mut disk_bytes_total = 0usize;
        for op in ops {
            if let FileOp::Edit(path, _) = op {
                if let Some(tab) = self
                    .tabs
                    .iter()
                    .find(|t| t.state.path.as_ref() == Some(path))
                {
                    if !documents
                        .iter()
                        .any(|(doc, _, _): &(rustpy_text::Document, _, _)| {
                            doc.identity() == tab.state.doc.identity()
                        })
                    {
                        let doc = tab.state.doc.share();
                        let history = doc.current_history();
                        documents.push((doc, history, history));
                    }
                } else {
                    if disk.iter().any(|(existing, _, _)| existing == path) {
                        continue;
                    }
                    let bytes = disk_bytes(path).ok()??;
                    disk_bytes_total = disk_bytes_total.checked_add(bytes.len())?;
                    if disk_bytes_total > 32 * 1024 * 1024 || documents.len() + disk.len() >= 128 {
                        return None;
                    }
                    disk.push((path.clone(), bytes.clone(), bytes));
                }
            }
        }
        let mut paths = Vec::new();
        if let Some(fs) = &filesystem {
            for tab in &self.tabs {
                if tab
                    .state
                    .path
                    .as_ref()
                    .is_some_and(|p| fs.roots.iter().any(|r| p.starts_with(r)))
                    || self.active.is_some_and(|i| {
                        self.tabs[i].view.id == tab.view.id && !tab.state.doc.is_read_only()
                    })
                {
                    if !documents
                        .iter()
                        .any(|(doc, _, _)| doc.identity() == tab.state.doc.identity())
                    {
                        let doc = tab.state.doc.share();
                        let history = doc.current_history();
                        documents.push((doc, history, history));
                    }
                    paths.push((
                        tab.state.doc.identity(),
                        tab.state.path.clone(),
                        tab.state.path.clone(),
                    ));
                }
            }
            disk.clear();
        }
        Some(UndoGroup {
            documents,
            disk,
            filesystem,
            paths,
            title: String::new(),
        })
    }
    pub(super) fn finish_refactoring_group(&mut self, mut group: UndoGroup) {
        if group.filesystem.is_some() {
            for (doc, before, _) in &mut group.documents {
                if doc.current_history() == *before {
                    let selections = self
                        .tabs
                        .iter()
                        .find(|tab| tab.state.doc.identity() == doc.identity())
                        .map(|tab| tab.state.selections())
                        .unwrap_or_default();
                    let _ = doc.record_refactoring_boundary(&selections, &group.title);
                }
            }
        }
        for (doc, before, after) in &mut group.documents {
            *after = doc.current_history();
            if *before != *after {
                doc.mark_refactoring();
            }
        }
        if group.filesystem.is_none() {
            group.documents.retain(|(_, before, after)| before != after);
        }
        for (id, _, after) in &mut group.paths {
            if let Some(tab) = self.tabs.iter().find(|t| t.state.doc.identity() == *id) {
                *after = tab.state.path.clone();
            }
        }
        group.disk.retain(|(_, before, after)| before != after);
        if group.filesystem.is_some() || group.documents.len() + group.disk.len() > 1 {
            self.refactoring_groups.push(group);
            while self.refactoring_groups.len() > 32
                || (self.refactoring_groups.len() > 1
                    && self
                        .refactoring_groups
                        .iter()
                        .map(UndoGroup::retained_bytes)
                        .sum::<usize>()
                        > 128 * 1024 * 1024)
            {
                self.refactoring_groups.remove(0);
            }
        }
    }
    pub(super) fn undo_refactoring_group(
        &mut self,
        document: rustpy_text::DocumentId,
        history: rustpy_text::HistoryId,
    ) -> Option<bool> {
        let index = self.refactoring_groups.iter().rposition(|g| {
            g.documents
                .iter()
                .any(|(doc, _, after)| doc.identity() == document && *after == history)
        })?;
        if self.refactoring_groups[index].filesystem.is_some() {
            return Some(self.undo_workspace_refactoring(index));
        }
        let group = &self.refactoring_groups[index];
        if group.documents.iter().any(|(doc, before, after)| {
            doc.current_history() != *after
                || doc.is_read_only()
                || doc.transaction_depth() != 0
                || !doc.undo_history().iter().any(|entry| entry.id == *before)
        }) || group
            .disk
            .iter()
            .any(|(path, _, after)| disk_bytes(path).ok().flatten().as_ref() != Some(after))
        {
            self.set_status("A related refactoring file changed; grouped Undo was refused");
            return Some(false);
        }
        for (path, _, _) in &group.disk {
            if std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().readonly())
                || std::fs::OpenOptions::new().append(true).open(path).is_err()
            {
                self.set_status("A related refactoring file is read-only");
                return Some(false);
            }
        }
        let mut restored = Vec::new();
        for (path, before, after) in &group.disk {
            let vfs = rustpy_core::vfs::Vfs::new();
            let limits = rustpy_core::vfs::ReadLimits::default();
            let result = vfs
                .read(path, limits)
                .map_err(|e| e.to_string())
                .and_then(|snapshot| {
                    if snapshot.bytes.as_ref() != after.as_slice() {
                        return Err("Related file changed".into());
                    }
                    crate::editor::save_actions::checked_write(
                        &vfs,
                        path,
                        before,
                        Some(snapshot.revision),
                        limits,
                    )
                    .map_err(|e| e.to_string())
                });
            if result.is_err() {
                for (path, bytes) in restored {
                    let _ = std::fs::write(path, bytes);
                }
                self.set_status("Could not restore a related refactoring file");
                return Some(false);
            }
            restored.push((path.clone(), after.clone()));
        }
        let mut group = self.refactoring_groups.remove(index);
        for (doc, before, _) in &mut group.documents {
            if let Some(tab) = self
                .tabs
                .iter_mut()
                .find(|t| t.state.doc.identity() == doc.identity())
            {
                let _ = tab.state.select_history(*before);
            } else {
                let _ = doc.select_history(*before);
            }
        }
        let paths: Vec<_> = group.disk.into_iter().map(|(path, _, _)| path).collect();
        self.invalidate_analysis_paths(&paths);
        self.focus_editor = true;
        Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn edit() -> TextRange {
        TextRange::new(TextSize::new(0), TextSize::new(3))
    }
    fn app() -> IdeApp {
        let mut app = IdeApp::new(Settings::default(), None);
        let mut state = EditorState::from_text("old = 1\n# old\n", Language::Python);
        state.path = Some(PathBuf::from("/preview.py"));
        app.push_tab(state);
        app
    }
    #[test]
    fn generic_preview_separates_noncode_and_refuses_stale_open_editor() {
        let mut app = app();
        app.preview_file_ops(
            vec![FileOp::Edit(
                PathBuf::from("/preview.py"),
                vec![
                    (edit(), "new".into()),
                    (
                        TextRange::new(TextSize::new(10), TextSize::new(13)),
                        "new".into(),
                    ),
                ],
            )],
            "Rename",
        );
        let Dialog::RefactoringPreview(state) = std::mem::replace(&mut app.dialog, Dialog::None)
        else {
            panic!("preview")
        };
        assert_eq!(
            state
                .tree
                .groups
                .iter()
                .find(|g| g.kind == GroupKind::NonCode)
                .unwrap()
                .items
                .len(),
            1
        );
        assert!(app.refactoring_baselines_current(&state).is_ok());
        app.tabs[0]
            .state
            .apply_text_edits(vec![(edit(), "changed".into())], "Typing");
        assert!(app.refactoring_baselines_current(&state).is_err());
    }
    #[test]
    fn inplace_typing_commits_one_tagged_history_unit() {
        let mut app = app();
        let before = app.tabs[0].state.doc.text();
        app.start_inplace_rename(
            0,
            vec![edit()],
            TextSize::new(0),
            inplace_rename::InplaceKind::Rename,
            rustpy_refactor::rename::RenameCheck::identifier_only("old"),
        );
        app.tabs[0]
            .state
            .apply_text_edits(vec![(edit(), "first".into())], "Typing");
        app.tabs[0].state.apply_text_edits(
            vec![(
                TextRange::new(TextSize::new(0), TextSize::new(5)),
                "final_name".into(),
            )],
            "Typing",
        );
        app.finish_inplace_rename();
        assert_eq!(app.tabs[0].state.doc.transaction_depth(), 0);
        assert!(app.tabs[0].state.doc.undo_is_refactoring());
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), before);
    }
    #[test]
    fn multi_document_rename_undo_is_one_coordinated_action() {
        let mut app = app();
        let mut peer = EditorState::from_text("old = 2\n", Language::Python);
        peer.path = Some(PathBuf::from("/peer.py"));
        app.push_tab(peer);
        let ops = vec![
            FileOp::Edit(PathBuf::from("/preview.py"), vec![(edit(), "new".into())]),
            FileOp::Edit(PathBuf::from("/peer.py"), vec![(edit(), "new".into())]),
        ];
        app.apply_file_ops_failures(ops, "Rename");
        let doc = app.tabs[0].state.doc.identity();
        let history = app.tabs[0].state.doc.current_history();
        assert_eq!(app.undo_refactoring_group(doc, history), Some(true));
        assert!(app.tabs[0].state.doc.text().starts_with("old"));
        assert_eq!(app.tabs[1].state.doc.text(), "old = 2\n");
    }
    #[test]
    fn related_document_edit_refuses_grouped_undo_without_partial_rollback() {
        let mut app = app();
        let mut peer = EditorState::from_text("old", Language::Python);
        peer.path = Some(PathBuf::from("/peer.py"));
        app.push_tab(peer);
        app.apply_file_ops_failures(
            vec![
                FileOp::Edit(PathBuf::from("/preview.py"), vec![(edit(), "new".into())]),
                FileOp::Edit(PathBuf::from("/peer.py"), vec![(edit(), "new".into())]),
            ],
            "Rename",
        );
        let doc = app.tabs[0].state.doc.identity();
        let history = app.tabs[0].state.doc.current_history();
        app.tabs[1]
            .state
            .apply_text_edits(vec![(edit(), "later".into())], "Typing");
        assert_eq!(app.undo_refactoring_group(doc, history), Some(false));
        assert!(app.tabs[0].state.doc.text().starts_with("new"));
        assert_eq!(app.tabs[1].state.doc.text(), "later");
    }
}

impl IdeApp {
    pub(super) fn validate_refactoring_ops(&self, ops: &[FileOp]) -> Result<(), String> {
        let paths = refactor_ui::file_op_paths(ops);
        if self.tabs.iter().any(|tab| {
            tab.state.doc.is_read_only()
                && tab
                    .state
                    .path
                    .as_ref()
                    .is_some_and(|path| paths.iter().any(|root| path.starts_with(root)))
        }) {
            return Err("A refactoring target is read-only".into());
        }
        for op in ops {
            match op {
                FileOp::Edit(path, edits) => {
                    let text = if let Some(tab) = self
                        .tabs
                        .iter()
                        .find(|t| t.state.path.as_ref() == Some(path))
                    {
                        if tab.state.doc.is_read_only() {
                            return Err(format!("{} is read-only", path.display()));
                        }
                        tab.state.doc.text()
                    } else {
                        if std::fs::metadata(path)
                            .map_err(|e| e.to_string())?
                            .permissions()
                            .readonly()
                        {
                            return Err(format!("{} is read-only", path.display()));
                        }
                        let bytes = disk_bytes(path)?.ok_or("Missing refactoring file")?;
                        rustpy_text::Encoding::decode(&bytes)
                            .map_err(|e| e.to_string())?
                            .0
                    };
                    refactor::try_apply_edits(&text, edits).map_err(|e| e.to_string())?;
                }
                FileOp::Create(path, _) if path.exists() => {
                    return Err(format!("{} already exists", path.display()))
                }
                FileOp::Move(from, to) if !from.exists() || to.exists() => {
                    return Err(format!(
                        "Move source is missing or destination exists: {} → {}",
                        from.display(),
                        to.display()
                    ))
                }
                FileOp::Delete(path)
                    if self.tabs.iter().any(|tab| {
                        tab.state.path.as_ref().is_some_and(|p| p.starts_with(path))
                            && tab.state.doc.is_modified()
                    }) =>
                {
                    return Err("Save modified editors before deleting their files".into())
                }
                FileOp::Delete(path) if !path.exists() => {
                    return Err(format!("{} no longer exists", path.display()))
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
enum FsNode {
    Directory,
    File(Vec<u8>, u32),
}
struct FilesystemUndo {
    roots: Vec<PathBuf>,
    before: BTreeMap<PathBuf, FsNode>,
    after: BTreeMap<PathBuf, FsNode>,
}
fn capture_tree(roots: &[PathBuf]) -> Result<BTreeMap<PathBuf, FsNode>, String> {
    for root in roots {
        if root.ancestors().any(|path| {
            std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        }) {
            return Err("Symlinked filesystem refactoring paths are unsupported".into());
        }
    }
    let mut out = BTreeMap::new();
    let mut queue = roots.to_vec();
    let mut bytes = 0usize;
    while let Some(path) = queue.pop() {
        if out.len() > 4096 {
            return Err("Filesystem refactoring exceeds 4096 entries".into());
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        if metadata.file_type().is_symlink() || metadata.permissions().readonly() {
            return Err("Refactoring snapshot contains a symlink or read-only entry".into());
        }
        if metadata.is_dir() {
            out.insert(path.clone(), FsNode::Directory);
            for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
                if queue.len() + out.len() >= 4096 {
                    return Err("Filesystem refactoring exceeds 4096 entries".into());
                }
                queue.push(entry.map_err(|e| e.to_string())?.path());
            }
        } else if metadata.is_file() {
            let content = disk_bytes(&path)?.ok_or("Missing snapshot file")?;
            bytes = bytes
                .checked_add(content.len())
                .ok_or("Snapshot size overflow")?;
            if bytes > 32 * 1024 * 1024 {
                return Err("Filesystem Undo exceeds 32 MiB".into());
            }
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode()
            };
            #[cfg(not(unix))]
            let mode = 0;
            out.insert(path, FsNode::File(content, mode));
        } else {
            return Err("Unsupported filesystem entry".into());
        }
    }
    Ok(out)
}
fn install_tree(roots: &[PathBuf], tree: &BTreeMap<PathBuf, FsNode>) -> Result<(), String> {
    for root in roots {
        match std::fs::symlink_metadata(root) {
            Ok(m) => if m.is_dir() {
                std::fs::remove_dir_all(root)
            } else {
                std::fs::remove_file(root)
            }
            .map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let mut entries: Vec<_> = tree.iter().collect();
    entries.sort_by_key(|(path, _)| path.components().count());
    for (path, node) in entries {
        match node {
            FsNode::Directory => std::fs::create_dir_all(path).map_err(|e| e.to_string())?,
            FsNode::File(bytes, mode) => {
                use std::io::Write;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?
                }
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|e| e.to_string())?;
                file.write_all(bytes).map_err(|e| e.to_string())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode))
                        .map_err(|e| e.to_string())?;
                }
                #[cfg(not(unix))]
                let _ = mode;
            }
        }
    }
    Ok(())
}
impl FilesystemUndo {
    fn capture(ops: &[FileOp]) -> Result<Self, String> {
        let mut paths = refactor_ui::file_op_paths(ops);
        for path in &mut paths {
            while let Some(parent) = path.parent() {
                if parent.exists() {
                    break;
                }
                *path = parent.to_path_buf();
            }
        }
        paths.sort_by_key(|p| p.components().count());
        let mut roots = Vec::<PathBuf>::new();
        for path in paths {
            if !roots.iter().any(|root| path.starts_with(root)) {
                roots.push(path)
            }
        }
        let before = capture_tree(&roots)?;
        let before_bytes: usize = before
            .values()
            .map(|entry| match entry {
                FsNode::File(bytes, _) => bytes.len(),
                _ => 0,
            })
            .sum();
        let added: usize = ops
            .iter()
            .map(|op| match op {
                FileOp::Create(_, text) => text.len(),
                FileOp::Edit(_, edits) => edits.iter().map(|(_, text)| text.len()).sum(),
                _ => 0,
            })
            .sum();
        if before_bytes.saturating_add(added) > 32 * 1024 * 1024 {
            return Err("Filesystem Undo exceeds 32 MiB".into());
        }
        Ok(Self {
            roots,
            before,
            after: BTreeMap::new(),
        })
    }
    fn finish(&mut self) -> Result<(), String> {
        self.after = capture_tree(&self.roots)?;
        Ok(())
    }
    fn restore(&self) -> Result<(), String> {
        for root in &self.roots {
            if root
                .ancestors()
                .skip(1)
                .filter_map(|path| std::fs::metadata(path).ok())
                .any(|metadata| metadata.permissions().readonly())
            {
                return Err("A refactoring parent became read-only".into());
            }
        }
        if capture_tree(&self.roots)? != self.after {
            return Err("Related files changed; Undo cannot overwrite later work".into());
        }
        if let Err(error) = install_tree(&self.roots, &self.before) {
            let _ = install_tree(&self.roots, &self.after);
            return Err(error);
        }
        Ok(())
    }
}
impl UndoGroup {
    pub(super) fn set_title(&mut self, title: &str) {
        self.title = title.into();
    }

    /// Capture the successful poststate before committing any editor histories.
    fn prepare_poststate(&mut self) -> Result<(), String> {
        if let Some(fs) = &mut self.filesystem {
            fs.finish()?;
        }
        for (path, _, after) in &mut self.disk {
            *after = disk_bytes(path)?.ok_or("A refactoring target disappeared")?;
        }
        Ok(())
    }

    fn rollback_disk(&mut self) -> Result<(), String> {
        if let Some(fs) = &mut self.filesystem {
            // This synchronous apply still owns the plan. Capture partial creates,
            // moves and deletes, including a partially written failed Create.
            fs.finish()?;
            return fs.restore();
        }
        let mut errors = Vec::new();
        for (path, before, _) in &self.disk {
            let vfs = rustpy_core::vfs::Vfs::new();
            let limits = rustpy_core::vfs::ReadLimits::default();
            let result = vfs
                .read(path, limits)
                .map_err(|e| e.to_string())
                .and_then(|snapshot| {
                    if snapshot.bytes.as_ref() == before.as_slice() {
                        return Ok(());
                    }
                    crate::editor::save_actions::checked_write(
                        &vfs,
                        path,
                        before,
                        Some(snapshot.revision),
                        limits,
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
                });
            if let Err(error) = result {
                errors.push(format!("{}: {error}", path.display()));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}
impl IdeApp {
    pub(super) fn apply_file_ops_atomically(
        &mut self,
        mut ops: Vec<FileOp>,
        title: &str,
    ) -> Vec<String> {
        if ops.is_empty() {
            return Vec::new();
        }
        if let Err(error) = self.validate_refactoring_ops(&ops) {
            self.set_status(&error);
            return vec![error];
        }
        let _operation = match self.project.as_ref().and_then(|p| p.git.as_ref()) {
            Some(git) => match rustpy_vcs::RepositoryOperation::enter(git.root(), title) {
                Ok(operation) => Some(operation),
                Err(error) => {
                    self.set_status(error.to_string());
                    return vec![error.to_string()];
                }
            },
            None => None,
        };
        let Some(mut group) = self.capture_refactoring_group(&ops) else {
            let error = "Cannot retain a bounded Undo snapshot; refactoring was not applied";
            self.set_status(error);
            return vec![error.into()];
        };
        group.set_title(title);
        let mut scopes = Vec::new();
        for (document, _, _) in &group.documents {
            let mut document = document.share();
            if document.transaction_depth() != 0 {
                let error = "Finish the active editor transaction before refactoring";
                self.set_status(error);
                let mut errors = vec![error.into()];
                errors.extend(self.rollback_refactoring_scopes(&mut scopes));
                return errors;
            }
            let selections = self
                .tabs
                .iter()
                .find(|tab| tab.state.doc.identity() == document.identity())
                .map(|tab| tab.state.selections())
                .unwrap_or_default();
            match document.begin_transaction(&selections, Some(title)) {
                Ok(token) => scopes.push((document, token)),
                Err(error) => {
                    self.set_status(error.to_string());
                    let mut errors = vec![error.to_string()];
                    errors.extend(self.rollback_refactoring_scopes(&mut scopes));
                    return errors;
                }
            }
        }
        // Edits refer to the original paths, before physical moves/deletions.
        ops.sort_by_key(|op| !matches!(op, FileOp::Edit(_, _)));
        let touched_paths = refactor_ui::file_op_paths(&ops);
        let mut moves = Vec::new();
        let mut deletes = Vec::new();
        let mut failed = None;
        for op in &ops {
            if let Err(error) = self.apply_refactoring_operation(op, title) {
                failed = Some(error);
                break;
            }
            match op {
                FileOp::Move(from, to) => moves.push((from.clone(), to.clone())),
                FileOp::Delete(path) => deletes.push(path.clone()),
                _ => {}
            }
        }
        if failed.is_none() {
            failed = group.prepare_poststate().err();
        }
        if let Some(error) = failed {
            let mut errors = vec![error];
            errors.extend(self.rollback_refactoring_scopes(&mut scopes));
            if let Err(error) = group.rollback_disk() {
                errors.push(format!("Rollback could not restore all files: {error}"));
            }
            self.invalidate_analysis_paths(&touched_paths);
            self.sync_analysis_now();
            if let Some(project) = &mut self.project {
                project.tree.invalidate_all();
            }
            self.set_status(format!("{title} was not completed: {}", errors.join("; ")));
            return errors;
        }
        for (mut document, token) in scopes {
            let selections = self
                .tabs
                .iter()
                .find(|tab| tab.state.doc.identity() == document.identity())
                .map(|tab| tab.state.selections())
                .unwrap_or_default();
            document
                .commit_transaction(token, &selections)
                .expect("the synchronous refactoring owns the outer editor scope");
        }
        // Publish editor/navigation/index side effects only after every write
        // and recoverable poststate capture succeeded.
        for (from, to) in moves {
            self.vcs_file_moved(&from, &to);
            for tab in &mut self.tabs {
                if let Some(rest) = tab
                    .state
                    .path
                    .as_ref()
                    .and_then(|path| path.strip_prefix(&from).ok())
                    .map(Path::to_path_buf)
                {
                    tab.state.path = Some(to.join(rest));
                    tab.state.synced_version = None;
                }
            }
            self.nav.history.rename_file(&from, Some(&to));
        }
        if let Some(fs) = &group.filesystem {
            self.acknowledge_refactoring_snapshot(&fs.after);
        }
        self.finish_refactoring_group(group);
        let deleted_tabs: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| {
                tab.state
                    .path
                    .as_ref()
                    .is_some_and(|path| deletes.iter().any(|root| path.starts_with(root)))
            })
            .map(|(index, _)| index)
            .collect();
        for index in deleted_tabs.into_iter().rev() {
            self.close_tab(index);
        }
        if let Some(project) = &mut self.project {
            project.tree.invalidate_all();
        }
        self.invalidate_analysis_paths(&touched_paths);
        self.reload_changed_files();
        self.sync_analysis_now();
        self.set_status(format!("{title}: {} file operation(s) applied", ops.len()));
        Vec::new()
    }

    fn apply_refactoring_operation(&mut self, op: &FileOp, title: &str) -> Result<(), String> {
        match op {
            FileOp::Edit(path, edits) => {
                if let Some(index) = self.tab_of(path) {
                    let before = self.tabs[index].state.doc.text();
                    let expected = refactor::try_apply_edits(&before, edits)
                        .map_err(|error| error.to_string())?;
                    self.snapshot(index, &format!("Before {title}"));
                    self.tabs[index]
                        .state
                        .apply_text_edits(edits.clone(), title);
                    if self.tabs[index].state.doc.text() != expected {
                        return Err(format!(
                            "{}: editor rejected the refactoring",
                            path.display()
                        ));
                    }
                    Ok(())
                } else {
                    if let Ok(text) = std::fs::read_to_string(path) {
                        crate::history_store::snapshot_text(
                            &crate::history_store::base_dir(),
                            self.project.as_ref().map(|p| p.root.as_path()),
                            path,
                            &text,
                            &format!("Before {title}"),
                        );
                    }
                    refactor_ui::edit_closed_file(path, edits)
                }
            }
            FileOp::Create(path, text) => {
                use std::io::Write;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|e| e.to_string())?;
                file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())
            }
            FileOp::Move(from, to) => {
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::rename(from, to).map_err(|e| {
                    format!("Could not move {} to {}: {e}", from.display(), to.display())
                })
            }
            FileOp::Delete(path) => {
                self.snapshot_before_delete(std::slice::from_ref(path));
                if path.is_dir() {
                    std::fs::remove_dir_all(path)
                } else {
                    std::fs::remove_file(path)
                }
                .map_err(|e| e.to_string())
            }
        }
    }

    fn rollback_refactoring_scopes(
        &mut self,
        scopes: &mut Vec<(rustpy_text::Document, rustpy_text::TransactionToken)>,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        for (mut document, token) in scopes.drain(..).rev() {
            let result = if let Some(tab) = self
                .tabs
                .iter_mut()
                .find(|tab| tab.state.doc.identity() == document.identity())
            {
                tab.state.rollback_transaction(token)
            } else {
                document.rollback_transaction(token).map(|_| ())
            };
            if let Err(error) = result {
                errors.push(format!("Editor rollback failed: {error}"));
            }
        }
        errors
    }

    pub(super) fn request_workspace_refactoring_undo(&mut self) -> bool {
        if !matches!(self.dialog, Dialog::None) {
            return false;
        }
        let Some(index) = self
            .refactoring_groups
            .iter()
            .rposition(|group| group.filesystem.is_some())
        else {
            return false;
        };
        let group = &self.refactoring_groups[index];
        if group
            .documents
            .iter()
            .any(|(doc, _, after)| doc.current_history() != *after || doc.transaction_depth() != 0)
        {
            return false;
        }
        if self.settings.refactoring.confirm_undo {
            self.dialog = Dialog::UndoWorkspaceRefactoring(index);
        } else {
            self.undo_workspace_refactoring(index);
        }
        true
    }
    pub(super) fn workspace_refactoring_undo_dialog(&mut self, ctx: &egui::Context, index: usize) {
        let Some(group) = self.refactoring_groups.get(index) else {
            return;
        };
        let title = group.title.clone();
        let mut open = true;
        let mut undo = false;
        let mut cancel = false;
        egui::Window::new("Undo Refactoring")
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label(format!("Undo {title} in all affected files?"));
                ui.horizontal(|ui| {
                    undo = ui.button("Undo").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if undo {
            self.undo_workspace_refactoring(index);
        } else if open && !cancel {
            self.dialog = Dialog::UndoWorkspaceRefactoring(index);
        }
    }
    /// Moving an open file does not replace its document/history authority.
    /// Only acknowledge bytes owned by the captured operation: a later external
    /// write must still go through ordinary external-change reconciliation.
    fn acknowledge_refactoring_snapshot(&mut self, tree: &BTreeMap<PathBuf, FsNode>) {
        for path in self.tabs.iter().filter_map(|tab| tab.state.path.as_ref()) {
            let Some(FsNode::File(expected, _)) = tree.get(path) else {
                continue;
            };
            if let Ok(snapshot) = self.file_vfs().read(path, self.normal_editor_read_limits()) {
                if snapshot.bytes.as_ref() == expected.as_slice() {
                    self.disk_revisions
                        .insert(path.clone(), Some(snapshot.revision));
                }
            }
        }
    }

    fn undo_workspace_refactoring(&mut self, index: usize) -> bool {
        let Some(group) = self.refactoring_groups.get(index) else {
            return false;
        };
        if group.documents.iter().any(|(doc, before, after)| {
            doc.current_history() != *after
                || doc.is_read_only()
                || doc.transaction_depth() != 0
                || !doc.undo_history().iter().any(|entry| entry.id == *before)
        }) {
            self.set_status("A related editor changed; workspace Undo was refused");
            return false;
        }
        // A newly opened/edited view inside a touched filesystem subtree also
        // owns work that must not be overwritten by restoring the disk tree.
        if self.tabs.iter().any(|tab| {
            tab.state.doc.is_modified()
                && tab.state.path.as_ref().is_some_and(|path| {
                    group
                        .filesystem
                        .as_ref()
                        .is_some_and(|fs| fs.roots.iter().any(|root| path.starts_with(root)))
                })
                && !group
                    .documents
                    .iter()
                    .any(|(doc, _, _)| doc.identity() == tab.state.doc.identity())
        }) {
            self.set_status("An affected file has new unsaved changes; workspace Undo was refused");
            return false;
        }
        if let Some(fs) = &group.filesystem {
            if let Err(error) = fs.restore() {
                self.set_status(error);
                return false;
            }
        }
        let mut group = self.refactoring_groups.remove(index);
        for (doc, before, _) in &mut group.documents {
            if let Some(tab) = self
                .tabs
                .iter_mut()
                .find(|t| t.state.doc.identity() == doc.identity())
            {
                let _ = tab.state.select_history(*before);
            } else {
                let _ = doc.select_history(*before);
            }
        }
        for (id, before, after) in group.paths {
            for tab in &mut self.tabs {
                if tab.state.doc.identity() == id && tab.state.path == after {
                    tab.state.path = before.clone();
                    tab.state.synced_version = None;
                }
            }
            if let (Some(from), Some(to)) = (after, before) {
                if from != to {
                    self.nav.history.rename_file(&from, Some(&to));
                }
            }
        }
        if let Some(fs) = group.filesystem {
            self.acknowledge_refactoring_snapshot(&fs.before);
            self.invalidate_analysis_paths(&fs.roots);
        }
        self.reload_changed_files();
        self.sync_analysis_now();
        if let Some(project) = &mut self.project {
            project.tree.invalidate_all();
        }
        self.set_status(format!("Undid {}", group.title));
        self.focus_editor = true;
        true
    }
}
impl UndoGroup {
    fn retained_bytes(&self) -> usize {
        self.documents
            .iter()
            .map(|(doc, _, _)| usize::from(doc.len()))
            .sum::<usize>()
            + self
                .disk
                .iter()
                .map(|(_, before, after)| before.len() + after.len())
                .sum::<usize>()
            + self.filesystem.as_ref().map_or(0, |fs| {
                fs.before
                    .values()
                    .chain(fs.after.values())
                    .map(|entry| match entry {
                        FsNode::File(bytes, _) => bytes.len(),
                        _ => 0,
                    })
                    .sum::<usize>()
            })
    }
}

#[cfg(test)]
mod filesystem_tests {
    use super::*;
    #[test]
    fn late_create_failure_restores_open_history_closed_bytes_and_earlier_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let open = dir.path().join("open.py");
        let closed = dir.path().join("closed.py");
        let deleted = dir.path().join("deleted.bin");
        let blocker = dir.path().join("new_parent");
        let original_bytes = b"\xef\xbb\xbfvalue = 1\r\n";
        std::fs::write(&open, "value = 1\n").unwrap();
        std::fs::write(&closed, original_bytes).unwrap();
        std::fs::write(&deleted, [0, 255, 128]).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        let mut state = EditorState::from_text("value = 1\n", Language::Python);
        state.path = Some(open.clone());
        app.push_tab(state);
        let history = app.tabs[0].state.doc.current_history();
        let selections = app.tabs[0].state.selections();
        let failures = app.apply_file_ops_failures(
            vec![
                FileOp::Edit(
                    open.clone(),
                    vec![(
                        TextRange::at(TextSize::new(8), TextSize::new(1)),
                        "2".into(),
                    )],
                ),
                FileOp::Edit(
                    closed.clone(),
                    vec![(
                        TextRange::at(TextSize::new(8), TextSize::new(1)),
                        "2".into(),
                    )],
                ),
                FileOp::Delete(deleted.clone()),
                FileOp::Create(blocker.clone(), "not a directory".into()),
                FileOp::Create(blocker.join("child.py"), "value = 3\n".into()),
            ],
            "Atomic refactoring",
        );
        assert!(!failures.is_empty());
        assert_eq!(app.tabs[0].state.doc.text(), "value = 1\n");
        assert_eq!(app.tabs[0].state.doc.current_history(), history);
        assert_eq!(app.tabs[0].state.doc.transaction_depth(), 0);
        assert_eq!(app.tabs[0].state.selections(), selections);
        assert!(!app.tabs[0].state.doc.undo_is_refactoring());
        assert_eq!(std::fs::read(&closed).unwrap(), original_bytes);
        assert_eq!(std::fs::read(&deleted).unwrap(), [0, 255, 128]);
        assert!(!blocker.exists());
        assert!(app.refactoring_groups.is_empty());
    }

    #[test]
    fn failed_plan_keeps_moved_and_deleted_editors_open_at_original_paths() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("original.py");
        let moved = dir.path().join("moved.py");
        let deleted = dir.path().join("delete.py");
        let blocker = dir.path().join("blocker");
        std::fs::write(&from, "original\n").unwrap();
        std::fs::write(&deleted, "delete\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        for (path, text) in [(&from, "original\n"), (&deleted, "delete\n")] {
            let mut state = EditorState::from_text(text, Language::Python);
            state.path = Some(path.clone());
            app.push_tab(state);
        }
        assert!(!app
            .apply_file_ops_failures(
                vec![
                    FileOp::Move(from.clone(), moved.clone()),
                    FileOp::Delete(deleted.clone()),
                    FileOp::Create(blocker.clone(), String::new()),
                    FileOp::Create(blocker.join("child"), String::new()),
                ],
                "Fail after moving",
            )
            .is_empty());
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[0].state.path.as_ref(), Some(&from));
        assert_eq!(app.tabs[1].state.path.as_ref(), Some(&deleted));
        assert_eq!(std::fs::read_to_string(&from).unwrap(), "original\n");
        assert_eq!(std::fs::read_to_string(&deleted).unwrap(), "delete\n");
        assert!(!moved.exists());
        assert!(!blocker.exists());
        assert!(app.closed_tabs.is_empty());
        assert!(app.refactoring_groups.is_empty());
    }

    #[test]
    fn filesystem_move_create_delete_restore_exact_bytes_and_refuse_later_work() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.py");
        let moved = dir.path().join("moved.py");
        let created = dir.path().join("created.py");
        let deleted = dir.path().join("deleted.bin");
        std::fs::write(&original, b"# coding: latin-1\nvalue='\xe9'\r\n").unwrap();
        std::fs::write(&deleted, [0, 255, 128]).unwrap();
        let ops = vec![
            FileOp::Move(original.clone(), moved.clone()),
            FileOp::Create(created.clone(), "new".into()),
            FileOp::Delete(deleted.clone()),
        ];
        let mut snapshot = FilesystemUndo::capture(&ops).unwrap();
        std::fs::rename(&original, &moved).unwrap();
        std::fs::write(&created, "new").unwrap();
        std::fs::remove_file(&deleted).unwrap();
        snapshot.finish().unwrap();
        std::fs::write(&moved, "later user work").unwrap();
        assert!(snapshot.restore().is_err());
        assert!(created.exists());
        assert!(!deleted.exists());
        std::fs::write(&moved, b"# coding: latin-1\nvalue='\xe9'\r\n").unwrap();
        snapshot.restore().unwrap();
        assert_eq!(
            std::fs::read(&original).unwrap(),
            b"# coding: latin-1\nvalue='\xe9'\r\n"
        );
        assert_eq!(std::fs::read(&deleted).unwrap(), [0, 255, 128]);
        assert!(!moved.exists());
        assert!(!created.exists());
    }
    #[test]
    fn filesystem_snapshot_acknowledgment_does_not_hide_external_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "value = 1\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        let mut state = EditorState::from_text("value = 1\n", Language::Python);
        state.path = Some(path.clone());
        app.push_tab(state);
        let tree = capture_tree(std::slice::from_ref(&path)).unwrap();
        app.acknowledge_refactoring_snapshot(&tree);
        let baseline = app.disk_revisions[&path];
        std::fs::write(&path, "external = 2\n").unwrap();
        app.acknowledge_refactoring_snapshot(&tree);
        assert_eq!(app.disk_revisions[&path], baseline);
        app.external_changes.push(path.clone());
        app.reconcile_external_changes();
        assert_eq!(app.tabs[0].state.doc.text(), "external = 2\n");
    }

    #[test]
    fn filesystem_only_operation_gets_a_real_undo_boundary_and_restores_open_path() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("a.py");
        let to = dir.path().join("b.py");
        std::fs::write(&from, "value = 1\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        let mut state = EditorState::from_text("value = 1\n", Language::Python);
        state.path = Some(from.clone());
        app.push_tab(state);
        let identity = app.tabs[0].state.doc.identity();
        assert!(app
            .apply_file_ops_failures(vec![FileOp::Move(from.clone(), to.clone())], "Move")
            .is_empty());
        assert_eq!(app.tabs[0].state.path, Some(to.clone()));
        assert_eq!(app.tabs[0].state.doc.identity(), identity);
        assert!(app.tabs[0].state.doc.undo_is_refactoring());
        assert!(app.undo_workspace_refactoring(0));
        assert_eq!(app.tabs[0].state.path, Some(from.clone()));
        assert_eq!(app.tabs[0].state.doc.identity(), identity);
        assert!(!app.tabs[0].state.doc.can_undo());
        assert!(from.exists());
        assert!(!to.exists());
    }
}

impl IdeApp {
    pub(super) fn needs_grouped_refactoring_undo(&self, document: &rustpy_text::Document) -> bool {
        self.refactoring_groups.iter().any(|group| {
            group.documents.iter().any(|(doc, _, after)| {
                doc.identity() == document.identity() && *after == document.current_history()
            })
        })
    }
}
