//! Small text helpers shared by the editor and language services.

use unicode_width::UnicodeWidthChar;

/// Visual width of `c` in columns, treating tabs as `tab_size` and
/// control characters as width 1 (rendered as placeholders).
pub fn char_width(c: char, tab_size: usize, column: usize) -> usize {
    match c {
        '\t' => tab_size - (column % tab_size),
        c if c.is_control() => 1,
        c => c.width().unwrap_or(1).max(1),
    }
}

/// Returns the leading whitespace of a line.
pub fn leading_whitespace(line: &str) -> &str {
    let end = line
        .find(|c: char| c != ' ' && c != '\t')
        .unwrap_or(line.len());
    &line[..end]
}

/// Visual indentation width (in columns) of a line.
pub fn indent_width(line: &str, tab_size: usize) -> usize {
    let mut col = 0;
    for c in leading_whitespace(line).chars() {
        col += char_width(c, tab_size, col);
    }
    col
}

/// Builds an indentation string of `columns` columns.
pub fn make_indent(columns: usize, use_tabs: bool, tab_size: usize) -> String {
    if use_tabs && tab_size > 0 {
        let mut s = "\t".repeat(columns / tab_size);
        s.push_str(&" ".repeat(columns % tab_size));
        s
    } else {
        " ".repeat(columns)
    }
}

/// True if the whole line is whitespace.
pub fn is_blank(line: &str) -> bool {
    line.chars().all(char::is_whitespace)
}

/// Indentation style of a file: tab characters, or `n` spaces per level.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum IndentStyle {
    Tabs,
    Spaces(usize),
}

impl IndentStyle {
    /// Status-bar style label: `Tab` or `4 spaces`.
    pub fn label(self) -> String {
        match self {
            IndentStyle::Tabs => "Tab".into(),
            IndentStyle::Spaces(1) => "1 space".into(),
            IndentStyle::Spaces(n) => format!("{n} spaces"),
        }
    }
}

/// Lines [`detect_indent`] looks at, at most.
const DETECT_SAMPLE_LINES: usize = 2000;

/// Detects the indentation a file already uses ("Detect and use existing file
/// indents"). Counts tab- vs space-indented lines and, for spaces, the most
/// common positive indent step between consecutive code lines. With `python`,
/// lines inside triple-quoted strings, open brackets and after a backslash
/// continuation (hanging and aligned indents) and comment-only lines are
/// skipped. Returns `None` when the file has no indented lines or no clear
/// majority.
pub fn detect_indent(text: &str, python: bool) -> Option<IndentStyle> {
    let mut scanner = PyLineScanner::default();
    let (mut tab_lines, mut space_lines) = (0usize, 0usize);
    // steps[n] = how often the indent grew by n columns (1..=8) between code lines.
    let mut steps = [0usize; 9];
    let mut prev: Option<usize> = None;
    let mut sampled = 0;
    for line in text.lines() {
        if sampled >= DETECT_SAMPLE_LINES {
            break;
        }
        let code_start = !python || scanner.at_code_start();
        if python {
            scanner.feed(line);
        }
        if !code_start || is_blank(line) || (python && line.trim_start().starts_with('#')) {
            continue;
        }
        sampled += 1;
        let ws = leading_whitespace(line);
        if ws.starts_with('\t') {
            tab_lines += 1;
            prev = None;
            continue;
        }
        if ws.contains('\t') {
            // Spaces followed by a tab: ambiguous, ignore.
            prev = None;
            continue;
        }
        let width = ws.len();
        if width > 0 {
            space_lines += 1;
        }
        if let Some(p) = prev {
            if width > p && width - p <= 8 {
                steps[width - p] += 1;
            }
        }
        prev = Some(width);
    }
    if tab_lines > 0 && tab_lines >= 2 * space_lines {
        return Some(IndentStyle::Tabs);
    }
    if space_lines == 0 || space_lines < 2 * tab_lines {
        return None;
    }
    let total: usize = steps.iter().sum();
    let (best, count) = steps
        .iter()
        .enumerate()
        .skip(1)
        .max_by_key(|(n, c)| (**c, std::cmp::Reverse(*n)))?;
    // A strict majority of the indent steps must agree.
    (*count > 0 && 2 * count > total).then_some(IndentStyle::Spaces(best))
}

/// Tracks Python strings and brackets across lines for [`detect_indent`].
#[derive(Default)]
struct PyLineScanner {
    /// Quote character of an open triple-quoted string.
    triple: Option<u8>,
    depth: usize,
    backslash: bool,
}

impl PyLineScanner {
    /// The next line starts a statement (not inside a string or brackets, no `\` continuation).
    fn at_code_start(&self) -> bool {
        self.triple.is_none() && self.depth == 0 && !self.backslash
    }

    fn feed(&mut self, line: &str) {
        let b = line.as_bytes();
        self.backslash = false;
        let mut i = 0;
        while i < b.len() {
            if let Some(q) = self.triple {
                if b[i] == b'\\' {
                    i += 2;
                } else if b[i] == q && b.get(i + 1) == Some(&q) && b.get(i + 2) == Some(&q) {
                    self.triple = None;
                    i += 3;
                } else {
                    i += 1;
                }
                continue;
            }
            match b[i] {
                b'#' => break,
                q @ (b'"' | b'\'') => {
                    if b.get(i + 1) == Some(&q) && b.get(i + 2) == Some(&q) {
                        self.triple = Some(q);
                        i += 3;
                        continue;
                    }
                    // Single-line string: skip to its closing quote.
                    i += 1;
                    while i < b.len() && b[i] != q {
                        i += if b[i] == b'\\' { 2 } else { 1 };
                    }
                }
                b'(' | b'[' | b'{' => self.depth += 1,
                b')' | b']' | b'}' => self.depth = self.depth.saturating_sub(1),
                b'\\' if i + 1 == b.len() => self.backslash = true,
                _ => {}
            }
            i += 1;
        }
    }
}

/// Classifies a character for word-boundary movement (Ctrl+Left/Right).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CharClass {
    Whitespace,
    Word,
    Punctuation,
}

pub fn char_class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Whitespace
    } else if rustpy_core::is_ident_continue(c) {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Whether `prev` → `next` is a camel-hump boundary (`fooBar`, `HTTPServer`).
pub fn is_camel_hump(prev: char, next: char, after_next: Option<char>) -> bool {
    (prev.is_lowercase() && next.is_uppercase())
        || (prev.is_uppercase()
            && next.is_uppercase()
            && after_next.is_some_and(|c| c.is_lowercase()))
        || (prev.is_alphabetic() && next.is_numeric())
        || (prev.is_numeric() && next.is_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indent_and_width() {
        assert_eq!(indent_width("    x", 4), 4);
        assert_eq!(indent_width("\t x", 4), 5);
        assert_eq!(make_indent(6, true, 4), "\t  ");
        assert_eq!(char_width('\t', 4, 2), 2);
    }

    #[test]
    fn detects_two_space_python() {
        let src = "import os\n\n\ndef f(a,\n        b):\n  if a:\n    return [\n        1,\n    ]\n  # comment\n  x = \"\"\"\n      text\n  \"\"\"\n  return b\n";
        assert_eq!(detect_indent(src, true), Some(IndentStyle::Spaces(2)));
    }

    #[test]
    fn detects_four_spaces_despite_continuation_lines() {
        let src = "class A:\n    def f(self):\n        x = call(1,\n                 2)\n        y = 1 + \\\n              2\n        return x\n";
        assert_eq!(detect_indent(src, true), Some(IndentStyle::Spaces(4)));
        // Without Python awareness the 6-column continuation step is outvoted.
        assert_eq!(detect_indent(src, false), Some(IndentStyle::Spaces(4)));
        // Strings that contain brackets or quotes do not confuse the scanner.
        let tricky = "def f():\n    s = \"(\" + '[' + \"\\\"\"\n    if s:\n        return s\n";
        assert_eq!(detect_indent(tricky, true), Some(IndentStyle::Spaces(4)));
    }

    #[test]
    fn detects_tabs() {
        let src = "def f():\n\tif x:\n\t\treturn 1\n\treturn 2\n";
        assert_eq!(detect_indent(src, true), Some(IndentStyle::Tabs));
        // Tabs followed by alignment spaces still count as tabs.
        assert_eq!(
            detect_indent("a:\n\tb = (1,\n\t     2)\n\tc\n", false),
            Some(IndentStyle::Tabs)
        );
    }

    #[test]
    fn no_result_without_indentation_or_majority() {
        assert_eq!(detect_indent("x = 1\ny = 2\n", true), None);
        assert_eq!(detect_indent("", true), None);
        // As many tab- as space-indented lines: mixed, no decision.
        assert_eq!(detect_indent("a:\n\tb\nc:\n    d\n", true), None);
        // 2-, 3- and 4-column steps with no majority.
        assert_eq!(detect_indent("a:\n  b\nc:\n   d\ne:\n    f\n", true), None);
    }

    #[test]
    fn plain_text_counts_every_line() {
        let yaml = "root:\n  child:\n    leaf: 1\n  other: [\n    1,\n  ]\n";
        assert_eq!(detect_indent(yaml, false), Some(IndentStyle::Spaces(2)));
        let json = "{\n  \"a\": {\n    \"b\": 1\n  }\n}\n";
        assert_eq!(detect_indent(json, false), Some(IndentStyle::Spaces(2)));
        assert_eq!(IndentStyle::Spaces(2).label(), "2 spaces");
        assert_eq!(IndentStyle::Tabs.label(), "Tab");
    }

    #[test]
    fn camel_humps() {
        assert!(is_camel_hump('o', 'B', Some('a')));
        assert!(is_camel_hump('P', 'S', Some('e')));
        assert!(!is_camel_hump('T', 'T', Some('P')));
    }
}
