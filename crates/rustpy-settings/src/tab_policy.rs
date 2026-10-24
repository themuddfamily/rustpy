//! Editor tab presentation and opening policies; absent fields preserve legacy layout.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabPlacement {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
    None,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabOpeningOrder {
    #[default]
    End,
    AfterCurrent,
    Alphabetical,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabPolicySettings {
    pub placement: TabPlacement,
    pub multiple_rows: bool,
    pub show_file_icons: bool,
    pub show_full_path: bool,
    pub opening_order: TabOpeningOrder,
    pub sort_by_name: bool,
    pub reuse_unmodified: bool,
    pub file_colors: Vec<TabFileColor>,
}
impl Default for TabPolicySettings {
    fn default() -> Self {
        Self {
            placement: TabPlacement::Top,
            multiple_rows: false,
            show_file_icons: false,
            show_full_path: false,
            opening_order: TabOpeningOrder::End,
            sort_by_name: false,
            reuse_unmodified: false,
            file_colors: Vec::new(),
        }
    }
}
/// A named directory scope. Component-wise matching prevents `src` matching `src2`.
/// Relative directories resolve under the current project; absolute scopes also cover libraries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabFileColor {
    pub name: String,
    pub directory: PathBuf,
    pub rgb: [u8; 3],
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_keep_top_end_no_reuse_and_new_policy_round_trips() {
        let old: crate::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(old.editor.tabs, TabPolicySettings::default());
        let mut settings = old;
        settings.editor.tabs = TabPolicySettings {
            placement: TabPlacement::Right,
            multiple_rows: true,
            show_full_path: true,
            opening_order: TabOpeningOrder::Alphabetical,
            reuse_unmodified: true,
            file_colors: vec![TabFileColor {
                name: "Tests".into(),
                directory: "tests/π".into(),
                rgb: [23, 40, 70],
            }],
            ..Default::default()
        };
        let saved = serde_json::to_string(&settings).unwrap();
        let loaded: crate::Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(loaded.editor.tabs, settings.editor.tabs);
    }
}
