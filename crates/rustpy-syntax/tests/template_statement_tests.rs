use rustpy_syntax::{
    template::{self, Dialect},
    template_statement::{validate, Error, StatementKind},
};
fn valid(src: &str, dialect: Dialect) {
    let parsed = validate(src, dialect);
    assert!(parsed.kind.is_some(), "unrecognized {src}");
    assert!(
        parsed.diagnostics.is_empty(),
        "{src}: {:?}",
        parsed.diagnostics
    );
    for r in parsed.bindings.iter().chain(&parsed.expressions) {
        assert!(src.get(r.clone()).is_some());
    }
}
#[test]
fn for_unpacking_filters_modifiers_and_identifier_collisions() {
    for src in [
        "for π, β in rows if π > 0 recursive",
        "for (π, (β, γ)) in rows",
        "for x in recursive",
        "for x in xs + recursive",
        "for x in xs if recursive",
    ] {
        valid(src, Dialect::Jinja);
    }
    for src in ["for π,β in rows reversed", "for x in rows|default:'🙂'"] {
        valid(src, Dialect::Django);
    }
    for src in [
        "for x rows",
        "for x.y in rows",
        "for ,x in rows",
        "for x in",
    ] {
        assert!(
            !validate(src, Dialect::Jinja).diagnostics.is_empty(),
            "{src}"
        );
    }
}
#[test]
fn set_assignment_capture_and_namespace_targets() {
    for src in [
        "set π, β = [1,2]",
        "set (π, β) = rows",
        "set ns.π = '🙂'",
        "set result | replace(old='=', new='-')",
        "set π",
    ] {
        valid(src, Dialect::Jinja);
    }
    for src in [
        "set x[0] = 1",
        "set ns.x, ns.y = rows",
        "set x = π +",
        "set x.y | trim",
    ] {
        assert!(
            !validate(src, Dialect::Jinja).diagnostics.is_empty(),
            "{src}"
        );
    }
}
#[test]
fn macro_parameters_defaults_nested_values_and_errors() {
    for src in ["macro résumé(π, β='🙂', values={'x':[1,2]})", "macro m()"] {
        valid(src, Dialect::Jinja);
    }
    for src in [
        "macro m(a=1,b)",
        "macro m(a=1,)",
        "macro m(*args)",
        "macro m(a",
        "macro m(a=)",
        "macro m(a) junk",
    ] {
        assert!(
            !validate(src, Dialect::Jinja).diagnostics.is_empty(),
            "{src}"
        );
    }
}
#[test]
fn jinja_include_import_and_from_options() {
    for src in [
        "include ['π.html','fallback.html'] ignore missing with context",
        "include template without context",
        "import 'π.html' as tools with context",
        "from 'π.html' import a as π, b without context",
        "extends parent",
    ] {
        valid(src, Dialect::Jinja);
    }
    for src in [
        "include 'x' with context ignore missing",
        "import 'x'",
        "import 'x' as",
        "from 'x' import a,",
        "from 'x' import _private",
        "from 'x' a",
    ] {
        assert!(
            !validate(src, Dialect::Jinja).diagnostics.is_empty(),
            "{src}"
        );
    }
}
#[test]
fn django_context_load_url_and_include() {
    for src in [
        "with π=person.name β='🙂 value'",
        "with person.name as π",
        "with person.name as π and person.age as β",
        "load static i18n",
        "load lower upper from mylibrary",
        "url 'app:π' user.id as target",
        "url 'app:π' pk=user.id name='🙂 value'",
        "include 'π.html' with π=user.name only",
        "include template only",
        "extends 'π.html'",
    ] {
        valid(src, Dialect::Django);
    }
    for src in [
        "with",
        "with π=",
        "with a as",
        "load",
        "load a from b c",
        "url",
        "url 'π' pk=",
        "include 'x' with",
        "include 'x' unexpected",
    ] {
        assert!(
            !validate(src, Dialect::Django).diagnostics.is_empty(),
            "{src}"
        );
    }
}
#[test]
fn blocks_dialect_flags_and_unknown_extensions() {
    valid("block π scoped required", Dialect::Jinja);
    valid("block content-title", Dialect::Django);
    for src in ["block π required scoped", "block π scoped scoped", "block"] {
        assert!(!validate(src, Dialect::Jinja).diagnostics.is_empty());
    }
    assert!(!validate("block π scoped", Dialect::Django)
        .diagnostics
        .is_empty());
    for dialect in [Dialect::Jinja, Dialect::Django] {
        let parsed = validate("custom arbitrary = 'π' nonsense", dialect);
        assert_eq!(parsed.kind, None);
        assert!(parsed.diagnostics.is_empty());
    }
}
#[test]
fn integrated_known_errors_have_original_unicode_byte_ranges() {
    let src = "🙂{% set π = β + %}{% custom %}{% endcustom %}";
    let doc = template::parse(src, Dialect::Jinja);
    assert!(doc
        .diagnostics
        .iter()
        .any(|d| d.error == template::Error::InvalidStatement));
    for diagnostic in &doc.diagnostics {
        assert!(src.get(diagnostic.range.clone()).is_some());
        assert!(diagnostic.range.start >= 4);
    }
    assert!(!doc
        .diagnostics
        .iter()
        .any(|d| d.error == template::Error::UnexpectedEnd));
    assert!(template::parse(
        "{% for π in rows if π > 0 %}{{ π }}{% endfor %}",
        Dialect::Jinja
    )
    .diagnostics
    .is_empty());
}
#[test]
fn captured_ranges_do_not_confuse_quotes_with_unicode_or_nested_keywords() {
    valid("macro m(x='Ģ',y={'in':'π'})", Dialect::Jinja);
    let parsed = validate("include ['with context', 'ignore missing']", Dialect::Jinja);
    assert_eq!(parsed.kind, Some(StatementKind::Include));
    assert!(parsed.diagnostics.is_empty());
    let parsed = validate("set π = '🙂' +", Dialect::Jinja);
    assert!(parsed
        .diagnostics
        .iter()
        .any(|d| d.error == Error::InvalidExpression));
}

#[test]
fn macro_empty_signature_and_trailing_comma_match_upstream() {
    valid("macro m()", Dialect::Jinja);
    let parsed = validate("macro m(a=1,)", Dialect::Jinja);
    assert!(parsed
        .diagnostics
        .iter()
        .any(|d| d.error == Error::InvalidParameter && d.range == (12..12)));
}
#[test]
fn loop_filter_allows_conditional_but_iterable_does_not() {
    valid("for x in xs if x if flag else y", Dialect::Jinja);
    assert!(template::parse(
        "{% for x in xs if x if flag else y %}{{ x }}{% endfor %}",
        Dialect::Jinja
    )
    .diagnostics
    .is_empty());
    let parsed = validate("for x in a if b else c", Dialect::Jinja);
    assert!(parsed
        .diagnostics
        .iter()
        .any(|d| d.error == Error::InvalidExpression && d.range == (16..20)));
}
#[test]
fn django_positional_in_clause_keeps_in_named_binding() {
    for src in ["for in in rows", "for in,π in rows reversed"] {
        let parsed = validate(src, Dialect::Django);
        assert!(
            parsed.diagnostics.is_empty(),
            "{src}: {:?}",
            parsed.diagnostics
        );
        assert_eq!(&src[parsed.bindings[0].clone()], "in");
    }
    let parsed = validate("for π over rows", Dialect::Django);
    assert!(parsed
        .diagnostics
        .iter()
        .any(|d| d.error == Error::MissingClause && d.range == (7..11)));
}
