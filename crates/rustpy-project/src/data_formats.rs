//! Bounded data parsing, source paths and outlines shared by editor consumers.
use serde_json::{Map, Number, Value};
use std::{ops::Range, path::Path};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonMode {
    Strict,
    Json5,
    Lines,
}
#[derive(Clone, Debug)]
pub struct Node {
    pub path: String,
    pub range: Range<usize>,
    pub depth: usize,
    pub label: String,
}
#[derive(Clone, Debug)]
pub struct Parsed {
    pub value: Value,
    pub nodes: Vec<Node>,
    pub nonfinite: bool,
}
#[derive(Clone, Debug)]
pub struct Error {
    pub offset: usize,
    pub message: String,
}
pub fn mode(path: &Path) -> JsonMode {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
    {
        "json5" | "jsonc" => JsonMode::Json5,
        "jsonl" | "ndjson" => JsonMode::Lines,
        _ => JsonMode::Strict,
    }
}
pub fn parse_json(source: &str, mode: JsonMode) -> Result<Parsed, Error> {
    if source.len() > 2 * 1024 * 1024 {
        return Err(Error {
            offset: 0,
            message: "Data document exceeds the 2 MiB structured preview budget".into(),
        });
    }
    if mode == JsonMode::Lines {
        let mut values = Vec::new();
        let mut nodes = Vec::new();
        let mut offset = 0;
        for (line, text) in source.split_inclusive('\n').enumerate() {
            let consumed = text.len();
            let text = text.trim_end_matches(['\n', '\r']);
            if text.trim().is_empty() {
                return Err(Error {
                    offset,
                    message: format!("Blank JSON Lines record at line {}", line + 1),
                });
            }
            let mut parsed = parse_json(text, JsonMode::Strict).map_err(|mut error| {
                error.offset += offset;
                error
            })?;
            for node in &mut parsed.nodes {
                node.path = format!("/{line}{}", node.path);
                node.range = node.range.start + offset..node.range.end + offset;
                node.depth += 1;
            }
            if nodes.len() + parsed.nodes.len() > 20000 {
                return Err(Error {
                    offset,
                    message: "JSON Lines node budget exceeded".into(),
                });
            }
            nodes.extend(parsed.nodes);
            values.push(parsed.value);
            offset += consumed;
        }
        return Ok(Parsed {
            value: Value::Array(values),
            nodes,
            nonfinite: false,
        });
    }
    if mode == JsonMode::Strict {
        serde_json::from_str::<Value>(source).map_err(|error| {
            let start = source
                .split_inclusive('\n')
                .take(error.line().saturating_sub(1))
                .map(str::len)
                .sum::<usize>();
            Error {
                offset: (start + error.column().saturating_sub(1)).min(source.len()),
                message: error.to_string(),
            }
        })?;
    }
    let mut parser = Parser {
        source,
        at: 0,
        relaxed: mode == JsonMode::Json5,
        nodes: Vec::new(),
        nonfinite: false,
    };
    let value = parser.value("", 0)?;
    parser.space()?;
    if parser.at != source.len() {
        return parser.fail("Unexpected trailing content");
    }
    Ok(Parsed {
        value,
        nodes: parser.nodes,
        nonfinite: parser.nonfinite,
    })
}
struct Parser<'a> {
    source: &'a str,
    at: usize,
    relaxed: bool,
    nodes: Vec<Node>,
    nonfinite: bool,
}
impl Parser<'_> {
    fn fail<T>(&self, message: &str) -> Result<T, Error> {
        Err(Error {
            offset: self.at,
            message: message.into(),
        })
    }
    fn peek(&self) -> Option<char> {
        self.source[self.at..].chars().next()
    }
    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += c.len_utf8();
        Some(c)
    }
    fn space(&mut self) -> Result<(), Error> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.next();
            }
            if self.relaxed && self.source[self.at..].starts_with("//") {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.next();
                }
            } else if self.relaxed && self.source[self.at..].starts_with("/*") {
                let Some(end) = self.source[self.at + 2..].find("*/") else {
                    return self.fail("Unterminated JSON5 comment");
                };
                self.at += end + 4;
            } else {
                return Ok(());
            }
        }
    }
    fn string(&mut self) -> Result<String, Error> {
        let quote = self.next().unwrap();
        let mut out = String::new();
        while let Some(c) = self.next() {
            if c == quote {
                return Ok(out);
            }
            if c == '\\' {
                let Some(escape) = self.next() else {
                    return self.fail("Unterminated string escape");
                };
                match escape {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'v' if self.relaxed => out.push('\u{b}'),
                    '0' if self.relaxed && !self.peek().is_some_and(|c| c.is_ascii_digit()) => {
                        out.push('\0')
                    }
                    '\n' if self.relaxed => {}
                    '\r' if self.relaxed => {
                        if self.peek() == Some('\n') {
                            self.next();
                        }
                    }
                    'u' | 'x' => {
                        let width = if escape == 'u' {
                            4
                        } else if self.relaxed {
                            2
                        } else {
                            return self.fail("Invalid escape");
                        };
                        let start = self.at;
                        for _ in 0..width {
                            if !self.next().is_some_and(|c| c.is_ascii_hexdigit()) {
                                return self.fail("Invalid Unicode escape");
                            }
                        }
                        let code = u32::from_str_radix(&self.source[start..self.at], 16).unwrap();
                        let code = if (0xd800..=0xdbff).contains(&code) {
                            if !self.source[self.at..].starts_with("\\u") {
                                return self.fail("Missing low surrogate");
                            };
                            self.at += 2;
                            let start = self.at;
                            for _ in 0..4 {
                                if !self.next().is_some_and(|c| c.is_ascii_hexdigit()) {
                                    return self.fail("Invalid low surrogate");
                                }
                            }
                            let low =
                                u32::from_str_radix(&self.source[start..self.at], 16).unwrap();
                            if !(0xdc00..=0xdfff).contains(&low) {
                                return self.fail("Invalid low surrogate");
                            };
                            0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00)
                        } else {
                            code
                        };
                        let Some(c) = char::from_u32(code) else {
                            return self.fail("Invalid Unicode scalar");
                        };
                        out.push(c);
                    }
                    other if self.relaxed || matches!(other, '"' | '\\' | '/') => out.push(other),
                    _ => return self.fail("Invalid string escape"),
                }
            } else {
                if c < ' ' || c == '\u{2028}' || c == '\u{2029}' {
                    return self.fail("Unescaped string newline/control character");
                };
                out.push(c);
            }
        }
        self.fail("Unterminated string")
    }
    fn value(&mut self, path: &str, depth: usize) -> Result<Value, Error> {
        if depth > 96 || self.nodes.len() > 20000 {
            return self.fail("Structured data nesting/node budget exceeded");
        };
        self.space()?;
        let start = self.at;
        let index = self.nodes.len();
        self.nodes.push(Node {
            path: path.into(),
            range: start..start,
            depth,
            label: path
                .rsplit('/')
                .next()
                .unwrap_or("$")
                .replace("~1", "/")
                .replace("~0", "~"),
        });
        let value = match self.peek() {
            Some('{') => {
                self.next();
                let mut object = Map::new();
                self.space()?;
                if self.peek() == Some('}') {
                    self.next();
                    Value::Object(object)
                } else {
                    loop {
                        self.space()?;
                        let key = match self.peek() {
                            Some('"') => self.string()?,
                            Some('\'') if self.relaxed => self.string()?,
                            Some(c)
                                if self.relaxed
                                    && (rustpy_core::is_ident_start(c)
                                        || c == '$'
                                        || c == '\\') =>
                            {
                                let mut key = String::new();
                                loop {
                                    let character = if self.peek() == Some('\\') {
                                        self.next();
                                        if self.next() != Some('u') {
                                            return self.fail("Expected Unicode identifier escape");
                                        }
                                        let mut value = 0u32;
                                        for _ in 0..4 {
                                            let Some(digit) =
                                                self.next().and_then(|c| c.to_digit(16))
                                            else {
                                                return self
                                                    .fail("Invalid Unicode identifier escape");
                                            };
                                            value = value * 16 + digit;
                                        }
                                        let Some(character) = char::from_u32(value) else {
                                            return self.fail("Invalid Unicode identifier scalar");
                                        };
                                        character
                                    } else {
                                        let Some(character) = self.peek() else { break };
                                        if !(rustpy_core::is_ident_continue(character)
                                            || character == '$')
                                        {
                                            break;
                                        }
                                        self.next();
                                        character
                                    };
                                    if !(if key.is_empty() {
                                        rustpy_core::is_ident_start(character)
                                    } else {
                                        rustpy_core::is_ident_continue(character)
                                    } || character == '$')
                                    {
                                        return self.fail("Invalid JSON5 identifier character");
                                    }
                                    key.push(character);
                                }
                                key
                            }
                            _ => return self.fail("Expected object property name"),
                        };
                        self.space()?;
                        if self.next() != Some(':') {
                            return self.fail("Expected colon after property name");
                        };
                        let child = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                        object.insert(key, self.value(&child, depth + 1)?);
                        self.space()?;
                        match self.next() {
                            Some('}') => break,
                            Some(',') => {
                                self.space()?;
                                if self.peek() == Some('}') && self.relaxed {
                                    self.next();
                                    break;
                                }
                            }
                            _ => return self.fail("Expected comma or closing brace"),
                        }
                    }
                    Value::Object(object)
                }
            }
            Some('[') => {
                self.next();
                let mut values = Vec::new();
                self.space()?;
                if self.peek() == Some(']') {
                    self.next();
                } else {
                    loop {
                        values.push(self.value(&format!("{path}/{}", values.len()), depth + 1)?);
                        self.space()?;
                        match self.next() {
                            Some(']') => break,
                            Some(',') => {
                                self.space()?;
                                if self.peek() == Some(']') && self.relaxed {
                                    self.next();
                                    break;
                                }
                            }
                            _ => return self.fail("Expected comma or closing bracket"),
                        }
                    }
                }
                Value::Array(values)
            }
            Some('"') => Value::String(self.string()?),
            Some('\'') if self.relaxed => Value::String(self.string()?),
            Some(_) => {
                let start = self.at;
                while self
                    .peek()
                    .is_some_and(|c| !c.is_whitespace() && !matches!(c, ',' | '}' | ']' | '/'))
                {
                    self.next();
                }
                let token = &self.source[start..self.at];
                match token {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    "null" => Value::Null,
                    "Infinity" | "+Infinity" | "-Infinity" | "NaN" | "+NaN" | "-NaN"
                        if self.relaxed =>
                    {
                        self.nonfinite = true;
                        Value::String(token.into())
                    }
                    _ => {
                        if token.is_empty() {
                            return self.fail("Expected value");
                        };
                        let unsigned = token.trim_start_matches(['+', '-']);
                        let number = if self.relaxed
                            && (unsigned.starts_with("0x") || unsigned.starts_with("0X"))
                        {
                            let n = u64::from_str_radix(&unsigned[2..], 16).map_err(|_| Error {
                                offset: start,
                                message: "Invalid JSON5 hexadecimal number".into(),
                            })?;
                            if token.starts_with('-') {
                                Number::from_f64(-(n as f64))
                            } else {
                                Some(Number::from(n))
                            }
                        } else {
                            if unsigned.len() > 1
                                && unsigned.starts_with('0')
                                && unsigned.as_bytes()[1].is_ascii_digit()
                            {
                                return self.fail("Leading zero in numeric literal");
                            };
                            token.parse::<Number>().ok().or_else(|| {
                                self.relaxed
                                    .then(|| token.parse::<f64>().ok().and_then(Number::from_f64))
                                    .flatten()
                            })
                        };
                        Value::Number(number.ok_or_else(|| Error {
                            offset: start,
                            message: "Invalid numeric literal".into(),
                        })?)
                    }
                }
            }
            None => return self.fail("Expected a value"),
        };
        self.nodes[index].range.end = self.at;
        Ok(value)
    }
}
pub fn json_path(parsed: &Parsed, offset: usize) -> String {
    let pointer = parsed
        .nodes
        .iter()
        .filter(|node| node.range.start <= offset && offset <= node.range.end)
        .max_by_key(|node| node.depth)
        .map(|node| node.path.as_str())
        .unwrap_or("");
    let mut out = "$".to_string();
    for part in pointer.split('/').skip(1) {
        let part = part.replace("~1", "/").replace("~0", "~");
        if part.parse::<usize>().is_ok() {
            out.push_str(&format!("[{part}]"));
        } else {
            out.push_str(&format!("[{}]", serde_json::to_string(&part).unwrap()));
        }
    }
    out
}
pub fn yaml_value(source: &str) -> Result<Value, Error> {
    let documents = rustpy_syntax::yaml::parse(source).map_err(|error| Error {
        offset: source
            .split_inclusive('\n')
            .take(error.line.saturating_sub(1))
            .map(str::len)
            .sum(),
        message: error.message,
    })?;
    fn node(doc: &rustpy_syntax::yaml::Document, id: usize, depth: usize) -> Result<Value, Error> {
        use rustpy_syntax::yaml::Kind;
        if depth > 64 {
            return Err(Error {
                offset: 0,
                message: "YAML schema depth limit".into(),
            });
        }
        Ok(match &doc.nodes[id].kind {
            Kind::Scalar { value, style } => {
                if format!("{style:?}") == "Plain" {
                    match value.as_str() {
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        "null" | "~" => Value::Null,
                        _ => serde_json::from_str(value)
                            .unwrap_or_else(|_| Value::String(value.clone())),
                    }
                } else {
                    Value::String(value.clone())
                }
            }
            Kind::Sequence(ids) => Value::Array(
                ids.iter()
                    .map(|id| node(doc, *id, depth + 1))
                    .collect::<Result<_, _>>()?,
            ),
            Kind::Mapping(entries) => {
                let mut result = Map::new();
                for (key, value) in entries {
                    let name = match &doc.nodes[*key].kind {
                        Kind::Scalar { value, .. } => value.clone(),
                        _ => {
                            return Err(Error {
                                offset: 0,
                                message: "Schema validation requires scalar YAML mapping keys"
                                    .into(),
                            })
                        }
                    };
                    result.insert(name, node(doc, *value, depth + 1)?);
                }
                Value::Object(result)
            }
            Kind::Alias(_) => {
                return Err(Error {
                    offset: 0,
                    message: "YAML alias graphs are not expanded for schema validation".into(),
                })
            }
        })
    }
    let mut values = documents
        .iter()
        .map(|doc| node(doc, doc.root, 0))
        .collect::<Result<Vec<_>, _>>()?;
    if values.len() == 1 {
        Ok(values.remove(0))
    } else {
        Ok(Value::Array(values))
    }
}
pub fn value(source: &str, path: &Path) -> Result<Parsed, Error> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
    {
        "yaml" | "yml" => Ok(Parsed {
            value: yaml_value(source)?,
            nodes: Vec::new(),
            nonfinite: false,
        }),
        "toml" => {
            let value: toml::Value = source.parse().map_err(|error: toml::de::Error| Error {
                offset: error.span().map_or(0, |range| range.start),
                message: error.to_string(),
            })?;
            Ok(Parsed {
                value: serde_json::to_value(value).map_err(|error| Error {
                    offset: 0,
                    message: error.to_string(),
                })?,
                nodes: Vec::new(),
                nonfinite: false,
            })
        }
        _ => parse_json(source, mode(path)),
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn json5_comments_quotes_hex_trailing_comma_and_path() {
        let source = "{ // c\n unquoted: ['π', 0x10,], 'a/b': true, }";
        let parsed = parse_json(source, JsonMode::Json5).unwrap();
        assert_eq!(parsed.value["unquoted"][1], 16);
        assert_eq!(
            json_path(&parsed, source.find("π").unwrap()),
            "$[\"unquoted\"][0]"
        );
        assert_eq!(parsed.nodes.last().unwrap().path, "/a~1b");
        assert!(parse_json(source, JsonMode::Strict).is_err());
    }
    #[test]
    fn json_lines_error_offsets_stay_in_original_document() {
        let error = parse_json("{\"a\":1}\n[invalid]\n", JsonMode::Lines).unwrap_err();
        assert!(error.offset >= 8);
        let parsed = parse_json("{\"a\":1}\n{\"a\":2}\n", JsonMode::Lines).unwrap();
        assert_eq!(parsed.value.as_array().unwrap().len(), 2);
        assert_eq!(parsed.nodes.last().unwrap().path, "/1/a");
    }
    #[test]
    fn yaml_boolean_keys_are_not_yaml11_coerced() {
        let value = yaml_value("on:\n  push: {}\nname: test\n").unwrap();
        assert!(value.get("on").is_some());
    }
}

/// Layout-only JSON/JSON5 formatter: retain property order, duplicate keys,
/// numeric spellings and comments rather than round-tripping through a map.
pub fn format_json(source: &str, mode: JsonMode) -> Result<String, Error> {
    parse_json(source, mode)?;
    if mode == JsonMode::Lines {
        return source
            .lines()
            .map(compact)
            .collect::<Vec<_>>()
            .join("\n")
            .pipe_newline();
    }
    let mut out = String::new();
    let mut chars = source.chars().peekable();
    let mut depth = 0usize;
    let mut quote = None;
    let mut escape = false;
    let mut newline = true;
    while let Some(c) = chars.next() {
        if let Some(active) = quote {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == active {
                quote = None;
            }
            continue;
        }
        if matches!(c, '"' | '\'') {
            if newline {
                out.push_str(&"  ".repeat(depth));
                newline = false;
            }
            quote = Some(c);
            out.push(c);
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            if !newline {
                out.push(' ');
            }
            out.push_str("//");
            chars.next();
            for c in chars.by_ref() {
                out.push(c);
                if c == '\n' {
                    break;
                }
            }
            newline = true;
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            if newline {
                out.push_str(&"  ".repeat(depth));
                newline = false;
            }
            out.push_str("/*");
            chars.next();
            let mut previous = ' ';
            for c in chars.by_ref() {
                out.push(c);
                if previous == '*' && c == '/' {
                    break;
                }
                previous = c;
            }
            out.push(' ');
            continue;
        }
        if c.is_whitespace() {
            continue;
        }
        match c {
            '{' | '[' => {
                if newline {
                    out.push_str(&"  ".repeat(depth));
                }
                out.push(c);
                depth += 1;
                out.push('\n');
                newline = true;
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if !newline {
                    out.push('\n');
                }
                out.push_str(&"  ".repeat(depth));
                out.push(c);
                newline = false;
            }
            ',' => {
                out.push(',');
                out.push('\n');
                newline = true;
            }
            ':' => {
                out.push_str(": ");
                newline = false;
            }
            _ => {
                if newline {
                    out.push_str(&"  ".repeat(depth));
                    newline = false;
                }
                out.push(c);
            }
        }
    }
    Ok(format!("{}\n", out.trim_end()))
}
fn compact(source: &str) -> String {
    let mut result = String::new();
    let (mut quoted, mut escape) = (false, false);
    for c in source.chars() {
        if quoted {
            result.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                quoted = false;
            }
        } else if c == '"' {
            quoted = true;
            result.push(c);
        } else if !c.is_whitespace() {
            result.push(c);
        }
    }
    result
}
trait Newline {
    fn pipe_newline(self) -> Result<String, Error>;
}
impl Newline for String {
    fn pipe_newline(self) -> Result<String, Error> {
        Ok(format!("{}\n", self.trim_end()))
    }
}

#[cfg(test)]
mod additional_fixtures {
    use super::*;
    #[test]
    fn json5_unicode_identifier_and_integer_precision() {
        let parsed = parse_json(r"{\u0061:9007199254740993}", JsonMode::Json5).unwrap();
        assert_eq!(parsed.value["a"].as_u64(), Some(9007199254740993));
        assert!(parse_json(r"{\u0020:1}", JsonMode::Json5).is_err());
    }
    #[test]
    fn formatter_preserves_comments_and_numeric_spelling() {
        let source = "{ /* retained */ x:0x10, n:9007199254740993,}";
        let formatted = format_json(source, JsonMode::Json5).unwrap();
        assert!(formatted.contains("/* retained */"));
        assert!(formatted.contains("0x10"));
        assert!(formatted.contains("9007199254740993"));
        assert_eq!(
            parse_json(source, JsonMode::Json5).unwrap().value,
            parse_json(&formatted, JsonMode::Json5).unwrap().value
        );
    }
}
