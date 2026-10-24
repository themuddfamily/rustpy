//! Port of `black/strings.py` and `black/numerics.py`: string prefix/quote
//! normalization, docstring indentation, display width, numeric literals.
//! The regular expressions of the original are emulated by hand-written
//! scanners with the same leftmost-first matching behaviour.

use super::width_table::WIDTH_TABLE;

const STRING_PREFIX_CHARS: &str = "fturbFTURB";

fn is_prefix_char(c: char) -> bool {
    STRING_PREFIX_CHARS.contains(c)
}

pub fn has_triple_quotes(s: &str) -> bool {
    let raw = s.trim_start_matches(is_prefix_char);
    raw.starts_with("\"\"\"") || raw.starts_with("'''")
}

pub fn get_string_prefix(s: &str) -> &str {
    let n = s.chars().take_while(|c| is_prefix_char(*c)).count();
    &s[..n]
}

/// Lowercase prefixes (except `R`), drop `u`, put `r` first.
pub fn normalize_string_prefix(s: &str) -> String {
    let orig = get_string_prefix(s);
    let rest = &s[orig.len()..];
    let mut new_prefix: String = orig
        .chars()
        .filter(|c| !matches!(c, 'u' | 'U'))
        .map(|c| match c {
            'F' => 'f',
            'B' => 'b',
            c => c,
        })
        .collect();
    if new_prefix.chars().count() == 2 && !new_prefix.starts_with(['r', 'R']) {
        new_prefix = new_prefix.chars().rev().collect();
    }
    format!("{new_prefix}{rest}")
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Pat {
    /// `([^\\]|^)\\((?:\\\\)*)Q` → `\1\2Q`
    Escaped,
    /// `(([^\\]|^)(\\\\)*)Q` → `\1\\Q`
    Unescaped,
}

fn starts_with_at(c: &[char], i: usize, q: &[char]) -> bool {
    c.len() >= i + q.len() && c[i..i + q.len()] == *q
}

fn count_pairs(c: &[char], mut j: usize) -> usize {
    let mut k = 0;
    while j + 1 < c.len() && c[j] == '\\' && c[j + 1] == '\\' {
        k += 1;
        j += 2;
    }
    k
}

/// Tries to match `pat` at `i`; returns (end, replacement).
fn match_at(c: &[char], i: usize, pat: Pat, q: &[char]) -> Option<(usize, String)> {
    let alt = |start_char: Option<char>, j: usize| -> Option<(usize, String)> {
        match pat {
            Pat::Escaped => {
                if j >= c.len() || c[j] != '\\' {
                    return None;
                }
                let j = j + 1;
                let k = count_pairs(c, j);
                let after = j + 2 * k;
                if starts_with_at(c, after, q) {
                    let mut r = String::new();
                    if let Some(ch) = start_char {
                        r.push(ch);
                    }
                    for _ in 0..2 * k {
                        r.push('\\');
                    }
                    r.extend(q.iter());
                    Some((after + q.len(), r))
                } else {
                    None
                }
            }
            Pat::Unescaped => {
                let k = count_pairs(c, j);
                let after = j + 2 * k;
                if starts_with_at(c, after, q) {
                    let mut r = String::new();
                    if let Some(ch) = start_char {
                        r.push(ch);
                    }
                    for _ in 0..2 * k {
                        r.push('\\');
                    }
                    r.push('\\');
                    r.extend(q.iter());
                    Some((after + q.len(), r))
                } else {
                    None
                }
            }
        }
    };
    if i < c.len() && c[i] != '\\' {
        if let Some(m) = alt(Some(c[i]), i + 1) {
            return Some(m);
        }
    }
    if i == 0 {
        if let Some(m) = alt(None, 0) {
            return Some(m);
        }
    }
    None
}

fn re_sub(s: &str, pat: Pat, quote: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let q: Vec<char> = quote.chars().collect();
    let mut out = String::with_capacity(s.len() + 4);
    let mut i = 0;
    while i <= c.len() {
        if let Some((end, rep)) = match_at(&c, i, pat, &q) {
            out.push_str(&rep);
            i = end;
            continue;
        }
        if i < c.len() {
            out.push(c[i]);
        }
        i += 1;
    }
    out
}

fn re_search(s: &str, pat: Pat, quote: &str) -> bool {
    let c: Vec<char> = s.chars().collect();
    let q: Vec<char> = quote.chars().collect();
    (0..=c.len()).any(|i| match_at(&c, i, pat, &q).is_some())
}

fn sub_twice(s: &str, pat: Pat, quote: &str) -> String {
    re_sub(&re_sub(s, pat, quote), pat, quote)
}

/// Group 1 of every match of Black's f-string replacement-field regex.
fn fstring_fields(body: &str) -> Vec<String> {
    let c: Vec<char> = body.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '{' && (i == 0 || c[i - 1] != '{') && i + 1 < c.len() && c[i + 1] != '{' {
            // `[^{]` then lazy `.*?` (no newlines) then `}` not followed by `}`.
            let mut k = i + 2;
            let mut found = None;
            loop {
                if k >= c.len() {
                    break;
                }
                if c[k] == '}' && (k + 1 >= c.len() || c[k + 1] != '}') {
                    found = Some(k);
                    break;
                }
                if c[k] == '\n' {
                    break;
                }
                k += 1;
            }
            if let Some(k) = found {
                out.push(c[i + 1..k].iter().collect());
                i = k + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn ends_with_unescaped_quote(body: &str) -> bool {
    if !body.ends_with('"') {
        return false;
    }
    let preceding = &body[..body.len() - 1];
    let backslashes = preceding.len() - preceding.trim_end_matches('\\').len();
    backslashes.is_multiple_of(2)
}

/// Prefer double quotes unless that adds escapes.
pub fn normalize_string_quotes(s: &str) -> String {
    let value = s.trim_start_matches(is_prefix_char);
    let (orig_quote, new_quote) = if value.starts_with("\"\"\"") {
        return s.to_string();
    } else if value.starts_with("'''") {
        ("'''", "\"\"\"")
    } else if value.starts_with('"') {
        ("\"", "'")
    } else {
        ("'", "\"")
    };
    let Some(first_quote_pos) = s.find(orig_quote) else {
        return s.to_string();
    };
    let prefix = &s[..first_quote_pos];
    if s.len() < first_quote_pos + 2 * orig_quote.len() {
        return s.to_string();
    }
    let mut s_out = s.to_string();
    let mut body = s[first_quote_pos + orig_quote.len()..s.len() - orig_quote.len()].to_string();
    let lower_prefix = prefix.to_lowercase();
    let mut new_body = if lower_prefix.contains('r') {
        if re_search(&body, Pat::Unescaped, new_quote) {
            return s.to_string();
        }
        body.clone()
    } else {
        let nb = sub_twice(&body, Pat::Escaped, new_quote);
        if body != nb {
            body = nb.clone();
            s_out = format!("{prefix}{orig_quote}{body}{orig_quote}");
        }
        let nb = sub_twice(&nb, Pat::Escaped, orig_quote);
        sub_twice(&nb, Pat::Unescaped, new_quote)
    };
    if lower_prefix.contains('f') || lower_prefix.contains('t') {
        for m in fstring_fields(&new_body) {
            if m.contains('\\') {
                return s_out;
            }
        }
    }
    if new_quote == "\"\"\"" && ends_with_unescaped_quote(&new_body) {
        new_body.pop();
        new_body.push_str("\\\"");
    }
    let orig_escape_count = body.matches('\\').count();
    let new_escape_count = new_body.matches('\\').count();
    if new_escape_count > orig_escape_count {
        return s_out;
    }
    if new_escape_count == orig_escape_count && orig_quote == "\"" {
        return s_out;
    }
    format!("{prefix}{new_quote}{new_body}{new_quote}")
}

/// Lowercase hex digits in `\x`, `\u`, `\U` escapes, uppercase `\N{...}` names.
pub fn normalize_unicode_escape_sequences(text: &str) -> String {
    let prefix = get_string_prefix(text);
    if prefix.to_lowercase().contains('r') {
        return text.to_string();
    }
    // Python does not decode Unicode escapes in bytes literals. Their spelling
    // is literal data, so changing its case changes the bytes themselves.
    let bytes = prefix.to_lowercase().contains('b');
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] != '\\' {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < c.len() && c[j] == '\\' {
            j += 1;
        }
        let run = j - i;
        for _ in 0..run {
            out.push('\\');
        }
        let hex = |from: usize, n: usize| -> bool {
            from + n <= c.len() && c[from..from + n].iter().all(|x| x.is_ascii_hexdigit())
        };
        let body_len = match c.get(j) {
            Some('u') if !bytes && hex(j + 1, 4) => Some(5),
            Some('U') if !bytes && hex(j + 1, 8) => Some(9),
            Some('x') if hex(j + 1, 2) => Some(3),
            Some('N') if !bytes && c.get(j + 1) == Some(&'{') => {
                let mut k = j + 2;
                while k < c.len() && (c[k].is_ascii_alphanumeric() || c[k] == ' ' || c[k] == '-') {
                    k += 1;
                }
                if k - (j + 2) >= 2 && c.get(k) == Some(&'}') {
                    Some(k + 1 - j)
                } else {
                    None
                }
            }
            _ => None,
        };
        match body_len {
            Some(len) => {
                let body: String = c[j..j + len].iter().collect();
                if !run.is_multiple_of(2) {
                    let first = &body[..1];
                    let rest = &body[1..];
                    if first == "N" {
                        out.push_str("N{");
                        out.push_str(&rest[1..rest.len() - 1].to_uppercase());
                        out.push('}');
                    } else {
                        out.push_str(first);
                        out.push_str(&rest.to_lowercase());
                    }
                } else {
                    out.push_str(&body);
                }
                i = j + len;
            }
            None => i = j,
        }
    }
    out
}

fn is_py_space(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c' | '\x1c'..='\x1f' | '\u{85}'
    ) || (c as u32 > 127 && c.is_whitespace())
}

/// Python `str.strip()` (whitespace as `str.isspace`).
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

pub fn py_lstrip(s: &str) -> &str {
    s.trim_start_matches(is_py_space)
}

pub fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(is_py_space)
}

/// Splits on `\r\n`, `\r` or `\n` (Black's `LINE_BREAK_RE`).
fn split_line_breaks(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\r' {
            out.push(&s[start..i]);
            if i + 1 < b.len() && b[i + 1] == b'\n' {
                i += 1;
            }
            start = i + 1;
        } else if b[i] == b'\n' {
            out.push(&s[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

// Black/inspect.cleandoc use Python expandtabs() default eight-column stops.
// This applies only to the leading prefix; inline payload tabs remain literal.
fn expand_docstring_tabs(s: &str) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for ch in s.chars() {
        match ch {
            '\t' => {
                let n = 8 - col % 8;
                for _ in 0..n {
                    out.push(' ');
                }
                col += n;
            }
            '\n' | '\r' => {
                out.push(ch);
                col = 0;
            }
            c => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

fn lines_with_leading_tabs_expanded(s: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in split_line_breaks(s) {
        let stripped = py_lstrip(line);
        if stripped.is_empty() || stripped.len() == line.len() {
            lines.push(line.to_string());
        } else {
            let prefix_length = line.len() - stripped.len();
            let prefix = expand_docstring_tabs(&line[..prefix_length]);
            lines.push(format!("{prefix}{stripped}"));
        }
    }
    lines
}

pub fn fix_multiline_docstring(docstring: &str, prefix: &str) -> String {
    let lines = lines_with_leading_tabs_expanded(docstring);
    let mut indent = usize::MAX;
    for line in &lines[1..] {
        let stripped = py_lstrip(line);
        if !stripped.is_empty() {
            indent = indent.min(line.chars().count() - stripped.chars().count());
        }
    }
    let mut trimmed = vec![py_strip(&lines[0]).to_string()];
    if indent < usize::MAX {
        let last_line_idx = lines.len().saturating_sub(2);
        for (i, line) in lines[1..].iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            let rest: String = if indent < chars.len() {
                chars[indent..].iter().collect()
            } else {
                String::new()
            };
            let stripped_line = py_rstrip(&rest);
            if !stripped_line.is_empty() || i == last_line_idx {
                trimmed.push(format!("{prefix}{stripped_line}"));
            } else {
                trimmed.push(String::new());
            }
        }
    }
    trimmed.join("\n")
}

/// Python `str.splitlines()`.
pub fn py_splitlines(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => {
                out.push(std::mem::take(&mut cur));
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(std::mem::take(&mut cur));
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn char_width(c: char) -> usize {
    let cp = c as u32;
    let t = WIDTH_TABLE;
    let (mut lo, mut hi) = (0usize, t.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        let (s, e, w) = t[mid];
        if cp < s {
            hi = mid;
        } else if cp > e {
            lo = mid + 1;
        } else {
            return w as usize;
        }
    }
    1
}

/// Display width (East Asian wide characters count 2).
pub fn str_width(s: &str) -> usize {
    if s.is_ascii() {
        return s.len();
    }
    s.chars().map(char_width).sum()
}

fn format_float_or_int_string(text: &str) -> String {
    match text.split_once('.') {
        None => text.to_string(),
        Some((before, after)) => {
            let before = if before.is_empty() { "0" } else { before };
            let after = if after.is_empty() { "0" } else { after };
            format!("{before}.{after}")
        }
    }
}

/// Lowercase everything except hex digits (uppercase); `1.` → `1.0`, `.5` → `0.5`.
pub fn normalize_numeric_literal(value: &str) -> String {
    let text = value.to_lowercase();
    if text.starts_with("0o") || text.starts_with("0b") {
        text
    } else if let Some(rest) = text.strip_prefix("0x") {
        format!("0x{}", rest.to_uppercase())
    } else if let Some((before, after)) = text.split_once('e') {
        let (sign, after) = if let Some(a) = after.strip_prefix('-') {
            ("-", a)
        } else if let Some(a) = after.strip_prefix('+') {
            ("", a)
        } else {
            ("", after)
        };
        format!("{}e{sign}{after}", format_float_or_int_string(before))
    } else if let Some(number) = text.strip_suffix('j') {
        format!("{}j", format_float_or_int_string(number))
    } else {
        format_float_or_int_string(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes() {
        assert_eq!(normalize_string_quotes("'a'"), "\"a\"");
        assert_eq!(normalize_string_quotes("'a\"b'"), "'a\"b'");
        assert_eq!(normalize_string_quotes("'a\\'b'"), "\"a'b\"");
        assert_eq!(normalize_string_quotes("\"\\'\""), "\"'\"");
        assert_eq!(normalize_string_quotes("''"), "\"\"");
        assert_eq!(normalize_string_quotes("'\"\"'"), "'\"\"'");
        assert_eq!(normalize_string_quotes("r'\\d'"), "r\"\\d\"");
        assert_eq!(normalize_string_quotes("'''a'''"), "\"\"\"a\"\"\"");
        assert_eq!(normalize_string_quotes("'''a\"'''"), "'''a\"'''");
        assert_eq!(normalize_string_quotes("f'{x}'"), "f\"{x}\"");
    }

    #[test]
    fn numbers() {
        assert_eq!(normalize_numeric_literal("0XABcd"), "0xABCD");
        assert_eq!(normalize_numeric_literal("1E5"), "1e5");
        assert_eq!(normalize_numeric_literal("10J"), "10j");
        assert_eq!(normalize_numeric_literal("1."), "1.0");
        assert_eq!(normalize_numeric_literal(".5"), "0.5");
        assert_eq!(normalize_numeric_literal("1.E+5"), "1.0e5");
    }

    #[test]
    fn escapes() {
        assert_eq!(
            normalize_unicode_escape_sequences("'\\xAB\\\\xAB\\N{dash}'"),
            "'\\xab\\\\xAB\\N{DASH}'"
        );
    }
}
