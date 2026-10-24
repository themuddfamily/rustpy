//! Reformat on paste: *Indent block* (roadmap §1.2 Smart Keys, §1.3 smart
//! paste). Pasted Python code is shifted so that its outermost lines land on
//! the caret's indentation while nested lines keep their relative indent.
//! Lines inside multi-line strings are never touched.

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::SyntaxKind;
use rustpy_text::util::{char_width, indent_width, make_indent};

use super::python_indent::{classify_lines, indent_after_enter_with_continuation, LineKind};
use super::state::{EditOptions, EditorState, Language};

/// Pastes larger than this are inserted unchanged (re-indenting needs a lexer pass).
const MAX_REINDENT_BYTES: usize = 1024 * 1024;

fn width(s: &str, tab_size: usize) -> usize {
    s.chars()
        .fold(0, |col, c| col + char_width(c, tab_size, col))
}

/// Whether the logical line starting `text` ends with a block-opening `:`.
fn first_statement_is_header(text: &str) -> bool {
    let (tokens, _) = rustpy_syntax::lexer::tokenize(text);
    let mut last = None;
    for t in &tokens {
        match t.kind {
            SyntaxKind::NEWLINE => break,
            k if k.is_trivia()
                || matches!(k, SyntaxKind::INDENT | SyntaxKind::DEDENT | SyntaxKind::EOF) => {}
            k => last = Some(k),
        }
    }
    last == Some(SyntaxKind::COLON)
}

/// Shifts the lines of `text` so the block's outermost indentation becomes
/// `target` columns. With `at_line_start` only whitespace precedes the paste
/// position and the first line is re-indented too; the returned flag tells
/// the caller to replace that whitespace (the first line now carries its own
/// indentation).
pub fn shift_block(
    text: &str,
    at_line_start: bool,
    target: usize,
    opts: &EditOptions,
) -> (String, bool) {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() == 1 && !at_line_start {
        return (text.to_string(), false);
    }
    let tab = opts.tab_size;
    let kinds = classify_lines(text);
    let ind = |i: usize| indent_width(lines[i], tab);
    let blank = |i: usize| lines[i].trim().is_empty();
    let is_code = |i: usize| !blank(i) && !lines[i].trim_start().starts_with('#');
    // Statement lines after the first one decide the block's base indentation.
    let statements: Vec<usize> = (1..lines.len())
        .filter(|&i| kinds[i] == LineKind::Logical && is_code(i))
        .collect();
    let first_has_ws = lines[0].starts_with([' ', '\t']);
    // Indentation the first line had where it was copied from.
    let first_indent: Option<usize> = if blank(0) {
        None
    } else if first_has_ws {
        Some(ind(0))
    } else {
        // Copied from the start of the code: infer it from the next statement
        // (one level less when the first statement opens a block).
        match statements.first() {
            Some(&next) if first_statement_is_header(text) => Some(
                ind(next)
                    .saturating_sub(opts.indent_size)
                    .min(statements.iter().map(|&line| ind(line)).min().unwrap()),
            ),
            Some(&next) => Some(ind(next)),
            None => None,
        }
    };
    let base = first_indent
        .into_iter()
        .chain(statements.iter().map(|&i| ind(i)))
        .min()
        .or_else(|| {
            // Only continuation lines follow (e.g. `foo(\n    a,\n)`): measure those.
            (1..lines.len())
                .filter(|&i| kinds[i] != LineKind::InString && !blank(i))
                .map(ind)
                .min()
        });
    let Some(base) = base else {
        return (text.to_string(), false);
    };
    let shift = target as isize - base as isize;
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let replaces_prefix = at_line_start && !blank(0);
    let mut block_columns = (first_indent.unwrap_or(base) as isize + shift).max(0) as usize;
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            if replaces_prefix {
                let own = first_indent.unwrap_or(base) as isize;
                out.push(format!(
                    "{}{}",
                    make_indent((own + shift).max(0) as usize, opts.use_tabs, tab),
                    line.trim_start()
                ));
            } else {
                out.push(line.to_string());
            }
            continue;
        }
        if kinds[i] == LineKind::Logical {
            block_columns = (ind(i) as isize + shift).max(0) as usize;
        }
        if kinds[i] == LineKind::InString || (shift == 0 && opts.smart_tabs.is_none()) {
            out.push(line.to_string());
        } else if blank(i) {
            out.push(String::new());
        } else {
            out.push(format!(
                "{}{}",
                if opts.use_tabs && opts.smart_tabs == Some(true) {
                    rustpy_format::TabIndent {
                        width: tab,
                        smart: true,
                    }
                    .render((ind(i) as isize + shift).max(0) as usize, block_columns)
                } else {
                    make_indent(
                        (ind(i) as isize + shift).max(0) as usize,
                        opts.use_tabs,
                        tab,
                    )
                },
                line.trim_start()
            ));
        }
    }
    (out.join("\n"), replaces_prefix)
}

impl EditorState {
    /// Whether Python smart paste applies to `text` in this editor.
    pub(super) fn reindents_paste(&self, text: &str, opts: &EditOptions) -> bool {
        opts.paste_indent_block
            && self.language.is_pythonic()
            && !self.is_large_file()
            && text.len() <= MAX_REINDENT_BYTES
    }

    /// Whether `off` lies inside a string literal (pasting there keeps the text as is).
    pub(super) fn offset_in_string(&mut self, off: TextSize) -> bool {
        if self.language != Language::Python {
            return false;
        }
        let root = self.syntax().syntax_node();
        super::syntax_support::token_at(&root, off).any(|t| {
            matches!(
                t.kind(),
                SyntaxKind::STRING | SyntaxKind::FSTRING_MIDDLE | SyntaxKind::TSTRING_MIDDLE
            ) && t.text_range().start() < off
                && off < t.text_range().end()
        })
    }

    /// Indentation for code placed on the blank line `line`: what Enter after
    /// the previous code line would give, but not deeper than the next code line.
    fn blank_line_indent(&self, line: usize, opts: &EditOptions) -> usize {
        let is_blank = |l: usize| self.doc.line_text(l).trim().is_empty();
        let prev = (0..line).rev().find(|&l| !is_blank(l));
        let next = (line + 1..self.doc.line_count()).find(|&l| !is_blank(l));
        let after_prev = prev.map(|p| {
            let before = self.doc.slice(TextRange::new(
                TextSize::new(0),
                self.doc.line_range(p).end(),
            ));
            indent_after_enter_with_continuation(
                &before,
                opts.tab_size,
                opts.indent_size,
                opts.continuation_indent,
            )
            .map(|e| e.indent)
            .unwrap_or_else(|| indent_width(&self.doc.line_text(p), opts.tab_size))
        });
        let next_indent = next.map(|n| indent_width(&self.doc.line_text(n), opts.tab_size));
        match (after_prev, next_indent) {
            (Some(a), Some(b)) => a.min(b),
            // Nothing follows: go deeper only into a block the previous line
            // opens; otherwise the caret's column 0 stands (top level).
            (Some(a), None) => {
                let prev_indent = prev
                    .map(|p| indent_width(&self.doc.line_text(p), opts.tab_size))
                    .unwrap_or(0);
                if a > prev_indent {
                    a
                } else {
                    0
                }
            }
            (None, b) => b.unwrap_or(0),
        }
    }

    /// Re-indented replacement for pasting `piece` over `range`.
    pub(super) fn reindented_paste(
        &mut self,
        range: TextRange,
        piece: &str,
        opts: &EditOptions,
    ) -> (TextRange, String) {
        let start = range.start();
        let line = self.doc.line_of(start);
        let lr = self.doc.line_range(line);
        let line_text = self.doc.line_text(line);
        let col = usize::from(start - lr.start()).min(line_text.len());
        let prefix = &line_text[..col];
        let at_line_start = prefix.trim().is_empty();
        if (!piece.contains('\n') && !(at_line_start && piece.starts_with([' ', '\t'])))
            || self.offset_in_string(start)
        {
            return (range, piece.to_string());
        }
        let line_blank = line_text.trim().is_empty();
        let target = if !at_line_start {
            indent_width(&line_text, opts.tab_size)
        } else if line_blank && col == 0 {
            self.blank_line_indent(line, opts)
        } else {
            width(prefix, opts.tab_size)
        };
        let (text, replaces_prefix) = shift_block(piece, at_line_start, target, opts);
        if !replaces_prefix {
            return (range, text);
        }
        // The first line brings its own indentation: replace the whitespace
        // before the caret (and the rest of a blank line).
        let end = if line_blank && range.is_empty() {
            lr.end()
        } else {
            range.end()
        };
        (TextRange::new(lr.start(), end), text)
    }

    /// Re-indented text for a whole-line paste inserted above `line`.
    pub(super) fn reindented_line_paste(
        &mut self,
        line: usize,
        text: &str,
        opts: &EditOptions,
    ) -> String {
        let at = self.doc.line_range(line).start();
        if self.offset_in_string(at) {
            return text.to_string();
        }
        let line_text = self.doc.line_text(line);
        let target = if line_text.trim().is_empty() {
            self.blank_line_indent(line, opts)
        } else {
            indent_width(&line_text, opts.tab_size)
        };
        shift_block(text, true, target, opts).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shift(text: &str, at_line_start: bool, target: usize) -> String {
        shift_block(text, at_line_start, target, &EditOptions::default()).0
    }

    #[test]
    fn shifts_whole_blocks_between_levels() {
        // Deep → shallow, with the first line's own indentation.
        assert_eq!(
            shift("        if x:\n            y()\n        z()\n", true, 4),
            "    if x:\n        y()\n    z()\n"
        );
        // Shallow → deep.
        assert_eq!(
            shift("if x:\n    y()\n", true, 8),
            "        if x:\n            y()\n"
        );
        // Copied from the code start: the base is inferred from the body.
        assert_eq!(
            shift("if x:\n            y()\n        z()", true, 0),
            "if x:\n    y()\nz()"
        );
        assert_eq!(
            shift("return a\n    b = 1", true, 8),
            "        return a\n        b = 1"
        );
        // After code on the caret line only the following lines move.
        assert_eq!(
            shift("foo(\n    a,\n)", false, 4),
            "foo(\n        a,\n    )"
        );
        assert_eq!(shift("x = 1", false, 4), "x = 1");
    }

    #[test]
    fn keeps_strings_blank_lines_and_handles_tabs() {
        let src = "        def f():\n            \"\"\"Doc\n  kept as is\n            \"\"\"\n\n            return 1\n";
        assert_eq!(
            shift(src, true, 0),
            "def f():\n    \"\"\"Doc\n  kept as is\n            \"\"\"\n\n    return 1\n"
        );
        let tabs = EditOptions {
            use_tabs: true,
            ..EditOptions::default()
        };
        assert_eq!(
            shift_block("if x:\n    y()", true, 4, &tabs).0,
            "\tif x:\n\t\ty()"
        );
        assert_eq!(
            shift_block("\t\tif x:\n\t\t\ty()", true, 0, &EditOptions::default()).0,
            "if x:\n    y()"
        );
    }
}

#[cfg(test)]
mod smart_tabs_tests {
    use super::*;
    #[test]
    fn actual_indent_block_paste_keeps_alignment_separate_from_block_tabs() {
        let source = "def f():\n\tpass\n";
        for smart in [false, true] {
            let opts = EditOptions {
                use_tabs: true,
                smart_tabs: Some(smart),
                tab_size: 4,
                indent_size: 4,
                ..Default::default()
            };
            let mut state = EditorState::from_text(source, Language::Python);
            state.set_carets(vec![super::super::Caret::new(
                TextSize::new(10),
                TextSize::new(14),
            )]);
            state.paste("return call(\n    α,\n)", &opts);
            let inner = if smart { "\t    " } else { "\t\t" };
            let expected = format!("def f():\n\treturn call(\n{inner}α,\n\t)\n");
            assert_eq!(state.doc.text(), expected);
            assert!(state.undo());
            assert_eq!(state.doc.text(), source);
            assert!(state.redo());
            assert_eq!(state.doc.text(), expected);
            state.doc.set_read_only(true);
            state.paste("BAD", &opts);
            assert_eq!(state.doc.text(), expected);
        }
    }
}
