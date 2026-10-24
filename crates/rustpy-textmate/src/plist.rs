//! A small XML property-list reader, sufficient for TextMate `.tmLanguage`
//! files (no external `plist` crate is available offline).
//!
//! Supported: `<dict>`/`<key>`, `<array>`, `<string>`, `<integer>`, `<real>`,
//! `<true/>`, `<false/>`, `<data>` and `<date>` (both kept as text), XML
//! comments, processing instructions, a `<!DOCTYPE …>` prolog, CDATA sections,
//! the five predefined entities and numeric character references. The result
//! is a [`serde_json::Value`] tree so JSON and plist grammars share one loader.

use serde_json::{Map, Number, Value};

/// Error produced by [`parse_plist`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("plist parse error at byte {offset}: {message}")]
pub struct PlistError {
    /// Byte offset in the input where the problem was detected.
    pub offset: usize,
    /// Human readable description.
    pub message: String,
}

/// Maximum nesting of `<dict>` / `<array>` elements (guards the recursion).
const MAX_DEPTH: usize = 256;

/// Parses an XML property list into a JSON value tree.
///
/// The root `<plist>` wrapper is optional; its single child value is returned.
pub fn parse_plist(src: &str) -> Result<Value, PlistError> {
    let mut p = Parser { src, pos: 0 };
    if p.rest().starts_with('\u{feff}') {
        p.pos += '\u{feff}'.len_utf8();
    }
    p.skip_misc()?;
    let tag = p.open_tag()?;
    let value = if tag.name == "plist" {
        if tag.self_closing {
            return Err(p.err("empty <plist> element"));
        }
        p.skip_misc()?;
        let child = p.open_tag()?;
        let value = p.value(child, 0)?;
        p.skip_misc()?;
        p.close_tag("plist")?;
        value
    } else {
        p.value(tag, 0)?
    };
    p.skip_misc()?;
    if p.pos < src.len() {
        return Err(p.err("unexpected content after the root element"));
    }
    Ok(value)
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

struct OpenTag<'a> {
    name: &'a str,
    self_closing: bool,
    at: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn err(&self, message: impl Into<String>) -> PlistError {
        PlistError {
            offset: self.pos,
            message: message.into(),
        }
    }

    fn skip_ws(&mut self) {
        let rest = self.rest();
        self.pos += rest.len() - rest.trim_start().len();
    }

    /// Skips whitespace, comments, processing instructions and DOCTYPE.
    fn skip_misc(&mut self) -> Result<(), PlistError> {
        loop {
            self.skip_ws();
            let rest = self.rest();
            if rest.starts_with("<!--") {
                self.skip_past("-->", "unterminated comment")?;
            } else if rest.starts_with("<?") {
                self.skip_past("?>", "unterminated processing instruction")?;
            } else if rest.starts_with("<!DOCTYPE") || rest.starts_with("<!doctype") {
                self.skip_doctype()?;
            } else {
                return Ok(());
            }
        }
    }

    fn skip_past(&mut self, terminator: &str, what: &str) -> Result<(), PlistError> {
        match self.rest().find(terminator) {
            Some(i) => {
                self.pos += i + terminator.len();
                Ok(())
            }
            None => Err(self.err(what)),
        }
    }

    fn skip_doctype(&mut self) -> Result<(), PlistError> {
        let mut depth = 0usize;
        for (i, c) in self.rest().char_indices() {
            match c {
                '[' => depth += 1,
                ']' => depth = depth.saturating_sub(1),
                '>' if depth == 0 => {
                    self.pos += i + 1;
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(self.err("unterminated DOCTYPE"))
    }

    /// Reads `<name attr="…">` or `<name/>`.
    fn open_tag(&mut self) -> Result<OpenTag<'a>, PlistError> {
        let at = self.pos;
        if !self.rest().starts_with('<') || self.rest().starts_with("</") {
            return Err(self.err("expected an element"));
        }
        let rest = &self.rest()[1..];
        let name_len = rest
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .ok_or_else(|| self.err("unterminated tag"))?;
        let name = &rest[..name_len];
        if name.is_empty() {
            return Err(self.err("empty tag name"));
        }
        // Skip attributes (quoted values may contain '>').
        let mut quote: Option<char> = None;
        for (i, c) in rest[name_len..].char_indices() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"') | (None, '\'') => quote = Some(c),
                (None, '>') => {
                    let before = &rest[name_len..name_len + i];
                    self.pos += 1 + name_len + i + 1;
                    return Ok(OpenTag {
                        name,
                        self_closing: before.trim_end().ends_with('/'),
                        at,
                    });
                }
                _ => {}
            }
        }
        Err(self.err("unterminated tag"))
    }

    fn close_tag(&mut self, name: &str) -> Result<(), PlistError> {
        self.skip_ws();
        let rest = self.rest();
        if let Some(after) = rest.strip_prefix("</") {
            if let Some(after_name) = after.strip_prefix(name) {
                let trimmed = after_name.trim_start();
                if trimmed.starts_with('>') {
                    self.pos += rest.len() - trimmed.len() + 1;
                    return Ok(());
                }
            }
        }
        Err(self.err(format!("expected </{name}>")))
    }

    fn value(&mut self, tag: OpenTag<'a>, depth: usize) -> Result<Value, PlistError> {
        if depth > MAX_DEPTH {
            return Err(self.err("plist nesting too deep"));
        }
        match tag.name {
            "dict" => {
                let mut map = Map::new();
                if tag.self_closing {
                    return Ok(Value::Object(map));
                }
                loop {
                    self.skip_misc()?;
                    if self.rest().starts_with("</") {
                        self.close_tag("dict")?;
                        return Ok(Value::Object(map));
                    }
                    let key_tag = self.open_tag()?;
                    if key_tag.name != "key" {
                        return Err(PlistError {
                            offset: key_tag.at,
                            message: format!("expected <key> in <dict>, found <{}>", key_tag.name),
                        });
                    }
                    let key = if key_tag.self_closing {
                        String::new()
                    } else {
                        self.text("key")?
                    };
                    self.skip_misc()?;
                    let value_tag = self.open_tag()?;
                    let value = self.value(value_tag, depth + 1)?;
                    map.insert(key, value);
                }
            }
            "array" => {
                let mut items = Vec::new();
                if tag.self_closing {
                    return Ok(Value::Array(items));
                }
                loop {
                    self.skip_misc()?;
                    if self.rest().starts_with("</") {
                        self.close_tag("array")?;
                        return Ok(Value::Array(items));
                    }
                    let item_tag = self.open_tag()?;
                    items.push(self.value(item_tag, depth + 1)?);
                }
            }
            "true" | "false" => {
                if !tag.self_closing {
                    self.text(tag.name)?;
                }
                Ok(Value::Bool(tag.name == "true"))
            }
            "string" | "date" | "data" => {
                let text = if tag.self_closing {
                    String::new()
                } else {
                    self.text(tag.name)?
                };
                if tag.name == "data" {
                    Ok(Value::String(
                        text.chars().filter(|c| !c.is_whitespace()).collect(),
                    ))
                } else {
                    Ok(Value::String(text))
                }
            }
            "integer" | "real" => {
                let text = if tag.self_closing {
                    String::new()
                } else {
                    self.text(tag.name)?
                };
                let t = text.trim();
                let number = if tag.name == "integer" {
                    parse_integer(t)
                } else {
                    t.parse::<f64>().ok().and_then(Number::from_f64)
                };
                number.map(Value::Number).ok_or_else(|| PlistError {
                    offset: tag.at,
                    message: format!("invalid <{}> value {t:?}", tag.name),
                })
            }
            other => Err(PlistError {
                offset: tag.at,
                message: format!("unsupported plist element <{other}>"),
            }),
        }
    }

    /// Reads character data up to `</name>` (entities, CDATA and comments
    /// handled), consuming the closing tag.
    fn text(&mut self, name: &str) -> Result<String, PlistError> {
        let mut out = String::new();
        loop {
            let rest = self.rest();
            let Some(lt) = rest.find('<') else {
                return Err(self.err(format!("unterminated <{name}>")));
            };
            decode_entities(&rest[..lt], &mut out);
            self.pos += lt;
            let rest = self.rest();
            if let Some(cdata) = rest.strip_prefix("<![CDATA[") {
                let end = cdata
                    .find("]]>")
                    .ok_or_else(|| self.err("unterminated CDATA section"))?;
                out.push_str(&cdata[..end].replace("\r\n", "\n"));
                self.pos += "<![CDATA[".len() + end + "]]>".len();
            } else if rest.starts_with("<!--") {
                self.skip_past("-->", "unterminated comment")?;
            } else {
                self.close_tag(name)?;
                return Ok(out);
            }
        }
    }
}

fn parse_integer(t: &str) -> Option<Number> {
    if let Ok(i) = t.parse::<i64>() {
        return Some(Number::from(i));
    }
    if let Ok(u) = t.parse::<u64>() {
        return Some(Number::from(u));
    }
    let hex = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"))?;
    u64::from_str_radix(hex, 16).ok().map(Number::from)
}

/// Appends `raw` to `out`, decoding XML entities and normalizing CRLF.
fn decode_entities(raw: &str, out: &mut String) {
    let raw = raw.replace("\r\n", "\n");
    let mut rest = raw.as_str();
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&semi| semi <= 12).and_then(|semi| {
            let name = &rest[1..semi];
            let c = match name {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    let code = if let Some(hex) =
                        name.strip_prefix("#x").or_else(|| name.strip_prefix("#X"))
                    {
                        u32::from_str_radix(hex, 16).ok()
                    } else if let Some(dec) = name.strip_prefix('#') {
                        dec.parse::<u32>().ok()
                    } else {
                        None
                    };
                    code.and_then(char::from_u32)
                }
            };
            c.map(|c| (c, semi))
        });
        match decoded {
            Some((c, semi)) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_typical_tmlanguage_shape() {
        let src = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <!-- a comment -->
    <key>scopeName</key>
    <string>source.demo</string>
    <key>fileTypes</key>
    <array><string>demo</string><string>dm</string></array>
    <key>patterns</key>
    <array>
        <dict>
            <key>match</key>
            <string>\b(if|else)\b &amp; &lt;x&gt; &#65;&#x42;</string>
            <key>applyEndPatternLast</key>
            <integer>1</integer>
            <key>disabled</key>
            <false/>
            <key>weight</key>
            <real>1.5</real>
            <key>raw</key>
            <string><![CDATA[a<b>&c]]></string>
            <key>empty</key>
            <string/>
        </dict>
    </array>
</dict>
</plist>
"#;
        let v = parse_plist(src).unwrap();
        assert_eq!(
            v,
            json!({
                "scopeName": "source.demo",
                "fileTypes": ["demo", "dm"],
                "patterns": [{
                    "match": "\\b(if|else)\\b & <x> AB",
                    "applyEndPatternLast": 1,
                    "disabled": false,
                    "weight": 1.5,
                    "raw": "a<b>&c",
                    "empty": ""
                }]
            })
        );
    }

    #[test]
    fn whitespace_inside_strings_is_preserved_and_crlf_normalized() {
        let v = parse_plist("<plist><dict><key>k</key><string>  a\r\n b </string></dict></plist>")
            .unwrap();
        assert_eq!(v, json!({"k": "  a\n b "}));
    }

    #[test]
    fn root_without_plist_wrapper() {
        assert_eq!(
            parse_plist("<array><true/><integer>-3</integer></array>").unwrap(),
            json!([true, -3])
        );
    }

    #[test]
    fn errors_are_reported_not_panicked() {
        for bad in [
            "",
            "<plist>",
            "<plist><dict><key>a</key></dict></plist>",
            "<plist><dict><string>x</string></dict></plist>",
            "<plist><foo/></plist>",
            "<plist><integer>abc</integer></plist>",
            "<plist><string>unterminated",
            "<plist><dict></plist>",
            "<plist><array></array></plist> trailing",
            "<!-- never closed",
            "<plist><string><![CDATA[x</string></plist>",
        ] {
            assert!(parse_plist(bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn unknown_entities_are_kept_verbatim() {
        let v = parse_plist("<string>a &nbsp; b &amp c &#xZZ;</string>").unwrap();
        assert_eq!(v, json!("a &nbsp; b &amp c &#xZZ;"));
    }

    #[test]
    fn deep_nesting_is_rejected() {
        let src = format!(
            "{}{}",
            "<array>".repeat(MAX_DEPTH + 10),
            "</array>".repeat(MAX_DEPTH + 10)
        );
        assert!(parse_plist(&src).is_err());
    }
}
