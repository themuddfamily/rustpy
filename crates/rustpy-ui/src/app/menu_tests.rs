//! Every action offered in the Edit menu and the editor context menu must
//! do something: either the editor runs it (`view::is_editor_action`) or
//! `IdeApp::handle_action` has an explicit arm for it, so nothing ends in the
//! "is not implemented yet" catch-all.

use std::collections::HashSet;

use crate::actions::Action;
use crate::editor::view;

/// `Action::Name` mentions in `src[from..]` up to the first `until` marker.
fn actions_in(src: &str, from: &str, until: &str) -> Vec<Action> {
    let start = src
        .find(from)
        .unwrap_or_else(|| panic!("marker {from:?} not found"));
    let rest = &src[start..];
    let block = &rest[..rest
        .find(until)
        .unwrap_or_else(|| panic!("marker {until:?} not found"))];
    let mut out = Vec::new();
    for (i, _) in block.match_indices("Action::") {
        // Match the Action type token, not the suffix of ContextAction.
        if block[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let name: String = block[i + "Action::".len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let action = Action::ALL
            .iter()
            .copied()
            .find(|a| format!("{a:?}") == name)
            .unwrap_or_else(|| panic!("unknown action {name}"));
        if !out.contains(&action) {
            out.push(action);
        }
    }
    assert!(!out.is_empty(), "no actions between {from:?} and {until:?}");
    out
}

#[test]
fn action_mentions_require_a_complete_type_token() {
    let source = "start Action::Cut crate::actions::Action::Paste crate::run_context::ContextAction::Launch(mode) Action::Copy end";
    assert_eq!(
        actions_in(source, "start", "end"),
        vec![Action::Cut, Action::Paste, Action::Copy]
    );
}

#[test]
#[should_panic(expected = "unknown action Launch")]
fn unknown_action_type_variant_is_still_rejected() {
    actions_in("start Action::Launch end", "start", "end");
}

/// Variant names matched by an arm of `IdeApp::handle_action`.
fn handled_actions(app_src: &str) -> HashSet<String> {
    super::source_inspection::handled_actions(app_src)
}

#[test]
fn edit_and_context_menu_actions_are_all_handled() {
    let app_src = include_str!("../app.rs");
    let view_src = include_str!("../editor/view.rs");
    let handled = handled_actions(app_src);
    assert!(
        handled.contains("Cut") && handled.contains("Paste") && handled.contains("PasteSimple"),
        "{handled:?}"
    );
    let mut actions = actions_in(
        app_src,
        "ui.menu_button(\"Edit\"",
        "ui.menu_button(\"View\"",
    );
    let context_actions = actions_in(
        view_src,
        "crate::a11y::bound_keyboard_context_menu(&resp,",
        "// Drag-and-drop",
    );
    assert!(
        context_actions.contains(&Action::Cut)
            && context_actions.contains(&Action::Copy)
            && context_actions.contains(&Action::Paste),
        "the actual authority-bound editor menu is inspected: {context_actions:?}"
    );
    actions.extend(context_actions);
    let missing: Vec<Action> = actions
        .into_iter()
        .filter(|a| !view::is_editor_action(*a) && !handled.contains(&format!("{a:?}")))
        .collect();
    assert!(
        missing.is_empty(),
        "menu actions that would hit the 'not implemented' catch-all: {missing:?}"
    );
}

#[test]
fn view_menu_and_tool_window_actions_are_all_handled() {
    let app_src = include_str!("../app.rs");
    let handled = handled_actions(app_src);
    let actions = actions_in(
        app_src,
        "ui.menu_button(\"View\"",
        "ui.menu_button(\"Navigate\"",
    );
    assert!(
        actions.contains(&Action::ToggleDistractionFree)
            && actions.contains(&Action::JumpToLastToolWindow),
        "{actions:?}"
    );
    let missing: Vec<Action> = actions
        .into_iter()
        .filter(|a| !view::is_editor_action(*a) && !handled.contains(&format!("{a:?}")))
        .collect();
    assert!(
        missing.is_empty(),
        "View menu actions that would hit the 'not implemented' catch-all: {missing:?}"
    );
    for a in [
        Action::JumpToLastToolWindow,
        Action::ToggleCommitView,
        Action::HideAllToolWindows,
        Action::ToggleNotificationsView,
    ] {
        assert!(
            handled.contains(&format!("{a:?}")),
            "{a:?} (F12, Alt+0, Ctrl+Shift+F12) needs an arm"
        );
    }
}

#[test]
fn new_editor_actions_are_dispatched_by_the_editor() {
    for a in [
        Action::ScrollUp,
        Action::ScrollDown,
        Action::CodeBlockStart,
        Action::CodeBlockEnd,
        Action::CommentBlock,
        Action::MoveElementLeft,
        Action::MoveElementRight,
        Action::SortLines,
        Action::ReverseLines,
        Action::RemoveDuplicateLines,
        Action::TrimTrailingWhitespace,
        Action::ConvertIndentsToSpaces,
        Action::ConvertIndentsToTabs,
    ] {
        assert!(view::is_editor_action(a), "{a:?}");
        let mut state =
            crate::editor::EditorState::from_text("b\na\n", crate::editor::Language::Python);
        assert!(
            view::handle_editor_action(a, &mut state, &crate::editor::EditOptions::default()),
            "{a:?}"
        );
    }
}
