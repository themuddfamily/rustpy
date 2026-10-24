//! Format an immutable, indentation-normalized Python paste candidate.
//! Only pasted bytes may change; incomplete/unsafe fragments keep indentation.
use rustpy_core::{TextRange, TextSize};
use rustpy_format::FormatOptions;
use rustpy_text::Edit;

const MAX_SOURCE: usize = 1024 * 1024;
const MAX_REPLACEMENTS: usize = 64;

/// Called once after indentation normalization and collision handling, before
/// caret mapping and the existing single Paste application. The caller guards
/// Python/large-file/string-literal/column modes and supplies its code style.
/// No live document, selections, history or filesystem is touched.
pub(super) fn reformat_replacements(
    source: &str,
    mut replacements: Vec<Edit>,
    options: &FormatOptions,
) -> Vec<Edit> {
    if source.len() > MAX_SOURCE || replacements.len() > MAX_REPLACEMENTS {
        return replacements;
    }
    replacements.sort_by_key(|edit| (edit.range.start(), edit.range.end()));
    if replacements.iter().any(|edit| {
        source
            .get(usize::from(edit.range.start())..usize::from(edit.range.end()))
            .is_none()
    }) || replacements
        .windows(2)
        .any(|pair| pair[1].range.start() < pair[0].range.end())
    {
        return replacements;
    }
    let mut candidate = source.to_owned();
    for edit in replacements.iter().rev() {
        candidate.replace_range(
            usize::from(edit.range.start())..usize::from(edit.range.end()),
            &edit.text,
        );
    }
    if candidate.len() > MAX_SOURCE {
        return replacements;
    }
    let Some(fingerprint) = rustpy_format::semantic_fingerprint(&candidate) else {
        return replacements;
    };
    let mut shifted = 0i64;
    let mut formatted = replacements.clone();
    for (original, result) in replacements.iter().zip(&mut formatted) {
        let start = (i64::from(u32::from(original.range.start())) + shifted) as usize;
        let end = start + original.text.len();
        shifted += original.text.len() as i64 - usize::from(original.range.len()) as i64;
        if start == end {
            continue;
        }
        let range = TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32));
        let Ok(changes) = rustpy_format::format_range(&candidate, range, options) else {
            return replacements;
        };
        let mut local = Vec::new();
        for (range, text) in changes {
            let old = &candidate[usize::from(range.start())..usize::from(range.end())];
            let (prefix, suffix) = unchanged_edges(old, &text);
            let from = usize::from(range.start()) + prefix;
            let to = usize::from(range.end()) - suffix;
            if from == to && prefix + suffix == text.len() {
                continue;
            }
            // A formatter line hunk may include unchanged surrounding source.
            // Trim it, then require its actual change to fit inside this paste.
            if from < start || to > end {
                return replacements;
            }
            local.push((
                from - start,
                to - start,
                text[prefix..text.len() - suffix].to_owned(),
            ));
        }
        for (from, to, text) in local.into_iter().rev() {
            result.text.replace_range(from..to, &text);
        }
    }
    let mut actual = source.to_owned();
    for edit in formatted.iter().rev() {
        actual.replace_range(
            usize::from(edit.range.start())..usize::from(edit.range.end()),
            &edit.text,
        );
    }
    if rustpy_format::semantic_fingerprint(&actual).as_ref() == Some(&fingerprint) {
        formatted
    } else {
        replacements
    }
}

fn unchanged_edges(old: &str, new: &str) -> (usize, usize) {
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = old[prefix..]
        .bytes()
        .rev()
        .zip(new[prefix..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    (prefix, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_text::{Document, EditGroup};
    fn paste(source: &str, replacements: Vec<Edit>) -> Document {
        let mut doc = Document::new(source);
        let edits = reformat_replacements(source, replacements, &FormatOptions::pep8());
        doc.edit(edits, &[], &[], EditGroup::Separate, Some("Paste"))
            .unwrap();
        doc
    }
    fn range(start: usize, end: usize) -> TextRange {
        TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
    }
    #[test]
    fn partial_unicode_expression_formats_without_touching_prefix_or_suffix() {
        let source = "result = old  # π🙂\n";
        let mut doc = paste(source, vec![Edit::replace(range(9, 12), "1+2")]);
        assert_eq!(doc.text(), "result = 1 + 2  # π🙂\n");
        assert_eq!(doc.undo_label().as_deref(), Some("Paste"));
        doc.undo().unwrap();
        assert_eq!(doc.text(), source);
        doc.redo().unwrap();
        assert_eq!(doc.text(), "result = 1 + 2  # π🙂\n");
    }
    #[test]
    fn indented_suite_uses_full_python_context() {
        let source = "def f():\n    pass\n";
        let doc = paste(
            source,
            vec![Edit::replace(range(9, 18), "    x=1+2\n    return x\n")],
        );
        assert_eq!(doc.text(), "def f():\n    x = 1 + 2\n    return x\n");
    }
    #[test]
    fn incomplete_chunk_and_unrelated_formatting_keep_prepared_bytes() {
        let source = "x = 1\n";
        let edits = vec![Edit::insert(TextSize::new(6), "if (")];
        assert_eq!(
            reformat_replacements(source, edits.clone(), &FormatOptions::pep8()),
            edits
        );
        let source = "untidy=0\nvalue = old\n";
        let doc = paste(source, vec![Edit::replace(range(17, 20), "1+2")]);
        assert_eq!(doc.text(), "untidy=0\nvalue = 1 + 2\n");
    }
    #[test]
    fn multiple_replacements_apply_once_and_preserve_literal_content() {
        let source = "a = old\nb = old\ntext = '''π\n    literal  spaces\n'''\n";
        let doc = paste(
            source,
            vec![
                Edit::replace(range(4, 7), "1+2"),
                Edit::replace(range(12, 15), "3+4"),
            ],
        );
        assert_eq!(
            doc.text(),
            "a = 1 + 2\nb = 3 + 4\ntext = '''π\n    literal  spaces\n'''\n"
        );
    }
    #[test]
    fn readonly_and_invalid_ranges_never_publish_edits() {
        let source = "x = 0\n";
        let edits = vec![Edit::replace(range(4, 5), "1+2")];
        let prepared = reformat_replacements(source, edits, &FormatOptions::pep8());
        let mut doc = Document::new(source);
        doc.set_read_only(true);
        assert!(doc
            .edit(prepared, &[], &[], EditGroup::Separate, Some("Paste"))
            .is_err());
        assert_eq!(doc.text(), source);
        assert!(!doc.can_undo());
        let invalid = vec![Edit::insert(TextSize::new(100), "x")];
        assert_eq!(
            reformat_replacements(source, invalid.clone(), &FormatOptions::pep8()),
            invalid
        );
    }
}
