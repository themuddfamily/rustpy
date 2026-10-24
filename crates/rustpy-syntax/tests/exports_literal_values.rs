use rustpy_syntax::{exports, parse};
#[test]
fn exact_python_literal_export_values_share_the_golden_decoder() {
    let source = r#"__all__ = ["\x61\u03c0", 'b' "🙂", r'\n', '''triple
line''', "\N{LATIN SMALL LETTER Z}"]"#;
    let value = exports::evaluate(&parse(source).syntax_node()).unwrap();
    assert!(value.exact);
    assert_eq!(value.names, ["aπ", "b🙂", "\\n", "triple\nline", "z"]);
}
#[test]
fn non_scalar_and_malformed_values_cannot_authorize_exports() {
    for source in [
        r#"__all__=["\ud800"]"#,
        r#"__all__=[b'bad']"#,
        r#"__all__=["\x0"]"#,
    ] {
        assert!(
            !exports::evaluate(&parse(source).syntax_node())
                .unwrap()
                .exact,
            "{source}"
        );
    }
}

#[test]
fn oversized_literal_remains_nonexact_under_evaluator_budget() {
    let source = format!("__all__=['{}']", "a".repeat(exports::MAX_TEXT + 1));
    assert!(
        !exports::evaluate(&parse(&source).syntax_node())
            .unwrap()
            .exact
    );
}
