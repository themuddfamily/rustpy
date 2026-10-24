//! Python code-style options, shared by schemes, EditorConfig and the formatter.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Wrap {
    pub mode: String,
    pub align: bool,
    pub newline_after_open: bool,
    pub closing_on_newline: bool,
}
impl Default for Wrap {
    fn default() -> Self {
        Self {
            mode: "inherit".into(),
            align: false,
            newline_after_open: true,
            closing_on_newline: true,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Indents {
    pub smart_tabs: bool,
    pub continuation: usize,
    pub keep_empty: bool,
    pub in_parentheses: bool,
    pub hanging_closing: bool,
    pub parameters: bool,
    pub arguments: bool,
    pub collections: bool,
}
impl Default for Indents {
    fn default() -> Self {
        Self {
            smart_tabs: false,
            continuation: 4,
            keep_empty: false,
            in_parentheses: true,
            hanging_closing: false,
            parameters: true,
            arguments: true,
            collections: true,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlankLines {
    pub max_declarations: usize,
    pub max_code: usize,
    pub before_imports: usize,
    pub after_imports: usize,
    pub before_first_method: usize,
    pub after_local_imports: usize,
    pub before_class_docstring: usize,
    pub after_class_docstring: usize,
}
impl Default for BlankLines {
    fn default() -> Self {
        Self {
            max_declarations: 2,
            max_code: 1,
            before_imports: 0,
            after_imports: 1,
            before_first_method: 0,
            after_local_imports: 1,
            before_class_docstring: 0,
            after_class_docstring: 1,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Imports {
    pub sort: bool,
    pub case_insensitive: bool,
    pub modules_first: bool,
    pub join_from: bool,
    pub align: bool,
    pub group: bool,
    pub remove_unused_only: bool,
    pub preserve_order: bool,
    pub force_from: bool,
    pub prefer_relative: bool,
    pub relative_style: String,
    pub import_before_from: bool,
    pub parentheses: bool,
    pub trailing_comma: bool,
}
impl Default for Imports {
    fn default() -> Self {
        Self {
            sort: true,
            case_insensitive: true,
            modules_first: true,
            join_from: false,
            align: false,
            group: true,
            remove_unused_only: false,
            preserve_order: false,
            force_from: false,
            prefer_relative: false,
            relative_style: "preserve".into(),
            import_before_from: true,
            parentheses: false,
            trailing_comma: true,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Arrangement {
    pub enabled: bool,
    pub order: Vec<String>,
}
impl Default for Arrangement {
    fn default() -> Self {
        Self {
            enabled: false,
            order: vec![
                "dunder".into(),
                "class_variables".into(),
                "static".into(),
                "abstract".into(),
                "public".into(),
                "protected".into(),
                "private".into(),
            ],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PythonStyle {
    pub indents: Indents,
    pub spaces: BTreeMap<String, bool>,
    pub wrapping: BTreeMap<String, Wrap>,
    pub blank_lines: BlankLines,
    pub imports: Imports,
    pub arrangement: Arrangement,
    pub dict_alignment: String,
    pub docstring_quotes: String,
    pub type_hint_quotes: String,
    pub multiline_trailing_comma: bool,
    pub explicit_continuation: bool,
    pub space_after_comment: bool,
    pub spaces_before_comment: usize,
    pub first_column_comments: bool,
    pub docstring_style: String,
    pub self_type_hints: bool,
    pub force_after_colon: bool,
}
impl Default for PythonStyle {
    fn default() -> Self {
        Self {
            indents: Indents::default(),
            spaces: BTreeMap::new(),
            wrapping: BTreeMap::new(),
            blank_lines: BlankLines::default(),
            imports: Imports::default(),
            arrangement: Arrangement::default(),
            dict_alignment: "none".into(),
            docstring_quotes: "inherit".into(),
            type_hint_quotes: "inherit".into(),
            multiline_trailing_comma: true,
            explicit_continuation: false,
            space_after_comment: true,
            spaces_before_comment: 2,
            first_column_comments: false,
            docstring_style: "auto".into(),
            self_type_hints: false,
            force_after_colon: false,
        }
    }
}
pub const SPACE_OPTIONS: &[(&str, &str)] = &[
    (
        "operator_at_line_end",
        "Binary operator at end of wrapped line",
    ),
    ("before_call", "Before method-call parentheses"),
    ("before_declaration", "Before declaration parentheses"),
    ("before_subscript", "Before subscript bracket"),
    ("assignment", "Around assignment operators"),
    ("keyword_equals", "Around keyword-argument ="),
    ("default_equals", "Around default-value ="),
    ("arrow", "Around return annotation ->"),
    ("equality", "Around equality operators"),
    ("relational", "Around relational operators"),
    ("bitwise", "Around bitwise operators"),
    ("additive", "Around additive operators"),
    ("multiplicative", "Around multiplicative operators"),
    ("shift", "Around shift operators"),
    ("power", "Around exponentiation **"),
    ("walrus", "Around assignment expression :="),
    ("type_union", "Around type-union |"),
    ("within_parentheses", "Within parentheses"),
    ("within_brackets", "Within brackets"),
    ("within_braces", "Within braces"),
    ("empty_call", "Within empty call parentheses"),
    ("before_dict_colon", "Before dictionary colon"),
    ("after_dict_colon", "After dictionary colon"),
    ("before_slice_colon", "Before slice colon"),
    ("after_slice_colon", "After slice colon"),
    ("before_annotation_colon", "Before annotation colon"),
    ("after_annotation_colon", "After annotation colon"),
    ("before_lambda_colon", "Before lambda colon"),
    ("after_lambda_colon", "After lambda colon"),
    ("after_comma", "After comma"),
    ("after_unpack", "After unpacking * / **"),
];
pub const WRAP_OPTIONS: &[(&str, &str)] = &[
    ("calls", "Method-call arguments"),
    ("parameters", "Declaration parameters"),
    ("lists", "List literals"),
    ("tuples", "Tuple literals"),
    ("sets", "Set literals"),
    ("dicts", "Dictionary literals"),
    ("imports", "From imports"),
    ("conditions", "If conditions"),
    ("binary", "Binary expressions"),
    ("comprehensions", "Comprehensions"),
    ("with", "With items"),
    ("chains", "Method chains"),
    ("lambdas", "Lambda expressions"),
];

/// Alternative `ij_python_*` property spellings accepted alongside the compact scheme keys.
pub const EDITORCONFIG_ALIASES: &[(&str, &str)] = &[
    (
        "ij_python_space_before_method_call_parentheses",
        "ij_python_space_before_call",
    ),
    (
        "ij_python_space_before_method_parentheses",
        "ij_python_space_before_declaration",
    ),
    (
        "ij_python_space_before_left_bracket",
        "ij_python_space_before_subscript",
    ),
    (
        "ij_python_space_around_assignment_operators",
        "ij_python_space_assignment",
    ),
    (
        "ij_python_space_around_eq_in_keyword_argument",
        "ij_python_space_keyword_equals",
    ),
    (
        "ij_python_space_around_eq_in_default_parameter",
        "ij_python_space_default_equals",
    ),
    (
        "ij_python_space_around_equality_operators",
        "ij_python_space_equality",
    ),
    (
        "ij_python_space_around_relational_operators",
        "ij_python_space_relational",
    ),
    (
        "ij_python_space_around_bitwise_operators",
        "ij_python_space_bitwise",
    ),
    (
        "ij_python_space_around_additive_operators",
        "ij_python_space_additive",
    ),
    (
        "ij_python_space_around_multiplicative_operators",
        "ij_python_space_multiplicative",
    ),
    (
        "ij_python_space_around_shift_operators",
        "ij_python_space_shift",
    ),
    (
        "ij_python_space_around_power_operator",
        "ij_python_space_power",
    ),
    (
        "ij_python_space_around_annotation_arrow",
        "ij_python_space_arrow",
    ),
    (
        "ij_python_space_around_walrus_operator",
        "ij_python_space_walrus",
    ),
    (
        "ij_python_space_around_union_operator",
        "ij_python_space_type_union",
    ),
    (
        "ij_python_space_within_parentheses",
        "ij_python_space_within_parentheses",
    ),
    (
        "ij_python_space_within_brackets",
        "ij_python_space_within_brackets",
    ),
    (
        "ij_python_space_within_braces",
        "ij_python_space_within_braces",
    ),
    (
        "ij_python_space_within_empty_method_call_parentheses",
        "ij_python_space_empty_call",
    ),
];
impl PythonStyle {
    pub fn space(&self, key: &str, default: bool) -> bool {
        self.spaces.get(key).copied().unwrap_or(default)
    }
    pub fn wrap(&self, key: &str) -> Wrap {
        self.wrapping.get(key).cloned().unwrap_or_default()
    }
    pub fn editorconfig(&self) -> String {
        let mut out = format!(
            "ij_python_style_options = {}\n",
            serde_json::to_string(self).unwrap_or_default()
        );
        for (key, _) in SPACE_OPTIONS {
            if let Some(value) = self.spaces.get(*key) {
                let compact = format!("ij_python_space_{key}");
                let exported = EDITORCONFIG_ALIASES
                    .iter()
                    .find(|(_, target)| *target == compact)
                    .map_or(compact.as_str(), |(key, _)| *key);
                out.push_str(&format!("{exported} = {value}\n"))
            }
        }
        for (key, rule) in &self.wrapping {
            out.push_str(&format!("ij_python_wrap_{key} = {}\nij_python_align_{key} = {}\nij_python_newline_after_open_{key} = {}\nij_python_closing_on_newline_{key} = {}\n",rule.mode,rule.align,rule.newline_after_open,rule.closing_on_newline))
        }
        out.push_str(&format!("ij_continuation_indent_size = {}\nij_smart_tabs = {}\nij_python_keep_indents_on_empty_lines = {}\nij_python_indent_in_parentheses = {}\nij_python_hang_closing_brackets = {}\nij_python_use_continuation_indent_for_parameters = {}\nij_python_use_continuation_indent_for_arguments = {}\nij_python_use_continuation_indent_for_collection_and_comprehensions = {}\n",self.indents.continuation,self.indents.smart_tabs,self.indents.keep_empty,self.indents.in_parentheses,self.indents.hanging_closing,self.indents.parameters,self.indents.arguments,self.indents.collections));
        out
    }
    pub fn import_property(&mut self, key: &str, value: &str) -> bool {
        if let Some((_, target)) = EDITORCONFIG_ALIASES
            .iter()
            .find(|(alias, target)| *alias == key && *alias != *target)
        {
            return self.import_property(target, value);
        }
        let wrap_key = match key {
            "ij_python_call_parameters_wrap" => Some("calls"),
            "ij_python_method_parameters_wrap" => Some("parameters"),
            "ij_python_list_items_wrap" => Some("lists"),
            "ij_python_dict_literals_wrap" => Some("dicts"),
            "ij_python_from_imports_wrap" => Some("imports"),
            "ij_python_binary_operation_wrap" => Some("binary"),
            _ => None,
        };
        if let Some(key) = wrap_key {
            let mode = match value {
                "0" => "never",
                "1" => "if_long",
                "2" => "always",
                "5" => "chop",
                _ => return true,
            };
            self.wrapping.entry(key.into()).or_default().mode = mode.into();
            return true;
        }
        let alignment = match key {
            "ij_python_align_multiline_parameters" => Some("parameters"),
            "ij_python_align_multiline_parameters_in_calls" => Some("calls"),
            "ij_python_align_multiline_dict" => Some("dicts"),
            "ij_python_align_multiline_imports" => Some("imports"),
            "ij_python_align_multiline_binary_operation" => Some("binary"),
            _ => None,
        };
        if let Some(key) = alignment {
            if let Ok(value) = value.parse() {
                self.wrapping.entry(key.into()).or_default().align = value;
            }
            return true;
        }
        if let Ok(number) = value.parse::<usize>() {
            let target = match key {
                "ij_python_keep_blank_lines_in_declarations" => {
                    Some(&mut self.blank_lines.max_declarations)
                }
                "ij_python_keep_blank_lines_in_code" => Some(&mut self.blank_lines.max_code),
                "ij_python_blank_lines_before_imports" => {
                    Some(&mut self.blank_lines.before_imports)
                }
                "ij_python_blank_lines_after_imports" => Some(&mut self.blank_lines.after_imports),
                "ij_python_blank_lines_before_first_method" => {
                    Some(&mut self.blank_lines.before_first_method)
                }
                "ij_python_blank_lines_after_local_imports" => {
                    Some(&mut self.blank_lines.after_local_imports)
                }
                "ij_python_blank_lines_before_class_docstring" => {
                    Some(&mut self.blank_lines.before_class_docstring)
                }
                "ij_python_blank_lines_after_class_docstring" => {
                    Some(&mut self.blank_lines.after_class_docstring)
                }
                "ij_python_spaces_before_number_sign" => Some(&mut self.spaces_before_comment),
                _ => None,
            };
            if let Some(target) = target {
                *target = number.min(100);
                return true;
            }
        }
        if key == "ij_python_space_after_number_sign" {
            if let Ok(value) = value.parse() {
                self.space_after_comment = value
            }
            return true;
        }
        if key == "ij_python_style_options" {
            if value.len() <= 65536 {
                if let Ok(style) = serde_json::from_str(value) {
                    *self = style
                }
            }
            return true;
        }
        if let Some(name) = key.strip_prefix("ij_python_space_") {
            if SPACE_OPTIONS.iter().any(|(key, _)| *key == name) {
                if let Ok(value) = value.parse() {
                    self.spaces.insert(name.into(), value);
                }
                return true;
            }
        }
        for (prefix, field) in [
            ("ij_python_wrap_", 0),
            ("ij_python_align_", 1),
            ("ij_python_newline_after_open_", 2),
            ("ij_python_closing_on_newline_", 3),
        ] {
            if let Some(name) = key.strip_prefix(prefix) {
                if WRAP_OPTIONS.iter().any(|(key, _)| *key == name) {
                    let wrap = self.wrapping.entry(name.into()).or_default();
                    match field {
                        0 if matches!(
                            value,
                            "inherit" | "never" | "if_long" | "chop" | "always"
                        ) =>
                        {
                            wrap.mode = value.into()
                        }
                        1 => {
                            if let Ok(v) = value.parse() {
                                wrap.align = v
                            }
                        }
                        2 => {
                            if let Ok(v) = value.parse() {
                                wrap.newline_after_open = v
                            }
                        }
                        3 => {
                            if let Ok(v) = value.parse() {
                                wrap.closing_on_newline = v
                            }
                        }
                        _ => {}
                    }
                    return true;
                }
            }
        }
        let boolean = value.parse::<bool>().ok();
        match key {
            "ij_continuation_indent_size" => {
                if let Ok(v) = value.parse::<usize>() {
                    self.indents.continuation = v.clamp(1, 256)
                }
            }
            "ij_smart_tabs" => {
                if let Some(v) = boolean {
                    self.indents.smart_tabs = v
                }
            }
            "ij_python_keep_indents_on_empty_lines" => {
                if let Some(v) = boolean {
                    self.indents.keep_empty = v
                }
            }
            "ij_python_indent_in_parentheses" => {
                if let Some(v) = boolean {
                    self.indents.in_parentheses = v
                }
            }
            "ij_python_hang_closing_brackets" => {
                if let Some(v) = boolean {
                    self.indents.hanging_closing = v
                }
            }
            "ij_python_use_continuation_indent_for_parameters" => {
                if let Some(v) = boolean {
                    self.indents.parameters = v
                }
            }
            "ij_python_use_continuation_indent_for_arguments" => {
                if let Some(v) = boolean {
                    self.indents.arguments = v
                }
            }
            "ij_python_use_continuation_indent_for_collection_and_comprehensions" => {
                if let Some(v) = boolean {
                    self.indents.collections = v
                }
            }
            _ => return false,
        }
        true
    }
}

/// Exact supported EditorConfig keys for language-aware completion/validation.
pub fn supported_editorconfig_keys() -> Vec<String> {
    let mut keys = EDITORCONFIG_ALIASES
        .iter()
        .map(|(key, _)| (*key).into())
        .collect::<Vec<String>>();
    keys.extend(
        SPACE_OPTIONS
            .iter()
            .map(|(key, _)| format!("ij_python_space_{key}")),
    );
    for (key, _) in WRAP_OPTIONS {
        for prefix in ["wrap", "align", "newline_after_open", "closing_on_newline"] {
            keys.push(format!("ij_python_{prefix}_{key}"))
        }
    }
    keys.extend(
        [
            "ij_python_style_options",
            "ij_continuation_indent_size",
            "ij_smart_tabs",
            "ij_python_keep_indents_on_empty_lines",
            "ij_python_indent_in_parentheses",
            "ij_python_hang_closing_brackets",
            "ij_python_use_continuation_indent_for_parameters",
            "ij_python_use_continuation_indent_for_arguments",
            "ij_python_use_continuation_indent_for_collection_and_comprehensions",
            "ij_python_call_parameters_wrap",
            "ij_python_method_parameters_wrap",
            "ij_python_list_items_wrap",
            "ij_python_dict_literals_wrap",
            "ij_python_from_imports_wrap",
            "ij_python_binary_operation_wrap",
            "ij_python_align_multiline_parameters",
            "ij_python_align_multiline_parameters_in_calls",
            "ij_python_align_multiline_dict",
            "ij_python_align_multiline_imports",
            "ij_python_align_multiline_binary_operation",
            "ij_python_keep_blank_lines_in_declarations",
            "ij_python_keep_blank_lines_in_code",
            "ij_python_blank_lines_before_imports",
            "ij_python_blank_lines_after_imports",
            "ij_python_blank_lines_before_first_method",
            "ij_python_blank_lines_after_local_imports",
            "ij_python_blank_lines_before_class_docstring",
            "ij_python_blank_lines_after_class_docstring",
            "ij_python_spaces_before_number_sign",
            "ij_python_space_after_number_sign",
        ]
        .into_iter()
        .map(String::from),
    );
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_visible_space_and_wrap_option_round_trips_through_editorconfig() {
        let mut original = PythonStyle::default();
        for (key, _) in SPACE_OPTIONS {
            original.spaces.insert((*key).into(), true);
        }
        for (key, _) in WRAP_OPTIONS {
            original.wrapping.insert(
                (*key).into(),
                Wrap {
                    mode: "always".into(),
                    align: true,
                    newline_after_open: false,
                    closing_on_newline: false,
                },
            );
        }
        original.indents.continuation = 8;
        original.imports.relative_style = "relative".into();
        original.blank_lines.after_local_imports = 2;
        original.dict_alignment = "values".into();
        let text = original.editorconfig();
        let mut restored = PythonStyle::default();
        for line in text.lines() {
            let (key, value) = line.split_once('=').unwrap();
            assert!(restored.import_property(key.trim(), value.trim()));
        }
        assert_eq!(original, restored)
    }
    #[test]
    fn incomplete_or_unknown_editorconfig_settings_do_not_invent_options() {
        let mut style = PythonStyle::default();
        assert!(!style.import_property("ij_python_space_unimplemented", "true"));
        assert!(style.import_property("ij_python_wrap_calls", "bad"));
        assert_eq!(style.wrap("calls"), Wrap::default());
        style.import_property("ij_continuation_indent_size", "0");
        assert_eq!(style.indents.continuation, 1)
    }
}
