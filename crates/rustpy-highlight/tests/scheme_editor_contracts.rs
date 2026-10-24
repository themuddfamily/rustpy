use rustpy_highlight::{ColorScheme, HighlightTag as T, Rgb, TextStyle};
#[test]
fn scheme_inheritance_preserves_custom_effects_and_refuses_cycles() {
    let mut scheme = ColorScheme::dark();
    let mut style = TextStyle::fg(0x123456);
    style.wave = true;
    style.strikeout = true;
    style.border = true;
    style.effect_color = Some(Rgb::hex(0x654321));
    scheme.styles.insert(T::String, style);
    assert!(scheme.inherit_style(T::Bytes, T::String));
    assert_eq!(scheme.style(T::Bytes), style);
    assert!(!scheme.inherit_style(T::String, T::Bytes));
    scheme.reset_style(T::Bytes);
    assert_ne!(scheme.style(T::Bytes), style);
}
#[test]
fn named_presets_and_context_overrides_are_distinct_and_stable() {
    for name in [
        "Monokai",
        "Twilight",
        "Slate Dark",
        "WarmNeon",
        "Solarized Dark",
        "Solarized Light",
    ] {
        let mut scheme = ColorScheme::by_name(name).unwrap();
        assert_eq!(scheme.name, name);
        let fallback = scheme.editor.foreground;
        assert_eq!(scheme.context_color("console.stderr", fallback), fallback);
        scheme
            .context_colors
            .insert("console.stderr".into(), Rgb::hex(0x123456));
        assert_eq!(
            scheme.context_color("console.stderr", fallback),
            Rgb::hex(0x123456)
        );
    }
    assert_ne!(
        ColorScheme::by_name("Monokai").unwrap().editor.background,
        ColorScheme::dark().editor.background
    );
}
#[test]
fn legacy_scheme_names_load_as_aliases_of_the_new_names() {
    for (legacy, current) in [
        ("Darcula", "Dark"),
        ("IntelliJ Light", "Light"),
        ("Default", "Light"),
        ("Rider Dark", "Slate Dark"),
    ] {
        assert_eq!(ColorScheme::by_name(legacy).unwrap().name, current);
        assert!(!ColorScheme::builtin_names().contains(&legacy));
        assert!(ColorScheme::builtin_names().contains(&current));
    }
}
