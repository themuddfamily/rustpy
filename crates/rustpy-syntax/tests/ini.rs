use rustpy_syntax::ini::{parse, Dialect, ErrorKind};

#[test]
fn setup_cfg_unicode_crlf_sections_values_and_continuations_have_exact_ranges() {
    let source = "\u{feff}; comment\r\n[metadata]\r\nname = package-π\r\n[options]\r\ninstall_requires = requests>=2\r\n    idna; python_version >= '3'\r\n    # a comment does not end the value\r\n    urllib3\r\nzip_safe: False\r\n";
    let parsed = parse(source, Dialect::ConfigParser);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.source, source);
    assert_eq!(parsed.sections.len(), 2);
    assert_eq!(&source[parsed.sections[0].name.clone()], "metadata");
    assert_eq!(&source[parsed.sections[1].name.clone()], "options");
    assert_eq!(parsed.properties.len(), 3);
    assert_eq!(parsed.properties[0].section, Some(0));
    assert_eq!(parsed.value(&parsed.properties[0]), "package-π");
    assert_eq!(parsed.properties[1].section, Some(1));
    assert_eq!(
        parsed.value(&parsed.properties[1]),
        "requests>=2\nidna; python_version >= '3'\nurllib3"
    );
    assert_eq!(
        &source[parsed.properties[1].range.clone()],
        "install_requires = requests>=2\r\n    idna; python_version >= '3'\r\n    # a comment does not end the value\r\n    urllib3"
    );
    assert_eq!(&source[parsed.properties[2].key.clone()], "zip_safe");
    assert_eq!(parsed.value(&parsed.properties[2]), "False");
}

#[test]
fn editorconfig_root_and_glob_section_preserve_unset_and_literal_delimiters() {
    let source = "root=true\n; glob\n[file[0-9].py]\nindent_style = space\nindent_size = unset\npattern = a:b=c#d;e\n";
    let parsed = parse(source, Dialect::EditorConfig);
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.properties[0].section, None);
    assert_eq!(parsed.value(&parsed.properties[0]), "true");
    assert_eq!(&source[parsed.sections[0].name.clone()], "file[0-9].py");
    assert_eq!(parsed.value(&parsed.properties[2]), "unset");
    assert_eq!(parsed.value(&parsed.properties[3]), "a:b=c#d;e");
    assert_eq!(parsed.properties[3].section, Some(0));
}

#[test]
fn duplicate_sections_and_case_folded_keys_remain_recoverable_records() {
    let parsed = parse("[x]\nΠ=one\nπ=two\n[x]\nΠ=three\n", Dialect::ConfigParser);
    let kinds: Vec<_> = parsed.diagnostics.iter().map(|error| error.kind).collect();
    assert_eq!(
        kinds,
        [
            ErrorKind::DuplicateKey,
            ErrorKind::DuplicateSection,
            ErrorKind::DuplicateKey
        ]
    );
    assert_eq!(parsed.sections.len(), 2);
    assert_eq!(parsed.properties.len(), 3);
    assert_eq!(parsed.value(&parsed.properties[2]), "three");
    assert_eq!(parsed.properties[2].section, Some(1));
}

#[test]
fn malformed_input_reports_ranges_and_recovers_next_valid_section() {
    let source = "outside=1\n[broken\n=empty\nbare\n[ok] junk\nvalid = π🙂\n";
    let parsed = parse(source, Dialect::ConfigParser);
    let kinds: Vec<_> = parsed.diagnostics.iter().map(|error| error.kind).collect();
    assert_eq!(
        kinds,
        [
            ErrorKind::MissingSection,
            ErrorKind::MalformedSection,
            ErrorKind::EmptyKey,
            ErrorKind::MissingDelimiter
        ]
    );
    assert_eq!(parsed.properties.len(), 2);
    assert_eq!(parsed.value(&parsed.properties[1]), "π🙂");
    assert_eq!(parsed.properties[1].section, Some(0));
    for error in parsed.diagnostics {
        assert!(source.get(error.range).is_some());
    }
}

#[test]
fn dialect_continuation_and_colon_policies_do_not_cross_contaminate() {
    let source = "[x]\nvalue=first\n    next=second\nflag:yes\n";
    let cfg = parse(source, Dialect::ConfigParser);
    assert!(cfg.diagnostics.is_empty());
    assert_eq!(cfg.properties.len(), 2);
    assert_eq!(cfg.value(&cfg.properties[0]), "first\nnext=second");
    let editor = parse(source, Dialect::EditorConfig);
    assert_eq!(editor.properties.len(), 2);
    assert_eq!(editor.value(&editor.properties[0]), "first");
    assert_eq!(&source[editor.properties[1].key.clone()], "next");
    assert_eq!(editor.diagnostics[0].kind, ErrorKind::MissingDelimiter);
}

#[test]
fn interpolation_quotes_and_inline_comment_bytes_are_not_evaluated() {
    let parsed = parse(
        "[x]\npath=%(base)s/π # literal\nquoted=\"a;b\"\nempty=\n",
        Dialect::ConfigParser,
    );
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.value(&parsed.properties[0]), "%(base)s/π # literal");
    assert_eq!(parsed.value(&parsed.properties[1]), "\"a;b\"");
    assert_eq!(parsed.value(&parsed.properties[2]), "");
}

#[test]
fn blank_parts_bom_and_missing_final_newline_are_source_preserved() {
    let source = "\u{feff}[π]\nvalue = first\n\n    second";
    let parsed = parse(source, Dialect::ConfigParser);
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.value(&parsed.properties[0]), "first\n\nsecond");
    assert_eq!(&source[parsed.sections[0].name.clone()], "π");
    for property in &parsed.properties {
        assert!(source.get(property.range.clone()).is_some());
        assert!(source.get(property.key.clone()).is_some());
        for value in &property.values {
            assert!(source.get(value.clone()).is_some());
        }
    }
}

#[test]
fn dense_properties_share_a_large_section_name_without_copying_it_per_key() {
    let name = "π".repeat(4096);
    let mut source = format!("[{name}]\n");
    for i in 0..2000 {
        source.push_str(&format!("key{i}=v{i}\n"));
    }
    let parsed = parse(&source, Dialect::ConfigParser);
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.sections.len(), 1);
    assert_eq!(parsed.properties.len(), 2000);
    assert_eq!(parsed.value(&parsed.properties[1999]), "v1999");
    assert!(parsed
        .properties
        .iter()
        .all(|property| property.section == Some(0)));
}

#[test]
fn inner_section_whitespace_is_literal_for_both_dialects() {
    let source = "[ x ]\na=1\n[x]\na=2\n[ ]\nb=π\n";
    for dialect in [Dialect::ConfigParser, Dialect::EditorConfig] {
        let parsed = parse(source, dialect);
        assert!(parsed.diagnostics.is_empty());
        let names: Vec<_> = parsed
            .sections
            .iter()
            .map(|s| &source[s.name.clone()])
            .collect();
        assert_eq!(names, [" x ", "x", " "]);
        assert_eq!(parsed.properties[0].section, Some(0));
        assert_eq!(parsed.properties[1].section, Some(1));
        assert_eq!(parsed.value(&parsed.properties[2]), "π");
    }
}

#[test]
fn editorconfig_repeated_globs_and_properties_are_valid_ordered_records() {
    let source = "[*.py]\nIndent_Size=2\nindent_size=3\n[*.py]\nindent_size=4\n";
    let parsed = parse(source, Dialect::EditorConfig);
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.sections.len(), 2);
    assert_eq!(parsed.properties.len(), 3);
    assert_eq!(parsed.properties[2].section, Some(1));
    assert_eq!(parsed.value(&parsed.properties[0]), "2");
    assert_eq!(parsed.value(&parsed.properties[1]), "3");
    assert_eq!(parsed.value(&parsed.properties[2]), "4");
    let cfg = parse(source, Dialect::ConfigParser);
    assert_eq!(
        cfg.diagnostics.iter().map(|d| d.kind).collect::<Vec<_>>(),
        [
            ErrorKind::DuplicateKey,
            ErrorKind::DuplicateSection,
            ErrorKind::DuplicateKey
        ]
    );
}

#[test]
fn editorconfig_header_inline_text_is_invalid_without_swallowing_following_records() {
    for suffix in [" # not a comment", " ; not a comment", " junk"] {
        let source = format!("[file[0-9].py]{suffix}\nx=1\n[ok]\ny=π\n");
        let parsed = parse(&source, Dialect::EditorConfig);
        assert_eq!(parsed.diagnostics.len(), 1);
        assert_eq!(parsed.diagnostics[0].kind, ErrorKind::TrailingSectionText);
        assert_eq!(&source[parsed.diagnostics[0].range.clone()], suffix);
        assert_eq!(parsed.properties.len(), 2);
        assert_eq!(&source[parsed.sections[1].name.clone()], "ok");
        assert_eq!(parsed.value(&parsed.properties[1]), "π");
    }
}

#[test]
fn configparser_unicode_indentation_uses_character_depth_and_byte_ranges() {
    let source = "[x]\n\u{2003}key=first\n  second π\n\u{2003}next=last\n";
    let parsed = parse(source, Dialect::ConfigParser);
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.properties.len(), 2);
    assert_eq!(parsed.value(&parsed.properties[0]), "first\nsecond π");
    assert_eq!(&source[parsed.properties[0].key.clone()], "key");
    assert_eq!(&source[parsed.properties[0].values[1].clone()], "second π");
    assert_eq!(parsed.value(&parsed.properties[1]), "last");
}

#[test]
fn repeated_configparser_default_headers_are_valid_without_evaluating_inheritance() {
    let source = "[DEFAULT]\na=1\n[DEFAULT]\nb=2\n[x]\nc=3\n";
    let parsed = parse(source, Dialect::ConfigParser);
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.sections.len(), 3);
    assert_eq!(parsed.properties.len(), 3);
    assert_eq!(parsed.value(&parsed.properties[1]), "2");
    let duplicate = parse("[DEFAULT]\na=1\n[DEFAULT]\na=2\n", Dialect::ConfigParser);
    assert_eq!(duplicate.diagnostics[0].kind, ErrorKind::DuplicateKey);
}
