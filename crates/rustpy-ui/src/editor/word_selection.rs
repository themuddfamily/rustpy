//! Option-aware word selection using the same CamelHumps boundaries as word movement.
use rustpy_core::{TextRange, TextSize};
use rustpy_text::util::is_camel_hump;

use super::state::{Caret, EditOptions, EditorState, LastOp};

impl EditorState {
    /// At an internal hump boundary select the segment to the right; at the
    /// identifier's final boundary select its last segment. Underscores remain
    /// identifier characters, matching the editor's existing word movement.
    pub fn word_range_with_opts(&self, offset: TextSize, opts: &EditOptions) -> Option<TextRange> {
        let offset = self.doc.clamp(offset);
        let raw = self.doc.word_range_at(offset)?;
        let whole = TextRange::new(
            super::unicode_edit::floor_offset(self, raw.start()),
            super::unicode_edit::ceil_offset(self, raw.end()),
        );
        if !opts.camel_humps {
            return Some(whole);
        }
        let text = self.doc.slice(whole);
        let chars: Vec<_> = text.char_indices().collect();
        let mut boundaries = vec![whole.start()];
        for index in 1..chars.len() {
            if is_camel_hump(
                chars[index - 1].1,
                chars[index].1,
                chars.get(index + 1).map(|entry| entry.1),
            ) {
                boundaries.push(whole.start() + TextSize::new(chars[index].0 as u32));
            }
        }
        boundaries.push(whole.end());
        boundaries
            .windows(2)
            .find(|pair| {
                pair[0] <= offset
                    && (offset < pair[1] || offset == whole.end() && pair[1] == offset)
            })
            .map(|pair| TextRange::new(pair[0], pair[1]))
    }

    /// Double-click/select-word behavior. Already selected carets are preserved,
    /// including reversed selections; no text or undo history is changed.
    pub fn select_word_with_opts(&mut self, opts: &EditOptions) {
        let carets = self
            .carets
            .iter()
            .map(|caret| {
                if !caret.has_selection() {
                    if let Some(range) = self.word_range_with_opts(caret.head(), opts) {
                        return Caret::new(range.start(), range.end());
                    }
                }
                *caret
            })
            .collect();
        self.set_carets(carets);
    }

    /// Handles the initial Ctrl+W step when every caret has a word to select.
    /// Return false to run the existing syntax-aware extension path instead.
    /// Later Ctrl+W steps expand the hump into its identifier/expression normally.
    pub fn extend_word_selection_with_opts(&mut self, opts: &EditOptions) -> bool {
        if !opts.camel_humps || self.carets.iter().any(Caret::has_selection) {
            return false;
        }
        let Some(ranges) = self
            .carets
            .iter()
            .map(|caret| self.word_range_with_opts(caret.head(), opts))
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        if self.selection_extended.as_ref() != Some(&self.carets) {
            self.selection_history.clear();
        }
        self.selection_history.push(self.carets.clone());
        self.set_carets(
            ranges
                .into_iter()
                .map(|range| Caret::new(range.start(), range.end()))
                .collect(),
        );
        self.selection_extended = Some(self.carets.clone());
        self.last_op = LastOp::Other;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Language;

    fn opts() -> EditOptions {
        EditOptions {
            camel_humps: true,
            ..EditOptions::default()
        }
    }
    fn selected(text: &str, offset: usize, options: &EditOptions) -> String {
        let mut e = EditorState::from_text(text, Language::Python);
        e.set_carets(vec![Caret::at(TextSize::new(offset as u32))]);
        e.select_word_with_opts(options);
        e.doc.slice(e.primary().range())
    }

    #[test]
    fn camel_acronym_digit_and_exact_boundaries_match_movement_policy() {
        let text = "HTTPServer42Value";
        for (offset, expected) in [
            (0, "HTTP"),
            (3, "HTTP"),
            (4, "Server"),
            (9, "Server"),
            (10, "42"),
            (12, "Value"),
            (17, "Value"),
        ] {
            assert_eq!(selected(text, offset, &opts()), expected);
        }
        assert_eq!(selected("fooBarBaz", 3, &opts()), "Bar");
        assert_eq!(selected(text, 4, &EditOptions::default()), text);
    }

    #[test]
    fn underscores_unicode_and_stale_byte_offsets_remain_safe() {
        assert_eq!(selected("foo_barBaz", 2, &opts()), "foo_bar");
        assert_eq!(selected("foo_barBaz", 7, &opts()), "Baz");
        assert_eq!(selected("élanÜber", 5, &opts()), "Über");
        let e = EditorState::from_text("élanÜber", Language::Python);
        let range = e.word_range_with_opts(TextSize::new(6), &opts()).unwrap();
        assert_eq!(e.doc.slice(range), "Über");
        assert_eq!(selected("δeltaΒeta", 6, &opts()), "Βeta");
        assert_eq!(selected("snake_case", 5, &opts()), "snake_case");
    }

    #[test]
    fn multicaret_preserves_primary_existing_reversed_selection_and_document_history() {
        let mut e = EditorState::from_text("fooBar HTTPServer", Language::Python);
        e.set_carets(vec![
            Caret::at(TextSize::new(11)),
            Caret::new(TextSize::new(3), TextSize::new(0)),
        ]);
        let version = e.doc.version();
        e.doc.set_read_only(true);
        e.select_word_with_opts(&opts());
        assert_eq!(e.doc.slice(e.primary().range()), "Server");
        assert_eq!(e.carets[1].sel.anchor, TextSize::new(3));
        assert_eq!(e.carets[1].head(), TextSize::new(0));
        assert_eq!(e.doc.version(), version);
        assert!(!e.undo());
    }

    #[test]
    fn first_extend_selects_hump_then_existing_extension_and_shrink_restore_steps() {
        let mut e = EditorState::from_text("fooBar(x)", Language::Python);
        e.set_carets(vec![Caret::at(TextSize::new(4))]);
        let initial = e.carets.clone();
        assert!(e.extend_word_selection_with_opts(&opts()));
        assert_eq!(e.doc.slice(e.primary().range()), "Bar");
        assert!(!e.extend_word_selection_with_opts(&opts()));
        e.extend_selection();
        assert_eq!(e.doc.slice(e.primary().range()), "fooBar");
        e.shrink_selection();
        assert_eq!(e.doc.slice(e.primary().range()), "Bar");
        e.shrink_selection();
        assert_eq!(e.carets, initial);
    }

    #[test]
    fn punctuation_and_disabled_first_extension_delegate_without_mutation() {
        let mut e = EditorState::from_text(" (foo)", Language::Python);
        let before = e.carets.clone();
        assert!(!e.extend_word_selection_with_opts(&opts()));
        assert_eq!(e.carets, before);
        e.set_carets(vec![Caret::at(TextSize::new(3))]);
        assert!(!e.extend_word_selection_with_opts(&EditOptions::default()));
        assert!(e.selection_history.is_empty());
    }
}
