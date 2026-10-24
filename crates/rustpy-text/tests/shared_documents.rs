use rustpy_text::{
    Affinity, Document, Edit, EditGroup, Encoding, LineEnding, MarkerPosition, PointDeletion,
    Selection, TextSize,
};
fn edit(doc: &mut Document, at: u32, text: &str) {
    doc.edit(
        vec![Edit::insert(TextSize::new(at), text)],
        &[Selection::caret(TextSize::new(at))],
        &[Selection::caret(TextSize::new(at + text.len() as u32))],
        EditGroup::Separate,
        Some("Shared edit"),
    )
    .unwrap();
}
#[test]
fn handles_share_actual_text_history_save_format_and_read_only_authority() {
    let mut first = Document::new("π\n");
    let mut second = first.share();
    assert!(first.shares_authority(&second));
    assert_eq!(first.identity(), second.identity());
    let before = first.content_checkpoint();
    edit(&mut first, 0, "🙂");
    assert_eq!(second.text(), "🙂π\n");
    assert_eq!(second.rope().to_string(), first.text());
    assert_eq!(second.version(), first.version());
    assert!(second.is_modified());
    assert_eq!(second.undo_label().as_deref(), Some("Shared edit"));
    assert!(second.undo().is_some());
    assert_eq!(first.text(), "π\n");
    assert!(!first.is_modified());
    assert!(first.redo().is_some());
    assert_eq!(second.text(), "🙂π\n");
    second.set_line_ending(LineEnding::CrLf);
    second.set_encoding(Encoding::Utf8Bom);
    assert_eq!(first.encoding(), Encoding::Utf8Bom);
    let encoded = first.to_bytes().unwrap();
    assert_eq!(&encoded[..3], &[0xef, 0xbb, 0xbf]);
    assert_eq!(&encoded[3..], "🙂π\r\n".as_bytes());
    first.mark_saved();
    assert!(!second.is_modified());
    second.set_read_only(true);
    assert!(first.is_read_only());
    assert!(first
        .edit(
            vec![Edit::insert(TextSize::new(0), "bad")],
            &[],
            &[],
            EditGroup::Separate,
            None
        )
        .is_err());
    assert_eq!(second.text(), "🙂π\n");
    assert!(!first.recognize_restored_content(&before));
}
#[test]
fn shared_markers_and_journal_follow_other_handle_edits_and_survive_handle_drop() {
    let mut first = Document::new("alpha\n");
    let mut second = first.share();
    let cursor = second.journal_cursor();
    let marker = second
        .point_marker(TextSize::new(2), Affinity::After, PointDeletion::Clamp)
        .unwrap();
    edit(&mut first, 0, "π");
    assert_eq!(
        second.marker(marker).unwrap(),
        Some(MarkerPosition::Point(TextSize::new(4)))
    );
    assert_eq!(second.edits_since(cursor).unwrap().len(), 1);
    drop(first);
    edit(&mut second, 0, "🙂");
    assert_eq!(second.text(), "🙂πalpha\n");
    assert!(second.undo().is_some());
    assert_eq!(second.text(), "πalpha\n");
}
#[test]
fn reload_updates_all_handles_and_expires_old_markers_without_independent_copy() {
    let mut first = Document::new("old\n");
    let second = first.share();
    let id = first.identity();
    let marker = first.line_marker(0).unwrap();
    first.reload_from(Document::new("β\r\n"));
    assert_eq!(second.text(), "β\n");
    assert_eq!(second.to_bytes().unwrap(), "β\r\n".as_bytes());
    assert_ne!(second.identity(), id);
    assert!(second.marker(marker).is_err());
    assert!(!second.can_undo());
}
