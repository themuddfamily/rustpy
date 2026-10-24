use rustpy_plugins::{PluginManifest, PluginRegistry, TableEditorKind};
#[test]
fn installed_csv_editor_registration_tracks_enable_state_and_legacy_schema() {
    let legacy =
        PluginManifest::from_json(r#"{"id":"org.test.old","name":"Old","version":"1.0.0"}"#)
            .unwrap();
    assert!(legacy.contributes.table_editors.is_empty());
    let source = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("plugin.json"), r#"{"id":"org.test.csv","name":"CSV","version":"1.0.0","contributes":{"table_editors":[{"name":"CSV Table","extensions":[".csv"],"kind":"Csv"}]}}"#).unwrap();
    let mut registry = PluginRegistry::load(config.path());
    registry.install_from_dir(source.path()).unwrap();
    let c = registry.contributions();
    assert_eq!(c.table_editors.len(), 1);
    assert_eq!(c.table_editors[0].plugin_id, "org.test.csv");
    assert_eq!(c.table_editors[0].extensions, ["csv"]);
    assert_eq!(c.table_editors[0].kind, TableEditorKind::Csv);
    registry.disable("org.test.csv").unwrap();
    assert!(registry.contributions().table_editors.is_empty());
    registry.enable("org.test.csv").unwrap();
    assert_eq!(registry.contributions().table_editors.len(), 1);
    let reloaded = PluginRegistry::load(config.path());
    assert_eq!(reloaded.contributions().table_editors.len(), 1);
}
#[test]
fn invalid_editor_kind_and_extensions_fail_before_registration() {
    for declaration in [
        r#"{"name":"CSV","extensions":[],"kind":"Csv"}"#,
        r#"{"name":"CSV","extensions":["../csv"],"kind":"Csv"}"#,
        r#"{"name":"CSV","extensions":["csv"],"kind":"Executable"}"#,
    ] {
        let manifest = format!(
            r#"{{"id":"org.test.csv","name":"CSV","version":"1.0.0","contributes":{{"table_editors":[{declaration}]}}}}"#
        );
        assert!(PluginManifest::from_json(&manifest).is_err());
    }
}
