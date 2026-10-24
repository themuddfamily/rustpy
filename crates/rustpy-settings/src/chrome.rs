//! Portable window chrome and toolbar preferences, independent of project state.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NavigationLocation {
    #[default]
    Editor,
    Toolbar,
}

/// Stable IDs are persisted independently of the localized widget captions.
pub const STATUS_WIDGETS: &[(&str, &str)] = &[
    ("status", "Status messages and project path"),
    ("task", "Current task and time tracking"),
    ("changes", "Changes to commit"),
    ("position", "Line, column and selection"),
    ("line-endings", "Line separators"),
    ("encoding", "File encoding"),
    ("indent", "Indentation"),
    ("read-only", "Read-only lock"),
    ("language", "File language and overwrite mode"),
    ("git", "Git branch"),
    ("interpreter", "Python interpreter"),
    ("highlighting", "Inspection highlighting level"),
    ("indexing", "Indexing state"),
    ("progress", "Background tasks"),
    ("notifications", "Notifications"),
    ("memory", "Memory indicator (requires opt-in)"),
    ("power-save", "Power Save Mode"),
    ("collaboration", "Collaboration"),
    ("reader", "Reader Mode"),
    ("column", "Column Selection Mode"),
    ("updates", "IDE and plugin updates"),
    ("zoom", "IDE zoom"),
    ("intentions", "Context actions"),
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Element {
    Project,
    Vcs,
    Run,
    Action { id: String },
    Group { label: String, actions: Vec<String> },
    Separator,
}
impl Element {
    pub fn label(&self) -> &str {
        match self {
            Self::Project => "Project widget",
            Self::Vcs => "VCS widget",
            Self::Run => "Run widget",
            Self::Action { id } => id,
            Self::Group { label, .. } => label,
            Self::Separator => "Separator",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub new_ui: bool,
    pub native_header: bool,
    pub separate_main_menu: bool,
    pub show_main_menu: bool,
    pub scale: f32,
    pub title_project: bool,
    pub title_file: bool,
    pub title_full_path: bool,
    pub title_branch: bool,
    pub toolbar: Vec<Element>,
    pub hidden_status_widgets: BTreeSet<String>,
    pub navigation_location: NavigationLocation,
    pub navigation_members: bool,
    pub tool_window_numbers: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            new_ui: true,
            native_header: true,
            separate_main_menu: true,
            show_main_menu: true,
            scale: 1.0,
            title_project: true,
            title_file: true,
            title_full_path: true,
            title_branch: true,
            toolbar: vec![
                Element::Project,
                Element::Vcs,
                Element::Run,
                Element::Action {
                    id: "SearchEverywhere".into(),
                },
                Element::Action {
                    id: "ShowSettings".into(),
                },
            ],
            hidden_status_widgets: BTreeSet::new(),
            navigation_location: NavigationLocation::Editor,
            navigation_members: true,
            tool_window_numbers: true,
        }
    }
}
impl Settings {
    pub fn effective_scale(&self) -> f32 {
        if self.scale.is_finite() {
            self.scale.clamp(0.5, 3.0)
        } else {
            1.0
        }
    }
    pub fn status_visible(&self, id: &str) -> bool {
        !self.hidden_status_widgets.contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn customized_toolbar_round_trips_order_groups_and_visibility() {
        let mut settings = Settings {
            toolbar: vec![
                Element::Group {
                    label: "Inspect".into(),
                    actions: vec!["InspectCode".into(), "ReformatCode".into()],
                },
                Element::Separator,
                Element::Project,
            ],
            ..Default::default()
        };
        settings.hidden_status_widgets.insert("encoding".into());
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
        assert!(!restored.status_visible("encoding"));
        assert!(restored.status_visible("new-widget"));
        assert_eq!(
            Settings {
                scale: f32::NAN,
                ..Default::default()
            }
            .effective_scale(),
            1.0
        );
    }
}
