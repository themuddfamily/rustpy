//! External tool execution and placement preferences; SSH uses agent/key/config authentication.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub main_menu: bool,
    pub editor_menu: bool,
    pub project_menu: bool,
    pub synchronize: bool,
    pub allow_parallel: bool,
    pub inherit_environment: bool,
    pub environment: BTreeMap<String, String>,
    pub output_filters: Vec<String>,
    pub ssh: Option<Ssh>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            main_menu: true,
            editor_menu: true,
            project_menu: true,
            synchronize: false,
            allow_parallel: true,
            inherit_environment: true,
            environment: BTreeMap::new(),
            output_filters: vec![],
            ssh: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ssh {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_file: Option<PathBuf>,
    pub ssh_program: String,
    /// Explicit local OpenSSH configuration; None keeps normal SSH config lookup.
    pub ssh_config_file: Option<PathBuf>,
    pub remote_project_root: String,
    pub remote_interpreter: String,
    pub mappings: Vec<PathMapping>,
}
impl Default for Ssh {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            user: String::new(),
            key_file: None,
            ssh_program: "ssh".into(),
            ssh_config_file: None,
            remote_project_root: String::new(),
            remote_interpreter: String::new(),
            mappings: vec![],
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PathMapping {
    pub local: PathBuf,
    pub remote: String,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_tools_preserve_all_menu_placements() {
        let tool: crate::ExternalTool =
            serde_json::from_str(r#"{"name":"Old","program":"echo"}"#).unwrap();
        assert!(tool.options.main_menu && tool.options.editor_menu && tool.options.project_menu);
        assert!(tool.options.ssh.is_none());
        assert!(!tool.options.synchronize);
    }
    #[test]
    fn ssh_settings_do_not_store_passwords() {
        let options = Options {
            ssh: Some(Ssh {
                host: "build.example".into(),
                ssh_config_file: Some("/private/ssh/config".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let encoded = serde_json::to_string(&options).unwrap();
        assert!(!encoded.contains("password"));
        assert_eq!(serde_json::from_str::<Options>(&encoded).unwrap(), options);
    }
}
