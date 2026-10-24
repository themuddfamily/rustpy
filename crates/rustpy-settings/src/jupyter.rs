//! Jupyter presentation and explicit connection defaults; credentials stay with the notebook owner.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub default_kernel: String,
    pub connection_kind: String,
    pub server_url: String,
    pub verify_ssl: bool,
    pub ca_file: String,
    pub ssh_host: String,
    pub docker_container: String,
    pub remote_python: String,
    pub debug_host: String,
    pub debug_bind: String,
    pub debug_port: u16,
    pub remote_cwd: String,
    pub show_line_numbers: bool,
    pub autoscroll: bool,
    pub collapse_outputs: bool,
    pub max_output_bytes: usize,
    pub markdown_preview: bool,
    pub plots: bool,
    pub split_console: bool,
    pub clear_outputs_before_commit: bool,
    pub trusted: std::collections::BTreeMap<String, String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            default_kernel: "python3".into(),
            connection_kind: "embedded".into(),
            server_url: String::new(),
            verify_ssl: true,
            ca_file: String::new(),
            ssh_host: String::new(),
            docker_container: String::new(),
            remote_python: String::new(),
            debug_host: String::new(),
            debug_bind: "127.0.0.1".into(),
            debug_port: 0,
            remote_cwd: String::new(),
            show_line_numbers: true,
            autoscroll: true,
            collapse_outputs: false,
            max_output_bytes: 8 * 1024 * 1024,
            markdown_preview: true,
            plots: true,
            split_console: false,
            clear_outputs_before_commit: false,
            trusted: Default::default(),
        }
    }
}
