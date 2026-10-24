//! Complete the current Python logical statement using the editor's syntax tree.
use super::*;
use rustpy_syntax::{SyntaxKind as K, SyntaxNode};

struct Completion {
    edits: Vec<rustpy_text::Edit>,
    caret: TextSize,
}

fn plan(root: &SyntaxNode, line: TextRange, options: &EditOptions) -> Option<Completion> {
    let text = root.text().to_string();
    let tokens: Vec<_> = root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .collect();
    // Leave an interior line of a multiline literal to the ordinary Enter key.
    if tokens.iter().any(|token| {
        matches!(
            token.kind(),
            K::STRING | K::FSTRING_MIDDLE | K::TSTRING_MIDDLE
        ) && token.text_range().start() < line.start()
            && token.text_range().end() > line.end()
    }) {
        return None;
    }
    let current = &text[usize::from(line.start())..usize::from(line.end())];
    let plain_line = current.trim_start().starts_with('#') || current.trim().is_empty();
    let start = if plain_line {
        line.start()
    } else {
        tokens
            .iter()
            .filter(|token| token.kind() == K::NEWLINE && token.text_range().end() <= line.start())
            .map(|token| token.text_range().end())
            .next_back()
            .unwrap_or_default()
    };
    // A statement continued onto later lines is completed after its existing
    // closing brackets, rather than duplicating them on the caret's line.
    let end = if plain_line {
        line.end()
    } else {
        tokens
            .iter()
            .find(|token| token.kind() == K::NEWLINE && token.text_range().start() >= line.end())
            .map_or(TextSize::of(&text), |token| token.text_range().start())
    };
    let mut stack = Vec::new();
    let mut significant = Vec::new();
    let mut top_colon = None;
    for token in tokens
        .iter()
        .filter(|token| token.text_range().start() >= start && token.text_range().end() <= end)
    {
        let kind = token.kind();
        match kind {
            K::WHITESPACE | K::COMMENT | K::INDENT | K::DEDENT | K::NL | K::NEWLINE | K::EOF => {
                continue;
            }
            K::ERROR_TOKEN => return None,
            K::L_PAREN => stack.push(K::R_PAREN),
            K::L_BRACK => stack.push(K::R_BRACK),
            K::L_BRACE => stack.push(K::R_BRACE),
            K::R_PAREN | K::R_BRACK | K::R_BRACE => {
                if stack.pop() != Some(kind) {
                    return None;
                }
            }
            K::COLON if stack.is_empty() => top_colon = Some(significant.len()),
            _ => {}
        }
        significant.push(token);
    }
    let first = significant.first().map(|token| token.kind());
    let compound = matches!(
        first,
        Some(
            K::IF_KW
                | K::ELIF_KW
                | K::ELSE_KW
                | K::FOR_KW
                | K::WHILE_KW
                | K::DEF_KW
                | K::CLASS_KW
                | K::WITH_KW
                | K::TRY_KW
                | K::EXCEPT_KW
                | K::FINALLY_KW
                | K::MATCH_KW
                | K::CASE_KW
        )
    ) || (first == Some(K::ASYNC_KW)
        && significant
            .get(1)
            .is_some_and(|token| matches!(token.kind(), K::DEF_KW | K::FOR_KW | K::WITH_KW)));
    let mut suffix = String::new();
    for close in stack.into_iter().rev() {
        suffix.push(match close {
            K::R_PAREN => ')',
            K::R_BRACK => ']',
            K::R_BRACE => '}',
            _ => unreachable!(),
        });
    }
    if compound && top_colon.is_none() {
        suffix.push(':');
    }
    let body = compound && top_colon.is_none_or(|index| index + 1 == significant.len());
    let indent: String = text[usize::from(start)..]
        .chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .collect();
    let mut next = format!("\n{indent}");
    if body {
        if options.use_tabs {
            next.push('\t');
        } else {
            next.push_str(&" ".repeat(options.indent_size.max(1)));
        }
    }
    let trimmed_end = text[..usize::from(end)]
        .trim_end_matches([' ', '\t', '\r'])
        .len();
    let insertion = significant
        .last()
        .map_or(TextSize::new(trimmed_end as u32), |token| {
            token.text_range().end()
        });
    let caret = TextSize::new((trimmed_end + suffix.len() + next.len()) as u32);
    let mut edits = Vec::new();
    if insertion == TextSize::new(trimmed_end as u32) {
        edits.push(rustpy_text::Edit::replace(
            TextRange::new(insertion, end),
            format!("{suffix}{next}"),
        ));
    } else {
        if !suffix.is_empty() {
            edits.push(rustpy_text::Edit::replace(
                TextRange::empty(insertion),
                suffix,
            ));
        }
        edits.push(rustpy_text::Edit::replace(
            TextRange::new(TextSize::new(trimmed_end as u32), end),
            next,
        ));
    }
    Some(Completion { edits, caret })
}

impl IdeApp {
    pub(super) fn complete_statement(&mut self) {
        let Some(index) = self.active.filter(|index| *index < self.tabs.len()) else {
            return;
        };
        let options = self.edit_options_for(index);
        let state = &mut self.tabs[index].state;
        if state.language != Language::Python || state.is_large_file() {
            return;
        }
        let line = state
            .doc
            .line_range(state.doc.line_of(state.primary().head()));
        let root = state.syntax().syntax_node();
        let Some(completion) = plan(&root, line, &options) else {
            return;
        };
        if state.apply(
            completion.edits,
            vec![crate::editor::Caret::at(completion.caret)],
            rustpy_text::EditGroup::Separate,
            Some("Complete statement"),
        ) {
            state.last_op = crate::editor::state::LastOp::Other;
            self.completion.visible = false;
            self.import_completion = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Caret, EditorState};

    fn complete(source: &str, line: usize, style: rustpy_text::util::IndentStyle) -> EditorState {
        let mut state = EditorState::from_text(source, Language::Python);
        let range = state.doc.line_range(line);
        let mut options = EditOptions::default();
        match style {
            rustpy_text::util::IndentStyle::Tabs => options.use_tabs = true,
            rustpy_text::util::IndentStyle::Spaces(size) => options.indent_size = size,
        }
        let root = state.syntax().syntax_node();
        let completion = plan(&root, range, &options).expect("valid completion");
        assert!(state.apply(
            completion.edits,
            vec![Caret::at(completion.caret)],
            rustpy_text::EditGroup::Separate,
            Some("Complete statement")
        ));
        state
    }

    #[test]
    fn headers_close_before_comments_and_use_file_indentation() {
        for (source, expected) in [
            ("if ready # explanation", "if ready: # explanation\n  "),
            ("if ready: # explanation", "if ready: # explanation\n  "),
            (
                "if check(\"(\\\"#\" # note",
                "if check(\"(\\\"#\"): # note\n  ",
            ),
            ("async for item in values", "async for item in values:\n  "),
        ] {
            let state = complete(source, 0, rustpy_text::util::IndentStyle::Spaces(2));
            assert_eq!(state.doc.text(), expected);
            assert_eq!(usize::from(state.primary().head()), expected.len());
        }
        assert_eq!(
            complete("\tif ready", 0, rustpy_text::util::IndentStyle::Tabs)
                .doc
                .text(),
            "\tif ready:\n\t\t"
        );
    }

    #[test]
    fn existing_multiline_closers_inline_bodies_and_soft_keyword_names_are_preserved() {
        for (source, expected) in [
            (
                "value = call(\n    [1, 2],\n)\nother = 3",
                "value = call(\n    [1, 2],\n)\n\nother = 3",
            ),
            ("if ready: run()", "if ready: run()\n"),
            ("match = call()", "match = call()\n"),
            ("value = '''[ # ]'''", "value = '''[ # ]'''\n"),
        ] {
            let mut state = complete(source, 0, rustpy_text::util::IndentStyle::Spaces(4));
            assert_eq!(state.doc.text(), expected);
            assert!(state.undo());
            assert_eq!(state.doc.text(), source);
            assert!(state.redo());
            assert_eq!(state.doc.text(), expected);
        }
    }

    fn fixture_app(source: &str) -> (tempfile::TempDir, IdeApp) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.py");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        app.open_file(&path);
        app.tabs[0].state.move_doc_end(false);
        (root, app)
    }

    #[test]
    fn actual_action_uses_file_indent_and_remains_one_undo_step() {
        let source = "if ready # keep";
        let (_root, mut app) = fixture_app(source);
        app.tabs[0].state.indent_override = Some(rustpy_text::util::IndentStyle::Spaces(2));
        app.handle_action(Action::CompleteStatement, &egui::Context::default());
        assert_eq!(app.tabs[0].state.doc.text(), "if ready: # keep\n  ");
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert!(app.tabs[0].state.redo());
        assert_eq!(app.tabs[0].state.doc.text(), "if ready: # keep\n  ");
    }

    #[test]
    fn actual_lookup_expands_live_and_postfix_templates() {
        let (_root, mut app) = fixture_app("roadtmpl");
        app.templates.push(LiveTemplate::new(
            "roadtmpl",
            "fixture",
            "print('done')$END$",
            &[TemplateContext::Statement],
            "fixture",
        ));
        app.open_completion(true);
        app.completion.selected = app
            .completion
            .items
            .iter()
            .position(|item| item.kind == CompletionKind::Template && item.label == "roadtmpl")
            .expect("live template in lookup");
        app.accept_completion(false);
        assert_eq!(app.tabs[0].state.doc.text(), "print('done')");
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), "roadtmpl");

        let (_root, mut app) = fixture_app("ready.if");
        app.open_completion(true);
        app.completion.selected = app
            .completion
            .items
            .iter()
            .position(|item| item.kind == CompletionKind::Postfix && item.label == "if")
            .expect("postfix template in lookup");
        app.accept_completion(false);
        assert!(app.tabs[0].state.doc.text().starts_with("if ready:"));
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), "ready.if");
    }
}
