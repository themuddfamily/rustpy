use rustpy_text::{
    Affinity::{After, Before},
    Document, Edit, EditError, EditGroup, HistoryError, MarkerError, MarkerId, MarkerPosition,
    PointDeletion::{Clamp, InvalidateCovered, InvalidateInterior},
    RangeDeletion, TextRange, TextSize,
};
fn at(n: u32) -> TextSize {
    TextSize::new(n)
}
fn range(s: u32, e: u32) -> TextRange {
    TextRange::new(at(s), at(e))
}
fn edit(doc: &mut Document, edits: Vec<Edit>) {
    doc.edit(edits, &[], &[], EditGroup::Separate, None)
        .unwrap();
}
fn point(doc: &Document, id: MarkerId) -> Option<u32> {
    doc.marker(id).unwrap().map(|p| match p {
        MarkerPosition::Point(p) => u32::from(p),
        _ => panic!("expected point"),
    })
}
fn marked_text(doc: &Document, id: MarkerId) -> Option<String> {
    doc.marker(id).unwrap().map(|p| match p {
        MarkerPosition::Range(r) => doc.slice(r),
        _ => panic!("expected range"),
    })
}

#[test]
fn byte_zero_fold_anchors_follow_import_and_docstring_prefixes_with_chosen_affinity() {
    let mut doc = Document::new("class C:\n    pass\n");
    let before = doc.point_marker(at(0), Before, InvalidateInterior).unwrap();
    let fold = doc.point_marker(at(0), After, InvalidateInterior).unwrap();
    let covered = doc.point_marker(at(0), After, InvalidateCovered).unwrap();
    let prefix = "\"\"\"module\"\"\"\nimport os\n";
    edit(&mut doc, vec![Edit::insert(at(0), prefix)]);
    assert_eq!(point(&doc, before), Some(0));
    assert_eq!(point(&doc, fold), Some(prefix.len() as u32));
    assert!(doc.text()[point(&doc, fold).unwrap() as usize..].starts_with("class C:"));
    doc.undo();
    assert_eq!(point(&doc, fold), Some(0));
    edit(&mut doc, vec![Edit::delete(range(0, 6))]);
    assert_eq!(point(&doc, fold), Some(0)); // Strict interior keeps a replacement boundary.
    assert_eq!(point(&doc, before), Some(0));
    assert_eq!(point(&doc, covered), None);
}

#[test]
fn all_same_slot_inserts_and_adjacent_replacement_boundaries_map_as_one_batch() {
    let mut doc = Document::new("target");
    let left = doc.point_marker(at(0), Before, Clamp).unwrap();
    let right = doc.point_marker(at(0), After, Clamp).unwrap();
    edit(
        &mut doc,
        vec![
            Edit::insert(at(0), "α"),
            Edit::insert(at(0), "🙂"),
            Edit::insert(at(0), "\n"),
        ],
    );
    assert_eq!(doc.text(), "α🙂\ntarget");
    assert_eq!(point(&doc, left), Some(0));
    assert_eq!(point(&doc, right), Some(7));
    doc.undo();
    assert_eq!(point(&doc, right), Some(0));
    doc.redo();
    assert_eq!(point(&doc, right), Some(7));
    let left_end = doc.point_marker(at(7), Before, Clamp).unwrap();
    let right_end = doc.point_marker(at(7), After, Clamp).unwrap();
    edit(
        &mut doc,
        vec![
            Edit::replace(range(0, 7), "x"),
            Edit::insert(at(7), "A"),
            Edit::insert(at(7), "B"),
        ],
    );
    assert_eq!(doc.text(), "xABtarget");
    assert_eq!(point(&doc, left_end), Some(1));
    assert_eq!(point(&doc, right_end), Some(3));
}

#[test]
fn multiedit_unicode_and_crlf_use_normalized_bytes_and_original_step_coordinates() {
    let mut doc = Document::new("α one\nβ target\n");
    let target = doc
        .range_marker(
            range(10, 16),
            After,
            Before,
            RangeDeletion::InvalidateOverlap,
        )
        .unwrap();
    edit(
        &mut doc,
        vec![
            Edit::insert(at(0), "🙂\r\n"),
            Edit::replace(range(3, 6), "é"),
        ],
    );
    assert_eq!(doc.text(), "🙂\nα é\nβ target\n");
    assert_eq!(marked_text(&doc, target).as_deref(), Some("target"));
    assert_eq!(
        doc.marker(target).unwrap(),
        Some(MarkerPosition::Range(range(14, 20)))
    );
    doc.undo();
    assert_eq!(
        doc.marker(target).unwrap(),
        Some(MarkerPosition::Range(range(10, 16)))
    );
    doc.redo();
    assert_eq!(marked_text(&doc, target).as_deref(), Some("target"));
}

#[test]
fn range_affinities_include_or_exclude_boundary_insertions_and_crossing_collapses() {
    let mut doc = Document::new("word");
    let exclude = doc
        .range_marker(range(0, 4), After, Before, RangeDeletion::Clamp)
        .unwrap();
    let include = doc
        .range_marker(range(0, 4), Before, After, RangeDeletion::Clamp)
        .unwrap();
    edit(
        &mut doc,
        vec![Edit::insert(at(0), "["), Edit::insert(at(4), "]")],
    );
    assert_eq!(marked_text(&doc, exclude).as_deref(), Some("word"));
    assert_eq!(marked_text(&doc, include).as_deref(), Some("[word]"));
    let empty = doc
        .range_marker(range(1, 1), After, Before, RangeDeletion::Clamp)
        .unwrap();
    edit(&mut doc, vec![Edit::insert(at(1), "prefix")]);
    assert_eq!(
        doc.marker(empty).unwrap(),
        Some(MarkerPosition::Range(range(7, 7)))
    );
}

#[test]
fn containing_deletion_clamps_or_invalidates_without_ordinary_undo_resurrection() {
    let mut doc = Document::new("prefix target suffix");
    let clamp_left = doc.point_marker(at(9), Before, Clamp).unwrap();
    let clamp_right = doc.point_marker(at(9), After, Clamp).unwrap();
    let deleted = doc.point_marker(at(7), After, InvalidateCovered).unwrap();
    let interior = doc.point_marker(at(9), Before, InvalidateInterior).unwrap();
    let highlight = doc
        .range_marker(
            range(7, 13),
            After,
            Before,
            RangeDeletion::InvalidateOverlap,
        )
        .unwrap();
    edit(&mut doc, vec![Edit::replace(range(7, 13), "é")]);
    assert_eq!(doc.text(), "prefix é suffix");
    assert_eq!(point(&doc, clamp_left), Some(7));
    assert_eq!(point(&doc, clamp_right), Some(9));
    assert_eq!(point(&doc, deleted), None);
    assert_eq!(point(&doc, interior), None);
    assert_eq!(marked_text(&doc, highlight), None);
    doc.undo();
    assert_eq!(doc.text(), "prefix target suffix");
    assert_eq!(point(&doc, deleted), None);
    assert_eq!(marked_text(&doc, highlight), None);
    assert_eq!(point(&doc, clamp_right), Some(13));
}

#[test]
fn inactive_equal_version_documents_and_net_zero_line_counts_do_not_share_marker_state() {
    let mut first = Document::new("one\ntarget\nlast\n");
    let mut second = Document::new(&first.text());
    assert_eq!(first.version(), second.version());
    let first_marker = first.point_marker(at(4), After, InvalidateCovered).unwrap();
    let second_marker = second
        .point_marker(at(4), After, InvalidateCovered)
        .unwrap();
    let cursor = first.journal_cursor();
    edit(&mut first, vec![Edit::insert(at(0), "new\n")]);
    edit(&mut first, vec![Edit::delete(range(15, 20))]);
    assert_eq!(first.text(), "new\none\ntarget\n");
    assert_eq!(first.text().lines().count(), second.text().lines().count());
    assert_eq!(point(&first, first_marker), Some(8));
    assert_eq!(point(&second, second_marker), Some(4));
    assert_eq!(
        first.marker(second_marker),
        Err(MarkerError::ForeignDocument)
    );
    assert_eq!(
        first.edits_since(second.journal_cursor()),
        Err(MarkerError::ForeignDocument)
    );
    assert_eq!(first.edits_since(cursor).unwrap().len(), 2);
}

#[test]
fn journal_replays_chronological_batches_including_history_branch_navigation() {
    let original = "α target\n";
    let mut doc = Document::new(original);
    let cursor = doc.journal_cursor();
    let marker = doc
        .range_marker(range(3, 9), After, Before, RangeDeletion::InvalidateOverlap)
        .unwrap();
    edit(
        &mut doc,
        vec![Edit::insert(at(0), "x\r\n"), Edit::insert(at(9), "!")],
    );
    let left = doc.current_history();
    doc.undo();
    edit(&mut doc, vec![Edit::insert(at(0), "right\n")]);
    let right = doc.current_history();
    doc.select_history(left).unwrap();
    assert_eq!(marked_text(&doc, marker).as_deref(), Some("target"));
    doc.select_history(right).unwrap();
    assert_eq!(marked_text(&doc, marker).as_deref(), Some("target"));
    let mut replay = Document::new(original);
    for batch in doc.edits_since(cursor).unwrap() {
        edit(&mut replay, batch.to_vec());
    }
    assert_eq!(replay.text(), doc.text());
    assert_eq!(
        doc.marker(marker).unwrap(),
        Some(MarkerPosition::Range(range(9, 15)))
    );
}

#[test]
fn eager_markers_outlive_bounded_journal_retention() {
    let mut doc = Document::new("target");
    let cursor = doc.journal_cursor();
    let marker = doc.point_marker(at(0), After, Clamp).unwrap();
    for _ in 0..80 {
        edit(&mut doc, vec![Edit::insert(at(0), "x\n")]);
    }
    assert_eq!(doc.edits_since(cursor), Err(MarkerError::JournalExpired));
    assert_eq!(point(&doc, marker), Some(160));
    assert_eq!(&doc.text()[160..], "target");
}

#[test]
fn rollback_restores_invalidated_removed_and_created_markers_even_in_empty_scopes() {
    let mut doc = Document::new("target");
    let marker = doc.point_marker(at(0), After, InvalidateCovered).unwrap();
    let scope = doc.begin_transaction(&[], None).unwrap();
    edit(&mut doc, vec![Edit::delete(range(0, 6))]);
    assert_eq!(point(&doc, marker), None);
    doc.remove_marker(marker).unwrap();
    let created = doc.point_marker(at(0), Before, Clamp).unwrap();
    doc.set_read_only(true);
    doc.rollback_transaction(scope).unwrap();
    assert_eq!(doc.text(), "target");
    assert_eq!(point(&doc, marker), Some(0));
    assert_eq!(doc.marker(created), Err(MarkerError::UnknownMarker));
    doc.set_read_only(false);
    let new = doc.point_marker(at(0), Before, Clamp).unwrap();
    assert_ne!(new, created);
    let empty = doc.begin_transaction(&[], None).unwrap();
    doc.remove_marker(marker).unwrap();
    let empty_created = doc.point_marker(at(6), Before, Clamp).unwrap();
    doc.rollback_transaction(empty).unwrap();
    assert_eq!(point(&doc, marker), Some(0));
    assert_eq!(doc.marker(empty_created), Err(MarkerError::UnknownMarker));
    let empty = doc.begin_transaction(&[], None).unwrap();
    doc.remove_marker(marker).unwrap();
    let kept = doc.point_marker(at(6), Before, Clamp).unwrap();
    doc.commit_transaction(empty, &[]).unwrap();
    assert_eq!(doc.marker(marker), Err(MarkerError::UnknownMarker));
    assert_eq!(point(&doc, kept), Some(6));
}

#[test]
fn nested_commit_and_rollback_restore_their_own_metadata_snapshots_with_safe_tokens() {
    let mut doc = Document::new("target");
    let original = doc.point_marker(at(0), After, InvalidateCovered).unwrap();
    let outer = doc.begin_transaction(&[], None).unwrap();
    edit(&mut doc, vec![Edit::insert(at(0), "prefix ")]);
    let outer_created = doc.point_marker(at(7), After, InvalidateCovered).unwrap();
    let inner = doc.begin_transaction(&[], None).unwrap();
    edit(&mut doc, vec![Edit::delete(range(7, 13))]);
    assert_eq!(point(&doc, original), None);
    assert_eq!(
        doc.rollback_transaction(outer),
        Err(HistoryError::TransactionOrder)
    );
    assert_eq!(point(&doc, original), None);
    doc.rollback_transaction(inner).unwrap();
    assert_eq!(point(&doc, original), Some(7));
    assert_eq!(point(&doc, outer_created), Some(7));
    let inner = doc.begin_transaction(&[], None).unwrap();
    doc.remove_marker(original).unwrap();
    let inner_created = doc.point_marker(at(7), Before, Clamp).unwrap();
    doc.commit_transaction(inner, &[]).unwrap();
    assert_eq!(doc.marker(original), Err(MarkerError::UnknownMarker));
    doc.rollback_transaction(outer).unwrap();
    assert_eq!(point(&doc, original), Some(0));
    assert_eq!(doc.marker(outer_created), Err(MarkerError::UnknownMarker));
    assert_eq!(doc.marker(inner_created), Err(MarkerError::UnknownMarker));
    assert_eq!(
        doc.commit_transaction(outer, &[]),
        Err(HistoryError::InvalidTransaction)
    );
    assert_eq!(point(&doc, original), Some(0));
}

#[test]
fn nested_outer_commit_keeps_membership_and_saved_rollback_recognition() {
    let mut doc = Document::new("A");
    let marker = doc.point_marker(at(0), After, InvalidateCovered).unwrap();
    let outer = doc.begin_transaction(&[], None).unwrap();
    let inner = doc.begin_transaction(&[], None).unwrap();
    doc.remove_marker(marker).unwrap();
    let kept = doc.point_marker(at(1), Before, Clamp).unwrap();
    doc.commit_transaction(inner, &[]).unwrap();
    doc.commit_transaction(outer, &[]).unwrap();
    assert_eq!(doc.marker(marker), Err(MarkerError::UnknownMarker));
    assert_eq!(point(&doc, kept), Some(1));
    let outer = doc.begin_transaction(&[], None).unwrap();
    edit(&mut doc, vec![Edit::replace(range(0, 1), "B")]);
    let inner = doc.begin_transaction(&[], None).unwrap();
    edit(&mut doc, vec![Edit::replace(range(0, 1), "A")]);
    doc.mark_saved();
    doc.commit_transaction(inner, &[]).unwrap();
    doc.rollback_transaction(outer).unwrap();
    assert_eq!(doc.text(), "A");
    assert!(!doc.is_modified());
    assert_eq!(point(&doc, kept), Some(1));
}

#[test]
fn reset_reload_identity_guards_and_failed_reload_preserve_registered_metadata() {
    let mut doc = Document::new("α target");
    let identity = doc.identity();
    let marker = doc.point_marker(at(3), After, Clamp).unwrap();
    let cursor = doc.journal_cursor();
    let scope = doc.begin_transaction(&[], None).unwrap();
    assert!(doc.reload_from_bytes(b"# coding: utf-8\n\xff").is_err());
    assert_eq!(doc.identity(), identity);
    assert_eq!(point(&doc, marker), Some(3));
    doc.rollback_transaction(scope).unwrap();
    doc.reset("α target");
    assert_ne!(doc.identity(), identity);
    assert_eq!(doc.marker(marker), Err(MarkerError::ForeignDocument));
    assert_eq!(doc.edits_since(cursor), Err(MarkerError::ForeignDocument));
    let marker = doc.point_marker(at(3), Before, Clamp).unwrap();
    let cursor = doc.journal_cursor();
    let scope = doc.begin_transaction(&[], None).unwrap();
    doc.reload_from_bytes("α target".as_bytes()).unwrap();
    assert_eq!(doc.marker(marker), Err(MarkerError::ForeignDocument));
    assert_eq!(doc.edits_since(cursor), Err(MarkerError::ForeignDocument));
    assert_eq!(
        doc.rollback_transaction(scope),
        Err(HistoryError::InvalidTransaction)
    );
}

#[test]
fn validation_noops_readonly_and_identical_accepted_replacement_preserve_markers() {
    let mut doc = Document::new("α target");
    assert_eq!(
        doc.point_marker(at(1), Before, Clamp),
        Err(MarkerError::NotCharBoundary(at(1)))
    );
    assert_eq!(
        doc.range_marker(range(0, 20), Before, After, RangeDeletion::Clamp),
        Err(MarkerError::OutOfBounds(at(20)))
    );
    let marker = doc.point_marker(at(4), After, InvalidateInterior).unwrap();
    let highlight = doc
        .range_marker(range(3, 9), After, Before, RangeDeletion::InvalidateOverlap)
        .unwrap();
    let cursor = doc.journal_cursor();
    edit(&mut doc, vec![]);
    edit(&mut doc, vec![Edit::insert(at(0), "")]);
    assert_eq!(doc.journal_cursor(), cursor);
    edit(&mut doc, vec![Edit::replace(range(3, 9), "target")]);
    assert_eq!(doc.version(), cursor.version() + 1);
    assert_eq!(point(&doc, marker), Some(4));
    assert_eq!(marked_text(&doc, highlight).as_deref(), Some("target"));
    assert_eq!(doc.edits_since(cursor).unwrap().len(), 1);
    doc.undo();
    doc.redo();
    assert_eq!(point(&doc, marker), Some(4));
    doc.set_read_only(true);
    assert!(doc.point_marker(at(0), Before, Clamp).is_ok());
    let version = doc.version();
    assert_eq!(
        doc.edit(
            vec![Edit::delete(range(0, 9))],
            &[],
            &[],
            EditGroup::Separate,
            None
        ),
        Err(EditError::ReadOnly)
    );
    assert_eq!(doc.version(), version);
    assert_eq!(point(&doc, marker), Some(4));
    doc.set_read_only(false);
    assert!(doc
        .edit(
            vec![Edit::delete(range(0, 6)), Edit::delete(range(3, 9))],
            &[],
            &[],
            EditGroup::Separate,
            None
        )
        .is_err());
    assert_eq!(point(&doc, marker), Some(4));
    assert_eq!(doc.version(), version);
}

#[test]
fn reloading_an_edited_source_expires_both_incarnations_and_preserves_loaded_dirty_identity() {
    let mut destination = Document::new("prefix source!");
    let destination_checkpoint = destination.content_checkpoint();
    let destination_marker = destination.point_marker(at(0), After, Clamp).unwrap();
    let destination_cursor = destination.journal_cursor();
    let destination_scope = destination.begin_transaction(&[], None).unwrap();
    let destination_history = destination.current_history();
    let version = destination.version();
    destination.set_read_only(true);

    let mut loaded = Document::new("source");
    let loaded_marker = loaded.point_marker(at(0), After, Clamp).unwrap();
    edit(&mut loaded, vec![Edit::insert(at(0), "prefix ")]);
    let loaded_scope = loaded.begin_transaction(&[], None).unwrap();
    edit(&mut loaded, vec![Edit::insert(at(13), "!")]);
    let loaded_cursor = loaded.journal_cursor();
    let loaded_history = loaded.current_history();
    let actual = loaded.text();
    let loaded_checkpoint = loaded.content_checkpoint();
    assert!(loaded.is_modified());
    destination.reload_from(loaded);
    assert_eq!(destination.text(), actual);
    assert!(destination.is_modified());
    assert_eq!(destination.version(), version + 1);
    assert!(destination.is_read_only());
    assert_eq!(destination.transaction_depth(), 0);
    assert!(!destination.can_undo());
    for marker in [loaded_marker, destination_marker] {
        assert_eq!(
            destination.marker(marker),
            Err(MarkerError::ForeignDocument)
        );
    }
    for cursor in [loaded_cursor, destination_cursor] {
        assert_eq!(
            destination.edits_since(cursor),
            Err(MarkerError::ForeignDocument)
        );
    }
    for scope in [loaded_scope, destination_scope] {
        assert_eq!(
            destination.rollback_transaction(scope),
            Err(HistoryError::InvalidTransaction)
        );
    }
    assert!(!destination.recognize_restored_content(&loaded_checkpoint));
    assert!(!destination.recognize_restored_content(&destination_checkpoint));
    destination.set_read_only(false);
    for history in [loaded_history, destination_history] {
        assert_eq!(
            destination.select_history(history),
            Err(HistoryError::UnknownHistory)
        );
    }
    let root = destination.current_history();
    destination.select_history(root).unwrap();
    assert!(destination.is_modified()); // A nonzero loaded identity must remain the new root.
    let cursor = destination.journal_cursor();
    let marker = destination.point_marker(at(0), After, Clamp).unwrap();
    edit(&mut destination, vec![Edit::insert(at(0), "new ")]);
    assert_eq!(point(&destination, marker), Some(4));
    destination.undo();
    assert_eq!(destination.text(), actual);
    assert!(destination.is_modified());
    assert_eq!(point(&destination, marker), Some(0));
    assert_eq!(destination.edits_since(cursor).unwrap().len(), 2);
    destination.mark_saved();
    edit(&mut destination, vec![Edit::insert(at(0), "another ")]);
    assert!(destination.is_modified());
    destination.undo();
    assert!(!destination.is_modified());
}

#[test]
fn mixed_identical_replacements_and_changed_unicode_insertion_only_map_changed_bytes() {
    let mut doc = Document::new("α target\n");
    let point_id = doc.point_marker(at(4), After, InvalidateInterior).unwrap();
    let target = doc
        .range_marker(range(3, 9), After, Before, RangeDeletion::InvalidateOverlap)
        .unwrap();
    let newline = doc
        .range_marker(
            range(9, 10),
            After,
            Before,
            RangeDeletion::InvalidateOverlap,
        )
        .unwrap();
    let cursor = doc.journal_cursor();
    edit(
        &mut doc,
        vec![
            Edit::replace(range(3, 9), "target"),
            Edit::insert(at(0), "🙂\r\n"),
            Edit::replace(range(9, 10), "\r\n"),
        ],
    );
    assert_eq!(doc.text(), "🙂\nα target\n");
    assert_eq!(point(&doc, point_id), Some(9));
    assert_eq!(
        doc.marker(target).unwrap(),
        Some(MarkerPosition::Range(range(8, 14)))
    );
    assert_eq!(marked_text(&doc, newline).as_deref(), Some("\n"));
    let changes = doc.edits_since(cursor).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].len(), 3); // Accepted identical steps remain in the journal.
    doc.undo();
    assert_eq!(point(&doc, point_id), Some(4));
    assert_eq!(marked_text(&doc, target).as_deref(), Some("target"));
    doc.redo();
    assert_eq!(point(&doc, point_id), Some(9));
    assert_eq!(marked_text(&doc, target).as_deref(), Some("target"));
}

#[test]
fn foreign_scope_tokens_cannot_pop_or_replace_document_marker_snapshots() {
    let mut doc = Document::new("target");
    let marker = doc.point_marker(at(0), After, InvalidateCovered).unwrap();
    let scope = doc.begin_transaction(&[], None).unwrap();
    edit(&mut doc, vec![Edit::delete(range(0, 6))]);
    let mut other = Document::new("other");
    let foreign = other.begin_transaction(&[], None).unwrap();
    assert_eq!(
        doc.rollback_transaction(foreign),
        Err(HistoryError::InvalidTransaction)
    );
    assert_eq!(
        doc.commit_transaction(foreign, &[]),
        Err(HistoryError::InvalidTransaction)
    );
    assert_eq!(point(&doc, marker), None);
    assert_eq!(doc.transaction_depth(), 1);
    assert_eq!(doc.text(), "");
    doc.rollback_transaction(scope).unwrap();
    assert_eq!(point(&doc, marker), Some(0));
    assert_eq!(doc.text(), "target");
    other.rollback_transaction(foreign).unwrap();
}
