//! Python-aware indentation (roadmap §1.2 auto-indent): the indent of the
//! line Enter creates (open brackets, hanging indents, multi-line headers,
//! backslash continuations, dedent after `return`), re-indenting
//! `else:` / `elif …:` / `except …:` / `finally:` / `case …:` when their
//! colon is typed, and the logical-line structure of a code fragment (used by
//! smart paste).

use rustpy_syntax::lexer::tokenize;
use rustpy_syntax::SyntaxKind::{self, *};
use rustpy_text::util::{char_width, indent_width};

/// Block indentation of an unfinished bracket/backslash logical statement.
/// Returns None for ordinary suite lines, preserving normal tab indentation.
pub fn continuation_base(before: &str, tab_size: usize) -> Option<usize> {
    let (tokens, _) = tokenize(before);
    let mut depth = 0usize;
    let mut statement = None;
    for token in &tokens {
        match token.kind {
            NEWLINE if !token.range.is_empty() => {
                statement = None;
                depth = 0;
            }
            kind if kind.is_trivia() || matches!(kind, NEWLINE | INDENT | DEDENT | EOF) => {}
            kind => {
                statement.get_or_insert(usize::from(token.range.start()));
                if matches!(kind, L_PAREN | L_BRACK | L_BRACE) {
                    depth += 1;
                }
                if matches!(kind, R_PAREN | R_BRACK | R_BRACE) {
                    depth = depth.saturating_sub(1);
                }
            }
        }
    }
    if depth == 0 && !before.trim_end_matches([' ', '\t']).ends_with('\\') {
        return None;
    }
    let offset = statement?;
    let start = line_start(before, offset);
    let end = before[start..]
        .find('\n')
        .map_or(before.len(), |n| start + n);
    Some(indent_width(&before[start..end], tab_size))
}

/// Statements after which the next line dedents.
const DEDENT_KEYWORDS: &[SyntaxKind] = &[RETURN_KW, PASS_KW, BREAK_KW, CONTINUE_KW, RAISE_KW];

/// What a physical line of a Python fragment is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// Starts a logical line (a statement, a blank line or a comment line).
    Logical,
    /// Continues a statement inside brackets or after a backslash.
    Continuation,
    /// Starts inside a multi-line string literal.
    InString,
}

/// Classifies every physical line of `text` (one entry per `\n` plus one).
pub fn classify_lines(text: &str) -> Vec<LineKind> {
    let n = text.matches('\n').count() + 1;
    let (tokens, _) = tokenize(text);
    let mut kinds = Vec::with_capacity(n);
    kinds.push(LineKind::Logical);
    let mut depth = 0usize;
    for t in &tokens {
        match t.kind {
            L_PAREN | L_BRACK | L_BRACE => depth += 1,
            R_PAREN | R_BRACK | R_BRACE => depth = depth.saturating_sub(1),
            _ => {}
        }
        let newlines = text[t.range].matches('\n').count();
        let kind = match t.kind {
            NEWLINE | NL if depth > 0 => LineKind::Continuation,
            NEWLINE | NL => LineKind::Logical,
            STRING | FSTRING_START | FSTRING_MIDDLE | FSTRING_END | TSTRING_START
            | TSTRING_MIDDLE | TSTRING_END => LineKind::InString,
            // Backslash continuations are whitespace; anything else is broken code.
            _ => LineKind::Continuation,
        };
        kinds.extend(std::iter::repeat_n(kind, newlines));
    }
    kinds.resize(n, LineKind::Logical);
    kinds
}

/// Indentation for the line Enter creates.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct EnterIndent {
    /// Indent (columns) of the new line.
    pub indent: usize,
    /// When the innermost open bracket ends its line (hanging indent), the
    /// indent its closing bracket gets if Enter splits `(|)`.
    pub closer_indent: Option<usize>,
}

/// Visual width of `s` starting at column 0.
fn width(s: &str, tab_size: usize) -> usize {
    s.chars()
        .fold(0, |col, c| col + char_width(c, tab_size, col))
}

fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// Whether a complete string token's text is terminated.
fn string_is_closed(tok: &str) -> bool {
    let body = tok.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let Some(q) = body.chars().next().filter(|c| *c == '"' || *c == '\'') else {
        return true;
    };
    let quote: String = if body.starts_with(&format!("{q}{q}{q}")) {
        std::iter::repeat_n(q, 3).collect()
    } else {
        q.to_string()
    };
    if body.len() < quote.len() * 2 || !body.ends_with(&quote) {
        return false;
    }
    // The closing quote must not be escaped.
    let inner = &body[..body.len() - quote.len()];
    inner
        .chars()
        .rev()
        .take_while(|c| *c == '\\')
        .count()
        .is_multiple_of(2)
}

/// Indent for a new line inserted at the end of `before` (the document text
/// before the caret). `None` when the caret is inside a string literal, where
/// the caller keeps the current line's indentation.
pub fn indent_after_enter(
    before: &str,
    tab_size: usize,
    indent_size: usize,
) -> Option<EnterIndent> {
    indent_after_enter_with_continuation(before, tab_size, indent_size, None)
}

pub fn indent_after_enter_with_continuation(
    before: &str,
    tab_size: usize,
    indent_size: usize,
    policy: Option<rustpy_format::ContinuationIndent>,
) -> Option<EnterIndent> {
    let (tokens, _) = tokenize(before);
    // Open brackets: token index of each.
    let mut stack: Vec<usize> = Vec::new();
    let mut stmt_start: Option<usize> = None;
    let mut first_kind: Option<SyntaxKind> = None;
    let mut last_significant: Option<SyntaxKind> = None;
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            // The lexer ends the input with a zero-width NEWLINE: not a real line end.
            NEWLINE if !t.range.is_empty() => {
                stmt_start = None;
                first_kind = None;
                last_significant = None;
                stack.clear();
                continue;
            }
            k if k.is_trivia() || matches!(k, NEWLINE | INDENT | DEDENT | EOF) => continue,
            L_PAREN | L_BRACK | L_BRACE => stack.push(i),
            R_PAREN | R_BRACK | R_BRACE => {
                stack.pop();
            }
            _ => {}
        }
        if stmt_start.is_none() {
            stmt_start = Some(usize::from(t.range.start()));
            first_kind = Some(t.kind);
        }
        last_significant = Some(t.kind);
    }
    // Inside a string literal?
    if let Some(t) = tokens
        .iter()
        .rev()
        .find(|t| !matches!(t.kind, EOF | DEDENT | NEWLINE) || !t.range.is_empty())
    {
        let at_end = usize::from(t.range.end()) >= before.len();
        let open_string = match t.kind {
            STRING => !string_is_closed(&before[t.range]),
            FSTRING_START | FSTRING_MIDDLE | TSTRING_START | TSTRING_MIDDLE => true,
            ERROR_TOKEN => before[t.range]
                .trim_start_matches(|c: char| c.is_ascii_alphabetic())
                .starts_with(['"', '\'']),
            _ => false,
        };
        if at_end && open_string {
            return None;
        }
    }
    let cur_start = line_start(before, before.len());
    let cur_line = &before[cur_start..];
    let indent_of_line_at = |offset: usize| -> usize {
        let s = line_start(before, offset);
        let end = before[s..]
            .find('\n')
            .map(|i| s + i)
            .unwrap_or(before.len());
        indent_width(&before[s..end], tab_size)
    };
    let Some(stmt_start) = stmt_start else {
        // Nothing typed on this logical line yet: keep the line's indentation.
        return Some(EnterIndent {
            indent: indent_width(cur_line, tab_size),
            closer_indent: None,
        });
    };
    // Innermost open bracket.
    if let Some(&bi) = stack.last() {
        let open = &tokens[bi];
        let open_end = usize::from(open.range.end());
        let parameters = open.kind == L_PAREN
            && tokens
                .iter()
                .enumerate()
                .find(|(_, t)| t.kind == L_PAREN && usize::from(t.range.start()) >= stmt_start)
                .map(|(index, _)| index)
                == Some(bi)
            && tokens
                .iter()
                .filter(|t| usize::from(t.range.start()) >= stmt_start)
                .take_while(|t| usize::from(t.range.start()) < open_end)
                .any(|t| t.kind == DEF_KW)
            && matches!(first_kind, Some(DEF_KW | ASYNC_KW));
        let previous = tokens[..bi]
            .iter()
            .rev()
            .find(|t| !t.kind.is_trivia() && !matches!(t.kind, NL | INDENT | DEDENT));
        let arguments = open.kind == L_PAREN
            && previous.is_some_and(|t| matches!(t.kind, IDENT | R_PAREN | R_BRACK | R_BRACE));
        let step = policy.map_or(indent_size, |p| p.step(parameters, arguments, indent_size));
        let next_on_line = tokens[bi + 1..]
            .iter()
            .take_while(|t| !matches!(t.kind, NL | NEWLINE))
            .find(|t| !t.kind.is_trivia() && !matches!(t.kind, INDENT | DEDENT | EOF));
        return Some(match next_on_line {
            // `foo(a,|` → align with `a`.
            Some(t) => {
                let s = usize::from(t.range.start());
                EnterIndent {
                    indent: width(&before[line_start(before, s)..s], tab_size),
                    closer_indent: None,
                }
            }
            // `foo(|` → hanging indent relative to the bracket's line.
            None => {
                let base = indent_of_line_at(open_end);
                EnterIndent {
                    indent: base + step,
                    closer_indent: Some(base),
                }
            }
        });
    }
    let stmt_indent = indent_of_line_at(stmt_start);
    // Backslash continuation: indent once relative to the statement.
    if before.trim_end_matches([' ', '\t']).ends_with('\\') {
        let on_first_line = line_start(before, stmt_start) == cur_start;
        let indent = if on_first_line {
            stmt_indent + policy.map_or(indent_size, |p| p.width.clamp(1, 256))
        } else {
            indent_width(cur_line, tab_size)
        };
        return Some(EnterIndent {
            indent,
            closer_indent: None,
        });
    }
    let indent = if last_significant == Some(COLON) {
        stmt_indent + indent_size
    } else if first_kind.is_some_and(|k| DEDENT_KEYWORDS.contains(&k)) {
        stmt_indent.saturating_sub(indent_size)
    } else {
        stmt_indent
    };
    Some(EnterIndent {
        indent,
        closer_indent: None,
    })
}

/// First word of a statement line (`async` skipped).
fn first_word(stripped: &str) -> &str {
    let s = stripped
        .strip_prefix("async")
        .filter(|r| r.starts_with([' ', '\t']))
        .map(str::trim_start)
        .unwrap_or(stripped);
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    &s[..end]
}

/// The clause keyword of `line` (ending in `:`) and the statements it can belong to.
fn clause_partners(line: &str) -> Option<(&str, &'static [&'static str])> {
    let stripped = line.trim();
    if !stripped.ends_with(':') {
        return None;
    }
    let word = first_word(stripped);
    let rest = &stripped[word.len()..];
    let partners: &'static [&'static str] = match word {
        "else" if rest.trim() == ":" => &["if", "elif", "for", "while", "try", "except"],
        "finally" if rest.trim() == ":" => &["try", "except", "else"],
        "elif" if rest.starts_with([' ', '\t', '(']) => &["if", "elif"],
        "except" if rest.starts_with([' ', '\t', ':', '*', '(']) => &["try", "except"],
        "case" if rest.starts_with([' ', '\t', '(', '[', '{']) => &["case", "match"],
        _ => return None,
    };
    Some((word, partners))
}

/// Whether `line` is an `else:` / `elif …:` / `except …:` / `finally:` / `case …:` line.
pub fn is_clause_line(line: &str) -> bool {
    clause_partners(line).is_some()
}

/// For a line ending in a just-typed `:` that is an `else` / `elif` /
/// `except` / `finally` / `case` clause, the indent of the statement it
/// belongs to, found in `above` (the text before the line). `Some` only when
/// that is a dedent of `line`.
pub fn clause_indent(
    line: &str,
    above: &str,
    tab_size: usize,
    indent_size: usize,
) -> Option<usize> {
    let (word, partners) = clause_partners(line)?;
    let cur = indent_width(line, tab_size);
    let kinds = classify_lines(above);
    let lines: Vec<&str> = above.split('\n').collect();
    let mut max_allowed = cur as isize;
    for (text, kind) in lines.iter().zip(kinds).rev() {
        let t = text.trim();
        if kind != LineKind::Logical || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let ind = indent_width(text, tab_size) as isize;
        if ind > max_allowed {
            continue;
        }
        let w = first_word(t);
        if partners.contains(&w) {
            let target = if w == "match" && word == "case" {
                ind as usize + indent_size
            } else {
                ind as usize
            };
            return (target < cur).then_some(target);
        }
        // A statement at this level closes every deeper block above it.
        max_allowed = ind - 1;
        if max_allowed < 0 {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter(before: &str) -> Option<usize> {
        indent_after_enter(before, 4, 4).map(|e| e.indent)
    }

    #[test]
    fn classifies_logical_continuation_and_string_lines() {
        let src = "x = foo(a,\n        b)\ns = \"\"\"doc\nmore\"\"\"\ny = 1 + \\\n    2\n";
        use LineKind::*;
        assert_eq!(
            classify_lines(src),
            vec![
                Logical,
                Continuation,
                Logical,
                InString,
                Logical,
                Continuation,
                Logical
            ]
        );
    }

    #[test]
    fn brackets_align_or_hang() {
        assert_eq!(
            enter("x = foo(a,"),
            Some(8),
            "align under the first argument"
        );
        assert_eq!(
            enter("    x = foo("),
            Some(8),
            "hanging indent from the bracket's line"
        );
        assert_eq!(
            indent_after_enter("    x = foo(", 4, 4)
                .unwrap()
                .closer_indent,
            Some(4)
        );
        assert_eq!(
            enter("x = [\n    foo(\n"),
            Some(8),
            "an empty continuation line keeps the hanging indent of the open bracket"
        );
        assert_eq!(
            enter("x = [\n    bar(1,"),
            Some(8),
            "nested aligned bracket"
        );
        assert_eq!(enter("x = {'a': (1,  # comment"), Some(11));
    }

    #[test]
    fn multi_line_headers_and_closed_calls() {
        assert_eq!(
            enter("def f(a,\n      b):"),
            Some(4),
            "body after a two-line header"
        );
        assert_eq!(enter("    if (a and\n            b):"), Some(8));
        assert_eq!(
            enter("x = foo(a,\n        b)"),
            Some(0),
            "statement indent after the call closes"
        );
        assert_eq!(
            enter("    return foo(\n        a,\n    )"),
            Some(0),
            "dedent after a multi-line return"
        );
        assert_eq!(enter("if x:  # why"), Some(4), "comment after the colon");
        assert_eq!(enter("x = 1  # note:"), Some(0), "colon inside a comment");
    }

    #[test]
    fn continuation_strings_and_tabs() {
        assert_eq!(enter("x = 1 + \\"), Some(4));
        assert_eq!(enter("x = 1 + \\\n    2 + \\"), Some(4));
        assert_eq!(enter("def f():\n    \"\"\"Doc"), None, "inside a docstring");
        assert_eq!(enter("s = 'a:'"), Some(0));
        assert_eq!(
            indent_after_enter("\tif x:", 4, 4).map(|e| e.indent),
            Some(8)
        );
        assert_eq!(
            indent_after_enter("\tfoo(a,", 4, 4).map(|e| e.indent),
            Some(8),
            "tab expands before alignment"
        );
        assert_eq!(
            enter("    "),
            Some(4),
            "empty logical line keeps the indent"
        );
    }

    #[test]
    fn clause_keywords_find_their_statement() {
        let above = "def f():\n    if a:\n        x = 1\n        y = 2\n";
        assert_eq!(clause_indent("        else:", above, 4, 4), Some(4));
        assert_eq!(clause_indent("        elif b:", above, 4, 4), Some(4));
        assert_eq!(
            clause_indent("    else:", above, 4, 4),
            None,
            "already at the if's level"
        );
        let t = "try:\n    for i in x:\n        pass\n";
        assert_eq!(
            clause_indent("        else:", t, 4, 4),
            Some(4),
            "for-else is the nearest partner"
        );
        assert_eq!(
            clause_indent("        except ValueError:", t, 4, 4),
            Some(0)
        );
        assert_eq!(clause_indent("        finally:", t, 4, 4), Some(0));
        assert_eq!(clause_indent("        elsewhere = 1:", t, 4, 4), None);
        let m = "match x:\n    case 1:\n        pass\n";
        assert_eq!(clause_indent("        case 2:", m, 4, 4), Some(4));
        // A dedented statement closes deeper blocks.
        let closed = "if a:\n    b = 1\nz = 2\n";
        assert_eq!(clause_indent("    else:", closed, 4, 4), None);
        // Tabs.
        assert_eq!(
            clause_indent("\t\telse:", "if a:\n\tif b:\n\t\tpass\n", 4, 4),
            Some(4)
        );
    }
}

#[cfg(test)]
mod continuation_policy_tests {
    use super::*;
    #[test]
    fn explicit_policy_distinguishes_parameters_calls_collections_and_alignment() {
        let policy = rustpy_format::ContinuationIndent {
            width: 6,
            parameters: true,
            arguments: false,
            collections: false,
        };
        for (source, expected) in [
            ("def f(", 6),
            ("def earlier(x):\n  pass\ndef later(", 6),
            ("async def f(", 6),
            ("result = call(", 2),
            ("result = [", 2),
            ("if True:", 2),
            ("call(one,", 5),
            ("result = 1 + \\", 6),
            ("def f(value=call(", 2),
        ] {
            assert_eq!(
                indent_after_enter_with_continuation(source, 4, 2, Some(policy))
                    .unwrap()
                    .indent,
                expected,
                "{source}"
            );
        }
        let all = rustpy_format::ContinuationIndent {
            arguments: true,
            collections: true,
            ..policy
        };
        for source in ["result = call(", "result = [", "result = {", "result = ("] {
            assert_eq!(
                indent_after_enter_with_continuation(source, 4, 2, Some(all))
                    .unwrap()
                    .indent,
                6,
                "{source}"
            );
        }
        assert_eq!(indent_after_enter("def f(", 4, 2).unwrap().indent, 2);
        assert!(
            indent_after_enter_with_continuation("text = \"unfinished", 4, 2, Some(all)).is_none()
        );
    }
}
