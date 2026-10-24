use rustpy_format::{
    comments_preserved, format_source, semantic_fingerprint, FormatOptions, FormatterTags,
    QuoteStyle,
};

fn stable(source: &str, options: &FormatOptions) -> String {
    let result = format_source(source, options).expect("supported lossless CST input");
    assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&result));
    assert!(comments_preserved(source, &result));
    assert_eq!(format_source(&result, options).unwrap(), result);
    result
}
#[test]
fn custom_cst_wraps_free_binary_expressions_without_losing_comments() {
    let mut options = FormatOptions::black();
    options.quote = QuoteStyle::Single; // exercise CST rather than Black engine
    options.max_line_length = 45;
    for source in [
        "answer = first_long_operand + second_long_operand + third_long_operand\n",
        "def answer():\n    return first_long_operand + second_long_operand + third_long_operand\n",
        "if first_long_operand and second_long_operand and third_long_operand:\n    answer = 1\n",
    ] {
        let result = stable(source, &options);
        assert!(result.lines().all(|line| line.len() <= 45), "{result}");
    }
    stable(
        "# important\nanswer = ('# not a comment', 2)  # keep\n# important\n",
        &options,
    );
}
#[test]
fn comment_contract_distinguishes_strings_bodies_and_multiplicity() {
    assert!(comments_preserved(
        "x='# literal' #body\n",
        "x = \"# literal\"  # body\n"
    ));
    assert!(!comments_preserved("# twice\n# twice\n", "# twice\n"));
    assert!(!comments_preserved("# first\n", "# replacement\n"));
}
#[test]
fn protected_regions_eol_and_custom_policies_reach_one_fixed_point() {
    let mut options = FormatOptions::black();
    options.quote = QuoteStyle::Single;
    options.indent = 2;
    options.formatter_tags = Some(FormatterTags {
        off: "format:off".into(),
        on: "format:on".into(),
    });
    let source =
        "# format:off\r\nx= [ 1,2 ]\r\n# format:on\r\ndef f():\r\n    return \"hello\"\r\n";
    let result = stable(source, &options);
    assert!(result.contains("x= [ 1,2 ]\r\n"));
    assert!(!result.replace("\r\n", "").contains('\n'));
}
#[test]
fn changed_line_policy_does_not_shift_original_coordinates() {
    let mut options = FormatOptions::black();
    options.changed_lines = Some(vec![(1, 1)]);
    let result = stable("first= 1\nsecond= 2\nthird= 3\n", &options);
    assert_eq!(result, "first= 1\nsecond = 2\nthird= 3\n");
}

#[test]
fn unselected_growth_and_collapse_leave_later_selection_in_original_coordinates() {
    let whole = FormatOptions::black();
    for (source, selected_line, grows) in [
        ("values= [1,2,3,]\n\nsecond= 2\nthird= 3\n", 2, true),
        (
            "values = [\n    1,\n    2\n]\n\nsecond= 2\nthird= 3\n",
            5,
            false,
        ),
    ] {
        let formatted = format_source(source, &whole).unwrap();
        // Prove that the earlier unselected expression really changes physical
        // line counts during whole-document formatting in this fixture.
        if grows {
            assert!(
                formatted.lines().count() > source.lines().count(),
                "{formatted}"
            );
        } else {
            assert!(
                formatted.lines().count() < source.lines().count(),
                "{formatted}"
            );
        }
        let options = FormatOptions {
            changed_lines: Some(vec![(selected_line, 1)]),
            ..whole.clone()
        };
        let selected = stable(source, &options);
        assert_eq!(selected, source.replace("second= 2", "second = 2"));
    }
}
