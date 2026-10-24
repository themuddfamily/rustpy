//! Settings › Appearance & Behavior › Path Variables: IDE-level names for
//! folders (`$NAME$`). Project files under `.rustpy/` store paths below such a
//! folder as `$NAME$/rest`, so a project shared between machines (or users)
//! keeps working when the folder lives elsewhere; run configurations and
//! external tools expand `$NAME$` too. `$USER_HOME$` is built in and read-only;
//! `$PROJECT_DIR$` is the project root wherever a project is known.
//!
//! The variables of the IDE settings are installed process-wide (see
//! [`install`]) by [`crate::Settings::load`] and [`crate::Settings::save`], so
//! code that reads or writes project files without the settings at hand (the
//! project model, run configurations) uses the user's current definitions.

use std::path::{Component, Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// Built-in: the user's home directory.
pub const USER_HOME: &str = "USER_HOME";
/// Built-in: the root of the open project.
pub const PROJECT_DIR: &str = "PROJECT_DIR";
/// Names the IDE defines itself; they cannot be redefined.
pub const RESERVED: &[&str] = &[USER_HOME, PROJECT_DIR, "MODULE_DIR", "APPLICATION_HOME_DIR"];

/// One user-defined variable.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PathVariable {
    pub name: String,
    pub path: PathBuf,
}

/// Settings › Appearance & Behavior › Path Variables.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PathVariables {
    /// User-defined variables, in the order shown on the page.
    pub variables: Vec<PathVariable>,
    /// Names never reported as undefined ("Ignored variables"): `$NAME$`
    /// tokens that are not path variables, like a tool's own placeholders.
    pub ignored: Vec<String>,
}

/// The user's home directory (`$USER_HOME$`).
pub fn user_home() -> Option<PathBuf> {
    dirs::home_dir().filter(|home| home.is_absolute())
}

/// Whether `name` can name a variable: ASCII letters, digits and `_`, not
/// starting with a digit (`$NAME$` must stay a single token).
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The name of the `$NAME$` token `text` starts with, when the token is a
/// whole path component (followed by nothing, `/` or `\`).
pub fn leading_variable(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('$')?;
    let end = rest.find('$')?;
    let name = &rest[..end];
    let after = &rest[end + 1..];
    (is_valid_name(name) && (after.is_empty() || after.starts_with(['/', '\\']))).then_some(name)
}

fn depth(path: &Path) -> usize {
    path.components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count()
}

impl PathVariables {
    /// The problem that keeps these variables from being saved, if any.
    pub fn validate(&self) -> Result<(), String> {
        for (i, var) in self.variables.iter().enumerate() {
            let name = var.name.as_str();
            if !is_valid_name(name) {
                return Err(format!(
                    "Path variable name \"{name}\" is not valid: use letters, digits and _, not starting with a digit"
                ));
            }
            if RESERVED.contains(&name) {
                return Err(format!(
                    "${name}$ is defined by the IDE and cannot be redefined"
                ));
            }
            if self.variables[..i].iter().any(|other| other.name == name) {
                return Err(format!("Path variable ${name}$ is defined twice"));
            }
            if var.path.as_os_str().is_empty() {
                return Err(format!("Path variable ${name}$ has no folder"));
            }
            if !var.path.is_absolute() {
                return Err(format!("The folder of ${name}$ must be an absolute path"));
            }
        }
        for name in &self.ignored {
            if !is_valid_name(name) {
                return Err(format!("Ignored variable \"{name}\" is not a valid name"));
            }
        }
        Ok(())
    }

    /// The folder `name` stands for: user-defined variables, `$USER_HOME$` and,
    /// with a `project`, `$PROJECT_DIR$`.
    pub fn value(&self, name: &str, project: Option<&Path>) -> Option<PathBuf> {
        match name {
            PROJECT_DIR => project.map(Path::to_path_buf),
            USER_HOME => user_home(),
            _ => self
                .variables
                .iter()
                .find(|v| v.name == name && !v.path.as_os_str().is_empty())
                .map(|v| v.path.clone()),
        }
    }

    /// Whether `name` is defined (or deliberately ignored), i.e. not reported
    /// as undefined.
    pub fn is_known(&self, name: &str) -> bool {
        RESERVED.contains(&name)
            || self.ignored.iter().any(|i| i == name)
            || self.variables.iter().any(|v| v.name == name)
    }

    /// Every variable that can stand for a folder, with that folder.
    fn candidates(&self, project: Option<&Path>) -> Vec<(&str, PathBuf)> {
        let mut out: Vec<(&str, PathBuf)> = self
            .variables
            .iter()
            .filter(|v| v.path.is_absolute() && is_valid_name(&v.name))
            .map(|v| (v.name.as_str(), v.path.clone()))
            .collect();
        out.extend(user_home().map(|home| (USER_HOME, home)));
        out.extend(project.map(|p| (PROJECT_DIR, p.to_path_buf())));
        // A folder that is the file-system root would match everything.
        out.retain(|(_, path)| depth(path) > 0);
        out
    }

    /// `path` written with the variable of the deepest folder that contains
    /// it (`$NAME$/rest`, `/` separators), together with that folder's depth
    /// (its number of named components), or `None` when no variable applies.
    pub fn collapse(&self, path: &Path, project: Option<&Path>) -> Option<(String, usize)> {
        if !path.is_absolute() {
            return None;
        }
        let (name, folder) = self
            .candidates(project)
            .into_iter()
            .filter(|(_, folder)| path.starts_with(folder))
            .max_by_key(|(name, folder)| (depth(folder), *name == PROJECT_DIR))?;
        let rest = path.strip_prefix(&folder).ok()?;
        let rest = rest
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let text = if rest.is_empty() {
            format!("${name}$")
        } else {
            format!("${name}$/{rest}")
        };
        Some((text, depth(&folder)))
    }

    /// `stored` with its leading `$NAME$` replaced by the folder, or `None`
    /// when it does not start with a defined variable.
    pub fn expand_path(&self, stored: &str, project: Option<&Path>) -> Option<PathBuf> {
        let name = leading_variable(stored)?;
        let mut out = self.value(name, project)?;
        let rest = &stored[name.len() + 2..];
        for part in rest.split(['/', '\\']).filter(|p| !p.is_empty()) {
            out.push(part);
        }
        Some(out)
    }

    /// `text` with every defined `$NAME$` replaced by its folder; other
    /// `$…$` text stays as written.
    pub fn expand(&self, text: &str, project: Option<&Path>) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find('$') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            let name_len = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            if name_len > 0 && after[name_len..].starts_with('$') {
                if let Some(folder) = self.value(&after[..name_len], project) {
                    out.push_str(&folder.to_string_lossy());
                    rest = &after[name_len + 1..];
                    continue;
                }
            }
            out.push('$');
            rest = after;
        }
        out.push_str(rest);
        out
    }

    /// Writes every absolute path string of a project file `value` with the
    /// deepest variable folder that contains it (see [`Self::collapse`]).
    pub fn collapse_json(&self, value: &mut serde_json::Value, project: Option<&Path>) {
        self.walk_strings(value, &mut |s| {
            self.collapse(Path::new(s.as_str()), project)
                .map(|(collapsed, _)| collapsed)
        });
    }

    /// Reverses [`Self::collapse_json`]: strings that start with a defined
    /// `$NAME$` become absolute paths. Undefined variables stay as written
    /// (see [`Self::undefined_in_json`]).
    pub fn expand_json(&self, value: &mut serde_json::Value, project: Option<&Path>) {
        self.walk_strings(value, &mut |s| {
            self.expand_path(s, project)
                .map(|p| p.to_string_lossy().into_owned())
        });
    }

    fn walk_strings(
        &self,
        value: &mut serde_json::Value,
        f: &mut dyn FnMut(&String) -> Option<String>,
    ) {
        match value {
            serde_json::Value::String(s) => {
                if let Some(new) = f(s) {
                    *s = new;
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    self.walk_strings(item, f);
                }
            }
            serde_json::Value::Object(map) => {
                for item in map.values_mut() {
                    self.walk_strings(item, f);
                }
            }
            _ => {}
        }
    }

    /// Names of the path-like `$NAME$` tokens (at the start of a string, as a
    /// whole path component) in `value` that are neither defined nor ignored,
    /// skipping the names `other_macro` claims (run-configuration macros like
    /// `$FilePath$`). Sorted, without duplicates.
    pub fn undefined_in_json(
        &self,
        value: &serde_json::Value,
        other_macro: &dyn Fn(&str) -> bool,
    ) -> Vec<String> {
        let mut out = Vec::new();
        let mut copy = value.clone();
        self.walk_strings(&mut copy, &mut |s| {
            if let Some(name) = leading_variable(s) {
                if !self.is_known(name) && !other_macro(name) {
                    out.push(name.to_string());
                }
            }
            None
        });
        out.sort();
        out.dedup();
        out
    }
}

static ACTIVE: RwLock<Option<PathVariables>> = RwLock::new(None);

/// Makes `vars` the definitions [`active`] returns (the IDE settings do this
/// when they are loaded or saved).
pub fn install(vars: &PathVariables) {
    if let Ok(mut active) = ACTIVE.write() {
        *active = Some(vars.clone());
    }
}

/// The definitions last [`install`]ed; only the built-in variables before that.
pub fn active() -> PathVariables {
    ACTIVE
        .read()
        .ok()
        .and_then(|active| active.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{}", p.replace('/', "\\")))
        } else {
            PathBuf::from(p)
        }
    }

    fn vars() -> PathVariables {
        PathVariables {
            variables: vec![
                PathVariable {
                    name: "SDKS".into(),
                    path: abs("/opt/sdks"),
                },
                PathVariable {
                    name: "PY311".into(),
                    path: abs("/opt/sdks/python3.11"),
                },
            ],
            ignored: vec!["TOOL_TOKEN".into()],
        }
    }

    #[test]
    fn names_are_single_tokens() {
        assert!(is_valid_name("MY_SDK2"));
        assert!(is_valid_name("_x"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("2X"));
        assert!(!is_valid_name("A-B"));
        assert!(!is_valid_name("A B"));
        assert_eq!(leading_variable("$A$/x"), Some("A"));
        assert_eq!(leading_variable("$A$"), Some("A"));
        assert_eq!(leading_variable("$A$x"), None);
        assert_eq!(leading_variable("x/$A$"), None);
    }

    #[test]
    fn validation_rejects_reserved_duplicate_and_relative() {
        assert_eq!(vars().validate(), Ok(()));
        for (name, path) in [
            ("USER_HOME", abs("/x")),
            ("1BAD", abs("/x")),
            ("SDKS", abs("/y")),
            ("REL", PathBuf::from("relative/dir")),
            ("EMPTY", PathBuf::new()),
        ] {
            let mut v = vars();
            v.variables.push(PathVariable {
                name: name.into(),
                path,
            });
            assert!(v.validate().is_err(), "{name} must be rejected");
        }
    }

    #[test]
    fn collapse_and_expand_round_trip_with_deepest_variable() {
        let v = vars();
        let interp = abs("/opt/sdks/python3.11/bin/python");
        let (collapsed, depth) = v.collapse(&interp, None).unwrap();
        assert_eq!(collapsed, "$PY311$/bin/python");
        assert_eq!(depth, 3);
        assert_eq!(v.expand_path(&collapsed, None), Some(interp));
        assert_eq!(v.collapse(&abs("/opt/sdks"), None).unwrap().0, "$SDKS$");
        assert_eq!(v.expand_path("$SDKS$", None), Some(abs("/opt/sdks")));
        assert_eq!(v.collapse(&abs("/usr/bin/python"), None), None);
        assert_eq!(v.collapse(Path::new("relative/x"), None), None);
    }

    #[test]
    fn project_dir_and_user_home_are_built_in() {
        let v = PathVariables::default();
        let project = abs("/work/app");
        assert_eq!(
            v.collapse(&abs("/work/app/src/main.py"), Some(&project))
                .unwrap()
                .0,
            "$PROJECT_DIR$/src/main.py"
        );
        assert_eq!(
            v.expand_path("$PROJECT_DIR$/src/main.py", Some(&project)),
            Some(abs("/work/app/src/main.py"))
        );
        assert_eq!(v.expand_path("$PROJECT_DIR$/x", None), None);
        if let Some(home) = user_home().filter(|h| depth(h) > 0) {
            let path = home.join(".pyenv").join("python");
            assert_eq!(
                v.collapse(&path, None).unwrap().0,
                "$USER_HOME$/.pyenv/python"
            );
            assert_eq!(v.expand_path("$USER_HOME$/.pyenv/python", None), Some(path));
        }
    }

    #[test]
    fn json_round_trip_keeps_non_paths_and_undefined_variables() {
        let v = vars();
        let project = abs("/work/app");
        let original = serde_json::json!({
            "name": "Run main",
            "interpreter": abs("/opt/sdks/python3.11/bin/python"),
            "kind": {"Python": {"script": abs("/work/app/main.py")}},
            "env_files": [abs("/opt/sdks/shared.env"), "relative.env"],
            "parameters": "--fast",
            "redirect_input": "$MISSING$/input.txt",
        });
        let mut stored = original.clone();
        v.collapse_json(&mut stored, Some(&project));
        assert_eq!(stored["interpreter"], "$PY311$/bin/python");
        assert_eq!(stored["kind"]["Python"]["script"], "$PROJECT_DIR$/main.py");
        assert_eq!(stored["env_files"][0], "$SDKS$/shared.env");
        assert_eq!(stored["env_files"][1], "relative.env");
        assert_eq!(stored["parameters"], "--fast");
        let mut loaded = stored.clone();
        v.expand_json(&mut loaded, Some(&project));
        assert_eq!(
            loaded,
            serde_json::to_value(&original).unwrap(),
            "collapse then expand restores every path"
        );
        assert_eq!(loaded["redirect_input"], "$MISSING$/input.txt");
    }

    #[test]
    fn undefined_variables_skip_defined_ignored_and_macros() {
        let v = vars();
        let value = serde_json::json!({
            "a": "$MISSING$/x",
            "b": ["$SDKS$/y", "$TOOL_TOKEN$", "$FilePath$", "$OTHER$", "$MISSING$"],
            "c": "text $NOT_LEADING$/z",
            "d": "$USER_HOME$/q",
        });
        assert_eq!(
            v.undefined_in_json(&value, &|name| name == "FilePath"),
            vec!["MISSING".to_string(), "OTHER".to_string()]
        );
    }

    #[test]
    fn expand_replaces_defined_variables_anywhere() {
        let v = vars();
        let project = abs("/work/app");
        let expected = format!(
            "--sdk={} --out={}/out $Unknown$ $$",
            abs("/opt/sdks").display(),
            project.display()
        );
        assert_eq!(
            v.expand(
                "--sdk=$SDKS$ --out=$PROJECT_DIR$/out $Unknown$ $$",
                Some(&project)
            ),
            expected
        );
    }

    #[test]
    fn older_settings_without_path_variables_still_load() {
        let settings: crate::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.path_variables, PathVariables::default());
        let back: crate::Settings =
            serde_json::from_value(serde_json::to_value(&settings).unwrap()).unwrap();
        assert_eq!(back.path_variables, PathVariables::default());
    }
}
