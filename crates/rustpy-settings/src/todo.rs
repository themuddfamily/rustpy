//! Persisted TODO regex, case and presentation policy.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TodoPattern {
    pub regex: String,
    pub case_sensitive: bool,
    pub color: [u8; 3],
}
impl Default for TodoPattern {
    fn default() -> Self {
        Self {
            regex: r"\bTODO\b".into(),
            case_sensitive: false,
            color: [168, 192, 35],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TodoSettings {
    pub patterns: Vec<TodoPattern>,
    pub multiline: bool,
    pub comments: bool,
    pub strings: bool,
}
impl Default for TodoSettings {
    fn default() -> Self {
        Self {
            patterns: ["TODO", "FIXME", "XXX", "HACK", "BUG"]
                .into_iter()
                .map(|name| TodoPattern {
                    regex: format!(r"\b{name}\b"),
                    ..Default::default()
                })
                .collect(),
            multiline: true,
            comments: true,
            strings: false,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_missing_settings_and_custom_policy_roundtrip() {
        let settings: crate::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.todo, TodoSettings::default());
        let mut settings = settings;
        settings.todo.patterns = vec![TodoPattern {
            regex: "REVIEW.*".into(),
            case_sensitive: true,
            color: [1, 2, 255],
        }];
        settings.todo.multiline = false;
        assert_eq!(
            serde_json::from_str::<crate::Settings>(&serde_json::to_string(&settings).unwrap())
                .unwrap()
                .todo,
            settings.todo
        );
    }
}
