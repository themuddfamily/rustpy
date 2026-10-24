//! Persistent watcher definitions; invocation and filesystem observation live in the UI runtime.
use crate::scopes::{PredefinedScope, ScopeSelection};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Level {
    Project,
    #[default]
    Global,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileWatcher {
    pub name: String,
    pub pattern: String,
    /// Language/file type name, or Any. The filename mask is an additional restriction.
    pub file_type: String,
    pub scope: ScopeSelection,
    pub program: String,
    pub arguments: String,
    pub working_dir: String,
    /// Newline-separated macro templates, relative to the invocation directory.
    pub output_paths: String,
    pub enabled: bool,
    pub auto_save: bool,
    pub external_changes: bool,
    pub show_console: bool,
    pub debounce_ms: u64,
    pub level: Level,
}
impl Default for FileWatcher {
    fn default() -> Self {
        Self {
            name: String::new(),
            pattern: "*".into(),
            file_type: "Any".into(),
            scope: ScopeSelection::Predefined(PredefinedScope::Project),
            program: String::new(),
            arguments: String::new(),
            working_dir: "$ProjectFileDir$".into(),
            output_paths: String::new(),
            enabled: false,
            auto_save: false,
            external_changes: false,
            show_console: false,
            debounce_ms: 400,
            level: Level::Global,
        }
    }
}
impl FileWatcher {
    pub const TEMPLATES: &[&str] = &[
        "Black", "isort", "Babel", "SCSS", "Less", "Prettier", "Custom",
    ];
    pub fn template(name: &str) -> Self {
        let mut w = Self {
            name: name.into(),
            ..Self::default()
        };
        let (kind, mask, program, args, outputs) = match name {
            "Black" => (
                "Python",
                "*.py",
                "python3",
                "-m black -q $FilePath$",
                "$FilePath$",
            ),
            "isort" => (
                "Python",
                "*.py",
                "python3",
                "-m isort $FilePath$",
                "$FilePath$",
            ),
            "Babel" => (
                "JavaScript",
                "*.js",
                "babel",
                "$FilePath$ --out-file $FileDir$/$FileNameWithoutExtension$.compiled.js",
                "$FileDir$/$FileNameWithoutExtension$.compiled.js",
            ),
            "SCSS" => (
                "SCSS",
                "*.scss",
                "sass",
                "$FilePath$ $FileDir$/$FileNameWithoutExtension$.css",
                "$FileDir$/$FileNameWithoutExtension$.css\n$FileDir$/$FileNameWithoutExtension$.css.map",
            ),
            "Less" => (
                "Less",
                "*.less",
                "lessc",
                "$FilePath$ $FileDir$/$FileNameWithoutExtension$.css",
                "$FileDir$/$FileNameWithoutExtension$.css",
            ),
            "Prettier" => ("Any", "*", "prettier", "--write $FilePath$", "$FilePath$"),
            _ => ("Any", "*", "", "$FilePath$", ""),
        };
        w.enabled = !program.is_empty();
        w.file_type = kind.into();
        w.pattern = mask.into();
        w.program = program.into();
        w.arguments = args.into();
        w.output_paths = outputs.into();
        w
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_definitions_gain_conservative_triggers() {
        let w:FileWatcher=serde_json::from_str(r#"{"name":"legacy","pattern":"*.py","program":"python","arguments":"-m black $FilePath$","enabled":true}"#).unwrap();
        assert!(!w.auto_save && !w.external_changes && !w.show_console);
        assert_eq!(w.level, Level::Global);
    }
    #[test]
    fn compiler_templates_declare_distinct_outputs() {
        for name in ["Babel", "SCSS", "Less"] {
            let w = FileWatcher::template(name);
            assert!(w.arguments.contains("$FilePath$"));
            assert!(!w.output_paths.is_empty());
            assert_ne!(w.output_paths, "$FilePath$");
        }
    }
}
