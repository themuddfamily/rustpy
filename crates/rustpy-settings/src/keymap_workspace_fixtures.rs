use crate::Settings;

#[test]
fn custom_profiles_keep_independent_extended_overrides_across_switch_and_reload() {
    let mut settings: Settings = serde_json::from_str(r#"{"keymap":"Default"}"#).unwrap();
    assert!(settings.custom_keymaps.is_empty());
    settings
        .custom_keymaps
        .push(("My profile".into(), "Default".into()));
    settings.custom_shortcuts = vec![(
        "SaveDocument".into(),
        vec!["Ctrl+K, Ctrl+S".into(), "Abbreviation:write".into()],
    )];
    settings.switch_keymap("My profile");
    settings.custom_shortcuts = vec![("Run".into(), vec!["Mouse:Alt+Middle".into()])];
    settings.keymap_quick_lists.push((
        "Editing".into(),
        vec!["SaveDocument".into(), "ReformatCode".into()],
        "Ctrl+Alt+Q".into(),
    ));
    let json = serde_json::to_string(&settings).unwrap();
    let mut restored: Settings = serde_json::from_str(&json).unwrap();
    restored.switch_keymap("Default");
    assert_eq!(
        restored.custom_shortcuts[0].1,
        vec!["Ctrl+K, Ctrl+S", "Abbreviation:write"]
    );
    restored.switch_keymap("My profile");
    assert_eq!(restored.custom_shortcuts[0].1, vec!["Mouse:Alt+Middle"]);
    assert_eq!(restored.keymap_quick_lists, settings.keymap_quick_lists);
    assert_eq!(restored.custom_keymaps, settings.custom_keymaps);
}
