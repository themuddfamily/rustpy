//! Dialog policies are IDE-local. Project preference files never grant trust or
//! carry executable paths, startup tasks, credentials or interpreter choices.
use crate::Settings;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub sizes: BTreeMap<String, [f32; 2]>,
    pub confirmations: BTreeMap<String, bool>,
    pub search_sections: BTreeSet<String>,
    pub sync_project_settings: bool,
    pub developer_mode: bool,
    pub show_widget_ids: bool,
    pub popup_rows: u16,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            sizes: BTreeMap::new(),
            confirmations: BTreeMap::new(),
            search_sections: BTreeSet::new(),
            sync_project_settings: false,
            developer_mode: false,
            show_widget_ids: false,
            popup_rows: 14,
        }
    }
}
impl Preferences {
    pub fn path() -> PathBuf {
        Settings::config_dir().join("dialog-policies.json")
    }
    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) -> anyhow::Result<()> {
        save(&Self::path(), self)
    }
    pub fn size(&self, key: &str, fallback: [f32; 2]) -> [f32; 2] {
        self.sizes
            .get(key)
            .copied()
            .filter(|size| {
                size.iter()
                    .all(|n| n.is_finite() && *n >= 240. && *n <= 4096.)
            })
            .unwrap_or(fallback)
    }
    pub fn remember_size(&mut self, key: &str, size: [f32; 2]) {
        if size
            .iter()
            .all(|n| n.is_finite() && *n >= 240. && *n <= 4096.)
        {
            self.sizes.insert(key.into(), size);
        }
    }
    /// Only non-destructive, explicitly registered confirmations may be skipped.
    pub fn answer(&self, key: &str) -> Option<bool> {
        if key == "rebuild-caches" {
            self.confirmations.get(key).copied()
        } else {
            None
        }
    }
    pub fn remember(&mut self, key: &str, answer: bool) {
        if key == "rebuild-caches" {
            self.confirmations.insert(key.into(), answer);
        }
    }
}
pub fn save(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.new", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    #[cfg(windows)]
    if path.exists() {
        let backup = path.with_extension("previous");
        std::fs::copy(path, &backup)?;
        std::fs::remove_file(path)?;
    }
    std::fs::rename(temporary, path)?;
    Ok(())
}
/// Shareable preferences are intentionally limited to pure formatting rules.
pub fn project_values(settings: &Settings) -> Value {
    let source = serde_json::to_value(&settings.code_style).unwrap_or_default();
    let mut style = serde_json::Map::new();
    for key in FORMAT_FIELDS {
        if let Some(value) = source.get(*key) {
            style.insert((*key).into(), value.clone());
        }
    }
    serde_json::json!({"version":1,"code_style":style})
}
const FORMAT_FIELDS: &[&str] = &[
    "quote_style",
    "magic_trailing_comma",
    "blank_lines_top_level",
    "blank_lines_in_class",
    "right_margin",
    "indent_size",
    "line_separator",
    "max_line_length",
];
pub fn apply_project(settings: &mut Settings, value: &Value) -> anyhow::Result<()> {
    anyhow::ensure!(value["version"] == 1, "Unsupported project preferences");
    if let Some(style) = value.get("code_style") {
        if let Some(quote) = style.get("quote_style") {
            anyhow::ensure!(
                matches!(quote.as_str(), Some("preserve" | "double" | "single")),
                "Unsupported project quote style"
            );
        }
        for key in ["blank_lines_top_level", "blank_lines_in_class"] {
            if let Some(count) = style.get(key) {
                anyhow::ensure!(
                    count.as_u64().is_some_and(|n| n <= 20),
                    "Project blank-line count exceeds 20"
                );
            }
        }
        for key in ["right_margin", "max_line_length"] {
            if let Some(count) = style.get(key) {
                anyhow::ensure!(
                    count.as_u64().is_some_and(|n| (1..=10000).contains(&n)),
                    "Project margin is outside 1..10000"
                );
            }
        }
        if let Some(count) = style.get("indent_size") {
            anyhow::ensure!(
                count.as_u64().is_some_and(|n| (1..=16).contains(&n)),
                "Project indent is outside 1..16"
            );
        }
        let mut merged = serde_json::to_value(&settings.code_style)?;
        for key in FORMAT_FIELDS {
            if let Some(value) = style.get(*key) {
                if let Some(slot) = merged.get_mut(*key) {
                    *slot = value.clone();
                }
            }
        }
        settings.code_style = serde_json::from_value(merged)?;
    }
    Ok(())
}
pub fn project_path(root: &Path) -> PathBuf {
    root.join(".rustpy/project-preferences.json")
}
pub fn save_project(root: &Path, settings: &Settings) -> anyhow::Result<()> {
    check_project_path(root)?;
    save(&project_path(root), &project_values(settings))
}
pub fn load_project(root: &Path, settings: &mut Settings) -> anyhow::Result<()> {
    check_project_path(root)?;
    let path = project_path(root);
    if path.is_file() {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 1024 * 1024, "Project preferences too large");
        apply_project(settings, &serde_json::from_slice(&bytes)?)?;
    }
    Ok(())
}
pub fn defaults_path() -> PathBuf {
    Settings::config_dir().join("new-project-preferences.json")
}
pub fn save_defaults(settings: &Settings) -> anyhow::Result<()> {
    save(&defaults_path(), &project_values(settings))
}
pub fn seed_project(root: &Path) -> anyhow::Result<()> {
    check_project_path(root)?;
    let destination = project_path(root);
    if destination.exists() || !defaults_path().is_file() {
        return Ok(());
    }
    let bytes = std::fs::read(defaults_path())?;
    let value: Value = serde_json::from_slice(&bytes)?;
    let mut checked = Settings::default();
    apply_project(&mut checked, &value)?;
    save(&destination, &project_values(&checked))
}
fn check_project_path(root: &Path) -> anyhow::Result<()> {
    for path in [root.join(".rustpy"), project_path(root)] {
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "Project preferences cannot follow symbolic links"
            );
        }
    }
    Ok(())
}
/// Leaf paths support per-page Reset without resetting unrelated editor pages.
pub fn leaves(value: &Value) -> Vec<Vec<String>> {
    fn walk(value: &Value, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        if out.len() >= 8192 {
            return;
        }
        if let Some(object) = value.as_object() {
            if object.is_empty() {
                out.push(path.clone());
                return;
            }
            for (key, value) in object {
                path.push(key.clone());
                walk(value, path, out);
                path.pop();
            }
        } else {
            out.push(path.clone());
        }
    }
    let mut result = vec![];
    walk(value, &mut vec![], &mut result);
    result
}
pub fn at<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(key)?;
    }
    Some(current)
}
pub fn replace(value: &mut Value, path: &[String], replacement: Value) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let mut current = value;
    for key in parents {
        let Some(next) = current.get_mut(key) else {
            return false;
        };
        current = next;
    }
    if let Some(slot) = current.get_mut(last) {
        *slot = replacement;
        true
    } else {
        false
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destructive_confirmation_cannot_be_remembered() {
        let mut p = Preferences::default();
        p.remember("delete-local-history", true);
        assert_eq!(p.answer("delete-local-history"), None);
        p.remember("rebuild-caches", true);
        assert_eq!(p.answer("rebuild-caches"), Some(true));
    }
    #[test]
    fn invalid_dialog_geometry_is_not_restored() {
        let mut prefs = Preferences::default();
        prefs.sizes.insert("settings".into(), [f32::NAN, 1000.]);
        assert_eq!(prefs.size("settings", [900., 600.]), [900., 600.]);
        prefs.remember_size("settings", [1000., 700.]);
        assert_eq!(prefs.size("settings", [900., 600.]), [1000., 700.]);
    }
    #[cfg(unix)]
    #[test]
    fn project_preferences_do_not_follow_workspace_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".rustpy")).unwrap();
        assert!(save_project(root.path(), &Settings::default()).is_err());
        assert!(!outside.path().join("project-preferences.json").exists());
    }
    #[test]
    fn project_preferences_cannot_grant_execution_or_trust() {
        let mut settings = Settings::default();
        let old = settings.trusted_projects.clone();
        let value = serde_json::json!({"version":1,"trusted_projects":["/untrusted"],"file_watchers":[{"program":"danger"}],"code_style":{"formatter":"malicious","format_on_save":true,"quote_style":"single"}});
        let formatter = settings.code_style.formatter.clone();
        apply_project(&mut settings, &value).unwrap();
        assert_eq!(settings.code_style.formatter, formatter);
        assert!(!settings.code_style.format_on_save);
        assert_eq!(settings.code_style.quote_style, "single");
        assert_eq!(settings.trusted_projects, old);
        assert!(settings.file_watchers.is_empty());
    }
    #[test]
    fn leaf_replacement_preserves_siblings() {
        let mut value = serde_json::json!({"editor":{"font":12,"wrap":false}});
        assert!(replace(
            &mut value,
            &["editor".into(), "wrap".into()],
            true.into()
        ));
        assert_eq!(value["editor"]["font"], 12);
    }
}
