use rustpy_settings::{EditorSettings, Settings, StripTrailing};

#[test]
fn legacy_editor_settings_default_to_caret_line_protection() {
    let editor: EditorSettings = serde_json::from_str(
        r#"{"strip_trailing_spaces_on_save":"All","ensure_newline_at_eof":false}"#,
    )
    .unwrap();
    assert!(editor.keep_trailing_spaces_on_caret_line);
    assert_eq!(editor.strip_trailing_spaces_on_save, StripTrailing::All);
    assert!(!editor.ensure_newline_at_eof);
    let settings: Settings = serde_json::from_str(r#"{"editor":{}}"#).unwrap();
    assert!(settings.editor.keep_trailing_spaces_on_caret_line);
    assert!(EditorSettings::default().keep_trailing_spaces_on_caret_line);
}

#[test]
fn caret_line_preference_roundtrips_independently_of_strip_and_newline() {
    for strip in [
        StripTrailing::None,
        StripTrailing::ModifiedLines,
        StripTrailing::All,
    ] {
        for keep in [false, true] {
            for newline in [false, true] {
                let mut settings = Settings::default();
                settings.editor.strip_trailing_spaces_on_save = strip;
                settings.editor.keep_trailing_spaces_on_caret_line = keep;
                settings.editor.ensure_newline_at_eof = newline;
                let encoded = serde_json::to_vec(&settings).unwrap();
                let loaded: Settings = serde_json::from_slice(&encoded).unwrap();
                assert_eq!(loaded.editor.keep_trailing_spaces_on_caret_line, keep);
                assert_eq!(loaded.editor.strip_trailing_spaces_on_save, strip);
                assert_eq!(loaded.editor.ensure_newline_at_eof, newline);
            }
        }
    }
}
