//! RustPy IDE — Python lexer, parser and lossless syntax tree (roadmap §2.1).

pub mod ast;
pub mod doctest;
pub mod exports;
pub mod ini;
pub mod lexer;
pub mod parser;
pub mod printer;
pub mod reparse;
pub mod rst;
pub mod string_literal;
pub mod syntax_kind;
pub mod template;
pub mod template_builtin;
pub mod template_expression;
pub mod template_final_tags;
pub mod template_statement;
pub mod yaml;

pub use lexer::{tokenize, Token};
pub use parser::{parse, parse_expression, Parse};
pub use reparse::reparse;
pub use rowan;
pub use rustpy_core::{TextRange, TextSize};
pub use syntax_kind::{PythonLanguage, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

pub mod rst_editing;

pub mod template_alternate;

/// [`SyntaxNode::token_at_offset`] in logarithmic time. Rowan's version
/// scans the children of every node on the way down, so a module with
/// thousands of top-level statements costs thousands of steps per lookup
/// (per diagnostic, in a per-problem loop); this one finds the tokens left
/// and right of `offset` by binary search ([`SyntaxNode::covering_element`]).
pub fn token_at_offset(node: &SyntaxNode, offset: TextSize) -> rowan::TokenAtOffset<SyntaxToken> {
    use rowan::{NodeOrToken, TokenAtOffset};
    let range = node.text_range();
    assert!(
        range.start() <= offset && offset <= range.end(),
        "Bad offset: range {range:?} offset {offset:?}"
    );
    let one = TextSize::from(1);
    // The token covering the byte before / at `offset`: tokens are
    // non-empty and char-aligned, so a one-byte range lies in exactly one.
    let covering = |start: TextSize| match node.covering_element(TextRange::at(start, one)) {
        NodeOrToken::Token(token) => Some(token),
        NodeOrToken::Node(inner) => inner.token_at_offset(start).right_biased(),
    };
    let left = (offset > range.start())
        .then(|| covering(offset - one))
        .flatten();
    let right = (offset < range.end()).then(|| covering(offset)).flatten();
    match (left, right) {
        (Some(left), Some(right)) if left == right => TokenAtOffset::Single(left),
        (Some(left), Some(right)) => TokenAtOffset::Between(left, right),
        (Some(token), None) | (None, Some(token)) => TokenAtOffset::Single(token),
        (None, None) => TokenAtOffset::None,
    }
}

#[cfg(test)]
mod token_at_offset_tests {
    use super::*;

    #[test]
    fn matches_rowan_at_every_offset() {
        let text = "import os\nx = 1  # é comment\n\ndef f(a, b):\n    return a + 'ü'\n\n\nclass C:\n    pass\n";
        let root = parse(text).syntax_node();
        for offset in 0..=text.len() {
            if !text.is_char_boundary(offset) {
                continue;
            }
            let offset = TextSize::from(offset as u32);
            let expected: Vec<_> = root.token_at_offset(offset).collect();
            let actual: Vec<_> = token_at_offset(&root, offset).collect();
            assert_eq!(actual, expected, "offset {offset:?}");
        }
    }
}
