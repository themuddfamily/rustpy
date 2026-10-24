//! Existing YAML scanner identifies *actual* collection/mapping punctuation;
//! scalar strings, block scalars, aliases, tags and comments are never emitted
//! from reconstructed values. The shared parser verifies the complete graph.
use super::Error;
use rustpy_core::{TextRange, TextSize};
use yaml_rust2::scanner::{Scanner, TokenType};
fn same(left: &[rustpy_syntax::yaml::Document], right: &[rustpy_syntax::yaml::Document]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.root == right.root
                && left.nodes.len() == right.nodes.len()
                && left
                    .nodes
                    .iter()
                    .zip(&right.nodes)
                    .all(|(left, right)| left.kind == right.kind && left.tag == right.tag)
        })
}
fn horizontal_before(source: &str, at: usize) -> usize {
    let mut start = at;
    while start > 0 && matches!(source.as_bytes()[start - 1], b' ' | b'\t') {
        start -= 1;
    }
    start
}
fn horizontal_after(source: &str, at: usize) -> usize {
    let mut end = at;
    while matches!(source.as_bytes().get(end), Some(b' ' | b'\t')) {
        end += 1;
    }
    end
}
pub(super) fn format(source: &str) -> Result<String, Error> {
    let before =
        rustpy_syntax::yaml::parse(source).map_err(|error| Error::Syntax(error.message))?;
    // yaml-rust markers count Unicode scalar positions, despite the upstream
    // index documentation. Resolve through an explicit character→byte table.
    let positions: Vec<_> = source
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(source.len()))
        .collect();
    let mut scanner = Scanner::new(source.chars());
    let mut edits = Vec::new();
    let mut count = 0usize;
    for token in scanner.by_ref() {
        count += 1;
        if count > super::MAX_TOKENS {
            return Err(Error::Budget("YAML tokens"));
        }
        let (expected, after, before) = match token.1 {
            TokenType::Value => (b':', Some(" "), true),
            TokenType::FlowEntry => (b',', Some(" "), true),
            TokenType::FlowSequenceStart => (b'[', Some(""), false),
            TokenType::FlowMappingStart => (b'{', Some(""), false),
            TokenType::FlowSequenceEnd => (b']', None, true),
            TokenType::FlowMappingEnd => (b'}', None, true),
            TokenType::BlockEntry => (b'-', Some(" "), false),
            _ => continue,
        };
        let at = *positions
            .get(token.0.index())
            .ok_or_else(|| Error::Unsafe("YAML marker has no source position".into()))?;
        if source.as_bytes().get(at) != Some(&expected) {
            return Err(Error::Unsafe(
                "YAML marker does not identify its punctuation".into(),
            ));
        }
        if before {
            let start = horizontal_before(source, at);
            // Preserve physical indentation when a closing delimiter is alone
            // on its line; this formatter only touches inline punctuation gaps.
            if start < at && start > 0 && !matches!(source.as_bytes()[start - 1], b'\n' | b'\r') {
                edits.push((
                    TextRange::new(TextSize::new(start as u32), TextSize::new(at as u32)),
                    String::new(),
                ));
            }
        }
        if let Some(gap) = after {
            let start = at + 1;
            let end = horizontal_after(source, start);
            let next = source.as_bytes().get(end).copied();
            if !matches!(next, None | Some(b'\n' | b'\r')) {
                let gap = if matches!(next, Some(b']' | b'}')) {
                    ""
                } else {
                    gap
                };
                if &source[start..end] != gap {
                    edits.push((
                        TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
                        gap.to_owned(),
                    ));
                }
            }
        }
    }
    if let Some(error) = scanner.get_error() {
        return Err(Error::Syntax(error.to_string()));
    }
    edits.sort_by_key(|(range, _)| (range.start(), range.end()));
    edits.dedup();
    if edits
        .windows(2)
        .any(|pair| pair[0].0.end() > pair[1].0.start())
    {
        return Err(Error::Unsafe("Overlapping YAML trivia edits".into()));
    }
    let output = rustpy_semantic::edit_helpers::try_apply_edits(source, &edits)
        .map_err(|error| Error::Unsafe(error.to_string()))?;
    let after =
        rustpy_syntax::yaml::parse(&output).map_err(|error| Error::Unsafe(error.message))?;
    if !same(&before, &after) {
        return Err(Error::Unsafe(
            "YAML scalar/tag/alias graph would change".into(),
        ));
    }
    Ok(output)
}
