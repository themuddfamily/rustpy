//! Code blocks in model replies, for the "Insert at caret" / "Replace selection" actions.

use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};

/// The fenced code blocks of a Markdown reply, in order, as `(language, code)`.
///
/// Parsing follows CommonMark (via pulldown-cmark): backtick and tilde fences,
/// fences nested in lists or block quotes, and longer fences around shorter ones
/// are handled; a fence left open runs to the end of the text, so a reply that
/// is still streaming yields its partial last block. The language is the first
/// word of the info string (`"python"` for ```` ```python title="x.py" ````), or
/// `None` when the fence has no info string. The code has no trailing line break.
/// Indented (unfenced) code blocks are not returned.
pub fn extract_code_blocks(markdown: &str) -> Vec<(Option<String>, String)> {
    let mut blocks = Vec::new();
    let mut current: Option<(Option<String>, String)> = None;
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                current = Some((info_language(&info), String::new()));
            }
            Event::Text(text) => {
                if let Some((_, code)) = current.as_mut() {
                    code.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((lang, mut code)) = current.take() {
                    if code.ends_with('\n') {
                        code.pop();
                        if code.ends_with('\r') {
                            code.pop();
                        }
                    }
                    blocks.push((lang, code));
                }
            }
            _ => {}
        }
    }
    blocks
}

/// The content of a reply that consists of a single fenced code block (models
/// sometimes wrap a docstring or a commit message in one); any other reply is
/// returned trimmed but otherwise unchanged.
pub fn strip_code_fence(reply: &str) -> String {
    let trimmed = reply.trim();
    let mut fenced: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let mut in_block: Option<(std::ops::Range<usize>, String)> = None;
    for (event, range) in Parser::new(trimmed).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => {
                in_block = Some((range, String::new()))
            }
            Event::Text(text) => {
                if let Some((_, code)) = in_block.as_mut() {
                    code.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => fenced.extend(in_block.take()),
            _ => {}
        }
    }
    match fenced.as_slice() {
        [(range, code)] if range.start == 0 && trimmed[range.end..].trim().is_empty() => {
            code.trim_end_matches(['\r', '\n']).to_string()
        }
        _ => trimmed.to_string(),
    }
}

fn info_language(info: &str) -> Option<String> {
    let first = info
        .split(|c: char| c.is_whitespace() || c == ',')
        .next()
        .unwrap_or("");
    let lang = first.trim_matches(|c: char| c == '{' || c == '}' || c == '.');
    (!lang.is_empty()).then(|| lang.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(lang: Option<&str>, code: &str) -> (Option<String>, String) {
        (lang.map(str::to_string), code.to_string())
    }

    #[test]
    fn extracts_blocks_with_languages() {
        let md = "Here is the fix:\n\n```python\ndef f(x):\n    return x + 1\n```\n\nAnd a shell command:\n\n~~~bash\npip install requests\n~~~\n\nPlain:\n\n```\nno language\n```\n";
        assert_eq!(
            extract_code_blocks(md),
            vec![
                block(Some("python"), "def f(x):\n    return x + 1"),
                block(Some("bash"), "pip install requests"),
                block(None, "no language"),
            ]
        );
    }

    #[test]
    fn info_string_attributes_and_nested_fences() {
        let md = "````markdown title=\"README.md\"\nUse:\n```python\nprint(1)\n```\n````\n\n```{.python}\nx = 1\n```\n\n```py,linenos\ny = 2\n```";
        assert_eq!(
            extract_code_blocks(md),
            vec![
                block(Some("markdown"), "Use:\n```python\nprint(1)\n```"),
                block(Some("python"), "x = 1"),
                block(Some("py"), "y = 2"),
            ]
        );
    }

    #[test]
    fn blocks_in_lists_and_quotes_lose_their_container_indent() {
        let md = "1. Change this:\n\n   ```python\n   if x:\n       pass\n   ```\n\n> ```js\n> let a = 1;\n> ```\n";
        assert_eq!(
            extract_code_blocks(md),
            vec![
                block(Some("python"), "if x:\n    pass"),
                block(Some("js"), "let a = 1;")
            ]
        );
    }

    #[test]
    fn unclosed_fence_runs_to_the_end_and_empty_blocks_are_kept() {
        assert_eq!(
            extract_code_blocks("Partial:\n```python\nimport os\nos.getcwd("),
            vec![block(Some("python"), "import os\nos.getcwd(")]
        );
        assert_eq!(
            extract_code_blocks("```python\n```"),
            vec![block(Some("python"), "")]
        );
        assert_eq!(
            extract_code_blocks("```rust\r\nfn main() {}\r\n```\r\n"),
            vec![block(Some("rust"), "fn main() {}")]
        );
    }

    #[test]
    fn ignores_indented_code_and_inline_code() {
        assert!(extract_code_blocks("Use `print()` here.\n\n    indented = True\n").is_empty());
        assert!(extract_code_blocks("").is_empty());
    }

    #[test]
    fn strip_code_fence_only_unwraps_a_lone_block() {
        assert_eq!(
            strip_code_fence("```python\n\"\"\"Return the sum.\"\"\"\n```\n"),
            "\"\"\"Return the sum.\"\"\""
        );
        assert_eq!(
            strip_code_fence("\n\n```\nfeat(ai): add client\n\nBody text.\n```"),
            "feat(ai): add client\n\nBody text."
        );
        assert_eq!(
            strip_code_fence("  fix(ui): wrap long lines \n"),
            "fix(ui): wrap long lines"
        );
        let mixed = "Here you go:\n```python\nx = 1\n```";
        assert_eq!(strip_code_fence(mixed), mixed);
        let two = "```\na\n```\n```\nb\n```";
        assert_eq!(strip_code_fence(two), two);
        let trailing = "```\na\n```\nThat's it.";
        assert_eq!(strip_code_fence(trailing), trailing);
    }
}
