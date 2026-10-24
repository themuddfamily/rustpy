//! Independent Python literal values. Unsupported semantics never become guessed values.
use super::{Result, Unsupported};
use serde_json::{json, Number, Value};
use std::str::FromStr;
use unicode_normalization::UnicodeNormalization;

pub(super) fn identifier(text: &str) -> Result<String> {
    if text.is_empty() {
        return Err(Unsupported("Empty identifier".into()));
    }
    if !super::unicode15_ident::identifier(text) {
        return Err(Unsupported(
            "Identifier outside CPython3.12 Unicode15 repertoire".into(),
        ));
    }
    Ok(text.nfkc().collect())
}
pub(super) fn decimal_limit_exceeded(text: &str) -> bool {
    let digits = text.replace('_', "");
    // CPython's lexer special-cases all-zero decimal literals: those need no
    // decimal-to-bigint conversion. Radix conversions are exempt from this limit.
    digits.bytes().all(|b| b.is_ascii_digit())
        && digits.bytes().any(|b| b != b'0')
        && digits.len() > 4300
}
pub(super) fn integer(text: &str) -> Result<Value> {
    if decimal_limit_exceeded(text) {
        return Err(Unsupported(
            "CPython default decimal4300-digit admission exceeded".into(),
        ));
    }
    let clean = text.replace('_', "");
    if clean.len() > 8192 {
        return Err(Unsupported(
            "Integer conversion exceeds 8192-digit budget".into(),
        ));
    }
    let (radix, digits) = if clean.starts_with("0x") || clean.starts_with("0X") {
        (16, &clean[2..])
    } else if clean.starts_with("0o") || clean.starts_with("0O") {
        (8, &clean[2..])
    } else if clean.starts_with("0b") || clean.starts_with("0B") {
        (2, &clean[2..])
    } else {
        (10, clean.as_str())
    };
    if digits.is_empty() {
        return Err(Unsupported("Empty integer".into()));
    }
    // Base conversion into little-endian decimal digits avoids machine-sized truncation.
    let mut decimal = vec![0u8];
    for c in digits.chars() {
        let mut carry = c
            .to_digit(radix)
            .ok_or_else(|| Unsupported("Invalid integer digit".into()))?;
        for d in &mut decimal {
            let n = u32::from(*d) * radix + carry;
            *d = (n % 10) as u8;
            carry = n / 10;
        }
        while carry > 0 {
            decimal.push((carry % 10) as u8);
            carry /= 10;
        }
    }
    let text: String = decimal
        .iter()
        .rev()
        .map(|d| char::from(b'0' + *d))
        .collect();
    Ok(Value::Number(Number::from_str(&text).map_err(|_| {
        Unsupported("JSON arbitrary_precision feature required".into())
    })?))
}
fn float_repr(value: f64) -> String {
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf"
        } else {
            "inf"
        }
        .into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let shortest = value.abs().to_string();
    let (mantissa, extra) = shortest
        .split_once(['e', 'E'])
        .map_or((shortest.as_str(), 0), |(m, e)| {
            (m, e.parse::<i32>().expect("Rust decimal exponent"))
        });
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let mut digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let leading = digits.chars().take_while(|c| *c == '0').count();
    digits.drain(..leading);
    let exponent = point + extra - leading as i32 - 1;
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    if !(-4..16).contains(&exponent) {
        let mut m = digits[..1].to_owned();
        if digits.len() > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        format!(
            "{sign}{m}e{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.abs()
        )
    } else {
        let point = exponent + 1;
        let value = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            format!(
                "{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        };
        format!("{sign}{value}")
    }
}
pub(super) fn float(text: &str, imaginary: bool) -> Result<Value> {
    let clean = text.trim_end_matches(['j', 'J']).replace('_', "");
    let value = clean
        .parse::<f64>()
        .map_err(|_| Unsupported("Invalid float literal".into()))?;
    if value.is_nan() {
        return Err(Unsupported("NaN literal".into()));
    }
    let mut repr = float_repr(value);
    if imaginary {
        if repr.ends_with(".0") {
            repr.truncate(repr.len() - 2);
        }
        repr.push('j');
    }
    Ok(json!({"literal_type":if imaginary{"complex"}else{"float"},"repr":repr}))
}
pub(super) use rustpy_syntax::string_literal::Decoded;
pub(super) fn string(raw: &str) -> Result<Decoded> {
    rustpy_syntax::string_literal::string(raw).map_err(|e| Unsupported(e.0))
}
pub(super) fn append(left: Decoded, right: Decoded) -> Result<Decoded> {
    rustpy_syntax::string_literal::append(left, right).map_err(|e| Unsupported(e.0))
}
fn decode_inner(text: &str, bytes: bool, raw_mode: bool) -> Result<Decoded> {
    rustpy_syntax::string_literal::decode_inner(text, bytes, raw_mode).map_err(|e| Unsupported(e.0))
}
#[cfg(test)]
pub(super) fn decode_text(text: &str, raw_mode: bool) -> Result<String> {
    match decode_inner(text, false, raw_mode)? {
        Decoded::Text(text, _) => Ok(text),
        _ => Err(Unsupported("Text is not scalar Unicode".into())),
    }
}
pub(super) fn constant(value: Decoded) -> Value {
    match value {
        Decoded::Text(value, kind) => json!({"value":value,"kind":kind}),
        Decoded::SurrogateText(points, kind) => {
            json!({"value":{"literal_type":"str","codepoints":points},"kind":kind})
        }
        Decoded::Bytes(bytes) => {
            let quote = if bytes.contains(&b'\'') && !bytes.contains(&b'"') {
                b'"'
            } else {
                b'\''
            };
            let mut repr = format!("b{}", char::from(quote));
            for b in bytes {
                match b {
                    b'\\' => repr.push_str("\\\\"),
                    b'\n' => repr.push_str("\\n"),
                    b'\r' => repr.push_str("\\r"),
                    b'\t' => repr.push_str("\\t"),
                    _ if b == quote => {
                        repr.push('\\');
                        repr.push(char::from(b));
                    }
                    32..=126 => repr.push(char::from(b)),
                    _ => repr.push_str(&format!("\\x{b:02x}")),
                }
            }
            repr.push(char::from(quote));
            json!({"value":{"literal_type":"bytes","repr":repr},"kind":null})
        }
    }
}

/// Lossless text payload for future JoinedStr callback integration.
pub(super) fn decode_text_value(text: &str, raw_mode: bool) -> Result<Value> {
    match decode_inner(text, false, raw_mode)? {
        Decoded::Text(text, _) => Ok(Value::String(text)),
        Decoded::SurrogateText(points, _) => Ok(json!({"literal_type":"str","codepoints":points})),
        Decoded::Bytes(_) => Err(Unsupported("Text decoder returned bytes".into())),
    }
}

#[cfg(test)]
mod remaining_tests {
    use super::*;
    #[test]
    fn primary_named_and_surrogate_constants() {
        let data: Value = serde_json::from_str(include_str!("literal-goldens.json")).unwrap();
        for fixture in data["fixtures"].as_array().unwrap() {
            let token = fixture["token"].as_str().unwrap();
            assert_eq!(
                constant(string(token).unwrap()),
                fixture["constant_fields"],
                "{token}"
            );
        }
        for fixture in data["concat"].as_array().unwrap() {
            let left = fixture["left"].as_str().unwrap();
            let right = fixture["right"].as_str().unwrap();
            assert_eq!(
                constant(append(string(left).unwrap(), string(right).unwrap()).unwrap()),
                fixture["constant_fields"],
                "{left} {right}"
            );
        }
        for fixture in data["rejected"].as_array().unwrap() {
            assert!(string(fixture["token"].as_str().unwrap()).is_err());
        }
    }
    #[test]
    fn lossless_payload_callback_retains_surrogate_pair() {
        assert_eq!(
            decode_text_value("\\ud83d\\ude00", false).unwrap(),
            json!({"literal_type":"str","codepoints":[55357,56832]})
        );
        assert_eq!(
            decode_text_value("\\ud800", true).unwrap(),
            Value::String("\\ud800".into())
        );
        assert!(decode_text("\\ud800", false).is_err());
    }
}
