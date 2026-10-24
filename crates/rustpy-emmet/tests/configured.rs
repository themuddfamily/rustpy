use rustpy_emmet::{configured_at, configured_before};
use std::collections::BTreeMap;
#[test]
fn configured_tab_preserves_context_and_filters_unicode_caret_offsets() {
    let snippets = BTreeMap::from([("card".into(), "div.card>p{π😀}".into())]);
    let (_, expansion) = configured_before("card", 4, "html", "  ", &snippets).unwrap();
    assert!(expansion.text.contains("π😀"));
    assert!(expansion.text.contains("class=\"card\""));
    let (_, escaped) = configured_before("p{π}|e", 7, "html", "  ", &BTreeMap::new()).unwrap();
    assert_eq!(escaped.text, "&lt;p&gt;π&lt;/p&gt;");
    assert!(escaped.text.is_char_boundary(escaped.caret));
    assert!(configured_before("<script>card", 12, "html", "  ", &snippets).is_none());
    assert!(configured_before("p|unknown", 9, "html", "  ", &snippets).is_none());
    let (_, single) = configured_before(
        "ul>li*2|s",
        "ul>li*2|s".len(),
        "html",
        "  ",
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(!single.text.contains('\n'));
}
#[test]
fn surround_raw_unicode_markup_is_not_abbreviation_syntax_and_is_bounded() {
    let source = "π <b>😀</b> & {literal}";
    let expansion = configured_at(
        source,
        0..source.len(),
        "section>p",
        "html",
        "  ",
        &BTreeMap::new(),
        Some(source),
    )
    .unwrap();
    assert!(expansion.text.contains(source));
    assert!(expansion.text.starts_with("<section>"));
    assert!(configured_at(
        source,
        0..source.len(),
        "img",
        "html",
        "  ",
        &BTreeMap::new(),
        Some(source)
    )
    .is_none());
    assert!(configured_at(
        source,
        0..source.len(),
        "div*10000",
        "html",
        "  ",
        &BTreeMap::new(),
        Some(&"x".repeat(1000))
    )
    .is_none());
    let cycle = BTreeMap::from([("a".into(), "b".into()), ("b".into(), "a".into())]);
    assert!(configured_at("", 0..0, "a", "html", "  ", &cycle, None).is_none());
}
