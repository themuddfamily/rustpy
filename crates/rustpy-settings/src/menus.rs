//! Declarative, bounded menu overrides. Absence means the native provider's defaults.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    Action {
        id: String,
        #[serde(default)]
        icon: String,
    },
    Separator,
    Group {
        label: String,
        children: Vec<Entry>,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Background {
    pub path: String,
    pub opacity: f32,
    pub placement: String,
    pub editor: bool,
    pub empty_frame: bool,
}
impl Default for Background {
    fn default() -> Self {
        Self {
            path: String::new(),
            opacity: 0.15,
            placement: "Fill".into(),
            editor: true,
            empty_frame: true,
        }
    }
}
/// Text coverage mode; source/layout metrics are independent of this choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAntialiasing {
    #[default]
    Grayscale,
    None,
    Subpixel,
}
impl TextAntialiasing {
    pub fn coverage(self, bgr: bool) -> u8 {
        match self {
            Self::Grayscale => 0,
            Self::None => 1,
            Self::Subpixel => {
                if bgr {
                    3
                } else {
                    2
                }
            }
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub background: Background,
    pub ide_text_antialiasing: TextAntialiasing,
    pub editor_text_antialiasing: TextAntialiasing,
    pub subpixel_bgr: bool,
    pub tree_indent_guides: bool,
    pub compact_tree_indents: bool,
    pub presentation_font_size: f32,
    pub sync_with_os: bool,
    pub tooltip_delay: f32,
    pub tooltip_shortcuts: bool,
    pub menu_mnemonics: bool,
    pub menu_icons: bool,
    /// Animation of ordinary IDE scroll areas; editor scrolling is separate.
    pub smooth_scrolling: bool,
    pub smooth_shapes: bool,
    pub pixel_aligned_text: bool,
}
impl Appearance {
    /// Accessibility overrides the preference, regardless of plugin ordering.
    pub fn animate_scrolling(&self, reduced_motion: bool) -> bool {
        self.smooth_scrolling && !reduced_motion
    }
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            background: Background::default(),
            ide_text_antialiasing: TextAntialiasing::Grayscale,
            editor_text_antialiasing: TextAntialiasing::Grayscale,
            subpixel_bgr: false,
            tree_indent_guides: false,
            compact_tree_indents: false,
            presentation_font_size: 24.0,
            sync_with_os: false,
            tooltip_delay: 0.5,
            tooltip_shortcuts: true,
            menu_mnemonics: false,
            menu_icons: false,
            smooth_scrolling: true,
            smooth_shapes: true,
            pixel_aligned_text: true,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub overrides: BTreeMap<String, Vec<Entry>>,
    pub appearance: Appearance,
}
pub const LOCATIONS: &[(&str, &str)] = &[
    ("main.root", "Main menu bar"),
    ("main.file", "File menu"),
    ("main.edit", "Edit menu"),
    ("main.view", "View menu"),
    ("main.navigate", "Navigate menu"),
    ("main.code", "Code menu"),
    ("main.refactor", "Refactor menu"),
    ("main.run", "Run menu"),
    ("main.tools", "Tools menu"),
    ("main.git", "Git menu"),
    ("main.window", "Window menu"),
    ("main.help", "Help menu"),
    ("editor.popup", "Editor popup"),
    ("project.popup", "Project View popup"),
    ("navigation.popup", "Navigation bar popup"),
    ("run.popup", "Run session popup"),
];
pub fn validate(entries: &[Entry]) -> Result<(), String> {
    fn walk(entries: &[Entry], depth: usize, total: &mut usize) -> Result<(), String> {
        if depth > 8 {
            return Err("Menu groups are limited to eight levels".into());
        }
        for entry in entries {
            *total += 1;
            if *total > 512 {
                return Err("A menu is limited to 512 entries".into());
            }
            match entry {
                Entry::Action { id, icon }
                    if id.is_empty() || id.len() > 160 || icon.chars().count() > 8 =>
                {
                    return Err("Invalid action ID or icon".into())
                }
                Entry::Group { label, children } => {
                    if label.trim().is_empty() || label.chars().count() > 128 {
                        return Err("Group names must contain 1–128 characters".into());
                    }
                    walk(children, depth + 1, total)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
    walk(entries, 0, &mut 0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_order_and_explicit_empty_override_survive_persistence() {
        let mut settings = Settings::default();
        settings.overrides.insert(
            "editor.popup".into(),
            vec![Entry::Group {
                label: "Review".into(),
                children: vec![
                    Entry::Action {
                        id: "GotoDeclaration".into(),
                        icon: "→".into(),
                    },
                    Entry::Separator,
                ],
            }],
        );
        settings
            .overrides
            .insert("project.popup".into(), Vec::new());
        let saved = serde_json::to_string(&settings).unwrap();
        let loaded: Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(loaded, settings);
        assert!(loaded.overrides.contains_key("project.popup"));
        assert!(validate(&loaded.overrides["editor.popup"]).is_ok());
    }
    #[test]
    fn ide_scroll_defaults_persist_and_reduced_motion_always_wins() {
        let legacy: Appearance = serde_json::from_str("{}").unwrap();
        assert!(legacy.smooth_scrolling);
        assert!(legacy.animate_scrolling(false));
        assert!(!legacy.animate_scrolling(true));
        let mut disabled = legacy;
        disabled.smooth_scrolling = false;
        let loaded: Appearance =
            serde_json::from_str(&serde_json::to_string(&disabled).unwrap()).unwrap();
        assert!(!loaded.animate_scrolling(false));
        assert!(!loaded.animate_scrolling(true));
        // This IDE policy does not mutate the separate editor preference.
        let editor = crate::EditorSettings::default();
        let before = editor.smooth_scrolling;
        let _ = loaded.animate_scrolling(false);
        assert_eq!(editor.smooth_scrolling, before);
    }
    #[test]
    fn recursive_input_is_bounded() {
        let mut entries = vec![Entry::Separator];
        for _ in 0..10 {
            entries = vec![Entry::Group {
                label: "Group".into(),
                children: entries,
            }];
        }
        assert!(validate(&entries).is_err());
    }
    #[test]
    fn text_modes_are_independent_and_legacy_preferences_keep_grayscale() {
        let legacy: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            legacy.appearance.ide_text_antialiasing,
            TextAntialiasing::Grayscale
        );
        let mut prefs = legacy;
        prefs.appearance.ide_text_antialiasing = TextAntialiasing::None;
        prefs.appearance.editor_text_antialiasing = TextAntialiasing::Subpixel;
        prefs.appearance.subpixel_bgr = true;
        let loaded: Settings =
            serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
        assert_eq!(loaded, prefs);
        assert_eq!(loaded.appearance.ide_text_antialiasing.coverage(true), 1);
        assert_eq!(loaded.appearance.editor_text_antialiasing.coverage(true), 3);
    }
}
