use rustpy_syntax::{
    template::{self, Dialect},
    template_builtin::{validate, Error},
};
fn valid(s: &str, dialect: Dialect) {
    let v = validate(s, dialect);
    assert!(v.recognized, "{s}");
    assert!(v.diagnostics.is_empty(), "{s}: {:?}", v.diagnostics);
}
fn invalid(s: &str, dialect: Dialect) {
    let v = validate(s, dialect);
    assert!(v.recognized);
    assert!(!v.diagnostics.is_empty(), "{s}");
    for d in v.diagnostics {
        assert!(s.get(d.range).is_some());
    }
}
#[test]
fn jinja_call_signatures_filter_chains_with_and_autoescape() {
    for s in [
        "call render()",
        "call(π='🙂')render(1)",
        "filter upper|replace('A','π')",
        "with π=1, β={'x':[1,2]}",
        "with",
        "autoescape true",
        "do items.append(π)",
        "print π, β",
    ] {
        valid(s, Dialect::Jinja);
    }
    for s in [
        "call π",
        "call(a,) render()",
        "filter",
        "with π=",
        "autoescape",
        "endfor junk",
    ] {
        invalid(s, Dialect::Jinja);
    }
}
#[test]
fn jinja_trans_declared_variables_context_and_body() {
    for s in [
        "trans",
        "trans 'ctx'",
        "trans trimmed n=1",
        "trans n=items|length, trimmed",
        "trans n",
    ] {
        valid(s, Dialect::Jinja);
    }
    invalid("trans n=1 trimmed", Dialect::Jinja);
    invalid("trans n=1,n=2", Dialect::Jinja);
    for s in [
        "{% trans n=items|length %}{{ n }} π{% pluralize n %}β{% endtrans %}",
        "{% trans %}{# comment #}π{% endtrans %}",
    ] {
        assert!(
            template::parse(s, Dialect::Jinja).diagnostics.is_empty(),
            "{s}"
        );
    }
    for s in [
        "{% trans %}{{ x|upper }}{% endtrans %}",
        "{% trans %}{% if x %}x{% endif %}{% endtrans %}",
    ] {
        assert!(template::parse(s, Dialect::Jinja)
            .diagnostics
            .iter()
            .any(|d| d.error == template::Error::InvalidBuiltin));
    }
}
#[test]
fn django_cycle_regroup_ifchanged_and_filter() {
    for s in [
        "cycle 'π' '🙂' as colors silent",
        "cycle colors",
        "regroup people by country as countries",
        "ifchanged",
        "ifchanged user.name user.age",
        "filter force_escape|lower",
    ] {
        valid(s, Dialect::Django);
    }
    for s in [
        "cycle 'π' silent",
        "regroup people country as grouped",
        "filter",
        "ifchanged π +",
    ] {
        invalid(s, Dialect::Django);
    }
}
#[test]
fn django_translation_date_static_and_prefix_options() {
    for s in [
        "translate 'π' noop context 'menu' as label",
        "trans name",
        "blocktranslate with π=user.name count n=items|length trimmed context 'menu' asvar text",
        "now 'Y-m-d' as today",
        "static 'π.css' as path",
        "get_static_prefix",
        "get_media_prefix as media",
    ] {
        valid(s, Dialect::Django);
    }
    for s in [
        "translate",
        "translate 'π' context",
        "translate 'π' noop noop",
        "blocktranslate count x=1 y=2",
        "now",
        "static 'x' extra",
        "get_static_prefix unexpected",
    ] {
        invalid(s, Dialect::Django);
    }
    let s="{% blocktranslate count n=items|length %}{{ person.name }}π{% plural %}β{% endblocktranslate %}";
    assert!(template::parse(s, Dialect::Django).diagnostics.is_empty());
}
#[test]
fn ordinary_builtin_argument_grammar_and_unknown_extensions() {
    for s in [
        "firstof a b 'π' as chosen",
        "resetcycle",
        "widthratio used total 100 as width",
        "templatetag openvariable",
        "autoescape off",
        "csrf_token",
    ] {
        valid(s, Dialect::Django);
    }
    for s in [
        "firstof",
        "widthratio 1 2",
        "templatetag unknown",
        "autoescape true",
        "else π",
        "endfor π",
    ] {
        invalid(s, Dialect::Django);
    }
    assert!(!validate("my_plugin arbitrary ???", Dialect::Django).recognized);
    assert!(!validate("endmy_plugin", Dialect::Jinja).recognized);
}
#[test]
fn required_blocks_and_duplicate_names_are_static_checks() {
    assert!(template::parse(
        "{% block π required %}{# note #}\n {% endblock %}",
        Dialect::Jinja
    )
    .diagnostics
    .is_empty());
    for s in [
        "{% block π required %}hello{% endblock %}",
        "{% block π required %}{{ value }}{% endblock %}",
        "{% block π %}{% endblock %}{% block π %}{% endblock %}",
    ] {
        assert!(
            template::parse(s, Dialect::Jinja)
                .diagnostics
                .iter()
                .any(|d| d.error == template::Error::InvalidBuiltin),
            "{s}"
        );
    }
}
#[test]
fn django_extends_first_nontext_node_and_jinja_runtime_boundary() {
    assert!(
        template::parse("text{# comment #}{% extends 'base' %}", Dialect::Django)
            .diagnostics
            .is_empty()
    );
    let s = "{% load static %}{% extends 'base' %}";
    let doc = template::parse(s, Dialect::Django);
    assert!(doc
        .diagnostics
        .iter()
        .any(|d| d.error == template::Error::InvalidBuiltin));
    // Multiple Jinja extends nodes parse; runtime inheritance selection is not guessed.
    assert!(
        template::parse("{% extends 'a' %}{% extends 'b' %}", Dialect::Jinja)
            .diagnostics
            .is_empty()
    );
}
#[test]
fn synthesized_grammar_delegates_preserve_original_unicode_error_ranges() {
    let s = "call(π,) render()";
    let v = validate(s, Dialect::Jinja);
    assert!(v
        .diagnostics
        .iter()
        .any(|d| d.error == Error::InvalidBinding && d.range == (8..8)));
    let s = "🙂{% autoescape π + %}x{% endautoescape %}";
    let doc = template::parse(s, Dialect::Jinja);
    for d in &doc.diagnostics {
        assert!(s.get(d.range.clone()).is_some());
    }
    assert!(doc
        .diagnostics
        .iter()
        .any(|d| d.error == template::Error::InvalidBuiltin));
}

#[test]
fn adjacent_jinja_tag_tokens_and_translation_plural_authority() {
    assert!(
        template::parse("{% call(π)render() %}{{ π }}{% endcall %}", Dialect::Jinja)
            .diagnostics
            .is_empty()
    );
    assert!(
        template::parse("{% if(true) %}π{% endif %}", Dialect::Jinja)
            .diagnostics
            .is_empty()
    );
    for s in [
        "{% trans %}x{% pluralize %}y{% endtrans %}",
        "{% trans n=1 %}x{% pluralize missing %}y{% endtrans %}",
    ] {
        assert!(template::parse(s, Dialect::Jinja)
            .diagnostics
            .iter()
            .any(|d| d.error == template::Error::InvalidBuiltin));
    }
    assert!(template::parse(
        "{% blocktranslate %}x{% plural %}y{% endblocktranslate %}",
        Dialect::Django
    )
    .diagnostics
    .iter()
    .any(|d| d.error == template::Error::InvalidBuiltin));
}

#[test]
fn django_named_cycle_references_require_prior_declared_source_authority() {
    let src = "{% cycle 'π' '🙂' as colors %}{% cycle colors %}{% resetcycle colors %}";
    assert!(template::parse(src, Dialect::Django).diagnostics.is_empty());
    for src in [
        "{% cycle missing %}",
        "{% resetcycle %}",
        "{% cycle 'a' 'b' as colors %}{% resetcycle missing %}",
    ] {
        assert!(template::parse(src, Dialect::Django)
            .diagnostics
            .iter()
            .any(|d| d.error == template::Error::InvalidBuiltin));
    }
}

#[test]
fn reviewed_jinja_header_and_filter_contracts() {
    for s in [
        "with (a,b)=(1,2)",
        "filter replace('a', value|upper)|lower",
        "trans 'ctx' trimmed n=1",
        "trans trimmed=1",
        "trans n, trimmed",
    ] {
        valid(s, Dialect::Jinja);
    }
    for s in [
        "with ns.x=1",
        "filter upper[0]|lower",
        "filter upper is defined|lower",
        "trans n,n",
        "trans n,n=1",
        "trans 'ctx'|upper",
        "trans trimmed, n=1",
        "trans 'ctx' trimmed n,n",
    ] {
        invalid(s, Dialect::Jinja);
    }
}
#[test]
fn implicit_plural_counter_comes_from_singular_body_only() {
    let good = "{% trans %}{{ n }} item{% pluralize %}{{ n }} items{% endtrans %}";
    assert!(template::parse(good, Dialect::Jinja).diagnostics.is_empty());
    for s in [
        "{% trans %}{{ n }} item{% pluralize n %}items{% endtrans %}",
        "{% trans %}item{% pluralize %}{{ n }} items{% endtrans %}",
    ] {
        assert!(
            !template::parse(s, Dialect::Jinja).diagnostics.is_empty(),
            "{s}"
        );
    }
}
