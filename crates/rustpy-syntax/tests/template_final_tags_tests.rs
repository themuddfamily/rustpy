use rustpy_syntax::{
    template::{self, Dialect, Error},
    template_final_tags::validate,
};
#[test]
fn django_convenience_tag_arguments_match_standard_libraries() {
    for s in [
        "lorem",
        "lorem 3 w random",
        "lorem count|default:2 p",
        "lorem random",
        "language 'fr'",
        "localtime",
        "localtime off",
        "timezone 'Europe/Paris'",
        "get_current_timezone as zone",
        "get_available_languages as languages",
        "get_current_language as lang",
        "get_current_language_bidi as rtl",
        "get_language_info for 'fr' as info",
        "get_language_info_list for languages as infos",
        "endlanguage ignored",
        "endlocaltime",
        "endtimezone",
    ] {
        let v = validate(s, Dialect::Django);
        assert!(
            v.recognized && v.diagnostics.is_empty(),
            "{s}: {:?}",
            v.diagnostics
        );
    }
    for s in [
        "lorem 2 w random extra",
        "lorem 1 2",
        "language",
        "language 'fr' 'de'",
        "localtime yes",
        "localtime on off",
        "timezone",
        "get_current_timezone zone",
        "get_current_timezone as \"x y\"",
        "get_available_languages as \"x y\"",
        "get_current_language as \"x y\"",
        "get_current_language_bidi as \"x y\"",
        "get_current_language as",
        "get_language_info 'fr' as info",
        "get_language_info for 'fr' into info",
    ] {
        let v = validate(s, Dialect::Django);
        assert!(v.recognized && !v.diagnostics.is_empty(), "{s}");
        for d in v.diagnostics {
            assert!(s.get(d.range).is_some());
        }
    }
}
#[test]
fn standard_library_blocks_are_nested_and_have_matching_ends() {
    let source = "π{% language 'fr' %}{% localtime off %}{% timezone zone %}{{ π }}{% endtimezone %}{% endlocaltime %}{% endlanguage %}";
    let doc = template::parse(source, Dialect::Django);
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    assert_eq!(doc.blocks.len(), 3);
    assert!(doc.blocks.iter().all(|b| b.end.is_some()));
    for source in [
        "{% language 'fr' %}",
        "{% localtime %}{% endtimezone %}",
        "{% timezone zone %}{% endlanguage %}",
    ] {
        assert!(!template::parse(source, Dialect::Django)
            .diagnostics
            .is_empty());
    }
    assert!(
        template::parse("{% custom %}{% endcustom %}", Dialect::Django)
            .diagnostics
            .is_empty()
    );
    assert!(
        template::parse("{% language %}{% endlanguage %}", Dialect::Django)
            .diagnostics
            .iter()
            .any(|d| d.error == Error::InvalidBuiltin)
    );
}
#[test]
fn convenience_names_remain_opaque_in_jinja_and_utf8_ranges_are_exact() {
    assert!(!validate("timezone 'fr'", Dialect::Jinja).recognized);
    let source = "language π|default:";
    let v = validate(source, Dialect::Django);
    assert!(!v.diagnostics.is_empty());
    for d in v.diagnostics {
        assert!(source.get(d.range).is_some());
    }
}
