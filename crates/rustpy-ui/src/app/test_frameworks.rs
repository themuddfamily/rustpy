//! Python Integrated Tools policy shared by direct/context/configuration runs.
use super::*;
use rustpy_test::{Framework, TestRunOptions};
impl IdeApp {
    pub(super) fn test_generation_settings(&mut self, ui: &mut egui::Ui) {
        let settings = &mut self.settings.python_testing;
        ui.label("Additional pytest arguments");
        ui.text_edit_singleline(&mut settings.pytest_arguments);
        ui.label("New test file naming pattern ({module})");
        ui.text_edit_singleline(&mut settings.test_file_pattern);
        ui.label("New test class name");
        ui.text_edit_singleline(&mut settings.test_class_name);
        ui.label("New test method prefix");
        ui.text_edit_singleline(&mut settings.test_method_prefix);
        ui.checkbox(&mut settings.setup, "Generate setup method");
        ui.checkbox(&mut settings.teardown, "Generate teardown method");
    }
    pub(super) fn preferred_test_framework(&self, root: &Path) -> Option<Framework> {
        let _ = root;
        Framework::from_key(&self.settings.testing.default_runner)
            .or_else(|| Framework::from_key(&self.settings.python_testing.runner))
    }
    pub(super) fn process_test_preferences(
        &self,
        mut configuration: RunConfig,
        framework: Framework,
    ) -> RunConfig {
        let mut arguments = RunConfig::split_args(&configuration.parameters);
        let mut options = TestRunOptions {
            inherit_environment: configuration.workflow.inherit_environment,
            env: configuration
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            ..Default::default()
        };
        let targets = match &configuration.kind {
            RunConfigKind::Unittest { target } | RunConfigKind::Pytest { target, .. }
                if !target.is_empty() =>
            {
                vec![target.clone()]
            }
            _ => Vec::new(),
        };
        self.configure_test_tools(framework, &targets, &mut arguments, &mut options);
        self.apply_test_preferences(framework, &mut arguments, &mut options);
        configuration.parameters = crate::run_anything::quote_args(&arguments);
        configuration.env = options.env.into_iter().collect();
        configuration
    }
    pub(super) fn apply_test_preferences(
        &self,
        framework: Framework,
        args: &mut Vec<String>,
        options: &mut TestRunOptions,
    ) {
        let preferences = &self.settings.python_testing;
        if framework == Framework::Pytest {
            let mut defaults = RunConfig::split_args(&preferences.pytest_arguments);
            defaults.append(args);
            *args = defaults;
            if !preferences.pytest_path.trim().is_empty()
                && !options
                    .env
                    .iter()
                    .any(|(key, _)| key == "RUSTPY_PYTEST_PATH")
            {
                options
                    .env
                    .push(("RUSTPY_PYTEST_PATH".into(), preferences.pytest_path.clone()));
            }
        }
    }
}
/// Customize generated declarations without changing the selected subject set.
pub(super) fn customize_test_source(
    mut source: String,
    framework: &str,
    class_name: &str,
    setup: bool,
    teardown: bool,
) -> Result<String, String> {
    if framework == "doctest" {
        return Ok(source);
    }
    if framework == "trial" {
        source = source.replace("import unittest", "from twisted.trial import unittest");
    }
    if !class_name.is_empty() {
        if !rustpy_syntax::parse(&format!("class {class_name}:\n    pass\n"))
            .errors()
            .is_empty()
        {
            return Err("Choose a valid test class name".into());
        }
        if let Some(start) = source.find("class ") {
            let name_start = start + 6;
            let end = source[name_start..]
                .find(['(', ':'])
                .map(|end| name_start + end)
                .ok_or("Invalid generated class")?;
            source.replace_range(name_start..end, class_name);
        }
    }
    let pytest = framework == "pytest";
    let hooks = if source.contains("class ") {
        let mut result = String::new();
        if setup {
            result.push_str(if pytest {
                "    def setup_method(self):\n        pass\n\n"
            } else {
                "    def setUp(self):\n        pass\n\n"
            });
        }
        if teardown {
            result.push_str(if pytest {
                "    def teardown_method(self):\n        pass\n\n"
            } else {
                "    def tearDown(self):\n        pass\n\n"
            });
        }
        if let Some(start) = source.find("class ") {
            if let Some(newline) = source[start..].find('\n') {
                source.insert_str(start + newline + 1, &result);
            }
        }
        String::new()
    } else {
        let mut result = String::new();
        if setup {
            result.push_str("def setup_function():\n    pass\n\n");
        }
        if teardown {
            result.push_str("def teardown_function():\n    pass\n\n");
        }
        result
    };
    source.push_str(&hooks);
    if !rustpy_syntax::parse(&source).errors().is_empty() {
        return Err("Generated test source is invalid".into());
    }
    Ok(source)
}

pub(super) fn doctest_source(
    module: &str,
    subjects: &[rustpy_refactor::generation::TestSubject],
) -> Result<String, String> {
    if subjects.is_empty() {
        return Err("Select a function or method to test".into());
    }
    let mut text =
        String::from("\"\"\"Examples. Replace expected results with the intended behavior.\n\n");
    for subject in subjects {
        let name = subject
            .class
            .as_ref()
            .or(subject.function.as_ref())
            .ok_or("Invalid test subject")?;
        text.push_str(&format!(">>> from {module} import {name}\n"));
        let expression = if let Some(method) = &subject.method {
            format!("{name}().{method}()")
        } else {
            format!("{name}()")
        };
        text.push_str(&format!(">>> {expression}\n<expected result>\n\n"));
    }
    text.push_str("\"\"\"\n");
    if !rustpy_syntax::parse(&text).errors().is_empty() {
        return Err("Selected subjects cannot form a doctest module".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn create_test_custom_class_and_fixture_methods_follow_framework() {
        let source = "import unittest\n\nclass TestOriginal(unittest.TestCase):\n    def test_selected(self):\n        self.fail('TODO')\n";
        let source =
            customize_test_source(source.into(), "trial", "ServiceTests", true, true).unwrap();
        assert!(source.contains("from twisted.trial import unittest"));
        assert!(source.contains("class ServiceTests(unittest.TestCase)"));
        assert!(source.contains("def setUp(self)"));
        assert!(source.contains("def tearDown(self)"));
        assert!(source.contains("def test_selected(self)"));
        assert!(customize_test_source(source, "unittest", "not a class", false, false).is_err());
    }
    #[test]
    fn create_doctest_captures_selected_method_in_owned_docstring() {
        let subject = rustpy_refactor::generation::TestSubject {
            key: "Service.selected".into(),
            class: Some("Service".into()),
            method: Some("selected".into()),
            function: None,
        };
        let source = doctest_source("package.subject", &[subject]).unwrap();
        let parse = rustpy_syntax::parse(&source);
        let examples = rustpy_syntax::doctest::discover(&source, &parse);
        assert_eq!(examples.len(), 1);
        assert!(source.contains(">>> Service().selected()"));
    }
}
