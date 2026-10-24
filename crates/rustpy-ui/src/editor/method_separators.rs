//! Python definition separators derived from the editor's cached syntax tree.
//! A separator belongs above the first decorator (or definition header), never
//! between decorators and their definition. Nested definitions get their own
//! boundary; the renderer may use `depth` for indentation. No separator is
//! useful above physical line zero. Comments and strings are not definitions.

use super::{EditorState, Language};
use rustpy_core::TextSize;
use rustpy_syntax::{ast, ast::AstNode, SyntaxKind};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DefinitionKind {
    Function,
    AsyncFunction,
    Class,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MethodSeparator {
    /// Byte position of the first decorator/header keyword in the document.
    pub offset: TextSize,
    /// Zero-based physical line; map through visible/folded rows when painting.
    pub line: usize,
    /// Number of enclosing class/function definitions (not arbitrary blocks).
    pub depth: usize,
    pub kind: DefinitionKind,
}

/// Get current boundaries without reparsing independently of EditorState.
/// Plain-text and large-file modes deliberately produce no separators.
pub fn boundaries(state: &mut EditorState) -> Vec<MethodSeparator> {
    if state.language != Language::Python || state.is_large_file() {
        return Vec::new();
    }
    // The cheap Parse clone lets Document line mapping borrow independently.
    let parse = state.syntax().clone();
    let mut result = Vec::new();
    for node in parse.syntax_node().descendants() {
        let (kind, keyword) = match node.kind() {
            SyntaxKind::FUNC_DEF => {
                let Some(function) = ast::FuncDef::cast(node.clone()) else {
                    continue;
                };
                // Parser recovery also creates FUNC_DEF for a lone decorator.
                // Require an actual header keyword and name before painting.
                if function.name().is_none() {
                    continue;
                }
                (
                    if function.is_async() {
                        DefinitionKind::AsyncFunction
                    } else {
                        DefinitionKind::Function
                    },
                    SyntaxKind::DEF_KW,
                )
            }
            SyntaxKind::CLASS_DEF => {
                let Some(class) = ast::ClassDef::cast(node.clone()) else {
                    continue;
                };
                if class.name().is_none() {
                    continue;
                }
                (DefinitionKind::Class, SyntaxKind::CLASS_KW)
            }
            _ => continue,
        };
        let direct_tokens: Vec<_> = node
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .collect();
        let Some(header) = direct_tokens.iter().find(|t| t.kind() == keyword) else {
            continue;
        };
        let decorator = node
            .children()
            .find(|n| n.kind() == SyntaxKind::DECORATORS)
            .and_then(|n| {
                n.descendants_with_tokens()
                    .filter_map(|e| e.into_token())
                    .find(|t| t.kind() == SyntaxKind::AT)
            });
        let offset = decorator
            .map(|t| t.text_range().start())
            .unwrap_or_else(|| {
                direct_tokens
                    .iter()
                    .find(|t| t.kind() == SyntaxKind::ASYNC_KW)
                    .unwrap_or(header)
                    .text_range()
                    .start()
            });
        let line = state.doc.line_of(offset);
        if line == 0 {
            continue;
        }
        let depth = node
            .ancestors()
            .skip(1)
            .filter(|n| matches!(n.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF))
            .count();
        result.push(MethodSeparator {
            offset,
            line,
            depth,
            kind,
        });
    }
    result.sort_unstable_by_key(|boundary| boundary.offset);
    result.dedup_by_key(|boundary| boundary.line);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_text::{Edit, EditGroup, TextRange};

    fn state(source: &str) -> EditorState {
        EditorState::from_text(source, Language::Python)
    }

    #[test]
    fn genuine_sync_async_class_and_nested_definitions_have_ordered_boundaries() {
        let mut editor = state(
            "# module\ndef outer():\n    def inner(): pass\nclass Example:\n    async def method(self): pass\nasync def later(): pass\n",
        );
        let got = boundaries(&mut editor);
        assert_eq!(
            got.iter()
                .map(|b| (b.line, b.depth, b.kind))
                .collect::<Vec<_>>(),
            vec![
                (1, 0, DefinitionKind::Function),
                (2, 1, DefinitionKind::Function),
                (3, 0, DefinitionKind::Class),
                (4, 1, DefinitionKind::AsyncFunction),
                (5, 0, DefinitionKind::AsyncFunction),
            ]
        );
        assert!(editor
            .doc
            .slice(TextRange::new(
                got[4].offset,
                got[4].offset + TextSize::new(5)
            ))
            .starts_with("async"));
    }

    #[test]
    fn multiline_decorators_attach_to_one_boundary_above_the_first_at() {
        let mut editor = state(
            "# module\n@decorate(\n    value=1,\n)\n@other\nasync def method(): pass\n\n@register\nclass Example: pass\n",
        );
        let got = boundaries(&mut editor);
        assert_eq!(
            got.iter().map(|b| (b.line, b.kind)).collect::<Vec<_>>(),
            vec![
                (1, DefinitionKind::AsyncFunction),
                (7, DefinitionKind::Class)
            ]
        );
        for boundary in got {
            assert_eq!(editor.doc.char_at(boundary.offset), Some('@'));
        }
    }

    #[test]
    fn comments_strings_lambdas_and_dangling_decorators_are_not_headers() {
        let mut editor = state(
            "# def false():\ntext = '''\ndef fake(): pass\nclass Fake: pass\n'''\nfn = lambda x: x\n@dangling\n",
        );
        assert!(boundaries(&mut editor).is_empty());
    }

    #[test]
    fn cached_parse_refreshes_after_real_edit_undo_and_redo() {
        let mut editor = state("# π🙂\nvalue = 1\n");
        assert!(boundaries(&mut editor).is_empty());
        let start = editor.doc.line_range(1).start();
        let selections = editor.selections();
        editor
            .doc
            .edit(
                vec![Edit::replace(
                    TextRange::new(start, editor.doc.len()),
                    "def changed(): pass\n",
                )],
                &selections,
                &selections,
                EditGroup::Separate,
                None,
            )
            .unwrap();
        let after = boundaries(&mut editor);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].offset, start);
        assert_eq!(after[0].line, 1);
        assert!(editor.undo());
        assert!(boundaries(&mut editor).is_empty());
        assert!(editor.redo());
        assert_eq!(boundaries(&mut editor), after);
    }

    #[test]
    fn partial_named_definition_is_visible_but_file_start_has_no_separator() {
        let mut editor = state("def first(): pass\n\ndef typed(");
        let got = boundaries(&mut editor);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].line, 2);
        assert_eq!(got[0].kind, DefinitionKind::Function);
    }

    #[test]
    fn non_python_and_large_modes_exclude_boundaries_before_requesting_syntax() {
        let mut editor =
            EditorState::from_text("# header\ndef fake(): pass\n", Language::PlainText);
        assert!(boundaries(&mut editor).is_empty());
        let text = format!(
            "# {}\ndef real(): pass\n",
            "x".repeat(EditorState::LARGE_FILE_BYTES)
        );
        let mut editor = state(&text);
        assert!(editor.is_large_file());
        assert!(boundaries(&mut editor).is_empty());
    }
}
