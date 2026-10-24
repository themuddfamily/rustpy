//! Fold clients exercise the actual document registry, including metadata rollback.
use super::{EditorState, Language};
use rustpy_core::{TextRange, TextSize};
use rustpy_text::{Edit, EditGroup};

fn ts(n: usize) -> TextSize {
    TextSize::new(n as u32)
}
fn editor(text: &str) -> EditorState {
    EditorState::from_text(text, Language::Python)
}
fn change(e: &mut EditorState, start: usize, end: usize, text: &str) {
    e.doc
        .edit(
            vec![Edit::replace(TextRange::new(ts(start), ts(end)), text)],
            &[],
            &[],
            EditGroup::Separate,
            None,
        )
        .unwrap();
}
const FUNCTION: &str = "def f():\n    a = 1\n    b = 2\n";

#[test]
fn byte_zero_import_and_docstring_folds_follow_exact_start_insertions() {
    for (source, imports, docs, expected) in [
        ("import os\nimport sys\nx = 1\n", true, false, vec![(2, 2)]),
        (
            "\"\"\"Summary.\nDetails.\n\"\"\"\nx = 1\n",
            false,
            true,
            vec![(2, 3)],
        ),
    ] {
        let mut e = editor(source);
        e.apply_default_folds(imports, docs);
        assert!(!e.hidden_line_ranges().is_empty());
        change(&mut e, 0, 0, "# π\r\n");
        assert_eq!(e.hidden_line_ranges(), expected);
        assert!(e.doc.undo().is_some());
        assert!(!e.hidden_line_ranges().is_empty());
        assert!(e.doc.redo().is_some());
        assert_eq!(e.hidden_line_ranges(), expected);
    }
}

#[test]
fn inactive_folds_outlive_journal_and_net_zero_line_edits() {
    let mut e = editor(FUNCTION);
    e.toggle_fold_at_line(0);
    for _ in 0..70 {
        change(&mut e, 0, 0, "# π\n");
    }
    for _ in 0..70 {
        change(&mut e, 0, "# π\n".len(), "");
    }
    assert_eq!(e.hidden_line_ranges(), vec![(1, 2)]);
}

#[test]
fn tentative_containing_deletion_and_syntax_refresh_restore_on_rollback() {
    let mut e = editor(&format!("# lead\n{FUNCTION}"));
    e.toggle_fold_at_line(1);
    let outer = e.doc.begin_transaction(&[], Some("outer")).unwrap();
    let inner = e.doc.begin_transaction(&[], Some("inner")).unwrap();
    let len = usize::from(e.doc.len());
    change(&mut e, 0, len, "x = 1\n");
    assert!(e.hidden_line_ranges().is_empty());
    e.doc.commit_transaction(inner, &[]).unwrap();
    assert!(e.hidden_line_ranges().is_empty());
    e.doc.rollback_transaction(outer).unwrap();
    assert_eq!(e.hidden_line_ranges(), vec![(2, 3)]);
}

#[test]
fn metadata_only_expand_and_collapse_rollbacks_refresh_without_version_change() {
    let mut e = editor(FUNCTION);
    e.toggle_fold_at_line(0);
    let version = e.doc.version();
    let scope = e.doc.begin_transaction(&[], None).unwrap();
    e.expand_all_folds();
    assert!(e.hidden_line_ranges().is_empty());
    e.doc.rollback_transaction(scope).unwrap();
    assert_eq!(e.doc.version(), version);
    assert_eq!(e.hidden_line_ranges(), vec![(1, 2)]);
    e.expand_all_folds();
    let scope = e.doc.begin_transaction(&[], None).unwrap();
    e.collapse_all_folds();
    assert_eq!(e.hidden_line_ranges(), vec![(1, 2)]);
    e.doc.rollback_transaction(scope).unwrap();
    assert!(e.hidden_line_ranges().is_empty());
    assert!(e.fold_markers.is_empty());
}

#[test]
fn committed_deletion_prunes_intent_and_ordinary_undo_does_not_revive_it() {
    let mut e = editor(&format!("# lead\n{FUNCTION}"));
    e.toggle_fold_at_line(1);
    let scope = e.doc.begin_transaction(&[], None).unwrap();
    let len = usize::from(e.doc.len());
    change(&mut e, 0, len, "x = 1\n");
    assert!(e.hidden_line_ranges().is_empty());
    assert!(!e.fold_markers.is_empty());
    e.doc.commit_transaction(scope, &[]).unwrap();
    assert!(e.hidden_line_ranges().is_empty());
    assert!(e.fold_markers.is_empty());
    e.doc.undo().unwrap();
    assert!(e.hidden_line_ranges().is_empty());
}

#[test]
fn retained_history_branches_move_original_fold_anchor() {
    let mut e = editor(FUNCTION);
    e.toggle_fold_at_line(0);
    change(&mut e, 0, 0, "# first\n");
    let first = e.doc.current_history();
    e.doc.undo().unwrap();
    change(&mut e, 0, 0, "# second\n# branch\n");
    let second = e.doc.current_history();
    assert_eq!(e.hidden_line_ranges(), vec![(3, 4)]);
    e.doc.select_history(first).unwrap();
    assert_eq!(e.hidden_line_ranges(), vec![(2, 3)]);
    e.doc.select_history(second).unwrap();
    assert_eq!(e.hidden_line_ranges(), vec![(3, 4)]);
}

#[test]
fn replacing_document_at_equal_version_expires_fold_and_syntax_identity() {
    let mut e = editor(FUNCTION);
    e.toggle_fold_at_line(0);
    let other = editor("def g():\n    pass\n");
    assert_eq!(e.doc.version(), other.doc.version());
    e.doc = other.doc;
    assert!(e.hidden_line_ranges().is_empty());
    assert!(e.fold_markers.is_empty());
    assert_eq!(e.fold_regions()[0].last_line, 1);
}

#[test]
fn explicit_unfold_and_language_reset_do_not_leave_recollapsing_markers() {
    let mut e = editor(FUNCTION);
    e.toggle_fold_at_line(0);
    let start = e.fold_regions()[0].range.start();
    e.unfold_start(start);
    change(&mut e, 0, 0, "# above\n");
    assert!(e.hidden_line_ranges().is_empty());
    e.collapse_all_folds();
    e.set_language(Language::PlainText);
    e.set_language(Language::Python);
    assert!(e.hidden_line_ranges().is_empty());
}

#[test]
fn extending_import_group_at_byte_zero_keeps_original_group_folded() {
    let mut e = editor("import os\nimport sys\nx = 1\n");
    e.apply_default_folds(true, false);
    change(&mut e, 0, 0, "import pathlib\n");
    assert_eq!(e.hidden_line_ranges(), vec![(1, 2)]);
    // Expansion must remove the original anchor inside the enlarged group.
    e.unfold_start(ts(0));
    assert!(e.hidden_line_ranges().is_empty());
    change(&mut e, 0, 0, "# prefix\n");
    assert!(e.hidden_line_ranges().is_empty());
}

#[test]
fn token_end_folds_stay_before_exact_anchor_header_comments() {
    for source in [
        FUNCTION,
        "class C:\n    a = 1\n    b = 2\n",
        "if True:\n    a = 1\n    b = 2\n",
        "values = [\n    1,\n    2,\n]\n",
    ] {
        let mut e = editor(source);
        e.toggle_fold_at_line(0);
        let hidden = e.hidden_line_ranges();
        assert!(!hidden.is_empty());
        let anchor = usize::from(e.fold_regions()[0].range.start());
        change(&mut e, anchor, anchor, " # trailing note");
        assert_eq!(e.hidden_line_ranges(), hidden, "{source}");
        e.doc.undo().unwrap();
        assert_eq!(e.hidden_line_ranges(), hidden);
        e.doc.redo().unwrap();
        assert_eq!(e.hidden_line_ranges(), hidden);
    }
}

#[test]
fn raw_prefix_extends_original_folded_docstring_token() {
    let mut e = editor("\"\"\"Summary.\nDetails.\n\"\"\"\nx = 1\n");
    e.apply_default_folds(false, true);
    let hidden = e.hidden_line_ranges();
    change(&mut e, 0, 0, "r");
    assert_eq!(e.hidden_line_ranges(), hidden);
    e.doc.undo().unwrap();
    assert_eq!(e.hidden_line_ranges(), hidden);
    e.doc.redo().unwrap();
    assert_eq!(e.hidden_line_ranges(), hidden);
    e.unfold_start(ts(0));
    assert!(e.hidden_line_ranges().is_empty());
}
