//! Shared code-style policy for UI, command line and LSP formatters.
use crate::{FormatError, FormatOptions};
use rustpy_settings::{editorconfig::EditorConfigProps, CodeStyleSettings};
use rustpy_syntax::ast::{self, AstNode};

pub fn apply(options: &mut FormatOptions, style: &CodeStyleSettings, ec: &EditorConfigProps) {
    options.python = style.python.clone();
    if let Some(value) = ec.ide.python.get("ij_python_style_options") {
        options
            .python
            .import_property("ij_python_style_options", value);
    }
    for (key, value) in &ec.ide.python {
        if key != "ij_python_style_options" {
            options.python.import_property(key, value);
        }
    }
    let indents = &options.python.indents;
    if options.python.indents != rustpy_settings::python_style::Indents::default() {
        options.continuation_indent = Some(crate::ContinuationIndent {
            width: indents.continuation,
            parameters: indents.parameters,
            arguments: indents.arguments,
            collections: indents.collections,
        });
    }
    if let Some(tabs) = &mut options.tabs {
        tabs.smart = indents.smart_tabs;
    }
    options.keep_first_column_comments = options.python.first_column_comments;
    options.keep_line_breaks = ec.ide.keep_line_breaks.unwrap_or(style.keep_line_breaks);
    options.keep_simple_blocks = ec
        .ide
        .keep_simple_blocks
        .unwrap_or(style.keep_simple_blocks);
    options.keep_first_column_comments = ec
        .ide
        .keep_first_column_comments
        .unwrap_or(style.keep_first_column_comments)
        || options.python.first_column_comments;
    if ec
        .ide
        .formatter_tags_enabled
        .unwrap_or(style.formatter_markers)
    {
        options.formatter_tags = Some(crate::FormatterTags {
            off: ec
                .ide
                .formatter_off_tag
                .clone()
                .unwrap_or_else(|| style.formatter_off.clone()),
            on: ec
                .ide
                .formatter_on_tag
                .clone()
                .unwrap_or_else(|| style.formatter_on.clone()),
        })
    } else {
        options.formatter_tags = None
    }
    if let Some(value) = ec.ide.quote_style.as_deref() {
        options.quote = match value {
            "single" => crate::QuoteStyle::Single,
            "double" => crate::QuoteStyle::Double,
            _ => crate::QuoteStyle::Preserve,
        }
    }
    if let Some(value) = ec.ide.magic_trailing_comma {
        options.magic_trailing_comma = value
    }
    if let Some(value) = ec.ide.blank_lines_top_level {
        options.blank_lines_top_level = value
    }
    if let Some(value) = ec.ide.blank_lines_in_class {
        options.blank_lines_in_class = value
    }
}
/// Keep preferences veto whole logical diff hunks, never half a split/collapse.
/// The resulting module is still checked for semantic equivalence.
pub(crate) fn keep(
    source: &str,
    formatted: String,
    options: &FormatOptions,
) -> Result<String, FormatError> {
    if !options.keep_line_breaks
        && !options.keep_simple_blocks
        && !options.keep_first_column_comments
    {
        return Ok(formatted);
    }
    let old = source.split_inclusive('\n').collect::<Vec<_>>();
    let mut protected = vec![false; old.len()];
    let mut breaks = vec![false; old.len()];
    if options.keep_first_column_comments {
        for (index, line) in old.iter().enumerate() {
            if line.starts_with('#') {
                protected[index] = true
            }
        }
    }
    let root = rustpy_syntax::parse(source).syntax_node();
    let starts = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(index, _)| index + 1))
        .collect::<Vec<_>>();
    let mut covered_expression_end = 0;
    for (index, node) in root.descendants().enumerate() {
        if index >= 100_000 {
            return Ok(source.into());
        }
        if node.kind() != rustpy_syntax::SyntaxKind::BLOCK
            && ast::Expr::cast(node.clone()).is_none()
        {
            continue;
        }
        let start = usize::from(node.text_range().start());
        let end = usize::from(node.text_range().end());
        if node.kind() != rustpy_syntax::SyntaxKind::BLOCK && end <= covered_expression_end {
            continue;
        }
        let text = &source[start..end];
        let simple = options.keep_simple_blocks
            && node.kind() == rustpy_syntax::SyntaxKind::BLOCK
            && !text.trim_end_matches(['\r', '\n']).contains('\n');
        let multiline = options.keep_line_breaks
            && ast::Expr::cast(node.clone()).is_some()
            && text.contains('\n');
        if simple || multiline {
            let start = usize::from(node.text_range().start());
            let end = usize::from(node.text_range().end());
            let first = starts
                .partition_point(|offset| *offset <= start)
                .saturating_sub(1);
            let last = starts
                .partition_point(|offset| *offset < end)
                .saturating_sub(1);
            if simple {
                for item in protected.iter_mut().take(last + 1).skip(first) {
                    *item = true
                }
            }
            if multiline {
                covered_expression_end = end;
                for item in breaks.iter_mut().take(last + 1).skip(first) {
                    *item = true
                }
            }
        }
    }
    filter_hunks(source, &formatted, |first, last, replacement| {
        !(first..=last).any(|line| protected.get(line).copied().unwrap_or(false))
            && (!(first..=last).any(|line| breaks.get(line).copied().unwrap_or(false))
                || replacement.split_inclusive('\n').count() == last - first + 1)
    })
}

fn filter_hunks(
    source: &str,
    formatted: &str,
    mut selected: impl FnMut(usize, usize, &str) -> bool,
) -> Result<String, FormatError> {
    let starts = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(index, _)| index + 1))
        .collect::<Vec<_>>();
    let whole = rustpy_core::TextRange::new(0.into(), rustpy_core::TextSize::of(source));
    let edits = crate::edits_touching(source, formatted, whole)?;
    let mut out = source.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        let start = usize::from(range.start());
        let end = usize::from(range.end());
        let first = starts
            .partition_point(|offset| *offset <= start)
            .saturating_sub(1);
        let last = starts
            .partition_point(|offset| *offset < end)
            .saturating_sub(1)
            .max(first);
        if selected(first, last, &replacement) {
            out.replace_range(start..end, &replacement)
        }
    }
    if crate::semantic_fingerprint(source) != crate::semantic_fingerprint(&out) {
        return Err(FormatError::Unsafe(
            "formatting preferences would split a logical change".into(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keep_existing_breaks_and_single_line_suites() {
        let mut options = FormatOptions::black();
        options.keep_line_breaks = true;
        options.keep_simple_blocks = true;
        let source = "values = [\n    1,\n    2\n]\nif True: print(1)\nother=2\n";
        let result = crate::format_source(source, &options).unwrap();
        assert!(result.contains("values = [\n"));
        assert!(result.contains("if True: print(1)"));
        assert!(result.contains("other = 2"));
        assert_eq!(
            crate::semantic_fingerprint(source),
            crate::semantic_fingerprint(&result)
        )
    }
    #[test]
    fn marker_and_imported_quote_options_have_actual_consumers() {
        let mut options = FormatOptions::pep8();
        let style = CodeStyleSettings {
            formatter_markers: true,
            ..Default::default()
        };
        let mut ec = EditorConfigProps::default();
        ec.ide.quote_style = Some("single".into());
        apply(&mut options, &style, &ec);
        let source = "# @formatter:off\nx=  1\n# @formatter:on\ny = \"text\"\n";
        let out = crate::format_source(source, &options).unwrap();
        assert!(out.contains("x=  1"));
        assert!(out.contains("'text'"))
    }
}

/// Retain only complete formatting hunks contained in changed source lines.
/// Each span is `(zero_based_start_line, line_count)` in `source`, never in
/// `formatted`. This never reformats an unchanged neighbor to complete a split.
pub fn changed_lines(
    source: &str,
    formatted: &str,
    lines: &[(usize, usize)],
) -> Result<String, FormatError> {
    filter_hunks(source, formatted, |first, last, _| {
        (first..=last).all(|line| {
            lines
                .iter()
                .any(|(start, count)| line >= *start && line < start.saturating_add(*count))
        })
    })
}

#[cfg(test)]
mod changed_tests {
    use super::*;
    #[test]
    fn changed_text_does_not_rewrite_unchanged_neighbors() {
        let source = "a=1\nb=2\n";
        let formatted = "a = 1\nb = 2\n";
        assert_eq!(
            changed_lines(source, formatted, &[(1, 1)]).unwrap(),
            "a=1\nb = 2\n"
        );
        assert_eq!(changed_lines(source, formatted, &[]).unwrap(), source)
    }
}
