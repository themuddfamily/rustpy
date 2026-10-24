use rustpy_highlight::{
    highlight_for, highlight_python, ColorScheme, HighlightTag as T, PYTHON_ATTRIBUTE_KEYS,
};

#[test]
fn remaining_bundled_formats_have_distinct_lexical_roles() {
    for (language, source, expected) in [
        (
            "gitignore",
            "# note\n!important/**\n",
            vec![("# note", T::Comment), ("!", T::Keyword), ("**", T::Number)],
        ),
        (
            "makefile",
            "all: input\n\techo $(VALUE)\n",
            vec![("all", T::Heading)],
        ),
        (
            "diff",
            "@@ -1 +1 @@\n-old\n+new\n",
            vec![("-old", T::Error), ("+new", T::String)],
        ),
        (
            "csv",
            "name,\"multi\nline\",42\n",
            vec![("\"multi\nline\"", T::String), ("42", T::Number)],
        ),
        (
            "regex",
            r"[α]+\d",
            vec![("[", T::Bracket), (r"\d", T::EscapeSequence)],
        ),
        (
            "properties",
            "! comment\na\\ key : value\n",
            vec![("! comment", T::Comment), ("a\\ key", T::Key)],
        ),
        (
            "dotenv",
            "export VALUE='text'\n",
            vec![("'text'", T::String)],
        ),
    ] {
        let spans = highlight_for(language, source);
        for (text, tag) in expected {
            assert!(
                spans.iter().any(|span| span.tag == tag
                    && &source[usize::from(span.range.start())..usize::from(span.range.end())]
                        == text),
                "{language} {text}: {spans:?}"
            );
        }
        for pair in spans.windows(2) {
            assert!(pair[0].range.end() <= pair[1].range.start());
        }
        for span in spans {
            assert!(source.is_char_boundary(usize::from(span.range.start())));
            assert!(source.is_char_boundary(usize::from(span.range.end())));
        }
    }
    assert!(highlight_for("plain text", "# untouched").is_empty());
}

#[test]
fn all_external_python_keys_have_scheme_styles_and_specific_roles() {
    assert_eq!(PYTHON_ATTRIBUTE_KEYS.len(), 35);
    for name in ColorScheme::builtin_names() {
        let scheme = ColorScheme::by_name(name).unwrap();
        for (key, tag) in PYTHON_ATTRIBUTE_KEYS {
            assert!(scheme.style(*tag).fg.is_some(), "{name} {key}");
        }
    }
    let source="def outer[T](value:T):\n    \"\"\":param value: documentation\"\"\"\n    def inner(): pass\n    value.method()\n    return '\\q'\n";
    let spans = highlight_python(source);
    for (text, tag) in [
        ("T", T::TypeParameter),
        ("inner", T::NestedFunctionDefinition),
        ("method", T::MethodCall),
        (":param", T::DocCommentTag),
        ("\\q", T::InvalidEscapeSequence),
    ] {
        assert!(
            spans.iter().any(|span| span.tag == tag
                && &source[usize::from(span.range.start())..usize::from(span.range.end())] == text),
            "{text}: {spans:?}"
        );
    }
}

#[test]
fn unicode_escape_validity_and_shared_external_string_key_are_actual() {
    for source in [
        r"'\U00110000'",
        r"'\N{NOT A UNICODE NAME}'",
        r"'''\U00110000'''",
        r"b'\u1234'",
    ] {
        assert!(
            highlight_python(source)
                .iter()
                .any(|span| span.tag == T::InvalidEscapeSequence),
            "{source}"
        );
    }
    for source in [r"'\U0001F600'", r"'\N{LATIN CAPITAL LETTER A}'"] {
        assert!(
            highlight_python(source)
                .iter()
                .any(|span| span.tag == T::EscapeSequence),
            "{source}"
        );
        assert!(
            !highlight_python(source)
                .iter()
                .any(|span| span.tag == T::InvalidEscapeSequence),
            "{source}"
        );
    }
    let mut scheme = ColorScheme::dark();
    let style = rustpy_highlight::TextStyle::fg(0x123456);
    assert!(scheme.set_python_attribute_style("PY.STRING.U", style));
    for span in highlight_python("f'plain {1}'") {
        if matches!(span.tag, T::String | T::FString) {
            assert_eq!(scheme.style(span.tag), style);
        }
    }
}

#[test]
fn make_assignment_operators_and_properties_value_delimiters_are_not_targets() {
    for operator in [":=", "::=", ":::=", "+=", "?=", "!=", "="] {
        let source = format!("NAME {operator} value\n");
        let spans = highlight_for("makefile", &source);
        assert!(
            spans.iter().any(|span| span.tag == T::Variable
                && &source[usize::from(span.range.start())..usize::from(span.range.end())]
                    == "NAME"),
            "{operator}: {spans:?}"
        );
        assert!(!spans.iter().any(|span| span.tag == T::Heading));
    }
    let source = "key==value\n";
    assert!(highlight_for("properties", source)
        .iter()
        .any(|span| span.tag == T::String
            && &source[usize::from(span.range.start())..usize::from(span.range.end())]
                == "=value"));
}

#[test]
fn primary_python_defaults_resolve_generic_inheritance_for_all_three_schemes() {
    use rustpy_highlight::Rgb;
    let light = ColorScheme::light();
    let dark = ColorScheme::dark();
    let contrast = ColorScheme::high_contrast();
    let tag = light.python_attribute_style("PY.DOC_COMMENT_TAG").unwrap();
    assert_eq!(tag.fg, Some(Rgb::hex(0x000000)));
    assert!(tag.bold && !tag.italic && tag.underline);
    assert_eq!(tag.effect_color, Some(Rgb::hex(0x808080)));
    let invalid = dark
        .python_attribute_style("PY.INVALID_STRING_ESCAPE")
        .unwrap();
    assert_eq!(invalid.fg, Some(Rgb::hex(0x6a8759)));
    assert!(invalid.wave);
    assert_eq!(invalid.effect_color, Some(Rgb::hex(0xff0000)));
    assert_eq!(
        light
            .python_attribute_style("PY.INVALID_STRING_ESCAPE")
            .unwrap()
            .bg,
        Some(Rgb::hex(0xffcccc))
    );
    let predefined = dark.python_attribute_style("PY.PREDEFINED_USAGE").unwrap();
    assert!(predefined.italic);
    assert_eq!(predefined.fg, Some(Rgb::hex(0xa9b7c6)));
    assert_eq!(
        contrast.python_attribute_style("PY.KEYWORD").unwrap().fg,
        Some(Rgb::hex(0xed864a))
    );
    let number = contrast.python_attribute_style("PY.NUMBER").unwrap();
    assert_eq!(number.fg, Some(Rgb::hex(0x33ccff)));
    assert!(number.bold);
    assert_eq!(contrast.style(T::Bytes).fg, Some(Rgb::hex(0x54b33e)));
    assert_eq!(contrast.style(T::FString).fg, Some(Rgb::hex(0x54b33e)));
    let invalid = contrast
        .python_attribute_style("PY.INVALID_STRING_ESCAPE")
        .unwrap();
    assert!(invalid.bold_underline);
    assert_eq!(invalid.effect_color, Some(Rgb::hex(0xff0d25)));
}
