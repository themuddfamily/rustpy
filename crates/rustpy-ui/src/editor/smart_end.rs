//! Smart End toggles between the last content character and physical line end.
use rustpy_core::TextSize;
use rustpy_text::Selection;

use super::state::{Caret, EditOptions, EditorState, LastOp};

impl EditorState {
    /// Mirrors Smart Home's two-position toggle. With Smart Home/End disabled,
    /// retain the existing physical-end behavior. Whitespace-only lines toggle
    /// between column zero and their physical end; no trailing bytes are edited.
    pub fn move_line_end_with_opts(&mut self, select: bool, opts: &EditOptions) {
        let select = select || self.sticky_selection;
        let virtual_before: Vec<_> = (0..self.carets.len())
            .map(|i| self.virtual_caret(i))
            .collect();
        let carets: Vec<_> = self
            .carets
            .iter()
            .map(|caret| {
                let line = self.doc.line_of(caret.head());
                let range = self.doc.line_range(line);
                let content_end = range.start()
                    + TextSize::new(
                        self.doc
                            .line_text(line)
                            .trim_end_matches(char::is_whitespace)
                            .len() as u32,
                    );
                let head = if !opts.smart_home || caret.head() == content_end {
                    range.end()
                } else {
                    content_end
                };
                Caret {
                    sel: if select {
                        Selection::new(caret.sel.anchor, head)
                    } else {
                        Selection::caret(head)
                    },
                    goal_column: None,
                }
            })
            .collect();
        for caret in &carets {
            self.reveal_offset(caret.head());
        }
        if self.virtual_space.enabled {
            let positions = carets
                .iter()
                .zip(virtual_before)
                .map(
                    |(caret, before)| super::state::virtual_space::VirtualCaret {
                        anchor: if select {
                            before.anchor
                        } else {
                            self.doc.offset_to_position(caret.head())
                        },
                        head: self.doc.offset_to_position(caret.head()),
                    },
                )
                .collect();
            self.set_virtual_carets(positions);
        } else {
            self.set_carets(carets);
        }
        self.last_op = LastOp::Other;
        self.scroll_to_caret = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Language;
    use rustpy_core::Position;

    #[test]
    fn smart_end_toggles_content_and_physical_end_without_editing() {
        let mut e = EditorState::from_text("  αβ \t", Language::Python);
        let version = e.doc.version();
        let opts = EditOptions::default();
        e.move_line_end_with_opts(false, &opts);
        assert_eq!(e.primary().head(), TextSize::new(6));
        e.move_line_end_with_opts(false, &opts);
        assert_eq!(e.primary().head(), TextSize::new(8));
        e.move_line_end_with_opts(false, &opts);
        assert_eq!(e.primary().head(), TextSize::new(6));
        assert_eq!(e.doc.version(), version);
        assert_eq!(e.doc.text(), "  αβ \t");
        assert!(!e.undo());
    }

    #[test]
    fn disabled_end_retains_old_behavior_and_selection_keeps_anchor() {
        let mut e = EditorState::from_text("foo  ", Language::Python);
        e.move_primary_to(Position::new(0, 1));
        let opts = EditOptions {
            smart_home: false,
            ..EditOptions::default()
        };
        e.move_line_end_with_opts(true, &opts);
        assert_eq!(e.primary().sel.anchor, TextSize::new(1));
        assert_eq!(e.primary().head(), TextSize::new(5));
        e.move_line_end_with_opts(true, &EditOptions::default());
        assert_eq!(e.primary().sel.anchor, TextSize::new(1));
        assert_eq!(e.primary().head(), TextSize::new(3));
        e.move_line_end_with_opts(true, &EditOptions::default());
        assert_eq!(e.primary().head(), TextSize::new(5));
    }

    #[test]
    fn blank_empty_unicode_whitespace_and_multicaret_goal_columns() {
        let mut e = EditorState::from_text(" \t\nfoo\u{2003}\n", Language::Python);
        e.set_carets(vec![
            Caret {
                sel: Selection::caret(TextSize::new(4)),
                goal_column: Some(10),
            },
            Caret::at(TextSize::new(1)),
            Caret::at(e.doc.len()),
        ]);
        e.doc.set_read_only(true);
        let opts = EditOptions::default();
        e.move_line_end_with_opts(false, &opts);
        assert_eq!(e.primary().head(), TextSize::new(6));
        assert_eq!(e.carets[1].head(), TextSize::new(0));
        assert_eq!(e.carets[2].head(), e.doc.len());
        assert!(e.carets.iter().all(|caret| caret.goal_column.is_none()));
        e.move_line_end_with_opts(false, &opts);
        assert_eq!(e.primary().head(), TextSize::new(9));
        assert_eq!(e.carets[1].head(), TextSize::new(2));
        assert!(!e.take_read_only_refusal());
    }

    #[test]
    fn folded_header_end_keeps_body_folded() {
        let mut e = EditorState::from_text("def f():  \n    return 1\n", Language::Python);
        e.toggle_fold_at_line(0);
        assert_eq!(e.hidden_line_ranges(), vec![(1, 1)]);
        e.move_line_end_with_opts(false, &EditOptions::default());
        assert_eq!(e.primary().head(), TextSize::new(8));
        assert_eq!(e.hidden_line_ranges(), vec![(1, 1)]);
        e.move_line_end_with_opts(false, &EditOptions::default());
        assert_eq!(e.primary().head(), TextSize::new(10));
        assert_eq!(e.hidden_line_ranges(), vec![(1, 1)]);
    }

    #[test]
    fn crlf_document_end_targets_normalized_line_not_newline_and_preserves_serialization() {
        let mut e = EditorState::new(
            rustpy_text::Document::from_bytes(b"a  \r\nb\r\n").unwrap(),
            None,
        );
        let before = e.doc.to_bytes().unwrap();
        e.move_line_end_with_opts(false, &EditOptions::default());
        assert_eq!(e.primary().head(), TextSize::new(1));
        e.move_line_end_with_opts(false, &EditOptions::default());
        assert_eq!(e.primary().head(), TextSize::new(3));
        assert_eq!(e.doc.to_bytes().unwrap(), before);
    }
}
