//! Immutable choices for pasting literal clipboard text into an ordinary Python
//! string. The caller binds a prompt to document identity/version/selections,
//! then applies one chosen edit as one Paste unit, or cancels without mutation.
use super::{EditorState, Language};
use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::{ast, ast::AstNode};
use rustpy_text::Edit;

#[derive(Clone, Debug)]
pub(crate) struct StringPasteChoice {
    pub edit: Edit,
    pub caret: TextSize,
}
#[derive(Clone, Debug)]
pub(crate) struct StringPastePlan {
    pub escaped: StringPasteChoice,
    pub raw: Option<StringPasteChoice>,
    /// Literal Unicode clipboard text inserted into the decoded existing value.
    pub value: String,
}

/// No live text, history, clipboard or selection mutation. None means the
/// ordinary paste path remains responsible; it never means discard the paste.
pub(crate) fn prepare(state: &EditorState, clipboard: &str) -> Option<StringPastePlan> {
    if state.language != Language::Python
        || state.doc.is_read_only()
        || state.is_large_file()
        || state.carets.len() != 1
        || state.column_mode
        || state.block.is_some()
        || (0..state.carets.len()).any(|i| {
            let virtual_caret = state.virtual_caret(i);
            virtual_caret.anchor != state.doc.offset_to_position(state.carets[i].sel.anchor)
                || virtual_caret.head != state.doc.offset_to_position(state.carets[i].head())
        })
        || clipboard.is_empty()
    {
        return None;
    }
    let source = state.doc.text();
    if source.len() > 1024 * 1024 || clipboard.len() > 1024 * 1024 {
        return None;
    }
    let selection = state.primary().range();
    let parsed = rustpy_syntax::parse(&source);
    let literal = parsed
        .syntax_node()
        .descendants()
        .filter_map(ast::StringExpr::cast)
        .find(|s| s.content_range().contains_range(selection))?;
    let token = literal.raw_text();
    let prefix = literal.prefix();
    if !matches!(prefix.as_str(), "" | "u" | "r") {
        return None;
    }
    let body = &token[prefix.len()..];
    let quote = if body.starts_with("\"\"\"") {
        "\"\"\""
    } else if body.starts_with("'''") {
        "'''"
    } else if body.starts_with('"') {
        "\""
    } else {
        "'"
    };
    if body.len() < quote.len() * 2 || !body.ends_with(quote) {
        return None;
    }
    let content = literal.content_range();
    let inner = &token[prefix.len() + quote.len()..token.len() - quote.len()];
    let start = usize::from(selection.start() - content.start());
    let end = usize::from(selection.end() - content.start());
    let left = inner.get(..start)?;
    let right = inner.get(end..)?;
    if !literal.is_raw() && (!safe_segment(left) || !safe_segment(right) || !safe_segment(inner)) {
        return None;
    }
    let decode = |s: &str| ast::decode_string_literal(&format!("{prefix}{quote}{s}{quote}"));
    let left = decode(left);
    let right = decode(right);
    // Document accepts LF-normalized edits: explicitly normalize clipboard too.
    let clipboard = rustpy_text::LineEnding::normalize_to_lf(clipboard);
    let value = format!("{left}{clipboard}{right}");
    let escaped_prefix = if prefix == "u" { "u" } else { "" };
    let encoded_left = escape(&left, quote);
    let encoded_paste = escape(&clipboard, quote);
    let escaped_text = format!(
        "{escaped_prefix}{quote}{encoded_left}{encoded_paste}{}{quote}",
        escape(&right, quote)
    );
    let range = literal.text_range();
    let escaped = choice(
        range,
        escaped_text,
        escaped_prefix.len() + quote.len() + encoded_left.len() + encoded_paste.len(),
        &value,
    )?;
    let raw = if !value.contains(quote)
        && !value.contains('\r')
        && !value.contains('\0')
        && (quote.len() == 3 || !value.contains('\n'))
        && value
            .chars()
            .rev()
            .take_while(|c| *c == '\\')
            .count()
            .is_multiple_of(2)
    {
        choice(
            range,
            format!("r{quote}{value}{quote}"),
            1 + quote.len() + left.len() + clipboard.len(),
            &value,
        )
    } else {
        None
    };
    Some(StringPastePlan {
        escaped,
        raw,
        value,
    })
}
fn choice(range: TextRange, text: String, caret: usize, value: &str) -> Option<StringPasteChoice> {
    let parsed = rustpy_syntax::parse(&format!("x = {text}\n"));
    if !parsed.errors().is_empty() {
        return None;
    }
    let literal = parsed
        .syntax_node()
        .descendants()
        .find_map(ast::StringExpr::cast)?;
    if literal.raw_text() != text || literal.value() != value {
        return None;
    }
    Some(StringPasteChoice {
        edit: Edit::replace(range, text),
        caret: range.start() + TextSize::new(caret as u32),
    })
}
fn escape(value: &str, quote: &str) -> String {
    let delimiter = quote.chars().next().unwrap();
    let mut out = String::new();
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\x00"),
            c if c == delimiter => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
/// Reject unsupported named escapes, invalid hex/surrogates, or a selection
/// splitting an escape. Existing syntax decoder is used only for proven input.
fn safe_segment(s: &str) -> bool {
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            continue;
        }
        let Some(c) = chars.next() else {
            return false;
        };
        let count = match c {
            'N' | '0'..='7' => return false,
            'x' => 2,
            'u' => 4,
            'U' => 8,
            _ => 0,
        };
        if count > 0 {
            let mut value = 0u32;
            for _ in 0..count {
                let Some(d) = chars.next().and_then(|c| c.to_digit(16)) else {
                    return false;
                };
                let Some(next) = value.checked_mul(16).and_then(|v| v.checked_add(d)) else {
                    return false;
                };
                value = next;
            }
            if char::from_u32(value).is_none() {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Caret;
    fn at(source: &str, needle: &str) -> EditorState {
        let mut state = EditorState::from_text(source, Language::Python);
        let offset = source.find(needle).unwrap();
        state.set_carets(vec![Caret::at(TextSize::new(offset as u32))]);
        state
    }
    fn apply(state: &mut EditorState, choice: StringPasteChoice) {
        assert!(state.apply(
            vec![choice.edit],
            vec![Caret::at(choice.caret)],
            rustpy_text::EditGroup::Separate,
            Some("Paste")
        ));
    }
    #[test]
    fn escaped_clipboard_preserves_unicode_quotes_backslashes_and_one_undo() {
        let original = "x = 'π-end'\n";
        let mut state = at(original, "-end");
        let before = state.selections();
        let plan = prepare(&state, "C:\\new\"\n🙂'").unwrap();
        assert_eq!(plan.value, "πC:\\new\"\n🙂'-end");
        assert!(plan.raw.is_none());
        apply(&mut state, plan.escaped);
        let result = state.doc.text();
        assert!(result.contains("C:\\\\new\"\\n🙂\\'"));
        assert!(state.undo());
        assert_eq!(state.doc.text(), original);
        assert_eq!(state.selections(), before);
        assert!(state.redo());
        assert_eq!(state.doc.text(), result);
    }
    #[test]
    fn raw_conversion_preserves_decoded_existing_escape_and_actual_payload() {
        let mut state = at("x = \"a\\tbEND\"", "END");
        let plan = prepare(&state, "C:\\temp").unwrap();
        assert_eq!(plan.value, "a\tbC:\\tempEND");
        apply(&mut state, plan.raw.unwrap());
        assert_eq!(state.doc.text(), "x = r\"a\tbC:\\tempEND\"");
    }
    #[test]
    fn selection_and_triple_quotes_preserve_surrounding_decoded_value() {
        let mut state = at("x = u'''αreplaceω'''", "replace");
        let start = state.primary().head();
        state.set_carets(vec![Caret::new(start, start + TextSize::new(7))]);
        let plan = prepare(&state, "one\r\ntwo").unwrap();
        assert_eq!(plan.value, "αone\ntwoω");
        assert!(plan.escaped.edit.text.starts_with("u'''"));
        apply(&mut state, plan.raw.unwrap());
        assert_eq!(state.doc.text(), "x = r'''αone\ntwoω'''");
    }
    #[test]
    fn raw_availability_rejects_delimiters_odd_slashes_and_single_line_newlines() {
        for text in ["'", "tail\\", "a\nb"] {
            let mut state = at("x = 'END'", "END");
            state.set_carets(vec![Caret::at(TextSize::new(8))]);
            let plan = prepare(&state, text).unwrap();
            assert!(plan.raw.is_none(), "{text:?}");
        }
        let mut state = at("x = 'END'", "END");
        state.set_carets(vec![Caret::at(TextSize::new(8))]);
        assert!(prepare(&state, "tail\\\\").unwrap().raw.is_some());
    }
    #[test]
    fn ambiguous_escapes_unsupported_contexts_and_guarded_editors_keep_normal_paste() {
        for source in [
            "x = f'END'",
            "x = b'END'",
            "x = '\\N{SPACE}END'",
            "x = '\\123END'",
            "x = '\\ud800END'",
        ] {
            assert!(prepare(&at(source, "END"), "x").is_none(), "{source}");
        }
        let mut state = at("x = 'END'", "END");
        state.doc.set_read_only(true);
        assert!(prepare(&state, "x").is_none());
        state.doc.set_read_only(false);
        state.set_carets(vec![
            Caret::at(TextSize::new(5)),
            Caret::at(TextSize::new(6)),
        ]);
        assert!(prepare(&state, "x").is_none());
        let state = at("x = '\\x41END'", "x41");
        assert!(
            prepare(&state, "x").is_none(),
            "selection must not split escape"
        );
    }
}
