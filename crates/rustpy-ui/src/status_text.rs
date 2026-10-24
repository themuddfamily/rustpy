//! Document-based text counts for the status bar.

use rustpy_text::{Document, TextRange};

/// Sum selected Unicode code points, rather than UTF-8 bytes or graphemes.
/// Each caret contributes independently, preserving the status bar's existing
/// policy for overlapping selections. Empty carets contribute zero. Clamp stale
/// ranges through the document's normal EOF/UTF-8-boundary policy.
pub(crate) fn selected_codepoints(
    document: &Document,
    ranges: impl IntoIterator<Item = TextRange>,
) -> usize {
    let rope = document.rope();
    ranges
        .into_iter()
        .map(|range| {
            let range = document.clamp_range(range);
            rope.byte_to_char(usize::from(range.end()))
                - rope.byte_to_char(usize::from(range.start()))
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::state::Caret;
    use rustpy_text::TextSize;

    fn caret(anchor: u32, head: u32) -> Caret {
        Caret::new(TextSize::new(anchor), TextSize::new(head))
    }

    #[test]
    fn ascii_and_reversed_actual_caret_selections_count_the_same_characters() {
        let document = Document::new("abcdef");
        let forward = caret(1, 4);
        let reversed = caret(4, 1);
        assert_eq!(selected_codepoints(&document, [forward.range()]), 3);
        assert_eq!(selected_codepoints(&document, [reversed.range()]), 3);
    }

    #[test]
    fn accented_and_supplementary_characters_are_not_utf8_byte_counts() {
        let document = Document::new("aé🙂z");
        assert_eq!(selected_codepoints(&document, [caret(1, 3).range()]), 1);
        assert_eq!(selected_codepoints(&document, [caret(3, 7).range()]), 1);
        assert_eq!(selected_codepoints(&document, [caret(1, 7).range()]), 2);
        assert_eq!(selected_codepoints(&document, [caret(0, 8).range()]), 4);
    }

    #[test]
    fn combining_marks_count_as_codepoints_instead_of_graphemes() {
        let document = Document::new("e\u{301}");
        assert_eq!(selected_codepoints(&document, [caret(0, 3).range()]), 2);
        assert_eq!(selected_codepoints(&document, [caret(1, 3).range()]), 1);
    }

    #[test]
    fn selections_across_newlines_include_each_logical_newline() {
        let document = Document::new("é\n🙂\nx");
        assert_eq!(selected_codepoints(&document, [caret(0, 9).range()]), 5);
        assert_eq!(selected_codepoints(&document, [caret(2, 8).range()]), 3);
    }

    #[test]
    fn empty_carets_and_no_carets_contribute_zero() {
        let document = Document::new("é🙂");
        assert_eq!(selected_codepoints(&document, []), 0);
        assert_eq!(
            selected_codepoints(
                &document,
                [
                    caret(0, 0).range(),
                    caret(2, 2).range(),
                    caret(6, 6).range()
                ]
            ),
            0
        );
        assert_eq!(
            selected_codepoints(&Document::new(""), [caret(0, 0).range()]),
            0
        );
    }

    #[test]
    fn multiple_carets_sum_independently_including_overlap() {
        let document = Document::new("aé🙂z");
        let carets = [caret(0, 3), caret(7, 3), caret(1, 7), caret(8, 8)];
        // a+é, 🙂, é+🙂, and an empty caret. Overlap stays counted per caret.
        assert_eq!(
            selected_codepoints(&document, carets.iter().map(Caret::range)),
            5
        );
    }

    #[test]
    fn stale_eof_and_utf8_interior_ranges_follow_document_clamping() {
        let document = Document::new("é🙂");
        assert_eq!(selected_codepoints(&document, [caret(1, 5).range()]), 1);
        assert_eq!(selected_codepoints(&document, [caret(2, 100).range()]), 1);
        assert_eq!(selected_codepoints(&document, [caret(100, 200).range()]), 0);
    }
}
