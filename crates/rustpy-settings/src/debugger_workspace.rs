//! Advanced Python debugger preferences shared with the backend.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadingPolicy {
    Synchronous,
    Asynchronous,
    #[default]
    OnDemand,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DebuggerWorkspaceSettings {
    pub breakpoints: Vec<AdvancedBreakpoint>,
    pub sub_process: bool,
    pub qt_support: String,
    pub django: bool,
    pub jinja: bool,
    pub jupyter: bool,
    pub use_cython: bool,
    pub use_frame_eval: bool,
    pub use_monitoring: bool,
    pub loading: LoadingPolicy,
    pub max_children: usize,
    pub show_special: bool,
    pub show_protected: bool,
    pub sort_variables: bool,
    pub enable_repr: bool,
    pub step_filters: Vec<String>,
    pub collect_runtime_types: bool,
    pub drop_failed_tests: bool,
    pub suspend_all_threads: bool,
    pub server_host: String,
    pub server_port: u16,
    pub jump_to_source: bool,
}
impl Default for DebuggerWorkspaceSettings {
    fn default() -> Self {
        Self {
            breakpoints: Vec::new(),
            sub_process: true,
            qt_support: "none".into(),
            django: false,
            jinja: false,
            jupyter: false,
            use_cython: true,
            use_frame_eval: true,
            use_monitoring: true,
            loading: LoadingPolicy::OnDemand,
            max_children: 100,
            show_special: false,
            show_protected: false,
            sort_variables: true,
            enable_repr: true,
            step_filters: Vec::new(),
            collect_runtime_types: false,
            drop_failed_tests: false,
            suspend_all_threads: true,
            server_host: "127.0.0.1".into(),
            server_port: 5678,
            jump_to_source: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuspendPolicy {
    #[default]
    All,
    Thread,
    None,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BreakpointMetadata {
    pub group: String,
    pub description: String,
    pub suspend_policy: SuspendPolicy,
    pub log_stack: bool,
    pub depends_on: Option<(std::path::PathBuf, usize)>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvancedBreakpoint {
    pub path: std::path::PathBuf,
    pub line: usize,
    pub metadata: BreakpointMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_advanced_preferences_preserve_safe_defaults() {
        let settings: DebuggerWorkspaceSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, DebuggerWorkspaceSettings::default());
        assert_eq!(settings.max_children, 100);
        assert_eq!(settings.loading, LoadingPolicy::OnDemand);
    }
    #[test]
    fn dependency_and_group_survive_workspace_roundtrip() {
        let metadata = BreakpointMetadata {
            group: "HTTP".into(),
            description: "after authentication".into(),
            suspend_policy: SuspendPolicy::Thread,
            log_stack: true,
            depends_on: Some(("server.py".into(), 17)),
        };
        let json = serde_json::to_string(&metadata).unwrap();
        let restored: BreakpointMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, metadata);
    }
}
