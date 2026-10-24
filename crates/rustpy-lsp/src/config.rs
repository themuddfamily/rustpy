//! Server configuration: the IDE's own settings (`settings.json`, read-only)
//! with the overrides an editor passes in `initializationOptions` or
//! `workspace/didChangeConfiguration`.
//!
//! ```json
//! {
//!   "interpreter": "/path/to/python",       // or "none"; omitted: discover like the IDE
//!   "settings": { "inspections": { "spelling": false }, "editor": { "right_margin": 100 } },
//!   "autoImportCompletions": true,
//!   "liveTemplates": true,
//!   "diagnosticsDelay": 300
//! }
//! ```
//!
//! `settings` uses the keys of the IDE's `settings.json` and is merged over it.

use std::path::PathBuf;
use std::time::Duration;

use rustpy_settings::Settings;
use serde_json::Value;

/// Which Python interpreter feeds module resolution (site-packages, version).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterpreterChoice {
    /// Discover interpreters like the IDE does (the project's choice from
    /// `.rustpy/workspace.json`, then a virtual environment, then the first found).
    Discover,
    /// An interpreter configured in the editor.
    Path(PathBuf),
    /// No interpreter: the bundled stdlib stubs only.
    Disabled,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// The effective IDE settings (inspections, code style, completion, inlay hints, …).
    pub settings: Settings,
    pub interpreter: InterpreterChoice,
    /// Offer names from other modules (with an import) in completion.
    pub auto_import_completions: bool,
    /// Offer live templates as snippets in completion.
    pub live_templates: bool,
    /// How long diagnostics wait after the last change (the IDE waits 0.3 s).
    pub diagnostics_delay: Duration,
}

impl Config {
    pub fn new(base: &Settings, options: Option<&Value>) -> Config {
        let mut cfg = Config {
            settings: base.clone(),
            interpreter: InterpreterChoice::Discover,
            auto_import_completions: true,
            live_templates: true,
            diagnostics_delay: Duration::from_millis(300),
        };
        if let Some(opts) = options.filter(|v| v.is_object()) {
            cfg.apply(base, opts);
        }
        cfg
    }

    fn apply(&mut self, base: &Settings, opts: &Value) {
        if let Some(overrides) = opts.get("settings").filter(|v| v.is_object()) {
            match serde_json::to_value(base) {
                Ok(mut merged) => {
                    merge(&mut merged, overrides);
                    match serde_json::from_value::<Settings>(merged) {
                        Ok(s) => self.settings = s,
                        Err(e) => log::warn!("lsp: ignoring invalid settings overrides: {e}"),
                    }
                }
                Err(e) => log::warn!("lsp: cannot apply settings overrides: {e}"),
            }
        }
        match opts.get("interpreter") {
            Some(Value::String(s)) if s.is_empty() || s.eq_ignore_ascii_case("none") => {
                self.interpreter = InterpreterChoice::Disabled
            }
            Some(Value::String(s)) => self.interpreter = InterpreterChoice::Path(PathBuf::from(s)),
            Some(Value::Bool(false)) => self.interpreter = InterpreterChoice::Disabled,
            _ => {}
        }
        if let Some(b) = opts.get("autoImportCompletions").and_then(Value::as_bool) {
            self.auto_import_completions = b;
        }
        if let Some(b) = opts.get("liveTemplates").and_then(Value::as_bool) {
            self.live_templates = b;
        }
        if let Some(ms) = opts.get("diagnosticsDelay").and_then(Value::as_u64) {
            self.diagnostics_delay = Duration::from_millis(ms.min(10_000));
        }
    }

    /// The options of a `workspace/didChangeConfiguration` notification: the
    /// `rustpy` section when there is one, else the whole object.
    pub fn section(settings: &Value) -> Option<&Value> {
        settings
            .get("rustpy")
            .or(Some(settings))
            .filter(|v| v.is_object())
    }

    pub fn inspection_options(
        &self,
        project_symbols: Vec<(String, String)>,
        python_version: (u32, u32),
    ) -> rustpy_analysis::inspections::InspectionOptions {
        let i = &self.settings.inspections;
        rustpy_analysis::inspections::InspectionOptions {
            max_line_length: self.settings.editor.right_margin,
            editorconfig_enabled: self.settings.editor.editorconfig_enabled,
            style: {
                let mut style = self.settings.inspections.style.clone();
                if style.quote_style == "code_style" {
                    style.quote_style = self.settings.code_style.quote_style.clone()
                }
                style
            },
            pep8: i.pep8,
            unused_imports: i.unused_imports,
            unused_variables: i.unused_variables,
            project_symbols: project_symbols.into(),
            type_checker: i.type_checker,
            security: i.security,
            compatibility: i.compatibility,
            code_smells: i.code_smells,
            naming: i.naming,
            missing_docstrings: i.missing_docstrings,
            python_version,
            ..Default::default()
        }
    }

    pub fn completion_settings(&self) -> rustpy_semantic::completion2::CompletionSettings {
        let c = &self.settings.completion;
        rustpy_semantic::completion2::CompletionSettings {
            show_as_you_type: c.show_as_you_type,
            case_sensitive: c.case_sensitive,
            match_middle: c.match_middle,
            hide_used: c.hide_used,
        }
    }

    /// Settings › Tools › Python Integrated Tools › Docstring format for a file.
    pub fn docstring_format(&self, text: &str) -> rustpy_semantic::docstrings::DocstringFormat {
        use rustpy_semantic::docstrings::DocstringFormat as F;
        match self.settings.docstring_format.as_str() {
            "plain" => F::Plain,
            "rest" => F::ReStructuredText,
            "epytext" => F::Epytext,
            "google" => F::Google,
            "numpy" => F::NumPy,
            _ => match rustpy_semantic::docstrings::detect(text) {
                F::Plain => F::ReStructuredText,
                f => f,
            },
        }
    }
}

/// Reads the IDE's `settings.json` without side effects (an invalid file is
/// only logged; the IDE itself keeps a backup of it when it starts).
pub fn load_ide_settings() -> Settings {
    let path = Settings::settings_path();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Settings>(&bytes).unwrap_or_else(|e| {
            log::warn!(
                "lsp: cannot read {}: {e}; using default settings",
                path.display()
            );
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

/// Deep-merges `overrides` into `base` (objects key by key, anything else replaced).
pub fn merge(base: &mut Value, overrides: &Value) {
    match (base, overrides) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overrides_merge_over_ide_settings() {
        let mut base = Settings::default();
        base.inspections.spelling = true;
        base.editor.right_margin = 120;
        let cfg = Config::new(
            &base,
            Some(
                &json!({"settings": {"inspections": {"spelling": false}}, "interpreter": "none", "diagnosticsDelay": 50}),
            ),
        );
        assert!(!cfg.settings.inspections.spelling);
        assert!(cfg.settings.inspections.pep8 == base.inspections.pep8);
        assert_eq!(cfg.settings.editor.right_margin, 120);
        assert_eq!(cfg.interpreter, InterpreterChoice::Disabled);
        assert_eq!(cfg.diagnostics_delay, Duration::from_millis(50));
        let cfg = Config::new(&base, Some(&json!({"interpreter": "/usr/bin/python3"})));
        assert_eq!(
            cfg.interpreter,
            InterpreterChoice::Path("/usr/bin/python3".into())
        );
        // A wrong type keeps the IDE settings.
        let cfg = Config::new(
            &base,
            Some(&json!({"settings": {"editor": {"right_margin": "wide"}}})),
        );
        assert_eq!(cfg.settings.editor.right_margin, 120);
    }
}
