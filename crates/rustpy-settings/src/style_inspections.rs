//! Shared editor, CLI and LSP style-inspection defaults. Per-inspection profile
//! options override these values; zero metric thresholds disable that metric.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct StyleInspections {
    /// auto uses the selected trusted interpreter and bundled pycodestyle;
    /// builtin keeps the Rust fast checks; external requires the full backend.
    pub pep8_backend: String,
    pub interpreter: Option<PathBuf>,
    pub pep8_ignore: Vec<String>,
    pub naming_ignore: Vec<String>,
    pub naming_ignore_patterns: Vec<String>,
    pub ignore_overrides: bool,
    pub check_constants: bool,
    pub doc_modules: bool,
    pub doc_classes: bool,
    pub doc_functions: bool,
    pub doc_methods: bool,
    pub doc_private: bool,
    pub single_quoted_docstrings: bool,
    pub mandatory_encoding: bool,
    pub missing_shebang: bool,
    pub non_ascii_identifiers: bool,
    pub non_ascii_comments: bool,
    pub quote_style: String,
    pub trailing_comma: bool,
    pub max_arguments: usize,
    pub max_branches: usize,
    pub max_returns: usize,
    pub max_statements: usize,
    pub max_locals: usize,
    pub duplicate_code: bool,
    pub duplicate_min_lines: usize,
    pub duplicate_min_tokens: usize,
}
impl Default for StyleInspections {
    fn default() -> Self {
        Self {
            pep8_backend: "auto".into(),
            interpreter: None,
            pep8_ignore: [
                "E121", "E123", "E126", "E226", "E24", "E704", "W503", "W504",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            naming_ignore: vec!["_".into()],
            naming_ignore_patterns: Vec::new(),
            ignore_overrides: true,
            // Opt-in (pylint's invalid-name style): the PEP 8 naming
            // inspection and pep8-naming never require module-level
            // assignments of literals to be UPPER_CASE.
            check_constants: false,
            doc_modules: true,
            doc_classes: true,
            doc_functions: true,
            doc_methods: true,
            doc_private: false,
            single_quoted_docstrings: true,
            mandatory_encoding: false,
            missing_shebang: false,
            non_ascii_identifiers: false,
            non_ascii_comments: false,
            quote_style: "code_style".into(),
            trailing_comma: false,
            max_arguments: 0,
            max_branches: 0,
            max_returns: 0,
            max_statements: 0,
            max_locals: 0,
            duplicate_code: false,
            duplicate_min_lines: 6,
            duplicate_min_tokens: 30,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_defaults_and_custom_style_options_roundtrip() {
        let legacy: StyleInspections = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy, StyleInspections::default());
        let mut configured = legacy;
        configured.naming_ignore = vec!["setUp".into()];
        configured.max_arguments = 5;
        configured.duplicate_code = true;
        configured.pep8_ignore = vec!["W503".into()];
        let decoded: StyleInspections =
            serde_json::from_str(&serde_json::to_string(&configured).unwrap()).unwrap();
        assert_eq!(decoded, configured);
    }
}
