//! Emacs editing commands backed by the real editor history, independent of the clipboard.
use std::collections::VecDeque;

use rustpy_core::TextRange;
use rustpy_text::{DocumentId, Edit, EditGroup};

use super::state::virtual_space::VirtualCaret;
use super::state::{map_offset, Caret, EditOptions, EditorState, LastOp};

const RING_ENTRIES: usize = 32;
const RING_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kill {
    WordForward,
    WordBackward,
    LineForward,
    LineBackward,
    Region,
}

#[derive(Clone, Debug)]
struct Stamp {
    document: DocumentId,
    version: u64,
    carets: Vec<Caret>,
    virtual_carets: Vec<VirtualCaret>,
}
impl Stamp {
    fn new(editor: &EditorState) -> Self {
        Self {
            document: editor.doc.identity(),
            version: editor.doc.version(),
            carets: editor.carets.clone(),
            virtual_carets: (0..editor.carets.len())
                .map(|i| editor.virtual_caret(i))
                .collect(),
        }
    }
    fn matches(&self, editor: &EditorState) -> bool {
        self.document == editor.doc.identity()
            && self.version == editor.doc.version()
            && self.carets == editor.carets
            && self.virtual_carets
                == (0..editor.carets.len())
                    .map(|i| editor.virtual_caret(i))
                    .collect::<Vec<_>>()
    }
}

#[derive(Debug)]
struct Yank {
    stamp: Stamp,
    ranges: Vec<TextRange>,
    index: usize,
}

/// Keep one instance at the application level to share the ring between editors.
/// Call `interrupt` on any intervening non-Emacs command or user interaction.
#[derive(Debug, Default)]
pub struct EmacsEditing {
    ring: VecDeque<Vec<String>>,
    kill: Option<Stamp>,
    yank: Option<Yank>,
}

impl EmacsEditing {
    pub fn interrupt(&mut self) {
        self.kill = None;
        self.yank = None;
    }

    pub fn ring_len(&self) -> usize {
        self.ring.len()
    }

    /// Clipboard representation of the newest entry, without exposing ring storage.
    pub fn latest_text(&self) -> Option<String> {
        self.ring.front().map(|pieces| pieces.join("\n"))
    }

    /// Emacs kill-ring-save copies selected regions without changing the document.
    pub fn copy_region(&mut self, editor: &EditorState) -> bool {
        self.interrupt();
        if !editor.has_selection() {
            return false;
        }
        let entry: Vec<_> = editor
            .carets
            .iter()
            .enumerate()
            .map(|(i, _)| editor.selected_caret_text(i))
            .collect();
        if entry.iter().map(String::len).sum::<usize>() > RING_BYTES {
            return false;
        }
        self.ring.push_front(entry);
        self.trim();
        true
    }

    fn trim(&mut self) {
        while self.ring.len() > RING_ENTRIES
            || self.ring.iter().flatten().map(String::len).sum::<usize>() > RING_BYTES
        {
            self.ring.pop_back();
        }
    }

    /// Selected regions take precedence over the directional movement target.
    /// Refuses an oversized kill rather than deleting text that cannot be retained.
    pub fn kill(&mut self, editor: &mut EditorState, command: Kill, options: &EditOptions) -> bool {
        if editor.has_virtual_padding() {
            return self.kill_virtual(editor, command, options);
        }
        let chain = self
            .kill
            .as_ref()
            .is_some_and(|stamp| stamp.matches(editor));
        self.yank = None;
        let backward = matches!(command, Kill::WordBackward | Kill::LineBackward);
        let mut ranges: Vec<TextRange> = editor
            .carets
            .iter()
            .map(|caret| {
                if caret.has_selection() {
                    return caret.range();
                }
                let head = caret.head();
                let line = editor.doc.line_range(editor.doc.line_of(head));
                let target = match command {
                    Kill::WordForward => editor.word_right(head, options),
                    Kill::WordBackward => editor.word_left(head, options),
                    Kill::LineForward => {
                        if head == line.end() {
                            editor.doc.next_boundary(head)
                        } else {
                            line.end()
                        }
                    }
                    Kill::LineBackward => {
                        if head == line.start() {
                            editor.doc.prev_boundary(head)
                        } else {
                            line.start()
                        }
                    }
                    Kill::Region => head,
                };
                TextRange::new(head.min(target), head.max(target))
            })
            .filter(|range| !range.is_empty())
            .collect();
        ranges.sort_by_key(|range| (range.start(), range.end()));
        let mut merged: Vec<TextRange> = Vec::new();
        for range in ranges {
            if let Some(previous) = merged.last_mut() {
                if range.start() <= previous.end() {
                    *previous = TextRange::new(previous.start(), previous.end().max(range.end()));
                    continue;
                }
            }
            merged.push(range);
        }
        if merged.is_empty() {
            self.kill = None;
            return false;
        }
        let edits: Vec<_> = merged.iter().copied().map(Edit::delete).collect();
        let carets: Vec<_> = editor
            .carets
            .iter()
            .map(|caret| Caret::at(map_offset(&edits, caret.head(), false)))
            .collect();
        // Normalize caret merging exactly as EditorState does, while retaining the
        // primary caret order. No editor mutation is performed before apply succeeds.
        let mut unique = Vec::new();
        for caret in &carets {
            if !unique.contains(caret) {
                unique.push(*caret);
            }
        }
        let pieces: Vec<String> = unique
            .iter()
            .map(|caret| {
                merged
                    .iter()
                    .find(|range| map_offset(&edits, range.start(), false) == caret.head())
                    .map(|range| editor.doc.slice(*range))
                    .unwrap_or_default()
            })
            .collect();
        let mut entry = pieces.clone();
        if chain {
            if let Some(previous) = self
                .ring
                .front()
                .filter(|previous| previous.len() == pieces.len())
            {
                entry = previous
                    .iter()
                    .zip(&pieces)
                    .map(|(old, new)| {
                        if backward {
                            format!("{new}{old}")
                        } else {
                            format!("{old}{new}")
                        }
                    })
                    .collect();
            }
        }
        if entry.iter().map(String::len).sum::<usize>() > RING_BYTES {
            self.kill = None;
            return false;
        }
        if !editor.apply(edits, carets, EditGroup::Open, Some("Kill")) {
            self.kill = None;
            return false;
        }
        // set_carets preserves primary first but sorts the other carets. Arrange
        // corresponding ring pieces in that exact post-edit order.
        let entry: Vec<_> = editor
            .carets
            .iter()
            .map(|caret| entry[unique.iter().position(|item| item == caret).unwrap()].clone())
            .collect();
        if chain
            && self
                .ring
                .front()
                .is_some_and(|old| old.len() == entry.len())
        {
            self.ring.pop_front();
        }
        self.ring.push_front(entry);
        self.trim();
        editor.last_op = LastOp::Other;
        editor.scroll_to_caret = true;
        self.kill = Some(Stamp::new(editor));
        true
    }

    fn kill_virtual(
        &mut self,
        editor: &mut EditorState,
        command: Kill,
        options: &EditOptions,
    ) -> bool {
        let chain = self
            .kill
            .as_ref()
            .is_some_and(|stamp| stamp.matches(editor));
        self.yank = None;
        if editor.doc.is_read_only() {
            editor.read_only_refused = true;
            self.kill = None;
            return false;
        }
        let positions: Vec<_> = (0..editor.carets.len())
            .map(|i| {
                let p = editor.virtual_caret(i);
                if p.anchor != p.head {
                    return p;
                }
                let caret = &editor.carets[i];
                let line = editor.doc.line_range(editor.doc.line_of(caret.head()));
                let virtual_head = editor.virtual_excess(i) > 0;
                let target = match command {
                    Kill::WordForward if virtual_head => return p,
                    Kill::LineForward if virtual_head => return p,
                    Kill::WordBackward if virtual_head => line.end(),
                    Kill::WordForward => editor.word_right(caret.head(), options),
                    Kill::WordBackward => editor.word_left(caret.head(), options),
                    Kill::LineForward => {
                        if caret.head() == line.end() {
                            editor.doc.next_boundary(caret.head())
                        } else {
                            line.end()
                        }
                    }
                    Kill::LineBackward => {
                        if caret.head() == line.start() {
                            editor.doc.prev_boundary(caret.head())
                        } else {
                            line.start()
                        }
                    }
                    Kill::Region => return p,
                };
                VirtualCaret {
                    anchor: p.head,
                    head: editor.doc.offset_to_position(target),
                }
            })
            .collect();
        if positions.iter().all(|p| p.anchor == p.head) {
            self.kill = None;
            return false;
        }
        let pieces: Vec<_> = positions
            .iter()
            .map(|p| editor.virtual_selection_text(*p))
            .collect();
        let backward = matches!(command, Kill::WordBackward | Kill::LineBackward);
        let mut entry = pieces.clone();
        if chain {
            if let Some(previous) = self
                .ring
                .front()
                .filter(|previous| previous.len() == pieces.len())
            {
                entry = previous
                    .iter()
                    .zip(&pieces)
                    .map(|(old, new)| {
                        if backward {
                            format!("{new}{old}")
                        } else {
                            format!("{old}{new}")
                        }
                    })
                    .collect();
            }
        }
        if entry.iter().map(String::len).sum::<usize>() > RING_BYTES {
            self.kill = None;
            return false;
        }
        let before = editor.virtual_history_snapshot();
        let token = match editor.begin_transaction("Kill") {
            Ok(token) => token,
            Err(_) => {
                editor.read_only_refused = true;
                self.kill = None;
                return false;
            }
        };
        editor.set_virtual_carets(positions);
        // Selection normalization can coalesce overlapping kills. Capture the
        // actual normalized pieces rather than carrying old caret indices.
        let normalized: Vec<_> = (0..editor.carets.len())
            .map(|i| editor.selected_caret_text(i))
            .collect();
        if normalized.len() != entry.len() {
            entry = normalized;
        }
        editor.delete_selection();
        editor
            .commit_transaction(token)
            .expect("virtual kill owns transaction");
        editor.remember_virtual_history(before);
        if chain
            && self
                .ring
                .front()
                .is_some_and(|old| old.len() == entry.len())
        {
            self.ring.pop_front();
        }
        self.ring.push_front(entry);
        self.trim();
        self.kill = Some(Stamp::new(editor));
        editor.last_op = LastOp::Other;
        true
    }

    pub fn yank(&mut self, editor: &mut EditorState) -> bool {
        if !self.ring.is_empty() {
            if let Some(result) = editor.try_edit_in_virtual_space("Yank", |state| self.yank(state))
            {
                return result;
            }
        }
        self.interrupt();
        if self.ring.is_empty() {
            return false;
        }
        let ranges = editor.carets.iter().map(Caret::range).collect();
        self.replace_yank(editor, ranges, 0, "Yank")
    }

    /// Only the immediately preceding successful yank/yank-pop may be replaced.
    /// Document edits, undo, caret moves, editor switches and explicit interruption
    /// invalidate this authority; no textual search or stale offset replacement.
    pub fn yank_pop(&mut self, editor: &mut EditorState) -> bool {
        self.kill = None;
        let Some(previous) = self.yank.take() else {
            return false;
        };
        if !previous.stamp.matches(editor) || self.ring.is_empty() {
            return false;
        }
        self.replace_yank(
            editor,
            previous.ranges,
            (previous.index + 1) % self.ring.len(),
            "Yank Pop",
        )
    }

    fn replace_yank(
        &mut self,
        editor: &mut EditorState,
        ranges: Vec<TextRange>,
        index: usize,
        label: &str,
    ) -> bool {
        let entry = &self.ring[index];
        let joined;
        let pieces: Vec<&str> = if entry.len() == ranges.len() {
            entry.iter().map(String::as_str).collect()
        } else {
            joined = entry.join("\n");
            vec![joined.as_str(); ranges.len()]
        };
        let mut edits: Vec<_> = ranges
            .iter()
            .zip(pieces)
            .map(|(range, text)| Edit::replace(*range, text))
            .collect();
        edits.sort_by_key(|edit| (edit.range.start(), edit.range.end()));
        if edits
            .windows(2)
            .any(|pair| pair[0].range.end() > pair[1].range.start())
        {
            return false;
        }
        let inserted: Vec<_> = ranges
            .iter()
            .map(|range| {
                let start = map_offset(&edits, range.start(), false);
                let end = map_offset(&edits, range.end(), true);
                TextRange::new(start, end)
            })
            .collect();
        let carets = inserted
            .iter()
            .map(|range| Caret::at(range.end()))
            .collect();
        if !editor.apply(edits, carets, EditGroup::Open, Some(label)) {
            return false;
        }
        editor.last_op = LastOp::Other;
        editor.scroll_to_caret = true;
        // Normalize inserted ranges to match the final primary-first caret order.
        let ranges = editor
            .carets
            .iter()
            .map(|caret| {
                *inserted
                    .iter()
                    .find(|range| range.end() == caret.head())
                    .unwrap()
            })
            .collect();
        self.yank = Some(Yank {
            stamp: Stamp::new(editor),
            ranges,
            index,
        });
        true
    }

    /// Swap adjacent Unicode scalar characters, advancing the caret. At line end
    /// swap the two preceding characters. At a line start, the preceding newline
    /// participates like any other character, matching Emacs transpose-chars.
    /// Overlapping multicaret swaps are performed once, not applied twice.
    pub fn transpose(&mut self, editor: &mut EditorState) -> bool {
        if let Some(result) =
            editor.try_edit_in_virtual_space("Transpose", |state| self.transpose(state))
        {
            return result;
        }
        self.interrupt();
        let mut edits = Vec::new();
        let mut targets = Vec::new();
        for caret in &editor.carets {
            if caret.has_selection() {
                targets.push(*caret);
                continue;
            }
            let line = editor.doc.line_range(editor.doc.line_of(caret.head()));
            let mut middle = caret.head();
            if middle == line.end() {
                middle = editor.doc.prev_boundary(middle);
            }
            if u32::from(middle) == 0 || middle == editor.doc.len() {
                targets.push(*caret);
                continue;
            }
            let start = editor.doc.prev_boundary(middle);
            let end = editor.doc.next_boundary(middle);
            let text = format!(
                "{}{}",
                editor.doc.slice(TextRange::new(middle, end)),
                editor.doc.slice(TextRange::new(start, middle))
            );
            let range = TextRange::new(start, end);
            if !edits.iter().any(|edit: &Edit| {
                edit.range.start() < range.end() && range.start() < edit.range.end()
            }) {
                edits.push(Edit::replace(range, text));
                targets.push(Caret::at(end));
            } else {
                targets.push(*caret);
            }
        }
        if edits.is_empty() {
            return false;
        }
        edits.sort_by_key(|edit| (edit.range.start(), edit.range.end()));
        let carets = targets
            .into_iter()
            .map(|caret| {
                Caret::new(
                    map_offset(&edits, caret.sel.anchor, true),
                    map_offset(&edits, caret.head(), true),
                )
            })
            .collect();
        let changed = editor.apply(edits, carets, EditGroup::Open, Some("Transpose Characters"));
        if changed {
            editor.last_op = LastOp::Other;
            editor.scroll_to_caret = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Language;
    use rustpy_core::{Position, TextSize};

    fn editor(text: &str) -> EditorState {
        EditorState::from_text(text, Language::Python)
    }
    fn at(offset: u32) -> Caret {
        Caret::at(TextSize::new(offset))
    }
    fn select(editor: &mut EditorState, start: u32, end: u32) {
        editor.set_carets(vec![Caret::new(TextSize::new(start), TextSize::new(end))]);
    }

    #[test]
    fn forward_line_kills_chain_through_newline_and_yank_undo_redo() {
        let mut e = editor("αβ\nnext");
        let mut emacs = EmacsEditing::default();
        assert!(emacs.kill(&mut e, Kill::LineForward, &EditOptions::default()));
        assert_eq!(e.doc.text(), "\nnext");
        assert!(emacs.kill(&mut e, Kill::LineForward, &EditOptions::default()));
        assert_eq!(e.doc.text(), "next");
        assert_eq!(emacs.latest_text().as_deref(), Some("αβ\n"));
        assert_eq!(emacs.ring_len(), 1);
        assert!(emacs.yank(&mut e));
        assert_eq!(e.doc.text(), "αβ\nnext");
        assert_eq!(e.primary().head(), TextSize::new(5));
        assert!(e.undo());
        assert_eq!(e.doc.text(), "next");
        assert!(e.redo());
        assert_eq!(e.doc.text(), "αβ\nnext");
        assert!(!emacs.yank_pop(&mut e));
    }

    #[test]
    fn backward_kills_prepend_and_word_boundaries_reuse_editor_policy() {
        let mut e = editor("one two");
        e.move_primary_to(Position::new(0, 7));
        let mut emacs = EmacsEditing::default();
        assert!(emacs.kill(&mut e, Kill::WordBackward, &EditOptions::default()));
        assert!(emacs.kill(&mut e, Kill::WordBackward, &EditOptions::default()));
        assert_eq!(e.doc.text(), "");
        assert_eq!(emacs.latest_text().as_deref(), Some("one two"));
        assert!(emacs.yank(&mut e));
        assert_eq!(e.doc.text(), "one two");
        emacs.interrupt();
        e.move_primary_to(Position::new(0, 0));
        assert!(emacs.kill(&mut e, Kill::WordForward, &EditOptions::default()));
        assert_eq!(e.doc.text(), " two");
        assert_eq!(emacs.ring_len(), 2);
        let mut e = editor("one\ntwo");
        e.move_primary_to(Position::new(1, 0));
        emacs.interrupt();
        assert!(emacs.kill(&mut e, Kill::LineBackward, &EditOptions::default()));
        assert_eq!(e.doc.text(), "onetwo");
        assert_eq!(emacs.latest_text().as_deref(), Some("\n"));
    }

    #[test]
    fn yank_pop_cycles_exact_previous_ranges_and_has_undo_history() {
        let mut source = editor("first second");
        let mut emacs = EmacsEditing::default();
        select(&mut source, 0, 5);
        assert!(emacs.copy_region(&source));
        select(&mut source, 6, 12);
        assert!(emacs.copy_region(&source));
        let mut target = editor("[]");
        target.set_carets(vec![at(1)]);
        assert!(emacs.yank(&mut target));
        assert_eq!(target.doc.text(), "[second]");
        assert!(emacs.yank_pop(&mut target));
        assert_eq!(target.doc.text(), "[first]");
        assert_eq!(target.doc.undo_label().as_deref(), Some("Yank Pop"));
        assert!(emacs.yank_pop(&mut target));
        assert_eq!(target.doc.text(), "[second]");
        assert!(target.undo());
        assert_eq!(target.doc.text(), "[first]");
        assert!(target.redo());
        assert_eq!(target.doc.text(), "[second]");
    }

    #[test]
    fn yank_pop_rejects_edits_navigation_explicit_interruption_and_other_documents() {
        let mut source = editor("word");
        let mut emacs = EmacsEditing::default();
        select(&mut source, 0, 4);
        assert!(emacs.copy_region(&source));
        let mut target = editor("");
        assert!(emacs.yank(&mut target));
        target.move_primary_to(Position::new(0, 0));
        assert!(!emacs.yank_pop(&mut target));
        assert_eq!(target.doc.text(), "word");
        assert!(emacs.yank(&mut target));
        target.insert_text("X", &EditOptions::default());
        assert!(!emacs.yank_pop(&mut target));
        let mut other = editor("word");
        other.move_primary_to(Position::new(0, 4));
        assert!(emacs.yank(&mut target));
        assert!(!emacs.yank_pop(&mut other));
        assert!(emacs.yank(&mut target));
        emacs.interrupt();
        assert!(!emacs.yank_pop(&mut target));
    }

    #[test]
    fn multicaret_kill_yank_and_overlap_are_single_atomic_edits() {
        let mut e = editor("one\ntwo");
        e.set_carets(vec![at(4), at(0)]);
        let before = e.carets.clone();
        let mut emacs = EmacsEditing::default();
        assert!(emacs.kill(&mut e, Kill::LineForward, &EditOptions::default()));
        assert_eq!(e.doc.text(), "\n");
        assert_eq!(emacs.latest_text().as_deref(), Some("two\none"));
        assert!(emacs.yank(&mut e));
        assert_eq!(e.doc.text(), "one\ntwo");
        assert!(e.undo());
        assert_eq!(e.doc.text(), "\n");
        assert!(e.undo());
        assert_eq!(e.doc.text(), "one\ntwo");
        assert_eq!(e.carets, before);
        emacs.interrupt();
        e.set_carets(vec![at(0), at(1)]);
        assert!(emacs.kill(&mut e, Kill::LineForward, &EditOptions::default()));
        assert_eq!(e.doc.text(), "\ntwo");
        assert_eq!(emacs.latest_text().as_deref(), Some("one"));
    }

    #[test]
    fn transpose_unicode_middle_eol_multicaret_and_selection_history() {
        let mut e = editor("αβγ\nab");
        e.set_carets(vec![at(2), at(9)]);
        let before = e.carets.clone();
        let mut emacs = EmacsEditing::default();
        assert!(emacs.transpose(&mut e));
        assert_eq!(e.doc.text(), "βαγ\nba");
        assert_eq!(e.carets, vec![at(4), at(9)]);
        assert!(e.undo());
        assert_eq!(e.doc.text(), "αβγ\nab");
        assert_eq!(e.carets, before);
        assert!(e.redo());
        assert_eq!(e.doc.text(), "βαγ\nba");
        let mut e = editor("ab\ncd");
        e.set_carets(vec![Caret::new(TextSize::new(0), TextSize::new(2)), at(4)]);
        assert!(emacs.transpose(&mut e));
        assert_eq!(e.doc.text(), "ab\ndc");
        assert_eq!(
            e.primary().range(),
            TextRange::new(TextSize::new(0), TextSize::new(2))
        );
        let mut e = editor("a\nb");
        e.set_carets(vec![at(1), at(2)]);
        assert!(emacs.transpose(&mut e));
        assert_eq!(e.doc.text(), "ab\n");
    }

    #[test]
    fn read_only_failures_do_not_change_ring_document_or_selections() {
        let mut e = editor("abcd");
        let mut emacs = EmacsEditing::default();
        select(&mut e, 0, 2);
        assert!(emacs.copy_region(&e));
        let before = e.carets.clone();
        e.doc.set_read_only(true);
        assert!(!emacs.kill(&mut e, Kill::Region, &EditOptions::default()));
        assert_eq!(emacs.latest_text().as_deref(), Some("ab"));
        assert_eq!(e.carets, before);
        assert!(!emacs.yank(&mut e));
        e.set_carets(vec![at(1)]);
        assert!(!emacs.transpose(&mut e));
        assert_eq!(e.doc.text(), "abcd");
        assert!(e.take_read_only_refusal());
    }

    #[test]
    fn ring_is_bounded_and_copy_does_not_modify_read_only_source() {
        let mut emacs = EmacsEditing::default();
        let mut e = editor("αβ");
        select(&mut e, 0, 4);
        e.doc.set_read_only(true);
        let version = e.doc.version();
        for _ in 0..40 {
            assert!(emacs.copy_region(&e));
        }
        assert_eq!(emacs.ring_len(), RING_ENTRIES);
        assert_eq!(e.doc.version(), version);
        assert_eq!(e.doc.text(), "αβ");
    }
}
