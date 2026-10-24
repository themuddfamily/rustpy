//! Python Integrated Tools testing preferences; no interpreter is run to edit them.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PythonTestingSettings {
    pub runner: String,
    pub pytest_arguments: String,
    pub pytest_path: String,
    pub unittest_pattern: String,
    pub test_file_pattern: String,
    pub test_class_name: String,
    pub test_method_prefix: String,
    pub setup: bool,
    pub teardown: bool,
}
impl Default for PythonTestingSettings {
    fn default() -> Self {
        Self {
            runner: "auto".into(),
            pytest_arguments: String::new(),
            pytest_path: String::new(),
            unittest_pattern: "test*.py".into(),
            test_file_pattern: "test_{module}.py".into(),
            test_class_name: "TestSubject".into(),
            test_method_prefix: "test_".into(),
            setup: false,
            teardown: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_settings_keep_safe_naming_and_runner_defaults() {
        let settings: PythonTestingSettings = serde_json::from_str(
            r#"{"runner":"nose2","pytest_arguments":"-m slow --lf -n 2 --cov=subject"}"#,
        )
        .unwrap();
        assert_eq!(settings.runner, "nose2");
        assert_eq!(settings.test_file_pattern, "test_{module}.py");
        assert_eq!(settings.test_method_prefix, "test_");
        assert_eq!(settings.unittest_pattern, "test*.py");
        assert_eq!(
            serde_json::from_str::<PythonTestingSettings>(
                &serde_json::to_string(&settings).unwrap()
            )
            .unwrap(),
            settings
        );
    }
}
