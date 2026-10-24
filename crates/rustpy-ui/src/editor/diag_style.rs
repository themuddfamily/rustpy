//! How problems look in the editor: unused
//! imports and local variables are grayed out with no underline, uses of
//! deprecated APIs are struck through, and every other problem gets a wavy
//! underline in its severity's color.

use rustpy_core::{Diagnostic, TextRange};
use rustpy_highlight::{HighlightTag, Span};
use rustpy_syntax::rowan::NodeOrToken;
use rustpy_syntax::{Parse, SyntaxKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Wave,
    /// Gray text ([`HighlightTag::Unused`]), no underline.
    Unused,
    /// A line through the text.
    StrikeOut,
}

pub fn look(d: &Diagnostic) -> Look {
    match d.code.as_str() {
        "PyUnusedImports" | "PyUnusedLocal" => Look::Unused,
        "PyDeprecation" => Look::StrikeOut,
        _ => Look::Wave,
    }
}

/// Messages of the problems at `offset` (as the editor's hover tooltip lists them).
pub fn messages_at(diags: &[Diagnostic], offset: rustpy_core::TextSize) -> Vec<String> {
    diags
        .iter()
        .filter(|d| {
            d.range.contains_inclusive(offset) && (!d.range.is_empty() || d.range.start() == offset)
        })
        .map(|d| d.message.clone())
        .collect()
}

/// The text to gray out, sorted and disjoint: unused locals, and unused
/// imports widened to their alias (`path as p`) or, when no name of the
/// statement is used, to the whole `import` statement.
pub fn unused_ranges(diags: &[Diagnostic], parse: &Parse) -> Vec<TextRange> {
    let root = parse.syntax_node();
    let end = root.text_range().end();
    let unused_imports: Vec<TextRange> = diags
        .iter()
        .filter(|d| d.code == "PyUnusedImports")
        .map(|d| d.range)
        .collect();
    let mut ranges: Vec<TextRange> = Vec::new();
    for d in diags
        .iter()
        .filter(|d| look(d) == Look::Unused && d.range.end() <= end)
    {
        let mut range = d.range;
        if d.code == "PyUnusedImports" {
            let node = match root.covering_element(d.range) {
                NodeOrToken::Node(n) => Some(n),
                NodeOrToken::Token(t) => t.parent(),
            };
            if let Some(alias) =
                node.and_then(|n| n.ancestors().find(|a| a.kind() == SyntaxKind::IMPORT_ALIAS))
            {
                range = alias.text_range();
                if let Some(stmt) = alias.parent().filter(|p| {
                    matches!(
                        p.kind(),
                        SyntaxKind::IMPORT_STMT | SyntaxKind::IMPORT_FROM_STMT
                    )
                }) {
                    let all_unused = stmt
                        .children()
                        .filter(|c| c.kind() == SyntaxKind::IMPORT_ALIAS)
                        .all(|a| {
                            unused_imports
                                .iter()
                                .any(|r| a.text_range().contains_range(*r))
                        });
                    if all_unused {
                        range = stmt.text_range();
                    }
                }
            }
        }
        ranges.push(range);
    }
    ranges.sort_by_key(|r| r.start());
    let mut merged: Vec<TextRange> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start() <= last.end() => *last = last.cover(r),
            _ => merged.push(r),
        }
    }
    merged
}

/// `spans` with the (sorted, disjoint) `ranges` re-tagged as
/// [`HighlightTag::Unused`].
pub fn with_unused(spans: Vec<Span>, ranges: &[TextRange]) -> Vec<Span> {
    if ranges.is_empty() {
        return spans;
    }
    let mut out = Vec::with_capacity(spans.len() + ranges.len() * 2);
    let mut first = 0;
    for s in spans {
        while first < ranges.len() && ranges[first].end() <= s.range.start() {
            first += 1;
        }
        let mut cursor = s.range.start();
        for r in ranges[first..]
            .iter()
            .take_while(|r| r.start() < s.range.end())
        {
            if r.start() > cursor {
                out.push(Span {
                    range: TextRange::new(cursor, r.start()),
                    tag: s.tag,
                });
            }
            cursor = cursor.max(r.end());
        }
        if cursor < s.range.end() {
            out.push(Span {
                range: TextRange::new(cursor, s.range.end()),
                tag: s.tag,
            });
        }
    }
    out.extend(ranges.iter().map(|r| Span {
        range: *r,
        tag: HighlightTag::Unused,
    }));
    out.sort_by_key(|s| s.range.start());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_core::{Severity, TextSize};

    /// A problem on the first `len` bytes of `needle` in `src`.
    fn diag(src: &str, needle: &str, len: usize, code: &str) -> Diagnostic {
        let start = src.find(needle).unwrap();
        Diagnostic::new(
            TextRange::at(TextSize::new(start as u32), TextSize::new(len as u32)),
            Severity::Warning,
            code,
            "",
        )
    }

    fn texts<'a>(src: &'a str, ranges: &[TextRange]) -> Vec<&'a str> {
        ranges.iter().map(|r| &src[*r]).collect()
    }

    #[test]
    fn unused_imports_gray_their_alias_or_whole_statement() {
        let src = "import os\nfrom sys import argv as a, path\nx = 1\n";
        let parse = rustpy_syntax::parse(src);
        let os = diag(src, "os", 2, "PyUnusedImports");
        let a = diag(src, "a, path", 1, "PyUnusedImports");
        let x = diag(src, "x =", 1, "PyUnusedLocal");
        let other = diag(src, "path", 4, "PyTypeChecker");
        // `os` is its statement's only name; `argv as a` is one of two.
        assert_eq!(
            texts(
                src,
                &unused_ranges(&[a.clone(), os.clone(), x, other.clone()], &parse)
            ),
            vec!["import os", "argv as a", "x"]
        );
        // Every name unused: the whole statement.
        let path = diag(src, "path", 4, "PyUnusedImports");
        assert_eq!(
            texts(src, &unused_ranges(&[a, path], &parse)),
            vec!["from sys import argv as a, path"]
        );
        assert_eq!(look(&other), Look::Wave);
        assert_eq!(look(&diag(src, "os", 2, "PyDeprecation")), Look::StrikeOut);
        assert_eq!(look(&os), Look::Unused);
    }

    #[test]
    fn unused_ranges_retag_spans() {
        let sp = |a: u32, b: u32, tag| Span {
            range: TextRange::new(TextSize::new(a), TextSize::new(b)),
            tag,
        };
        let spans = vec![
            sp(0, 6, HighlightTag::Keyword),
            sp(10, 20, HighlightTag::String),
        ];
        let ranges = [
            TextRange::new(TextSize::new(0), TextSize::new(9)),
            TextRange::new(TextSize::new(12), TextSize::new(14)),
        ];
        let got: Vec<(u32, u32, HighlightTag)> = with_unused(spans, &ranges)
            .iter()
            .map(|s| (s.range.start().into(), s.range.end().into(), s.tag))
            .collect();
        assert_eq!(
            got,
            vec![
                (0, 9, HighlightTag::Unused),
                (10, 12, HighlightTag::String),
                (12, 14, HighlightTag::Unused),
                (14, 20, HighlightTag::String)
            ]
        );
    }
}
