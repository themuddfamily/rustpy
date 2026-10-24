use rustpy_text::{
    Document, Edit, EditError, EditGroup, HistoryError, Selection, TextRange, TextSize,
};

fn carets(offsets: &[u32]) -> Vec<Selection> {
    offsets
        .iter()
        .map(|&n| Selection::caret(TextSize::new(n)))
        .collect()
}
fn replace(doc: &mut Document, text: &str, before: &[Selection], after: &[Selection]) {
    doc.set_text(text, before, after).unwrap();
}

#[test]
fn branches_restore_unicode_text_and_multiple_selections() {
    let mut doc = Document::new("α");
    let root = doc.current_history();
    let a = carets(&[0, 2]);
    let b = carets(&[2, 6]);
    let c = carets(&[0, 4]);
    replace(&mut doc, "α🙂", &a, &b);
    let left = doc.current_history();
    assert_eq!(doc.undo(), Some(a.clone()));
    replace(&mut doc, "βλ", &a, &c);
    let right = doc.current_history();
    let history = doc.undo_history();
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].preferred_child, Some(right));
    assert_eq!(history[1].parent, Some(root));
    assert_eq!(history[2].depth, 1);
    assert_eq!(doc.select_history(left).unwrap(), b);
    assert_eq!(doc.text(), "α🙂");
    assert_eq!(doc.undo(), Some(a.clone()));
    assert_eq!(doc.redo(), Some(carets(&[2, 6])));
    assert_eq!(doc.select_history(right).unwrap(), c);
    assert_eq!(doc.text(), "βλ");
    assert_eq!(doc.select_history(root).unwrap(), a);
    assert_eq!(doc.text(), "α");
}

#[test]
fn saved_and_navigated_typing_groups_are_immutable() {
    let mut doc = Document::new("");
    let type_char = |doc: &mut Document, text: &str, group| {
        doc.edit(
            vec![Edit::insert(doc.len(), text)],
            &[],
            &[],
            group,
            Some("Typing"),
        )
        .unwrap();
    };
    type_char(&mut doc, "a", EditGroup::Open);
    type_char(&mut doc, "b", EditGroup::Merge);
    let saved = doc.current_history();
    doc.mark_saved();
    type_char(&mut doc, "c", EditGroup::Merge);
    assert_eq!(doc.undo_history().len(), 3);
    doc.undo();
    assert_eq!(doc.text(), "ab");
    assert!(!doc.is_modified());
    type_char(&mut doc, "d", EditGroup::Merge);
    let branch = doc.current_history();
    assert_eq!(doc.undo_history().len(), 4);
    doc.select_history(saved).unwrap();
    assert_eq!(doc.text(), "ab");
    assert!(!doc.is_modified());
    doc.select_history(branch).unwrap();
    assert_eq!(doc.text(), "abd");
    doc.undo();
    doc.undo();
    assert_eq!(doc.text(), "");
    assert_eq!(doc.redo_label().as_deref(), Some("Typing"));
}

#[test]
fn nested_commit_is_one_unit_with_outer_caret_restore() {
    let mut doc = Document::new("α");
    let before = carets(&[0, 2]);
    let after = carets(&[2, 6]);
    let outer = doc.begin_transaction(&before, Some("Refactor")).unwrap();
    replace(&mut doc, "β", &[], &[]);
    let inner = doc.begin_transaction(&carets(&[2]), Some("Inner")).unwrap();
    doc.edit(
        vec![Edit::insert(TextSize::new(2), "🙂")],
        &[],
        &after,
        EditGroup::Open,
        Some("Typing"),
    )
    .unwrap();
    doc.commit_transaction(inner, &after).unwrap();
    assert_eq!(doc.transaction_depth(), 1);
    assert_eq!(doc.undo_history().len(), 1);
    assert!(!doc.can_undo());
    assert_eq!(doc.undo(), None);
    doc.commit_transaction(outer, &after).unwrap();
    assert_eq!(doc.undo_label().as_deref(), Some("Refactor"));
    assert_eq!(doc.undo(), Some(before));
    assert_eq!(doc.text(), "α");
    assert_eq!(doc.redo(), Some(after));
    assert_eq!(doc.text(), "β🙂");
}

#[test]
fn inner_rollback_excludes_steps_and_outer_rollback_preserves_redo_branches() {
    let mut doc = Document::new("A");
    replace(&mut doc, "old future", &[], &[]);
    let future = doc.current_history();
    doc.undo();
    let outer = doc.begin_transaction(&carets(&[1]), Some("Outer")).unwrap();
    replace(&mut doc, "B", &[], &[]);
    let inner = doc.begin_transaction(&carets(&[1]), None).unwrap();
    replace(&mut doc, "discarded", &[], &[]);
    assert_eq!(doc.rollback_transaction(inner).unwrap(), carets(&[1]));
    assert_eq!(doc.text(), "B");
    let inner = doc.begin_transaction(&[], None).unwrap();
    replace(&mut doc, "C", &[], &[]);
    doc.commit_transaction(inner, &[]).unwrap();
    assert_eq!(doc.rollback_transaction(outer).unwrap(), carets(&[1]));
    assert_eq!(doc.text(), "A");
    assert_eq!(doc.undo_history().len(), 2);
    doc.select_history(future).unwrap();
    assert_eq!(doc.text(), "old future");

    let outer = doc.begin_transaction(&[], None).unwrap();
    replace(&mut doc, "kept", &[], &[]);
    let inner = doc.begin_transaction(&[], None).unwrap();
    replace(&mut doc, "discarded", &[], &[]);
    doc.rollback_transaction(inner).unwrap();
    doc.commit_transaction(outer, &[]).unwrap();
    doc.undo();
    assert_eq!(doc.text(), "old future");
    doc.redo();
    assert_eq!(doc.text(), "kept");
}

#[test]
fn scope_tokens_guards_and_failed_edits_are_atomic() {
    let mut doc = Document::new("é");
    let root = doc.current_history();
    let outer = doc.begin_transaction(&[], None).unwrap();
    let inner = doc.begin_transaction(&[], None).unwrap();
    let version = doc.version();
    assert_eq!(
        doc.commit_transaction(outer, &[]),
        Err(HistoryError::TransactionOrder)
    );
    assert_eq!(
        doc.rollback_transaction(outer),
        Err(HistoryError::TransactionOrder)
    );
    assert_eq!(
        doc.select_history(root),
        Err(HistoryError::TransactionActive)
    );
    assert_eq!(doc.set_undo_limit(0), Err(HistoryError::TransactionActive));
    let mut other = Document::new("");
    assert_eq!(
        other.commit_transaction(inner, &[]),
        Err(HistoryError::InvalidTransaction)
    );
    assert_eq!(
        doc.select_history(other.current_history()),
        Err(HistoryError::TransactionActive)
    );
    assert_eq!(
        doc.edit(
            vec![Edit::insert(TextSize::new(3), "x")],
            &[],
            &[],
            EditGroup::Separate,
            None
        ),
        Err(EditError::OutOfBounds(
            TextRange::empty(TextSize::new(3)),
            TextSize::new(2)
        ))
    );
    assert_eq!(doc.version(), version);
    doc.commit_transaction(inner, &[]).unwrap();
    assert_eq!(
        doc.rollback_transaction(inner),
        Err(HistoryError::InvalidTransaction)
    );
    doc.edit(vec![], &[], &[], EditGroup::Merge, None).unwrap();
    doc.commit_transaction(outer, &[]).unwrap();
    assert_eq!(doc.undo_history().len(), 1);
    assert_eq!(
        doc.select_history(other.current_history()),
        Err(HistoryError::UnknownHistory)
    );
    assert_eq!(doc.text(), "é");
}

#[test]
fn rollback_remains_available_after_read_only_and_failed_reload() {
    let mut doc = Document::new("A");
    let token = doc.begin_transaction(&carets(&[1]), None).unwrap();
    replace(&mut doc, "B", &[], &[]);
    assert!(doc.reload_from_bytes(b"# coding: utf-8\n\xff").is_err());
    assert_eq!(doc.transaction_depth(), 1);
    doc.set_read_only(true);
    assert_eq!(
        doc.begin_transaction(&[], None),
        Err(HistoryError::ReadOnly)
    );
    assert_eq!(
        doc.select_history(doc.current_history()),
        Err(HistoryError::ReadOnly)
    );
    assert_eq!(doc.rollback_transaction(token).unwrap(), carets(&[1]));
    assert_eq!(doc.text(), "A");
    assert!(!doc.is_modified());
    doc.set_read_only(false);
    let token = doc.begin_transaction(&[], None).unwrap();
    let id = doc.current_history();
    doc.reset("fresh");
    assert_eq!(
        doc.commit_transaction(token, &[]),
        Err(HistoryError::InvalidTransaction)
    );
    assert_eq!(doc.select_history(id), Err(HistoryError::UnknownHistory));
    let token = doc.begin_transaction(&[], None).unwrap();
    doc.reload_from_bytes(b"loaded").unwrap();
    assert_eq!(
        doc.rollback_transaction(token),
        Err(HistoryError::InvalidTransaction)
    );
}

#[test]
fn save_and_checkpoint_aliases_survive_pending_rollback_and_branches() {
    let mut doc = Document::new("A");
    let original = doc.content_checkpoint();
    replace(&mut doc, "B", &[], &[]);
    let branch = doc.current_history();
    doc.undo();
    let token = doc.begin_transaction(&[], None).unwrap();
    replace(&mut doc, "C", &[], &[]);
    let inner = doc.begin_transaction(&[], None).unwrap();
    replace(&mut doc, "A", &[], &[]);
    doc.mark_saved();
    assert!(doc.recognize_restored_content(&original));
    doc.rollback_transaction(inner).unwrap();
    assert!(doc.is_modified());
    doc.rollback_transaction(token).unwrap();
    assert_eq!(doc.text(), "A");
    assert!(!doc.is_modified());
    let root = doc.current_history();
    doc.select_history(branch).unwrap();
    assert!(doc.is_modified());
    doc.select_history(root).unwrap();
    assert!(!doc.is_modified());

    let token = doc.begin_transaction(&[], None).unwrap();
    replace(&mut doc, "saved inside", &[], &[]);
    doc.mark_saved();
    doc.rollback_transaction(token).unwrap();
    assert_eq!(doc.text(), "A");
    assert!(doc.is_modified());
}

#[test]
fn pruning_retains_current_state_and_never_leaves_dangling_ids() {
    let mut doc = Document::new("A");
    let root = doc.current_history();
    replace(&mut doc, "B", &[], &[]);
    let b = doc.current_history();
    replace(&mut doc, "C", &[], &[]);
    let c = doc.current_history();
    doc.undo();
    replace(&mut doc, "D", &[], &[]);
    let d = doc.current_history();
    doc.set_undo_limit(2).unwrap();
    assert_eq!(doc.select_history(c), Err(HistoryError::UnknownHistory));
    assert_eq!(doc.text(), "D");
    assert_eq!(doc.undo_history().len(), 3);
    assert_eq!(doc.select_history(b).unwrap(), Vec::<Selection>::new());
    doc.redo();
    assert_eq!(doc.current_history(), d);
    doc.set_undo_limit(1).unwrap();
    assert_eq!(doc.select_history(root), Err(HistoryError::UnknownHistory));
    doc.undo();
    assert_eq!(doc.text(), "B");
    assert!(!doc.can_undo());
    doc.redo();
    assert_eq!(doc.text(), "D");
    doc.set_undo_limit(0).unwrap();
    assert_eq!(doc.current_history(), d);
    assert_eq!(doc.undo_history().len(), 1);
    assert!(!doc.can_undo());
    assert!(!doc.can_redo());
    replace(&mut doc, "E", &[], &[]);
    assert_eq!(doc.undo_history().len(), 1);
    assert_eq!(doc.text(), "E");
}

#[test]
fn navigation_and_rollback_advance_versions_and_replayable_change_log() {
    let mut doc = Document::new("α");
    let root = doc.current_history();
    let version = doc.version();
    let token = doc.begin_transaction(&[], None).unwrap();
    replace(&mut doc, "α🙂", &[], &[]);
    replace(&mut doc, "β🙂", &[], &[]);
    doc.commit_transaction(token, &[]).unwrap();
    let branch = doc.current_history();
    doc.select_history(root).unwrap();
    assert_eq!(doc.version(), version + 4);
    doc.select_history(branch).unwrap();
    let token = doc.begin_transaction(&[], None).unwrap();
    replace(&mut doc, "discard", &[], &[]);
    doc.rollback_transaction(token).unwrap();
    assert_eq!(doc.version(), version + 8);
    let changes = doc.changes_since(version).unwrap();
    assert_eq!(changes.len(), 8);
    let mut replay = Document::new("α");
    for edits in changes {
        replay
            .edit(edits.to_vec(), &[], &[], EditGroup::Separate, None)
            .unwrap();
    }
    assert_eq!(replay.text(), doc.text());
    doc.select_history(branch).unwrap();
    assert_eq!(doc.version(), version + 8);
    for _ in 0..33 {
        doc.undo();
        doc.redo();
    }
    assert!(doc.changes_since(version).is_none());
}

#[test]
fn rollback_recognizes_a_save_of_exact_original_content_without_an_external_checkpoint() {
    let mut doc = Document::new("original");
    let root = doc.current_history();
    replace(&mut doc, "future", &[], &[]);
    let future = doc.current_history();
    doc.undo();
    let outer = doc.begin_transaction(&[], None).unwrap();
    replace(&mut doc, "different", &[], &[]);
    let inner = doc.begin_transaction(&[], None).unwrap();
    replace(&mut doc, "original", &[], &[]);
    doc.mark_saved();
    doc.rollback_transaction(inner).unwrap();
    assert_eq!(doc.text(), "different");
    assert!(doc.is_modified());
    doc.rollback_transaction(outer).unwrap();
    assert_eq!(doc.text(), "original");
    assert!(!doc.is_modified());
    doc.select_history(future).unwrap();
    assert!(doc.is_modified());
    doc.select_history(root).unwrap();
    assert!(!doc.is_modified());

    let scope = doc.begin_transaction(&[], None).unwrap();
    replace(&mut doc, "actually saved", &[], &[]);
    doc.mark_saved();
    doc.rollback_transaction(scope).unwrap();
    assert_eq!(doc.text(), "original");
    assert!(doc.is_modified());
}

#[test]
fn selecting_distant_branches_replays_only_the_route_and_keeps_preferred_redo() {
    let mut doc = Document::new("root");
    replace(&mut doc, "common", &carets(&[4]), &carets(&[6]));
    let common = doc.current_history();
    replace(&mut doc, "left", &carets(&[6]), &carets(&[4]));
    replace(&mut doc, "left leaf", &carets(&[4]), &carets(&[9]));
    let left = doc.current_history();
    doc.select_history(common).unwrap();
    replace(&mut doc, "right", &carets(&[6]), &carets(&[5]));
    replace(&mut doc, "right leaf", &carets(&[5]), &carets(&[10]));
    let right = doc.current_history();
    let version = doc.version();
    assert_eq!(doc.select_history(left).unwrap(), carets(&[9]));
    assert_eq!(doc.version(), version + 4);
    assert_eq!(doc.text(), "left leaf");
    doc.select_history(common).unwrap();
    doc.redo();
    doc.redo();
    assert_eq!(doc.current_history(), left);
    doc.select_history(right).unwrap();
    doc.select_history(common).unwrap();
    doc.redo();
    doc.redo();
    assert_eq!(doc.current_history(), right);
    assert_eq!(doc.text(), "right leaf");
}

#[test]
fn pruning_when_cursor_is_an_internal_node_repairs_redo_and_saved_root() {
    let mut doc = Document::new("root");
    replace(&mut doc, "saved", &[], &[]);
    doc.mark_saved();
    let saved = doc.current_history();
    replace(&mut doc, "left", &[], &[]);
    let left = doc.current_history();
    doc.undo();
    replace(&mut doc, "right", &[], &[]);
    let right = doc.current_history();
    doc.undo();
    doc.set_undo_limit(2).unwrap();
    assert_eq!(doc.current_history(), saved);
    assert_eq!(doc.select_history(left), Err(HistoryError::UnknownHistory));
    doc.redo();
    assert_eq!(doc.current_history(), right);
    doc.undo();
    doc.set_undo_limit(0).unwrap();
    assert_eq!(doc.current_history(), saved);
    assert_eq!(doc.text(), "saved");
    assert!(!doc.is_modified());
    assert_eq!(doc.undo_history().len(), 1);
    assert_eq!(doc.select_history(right), Err(HistoryError::UnknownHistory));
}

#[test]
fn undo_closes_ancestor_typing_even_when_its_redo_child_is_pruned() {
    let mut doc = Document::new("");
    doc.edit(
        vec![Edit::insert(TextSize::new(0), "a")],
        &[],
        &[],
        EditGroup::Open,
        None,
    )
    .unwrap();
    let a = doc.current_history();
    replace(&mut doc, "b", &[], &[]);
    doc.undo();
    doc.set_undo_limit(1).unwrap();
    doc.edit(
        vec![Edit::insert(TextSize::new(1), "c")],
        &[],
        &[],
        EditGroup::Merge,
        None,
    )
    .unwrap();
    assert_ne!(doc.current_history(), a);
    assert_eq!(doc.text(), "ac");
    doc.undo();
    assert_eq!(doc.text(), "a");
}

#[test]
fn nested_multi_edit_steps_keep_utf8_coordinates_and_file_format() {
    let mut doc = Document::from_bytes("α one\r\nβ two\r\n".as_bytes()).unwrap();
    let before = carets(&[2, 9]);
    let scope = doc.begin_transaction(&before, Some("Two carets")).unwrap();
    doc.edit(
        vec![
            Edit::replace(TextRange::new(TextSize::new(3), TextSize::new(6)), "🙂"),
            Edit::replace(TextRange::new(TextSize::new(10), TextSize::new(13)), "é"),
        ],
        &before,
        &carets(&[7, 13]),
        EditGroup::Separate,
        None,
    )
    .unwrap();
    assert_eq!(doc.text(), "α 🙂\nβ é\n");
    let inner = doc.begin_transaction(&carets(&[7, 13]), None).unwrap();
    doc.edit(
        vec![Edit::insert(TextSize::new(8), "x\r\ny\r")],
        &[],
        &[],
        EditGroup::Separate,
        None,
    )
    .unwrap();
    assert_eq!(doc.text(), "α 🙂\nx\ny\nβ é\n");
    doc.rollback_transaction(inner).unwrap();
    assert_eq!(doc.text(), "α 🙂\nβ é\n");
    doc.set_line_ending(rustpy_text::LineEnding::Lf);
    assert_eq!(doc.rollback_transaction(scope).unwrap(), before);
    assert_eq!(doc.text(), "α one\nβ two\n");
    assert_eq!(doc.line_ending(), rustpy_text::LineEnding::Lf);
    assert!(doc.is_modified()); // Rolling back text does not undo a format change.
}

#[test]
fn default_limit_rebases_long_history_and_iterative_metadata_remains_usable() {
    let mut doc = Document::new("0");
    let expired = doc.current_history();
    for n in 1..=10_002 {
        replace(&mut doc, &n.to_string(), &[], &[]);
    }
    let history = doc.undo_history();
    assert_eq!(history.len(), 10_001);
    assert_eq!(history.last().unwrap().depth, 10_000);
    assert!(history.last().unwrap().is_current);
    assert_eq!(
        doc.select_history(expired),
        Err(HistoryError::UnknownHistory)
    );
    let root = history[0].id;
    let leaf = doc.current_history();
    doc.select_history(root).unwrap();
    assert_eq!(doc.text(), "2");
    assert!(!doc.can_undo());
    doc.select_history(leaf).unwrap();
    assert_eq!(doc.text(), "10002");
}
