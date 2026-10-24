use rustpy_highlight::HighlightTag;
use rustpy_text::Document;
use rustpy_textmate::Registry;
use rustpy_ui::editor::{EditorState, Language};

#[test]
fn imported_json_and_plist_grammars_reach_editor_and_edit_invalidation() {
    let directory = tempfile::tempdir().unwrap();
    let json = directory.path().join("demo.tmLanguage.json");
    std::fs::write(&json,r##"{"scopeName":"source.demo","fileTypes":["demo"],"patterns":[{"match":"\\bif\\b","name":"keyword.control.demo"},{"begin":"\"","end":"\"","name":"string.quoted.demo"}]}"##).unwrap();
    let plist = directory.path().join("other.tmLanguage");
    std::fs::write(&plist,r#"<?xml version="1.0"?><plist version="1.0"><dict><key>scopeName</key><string>source.other</string><key>patterns</key><array><dict><key>match</key><string>yes</string><key>name</key><string>constant.language.other</string></dict></array></dict></plist>"#).unwrap();
    let mut registry = Registry::new();
    let loaded = registry.load_bundle(directory.path());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    assert_eq!(loaded.loaded.len(), 2);
    let mut editor = EditorState::new(
        Document::new("if \"open\nstill\"\n"),
        Some("module.demo".into()),
    );
    editor.set_textmate_grammar(Some((
        registry.grammar("source.demo").unwrap(),
        "Demo".into(),
    )));
    assert_eq!(editor.language_label(), "Demo");
    assert!(editor
        .highlight_spans()
        .iter()
        .any(|span| span.tag == HighlightTag::Keyword));
    assert!(editor
        .highlight_spans()
        .iter()
        .any(|span| span.tag == HighlightTag::String));
    editor.doc.set_text("ordinary\n", &[], &[]).unwrap();
    assert!(!editor
        .highlight_spans()
        .iter()
        .any(|span| span.tag == HighlightTag::Keyword));
    editor.set_textmate_grammar(None);
    assert_eq!(editor.language, Language::Other);
    assert!(editor.textmate_grammar().is_none());
    assert!(registry.remove("source.demo"));
    assert!(registry.grammar("source.demo").is_none());
}
