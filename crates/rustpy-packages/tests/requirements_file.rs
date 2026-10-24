use rustpy_packages::requirements::{parse_requirement, parse_requirements};
use rustpy_packages::requirements_file::{parse, Syntax, Target};
#[test]
fn named_full_grammar_and_legacy_adapter() {
    let text = "Pkg_Name[Speed,security] (>=1.2,!=1.4.*,<3) ; (python_version >= '3.9' and os_name not in 'nt') or extra == 'fast'\nPkg @ https://example.invalid/x.whl#sha256=abcd ; sys_platform == 'linux'\n";
    let parsed = parse(text);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let Syntax::Requirement {
        target: Target::Named(named),
        ..
    } = &parsed.records[1].syntax
    else {
        panic!("named URL");
    };
    assert!(named.version_or_url.is_some());
    let legacy = parse_requirements(text);
    assert_eq!(legacy.len(), 2);
    assert_eq!(legacy[0].name, "Pkg_Name");
    assert_eq!(legacy[0].specifier.as_deref(), Some(">=1.2,!=1.4.*,<3"));
    assert_eq!(legacy[1].marker.as_deref(), Some("sys_platform == 'linux'"));
}
#[test]
fn source_kinds_never_fictitious_packages() {
    let text="./local π\n./local\nC:\\src\\pkg\nproject.whl\nproject.tar.gz\nhttps://example.invalid/archive.zip#sha256=abcd\ngit+https://example.invalid/repo#egg=project\n-e './local π'\n";
    let parsed = parse(text);
    assert_eq!(parsed.records.len(), 8);
    assert_eq!(parsed.diagnostics.len(), 1); // unquoted whitespace path
    assert!(parse_requirements(text).is_empty());
    assert!(matches!(
        parsed.records[3].syntax,
        Syntax::Requirement {
            target: Target::Archive(_),
            ..
        }
    ));
    assert!(matches!(
        parsed.records[6].syntax,
        Syntax::Requirement {
            target: Target::Vcs(_),
            ..
        }
    ));
    assert!(matches!(
        parsed.records[7].syntax,
        Syntax::Requirement { editable: true, .. }
    ));
}
#[test]
fn options_includes_hashes_config_and_environment_are_inert() {
    let parsed=parse("-r other.txt\n-cconstraints.txt\n--index-url ${INDEX_URL} --extra-index-url https://example.invalid --no-index\n-f './wheels π'\nPkg==1 --hash=sha256:abcd --hash sha512:abcd -C build=π\n-e git+https://example.invalid/repo --config-settings=build=\n");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let Syntax::Options(options) = &parsed.records[2].syntax else {
        panic!("options")
    };
    assert_eq!(options[0].value.as_deref(), Some("${INDEX_URL}"));
    let Syntax::Requirement { options, .. } = &parsed.records[4].syntax else {
        panic!("requirement")
    };
    assert_eq!(options.len(), 3);
    assert_eq!(options[2].value.as_deref(), Some("build=π"));
    assert_eq!(
        parse_requirements("Pkg==1 --hash=sha256:abcd\n")[0]
            .specifier
            .as_deref(),
        Some("==1")
    );
}
#[test]
fn continuation_crlf_bom_unicode_offsets_and_url_fragment() {
    let text = "\u{feff}Pkg>=1 \\\r\n  ,<3 # π comment\r\nhttps://example.invalid/x#fragment\r\n";
    let p = parse(text);
    assert!(p.diagnostics.is_empty());
    assert_eq!(p.records[0].line, 1);
    assert_eq!(p.records[0].text, "Pkg>=1   ,<3");
    for r in &p.records {
        for (i, c) in r.text.char_indices() {
            assert_eq!(text[r.source_offsets[i]..].chars().next(), Some(c));
        }
    }
    assert!(p.records[1].text.ends_with("#fragment"));
}
#[test]
fn malformed_named_grammar_recovers_without_extraction() {
    let text="bad[extra,]\nbad>=wat\nbad ; imaginary == 'x'\nbad ; python_version <\nbad--\nbad==1 --hash=sha256:nothex\n--unknown x\ngood>=1\n";
    let p = parse(text);
    assert_eq!(p.diagnostics.len(), 7);
    assert_eq!(parse_requirements(text).len(), 1);
    assert_eq!(parse_requirements(text)[0].name, "good");
    assert!(parse_requirement("bad ; os_name ==", 1).is_none());
}
#[test]
fn quoted_minus_marker_is_not_an_option_and_semicolon_in_url_is_url() {
    assert!(
        parse("Pkg ; os_name == '-C'\nPkg @ https://example.invalid/path;a\n")
            .diagnostics
            .is_empty()
    );
    assert_eq!(
        parse_requirements("Pkg ; os_name == '-C'\n")[0]
            .marker
            .as_deref(),
        Some("os_name == '-C'")
    );
    assert_eq!(
        parse_requirement("Pkg @ https://example.invalid/path;a", 1)
            .unwrap()
            .marker,
        None
    );
}
#[test]
fn dangling_continuation_and_budgets_report_without_panics() {
    let dangling = parse("Pkg\\");
    assert_eq!(dangling.diagnostics.len(), 1);
    assert!(matches!(dangling.records[0].syntax, Syntax::Invalid));
    assert!(parse_requirements("Pkg\\").is_empty());
    assert!(parse_requirement(&"x".repeat(64 * 1024 + 1), 1).is_none());
    let nested = format!("Pkg ; {}os_name == 'nt'{}", "(".repeat(65), ")".repeat(65));
    assert!(parse_requirement(&nested, 1).is_none());
    assert!(parse_requirements(&nested).is_empty());
    assert!(parse(&"x".repeat(2 * 1024 * 1024 + 1)).records.is_empty());
    let p = parse(&"x".repeat(64 * 1024 + 1));
    assert_eq!(p.diagnostics.len(), 1);
    assert_eq!(parse("--π=1\ngood\n").diagnostics.len(), 1);
}

#[test]
fn whitespace_only_dangling_continuation_preserves_previous_dependency() {
    let source = "good\n  \\\n";
    let parsed = parse(source);
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.records.len(), 1);
    assert!(matches!(
        parsed.records[0].syntax,
        Syntax::Requirement {
            target: Target::Named(_),
            ..
        }
    ));
    let extracted = parse_requirements(source);
    assert_eq!(extracted.len(), 1);
    assert_eq!(extracted[0].name, "good");
    assert_eq!(extracted[0].line, 1);
}
