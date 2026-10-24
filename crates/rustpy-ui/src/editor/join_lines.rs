//! Python Join Lines preserves comment ownership, block boundaries and literals.
use rustpy_syntax::{tokenize, SyntaxKind as K};

fn significant(kind: K) -> bool {
    !matches!(
        kind,
        K::WHITESPACE | K::NL | K::NEWLINE | K::INDENT | K::DEDENT | K::EOF
    )
}

/// None preserves this physical boundary. The caller can still join other
/// safe boundaries in the same selection in its single edit transaction.
pub(super) fn python(
    left: &str,
    right: &str,
    previous_indent: usize,
    next_indent: usize,
) -> Option<(String, usize)> {
    let left = left.trim_end();
    let right = right.trim_start();
    if left.is_empty() {
        return Some((right.to_string(), 0));
    }
    if right.is_empty() {
        return Some((left.to_string(), left.len()));
    }
    let (left_tokens, _) = tokenize(left);
    let (right_tokens, _) = tokenize(right);
    let last = left_tokens
        .iter()
        .rev()
        .find(|token| significant(token.kind));
    let first = right_tokens.iter().find(|token| significant(token.kind))?;
    let left_comment = last.is_some_and(|token| token.kind == K::COMMENT);
    let right_comment = first.kind == K::COMMENT;
    for (source, tokens) in [(left, &left_tokens), (right, &right_tokens)] {
        if tokens
            .iter()
            .filter(|token| token.kind == K::COMMENT)
            .any(|token| {
                super::hard_wrap::protected_comment(
                    &source[usize::from(token.range.start())..usize::from(token.range.end())],
                )
            })
        {
            return None;
        }
    }
    if left_comment {
        if !right_comment {
            return None;
        }
        return Some((
            format!("{left} {}", right.trim_start_matches('#').trim_start()),
            left.len(),
        ));
    }
    if right_comment {
        return Some((format!("{left} {right}"), left.len()));
    }
    let mut depth = 0usize;
    for token in &left_tokens {
        match token.kind {
            K::L_PAREN | K::L_BRACK | K::L_BRACE => depth += 1,
            K::R_PAREN | K::R_BRACK | K::R_BRACE => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    // A terminal explicit continuation is WHITESPACE in the lexer, so the
    // last significant token can legitimately be the preceding string.
    // Classify the backslash's own token, not that preceding token.
    let continued = left.ends_with('\\')
        && left_tokens.iter().any(|token| {
            token.kind == K::WHITESPACE
                && token.range.end() == rustpy_core::TextSize::of(left)
                && token
                    .range
                    .contains(rustpy_core::TextSize::new((left.len() - 1) as u32))
        });
    if left.ends_with('\\') && !continued {
        // An unfinished literal owns this backslash; joining may change its
        // escape or raw-string semantics instead of removing a continuation.
        return None;
    }
    if depth == 0
        && !continued
        && (next_indent < previous_indent
            || matches!(
                first.kind,
                K::ELIF_KW
                    | K::ELSE_KW
                    | K::EXCEPT_KW
                    | K::FINALLY_KW
                    | K::DEF_KW
                    | K::CLASS_KW
                    | K::IF_KW
                    | K::FOR_KW
                    | K::WHILE_KW
                    | K::WITH_KW
                    | K::TRY_KW
                    | K::ASYNC_KW
                    | K::AT
            ))
    {
        return None;
    }
    let mut left = left.to_string();
    if continued {
        left.pop();
        left = left.trim_end().to_string();
        // Merge only ordinary, complete literals. A raw/f/bytes prefix has
        // different escape semantics and retains implicit concatenation.
        let tokens = tokenize(&left).0;
        if let Some(token) = tokens.iter().rev().find(|token| significant(token.kind)) {
            let raw = &left[usize::from(token.range.start())..usize::from(token.range.end())];
            if token.kind == K::STRING
                && token.range.end() == rustpy_core::TextSize::of(&left)
                && raw.starts_with(['\'', '"'])
                && !raw.starts_with("'''")
                && !raw.starts_with("\"\"\"")
                && first.kind == K::STRING
                && first.range.start() == rustpy_core::TextSize::new(0)
            {
                let quote = raw.chars().next()?;
                let body = &raw[1..raw.len() - 1];
                let octal_digits = body
                    .bytes()
                    .rev()
                    .take_while(|b| matches!(*b, b'0'..=b'7'))
                    .count();
                let escape_slashes = body[..body.len() - octal_digits]
                    .bytes()
                    .rev()
                    .take_while(|b| *b == b'\\')
                    .count();
                let extends_octal = (1..3).contains(&octal_digits)
                    && escape_slashes % 2 == 1
                    && right
                        .as_bytes()
                        .get(1)
                        .is_some_and(|b| matches!(*b, b'0'..=b'7'));
                if !extends_octal
                    && right.starts_with(quote)
                    && !right.starts_with(&quote.to_string().repeat(3))
                {
                    left.pop();
                    let join = left.len();
                    return Some((format!("{left}{}", &right[1..]), join));
                }
            }
        }
    }
    let separator = if left.ends_with(['(', '[', '{']) || right.starts_with([')', ']', '}', ',']) {
        ""
    } else if depth == 0 && !continued && !left.ends_with([':', ';']) {
        "; "
    } else {
        " "
    };
    let join = left.len();
    Some((format!("{left}{separator}{right}"), join))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Caret, EditOptions, EditorState, Language};
    use rustpy_core::TextSize;
    #[test]
    fn comments_directives_dedents_and_clauses_keep_their_boundary() {
        for (left, right, before, after) in [
            ("value = 1 # explanation", "other = 2", 0, 0),
            ("# noinspection PyX", "value = 1", 0, 0),
            ("    run()", "outside()", 4, 0),
            ("if ready: run()", "else: stop()", 0, 0),
        ] {
            assert!(python(left, right, before, after).is_none());
        }
        assert_eq!(python("# alpha", "# beta", 0, 0).unwrap().0, "# alpha beta");
        assert_eq!(python("a = 1", "b = 2", 0, 0).unwrap().0, "a = 1; b = 2");
        assert_eq!(
            python("if ready:", "    run()", 0, 4).unwrap().0,
            "if ready: run()"
        );
    }
    #[test]
    fn raw_strings_keep_their_prefix_and_escape_boundaries() {
        assert_eq!(
            python("value = r\"\\n\" \\", "\"\\t\"", 0, 4).unwrap().0,
            "value = r\"\\n\" \"\\t\""
        );
        assert_eq!(
            python("value = \"alpha\" \\", "\"beta\"", 0, 4).unwrap().0,
            "value = \"alphabeta\""
        );
    }
    #[test]
    fn explicit_continuations_do_not_extend_literal_escapes() {
        for (left, right, expected) in [
            (r#"value = "\1" \"#, r#""2""#, r#"value = "\1" "2""#),
            (r#"value = "\\" \"#, r#""n""#, r#"value = "\\n""#),
            (r#"value = b"a" \"#, r#"b"b""#, r#"value = b"a" b"b""#),
        ] {
            let joined = python(left, right, 0, 4).unwrap().0;
            assert_eq!(joined, expected);
            assert!(rustpy_syntax::parse(&joined).ok());
        }
        assert!(python(r#"value = r"unfinished\"#, "next", 0, 4).is_none());
    }

    #[test]
    fn selected_join_preserves_unsafe_boundaries_and_has_one_undo_step() {
        let source = "a = 1\nb = 2 # comment\nc = 3\n";
        let mut state = EditorState::from_text(source, Language::Python);
        state.set_carets(vec![Caret::new(TextSize::new(0), state.doc.len())]);
        state.join_lines_with_options(&EditOptions::default());
        assert_eq!(state.doc.text(), "a = 1; b = 2 # comment\nc = 3\n");
        assert!(rustpy_syntax::parse(&state.doc.text()).ok());
        assert!(state.undo());
        assert_eq!(state.doc.text(), source);
    }
}
