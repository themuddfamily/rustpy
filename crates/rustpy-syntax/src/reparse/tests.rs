//! Incremental reparsing must be indistinguishable from a full parse: every
//! check below compares the reparse result with `parse(new_text)` (green tree
//! including trivia, the error list, and the debug dumps with ranges).
//!
//! Beyond the default suite, ignored tests run long fuzzing campaigns
//! (`fuzz_many_seeds`, `fuzz_stdlib_corpus`; set `RUSTPY_REPARSE_DUMP=<dir>`
//! to save a failing case for `replay_dumped_case`) and a micro-benchmark
//! (`bench_one_char_edit_in_20k_line_file`). Run them with `--release`.

use std::ops::Range;
use std::time::{Duration, Instant};

use super::*;
use crate::parser::debug_tree;

// ---------------------------------------------------------------------------
// Helpers

fn apply(text: &str, range: Range<usize>, ins: &str) -> String {
    let mut s = String::with_capacity(text.len() - range.len() + ins.len());
    s.push_str(&text[..range.start]);
    s.push_str(ins);
    s.push_str(&text[range.end..]);
    s
}

fn text_range(r: &Range<usize>) -> TextRange {
    TextRange::new(offset(r.start), offset(r.end))
}

/// Panics with a readable report unless `incr` is exactly the full parse of `text`.
#[track_caller]
fn assert_same_as_full(incr: &Parse, text: &str, context: &dyn Fn() -> String) {
    let full = parse(text);
    // `Parse: PartialEq` compares the green trees structurally (kind and text
    // of every node and token, trivia included) and the diagnostics (range,
    // severity, message, code, in order). Equal green trees imply equal debug
    // dumps (which skip trivia); those are compared as well unless the text is
    // large, where rendering them would dominate the test's run time.
    if incr == &full {
        if text.len() <= 32 * 1024 {
            assert_eq!(
                debug_tree(&incr.syntax_node()),
                debug_tree(&full.syntax_node()),
                "{}",
                context()
            );
        }
        return;
    }
    let (a, b) = (
        debug_tree(&incr.syntax_node()),
        debug_tree(&full.syntax_node()),
    );
    let first_diff = a
        .lines()
        .zip(b.lines())
        .position(|(x, y)| x != y)
        .unwrap_or(a.lines().count().min(b.lines().count()));
    let window = |s: &str| {
        s.lines()
            .skip(first_diff.saturating_sub(8))
            .take(16)
            .collect::<Vec<_>>()
            .join("\n")
    };
    panic!(
        "reparse differs from full parse ({})\n--- tree dumps equal: {}, green equal: {}, errors equal: {}\n--- reparse (from line {first_diff}):\n{}\n--- full parse:\n{}\n--- reparse errors: {:?}\n--- full errors:    {:?}",
        context(),
        a == b,
        incr.green() == full.green(),
        incr.errors() == full.errors(),
        window(&a),
        window(&b),
        incr.errors(),
        full.errors(),
    );
}

/// Applies one edit to `old_text`, reparses incrementally, checks the result
/// against a full parse and returns the strategy taken.
#[track_caller]
fn check_edit(old_text: &str, range: Range<usize>, ins: &str) -> ReparseKind {
    let old = parse(old_text);
    let new_text = apply(old_text, range.clone(), ins);
    let (incr, kind) = reparse_with_kind(&old, old_text, text_range(&range), ins, &new_text);
    assert_same_as_full(&incr, &new_text, &|| {
        format!("edit {range:?} -> {ins:?} of {old_text:?}")
    });
    kind
}

/// Like [`check_edit`], at `delta` bytes into the first occurrence of `needle`,
/// deleting `del` bytes.
#[track_caller]
fn check_edit_at(old_text: &str, needle: &str, delta: usize, del: usize, ins: &str) -> ReparseKind {
    let at = old_text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in sample"))
        + delta;
    check_edit(old_text, at..at + del, ins)
}

const SAMPLE: &str = r#"import os


class Greeter:
    """Says hello."""

    def __init__(self, name):
        self.name = name  # who

    def greet(self, loud=False):
        msg = f"hello {self.name}"
        if loud:
            msg = msg.upper()
        else:
            pass
        return msg


def main():
    g = Greeter("world")
    print(g.greet(loud=True), 42)


if __name__ == "__main__":
    main()
"#;

// ---------------------------------------------------------------------------
// Token strategy

#[test]
fn identifier_number_string_comment_and_space_edits_relex_one_token() {
    assert_eq!(
        check_edit_at(SAMPLE, "msg = f", 1, 0, "e"),
        ReparseKind::Token
    );
    assert_eq!(
        check_edit_at(SAMPLE, "msg = f", 0, 3, "message"),
        ReparseKind::Token
    );
    assert_eq!(check_edit_at(SAMPLE, "42)", 1, 1, "7"), ReparseKind::Token);
    assert_eq!(
        check_edit_at(SAMPLE, "\"world\"", 3, 0, " wide "),
        ReparseKind::Token
    );
    assert_eq!(
        check_edit_at(SAMPLE, "# who", 5, 0, "m it may concern"),
        ReparseKind::Token
    );
    assert_eq!(
        check_edit_at(SAMPLE, "name = name", 4, 1, "   "),
        ReparseKind::Token
    );
    assert_eq!(
        check_edit_at(SAMPLE, "Greeter:", 0, 7, "Welcomer"),
        ReparseKind::Token
    );
    assert_eq!(
        check_edit_at(SAMPLE, "import os", 7, 2, "sys"),
        ReparseKind::Token
    );
}

#[test]
fn token_edits_shift_later_errors_and_keep_earlier_ones() {
    let text = "x = $\nvalue = 1\ny = (2,\nz = 3 +\n";
    assert!(!parse(text).ok());
    assert_eq!(
        check_edit_at(text, "value", 5, 0, "s_and_more"),
        ReparseKind::Token
    );
    assert_eq!(check_edit_at(text, "value", 0, 5, "v"), ReparseKind::Token);
}

#[test]
fn edits_that_could_change_neighbouring_tokens_are_not_token_level() {
    // String delimiters, prefixes that merge with the next token, soft keywords,
    // conversion letters, keywords, indentation and line continuations.
    let cases: &[(&str, &str, usize, usize, &str)] = &[
        ("s = 'abc' + 'd'\n", "abc", 1, 0, "'"),
        ("s = 'abc' + 'd'\n", "abc", 1, 0, "\\"),
        ("x = b + 'd'\n", "b +", 1, 1, ""),
        ("x = ab'd'\n", "ab", 1, 0, "c"),
        ("x = 1 + 2\n", "1 +", 1, 0, "e5"),
        ("x = a.b\n", "a.b", 0, 0, "1"),
        ("match x:\n    case y:\n        pass\n", "y:", 0, 1, "_"),
        ("match = 1\nmatc(x)\n", "matc(", 4, 0, "h"),
        ("f'{x!r}'\n", "r}", 0, 1, "s"),
        ("f'{x!q}'\n", "q}", 0, 1, "a"),
        ("typ = 1\n", "typ", 3, 0, "e"),
        ("iff = 1\n", "iff", 2, 1, ""),
        ("if x:\n    y = 1\n", "    y", 0, 0, " "),
        ("x = 1 \\\n  + 2\n", "  + 2", 0, 1, ""),
        ("# c\nx = 1\n", "# c", 0, 0, "y"),
    ];
    for &(text, needle, delta, del, ins) in cases {
        let kind = check_edit_at(text, needle, delta, del, ins);
        assert_ne!(
            kind,
            ReparseKind::Token,
            "{text:?}: {needle:?}+{delta} -{del} +{ins:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Block strategy

#[test]
fn statement_edits_in_a_function_body_reparse_one_run() {
    assert_eq!(
        check_edit_at(SAMPLE, "        return msg", 0, 0, "        msg += '!'\n"),
        ReparseKind::Block
    );
    assert_eq!(
        check_edit_at(SAMPLE, "if loud:", 7, 1, ""),
        ReparseKind::Block
    );
    assert_eq!(
        check_edit_at(SAMPLE, "msg.upper()", 9, 2, ""),
        ReparseKind::Block
    );
    assert_eq!(
        check_edit_at(SAMPLE, "self.name = name", 16, 0, ")"),
        ReparseKind::Block
    );
    assert_eq!(
        check_edit_at(
            SAMPLE,
            "            pass",
            12,
            4,
            "for x in y:\n                yield x"
        ),
        ReparseKind::Block
    );
    assert_eq!(
        check_edit_at(
            SAMPLE,
            "        else:",
            0,
            0,
            "        elif quiet:\n            msg = ''\n"
        ),
        ReparseKind::Block
    );
}

#[test]
fn module_level_edits_reparse_one_run() {
    assert_eq!(
        check_edit_at(SAMPLE, "def main", 0, 0, "x = 1\n\n"),
        ReparseKind::Block
    );
    assert_eq!(
        check_edit_at(SAMPLE, "import os", 9, 0, "\nimport sys as system"),
        ReparseKind::Block
    );
    assert_eq!(
        check_edit_at(SAMPLE, "if __name__", 0, 0, "@decorator\n"),
        ReparseKind::Block
    );
    assert_eq!(
        check_edit_at(SAMPLE, "    main()", 10, 0, "\nelse:\n    pass"),
        ReparseKind::Block
    );
}

#[test]
fn runs_grow_until_their_boundaries_are_independent_of_the_edit() {
    // An unclosed `(` swallows the next line; the run grows to the line
    // where the lexer's recovery closes it (`return`).
    let text = "def f():\n    a = 1\n    b = g\n    c = 2\n    return a\n    d = 4\nz = 3\n";
    assert_eq!(check_edit_at(text, "b = g", 5, 0, "("), ReparseKind::Block);
    // An open f-string replacement field runs to the next `:` (`def main():`).
    assert_eq!(
        check_edit_at(SAMPLE, "return msg", 0, 0, "f'{"),
        ReparseKind::Block
    );
    // `else` continues the `if` before it, so the run starts at the `if`.
    let text = "x = 0\nif a:\n    b\nx = 1\ny = 2\n";
    assert_eq!(
        check_edit_at(text, "x = 1", 0, 5, "else:\n    c"),
        ReparseKind::Block
    );
    let text = "def f():\n    try:\n        a\n    except E:\n        b\n    c = 1\n    d = 2\n";
    assert_eq!(
        check_edit_at(text, "c = 1", 0, 5, "finally:\n        e"),
        ReparseKind::Block
    );
}

#[test]
fn errors_before_inside_and_after_the_run_are_kept_replaced_and_shifted() {
    let text = "a = (1,\nb = 2\n\ndef f():\n    x = $\n    y = 1\n    z = [\n\nc = )\nd = 4 +\n";
    assert!(
        parse(text).errors().len() >= 4,
        "{:?}",
        parse(text).errors()
    );
    check_edit_at(text, "    y = 1", 8, 1, "(2 + 3)");
    check_edit_at(text, "    y = 1", 0, 0, "    if y:\n        pass\n");
    check_edit_at(text, "c = )", 4, 1, "5");
    check_edit_at(text, "b = 2", 4, 1, "[");
}

#[test]
fn indentation_changes_are_reparsed_correctly() {
    for (needle, delta, del, ins) in [
        ("            msg = msg", 0, 4, ""),
        ("            msg = msg", 0, 0, "  "),
        ("        return msg", 0, 8, ""),
        ("        return msg", 0, 0, "\t"),
        ("    def greet", 0, 4, ""),
        ("    main()", 0, 4, ""),
        ("        else:", 4, 0, " "),
    ] {
        check_edit_at(SAMPLE, needle, delta, del, ins);
    }
}

#[test]
fn unbalanced_brackets_and_strings_fall_back_when_needed() {
    // A bracket that stays open until EOF cannot be confined to a run.
    assert_eq!(
        check_edit("x = 1\ny = 2\nz = 3\n", 0..0, "("),
        ReparseKind::Full
    );
    check_edit_at(SAMPLE, "print(g", 6, 0, "[");
    check_edit_at(SAMPLE, "msg = f\"", 7, 1, "");
    check_edit_at(SAMPLE, "    \"\"\"Says", 4, 0, "\"\"\"");
    check_edit_at(SAMPLE, "self.name = name", 0, 0, "f'''");
    check_edit_at(SAMPLE, "Greeter(\"world\")", 8, 0, "\\\n");
}

#[test]
fn line_after_a_backslash_continuation_is_not_a_run_boundary() {
    // `z = 3` is parsed inside the unexpected-indent recovery loop of the
    // module (its line follows a continuation, so the lexer never saw its
    // indentation); a run starting there would lose the closing DEDENT.
    let text = "x = 1\n    y = 2\n\\\nz = 3\nw = 4\n";
    check_edit_at(text, "3\n", 1, 0, ")");
    check_edit_at(text, "z = 3", 0, 1, "zz");
    let text = "if a:\n    x = 1\n    \\\n    y = 2\n    z = 3\n";
    check_edit_at(text, "y = 2", 4, 1, "(");
}

#[test]
fn unexpected_indents_inside_blocks() {
    let text = "def f():\n    x = 1\n        y = 2\n            z = 3\n    w = 4\nv = 5\n";
    for (needle, delta, del, ins) in [
        ("w = 4", 4, 1, "44"),
        ("y = 2", 0, 0, "    "),
        ("w = 4", 0, 0, "    "),
        ("x = 1", 5, 0, "\n      q"),
    ] {
        check_edit_at(text, needle, delta, del, ins);
    }
}

#[test]
fn crlf_tabs_and_form_feeds() {
    let text = "class A:\r\n\tdef f(self):\r\n\t\treturn 1\r\n\r\n\tdef g(self):\r\n\t\tpass\r\n\x0cx = 2\r\n";
    check_edit_at(text, "return 1", 7, 1, "2");
    check_edit_at(text, "\t\tpass", 2, 4, "x = (\r\n\t\t\t1)");
    check_edit_at(text, "\t\tpass", 0, 0, "\t\ty = 0\r\n");
    check_edit_at(text, "\r\n\tdef g", 1, 0, "\r");
    check_edit_at(text, "\t\tpass", 0, 1, "        ");
    check_edit_at(text, "\x0cx = 2", 0, 1, "");
}

#[test]
fn compound_statement_continuations_at_run_boundaries() {
    let text = "try:\n    a()\nexcept E:\n    b()\n\nx = 1\nif c:\n    d()\nelif e:\n    f()\ny = 2\n@deco\ndef g():\n    pass\n";
    for (needle, delta, del, ins) in [
        ("x = 1", 0, 0, "else:\n    pass\n"),
        ("x = 1", 0, 1, "finally"),
        ("y = 2", 0, 1, "else"),
        ("elif e", 0, 4, "else"),
        ("elif e", 0, 4, "if"),
        ("def g", 0, 0, "@other\n"),
        ("def g", 0, 3, "class"),
        ("def g", 0, 0, "async "),
        ("@deco", 5, 0, "\nz = 3"),
        ("except E", 0, 6, "finally"),
    ] {
        check_edit_at(text, needle, delta, del, ins);
    }
}

#[test]
fn soft_keyword_statements() {
    let text = "match cmd:\n    case [x, *rest] if x:\n        go(x)\n    case _:\n        pass\ntype Alias = list[int]\nmatch = 3\n";
    for (needle, delta, del, ins) in [
        ("go(x)", 3, 1, "y"),
        ("case _", 5, 1, "Point(x=0)"),
        ("type Alias", 4, 0, "d"),
        ("match = 3", 5, 0, "(1)"),
        ("match cmd", 5, 0, "es"),
        ("        pass", 8, 4, "match y:\n            case 1: pass"),
    ] {
        check_edit_at(text, needle, delta, del, ins);
    }
}

// ---------------------------------------------------------------------------
// API details

#[test]
fn degenerate_texts() {
    for (text, range, ins) in [
        ("", 0..0, "x"),
        ("", 0..0, "\n"),
        ("", 0..0, "if x:\n    pass\n"),
        ("x", 0..1, ""),
        ("x = 1\n", 0..6, ""),
        ("\n", 0..0, " "),
        ("\n", 1..1, "    y\n"),
        ("   ", 3..3, "z"),
        ("# only a comment", 16..16, "\nx = 1"),
        ("# only a comment\n", 0..1, ""),
        ("\u{e9} = 1\n", 0..2, "\u{e8}"),
        ("x = 1", 5..5, "\n"),
        ("def f():\n    pass", 17..17, "\n    return"),
    ] {
        check_edit(text, range, ins);
    }
}

#[test]
fn no_op_and_identity_edits_reuse_the_old_parse() {
    let old = parse(SAMPLE);
    let at = SAMPLE.find("greet").unwrap();
    let (p, kind) = reparse_with_kind(&old, SAMPLE, TextRange::empty(offset(at)), "", SAMPLE);
    assert_eq!((kind, p.green() == old.green()), (ReparseKind::Token, true));
    let (p, kind) = reparse_with_kind(
        &old,
        SAMPLE,
        TextRange::new(offset(at), offset(at + 5)),
        "greet",
        SAMPLE,
    );
    assert_eq!((kind, p.green() == old.green()), (ReparseKind::Token, true));
}

#[test]
fn inconsistent_arguments_fall_back_to_a_full_parse() {
    let old = parse(SAMPLE);
    let new_text = SAMPLE.replace("hello", "goodbye");
    // Wrong range (lengths do not add up).
    let (p, kind) = reparse_with_kind(
        &old,
        SAMPLE,
        TextRange::new(offset(0), offset(3)),
        "x",
        &new_text,
    );
    assert_eq!(kind, ReparseKind::Full);
    assert_same_as_full(&p, &new_text, &|| "wrong range".into());
    // Range past the end, and an old parse of another text.
    let (_, kind) = reparse_with_kind(
        &old,
        SAMPLE,
        TextRange::new(offset(SAMPLE.len() + 1), offset(SAMPLE.len() + 1)),
        "",
        SAMPLE,
    );
    assert_eq!(kind, ReparseKind::Full);
    let other = parse("x = 1\n");
    let (p, kind) = reparse_with_kind(
        &other,
        SAMPLE,
        TextRange::empty(offset(0)),
        "#",
        &format!("#{SAMPLE}"),
    );
    assert_eq!(kind, ReparseKind::Full);
    assert_same_as_full(&p, &format!("#{SAMPLE}"), &|| "foreign parse".into());
}

#[test]
fn diff_edit_finds_the_minimal_single_edit() {
    assert_eq!(
        diff_edit("abcdef", "abXYef"),
        (TextRange::new(2.into(), 4.into()), "XY")
    );
    assert_eq!(
        diff_edit("abc", "abc"),
        (TextRange::new(3.into(), 3.into()), "")
    );
    assert_eq!(
        diff_edit("aaa", "aaaa"),
        (TextRange::new(3.into(), 3.into()), "a")
    );
    assert_eq!(
        diff_edit("", "x"),
        (TextRange::new(0.into(), 0.into()), "x")
    );
    assert_eq!(diff_edit("x", ""), (TextRange::new(0.into(), 1.into()), ""));
    // Never splits a UTF-8 character: é = C3 A9, è = C3 A8.
    assert_eq!(
        diff_edit("caf\u{e9}!", "caf\u{e8}!"),
        (TextRange::new(3.into(), 5.into()), "\u{e8}")
    );
    assert_eq!(
        diff_edit("\u{e9}\u{e9}", "\u{e9}a\u{e9}"),
        (TextRange::new(2.into(), 2.into()), "a")
    );
}

#[test]
fn reparse_text_derives_the_edit() {
    let old = parse(SAMPLE);
    let new_text = SAMPLE.replace("loud=False", "loud=True, times=2");
    let (p, kind) = reparse_text(&old, SAMPLE, &new_text);
    assert_ne!(kind, ReparseKind::Full);
    assert_same_as_full(&p, &new_text, &|| "reparse_text".into());
}

#[test]
fn stats_count_each_strategy() {
    let before = reparse_stats();
    check_edit_at(SAMPLE, "msg = f", 1, 0, "e");
    check_edit_at(SAMPLE, "def main", 0, 0, "x = 1\n");
    check_edit("x = 1\ny = 2\n", 0..0, "(");
    let after = reparse_stats();
    assert!(
        after.token > before.token && after.block > before.block && after.full > before.full,
        "{before:?} -> {after:?}"
    );
}

// ---------------------------------------------------------------------------
// Property tests: pseudo-random edit sequences checked against full parses

/// SplitMix64: small, deterministic, good enough for test inputs.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const CHARS: &[&str] = &[
    "a", "e", "x", "_", "1", "0", " ", " ", " ", "\t", "\n", "\n", "\n", "\r\n", "\r", ":", ":",
    "(", ")", "[", "]", "{", "}", "'", "\"", "#", "\\", ",", ".", "=", "@", "+", "-", "*", "f",
    "r", "b", ";", "!", "$", "\u{e9}", "\x0c",
];

const SNIPPETS: &[&str] = &[
    "foo",
    "x1",
    "match",
    "case",
    "type",
    "_",
    "r",
    "async",
    "await",
    "lambda: 0",
    "if x:",
    "elif y:",
    "else:",
    "try:",
    "except E as e:",
    "finally:",
    "while 1:",
    "for i in r:",
    "with a as b:",
    "def f(a, b=1):",
    "class C(B):",
    "return",
    "pass",
    "yield x",
    "@deco",
    "global g",
    "del z",
    "\n    ",
    "\n        ",
    "\n",
    "    ",
    "\t",
    "):",
    "(",
    "[",
    "]",
    "{",
    "}",
    "'''",
    "\"\"\"",
    "'",
    "\"",
    "f'{x}'",
    "f\"{a!r:>{w}}\"",
    "f'{",
    "}",
    "\\\n",
    "# note",
    "x = (1,\n     2)",
    "\n@d\ndef g():\n    pass\n",
    "match p:\n    case [1, *r]:\n        pass\n",
    "type T = int",
    "x: int = 0",
    "print(*args, **kw)",
    "lambda",
    "0x1F",
    "1.5e3",
    "2j",
    "    if y:\n        pass\n",
    "else:\n    pass\n",
    "except:\n    pass\n",
    "\n\n",
    "t'{v}'",
    "rb'\\x00'",
    "...",
    "->",
    ":=",
    "not in",
    "is not",
];

fn snap_left(text: &str, mut pos: usize) -> usize {
    while !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

/// Advances `n` characters from `pos` (clamped to the end).
fn chars_after(text: &str, pos: usize, n: usize) -> usize {
    text[pos..]
        .char_indices()
        .nth(n)
        .map_or(text.len(), |(i, _)| pos + i)
}

fn line_bounds(text: &str, pos: usize) -> (usize, usize) {
    let start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    let end = text[pos..].find('\n').map_or(text.len(), |i| pos + i + 1);
    (start, end)
}

/// A random position, biased towards line starts, indentation ends and line ends.
fn random_pos(rng: &mut Rng, text: &str) -> usize {
    let pos = snap_left(text, rng.below(text.len() + 1));
    let (ls, le) = line_bounds(text, pos);
    match rng.below(8) {
        0 => ls,
        1 => {
            ls + text[ls..le]
                .bytes()
                .take_while(|b| matches!(b, b' ' | b'\t'))
                .count()
        }
        2 => snap_left(
            text,
            if le > ls && text[..le].ends_with('\n') {
                le - 1
            } else {
                le
            },
        ),
        _ => pos,
    }
}

/// The end of the line `n` lines after the one ending at `le`.
fn lines_end(text: &str, mut le: usize, n: usize) -> usize {
    for _ in 0..n {
        le = line_bounds(text, le).1;
    }
    le
}

/// One random edit of `text`: (range, inserted).
fn random_edit(rng: &mut Rng, text: &str) -> (Range<usize>, String) {
    let pos = random_pos(rng, text);
    let (ls, le) = line_bounds(text, pos);
    match rng.below(16) {
        // Indent or dedent a few lines (Tab / Shift+Tab on a selection).
        14 => {
            let end = lines_end(text, le, rng.below(5));
            let indent = rng.chance(50);
            let lines: String = text[ls..end]
                .split_inclusive('\n')
                .map(|l| {
                    if indent {
                        format!("    {l}")
                    } else {
                        l.strip_prefix("    ")
                            .or_else(|| l.strip_prefix('\t'))
                            .unwrap_or(l)
                            .to_string()
                    }
                })
                .collect();
            return (ls..end, lines);
        }
        // Paste a copy of a few lines from elsewhere at this line's start.
        15 => {
            let from = line_bounds(text, random_pos(rng, text)).0;
            let to = lines_end(text, line_bounds(text, from).1, rng.below(6));
            return (ls..ls, text[from..to].to_string());
        }
        _ => {}
    }
    match rng.below(14) {
        0..=2 => (pos..pos, rng.pick(CHARS).to_string()),
        3..=5 => (pos..pos, rng.pick(SNIPPETS).to_string()),
        6..=7 => (pos..chars_after(text, pos, 1 + rng.below(3)), String::new()),
        8 => (
            pos..chars_after(text, pos, 1 + rng.below(40)),
            String::new(),
        ),
        9 => (ls..le, String::new()),
        10 => (ls..ls, text[ls..le].to_string()),
        11 => (ls..ls, rng.pick(&["    ", "  ", " ", "\t"]).to_string()),
        12 => {
            let ws = text[ls..le]
                .bytes()
                .take_while(|b| matches!(b, b' ' | b'\t'))
                .count();
            (ls..ls + ws.min(1 + rng.below(4)), String::new())
        }
        _ => (
            pos..chars_after(text, pos, 1 + rng.below(12)),
            rng.pick(SNIPPETS).to_string(),
        ),
    }
}

/// Applies `steps` random edits (with occasional undos and resets) starting
/// from `original`, checking every reparse; returns counts per strategy.
fn fuzz(name: &str, original: &str, seed: u64, steps: usize) -> [usize; 3] {
    let mut rng = Rng(seed);
    let mut text = original.to_string();
    let mut current = parse(&text);
    let mut undo: Option<(Range<usize>, String)> = None;
    let mut counts = [0usize; 3];
    for step in 0..steps {
        if step % 64 == 63 || text.len() > original.len() * 2 + 4096 {
            text = original.to_string();
            current = parse(&text);
            undo = None;
        }
        let (range, ins) = match undo.take() {
            Some(u) if rng.chance(20) => u,
            _ => random_edit(&mut rng, &text),
        };
        let new_text = apply(&text, range.clone(), &ins);
        let (incr, kind) = reparse_with_kind(&current, &text, text_range(&range), &ins, &new_text);
        if let Ok(dir) = std::env::var("RUSTPY_REPARSE_DUMP") {
            // Debugging aid: save the first failing case for `replay_dumped_case`.
            if incr != parse(&new_text) {
                std::fs::write(format!("{dir}/old.py"), &text).unwrap();
                std::fs::write(
                    format!("{dir}/edit.txt"),
                    format!("{} {}\n{ins}", range.start, range.end),
                )
                .unwrap();
            }
        }
        assert_same_as_full(&incr, &new_text, &|| {
            let (lo, hi) = (
                snap_left(&text, range.start.saturating_sub(120)),
                snap_left(&text, (range.end + 120).min(text.len())),
            );
            format!("{name}, seed {seed}, step {step}, kind {kind:?}: edit {range:?} -> {ins:?}\nold text around the edit:\n{}", &text[lo..hi])
        });
        counts[kind as usize] += 1;
        undo = Some((
            range.start..range.start + ins.len(),
            text[range].to_string(),
        ));
        text = new_text;
        current = incr;
    }
    counts
}

/// Replays a case saved by `fuzz` (see `RUSTPY_REPARSE_DUMP`).
#[test]
#[ignore]
fn replay_dumped_case() {
    let dir = std::env::var("RUSTPY_REPARSE_DUMP").expect("set RUSTPY_REPARSE_DUMP");
    let text = std::fs::read_to_string(format!("{dir}/old.py")).unwrap();
    let edit = std::fs::read_to_string(format!("{dir}/edit.txt")).unwrap();
    let (header, ins) = edit.split_once('\n').unwrap();
    let (s, e) = header.split_once(' ').unwrap();
    let kind = check_edit(&text, s.parse().unwrap()..e.parse().unwrap(), ins);
    eprintln!("replayed: {kind:?}");
}

fn stdlib_samples() -> Vec<(String, String)> {
    let dirs = [
        "/usr/lib/python3.12",
        "/usr/lib/python3.13",
        "/usr/lib/python3.11",
    ];
    let Some(dir) = dirs.iter().find(|d| std::path::Path::new(d).is_dir()) else {
        return Vec::new();
    };
    ["textwrap.py", "json/decoder.py", "dataclasses.py"]
        .iter()
        .filter_map(|f| {
            std::fs::read_to_string(format!("{dir}/{f}"))
                .ok()
                .map(|text| (f.to_string(), text))
        })
        .collect()
}

/// Inline fixtures covering the lexer's and parser's corner cases.
const FIXTURES: &[(&str, &str)] = &[
    ("sample", SAMPLE),
    (
        "compound",
        "import a.b as c, d\nfrom ..pkg import (x as y,\n    z)\n\n@dataclass(frozen=True)\nclass Point(Base, metaclass=Meta):\n    x: int = 0\n    y: int = 0\n\n    @property\n    def norm(self) -> float:\n        return (self.x ** 2 + self.y ** 2) ** 0.5\n\n    async def fetch(self, *args, key=None, **kw):\n        async with session() as s:\n            async for item in s:\n                await item\n        return [i for i in range(10) if i % 2]\n\n\ndef gen(n):\n    while n > 0:\n        yield n\n        n -= 1\n    else:\n        pass\n    try:\n        risky()\n    except (ValueError, TypeError) as e:\n        raise RuntimeError() from e\n    except* OSError:\n        pass\n    else:\n        ok()\n    finally:\n        done()\n    for i, j in pairs:\n        if i:\n            continue\n        elif j:\n            break\n    else:\n        global g\n    lam = lambda a, b=2: a + b\n    return {k: v for k, v in d.items()}, {*s}, (x := 3)\n",
    ),
    (
        "strings",
        "a = 'single' \"double\" r'raw\\d' b'bytes' u'uni'\nb = '''triple\nline'''\nc = f\"value {x!r:>{width}} and {y=} {{literal}}\"\nd = f'''multi\n{a +\n b}\nend'''\ne = f'{f\"{nested}\"}'\nf = t'{tmpl}'\ng = \"\\N{BULLET} {x}\"\nh = (\n    'implicit'\n    'concat'\n)\n# comment with 'quotes' and \"\"\"\n",
    ),
    (
        "errors",
        "def broken(:\n    pass\nx = (1, 2\ny = 3\nif x\n    y\nclass\n    z = $\n  w = 1\nreturn ]\nwhile True:\npass\ntry:\n    a\nb = 1\n@deco\nc = 2\nasync x\nlambda\n",
    ),
    (
        "match_and_types",
        "match command.split():\n    case [action]:\n        run(action)\n    case [\"go\", direction] if direction in exits:\n        go(direction)\n    case Point(x=0, y=0) | {\"k\": _, **rest}:\n        pass\n    case _:\n        unknown()\ntype Pair[T: int, *Ts, **P] = tuple[T, T]\nmatch = case = type = 1\nprint(match, case, type)\n",
    ),
    ("tabs_crlf", "class A:\r\n\tdef f(self):\r\n\t\tif self:\r\n\t\t\treturn 1\r\n\t\treturn 2\r\n\r\n\tdef g(self):  # c\r\n\t\tpass\r\n\x0cx = 2\r\nif x:\r\n        y = 3\r\n\telse_ = 4\r\n"),
    (
        "continuations",
        "x = 1 + \\\n    2\nif a and \\\n   b:\n    pass\n\\\ny = [\n    1,\n    2,\n]\nz = foo(a,\n        b)(c)[d]\n    \\\nw = 4\nv = {\n  'k': (1,\n        2),\n}\n",
    ),
    (
        "deep_nesting",
        "def a():\n    def b():\n        def c():\n            if x:\n                for y in z:\n                    while w:\n                        with v:\n                            try:\n                                pass\n                            except:\n                                pass\n                        q = 1\n                    r = 2\n            s = 3\n        t = 4\n    u = 5\nv = 6\n",
    ),
];

#[test]
fn random_edits_on_fixtures_match_full_parse() {
    let mut totals = [0usize; 3];
    for (i, (name, text)) in FIXTURES.iter().enumerate() {
        let counts = fuzz(name, text, 0x5EED_0000 + i as u64, 400);
        for (t, c) in totals.iter_mut().zip(counts) {
            *t += c;
        }
    }
    eprintln!("fixtures: token/block/full = {totals:?}");
    let incremental = totals[0] + totals[1];
    // Random edits often leave brackets unbalanced or lines over-indented, and
    // then every reparse is a full one (the lexer's recovery for those looks at
    // the whole file); this test is about exactness.
    assert!(
        incremental * 10 > totals.iter().sum::<usize>(),
        "too few incremental reparses: {totals:?}"
    );
}

/// Fuzzes one file of the local CPython standard library (skipped if absent).
fn fuzz_stdlib_file(file: &str, seed: u64, steps: usize) {
    let Some((name, text)) = stdlib_samples().into_iter().find(|(name, _)| name == file) else {
        eprintln!("{file}: no CPython stdlib found; skipping");
        return;
    };
    let counts = fuzz(&name, &text, seed, steps);
    eprintln!("{name}: token/block/full = {counts:?}");
    // Random edits often leave a bracket open; with the lexer's whole-file
    // bracket recovery those fall back to full parses.
    assert!(
        (counts[0] + counts[1]) * 20 > steps,
        "too few incremental reparses: {counts:?}"
    );
}

#[test]
fn random_edits_on_stdlib_textwrap_match_full_parse() {
    fuzz_stdlib_file("textwrap.py", 0xF11E_0001, 400);
}

#[test]
fn random_edits_on_stdlib_json_decoder_match_full_parse() {
    fuzz_stdlib_file("json/decoder.py", 0xF11E_0002, 400);
}

#[test]
fn random_edits_on_stdlib_dataclasses_match_full_parse() {
    fuzz_stdlib_file("dataclasses.py", 0xF11E_0003, 250);
}

/// Small programs around the constructs whose boundaries are delicate:
/// clause continuations, decorators, `async`, soft keywords, continuations,
/// nested and unexpected indentation, blank/comment lines, CRLF and tabs.
const SMALL_PROGRAMS: &[&str] = &[
    "if a:\n    b\nelif c:\n    d\nelse:\n    e\nx\n",
    "try:\n    a\nexcept E:\n    b\n\n# c\nfinally:\n    c\ny = 1\n",
    "@d\ndef f():\n    pass\n@e\nasync def g():\n    return\nclass C: pass\n",
    "def f():\n    x = (1,\n         2)\n    if y:\n        z\n    return w\nv\n",
    "match x:\n    case 1:\n        pass\n    case _:\n        y\ntype T = int\n",
    "x = 1 + \\\n    2\n\\\ny = f'{a}' \"b\"\nz\n",
    "for i in r:\n    if i:\n        continue\nelse:\n    done()\nwhile 0:\n    pass\n",
    "def a():\n    def b():\n        c\n\n    d\ne\n",
    "def f():\n    x\n        y\n            z\n    w\nv\n",
    "class A:\r\n\tdef f(self):\r\n\t\treturn 1\r\n\r\n\tx = 2\r\ny\r\n",
    "def f(:\n    x = $\n  y\nz = (\nw\n",
    "if a:\n    @d\n\nb\nwith c as d: e\n",
    "x = (1,\ndef f():\n    pass\ny = [2,\n@d\nclass C: pass\n",
    "def f():\n    try:\n        a\n\nb = 1\nclass A:\n    def g(self):\n        return 1",
    "while x:\n    if y: pass\n    else: z\nfor a in b: c\nelse: d",
];

/// Types a snippet into a function body one character at a time, then deletes
/// it again one character at a time, like an editor would.
#[test]
fn typing_and_backspacing_character_by_character_match_full_parse() {
    let snippet = "if x and (y or z):\n            return [i for i in y if i]  # done\n        elif f'{x!r}':\n            pass\n        else:\n            raise E() from None\n        ";
    let mut text = SAMPLE.to_string();
    let mut current = parse(&text);
    let mut at = text.find("return msg").unwrap();
    let mut counts = [0usize; 3];
    for c in snippet.chars() {
        let ins = c.to_string();
        let new_text = apply(&text, at..at, &ins);
        let (incr, kind) =
            reparse_with_kind(&current, &text, text_range(&(at..at)), &ins, &new_text);
        assert_same_as_full(&incr, &new_text, &|| format!("typing {ins:?} at {at}"));
        counts[kind as usize] += 1;
        (text, current, at) = (new_text, incr, at + ins.len());
    }
    while text != SAMPLE {
        let start = snap_left(&text, at - 1);
        let new_text = apply(&text, start..at, "");
        let (incr, kind) =
            reparse_with_kind(&current, &text, text_range(&(start..at)), "", &new_text);
        assert_same_as_full(&incr, &new_text, &|| format!("backspace at {at}"));
        counts[kind as usize] += 1;
        (text, current, at) = (new_text, incr, start);
    }
    eprintln!("typing: token/block/full = {counts:?}");
    // Typed one character at a time (no auto-indent, no auto-closed brackets),
    // the text passes through states with an open bracket or an over-indented
    // line; those need a full parse because the lexer's recovery for them looks
    // at the whole file. Most keystrokes are still incremental.
    assert!(
        counts[2] * 2 <= counts.iter().sum::<usize>(),
        "typing in a function body should mostly be incremental: {counts:?}"
    );
}

/// Every insertion from a set of characters and keywords at every position,
/// and every deletion of up to four characters, on [`SMALL_PROGRAMS`].
#[test]
fn exhaustive_small_edits_on_small_programs_match_full_parse() {
    const INSERTS: &[&str] = &[
        "a", " ", "\t", "\n", "\r", ":", "(", ")", "'", "\"", "#", "\\", "@", ",", "=", "\x0c",
        "else", "elif x", "except", "finally", "def", "async ", "    ", "\n    ", "if x:", "pass",
        "match", "case", "_", "type", "\"\"\"", "f'{",
    ];
    let mut counts = [0usize; 3];
    for program in SMALL_PROGRAMS {
        let old = parse(program);
        let positions: Vec<usize> = (0..=program.len())
            .filter(|&i| program.is_char_boundary(i))
            .collect();
        let mut edits: Vec<(Range<usize>, &str)> = Vec::new();
        for &at in &positions {
            edits.extend(INSERTS.iter().map(|ins| (at..at, *ins)));
            edits.extend(
                positions
                    .iter()
                    .filter(|&&end| end > at && end <= at + 4)
                    .map(|&end| (at..end, "")),
            );
        }
        for (range, ins) in edits {
            let new_text = apply(program, range.clone(), ins);
            let (incr, kind) = reparse_with_kind(&old, program, text_range(&range), ins, &new_text);
            assert_same_as_full(&incr, &new_text, &|| {
                format!("edit {range:?} -> {ins:?} of {program:?}")
            });
            counts[kind as usize] += 1;
        }
    }
    eprintln!("exhaustive: token/block/full = {counts:?}");
    assert!(counts[0] > 0 && counts[1] > counts[2], "{counts:?}");
}

/// Random edits on every module of the local CPython standard library:
/// `cargo test -p rustpy-syntax --release -- --ignored fuzz_stdlib_corpus`
/// (`RUSTPY_FUZZ_STEPS` sets the edits per file, default 40).
#[test]
#[ignore]
fn fuzz_stdlib_corpus() {
    let steps: usize = std::env::var("RUSTPY_FUZZ_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let Some(dir) = [
        "/usr/lib/python3.12",
        "/usr/lib/python3.13",
        "/usr/lib/python3.11",
    ]
    .into_iter()
    .find(|d| std::path::Path::new(d).is_dir()) else {
        return;
    };
    let mut files = Vec::new();
    let mut dirs = vec![std::path::PathBuf::from(dir)];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() && !p.ends_with("site-packages") && !p.ends_with("__pycache__") {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "py") {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut totals = [0usize; 3];
    for (i, f) in files.iter().enumerate() {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        if text.len() > 256 * 1024 {
            continue;
        }
        let counts = fuzz(&f.display().to_string(), &text, 0xC0FFEE ^ i as u64, steps);
        for (t, c) in totals.iter_mut().zip(counts) {
            *t += c;
        }
    }
    eprintln!(
        "stdlib corpus: {} files, token/block/full = {totals:?}",
        files.len()
    );
}

/// Long-running variant: `cargo test -p rustpy-syntax --release -- --ignored fuzz_many_seeds`
/// (`RUSTPY_FUZZ_SEEDS` sets the number of seeds per input, default 50, and
/// `RUSTPY_FUZZ_SEED_BASE` the first one, default 0).
#[test]
#[ignore]
fn fuzz_many_seeds() {
    let env = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    };
    let (base, seeds) = (
        env("RUSTPY_FUZZ_SEED_BASE", 0),
        env("RUSTPY_FUZZ_SEEDS", 50),
    );
    let inputs: Vec<(String, String)> = FIXTURES
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .chain(stdlib_samples())
        .collect();
    let mut totals = [0usize; 3];
    for (i, (name, text)) in inputs.iter().enumerate() {
        for seed in base..base + seeds {
            let counts = fuzz(name, text, (i as u64) << 32 | seed, 500);
            for (t, c) in totals.iter_mut().zip(counts) {
                *t += c;
            }
        }
    }
    eprintln!("fuzz: token/block/full = {totals:?}");
}

// ---------------------------------------------------------------------------
// Micro-benchmark

/// A synthetic module of about `lines` lines (classes, methods, functions).
fn big_module(lines: usize) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.bytes().filter(|&b| b == b'\n').count() < lines {
        s.push_str(&format!(
            "class Widget{i}(Base):\n    \"\"\"Widget number {i}.\"\"\"\n\n    def __init__(self, value={i}):\n        self.value = value\n        self.items = [x * 2 for x in range(value)]\n\n    def compute(self, factor):\n        total = 0\n        for item in self.items:\n            if item % 3 == 0:\n                total += item * factor\n            elif item % 5 == 0:\n                total -= item\n            else:\n                total += 1\n        return total\n\n\ndef helper_{i}(a, b=2, *args, **kwargs):\n    return f\"{{a}}-{{b}}\" + str(len(args))\n\n\n"
        ));
        i += 1;
    }
    s
}

fn median_time(runs: usize, mut f: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    times.sort();
    times[runs / 2]
}

/// Prints the timings of one-character edits in the middle of a 20k-line
/// module, reparsed incrementally vs. parsed from scratch. Run optimized:
/// `cargo test -p rustpy-syntax --release -- --ignored --nocapture bench_one_char_edit`
#[test]
#[ignore]
fn bench_one_char_edit_in_20k_line_file() {
    let text = big_module(20_000);
    let old = parse(&text);
    assert!(old.ok(), "{:?}", &old.errors()[..old.errors().len().min(3)]);
    let full = median_time(7, || {
        std::hint::black_box(parse(&text));
    });
    let middle = text.len() / 2;
    let edits = [
        (
            "rename an identifier (one char)",
            "total += item * factor",
            5,
            0,
            "s",
            ReparseKind::Token,
        ),
        (
            "delete a ':' (one char)",
            "            else:",
            16,
            1,
            "",
            ReparseKind::Block,
        ),
        (
            "type a '(' (one char)",
            "        return total",
            15,
            0,
            "(",
            ReparseKind::Block,
        ),
        (
            "insert a newline (one char)",
            "self.value = value",
            10,
            0,
            "\n",
            ReparseKind::Block,
        ),
    ];
    for (what, needle, delta, del, ins, expected) in edits {
        let at = middle + text[middle..].find(needle).unwrap() + delta;
        let range = at..at + del;
        let new_text = apply(&text, range.clone(), ins);
        let (incr, kind) = reparse_with_kind(&old, &text, text_range(&range), ins, &new_text);
        assert_same_as_full(&incr, &new_text, &|| what.to_string());
        assert_eq!(kind, expected, "{what}");
        let t = median_time(51, || {
            std::hint::black_box(reparse(&old, &text, text_range(&range), ins, &new_text));
        });
        let speedup = full.as_secs_f64() / t.as_secs_f64();
        eprintln!("{} lines, {} bytes; {what}: {kind:?} reparse {t:?} vs full parse {full:?} ({speedup:.0}x faster)", text.lines().count(), text.len());
        assert!(speedup > 20.0, "{what}: incremental {t:?} vs full {full:?}");
    }
}
