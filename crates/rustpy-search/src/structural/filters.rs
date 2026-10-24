//! Persisted templates and a deliberately non-executable, bounded filter language.
use super::{compile, PatternOptions};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::Path};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Template {
    pub name: String,
    pub pattern: String,
    pub replacement: String,
    /// A replacement may intentionally be empty (delete the matched source).
    pub replace: bool,
    pub options: PatternOptions,
    pub inspection: bool,
}

const LIBRARY_LIMIT: usize = 1_048_576;

pub fn load_library(path: &Path) -> Result<Vec<Template>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    file.take((LIBRARY_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > LIBRARY_LIMIT {
        return Err("Template library exceeds 1 MiB".into());
    }
    let templates: Vec<Template> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate_library(&templates)?;
    Ok(templates)
}

fn validate_library(templates: &[Template]) -> Result<(), String> {
    if templates.len() > 128 {
        return Err("At most 128 saved templates are supported".into());
    }
    let mut names = std::collections::HashSet::new();
    for template in templates {
        if template.name.trim().is_empty()
            || template.name.len() > 256
            || !names.insert(&template.name)
        {
            return Err("Template names must be unique, nonempty and at most 256 bytes".into());
        }
        compile(&template.pattern, &template.options)?;
        if template.replacement.len() > 65_536 {
            return Err("Replacement exceeds 64 KiB".into());
        }
    }
    Ok(())
}

/// Stable cache key: HashMap iteration order must never restart background analysis.
pub fn library_key(templates: &[Template]) -> Vec<u8> {
    let normalized: Vec<_> = templates
        .iter()
        .map(|template| {
            let constraints: std::collections::BTreeMap<_, _> =
                template.options.constraints.iter().collect();
            (
                &template.name,
                &template.pattern,
                &template.replacement,
                template.inspection,
                template.replace,
                constraints,
            )
        })
        .collect();
    serde_json::to_vec(&normalized).unwrap_or_default()
}

pub fn save_library(path: &Path, templates: &[Template]) -> Result<(), String> {
    validate_library(templates)?;
    let bytes = serde_json::to_vec_pretty(templates).map_err(|e| e.to_string())?;
    if bytes.len() > LIBRARY_LIMIT {
        return Err("Template library exceeds 1 MiB".into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // A unique owned temporary file prevents one window from truncating another writer's library.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temporary = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut owned = false;
    use std::io::Write;
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        owned = true;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() && owned {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Clone, Debug)]
pub(super) struct Script(Vec<Vec<Predicate>>);
#[derive(Clone, Debug)]
enum Predicate {
    Text(String, String),
    Count(String, usize),
}

impl Script {
    /// Script is an OR of AND clauses. No Python evaluation, filesystem, imports or callbacks.
    pub(super) fn compile(source: &str) -> Result<Self, String> {
        if source.len() > 4096 {
            return Err("Script filter exceeds 4096 bytes".into());
        }
        let ors = split(source, "||")?;
        let mut groups = Vec::new();
        let mut budget = 32usize;
        for group in ors {
            let mut predicates = Vec::new();
            for term in split(group, "&&")? {
                budget = budget
                    .checked_sub(1)
                    .ok_or("Script filter exceeds 32 predicates")?;
                let term = term.trim();
                if let Some(rest) = term.strip_prefix("text.") {
                    let (method, argument) = rest
                        .split_once('(')
                        .ok_or("Expected text.contains(\"value\")")?;
                    if !matches!(method, "contains" | "starts_with" | "ends_with") {
                        return Err("Unknown text method".into());
                    }
                    let argument = argument
                        .strip_suffix(')')
                        .ok_or("Missing closing parenthesis")?;
                    let value: String = serde_json::from_str(argument.trim())
                        .map_err(|e| format!("Script string: {e}"))?;
                    predicates.push(Predicate::Text(method.into(), value));
                } else {
                    let (field, rest) = if let Some(rest) = term.strip_prefix("count") {
                        ("count", rest)
                    } else if let Some(rest) = term.strip_prefix("text") {
                        ("text", rest)
                    } else {
                        return Err("Script operands are text or count".into());
                    };
                    let rest = rest.trim();
                    let op = ["==", "!=", ">=", "<=", ">", "<"]
                        .into_iter()
                        .find(|op| rest.starts_with(op))
                        .ok_or("Expected comparison operator")?;
                    let argument = rest[op.len()..].trim();
                    if field == "count" {
                        predicates.push(Predicate::Count(
                            op.into(),
                            argument
                                .parse()
                                .map_err(|_| "Count must be a nonnegative integer")?,
                        ));
                    } else {
                        if !matches!(op, "==" | "!=") {
                            return Err("Text supports == or !=".into());
                        }
                        predicates.push(Predicate::Text(
                            op.into(),
                            serde_json::from_str(argument)
                                .map_err(|e| format!("Script string: {e}"))?,
                        ));
                    }
                }
            }
            groups.push(predicates);
        }
        Ok(Self(groups))
    }
    pub(super) fn evaluate(&self, text: &str, count: usize) -> bool {
        self.0.iter().any(|group| {
            group.iter().all(|p| match p {
                Predicate::Text(op, value) => match op.as_str() {
                    "contains" => text.contains(value),
                    "starts_with" => text.starts_with(value),
                    "ends_with" => text.ends_with(value),
                    "==" => text == value,
                    "!=" => text != value,
                    _ => false,
                },
                Predicate::Count(op, value) => match op.as_str() {
                    "==" => count == *value,
                    "!=" => count != *value,
                    ">=" => count >= *value,
                    "<=" => count <= *value,
                    ">" => count > *value,
                    "<" => count < *value,
                    _ => false,
                },
            })
        })
    }
}

fn split<'a>(source: &'a str, delimiter: &str) -> Result<Vec<&'a str>, String> {
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    let mut out = Vec::new();
    for (index, ch) in source.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
        }
        if !quoted && source[index..].starts_with(delimiter) {
            out.push(source[start..index].trim());
            start = index + delimiter.len();
        }
    }
    if quoted {
        return Err("Unterminated script string".into());
    }
    out.push(source[start..].trim());
    if out.iter().any(|s| s.is_empty()) {
        return Err("Empty script predicate".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structural::{self, Constraint};

    #[test]
    fn scripts_evaluate_unicode_counts_quoted_operators_and_refuse_execution() {
        let script =
            Script::compile(r#"text.contains("λ&&π") && count >= 2 || text == "safe""#).unwrap();
        assert!(script.evaluate("f(λ&&π)", 2));
        assert!(script.evaluate("safe", 0));
        assert!(!script.evaluate("f(λ&&π)", 1));
        assert!(Script::compile("__import__(\"os\").system(\"bad\")").is_err());
        assert!(Script::compile("text.contains(\"unclosed)").is_err());
    }

    #[test]
    fn saved_templates_round_trip_all_filters_and_inspection_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.json");
        let mut options = PatternOptions::default();
        options.constraints.insert(
            "args".into(),
            Constraint {
                regex: Some("π".into()),
                min: 1,
                max: 2,
                script: Some("count == 1".into()),
                ..Default::default()
            },
        );
        let template = Template {
            name: "Unicode logging".into(),
            pattern: "print($args...$)".into(),
            replacement: "log($args...$)".into(),
            replace: true,
            options,
            inspection: true,
        };
        save_library(&path, &[template]).unwrap();
        let templates = load_library(&path).unwrap();
        assert!(templates[0].inspection);
        let pattern = compile(&templates[0].pattern, &templates[0].options).unwrap();
        let text = "print(π)\nprint(π, 2)\nprint(other)\n";
        let edits = structural::replace_text(&pattern, &templates[0].replacement, text);
        assert_eq!(
            structural::apply_edits(text, &edits),
            "log(π)\nprint(π, 2)\nprint(other)\n"
        );
        assert!(save_library(&path, &[templates[0].clone(), templates[0].clone()]).is_err());
        assert_eq!(load_library(&path).unwrap().len(), 1);
    }

    #[test]
    fn semantic_type_filter_cannot_be_bypassed_by_text_only_replacement() {
        let db = rustpy_semantic::Analysis::for_project(Path::new("/"));
        let path = Path::new("/structural_types.py");
        let text = "print('π')\nprint(42)\nprint(unknown)\n";
        db.set_file_text(path, text);
        let file = db.file_for_path(path).unwrap();
        let mut options = PatternOptions::default();
        options.constraints.insert(
            "x".into(),
            Constraint {
                type_regex: Some("^str$".into()),
                ..Default::default()
            },
        );
        let pattern = compile("print($x$)", &options).unwrap();
        assert!(structural::search_text(&pattern, text).is_empty());
        assert!(structural::replace_text(&pattern, "log($x$)", text).is_empty());
        let edits = structural::replace_with_analysis(&pattern, "log($x$)", &db, &file);
        assert_eq!(
            structural::apply_edits(text, &edits),
            "log('π')\nprint(42)\nprint(unknown)\n"
        );
        db.set_file_text(path, "print(42)\n");
        let current = db.file_for_path(path).unwrap();
        assert!(structural::search_with_analysis(&pattern, &db, &current).is_empty());
    }

    #[test]
    fn library_cache_key_ignores_hash_map_iteration_order() {
        let mut first = Template {
            name: "Pair".into(),
            pattern: "f($x$, $y$)".into(),
            ..Default::default()
        };
        first.options.constraints.insert(
            "x".into(),
            Constraint {
                regex: Some("x".into()),
                ..Default::default()
            },
        );
        first.options.constraints.insert(
            "y".into(),
            Constraint {
                regex: Some("y".into()),
                ..Default::default()
            },
        );
        let mut second = first.clone();
        second.options.constraints.clear();
        second
            .options
            .constraints
            .insert("y".into(), first.options.constraints["y"].clone());
        second
            .options
            .constraints
            .insert("x".into(), first.options.constraints["x"].clone());
        assert_eq!(library_key(&[first]), library_key(&[second.clone()]));
        second.inspection = true;
        assert_ne!(
            library_key(&[second.clone()]),
            library_key(&[Template {
                inspection: false,
                ..second
            }])
        );
    }

    #[test]
    fn invalid_filter_names_ranges_and_oversized_scripts_are_rejected() {
        let mut options = PatternOptions::default();
        options
            .constraints
            .insert("missing".into(), Constraint::default());
        assert!(compile("print($x$)", &options).is_err());
        options.constraints.clear();
        options.constraints.insert(
            "x".into(),
            Constraint {
                min: 3,
                max: 1,
                ..Default::default()
            },
        );
        assert!(compile("print($x$)", &options).is_err());
        assert!(Script::compile(&"text == \"x\" || ".repeat(100)).is_err());
    }
}
