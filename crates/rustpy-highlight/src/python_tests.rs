use super::*;

fn assert_tag(source: &str, needle: &str, tag: HighlightTag) {
    let offset = source.find(needle).unwrap();
    let spans = highlight_python(source);
    assert_eq!(
        tag_at(&spans, TextSize::new(offset as u32)),
        Some(tag),
        "{needle:?} in {source:?}: {spans:?}"
    );
}

#[test]
fn every_python_attribute_has_an_actual_source_span() {
    use HighlightTag::*;
    let cases = [
        ("if True: pass\n", "if", Keyword),
        ("print(len([]))\n", "len", Builtin),
        ("@pkg.decorator\ndef f(): pass\n", "decorator", Decorator),
        ("value = 'text'\n", "'text'", String),
        ("value = rb'bytes'\n", "rb'bytes'", Bytes),
        ("value = f'{len(items) + 2}'\n", "len", Builtin),
        ("value = f'{len(items) + 2}'\n", "{", FStringBrace),
        ("value = 1.5e2 + 3j\n", "1.5", Number),
        ("value = 1 + 2\n", "+", Operator),
        ("def f(parameter): pass\n", "parameter", Parameter),
        ("def f(self): return self\n", "self", SelfParameter),
        ("class Thing: pass\n", "Thing", ClassDefinition),
        (
            "async def function(): pass\n",
            "function",
            FunctionDefinition,
        ),
        ("value = obj.__class__\n", "__class__", PredefinedUsage),
        ("# plain comment\n", "# plain", Comment),
        ("def f(): 'documentation'\n", "'documentation'", Docstring),
        ("value: User = item\n", "User", TypeAnnotation),
        ("f(keyword=value)\n", "keyword", KeywordArgument),
    ];
    for (source, needle, tag) in cases {
        assert_tag(source, needle, tag);
    }
}

#[test]
fn function_and_lambda_parameter_names_are_not_keyword_arguments() {
    use HighlightTag::*;
    let source = "def f(pos, /, default=1, *args, named=None, **kwargs): pass\ncallback = lambda item=1, *rest, **options: item\n";
    for name in [
        "pos", "default", "args", "named", "kwargs", "item", "rest", "options",
    ] {
        assert_tag(source, name, Parameter);
    }
    assert_tag(source, "None", Keyword);
    assert_tag("def f(self, cls): pass\n", "self", SelfParameter);
    assert_tag("def f(self, cls): pass\n", "cls", SelfParameter);
}

#[test]
fn annotations_cover_names_without_recoloring_expressions_or_defaults() {
    use HighlightTag::*;
    let source = "def f(value: pkg.Box[User], *args: int, **kwargs: str) -> Result | None: pass\nfield: list[Other] = maker(keyword=42)\n";
    for name in [
        "pkg", "Box", "User", "int", "str", "Result", "list", "Other",
    ] {
        assert_tag(source, name, TypeAnnotation);
    }
    assert_tag(source, "None", Keyword);
    assert_tag(source, "|", Operator);
    assert_tag(source, "maker", FunctionCall);
    assert_tag(source, "keyword", KeywordArgument);
    assert_tag(source, "42", Number);
    assert_tag(
        "def f(x: 'Forward') -> 'Result': pass\n",
        "'Forward'",
        String,
    );
}

#[test]
fn module_class_and_function_docstrings_use_first_statement_identity() {
    use HighlightTag::*;
    let source = "# header\n('module ' 'documentation')\nclass C:\n    r'class documentation'\n    def f(): 'function documentation'\n";
    for literal in [
        "'module '",
        "'documentation'",
        "r'class documentation'",
        "'function documentation'",
    ] {
        assert_tag(source, literal, Docstring);
    }
    assert_tag(
        "def f(): pass\n'not documentation'\n",
        "'not documentation'",
        String,
    );
    assert_tag(
        "if True:\n    'not documentation'\n",
        "'not documentation'",
        String,
    );
    assert_tag(
        "def f():\n    pass\n    'not documentation'\n",
        "'not documentation'",
        String,
    );
    assert_tag("b'not documentation'\n", "b'not documentation'", Bytes);
    assert_tag("f'not documentation'\n", "f'", FString);
}

#[test]
fn decorator_names_do_not_swallow_argument_highlighting() {
    use HighlightTag::*;
    let source = "@pkg.decorator(len(items), enabled=True, label='text')\ndef f(): pass\n";
    assert_tag(source, "@", Decorator);
    assert_tag(source, "pkg", Decorator);
    assert_tag(source, ".", Decorator);
    assert_tag(source, "decorator", Decorator);
    assert_tag(source, "len", Builtin);
    assert_tag(source, "enabled", KeywordArgument);
    assert_tag(source, "True", Keyword);
    assert_tag(source, "'text'", String);
    assert_tag("@factory(123).decorate\ndef f(): pass\n", "123", Number);
    assert_tag(
        "@factory(123).decorate\ndef f(): pass\n",
        "decorate",
        Decorator,
    );
}

#[test]
fn keyword_argument_tags_do_not_leak_into_assignment_or_parameter_defaults() {
    use HighlightTag::*;
    let source = "def f(default=1): pass\nf(first=1, second=2, **options)\nassigned = 1\n";
    assert_tag(source, "default", Parameter);
    assert_tag(source, "first", KeywordArgument);
    assert_tag(source, "second", KeywordArgument);
    let offset = source.find("assigned").unwrap();
    assert_eq!(
        tag_at(&highlight_python(source), TextSize::new(offset as u32)),
        None
    );
}

#[test]
fn nested_fstrings_keep_expression_and_literal_precedence() {
    use HighlightTag::*;
    let source = "result = f\"literal {len(data):>{width}} {f'{sum(items)}'}\"\n";
    assert_tag(source, "f\"", FString);
    assert_tag(source, "literal", FString);
    for name in crate::ColorScheme::builtin_names() {
        let scheme = crate::ColorScheme::by_name(name).unwrap();
        assert_eq!(scheme.style(FString), scheme.style(String), "{name}");
        assert_eq!(
            scheme.python_attribute_style("PY.STRING.U"),
            Some(scheme.style(FString)),
            "{name}"
        );
    }
    assert_tag(source, "len", Builtin);
    assert_tag(source, "sum", Builtin);
    let spans = highlight_python(source);
    assert_eq!(
        spans.iter().filter(|span| span.tag == FStringBrace).count(),
        8
    );
}

#[test]
fn broken_code_preserves_lexical_highlighting_and_valid_context() {
    use HighlightTag::*;
    let source =
        "def broken(parameter: User, default=1):\n    return f'{len(items)'\n@unfinished.\n";
    assert_tag(source, "parameter", Parameter);
    assert_tag(source, "User", TypeAnnotation);
    assert_tag(source, "default", Parameter);
    assert_tag(source, "return", Keyword);
    assert_tag(source, "len", Builtin);
    assert_tag(source, "f'", FString);
}

#[test]
fn parsed_and_lexer_entrypoints_agree_and_spans_are_byte_safe() {
    let source = "@decorator(label='🙂')\ndef café(α: Type = 1):\n    '文 documentation'\n    return f'{α + 2}' # comment\n";
    let parse = rustpy_syntax::parse(source);
    let spans = highlight_python_with_parse(source, &parse);
    assert_eq!(spans, highlight_python(source));
    assert_eq!(
        spans,
        highlight_tokens(source, &rustpy_syntax::tokenize(source).0)
    );
    for span in &spans {
        let start = usize::from(span.range.start());
        let end = usize::from(span.range.end());
        assert!(start < end && source.is_char_boundary(start) && source.is_char_boundary(end));
    }
    assert!(spans
        .windows(2)
        .all(|pair| pair[0].range.end() <= pair[1].range.start()));
}

#[test]
fn cached_parse_rehighlighting_changes_context_after_an_edit() {
    let before = "def f(named=1): pass\n";
    let after = "f(named=1)\n";
    let old = rustpy_syntax::parse(before);
    let updated = rustpy_syntax::reparse::reparse_text(&old, before, after).0;
    assert_tag(before, "named", HighlightTag::Parameter);
    let spans = highlight_python_with_parse(after, &updated);
    assert_eq!(
        tag_at(&spans, TextSize::new(after.find("named").unwrap() as u32)),
        Some(HighlightTag::KeywordArgument)
    );
}

#[test]
fn malformed_escapes_never_split_unicode_scalars() {
    let source = "value = '\\x🙂 \\é \\u文'\n";
    let spans = highlight_python(source);
    assert!(spans
        .iter()
        .any(|span| span.tag == HighlightTag::InvalidEscapeSequence));
    for span in spans {
        assert!(source.is_char_boundary(usize::from(span.range.start())));
        assert!(source.is_char_boundary(usize::from(span.range.end())));
        assert!(!span.range.is_empty());
    }
}

#[test]
fn fstring_format_fields_and_raw_literals_keep_distinct_lexical_modes() {
    let formatted = "value = f'{(lambda x: x)(3):>{width}}'\n";
    let spans = highlight_python(formatted);
    assert_eq!(
        spans
            .iter()
            .filter(|span| span.tag == HighlightTag::FStringBrace)
            .count(),
        4
    );
    assert_tag(formatted, "lambda", HighlightTag::Keyword);
    assert_tag(formatted, "x", HighlightTag::Parameter);
    assert_tag(formatted, "3", HighlightTag::Number);
    for source in ["value = r'raw\\n'\n", "value = fr'raw\\n{1}'\n"] {
        let spans = highlight_python(source);
        assert!(!spans
            .iter()
            .any(|span| span.tag == HighlightTag::EscapeSequence));
    }
    assert_tag(
        "value = f'rabbit\\n{1}'\n",
        "\\n",
        HighlightTag::EscapeSequence,
    );
}

#[test]
fn lexical_attribute_variants_keep_their_exact_token_tags() {
    use HighlightTag::*;
    for literal in ["42", "0xff", "0b101", "1_000", "1.25", "2e3", "3j"] {
        assert_tag(&format!("value = {literal}\n"), literal, Number);
    }
    for literal in ["b'abc'", "B'abc'", "rb'abc'", "Br'abc'"] {
        assert_tag(&format!("value = {literal}\n"), literal, Bytes);
    }
    for operator in ["+", "**", "//", "==", ">>", "@"] {
        assert_tag(
            &format!("value = left {operator} right\n"),
            operator,
            Operator,
        );
    }
    let source = "value = obj.self\n";
    let offset = TextSize::new(source.find("self").unwrap() as u32);
    assert_eq!(tag_at(&highlight_python(source), offset), None);
}
