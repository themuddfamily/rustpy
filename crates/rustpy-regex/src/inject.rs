//! Discovery of regex-injected string literals in a parsed Python file.

use rustpy_core::TextRange;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{Parse, SyntaxKind};

/// A string literal that holds a regular expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Injection {
    /// Range of the whole string literal (prefix, quotes and content).
    pub string_range: TextRange,
    /// Range of the raw content between the quotes.
    pub value_range: TextRange,
    /// The pattern value: raw strings verbatim, normal strings with escapes decoded.
    pub pattern: String,
    /// `re.VERBOSE` / `re.X` was passed to the call.
    pub verbose: bool,
    /// The injecting call (`re.compile`, `re.sub`, …) or `# language=regex`.
    pub call: String,
}

const RE_FUNCTIONS: &[&str] = &[
    "compile",
    "match",
    "search",
    "fullmatch",
    "findall",
    "finditer",
    "sub",
    "subn",
    "split",
];
const RE_MODULES: &[&str] = &["re", "regex"];

/// Finds all injected regular expressions in `parse` of `file_text`.
pub fn injections(file_text: &str, parse: &Parse) -> Vec<Injection> {
    let root = parse.syntax_node();
    let mut out: Vec<Injection> = Vec::new();
    let lines = LineIndex::new(file_text);

    // 1. Calls into the `re` / `regex` modules.
    for node in root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CALL_EXPR)
    {
        let Some(call) = ast::CallExpr::cast(node.clone()) else {
            continue;
        };
        let Some(callee) = call.callee() else {
            continue;
        };
        let Some((module, func)) = re_callee(&callee) else {
            continue;
        };
        if !RE_FUNCTIONS.contains(&func.as_str()) {
            continue;
        }
        let Some(arg_list) = call.arg_list() else {
            continue;
        };
        let args: Vec<ast::AnyArg> = arg_list.args().collect();
        // Pattern: `pattern=` keyword, else the first positional argument.
        let pattern_expr = args
            .iter()
            .find_map(|a| match a {
                ast::AnyArg::Keyword(k) if k.name_text() == "pattern" => k.value(),
                _ => None,
            })
            .or_else(|| {
                args.iter().find_map(|a| match a {
                    ast::AnyArg::Positional(p) => p.expr(),
                    _ => None,
                })
            });
        let Some(ast::Expr::String(s)) = pattern_expr else {
            continue;
        };
        if s.is_bytes() {
            continue;
        }
        let pattern_range = s.syntax().text_range();
        let verbose = args.iter().any(|a| {
            // Skip the pattern argument itself; look for VERBOSE / X flags anywhere else.
            let is_pattern = a
                .value()
                .is_some_and(|v| v.syntax().text_range() == pattern_range);
            !is_pattern && has_verbose_flag(&a.syntax().text().to_string())
        });
        out.push(Injection {
            string_range: pattern_range,
            value_range: s.content_range(),
            pattern: s.value(),
            verbose,
            call: format!("{module}.{func}"),
        });
    }

    // 2. `# language=regex` / `# language=PythonRegExp` annotated literals.
    for node in root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::STRING_EXPR)
    {
        let Some(s) = ast::StringExpr::cast(node.clone()) else {
            continue;
        };
        if s.is_bytes() {
            continue;
        }
        let range = s.syntax().text_range();
        if out.iter().any(|i| i.string_range == range) {
            continue;
        }
        let line = lines.line_of(usize::from(range.start()));
        let same_line = lines.line_text(file_text, line).is_some_and(|t| {
            // Only a comment after the literal on the same line counts.
            let rel_end = usize::from(range.end())
                .saturating_sub(lines.line_start(line))
                .min(t.len());
            is_language_comment(&t[rel_end..])
        });
        let previous_line = line > 0
            && lines
                .line_text(file_text, line - 1)
                .is_some_and(|t| t.trim_start().starts_with('#') && is_language_comment(t));
        if same_line || previous_line {
            out.push(Injection {
                string_range: range,
                value_range: s.content_range(),
                pattern: s.value(),
                verbose: false,
                call: "# language=regex".into(),
            });
        }
    }
    out.sort_by_key(|i| i.string_range.start());
    out
}

/// `(module, function)` for `re.compile` / `regex.search` style callees.
fn re_callee(callee: &ast::Expr) -> Option<(String, String)> {
    match callee {
        ast::Expr::Attribute(a) => {
            let func = a.attr_text();
            match a.object()? {
                ast::Expr::Name(n) if RE_MODULES.contains(&n.name_text().as_str()) => {
                    Some((n.name_text(), func))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn has_verbose_flag(text: &str) -> bool {
    text.contains("VERBOSE")
        || text
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
            .any(|w| w == "re.X" || w == "regex.X")
}

fn is_language_comment(text: &str) -> bool {
    let Some(hash) = text.find('#') else {
        return false;
    };
    let comment: String = text[hash + 1..]
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    comment.starts_with("language=regex") || comment.starts_with("language=pythonregexp")
}

struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { starts }
    }

    fn line_of(&self, offset: usize) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    fn line_start(&self, line: usize) -> usize {
        self.starts.get(line).copied().unwrap_or(0)
    }

    fn line_text<'a>(&self, text: &'a str, line: usize) -> Option<&'a str> {
        let start = *self.starts.get(line)?;
        let end = self
            .starts
            .get(line + 1)
            .map(|e| e - 1)
            .unwrap_or(text.len())
            .max(start);
        Some(text[start..end.min(text.len())].trim_end_matches('\r'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"import re
import regex

PAT = re.compile(r"(?P<y>\d{4})-\d\d", re.VERBOSE | re.I)
m = re.match("a\\d+b", text)
re.sub(pattern=r"\s+", repl=" ", string=s)
regex.search(r"[a-z]+", s)
needle = r"\bfoo\b"  # language=regex
# language=PythonRegExp
other = "x.y"
plain = "not a regex"
raw_bytes = re.compile(b"ab")
f = re.compile(f"{x}")
flagged = re.compile(r"a b", flags=re.X)
"#;

    #[test]
    fn finds_calls_and_annotations() {
        let parse = rustpy_syntax::parse(SRC);
        let inj = injections(SRC, &parse);
        let calls: Vec<(&str, &str, bool)> = inj
            .iter()
            .map(|i| (i.call.as_str(), i.pattern.as_str(), i.verbose))
            .collect();
        assert_eq!(
            calls,
            vec![
                ("re.compile", r"(?P<y>\d{4})-\d\d", true),
                ("re.match", r"a\d+b", false),
                ("re.sub", r"\s+", false),
                ("regex.search", "[a-z]+", false),
                ("# language=regex", r"\bfoo\b", false),
                ("# language=regex", "x.y", false),
                ("re.compile", "a b", true),
            ]
        );
        // Ranges point at the literal and its content.
        let first = &inj[0];
        assert_eq!(
            &SRC[usize::from(first.string_range.start())..usize::from(first.string_range.end())],
            r#"r"(?P<y>\d{4})-\d\d""#
        );
        assert_eq!(
            &SRC[usize::from(first.value_range.start())..usize::from(first.value_range.end())],
            r"(?P<y>\d{4})-\d\d"
        );
    }

    #[test]
    fn decodes_non_raw_strings() {
        let src = "import re\nre.search(\"\\\\d+\\t\", x)\n";
        let parse = rustpy_syntax::parse(src);
        let inj = injections(src, &parse);
        assert_eq!(inj.len(), 1);
        assert_eq!(inj[0].pattern, "\\d+\t");
    }

    #[test]
    fn ignores_unrelated_calls() {
        let src = "import re\nprint(\"a+\")\nos.path.match(\"x\")\nre.escape(\"a.b\")\n";
        let parse = rustpy_syntax::parse(src);
        assert!(injections(src, &parse).is_empty());
    }
}
