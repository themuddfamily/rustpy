use rustpy_syntax::template::{highlight_constructs, parse, Dialect, Error, Kind};
fn partition(src: &str, dialect: Dialect) {
    let doc = parse(src, dialect);
    let mut end = 0;
    for region in &doc.regions {
        assert_eq!(region.range.start, end);
        assert!(src.get(region.range.clone()).is_some());
        assert!(src.get(region.content.clone()).is_some());
        end = region.range.end;
    }
    assert_eq!(end, src.len());
}
#[test]
fn nested_unicode_branches_and_named_end() {
    let src="π{% block résumé %}{% if π %}{{ {'π': '}}'} }}{% elif β %}β{% else %}🙂{% endif %}{% endblock résumé %}";
    let doc = parse(src, Dialect::Jinja);
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    assert_eq!(doc.blocks.len(), 2);
    assert_eq!(doc.blocks[1].parent, Some(0));
    assert_eq!(doc.blocks[1].branches.len(), 2);
    assert!(doc.blocks.iter().all(|b| b.end.is_some()));
    partition(src, Dialect::Jinja);
}
#[test]
fn raw_and_named_verbatim_ignore_fake_syntax() {
    for (dialect, src) in [
        (
            Dialect::Jinja,
            "{% raw -%}{{ 'unterminated {% if x %}{% endraw %}",
        ),
        (
            Dialect::Django,
            "{% verbatim π %}{% endverbatim %}{{ bad {% endverbatim π %}",
        ),
        (
            Dialect::Django,
            "{% comment %}{% if x %}{{bad{% endcomment %}",
        ),
    ] {
        let doc = parse(src, dialect);
        assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(doc.regions.len(), 3);
        assert_eq!(doc.regions[1].kind, Kind::RawText);
        partition(src, dialect);
    }
}
#[test]
fn quotes_controls_and_comments() {
    let src = "{# {{ ' #}{{- \"}}\" + 'π\\\'x' -}}{%+ include 'a%}b' +%}";
    let doc = parse(src, Dialect::Jinja);
    assert!(doc.diagnostics.is_empty());
    assert_eq!(doc.regions.len(), 3);
    assert_eq!(&src[doc.regions[1].content.clone()], "\"}}\" + 'π\\\'x'");
    partition(src, Dialect::Jinja);
}
#[test]
fn django_empty_and_jinja_capture_set() {
    assert!(parse(
        "{% for x in xs %}π{% empty %}🙂{% endfor %}",
        Dialect::Django
    )
    .diagnostics
    .is_empty());
    let doc = parse(
        "{% set x = '=' %}{% set y | replace('=', '-') %}π{% endset %}",
        Dialect::Jinja,
    );
    assert!(doc.diagnostics.is_empty());
    assert_eq!(doc.blocks.len(), 1);
    assert!(
        parse("{% for x in xs %}{% empty %}{% endfor %}", Dialect::Jinja)
            .diagnostics
            .iter()
            .any(|d| d.error == Error::InvalidBranch)
    );
}
#[test]
fn malformed_structure_recovery_preserves_source() {
    let src = "{% if x %}{% for x in xs %}{% endif %}{% else %}{% endfor %}{{ π";
    let doc = parse(src, Dialect::Jinja);
    for error in [
        Error::MismatchedEnd,
        Error::UnclosedBlock,
        Error::InvalidBranch,
        Error::UnexpectedEnd,
        Error::UnclosedDelimiter,
    ] {
        assert!(
            doc.diagnostics.iter().any(|d| d.error == error),
            "missing {error:?}"
        );
    }
    partition(src, Dialect::Jinja);
}
#[test]
fn final_branch_named_end_and_empty_diagnostics() {
    let doc = parse(
        "{% block π %}{% endblock β %}{% if x %}{% else %}{% elif y %}{% endif %}{{ }}{% %}",
        Dialect::Jinja,
    );
    assert!(doc
        .diagnostics
        .iter()
        .any(|d| d.error == Error::NamedEndMismatch));
    assert!(doc
        .diagnostics
        .iter()
        .any(|d| d.error == Error::InvalidBranch));
    assert_eq!(
        doc.diagnostics
            .iter()
            .filter(|d| d.error == Error::EmptyConstruct)
            .count(),
        2
    );
}
#[test]
fn unknown_extensions_are_opaque_not_invented_blocks() {
    let src = "{% custom π %}{{ π }}";
    let doc = parse(src, Dialect::Django);
    assert!(doc.blocks.is_empty());
    assert!(doc.diagnostics.is_empty());
    partition(src, Dialect::Django);
}
#[test]
fn compatibility_scanner_retains_old_highlighting_boundaries() {
    let src = "{{ '}}' }}{% raw %}{{ x }}{% endraw %}";
    assert_eq!(
        highlight_constructs(src),
        vec![(0, 6, b'{'), (10, 19, b'%'), (19, 26, b'{'), (26, 38, b'%')]
    );
    assert_eq!(highlight_constructs("π{{ x"), vec![(2, 6, b'{')]);
}
#[test]
fn large_flat_and_raw_inputs_are_iterative() {
    let src = format!("{{% raw %}}{}{{% endraw %}}", "{% fake ".repeat(20000));
    let doc = parse(&src, Dialect::Jinja);
    assert_eq!(doc.regions.len(), 3);
    assert!(doc.diagnostics.is_empty());
    let src = format!(
        "{}{}",
        "{% if π %}".repeat(10000),
        "{% endif %}".repeat(10000)
    );
    let doc = parse(&src, Dialect::Jinja);
    assert_eq!(doc.blocks.len(), 10000);
    assert!(doc.diagnostics.is_empty());
}

#[test]
fn paired_extensions_remain_opaque_but_known_orphan_ends_are_errors() {
    for dialect in [Dialect::Django, Dialect::Jinja] {
        let src = "{% custom π %}content{% endcustom %}";
        let doc = parse(src, dialect);
        assert!(doc.blocks.is_empty());
        assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
        partition(src, dialect);
        assert!(parse("{% endif %}", dialect)
            .diagnostics
            .iter()
            .any(|d| d.error == Error::UnexpectedEnd));
    }
}
#[test]
fn capture_filter_keyword_equals_do_not_become_assignment() {
    let src = "{% set result | replace(old='=', new='-') %}π{% endset %}";
    let doc = parse(src, Dialect::Jinja);
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    assert_eq!(doc.blocks.len(), 1);
    assert!(doc.blocks[0].end.is_some());
    partition(src, Dialect::Jinja);
    let assigned = parse(
        "{% set result = replace(old='=', new='-') %}",
        Dialect::Jinja,
    );
    assert!(assigned.blocks.is_empty());
    assert!(assigned.diagnostics.is_empty());
}
