//! Conservative hard wrapping: prose, Python comments and implicit continuations.
use super::state::map_offset;
use super::{Caret, EditOptions, EditorState, Language};
use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::{tokenize, SyntaxKind};
use rustpy_text::util::{char_width, leading_whitespace};
use rustpy_text::{Edit, EditGroup};

impl EditorState {
    pub(super) fn hard_wrap_typed_lines(&mut self, opts: &EditOptions) {
        if opts.right_margin == 0 || self.is_large_file() {
            return;
        }
        let text = self.doc.text();
        let (tokens, _) = tokenize(&text);
        let mut depth = 0usize;
        let mut safe = Vec::new();
        for token in &tokens {
            match token.kind {
                SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_BRACE => depth += 1,
                SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_BRACE => {
                    depth = depth.saturating_sub(1)
                }
                SyntaxKind::WHITESPACE if depth > 0 => safe.push(token.range),
                _ => {}
            }
        }
        let mut lines: Vec<_> = self
            .carets
            .iter()
            .map(|c| self.doc.line_of(c.head()))
            .collect();
        lines.sort_unstable();
        lines.dedup();
        let mut edits = Vec::new();
        for line in lines {
            let range = self.doc.line_range(line);
            let body = self.doc.slice(range);
            let indent = leading_whitespace(&body);
            let start = usize::from(range.start());
            let comment = self.language.is_pythonic()
                && tokens.iter().any(|t| {
                    t.kind == SyntaxKind::COMMENT
                        && usize::from(t.range.start()) == start + indent.len()
                });
            let prose = self.language == Language::PlainText;
            // Directives attach to physical lines, even when trailing Python code.
            if self.language.is_pythonic()
                && tokens.iter().any(|token| {
                    token.kind == SyntaxKind::COMMENT
                        && range.contains(token.range.start())
                        && protected_comment(&self.doc.slice(token.range))
                })
            {
                continue;
            }
            // Interpreter headers must remain on their original physical lines.
            if comment
                && line < 2
                && (body.trim_start().starts_with("#!")
                    || body.contains("coding:")
                    || body.contains("coding="))
            {
                continue;
            }
            if !prose && !comment && !self.language.is_pythonic() {
                continue;
            }
            let prefix = if comment {
                format!("{indent}# ")
            } else if prose {
                indent.to_string()
            } else {
                format!("{indent}{}", " ".repeat(opts.indent_size))
            };
            let min = if comment {
                indent.len() + 1
            } else {
                indent.len()
            };
            let mut segment = 0;
            loop {
                let mut column = if segment == 0 {
                    0
                } else {
                    width(&prefix, opts.tab_size)
                };
                let mut last = None;
                let mut over = false;
                for (offset, ch) in body[segment..].char_indices() {
                    let at = segment + offset;
                    if matches!(ch, ' ' | '\t')
                        && at > min
                        && column <= opts.right_margin
                        && !body[..at].ends_with([' ', '\t'])
                    {
                        let abs = TextSize::new((start + at) as u32);
                        if prose || comment || safe.iter().any(|r| r.contains(abs)) {
                            last = Some(at);
                        }
                    }
                    column += char_width(ch, opts.tab_size.max(1), column);
                    if column > opts.right_margin {
                        over = true;
                        break;
                    }
                }
                if !over {
                    break;
                }
                let Some(at) = last else {
                    break;
                }; // Never split words or Python literals.
                let end = at
                    + body[at..]
                        .chars()
                        .take_while(|c| matches!(c, ' ' | '\t'))
                        .map(char::len_utf8)
                        .sum::<usize>();
                if end == body.len() {
                    break;
                }
                edits.push(Edit::replace(
                    TextRange::new(
                        TextSize::new((start + at) as u32),
                        TextSize::new((start + end) as u32),
                    ),
                    format!("\n{prefix}"),
                ));
                segment = end;
            }
        }
        if edits.is_empty() {
            return;
        }
        edits.sort_by_key(|e| e.range.start());
        let carets = self
            .carets
            .iter()
            .map(|c| {
                Caret::new(
                    map_offset(&edits, c.sel.anchor, true),
                    map_offset(&edits, c.sel.head, true),
                )
            })
            .collect();
        self.apply(edits, carets, EditGroup::Separate, Some("Hard Wrap"));
    }
}
pub(crate) fn protected_comment(comment: &str) -> bool {
    let text = comment.to_ascii_lowercase();
    // Conservatively preserve prose mentioning these markers too: wrapping a
    // line directive can change inspections, type checking, formatting or folds.
    [
        "noqa",
        "noinspection",
        "pyright:",
        "mypy:",
        "type: ignore",
        "pylint:",
        "fmt:",
        "ruff:",
        "editor-fold",
    ]
    .iter()
    .any(|marker| text.contains(marker))
        || text
            .trim_start_matches('#')
            .split_whitespace()
            .next()
            .is_some_and(|word| matches!(word, "region" | "endregion"))
}

fn width(s: &str, tab: usize) -> usize {
    s.chars()
        .fold(0, |col, c| col + char_width(c, tab.max(1), col))
}

/// Nonzero, distinct columns; the effective file margin comes first in the input.
pub fn visual_guide_columns(margin: usize, extra: &[usize]) -> Vec<usize> {
    let mut columns: Vec<_> = std::iter::once(margin)
        .chain(extra.iter().copied())
        .filter(|c| *c > 0)
        .collect();
    columns.sort_unstable();
    columns.dedup();
    columns
}

#[cfg(test)]
mod tests {
    use super::*;
    fn opts(margin: usize) -> EditOptions {
        EditOptions {
            right_margin: margin,
            wrap_on_typing: true,
            ..Default::default()
        }
    }
    fn end(e: &mut EditorState) {
        e.set_carets(vec![Caret::at(e.doc.len())]);
    }
    #[test]
    fn unicode_prose_wrap_is_one_undo_with_exact_caret_and_crlf() {
        let mut e = EditorState::from_text("αβ界 word\r\n", Language::PlainText);
        e.set_carets(vec![Caret::at(TextSize::new("αβ界 word".len() as u32))]);
        let before = e.selections();
        e.insert_text(" last", &opts(12));
        assert_eq!(
            e.doc.to_bytes().unwrap(),
            "αβ界 word\r\nlast\r\n".as_bytes()
        );
        let after = e.selections();
        assert!(e.undo());
        assert_eq!(e.doc.text(), "αβ界 word\n");
        assert_eq!(e.selections(), before);
        assert!(e.redo());
        assert_eq!(e.selections(), after);
    }
    #[test]
    fn python_comments_continue_prefix_but_strings_and_statements_remain_intact() {
        let mut e = EditorState::from_text("    # alpha beta", Language::Python);
        end(&mut e);
        e.insert_text(" gamma", &opts(20));
        assert_eq!(e.doc.text(), "    # alpha beta\n    # gamma");
        for text in [
            "value = 'alpha beta",
            "alpha = beta + gamma",
            "\"\"\"\n# alpha beta",
        ] {
            let mut e = EditorState::from_text(text, Language::Python);
            end(&mut e);
            e.insert_text(" delta", &opts(12));
            assert_eq!(e.doc.text(), format!("{text} delta"));
        }
    }
    #[test]
    fn python_implicit_continuation_wraps_at_token_whitespace() {
        let mut e = EditorState::from_text("values = [alpha, beta, gamm]", Language::Python);
        e.set_carets(vec![Caret::at(e.doc.len() - TextSize::new(1))]);
        e.insert_text("a", &opts(20));
        assert_eq!(e.doc.text(), "values = [alpha,\n    beta, gamma]");
        assert!(rustpy_syntax::parse(&e.doc.text()).ok());
        let meaningful = |text: &str| {
            tokenize(text)
                .0
                .into_iter()
                .filter(|t| {
                    !t.kind.is_trivia()
                        && !matches!(
                            t.kind,
                            SyntaxKind::NEWLINE
                                | SyntaxKind::INDENT
                                | SyntaxKind::DEDENT
                                | SyntaxKind::EOF
                        )
                })
                .map(|t| {
                    (
                        t.kind,
                        text[usize::from(t.range.start())..usize::from(t.range.end())].to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            meaningful(&e.doc.text()),
            meaningful("values = [alpha, beta, gamma]")
        );
        assert!(e.undo());
        assert_eq!(e.doc.text(), "values = [alpha, beta, gamm]");
    }
    #[test]
    fn tabs_multiple_carets_disabled_and_read_only() {
        let mut e = EditorState::from_text("\tone two\n\tone two", Language::PlainText);
        e.set_carets(vec![Caret::at(TextSize::new(8)), Caret::at(e.doc.len())]);
        e.insert_text(" three", &opts(12));
        assert_eq!(e.doc.text(), "\tone two\n\tthree\n\tone two\n\tthree");
        let mut e = EditorState::from_text("one two", Language::PlainText);
        end(&mut e);
        e.insert_text(
            " three",
            &EditOptions {
                right_margin: 5,
                ..Default::default()
            },
        );
        assert_eq!(e.doc.text(), "one two three");
        e.doc.set_read_only(true);
        let before = e.selections();
        e.insert_text(" four", &opts(5));
        assert_eq!(e.selections(), before);
        assert!(e.take_read_only_refusal());
    }
    #[test]
    fn zero_margin_long_words_and_visual_columns() {
        for margin in [0, 4] {
            let mut e = EditorState::from_text("longword", Language::PlainText);
            end(&mut e);
            e.insert_text("x", &opts(margin));
            assert_eq!(e.doc.text(), "longwordx");
        }
        assert_eq!(
            visual_guide_columns(79, &[120, 0, 79, 80, 80]),
            vec![79, 80, 120]
        );
    }
    #[test]
    fn physical_line_directives_are_preserved_while_comment_prose_wraps() {
        for text in [
            "# noqa: E501 a long explanation that must remain attached",
            "# type: ignore[assignment] a long explanation of the suppression",
            "# pylint: disable=invalid-name a long explanation",
            "# fmt: skip a deliberately formatted long line",
            "# ruff: noqa: E501 a long suppression explanation",
            "#region a long named folding section that must stay intact",
            "# endregion a long named folding section",
            "# <editor-fold desc=\"a long named section\">",
            "values = [alpha, beta, gamma]  # noqa: E501",
        ] {
            let mut e = EditorState::from_text(text, Language::Python);
            end(&mut e);
            e.insert_text(" more", &opts(20));
            assert_eq!(e.doc.text(), format!("{text} more"));
        }
        let mut prose = EditorState::from_text("# ordinary comment prose", Language::Python);
        end(&mut prose);
        prose.insert_text(" continues", &opts(20));
        assert_eq!(prose.doc.text(), "# ordinary comment\n# prose continues");
    }
}
