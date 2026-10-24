//! Fallible Python literal decoder shared with the CPython AST exporter.
//! Surrogates remain codepoints; no replacement scalar or runtime Python.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported(pub String);
impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Unsupported {}
pub type Result<T> = std::result::Result<T, Unsupported>;
#[path = "../examples/support/unicode_names.rs"]
mod unicode_names;
#[derive(Debug, PartialEq, Eq)]
pub enum Decoded {
    Text(String, Option<String>),
    Bytes(Vec<u8>),
    SurrogateText(Vec<u32>, Option<String>),
}
fn scalar(value: u32) -> Result<char> {
    char::from_u32(value).ok_or_else(|| {
        Unsupported(
            "Lone-surrogate/out-of-range string scalar not representable in JSON String".into(),
        )
    })
}
pub fn string(raw: &str) -> Result<Decoded> {
    let (tokens, diagnostics) = crate::tokenize(raw);
    if !diagnostics.is_empty()
        || !tokens.iter().any(|t| {
            t.kind == crate::SyntaxKind::STRING
                && u32::from(t.range.start()) == 0
                && u32::from(t.range.end()) as usize == raw.len()
        })
        || tokens
            .iter()
            .filter(|t| t.kind == crate::SyntaxKind::STRING)
            .count()
            != 1
        || tokens.iter().any(|t| {
            !matches!(
                t.kind,
                crate::SyntaxKind::STRING | crate::SyntaxKind::NEWLINE | crate::SyntaxKind::EOF
            )
        })
    {
        return Err(Unsupported(
            "Expected exactly one valid Python string token".into(),
        ));
    }
    let prefix_len = raw.bytes().take_while(u8::is_ascii_alphabetic).count();
    let prefix = &raw[..prefix_len];
    let lower = prefix.to_ascii_lowercase();
    if !matches!(lower.as_str(), "" | "r" | "u" | "b" | "br" | "rb") {
        return Err(Unsupported("Formatted/template string literal".into()));
    }
    let body = &raw[prefix_len..];
    let q = if body.starts_with("\"\"\"") || body.starts_with("'''") {
        3
    } else {
        1
    };
    if body.len() < 2 * q {
        return Err(Unsupported("Unterminated string token".into()));
    }
    let decoded = decode_inner(
        &body[q..body.len() - q],
        lower.contains('b'),
        lower.contains('r'),
    )?;
    Ok(match decoded {
        Decoded::Text(text, _) => Decoded::Text(
            text,
            if prefix == "u" {
                Some("u".into())
            } else {
                None
            },
        ),
        Decoded::SurrogateText(points, _) => Decoded::SurrogateText(
            points,
            if prefix == "u" {
                Some("u".into())
            } else {
                None
            },
        ),
        bytes => bytes,
    })
}
/// Decode source text shared by literal handling and editor doctest mapping.
pub fn decode_text(text: &str, raw_mode: bool) -> Result<String> {
    match decode_inner(text, false, raw_mode)? {
        Decoded::Text(text, _) => Ok(text),
        Decoded::Bytes(_) => Err(Unsupported("Text decoder returned bytes".into())),
        Decoded::SurrogateText(_, _) => Err(Unsupported(
            "Surrogate text requires lossless value callback".into(),
        )),
    }
}
pub fn decode_inner(text: &str, bytes: bool, raw_mode: bool) -> Result<Decoded> {
    let inner = text.replace("\r\n", "\n").replace('\r', "\n");
    if bytes && !inner.is_ascii() {
        return Err(Unsupported("Non-ASCII source in bytes literal".into()));
    }
    let mut out: Vec<u32> = Vec::new();
    let mut byte_out = Vec::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        let mut pieces = String::new();
        let mut byte = None;
        let mut point = None;
        if c != '\\' || raw_mode {
            pieces.push(c);
        } else {
            let e = chars
                .next()
                .ok_or_else(|| Unsupported("Dangling string escape".into()))?;
            match e {
                '\n' => {}
                '\\' | '\'' | '"' => pieces.push(e),
                'a' => pieces.push('\x07'),
                'b' => pieces.push('\x08'),
                'f' => pieces.push('\x0c'),
                'n' => pieces.push('\n'),
                'r' => pieces.push('\r'),
                't' => pieces.push('\t'),
                'v' => pieces.push('\x0b'),
                '0'..='7' => {
                    let mut n = e.to_digit(8).unwrap();
                    for _ in 0..2 {
                        if chars.peek().is_some_and(|c| matches!(c, '0'..='7')) {
                            n = n * 8 + chars.next().unwrap().to_digit(8).unwrap();
                        } else {
                            break;
                        }
                    }
                    if bytes {
                        byte = Some((n % 256) as u8);
                    } else {
                        pieces.push(scalar(n)?);
                    }
                }
                'x' | 'u' | 'U' if e == 'x' || !bytes => {
                    let count = match e {
                        'x' => 2,
                        'u' => 4,
                        _ => 8,
                    };
                    let mut n = 0;
                    for _ in 0..count {
                        n = n * 16
                            + chars
                                .next()
                                .and_then(|c| c.to_digit(16))
                                .ok_or_else(|| Unsupported("Malformed hex escape".into()))?;
                    }
                    if bytes {
                        byte = Some(n as u8);
                    } else {
                        if n > 0x10ffff {
                            return Err(Unsupported("Out-of-range Unicode escape".into()));
                        }
                        point = Some(n);
                    }
                }
                'N' if !bytes => {
                    if chars.next() != Some('{') {
                        return Err(Unsupported("Malformed named Unicode escape".into()));
                    }
                    let mut name = String::new();
                    let mut closed = false;
                    for c in chars.by_ref() {
                        if c == '}' {
                            closed = true;
                            break;
                        }
                        if name.len() >= 256 {
                            return Err(Unsupported("Unicode name budget exceeded".into()));
                        }
                        name.push(c);
                    }
                    if !closed {
                        return Err(Unsupported("Unclosed named Unicode escape".into()));
                    }
                    point = Some(unicode_names::lookup(&name)?);
                }
                _ => {
                    pieces.push('\\');
                    pieces.push(e);
                }
            }
        }
        if bytes {
            if let Some(b) = byte {
                byte_out.push(b);
            } else {
                byte_out.extend(pieces.bytes());
            }
        } else {
            out.extend(pieces.chars().map(u32::from));
            if let Some(point) = point {
                out.push(point);
            }
        }
    }
    Ok(if bytes {
        Decoded::Bytes(byte_out)
    } else {
        if out.iter().any(|n| (0xd800..=0xdfff).contains(n)) {
            Decoded::SurrogateText(out, None)
        } else {
            Decoded::Text(
                out.into_iter()
                    .map(|n| char::from_u32(n).expect("validated scalar"))
                    .collect(),
                None,
            )
        }
    })
}
pub fn append(left: Decoded, right: Decoded) -> Result<Decoded> {
    Ok(match (left, right) {
        (Decoded::Text(mut a, kind), Decoded::Text(b, _)) => {
            a.push_str(&b);
            Decoded::Text(a, kind)
        }
        (Decoded::Bytes(mut a), Decoded::Bytes(b)) => {
            a.extend(b);
            Decoded::Bytes(a)
        }
        (Decoded::SurrogateText(mut a, kind), Decoded::SurrogateText(b, _)) => {
            a.extend(b);
            Decoded::SurrogateText(a, kind)
        }
        (Decoded::SurrogateText(mut a, kind), Decoded::Text(b, _)) => {
            a.extend(b.chars().map(u32::from));
            Decoded::SurrogateText(a, kind)
        }
        (Decoded::Text(a, kind), Decoded::SurrogateText(b, _)) => {
            let mut points: Vec<u32> = a.chars().map(u32::from).collect();
            points.extend(b);
            Decoded::SurrogateText(points, kind)
        }
        _ => return Err(Unsupported("Mixed bytes and text concatenation".into())),
    })
}

/// Only ordinary Unicode text can authorize an export name.
pub fn text(raw: &str) -> Result<String> {
    match string(raw)? {
        Decoded::Text(value, _) => Ok(value),
        _ => Err(Unsupported(
            "Export name must be scalar Unicode text".into(),
        )),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_escaped_raw_triple_and_unicode() {
        assert_eq!(
            text(r#""\x61\u03c0\U0001f642\N{LATIN SMALL LETTER B}""#).unwrap(),
            "aπ🙂b"
        );
        assert_eq!(text(r#"r"\nπ""#).unwrap(), r"\nπ");
        assert_eq!(
            text(
                "'''π
line'''"
            )
            .unwrap(),
            "π\nline"
        );
        assert_eq!(text(r#""\q\141""#).unwrap(), r"\qa");
        assert_eq!(
            append(string("'π'").unwrap(), string("'🙂'").unwrap()).unwrap(),
            Decoded::Text("π🙂".into(), None)
        );
    }
    #[test]
    fn exporter_large_literals_retain_lossless_decoder_admission() {
        let content = "π".repeat((1 << 19) + 1);
        let token = format!("'{content}'");
        assert!(token.len() > 1 << 20);
        assert_eq!(text(&token).unwrap(), content);
        assert_eq!(
            decode_inner(&content, false, true).unwrap(),
            Decoded::Text(content, None)
        );
    }
    #[test]
    fn malformed_bytes_and_surrogates_never_fabricate_scalar_names() {
        for raw in [
            "'unterminated",
            "'a'\n",
            "'a'\n\n",
            " 'a'",
            "'a' ",
            r#""\x0""#,
            "'a' 'b'",
            "b'bad'",
            r#""\ud800""#,
            r#""\U00110000""#,
            r#""\N{NOT A NAME}""#,
        ] {
            assert!(text(raw).is_err(), "{raw}");
        }
        assert_eq!(
            string(r#""\ud800\udc00""#).unwrap(),
            Decoded::SurrogateText(vec![0xd800, 0xdc00], None)
        );
    }
}
