//! Bounded documentation-only type syntax. Python annotations and type
//! comments continue through their ordinary parser without markup rewriting.
use super::{clean, detect_one, is_underline, DocstringFormat};
use once_cell::sync::Lazy;
use regex::Regex;

static ROLE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r":(?:py:)?(?:class|type|obj):`([^`\n]+)`").unwrap());

/// Remove type-role presentation markup while retaining its actual target.
/// Union prose is recognized only outside quotes and balanced type arguments.
pub fn normalize(text: &str) -> Option<String> {
    if text.len() > 16384 {
        return None;
    }
    let text = text.trim().trim_end_matches(", optional").trim();
    let text = if text.starts_with("``") && text.ends_with("``") && text.len() >= 4 {
        &text[2..text.len() - 2]
    } else {
        text
    };
    if text.is_empty() {
        return None;
    }
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let ch = rest.chars().next()?;
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
        } else {
            if ch == ':' {
                if let Some(captures) = ROLE.captures(rest).filter(|captures| {
                    captures[0].starts_with(':')
                        && captures.get(0).is_some_and(|role| role.start() == 0)
                }) {
                    let content = captures[1].trim();
                    let target = content
                        .rsplit_once('<')
                        .and_then(|(_, target)| target.strip_suffix('>'))
                        .unwrap_or(content);
                    out.push_str(target.trim().trim_start_matches('~'));
                    at += captures[0].len();
                    continue;
                }
            }
            match ch {
                '\'' | '"' => quote = Some(ch),
                '[' | '(' | '{' => {
                    depth += 1;
                    if depth > 32 {
                        return None;
                    }
                }
                ']' | ')' | '}' => depth = depth.checked_sub(1)?,
                _ => {}
            }
            if depth == 0 && rest.starts_with(" or ") {
                out.push_str(" | ");
                at += 4;
                continue;
            }
        }
        out.push(ch);
        at += ch.len_utf8();
    }
    (quote.is_none() && depth == 0).then_some(out)
}

/// Split a typed result entry without treating role colons or literal text as
/// the separator before its description.
pub(super) fn typed_entry(text: &str) -> Option<(&str, &str)> {
    let mut at = 0;
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    while at < text.len() {
        let ch = text[at..].chars().next()?;
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
        } else {
            if ch == ':' {
                if let Some(role) = ROLE.find_at(text, at).filter(|role| role.start() == at) {
                    at = role.end();
                    continue;
                }
                if depth == 0 {
                    return Some((&text[..at], text[at + 1..].trim()));
                }
            }
            match ch {
                '\'' | '"' | '`' => quote = Some(ch),
                '[' | '(' | '{' => depth += 1,
                ']' | ')' | '}' => depth = depth.checked_sub(1)?,
                _ => {}
            }
        }
        at += ch.len_utf8();
    }
    None
}

/// The fact parser retains one result field for compatibility with existing
/// rendering/generation callers. Its final result section determines whether
/// that field describes a returned object or each yielded element.
pub fn result_is_yield(doc: &str) -> bool {
    let lines = clean(doc);
    let format = detect_one(doc);
    let mut is_yield = false;
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let line = line.trim();
        let marker = match format {
            DocstringFormat::ReStructuredText => line.strip_prefix(':'),
            DocstringFormat::Epytext => line.strip_prefix('@'),
            DocstringFormat::Google => line.strip_suffix(':'),
            DocstringFormat::NumPy
                if lines
                    .get(index + 1)
                    .is_some_and(|line| is_underline(line, '-') || is_underline(line, '=')) =>
            {
                Some(line)
            }
            _ => None,
        };
        let Some(marker) = marker else { continue };
        let marker = marker.split([':', ' ']).next().unwrap_or("");
        match marker {
            "yield" | "yields" | "ytype" | "Yields" | "Yield" => is_yield = true,
            "return" | "returns" | "rtype" | "Returns" | "Return" => is_yield = false,
            _ => {}
        }
    }
    is_yield
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn type_markup_preserves_targets_quoted_values_and_bounds() {
        assert_eq!(
            normalize(":py:class:`Visible <~pkg.Actual>` or None"),
            Some("pkg.Actual | None".into())
        );
        assert_eq!(
            normalize("``list[int]``, optional"),
            Some("list[int]".into())
        );
        assert_eq!(
            normalize("Literal['yes or no']"),
            Some("Literal['yes or no']".into())
        );
        assert_eq!(
            normalize("Literal[':class:`str`']"),
            Some("Literal[':class:`str`']".into())
        );
        assert!(normalize("list[").is_none());
        assert!(normalize(&"[".repeat(33)).is_none());
        assert!(normalize(&"x".repeat(16385)).is_none());
    }
}
