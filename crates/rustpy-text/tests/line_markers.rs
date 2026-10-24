//! Original line identity through real accepted Document edit sequences.
use rustpy_text::{
    Document, Edit, EditGroup, MarkerError, MarkerId, MarkerPosition, TextRange, TextSize,
};
fn ts(n: usize) -> TextSize {
    TextSize::new(n as u32)
}
fn doc(text: &str) -> Document {
    Document::from_bytes(text.as_bytes()).unwrap()
}
fn edit(d: &mut Document, start: usize, end: usize, text: &str) {
    d.edit(
        vec![Edit::replace(TextRange::new(ts(start), ts(end)), text)],
        &[],
        &[],
        EditGroup::Separate,
        None,
    )
    .unwrap();
}
fn point(d: &Document, id: MarkerId) -> Option<TextSize> {
    match d.marker(id).unwrap() {
        Some(MarkerPosition::Point(at)) => Some(at),
        None => None,
        _ => panic!("line marker must project a point"),
    }
}
fn line(d: &Document, id: MarkerId) -> Option<usize> {
    point(d, id).map(|at| d.line_of(at))
}

#[test]
fn joined_original_line_then_full_body_rename_survives_and_undo_restores_row() {
    let mut d = doc("left\nright\ntail\n");
    let mark = d.line_marker(1).unwrap();
    edit(&mut d, 4, 5, "");
    assert_eq!(line(&d, mark), Some(0));
    edit(&mut d, 4, 9, "π renamed");
    assert_eq!(point(&d, mark), Some(ts(4)));
    assert_eq!(line(&d, mark), Some(0));
    d.undo().unwrap();
    d.undo().unwrap();
    assert_eq!(point(&d, mark), Some(ts(5)));
    assert_eq!(line(&d, mark), Some(1));
    d.redo().unwrap();
    d.redo().unwrap();
    assert_eq!(point(&d, mark), Some(ts(4)));
}

#[test]
fn inactive_cumulative_joins_replacements_and_expired_journal_keep_owner() {
    let mut d = doc("a\nb\noriginal\ntail\n");
    let mark = d.line_marker(2).unwrap();
    edit(&mut d, 1, 2, "");
    edit(&mut d, 2, 3, "");
    edit(&mut d, 2, 10, "replacement");
    for _ in 0..70 {
        edit(&mut d, 0, 0, "# π\r\n");
    }
    for _ in 0..70 {
        edit(&mut d, 0, "# π\n".len(), "");
    }
    assert_eq!(point(&d, mark), Some(ts(2)));
    assert_eq!(line(&d, mark), Some(0));
    for _ in 0..143 {
        d.undo().unwrap();
    }
    assert_eq!(d.text(), "a\nb\noriginal\ntail\n");
    assert_eq!(point(&d, mark), Some(ts(4)));
}

#[test]
fn body_only_deletion_leaves_blank_owner_and_refilling_keeps_original_row() {
    let mut d = doc("head\nbody\ntail");
    let mark = d.line_marker(1).unwrap();
    edit(&mut d, 5, 9, "");
    assert_eq!(d.text(), "head\n\ntail");
    assert_eq!(line(&d, mark), Some(1));
    edit(&mut d, 5, 5, "新 body");
    assert_eq!(point(&d, mark), Some(ts(5)));
    d.undo().unwrap();
    d.undo().unwrap();
    assert_eq!(line(&d, mark), Some(1));
}

#[test]
fn actual_whole_middle_final_and_blank_line_deletions_retire_only_owner() {
    for (source, row, start, end) in [
        ("head\nbody\ntail", 1, 5, 10),
        ("head\nbody", 1, 4, 9),
        ("head\n\ntail", 1, 5, 6),
        ("head\n", 1, 4, 5),
        ("only", 0, 0, 4),
    ] {
        let mut d = doc(source);
        let mark = d.line_marker(row).unwrap();
        edit(&mut d, start, end, "");
        assert_eq!(point(&d, mark), None, "{source:?}");
        d.undo().unwrap();
        assert_eq!(
            point(&d, mark),
            None,
            "ordinary Undo cannot revive a deleted owner"
        );
    }
}

#[test]
fn original_joined_content_pure_deletion_retires_owner_not_neighbor() {
    let mut d = doc("left\nright\n");
    let left = d.line_marker(0).unwrap();
    let right = d.line_marker(1).unwrap();
    edit(&mut d, 4, 5, "");
    edit(&mut d, 4, 9, "");
    assert_eq!(point(&d, right), None);
    assert_eq!(line(&d, left), Some(0));
}

#[test]
fn simultaneous_pure_deletion_union_and_nonempty_replacement_are_distinct() {
    let mut d = doc("head\nαβ\ntail\n");
    let mark = d.line_marker(1).unwrap();
    d.edit(
        vec![
            Edit::delete(TextRange::new(ts(5), ts(7))),
            Edit::delete(TextRange::new(ts(7), ts(10))),
        ],
        &[],
        &[],
        EditGroup::Separate,
        None,
    )
    .unwrap();
    assert_eq!(point(&d, mark), None);
    let mut d = doc("head\nαβ\ntail\n");
    let mark = d.line_marker(1).unwrap();
    d.edit(
        vec![
            Edit::replace(TextRange::new(ts(5), ts(9)), "new"),
            Edit::delete(TextRange::new(ts(9), ts(10))),
        ],
        &[],
        &[],
        EditGroup::Separate,
        None,
    )
    .unwrap();
    assert_eq!(d.text(), "head\nnewtail\n");
    assert_eq!(point(&d, mark), Some(ts(5)));
}

#[test]
fn same_offset_unicode_prefixes_and_tail_typing_map_real_normalized_batch() {
    let mut d = doc("body\ntail\n");
    let mark = d.line_marker(0).unwrap();
    d.edit(
        vec![
            Edit::insert(ts(0), "π\r\n"),
            Edit::insert(ts(0), "新\n"),
            Edit::insert(ts(4), " appended"),
        ],
        &[],
        &[],
        EditGroup::Separate,
        None,
    )
    .unwrap();
    assert_eq!(d.text(), "π\n新\nbody appended\ntail\n");
    assert_eq!(point(&d, mark), Some(ts("π\n新\n".len())));
    assert_eq!(line(&d, mark), Some(2));
    let start = "π\n新\n".len();
    edit(&mut d, start, start + "body appended".len(), "replacement");
    assert_eq!(point(&d, mark), Some(ts(start)));
}

#[test]
fn end_newline_insertion_excludes_new_rows_from_original_owner() {
    let mut d = doc("body\ntail\n");
    let mark = d.line_marker(0).unwrap();
    edit(&mut d, 4, 4, " append\nnew");
    assert_eq!(line(&d, mark), Some(0));
    edit(&mut d, 0, "body append\n".len(), "");
    assert_eq!(point(&d, mark), None);
}

#[test]
fn nested_rollback_restores_invalidated_membership_and_created_ids_expire() {
    let mut d = doc("head\nbody\ntail");
    let mark = d.line_marker(1).unwrap();
    let outer = d.begin_transaction(&[], None).unwrap();
    let inner = d.begin_transaction(&[], None).unwrap();
    edit(&mut d, 5, 10, "");
    assert_eq!(point(&d, mark), None);
    d.commit_transaction(inner, &[]).unwrap();
    let created = d.line_marker(0).unwrap();
    d.rollback_transaction(outer).unwrap();
    assert_eq!(line(&d, mark), Some(1));
    assert_eq!(d.marker(created), Err(MarkerError::UnknownMarker));
    let version = d.version();
    let scope = d.begin_transaction(&[], None).unwrap();
    d.remove_marker(mark).unwrap();
    d.rollback_transaction(scope).unwrap();
    assert_eq!(d.version(), version);
    assert_eq!(line(&d, mark), Some(1));
}

#[test]
fn retained_branch_selection_returns_surviving_owner_to_original_row() {
    let mut d = doc("head\nbody\ntail");
    let mark = d.line_marker(1).unwrap();
    edit(&mut d, 4, 5, "");
    let joined = d.current_history();
    d.undo().unwrap();
    edit(&mut d, 0, 0, "π\n");
    let prefixed = d.current_history();
    assert_eq!(line(&d, mark), Some(2));
    d.select_history(joined).unwrap();
    assert_eq!(line(&d, mark), Some(0));
    d.select_history(prefixed).unwrap();
    assert_eq!(line(&d, mark), Some(2));
}

#[test]
fn readonly_noops_empty_document_and_reset_have_explicit_identity_rules() {
    let mut d = doc("");
    d.set_read_only(true);
    let mark = d.line_marker(0).unwrap();
    assert_eq!(point(&d, mark), Some(ts(0)));
    assert_eq!(d.line_marker(1), Err(MarkerError::LineOutOfBounds(1)));
    assert_eq!(d.version(), 0);
    d.set_read_only(false);
    edit(&mut d, 0, 0, "");
    assert_eq!(d.version(), 0);
    edit(&mut d, 0, 0, "π");
    assert_eq!(point(&d, mark), Some(ts(0)));
    edit(&mut d, 0, "π".len(), "π");
    assert_eq!(point(&d, mark), Some(ts(0)));
    let other = doc("π");
    assert_eq!(other.marker(mark), Err(MarkerError::ForeignDocument));
    d.reload_from(other);
    assert_eq!(d.marker(mark), Err(MarkerError::ForeignDocument));
}

#[test]
fn single_replacement_preserves_owner_but_separate_delete_insert_creates_new_line() {
    for (edits, survives) in [
        (
            vec![Edit::replace(TextRange::new(ts(5), ts(10)), "new\n")],
            true,
        ),
        (
            vec![
                Edit::delete(TextRange::new(ts(5), ts(10))),
                Edit::insert(ts(5), "new\n"),
            ],
            false,
        ),
    ] {
        let mut d = doc("head\nbody\ntail");
        let mark = d.line_marker(1).unwrap();
        d.edit(edits, &[], &[], EditGroup::Separate, None).unwrap();
        assert_eq!(d.text(), "head\nnew\ntail");
        assert_eq!(point(&d, mark), survives.then_some(ts(5)));
    }
}

#[test]
fn neighboring_replacement_does_not_hide_deleted_original_line_in_same_batch() {
    for source in ["head\nbody\ntail", "head\n\ntail"] {
        let mut d = doc(source);
        let mark = d.line_marker(1).unwrap();
        let removed_end = if source.contains("body") { 10 } else { 6 };
        d.edit(
            vec![
                Edit::replace(TextRange::new(ts(0), ts(5)), "newhead\n"),
                Edit::delete(TextRange::new(ts(5), ts(removed_end))),
            ],
            &[],
            &[],
            EditGroup::Separate,
            None,
        )
        .unwrap();
        assert_eq!(point(&d, mark), None);
    }
}

#[test]
fn multiline_replacement_keeps_first_new_row_and_deleting_that_row_retires_owner() {
    let mut d = doc("head\nbody\ntail");
    let mark = d.line_marker(1).unwrap();
    edit(&mut d, 5, 9, "first\nsecond");
    assert_eq!(line(&d, mark), Some(1));
    edit(&mut d, 5, 11, "");
    assert_eq!(d.text(), "head\nsecond\ntail");
    assert_eq!(point(&d, mark), None);
    d.undo().unwrap();
    assert_eq!(point(&d, mark), None);
}

#[test]
fn interior_newline_split_keeps_first_row_identity_through_undo_and_redo() {
    let mut d = doc("head\noriginalbody\ntail");
    let mark = d.line_marker(1).unwrap();
    edit(&mut d, 13, 13, "\n");
    assert_eq!(d.text(), "head\noriginal\nbody\ntail");
    assert_eq!(point(&d, mark), Some(ts(5)));
    d.undo().unwrap();
    assert_eq!(point(&d, mark), Some(ts(5)));
    d.redo().unwrap();
    assert_eq!(point(&d, mark), Some(ts(5)));
    edit(&mut d, 5, 14, "");
    assert_eq!(d.text(), "head\nbody\ntail");
    assert_eq!(point(&d, mark), None);
}
