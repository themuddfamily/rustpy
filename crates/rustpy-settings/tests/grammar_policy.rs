use rustpy_settings::InspectionSettings;

#[test]
fn legacy_inspection_settings_get_english_all_categories_without_losing_toggle() {
    let settings: InspectionSettings = serde_json::from_str(r#"{"grammar":false}"#).unwrap();
    assert!(!settings.grammar);
    assert_eq!(settings.grammar_language, "en-US");
    assert!(settings.grammar_disabled_categories.is_empty());
}

#[test]
fn explicit_policy_roundtrips_and_future_values_are_retained_for_visible_validation() {
    let settings = InspectionSettings {
        grammar_language: "en-GB".into(),
        grammar_disabled_categories: vec!["grammar".into(), "punctuation".into()],
        ..Default::default()
    };
    let encoded = serde_json::to_string(&settings).unwrap();
    assert_eq!(
        serde_json::from_str::<InspectionSettings>(&encoded).unwrap(),
        settings
    );
    let future: InspectionSettings = serde_json::from_str(
        r#"{"grammar_language":"fr-FR","grammar_disabled_categories":["future-category"]}"#,
    )
    .unwrap();
    assert_eq!(future.grammar_language, "fr-FR");
    assert_eq!(future.grammar_disabled_categories, ["future-category"]);
}
