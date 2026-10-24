//! Line classification shared by the highlighter and the parser.
//!
//! Gherkin is line oriented; what a line means depends a little on context
//! (text after `Feature:` is description, after `Scenario:` a step keyword
//! starts a step, everything inside a doc string is content), so the scanner
//! is a small state machine over lines.

use std::ops::Range;

use crate::dialect::{default_dialect, dialect, Dialect};
use crate::model::{BlockKind, StepKeyword};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Tok {
    Empty,
    Comment,
    /// `# language: de` (only before the feature). `code` is the code's range.
    Language {
        code: Range<usize>,
    },
    Tags {
        tags: Vec<Range<usize>>,
        comment: Option<Range<usize>>,
    },
    /// `Feature: name`. `keyword` covers the keyword and its colon.
    Block {
        kind: BlockKind,
        keyword: Range<usize>,
        name: Range<usize>,
    },
    /// `Given text`. `keyword` excludes the separating whitespace.
    Step {
        keyword_type: StepKeyword,
        keyword: Range<usize>,
        text: Range<usize>,
    },
    DocOpen {
        fence: &'static str,
        delimiter: Range<usize>,
        media: Range<usize>,
    },
    DocLine,
    DocClose {
        delimiter: Range<usize>,
    },
    /// `| a | b |`: trimmed cell contents. `header` marks the first row of a
    /// table; `examples` marks rows of an `Examples:` table.
    Row {
        cells: Vec<Range<usize>>,
        header: bool,
        examples: bool,
    },
    Description,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LineTok {
    /// 0-based line index.
    pub line: usize,
    pub start: usize,
    /// End of the line content (excluding `\r\n`).
    pub end: usize,
    /// Offset of the first non-whitespace character (`end` for blank lines).
    pub indent: usize,
    /// End of the content without trailing whitespace.
    pub trim_end: usize,
    pub tok: Tok,
    /// Inside a scenario outline (placeholders are substituted there).
    pub outline: bool,
}

pub(crate) struct Scan {
    pub lines: Vec<LineTok>,
    pub dialect: &'static Dialect,
    /// The language code from the header and whether it is known.
    pub language: Option<(String, bool)>,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Ctx {
    /// Before the `Feature:` line.
    Start,
    /// Right after a block line: free text is description.
    Header,
    /// After the first step of a background / scenario.
    Steps,
    /// After the first row of an examples table.
    Table,
}

/// `(start, end)` of each line, `end` excluding `\n` / `\r\n`.
pub(crate) fn line_bounds(src: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = src.as_bytes();
    while start <= src.len() {
        let nl = bytes[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| start + p);
        let next = nl.map(|p| p + 1);
        let mut end = nl.unwrap_or(src.len());
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        out.push((start, end));
        match next {
            Some(n) if n < src.len() => start = n,
            Some(_) => break,
            None => break,
        }
    }
    out
}

/// Parses `# language: xx`; returns the code's byte range relative to `s`.
fn language_header(s: &str) -> Option<Range<usize>> {
    let rest = s.strip_prefix('#')?;
    let after_hash = s.len() - rest.len();
    let t = rest.trim_start();
    let t_off = after_hash + (rest.len() - t.len());
    let t2 = t.strip_prefix("language")?;
    let t3 = t2.trim_start();
    let t4 = t3.strip_prefix(':')?;
    let code = t4.trim();
    if code.is_empty()
        || !code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let code_off = t_off + (t.len() - t4.len()) + (t4.len() - t4.trim_start().len());
    Some(code_off..code_off + code.len())
}

/// Splits a table row into trimmed cell ranges (absolute offsets). `s` starts
/// with `|` at absolute offset `base`. A trailing cell without a closing pipe
/// is kept when non-empty (the user is probably still typing it).
pub(crate) fn row_cells(s: &str, base: usize) -> Vec<Range<usize>> {
    let mut cells = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 1; // after the first '|'
    let mut cell_start = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'|' => {
                cells.push(trim_range(s, cell_start, i, base));
                cell_start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    let tail = trim_range(s, cell_start.min(s.len()), s.len(), base);
    if !tail.is_empty() {
        cells.push(tail);
    }
    cells
}

fn trim_range(s: &str, a: usize, b: usize, base: usize) -> Range<usize> {
    let a = a.min(s.len());
    let b = b.min(s.len()).max(a);
    let piece = &s[a..b];
    let lead = piece.len() - piece.trim_start().len();
    let trail = piece.len() - piece.trim_end().len();
    let start = a + lead;
    let end = (b - trail).max(start);
    base + start..base + end
}

/// Tag ranges and an optional trailing comment of a tag line starting at `base`.
fn tag_ranges(s: &str, base: usize) -> (Vec<Range<usize>>, Option<Range<usize>>) {
    let mut tags = Vec::new();
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i] == b'#' {
            let end = s.trim_end().len().max(i);
            return (tags, Some(base + i..base + end));
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes[start] == b'@' && i > start + 1 {
            tags.push(base + start..base + i);
        }
    }
    (tags, None)
}

pub(crate) fn scan(src: &str) -> Scan {
    let mut dialect_ref: &'static Dialect = default_dialect();
    let mut language: Option<(String, bool)> = None;
    let mut ctx = Ctx::Start;
    let mut block: Option<BlockKind> = None;
    let mut doc_fence: Option<&'static str> = None;
    // Whether the previous significant token was a table row (for headers).
    let mut in_table = false;
    // Only blank and comment lines so far (where `# language:` may appear).
    let mut header_zone = true;
    let mut lines: Vec<LineTok> = Vec::new();
    for (idx, (start, end)) in line_bounds(src).into_iter().enumerate() {
        let text = &src[start..end];
        let lead = text.len() - text.trim_start().len();
        let indent = start + lead;
        let trimmed = text.trim();
        let trim_end = indent + trimmed.len();
        let tok = if let Some(fence) = doc_fence {
            if trimmed == fence {
                doc_fence = None;
                Tok::DocClose {
                    delimiter: indent..indent + fence.len(),
                }
            } else {
                Tok::DocLine
            }
        } else if trimmed.is_empty() {
            Tok::Empty
        } else if trimmed.starts_with('#') {
            match (ctx, language_header(trimmed)) {
                (Ctx::Start, Some(r)) if header_zone => {
                    let code = &trimmed[r.clone()];
                    let found = dialect(code);
                    dialect_ref = found.unwrap_or(default_dialect());
                    language = Some((code.to_string(), found.is_some()));
                    Tok::Language {
                        code: indent + r.start..indent + r.end,
                    }
                }
                _ => Tok::Comment,
            }
        } else if trimmed.starts_with('@') {
            in_table = false;
            let (tags, comment) = tag_ranges(trimmed, indent);
            Tok::Tags { tags, comment }
        } else if trimmed.starts_with("\"\"\"") || trimmed.starts_with("```") {
            let fence: &'static str = if trimmed.starts_with("\"\"\"") {
                "\"\"\""
            } else {
                "```"
            };
            doc_fence = Some(fence);
            in_table = false;
            let media = &trimmed[fence.len()..];
            let m_lead = media.len() - media.trim_start().len();
            let m_start = indent + fence.len() + m_lead;
            Tok::DocOpen {
                fence,
                delimiter: indent..indent + fence.len(),
                media: m_start..m_start + media.trim().len(),
            }
        } else if trimmed.starts_with('|') {
            let examples = block == Some(BlockKind::Examples);
            let header = !in_table;
            in_table = true;
            if examples {
                ctx = Ctx::Table;
            }
            Tok::Row {
                cells: row_cells(trimmed, indent),
                header,
                examples,
            }
        } else if let Some((kind, _kw, kw_len)) = dialect_ref.match_block(trimmed) {
            in_table = false;
            block = Some(kind);
            ctx = Ctx::Header;
            let name = &trimmed[kw_len..];
            let n_lead = name.len() - name.trim_start().len();
            let n_start = indent + kw_len + n_lead;
            Tok::Block {
                kind,
                keyword: indent..indent + kw_len,
                name: n_start..n_start + name.trim().len(),
            }
        } else {
            in_table = false;
            let steps_allowed = matches!(
                block,
                Some(BlockKind::Background | BlockKind::Scenario | BlockKind::ScenarioOutline)
            ) && matches!(ctx, Ctx::Header | Ctx::Steps);
            match dialect_ref.match_step(trimmed).filter(|_| steps_allowed) {
                Some((keyword_type, kw_len)) => {
                    ctx = Ctx::Steps;
                    let rest = &trimmed[kw_len..];
                    let r_lead = rest.len() - rest.trim_start().len();
                    let t_start = indent + kw_len + r_lead;
                    Tok::Step {
                        keyword_type,
                        keyword: indent..indent + kw_len,
                        text: t_start..t_start + rest.trim().len(),
                    }
                }
                None if ctx == Ctx::Header => Tok::Description,
                None => Tok::Other,
            }
        };
        if !matches!(tok, Tok::Row { .. } | Tok::Comment | Tok::Empty) {
            in_table = false;
        }
        if !matches!(tok, Tok::Comment | Tok::Empty) {
            header_zone = false;
        }
        lines.push(LineTok {
            line: idx,
            start,
            end,
            indent,
            trim_end,
            tok,
            outline: false,
        });
    }
    mark_outlines(&mut lines);
    Scan {
        lines,
        dialect: dialect_ref,
        language,
    }
}

/// Marks the lines of scenario outlines (outline keyword, or a scenario that
/// has an `Examples:` section).
fn mark_outlines(lines: &mut [LineTok]) {
    let mut i = 0;
    while i < lines.len() {
        let is_scenario = matches!(
            lines[i].tok,
            Tok::Block {
                kind: BlockKind::Scenario | BlockKind::ScenarioOutline,
                ..
            }
        );
        if !is_scenario {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut has_examples = false;
        while j < lines.len() {
            match &lines[j].tok {
                Tok::Block {
                    kind: BlockKind::Examples,
                    ..
                } => has_examples = true,
                Tok::Block { .. } => break,
                _ => {}
            }
            j += 1;
        }
        let outline_kw = matches!(
            lines[i].tok,
            Tok::Block {
                kind: BlockKind::ScenarioOutline,
                ..
            }
        );
        if outline_kw || has_examples {
            for l in &mut lines[i..j] {
                l.outline = true;
            }
        }
        i = j;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<String> {
        scan(src)
            .lines
            .iter()
            .map(|l| match &l.tok {
                Tok::Empty => "empty".to_string(),
                Tok::Comment => "comment".into(),
                Tok::Language { .. } => "language".into(),
                Tok::Tags { .. } => "tags".into(),
                Tok::Block { kind, .. } => format!("{kind:?}"),
                Tok::Step { keyword_type, .. } => format!("step:{keyword_type:?}"),
                Tok::DocOpen { .. } => "doc-open".into(),
                Tok::DocLine => "doc".into(),
                Tok::DocClose { .. } => "doc-close".into(),
                Tok::Row {
                    header, examples, ..
                } => format!(
                    "row{}{}",
                    if *header { ":header" } else { "" },
                    if *examples { ":ex" } else { "" }
                ),
                Tok::Description => "description".into(),
                Tok::Other => "other".into(),
            })
            .collect()
    }

    #[test]
    fn classifies_lines_in_context() {
        let src = "# language: en\n@tag\nFeature: F\n  And this is description\n\n  Scenario: S\n    a description\n    Given x\n      | h |\n      | 1 |\n    And y\n      \"\"\"\n      Given inside doc\n      \"\"\"\n    stray text\n";
        assert_eq!(
            kinds(src),
            vec![
                "language",
                "tags",
                "Feature",
                "description",
                "empty",
                "Scenario",
                "description",
                "step:Given",
                "row:header",
                "row",
                "step:And",
                "doc-open",
                "doc",
                "doc-close",
                "other"
            ]
        );
    }

    #[test]
    fn examples_rows_and_outline_marking() {
        let src = "Feature: F\nScenario Outline: add <a>\n  When I add <a>\n  Examples:\n    | a |\n    | 1 |\nScenario: plain <x>\n  Given <x>\n";
        let s = scan(src);
        assert_eq!(
            kinds(src),
            vec![
                "Feature",
                "ScenarioOutline",
                "step:When",
                "Examples",
                "row:header:ex",
                "row:ex",
                "Scenario",
                "step:Given"
            ]
        );
        let outline: Vec<bool> = s.lines.iter().map(|l| l.outline).collect();
        assert_eq!(
            outline,
            vec![false, true, true, true, true, true, false, false]
        );
    }

    #[test]
    fn scenario_with_examples_is_an_outline() {
        let s = scan("Feature: F\nScenario: s\n  Given <x>\n  Examples:\n    | x |\n");
        assert!(s.lines[2].outline);
    }

    #[test]
    fn language_header_switches_dialect() {
        let s = scan("# language: de\nFunktionalität: Rechner\n  Szenario: Addieren\n    Angenommen ich habe 5\n");
        assert_eq!(s.dialect.code, "de");
        assert_eq!(s.language, Some(("de".to_string(), true)));
        assert!(matches!(
            s.lines[1].tok,
            Tok::Block {
                kind: BlockKind::Feature,
                ..
            }
        ));
        assert!(matches!(
            s.lines[3].tok,
            Tok::Step {
                keyword_type: StepKeyword::Given,
                ..
            }
        ));
        // The header only counts before anything else.
        let late = scan("Feature: F\n# language: de\n");
        assert_eq!(late.dialect.code, "en");
        assert!(matches!(late.lines[1].tok, Tok::Comment));
        let unknown = scan("#language:xx\nFeature: F\n");
        assert_eq!(unknown.language, Some(("xx".to_string(), false)));
        assert_eq!(unknown.dialect.code, "en");
    }

    #[test]
    fn language_header_ranges() {
        let src = "  #  language :  fr  \n";
        let s = scan(src);
        let Tok::Language { code } = &s.lines[0].tok else {
            panic!("{:?}", s.lines[0].tok)
        };
        assert_eq!(&src[code.clone()], "fr");
    }

    #[test]
    fn cells_and_tags() {
        let src = "| a | b \\| c |  d  \n";
        let cells = row_cells(src.trim_end(), 0);
        let texts: Vec<&str> = cells.iter().map(|r| &src[r.clone()]).collect();
        assert_eq!(texts, vec!["a", "b \\| c", "d"]);
        let (tags, comment) = tag_ranges("@a  @b-c #note", 10);
        assert_eq!(tags, vec![10..12, 14..18]);
        assert_eq!(comment, Some(19..24));
    }

    #[test]
    fn crlf_and_missing_trailing_newline() {
        let b = line_bounds("a\r\nb");
        assert_eq!(b, vec![(0, 1), (3, 4)]);
        assert_eq!(line_bounds(""), vec![(0, 0)]);
        assert_eq!(line_bounds("x\n"), vec![(0, 1)]);
    }
}
