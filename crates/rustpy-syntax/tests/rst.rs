use rustpy_syntax::rst::{parse, ErrorKind, Kind};
fn ranges(source: &str) {
    let doc = parse(source);
    for b in doc.blocks {
        assert!(source.get(b.range).is_some());
        match b.kind {
            Kind::Heading { title, .. } => {
                assert!(source.get(title).is_some());
            }
            Kind::Directive { name, arguments } => {
                assert!(source.get(name).is_some());
                assert!(source.get(arguments).is_some());
            }
            Kind::Field { name, content } => {
                assert!(source.get(name).is_some());
                assert!(source.get(content).is_some());
            }
            Kind::ListItem { marker, content } => {
                assert!(source.get(marker).is_some());
                assert!(source.get(content).is_some());
            }
            _ => {}
        }
    }
}
#[test]
fn unicode_crlf_headings_and_paragraph_exact_source() {
    let s = "π🙂 Title\r\n=========\r\n\r\nbody é\r\nsecond line\r\n";
    let d = parse(s);
    assert!(d.diagnostics.is_empty());
    assert_eq!(d.blocks.len(), 2);
    let Kind::Heading {
        title,
        adornment,
        overline,
    } = &d.blocks[0].kind
    else {
        panic!()
    };
    assert_eq!(&s[title.clone()], "π🙂 Title");
    assert_eq!(*adornment, '=');
    assert!(!overline);
    assert_eq!(&s[d.blocks[1].range.clone()], "body é\r\nsecond line");
    ranges(s);
}
#[test]
fn overline_heading_and_short_adornment_are_not_lost() {
    let s = "=======\nπ title\n=======\n\nToo long title\n---\n";
    let d = parse(s);
    assert_eq!(d.blocks.len(), 2);
    assert!(matches!(
        d.blocks[0].kind,
        Kind::Heading { overline: true, .. }
    ));
    assert_eq!(d.diagnostics[0].kind, ErrorKind::ShortAdornment);
    assert_eq!(&s[d.diagnostics[0].range.clone()], "---");
    ranges(s);
}
#[test]
fn directive_keeps_options_nested_body_and_dedent_boundary() {
    let s = ".. code-block:: python\n   :caption: π🙂\n\n   print('λ')\n\nnext paragraph\n";
    let d = parse(s);
    assert_eq!(d.blocks.len(), 2);
    let Kind::Directive { name, arguments } = &d.blocks[0].kind else {
        panic!()
    };
    assert_eq!(&s[name.clone()], "code-block");
    assert_eq!(&s[arguments.clone()], "python");
    assert!(&s[d.blocks[0].range.clone()].contains("print('λ')"));
    assert_eq!(&s[d.blocks[1].range.clone()], "next paragraph");
    ranges(s);
}
#[test]
fn literal_keeps_blank_lines_and_ignores_markup_inside() {
    let s = "Example::\n\n    :not-a-field: π\n\n    .. not-a-directive::\n\nplain\n";
    let d = parse(s);
    assert_eq!(d.blocks.len(), 3);
    assert!(matches!(d.blocks[1].kind, Kind::Literal));
    assert_eq!(
        &s[d.blocks[1].range.clone()],
        "    :not-a-field: π\n\n    .. not-a-directive::"
    );
    assert!(d.diagnostics.is_empty());
    ranges(s);
    let d = parse("::\n\n    π\n");
    assert!(matches!(d.blocks[1].kind, Kind::Literal));
}
#[test]
fn lists_and_fields_preserve_unicode_entry_ranges_and_continuations() {
    let s =
        "• π item\n  continued🙂\n- other\n\n:param π: value λ\n    continuation\n:returns: 🙂\n";
    let d = parse(s);
    assert_eq!(d.blocks.len(), 4);
    let Kind::ListItem { marker, content } = &d.blocks[0].kind else {
        panic!()
    };
    assert_eq!(&s[marker.clone()], "•");
    assert_eq!(&s[content.clone()], "π item");
    let Kind::Field { name, content } = &d.blocks[2].kind else {
        panic!()
    };
    assert_eq!(&s[name.clone()], "param π");
    assert_eq!(&s[content.clone()], "value λ");
    assert!(&s[d.blocks[2].range.clone()].ends_with("continuation"));
    ranges(s);
}
#[test]
fn unknown_explicit_markup_and_missing_literal_recover_following_heading() {
    let s = ".. bad name:: x\n\nexample::\n\nNext\n====\n";
    let d = parse(s);
    assert_eq!(
        d.diagnostics.iter().map(|x| x.kind).collect::<Vec<_>>(),
        [ErrorKind::MissingLiteral]
    );
    assert!(matches!(
        d.blocks.last().unwrap().kind,
        Kind::Heading { .. }
    ));
    ranges(s);
}
#[test]
fn numbered_markers_comments_and_missing_final_newline_remain_bounded() {
    let s = "1. π\n(a) λ\n#. next\n\n.. comment\n   not executed\n\nlast🙂";
    let d = parse(s);
    assert_eq!(d.blocks.len(), 5);
    assert!(d.blocks[..3]
        .iter()
        .all(|x| matches!(x.kind, Kind::ListItem { .. })));
    assert!(matches!(d.blocks[3].kind, Kind::ExplicitMarkup));
    assert_eq!(&s[d.blocks[4].range.clone()], "last🙂");
    ranges(s);
}

#[test]
fn valid_substitutions_comments_and_extended_directive_names_have_no_false_errors() {
    let source = ".. |logo| image:: logo.png\n\n.. This comment contains:: text\n\n.. domain.name+part:leaf:: π\n   body\n";
    let doc = parse(source);
    assert!(doc.diagnostics.is_empty());
    assert_eq!(doc.blocks.len(), 3);
    assert!(matches!(doc.blocks[0].kind, Kind::ExplicitMarkup));
    assert!(matches!(doc.blocks[1].kind, Kind::ExplicitMarkup));
    let Kind::Directive { name, arguments } = &doc.blocks[2].kind else {
        panic!()
    };
    assert_eq!(&source[name.clone()], "domain.name+part:leaf");
    assert_eq!(&source[arguments.clone()], "π");
    ranges(source);
}

#[test]
fn quoted_literal_retains_prefix_and_stops_at_blank_line_without_false_missing_error() {
    let source = "Paragraph::\n\n> quote π\n> second 🙂\n\nnext paragraph\n";
    let doc = parse(source);
    assert!(doc.diagnostics.is_empty());
    assert_eq!(doc.blocks.len(), 3);
    assert!(matches!(doc.blocks[1].kind, Kind::Literal));
    assert_eq!(
        &source[doc.blocks[1].range.clone()],
        "> quote π\n> second 🙂"
    );
    assert_eq!(&source[doc.blocks[2].range.clone()], "next paragraph");
    ranges(source);
}

#[test]
fn escaped_literal_marker_is_plain_text_while_even_backslashes_preserve_marker() {
    let escaped = "Paragraph\\::\n\nplain\n";
    let doc = parse(escaped);
    assert!(doc.diagnostics.is_empty());
    assert_eq!(doc.blocks.len(), 2);
    assert!(doc.blocks.iter().all(|b| matches!(b.kind, Kind::Paragraph)));
    assert_eq!(&escaped[doc.blocks[0].range.clone()], "Paragraph\\::");
    ranges(escaped);
    let even = "Paragraph\\\\::\n\n    literal π\n";
    let doc = parse(even);
    assert!(doc.diagnostics.is_empty());
    assert_eq!(doc.blocks.len(), 2);
    assert!(matches!(doc.blocks[1].kind, Kind::Literal));
    assert_eq!(&even[doc.blocks[1].range.clone()], "    literal π");
    ranges(even);
    let missing = parse("Paragraph\\\\::\n\nplain\n");
    assert_eq!(missing.diagnostics[0].kind, ErrorKind::MissingLiteral);
}
