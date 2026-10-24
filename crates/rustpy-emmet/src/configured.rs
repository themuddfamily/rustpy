//! Additive configured abbreviations and explicit output filters.
use crate::{abbreviation_before, css_abbreviation_before, expand_at, is_css_context, Expansion};
use std::{collections::BTreeMap, ops::Range};

const MAX_OUTPUT: usize = 1024 * 1024;

/// Whether this source position uses stylesheet abbreviations, including HTML
/// style blocks and attributes. Invalid byte positions never select a context.
pub fn stylesheet_context(text: &str, offset: usize, language: &str) -> bool {
    offset <= text.len()
        && text.is_char_boundary(offset)
        && (is_css_context(language)
            || matches!(
                crate::extract::raw_text_element(text, offset),
                Some(("style", _))
            )
            || crate::extract::style_attribute_start(text, offset).is_some())
}

fn split_filters(abbreviation: &str) -> Option<(&str, Vec<&str>)> {
    let mut depth = 0;
    let mut split = None;
    for (at, ch) in abbreviation.char_indices() {
        match ch {
            '[' | '{' | '(' => depth += 1,
            ']' | '}' | ')' => depth -= 1,
            '|' if depth == 0 => {
                split = Some(at);
                break;
            }
            _ => {}
        }
    }
    let Some(at) = split else {
        return Some((abbreviation, Vec::new()));
    };
    let filters: Vec<_> = abbreviation[at + 1..].split('|').collect();
    if filters
        .iter()
        .any(|filter| !matches!(*filter, "c" | "e" | "s"))
    {
        return None;
    }
    Some((&abbreviation[..at], filters))
}

fn transform(mut expansion: Expansion, filter: &str) -> Expansion {
    if filter == "c" {
        let prefix = "<!-- Emmet -->\n";
        expansion.text = format!("{prefix}{}\n<!-- /Emmet -->", expansion.text);
        expansion.caret += prefix.len();
        for stop in &mut expansion.stops {
            stop.start += prefix.len();
            stop.end += prefix.len();
        }
        return expansion;
    }
    let mut output = String::new();
    let mut positions = vec![0; expansion.text.len() + 1];
    let mut line_start = false;
    for (at, ch) in expansion.text.char_indices() {
        positions[at] = output.len();
        match filter {
            "e" => match ch {
                '&' => output.push_str("&amp;"),
                '<' => output.push_str("&lt;"),
                '>' => output.push_str("&gt;"),
                '"' => output.push_str("&quot;"),
                _ => output.push(ch),
            },
            "s" => {
                if ch == '\n' {
                    line_start = true;
                } else if !(line_start && matches!(ch, ' ' | '\t')) {
                    output.push(ch);
                    line_start = false;
                }
            }
            _ => output.push(ch),
        }
        positions[at + ch.len_utf8()] = output.len();
    }
    expansion.caret = positions[expansion.caret];
    for stop in &mut expansion.stops {
        stop.start = positions[stop.start];
        stop.end = positions[stop.end];
    }
    expansion.text = output;
    expansion
}

/// Explicit preview/surround abbreviation. Custom values are abbreviations,
/// not executable templates; cycles are refused. Existing context checks apply.
pub fn configured_at(
    text: &str,
    range: Range<usize>,
    abbreviation: &str,
    language: &str,
    indent: &str,
    snippets: &BTreeMap<String, String>,
    surround: Option<&str>,
) -> Option<Expansion> {
    if range.end > text.len()
        || range.start > range.end
        || !text.is_char_boundary(range.start)
        || !text.is_char_boundary(range.end)
        || abbreviation.len() > 8192
    {
        return None;
    }
    let (mut base, filters) = split_filters(abbreviation)?;
    let mut seen = Vec::new();
    while let Some(value) = snippets.get(base) {
        if seen.contains(&base) || seen.len() >= 32 {
            return None;
        }
        seen.push(base);
        base = value;
    }
    if base.len() > 8192 {
        return None;
    }
    let mut candidate = text[..range.start].to_owned();
    candidate.push_str(base);
    let offset = candidate.len();
    candidate.push_str(&text[range.end..]);
    let (actual, mut expansion) = expand_at(&candidate, offset, language, indent)?;
    if actual.start != range.start || actual.end != offset {
        return None;
    }
    if let Some(selected) = surround {
        if crate::is_css_context(language)
            || crate::extract::raw_text_element(text, range.start).is_some()
            || crate::extract::style_attribute_start(text, range.start).is_some()
        {
            return None;
        }
        let parent = crate::extract::enclosing_element(text, range.start);
        expansion = crate::html::surround_html(
            base,
            selected,
            indent,
            parent.as_deref(),
            matches!(
                language,
                "jsx" | "tsx" | "javascriptreact" | "typescriptreact"
            ),
        )?;
        let line_start = text[..range.start].rfind('\n').map_or(0, |at| at + 1);
        let before = &text[line_start..range.start];
        let base_indent = &before[..before.len() - before.trim_start_matches([' ', '\t']).len()];
        expansion = expansion.with_base_indent(base_indent);
    }
    for filter in filters {
        if filter == "c" && stylesheet_context(text, range.start, language) {
            return None;
        }
        expansion = transform(expansion, filter);
        if expansion.text.len() > MAX_OUTPUT {
            return None;
        }
    }
    (expansion.text.len() <= MAX_OUTPUT).then_some(expansion)
}

/// Tab keeps old extraction and context semantics, with suffix filters/custom
/// aliases added. No source is changed until a complete expansion is returned.
pub fn configured_before(
    text: &str,
    offset: usize,
    language: &str,
    indent: &str,
    snippets: &BTreeMap<String, String>,
) -> Option<(Range<usize>, Expansion)> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let line_start = text[..offset].rfind('\n').map_or(0, |at| at + 1);
    let tail = &text[line_start..offset];
    let filter_start = tail.rfind('|').map(|at| line_start + at);
    let mut end = offset;
    if filter_start.is_some() {
        while let Some(at) = text[line_start..end].rfind('|').map(|at| line_start + at) {
            if text[at + 1..end].chars().all(|ch| ch.is_ascii_alphabetic()) {
                end = at;
            } else {
                break;
            }
        }
    }
    let extracted = if is_css_context(language) {
        css_abbreviation_before(text, end)
    } else {
        abbreviation_before(text, end)
    }?;
    let range = extracted.0..if end == offset {
        extracted.0 + extracted.1.len()
    } else {
        offset
    };
    let expansion = configured_at(
        text,
        range.clone(),
        &text[range.clone()],
        language,
        indent,
        snippets,
        None,
    )?;
    Some((range, expansion))
}

#[cfg(test)]
mod jsx_fixtures {
    #[test]
    fn react_expansion_and_surround_use_react_attributes_and_void_tags() {
        let expansion =
            crate::html::expand_jsx_in("label.field>input[autofocus]", "  ", None).unwrap();
        assert!(expansion.text.contains("className=\"field\""));
        assert!(expansion.text.contains("autoFocus"));
        assert!(expansion.text.contains(" />"));
        let wrapped = crate::html::surround_html("div.card", "λ", "  ", None, true).unwrap();
        assert!(wrapped.text.contains("className=\"card\""));
        assert!(wrapped.text.contains('λ'));
    }
}
