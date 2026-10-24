use rustpy_syntax::{
    template::Dialect,
    template_expression::{parse, Context, Error, Kind},
};
fn valid(
    src: &str,
    dialect: Dialect,
    context: Context,
) -> rustpy_syntax::template_expression::Expression<'_> {
    let parsed = parse(src, dialect, context);
    assert!(
        parsed.diagnostics.is_empty(),
        "{src}: {:?}",
        parsed.diagnostics
    );
    assert!(parsed.root.is_some());
    for node in &parsed.nodes {
        assert!(src.get(node.range.clone()).is_some(), "{:?}", node.range);
    }
    parsed
}
#[test]
fn arithmetic_precedence_and_jinja_left_associative_power() {
    let ast = valid("1 + 2 * 3", Dialect::Jinja, Context::Output);
    let Kind::Binary {
        right, operator, ..
    } = &ast.nodes[ast.root.unwrap()].kind
    else {
        panic!()
    };
    assert_eq!(&ast.source[operator.clone()], "+");
    assert!(matches!(ast.nodes[*right].kind, Kind::Binary { .. }));
    let ast = valid("-2 ** 3 ** 2", Dialect::Jinja, Context::Output);
    let Kind::Binary { left, operator, .. } = &ast.nodes[ast.root.unwrap()].kind else {
        panic!()
    };
    assert_eq!(&ast.source[operator.clone()], "**");
    assert!(matches!(ast.nodes[*left].kind, Kind::Binary { .. }));
}
#[test]
fn lookup_slices_call_keywords_filters_tests_and_conditionals() {
    for src in [
        "foo.bar[1:4:2]|default('π')",
        "f(1, name='π', *args, **kwargs)",
        "x is divisibleby 3",
        "x is not defined",
        "x if flag else y",
        "x if flag",
        "a[::, 1]",
        "value|custom.filter(π=1)",
        "x is custom.test(2)",
    ] {
        valid(src, Dialect::Jinja, Context::Output);
    }
    let ast = valid("foo[1:4:2]", Dialect::Jinja, Context::Output);
    assert!(ast.nodes.iter().any(|n| matches!(
        n.kind,
        Kind::Slice {
            start: Some(_),
            stop: Some(_),
            step: Some(_)
        }
    )));
}
#[test]
fn collections_strings_numbers_and_unicode_ranges() {
    for src in [
        "{'π': [1, 2], 'x': (3,)}",
        "1, π,",
        "0xff + 0b10 + 0o7 + 1_000 + 1.25e-2",
        "'π' '🙂'",
        "résumé.π",
        "1 < π <= 4",
        "not (x in ys) or a ~ b",
    ] {
        valid(src, Dialect::Jinja, Context::Output);
    }
}
#[test]
fn malformed_utf8_ranges_and_argument_order() {
    for (src, error) in [
        ("π +", Error::ExpectedExpression),
        ("f(a=1, 2)", Error::InvalidArgumentOrder),
        ("f(a=1, a=2)", Error::DuplicateKeyword),
        ("a[1", Error::ExpectedDelimiter),
        ("'π", Error::UnterminatedString),
        ("0xZZ", Error::InvalidNumber),
        ("1e+", Error::InvalidNumber),
        ("π 🙂", Error::InvalidCharacter),
    ] {
        let ast = parse(src, Dialect::Jinja, Context::Output);
        assert!(
            ast.diagnostics.iter().any(|d| d.error == error),
            "{src}: {:?}",
            ast.diagnostics
        );
        for d in ast.diagnostics {
            assert!(src.get(d.range).is_some());
        }
    }
    let ast = parse("π +", Dialect::Jinja, Context::Output);
    assert!(ast.diagnostics.iter().any(|d| d.range == (4..4)));
}
#[test]
fn django_filter_and_condition_grammar_is_distinct() {
    for src in ["user.π|default:'🙂'", "items.0", "value|add:-2", "_('π')"] {
        valid(src, Dialect::Django, Context::Output);
    }
    for src in [
        "not a and b or c",
        "a not in items",
        "a is not None",
        "a == 3",
        "a|length >= 2",
    ] {
        valid(src, Dialect::Django, Context::Condition);
    }
    for src in [
        "value + 2",
        "f()",
        "a[0]",
        "[1,2]",
        "x if y else z",
        "_private",
        "a._secret",
        "not a",
        "value | lower",
        "value|default: 'π'",
    ] {
        assert!(
            !parse(src, Dialect::Django, Context::Output)
                .diagnostics
                .is_empty(),
            "{src}"
        );
    }
    assert!(!parse("(a or b)", Dialect::Django, Context::Condition)
        .diagnostics
        .is_empty());
}
#[test]
fn bounded_nesting_and_flat_comparison_arena() {
    let src = format!("{}π{}", "(".repeat(1000), ")".repeat(1000));
    assert!(parse(&src, Dialect::Jinja, Context::Output)
        .diagnostics
        .iter()
        .any(|d| d.error == Error::TooDeep));
    let src = vec!["π"; 10000].join(" < ");
    let ast = valid(&src, Dialect::Jinja, Context::Output);
    let Kind::Compare { rest, .. } = &ast.nodes[ast.root.unwrap()].kind else {
        panic!()
    };
    assert_eq!(rest.len(), 9999);
}
#[test]
fn template_integrates_precise_expression_errors_and_preserves_custom_tags() {
    let src = "π{{ value + }}{% custom %}{% endcustom %}";
    let doc = rustpy_syntax::template::parse(src, Dialect::Jinja);
    assert!(doc.diagnostics.iter().any(|d| d.error
        == rustpy_syntax::template::Error::InvalidExpression
        && d.range == (12..12)));
    assert!(!doc
        .diagnostics
        .iter()
        .any(|d| d.error == rustpy_syntax::template::Error::UnexpectedEnd));
    assert!(
        rustpy_syntax::template::parse("{{ value|default:'π' }}", Dialect::Django)
            .diagnostics
            .is_empty()
    );
}

#[test]
fn upstream_grouping_concat_unary_and_implicit_test_argument() {
    let ast = valid("a ~ b + c", Dialect::Jinja, Context::Output);
    let Kind::Binary { left, operator, .. } = &ast.nodes[ast.root.unwrap()].kind else {
        panic!()
    };
    assert_eq!(&ast.source[operator.clone()], "+");
    let Kind::Binary { operator, .. } = &ast.nodes[*left].kind else {
        panic!()
    };
    assert_eq!(&ast.source[operator.clone()], "~");
    let ast = valid("a + b ~ c", Dialect::Jinja, Context::Output);
    let Kind::Binary {
        right, operator, ..
    } = &ast.nodes[ast.root.unwrap()].kind
    else {
        panic!()
    };
    assert_eq!(&ast.source[operator.clone()], "+");
    let Kind::Binary { operator, .. } = &ast.nodes[*right].kind else {
        panic!()
    };
    assert_eq!(&ast.source[operator.clone()], "~");
    for src in ["-value|abs", "-value is number"] {
        let ast = valid(src, Dialect::Jinja, Context::Output);
        let value = match &ast.nodes[ast.root.unwrap()].kind {
            Kind::Filter { value, .. } | Kind::Test { value, .. } => *value,
            _ => panic!(),
        };
        assert!(matches!(ast.nodes[value].kind, Kind::Unary { .. }));
    }
    let ast = valid("x is divisibleby y|int", Dialect::Jinja, Context::Output);
    let Kind::Filter { value, .. } = &ast.nodes[ast.root.unwrap()].kind else {
        panic!()
    };
    let Kind::Test { arguments, .. } = &ast.nodes[*value].kind else {
        panic!()
    };
    let rustpy_syntax::template_expression::Argument::Positional(arg) = &arguments[0] else {
        panic!()
    };
    assert!(matches!(ast.nodes[*arg].kind, Kind::Name));
    assert!(
        !parse("x is defined is number", Dialect::Jinja, Context::Output)
            .diagnostics
            .is_empty()
    );
}
#[test]
fn keyword_after_star_and_conditional_context_match_upstream() {
    valid("f(*args, name=1)", Dialect::Jinja, Context::Output);
    for src in ["f(**kwargs, name=1)", "f(*args, 1)", "f(*a, *b)"] {
        assert!(parse(src, Dialect::Jinja, Context::Output)
            .diagnostics
            .iter()
            .any(|d| d.error == Error::InvalidArgumentOrder));
    }
    assert!(!parse("x if y else z", Dialect::Jinja, Context::Condition)
        .diagnostics
        .is_empty());
    valid("(x if y else z)", Dialect::Jinja, Context::Condition);
    assert!(
        rustpy_syntax::template::parse("{% if x if y else z %}x{% endif %}", Dialect::Jinja)
            .diagnostics
            .iter()
            .any(|d| d.error == rustpy_syntax::template::Error::InvalidExpression)
    );
    assert!(
        rustpy_syntax::template::parse("{% if (x if y else z) %}x{% endif %}", Dialect::Jinja)
            .diagnostics
            .is_empty()
    );
}
