//! Source-authored regression contracts. Execution belongs to final verification.
use rustpy_format::{
    comments_preserved, format_source, semantic_fingerprint, FormatOptions, QuoteStyle,
};
use rustpy_settings::python_style::Wrap;

fn custom() -> FormatOptions {
    let mut options = FormatOptions::black();
    options.quote = QuoteStyle::Single;
    options
}
fn stable(source: &str, options: &FormatOptions) -> String {
    let output = format_source(source, options).expect("supported configured CST formatting");
    assert!(semantic_fingerprint(source).is_some());
    assert_eq!(
        semantic_fingerprint(source),
        semantic_fingerprint(&output),
        "{output}"
    );
    assert!(comments_preserved(source, &output));
    assert_eq!(format_source(&output, options).unwrap(), output);
    output
}
fn wrapping(options: &mut FormatOptions, name: &str, mode: &str) {
    options.python.wrapping.insert(
        name.into(),
        Wrap {
            mode: mode.into(),
            ..Default::default()
        },
    );
}

#[test]
fn context_manager_layout_honors_each_delimiter_switch_and_async_items() {
    for asynchronous in [false, true] {
        for open in [false, true] {
            for close in [false, true] {
                for align in [false, true] {
                    let mut options = custom();
                    options.python.wrapping.insert(
                        "with".into(),
                        Wrap {
                            mode: "always".into(),
                            align,
                            newline_after_open: open,
                            closing_on_newline: close,
                        },
                    );
                    let prefix = if asynchronous { "async with" } else { "with" };
                    let source = format!("async def work():\n    {prefix} first('# literal', 1) as a, second(2, 3) as b:\n        consume(a, b)\n");
                    let output = stable(&source, &options);
                    assert!(
                        output
                            .contains(&format!("{prefix} ({}", if open { "\n" } else { "first" })),
                        "{output}"
                    );
                    assert_eq!(output.contains("\n    ):"), close, "{output}");
                    assert!(output.contains("# literal"));
                }
            }
        }
    }
}

#[test]
fn existing_grouped_with_items_can_be_flattened_without_changing_targets() {
    let mut options = custom();
    wrapping(&mut options, "with", "never");
    let output = stable(
        "with (\n    manager(1, 2) as (a, b),\n    another() as c,\n):\n    consume(a, b, c)\n",
        &options,
    );
    assert!(
        output.contains("with manager(1, 2) as (a, b), another() as c:"),
        "{output}"
    );
}

#[test]
fn explicit_continuations_keep_containing_block_and_nested_argument_commas() {
    let mut options = custom();
    options.max_line_length = 45;
    options.python.explicit_continuation = true;
    wrapping(&mut options, "binary", "always");
    let output = stable("def compute():\n    answer = (\n        combine(first, second)\n        + another_long_operand\n    )\n    return answer\n", &options);
    assert!(output.contains(" \\\n        "), "{output}");
    stable(
        "def compute():\n    return (\n        first + second\n    ) * third\n",
        &options,
    );
    stable("values = (\n    first,\n    second,\n)\n", &options);
    stable("values = (\n    item for item in source\n)\n", &options);
}

#[test]
fn multiline_payloads_and_directive_order_are_authoritative() {
    let mut options = custom();
    wrapping(&mut options, "with", "always");
    for source in [
        "with manager('''first\nsecond''') as value:\n    use(value)\n",
        "with manager(f'''first\n{value}''') as value:\n    use(value)\n",
        "# fmt: off\nwith manager( 1), another( 2):\n    use( 3)\n# fmt: on\n",
        "with (\n    first(), # acquire first\n    second(), # acquire second\n):\n    use()\n",
    ] {
        stable(source, &options);
    }
    assert!(!comments_preserved(
        "# first\n# second\n",
        "# second\n# first\n"
    ));
    assert!(comments_preserved(
        "x='# second' # first\n# second\n",
        "x = '# second'  # first\n# second\n"
    ));
}

#[test]
fn configured_operator_splitting_preserves_expression_ownership() {
    let mut options = custom();
    options.max_line_length = 38;
    wrapping(&mut options, "binary", "always");
    wrapping(&mut options, "conditions", "always");
    for expression in [
        "first_operand % second_operand + third_operand",
        "first_operand << second_operand | third_operand",
        "first_operand ** second_operand ** third_operand",
        "first_operand @ second_operand - -third_operand",
        "first_operand is not second_operand and third_operand not in collection",
        "'# literal' + first_operand + second_operand",
    ] {
        stable(&format!("result = {expression}\n"), &options);
    }
    wrapping(&mut options, "binary", "never");
    let output = stable("result = (\n    first + second\n) * third\n", &options);
    assert!(output.contains("(first + second)"), "{output}");
}

#[test]
fn unicode_display_width_drives_custom_line_budget() {
    let mut options = custom();
    options.max_line_length = 32;
    let output = stable("result = ['界界界界界界', '界界界界界界']\n", &options);
    assert!(output.lines().count() > 1, "{output}");
}

#[test]
fn keep_empty_indentation_never_borrows_another_multiline_literals_whitespace() {
    let mut options = custom();
    options.python.indents.keep_empty = true;
    let source =
        "first = \"\"\"\nalpha\n    \nbeta\n\"\"\"\nsecond = \"\"\"\nalpha\n\t\nbeta\n\"\"\"\n";
    let output = stable(source, &options);
    assert!(output.contains("alpha\n    \nbeta"));
    assert!(output.contains("alpha\n\t\nbeta"));
}

#[test]
fn oversized_custom_continuation_is_bounded_for_binary_and_from_import_layout() {
    let mut options = custom();
    options.python.indents.continuation = usize::MAX;
    wrapping(&mut options, "binary", "always");
    wrapping(&mut options, "imports", "always");
    let source = "from package import first, second\nresult = first + second\n";
    let output = stable(source, &options);
    assert!(output.len() < 8192, "unexpected unbounded padding");
    assert!(output.lines().all(|line| line.len() < 4096));
}

#[test]
fn ordinary_comment_coalescing_consumes_whole_fragments_in_order() {
    assert!(comments_preserved(
        "# first\n# second\n# third\n",
        "# first  # second  # third\n"
    ));
    assert!(!comments_preserved(
        "# first\n# second\n",
        "# second  # first\n"
    ));
    assert!(!comments_preserved("# first\n# second\n", "# first\n"));
    assert!(!comments_preserved(
        "# first\n# second\n",
        "# first  # second  # second\n"
    ));
    assert!(
        !comments_preserved("# first  # second\n", "# first\n# second\n"),
        "a hash within one payload cannot be guessed into separate comments"
    );
    assert!(
        !comments_preserved("# first\n# second\n", "# first # second\n"),
        "only the renderer's exact boundary is valid"
    );
}

#[test]
fn directive_fragments_remain_indivisible_comment_barriers() {
    for directive in [
        "# fmt: off",
        "# isort: skip",
        "# type: ignore",
        "# noqa: F401",
        "# pylint: disable=all",
        "# pyright: ignore",
        "# -*- coding: utf-8 -*-",
        "#!/usr/bin/python3",
    ] {
        let source = format!("# explanation\n{directive}\n");
        assert!(comments_preserved(&source, &source));
        assert!(
            !comments_preserved(&source, &format!("# explanation  {directive}\n")),
            "{directive}"
        );
        assert!(
            !comments_preserved(
                &format!("{directive}\n# explanation\n"),
                &format!("{directive}  # explanation\n")
            ),
            "{directive}"
        );
    }
}

#[test]
fn black_function_comment_coalescing_preserves_every_original_fragment() {
    let source = "def convert(  # first\n    value  # second\n):\n    return value\n";
    let output = stable(source, &FormatOptions::black());
    assert_eq!(
        output,
        "def convert(value):  # first  # second\n    return value\n"
    );
}

#[test]
fn docstring_leading_tabs_follow_eight_column_stops_without_expanding_inline_tabs() {
    let source =
        "def document():\n    \"\"\"Heading\n    \t\tstart\tinline\n    base\n    \"\"\"\n";
    let output = stable(source, &FormatOptions::black());
    assert!(
        output.contains(&format!("\n{}start\tinline\n", " ".repeat(16))),
        "{output}"
    );
}
