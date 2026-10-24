//! IDE defaults and independent, persisted project documentation preferences.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DocstringPreferences {
    pub format: String,
    pub analyze_doc_comments: bool,
}
impl Default for DocstringPreferences {
    fn default() -> Self {
        Self {
            format: "auto".into(),
            analyze_doc_comments: true,
        }
    }
}
impl DocstringPreferences {
    pub fn format_key(&self) -> &str {
        match self.format.as_str() {
            "plain" | "rest" | "epytext" | "google" | "numpy" => &self.format,
            _ => "auto",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DocumentationSettings {
    /// Follow caret changes in the unpinned Documentation tool window.
    pub auto_update: bool,
    pub font_size: u8,
    pub analyze_doc_comments: bool,
    pub projects: BTreeMap<PathBuf, DocstringPreferences>,
}
impl Default for DocumentationSettings {
    fn default() -> Self {
        Self {
            auto_update: true,
            font_size: 13,
            analyze_doc_comments: true,
            projects: BTreeMap::new(),
        }
    }
}
impl DocumentationSettings {
    pub fn effective(&self, project: Option<&Path>, legacy_format: &str) -> DocstringPreferences {
        project
            .and_then(|root| self.projects.get(root))
            .cloned()
            .unwrap_or_else(|| DocstringPreferences {
                format: legacy_format.into(),
                analyze_doc_comments: self.analyze_doc_comments,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn documentation_window_preferences_migrate_and_roundtrip_independently_of_docstring_style() {
        let legacy: DocumentationSettings = serde_json::from_str("{}").unwrap();
        assert!(legacy.auto_update);
        assert_eq!(legacy.font_size, 13);
        let mut settings = legacy;
        settings.auto_update = false;
        settings.font_size = 19;
        let restored: DocumentationSettings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert!(!restored.auto_update);
        assert_eq!(restored.font_size, 19);
        assert!(restored.analyze_doc_comments);
    }
    #[test]
    fn project_overrides_and_legacy_defaults_roundtrip_without_cross_project_leaks() {
        let mut settings = DocumentationSettings::default();
        let first = PathBuf::from("/workspace/first");
        let second = PathBuf::from("/workspace/second");
        settings.projects.insert(
            first.clone(),
            DocstringPreferences {
                format: "numpy".into(),
                analyze_doc_comments: false,
            },
        );
        let restored: DocumentationSettings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(
            restored.effective(Some(&first), "rest").format_key(),
            "numpy"
        );
        assert!(
            !restored
                .effective(Some(&first), "rest")
                .analyze_doc_comments
        );
        assert_eq!(
            restored.effective(Some(&second), "google").format_key(),
            "google"
        );
        assert!(
            restored
                .effective(Some(&second), "google")
                .analyze_doc_comments
        );
        assert!(
            serde_json::from_str::<DocumentationSettings>("{}")
                .unwrap()
                .analyze_doc_comments
        );
        assert_eq!(
            DocstringPreferences {
                format: "unknown".into(),
                ..Default::default()
            }
            .format_key(),
            "auto"
        );
    }
}
