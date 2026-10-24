//! Fill prose paragraphs without treating executable Python as documentation.
use super::*;
use rustpy_text::util::{char_width, leading_whitespace};

fn columns(text: &str, tab: usize) -> usize {
    text.chars()
        .fold(0, |column, ch| column + char_width(ch, tab.max(1), column))
}

fn prose(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && !text.ends_with(':')
        && !text.contains('\\')
        && !text.starts_with([':', '@', '#', '-', '*', '+', '|', '='])
        && !text.starts_with(">>>")
        && !text.starts_with("...")
        && !text.starts_with("```")
        && !text.starts_with(".. ")
        && !text.split_whitespace().next().is_some_and(|word| {
            word.trim_end_matches(['.', ')'])
                .chars()
                .all(|ch| ch.is_ascii_digit())
        })
}

fn wrapped(words: &[&str], first: &str, continuation: &str, margin: usize, tab: usize) -> String {
    let mut result = first.to_string();
    let mut column = columns(first, tab);
    let mut has_word = false;
    for word in words {
        let width = columns(word, tab);
        if has_word && column + 1 + width > margin {
            result.push('\n');
            result.push_str(continuation);
            column = columns(continuation, tab);
            has_word = false;
        }
        if has_word {
            result.push(' ');
            column += 1;
        }
        result.push_str(word);
        column += width;
        has_word = true;
    }
    result
}

impl IdeApp {
    pub(super) fn fill_paragraph(&mut self) {
        let Some(index) = self.active.filter(|index| *index < self.tabs.len()) else {
            return;
        };
        let options = self.edit_options_for(index);
        if options.right_margin == 0 {
            self.set_status("Set a right margin to fill paragraphs");
            return;
        }
        let state = &mut self.tabs[index].state;
        if state.is_large_file() {
            return;
        }
        let caret = state.primary().head();
        let line = state.doc.line_of(caret);
        let current = state.doc.line_text(line);
        let indent = leading_whitespace(&current).to_string();
        let document = if state.language == Language::Python {
            crate::editor::doc_render::region_at_line(state, line)
        } else {
            None
        };
        let comment_starts: std::collections::HashSet<_> = if state.language.is_pythonic() {
            state
                .syntax()
                .syntax_node()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| token.kind() == rustpy_syntax::SyntaxKind::COMMENT)
                .map(|token| token.text_range().start())
                .collect()
        } else {
            Default::default()
        };
        let comment = document.is_none()
            && comment_starts.contains(
                &(state.doc.line_range(line).start() + TextSize::new(indent.len() as u32)),
            );
        if document.is_none() && !comment && state.language != Language::PlainText {
            self.set_status("Place the caret inside a comment or docstring paragraph");
            return;
        }
        let literal = document
            .as_ref()
            .map(|region| region.literal.as_ref())
            .unwrap_or("");
        let literal_prefix = literal.bytes().take_while(u8::is_ascii_alphabetic).count();
        let quote_width = if literal
            .get(literal_prefix..)
            .is_some_and(|text| text.starts_with("\"\"\"") || text.starts_with("'''"))
        {
            3
        } else {
            1
        };
        let body = document.as_ref().map(|region| {
            TextRange::new(
                region.start + TextSize::new((literal_prefix + quote_width) as u32),
                region.end - TextSize::new(quote_width as u32),
            )
        });
        let base_indent = document
            .as_ref()
            .map(|region| leading_whitespace(&state.doc.line_text(region.first_line)).to_string())
            .unwrap_or_else(|| indent.clone());
        let line_body = |line: usize| -> Option<(TextRange, String)> {
            let line_range = state.doc.line_range(line);
            let text = state.doc.line_text(line);
            let indentation = leading_whitespace(&text);
            if indentation != base_indent {
                return None;
            }
            let range = if let Some(body) = body {
                line_range.intersect(body)?
            } else if comment {
                let at = line_range.start() + TextSize::new(indentation.len() as u32);
                if !comment_starts.contains(&at)
                    || crate::editor::hard_wrap::protected_comment(text.trim_start())
                {
                    return None;
                }
                TextRange::new(at + TextSize::new(1), line_range.end())
            } else {
                line_range
            };
            let content = state.doc.slice(range);
            prose(&content).then_some((range, content))
        };
        if line_body(line).is_none() {
            return;
        }
        let (mut first, mut last) = (line, line);
        while first > 0 && line_body(first - 1).is_some() {
            first -= 1;
        }
        while last + 1 < state.doc.line_count() && line_body(last + 1).is_some() {
            last += 1;
        }
        let pieces: Vec<_> = (first..=last).filter_map(line_body).collect();
        let words: Vec<_> = pieces
            .iter()
            .flat_map(|(_, text)| text.split_whitespace())
            .collect();
        let prefix = if comment {
            format!("{base_indent}# ")
        } else {
            base_indent.clone()
        };
        let start = if body.is_some() {
            pieces[0].0.start()
        } else {
            state.doc.line_range(first).start()
        };
        let end = pieces.last().unwrap().0.end();
        let opening_line = document
            .as_ref()
            .is_some_and(|region| first == region.first_line);
        let initial = if opening_line { "" } else { prefix.as_str() };
        // Opening quotes already occupy columns outside the replacement range.
        let first_margin = if opening_line {
            options.right_margin.saturating_sub(columns(
                &state
                    .doc
                    .slice(TextRange::new(state.doc.line_range(first).start(), start)),
                options.tab_size,
            ))
        } else {
            options.right_margin
        };
        let mut replacement = wrapped(
            &words,
            initial,
            &prefix,
            first_margin.max(columns(&prefix, options.tab_size) + 1),
            options.tab_size,
        );
        let mut range = TextRange::new(start, end);
        if document.is_some() && quote_width == 1 && replacement.contains('\n') {
            let Some(region) = document.as_ref() else {
                return;
            };
            if region.first_line != region.last_line {
                return;
            }
            let quote = &literal[literal_prefix..literal_prefix + 1];
            let body = wrapped(
                &words,
                &prefix,
                &prefix,
                options.right_margin,
                options.tab_size,
            );
            replacement = format!(
                "{}{}\n{body}\n{base_indent}{}",
                &literal[..literal_prefix],
                quote.repeat(3),
                quote.repeat(3)
            );
            range = TextRange::new(region.start, region.end);
        }
        if state.doc.slice(range) == replacement {
            return;
        }
        let new_caret = range.start();
        state.apply(
            vec![rustpy_text::Edit::replace(range, replacement)],
            vec![crate::editor::Caret::at(new_caret)],
            rustpy_text::EditGroup::Separate,
            Some("Fill paragraph"),
        );
        state.last_op = crate::editor::state::LastOp::Other;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(source: &str, caret: &str) -> IdeApp {
        let mut app = IdeApp::new(Settings::default(), None);
        app.settings.editor.right_margin = 26;
        app.push_tab(EditorState::from_text(source, Language::Python));
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(
                source.find(caret).unwrap() as u32,
            ))]);
        app
    }
    #[test]
    fn ordinary_code_and_non_docstring_literals_are_never_reflowed() {
        for (source, caret) in [
            ("first = 1\nsecond = 2\n", "first"),
            (
                "value = '''\nalpha beta gamma delta epsilon zeta eta\n'''\n",
                "alpha",
            ),
            (
                "# noinspection PyUnresolvedReferences alpha beta gamma delta\nunknown()\n",
                "noinspection",
            ),
        ] {
            let mut app = fixture(source, caret);
            let history = app.tabs[0].state.doc.current_history();
            app.fill_paragraph();
            assert_eq!(app.tabs[0].state.doc.text(), source);
            assert_eq!(app.tabs[0].state.doc.current_history(), history);
        }
    }
    #[test]
    fn comment_and_docstring_paragraphs_reflow_without_touching_code_or_sections() {
        for (source, caret) in [
            (
                "# alpha beta gamma delta epsilon zeta eta theta\nvalue = 1\n",
                "alpha",
            ),
            (
                "def f():\n    \"\"\"\n    alpha beta gamma delta epsilon zeta eta theta\n\n    Args:\n        value: unchanged description\n    \"\"\"\n    return 1\n",
                "alpha",
            ),
            (
                "def f():\n    'alpha beta gamma delta epsilon zeta eta theta'\n    return 1\n",
                "alpha",
            ),
        ] {
            let mut app = fixture(source, caret);
            app.fill_paragraph();
            let result = app.tabs[0].state.doc.text();
            assert_ne!(result, source);
            assert!(rustpy_syntax::parse(&result).ok(), "{result}");
            assert_eq!(
                result.contains("    return 1\n"),
                source.contains("    return 1\n")
            );
            if source.contains("Args:") {
                assert!(result.contains("    Args:\n        value: unchanged description"));
            }
            assert!(app.tabs[0].state.undo());
            assert_eq!(app.tabs[0].state.doc.text(), source);
        }
    }
}
