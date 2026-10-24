use super::*;
use crate::editor::{state::EditOptions, Language};

fn editor(text: &str) -> EditorState {
    let mut state = EditorState::from_text(text, Language::PlainText);
    state.set_virtual_space_enabled(true);
    state
}
fn position(line: u32, column: u32) -> VirtualCaret {
    let head = Position::new(line, column);
    VirtualCaret { anchor: head, head }
}

#[test]
fn general_navigation_keeps_unicode_columns_without_editing_or_crossing_eol() {
    let mut state = editor("π😀\nx\nlong");
    state.set_virtual_carets(vec![position(0, 2)]);
    state.move_right(false);
    state.move_right(false);
    assert_eq!(state.virtual_caret(0), position(0, 4));
    assert_eq!(state.primary().head(), TextSize::new(6));
    state.move_vertical(1, false);
    assert_eq!(state.virtual_caret(0), position(1, 4));
    state.move_left(false);
    assert_eq!(state.virtual_caret(0), position(1, 3));
    state.move_vertical(1, false);
    assert_eq!(state.virtual_caret(0), position(2, 3));
    assert_eq!(state.doc.text(), "π😀\nx\nlong");
    assert!(!state.doc.can_undo());
}

#[test]
fn typing_materializes_shared_line_padding_once_and_undo_restores_virtual_carets() {
    let mut state = editor("π😀\nx");
    state.set_virtual_carets(vec![position(0, 5), position(0, 3), position(1, 2)]);
    assert_eq!(state.carets.len(), 3);
    let before: Vec<_> = (0..3).map(|i| state.virtual_caret(i)).collect();
    state.insert_text("Ω", &EditOptions::default());
    assert_eq!(state.doc.text(), "π😀 Ω  Ω\nx Ω");
    assert!(state.undo());
    assert_eq!(state.doc.text(), "π😀\nx");
    assert_eq!(
        (0..3).map(|i| state.virtual_caret(i)).collect::<Vec<_>>(),
        before
    );
    assert!(!state.doc.can_undo());
    assert!(state.redo());
    assert_eq!(state.doc.text(), "π😀 Ω  Ω\nx Ω");
}

#[test]
fn virtual_selection_copy_and_replace_are_real_command_routes() {
    let mut state = editor("π😀");
    state.set_virtual_carets(vec![VirtualCaret {
        anchor: Position::new(0, 3),
        head: Position::new(0, 6),
    }]);
    assert!(state.has_selection());
    assert_eq!(state.copy_text(), "   ");
    assert_eq!(state.copy_payload().pieces, Some(vec!["   ".into()]));
    state.insert_text("Ω", &EditOptions::default());
    assert_eq!(state.doc.text(), "π😀 Ω");
    assert!(state.undo());
    assert_eq!(state.doc.text(), "π😀");
    assert_eq!(state.virtual_caret(0).head.column, 6);
}

#[test]
fn mixed_real_and_virtual_backspace_and_delete_preserve_neighboring_lines() {
    let mut state = editor("π😀\nxy");
    state.set_virtual_carets(vec![position(0, 5), position(1, 2)]);
    state.backspace(&EditOptions::default());
    assert_eq!(state.doc.text(), "π😀\nx");
    assert_eq!(state.virtual_caret(0).head.column, 4);
    state.delete_forward();
    assert_eq!(state.doc.text(), "π😀\nx");
    state.set_virtual_carets(vec![position(0, 4)]);
    state.backspace(&EditOptions::default());
    assert_eq!(state.virtual_caret(0), position(0, 3));
    assert_eq!(state.doc.text(), "π😀\nx");
}

#[test]
fn readonly_virtual_edit_refuses_before_padding_and_disabled_mode_clips() {
    let mut state = editor("π😀");
    state.set_virtual_carets(vec![position(0, 7)]);
    state.doc.set_read_only(true);
    state.insert_text("Ω", &EditOptions::default());
    assert_eq!(state.doc.text(), "π😀");
    assert_eq!(state.virtual_caret(0), position(0, 7));
    assert!(state.take_read_only_refusal());
    assert!(!state.doc.can_undo());
    state.set_virtual_space_enabled(false);
    state.move_right(false);
    assert_eq!(state.primary().head(), state.doc.len());
    assert_eq!(state.virtual_caret(0), position(0, 2));
}

#[test]
fn sticky_selection_extends_all_navigation_without_shift_and_escape_ends_it() {
    let mut state = editor("one two\nπ😀");
    state.set_virtual_carets(vec![position(0, 3)]);
    state.toggle_sticky_selection();
    state.move_word_right(false, &EditOptions::default());
    assert_eq!(state.primary().sel.anchor, TextSize::new(3));
    assert!(state.has_selection());
    state.move_vertical(1, false);
    assert_eq!(state.virtual_caret(0).anchor, Position::new(0, 3));
    assert!(state.cancel_sticky_selection());
    assert!(!state.sticky_selection);
    assert!(!state.has_selection());
    assert!(!state.cancel_sticky_selection());
    state.move_left(false);
    assert!(!state.has_selection());
    assert_eq!(state.doc.text(), "one two\nπ😀");
}

#[test]
fn payload_paste_and_enter_materialize_virtual_space_in_one_undo_step() {
    let mut state = editor("π\nx");
    state.set_virtual_carets(vec![position(0, 3), position(1, 2)]);
    let payload = crate::editor::commands::clipboard_payload::ClipboardPayload {
        text: "A\n😀".into(),
        pieces: Some(vec!["A".into(), "😀".into()]),
    };
    state.paste_payload(&payload.text, Some(&payload), &EditOptions::default());
    assert_eq!(state.doc.text(), "π  A\nx 😀");
    assert!(state.undo());
    assert_eq!(state.doc.text(), "π\nx");
    state.set_virtual_carets(vec![position(0, 3)]);
    state.newline(&EditOptions::default());
    // Enter materializes its input position, then applies the existing Enter
    // policy that removes whitespace before the newline.
    assert_eq!(state.doc.text(), "π\n\nx");
    assert_eq!(state.virtual_caret(0), position(1, 0));
    assert!(state.undo());
    assert_eq!(state.doc.text(), "π\nx");
    assert_eq!(state.virtual_caret(0), position(0, 3));
    assert!(!state.undo(), "padding and Enter form one history entry");
    assert!(state.redo());
    assert_eq!(state.doc.text(), "π\n\nx");
    assert_eq!(state.virtual_caret(0), position(1, 0));
}

#[test]
fn sticky_horizontal_navigation_without_virtual_space_never_collapses_existing_selection() {
    let mut state = EditorState::from_text("abcd", Language::PlainText);
    state.toggle_sticky_selection();
    state.move_right(false);
    state.move_right(false);
    assert_eq!(
        state.primary().range(),
        rustpy_core::TextRange::new(TextSize::new(0), TextSize::new(2))
    );
    state.move_left(false);
    assert_eq!(
        state.primary().range(),
        rustpy_core::TextRange::new(TextSize::new(0), TextSize::new(1))
    );
    state.move_left(false);
    assert!(!state.has_selection());
    assert!(state.cancel_sticky_selection());
    state.set_carets(vec![Caret::new(TextSize::new(1), TextSize::new(3))]);
    state.toggle_sticky_selection();
    state.move_right(false);
    assert_eq!(state.primary().sel.anchor, TextSize::new(1));
    assert_eq!(state.primary().head(), TextSize::new(4));
    state.move_left(false);
    assert_eq!(state.primary().head(), TextSize::new(3));
    assert_eq!(state.doc.text(), "abcd");
    assert!(!state.doc.can_undo());
}

#[test]
fn wrapped_vertical_navigation_preserves_last_row_virtual_column_and_sticky_anchor() {
    let mut state = editor("abcdef\nx");
    state.soft_wrap = Some(crate::editor::soft_wrap::SoftWrap {
        original_indent: false,
        cols: 4,
        tab_size: 4,
    });
    state.set_virtual_carets(vec![position(0, 8)]);
    state.move_vertical(1, false);
    assert_eq!(state.virtual_caret(0), position(1, 4));
    state.move_vertical(-1, false);
    assert_eq!(state.virtual_caret(0), position(0, 8));
    state.toggle_sticky_selection();
    state.move_left(false);
    state.move_left(false);
    assert_eq!(state.virtual_caret(0).anchor, Position::new(0, 8));
    assert_eq!(state.virtual_caret(0).head, Position::new(0, 6));
    assert_eq!(state.copy_text(), "  ");
    assert_eq!(state.doc.text(), "abcdef\nx");
    assert!(!state.doc.can_undo());
}

#[test]
fn word_deletion_respects_virtual_whitespace_and_mixed_real_carets() {
    let mut state = editor("π😀\none two");
    state.set_virtual_carets(vec![position(0, 5), position(1, 7)]);
    state.delete_to_word_start(&EditOptions::default());
    assert_eq!(state.doc.text(), "π😀\none ");
    assert_eq!(state.virtual_caret(0), position(0, 2));
    assert!(state.undo());
    assert_eq!(state.doc.text(), "π😀\none two");
    assert_eq!(state.virtual_caret(0), position(0, 5));
    state.set_virtual_carets(vec![position(0, 5)]);
    state.delete_to_word_end(&EditOptions::default());
    assert_eq!(state.doc.text(), "π😀\none two");
    assert_eq!(state.virtual_caret(0), position(0, 5));
    state.delete_to_word_start(&EditOptions::default());
    assert_eq!(state.virtual_caret(0), position(0, 2));
    assert_eq!(state.doc.text(), "π😀\none two");
}

#[test]
fn virtual_cut_never_inserts_padding_and_block_policy_controls_column_paste() {
    let mut state = editor("π😀");
    state.set_virtual_carets(vec![VirtualCaret {
        anchor: Position::new(0, 3),
        head: Position::new(0, 5),
    }]);
    assert_eq!(state.cut(), "  ");
    assert_eq!(state.virtual_caret(0), position(0, 3));
    assert_eq!(state.doc.text(), "π😀");
    assert!(!state.doc.can_undo());
    for allowed in [false, true] {
        let mut state = EditorState::from_text("abcd\nx", Language::PlainText);
        state.column_mode = true;
        state.set_virtual_space_policy(false, allowed);
        state.set_block_selection(Position::new(0, 3), Position::new(1, 3));
        state.paste("Ω\n😀", &EditOptions::default());
        assert_eq!(
            state.doc.text(),
            if allowed {
                "abcΩd\nx  😀"
            } else {
                "abcΩd\nx😀"
            }
        );
        assert!(state.undo());
        assert_eq!(state.doc.text(), "abcd\nx");
        assert!(!state.doc.can_undo());
    }
}

#[test]
fn mixed_virtual_and_actual_cut_restores_selection_with_one_undo_and_readonly_refuses() {
    let mut state = editor("π😀\none");
    state.set_virtual_carets(vec![
        VirtualCaret {
            anchor: Position::new(0, 3),
            head: Position::new(0, 5),
        },
        VirtualCaret {
            anchor: Position::new(1, 0),
            head: Position::new(1, 3),
        },
    ]);
    assert_eq!(state.cut(), "  \none");
    assert_eq!(state.doc.text(), "π😀\n");
    assert_eq!(state.virtual_caret(0), position(0, 3));
    assert!(state.undo());
    assert_eq!(state.doc.text(), "π😀\none");
    assert_eq!(state.virtual_caret(0).head.column, 5);
    assert!(!state.doc.can_undo());
    state.doc.set_read_only(true);
    assert_eq!(state.cut(), "  \none");
    assert_eq!(state.doc.text(), "π😀\none");
    assert!(state.take_read_only_refusal());
    assert!(state.has_selection());
}

#[test]
fn block_only_virtual_selection_exposes_both_endpoints_and_typing_has_one_undo() {
    let mut state = EditorState::from_text("π\nx", Language::PlainText);
    state.set_virtual_space_policy(false, true);
    state.set_block_selection(Position::new(0, 3), Position::new(1, 5));
    assert_eq!(state.virtual_caret(0).anchor.column, 3);
    assert_eq!(state.virtual_caret(0).head.column, 5);
    assert_eq!(state.copy_text(), "  \n  ");
    state.insert_text("Ω", &EditOptions::default());
    assert_eq!(state.doc.text(), "π  Ω\nx  Ω");
    assert!(state.undo());
    assert_eq!(state.doc.text(), "π\nx");
    assert_eq!(state.virtual_caret(0).anchor.column, 3);
    assert_eq!(state.virtual_caret(0).head.column, 5);
    assert!(!state.doc.can_undo());
}

#[test]
fn emacs_virtual_copy_kill_and_yank_pop_use_intended_columns_with_one_undo() {
    use crate::editor::emacs::{EmacsEditing, Kill};
    let mut ring = EmacsEditing::default();
    let mut donor = EditorState::from_text("alpha beta", Language::PlainText);
    donor.set_carets(vec![Caret::new(TextSize::new(0), TextSize::new(5))]);
    assert!(ring.copy_region(&donor));
    donor.set_carets(vec![Caret::new(TextSize::new(6), TextSize::new(10))]);
    assert!(ring.copy_region(&donor));
    let mut state = editor("π😀");
    state.set_virtual_carets(vec![position(0, 4)]);
    assert!(ring.yank(&mut state));
    assert_eq!(state.doc.text(), "π😀  beta");
    assert!(ring.yank_pop(&mut state));
    assert_eq!(state.doc.text(), "π😀  alpha");
    assert!(state.undo());
    assert_eq!(state.doc.text(), "π😀  beta");
    assert!(state.undo());
    assert_eq!(state.doc.text(), "π😀");
    assert_eq!(state.virtual_caret(0), position(0, 4));
    assert!(!state.doc.can_undo());
    state.set_virtual_carets(vec![position(0, 5)]);
    assert!(ring.kill(&mut state, Kill::WordBackward, &EditOptions::default()));
    assert_eq!(ring.latest_text(), Some("   ".into()));
    assert_eq!(state.virtual_caret(0), position(0, 2));
    assert_eq!(state.doc.text(), "π😀");
    assert!(!state.doc.can_undo());
    state.set_virtual_carets(vec![VirtualCaret {
        anchor: Position::new(0, 3),
        head: Position::new(0, 5),
    }]);
    assert!(ring.copy_region(&state));
    assert_eq!(ring.latest_text(), Some("  ".into()));
    assert!(ring.kill(&mut state, Kill::Region, &EditOptions::default()));
    assert_eq!(state.doc.text(), "π😀");
    assert_eq!(state.virtual_caret(0), position(0, 3));
}

#[test]
fn emacs_virtual_transpose_and_refusal_do_not_mutate_clamped_source() {
    use crate::editor::emacs::EmacsEditing;
    let mut ring = EmacsEditing::default();
    let mut state = editor("π😀");
    state.set_virtual_carets(vec![position(0, 3)]);
    assert!(ring.transpose(&mut state));
    assert_eq!(state.doc.text(), "π 😀");
    assert!(state.undo());
    assert_eq!(state.doc.text(), "π😀");
    assert_eq!(state.virtual_caret(0), position(0, 3));
    assert!(!state.doc.can_undo());
    state.doc.set_read_only(true);
    assert!(!ring.transpose(&mut state));
    assert!(state.take_read_only_refusal());
    assert_eq!(state.doc.text(), "π😀");
    assert_eq!(state.virtual_caret(0), position(0, 3));
}
