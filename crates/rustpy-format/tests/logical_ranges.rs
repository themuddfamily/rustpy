//! Original-coordinate logical ownership regressions. Authored, not executed.
use rustpy_format::{format_logical_ranges, format_range, FormatOptions};
use rustpy_syntax::{TextRange, TextSize};
fn selection(source: &str, needle: &str) -> TextRange {
    let start = source.find(needle).unwrap();
    TextRange::new(
        TextSize::new(start as u32),
        TextSize::new((start + needle.len()) as u32),
    )
}
fn apply(source: &str, ranges: &[TextRange]) -> (String, Vec<TextRange>) {
    let result = format_logical_ranges(source, ranges, &FormatOptions::black()).unwrap();
    let output = rustpy_syntax::printer::apply_text_edits(source, &result.edits).unwrap();
    assert_eq!(
        rustpy_format::semantic_fingerprint(source),
        rustpy_format::semantic_fingerprint(&output)
    );
    assert!(rustpy_format::comments_preserved(source, &output));
    let again =
        format_logical_ranges(&output, &result.mapped_ranges, &FormatOptions::black()).unwrap();
    assert!(
        again.edits.is_empty(),
        "mapped ownership is not stable: {output}"
    );
    (output, result.mapped_ranges)
}
#[test]
fn interior_selection_expands_assignment_but_strict_selection_remains_strict() {
    let source = "head=0\nvalues = [\n    1,\n    2\n]\ntail=3\n";
    let range = selection(source, "    1,");
    assert!(format_range(source, range, &FormatOptions::black()).is_err());
    let (output, _) = apply(source, &[range]);
    assert_eq!(output, "head=0\nvalues = [1, 2]\ntail=3\n");
}
#[test]
fn changed_header_line_count_does_not_select_previously_unselected_body() {
    let source="def selected(a,\n b):\n    untouched  =  1\n    return  untouched\n\ndef other(  ):\n pass\n";
    let (output, mapped) = apply(source, &[selection(source, "selected(a,")]);
    assert!(output.starts_with("def selected(a, b):\n    untouched  =  1\n    return  untouched\n"));
    assert!(output.ends_with("def other(  ):\n pass\n"));
    assert_eq!(mapped.len(), 1);
    assert_eq!(
        &output[usize::from(mapped[0].start())..usize::from(mapped[0].end())],
        "def selected(a, b):\n"
    );
}
#[test]
fn changed_indentation_captures_its_complete_suite_and_retains_other_suite() {
    let source = "if ready:\n  first(  )\n  second(  )\n\nif other:\n  untouched(  )\n";
    let (output, _) = apply(source, &[selection(source, "first(")]);
    assert!(output.starts_with("if ready:\n    first()\n    second()\n"));
    assert!(output.ends_with("if other:\n  untouched(  )\n"));
}
#[test]
fn unordered_duplicate_multi_ranges_keep_intervening_statement_bytes() {
    let source = "first=[1,2]\nuntouched   =   1\nlast=[3,4]\n";
    let first = selection(source, "first=");
    let last = selection(source, "last=");
    let (output, _) = apply(source, &[last, first, first]);
    assert_eq!(output, "first = [1, 2]\nuntouched   =   1\nlast = [3, 4]\n");
}
#[test]
fn unicode_crlf_and_eof_bounds_map_by_original_bytes() {
    let source = "π=0\r\nvalue=[1,2]\r\nuntouched=  4";
    let (output, _) = apply(source, &[selection(source, "value=")]);
    assert_eq!(output, "π=0\r\nvalue = [1, 2]\r\nuntouched=  4");
    let eof = selection(source, "untouched=");
    let (output, mapped) = apply(source, &[eof]);
    assert_eq!(output, "π=0\r\nvalue=[1,2]\r\nuntouched = 4\r\n");
    assert_eq!(usize::from(mapped[0].end()), output.len());
}
#[test]
fn empty_selection_set_means_no_selected_statement() {
    let source = "unselected=  1\n";
    let result = format_logical_ranges(source, &[], &FormatOptions::black()).unwrap();
    assert!(result.edits.is_empty());
    assert!(result.mapped_ranges.is_empty());
}

#[test]
fn empty_eof_caret_owns_unterminated_final_statement() {
    for source in ["value=  1", "π=0\r\nvalue=  1"] {
        let (output, mapped) = apply(source, &[TextRange::empty(TextSize::of(source))]);
        let expected = if source.starts_with('π') {
            "π=0\r\nvalue = 1\r\n"
        } else {
            "value = 1\n"
        };
        assert_eq!(output, expected);
        assert_eq!(mapped.len(), 1);
        assert_eq!(usize::from(mapped[0].end()), output.len());
    }
}
#[test]
fn empty_interior_boundary_caret_does_not_acquire_previous_statement() {
    for source in ["first=  1\nvalue=  2", "π=  1\r\nvalue=  2"] {
        let at = source.find("value=").unwrap();
        let caret = TextRange::empty(TextSize::new(u32::try_from(at).unwrap()));
        let (output, mapped) = apply(source, &[caret]);
        assert_eq!(&output[..at], &source[..at]);
        assert!(output[at..].starts_with("value = 2"));
        assert_eq!(mapped.len(), 1);
        assert_eq!(usize::from(mapped[0].start()), at);
    }
}
