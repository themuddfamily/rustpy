//! IDE testing preferences; project roots and SDKs remain in the project model.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewOptions {
    pub show_ignored: bool,
    pub inline_statistics: bool,
    pub track_running: bool,
    pub autoscroll: bool,
    pub select_first_failure: bool,
    pub sort: String,
}
impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            show_ignored: true,
            inline_statistics: true,
            track_running: true,
            autoscroll: true,
            select_first_failure: true,
            sort: "collection".into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TestingSettings {
    /// auto | pytest | unittest | doctest | behave | trial | nose | nose2 | lettuce | tox
    pub default_runner: String,
    pub pytest_path: Option<PathBuf>,
    pub suppress_buffering: bool,
    pub unittest_pattern: String,
    pub unittest_top_level: Option<PathBuf>,
    pub requirements_file: Option<PathBuf>,
    pub pipenv: String,
    pub poetry: String,
    pub uv: String,
    pub notify_detection: bool,
    /// ask | script | tests for files containing both tests and a main guard.
    pub main_conflict: String,
    pub view: ViewOptions,
}
impl Default for TestingSettings {
    fn default() -> Self {
        Self {
            default_runner: "auto".into(),
            pytest_path: None,
            suppress_buffering: false,
            unittest_pattern: "test*.py".into(),
            unittest_top_level: None,
            requirements_file: None,
            pipenv: "pipenv".into(),
            poetry: "poetry".into(),
            uv: "uv".into(),
            notify_detection: true,
            main_conflict: "ask".into(),
            view: ViewOptions::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_get_safe_testing_defaults_and_options_round_trip() {
        let mut settings: TestingSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.default_runner, "auto");
        assert_eq!(settings.main_conflict, "ask");
        settings.default_runner = "unittest".into();
        settings.unittest_pattern = "check_*.py".into();
        settings.uv = "/tools/uv".into();
        settings.view.sort = "duration".into();
        assert_eq!(
            serde_json::from_str::<TestingSettings>(&serde_json::to_string(&settings).unwrap())
                .unwrap(),
            settings
        );
    }
}
