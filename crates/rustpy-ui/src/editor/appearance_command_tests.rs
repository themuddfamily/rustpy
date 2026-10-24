//! Editor command regressions for Insert/Overwrite presentation controls.

use rustpy_core::TextSize;

use super::{view::handle_editor_action, Caret, EditOptions, EditorState, Language};
use crate::actions::Action;

fn offset(bytes: u32) -> TextSize {
    TextSize::new(bytes)
}

fn editor(text: &str, carets: Vec<Caret>) -> EditorState {
    let mut state = EditorState::from_text(text, Language::PlainText);
    state.set_carets(carets);
    state
}

fn action(state: &mut EditorState, action: Action) {
    assert!(handle_editor_action(action, state, &EditOptions::default()));
}

#[test]
fn insert_action_switches_between_overwrite_and_insert_without_changing_source() {
    let mut state = editor("abcd", vec![Caret::at(offset(1))]);
    action(&mut state, Action::ToggleInsertState);
    assert!(state.overwrite_mode);
    assert_eq!(state.doc.text(), "abcd");
    state.insert_text("X", &EditOptions::default());
    assert_eq!(state.doc.text(), "aXcd");
    assert_eq!(state.primary().head(), offset(2));
    action(&mut state, Action::ToggleInsertState);
    assert!(!state.overwrite_mode);
    state.insert_text("Y", &EditOptions::default());
    assert_eq!(state.doc.text(), "aXYcd");
    assert_eq!(state.primary().head(), offset(3));
}

#[test]
fn overwrite_replaces_one_wide_unicode_character_and_undo_restores_exact_caret() {
    let original = "a界é\n";
    let before = vec![Caret::at(offset(1))];
    let mut state = editor(original, before.clone());
    action(&mut state, Action::ToggleInsertState);
    state.insert_text("λ", &EditOptions::default());
    assert_eq!(state.doc.text(), "aλé\n");
    assert_eq!(state.primary().head(), offset(3));
    action(&mut state, Action::Undo);
    assert_eq!(state.doc.text(), original);
    assert_eq!(state.carets, before);
    action(&mut state, Action::Redo);
    assert_eq!(state.doc.text(), "aλé\n");
    assert_eq!(state.primary().head(), offset(3));
}

#[test]
fn overwrite_at_line_end_inserts_without_consuming_the_newline() {
    let mut state = editor("ab\ncd", vec![Caret::at(offset(2))]);
    action(&mut state, Action::ToggleInsertState);
    state.insert_text("界", &EditOptions::default());
    assert_eq!(state.doc.text(), "ab界\ncd");
    assert_eq!(state.primary().head(), offset(5));
    action(&mut state, Action::Undo);
    assert_eq!(state.doc.text(), "ab\ncd");
    assert_eq!(state.primary().head(), offset(2));
}

#[test]
fn overwrite_at_eof_and_enter_keep_existing_characters() {
    let mut state = editor("a界", vec![Caret::at(offset(4))]);
    action(&mut state, Action::ToggleInsertState);
    state.insert_text("é", &EditOptions::default());
    assert_eq!(state.doc.text(), "a界é");
    assert_eq!(state.primary().head(), offset(6));
    action(&mut state, Action::Enter);
    assert_eq!(state.doc.text(), "a界é\n");
    assert_eq!(state.primary().head(), offset(7));
    action(&mut state, Action::Undo);
    assert_eq!(state.doc.text(), "a界é");
    assert_eq!(state.primary().head(), offset(6));
}

#[test]
fn overwrite_replaces_selected_unicode_range_without_consuming_following_text() {
    let original = "a界éz";
    let before = vec![Caret::new(offset(1), offset(6))];
    let mut state = editor(original, before.clone());
    action(&mut state, Action::ToggleInsertState);
    state.insert_text("X", &EditOptions::default());
    assert_eq!(state.doc.text(), "aXz");
    assert_eq!(state.carets, vec![Caret::at(offset(2))]);
    action(&mut state, Action::Undo);
    assert_eq!(state.doc.text(), original);
    assert_eq!(state.carets, before);
}

#[test]
fn multicaret_overwrite_is_one_undo_step_with_exact_unicode_offsets() {
    let original = "界a\néb\n";
    let before = vec![Caret::at(offset(0)), Caret::at(offset(5))];
    let mut state = editor(original, before.clone());
    action(&mut state, Action::ToggleInsertState);
    state.insert_text("λ", &EditOptions::default());
    assert_eq!(state.doc.text(), "λa\nλb\n");
    assert_eq!(
        state.carets,
        vec![Caret::at(offset(2)), Caret::at(offset(6))]
    );
    action(&mut state, Action::Undo);
    assert_eq!(state.doc.text(), original);
    assert_eq!(state.carets, before);
    assert!(!state.undo(), "one Undo exhausts the only text change");
    action(&mut state, Action::Redo);
    assert_eq!(state.doc.text(), "λa\nλb\n");
    assert_eq!(
        state.carets,
        vec![Caret::at(offset(2)), Caret::at(offset(6))]
    );
}

#[test]
fn overwrite_never_changes_read_only_source_carets_or_history() {
    let original = "界a\néb\n";
    let before = vec![Caret::at(offset(0)), Caret::at(offset(5))];
    let mut state = editor(original, before.clone());
    state.doc.set_read_only(true);
    action(&mut state, Action::ToggleInsertState);
    assert!(state.overwrite_mode);
    state.insert_text("λ", &EditOptions::default());
    assert_eq!(state.doc.text(), original);
    assert_eq!(state.carets, before);
    assert!(state.doc.is_read_only());
    assert!(!state.doc.is_modified());
    assert!(state.take_read_only_refusal());
    assert!(!state.undo());
    assert!(!state.redo());
}
