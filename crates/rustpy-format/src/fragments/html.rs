//! Only tag trivia is reformatted. Text nodes, raw-text elements, comments,
//! declarations and attribute values retain their exact bytes.
use super::{Error, Options};

fn name(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.') || byte >= 128
}
fn tag(source: &str, start: usize) -> Result<(usize, String, String, bool), Error> {
    let b = source.as_bytes();
    let mut at = start + 1;
    let closing = b.get(at) == Some(&b'/');
    if closing {
        at += 1;
    }
    let head = at;
    while b.get(at).is_some_and(|c| name(*c)) {
        at += 1;
    }
    if at == head || !source.is_char_boundary(at) {
        return Err(Error::Syntax("Invalid HTML tag name".into()));
    }
    let tag_name = source[head..at].to_owned();
    let mut out = String::from(if closing { "</" } else { "<" });
    out.push_str(&tag_name);
    let mut count = 0;
    loop {
        let gap = at;
        while b.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if b.get(at) == Some(&b'>') {
            out.push('>');
            return Ok((at + 1, out, tag_name, closing));
        }
        if b.get(at..at + 2) == Some(b"/>") {
            out.push_str(" />");
            return Ok((at + 2, out, tag_name, true));
        }
        if closing || gap == at {
            return Err(Error::Syntax("HTML attributes require separation".into()));
        }
        count += 1;
        if count > super::MAX_TOKENS {
            return Err(Error::Budget("HTML attributes"));
        }
        let begin = at;
        while b.get(at).is_some_and(|c| name(*c)) {
            at += 1;
        }
        if begin == at || !source.is_char_boundary(at) {
            return Err(Error::Unsafe("Dynamic HTML attribute syntax".into()));
        }
        out.push(' ');
        out.push_str(&source[begin..at]);
        let after_name = at;
        while b.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if b.get(at) != Some(&b'=') {
            at = after_name;
            continue;
        }
        at += 1;
        while b.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let value = at;
        if matches!(b.get(at), Some(b'\'' | b'"')) {
            let quote = b[at];
            at += 1;
            while b.get(at).is_some_and(|c| *c != quote) {
                at += 1;
            }
            if at == b.len() {
                return Err(Error::Syntax("Unterminated HTML attribute".into()));
            }
            at += 1;
        } else {
            while b
                .get(at)
                .is_some_and(|c| !c.is_ascii_whitespace() && *c != b'>')
            {
                if matches!(b[at], b'\'' | b'"' | b'`' | b'=' | b'<') {
                    return Err(Error::Syntax("Invalid unquoted HTML attribute".into()));
                }
                at += 1;
            }
            if value == at {
                return Err(Error::Syntax("Missing HTML attribute value".into()));
            }
        }
        out.push('=');
        out.push_str(&source[value..at]);
    }
}
pub(super) fn format(source: &str, _options: Options) -> Result<String, Error> {
    if ["{{", "{%", "<%"]
        .iter()
        .any(|marker| source.contains(marker))
    {
        return Err(Error::Unsafe(
            "Template delimiters require their host formatter".into(),
        ));
    }
    let mut output = String::new();
    let mut at = 0;
    let mut count = 0;
    while at < source.len() {
        count += 1;
        if count > super::MAX_TOKENS {
            return Err(Error::Budget("HTML tokens"));
        }
        let Some(relative) = source[at..].find('<') else {
            output.push_str(&source[at..]);
            break;
        };
        let start = at + relative;
        output.push_str(&source[at..start]);
        let rest = &source[start..];
        let opaque = if rest.starts_with("<!--") {
            Some(("-->", 4))
        } else if rest.starts_with("<![CDATA[") {
            Some(("]]>", 9))
        } else if rest.starts_with("<?") {
            Some(("?>", 2))
        } else {
            None
        };
        if let Some((close, skip)) = opaque {
            let end = start
                + skip
                + source[start + skip..]
                    .find(close)
                    .ok_or_else(|| Error::Syntax("Unterminated HTML declaration".into()))?
                + close.len();
            output.push_str(&source[start..end]);
            at = end;
            continue;
        }
        if rest.starts_with("<!") {
            // Internal subsets and declarations with quoted '>' are left alone
            // rather than guessed. A simple doctype is an opaque token.
            let end = start
                + rest
                    .find('>')
                    .ok_or_else(|| Error::Syntax("Unterminated doctype".into()))?
                + 1;
            if !rest[..end - start]
                .to_ascii_lowercase()
                .starts_with("<!doctype ")
                || source[start..end].contains(['[', '\'', '"'])
            {
                return Err(Error::Unsafe("Complex HTML declaration".into()));
            }
            output.push_str(&source[start..end]);
            at = end;
            continue;
        }
        let (end, rendered, name, closed) = tag(source, start)?;
        output.push_str(&rendered);
        at = end;
        if !closed
            && ["script", "style", "textarea", "title"]
                .iter()
                .any(|raw| name.eq_ignore_ascii_case(raw))
        {
            let mut search = at;
            let close = format!("</{}", name.to_ascii_lowercase());
            loop {
                let Some(relative) = source[search..].find('<') else {
                    return Err(Error::Syntax("Unterminated HTML raw-text element".into()));
                };
                let candidate = search + relative;
                if source[candidate..]
                    .get(..close.len())
                    .is_some_and(|s| s.eq_ignore_ascii_case(&close))
                    && source
                        .as_bytes()
                        .get(candidate + close.len())
                        .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>')
                {
                    output.push_str(&source[at..candidate]);
                    at = candidate;
                    break;
                }
                // Script escape states have different end-tag rules.
                if name.eq_ignore_ascii_case("script")
                    && (source[candidate..].starts_with("<!--")
                        || source[candidate..]
                            .get(..7)
                            .is_some_and(|s| s.eq_ignore_ascii_case("<script")))
                {
                    return Err(Error::Unsafe("Escaped HTML script state".into()));
                }
                search = candidate + 1;
            }
        }
        super::checked_growth(&output)?;
    }
    Ok(output)
}
