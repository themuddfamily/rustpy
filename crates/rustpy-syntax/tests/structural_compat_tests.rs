use rustpy_syntax::template::{self, Dialect, Error, Kind};
#[test]
fn django_verbatim_preserves_entire_opaque_suffix() {
    let source = "π{% verbatim name  extra %}{{ invalid + }}{% endverbatim name %}🙂{% endverbatim name  extra %}";
    let doc = template::parse(source, Dialect::Django);
    assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
    assert_eq!(doc.blocks.len(), 1);
    assert!(doc.blocks[0].end.is_some());
    let raw = doc
        .regions
        .iter()
        .find(|r| r.kind == Kind::RawText)
        .unwrap();
    assert_eq!(
        &source[raw.range.clone()],
        "{{ invalid + }}{% endverbatim name %}🙂"
    );
    for source in [
        "{% verbatim name  extra %}π{% endverbatim name extra %}",
        "{% verbatim name extra %}π{% endverbatim name %}",
    ] {
        assert!(template::parse(source, Dialect::Django)
            .diagnostics
            .iter()
            .any(|d| d.error == Error::UnclosedBlock));
    }
}
#[test]
fn optional_block_end_names_follow_each_dialect() {
    for dialect in [Dialect::Django, Dialect::Jinja] {
        for source in [
            "{% block π %}🙂{% endblock %}",
            "{% block π %}🙂{% endblock π %}",
        ] {
            assert!(
                template::parse(source, dialect).diagnostics.is_empty(),
                "{source}"
            );
        }
        for source in [
            "{% block π %}🙂{% endblock other %}",
            "{% block π %}🙂{% endblock π extra %}",
        ] {
            let doc = template::parse(source, dialect);
            assert!(!doc.diagnostics.is_empty(), "{source}");
            assert!(doc
                .diagnostics
                .iter()
                .all(|d| source.get(d.range.clone()).is_some()));
        }
    }
    let source = "{% block π %}🙂{% endblock   π %}";
    assert!(template::parse(source, Dialect::Django)
        .diagnostics
        .iter()
        .any(|d| d.error == Error::NamedEndMismatch));
    assert!(template::parse(source, Dialect::Jinja)
        .diagnostics
        .is_empty());
    assert!(
        template::parse("{% custom %}π{% endcustom %}", Dialect::Django)
            .diagnostics
            .is_empty()
    );
}
