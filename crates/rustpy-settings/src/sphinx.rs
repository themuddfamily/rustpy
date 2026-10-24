//! Python Integrated Tools: documentation build preferences.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SphinxSettings {
    /// Empty discovers docs/conf.py, doc/conf.py, then conf.py.
    pub working_directory: PathBuf,
    /// Paths below are relative to the working directory unless absolute.
    pub source_directory: PathBuf,
    pub configuration_directory: PathBuf,
    pub output_directory: PathBuf,
    pub builder: String,
    pub warnings_as_errors: bool,
    pub fresh_environment: bool,
    pub additional_arguments: String,
}
impl Default for SphinxSettings {
    fn default() -> Self {
        Self {
            working_directory: PathBuf::new(),
            source_directory: ".".into(),
            configuration_directory: ".".into(),
            output_directory: "_build/html".into(),
            builder: "html".into(),
            warnings_as_errors: false,
            fresh_environment: false,
            additional_arguments: String::new(),
        }
    }
}
impl SphinxSettings {
    pub fn working_directory(&self, root: &Path) -> Option<PathBuf> {
        if !self.working_directory.as_os_str().is_empty() {
            return Some(if self.working_directory.is_absolute() {
                self.working_directory.clone()
            } else {
                root.join(&self.working_directory)
            });
        }
        ["docs", "doc", "."]
            .iter()
            .map(|name| root.join(name))
            .find(|path| path.join("conf.py").is_file())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_round_trip_and_relative_working_directory() {
        let prefs = SphinxSettings {
            working_directory: "documentation with spaces".into(),
            builder: "dirhtml".into(),
            ..Default::default()
        };
        assert_eq!(
            prefs.working_directory(Path::new("project")),
            Some(Path::new("project/documentation with spaces").into())
        );
        assert_eq!(
            serde_json::from_str::<SphinxSettings>(&serde_json::to_string(&prefs).unwrap())
                .unwrap(),
            prefs
        );
    }
}
