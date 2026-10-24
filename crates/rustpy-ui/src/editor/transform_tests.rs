//! Editor transformations must preserve complete undo/redo and UTF-8 selections.
use super::{Caret, EditOptions, EditorState, Language};
use crate::actions::Action;
use rustpy_core::{Position, TextSize};
fn t(n: u32) -> TextSize {
    TextSize::new(n)
}

#[test]
fn case_expansion_restores_original_carets_and_mixed_word_targets() {
    let mut e = EditorState::from_text("İX beta", Language::PlainText);
    e.set_carets(vec![Caret::new(t(3), t(0)), Caret::at(t(5))]);
    let before = e.selections();
    e.toggle_case();
    assert_eq!(e.doc.text(), "i\u{307}x BETA");
    assert_eq!(e.doc.slice(e.carets[0].range()), "i\u{307}x");
    assert!(e.carets[0].sel.anchor > e.carets[0].sel.head);
    assert_eq!(e.doc.slice(e.carets[1].range()), "BETA");
    let after = e.selections();
    assert!(e.undo());
    assert_eq!(e.doc.text(), "İX beta");
    assert_eq!(e.selections(), before);
    assert!(e.redo());
    assert_eq!(e.doc.text(), "i\u{307}x BETA");
    assert_eq!(e.selections(), after);
}

#[test]
fn case_word_expansion_coalesces_carets_and_noop_does_not_add_history() {
    let mut e = EditorState::from_text("straße", Language::PlainText);
    e.set_carets(vec![Caret::at(t(1)), Caret::at(t(3))]);
    let before = e.selections();
    e.toggle_case();
    assert_eq!(e.doc.text(), "STRASSE");
    assert_eq!(e.carets.len(), 1);
    assert!(e.undo());
    assert_eq!(e.selections(), before);
    let mut numbers = EditorState::from_text("123", Language::PlainText);
    numbers.move_primary_to(Position::new(0, 1));
    let before = numbers.selections();
    numbers.toggle_case();
    assert_eq!(numbers.selections(), before);
    assert!(!numbers.doc.can_undo());
}

#[test]
fn line_transform_redo_restores_logical_caret_and_backward_selection() {
    let mut e = EditorState::from_text("longest\nx\nz\n", Language::PlainText);
    e.move_primary_to(Position::new(0, 6));
    let before = e.selections();
    e.reverse_lines();
    assert_eq!(e.primary_position(), Position::new(0, 1));
    let after = e.selections();
    assert!(e.undo());
    assert_eq!(e.selections(), before);
    assert!(e.redo());
    assert_eq!(e.selections(), after);
    e.set_carets(vec![Caret::new(t(4), t(0))]);
    e.reverse_lines();
    assert_eq!(e.doc.text(), "x\nz\nlongest\n");
    assert!(e.primary().sel.anchor > e.primary().sel.head);
}

#[test]
fn all_transformations_refuse_read_only_without_changing_text_or_selections() {
    let actions = [
        Action::ToggleCase,
        Action::SortLines,
        Action::ReverseLines,
        Action::RemoveDuplicateLines,
        Action::TrimTrailingWhitespace,
        Action::ConvertIndentsToSpaces,
        Action::ConvertIndentsToTabs,
    ];
    for action in actions {
        let mut e = EditorState::from_text("    b \n\ta\nb\nb\n", Language::PlainText);
        e.move_primary_to(Position::new(0, 5));
        e.doc.set_read_only(true);
        let text = e.doc.text();
        let before = e.selections();
        assert!(super::view::handle_editor_action(
            action,
            &mut e,
            &EditOptions::default()
        ));
        assert_eq!(e.doc.text(), text, "{action:?}");
        assert_eq!(e.selections(), before, "{action:?}");
        assert!(e.take_read_only_refusal(), "{action:?}");
        assert!(!e.doc.can_undo());
    }
}

#[test]
fn dedupe_includes_empty_lines_but_preserves_whitespace_distinct_lines() {
    let mut e = EditorState::from_text("α\n\nα\n\n \n", Language::PlainText);
    e.remove_duplicate_lines();
    assert_eq!(e.doc.text(), "α\n\n \n");
    assert!(e.undo());
    assert_eq!(e.doc.text(), "α\n\nα\n\n \n");
}

#[test]
fn selected_line_boundary_limits_trim_and_indent_conversion() {
    let mut e = EditorState::from_text("\tα \n\tβ \n", Language::PlainText);
    e.set_carets(vec![Caret::new(t(0), t(5))]); // next line start excludes that line
    let before = e.selections();
    e.trim_trailing_whitespace();
    assert_eq!(e.doc.text(), "\tα\n\tβ \n");
    e.convert_indents(false, &EditOptions::default());
    assert_eq!(e.doc.text(), "    α\n\tβ \n");
    assert!(e.undo());
    assert_eq!(e.doc.text(), "\tα\n\tβ \n");
    assert!(e.undo());
    assert_eq!(e.selections(), before);
}

#[test]
fn disjoint_line_blocks_keep_primary_caret_direction_and_undo_state() {
    let mut e = EditorState::from_text("b\na\nmiddle\nd\nc\n", Language::PlainText);
    e.set_carets(vec![Caret::new(t(15), t(11)), Caret::new(t(0), t(4))]);
    let before = e.selections();
    e.sort_lines();
    assert_eq!(e.doc.text(), "a\nb\nmiddle\nc\nd\n");
    assert_eq!(e.doc.slice(e.primary().range()), "c\nd");
    assert!(e.primary().sel.anchor > e.primary().sel.head);
    assert_eq!(e.doc.slice(e.carets[1].range()), "a\nb");
    let after = e.selections();
    assert!(e.undo());
    assert_eq!(e.selections(), before);
    assert!(e.redo());
    assert_eq!(e.selections(), after);
}

#[test]
fn adjacent_line_selections_coalesce_with_primary_direction() {
    let mut e = EditorState::from_text("b\na\n", Language::PlainText);
    e.set_carets(vec![Caret::new(t(0), t(1)), Caret::new(t(3), t(2))]);
    let before = e.selections();
    e.sort_lines();
    assert_eq!(e.doc.text(), "a\nb\n");
    assert!(e.primary().sel.head > e.primary().sel.anchor);
    assert!(e.undo());
    assert_eq!(e.selections(), before);
}

#[test]
fn line_transforms_preserve_crlf_serialization_and_missing_final_newline() {
    for (input, expected) in [("β\r\nα\r\n", "α\r\nβ\r\n"), ("β\r\nα", "α\r\nβ")] {
        let mut e = EditorState::from_text(input, Language::PlainText);
        e.sort_lines();
        assert_eq!(e.doc.to_bytes().unwrap(), expected.as_bytes());
        assert!(e.undo());
        assert_eq!(e.doc.to_bytes().unwrap(), input.as_bytes());
        assert!(e.redo());
        assert_eq!(e.doc.to_bytes().unwrap(), expected.as_bytes());
    }
}
