//! Persisted terminal layout and connection preferences. Authentication stays in the PTY/SSH agent.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    /// local, ssh, wsl, docker, compose, vagrant
    pub target_kind: String,
    pub target: String,
    pub shell: String,
    pub arguments: Vec<String>,
    pub directory: String,
    pub ssh_port: u16,
    pub ssh_user: String,
    pub ssh_key: String,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "Local".into(),
            target_kind: "local".into(),
            target: String::new(),
            shell: String::new(),
            arguments: Vec::new(),
            directory: String::new(),
            ssh_port: 22,
            ssh_user: String::new(),
            ssh_key: String::new(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTab {
    pub title: String,
    pub cwd: PathBuf,
    pub local_directory: PathBuf,
    pub profile: String,
    pub group: u64,
    pub vertical: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSessions {
    pub root: PathBuf,
    pub tabs: Vec<SavedTab>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub arguments: Vec<String>,
    pub start_directory: String,
    pub environment: BTreeMap<String, String>,
    pub tab_name: String,
    pub scrollback: usize,
    pub close_on_exit: bool,
    pub copy_on_selection: bool,
    pub paste_on_middle: bool,
    pub mouse_reporting: bool,
    pub audible_bell: bool,
    pub visual_bell: bool,
    pub cursor: String,
    pub cursor_blink: bool,
    pub option_as_meta: bool,
    pub highlight_links: bool,
    pub osc52_clipboard: bool,
    pub shell_integration: bool,
    pub engine: String,
    pub block_separators: bool,
    pub prompt_prefix: String,
    pub smart_commands: bool,
    pub restore_sessions: bool,
    pub override_shortcuts: Vec<String>,
    /// Escape moves the keyboard from the terminal to the editor instead of
    /// being sent to the shell ("Move focus to the editor with Escape").
    pub escape_focuses_editor: bool,
    pub profiles: Vec<Profile>,
    pub selected_profile: String,
    pub sessions: Vec<ProjectSessions>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            arguments: Vec::new(),
            start_directory: String::new(),
            environment: BTreeMap::new(),
            tab_name: "Local".into(),
            scrollback: 10_000,
            close_on_exit: false,
            copy_on_selection: false,
            paste_on_middle: cfg!(target_os = "linux"),
            mouse_reporting: true,
            audible_bell: false,
            visual_bell: true,
            cursor: "block".into(),
            cursor_blink: true,
            option_as_meta: true,
            highlight_links: true,
            osc52_clipboard: false,
            shell_integration: true,
            engine: "new".into(),
            block_separators: true,
            prompt_prefix: "❯ ".into(),
            smart_commands: true,
            restore_sessions: true,
            override_shortcuts: vec![
                "Ctrl+F".into(),
                "Ctrl+C".into(),
                "Ctrl+V".into(),
                "Ctrl+L".into(),
                "Ctrl+R".into(),
            ],
            escape_focuses_editor: true,
            profiles: vec![Profile::default()],
            selected_profile: "Local".into(),
            sessions: Vec::new(),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_roundtrip_retains_split_groups_and_remote_cwds_without_credentials() {
        let p = Preferences {
            sessions: vec![ProjectSessions {
                root: "/project".into(),
                tabs: vec![SavedTab {
                    title: "remote".into(),
                    cwd: "/srv/work".into(),
                    local_directory: "/project".into(),
                    profile: "SSH".into(),
                    group: 42,
                    vertical: true,
                }],
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        let restored: Preferences = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, p);
        assert!(!json.contains("password"));
        assert!(!json.contains("passphrase"));
    }
    #[test]
    fn partial_settings_migrate_with_clipboard_opt_in() {
        let prefs: Preferences = serde_json::from_str(r#"{"scrollback":20000}"#).unwrap();
        assert!(!prefs.osc52_clipboard);
        assert!(prefs.restore_sessions);
        assert_eq!(prefs.scrollback, 20000);
    }
}
