use super::*;

fn fixture() -> (tempfile::TempDir, IdeApp, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("α a.txt");
    let b = dir.path().join("β b.txt");
    std::fs::write(&a, "α original\n").unwrap();
    std::fs::write(&b, "β original\n").unwrap();
    let mut app = IdeApp::new(rustpy_settings::Settings::default(), None);
    app.open_file(&a);
    app.open_file(&b);
    (dir, app, a, b)
}

#[test]
fn captured_owner_survives_switch_and_close_and_other_uses_dirty_buffer() {
    let (_dir, mut app, a, b) = fixture();
    let source = app.capture_comparison(Some(&a), false).unwrap();
    assert_eq!(source.path.as_deref(), Some(a.as_path()));
    assert_eq!(source.text, "α original\n");
    let i = app
        .tabs
        .iter()
        .position(|t| t.state.path.as_ref() == Some(&b))
        .unwrap();
    app.active = Some(i);
    assert!(app.tabs[i].state.apply_text_edits(
        vec![(TextRange::empty(TextSize::new(0)), "dirty ".into())],
        "Edit"
    ));
    assert_eq!(
        app.capture_comparison(Some(&b), false).unwrap().text,
        "dirty β original\n"
    );
    app.tabs.retain(|t| t.state.path.as_ref() != Some(&a));
    app.compare_captured_file(&source, &b).unwrap();
    let Dialog::Diff(diff) = &app.dialog else {
        panic!("expected diff")
    };
    assert_eq!(diff.right_title, source.title);
    assert!(diff.diff.changes > 0);
    assert_eq!(std::fs::read_to_string(b).unwrap(), "β original\n");
}

#[test]
fn clipboard_is_external_or_empty_never_old_ide_history_and_error_keeps_dialog() {
    let (_dir, mut app, _, _) = fixture();
    app.clipboard_history = vec!["unrelated old copy".into()];
    app.compare_clipboard_with(|| Ok("β original\n".into()))
        .unwrap();
    let Dialog::Diff(diff) = &app.dialog else {
        panic!("expected diff")
    };
    assert_eq!(diff.diff.changes, 0);
    app.compare_clipboard_with(|| Ok(String::new())).unwrap();
    let Dialog::Diff(diff) = &app.dialog else {
        panic!("expected diff")
    };
    assert!(diff.diff.changes > 0);
    let title = diff.title.clone();
    assert!(app
        .compare_clipboard_with(|| Err("clipboard unavailable".into()))
        .is_err());
    let Dialog::Diff(diff) = &app.dialog else {
        panic!("lost prior diff")
    };
    assert_eq!(diff.title, title);
}

#[test]
fn checked_decoder_handles_cookie_and_refuses_missing_invalid_and_oversized() {
    let (dir, mut app, a, _) = fixture();
    let source = app.capture_comparison(Some(&a), false).unwrap();
    let latin = dir.path().join("legacy.py");
    std::fs::write(&latin, b"# coding: latin-1\nname='caf\xe9'\n").unwrap();
    assert!(app
        .capture_comparison(Some(&latin), false)
        .unwrap()
        .text
        .contains("café"));
    app.compare_captured_file(&source, &latin).unwrap();
    let bad = dir.path().join("bad.txt");
    std::fs::write(&bad, [0xff]).unwrap();
    assert_eq!(app.capture_comparison(Some(&bad), false).unwrap().text, "ÿ");
    std::fs::write(&bad, [0xef, 0xbb, 0xbf, 0xff]).unwrap();
    assert!(app.compare_captured_file(&source, &bad).is_err());
    assert!(app
        .compare_captured_file(&source, &dir.path().join("missing"))
        .is_err());
    assert!(app.compare_captured_file(&source, dir.path()).is_err());
    std::fs::write(&bad, vec![b'x'; MAX_COMPARE_BYTES + 1]).unwrap();
    assert!(app.compare_captured_file(&source, &bad).is_err());
    assert!(matches!(app.dialog, Dialog::Diff(_)));
    assert_eq!(source.text, "α original\n");
}

#[test]
fn closed_operand_alias_uses_normalized_saved_encoding_like_editor_open() {
    let (dir, mut app, _, _) = fixture();
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let path = dir.path().join("encoded.txt");
    std::fs::write(&path, [b'p', 0, 0xe9, 0]).unwrap();
    app.publish_encoding_override(&path, rustpy_text::Encoding::Utf16LeNoBom);
    let alias = folder.join("../encoded.txt");
    let captured = app.capture_comparison(Some(&alias), false).unwrap();
    assert_eq!(captured.path.as_deref(), Some(path.as_path()));
    assert_eq!(captured.text, "pé");
    app.open_file(&alias);
    assert_eq!(
        app.tabs[app.active.unwrap()].state.doc.text(),
        captured.text
    );
    assert_eq!(std::fs::read(&path).unwrap(), [b'p', 0, 0xe9, 0]);
}

thread_local! {
    static CLIPBOARD: std::cell::RefCell<Option<Result<String, String>>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn clipboard_override() -> Option<Result<String, String>> {
    CLIPBOARD.with(|value| value.borrow().clone())
}
struct ClipboardOverride;
impl Drop for ClipboardOverride {
    fn drop(&mut self) {
        CLIPBOARD.with(|value| *value.borrow_mut() = None);
    }
}

#[test]
fn actual_action_and_shared_picker_confirmation_keep_captured_owner_on_error_and_switch() {
    let (_dir, mut app, a, b) = fixture();
    let ctx = egui::Context::default();
    app.handle_action(Action::CompareFiles, &ctx);
    let Dialog::CompareFilePick(state) = std::mem::replace(&mut app.dialog, Dialog::None) else {
        panic!("picker not dispatched")
    };
    assert_eq!(state.1.path.as_ref(), Some(&b));
    app.active = app
        .tabs
        .iter()
        .position(|tab| tab.state.path.as_ref() == Some(&a));
    app.confirm_comparison_picker(state, Path::new("/missing-comparison-fixture"));
    assert!(app
        .status
        .as_ref()
        .is_some_and(|(text, _)| text.contains("Cannot compare")));
    let Dialog::CompareFilePick(state) = std::mem::replace(&mut app.dialog, Dialog::None) else {
        panic!("picker lost after error")
    };
    assert_eq!(state.1.path.as_ref(), Some(&b));
    let captured_title = state.1.title.clone();
    app.confirm_comparison_picker(state, &a);
    let Dialog::Diff(diff) = &app.dialog else {
        panic!("comparison not published")
    };
    assert_eq!(diff.right_title, captured_title);
    assert_eq!(app.tabs[app.active.unwrap()].state.path.as_ref(), Some(&a));
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "α original\n");
}

#[test]
fn actual_clipboard_action_reads_current_authority_and_keeps_diff_on_unavailable() {
    let (_dir, mut app, _, _) = fixture();
    let ctx = egui::Context::default();
    let _reset = ClipboardOverride;
    app.clipboard_history = vec!["old unrelated clipboard".into()];
    CLIPBOARD.with(|value| *value.borrow_mut() = Some(Ok("β original\n".into())));
    app.handle_action(Action::CompareWithClipboard, &ctx);
    let Dialog::Diff(diff) = &app.dialog else {
        panic!("action not dispatched")
    };
    assert_eq!(diff.diff.changes, 0);
    CLIPBOARD.with(|value| *value.borrow_mut() = Some(Err("clipboard refused".into())));
    app.handle_action(Action::CompareWithClipboard, &ctx);
    let Dialog::Diff(diff) = &app.dialog else {
        panic!("prior dialog lost")
    };
    assert_eq!(diff.diff.changes, 0);
    assert!(app
        .status
        .as_ref()
        .is_some_and(|(text, _)| text == "clipboard refused"));
}
