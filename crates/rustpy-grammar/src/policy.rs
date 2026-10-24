//! Validated, explicit policy for the built-in English checker.
//! Unsupported languages fail instead of silently applying English rules.

use crate::Issue;
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind,
};
use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    EnglishUs,
    EnglishGb,
}
impl Language {
    pub const ALL: [Self; 2] = [Self::EnglishUs, Self::EnglishGb];
    pub const fn key(self) -> &'static str {
        match self {
            Self::EnglishUs => "en-US",
            Self::EnglishGb => "en-GB",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::EnglishUs => "English (United States)",
            Self::EnglishGb => "English (United Kingdom)",
        }
    }
    pub fn from_key(key: &str) -> Result<Self, PolicyError> {
        match key {
            "en" | "en-US" => Ok(Self::EnglishUs),
            "en-GB" => Ok(Self::EnglishGb),
            _ => Err(PolicyError::UnsupportedLanguage(key.into())),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Grammar,
    Capitalization,
    Punctuation,
    Style,
}
impl Category {
    pub const ALL: [Self; 4] = [
        Self::Grammar,
        Self::Capitalization,
        Self::Punctuation,
        Self::Style,
    ];
    pub const fn key(self) -> &'static str {
        match self {
            Self::Grammar => "grammar",
            Self::Capitalization => "capitalization",
            Self::Punctuation => "punctuation",
            Self::Style => "style",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::Grammar => "Grammar",
            Self::Capitalization => "Capitalization",
            Self::Punctuation => "Punctuation",
            Self::Style => "Style and usage",
        }
    }
    pub fn from_key(key: &str) -> Result<Self, PolicyError> {
        Self::ALL
            .into_iter()
            .find(|c| c.key() == key)
            .ok_or_else(|| PolicyError::UnknownCategory(key.into()))
    }
    const fn bit(self) -> u8 {
        match self {
            Self::Grammar => 1,
            Self::Capitalization => 2,
            Self::Punctuation => 4,
            Self::Style => 8,
        }
    }
}

/// Every built-in rule has one stable category; unknown future rules are not hidden.
pub fn rule_category(rule: &str) -> Option<Category> {
    match rule {
        "repeated-word" | "article" | "could-of" => Some(Category::Grammar),
        "lowercase-i" | "sentence-start" => Some(Category::Capitalization),
        "space-before-punct" => Some(Category::Punctuation),
        "common-mistake" => Some(Category::Style),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyError {
    UnsupportedLanguage(String),
    UnknownCategory(String),
}
impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedLanguage(key) => {
                write!(f, "Grammar checking does not support language '{key}'")
            }
            Self::UnknownCategory(key) => write!(f, "Unknown grammar category '{key}'"),
        }
    }
}
impl Error for PolicyError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub language: Language,
    enabled: u8,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            language: Language::default(),
            enabled: 15,
        }
    }
}
impl Policy {
    /// Persist locale/category keys independently of the checker implementation.
    pub fn from_preferences<'a>(
        language: &str,
        disabled: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, PolicyError> {
        let mut policy = Self {
            language: Language::from_key(language)?,
            ..Self::default()
        };
        for key in disabled {
            policy.set_enabled(Category::from_key(key)?, false);
        }
        Ok(policy)
    }
    pub fn enabled(self, category: Category) -> bool {
        self.enabled & category.bit() != 0
    }
    pub fn set_enabled(&mut self, category: Category, enabled: bool) {
        if enabled {
            self.enabled |= category.bit();
        } else {
            self.enabled &= !category.bit();
        }
    }
    fn accepts(self, issue: &Issue) -> bool {
        rule_category(issue.rule).is_none_or(|category| self.enabled(category))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Python,
    Markdown,
    Prose,
    CommitMessage,
}

/// Applies the same selected policy in every prose surface, preserving byte ranges
/// and replacement payloads from the checker. Both English locales share these
/// pronunciation/usage rules; no spelling or unsupported-language checks are implied.
pub fn check(text: &str, context: Context, policy: Policy) -> Vec<Issue> {
    let issues = match context {
        Context::Python => check_python_prose(text),
        Context::Markdown => crate::check_markdown(text),
        Context::Prose | Context::CommitMessage => crate::check_prose(text),
    };
    issues
        .into_iter()
        .filter(|issue| policy.accepts(issue))
        .collect()
}

/// Restrict string checking to actual module/class/function docstrings, including
/// single-quoted docstrings. Assigned strings, bytes and f-strings are code.
fn check_python_prose(text: &str) -> Vec<Issue> {
    let tree = rustpy_syntax::parse(text).syntax_node();
    let mut docs = Vec::new();
    if let Some(module) = ast::Module::cast(tree.clone()) {
        if let Some(doc) = module.docstring() {
            docs.push(doc.syntax().text_range());
        }
    }
    for node in tree.descendants() {
        let doc = match node.kind() {
            SyntaxKind::FUNC_DEF => ast::FuncDef::cast(node).and_then(|f| f.docstring()),
            SyntaxKind::CLASS_DEF => ast::ClassDef::cast(node).and_then(|c| c.docstring()),
            _ => None,
        };
        if let Some(doc) = doc {
            docs.push(doc.syntax().text_range());
        }
    }
    let mut out = Vec::new();
    for token in rustpy_syntax::tokenize(text).0 {
        let start = usize::from(token.range.start());
        let end = usize::from(token.range.end());
        let raw = &text[start..end];
        if token.kind == SyntaxKind::COMMENT {
            // Reuse directive/code-comment exclusions and all existing message/fix bodies.
            for mut issue in crate::check_python(raw) {
                issue.range = issue.range.start + start..issue.range.end + start;
                out.push(issue);
            }
        } else if token.kind == SyntaxKind::STRING
            && docs.iter().any(|r| r.contains_range(token.range))
        {
            let Some(prefix) = raw.find(['\'', '"']) else {
                continue;
            };
            if raw[..prefix]
                .chars()
                .any(|c| matches!(c, 'b' | 'B' | 'f' | 'F'))
            {
                continue;
            }
            let quoted = &raw[prefix..];
            let width = if quoted.starts_with("\"\"\"") || quoted.starts_with("'''") {
                3
            } else {
                1
            };
            if quoted.len() < width * 2 || !quoted.ends_with(&quoted[..width]) {
                continue;
            }
            for mut issue in crate::check_prose(&quoted[width..quoted.len() - width]) {
                let offset = start + prefix + width;
                issue.range = issue.range.start + offset..issue.range.end + offset;
                out.push(issue);
            }
        }
    }
    out.sort_by_key(|issue| issue.range.start);
    out
}
