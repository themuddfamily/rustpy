//! Read-only framework configuration and naming evidence, without importing code.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PytestConfiguration {
    pub path: Option<PathBuf>,
    pub values: BTreeMap<String, Vec<String>>,
}
impl PytestConfiguration {
    pub fn load(root: &Path) -> Self {
        for (file, section) in [
            ("pytest.ini", "pytest"),
            (".pytest.ini", "pytest"),
            ("pyproject.toml", ""),
            ("tox.ini", "pytest"),
            ("setup.cfg", "tool:pytest"),
        ] {
            let path = root.join(file);
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            if file == "pyproject.toml" {
                let Ok(value) = toml::from_str::<toml::Value>(&source) else {
                    continue;
                };
                let table = value
                    .get("tool")
                    .and_then(|v| v.get("pytest"))
                    .and_then(|v| v.get("ini_options").or(Some(v)))
                    .and_then(|v| v.as_table());
                if let Some(table) = table {
                    let values = table
                        .iter()
                        .map(|(key, value)| {
                            (
                                key.clone(),
                                if let Some(array) = value.as_array() {
                                    array
                                        .iter()
                                        .filter_map(|v| v.as_str().map(str::to_string))
                                        .collect()
                                } else {
                                    value
                                        .as_str()
                                        .unwrap_or("")
                                        .split_whitespace()
                                        .map(str::to_string)
                                        .collect()
                                },
                            )
                        })
                        .collect();
                    return Self {
                        path: Some(path),
                        values,
                    };
                }
            } else {
                let document =
                    rustpy_syntax::ini::parse(&source, rustpy_syntax::ini::Dialect::ConfigParser);
                if let Some(index) = document
                    .sections
                    .iter()
                    .position(|s| &source[s.name.clone()] == section)
                {
                    let values = document
                        .properties
                        .iter()
                        .filter(|p| p.section == Some(index))
                        .map(|p| {
                            (
                                source[p.key.clone()].to_string(),
                                document
                                    .value(p)
                                    .split_whitespace()
                                    .map(str::to_string)
                                    .collect(),
                            )
                        })
                        .collect();
                    return Self {
                        path: Some(path),
                        values,
                    };
                }
                // An empty pytest.ini is itself authoritative pytest evidence.
                if file.ends_with("pytest.ini") {
                    return Self {
                        path: Some(path),
                        values: BTreeMap::new(),
                    };
                }
            }
        }
        Self::default()
    }
    pub fn matches(&self, key: &str, name: &str, default: &[&str]) -> bool {
        if let Some(patterns) = self.values.get(key) {
            patterns.iter().any(|pattern| glob_matches(pattern, name))
        } else {
            default.iter().any(|pattern| glob_matches(pattern, name))
        }
    }
}
/// Shell-style naming globs (`*`, `?`, bracket classes/ranges), without recursive backtracking.
pub fn glob_matches(pattern: &str, value: &str) -> bool {
    let p: Vec<_> = pattern.chars().collect();
    let v: Vec<_> = value.chars().collect();
    fn one(pattern: &[char], index: usize, value: char) -> Option<usize> {
        let character = *pattern.get(index)?;
        if character == '?' {
            return Some(index + 1);
        }
        if character == '*' {
            return None;
        }
        if character != '[' {
            return (character == value).then_some(index + 1);
        }
        let mut cursor = index + 1;
        let negate = matches!(pattern.get(cursor), Some('!' | '^'));
        if negate {
            cursor += 1;
        }
        let begin = cursor;
        if pattern.get(cursor) == Some(&']') {
            cursor += 1;
        }
        while cursor < pattern.len() && pattern[cursor] != ']' {
            cursor += 1;
        }
        if cursor == pattern.len() {
            return (value == '[').then_some(index + 1);
        }
        let end = cursor;
        cursor = begin;
        let mut matched = false;
        while cursor < end {
            if cursor + 2 < end && pattern[cursor + 1] == '-' {
                matched |= pattern[cursor] <= value && value <= pattern[cursor + 2];
                cursor += 3;
            } else {
                matched |= pattern[cursor] == value;
                cursor += 1;
            }
        }
        (matched != negate).then_some(end + 1)
    }
    let (mut pi, mut vi, mut star, mut restart) = (0, 0, None, 0);
    while vi < v.len() {
        if let Some(next) = one(&p, pi, v[vi]) {
            pi = next;
            vi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            restart = vi;
        } else if let Some(position) = star {
            restart += 1;
            vi = restart;
            pi = position + 1;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}
pub fn is_test_file_in(root: &Path, path: &Path) -> bool {
    let config = PytestConfiguration::load(root);
    if config.values.contains_key("python_files") {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        name.ends_with(".py")
            && name != "conftest.py"
            && name != "__init__.py"
            && config.matches("python_files", &name, &[])
    } else {
        crate::is_test_file(path.strip_prefix(root).unwrap_or(path))
    }
}
/// Per-environment run plans for Test Runner tabs. Explicit `-e` selectors win;
/// otherwise use tox's configured env list. Native tox expands conditional names.
pub fn tox_environment_plans(
    root: &Path,
    targets: &[String],
    args: &[String],
) -> Vec<(String, Vec<String>)> {
    let mut environments = Vec::new();
    let mut remaining = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if matches!(arg.as_str(), "-e" | "--env" | "--environment") && index + 1 < args.len() {
            environments.extend(expand_tox_environments(&args[index + 1]));
            index += 2;
            continue;
        } else if let Some(value) = arg
            .strip_prefix("--env=")
            .or_else(|| arg.strip_prefix("-e="))
        {
            environments.extend(expand_tox_environments(value));
        } else {
            remaining.push(arg.clone());
        }
        index += 1;
    }
    if environments.is_empty() {
        environments.extend(
            targets
                .iter()
                .flat_map(|target| expand_tox_environments(target)),
        );
    }
    if environments.is_empty() {
        if let Ok(source) = std::fs::read_to_string(root.join("tox.ini")) {
            let document =
                rustpy_syntax::ini::parse(&source, rustpy_syntax::ini::Dialect::ConfigParser);
            for property in &document.properties {
                if property
                    .section
                    .and_then(|i| document.sections.get(i))
                    .is_some_and(|s| &source[s.name.clone()] == "tox")
                    && matches!(&source[property.key.clone()], "envlist" | "env_list")
                {
                    environments.extend(expand_tox_environments(&document.value(property)));
                }
            }
        }
    }
    if environments.is_empty() {
        for filename in ["tox.toml", "pyproject.toml"] {
            let Ok(source) = std::fs::read_to_string(root.join(filename)) else {
                continue;
            };
            let Ok(parsed) = toml::from_str::<toml::Value>(&source) else {
                continue;
            };
            let configuration = if filename == "tox.toml" {
                Some(&parsed)
            } else {
                parsed.get("tool").and_then(|value| value.get("tox"))
            };
            if let Some(value) = configuration
                .and_then(|value| value.get("env_list").or_else(|| value.get("envlist")))
            {
                if let Some(names) = value.as_array() {
                    environments.extend(
                        names
                            .iter()
                            .filter_map(toml::Value::as_str)
                            .flat_map(expand_tox_environments),
                    );
                } else if let Some(names) = value.as_str() {
                    environments.extend(expand_tox_environments(names));
                }
            }
        }
    }
    let mut unique = std::collections::BTreeSet::new();
    environments
        .into_iter()
        .filter(|name| !name.is_empty() && unique.insert(name.clone()))
        .map(|name| {
            let mut arguments = vec!["-e".into(), name.clone()];
            arguments.extend(remaining.clone());
            (name, arguments)
        })
        .collect()
}

pub fn expand_tox_environments(value: &str) -> Vec<String> {
    fn pieces(value: &str) -> Vec<&str> {
        let (mut depth, mut start) = (0usize, 0usize);
        let mut result = Vec::new();
        for (index, ch) in value.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                ',' | ' ' | '\n' | '\t' if depth == 0 => {
                    if start < index {
                        result.push(&value[start..index]);
                    }
                    start = index + ch.len_utf8();
                }
                _ => {}
            }
        }
        if start < value.len() {
            result.push(&value[start..]);
        }
        result
    }
    fn expand(value: &str, out: &mut Vec<String>, depth: usize) {
        if out.len() >= 256 || depth >= 8 {
            return;
        }
        if let Some(start) = value.find('{') {
            if let Some(end) = value[start..].find('}').map(|end| end + start) {
                for choice in pieces(&value[start + 1..end]) {
                    expand(
                        &format!("{}{}{}", &value[..start], choice, &value[end + 1..]),
                        out,
                        depth + 1,
                    );
                }
                return;
            }
        }
        out.push(value.to_string());
    }
    let mut result = Vec::new();
    for piece in pieces(value) {
        expand(piece, &mut result, 0);
    }
    result
}
