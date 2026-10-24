//! Python Console launch and editor preferences, separate from Run configurations.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PythonConsoleSettings {
    pub startup_script: String,
    pub working_directory: Option<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub add_content_roots: bool,
    pub add_source_roots: bool,
    pub use_existing_console: bool,
    pub show_variables: bool,
    pub command_queue: bool,
    pub use_ipython: bool,
    pub interactive: bool,
    pub execute_by_lines: bool,
    pub soft_wrap: bool,
    pub history_limit: usize,
}
impl Default for PythonConsoleSettings {
    fn default() -> Self {
        Self {
            startup_script: "import sys\n# sys.path.extend(['/additional/source/root'])\n".into(),
            working_directory: None,
            environment: BTreeMap::new(),
            add_content_roots: true,
            add_source_roots: true,
            use_existing_console: true,
            show_variables: true,
            command_queue: true,
            use_ipython: true,
            interactive: true,
            execute_by_lines: false,
            soft_wrap: true,
            history_limit: 1000,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn console_preferences_migrate_and_preserve_launch_environment() {
        let mut settings: PythonConsoleSettings = serde_json::from_str("{}").unwrap();
        assert!(settings.use_existing_console && settings.use_ipython && settings.command_queue);
        settings.startup_script = "import project_bootstrap".into();
        settings.environment.insert("CUSTOM".into(), "a=b".into());
        settings.working_directory = Some(PathBuf::from("/workspace/sub"));
        settings.execute_by_lines = true;
        let restored: PythonConsoleSettings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(settings, restored);
    }
}
