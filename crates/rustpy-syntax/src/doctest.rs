//! Source-owned doctest prompts, shared by gutter, injection and completion.
use crate::{
    ast::{self, AstNode},
    Parse, SyntaxKind, TextRange, TextSize,
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExampleLine {
    pub prompt: TextRange,
    pub code: TextRange,
    pub primary: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocTest {
    pub literal: TextRange,
    pub qualified: String,
    pub lines: Vec<ExampleLine>,
    pub raw: bool,
}
pub fn discover(source: &str, parse: &Parse) -> Vec<DocTest> {
    if source.len() > 1024 * 1024 {
        return vec![];
    }
    let mut result = Vec::new();
    for node in parse.syntax_node().descendants() {
        let (doc, qualified) = if let Some(function) = ast::FuncDef::cast(node.clone()) {
            if function
                .syntax()
                .ancestors()
                .skip(1)
                .any(|n| n.kind() == SyntaxKind::FUNC_DEF)
            {
                continue;
            }
            let mut names: Vec<_> = function
                .syntax()
                .ancestors()
                .filter_map(ast::ClassDef::cast)
                .map(|c| c.name_text())
                .collect();
            names.reverse();
            names.push(function.name_text());
            (function.docstring(), names.join("."))
        } else if let Some(class) = ast::ClassDef::cast(node.clone()) {
            if class
                .syntax()
                .ancestors()
                .skip(1)
                .any(|n| n.kind() == SyntaxKind::FUNC_DEF)
            {
                continue;
            }
            let mut names: Vec<_> = class
                .syntax()
                .ancestors()
                .filter_map(ast::ClassDef::cast)
                .map(|c| c.name_text())
                .collect();
            names.reverse();
            (class.docstring(), names.join("."))
        } else if let Some(module) = ast::Module::cast(node) {
            (module.docstring(), "<module>".into())
        } else {
            continue;
        };
        let Some(ast::StringLike::Single(string)) = doc else {
            continue;
        };
        if string.is_bytes() {
            continue;
        }
        let literal = string.text_range();
        let range = string.content_range();
        let token = &source[usize::from(literal.start())..usize::from(literal.end())];
        let prefix = token.bytes().take_while(u8::is_ascii_alphabetic).count();
        let raw = token[..prefix].contains(['r', 'R']);
        let mut offset = usize::from(range.start());
        let mut lines = Vec::new();
        let mut active = false;
        let mut bytes = 0usize;
        for line in
            source[usize::from(range.start())..usize::from(range.end())].split_inclusive('\n')
        {
            let physical = line.trim_end_matches(['\r', '\n']);
            let trimmed = physical.trim_start();
            let indent = physical.len() - trimmed.len();
            let primary = trimmed.starts_with(">>> ") || trimmed == ">>>";
            let continuation = active && (trimmed.starts_with("... ") || trimmed == "...");
            if primary || continuation {
                let start = offset + indent;
                let code = start + 3 + usize::from(trimmed.as_bytes().get(3) == Some(&b' '));
                bytes += physical.len();
                if lines.len() >= 4096 || bytes > 128 * 1024 {
                    lines.clear();
                    break;
                }
                lines.push(ExampleLine {
                    prompt: TextRange::new(TextSize::new(start as u32), TextSize::new(code as u32)),
                    code: TextRange::new(
                        TextSize::new(code as u32),
                        TextSize::new((offset + physical.len()) as u32),
                    ),
                    primary,
                });
                active = true;
            } else {
                active = false;
            }
            offset += line.len();
        }
        if lines.iter().any(|line| line.primary) {
            result.push(DocTest {
                literal,
                qualified,
                lines,
                raw,
            });
        }
    }
    result
}
/// Decode host escapes in the isolated Python query. The replacement prefix
/// must retain identical identifier bytes; an encoded identifier is refused
/// rather than replacing part of its host escape spelling.
pub fn completion_source(
    source: &str,
    test: &DocTest,
    caret: TextSize,
) -> Option<(String, TextSize, TextRange)> {
    let line = test
        .lines
        .iter()
        .find(|line| line.code.contains_inclusive(caret))?;
    let mut text = String::with_capacity(source.len() + 128 * 1024);
    text.push_str(source);
    text.push('\n');
    for prior in &test.lines {
        let start = usize::from(prior.code.start());
        let end = usize::from(prior.code.end());
        if prior.code.start() == line.code.start() {
            let host_before = &source[start..usize::from(caret)];
            let decoded_before = crate::string_literal::decode_text(host_before, test.raw).ok()?;
            let prefix = |value: &str| {
                value
                    .rsplit(|c: char| !rustpy_core::is_ident_continue(c))
                    .next()
                    .unwrap_or("")
                    .to_string()
            };
            if prefix(host_before) != prefix(&decoded_before) {
                return None;
            }
            let target = text.len() + decoded_before.len();
            text.push_str(&crate::string_literal::decode_text(&source[start..end], test.raw).ok()?);
            text.push('\n');
            return Some((text, TextSize::new(target as u32), line.code));
        }
        text.push_str(&crate::string_literal::decode_text(&source[start..end], test.raw).ok()?);
        text.push('\n');
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_ownership_multiline_unicode_and_output_are_exact() {
        let text="label = '''>>> fake()'''\r\ndef f():\r\n    r\"\"\"Doc.\r\n    >>> values = ['π']\r\n    >>> for value in values:\r\n    ...     print(value)\r\n    π\r\n    >>> value.up\r\n    \"\"\"\r\n";
        let tests = discover(text, &crate::parse(text));
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].qualified, "f");
        assert_eq!(tests[0].lines.len(), 4);
        let caret = TextSize::new((text.find("value.up").unwrap() + 8) as u32);
        let (query, at, _) = completion_source(text, &tests[0], caret).unwrap();
        assert!(
            query.ends_with("values = ['π']\nfor value in values:\n    print(value)\nvalue.up\n")
        );
        assert_eq!(&query[usize::from(at) - 2..usize::from(at)], "up");
    }
    #[test]
    fn no_output_comment_or_nested_runtime_local_can_become_a_prompt() {
        let text="# >>> comment\nvalue='>>> string'\ndef outer():\n    def inner():\n        \"\"\">>> hidden()\"\"\"\n";
        assert!(discover(text, &crate::parse(text)).is_empty());
    }
}
