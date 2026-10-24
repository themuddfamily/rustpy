//! Grapheme editing only. Protocol/analysis offsets remain raw UTF-8 byte boundaries.
use super::state::EditorState;
use rustpy_core::TextSize;
use unicode_segmentation::UnicodeSegmentation;
pub fn previous(state: &EditorState, offset: TextSize) -> TextSize {
    let line = state.doc.line_of(offset);
    let range = state.doc.line_range(line);
    if offset <= range.start() {
        return state.doc.prev_boundary(offset);
    }
    let text = state.doc.line_text(line);
    let at = usize::from(offset - range.start()).min(text.len());
    let previous = text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .take_while(|i| *i < at)
        .last()
        .unwrap_or(0);
    range.start() + TextSize::new(previous as u32)
}
pub fn next(state: &EditorState, offset: TextSize) -> TextSize {
    let line = state.doc.line_of(offset);
    let range = state.doc.line_range(line);
    let text = state.doc.line_text(line);
    let at = usize::from(offset - range.start()).min(text.len());
    if at >= text.len() {
        return state.doc.next_boundary(offset);
    }
    let end = text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .find(|i| *i > at)
        .unwrap_or(text.len());
    range.start() + TextSize::new(end as u32)
}
pub fn floor_column(text: &str, column: usize) -> usize {
    let byte = text
        .char_indices()
        .nth(column)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let start = text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .take_while(|i| *i <= byte)
        .last()
        .unwrap_or(0);
    if byte == text.len() {
        column.min(text.chars().count())
    } else {
        text[..start].chars().count()
    }
}
pub fn floor_offset(state: &EditorState, offset: TextSize) -> TextSize {
    let p = state.doc.offset_to_position(offset);
    let text = state.doc.line_text(p.line as usize);
    let column = floor_column(&text, p.column as usize);
    state
        .doc
        .position_to_offset(rustpy_core::Position::new(p.line, column as u32))
}
pub fn ceil_offset(state: &EditorState, offset: TextSize) -> TextSize {
    let floor = floor_offset(state, offset);
    if floor == offset {
        offset
    } else {
        next(state, floor)
    }
}
#[cfg(test)]
mod tests {
    use super::super::{EditOptions, Language};
    use super::*;
    #[test]
    fn movement_and_deletion_keep_combining_zwj_flags_and_literal_normalization() {
        let mut state = EditorState::from_text("e\u{301}👩🏽‍💻🇯🇵가", Language::PlainText);
        state.move_doc_end(false);
        state.move_left(false);
        assert_eq!(
            state.primary().head(),
            TextSize::new("e\u{301}👩🏽‍💻🇯🇵".len() as u32)
        );
        state.backspace(&EditOptions::default());
        assert_eq!(state.doc.text(), "e\u{301}👩🏽‍💻가");
        state.move_left(false);
        assert_eq!(
            state.primary().head(),
            TextSize::new("e\u{301}".len() as u32)
        );
        state.delete_forward();
        assert_eq!(state.doc.text(), "e\u{301}가");
        state.backspace(&EditOptions::default());
        assert_eq!(state.doc.text(), "가");
    }
    #[test]
    fn semantic_caret_inside_cluster_deletes_whole_cluster_and_undo_restores_exact_bytes() {
        let source = "a e\u{301}👩🏽‍💻 z";
        let mut state = EditorState::from_text(source, Language::PlainText);
        state.set_carets(vec![super::super::Caret::at(TextSize::new(
            "a e".len() as u32
        ))]);
        state.delete_forward();
        assert_eq!(state.doc.text(), "a 👩🏽‍💻 z");
        assert!(state.undo());
        assert_eq!(state.doc.text().as_bytes(), source.as_bytes());
    }
    #[test]
    fn vertical_movement_snaps_out_of_combining_clusters() {
        let mut state = EditorState::from_text("abc\ne\u{301}z", Language::PlainText);
        state.set_carets(vec![super::super::Caret::at(TextSize::new(1))]);
        state.move_vertical(1, false);
        assert_eq!(state.primary().head(), TextSize::new(4));
    }
    #[test]
    fn word_movement_and_selection_keep_attached_marks() {
        let source = "cafe\u{301}";
        let mut state = EditorState::from_text(source, Language::PlainText);
        state.move_word_right(false, &EditOptions::default());
        assert_eq!(state.primary().head(), state.doc.len());
        state.set_carets(vec![super::super::Caret::at(TextSize::new(1))]);
        state.select_word_with_opts(&EditOptions::default());
        assert_eq!(state.doc.slice(state.primary().range()), source);
    }
}
