//! Structured documentation must retain Python Unicode names in every format.
use rustpy_semantic::docstrings::{parse, render_markdown, DocstringFormat};

#[test]
fn rest_and_epytext_render_unicode_parameter_name_and_description() {
    for doc in [
        "Summary.\n\n:param π: Unicode parameter.\n:returns: Result.",
        "Summary.\n\n@param π: Unicode parameter.\n@return: Result.",
    ] {
        let parsed = parse(doc);
        assert_eq!(parsed.params[0].0, "π", "{doc:?}");
        let rendered = render_markdown(doc, DocstringFormat::Plain);
        assert!(rendered.contains("**Parameters**"), "{rendered}");
        assert!(
            rendered.contains("- `π` – Unicode parameter."),
            "{rendered}"
        );
        assert!(rendered.contains("Result."), "{rendered}");
    }
}

#[test]
fn google_renders_unicode_parameter_instead_of_dropping_args_section() {
    let doc = "Summary.\n\nArgs:\n    π (str): Unicode parameter.\nReturns:\n    str: Result.";
    let parsed = parse(doc);
    assert_eq!(parsed.format, DocstringFormat::Google);
    assert_eq!(
        parsed.params,
        vec![(
            "π".to_owned(),
            Some("str".to_owned()),
            "Unicode parameter.".to_owned()
        )]
    );
    let rendered = render_markdown(doc, DocstringFormat::Plain);
    assert!(rendered.contains("**Parameters**"), "{rendered}");
    assert!(
        rendered.contains("- `π` (*str*) – Unicode parameter."),
        "{rendered}"
    );
    assert!(rendered.contains("*str* – Result."), "{rendered}");
}

#[test]
fn numpy_renders_unicode_parameter_name_and_description() {
    let doc = "Summary.\n\nParameters\n----------\nπ : str\n    Unicode parameter.\nReturns\n-------\nstr\n    Result.";
    let parsed = parse(doc);
    assert_eq!(parsed.format, DocstringFormat::NumPy);
    assert_eq!(parsed.params[0].0, "π");
    let rendered = render_markdown(doc, DocstringFormat::Plain);
    assert!(
        rendered.contains("- `π` (*str*) – Unicode parameter."),
        "{rendered}"
    );
    assert!(rendered.contains("Result."), "{rendered}");
}

#[test]
fn google_unicode_xid_names_cover_combining_astral_stars_attributes_and_exceptions() {
    let doc = "Summary.\n\nArgs:\n    e\u{301} (str): Combining name.\n    𝒙 (int): Astral name.\n    *参数: Starred name.\n    **_键: Keyword name.\n    3bad: Not a Python identifier.\n    \u{301}bad: Not an identifier start.\nAttributes:\n    值 (str): Unicode attribute.\nRaises:\n    包.错误: Unicode exception.";
    let parsed = parse(doc);
    let names = parsed
        .params
        .iter()
        .map(|param| param.0.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["e\u{301}", "𝒙", "参数", "_键"]);
    assert_eq!(parsed.params[0].1.as_deref(), Some("str"));
    assert_eq!(parsed.params[1].1.as_deref(), Some("int"));
    assert_eq!(
        parsed.attributes,
        [("值".to_owned(), "Unicode attribute.".to_owned())]
    );
    assert_eq!(
        parsed.raises,
        [("包.错误".to_owned(), "Unicode exception.".to_owned())]
    );
    let rendered = render_markdown(doc, DocstringFormat::Plain);
    for name in names {
        assert!(rendered.contains(name), "{rendered}");
    }
    assert!(rendered.contains("Unicode attribute."), "{rendered}");
    assert!(rendered.contains("包.错误"), "{rendered}");
    assert!(!rendered.contains("3bad"), "{rendered}");
}
