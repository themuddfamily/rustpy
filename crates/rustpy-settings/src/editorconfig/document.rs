//! Lossless EditorConfig document context for resolution, inspections and lookup.
use std::{collections::HashSet, ops::Range};

#[derive(Clone, Debug)]
pub struct Property {
    pub key: String,
    pub value: String,
    pub key_range: Range<usize>,
    pub value_range: Range<usize>,
}
#[derive(Clone, Debug)]
pub struct Section {
    pub pattern: String,
    pub range: Range<usize>,
    pub properties: Vec<Property>,
}
#[derive(Clone, Debug)]
pub struct Issue {
    pub range: Range<usize>,
    pub message: String,
    pub error: bool,
    pub replacement: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct Document {
    pub root: bool,
    pub sections: Vec<Section>,
    pub issues: Vec<Issue>,
}
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub label: String,
    pub insert: String,
    pub detail: String,
}
pub struct Completion {
    pub range: Range<usize>,
    pub items: Vec<Suggestion>,
}

pub const STANDARD_KEYS: &[&str] = &[
    "indent_style",
    "indent_size",
    "tab_width",
    "end_of_line",
    "charset",
    "spelling_language",
    "trim_trailing_whitespace",
    "insert_final_newline",
    "max_line_length",
];
pub const IDE_KEYS: &[&str] = &[
    "ij_wrap_on_typing",
    "ij_visual_guides",
    "ij_continuation_indent_size",
    "ij_smart_tabs",
    "ij_formatter_enabled",
    "ij_formatter_tags_enabled",
    "ij_formatter_off_tag",
    "ij_formatter_on_tag",
    "ij_python_use_continuation_indent_for_parameters",
    "ij_python_use_continuation_indent_for_arguments",
    "ij_python_use_continuation_indent_for_collection_and_comprehensions",
    "ij_python_blank_lines_around_top_level_classes_functions",
    "ij_python_blank_lines_around_method",
    "ij_any_blank_lines_around_method",
    "ij_python_quote_style",
    "ij_python_magic_trailing_comma",
    "ij_python_keep_line_breaks",
    "ij_python_keep_simple_blocks_in_one_line",
    "ij_python_keep_first_column_comment",
];
fn python_values(key: &str) -> Option<&'static [&'static str]> {
    use std::collections::BTreeSet;
    use std::sync::LazyLock;
    static KEYS: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
        crate::python_style::supported_editorconfig_keys()
            .into_iter()
            .collect()
    });
    if !KEYS.contains(key) {
        return None;
    }
    let canonical = crate::python_style::EDITORCONFIG_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map_or(key, |(_, target)| *target);
    Some(if canonical == "ij_python_style_options" {
        &[]
    } else if canonical.starts_with("ij_python_wrap_") {
        &["inherit", "never", "if_long", "chop", "always", "unset"]
    } else if matches!(
        canonical,
        "ij_python_call_parameters_wrap"
            | "ij_python_method_parameters_wrap"
            | "ij_python_list_items_wrap"
            | "ij_python_dict_literals_wrap"
            | "ij_python_from_imports_wrap"
            | "ij_python_binary_operation_wrap"
    ) {
        &["0", "1", "2", "5", "unset"]
    } else if python_count(canonical) {
        &["0", "1", "2", "unset"]
    } else {
        &["true", "false", "unset"]
    })
}
fn python_count(key: &str) -> bool {
    key.starts_with("ij_python_keep_blank_lines_")
        || key.starts_with("ij_python_blank_lines_")
        || key == "ij_python_spaces_before_number_sign"
}
pub fn values(key: &str) -> &'static [&'static str] {
    match key {
        "root"
        | "trim_trailing_whitespace"
        | "insert_final_newline"
        | "ij_smart_tabs"
        | "ij_formatter_enabled"
        | "ij_formatter_tags_enabled"
        | "ij_wrap_on_typing"
        | "ij_python_use_continuation_indent_for_parameters"
        | "ij_python_use_continuation_indent_for_arguments"
        | "ij_python_use_continuation_indent_for_collection_and_comprehensions"
        | "ij_python_magic_trailing_comma"
        | "ij_python_keep_line_breaks"
        | "ij_python_keep_simple_blocks_in_one_line"
        | "ij_python_keep_first_column_comment" => &["true", "false", "unset"],
        "indent_style" => &["space", "tab", "unset"],
        "indent_size" => &["2", "4", "8", "tab", "unset"],
        "tab_width" | "ij_continuation_indent_size" => &["2", "4", "8", "unset"],
        "end_of_line" => &["lf", "crlf", "cr", "unset"],
        "charset" => &[
            "utf-8",
            "utf-8-bom",
            "latin1",
            "utf-16le",
            "utf-16be",
            "unset",
        ],
        "spelling_language" => &["en", "en-US", "en-GB", "unset"],
        "max_line_length" => &["79", "88", "100", "120", "off", "unset"],
        "ij_python_quote_style" => &["preserve", "single", "double", "unset"],
        _ => python_values(key).unwrap_or(&[]),
    }
}
pub(super) fn valid_value(key: &str, value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if lower == "unset" {
        return true;
    }
    if key == "ij_python_style_options" {
        return value.len() <= 65536
            && serde_json::from_str::<crate::python_style::PythonStyle>(value).is_ok();
    }
    if python_values(key).is_some() && python_count(key) {
        return value.parse::<usize>().is_ok_and(|number| number <= 100);
    }
    match key {
        "indent_size" => lower == "tab" || positive(value),
        "tab_width" | "ij_continuation_indent_size" => positive(value),
        "max_line_length" => lower == "off" || positive(value),
        "spelling_language" => spelling_language(value).is_some(),
        "ij_visual_guides" => {
            value.is_empty()
                || value
                    .split(',')
                    .all(|value| value.trim().parse::<u32>().is_ok())
        }
        "ij_python_blank_lines_around_top_level_classes_functions"
        | "ij_python_blank_lines_around_method"
        | "ij_any_blank_lines_around_method" => value.parse::<usize>().is_ok(),
        _ => {
            values(key).is_empty()
                || values(key)
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(value))
        }
    }
}
fn positive(value: &str) -> bool {
    value.parse::<usize>().is_ok_and(|number| number > 0)
}
pub fn spelling_language(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    if !(bytes.len() == 2 || (bytes.len() == 5 && bytes[2] == b'-')) {
        return None;
    }
    if !bytes[..2].iter().all(u8::is_ascii_alphabetic)
        || (bytes.len() == 5 && !bytes[3..].iter().all(u8::is_ascii_alphabetic))
    {
        return None;
    }
    Some(if bytes.len() == 2 {
        value.to_ascii_lowercase()
    } else {
        format!(
            "{}-{}",
            value[..2].to_ascii_lowercase(),
            value[3..].to_ascii_uppercase()
        )
    })
}

pub fn parse(source: &str) -> Document {
    let mut out = Document::default();
    if source.len() > 8 * 1024 * 1024 {
        out.issues.push(Issue {
            range: 0..0,
            message: "EditorConfig exceeds the 8 MiB document limit".into(),
            error: true,
            replacement: None,
        });
        return out;
    }
    let mut offset = 0;
    let mut keys = HashSet::new();
    for raw in source.split_inclusive('\n') {
        let raw_content = raw.trim_end_matches(['\r', '\n']);
        let content = if offset == 0 {
            raw_content.trim_start_matches('\u{feff}')
        } else {
            raw_content
        };
        let line = content.trim();
        let start =
            offset + raw_content.len() - content.len() + content.len() - content.trim_start().len();
        let range = start..start + line.len();
        offset += raw.len();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let pattern = &line[1..line.len() - 1];
            if super::glob::Glob::compile(pattern).is_none() {
                out.issues.push(Issue {
                    range: range.clone(),
                    message: "Section pattern exceeds the supported matching limits".into(),
                    error: true,
                    replacement: None,
                });
            }
            out.sections.push(Section {
                pattern: pattern.into(),
                range,
                properties: Vec::new(),
            });
            keys.clear();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            out.issues.push(Issue {
                range,
                message:
                    "Expected [section pattern] or key = value (inline comments are not supported)"
                        .into(),
                error: true,
                replacement: None,
            });
            continue;
        };
        let key_text = key.trim();
        let key_range = start..start + key.trim_end().len();
        let value_start = start + key.len() + 1 + value.len() - value.trim_start().len();
        let value_range = value_start..value_start + value.trim().len();
        let key = key_text.to_lowercase();
        let value = value.trim().to_string();
        if key.is_empty() {
            out.issues.push(Issue {
                range: key_range,
                message: "Property key cannot be empty".into(),
                error: true,
                replacement: None,
            });
            continue;
        }
        if !valid_value(&key, &value) {
            let replacement = values(&key)
                .iter()
                .find(|candidate| {
                    candidate.eq_ignore_ascii_case(value.trim_end_matches(['#', ';']).trim())
                })
                .map(|value| value.to_string());
            out.issues.push(Issue {
                range: value_range.clone(),
                message: format!("Unsupported {key} value '{value}'"),
                error: false,
                replacement,
            });
        }
        if !keys.insert(key.clone()) {
            out.issues.push(Issue {
                range: key_range.clone(),
                message: "This property overrides an earlier value in the same section".into(),
                error: false,
                replacement: None,
            });
        }
        if let Some(section) = out.sections.last_mut() {
            if key == "root" {
                out.issues.push(Issue {
                    range: key_range.clone(),
                    message: "root only takes effect before the first section".into(),
                    error: false,
                    replacement: None,
                });
            }
            section.properties.push(Property {
                key,
                value,
                key_range,
                value_range,
            });
        } else if key == "root" {
            out.root = value.eq_ignore_ascii_case("true");
        } else {
            out.issues.push(Issue {
                range: key_range,
                message: "A property outside a section has no effect".into(),
                error: false,
                replacement: None,
            });
        }
    }
    out
}

pub fn complete(source: &str, offset: usize) -> Option<Completion> {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    let line_start = source[..offset].rfind('\n').map_or(0, |at| at + 1);
    let before = &source[line_start..offset];
    if before.trim_start().starts_with(['#', ';']) {
        return None;
    }
    let line_end = source[offset..]
        .find('\n')
        .map_or(source.len(), |at| offset + at);
    let (start, candidates, value) = if let Some(equal) = before.find('=') {
        let key = before[..equal].trim().to_lowercase();
        let start = line_start + equal + 1 + before[equal + 1..].len()
            - before[equal + 1..].trim_start().len();
        (
            start,
            values(&key)
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            true,
        )
    } else if before.trim_start().starts_with('[') {
        return None;
    } else {
        let in_section = source[..line_start].lines().any(|line| {
            let line = line.trim();
            line.starts_with('[') && line.ends_with(']')
        });
        let mut keys = if in_section {
            STANDARD_KEYS
                .iter()
                .chain(IDE_KEYS)
                .map(|key| (*key).to_string())
                .collect::<Vec<_>>()
        } else {
            vec!["root".to_string()]
        };
        if in_section {
            keys.extend(crate::python_style::supported_editorconfig_keys());
        }
        keys.sort_unstable();
        keys.dedup();
        (
            line_start + before.len() - before.trim_start().len(),
            keys,
            false,
        )
    };
    let prefix = source[start..offset].to_ascii_lowercase();
    let end = if value {
        line_end - source[offset..line_end].len()
            + source[offset..line_end].trim_end_matches('\r').len()
    } else {
        offset
            + source[offset..line_end]
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .map(char::len_utf8)
                .sum::<usize>()
    };
    Some(Completion {
        range: start..end,
        items: candidates
            .into_iter()
            .filter(|candidate| candidate.to_ascii_lowercase().starts_with(&prefix))
            .map(|candidate| Suggestion {
                label: candidate.clone(),
                insert: candidate,
                detail: if value {
                    "EditorConfig value"
                } else {
                    "EditorConfig property"
                }
                .into(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_preserves_unknown_values_and_offsets_and_last_root_pair() {
        let source = "\u{feff}ROOT = TRUE\r\nroot = false\r\n[*.{py,pyi}]\r\ncustom = Value # literal ; text\r\nindent_style = TAB\r\nindent_size = 3\r\nindent_size = 4\r\n[*.py]\r\ncharset =\r\n";
        let parsed = parse(source);
        assert!(!parsed.root);
        assert_eq!(
            parsed.sections[0].properties[0].value,
            "Value # literal ; text"
        );
        for section in &parsed.sections {
            for property in &section.properties {
                assert_eq!(
                    source[property.key_range.clone()].to_lowercase(),
                    property.key
                );
                assert_eq!(&source[property.value_range.clone()], property.value);
            }
        }
        assert_eq!(parsed.sections.len(), 2);
        assert!(
            parsed.issues.iter().all(|issue| !issue.error),
            "{:?}",
            parsed.issues
        );
        assert!(parsed
            .issues
            .iter()
            .any(|issue| issue.message.contains("overrides")));
        assert!(parsed
            .issues
            .iter()
            .any(|issue| issue.message.contains("Unsupported charset")));
    }
    #[test]
    fn completion_is_contextual_and_uses_whole_property_or_value_ranges() {
        let source = "root = t";
        let completion = complete(source, source.len()).unwrap();
        assert_eq!(&source[completion.range], "t");
        assert_eq!(completion.items[0].label, "true");
        let source = "[*.py]\nindent_siz = 4\n";
        let offset = source.find(" =").unwrap();
        let completion = complete(source, offset).unwrap();
        assert_eq!(&source[completion.range], "indent_siz");
        assert!(completion
            .items
            .iter()
            .any(|item| item.label == "indent_size"));
        let source = "[*.py]\ncharset = UTF-8-bom";
        let offset = source.find("-bom").unwrap();
        let completion = complete(source, offset).unwrap();
        assert_eq!(&source[completion.range], "UTF-8-bom");
        assert!(completion
            .items
            .iter()
            .any(|item| item.label == "utf-8-bom"));
        assert!(complete("# charset = u", 13).is_none());
    }
    #[test]
    fn language_style_metadata_drives_completion_and_value_diagnostics() {
        let source = "[*.py]\nij_python_space_around_assignment";
        let completion = complete(source, source.len()).unwrap();
        assert!(completion
            .items
            .iter()
            .any(|item| { item.label == "ij_python_space_around_assignment_operators" }));
        let source = "[*.py]\nij_python_wrap_calls = ch";
        assert_eq!(
            complete(source, source.len()).unwrap().items[0].insert,
            "chop"
        );
        let source = "[*.py]\nij_python_wrap_calls = chop\nij_python_space_assignment = FALSE\nij_python_method_parameters_wrap = 5\nij_python_blank_lines_before_first_method = 3\nij_python_style_options = {}\n";
        assert!(parse(source).issues.is_empty());
        let source = "[*.py]\nij_python_wrap_calls = invalid\nij_python_space_assignment = maybe\nij_python_method_parameters_wrap = 3\nij_python_blank_lines_before_first_method = -1\nij_python_style_options = {broken}\n";
        let parsed = parse(source);
        assert_eq!(parsed.issues.len(), 5);
        for issue in &parsed.issues {
            assert!(!issue.error);
            assert!(!source[issue.range.clone()].is_empty());
        }
        assert!(parse("[*.py]\nij_future_property = arbitrary")
            .issues
            .is_empty());
    }
}
