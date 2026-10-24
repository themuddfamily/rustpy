use rustpy_text::{
    Affinity, Document, Edit, EditGroup, Encoding, EncodingConversionError, MarkerPosition,
    PointDeletion, RangeDeletion, Selection, SourcePolicy, TextRange, TextSize,
};

fn at(byte: usize) -> TextSize {
    TextSize::new(byte as u32)
}
fn conversion(document: &mut Document, encoding: Encoding, policy: SourcePolicy) {
    let plan = document
        .prepare_encoding_conversion(encoding, policy)
        .unwrap();
    document.commit_encoding_conversion(plan, &[], &[]).unwrap();
}

#[test]
fn format_only_conversion_has_one_undo_unit_and_saved_identity_survives_branches() {
    let mut document = Document::new("é\n");
    let root = document.current_history();
    let plan = document
        .prepare_encoding_conversion(Encoding::Utf8Bom, SourcePolicy::Generic)
        .unwrap();
    assert!(plan.edits().is_empty());
    assert_eq!(plan.bytes(), &[0xef, 0xbb, 0xbf, 0xc3, 0xa9, b'\n']);
    let before = [Selection::caret(at(0))];
    let after = [Selection::caret(at(2))];
    document
        .commit_encoding_conversion(plan, &before, &after)
        .unwrap();
    let converted = document.current_history();
    assert_ne!(root, converted);
    assert!(document.is_modified());
    assert_eq!(document.text(), "é\n");
    document.mark_saved();
    assert!(!document.is_modified());
    assert_eq!(document.undo(), Some(before.to_vec()));
    assert_eq!(document.encoding(), Encoding::Utf8);
    assert!(document.is_modified());
    assert_eq!(document.redo(), Some(after.to_vec()));
    assert_eq!(document.encoding(), Encoding::Utf8Bom);
    assert!(!document.is_modified());
    document.undo();
    conversion(&mut document, Encoding::Latin1, SourcePolicy::Generic);
    let latin = document.current_history();
    assert_ne!(latin, converted);
    document.select_history(converted).unwrap();
    assert_eq!(document.encoding(), Encoding::Utf8Bom);
    assert!(!document.is_modified());
    document.select_history(latin).unwrap();
    assert_eq!(document.encoding(), Encoding::Latin1);
    assert!(document.is_modified());
}

#[test]
fn cookie_and_encoding_replay_atomically_without_destroying_unrelated_markers() {
    let original = "# coding: latin-1\nname = 'é'\n";
    let mut document = Document::new(original);
    document.set_encoding(Encoding::Latin1);
    document.mark_saved();
    let body = original.find("name").unwrap();
    let point = document
        .point_marker(at(body), Affinity::After, PointDeletion::InvalidateCovered)
        .unwrap();
    let range = document
        .range_marker(
            TextRange::new(at(body), at(body + 4)),
            Affinity::After,
            Affinity::Before,
            RangeDeletion::InvalidateOverlap,
        )
        .unwrap();
    let line = document.line_marker(1).unwrap();
    let plan = document
        .prepare_encoding_conversion(Encoding::Utf8, SourcePolicy::Python)
        .unwrap();
    assert_eq!(plan.edits().len(), 1);
    assert!(usize::from(plan.edits()[0].range.end()) < body);
    assert_eq!(plan.encoding(), Encoding::Utf8);
    let bytes = plan.bytes().to_vec();
    document.commit_encoding_conversion(plan, &[], &[]).unwrap();
    assert_eq!(
        document.to_bytes_with_policy(SourcePolicy::Python).unwrap(),
        bytes
    );
    assert_eq!(document.encoding(), Encoding::Utf8);
    let new_body = document.text().find("name").unwrap();
    assert_eq!(
        document.marker(point).unwrap(),
        Some(MarkerPosition::Point(at(new_body)))
    );
    let Some(MarkerPosition::Range(marked)) = document.marker(range).unwrap() else {
        panic!("range survives")
    };
    assert_eq!(document.slice(marked), "name");
    assert_eq!(
        document.marker(line).unwrap(),
        Some(MarkerPosition::Point(at(new_body)))
    );
    document.mark_saved();
    document.undo();
    assert_eq!(document.text(), original);
    assert_eq!(document.encoding(), Encoding::Latin1);
    assert_eq!(
        document.marker(point).unwrap(),
        Some(MarkerPosition::Point(at(body)))
    );
    assert!(document.is_modified());
    document.redo();
    assert_eq!(
        document.to_bytes_with_policy(SourcePolicy::Python).unwrap(),
        bytes
    );
    assert!(!document.is_modified());
}

#[test]
fn nested_conversion_steps_rollback_encoding_but_legacy_format_setters_keep_their_contract() {
    let mut document = Document::new("é\r\n");
    let point = document
        .point_marker(at(0), Affinity::After, PointDeletion::Clamp)
        .unwrap();
    let outer = document
        .begin_transaction(&[], Some("Encoding scope"))
        .unwrap();
    conversion(&mut document, Encoding::Utf8Bom, SourcePolicy::Generic);
    let inner = document.begin_transaction(&[], None).unwrap();
    conversion(&mut document, Encoding::Latin1, SourcePolicy::Generic);
    document.rollback_transaction(inner).unwrap();
    assert_eq!(document.encoding(), Encoding::Utf8Bom);
    document.set_line_ending(rustpy_text::LineEnding::Lf);
    document.rollback_transaction(outer).unwrap();
    assert_eq!(document.encoding(), Encoding::Utf8);
    assert_eq!(document.line_ending(), rustpy_text::LineEnding::Lf);
    assert!(document.is_modified());
    assert_eq!(
        document.marker(point).unwrap(),
        Some(MarkerPosition::Point(at(0)))
    );
    assert!(!document.can_undo());
    let outer = document.begin_transaction(&[], None).unwrap();
    conversion(&mut document, Encoding::Utf8Bom, SourcePolicy::Generic);
    let inner = document.begin_transaction(&[], None).unwrap();
    conversion(&mut document, Encoding::Latin1, SourcePolicy::Generic);
    document.commit_transaction(inner, &[]).unwrap();
    document.commit_transaction(outer, &[]).unwrap();
    document.undo();
    assert_eq!(document.encoding(), Encoding::Utf8);
    document.redo();
    assert_eq!(document.encoding(), Encoding::Latin1);
}

#[test]
fn failed_planning_and_unpublished_write_failures_leave_document_history_and_markers_unchanged() {
    let mut document = Document::new("😀\n");
    let marker = document
        .point_marker(at(4), Affinity::Before, PointDeletion::Clamp)
        .unwrap();
    let state = (
        document.version(),
        document.current_history(),
        document.text(),
        document.encoding(),
        document.is_modified(),
        document.marker(marker).unwrap(),
    );
    assert!(document
        .prepare_encoding_conversion(Encoding::Latin1, SourcePolicy::Generic)
        .is_err());
    assert!(document
        .prepare_encoding_conversion(Encoding::Utf16Le, SourcePolicy::Python)
        .is_err());
    let plan = document
        .prepare_encoding_conversion(Encoding::Utf8Bom, SourcePolicy::Generic)
        .unwrap();
    // The caller writes immutable bytes first. A failed writer simply drops
    // the plan: there is no compensating editor rollback that could lose state.
    let write: std::io::Result<()> = Err(std::io::Error::other("disk failure"));
    assert!(write.is_err());
    drop(plan);
    assert_eq!(
        (
            document.version(),
            document.current_history(),
            document.text(),
            document.encoding(),
            document.is_modified(),
            document.marker(marker).unwrap()
        ),
        state
    );
    document.set_read_only(true);
    assert!(matches!(
        document.prepare_encoding_conversion(Encoding::Utf8Bom, SourcePolicy::Generic),
        Err(EncodingConversionError::ReadOnly)
    ));
}

#[test]
fn stale_and_cross_document_plans_cannot_publish_or_record_history() {
    let mut document = Document::new("é\n");
    let plan = document
        .prepare_encoding_conversion(Encoding::Utf8Bom, SourcePolicy::Generic)
        .unwrap();
    document
        .edit(
            vec![Edit::insert(at(0), "x")],
            &[],
            &[],
            EditGroup::Separate,
            None,
        )
        .unwrap();
    let state = (
        document.version(),
        document.current_history(),
        document.text(),
        document.encoding(),
    );
    assert!(matches!(
        document.commit_encoding_conversion(plan, &[], &[]),
        Err(EncodingConversionError::StalePlan)
    ));
    assert_eq!(
        (
            document.version(),
            document.current_history(),
            document.text(),
            document.encoding()
        ),
        state
    );
    let plan = document
        .prepare_encoding_conversion(Encoding::Utf8Bom, SourcePolicy::Generic)
        .unwrap();
    document.set_line_ending(rustpy_text::LineEnding::CrLf);
    assert!(matches!(
        document.commit_encoding_conversion(plan, &[], &[]),
        Err(EncodingConversionError::StalePlan)
    ));
    let plan = document
        .prepare_encoding_conversion(Encoding::Utf8Bom, SourcePolicy::Generic)
        .unwrap();
    document.set_encoding(Encoding::Latin1);
    assert!(matches!(
        document.commit_encoding_conversion(plan, &[], &[]),
        Err(EncodingConversionError::StalePlan)
    ));
    let plan = document
        .prepare_encoding_conversion(Encoding::Utf8Bom, SourcePolicy::Generic)
        .unwrap();
    let mut other = Document::new(&document.text());
    assert!(matches!(
        other.commit_encoding_conversion(plan, &[], &[]),
        Err(EncodingConversionError::StalePlan)
    ));
    let plan = document
        .prepare_encoding_conversion(Encoding::Utf8Bom, SourcePolicy::Generic)
        .unwrap();
    document.set_read_only(true);
    assert!(matches!(
        document.commit_encoding_conversion(plan, &[], &[]),
        Err(EncodingConversionError::ReadOnly)
    ));
    document.set_read_only(false);
    let plan = document
        .prepare_encoding_conversion(Encoding::Utf8Bom, SourcePolicy::Generic)
        .unwrap();
    document.reset("replacement");
    assert!(matches!(
        document.commit_encoding_conversion(plan, &[], &[]),
        Err(EncodingConversionError::StalePlan)
    ));
}

#[test]
fn selected_bomless_utf16_reload_expires_history_and_handles_but_preserves_disk_format() {
    let mut document = Document::new("old\n");
    document
        .edit(
            vec![Edit::insert(at(0), "x")],
            &[],
            &[],
            EditGroup::Separate,
            None,
        )
        .unwrap();
    let marker = document
        .point_marker(at(0), Affinity::After, PointDeletion::Clamp)
        .unwrap();
    let old_identity = document.identity();
    let old_version = document.version();
    let bytes = [0xe9, 0x00, 0x0d, 0x00, 0x0a, 0x00];
    let loaded =
        Document::from_bytes_with_policy(&bytes, SourcePolicy::Generic, Some(Encoding::Utf16Le))
            .unwrap();
    assert_eq!(loaded.encoding(), Encoding::Utf16LeNoBom);
    assert_eq!(
        loaded.to_bytes_with_policy(SourcePolicy::Generic).unwrap(),
        bytes
    );
    document.reload_from(loaded);
    assert_eq!(document.text(), "é\n");
    assert_eq!(document.encoding(), Encoding::Utf16LeNoBom);
    assert_eq!(document.line_ending(), rustpy_text::LineEnding::CrLf);
    assert_eq!(
        document
            .to_bytes_with_policy(SourcePolicy::Generic)
            .unwrap(),
        bytes
    );
    assert!(document.version() > old_version);
    assert_ne!(document.identity(), old_identity);
    assert!(document.marker(marker).is_err());
    assert!(!document.can_undo());
    assert!(!document.is_modified());
    assert!(
        Document::from_bytes_with_policy(&[0xff], SourcePolicy::Generic, Some(Encoding::Utf8))
            .is_err()
    );
}

#[test]
fn identical_conversion_is_noop_and_format_only_conversion_keeps_marker_membership() {
    let mut document = Document::new("unchanged\n");
    let marker = document.line_marker(0).unwrap();
    let version = document.version();
    conversion(&mut document, Encoding::Utf8, SourcePolicy::Generic);
    assert_eq!(document.version(), version);
    assert!(!document.can_undo());
    conversion(&mut document, Encoding::Utf16BeNoBom, SourcePolicy::Generic);
    assert_eq!(
        document.marker(marker).unwrap(),
        Some(MarkerPosition::Point(at(0)))
    );
    document.undo();
    assert_eq!(
        document.marker(marker).unwrap(),
        Some(MarkerPosition::Point(at(0)))
    );
    assert!(!document.is_modified());
    document.redo();
    assert_eq!(document.encoding(), Encoding::Utf16BeNoBom);
}

#[test]
fn utf16_conversion_history_retains_endianness_and_bom_choice() {
    for (encoding, expected) in [
        (Encoding::Utf16Le, vec![0xff, 0xfe, 0xe9, 0, 10, 0]),
        (Encoding::Utf16Be, vec![0xfe, 0xff, 0, 0xe9, 0, 10]),
        (Encoding::Utf16LeNoBom, vec![0xe9, 0, 10, 0]),
        (Encoding::Utf16BeNoBom, vec![0, 0xe9, 0, 10]),
    ] {
        let mut document = Document::new("é\n");
        let plan = document
            .prepare_encoding_conversion(encoding, SourcePolicy::Generic)
            .unwrap();
        assert!(plan.edits().is_empty());
        assert_eq!(plan.bytes(), expected);
        document.commit_encoding_conversion(plan, &[], &[]).unwrap();
        document.mark_saved();
        assert_eq!(
            document
                .to_bytes_with_policy(SourcePolicy::Generic)
                .unwrap(),
            expected
        );
        document.undo();
        assert_eq!(document.encoding(), Encoding::Utf8);
        assert!(document.is_modified());
        document.redo();
        assert_eq!(document.encoding(), encoding);
        assert_eq!(
            document
                .to_bytes_with_policy(SourcePolicy::Generic)
                .unwrap(),
            expected
        );
        assert!(!document.is_modified());
    }
}

#[test]
fn pruned_encoding_history_rebases_root_and_preserves_saved_format_identity() {
    let mut document = Document::new("é\n");
    let original_root = document.current_history();
    conversion(&mut document, Encoding::Utf8Bom, SourcePolicy::Generic);
    conversion(&mut document, Encoding::Latin1, SourcePolicy::Generic);
    document.mark_saved();
    conversion(&mut document, Encoding::Utf16BeNoBom, SourcePolicy::Generic);
    document.set_undo_limit(1).unwrap();
    assert_eq!(
        document.select_history(original_root),
        Err(rustpy_text::HistoryError::UnknownHistory)
    );
    document.undo();
    assert_eq!(document.encoding(), Encoding::Latin1);
    assert_eq!(
        document
            .to_bytes_with_policy(SourcePolicy::Generic)
            .unwrap(),
        [0xe9, b'\n']
    );
    assert!(!document.is_modified());
    assert!(!document.can_undo());
    let retained_root = document.current_history();
    document.redo();
    assert_eq!(document.encoding(), Encoding::Utf16BeNoBom);
    assert_eq!(
        document
            .to_bytes_with_policy(SourcePolicy::Generic)
            .unwrap(),
        [0, 0xe9, 0, 10]
    );
    assert!(document.is_modified());
    document.select_history(retained_root).unwrap();
    assert_eq!(document.encoding(), Encoding::Latin1);
    assert!(!document.is_modified());
}

#[test]
fn mixed_endings_save_and_conversion_undo_keep_honest_normalized_metadata() {
    let mut document = Document::from_bytes(b"a\r\nb\n").unwrap();
    assert!(document.has_mixed_line_endings());
    let plan = document
        .prepare_encoding_conversion(Encoding::Utf8Bom, SourcePolicy::Generic)
        .unwrap();
    assert_eq!(plan.bytes(), b"\xef\xbb\xbfa\r\nb\r\n");
    assert!(
        document.has_mixed_line_endings(),
        "planning must not publish a save"
    );
    document.commit_encoding_conversion(plan, &[], &[]).unwrap();
    document.mark_saved();
    assert!(!document.has_mixed_line_endings());
    assert!(!document.is_modified());
    document.undo();
    assert_eq!(document.encoding(), Encoding::Utf8);
    assert!(
        !document.has_mixed_line_endings(),
        "undo does not invent lost raw separators"
    );
    assert_eq!(
        document
            .to_bytes_with_policy(SourcePolicy::Generic)
            .unwrap(),
        b"a\r\nb\r\n"
    );
    assert!(document.is_modified());
    document.redo();
    assert_eq!(
        document
            .to_bytes_with_policy(SourcePolicy::Generic)
            .unwrap(),
        b"\xef\xbb\xbfa\r\nb\r\n"
    );
    assert!(!document.is_modified());
}

#[test]
fn saving_an_explicit_conversion_inside_a_scope_then_rollback_leaves_buffer_dirty() {
    let mut document = Document::new("é\n");
    let scope = document.begin_transaction(&[], None).unwrap();
    let plan = document
        .prepare_encoding_conversion(Encoding::Utf8Bom, SourcePolicy::Generic)
        .unwrap();
    let written = plan.bytes().to_vec();
    document.commit_encoding_conversion(plan, &[], &[]).unwrap();
    document.mark_saved();
    assert!(!document.is_modified());
    document.rollback_transaction(scope).unwrap();
    assert_eq!(document.encoding(), Encoding::Utf8);
    assert_eq!(written, b"\xef\xbb\xbf\xc3\xa9\n");
    assert_eq!(
        document
            .to_bytes_with_policy(SourcePolicy::Generic)
            .unwrap(),
        b"\xc3\xa9\n"
    );
    assert!(
        document.is_modified(),
        "rollback differs from the already persisted representation"
    );
    assert!(!document.can_undo());
}
