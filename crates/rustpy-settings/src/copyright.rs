//! Copyright profiles and pure, bounded header editing. No filesystem writes.
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdatePolicy {
    #[default]
    ReplaceOrAdd,
    AddMissingOnly,
    UpdateExistingOnly,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommentStyle {
    #[default]
    Line,
    Block,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CopyrightProfile {
    pub name: String,
    pub text: String,
    /// A literal, case-insensitive ownership marker. Empty markers are refused.
    pub marker: String,
    pub policy: UpdatePolicy,
    pub style: CommentStyle,
    pub blank_after: bool,
    pub border: bool,
    /// Wrap notice prose at this many Unicode characters; zero preserves lines.
    pub width: usize,
}
impl Default for CopyrightProfile {
    fn default() -> Self {
        Self {
            name: "Default".into(),
            text: "Copyright (c) $today.year $user. All rights reserved.".into(),
            marker: "copyright".into(),
            policy: UpdatePolicy::ReplaceOrAdd,
            style: CommentStyle::Line,
            blank_after: true,
            border: false,
            width: 0,
        }
    }
}
impl CopyrightProfile {
    /// Preserve old custom notices that used (c), ©, or a plain static title
    /// instead of the default Copyright word. Variable-only notices need an
    /// explicit stable ownership marker in the profile editor.
    pub fn from_legacy(text: &str) -> Self {
        let lower = text.to_lowercase();
        let marker = if lower.contains("copyright") {
            "copyright".to_string()
        } else if lower.contains("(c)") {
            "(c)".to_string()
        } else if text.contains('©') {
            "©".to_string()
        } else {
            text.split_whitespace()
                .find(|word| !word.contains('$'))
                .unwrap_or("copyright")
                .to_string()
        };
        Self {
            text: text.to_string(),
            marker,
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CopyrightScope {
    pub name: String,
    pub profile: String,
    /// Project-relative glob: * and ? stay within a component, ** crosses /.
    pub include: String,
    pub exclude: String,
}
impl Default for CopyrightScope {
    fn default() -> Self {
        Self {
            name: "All files".into(),
            profile: "Default".into(),
            include: "**".into(),
            exclude: String::new(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CopyrightProfilesSettings {
    pub profiles: Vec<CopyrightProfile>,
    pub default_profile: Option<String>,
    /// Ordered rules, first matching rule wins. Empty profile explicitly excludes.
    pub scopes: Vec<CopyrightScope>,
}
fn ambiguous_marker(marker: &str) -> bool {
    let marker = marker.to_lowercase();
    marker.chars().any(|c| c == '\r' || c == '\n')
        || [
            "#",
            "# ---",
            "# ----------------",
            "//",
            "// ---",
            "// ----------------",
            "/*",
            " * ",
            " */",
            "<!--",
            "  ",
            "-->",
        ]
        .iter()
        .any(|delimiter| delimiter.contains(marker.as_str()))
}

impl CopyrightProfilesSettings {
    /// Legacy fallback applies only to a genuinely unconfigured profile set.
    pub fn is_unconfigured(&self) -> bool {
        self.profiles.is_empty() && self.default_profile.is_none() && self.scopes.is_empty()
    }

    pub fn profile_for(
        &self,
        path: &Path,
        project: Option<&Path>,
    ) -> Result<Option<&CopyrightProfile>, String> {
        let relative = project
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path);
        let key = relative.to_string_lossy().replace('\\', "/");
        let name = self
            .scopes
            .iter()
            .find(|rule| {
                glob_matches(&rule.include, &key)
                    && (rule.exclude.is_empty() || !glob_matches(&rule.exclude, &key))
            })
            .map(|rule| rule.profile.as_str())
            .or(self.default_profile.as_deref());
        match name {
            None | Some("") => Ok(None),
            Some(name) => self
                .profiles
                .iter()
                .find(|p| p.name == name)
                .map(Some)
                .ok_or_else(|| format!("Copyright profile '{name}' does not exist")),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        let mut names = std::collections::HashSet::new();
        for profile in &self.profiles {
            if profile.name.trim().is_empty() || !names.insert(&profile.name) {
                return Err("Copyright profile names must be nonempty and unique".into());
            }
            if profile.marker.trim().is_empty() {
                return Err("Copyright ownership marker must not be empty".into());
            }
            if ambiguous_marker(&profile.marker) {
                return Err("Copyright ownership marker must identify notice text, not comment delimiters or multiple lines".into());
            }
            if profile.width > 240 {
                return Err("Copyright wrapping width must be at most 240".into());
            }
        }
        for name in self
            .scopes
            .iter()
            .map(|s| s.profile.as_str())
            .chain(self.default_profile.as_deref())
        {
            if !name.is_empty() && !self.profiles.iter().any(|p| p.name == name) {
                return Err(format!("Copyright profile '{name}' does not exist"));
            }
        }
        Ok(())
    }
}
/// Small dynamic-programming glob matcher with bounded pattern/path inputs.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    if pattern.len() > 1024 || path.len() > 16384 {
        return false;
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = path.chars().collect();
    let mut row = vec![false; t.len() + 1];
    row[0] = true;
    let mut i = 0;
    while i < p.len() {
        let mut next = vec![false; t.len() + 1];
        if p[i] == '*' {
            let deep = p.get(i + 1) == Some(&'*');
            next[0] = row[0];
            for j in 1..=t.len() {
                next[j] = row[j] || next[j - 1] && (deep || t[j - 1] != '/');
            }
            if deep {
                i += 1;
            }
            // **/ also matches zero directory components.
            if deep && p.get(i + 1) == Some(&'/') {
                let mut slash = vec![false; t.len() + 1];
                for j in 1..=t.len() {
                    slash[j] = next[j - 1] && t[j - 1] == '/';
                }
                for j in 0..=t.len() {
                    slash[j] |= row[j];
                }
                next = slash;
                i += 1;
            }
        } else {
            for j in 1..=t.len() {
                next[j] = row[j - 1] && (p[i] == t[j - 1] || p[i] == '?' && t[j - 1] != '/');
            }
        }
        row = next;
        i += 1;
    }
    row[t.len()]
}

pub struct Variables<'a> {
    pub year: &'a str,
    pub date: &'a str,
    pub project: &'a str,
    pub user: &'a str,
    pub file: &'a str,
}
/// Single-pass substitution: values cannot introduce new template directives.
pub fn expand(template: &str, vars: &Variables<'_>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let fields = [
            ("$today.year", vars.year),
            ("$today.date", vars.date),
            ("$project", vars.project),
            ("$user", vars.user),
            ("$file", vars.file),
            ("$$", "$"),
        ];
        if let Some((key, value)) = fields.iter().find(|(key, _)| rest.starts_with(key)) {
            out.push_str(value);
            rest = &rest[key.len()..];
        } else {
            return Err(format!("Unknown copyright variable near {rest}"));
        }
    }
    out.push_str(rest);
    if out.len() > 1024 * 1024 {
        return Err("Copyright notice exceeds 1 MiB".into());
    }
    Ok(out)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyrightEdit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}
#[derive(Clone, Copy)]
enum Syntax {
    Hash,
    Slash,
    BlockOnly,
    Xml,
}
fn syntax(path: &Path) -> Result<Syntax, String> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "py" | "pyi" | "pyw" | "sh" | "rb" | "yaml" | "yml" | "toml" => Ok(Syntax::Hash),
        "js" | "jsx" | "ts" | "tsx" | "c" | "h" | "cpp" | "hpp" | "rs" | "java" | "go" | "kt" => {
            Ok(Syntax::Slash)
        }
        "css" | "scss" => Ok(Syntax::BlockOnly),
        "html" | "htm" | "xml" | "svg" => Ok(Syntax::Xml),
        _ => Err("Copyright comments are unsupported for this file type".into()),
    }
}
/// Whether this filename has a comment syntax supported by the updater.
pub fn supports_comments(path: &Path) -> bool {
    syntax(path).is_ok()
}

fn cookie(line: &str) -> bool {
    let line = line.trim_start();
    if !line.starts_with('#') {
        return false;
    }
    let Some((_, tail)) = line.split_once("coding") else {
        return false;
    };
    let tail = tail.trim_start();
    tail.starts_with(':') || tail.starts_with('=')
}
fn is_header_start(line: &str) -> bool {
    matches!(line.trim(), "# ---" | "// ---") || is_border(line)
}

fn is_border(line: &str) -> bool {
    matches!(line.trim(), "# ----------------" | "// ----------------")
}

fn directive(line: &str) -> bool {
    let text = line.trim_start().to_ascii_lowercase();
    [
        "# pylint:",
        "# noqa",
        "# noinspection",
        "# type:",
        "# fmt:",
        "# isort:",
        "// eslint",
        "// @ts-",
    ]
    .iter()
    .any(|p| text.starts_with(p))
}
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in text.lines() {
        if width == 0 || line.chars().count() <= width {
            result.push(line.to_string());
            continue;
        }
        let mut current = String::new();
        for word in line.split_whitespace() {
            if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > width {
                result.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        result.push(current);
    }
    result
}
fn render(
    profile: &CopyrightProfile,
    notice: &str,
    syntax: Syntax,
    eol: &str,
) -> Result<String, String> {
    if notice.trim().is_empty() {
        return Err("Copyright notice must not be empty".into());
    }
    let lines = wrap(notice, profile.width);
    if (matches!(syntax, Syntax::Hash)
        || matches!((syntax, profile.style), (Syntax::Slash, CommentStyle::Line)))
        && lines
            .iter()
            .any(|line| matches!(line.trim(), "---" | "----------------"))
    {
        return Err(
            "Use the border option instead of a standalone --- or ---------------- notice line"
                .into(),
        );
    }
    let (open, prefix, close) = match (syntax, profile.style) {
        (Syntax::Hash, CommentStyle::Block) => ("# ----------------", "# ", "# ----------------"),
        (Syntax::Hash, _) => (
            if profile.border {
                "# ----------------"
            } else {
                "# ---"
            },
            "# ",
            "# ----------------",
        ),
        (Syntax::BlockOnly, _) | (Syntax::Slash, CommentStyle::Block) => ("/*", " * ", " */"),
        (Syntax::Slash, _) => (
            if profile.border {
                "// ----------------"
            } else {
                "// ---"
            },
            "// ",
            "// ----------------",
        ),
        (Syntax::Xml, _) => ("<!--", "  ", "-->"),
    };
    if matches!(syntax, Syntax::Slash | Syntax::BlockOnly) && notice.contains("*/")
        || matches!(syntax, Syntax::Xml) && notice.contains("--")
    {
        return Err("Copyright text contains a comment terminator".into());
    }
    let mut out = String::new();
    if !open.is_empty() {
        out.push_str(open);
        out.push_str(eol);
    }
    for line in lines {
        out.push_str(prefix);
        out.push_str(&line);
        out.push_str(eol);
    }
    if !close.is_empty() {
        out.push_str(close);
        out.push_str(eol);
    }
    Ok(out)
}
/// Plan only a leading owned comment paragraph, preserving unrelated header
/// paragraphs, shebangs, Python encoding cookies and the entire executable body.
pub fn plan_update(
    source: &str,
    path: &Path,
    profile: &CopyrightProfile,
    vars: &Variables<'_>,
) -> Result<Option<CopyrightEdit>, String> {
    if profile.marker.trim().is_empty() {
        return Err("Copyright ownership marker must not be empty".into());
    }
    if ambiguous_marker(&profile.marker) {
        return Err("Copyright ownership marker must identify notice text, not comment delimiters or multiple lines".into());
    }
    if profile.width > 240 {
        return Err("Copyright wrapping width must be at most 240".into());
    }
    let syntax = syntax(path)?;
    let eol = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let notice = expand(&profile.text, vars)?;
    if !notice
        .to_lowercase()
        .contains(&profile.marker.to_lowercase())
    {
        return Err("Expanded notice must contain its ownership marker".into());
    }
    let mut insertion = if source.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    if matches!(syntax, Syntax::Xml) && source[insertion..].starts_with("<?xml") {
        let Some(end) = source[insertion..].find("?>") else {
            return Err("Unterminated XML declaration".into());
        };
        insertion += end + 2;
        if source[insertion..].starts_with("\r\n") {
            insertion += 2;
        } else if source[insertion..].starts_with('\n') {
            insertion += 1;
        }
    }
    let lines: Vec<(usize, &str)> = source[insertion..]
        .split_inclusive('\n')
        .take(2)
        .scan(insertion, |pos, line| {
            let start = *pos;
            *pos += line.len();
            Some((start, line))
        })
        .collect();
    for (i, (start, line)) in lines.iter().take(2).enumerate() {
        if (i == 0 && line.starts_with("#!")) || cookie(line) {
            insertion = start + line.len();
        }
    }
    if lines.iter().any(|(start, line)| {
        *start < insertion
            && line.trim_start().starts_with('#')
            && !line.starts_with("#!")
            && !cookie(line)
            && line.to_lowercase().contains(&profile.marker.to_lowercase())
    }) {
        return Err("A copyright line precedes the encoding cookie; move the notice after the cookie before updating".into());
    }
    let mut offset = insertion;
    let mut owned = None;
    // Bound header detection; never scan arbitrary body/comment contents.
    while offset < source.len() && offset - insertion < 65536 {
        let rest = &source[offset..];
        let line = rest.split_inclusive('\n').next().unwrap_or(rest);
        if line.trim().is_empty() {
            offset += line.len();
            continue;
        }
        let trimmed = line.trim_start();
        let (start, end) = if trimmed.starts_with("/*") || trimmed.starts_with("<!--") {
            let terminator = if trimmed.starts_with("/*") {
                "*/"
            } else {
                "-->"
            };
            let Some(end) = rest.find(terminator) else {
                return Err("Unterminated header comment".into());
            };
            let mut end = offset + end + terminator.len();
            if source[end..].starts_with("\r\n") {
                end += 2;
            } else if source[end..].starts_with('\n') {
                end += 1;
            }
            (offset, end)
        } else if trimmed.starts_with('#') || trimmed.starts_with("//") {
            let prefix = if trimmed.starts_with('#') { "#" } else { "//" };
            let mut end = offset;
            for line in rest.split_inclusive('\n') {
                if !line.trim_start().starts_with(prefix)
                    || cookie(line)
                    || line.starts_with("#!")
                    || directive(line)
                {
                    break;
                }
                end += line.len();
            }
            (offset, end)
        } else {
            break;
        };
        if source[start..end]
            .to_lowercase()
            .contains(&profile.marker.to_lowercase())
        {
            if owned.is_some() {
                return Err("Multiple copyright headers match; resolve manually".into());
            }
            let mut owned_start = start;
            let mut owned_end = end;
            if !(trimmed.starts_with("/*") || trimmed.starts_with("<!--")) {
                let mut pos = start;
                let mut boundary = None;
                for line in source[start..end].split_inclusive('\n') {
                    if line.to_lowercase().contains(&profile.marker.to_lowercase()) {
                        owned_start = pos;
                        // Unbounded legacy line headers have no reliable end:
                        // replace only the marker line, preserving unknown prose.
                        owned_end = pos + line.len();
                        let mut tail = owned_end;
                        for following in source[owned_end..end].split_inclusive('\n') {
                            tail += following.len();
                            if is_border(following) {
                                if let Some(boundary) = boundary {
                                    owned_start = boundary;
                                    owned_end = tail;
                                }
                                break;
                            }
                        }
                        break;
                    }
                    if is_header_start(line) {
                        boundary = Some(pos);
                    }
                    pos += line.len();
                }
            }
            owned = Some((owned_start, owned_end));
        }
        if end == offset {
            offset += line.len();
            continue;
        }
        offset = end;
    }
    if owned.is_some() && profile.policy == UpdatePolicy::AddMissingOnly
        || owned.is_none() && profile.policy == UpdatePolicy::UpdateExistingOnly
    {
        return Ok(None);
    }
    let mut header = render(profile, &notice, syntax, eol)?;
    let (start, end) = owned.unwrap_or((insertion, insertion));
    if owned.is_none() && insertion > 0 && !source[..insertion].ends_with('\n') {
        header.insert_str(0, eol);
    }
    if profile.blank_after
        && !source[end..].starts_with('\n')
        && !source[end..].starts_with("\r\n")
        && end < source.len()
    {
        header.push_str(eol);
    }
    if source.get(start..end) == Some(header.as_str()) {
        return Ok(None);
    }
    Ok(Some(CopyrightEdit {
        start,
        end,
        replacement: header,
    }))
}
