//! Token-based highlighting of `.feature` files.

use std::ops::Range;

use crate::lexer::{scan, Tok};
use crate::model::BlockKind;

/// `Feature:`, `Scenario:`, step keywords, ...
pub const KEYWORD: &str = "Keyword";
/// Doc strings, quoted step arguments, table cells.
pub const STRING: &str = "String";
/// `# ...` comments and the `# language:` header.
pub const COMMENT: &str = "Comment";
/// `@tags`.
pub const TAG: &str = "Tag";
/// `<placeholders>` of scenario outlines.
pub const VARIABLE: &str = "Variable";
/// Table header cells.
pub const KEY: &str = "Key";
/// Numbers in steps and table cells.
pub const NUMBER: &str = "Number";

/// Every tag name [`highlight`] can produce.
pub const TAG_NAMES: &[&str] = &[KEYWORD, STRING, COMMENT, TAG, VARIABLE, KEY, NUMBER];

/// Highlights a `.feature` file. Spans are sorted, non-overlapping, non-empty
/// and on char boundaries; tag names are the constants of this module
/// (`"Keyword"`, `"String"`, `"Comment"`, `"Tag"`, `"Variable"`, `"Key"`,
/// `"Number"`). Text without a span (names, descriptions) is plain.
pub fn highlight(src: &str) -> Vec<(Range<usize>, &'static str)> {
    let scanned = scan(src);
    let mut out: Vec<(Range<usize>, &'static str)> = Vec::new();
    for l in &scanned.lines {
        match &l.tok {
            Tok::Empty | Tok::Description | Tok::Other => {}
            Tok::Comment | Tok::Language { .. } => push(&mut out, l.indent..l.trim_end, COMMENT),
            Tok::Tags { tags, comment } => {
                for t in tags {
                    push(&mut out, t.clone(), TAG);
                }
                if let Some(c) = comment {
                    push(&mut out, c.clone(), COMMENT);
                }
            }
            Tok::Block {
                kind,
                keyword,
                name,
            } => {
                push(&mut out, keyword.clone(), KEYWORD);
                if l.outline && matches!(kind, BlockKind::Scenario | BlockKind::ScenarioOutline) {
                    placeholders_only(src, name.clone(), &mut out);
                }
            }
            Tok::Step { keyword, text, .. } => {
                push(&mut out, keyword.clone(), KEYWORD);
                step_text(src, text.clone(), l.outline, &mut out);
            }
            Tok::DocOpen {
                delimiter, media, ..
            } => {
                let end = if media.is_empty() {
                    delimiter.end
                } else {
                    media.end
                };
                push(&mut out, delimiter.start..end, STRING);
            }
            Tok::DocClose { delimiter } => push(&mut out, delimiter.clone(), STRING),
            Tok::DocLine => {
                if l.indent < l.trim_end {
                    with_placeholders(src, l.indent..l.trim_end, STRING, l.outline, &mut out);
                }
            }
            Tok::Row {
                cells,
                header,
                examples,
            } => {
                for c in cells {
                    let cell = &src[c.clone()];
                    if *header {
                        with_placeholders(src, c.clone(), KEY, l.outline && !examples, &mut out);
                    } else if is_number(cell) {
                        push(&mut out, c.clone(), NUMBER);
                    } else {
                        with_placeholders(src, c.clone(), STRING, l.outline && !examples, &mut out);
                    }
                }
            }
        }
    }
    finish(src, out)
}

fn push(out: &mut Vec<(Range<usize>, &'static str)>, r: Range<usize>, tag: &'static str) {
    if r.start < r.end {
        out.push((r, tag));
    }
}

/// Finds a `<placeholder>` starting at `i` (which must be `<`); returns the
/// end offset (after `>`).
fn placeholder_end(bytes: &[u8], i: usize, end: usize) -> Option<usize> {
    if bytes.get(i) != Some(&b'<') {
        return None;
    }
    let mut j = i + 1;
    while j < end {
        match bytes[j] {
            b'>' if j > i + 1 => return Some(j + 1),
            b'<' | b'>' => return None,
            _ => j += 1,
        }
    }
    None
}

fn placeholders_only(src: &str, r: Range<usize>, out: &mut Vec<(Range<usize>, &'static str)>) {
    let bytes = src.as_bytes();
    let mut i = r.start;
    while i < r.end {
        if let Some(e) = placeholder_end(bytes, i, r.end) {
            push(out, i..e, VARIABLE);
            i = e;
        } else {
            i += 1;
        }
    }
}

/// Spans `r` with `tag`, carving out `<placeholders>` as variables.
fn with_placeholders(
    src: &str,
    r: Range<usize>,
    tag: &'static str,
    outline: bool,
    out: &mut Vec<(Range<usize>, &'static str)>,
) {
    if !outline {
        push(out, r, tag);
        return;
    }
    let bytes = src.as_bytes();
    let mut seg_start = r.start;
    let mut i = r.start;
    while i < r.end {
        if let Some(e) = placeholder_end(bytes, i, r.end) {
            push(out, seg_start..i, tag);
            push(out, i..e, VARIABLE);
            i = e;
            seg_start = e;
        } else {
            i += 1;
        }
    }
    push(out, seg_start..r.end, tag);
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Step text: `"quoted"` arguments, numbers and (in outlines) placeholders.
fn step_text(
    src: &str,
    r: Range<usize>,
    outline: bool,
    out: &mut Vec<(Range<usize>, &'static str)>,
) {
    let bytes = src.as_bytes();
    let mut i = r.start;
    while i < r.end {
        let b = bytes[i];
        if outline {
            if let Some(e) = placeholder_end(bytes, i, r.end) {
                push(out, i..e, VARIABLE);
                i = e;
                continue;
            }
        }
        if b == b'"' {
            if let Some(close) = bytes[i + 1..r.end]
                .iter()
                .position(|&c| c == b'"')
                .map(|p| i + 1 + p)
            {
                with_placeholders(src, i..close + 1, STRING, outline, out);
                i = close + 1;
                continue;
            }
        }
        let starts_number = b.is_ascii_digit()
            || (b == b'-' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) && i + 1 < r.end);
        let boundary_before = i == r.start || !is_word_byte(bytes[i - 1]) && bytes[i - 1] != b'.';
        if starts_number && boundary_before {
            let mut j = i + 1;
            while j < r.end && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j + 1 < r.end && bytes[j] == b'.' && bytes[j + 1].is_ascii_digit() {
                j += 1;
                while j < r.end && bytes[j].is_ascii_digit() {
                    j += 1;
                }
            }
            if j >= r.end || !is_word_byte(bytes[j]) {
                push(out, i..j, NUMBER);
                i = j;
                continue;
            }
            // Part of a word (`3rd`, `v2x`): skip the whole word.
            while j < r.end && is_word_byte(bytes[j]) {
                j += 1;
            }
            i = j;
            continue;
        }
        if is_word_byte(b) {
            // Skip words so digits inside them (`item42`) are not numbers.
            let mut j = i + 1;
            while j < r.end && is_word_byte(bytes[j]) {
                j += 1;
            }
            i = j;
            continue;
        }
        i += 1;
    }
}

/// Integer or decimal, optionally signed.
pub(crate) fn is_number(s: &str) -> bool {
    let t = s.strip_prefix(['-', '+']).unwrap_or(s);
    let mut parts = t.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next();
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    match frac {
        None => digits(int),
        Some(f) => (int.is_empty() || digits(int)) && digits(f),
    }
}

fn finish(
    src: &str,
    mut spans: Vec<(Range<usize>, &'static str)>,
) -> Vec<(Range<usize>, &'static str)> {
    spans.retain(|(r, _)| {
        r.start < r.end
            && r.end <= src.len()
            && src.is_char_boundary(r.start)
            && src.is_char_boundary(r.end)
    });
    spans.sort_by_key(|(r, _)| (r.start, r.end));
    let mut out: Vec<(Range<usize>, &'static str)> = Vec::with_capacity(spans.len());
    let mut last_end = 0;
    for (r, t) in spans {
        if r.start >= last_end {
            last_end = r.end;
            out.push((r, t));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tagged(src: &str) -> Vec<(&str, &'static str)> {
        highlight(src)
            .into_iter()
            .map(|(r, t)| (&src[r], t))
            .collect()
    }

    #[test]
    fn keywords_tags_comments() {
        let src = "# a comment\n@smoke @ui #why\nFeature: Login\n  Background:\n    Given a user\n  Scenario: ok\n    When I log in\n    Then I see 3 items\n";
        assert_eq!(
            tagged(src),
            vec![
                ("# a comment", COMMENT),
                ("@smoke", TAG),
                ("@ui", TAG),
                ("#why", COMMENT),
                ("Feature:", KEYWORD),
                ("Background:", KEYWORD),
                ("Given", KEYWORD),
                ("Scenario:", KEYWORD),
                ("When", KEYWORD),
                ("Then", KEYWORD),
                ("3", NUMBER),
            ]
        );
    }

    #[test]
    fn outline_placeholders_tables_and_doc_strings() {
        let src = "Feature: F\n  Scenario Outline: eat <n>\n    Given I have <n> \"<kind>\" apples\n      \"\"\"json\n      {\"n\": <n>}\n      \"\"\"\n    Examples: Some\n      | n  | kind |\n      | 12 | red  |\n";
        assert_eq!(
            tagged(src),
            vec![
                ("Feature:", KEYWORD),
                ("Scenario Outline:", KEYWORD),
                ("<n>", VARIABLE),
                ("Given", KEYWORD),
                ("<n>", VARIABLE),
                ("\"", STRING),
                ("<kind>", VARIABLE),
                ("\"", STRING),
                ("\"\"\"json", STRING),
                ("{\"n\": ", STRING),
                ("<n>", VARIABLE),
                ("}", STRING),
                ("\"\"\"", STRING),
                ("Examples:", KEYWORD),
                ("n", KEY),
                ("kind", KEY),
                ("12", NUMBER),
                ("red", STRING),
            ]
        );
    }

    #[test]
    fn placeholders_only_in_outlines() {
        let src = "Feature: F\n  Scenario: html\n    Given the page shows <b>bold</b>\n";
        assert!(tagged(src).iter().all(|(_, t)| *t != VARIABLE));
    }

    #[test]
    fn numbers_need_word_boundaries() {
        let src = "Feature: F\n Scenario: s\n  Given item42 and -3 and 1.5 and v2.0 and 3rd and \"x 1\"\n";
        let spans = tagged(src);
        let numbers: Vec<&str> = spans
            .iter()
            .filter(|(_, t)| *t == NUMBER)
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(numbers, vec!["-3", "1.5"]);
        assert!(spans.contains(&("\"x 1\"", STRING)));
    }

    #[test]
    fn localized_keywords() {
        let src = "# language: fr\nFonctionnalité: Calcul\n  Scénario: Addition\n    Soit un nombre 5\n    Lorsqu'on ajoute 2\n";
        let spans = tagged(src);
        assert!(spans.contains(&("# language: fr", COMMENT)));
        assert!(spans.contains(&("Fonctionnalité:", KEYWORD)));
        assert!(spans.contains(&("Scénario:", KEYWORD)));
        assert!(spans.contains(&("Soit", KEYWORD)));
        assert!(spans.contains(&("Lorsqu'", KEYWORD)));
        let de = tagged("# language: de\nFunktionalität: X\n  Szenario: Y\n    Angenommen es gibt 2\n    Und dann\n");
        assert!(de.contains(&("Angenommen", KEYWORD)));
        assert!(de.contains(&("Und", KEYWORD)));
        assert!(de.contains(&("2", NUMBER)));
    }

    #[test]
    fn invariants_on_garbage() {
        for src in [
            "",
            "\n\n",
            "\"\"\"\nunterminated",
            "| a | <b",
            "@\n#\n|",
            "Feature:\u{fffd}ü\n Scenario Outline: <\n  Given <<x>> \"",
            "\r\n\tGiven x\r\n",
        ] {
            let spans = highlight(src);
            let mut last = 0;
            for (r, t) in spans {
                assert!(
                    r.start >= last && r.start < r.end && r.end <= src.len(),
                    "{src:?}"
                );
                assert!(src.is_char_boundary(r.start) && src.is_char_boundary(r.end));
                assert!(TAG_NAMES.contains(&t));
                last = r.end;
            }
        }
    }

    #[test]
    fn numbers() {
        for n in ["1", "-2", "+3", "1.5", ".5", "10.25"] {
            assert!(is_number(n), "{n}");
        }
        for n in ["", "-", "1.", "a", "1a", "1.2.3", "1e5"] {
            assert!(!is_number(n), "{n}");
        }
    }
}
