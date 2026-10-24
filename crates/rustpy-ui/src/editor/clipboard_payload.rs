//! Private editor-copy provenance; flattened clipboard text alone is ambiguous.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardPayload {
    pub text: String,
    pub pieces: Option<Vec<String>>,
}
impl ClipboardPayload {
    pub fn matching_pieces(&self, text: &str, carets: usize) -> Option<&[String]> {
        let pieces = self.pieces.as_deref()?;
        (self.text == text && pieces.len() == carets).then_some(pieces)
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::{Caret, EditOptions, EditorState, Language};
    use rustpy_core::{TextRange, TextSize};
    fn selected() -> EditorState {
        let mut state = EditorState::from_text("é\nβ--tail", Language::PlainText);
        state.set_carets(vec![
            Caret::new(TextSize::new(0), TextSize::new(5)),
            Caret::at(TextSize::new(9)),
        ]);
        state
    }
    #[test]
    fn exact_multiline_and_terminal_empty_payload_round_trip_one_undo_redo() {
        let source = selected();
        let payload = source.copy_payload();
        assert_eq!(payload.pieces.as_ref().unwrap(), &["é\nβ", ""]);
        let mut dest = EditorState::from_text("ab", Language::PlainText);
        dest.set_carets(vec![
            Caret::at(TextSize::new(0)),
            Caret::at(TextSize::new(2)),
        ]);
        let before = dest.carets.clone();
        dest.paste_payload(&payload.text, Some(&payload), &EditOptions::default());
        assert_eq!(dest.doc.text(), "é\nβab");
        let after = dest.carets.clone();
        assert!(dest.undo());
        assert_eq!(dest.doc.text(), "ab");
        assert_eq!(dest.carets, before);
        assert!(!dest.doc.can_undo());
        assert!(dest.redo());
        assert_eq!(dest.doc.text(), "é\nβab");
        assert_eq!(dest.carets, after);
    }
    #[test]
    fn external_mismatch_and_whole_line_copy_keep_ordinary_paste_behavior() {
        let payload = selected().copy_payload();
        for text in ["external\nunequal\nrows", "plain"] {
            let mut actual = EditorState::from_text("ab", Language::PlainText);
            actual.set_carets(vec![
                Caret::at(TextSize::new(0)),
                Caret::at(TextSize::new(2)),
            ]);
            let mut expected = EditorState::from_text("ab", Language::PlainText);
            expected.set_carets(actual.carets.clone());
            actual.paste_payload(text, Some(&payload), &EditOptions::default());
            expected.paste(text, &EditOptions::default());
            assert_eq!(actual.doc.text(), expected.doc.text());
            assert_eq!(actual.carets, expected.carets);
        }
        let source = EditorState::from_text("whole\nline", Language::PlainText);
        let payload = source.copy_payload();
        assert!(payload.pieces.is_none());
        let mut dest = EditorState::from_text("target", Language::PlainText);
        dest.paste_payload(&payload.text, Some(&payload), &EditOptions::default());
        assert_eq!(dest.doc.text(), "whole\ntarget");
    }
    #[test]
    fn cut_payload_and_read_only_paste_preserve_exact_fragments() {
        let mut source = selected();
        let payload = source.copy_payload();
        assert_eq!(source.cut(), payload.text);
        assert!(source.undo());
        assert_eq!(source.copy_payload(), payload);
        let mut dest = selected();
        let before = dest.carets.clone();
        let text = dest.doc.text();
        dest.doc.set_read_only(true);
        dest.paste_payload(&payload.text, Some(&payload), &EditOptions::default());
        assert_eq!(dest.doc.text(), text);
        assert_eq!(dest.carets, before);
        assert!(dest.take_read_only_refusal());
        assert!(!dest.doc.can_undo());
        assert_eq!(
            dest.doc
                .slice(TextRange::new(TextSize::new(0), TextSize::new(5))),
            "é\nβ"
        );
    }

    #[test]
    fn rich_clipboard_reformat_preserves_empty_fragment_and_one_undo() {
        let mut source = EditorState::from_text("x=1+2\nmarker", Language::Python);
        source.set_carets(vec![
            Caret::new(TextSize::new(0), TextSize::new(6)),
            Caret::at(source.doc.len()),
        ]);
        let payload = source.copy_payload();
        assert_eq!(payload.pieces.as_ref().unwrap(), &["x=1+2\n", ""]);
        let original = "before = 0\n\nafter = 9\n";
        let mut dest = EditorState::from_text(original, Language::Python);
        dest.set_carets(vec![
            Caret::at(dest.doc.line_range(1).start()),
            Caret::at(dest.doc.len()),
        ]);
        let before = dest.carets.clone();
        let opts = EditOptions {
            paste_reformat: true,
            paste_indent_block: false,
            ..Default::default()
        };
        dest.paste_payload(&payload.text, Some(&payload), &opts);
        let formatted = "before = 0\nx = 1 + 2\n\nafter = 9\n";
        assert_eq!(dest.doc.text(), formatted);
        let after = dest.carets.clone();
        assert!(dest.undo());
        assert_eq!(dest.doc.text(), original);
        assert_eq!(dest.carets, before);
        assert!(!dest.doc.can_undo());
        assert!(dest.redo());
        assert_eq!(dest.doc.text(), formatted);
        assert_eq!(dest.carets, after);
    }
}
