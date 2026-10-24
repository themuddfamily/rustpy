//! UTF-16 LSP positions and validated source edits, independent of GUI/processes.
use rustpy_core::{TextRange, TextSize};
use serde_json::Value;
pub fn position(source: &str, offset: usize) -> Value {
    let offset = offset.min(source.len());
    let before = &source[..offset];
    let start = before.rfind('\n').map_or(0, |at| at + 1);
    serde_json::json!({"line":before.bytes().filter(|byte|*byte==b'\n').count(),"character":source[start..offset].encode_utf16().count()})
}
pub fn offset(source: &str, value: &Value) -> Result<usize, String> {
    let line = value["line"].as_u64().ok_or("Missing LSP line")? as usize;
    let character = value["character"]
        .as_u64()
        .ok_or("Missing LSP UTF-16 character")? as usize;
    let start = if line == 0 {
        0
    } else {
        source
            .match_indices('\n')
            .nth(line - 1)
            .map(|(at, _)| at + 1)
            .ok_or("LSP line outside document")?
    };
    let text = source[start..]
        .split(['\r', '\n'])
        .next()
        .unwrap_or_default();
    let mut units = 0;
    for (at, c) in text.char_indices() {
        if units == character {
            return Ok(start + at);
        }
        units += c.len_utf16();
        if units > character {
            return Err("LSP position splits a UTF-16 surrogate pair".into());
        }
    }
    if units == character {
        Ok(start + text.len())
    } else {
        Err("LSP column outside document".into())
    }
}
pub fn range(source: &str, value: &Value) -> Result<TextRange, String> {
    let start = offset(source, &value["start"])?;
    let end = offset(source, &value["end"])?;
    if start > end {
        return Err("Reversed LSP range".into());
    }
    Ok(TextRange::new(
        TextSize::new(start as u32),
        TextSize::new(end as u32),
    ))
}
pub fn edits(source: &str, values: &Value) -> Result<Vec<(TextRange, String)>, String> {
    let rows = values.as_array().ok_or("Expected LSP text edits")?;
    if rows.len() > 10000 {
        return Err("More than 10000 LSP edits".into());
    }
    let mut out = Vec::new();
    let mut bytes = 0;
    for value in rows {
        let text = value["newText"]
            .as_str()
            .ok_or("Missing LSP replacement text")?;
        bytes += text.len();
        if bytes > 4 * 1024 * 1024 {
            return Err("LSP replacements exceed 4 MiB".into());
        }
        out.push((range(source, &value["range"])?, text.into()));
    }
    out.sort_by_key(|(range, _)| (range.start(), range.end()));
    if out.windows(2).any(|pair| {
        pair[0].0.end() > pair[1].0.start() || (pair[0].0.is_empty() && pair[0].0 == pair[1].0)
    }) {
        return Err("Overlapping LSP edits".into());
    }
    Ok(out)
}
pub fn text(value: &Value) -> String {
    if let Some(value) = value.as_str() {
        value.into()
    } else if let Some(values) = value.as_array() {
        values.iter().map(text).collect::<Vec<_>>().join("\n\n")
    } else {
        value["value"]
            .as_str()
            .or_else(|| value["message"].as_str())
            .unwrap_or_default()
            .into()
    }
}
pub fn language(path: &std::path::Path) -> Option<(&'static str, &'static str)> {
    Some(match path.extension()?.to_str()? {
        "js" | "mjs" | "cjs" => ("typescript", "javascript"),
        "jsx" => ("typescript", "javascriptreact"),
        "ts" | "mts" | "cts" => ("typescript", "typescript"),
        "tsx" => ("typescript", "typescriptreact"),
        "html" | "htm" | "xhtml" => ("html", "html"),
        "css" => ("css", "css"),
        "scss" => ("css", "scss"),
        "less" => ("css", "less"),
        "json" | "jsonc" => ("json", "json"),
        "vue" => ("vue", "vue"),
        "svelte" => ("svelte", "svelte"),
        _ => return None,
    })
}
#[cfg(test)]
mod fixtures {
    use super::*;
    use serde_json::json;
    #[test]
    fn utf16_offsets_preserve_astral_and_crlf_boundaries() {
        let source = "π😀x\r\nnext";
        assert_eq!(
            position(source, "π😀".len()),
            json!({"line":0,"character":3})
        );
        assert_eq!(
            offset(source, &json!({"line":0,"character":3})).unwrap(),
            "π😀".len()
        );
        assert!(offset(source, &json!({"line":0,"character":2})).is_err());
        assert_eq!(
            offset(source, &json!({"line":1,"character":4})).unwrap(),
            source.len()
        );
    }
    #[test]
    fn overlapping_and_out_of_bounds_edits_are_rejected() {
        let edit = json!({"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"newText":"b"});
        assert!(edits("a", &json!([edit.clone(), edit])).is_err());
    }
}

/// Standard LSP snippet placeholders, mirrors, choices and literal variables.
/// Regex transforms are rejected instead of inserting an incorrect expansion.
#[derive(Default, Debug)]
pub struct Snippet {
    pub text: String,
    pub stops: Vec<(u32, usize, usize)>,
    pub end: Option<usize>,
}
pub fn snippet(
    source: &str,
    variables: &std::collections::BTreeMap<String, String>,
) -> Result<Snippet, String> {
    if source.len() > 1024 * 1024 {
        return Err("Snippet exceeds 1 MiB".into());
    }
    let mut out = Snippet::default();
    let mut at = 0;
    let mut values = variables.clone();
    snippet_part(source, &mut at, false, 0, &mut out, &mut values)?;
    out.stops.sort_by_key(|(index, start, _)| (*index, *start));
    Ok(out)
}
fn snippet_part(
    source: &str,
    at: &mut usize,
    closing: bool,
    depth: usize,
    out: &mut Snippet,
    values: &mut std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    if depth > 16 || out.stops.len() > 4096 {
        return Err("Snippet nesting/field budget exceeded".into());
    }
    while *at < source.len() {
        let character = source[*at..].chars().next().unwrap();
        *at += character.len_utf8();
        if closing && character == '}' {
            return Ok(());
        }
        if character == '\\' {
            let Some(next) = source[*at..].chars().next() else {
                out.text.push('\\');
                break;
            };
            if matches!(next, '\\' | '$' | '}' | ',' | '|') {
                *at += next.len_utf8();
                out.text.push(next);
            } else {
                out.text.push('\\');
            }
            continue;
        }
        if character != '$' {
            out.text.push(character);
            continue;
        }
        let braced = source[*at..].starts_with('{');
        if braced {
            *at += 1;
        }
        let start = *at;
        let numeric = source[*at..].starts_with(|character: char| character.is_ascii_digit());
        while source[*at..].chars().next().is_some_and(|character| {
            if numeric {
                character.is_ascii_digit()
            } else {
                character.is_ascii_alphanumeric() || character == '_'
            }
        }) {
            *at += source[*at..].chars().next().unwrap().len_utf8();
        }
        if start == *at {
            out.text.push('$');
            if braced {
                out.text.push('{');
            }
            continue;
        }
        let name = &source[start..*at];
        let index = name.parse::<u32>().ok();
        let field_start = out.text.len();
        if braced {
            match source[*at..].chars().next() {
                Some(':') => {
                    *at += 1;
                    snippet_part(source, at, true, depth + 1, out, values)?;
                }
                Some('|') => {
                    *at += 1;
                    let mut choice = String::new();
                    let mut first = true;
                    let mut escaped = false;
                    let mut closed = false;
                    while let Some(character) = source[*at..].chars().next() {
                        *at += character.len_utf8();
                        if escaped {
                            if first {
                                choice.push(character);
                            }
                            escaped = false;
                        } else if character == '\\' {
                            escaped = true;
                        } else if character == '|' && source[*at..].starts_with('}') {
                            *at += 1;
                            closed = true;
                            break;
                        } else if character == ',' {
                            first = false;
                        } else if first {
                            choice.push(character);
                        }
                    }
                    if !closed {
                        return Err("Unclosed snippet choice".into());
                    }
                    out.text.push_str(&choice);
                }
                Some('}') => {
                    *at += 1;
                    out.text
                        .push_str(values.get(name).map(String::as_str).unwrap_or_default());
                }
                Some('/') => {
                    return Err(
                        "Snippet regex transforms require an installed snippet extension".into(),
                    )
                }
                _ => return Err("Malformed snippet placeholder".into()),
            }
        } else {
            out.text
                .push_str(values.get(name).map(String::as_str).unwrap_or_default());
        }
        let field_end = out.text.len();
        if let Some(index) = index {
            if index == 0 {
                out.end = Some(field_start);
            } else {
                values
                    .entry(name.into())
                    .or_insert_with(|| out.text[field_start..field_end].into());
                out.stops.push((index, field_start, field_end));
            }
        }
    }
    if closing {
        Err("Unclosed snippet placeholder".into())
    } else {
        Ok(())
    }
}
#[cfg(test)]
mod snippet_fixtures {
    use super::*;
    #[test]
    fn snippet_mirrors_choices_unicode_and_final_stop_preserve_ranges() {
        let expanded = snippet("const ${1|π,value|} = ${2:42}; $1$0", &Default::default()).unwrap();
        assert_eq!(expanded.text, "const π = 42; π");
        assert_eq!(expanded.stops.len(), 3);
        assert_eq!(
            &expanded.text[expanded.stops[0].1..expanded.stops[0].2],
            "π"
        );
        assert_eq!(expanded.end, Some(expanded.text.len()));
        assert!(snippet("${1/regexp/replacement/}", &Default::default()).is_err());
    }
}

#[cfg(test)]
mod framework_fixtures {
    use super::*;
    #[test]
    fn framework_documents_select_installed_protocol_adapters_and_native_colors() {
        for (name, source, provider, id) in [
            (
                "component.tsx",
                include_str!("../tests/fixtures/web-development/component.tsx"),
                "typescript",
                "typescriptreact",
            ),
            (
                "component.vue",
                include_str!("../tests/fixtures/web-development/component.vue"),
                "vue",
                "vue",
            ),
            (
                "component.svelte",
                include_str!("../tests/fixtures/web-development/component.svelte"),
                "svelte",
                "svelte",
            ),
        ] {
            assert_eq!(language(std::path::Path::new(name)), Some((provider, id)));
            let spans = rustpy_highlight::highlight_for(
                if name.ends_with("tsx") { "tsx" } else { "html" },
                source,
            );
            assert!(!spans.is_empty());
            assert!(spans
                .iter()
                .all(|span| usize::from(span.range.end()) <= source.len()));
        }
        let package: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/web-development/package.json"
        ))
        .unwrap();
        assert_eq!(package["scripts"]["build"], "tsc --noEmit");
    }
    #[test]
    fn numeric_snippet_stop_does_not_consume_identifier_suffix() {
        let expanded = snippet("${1:π} $1suffix$0", &Default::default()).unwrap();
        assert_eq!(expanded.text, "π πsuffix");
        assert_eq!(expanded.stops.len(), 2);
    }
}
