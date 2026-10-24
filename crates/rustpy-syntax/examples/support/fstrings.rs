//! Independent CPython3.12 JoinedStr mapping from actual lossless CST.
//! Literal decoding and expression mapping are supplied by the independent exporter.
use super::{Result, Unsupported};
use rustpy_syntax::{ast::*, SyntaxKind, SyntaxNode};
use serde_json::{json, Value};
const MAX_PARTS: usize = 4096;
const MAX_TEXT: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
fn fail(message: &str) -> Unsupported {
    Unsupported(format!("F-string: {message}"))
}
fn node(kind: &str, fields: Value) -> Value {
    json!({"node":kind,"fields":fields})
}
fn text_weight(value: &Value) -> Result<usize> {
    if let Some(text) = value.as_str() {
        return Ok(text.len());
    }
    let object = value
        .as_object()
        .ok_or_else(|| fail("non-text decoded value"))?;
    if object.len() != 2 || object.get("literal_type").and_then(Value::as_str) != Some("str") {
        return Err(fail("unrecognized decoded text protocol"));
    }
    let points = object
        .get("codepoints")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("missing string codepoints"))?;
    if points.len() > MAX_TEXT {
        return Err(fail("codepoint budget exceeded"));
    }
    let mut weight = 0usize;
    let mut surrogate = false;
    for point in points {
        let point = point
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n <= 0x10ffff)
            .ok_or_else(|| fail("invalid string codepoint"))?;
        let width = if (0xd800..=0xdfff).contains(&point) {
            surrogate = true;
            3
        } else {
            char::from_u32(point)
                .ok_or_else(|| fail("invalid scalar"))?
                .len_utf8()
        };
        weight = weight.saturating_add(width);
        if weight > MAX_TEXT {
            return Err(fail("codepoint text budget exceeded"));
        }
    }
    if !surrogate {
        return Err(fail("noncanonical tagged scalar string"));
    }
    Ok(weight)
}
fn points(value: &Value) -> Vec<u32> {
    if let Some(text) = value.as_str() {
        text.chars().map(u32::from).collect()
    } else {
        // text_weight validates every tagged codepoint before this conversion.
        value["codepoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_u64().unwrap() as u32)
            .collect()
    }
}
fn merged_text(left: &Value, right: &Value) -> Result<Value> {
    if text_weight(left)?.saturating_add(text_weight(right)?) > MAX_TEXT {
        return Err(fail("merged text budget exceeded"));
    }
    if let (Some(left), Some(right)) = (left.as_str(), right.as_str()) {
        let mut text = left.to_owned();
        text.push_str(right);
        return Ok(Value::String(text));
    }
    let mut codepoints = points(left);
    codepoints.extend(points(right));
    // Keep individual surrogate codepoints. Never combine a UTF16 surrogate pair.
    Ok(json!({"literal_type":"str","codepoints":codepoints}))
}
fn text(values: &mut Vec<Value>, value: Value, kind: Option<String>) -> Result<()> {
    if text_weight(&value)? > MAX_TEXT {
        return Err(fail("text budget exceeded"));
    }
    if let Some(last) = values.last_mut() {
        if last.get("node").and_then(Value::as_str) == Some("Constant") {
            let merged = merged_text(&last["fields"]["value"], &value)?;
            last["fields"]["value"] = merged;
            return Ok(());
        }
    }
    push(values, node("Constant", json!({"value":value,"kind":kind})))
}
fn push(values: &mut Vec<Value>, value: Value) -> Result<()> {
    if values.len() >= MAX_PARTS {
        return Err(fail("part budget exceeded"));
    }
    values.push(value);
    Ok(())
}
fn joined(values: Vec<Value>) -> Value {
    node("JoinedStr", json!({"values":values}))
}
/// `decode_text` decodes a literal payload (not a full quoted token), with raw mode.
/// Value is ordinary JSON String or protocol-v2 str/codepoints containing surrogates.
/// `decode_literal` accepts only a normal text literal, rejecting bytes.
pub(super) fn expression<E, D, L>(
    value: Expr,
    expr: &mut E,
    decode_text: &mut D,
    decode_literal: &mut L,
) -> Result<Value>
where
    E: FnMut(Expr) -> Result<Value>,
    D: FnMut(&str, bool) -> Result<Value>,
    L: FnMut(Expr) -> Result<(Value, Option<String>)>,
{
    let mut values = Vec::new();
    match value {
        Expr::FString(f) => fstring(&f, &mut values, expr, decode_text, 0)?,
        Expr::ConcatString(c) => {
            let parts: Vec<_> = c.parts().collect();
            if parts.len() > MAX_PARTS {
                return Err(fail("concatenation budget exceeded"));
            }
            if !parts.iter().any(|p| matches!(p, Expr::FString(_))) {
                return Err(fail("ordinary concatenation belongs to literal mapper"));
            }
            for part in parts {
                match part {
                    Expr::FString(f) => fstring(&f, &mut values, expr, decode_text, 0)?,
                    Expr::String(_) => {
                        let (value, kind) = decode_literal(part)?;
                        text(&mut values, value, kind)?;
                    }
                    _ => return Err(fail("unrecognized adjacent string part")),
                }
            }
        }
        _ => return Err(fail("not a formatted string expression")),
    }
    values.retain(|v| {
        !(v.get("node").and_then(Value::as_str) == Some("Constant")
            && v["fields"]["value"].as_str() == Some(""))
    });
    Ok(joined(values))
}
fn fstring<E, D>(
    value: &FStringExpr,
    values: &mut Vec<Value>,
    expr: &mut E,
    decode: &mut D,
    depth: usize,
) -> Result<()>
where
    E: FnMut(Expr) -> Result<Value>,
    D: FnMut(&str, bool) -> Result<Value>,
{
    let start = value
        .syntax()
        .first_token()
        .ok_or_else(|| fail("missing start"))?;
    if start.kind() != SyntaxKind::FSTRING_START || value.is_template() {
        return Err(fail(
            "Python3.14 template strings are not Python3.12 f-strings",
        ));
    }
    let prefix = start
        .text()
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .count();
    let prefix = start.text()[..prefix].to_ascii_lowercase();
    if !matches!(prefix.as_str(), "f" | "fr" | "rf") {
        return Err(fail("unrecognized formatted prefix"));
    }
    pieces(
        value.syntax(),
        values,
        expr,
        decode,
        prefix.contains('r'),
        false,
        depth,
    )
}
fn pieces<E, D>(
    syntax: &SyntaxNode,
    values: &mut Vec<Value>,
    expr: &mut E,
    decode: &mut D,
    raw: bool,
    format_spec: bool,
    depth: usize,
) -> Result<()>
where
    E: FnMut(Expr) -> Result<Value>,
    D: FnMut(&str, bool) -> Result<Value>,
{
    if depth > MAX_DEPTH {
        return Err(fail("format nesting budget exceeded"));
    }
    let mut previous_middle = false;
    let mut final_replacement = false;
    for element in syntax.children_with_tokens() {
        if let Some(token) = element.as_token() {
            match token.kind() {
                SyntaxKind::FSTRING_MIDDLE => {
                    // Doubled braces are lexical text escapes only outside a format spec.
                    // Collapse before literal decoding, so decoded Unicode braces stay text.
                    let payload = if format_spec {
                        token.text().to_owned()
                    } else {
                        token.text().replace("{{", "{").replace("}}", "}")
                    };
                    previous_middle = true;
                    final_replacement = false;
                    let decoded = decode(&payload, raw)?;
                    if format_spec {
                        if text_weight(&decoded)? > MAX_TEXT {
                            return Err(fail("format text budget exceeded"));
                        }
                        push(
                            values,
                            node("Constant", json!({"value":decoded,"kind":null})),
                        )?;
                    } else {
                        text(values, decoded, None)?;
                    }
                }
                SyntaxKind::FSTRING_START | SyntaxKind::FSTRING_END if !format_spec => {}
                SyntaxKind::COLON if format_spec => {}
                SyntaxKind::WHITESPACE | SyntaxKind::COMMENT => {}
                _ => return Err(fail("unexpected direct literal/spec token")),
            }
        } else if let Some(child) = element.as_node() {
            let replacement = FStringReplacement::cast(child.clone())
                .ok_or_else(|| fail("unrecognized direct literal/spec child"))?;
            // CPython's format tokenizer emits an empty middle before a field
            // beginning with a dict/set brace when no preceding middle was emitted.
            if format_spec
                && !previous_middle
                && replacement.syntax().text().to_string().starts_with("{{")
            {
                push(values, node("Constant", json!({"value":"","kind":null})))?;
            }
            if !format_spec
                && values.last().is_some_and(|v| {
                    v.get("node").and_then(Value::as_str) == Some("Constant")
                        && v["fields"]["value"].as_str() == Some("")
                })
            {
                values.pop();
            }
            replacement_values(&replacement, values, expr, decode, raw, depth + 1)?;
            previous_middle = false;
            final_replacement = true;
        }
    }
    // CPython3.12 emits a trailing empty FSTRING_MIDDLE after a final field
    // in a format spec. The host lexer drops zero-length tokens, so restore it.
    if format_spec && final_replacement {
        push(values, node("Constant", json!({"value":"","kind":null})))?;
    }
    Ok(())
}
fn replacement_values<E, D>(
    replacement: &FStringReplacement,
    values: &mut Vec<Value>,
    expr: &mut E,
    decode: &mut D,
    raw: bool,
    depth: usize,
) -> Result<()>
where
    E: FnMut(Expr) -> Result<Value>,
    D: FnMut(&str, bool) -> Result<Value>,
{
    if depth > MAX_DEPTH {
        return Err(fail("replacement nesting budget exceeded"));
    }
    let syntax = replacement.syntax();
    let conversion = syntax
        .children()
        .find(|n| n.kind() == SyntaxKind::FSTRING_CONVERSION);
    let spec = syntax
        .children()
        .find(|n| n.kind() == SyntaxKind::FSTRING_FORMAT_SPEC);
    let debug = syntax
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| t.kind() == SyntaxKind::EQ);
    let mut conversion_code = if debug && spec.is_none() { 114 } else { -1 };
    if let Some(conversion) = conversion.as_ref() {
        let name = conversion
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT)
            .ok_or_else(|| fail("missing conversion identifier"))?;
        conversion_code = match name.text() {
            "r" => 114,
            "s" => 115,
            "a" => 97,
            _ => return Err(fail("unsupported conversion")),
        };
    }
    if debug {
        let delimiter = conversion
            .as_ref()
            .or(spec.as_ref())
            .map(|n| n.text_range().start())
            .or_else(|| {
                syntax
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .find(|t| t.kind() == SyntaxKind::R_BRACE)
                    .map(|t| t.text_range().start())
            })
            .ok_or_else(|| fail("missing debug delimiter"))?;
        let end = usize::from(delimiter - syntax.text_range().start());
        let source = syntax.text().to_string();
        let prefix = source
            .get(1..end)
            .ok_or_else(|| fail("invalid debug source range"))?;
        // CPython decodes debug text using the enclosing f-string's raw mode.
        let prefix = prefix.replace("\r\n", "\n").replace('\r', "\n");
        // CPython 3.12.3 tokenizer metadata strips every '#' through newline,
        // including quoted hashes; this is exporter version parity, not lexing.
        let mut debug_text = String::with_capacity(prefix.len());
        for line in prefix.split_inclusive('\n') {
            if let Some((before_hash, _)) = line.split_once('#') {
                debug_text.push_str(before_hash);
                if line.ends_with('\n') {
                    debug_text.push('\n');
                }
            } else {
                debug_text.push_str(line);
            }
        }
        text(values, decode(&debug_text, raw)?, None)?;
    }
    let expression = expr(
        replacement
            .expr()
            .ok_or_else(|| fail("missing replacement expression"))?,
    )?;
    let format = if let Some(spec) = spec {
        let mut spec_values = Vec::new();
        pieces(&spec, &mut spec_values, expr, decode, raw, true, depth)?;
        if spec_values.len() == 1
            && spec_values[0].get("node").and_then(Value::as_str) == Some("Constant")
            && spec_values[0]["fields"]["value"].as_str() == Some("")
        {
            spec_values.clear();
        }
        joined(spec_values)
    } else {
        Value::Null
    };
    push(
        values,
        node(
            "FormattedValue",
            json!({"value":expression,"conversion":conversion_code,"format_spec":format}),
        ),
    )
}

#[cfg(test)]
mod value_protocol_tests {
    use super::*;
    #[test]
    fn text_merge_preserves_kind_individual_surrogates_and_scalar_emoji() {
        let mut values = Vec::new();
        text(&mut values, json!("π"), Some("u".into())).unwrap();
        text(
            &mut values,
            json!({"literal_type":"str","codepoints":[55357,56832]}),
            None,
        )
        .unwrap();
        text(&mut values, json!("🙂"), None).unwrap();
        assert_eq!(
            values,
            vec![node(
                "Constant",
                json!({"value":{"literal_type":"str","codepoints":[960,55357,56832,128578]},"kind":"u"})
            )]
        );
    }
    #[test]
    fn tagged_text_on_both_sides_retains_first_kind_and_empty_prefix() {
        let mut values = Vec::new();
        text(&mut values, json!(""), Some("u".into())).unwrap();
        text(
            &mut values,
            json!({"literal_type":"str","codepoints":[55296,65]}),
            None,
        )
        .unwrap();
        text(
            &mut values,
            json!({"literal_type":"str","codepoints":[66,57343]}),
            Some("ignored".into()),
        )
        .unwrap();
        assert_eq!(
            values[0]["fields"],
            json!({"value":{"literal_type":"str","codepoints":[55296,65,66,57343]},"kind":"u"})
        );
        assert_eq!(merged_text(&json!("α"), &json!("β")).unwrap(), json!("αβ"));
    }
    #[test]
    fn malformed_protocol_and_over_budget_merge_refuse_without_changing_values() {
        for value in [
            json!(7),
            json!({"literal_type":"str","codepoints":[65]}),
            json!({"literal_type":"str","codepoints":[1114112]}),
            json!({"literal_type":"str","codepoints":[55296.5]}),
            json!({"literal_type":"str","codepoints":[55296],"extra":true}),
        ] {
            assert!(text_weight(&value).is_err());
        }
        let mut values = Vec::new();
        text(&mut values, json!("a".repeat(MAX_TEXT)), None).unwrap();
        let original = values.clone();
        assert!(text(
            &mut values,
            json!({"literal_type":"str","codepoints":[55296]}),
            None
        )
        .is_err());
        assert_eq!(values, original);
    }
}
