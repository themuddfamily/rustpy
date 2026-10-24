//! Fold regions for files without a syntax tree (roadmap §1.1 code folding):
//! bracket pairs for JSON / JavaScript / CSS, indentation blocks for YAML /
//! HTML / templates, `[section]`s for TOML / INI and headings for Markdown.
//!
//! A region hides the lines after its header line; for brackets the line of
//! the closing bracket stays visible (like other editors do).

use rustpy_core::{TextRange, TextSize};

use super::state::Language;
use super::syntax_support::{FoldKind, FoldRegion};

fn ts(n: usize) -> TextSize {
    TextSize::new(n as u32)
}

/// Whether `language` gets fold regions from this module.
pub fn supports(language: Language) -> bool {
    matches!(
        language,
        Language::Json
            | Language::JavaScript
            | Language::Css
            | Language::Yaml
            | Language::Html
            | Language::Template
            | Language::Toml
            | Language::Ini
            | Language::Markdown
    )
}

/// Fold regions of `text` in `language`, sorted like the Python ones.
pub fn fold_regions(
    language: Language,
    text: &str,
    line_of: &dyn Fn(TextSize) -> usize,
) -> Vec<FoldRegion> {
    let lines = split_lines(text);
    let mut out = match language {
        Language::Json | Language::JavaScript | Language::Css => {
            bracket_regions(text, language, line_of)
        }
        Language::Yaml | Language::Html | Language::Template => indent_regions(&lines),
        Language::Toml | Language::Ini => section_regions(&lines),
        Language::Markdown => heading_regions(&lines),
        _ => Vec::new(),
    };
    out.sort_by_key(|r| (r.first_line, std::cmp::Reverse(r.last_line)));
    out.dedup_by_key(|r| r.first_line);
    out
}

/// `(start offset, text)` of every line (without the newline).
fn split_lines(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            out.push((start, &text[start..i]));
            start = i + 1;
        }
    }
    out.push((start, &text[start..]));
    out
}

fn indent_of(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// Region hiding lines `first+1 ..= last` (header line `first`).
fn block(lines: &[(usize, &str)], first: usize, last: usize, placeholder: &str) -> FoldRegion {
    let start = lines[first].0 + lines[first].1.len();
    let end = lines[last].0 + lines[last].1.len();
    FoldRegion {
        range: TextRange::new(ts(start), ts(end.max(start))),
        first_line: first,
        last_line: last,
        placeholder: placeholder.to_string(),
        kind: FoldKind::Block,
    }
}

fn bracket_regions(
    text: &str,
    language: Language,
    line_of: &dyn Fn(TextSize) -> usize,
) -> Vec<FoldRegion> {
    let mut out = Vec::new();
    for (open, close) in bracket_pairs(text, language) {
        let (first, close_line) = (line_of(ts(open)), line_of(ts(close)));
        // Keep the closing bracket's line visible.
        if close_line > first + 1 {
            let (kind, placeholder) = match text.as_bytes()[open] {
                b'{' => (FoldKind::Block, "{...}"),
                b'[' => (FoldKind::Brackets, "[...]"),
                _ => (FoldKind::Brackets, "(...)"),
            };
            out.push(FoldRegion {
                range: TextRange::new(ts(open + 1), ts(close)),
                first_line: first,
                last_line: close_line - 1,
                placeholder: placeholder.to_string(),
                kind,
            });
        }
    }
    out
}

/// Matching `()`, `[]` and `{}` pairs as `(open, close)` byte offsets, skipping
/// string literals and comments of C-like languages (JSON, JavaScript, CSS,
/// SQL); other text is scanned as is.
pub fn bracket_pairs(text: &str, language: Language) -> Vec<(usize, usize)> {
    let line_comments = language == Language::JavaScript;
    let block_comments = matches!(
        language,
        Language::JavaScript | Language::Css | Language::Sql
    );
    let strings = matches!(
        language,
        Language::Json
            | Language::JavaScript
            | Language::Css
            | Language::Sql
            | Language::Python
            | Language::Cython
    );
    let bytes = text.as_bytes();
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'"' | b'\'' | b'`' if strings && (b != b'`' || language == Language::JavaScript) => {
                // Skip the string literal (template strings may span lines).
                i += 1;
                while i < bytes.len() && bytes[i] != b {
                    if bytes[i] == b'\\' {
                        i += 1;
                    } else if bytes[i] == b'\n' && b != b'`' {
                        break;
                    }
                    i += 1;
                }
            }
            b'/' if line_comments && bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if block_comments && bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'{' | b'[' | b'(' => stack.push((b, i)),
            b'}' | b']' | b')' => {
                let open = match b {
                    b'}' => b'{',
                    b']' => b'[',
                    _ => b'(',
                };
                if let Some(pos) = stack.iter().rposition(|(c, _)| *c == open) {
                    let (_, at) = stack[pos];
                    stack.truncate(pos);
                    out.push((at, i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Lines followed by more-indented lines fold those lines.
fn indent_regions(lines: &[(usize, &str)]) -> Vec<FoldRegion> {
    let mut out = Vec::new();
    // Open headers: (indent, line).
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut last_non_blank: Option<usize> = None;
    for (i, (_, text)) in lines.iter().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        let w = indent_of(text);
        while let Some(&(hw, hl)) = stack.last() {
            if hw < w {
                break;
            }
            stack.pop();
            if let Some(last) = last_non_blank.filter(|l| *l > hl) {
                out.push(block(lines, hl, last, "..."));
            }
        }
        stack.push((w, i));
        last_non_blank = Some(i);
    }
    while let Some((_, hl)) = stack.pop() {
        if let Some(last) = last_non_blank.filter(|l| *l > hl) {
            out.push(block(lines, hl, last, "..."));
        }
    }
    out
}

/// `[section]` / `[[table]]` headers fold up to the next header.
fn section_regions(lines: &[(usize, &str)]) -> Vec<FoldRegion> {
    let is_header = |t: &str| {
        let t = t.trim();
        t.starts_with('[') && t.ends_with(']')
    };
    let mut out = Vec::new();
    let mut current: Option<usize> = None;
    let mut last_non_blank = 0;
    for (i, (_, text)) in lines.iter().enumerate() {
        if is_header(text) {
            if let Some(h) = current.take() {
                if last_non_blank > h {
                    out.push(block(lines, h, last_non_blank, "..."));
                }
            }
            current = Some(i);
        }
        if !text.trim().is_empty() {
            last_non_blank = i;
        }
    }
    if let Some(h) = current {
        if last_non_blank > h {
            out.push(block(lines, h, last_non_blank, "..."));
        }
    }
    out
}

/// Markdown headings fold up to the next heading of the same or a higher level
/// (fenced code blocks are skipped).
fn heading_regions(lines: &[(usize, &str)]) -> Vec<FoldRegion> {
    let mut out = Vec::new();
    // Open headings: (level, line).
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut last_non_blank = 0;
    let mut in_fence = false;
    let close =
        |stack: &mut Vec<(usize, usize)>, level: usize, last: usize, out: &mut Vec<FoldRegion>| {
            while let Some(&(l, line)) = stack.last() {
                if l < level {
                    break;
                }
                stack.pop();
                if last > line {
                    out.push(block(lines, line, last, "..."));
                }
            }
        };
    for (i, (_, text)) in lines.iter().enumerate() {
        let t = text.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence && t.starts_with('#') {
            let level = t.chars().take_while(|c| *c == '#').count();
            if level <= 6 && (t[level..].starts_with([' ', '\t']) || t.len() == level) {
                close(&mut stack, level, last_non_blank, &mut out);
                stack.push((level, i));
            }
        }
        if !text.trim().is_empty() {
            last_non_blank = i;
        }
    }
    close(&mut stack, 0, last_non_blank, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions(language: Language, text: &str) -> Vec<(usize, usize)> {
        let line_of = |off: TextSize| text[..usize::from(off)].matches('\n').count();
        fold_regions(language, text, &line_of)
            .iter()
            .map(|r| (r.first_line, r.last_line))
            .collect()
    }

    #[test]
    fn json_brackets_keep_the_closing_line_visible() {
        let text = "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"s\": \"{not a bracket\"\n}\n";
        assert_eq!(regions(Language::Json, text), vec![(0, 5), (1, 3)]);
    }

    #[test]
    fn javascript_skips_comments_and_strings() {
        let text = "function f() {\n  // }\n  const s = '}';\n  return 1;\n}\n";
        assert_eq!(regions(Language::JavaScript, text), vec![(0, 3)]);
    }

    #[test]
    fn yaml_folds_by_indentation() {
        let text = "a:\n  b: 1\n  c:\n    - x\n\nd: 2\n";
        assert_eq!(regions(Language::Yaml, text), vec![(0, 3), (2, 3)]);
    }

    #[test]
    fn toml_sections_and_markdown_headings() {
        assert_eq!(
            regions(Language::Toml, "[a]\nx = 1\ny = 2\n\n[b]\nz = 3\n"),
            vec![(0, 2), (4, 5)]
        );
        let md = "# Title\ntext\n## Sub\nmore\n```\n# not a heading\n```\n# Next\nend\n";
        assert_eq!(
            regions(Language::Markdown, md),
            vec![(0, 6), (2, 6), (7, 8)]
        );
    }
}
