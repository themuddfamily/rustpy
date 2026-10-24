//! Editor page preferences shared by settings controls and actual consumers.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub scheme_fonts: BTreeMap<String, SchemeFont>,
    pub css_color_background: bool,
    pub code_vision_inheritors: bool,
    pub font_fallbacks: Vec<String>,
    pub inlay_parameter_blacklist: Vec<String>,
    pub inlay_disabled_languages: Vec<String>,
    pub code_vision_position: String,
    pub drag_and_drop: bool,
    pub double_click_camel_humps: bool,
    pub caret_when_unfocused: bool,
    pub diagnostic_tooltips: bool,
    pub error_stripe_min_height: f32,
    pub reparse_delay_ms: u64,
    pub soft_wrap_masks: String,
    pub soft_wrap_indent: bool,
    pub show_soft_wrap_indent: bool,
    pub rich_text_copy: bool,
    pub reformat_notifications: bool,
    pub max_editor_lines: usize,
    pub breadcrumbs_disabled: Vec<String>,
    pub gutter_disabled: Vec<String>,
    pub prefer_from_imports: bool,
    pub excluded_imports: Vec<String>,
    pub postfix_enabled: bool,
    pub postfix_disabled: Vec<String>,
    pub ignored_patterns: String,
    pub associations: Vec<Association>,
    pub injections_enabled: bool,
    pub injections_disabled: Vec<String>,
    pub injection_rules: Vec<InjectionRule>,
    pub encoding_default: String,
    pub encoding_rules: Vec<EncodingRule>,
    pub new_utf8_bom: bool,
    pub new_line_separator: String,
    pub scrollbar_code_preview: bool,
    pub duplicate_min_size: usize,
    pub duplicate_anonymize_variables: bool,
    pub duplicate_anonymize_functions: bool,
    pub duplicate_anonymize_literals: bool,
    pub duplicate_anonymize_fields: bool,
    pub image_checkerboard: bool,
    pub image_grid: bool,
    pub image_zoom: f32,
    pub image_editor: String,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            scheme_fonts: BTreeMap::new(),
            css_color_background: false,
            code_vision_inheritors: false,
            font_fallbacks: Vec::new(),
            inlay_parameter_blacklist: Vec::new(),
            inlay_disabled_languages: Vec::new(),
            code_vision_position: "top".into(),
            drag_and_drop: true,
            double_click_camel_humps: true,
            caret_when_unfocused: false,
            diagnostic_tooltips: true,
            error_stripe_min_height: 3.0,
            reparse_delay_ms: 300,
            soft_wrap_masks: "*".into(),
            soft_wrap_indent: false,
            show_soft_wrap_indent: false,
            rich_text_copy: true,
            reformat_notifications: true,
            max_editor_lines: 100_000,
            breadcrumbs_disabled: Vec::new(),
            gutter_disabled: Vec::new(),
            prefer_from_imports: true,
            excluded_imports: Vec::new(),
            postfix_enabled: true,
            postfix_disabled: Vec::new(),
            ignored_patterns: String::new(),
            associations: Vec::new(),
            injections_enabled: true,
            injections_disabled: Vec::new(),
            injection_rules: Vec::new(),
            encoding_default: String::new(),
            encoding_rules: Vec::new(),
            new_utf8_bom: false,
            new_line_separator: "preserve".into(),
            scrollbar_code_preview: true,
            duplicate_min_size: 32,
            duplicate_anonymize_variables: false,
            duplicate_anonymize_functions: false,
            duplicate_anonymize_literals: false,
            duplicate_anonymize_fields: false,
            image_checkerboard: true,
            image_grid: false,
            image_zoom: 1.0,
            image_editor: String::new(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Association {
    pub name: String,
    pub patterns: String,
    pub shebang: String,
    pub language: String,
    pub external_application: bool,
    pub keywords: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InjectionRule {
    pub language: String,
    pub file_masks: String,
    pub prefix: String,
    pub enabled: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EncodingRule {
    pub project: Option<PathBuf>,
    pub path: PathBuf,
    pub encoding: String,
}
/// Bounded wildcard matcher; '*' and '?' match characters, ';' separates masks.
/// Full paths and basenames are supported, without regex backtracking.
pub fn mask(pattern: &str, value: &str) -> bool {
    if pattern.len() > 1024 || value.len() > 16384 {
        return false;
    }
    let p: Vec<_> = pattern.chars().collect();
    let v: Vec<_> = value.chars().collect();
    let (mut a, mut b, mut star, mut retry) = (0, 0, None, 0);
    while b < v.len() {
        if a < p.len() && (p[a] == '?' || p[a] == v[b]) {
            a += 1;
            b += 1;
        } else if a < p.len() && p[a] == '*' {
            star = Some(a);
            a += 1;
            retry = b;
        } else if let Some(s) = star {
            retry += 1;
            b = retry;
            a = s + 1;
        } else {
            return false;
        }
    }
    while a < p.len() && p[a] == '*' {
        a += 1;
    }
    a == p.len()
}
pub fn matches(masks: &str, path: &Path) -> bool {
    let path = path.to_string_lossy().replace('\\', "/");
    let name = path.rsplit('/').next().unwrap_or(&path);
    masks
        .split(';')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .take(128)
        .any(|m| mask(m, if m.contains('/') { &path } else { name }))
}
impl Preferences {
    pub fn association(&self, path: &Path, first_line: &str) -> Option<&Association> {
        self.associations.iter().rev().take(256).find(|a| {
            matches(&a.patterns, path)
                || !a.shebang.is_empty()
                    && first_line.starts_with("#!")
                    && mask(&a.shebang, first_line)
        })
    }
    pub fn excluded_import(&self, name: &str) -> bool {
        self.excluded_imports.iter().take(256).any(|p| {
            name == p
                || name
                    .strip_prefix(p)
                    .is_some_and(|rest| rest.starts_with('.'))
                || mask(p, name)
        })
    }
    pub fn encoding<'a>(&'a self, path: &Path, project: Option<&Path>) -> Option<&'a str> {
        self.encoding_rules
            .iter()
            .filter(|r| {
                r.project
                    .as_deref()
                    .is_none_or(|root| Some(root) == project)
            })
            .filter_map(|r| {
                let root = r.project.as_deref().or(project);
                let base = if r.path.is_absolute() {
                    r.path.clone()
                } else {
                    root.map_or_else(|| r.path.clone(), |root| root.join(&r.path))
                };
                path.starts_with(&base).then_some((
                    base.components().count(),
                    r.project.is_some(),
                    r.encoding.as_str(),
                ))
            })
            .max_by_key(|(length, project, _)| (*length, *project))
            .map(|(_, _, codec)| codec)
            .or_else(|| {
                (!self.encoding_default.is_empty()).then_some(self.encoding_default.as_str())
            })
    }
    pub fn duplicate_key(&self, text: &str) -> String {
        let mut out = String::new();
        let mut vars = BTreeMap::<String, usize>::new();
        let mut chars = text.chars().peekable();
        let mut previous = String::new();
        while let Some(c) = chars.next() {
            if c.is_whitespace() {
                continue;
            }
            if c == '\'' || c == '"' {
                let mut value = String::from(c);
                let mut escaped = false;
                for next in chars.by_ref() {
                    value.push(next);
                    if next == c && !escaped {
                        break;
                    }
                    escaped = next == '\\' && !escaped;
                }
                out.push_str(if self.duplicate_anonymize_literals {
                    "<string>"
                } else {
                    &value
                });
                previous = value;
            } else if c.is_alphanumeric() || c == '_' {
                let mut token = String::from(c);
                while chars
                    .peek()
                    .is_some_and(|next| next.is_alphanumeric() || *next == '_')
                {
                    token.push(chars.next().unwrap());
                }
                let keyword = [
                    "def", "class", "return", "if", "elif", "else", "for", "while", "in", "is",
                    "not", "and", "or", "with", "as", "try", "except", "finally", "raise",
                    "import", "from", "pass", "True", "False", "None", "await", "async", "yield",
                    "lambda", "global", "nonlocal", "del", "assert", "break", "continue", "match",
                    "case",
                ]
                .contains(&token.as_str());
                let replace = if token.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    self.duplicate_anonymize_literals
                } else if previous == "def" || previous == "class" {
                    self.duplicate_anonymize_functions
                } else if previous == "." {
                    self.duplicate_anonymize_fields
                } else {
                    self.duplicate_anonymize_variables && !keyword
                };
                if replace {
                    let next = vars.len();
                    let index = *vars.entry(token.clone()).or_insert(next);
                    out.push_str(&format!("<v{index}>"));
                } else {
                    out.push_str(&token);
                }
                previous = token;
            } else {
                out.push(c);
                previous = c.to_string();
            }
        }
        out
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchemeFont {
    pub enabled: bool,
    pub family: String,
    pub size: f32,
}
impl Default for SchemeFont {
    fn default() -> Self {
        Self {
            enabled: false,
            family: "Hack".into(),
            size: 13.0,
        }
    }
}

#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn masks_import_exclusion_and_project_codec_precedence() {
        let mut prefs = Preferences::default();
        assert!(matches("*.py;*.md", Path::new("dir/name.py")));
        assert!(!matches("*.py", Path::new("x.json")));
        prefs.excluded_imports = vec!["internal".into()];
        assert!(prefs.excluded_import("internal.sub.name"));
        assert!(!prefs.excluded_import("internally"));
        prefs.encoding_default = "utf-8".into();
        prefs.encoding_rules.push(EncodingRule {
            project: Some("/project".into()),
            path: "data".into(),
            encoding: "latin-1".into(),
        });
        assert_eq!(
            prefs.encoding(
                Path::new("/project/data/a.txt"),
                Some(Path::new("/project"))
            ),
            Some("latin-1")
        );
        assert_eq!(
            prefs.encoding(Path::new("/other/a.txt"), Some(Path::new("/other"))),
            Some("utf-8")
        );
    }
    #[test]
    fn duplicate_anonymization_preserves_name_relationships() {
        let prefs = Preferences {
            duplicate_anonymize_variables: true,
            duplicate_anonymize_literals: true,
            ..Default::default()
        };
        assert_eq!(
            prefs.duplicate_key("alpha = alpha + 1"),
            prefs.duplicate_key("beta = beta + 7")
        );
        assert_ne!(
            prefs.duplicate_key("alpha = alpha + 1"),
            prefs.duplicate_key("alpha = beta + 1")
        );
    }
}
