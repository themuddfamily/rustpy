//! One owned IME preedit per view. Intermediate composition never edits source or history.
use super::state::{Caret, EditOptions, EditorState};
#[derive(Default)]
pub struct Composition {
    pub preedit: String,
    owner: Option<(rustpy_text::DocumentId, u64, Vec<Caret>)>,
    pub interrupt: bool,
    blocked: bool,
}
impl Composition {
    pub fn composing(&self) -> bool {
        self.owner.is_some()
    }
    pub fn validate(&mut self, state: &EditorState, focused: bool) {
        if self
            .owner
            .as_ref()
            .is_some_and(|(document, version, carets)| {
                !focused
                    || *document != state.doc.identity()
                    || *version != state.doc.version()
                    || *carets != state.carets
                    || state.doc.is_read_only()
            })
        {
            self.cancel();
        }
    }
    pub fn enabled(&mut self) {
        self.blocked = false;
        self.interrupt = false;
    }
    pub fn cancel(&mut self) {
        self.blocked = true;
        self.owner = None;
        self.preedit.clear();
        self.interrupt = true;
    }
    pub fn preedit(&mut self, state: &EditorState, text: String) {
        self.validate(state, true);
        if state.doc.is_read_only() {
            return;
        }
        self.blocked = false;
        if self.owner.is_none() {
            self.owner = Some((
                state.doc.identity(),
                state.doc.version(),
                state.carets.clone(),
            ));
        }
        self.preedit = text;
    }
    pub fn commit(&mut self, state: &mut EditorState, text: &str, options: &EditOptions) -> bool {
        if self.blocked
            || self
                .owner
                .as_ref()
                .is_some_and(|(document, version, carets)| {
                    *document != state.doc.identity()
                        || *version != state.doc.version()
                        || *carets != state.carets
                })
            || state.doc.is_read_only()
        {
            self.cancel();
            return false;
        }
        self.owner = None;
        self.preedit.clear();
        if text.is_empty() {
            return false;
        }
        let before = state.doc.version();
        state.last_op = super::state::LastOp::Other;
        state.insert_text(text, options);
        state.last_op = super::state::LastOp::Other;
        state.doc.version() != before
    }
}
#[cfg(test)]
mod tests {
    use super::super::Language;
    use super::*;
    #[test]
    fn preedit_is_ephemeral_and_commit_preserves_decomposed_spelling() {
        let mut state = EditorState::from_text("", Language::PlainText);
        let mut ime = Composition::default();
        ime.preedit(&state, "にほん".into());
        assert_eq!(state.doc.text(), "");
        assert!(ime.commit(&mut state, "日本e\u{301}", &EditOptions::default()));
        assert_eq!(state.doc.text(), "日本e\u{301}");
        assert!(!ime.composing());
    }
    #[test]
    fn changed_or_readonly_owner_cannot_receive_a_late_composition() {
        let mut state = EditorState::from_text("", Language::PlainText);
        let mut ime = Composition::default();
        ime.preedit(&state, "候補".into());
        state.insert_text("changed", &EditOptions::default());
        assert!(!ime.commit(&mut state, "誤入力", &EditOptions::default()));
        assert_eq!(state.doc.text(), "changed");
        state.doc.set_read_only(true);
        ime.preedit(&state, "候補".into());
        assert!(!ime.commit(&mut state, "書換", &EditOptions::default()));
    }
    #[test]
    fn focus_loss_rejects_late_commit_and_next_enabled_session_can_type() {
        let mut state = EditorState::from_text("", Language::PlainText);
        let mut ime = Composition::default();
        ime.preedit(&state, "한국".into());
        ime.validate(&state, false);
        assert!(!ime.commit(&mut state, "stale", &EditOptions::default()));
        assert_eq!(state.doc.text(), "");
        ime.enabled();
        ime.preedit(&state, "한국".into());
        assert!(ime.commit(&mut state, "한국", &EditOptions::default()));
        assert_eq!(state.doc.text(), "한국");
    }
    #[test]
    fn committed_composition_is_one_undo_step_separate_from_prior_typing() {
        let mut state = EditorState::from_text("", Language::PlainText);
        state.insert_text("prefix", &EditOptions::default());
        let mut ime = Composition::default();
        let version = state.doc.version();
        ime.preedit(&state, "に".into());
        ime.preedit(&state, "日本".into());
        assert_eq!(state.doc.version(), version);
        assert!(ime.commit(&mut state, "日本", &EditOptions::default()));
        assert!(state.undo());
        assert_eq!(state.doc.text(), "prefix");
    }
}
