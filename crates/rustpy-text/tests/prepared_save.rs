use rustpy_text::{
    Affinity, Document, Edit, Encoding, LineEnding, MarkerPosition, PointDeletion,
    PreparedSaveError, Selection, SourcePolicy, TextRange, TextSize,
};
fn at(n: u32) -> TextSize {
    TextSize::new(n)
}
fn save(doc: &mut Document, encoding: Encoding, ending: LineEnding) {
    let plan = doc
        .prepare_save(vec![], encoding, ending, SourcePolicy::Generic)
        .unwrap();
    doc.commit_prepared_save(plan, &[], &[]).unwrap();
}

#[test]
fn ordered_stages_and_full_format_are_one_reversible_unit() {
    let mut doc = Document::new("é body\n");
    let marker = doc
        .point_marker(at(3), Affinity::After, PointDeletion::InvalidateCovered)
        .unwrap();
    let plan = doc
        .prepare_save(
            vec![
                vec![Edit::insert(at(0), "head\r\n")],
                vec![Edit::replace(TextRange::new(at(5), at(7)), "E")],
            ],
            Encoding::Utf8Bom,
            LineEnding::CrLf,
            SourcePolicy::Generic,
        )
        .unwrap();
    assert_eq!(doc.text(), "é body\n");
    assert_eq!(plan.text(), "head\nE body\n");
    assert_eq!(plan.stages()[0][0].text, "head\n");
    assert_eq!(plan.bytes(), b"\xef\xbb\xbfhead\r\nE body\r\n");
    let before = [Selection::caret(at(3))];
    let after = [Selection::caret(at(7))];
    doc.commit_prepared_save(plan, &before, &after).unwrap();
    assert_eq!(doc.undo_label().as_deref(), Some("Save Actions"));
    assert_eq!(
        doc.marker(marker).unwrap(),
        Some(MarkerPosition::Point(at(7)))
    );
    doc.mark_saved();
    assert_eq!(doc.undo(), Some(before.to_vec()));
    assert_eq!(doc.text(), "é body\n");
    assert_eq!(doc.encoding(), Encoding::Utf8);
    assert_eq!(doc.line_ending(), LineEnding::Lf);
    assert!(doc.is_modified());
    assert_eq!(
        doc.marker(marker).unwrap(),
        Some(MarkerPosition::Point(at(3)))
    );
    assert_eq!(doc.redo(), Some(after.to_vec()));
    assert!(!doc.is_modified());
    assert_eq!(doc.to_bytes().unwrap(), b"\xef\xbb\xbfhead\r\nE body\r\n");
}

#[test]
fn late_stage_and_encoding_failures_leave_everything_unchanged() {
    let doc = Document::new("😀\n");
    let history = doc.current_history();
    assert!(matches!(
        doc.prepare_save(
            vec![
                vec![Edit::insert(at(0), "x")],
                vec![Edit::delete(TextRange::new(at(0), at(99)))]
            ],
            Encoding::Utf8,
            LineEnding::Lf,
            SourcePolicy::Generic
        ),
        Err(PreparedSaveError::Edit(_))
    ));
    assert!(matches!(
        doc.prepare_save(
            vec![],
            Encoding::Latin1,
            LineEnding::CrLf,
            SourcePolicy::Generic
        ),
        Err(PreparedSaveError::Encoding(_))
    ));
    assert_eq!(doc.text(), "😀\n");
    assert_eq!(doc.version(), 0);
    assert_eq!(doc.current_history(), history);
    assert!(!doc.is_modified());
    let plan = doc
        .prepare_save(
            vec![vec![Edit::insert(at(0), "x")]],
            Encoding::Utf8,
            LineEnding::Lf,
            SourcePolicy::Generic,
        )
        .unwrap();
    drop(plan); // Failed disk write never publishes.
    assert_eq!(doc.version(), 0);
    assert_eq!(doc.text(), "😀\n");
}

#[test]
fn net_zero_stages_preserve_marker_identity_version_and_history() {
    let mut doc = Document::new("abc\n");
    let marker = doc
        .point_marker(at(1), Affinity::After, PointDeletion::InvalidateCovered)
        .unwrap();
    let history = doc.current_history();
    let plan = doc
        .prepare_save(
            vec![
                vec![Edit::delete(TextRange::new(at(0), at(3)))],
                vec![Edit::insert(at(0), "abc")],
            ],
            Encoding::Utf8,
            LineEnding::Lf,
            SourcePolicy::Generic,
        )
        .unwrap();
    assert!(plan.stages().is_empty());
    doc.commit_prepared_save(plan, &[], &[]).unwrap();
    assert_eq!(doc.version(), 0);
    assert_eq!(doc.current_history(), history);
    assert_eq!(
        doc.marker(marker).unwrap(),
        Some(MarkerPosition::Point(at(1)))
    );
}

#[test]
fn format_only_restores_mixed_warning_and_carets_without_inventing_separators() {
    let mut doc = Document::new("a\r\nb\nc\r");
    assert!(doc.has_mixed_line_endings());
    let original = doc.text();
    let original_ending = doc.line_ending();
    let plan = doc
        .prepare_save(
            vec![],
            Encoding::Utf16LeNoBom,
            LineEnding::CrLf,
            SourcePolicy::Generic,
        )
        .unwrap();
    let bytes = plan.bytes().to_vec();
    let before = [Selection::caret(at(0))];
    let after = [Selection::caret(at(2))];
    doc.commit_prepared_save(plan, &before, &after).unwrap();
    doc.mark_saved();
    assert!(!doc.has_mixed_line_endings());
    assert!(!doc.is_modified());
    assert_eq!(doc.undo(), Some(before.to_vec()));
    assert!(doc.has_mixed_line_endings());
    assert_eq!(doc.line_ending(), original_ending);
    assert_eq!(doc.text(), original);
    assert!(doc.is_modified());
    assert_eq!(doc.redo(), Some(after.to_vec()));
    assert_eq!(doc.to_bytes().unwrap(), bytes);
    assert!(!doc.is_modified());
}

#[test]
fn prewrite_guard_rejects_scope_format_revision_readonly_and_other_document() {
    let mut doc = Document::new("a\n");
    let plan = doc
        .prepare_save(
            vec![],
            Encoding::Utf8,
            LineEnding::Lf,
            SourcePolicy::Generic,
        )
        .unwrap();
    let token = doc.begin_transaction(&[], None).unwrap();
    assert!(matches!(
        doc.check_prepared_save(&plan),
        Err(PreparedSaveError::StalePlan)
    ));
    doc.rollback_transaction(token).unwrap();
    let plan = doc
        .prepare_save(
            vec![],
            Encoding::Utf8,
            LineEnding::Lf,
            SourcePolicy::Generic,
        )
        .unwrap();
    doc.set_line_ending(LineEnding::CrLf);
    assert!(matches!(
        doc.commit_prepared_save(plan, &[], &[]),
        Err(PreparedSaveError::StalePlan)
    ));
    let plan = doc
        .prepare_save(
            vec![],
            Encoding::Utf8,
            LineEnding::Lf,
            SourcePolicy::Generic,
        )
        .unwrap();
    assert!(matches!(
        Document::new("a\n").check_prepared_save(&plan),
        Err(PreparedSaveError::StalePlan)
    ));
    doc.set_read_only(true);
    assert!(matches!(
        doc.check_prepared_save(&plan),
        Err(PreparedSaveError::ReadOnly)
    ));
}

#[test]
fn save_inside_nested_scope_can_rollback_and_remains_dirty_against_written_file() {
    let mut doc = Document::new("a\n");
    let outer = doc.begin_transaction(&[], Some("outer")).unwrap();
    let inner = doc.begin_transaction(&[], Some("inner")).unwrap();
    let plan = doc
        .prepare_save(
            vec![vec![Edit::insert(at(0), "b")]],
            Encoding::Utf8Bom,
            LineEnding::CrLf,
            SourcePolicy::Generic,
        )
        .unwrap();
    let written = plan.bytes().to_vec();
    doc.commit_prepared_save(plan, &[], &[]).unwrap();
    doc.mark_saved();
    assert!(!doc.is_modified());
    doc.commit_transaction(inner, &[]).unwrap();
    doc.rollback_transaction(outer).unwrap();
    assert_eq!(doc.text(), "a\n");
    assert_eq!(doc.encoding(), Encoding::Utf8);
    assert_eq!(doc.line_ending(), LineEnding::Lf);
    assert!(doc.is_modified());
    assert_ne!(doc.to_bytes().unwrap(), written);
}

#[test]
fn full_format_history_branches_restore_saved_identity() {
    let mut doc = Document::new("é\n");
    save(&mut doc, Encoding::Utf8Bom, LineEnding::CrLf);
    let first = doc.current_history();
    doc.mark_saved();
    doc.undo();
    save(&mut doc, Encoding::Latin1, LineEnding::Cr);
    let second = doc.current_history();
    doc.select_history(first).unwrap();
    assert_eq!(doc.encoding(), Encoding::Utf8Bom);
    assert_eq!(doc.line_ending(), LineEnding::CrLf);
    assert!(!doc.is_modified());
    doc.select_history(second).unwrap();
    assert_eq!(doc.encoding(), Encoding::Latin1);
    assert_eq!(doc.line_ending(), LineEnding::Cr);
    assert!(doc.is_modified());
}

#[test]
fn utf8_interior_ranges_and_cr_insertions_are_normalized_before_publication() {
    let mut doc = Document::new("😀z\n");
    let plan = doc
        .prepare_save(
            vec![vec![Edit::replace(TextRange::new(at(1), at(3)), "é\r\n")]],
            Encoding::Utf8,
            LineEnding::Lf,
            SourcePolicy::Generic,
        )
        .unwrap();
    assert_eq!(plan.stages()[0][0].range, TextRange::empty(at(0)));
    assert_eq!(plan.stages()[0][0].text, "é\n");
    assert_eq!(plan.bytes(), "é\n😀z\n".as_bytes());
    doc.commit_prepared_save(plan, &[], &[]).unwrap();
    assert_eq!(doc.text(), "é\n😀z\n");
    doc.undo();
    assert_eq!(doc.text(), "😀z\n");
    assert!(matches!(
        doc.prepare_save(
            vec![vec![
                Edit::delete(TextRange::new(at(0), at(4))),
                Edit::delete(TextRange::new(at(3), at(5)))
            ]],
            Encoding::Utf8,
            LineEnding::Lf,
            SourcePolicy::Generic
        ),
        Err(PreparedSaveError::Edit(_))
    ));
    assert_eq!(doc.text(), "😀z\n");
}

#[test]
fn stale_publication_after_edit_reset_history_and_replaced_scope_is_atomic() {
    let mut doc = Document::new("a\n");
    let plan = doc
        .prepare_save(
            vec![],
            Encoding::Utf8Bom,
            LineEnding::CrLf,
            SourcePolicy::Generic,
        )
        .unwrap();
    doc.edit(
        vec![Edit::insert(at(0), "x")],
        &[],
        &[],
        rustpy_text::EditGroup::Separate,
        None,
    )
    .unwrap();
    assert!(matches!(
        doc.commit_prepared_save(plan, &[], &[]),
        Err(PreparedSaveError::StalePlan)
    ));
    assert_eq!(doc.text(), "xa\n");
    assert_eq!(doc.encoding(), Encoding::Utf8);
    let plan = doc
        .prepare_save(
            vec![],
            Encoding::Utf8Bom,
            LineEnding::CrLf,
            SourcePolicy::Generic,
        )
        .unwrap();
    doc.undo();
    assert!(matches!(
        doc.commit_prepared_save(plan, &[], &[]),
        Err(PreparedSaveError::StalePlan)
    ));
    let plan = doc
        .prepare_save(
            vec![],
            Encoding::Utf8Bom,
            LineEnding::CrLf,
            SourcePolicy::Generic,
        )
        .unwrap();
    doc.reset("a\n");
    assert!(matches!(
        doc.commit_prepared_save(plan, &[], &[]),
        Err(PreparedSaveError::StalePlan)
    ));
    let first = doc.begin_transaction(&[], None).unwrap();
    let plan = doc
        .prepare_save(
            vec![],
            Encoding::Utf8Bom,
            LineEnding::CrLf,
            SourcePolicy::Generic,
        )
        .unwrap();
    doc.commit_transaction(first, &[]).unwrap();
    let second = doc.begin_transaction(&[], None).unwrap();
    assert!(matches!(
        doc.commit_prepared_save(plan, &[], &[]),
        Err(PreparedSaveError::StalePlan)
    ));
    doc.rollback_transaction(second).unwrap();
}

#[test]
fn pruned_full_format_root_preserves_retained_undo_redo_and_saved_identity() {
    let mut doc = Document::new("a\n");
    doc.set_undo_limit(1).unwrap();
    save(&mut doc, Encoding::Utf8Bom, LineEnding::CrLf);
    let expired = doc.current_history();
    doc.mark_saved();
    save(&mut doc, Encoding::Latin1, LineEnding::Cr);
    save(&mut doc, Encoding::Utf16BeNoBom, LineEnding::Lf);
    assert!(doc.select_history(expired).is_err());
    doc.undo();
    assert_eq!(doc.encoding(), Encoding::Latin1);
    assert_eq!(doc.line_ending(), LineEnding::Cr);
    assert!(doc.is_modified());
    assert!(!doc.can_undo());
    doc.redo();
    assert_eq!(doc.encoding(), Encoding::Utf16BeNoBom);
    assert_eq!(doc.line_ending(), LineEnding::Lf);
    assert_eq!(doc.to_bytes().unwrap(), [0, b'a', 0, b'\n']);
    assert!(doc.is_modified());
}
