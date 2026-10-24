//! Document-owned anchors behind persistent line models and sticky highlights.
//! Published rows are a projection, never the source of a tentative tombstone.
use super::*;
use crate::breakpoints::{BreakpointStore, LineBp};
use rustpy_text::{
    Affinity, DocumentId, Edit as DocumentEdit, MarkerId, MarkerPosition, RangeDeletion,
};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct MarkerState {
    docs: HashMap<DocumentId, DocumentMarkers>,
}

#[derive(Default)]
struct DocumentMarkers {
    path: Option<PathBuf>,
    breakpoints: LineRows<LineBp>,
    bookmarks: LineRows<Bookmark>,
    highlights: Vec<MarkerId>,
    retired_highlights: Vec<MarkerId>,
}

struct LineRows<T> {
    entries: Vec<LineRow<T>>,
    published: Vec<(usize, T)>,
}
impl<T> Default for LineRows<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            published: Vec::new(),
        }
    }
}

struct LineRow<T> {
    anchor: LineAnchor,
    value: T,
    published_line: Option<usize>,
    last_visible_line: usize,
    // Explicit model deletion survives text rollback. A tentative text
    // deletion instead keeps its handles and properties until the scope ends.
    retired: bool,
}

// The Document owns logical-line identity and eagerly updates its span for
// every accepted edit. The adapter only projects that identity into persistent
// rows; it must not reconstruct anchors from a bounded edit journal.
struct LineAnchor {
    id: MarkerId,
}
impl LineAnchor {
    fn new(doc: &mut Document, line: usize) -> Option<Self> {
        let line = line
            .saturating_sub(1)
            .min(doc.line_count().saturating_sub(1));
        doc.line_marker(line).ok().map(|id| Self { id })
    }

    fn line(&self, doc: &Document) -> Option<usize> {
        let MarkerPosition::Point(at) = doc.marker(self.id).ok()?? else {
            return None;
        };
        Some(doc.line_of(at) + 1)
    }

    fn remove(&self, doc: &mut Document) {
        let _ = doc.remove_marker(self.id);
    }
}

impl<T: Clone + PartialEq> LineRows<T> {
    fn reconcile(
        &mut self,
        doc: &mut Document,
        current: Vec<(usize, T)>,
        canonical_lines: bool,
    ) -> (Vec<(usize, T)>, bool) {
        let external_change = current != self.published;
        let removed_lines: HashSet<_> = self
            .published
            .iter()
            .filter(|(line, _)| !current.iter().any(|(at, _)| at == line))
            .map(|(line, _)| *line)
            .collect();
        let mut consumed = HashSet::new();
        // Compare with our last publication before consulting moved anchors:
        // this separates a model edit from an edit to the document's text.
        for row in &mut self.entries {
            if row.retired {
                continue;
            }
            // Joined original lines can share one published row. Explicitly
            // deleting that row deletes every owner behind it, including the
            // live collision owners that are temporarily hidden.
            if row
                .anchor
                .line(doc)
                .is_some_and(|line| removed_lines.contains(&line))
            {
                row.retired = true;
                row.anchor.remove(doc);
                continue;
            }
            if let Some(line) = row.published_line {
                if let Some((index, (_, value))) =
                    current.iter().enumerate().find(|(_, (at, _))| *at == line)
                {
                    row.value = value.clone();
                    consumed.insert(index);
                } else {
                    row.retired = true;
                    row.anchor.remove(doc);
                }
            }
        }
        for (index, (line, value)) in current.iter().enumerate() {
            if consumed.contains(&index) {
                continue;
            }
            // Remove/recreate is an explicit new model row, not permission to
            // revive a hidden old row after transaction rollback.
            for row in &mut self.entries {
                if row.published_line.is_none() && row.last_visible_line == *line {
                    row.retired = true;
                    row.anchor.remove(doc);
                }
            }
            if let Some(anchor) = LineAnchor::new(doc, *line) {
                self.entries.push(LineRow {
                    anchor,
                    value: value.clone(),
                    published_line: None,
                    last_visible_line: *line,
                    retired: false,
                });
            }
        }
        let in_transaction = doc.transaction_depth() > 0;
        let mut output = Vec::new();
        let mut occupied = HashSet::new();
        self.entries.retain_mut(|row| {
            let line = (!row.retired).then(|| row.anchor.line(doc)).flatten();
            let visible = line.filter(|line| occupied.insert(*line));
            row.published_line = visible;
            if let Some(line) = visible {
                row.last_visible_line = line;
                output.push((line, row.value.clone()));
            } else if line.is_none() && !in_transaction {
                row.anchor.remove(doc);
                return false;
            }
            true
        });
        if canonical_lines {
            output.sort_by_key(|(line, _)| *line);
        }
        let changed = external_change || output != current;
        self.published = output.clone();
        (output, changed)
    }

    fn remove(&mut self, doc: &mut Document) {
        for row in &mut self.entries {
            row.anchor.remove(doc);
            row.retired = true;
            row.published_line = None;
        }
        self.published.clear();
        self.clean_retired(doc);
    }

    fn clean_retired(&mut self, doc: &mut Document) {
        if doc.transaction_depth() == 0 {
            self.entries.retain(|row| {
                if row.retired {
                    row.anchor.remove(doc);
                }
                !row.retired
            });
        }
    }
}

impl MarkerState {
    fn reconcile(
        &mut self,
        tabs: &mut [Tab],
        breakpoints: &mut BreakpointStore,
        bookmarks: &mut Vec<Bookmark>,
    ) -> Vec<PathBuf> {
        let open: HashSet<_> = tabs.iter().map(|tab| tab.state.doc.identity()).collect();
        self.docs.retain(|id, _| open.contains(id));
        let mut changed_paths = Vec::new();
        for tab in tabs {
            let doc = &mut tab.state.doc;
            let state = self.docs.entry(doc.identity()).or_default();
            if state.path != tab.state.path {
                state.breakpoints.remove(doc);
                state.bookmarks.remove(doc);
                state.path = tab.state.path.clone();
            }
            state.breakpoints.clean_retired(doc);
            state.bookmarks.clean_retired(doc);
            let in_transaction = doc.transaction_depth() > 0;
            // Explicit clear persists through metadata rollback. The backend
            // may restore removed pre-scope IDs, so keep ownership until every
            // enclosing scope ends and remove them again after restoration.
            for id in &state.retired_highlights {
                let _ = doc.remove_marker(*id);
            }
            if !in_transaction {
                state.retired_highlights.clear();
            }
            state.highlights.retain(|id| {
                let visible = matches!(doc.marker(*id), Ok(Some(MarkerPosition::Range(_))));
                if !visible && !in_transaction {
                    let _ = doc.remove_marker(*id);
                }
                visible || in_transaction
            });
            let Some(path) = &state.path else { continue };
            let current = breakpoints
                .lines(path)
                .into_iter()
                .filter_map(|line| breakpoints.get(path, line).cloned().map(|bp| (line, bp)))
                .collect();
            let (rows, changed) = state.breakpoints.reconcile(doc, current, true);
            if changed {
                // Remove every old row before inserting any projected row:
                // adjacent marks cannot overwrite each other while shifting.
                for line in breakpoints.lines(path) {
                    breakpoints.remove(path, line);
                }
                for (line, value) in rows {
                    breakpoints.set(path, line, value);
                }
                changed_paths.push(path.clone());
            }
            let current = bookmarks
                .iter()
                .filter(|mark| {
                    mark.path == *path && mark.kind == rustpy_settings::BookmarkKind::Line
                })
                .map(|mark| (mark.line, mark.clone()))
                .collect();
            let (rows, changed) = state.bookmarks.reconcile(doc, current, false);
            if changed {
                let first = bookmarks
                    .iter()
                    .position(|mark| {
                        mark.path == *path && mark.kind == rustpy_settings::BookmarkKind::Line
                    })
                    .unwrap_or(bookmarks.len());
                bookmarks.retain(|mark| {
                    mark.path != *path || mark.kind != rustpy_settings::BookmarkKind::Line
                });
                let moved = rows.into_iter().map(|(line, mut mark)| {
                    mark.line = line;
                    mark
                });
                bookmarks.splice(
                    first.min(bookmarks.len())..first.min(bookmarks.len()),
                    moved,
                );
                // The externally visible bookmark contains its projected line.
                state.bookmarks.published = bookmarks
                    .iter()
                    .filter(|mark| {
                        mark.path == *path && mark.kind == rustpy_settings::BookmarkKind::Line
                    })
                    .map(|mark| (mark.line, mark.clone()))
                    .collect();
            }
        }
        changed_paths.sort();
        changed_paths.dedup();
        changed_paths
    }

    fn set_highlights(&mut self, doc: &mut Document, ranges: Vec<TextRange>) {
        let state = self.docs.entry(doc.identity()).or_default();
        for id in state.highlights.drain(..) {
            let _ = doc.remove_marker(id);
            if doc.transaction_depth() > 0 {
                state.retired_highlights.push(id);
            }
        }
        state.highlights = ranges
            .into_iter()
            .filter_map(|range| {
                doc.range_marker(
                    range,
                    Affinity::After,
                    Affinity::Before,
                    RangeDeletion::InvalidateOverlap,
                )
                .ok()
            })
            .collect();
    }

    fn highlights(&self, doc: &Document) -> Vec<TextRange> {
        self.docs
            .get(&doc.identity())
            .into_iter()
            .flat_map(|state| &state.highlights)
            .filter_map(|id| match doc.marker(*id) {
                Ok(Some(MarkerPosition::Range(range))) => Some(range),
                _ => None,
            })
            .collect()
    }
}

impl IdeApp {
    pub(super) fn reconcile_document_markers(&mut self) {
        let changed = self.document_markers.reconcile(
            &mut self.tabs,
            &mut self.breakpoints,
            &mut self.bookmarks,
        );
        for path in changed {
            self.sync_breakpoints(&path);
        }
    }

    pub(super) fn set_sticky_highlights(&mut self, i: usize, ranges: Vec<TextRange>) {
        if let Some(tab) = self.tabs.get_mut(i) {
            self.document_markers
                .set_highlights(&mut tab.state.doc, ranges);
        }
    }

    pub(super) fn sticky_highlights_for(&self, i: usize) -> Vec<TextRange> {
        self.tabs
            .get(i)
            .map(|tab| self.document_markers.highlights(&tab.state.doc))
            .unwrap_or_default()
    }

    pub(super) fn clear_sticky_highlights(&mut self) {
        if let Some(i) = self.active {
            self.set_sticky_highlights(i, Vec::new());
        }
    }
}

/// Refine formatter replacements into character-boundary edits. Equal middle
/// spans remain untouched, so their markers survive a coarse formatter hunk.
pub(super) fn refine_formatter_edits(
    doc: &Document,
    edits: &[(TextRange, String)],
) -> Vec<DocumentEdit> {
    use similar::{ChangeTag, TextDiff};
    let mut refined = Vec::new();
    for (range, replacement) in edits {
        if range.end() > doc.len() {
            // Keep the original edit's validation failure rather than silently
            // formatting a clipped range.
            refined.push(DocumentEdit::replace(*range, replacement.clone()));
            continue;
        }
        let replacement = rustpy_text::LineEnding::normalize_to_lf(replacement);
        let range = doc.clamp_range(*range);
        let old = doc.slice(range);
        let diff = TextDiff::configure()
            .timeout(std::time::Duration::from_millis(500))
            .diff_chars(&old, replacement.as_ref());
        let mut at = range.start();
        let mut pending: Option<(TextSize, TextSize, String)> = None;
        for change in diff.iter_all_changes() {
            match change.tag() {
                ChangeTag::Equal => {
                    if let Some((start, end, text)) = pending.take() {
                        refined.push(DocumentEdit::replace(TextRange::new(start, end), text));
                    }
                    at += TextSize::new(change.value().len() as u32);
                }
                ChangeTag::Delete => {
                    let entry = pending.get_or_insert_with(|| (at, at, String::new()));
                    at += TextSize::new(change.value().len() as u32);
                    entry.1 = at;
                }
                ChangeTag::Insert => pending
                    .get_or_insert_with(|| (at, at, String::new()))
                    .2
                    .push_str(change.value()),
            }
        }
        if let Some((start, end, text)) = pending {
            refined.push(DocumentEdit::replace(TextRange::new(start, end), text));
        }
    }
    refined
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_text::EditGroup;

    fn fixture(text: &str) -> (tempfile::TempDir, IdeApp, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.py");
        std::fs::write(&path, text).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(dir.path());
        let mut app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        app.open_file(&path);
        (dir, app, path)
    }

    fn mark(app: &mut IdeApp, path: &Path, line: usize) {
        app.breakpoints.set(
            path,
            line,
            LineBp {
                condition: "keep > 0".into(),
                log_message: "μ {keep}".into(),
                enabled: false,
                ..Default::default()
            },
        );
        app.bookmarks.push(Bookmark {
            kind: rustpy_settings::BookmarkKind::Line,
            list: String::new(),
            path: path.to_path_buf(),
            line,
            mnemonic: Some('K'),
            description: "Keep this line".into(),
        });
        app.reconcile_document_markers();
    }

    fn edit(app: &mut IdeApp, i: usize, edits: Vec<DocumentEdit>) {
        app.tabs[i]
            .state
            .doc
            .edit(edits, &[], &[], EditGroup::Separate, Some("Marker fixture"))
            .unwrap();
    }

    #[test]
    fn joined_distinct_rows_restore_on_undo_and_transaction_rollback() {
        for transaction in [false, true] {
            let (_dir, mut app, path) = fixture("keep = 2\nlast = 3\n");
            mark(&mut app, &path, 1);
            mark(&mut app, &path, 2);
            let mut second = app.breakpoints.get(&path, 2).unwrap().clone();
            second.enabled = true;
            second.condition = "last == 3".into();
            second.log_message = "second original row".into();
            app.breakpoints.set(&path, 2, second);
            app.bookmarks[1].mnemonic = Some('L');
            app.bookmarks[1].description = "Second original row".into();
            app.reconcile_document_markers();
            let original_bp: Vec<_> = [1, 2]
                .into_iter()
                .map(|line| (line, app.breakpoints.get(&path, line).unwrap().clone()))
                .collect();
            let original_bm = app.bookmarks.clone();
            let scope = transaction.then(|| {
                app.tabs[0]
                    .state
                    .doc
                    .begin_transaction(&[], Some("Join rows"))
                    .unwrap()
            });
            edit(
                &mut app,
                0,
                vec![DocumentEdit::delete(TextRange::new(
                    TextSize::new(8),
                    TextSize::new(9),
                ))],
            );
            app.reconcile_document_markers();
            app.reconcile_document_markers();
            assert_eq!(app.breakpoints.lines(&path), vec![1]);
            assert_eq!(app.bookmarks.len(), 1);
            assert_eq!(
                app.document_markers.docs[&app.tabs[0].state.doc.identity()]
                    .breakpoints
                    .entries
                    .len(),
                2
            );
            if let Some(scope) = scope {
                app.tabs[0].state.doc.rollback_transaction(scope).unwrap();
            } else {
                assert!(app.tabs[0].state.undo());
            }
            app.reconcile_document_markers();
            for (line, value) in original_bp {
                assert_eq!(app.breakpoints.get(&path, line), Some(&value));
            }
            assert_eq!(app.bookmarks, original_bm);
        }
    }

    #[test]
    fn explicit_joined_row_deletion_retires_hidden_owners_before_undo() {
        for transaction in [false, true] {
            let (_dir, mut app, path) = fixture("keep = 2\nlast = 3\n");
            mark(&mut app, &path, 1);
            mark(&mut app, &path, 2);
            let scope = transaction.then(|| {
                app.tabs[0]
                    .state
                    .doc
                    .begin_transaction(&[], Some("Explicit joined deletion"))
                    .unwrap()
            });
            edit(
                &mut app,
                0,
                vec![DocumentEdit::delete(TextRange::new(
                    TextSize::new(8),
                    TextSize::new(9),
                ))],
            );
            app.reconcile_document_markers();
            app.breakpoints.remove(&path, 1);
            app.bookmarks.clear();
            app.reconcile_document_markers();
            if let Some(scope) = scope {
                app.tabs[0].state.doc.rollback_transaction(scope).unwrap();
            } else {
                assert!(app.tabs[0].state.undo());
            }
            app.reconcile_document_markers();
            assert!(app.breakpoints.lines(&path).is_empty());
            assert!(app.bookmarks.is_empty());
        }
    }

    #[test]
    fn joined_line_then_rename_preserves_models_through_undo_redo() {
        for reconcile_between in [false, true] {
            let (_dir, mut app, path) = fixture("first = 1\nkeep = 2\nlast = 3\n");
            mark(&mut app, &path, 2);
            let bp = app.breakpoints.get(&path, 2).unwrap().clone();
            let bookmark = app.bookmarks[0].clone();
            edit(
                &mut app,
                0,
                vec![DocumentEdit::delete(TextRange::new(
                    TextSize::new(18),
                    TextSize::new(19),
                ))],
            );
            if reconcile_between {
                app.reconcile_document_markers();
                assert_eq!(app.breakpoints.get(&path, 2), Some(&bp));
                assert_eq!(app.bookmarks, vec![bookmark.clone()]);
            }
            edit(
                &mut app,
                0,
                vec![DocumentEdit::replace(
                    TextRange::new(TextSize::new(10), TextSize::new(14)),
                    "renamed",
                )],
            );
            app.reconcile_document_markers();
            assert_eq!(app.breakpoints.get(&path, 2), Some(&bp));
            assert_eq!(app.bookmarks, vec![bookmark.clone()]);
            for _ in 0..2 {
                assert!(app.tabs[0].state.undo());
                app.reconcile_document_markers();
                assert_eq!(app.breakpoints.get(&path, 2), Some(&bp));
                assert_eq!(app.bookmarks, vec![bookmark.clone()]);
            }
            assert_eq!(
                app.tabs[0].state.doc.text(),
                "first = 1\nkeep = 2\nlast = 3\n"
            );
            for _ in 0..2 {
                assert!(app.tabs[0].state.redo());
                app.reconcile_document_markers();
                assert_eq!(app.breakpoints.get(&path, 2), Some(&bp));
                assert_eq!(app.bookmarks, vec![bookmark.clone()]);
            }
        }
    }

    #[test]
    fn joined_line_and_rename_survive_intermediate_transaction_rollback() {
        let (_dir, mut app, path) = fixture("first = 1\nkeep = 2\nlast = 3\n");
        mark(&mut app, &path, 2);
        let bp = app.breakpoints.get(&path, 2).unwrap().clone();
        let bookmark = app.bookmarks[0].clone();
        let scope = app.tabs[0]
            .state
            .doc
            .begin_transaction(&[], Some("Join and rename"))
            .unwrap();
        edit(
            &mut app,
            0,
            vec![DocumentEdit::delete(TextRange::new(
                TextSize::new(18),
                TextSize::new(19),
            ))],
        );
        app.reconcile_document_markers();
        assert_eq!(app.breakpoints.get(&path, 2), Some(&bp));
        edit(
            &mut app,
            0,
            vec![DocumentEdit::replace(
                TextRange::new(TextSize::new(10), TextSize::new(14)),
                "renamed",
            )],
        );
        app.reconcile_document_markers();
        assert_eq!(app.breakpoints.get(&path, 2), Some(&bp));
        assert_eq!(app.bookmarks, vec![bookmark.clone()]);
        app.tabs[0].state.doc.rollback_transaction(scope).unwrap();
        app.reconcile_document_markers();
        assert_eq!(app.breakpoints.get(&path, 2), Some(&bp));
        assert_eq!(app.bookmarks, vec![bookmark]);
        assert_eq!(
            app.tabs[0].state.doc.text(),
            "first = 1\nkeep = 2\nlast = 3\n"
        );
    }

    #[test]
    fn inactive_documents_keep_adjacent_models_past_journal_expiry() {
        let (dir, mut app, path) = fixture("first = 1\nkeep = 2\nlast = 3\n");
        mark(&mut app, &path, 2);
        app.breakpoints.set(
            &path,
            1,
            LineBp {
                temporary: true,
                ..Default::default()
            },
        );
        app.reconcile_document_markers();
        let second = dir.path().join("other.py");
        std::fs::write(&second, "other = 1\n").unwrap();
        app.open_file(&second);
        assert_eq!(app.active, Some(1));
        for _ in 0..70 {
            edit(
                &mut app,
                0,
                vec![DocumentEdit::insert(TextSize::new(0), "# inserted\n")],
            );
        }
        app.reconcile_document_markers();
        assert_eq!(app.breakpoints.lines(&path), vec![71, 72]);
        assert!(app.breakpoints.get(&path, 71).unwrap().temporary);
        assert_eq!(
            app.breakpoints.get(&path, 72).unwrap().condition,
            "keep > 0"
        );
        assert!(!app.breakpoints.get(&path, 72).unwrap().enabled);
        assert_eq!(app.bookmarks[0].line, 72);
        assert_eq!(app.bookmarks[0].description, "Keep this line");
    }

    #[test]
    fn net_zero_line_edits_and_body_renames_do_not_use_a_global_delta() {
        let (_dir, mut app, path) = fixture("first = 1\nkeep = 2\nlast = 3\n");
        mark(&mut app, &path, 2);
        edit(
            &mut app,
            0,
            vec![DocumentEdit::insert(TextSize::new(0), "# above\n")],
        );
        let last = app.tabs[0].state.doc.line_range_with_newline(3);
        edit(&mut app, 0, vec![DocumentEdit::delete(last)]);
        app.reconcile_document_markers();
        assert_eq!(app.breakpoints.lines(&path), vec![3]);
        let start = app.tabs[0].state.doc.line_range(2).start();
        edit(
            &mut app,
            0,
            vec![DocumentEdit::replace(
                TextRange::new(start, start + TextSize::new(4)),
                "renamed",
            )],
        );
        app.reconcile_document_markers();
        assert_eq!(app.breakpoints.lines(&path), vec![3]);
        assert_eq!(app.bookmarks[0].line, 3);
    }

    #[test]
    fn original_empty_lines_follow_insertions_at_their_start() {
        for text in ["a\n\nb\n", "a\n"] {
            let (_dir, mut app, path) = fixture(text);
            mark(&mut app, &path, 2);
            edit(
                &mut app,
                0,
                vec![DocumentEdit::insert(TextSize::new(2), "#x\n")],
            );
            app.reconcile_document_markers();
            assert_eq!(app.breakpoints.lines(&path), vec![3], "{text:?}");
            assert_eq!(app.bookmarks[0].line, 3, "{text:?}");
        }
    }

    #[test]
    fn deleting_final_content_empty_and_sole_lines_retires_models() {
        for (text, line) in [
            ("first = 1\nlast = 2", 2),
            ("first = 1\n", 2),
            ("last = 2", 1),
        ] {
            let (_dir, mut app, path) = fixture(text);
            mark(&mut app, &path, line);
            app.tabs[0].state.go_to_line(line, None);
            app.tabs[0].state.delete_lines();
            app.reconcile_document_markers();
            assert!(app.breakpoints.lines(&path).is_empty(), "{text:?}");
            assert!(app.bookmarks.is_empty(), "{text:?}");
        }
    }

    #[test]
    fn intermediate_transaction_projection_retains_tombstone_properties_for_rollback() {
        let (_dir, mut app, path) = fixture("first = 1\nkeep = 2\nlast = 3\n");
        mark(&mut app, &path, 2);
        app.breakpoints.muted = true;
        let highlight = app.tabs[0].state.doc.line_range(1);
        app.set_sticky_highlights(0, vec![highlight]);
        let token = app.tabs[0]
            .state
            .doc
            .begin_transaction(&[], Some("Tentative delete"))
            .unwrap();
        let deleted = app.tabs[0].state.doc.line_range_with_newline(1);
        edit(&mut app, 0, vec![DocumentEdit::delete(deleted)]);
        app.reconcile_document_markers();
        app.reconcile_document_markers();
        assert!(app.breakpoints.lines(&path).is_empty());
        assert!(app.bookmarks.is_empty());
        assert!(app.sticky_highlights_for(0).is_empty());
        app.tabs[0].state.doc.rollback_transaction(token).unwrap();
        app.reconcile_document_markers();
        assert_eq!(app.breakpoints.lines(&path), vec![2]);
        assert_eq!(
            app.breakpoints.get(&path, 2).unwrap().log_message,
            "μ {keep}"
        );
        assert!(app.breakpoints.muted);
        assert_eq!(app.bookmarks[0].mnemonic, Some('K'));
        assert_eq!(app.sticky_highlights_for(0), vec![highlight]);
    }

    #[test]
    fn explicit_model_deletion_and_property_edits_are_not_text_rollback() {
        let (_dir, mut app, path) = fixture("first = 1\nkeep = 2\nlast = 3\n");
        mark(&mut app, &path, 2);
        let token = app.tabs[0].state.doc.begin_transaction(&[], None).unwrap();
        let mut bp = app.breakpoints.get(&path, 2).unwrap().clone();
        bp.hit_condition = ">= 7".into();
        app.breakpoints.set(&path, 2, bp);
        app.bookmarks[0].description = "Updated externally".into();
        edit(
            &mut app,
            0,
            vec![DocumentEdit::insert(TextSize::new(0), "# above\n")],
        );
        app.reconcile_document_markers();
        assert_eq!(app.breakpoints.get(&path, 3).unwrap().hit_condition, ">= 7");
        app.breakpoints.remove(&path, 3);
        app.bookmarks.clear();
        app.reconcile_document_markers();
        app.tabs[0].state.doc.rollback_transaction(token).unwrap();
        app.reconcile_document_markers();
        assert!(app.breakpoints.lines(&path).is_empty());
        assert!(app.bookmarks.is_empty());
    }

    #[test]
    fn commit_finalizes_deleted_handles_even_without_a_new_text_version() {
        let (_dir, mut app, path) = fixture("first = 1\nkeep = 2\n");
        mark(&mut app, &path, 2);
        let token = app.tabs[0].state.doc.begin_transaction(&[], None).unwrap();
        let range = app.tabs[0].state.doc.line_range_with_newline(1);
        edit(&mut app, 0, vec![DocumentEdit::delete(range)]);
        app.reconcile_document_markers();
        let version = app.tabs[0].state.doc.version();
        app.tabs[0]
            .state
            .doc
            .commit_transaction(token, &[])
            .unwrap();
        assert_eq!(app.tabs[0].state.doc.version(), version);
        app.reconcile_document_markers();
        let id = app.tabs[0].state.doc.identity();
        assert!(app.document_markers.docs[&id]
            .breakpoints
            .entries
            .is_empty());
        assert!(app.document_markers.docs[&id].bookmarks.entries.is_empty());
    }

    #[test]
    fn sticky_ranges_are_per_document_and_reset_or_closed_ids_are_pruned() {
        let (dir, mut app, _path) = fixture("first = 1\nkeep = 2\n");
        let range = app.tabs[0].state.doc.line_range(1);
        app.set_sticky_highlights(0, vec![range]);
        let other = dir.path().join("other.py");
        std::fs::write(&other, "other = 1\n").unwrap();
        app.open_file(&other);
        let second = app.tabs[1].state.doc.line_range(0);
        app.set_sticky_highlights(1, vec![second]);
        assert_eq!(
            app.tabs[0].state.doc.version(),
            app.tabs[1].state.doc.version()
        );
        app.clear_sticky_highlights();
        assert!(app.sticky_highlights_for(1).is_empty());
        assert_eq!(app.sticky_highlights_for(0), vec![range]);
        let old = app.tabs[0].state.doc.identity();
        app.tabs[0].state.doc.reset("replacement\n");
        app.reconcile_document_markers();
        assert!(!app.document_markers.docs.contains_key(&old));
        assert!(app.sticky_highlights_for(0).is_empty());
        let closed = app.tabs[1].state.doc.identity();
        app.tabs.remove(1);
        app.active = Some(0);
        app.reconcile_document_markers();
        assert!(!app.document_markers.docs.contains_key(&closed));
    }

    #[test]
    fn metadata_only_rollback_expires_new_highlights_without_version_changes() {
        let (_dir, mut app, _path) = fixture("keep = 2\n");
        let version = app.tabs[0].state.doc.version();
        let token = app.tabs[0].state.doc.begin_transaction(&[], None).unwrap();
        let range = app.tabs[0].state.doc.line_range(0);
        app.set_sticky_highlights(0, vec![range]);
        app.tabs[0].state.doc.rollback_transaction(token).unwrap();
        assert_eq!(app.tabs[0].state.doc.version(), version);
        app.reconcile_document_markers();
        assert!(app.sticky_highlights_for(0).is_empty());
    }

    #[test]
    fn formatter_refinement_preserves_middle_markers_unicode_offsets_and_atomic_undo() {
        let old = "x=1\nkeep = 2\ny=3\n# μ😀\n";
        let new = "x = 1\nkeep = 2\ny = 3\n# μ😀\n";
        let (_dir, mut app, path) = fixture(old);
        mark(&mut app, &path, 2);
        let range = app.tabs[0].state.doc.line_range(1);
        app.set_sticky_highlights(0, vec![range]);
        let doc = &app.tabs[0].state.doc;
        let refined = refine_formatter_edits(
            doc,
            &[(TextRange::new(TextSize::new(0), doc.len()), new.into())],
        );
        assert!(refined.iter().all(|edit| {
            !edit
                .range
                .intersect(range)
                .is_some_and(|intersection| !intersection.is_empty())
        }));
        edit(&mut app, 0, refined);
        app.reconcile_document_markers();
        assert_eq!(app.tabs[0].state.doc.text(), new);
        assert_eq!(app.breakpoints.lines(&path), vec![2]);
        assert_eq!(app.bookmarks[0].line, 2);
        assert_eq!(
            app.sticky_highlights_for(0),
            vec![TextRange::new(
                range.start() + TextSize::new(2),
                range.end() + TextSize::new(2)
            )]
        );
        app.tabs[0].state.undo();
        app.reconcile_document_markers();
        assert_eq!(app.tabs[0].state.doc.text(), old);
        assert_eq!(app.sticky_highlights_for(0), vec![range]);
        let doc = &app.tabs[0].state.doc;
        let unicode = doc.line_range(3);
        let refined = refine_formatter_edits(doc, &[(unicode, "# Ω😀!".into())]);
        edit(&mut app, 0, refined);
        assert_eq!(app.tabs[0].state.doc.line_text(3), "# Ω😀!");
    }
    #[test]
    fn formatter_refinement_normalizes_whole_replacements_before_diffing() {
        for formatted in ["x = 2\r\nkeep = 2\r\n", "x = 2\rkeep = 2\r\n"] {
            let mut coarse = Document::new("x = 1\nkeep = 2\n");
            let whole = TextRange::new(TextSize::new(0), coarse.len());
            let mut refined = Document::new(&coarse.text());
            let edits = refine_formatter_edits(&refined, &[(whole, formatted.into())]);
            coarse
                .edit(
                    vec![DocumentEdit::replace(whole, formatted)],
                    &[],
                    &[],
                    EditGroup::Separate,
                    None,
                )
                .unwrap();
            refined
                .edit(edits, &[], &[], EditGroup::Separate, None)
                .unwrap();
            assert_eq!(refined.text(), coarse.text());
            assert_eq!(refined.text(), "x = 2\nkeep = 2\n");
        }
        let mut doc = Document::new("# μ😀\nx = 1\nkeep = 2\n");
        let selected = doc.line_range_with_newline(1);
        let edits = refine_formatter_edits(&doc, &[(selected, "x = 2\r\n".into())]);
        doc.edit(edits, &[], &[], EditGroup::Separate, None)
            .unwrap();
        assert_eq!(doc.text(), "# μ😀\nx = 2\nkeep = 2\n");
    }

    #[test]
    fn expired_highlights_remove_registry_metadata_after_commit() {
        let (_dir, mut app, _path) = fixture("keep = 2\n");
        let range = app.tabs[0].state.doc.line_range(0);
        app.set_sticky_highlights(0, vec![range]);
        let id = app.document_markers.docs[&app.tabs[0].state.doc.identity()].highlights[0];
        edit(&mut app, 0, vec![DocumentEdit::delete(range)]);
        app.reconcile_document_markers();
        assert!(app.tabs[0].state.doc.marker(id).is_err());
        assert!(app.sticky_highlights_for(0).is_empty());
    }
    #[test]
    fn adding_lower_breakpoints_has_a_stable_idle_projection() {
        let (_dir, mut app, path) = fixture("one\ntwo\nthree\nfour\n");
        app.breakpoints.set(&path, 4, LineBp::default());
        app.reconcile_document_markers();
        app.breakpoints.set(
            &path,
            2,
            LineBp {
                condition: "updated".into(),
                ..Default::default()
            },
        );
        app.reconcile_document_markers();
        assert_eq!(app.breakpoints.lines(&path), vec![2, 4]);
        let changed =
            app.document_markers
                .reconcile(&mut app.tabs, &mut app.breakpoints, &mut app.bookmarks);
        assert!(
            changed.is_empty(),
            "idle snapshots must not resend DAP breakpoints"
        );
        assert_eq!(app.breakpoints.get(&path, 2).unwrap().condition, "updated");
    }
    #[test]
    fn explicit_highlight_clear_does_not_leak_restored_metadata() {
        let (_dir, mut app, _path) = fixture("keep = 2\n");
        let range = app.tabs[0].state.doc.line_range(0);
        app.set_sticky_highlights(0, vec![range]);
        let id = app.document_markers.docs[&app.tabs[0].state.doc.identity()].highlights[0];
        let outer = app.tabs[0].state.doc.begin_transaction(&[], None).unwrap();
        let inner = app.tabs[0].state.doc.begin_transaction(&[], None).unwrap();
        app.clear_sticky_highlights();
        app.tabs[0].state.doc.rollback_transaction(inner).unwrap();
        app.reconcile_document_markers();
        assert!(app.tabs[0].state.doc.marker(id).is_err());
        app.tabs[0].state.doc.rollback_transaction(outer).unwrap();
        app.reconcile_document_markers();
        assert!(app.sticky_highlights_for(0).is_empty());
        assert!(app.tabs[0].state.doc.marker(id).is_err());
        assert!(app.document_markers.docs[&app.tabs[0].state.doc.identity()]
            .retired_highlights
            .is_empty());
    }

    #[test]
    fn actual_reformat_and_cleanup_preserve_models_selection_and_one_undo_unit() {
        let old = "x=1\nkeep = 2\ny=3\n";
        let expected = "x = 1\nkeep = 2\ny = 3\n";
        for cleanup in [false, true] {
            let (_dir, mut app, path) = fixture(old);
            app.settings.code_style.formatter = "builtin".into();
            mark(&mut app, &path, 2);
            let original_range = app.tabs[0].state.doc.line_range(1);
            app.set_sticky_highlights(0, vec![original_range]);
            let original_selection = TextRange::new(TextSize::new(0), app.tabs[0].state.doc.len());
            app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
                original_selection.start(),
                original_selection.end(),
            )]);
            let history = app.tabs[0].state.doc.undo_history().len();
            if cleanup {
                app.code_cleanup();
            } else {
                app.reformat_code();
            }
            app.reconcile_document_markers();
            assert_eq!(app.tabs[0].state.doc.text(), expected, "cleanup={cleanup}");
            assert_eq!(app.tabs[0].state.doc.transaction_depth(), 0);
            assert_eq!(app.tabs[0].state.doc.undo_history().len(), history + 1);
            assert_eq!(
                app.tabs[0].state.primary().range(),
                TextRange::new(TextSize::new(0), app.tabs[0].state.doc.len())
            );
            assert_eq!(app.breakpoints.lines(&path), vec![2]);
            assert_eq!(app.bookmarks[0].line, 2);
            assert_eq!(
                app.sticky_highlights_for(0),
                vec![TextRange::new(
                    original_range.start() + TextSize::new(2),
                    original_range.end() + TextSize::new(2)
                )]
            );
            app.tabs[0].state.undo();
            app.reconcile_document_markers();
            assert_eq!(app.tabs[0].state.doc.text(), old);
            assert_eq!(app.tabs[0].state.primary().range(), original_selection);
            assert_eq!(app.sticky_highlights_for(0), vec![original_range]);
            assert_eq!(app.breakpoints.lines(&path), vec![2]);
        }
    }
    #[test]
    fn path_change_during_metadata_scope_keeps_cleanup_ownership() {
        for untitled in [false, true] {
            let (_dir, mut app, path) = fixture("keep = 2\n");
            mark(&mut app, &path, 1);
            let doc = app.tabs[0].state.doc.identity();
            let bp = app.document_markers.docs[&doc].breakpoints.entries[0]
                .anchor
                .id;
            let bookmark = app.document_markers.docs[&doc].bookmarks.entries[0]
                .anchor
                .id;
            let token = app.tabs[0].state.doc.begin_transaction(&[], None).unwrap();
            app.tabs[0].state.path = if untitled {
                None
            } else {
                Some(path.with_file_name("renamed.py"))
            };
            app.reconcile_document_markers();
            app.tabs[0].state.doc.rollback_transaction(token).unwrap();
            app.reconcile_document_markers();
            assert!(app.tabs[0].state.doc.marker(bp).is_err());
            assert!(app.tabs[0].state.doc.marker(bookmark).is_err());
            assert!(app.document_markers.docs[&doc]
                .breakpoints
                .entries
                .is_empty());
            assert!(app.document_markers.docs[&doc].bookmarks.entries.is_empty());
            assert_eq!(
                app.breakpoints.lines(&path),
                vec![1],
                "old persisted path models stay intact"
            );
        }
    }
    #[test]
    fn whole_file_bookmark_and_named_line_membership_survive_line_edit_and_undo() {
        let (_root, mut app, path) = fixture("π = 1\nvalue = 2\n");
        app.bookmarks.push(Bookmark {
            path: path.clone(),
            line: 0,
            mnemonic: None,
            description: "whole file".into(),
            kind: rustpy_settings::BookmarkKind::File,
            list: "Files".into(),
        });
        app.bookmarks.push(Bookmark {
            path: path.clone(),
            line: 2,
            mnemonic: Some('2'),
            description: "line".into(),
            kind: rustpy_settings::BookmarkKind::Line,
            list: "Review".into(),
        });
        app.reconcile_document_markers();
        edit(
            &mut app,
            0,
            vec![DocumentEdit::insert(TextSize::new(0), "# above\n")],
        );
        app.reconcile_document_markers();
        assert_eq!(app.bookmarks.len(), 2);
        let file = app
            .bookmarks
            .iter()
            .find(|b| b.kind == rustpy_settings::BookmarkKind::File)
            .unwrap();
        assert_eq!(
            (file.line, file.list.as_str(), file.description.as_str()),
            (0, "Files", "whole file")
        );
        let line = app
            .bookmarks
            .iter()
            .find(|b| b.kind == rustpy_settings::BookmarkKind::Line)
            .unwrap();
        assert_eq!((line.line, line.list.as_str()), (3, "Review"));
        app.tabs[0].state.undo();
        app.reconcile_document_markers();
        assert_eq!(app.bookmarks.len(), 2);
        assert_eq!(
            app.bookmarks
                .iter()
                .find(|b| b.kind == rustpy_settings::BookmarkKind::Line)
                .unwrap()
                .line,
            2
        );
        assert_eq!(
            app.bookmarks
                .iter()
                .find(|b| b.kind == rustpy_settings::BookmarkKind::File)
                .unwrap()
                .line,
            0
        );
    }
}
