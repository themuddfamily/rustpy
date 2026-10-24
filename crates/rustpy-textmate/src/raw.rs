//! Grammar documents (`.tmLanguage.json` / XML plist `.tmLanguage`) parsed
//! into raw rules, before includes are resolved.
//!
//! Parsing is lenient like vscode-textmate: entries of the wrong type are
//! ignored, only a missing `scopeName` is an error.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::plist::{parse_plist, PlistError};

/// Error loading a grammar.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrammarError {
    /// The JSON document could not be parsed.
    #[error("invalid JSON grammar: {0}")]
    Json(String),
    /// The XML property list could not be parsed.
    #[error(transparent)]
    Plist(#[from] PlistError),
    /// The document is not a JSON object / plist dictionary.
    #[error("a grammar must be a JSON object or plist dictionary")]
    NotAnObject,
    /// The grammar has no (non-empty) `scopeName`.
    #[error("grammar has no scopeName")]
    MissingScopeName,
    /// A file could not be read.
    #[error("cannot read {path}: {message}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        message: String,
    },
}

/// Index of a [`RawRule`] in [`RawGrammar::rules`].
pub(crate) type RawRuleId = usize;

/// A `repository` object: rule name → rule.
pub(crate) type Repository = HashMap<String, RawRuleId>;

/// An entry of a `patterns` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawPattern {
    /// `{ "include": "…" }` (other keys are ignored, as in vscode-textmate).
    Include(String),
    Rule(RawRuleId),
}

/// One rule object (also used for capture entries).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RawRule {
    pub name: Option<String>,
    pub content_name: Option<String>,
    pub match_: Option<String>,
    pub begin: Option<String>,
    pub end: Option<String>,
    pub while_: Option<String>,
    pub captures: Option<RawCaptures>,
    pub begin_captures: Option<RawCaptures>,
    pub end_captures: Option<RawCaptures>,
    pub while_captures: Option<RawCaptures>,
    pub patterns: Option<Vec<RawPattern>>,
    /// A rule that is only `{ "include": … }` inside a repository.
    pub include: Option<String>,
    pub repository: Option<Repository>,
    pub apply_end_pattern_last: bool,
    pub disabled: bool,
}

/// Capture number → capture rule, sorted by number.
pub(crate) type RawCaptures = Vec<(usize, RawRuleId)>;

/// Maximum nesting of rule objects (guards the recursion).
const MAX_DEPTH: usize = 256;
/// Capture numbers above this are ignored.
const MAX_CAPTURE: usize = 1000;

/// A parsed grammar document whose includes are not resolved yet. Link it
/// into a [`crate::Grammar`] with [`crate::Grammar::from_raw`] or through a
/// [`crate::Registry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawGrammar {
    pub(crate) scope_name: String,
    pub(crate) name: Option<String>,
    pub(crate) file_types: Vec<String>,
    pub(crate) first_line_match: Option<String>,
    pub(crate) rules: Vec<RawRule>,
    pub(crate) patterns: Vec<RawPattern>,
    pub(crate) repository: Repository,
}

impl RawGrammar {
    /// Parses a `.tmLanguage.json` document.
    pub fn from_json(src: &str) -> Result<RawGrammar, GrammarError> {
        let value: Value =
            serde_json::from_str(src).map_err(|e| GrammarError::Json(e.to_string()))?;
        RawGrammar::from_value(&value)
    }

    /// Parses an XML property list (`.tmLanguage`) document.
    pub fn from_plist(src: &str) -> Result<RawGrammar, GrammarError> {
        RawGrammar::from_value(&parse_plist(src)?)
    }

    /// Parses either format, sniffing the first non-blank character (`<`
    /// means plist, anything else JSON).
    pub fn parse(src: &str) -> Result<RawGrammar, GrammarError> {
        if src
            .trim_start_matches('\u{feff}')
            .trim_start()
            .starts_with('<')
        {
            RawGrammar::from_plist(src)
        } else {
            RawGrammar::from_json(src)
        }
    }

    /// Builds a grammar from an already parsed document.
    pub fn from_value(value: &Value) -> Result<RawGrammar, GrammarError> {
        let obj = value.as_object().ok_or(GrammarError::NotAnObject)?;
        let scope_name = string(obj, "scopeName")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(GrammarError::MissingScopeName)?
            .to_string();
        let mut builder = Builder { rules: Vec::new() };
        let patterns = builder.patterns(obj.get("patterns"), 0);
        let repository = builder.repository(obj.get("repository"), 0);
        let file_types = match obj.get("fileTypes") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        Ok(RawGrammar {
            scope_name,
            name: string(obj, "name").map(str::to_string),
            file_types,
            first_line_match: string(obj, "firstLineMatch").map(str::to_string),
            rules: builder.rules,
            patterns,
            repository,
        })
    }

    /// The grammar's root scope, e.g. `source.rust`.
    pub fn scope_name(&self) -> &str {
        &self.scope_name
    }

    /// The display name, e.g. `Rust`.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// File extensions (without the dot) or full file names it applies to.
    pub fn file_types(&self) -> &[String] {
        &self.file_types
    }

    /// The `firstLineMatch` regex (Oniguruma syntax), if any.
    pub fn first_line_match(&self) -> Option<&str> {
        self.first_line_match.as_deref()
    }

    /// Scope names this grammar includes (`source.css`, `text.html.basic#tags`
    /// → `text.html.basic`), sorted, excluding itself.
    pub fn external_scopes(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .rules
            .iter()
            .flat_map(|r| {
                r.patterns
                    .iter()
                    .flatten()
                    .cloned()
                    .chain(r.include.clone().map(RawPattern::Include))
            })
            .chain(self.patterns.iter().cloned())
            .filter_map(|p| match p {
                RawPattern::Include(s) => external_scope(&s).map(str::to_string),
                RawPattern::Rule(_) => None,
            })
            .filter(|s| *s != self.scope_name)
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

/// The scope part of an include that refers to another grammar.
pub(crate) fn external_scope(include: &str) -> Option<&str> {
    if include.starts_with('#') || include.starts_with('$') {
        return None;
    }
    let scope = include.split('#').next().unwrap_or(include);
    (!scope.is_empty()).then_some(scope)
}

fn string<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(Value::as_str)
}

/// `true`, `1`, `"1"`, `"true"`.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => matches!(s.trim(), "1" | "true" | "YES" | "yes"),
        _ => false,
    }
}

struct Builder {
    rules: Vec<RawRule>,
}

impl Builder {
    fn patterns(&mut self, value: Option<&Value>, depth: usize) -> Vec<RawPattern> {
        let Some(Value::Array(items)) = value else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| {
                let obj = item.as_object()?;
                match string(obj, "include") {
                    Some(include) => Some(RawPattern::Include(include.to_string())),
                    None => self.rule(obj, depth + 1).map(RawPattern::Rule),
                }
            })
            .collect()
    }

    fn repository(&mut self, value: Option<&Value>, depth: usize) -> Repository {
        let Some(Value::Object(entries)) = value else {
            return Repository::new();
        };
        entries
            .iter()
            .filter_map(|(key, item)| Some((key.clone(), self.rule(item.as_object()?, depth + 1)?)))
            .collect()
    }

    fn captures(&mut self, value: Option<&Value>, depth: usize) -> Option<RawCaptures> {
        let entries: Vec<(usize, &Value)> = match value? {
            Value::Object(map) => map
                .iter()
                .filter_map(|(k, v)| Some((k.trim().parse().ok()?, v)))
                .collect(),
            Value::Array(items) => items.iter().enumerate().collect(),
            _ => return None,
        };
        let mut out: RawCaptures = entries
            .into_iter()
            .filter(|(n, _)| *n <= MAX_CAPTURE)
            .filter_map(|(n, v)| Some((n, self.rule(v.as_object()?, depth + 1)?)))
            .collect();
        out.sort_by_key(|(n, _)| *n);
        out.dedup_by_key(|(n, _)| *n);
        Some(out)
    }

    fn rule(&mut self, obj: &Map<String, Value>, depth: usize) -> Option<RawRuleId> {
        if depth > MAX_DEPTH {
            return None;
        }
        // Reserve the slot first so nested rules get later ids.
        let id = self.rules.len();
        self.rules.push(RawRule::default());
        let rule = RawRule {
            name: string(obj, "name").map(str::to_string),
            content_name: string(obj, "contentName").map(str::to_string),
            match_: string(obj, "match").map(str::to_string),
            begin: string(obj, "begin").map(str::to_string),
            end: string(obj, "end").map(str::to_string),
            while_: string(obj, "while").map(str::to_string),
            captures: self.captures(obj.get("captures"), depth),
            begin_captures: self.captures(obj.get("beginCaptures"), depth),
            end_captures: self.captures(obj.get("endCaptures"), depth),
            while_captures: self.captures(obj.get("whileCaptures"), depth),
            patterns: obj.get("patterns").map(|p| self.patterns(Some(p), depth)),
            include: string(obj, "include").map(str::to_string),
            repository: obj
                .get("repository")
                .map(|r| self.repository(Some(r), depth)),
            apply_end_pattern_last: truthy(obj.get("applyEndPatternLast")),
            disabled: truthy(obj.get("disabled")),
        };
        self.rules[id] = rule;
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rules_captures_and_repository() {
        let g = RawGrammar::from_json(
            r##"{
                "scopeName": " source.demo ",
                "name": "Demo",
                "fileTypes": ["demo", 3, "dm"],
                "firstLineMatch": "^#!.*demo",
                "patterns": [
                    { "include": "#strings", "name": "ignored" },
                    { "match": "\\bif\\b", "name": "keyword.control.demo" },
                    "not an object",
                    { "begin": "\"", "end": "\"", "applyEndPatternLast": 1,
                      "captures": { "0": { "name": "punctuation" }, "x": {}, "2": { "patterns": [] } } }
                ],
                "repository": {
                    "strings": { "patterns": [ { "include": "source.other#x" } ], "repository": { "inner": {} } },
                    "bad": 42
                }
            }"##,
        )
        .unwrap();
        assert_eq!(g.scope_name(), "source.demo");
        assert_eq!(g.name(), Some("Demo"));
        assert_eq!(g.file_types(), ["demo".to_string(), "dm".to_string()]);
        assert_eq!(g.first_line_match(), Some("^#!.*demo"));
        assert_eq!(g.patterns.len(), 3);
        assert_eq!(g.patterns[0], RawPattern::Include("#strings".into()));
        let RawPattern::Rule(string_rule) = g.patterns[2] else {
            panic!()
        };
        let rule = &g.rules[string_rule];
        assert!(rule.apply_end_pattern_last);
        let caps = rule.captures.as_ref().unwrap();
        assert_eq!(caps.iter().map(|c| c.0).collect::<Vec<_>>(), vec![0, 2]);
        assert_eq!(g.rules[caps[0].1].name.as_deref(), Some("punctuation"));
        assert_eq!(g.repository.len(), 1);
        let strings = &g.rules[g.repository["strings"]];
        assert!(strings.repository.as_ref().unwrap().contains_key("inner"));
        assert_eq!(g.external_scopes(), vec!["source.other".to_string()]);
    }

    #[test]
    fn plist_and_sniffing() {
        let src = r#"<?xml version="1.0"?><plist version="1.0"><dict>
            <key>scopeName</key><string>source.p</string>
            <key>patterns</key><array><dict><key>match</key><string>a&amp;b</string></dict></array>
            <key>repository</key><dict/>
        </dict></plist>"#;
        let g = RawGrammar::parse(src).unwrap();
        assert_eq!(g.scope_name(), "source.p");
        let RawPattern::Rule(id) = g.patterns[0] else {
            panic!()
        };
        assert_eq!(g.rules[id].match_.as_deref(), Some("a&b"));
        assert_eq!(
            RawGrammar::parse(r#"{"scopeName":"x"}"#)
                .unwrap()
                .scope_name(),
            "x"
        );
    }

    #[test]
    fn errors() {
        assert_eq!(RawGrammar::from_json("[]"), Err(GrammarError::NotAnObject));
        assert_eq!(
            RawGrammar::from_json(r#"{"name":"x"}"#),
            Err(GrammarError::MissingScopeName)
        );
        assert_eq!(
            RawGrammar::from_json(r#"{"scopeName":"  "}"#),
            Err(GrammarError::MissingScopeName)
        );
        assert!(matches!(
            RawGrammar::from_json("{"),
            Err(GrammarError::Json(_))
        ));
        assert!(matches!(
            RawGrammar::from_plist("<plist>"),
            Err(GrammarError::Plist(_))
        ));
    }

    #[test]
    fn deep_nesting_is_cut_off_without_overflowing() {
        let mut v = serde_json::json!({ "match": "x" });
        for _ in 0..(MAX_DEPTH + 50) {
            v = serde_json::json!({ "patterns": [v] });
        }
        let g = RawGrammar::from_value(&serde_json::json!({ "scopeName": "s", "patterns": [v] }))
            .unwrap();
        assert!(g.rules.len() <= MAX_DEPTH + 1);
    }
}
