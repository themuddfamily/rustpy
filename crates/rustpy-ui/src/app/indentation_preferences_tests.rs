use super::*;
use crate::editor::indent_options::{file_edit_options, IndentSource};
use rustpy_text::{util::IndentStyle, Document};

fn fixture() -> (tempfile::TempDir, IdeApp, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(directory.path().join(".editorconfig"), "root = true\n[*]\nindent_style = space\nindent_size = 2\ntab_width = 4\nmax_line_length = 79\ntrim_trailing_whitespace = true\n").unwrap();
    std::fs::write(&path, "def f():\n    return 1\n").unwrap();
    let mut settings = Settings::default();
    settings.editor.detect_indents = false;
    settings.editor.indent_size = 6;
    settings.trust_project(directory.path());
    let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
    app.open_file(&path);
    (directory, app, path)
}

#[test]
fn editorconfig_toggle_controls_edit_and_actual_format_options_without_a_restart() {
    let (_directory, mut app, path) = fixture();
    let (options, source) = file_edit_options(&app.settings.editor, &app.tabs[0].state);
    assert_eq!(
        (options.indent_size, options.right_margin, source),
        (2, 79, IndentSource::EditorConfig)
    );
    let candidate = Document::new(&app.tabs[0].state.doc.text());
    let edits = app
        .prepare_format_edits(0, &candidate, &app.tabs[0].state.carets)
        .unwrap();
    let after = rustpy_refactor::refactor::try_apply_edits(
        &candidate.text(),
        &edits
            .iter()
            .map(|edit| (edit.range, edit.text.clone()))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(after.contains("\n  return 1\n"), "{after}");
    app.settings.editor.editorconfig_enabled = false;
    let (options, source) = file_edit_options(&app.settings.editor, &app.tabs[0].state);
    assert_eq!((options.indent_size, source), (6, IndentSource::Settings));
    assert!(app
        .settings
        .editor
        .editorconfig_for(&path)
        .trim_trailing_whitespace
        .is_none());
    let edits = app
        .prepare_format_edits(0, &candidate, &app.tabs[0].state.carets)
        .unwrap();
    let after = rustpy_refactor::refactor::try_apply_edits(
        &candidate.text(),
        &edits
            .iter()
            .map(|edit| (edit.range, edit.text.clone()))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(after.contains("\n      return 1\n"), "{after}");
    app.settings.editor.editorconfig_enabled = true;
    assert_eq!(app.edit_options_for(0).indent_size, 2);
}

#[test]
fn custom_file_indent_overrides_editorconfig_and_conversion_is_undoable() {
    let (_directory, mut app, _path) = fixture();
    app.tabs[0].state.indent_override = Some(IndentStyle::Spaces(3));
    assert_eq!(app.edit_options_for(0).indent_size, 3);
    app.tabs[0].state.indent_override = Some(IndentStyle::Tabs);
    let options = app.edit_options_for(0);
    assert!(options.use_tabs);
    assert_eq!(options.tab_size, 4);
    let original = app.tabs[0].state.doc.text();
    app.tabs[0].state.convert_indents(true, &options);
    assert!(app.tabs[0].state.doc.text().contains("\n\treturn 1\n"));
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), original);
    app.tabs[0].state.indent_override = None;
    assert_eq!(app.edit_options_for(0).indent_size, 2);
}

#[test]
fn old_settings_keep_editorconfig_enabled_and_switch_round_trips() {
    let old: rustpy_settings::EditorSettings = serde_json::from_str("{}").unwrap();
    assert!(old.editorconfig_enabled);
    let (_directory, mut app, _path) = fixture();
    app.settings.editor.editorconfig_enabled = false;
    let loaded: Settings =
        serde_json::from_str(&serde_json::to_string(&app.settings).unwrap()).unwrap();
    assert!(!loaded.editor.editorconfig_enabled);
}
