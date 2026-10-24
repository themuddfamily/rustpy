#[path = "support/black_corpus.rs"]
mod corpus;
#[test]
fn pinned_black_corpus_has_complete_verified_inventory() {
    let manifest = corpus::verified_manifest().expect("licensed pinned upstream corpus");
    assert_eq!(manifest.files.len(), 197);
}
#[test]
fn upstream_fixture_protocol_preserves_already_formatted_and_flags() {
    let (flags, input, output) = corpus::fixture("# flags: --skip-string-normalization\nx = 'a'\n");
    assert_eq!(flags, ["--skip-string-normalization"]);
    assert_eq!(input, "x = 'a'\n");
    assert_eq!(output, input);
    let (_, input, output) = corpus::fixture("x=1\n# output\nx = 1\n");
    assert_eq!(input, "x=1\n");
    assert_eq!(output, "x = 1\n");
}
#[test]
fn pinned_black_assessment_preserves_comments_and_stability() {
    let report = corpus::assess().expect("complete offline assessment");
    assert_eq!(report.cases.len(), report.total);
    assert_eq!(
        report.total, 197,
        "every real pinned case remains in the denominator"
    );
    assert_eq!(
        report.exact
            + report.different
            + report.refused
            + report.unsupported
            + report.unstable
            + report.comments_changed,
        report.total,
        "a refusal or unsupported mode is not an exact conformance match"
    );
    assert_eq!(
        report.unstable,
        0,
        "{}",
        serde_json::to_string_pretty(&report).unwrap()
    );
    assert_eq!(
        report.comments_changed,
        0,
        "{}",
        serde_json::to_string_pretty(&report).unwrap()
    );
    // Best-effort exactness is reported, never fabricated by dropping refusals
    // or unsupported preview/target-version modes from the denominator.
}

#[test]
fn logical_line_ranges_use_original_lines_and_mapped_second_pass_ownership() {
    let mode = corpus::options(&["--line-ranges=1-2".into()]).unwrap();
    let source = "def selected(a,\n b):\n    body  =  1\n    return  body\n";
    let first = mode.format(source).unwrap();
    assert_eq!(
        first.text,
        "def selected(a, b):\n    body  =  1\n    return  body\n"
    );
    assert_eq!(mode.format_again(&first).unwrap().text, first.text);
}
#[test]
fn line_ranges_preserve_outside_source_and_union_multiple_real_selections() {
    let source = "π=[1,2]\nuntouched   =   1\nlast=[3,4]";
    let outside = corpus::options(&["--line-ranges=5000-6000".into()]).unwrap();
    let result = outside.format(source).unwrap();
    assert_eq!(result.text, source);
    assert_eq!(outside.format_again(&result).unwrap().text, source);
    let mode =
        corpus::options(&["--line-ranges=3-999".into(), "--line-ranges=1-1".into()]).unwrap();
    let result = mode.format(source).unwrap();
    assert_eq!(
        result.text,
        "π = [1, 2]\nuntouched   =   1\nlast = [3, 4]\n"
    );
    assert_eq!(mode.format_again(&result).unwrap().text, result.text);
}
#[test]
fn range_dispatch_does_not_admit_unimplemented_other_modes() {
    for flag in [
        "--pyi",
        "--preview",
        "--unstable",
        "--target-version=py312",
        "--line-ranges=0-1",
        "--line-ranges=3-2",
    ] {
        assert!(corpus::options(&[flag.into()]).is_err(), "{flag}");
    }
}
