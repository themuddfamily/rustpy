//! Additional actions share the prepared, checked save transaction.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebTool {
    pub enabled: bool,
    /// Empty uses the project-local node_modules/.bin tool, then PATH.
    pub executable: String,
    pub arguments: String,
    pub masks: String,
}
impl Default for WebTool {
    fn default() -> Self {
        Self {
            enabled: false,
            executable: String::new(),
            arguments: String::new(),
            masks: "*.js;*.jsx;*.mjs;*.cjs;*.ts;*.tsx;*.vue;*.svelte".into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub masks: String,
    pub on_auto_save: bool,
    pub update_copyright: bool,
    pub run_file_watchers: bool,
    pub upload: bool,
    pub prettier: WebTool,
    pub eslint: WebTool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            masks: "*".into(), on_auto_save: true, update_copyright: false,
            run_file_watchers: true, upload: true,
            prettier: WebTool { masks: "*.js;*.jsx;*.mjs;*.cjs;*.ts;*.tsx;*.vue;*.svelte;*.html;*.css;*.scss;*.less;*.json;*.yaml;*.yml;*.md;*.markdown".into(), ..WebTool::default() },
            eslint: WebTool::default(),
        }
    }
}
