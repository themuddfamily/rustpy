//! In-place rename (roadmap §2.8): Shift+F6 when every usage is in the
//! file, and the name a refactoring has just introduced (Extract Variable,
//! Introduce Constant / Field / Parameter, Extract Method …).
//!
//! Every occurrence is a segment with a caret of its own, so typing renames
//! them all at once; the lookup does not pop up while typing the name.
//! Enter, Tab or Escape finishes the session, as do moving the caret out of
//! the occurrences and switching editors. The name is checked when the
//! session finishes: one that is not an identifier, or (for Rename) clashes
//! with another name of the scope or class, is refused and the old name put
//! back. Shift+F6 during the session opens the Rename
//! dialog with the name typed so far.

use super::*;
use crate::editor::state::{map_offset, LastOp};
use crate::editor::Caret;
use rustpy_refactor::rename::RenameCheck;
use rustpy_text::{ContentCheckpoint, Edit, EditGroup};

/// What an in-place session renames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum InplaceKind {
    /// Rename (Shift+F6) of an existing symbol.
    Rename,
    /// The name a refactoring introduced, with the refactoring's title.
    Introduced(String),
}

/// How an in-place session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Finish {
    /// Every occurrence reads the new name.
    Renamed(String),
    /// The name did not change.
    Unchanged,
    /// The typed name was refused for the reason given; the old name is back.
    Refused { typed: String, reason: String },
}

/// An in-place rename in progress.
pub(super) struct InplaceRename {
    /// The editor holding the occurrences.
    pub tab_id: egui::Id,
    pub kind: InplaceKind,
    /// The occurrences in the current text, in document order.
    segments: Vec<TextRange>,
    /// The occurrence the caret edits (the others follow it).
    primary: usize,
    /// Document version `segments` belong to.
    version: u64,
    /// Checks the new name; holds the old one.
    check: RenameCheck,
    original_content: ContentCheckpoint,
    transaction: Option<rustpy_text::TransactionToken>,
}

impl InplaceRename {
    /// A session over `segments` of `doc`; the occurrence
    /// holding `at` (else the first) is the one the caret edits.
    pub fn new(
        tab_id: egui::Id,
        kind: InplaceKind,
        mut segments: Vec<TextRange>,
        at: TextSize,
        doc: &Document,
        check: RenameCheck,
    ) -> Self {
        segments.sort_by_key(|r| (r.start(), r.end()));
        segments.dedup();
        let primary = segments
            .iter()
            .position(|r| r.contains_inclusive(at))
            .unwrap_or(0);
        Self {
            tab_id,
            kind,
            segments,
            primary,
            version: doc.version(),
            check,
            original_content: doc.content_checkpoint(),
            transaction: None,
        }
    }

    pub fn old_name(&self) -> &str {
        &self.check.old_name
    }

    pub fn occurrences(&self) -> usize {
        self.segments.len()
    }

    /// Follows the edits made since the last call. `false` when an edit
    /// reached across an occurrence's bounds (the session cannot go on).
    pub fn sync(&mut self, doc: &Document) -> bool {
        if doc.version() == self.version {
            return true;
        }
        let Some(changes) = doc.changes_since(self.version) else {
            return false;
        };
        for edits in changes {
            for r in &mut self.segments {
                // Typing at either end of an occurrence extends it.
                match remap_range(*r, &edits, true) {
                    Some(n) => *r = n,
                    None => return false,
                }
            }
        }
        self.version = doc.version();
        true
    }

    /// The occurrence holding `offset` (its ends included).
    pub fn segment_at(&self, offset: TextSize) -> Option<usize> {
        self.segments
            .iter()
            .position(|r| r.contains_inclusive(offset))
    }

    /// The carets of the session: one per occurrence, placed in each as
    /// `caret` is in occurrence `k` (its selection kept inside it), with
    /// occurrence `k`'s first (the primary caret).
    pub fn mirrored_carets(&self, k: usize, caret: Caret) -> Vec<Caret> {
        let seg = self.segments[k];
        let rel = |o: TextSize| o.clamp(seg.start(), seg.end()) - seg.start();
        let (anchor, head) = (rel(caret.sel.anchor), rel(caret.sel.head));
        let place = |r: &TextRange| {
            Caret::new(
                r.start() + anchor.min(r.len()),
                r.start() + head.min(r.len()),
            )
        };
        std::iter::once(place(&seg))
            .chain(
                self.segments
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != k)
                    .map(|(_, r)| place(r)),
            )
            .collect()
    }

    /// Keeps a caret in every occurrence after the editor handled input (a
    /// click in one of them leaves a single caret there). `false` when the
    /// caret has left the occurrences: the session is over.
    pub fn follow_carets(&mut self, state: &mut EditorState) -> bool {
        let primary = state.primary();
        let Some(k) = self.segment_at(primary.head()) else {
            return false;
        };
        self.primary = k;
        let want = self.mirrored_carets(k, primary);
        if !same_carets(&state.carets, &want) {
            state.set_carets(want);
        }
        true
    }

    /// Ends the session in `state` (after [`Self::sync`]): occurrences that
    /// drifted apart take the text of the one being edited, then the name is
    /// checked, and the old one put back when it is refused. One caret
    /// stays, where the primary one was.
    pub fn finish(&mut self, state: &mut EditorState) -> Finish {
        let head = state.primary().head();
        let k = self.segment_at(head).unwrap_or(self.primary);
        let typed = state.doc.slice(self.segments[k]);
        let old = self.check.old_name.clone();
        let result = if typed == old {
            Finish::Unchanged
        } else {
            match self.check.check(&typed) {
                Ok(()) => Finish::Renamed(typed.clone()),
                Err(reason) => Finish::Refused {
                    typed: typed.clone(),
                    reason,
                },
            }
        };
        let name = if matches!(result, Finish::Refused { .. }) {
            old
        } else {
            typed
        };
        self.set_all(state, &name, head, false);
        if !matches!(result, Finish::Renamed(_)) {
            state.doc.recognize_restored_content(&self.original_content);
        }
        self.commit(state);
        result
    }

    fn commit(&mut self, state: &mut EditorState) {
        if let Some(token) = self.transaction.take() {
            let _ = state.commit_transaction(token);
        }
    }

    /// Puts the old name back in every occurrence and returns the name typed
    /// so far (Shift+F6 opens the Rename dialog with it). The caret ends up
    /// after the name in the occurrence it was editing.
    pub fn revert(&mut self, state: &mut EditorState) -> String {
        let head = state.primary().head();
        let k = self.segment_at(head).unwrap_or(self.primary);
        let typed = state.doc.slice(self.segments[k]);
        let old = self.check.old_name.clone();
        self.set_all(state, &old, self.segments[k].end(), true);
        state.doc.recognize_restored_content(&self.original_content);
        self.commit(state);
        typed
    }

    /// Writes `name` into every occurrence that reads otherwise (one undo
    /// step) and leaves one caret: at `head` mapped through the edits, or
    /// after the name when `head` was in an edited occurrence (or `to_end`).
    fn set_all(&mut self, state: &mut EditorState, name: &str, head: TextSize, to_end: bool) {
        let edits: Vec<Edit> = self
            .segments
            .iter()
            .filter(|r| state.doc.slice(**r) != name)
            .map(|r| Edit::replace(*r, name.to_string()))
            .collect();
        let caret = match self.segment_at(head) {
            Some(k) if to_end || edits.iter().any(|e| e.range == self.segments[k]) => {
                map_offset(&edits, self.segments[k].start(), false) + TextSize::of(name)
            }
            _ => map_offset(&edits, head, false),
        };
        if edits.is_empty() {
            state.set_carets(vec![Caret::at(caret)]);
        } else {
            state.apply(
                edits,
                vec![Caret::at(caret)],
                EditGroup::Separate,
                Some("Rename"),
            );
            self.sync(&state.doc);
        }
        state.last_op = LastOp::Other;
    }
}

/// The same carets (the primary first), whatever the order of the others.
fn same_carets(a: &[Caret], b: &[Caret]) -> bool {
    let sels = |c: &[Caret]| {
        let mut v: Vec<(TextSize, TextSize)> =
            c.iter().map(|k| (k.sel.anchor, k.sel.head)).collect();
        v.sort();
        v
    };
    a.len() == b.len() && a.first().map(|k| k.sel) == b.first().map(|k| k.sel) && sels(a) == sels(b)
}

/// The status bar message for an in-place session that ended with `finish`.
pub(super) fn finish_message(
    kind: &InplaceKind,
    old: &str,
    finish: &Finish,
    occurrences: usize,
) -> String {
    match (kind, finish) {
        (InplaceKind::Rename, Finish::Renamed(new)) => {
            format!("Renamed '{old}' to '{new}' ({occurrences} occurrence(s))")
        }
        (InplaceKind::Rename, Finish::Unchanged) => {
            format!("Rename finished: '{old}' is unchanged")
        }
        (InplaceKind::Rename, Finish::Refused { typed, reason }) => {
            format!("Cannot rename '{old}' to '{typed}': {reason}")
        }
        (InplaceKind::Introduced(title), Finish::Renamed(new)) => {
            format!("{title}: renamed to '{new}'")
        }
        (InplaceKind::Introduced(title), Finish::Unchanged) => title.clone(),
        (InplaceKind::Introduced(title), Finish::Refused { reason, .. }) => {
            format!("{title}: {reason}, so the name stays '{old}'")
        }
    }
}

/// The first of `keys` pressed without any modifier this frame, taken out
/// of the input (Alt+Enter and Shift+Tab keep their own meaning).
fn take_plain_key(ui: &mut egui::Ui, keys: &[egui::Key]) -> Option<egui::Key> {
    ui.input_mut(|inp| {
        let key = inp.events.iter().find_map(|e| match e {
            egui::Event::Key { key, pressed: true, modifiers, .. } if keys.contains(key) && modifiers.is_none() => Some(*key),
            _ => None,
        })?;
        inp.events.retain(|e| !matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && modifiers.is_none()));
        Some(key)
    })
}

impl IdeApp {
    /// Whether an in-place rename is going on in the editor of tab `i`.
    pub(super) fn inplace_rename_in(&self, i: usize) -> bool {
        self.inplace_rename
            .as_ref()
            .is_some_and(|s| self.tabs.get(i).is_some_and(|t| t.view.id == s.tab_id))
    }

    /// Starts an in-place rename of `ranges` (every occurrence, in the
    /// current text of tab `i`); the occurrence holding `at` gets the primary
    /// caret, and each caret selects its occurrence.
    pub(super) fn start_inplace_rename(
        &mut self,
        i: usize,
        ranges: Vec<TextRange>,
        at: TextSize,
        kind: InplaceKind,
        check: RenameCheck,
    ) {
        self.start_inplace_rename_transaction(i, ranges, at, kind, check, None);
    }

    pub(super) fn start_inplace_rename_transaction(
        &mut self,
        i: usize,
        ranges: Vec<TextRange>,
        at: TextSize,
        kind: InplaceKind,
        check: RenameCheck,
        transaction: Option<rustpy_text::TransactionToken>,
    ) {
        self.finish_inplace_rename();
        let mut sess = InplaceRename::new(
            self.tabs[i].view.id,
            kind,
            ranges,
            at,
            &self.tabs[i].state.doc,
            check,
        );
        // A session going on ends first; putting its old name back moves the new occurrences.
        // Tab and Enter belong to the rename now; the lookup stays closed.
        self.template_session = None;
        self.completion.visible = false;
        let tab = &mut self.tabs[i];
        if sess.segments.is_empty() || !sess.sync(&tab.state.doc) {
            if let Some(token) = transaction {
                let _ = tab.state.commit_transaction(token);
                tab.state.doc.mark_refactoring();
            }
            if let InplaceKind::Introduced(title) = sess.kind {
                self.set_status(title);
            }
            return;
        }
        sess.transaction = transaction.or_else(|| tab.state.begin_transaction("Rename").ok());
        sess.original_content = tab.state.doc.content_checkpoint();
        let seg = sess.segments[sess.primary];
        tab.state
            .set_carets(sess.mirrored_carets(sess.primary, Caret::new(seg.start(), seg.end())));
        tab.state.last_op = LastOp::Other;
        let msg = match &sess.kind {
            InplaceKind::Rename => format!("Rename '{}' in place ({} occurrence(s)): type the new name, Enter to finish, Shift+F6 for the dialog", sess.old_name(), sess.occurrences()),
            InplaceKind::Introduced(title) => format!("{title}: type the name, Enter to finish"),
        };
        self.inplace_rename = Some(sess);
        self.set_status(msg);
        self.focus_editor = true;
    }

    /// Ends the in-place rename (in whichever editor holds it): checks the
    /// name, restores the old one when it is refused, and says how it went.
    pub(super) fn finish_inplace_rename(&mut self) {
        let Some(mut sess) = self.inplace_rename.take() else {
            return;
        };
        let Some(tab) = self.tabs.iter_mut().find(|t| t.view.id == sess.tab_id) else {
            return;
        };
        if !sess.sync(&tab.state.doc) {
            sess.commit(&mut tab.state);
            tab.state.doc.mark_refactoring();
            self.set_status("In-place rename ended: its occurrences were edited");
            return;
        }
        let finish = sess.finish(&mut tab.state);
        if matches!(finish, Finish::Renamed(_)) || matches!(sess.kind, InplaceKind::Introduced(_)) {
            tab.state.doc.mark_refactoring();
        }
        let msg = finish_message(&sess.kind, sess.old_name(), &finish, sess.occurrences());
        self.set_status(msg);
    }

    /// Enter, Tab or Escape (without modifiers) in the editor of an in-place
    /// rename finish it; an open lookup, intention list or parameter info
    /// takes its keys first. Returns whether a key was used.
    pub(super) fn inplace_rename_keys(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(i) = self.active.filter(|i| self.inplace_rename_in(*i)) else {
            return false;
        };
        if self.completion.visible
            || self.intentions.is_some()
            || self.choice.is_some()
            || !matches!(self.dialog, Dialog::None)
            || !ui.memory(|m| m.has_focus(self.tabs[i].view.id))
        {
            return false;
        }
        if ui.input(|input| {
            input.modifiers.command && !input.modifiers.shift && input.key_pressed(egui::Key::Z)
        }) {
            // Finish the scope before the ordinary Undo handler sees this key.
            self.finish_inplace_rename();
            return false;
        }
        let keys: &[egui::Key] = if self.param_info.is_some() || self.quick_doc.is_some() {
            &[egui::Key::Enter, egui::Key::Tab]
        } else {
            &[egui::Key::Enter, egui::Key::Tab, egui::Key::Escape]
        };
        if take_plain_key(ui, keys).is_none() {
            return false;
        }
        self.finish_inplace_rename();
        true
    }

    /// Before the active editor (tab `i`) handles input: a session in another
    /// editor ends (the user switched editors or closed its tab).
    pub(super) fn inplace_rename_check_editor(&mut self, i: usize) {
        if self.inplace_rename.is_some() && !self.inplace_rename_in(i) {
            self.finish_inplace_rename();
        }
    }

    /// After the active editor (tab `i`) handled input: the occurrences
    /// follow the edits and keep a caret each; the session ends when the
    /// caret has left them, or is dropped when an edit broke one up.
    pub(super) fn inplace_rename_follow(&mut self, i: usize) {
        if !self.inplace_rename_in(i) {
            return;
        }
        let (Some(sess), Some(tab)) = (self.inplace_rename.as_mut(), self.tabs.get_mut(i)) else {
            return;
        };
        if !sess.sync(&tab.state.doc) || !sess.follow_carets(&mut tab.state) {
            self.finish_inplace_rename();
        }
    }

    /// Shift+F6 during an in-place rename in the active editor: puts the old
    /// name back and opens the Rename dialog with the name typed so far.
    pub(super) fn inplace_rename_to_dialog(&mut self) -> bool {
        let Some(i) = self.active.filter(|i| self.inplace_rename_in(*i)) else {
            return false;
        };
        let Some(mut sess) = self.inplace_rename.take() else {
            return false;
        };
        let tab = &mut self.tabs[i];
        if !sess.sync(&tab.state.doc) {
            sess.commit(&mut tab.state);
            tab.state.doc.mark_refactoring();
            return false;
        }
        let typed = sess.revert(&mut tab.state);
        self.open_rename(true, Some(typed).filter(|t| !t.trim().is_empty()));
        true
    }

    /// Rename (Shift+F6): in place when every usage is in this file (and the
    /// in-place mode is on), otherwise the Rename dialog, which also explains
    /// why a symbol cannot be renamed.
    pub(super) fn rename_at_caret(&mut self) {
        if self.inplace_rename_to_dialog() {
            return;
        }
        // In place when every usage is certain and in this file, else the
        // Rename dialog with its preview (rename_ui.rs).
        self.open_rename(false, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: u32, e: u32) -> TextRange {
        TextRange::new(TextSize::new(s), TextSize::new(e))
    }

    /// An editor over `text` with an in-place session on `ranges`; the one
    /// holding `at` gets the primary caret, each caret selecting its occurrence.
    fn session_on(
        text: &str,
        ranges: Vec<TextRange>,
        at: TextSize,
        check: RenameCheck,
    ) -> (EditorState, InplaceRename) {
        session_on_state(
            EditorState::from_text(text, Language::Python),
            ranges,
            at,
            check,
        )
    }

    fn session_on_state(
        mut state: EditorState,
        ranges: Vec<TextRange>,
        at: TextSize,
        check: RenameCheck,
    ) -> (EditorState, InplaceRename) {
        let sess = InplaceRename::new(
            egui::Id::new("t"),
            InplaceKind::Rename,
            ranges,
            at,
            &state.doc,
            check,
        );
        let seg = sess.segments[sess.primary];
        state.set_carets(sess.mirrored_carets(sess.primary, Caret::new(seg.start(), seg.end())));
        (state, sess)
    }

    /// [`session_on`] every `name` in `text`, the `nth` one primary.
    fn session(
        text: &str,
        name: &str,
        nth: usize,
        check: RenameCheck,
    ) -> (EditorState, InplaceRename) {
        let ranges: Vec<TextRange> = text
            .match_indices(name)
            .map(|(i, _)| r(i as u32, (i + name.len()) as u32))
            .collect();
        let at = ranges[nth].end();
        session_on(text, ranges, at, check)
    }

    fn type_text(state: &mut EditorState, sess: &mut InplaceRename, text: &str) {
        state.insert_text(text, &EditOptions::default());
        assert!(sess.sync(&state.doc));
        assert!(sess.follow_carets(state));
    }

    const SRC: &str = "def say(self, message):\n    print(self.prefix + message)\n";

    #[test]
    fn typing_renames_every_occurrence_and_enter_keeps_it() {
        let (mut state, mut sess) =
            session(SRC, "message", 0, RenameCheck::identifier_only("message"));
        assert_eq!(state.carets.len(), 2);
        type_text(&mut state, &mut sess, "val");
        assert_eq!(
            state.doc.text(),
            "def say(self, val):\n    print(self.prefix + val)\n"
        );
        assert_eq!(sess.finish(&mut state), Finish::Renamed("val".into()));
        assert_eq!(
            state.doc.text(),
            "def say(self, val):\n    print(self.prefix + val)\n"
        );
        // One caret stays, after the name it was typed into.
        assert_eq!(state.carets.len(), 1);
        assert_eq!(state.primary().head(), TextSize::new(17));
    }

    #[test]
    fn refusal_and_cancel_preserve_edits_that_were_dirty_before_rename() {
        for refuse in [false, true] {
            let mut state = EditorState::from_text(SRC, Language::Python);
            let end = state.doc.len();
            state.apply(
                vec![Edit::insert(end, "# unsaved before rename\n")],
                vec![Caret::at(end)],
                EditGroup::Separate,
                Some("Earlier edit"),
            );
            let original = state.doc.text();
            let ranges = original
                .match_indices("message")
                .map(|(i, _)| r(i as u32, (i + 7) as u32))
                .collect();
            let (mut state, mut sess) = session_on_state(
                state,
                ranges,
                TextSize::new(14),
                RenameCheck::identifier_only("message"),
            );
            type_text(&mut state, &mut sess, "for");
            if refuse {
                assert!(matches!(sess.finish(&mut state), Finish::Refused { .. }));
            } else {
                assert_eq!(sess.revert(&mut state), "for");
            }
            assert_eq!(state.doc.text(), original);
            assert!(state.doc.is_modified());
            assert!(state.undo()); // restoration
            assert!(state.undo()); // typing
            assert_eq!(state.doc.text(), original);
            assert!(state.doc.is_modified());
            assert!(state.undo()); // earlier unrelated edit
            assert_eq!(state.doc.text(), SRC);
            assert!(!state.doc.is_modified());
            assert!(state.redo());
            assert_eq!(state.doc.text(), original);
            assert!(state.doc.is_modified());
        }
    }

    #[test]
    fn restoring_a_preexisting_dirty_identity_does_not_implicitly_save_it() {
        for refuse in [false, true] {
            let mut state = EditorState::from_text(SRC, Language::Python);
            state
                .doc
                .set_text("temporary unrelated edit", &[], &[])
                .unwrap();
            state.doc.set_text(SRC, &[], &[]).unwrap();
            assert!(state.doc.is_modified());
            let ranges = SRC
                .match_indices("message")
                .map(|(i, _)| r(i as u32, (i + 7) as u32))
                .collect();
            let (mut state, mut sess) = session_on_state(
                state,
                ranges,
                TextSize::new(14),
                RenameCheck::identifier_only("message"),
            );
            type_text(&mut state, &mut sess, "for");
            if refuse {
                assert!(matches!(sess.finish(&mut state), Finish::Refused { .. }));
            } else {
                sess.revert(&mut state);
            }
            assert_eq!(state.doc.text(), SRC);
            assert!(
                state.doc.is_modified(),
                "the preexisting dirty identity was not saved"
            );
            assert!(state.undo());
            assert!(state.redo());
            assert!(state.doc.is_modified());
        }
    }

    #[test]
    fn restoring_a_refused_name_preserves_undo_and_redo_dirty_states() {
        let (mut state, mut sess) =
            session(SRC, "message", 1, RenameCheck::identifier_only("message"));
        type_text(&mut state, &mut sess, "for");
        let typed = state.doc.text();
        let typed_carets = state.carets.clone();
        assert!(matches!(sess.finish(&mut state), Finish::Refused { .. }));
        let restored_caret = state.primary().head();
        assert!(!state.doc.is_modified());
        assert!(state.undo());
        assert_eq!(state.doc.text(), typed);
        assert!(state.doc.is_modified());
        assert!(same_carets(&state.carets, &typed_carets));
        assert!(state.redo());
        assert_eq!(state.doc.text(), SRC);
        assert!(!state.doc.is_modified());
        assert_eq!(state.primary().head(), restored_caret);
        assert!(state.undo());
        assert!(state.undo());
        assert_eq!(state.doc.text(), SRC);
        assert!(!state.doc.is_modified());
    }

    #[test]
    fn unchanged_after_undo_keeps_clean_state_through_redo_and_undo() {
        let (mut state, mut sess) =
            session(SRC, "message", 0, RenameCheck::identifier_only("message"));
        for name in ["temporary", "message", "another"] {
            let seg = sess.segments[0];
            state.set_carets(sess.mirrored_carets(0, Caret::new(seg.start(), seg.end())));
            state.last_op = LastOp::Other;
            type_text(&mut state, &mut sess, name);
        }
        assert!(state.undo());
        assert!(sess.sync(&state.doc));
        assert_eq!(state.doc.text(), SRC);
        assert_eq!(sess.finish(&mut state), Finish::Unchanged);
        assert!(!state.doc.is_modified());
        assert!(state.redo());
        assert_eq!(state.doc.text(), SRC.replace("message", "another"));
        assert!(state.doc.is_modified());
        assert!(state.undo());
        assert_eq!(state.doc.text(), SRC);
        assert!(!state.doc.is_modified());
    }

    #[test]
    fn reverting_keeps_a_format_change_modified() {
        let (mut state, mut sess) =
            session(SRC, "message", 0, RenameCheck::identifier_only("message"));
        type_text(&mut state, &mut sess, "temporary");
        state.doc.set_line_ending(rustpy_text::LineEnding::CrLf);
        sess.revert(&mut state);
        assert_eq!(state.doc.text(), SRC);
        assert!(state.doc.is_modified());
        assert_eq!(state.doc.line_ending(), rustpy_text::LineEnding::CrLf);
    }

    #[test]
    fn reverting_to_the_original_name_saved_during_the_session_is_clean() {
        let (mut state, mut sess) =
            session(SRC, "message", 0, RenameCheck::identifier_only("message"));
        type_text(&mut state, &mut sess, "temporary");
        state.set_carets(sess.mirrored_carets(
            0,
            Caret::new(sess.segments[0].start(), sess.segments[0].end()),
        ));
        type_text(&mut state, &mut sess, "message");
        state.doc.mark_saved();
        state.set_carets(sess.mirrored_carets(
            0,
            Caret::new(sess.segments[0].start(), sess.segments[0].end()),
        ));
        type_text(&mut state, &mut sess, "for");
        assert!(matches!(sess.finish(&mut state), Finish::Refused { .. }));
        assert_eq!(state.doc.text(), SRC);
        assert!(!state.doc.is_modified());
        assert!(state.undo());
        assert!(state.doc.is_modified());
        assert!(state.redo());
        assert_eq!(state.doc.text(), SRC);
        assert!(!state.doc.is_modified());
    }

    #[test]
    fn reverting_after_a_save_during_rename_does_not_mark_original_saved() {
        let (mut state, mut sess) =
            session(SRC, "message", 0, RenameCheck::identifier_only("message"));
        type_text(&mut state, &mut sess, "temporary");
        let saved = state.doc.text();
        state.doc.mark_saved();
        sess.revert(&mut state);
        assert_eq!(state.doc.text(), SRC);
        assert!(
            state.doc.is_modified(),
            "the saved file contains the typed name"
        );
        assert!(state.undo());
        assert_eq!(state.doc.text(), saved);
        assert!(!state.doc.is_modified());
        assert!(state.redo());
        assert_eq!(state.doc.text(), SRC);
        assert!(state.doc.is_modified());
    }

    #[test]
    fn reverting_preserves_an_unrelated_edit_during_the_session() {
        let (mut state, mut sess) =
            session(SRC, "message", 0, RenameCheck::identifier_only("message"));
        type_text(&mut state, &mut sess, "temporary");
        let end = state.doc.len();
        state.apply(
            vec![Edit::insert(end, "# keep this change\n")],
            state.carets.clone(),
            EditGroup::Separate,
            Some("Other edit"),
        );
        assert!(sess.sync(&state.doc));
        sess.revert(&mut state);
        assert_eq!(state.doc.text(), format!("{SRC}# keep this change\n"));
        assert!(state.doc.is_modified());
        assert!(state.undo());
        assert!(state.doc.text().ends_with("# keep this change\n"));
    }

    #[test]
    fn a_click_inside_an_occurrence_keeps_a_caret_in_each() {
        let (mut state, mut sess) =
            session(SRC, "message", 0, RenameCheck::identifier_only("message"));
        // A click in the second occurrence leaves one caret there.
        let second = SRC.rfind("message").unwrap() as u32;
        state.set_carets(vec![Caret::at(TextSize::new(second + 3))]);
        assert!(sess.follow_carets(&mut state));
        assert_eq!(state.carets.len(), 2);
        assert_eq!(state.primary().head(), TextSize::new(second + 3));
        assert!(state
            .carets
            .iter()
            .any(|c| c.head() == TextSize::new(14 + 3)));
        type_text(&mut state, &mut sess, "X");
        assert_eq!(
            state.doc.text(),
            "def say(self, mesXsage):\n    print(self.prefix + mesXsage)\n"
        );
        // Moving away ends the session.
        state.set_carets(vec![Caret::at(TextSize::new(0))]);
        assert!(!sess.follow_carets(&mut state));
        assert_eq!(sess.finish(&mut state), Finish::Renamed("mesXsage".into()));
        assert_eq!(state.primary().head(), TextSize::new(0));
    }

    #[test]
    fn a_refused_name_restores_the_old_one() {
        let check = || RenameCheck::identifier_only("message");
        for bad in ["1x", "for", "a b"] {
            let (mut state, mut sess) = session(SRC, "message", 1, check());
            type_text(&mut state, &mut sess, bad);
            match sess.finish(&mut state) {
                Finish::Refused { typed, .. } => assert_eq!(typed, bad),
                other => panic!("{bad}: {other:?}"),
            }
            assert_eq!(state.doc.text(), SRC, "{bad}");
            assert!(
                !state.doc.is_modified(),
                "refusal must restore clean content: {bad}"
            );
            assert_eq!(state.carets.len(), 1);
            // The caret stays after the name in the occurrence it edited.
            assert_eq!(
                state.primary().head(),
                TextSize::new((SRC.rfind("message").unwrap() + 7) as u32)
            );
        }
        // Unchanged: nothing typed.
        let (mut state, mut sess) = session(SRC, "message", 0, check());
        assert_eq!(sess.finish(&mut state), Finish::Unchanged);
        assert_eq!(state.doc.text(), SRC);
    }

    #[test]
    fn a_name_taken_in_the_scope_is_refused_with_the_rename_check() {
        // QA: renaming `excited` to `name` in place gave `def greet(name, name=False)`.
        let src = "def greet(name, excited=False):\n    if excited:\n        return name.upper()\n    return name\n\n\nprint(greet(\"bob\", excited=True))\n";
        let db = Analysis::for_project(Path::new("/proj"));
        db.set_file_text(Path::new("/proj/rn4.py"), src);
        let file = db.file_for_path(Path::new("/proj/rn4.py")).unwrap();
        let at = TextSize::new(src.find("excited").unwrap() as u32);
        let check = rustpy_refactor::rename::rename_check(&db, &file, at).unwrap();
        let ranges: Vec<TextRange> =
            rustpy_search::usages::find_usages(&db, &file, at, &[PathBuf::from("/proj/rn4.py")])
                .iter()
                .map(|u| u.range)
                .collect();
        assert_eq!(ranges.len(), 3);
        let (mut state, mut sess) = session_on(src, ranges.clone(), at, check.clone());
        type_text(&mut state, &mut sess, "name");
        assert!(state.doc.text().starts_with("def greet(name, name=False):"));
        let finish = sess.finish(&mut state);
        assert_eq!(
            finish,
            Finish::Refused {
                typed: "name".into(),
                reason: "'name' is already defined in the scope of 'excited'".into()
            }
        );
        assert_eq!(state.doc.text(), src);
        assert_eq!(finish_message(&sess.kind, sess.old_name(), &finish, sess.occurrences()), "Cannot rename 'excited' to 'name': 'name' is already defined in the scope of 'excited'");
        // A free name goes through.
        let (mut state, mut sess) = session_on(src, ranges, at, check);
        type_text(&mut state, &mut sess, "loud");
        assert_eq!(sess.finish(&mut state), Finish::Renamed("loud".into()));
        assert_eq!(state.doc.text(), src.replace("excited", "loud"));
    }

    #[test]
    fn occurrences_that_drifted_apart_take_the_edited_name() {
        let (mut state, mut sess) =
            session(SRC, "message", 0, RenameCheck::identifier_only("message"));
        // Only the primary occurrence was edited (one caret).
        state.set_carets(vec![Caret::new(TextSize::new(14), TextSize::new(21))]);
        state.insert_text("val", &EditOptions::default());
        assert!(sess.sync(&state.doc));
        assert_eq!(sess.finish(&mut state), Finish::Renamed("val".into()));
        assert_eq!(
            state.doc.text(),
            "def say(self, val):\n    print(self.prefix + val)\n"
        );
    }

    #[test]
    fn revert_for_the_dialog_returns_the_typed_name() {
        let (mut state, mut sess) =
            session(SRC, "message", 0, RenameCheck::identifier_only("message"));
        type_text(&mut state, &mut sess, "va");
        assert_eq!(sess.revert(&mut state), "va");
        assert_eq!(state.doc.text(), SRC);
        assert!(
            !state.doc.is_modified(),
            "cancelling the dialog must leave the original clean"
        );
        assert_eq!(state.carets.len(), 1);
        assert_eq!(state.primary().head(), TextSize::new(21));
    }

    #[test]
    fn edits_across_an_occurrence_end_the_session() {
        let (mut state, mut sess) =
            session(SRC, "message", 0, RenameCheck::identifier_only("message"));
        // Deleting from inside the name to the line start.
        state.set_carets(vec![Caret::new(TextSize::new(0), TextSize::new(16))]);
        state.insert_text("x", &EditOptions::default());
        assert!(!sess.sync(&state.doc));
        // Undoing a rename typed in place restores the occurrences.
        let (mut state, mut sess) =
            session(SRC, "message", 0, RenameCheck::identifier_only("message"));
        type_text(&mut state, &mut sess, "val");
        state.undo();
        assert!(sess.sync(&state.doc));
        assert!(sess.follow_carets(&mut state));
        assert_eq!(state.doc.text(), SRC);
        assert_eq!(state.carets.len(), 2);
    }

    #[test]
    fn a_single_occurrence_is_a_session_too() {
        // An unused parameter has one occurrence: one caret, still in the session.
        let src = "def greet(name, excited=False):\n    return name\n";
        let (mut state, mut sess) =
            session(src, "excited", 0, RenameCheck::identifier_only("excited"));
        assert_eq!(state.carets.len(), 1);
        type_text(&mut state, &mut sess, "loud");
        assert!(sess.follow_carets(&mut state));
        assert_eq!(sess.finish(&mut state), Finish::Renamed("loud".into()));
        assert_eq!(
            state.doc.text(),
            "def greet(name, loud=False):\n    return name\n"
        );
    }

    #[test]
    fn messages() {
        let intro = InplaceKind::Introduced("Extract variable 'value'".into());
        assert_eq!(
            finish_message(&intro, "value", &Finish::Renamed("zq".into()), 2),
            "Extract variable 'value': renamed to 'zq'"
        );
        assert_eq!(finish_message(&intro, "value", &Finish::Refused { typed: "1x".into(), reason: "'1x' is not a valid Python identifier".into() }, 2), "Extract variable 'value': '1x' is not a valid Python identifier, so the name stays 'value'");
        assert_eq!(
            finish_message(
                &InplaceKind::Rename,
                "message",
                &Finish::Renamed("val".into()),
                2
            ),
            "Renamed 'message' to 'val' (2 occurrence(s))"
        );
    }
}
