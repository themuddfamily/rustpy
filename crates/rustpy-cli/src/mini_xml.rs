//! A small XML reader for `.idea`-style configuration files (code style schemes,
//! inspection profiles): elements and their attributes, nothing else. Text,
//! comments, processing instructions, `<!DOCTYPE>` and CDATA are skipped.

/// A start tag (`<a x="1">` or `<a x="1"/>`) or an end tag (`</a>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tag {
    Start {
        name: String,
        attrs: Vec<(String, String)>,
        empty: bool,
    },
    End {
        name: String,
    },
}

impl Tag {
    /// The value of attribute `key` of a start tag.
    pub fn attr(&self, key: &str) -> Option<&str> {
        match self {
            Tag::Start { attrs, .. } => attrs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str()),
            Tag::End { .. } => None,
        }
    }
}

/// Replaces the predefined entities and character references.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e <= 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let ch = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The tags of `xml` in document order.
pub fn tags(xml: &str) -> Result<Vec<Tag>, String> {
    let mut out = Vec::new();
    let mut rest = xml;
    let mut depth: Vec<String> = Vec::new();
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        let skip_to = |rest: &str, close: &str| rest.find(close).map(|e| e + close.len());
        if rest.starts_with("<!--") {
            rest = &rest[skip_to(rest, "-->").ok_or("unterminated comment")?..];
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            rest = &rest[skip_to(rest, "]]>").ok_or("unterminated CDATA section")?..];
            continue;
        }
        if rest.starts_with("<?") {
            rest = &rest[skip_to(rest, "?>").ok_or("unterminated processing instruction")?..];
            continue;
        }
        if rest.starts_with("<!") {
            rest = &rest[skip_to(rest, ">").ok_or("unterminated declaration")?..];
            continue;
        }
        // A tag: find its end outside quoted attribute values.
        let bytes = rest.as_bytes();
        let mut quote: Option<u8> = None;
        let mut end = None;
        for (j, &b) in bytes.iter().enumerate().skip(1) {
            match quote {
                Some(q) if b == q => quote = None,
                Some(_) => {}
                None if b == b'"' || b == b'\'' => quote = Some(b),
                None if b == b'>' => {
                    end = Some(j);
                    break;
                }
                None => {}
            }
        }
        let end = end.ok_or("unterminated tag")?;
        let inner = &rest[1..end];
        rest = &rest[end + 1..];
        if let Some(name) = inner.strip_prefix('/') {
            let name = name.trim().to_string();
            match depth.pop() {
                Some(open) if open == name => {}
                Some(open) => return Err(format!("</{name}> closes <{open}>")),
                None => return Err(format!("</{name}> without a start tag")),
            }
            out.push(Tag::End { name });
            continue;
        }
        let (inner, empty) = match inner.strip_suffix('/') {
            Some(i) => (i, true),
            None => (inner, false),
        };
        let name_end = inner
            .find(|c: char| c.is_whitespace())
            .unwrap_or(inner.len());
        let name = inner[..name_end].to_string();
        if name.is_empty() {
            return Err("a tag without a name".into());
        }
        let mut attrs = Vec::new();
        let mut a = inner[name_end..].trim_start();
        while !a.is_empty() {
            let eq = a
                .find('=')
                .ok_or_else(|| format!("attribute without a value in <{name}>"))?;
            let key = a[..eq].trim().to_string();
            let v = a[eq + 1..].trim_start();
            let q = v
                .chars()
                .next()
                .filter(|c| *c == '"' || *c == '\'')
                .ok_or_else(|| format!("unquoted attribute value in <{name}>"))?;
            let close = v[1..]
                .find(q)
                .ok_or_else(|| format!("unterminated attribute value in <{name}>"))?;
            attrs.push((key, unescape(&v[1..1 + close])));
            a = v[close + 2..].trim_start();
        }
        if !empty {
            depth.push(name.clone());
        }
        out.push(Tag::Start { name, attrs, empty });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tags_and_attributes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- a comment with <tags> -->
<component name="X">
  <option name="A" value="a &amp; &quot;b&quot; &#65;&#x42;" />
  <option name='B' value='1>0'/>
  text <![CDATA[ <not-a-tag> ]]>
</component>"#;
        let t = tags(xml).unwrap();
        assert_eq!(t.len(), 4);
        assert_eq!(t[0].attr("name"), Some("X"));
        assert_eq!(t[1].attr("value"), Some("a & \"b\" AB"));
        assert_eq!(
            t[2],
            Tag::Start {
                name: "option".into(),
                attrs: vec![("name".into(), "B".into()), ("value".into(), "1>0".into())],
                empty: true
            }
        );
        assert_eq!(
            t[3],
            Tag::End {
                name: "component".into()
            }
        );
    }

    #[test]
    fn rejects_broken_documents() {
        assert!(tags("<a><b></a>").is_err());
        assert!(tags("<a x=1/>").is_err());
        assert!(tags("<a x=\"1/>").is_err());
        assert!(tags("</a>").is_err());
        assert!(tags("<!-- open").is_err());
        assert_eq!(unescape("a &unknown; b & c"), "a &unknown; b & c");
    }
}
