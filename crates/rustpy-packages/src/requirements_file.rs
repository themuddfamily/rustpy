//! Syntax-only pip requirements files. Includes, variables and URLs are never resolved.
use pep508_rs::{Pep508Url, Requirement as NamedRequirement};
use std::{fmt, ops::Range, path::Path, str::FromStr};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawUrl(pub String);
impl fmt::Display for RawUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl Pep508Url for RawUrl {
    type Err = std::io::Error;
    fn parse_url(value: &str, _: Option<&Path>) -> Result<Self, Self::Err> {
        if value.is_empty() || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty or whitespace-containing URI reference",
            ));
        }
        Ok(Self(value.into()))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptionValue {
    pub name: String,
    pub value: Option<String>,
}
#[derive(Clone, Debug)]
pub enum Target {
    Named(NamedRequirement<RawUrl>),
    Url(String),
    Vcs(String),
    Archive(String),
    LocalPath(String),
}
#[derive(Clone, Debug)]
pub enum Syntax {
    Requirement {
        target: Target,
        requirement: String,
        editable: bool,
        options: Vec<OptionValue>,
    },
    Options(Vec<OptionValue>),
    Invalid,
}
#[derive(Clone, Debug)]
pub struct Record {
    pub line: usize,
    /// Enclosing original physical byte range, including continuation/newline bytes.
    pub range: Range<usize>,
    /// Logical bytes after joining continuations and stripping pip comments.
    pub text: String,
    /// Each logical byte's original source offset; final element maps EOF.
    pub source_offsets: Vec<usize>,
    pub syntax: Syntax,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub message: String,
}
#[derive(Clone, Debug, Default)]
pub struct ParsedFile {
    pub records: Vec<Record>,
    pub diagnostics: Vec<Diagnostic>,
}
const MAX_SOURCE: usize = 2 * 1024 * 1024;
const MAX_LINE: usize = 64 * 1024;

fn comment_start(s: &str) -> usize {
    s.char_indices()
        .find(|&(i, c)| {
            c == '#' && (i == 0 || s[..i].chars().next_back().is_some_and(char::is_whitespace))
        })
        .map_or(s.len(), |(i, _)| i)
}
/// Recover at logical-line boundaries. A malformed entry never becomes a named package.
pub fn parse(text: &str) -> ParsedFile {
    let mut result = ParsedFile::default();
    if text.len() > MAX_SOURCE {
        result.diagnostics.push(Diagnostic {
            range: 0..text.len(),
            message: "requirements file exceeds 2 MiB syntax budget".into(),
        });
        return result;
    }
    let mut pending = String::new();
    let mut offsets = Vec::new();
    let (mut start, mut first, mut base) = (0, 1, 0);
    for (index, physical) in text.split_inclusive('\n').enumerate() {
        let mut content = physical.strip_suffix('\n').unwrap_or(physical);
        content = content.strip_suffix('\r').unwrap_or(content);
        let bom = if base == 0 && content.starts_with('\u{feff}') {
            3
        } else {
            0
        };
        content = &content[bom..];
        if pending.is_empty() && offsets.is_empty() {
            start = base + bom;
            first = index + 1;
        }
        // pip ignores comment-only lines while joining, and removes one final slash.
        let comment_only = content.trim_start().starts_with('#');
        let continued = !comment_only && content.ends_with('\\');
        let end = content.len() - usize::from(continued);
        if !comment_only {
            pending.push_str(&content[..end]);
            offsets.extend((base + bom)..(base + bom + end));
        }
        base += physical.len();
        if continued || (comment_only && !offsets.is_empty()) {
            continue;
        }
        finish(&mut result, &mut pending, &mut offsets, start..base, first);
    }
    if !pending.is_empty() || !offsets.is_empty() {
        result.diagnostics.push(Diagnostic {
            range: start..base,
            message: "unterminated line continuation".into(),
        });
        let record_index = result.records.len();
        finish(&mut result, &mut pending, &mut offsets, start..base, first);
        if let Some(record) = result.records.get_mut(record_index) {
            record.syntax = Syntax::Invalid;
        }
    }
    result
}
fn finish(
    out: &mut ParsedFile,
    pending: &mut String,
    offsets: &mut Vec<usize>,
    range: Range<usize>,
    line: usize,
) {
    let cut = comment_start(pending);
    let trimmed = pending[..cut].trim();
    let begin = pending[..cut].len() - pending[..cut].trim_start().len();
    let end = begin + trimmed.len();
    if trimmed.is_empty() {
        pending.clear();
        offsets.clear();
        return;
    }
    let mut source_offsets = offsets[begin..end].to_vec();
    source_offsets.push(offsets.get(end).copied().unwrap_or(range.end));
    let mut record = Record {
        line,
        range,
        text: trimmed.into(),
        source_offsets,
        syntax: Syntax::Invalid,
    };
    match if record.text.len() > MAX_LINE {
        Err("logical line exceeds 64 KiB syntax budget".into())
    } else {
        parse_line(&record.text)
    } {
        Ok(syntax) if record.text.len() <= MAX_LINE => record.syntax = syntax,
        Ok(_) => out.diagnostics.push(Diagnostic {
            range: record.range.clone(),
            message: "logical line exceeds 64 KiB syntax budget".into(),
        }),
        Err(message) => out.diagnostics.push(Diagnostic {
            range: record.range.clone(),
            message,
        }),
    }
    out.records.push(record);
    pending.clear();
    offsets.clear();
}
// Split pip's option tail, not PEP 508 quoted marker strings or quoted paths.
fn tokens(s: &str) -> Result<Vec<(usize, usize, String)>, String> {
    let mut out = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut value = String::new();
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\'
                && q == '"'
                && chars.peek().is_some_and(|(_, n)| *n == '"' || *n == '\\')
            {
                value.push(chars.next().expect("peeked character").1);
            } else {
                value.push(c);
            }
        } else if c.is_whitespace() {
            if let Some(a) = start.take() {
                out.push((a, i, std::mem::take(&mut value)));
            }
        } else {
            start.get_or_insert(i);
            if c == '\'' || c == '"' {
                quote = Some(c);
            } else {
                value.push(c);
            }
        }
    }
    if quote.is_some() {
        return Err("unterminated quoted option or marker".into());
    }
    if let Some(a) = start {
        out.push((a, s.len(), value));
    }
    Ok(out)
}
fn option_kind(name: &str) -> Option<(bool, bool)> {
    // (takes value, per-requirement). Unknown options are diagnosed, never executed.
    Some(match name {
        "-r" | "--requirement" | "-c" | "--constraint" | "-e" | "--editable" | "-i"
        | "--index-url" | "--pypi-url" | "--extra-index-url" | "-f" | "--find-links"
        | "--trusted-host" | "--use-feature" | "--use-deprecated" | "--only-binary"
        | "--no-binary" => (true, false),
        "--hash" | "--config-settings" | "-C" | "--global-option" => (true, true),
        "--no-index" | "--pre" | "--prefer-binary" | "--require-hashes" => (false, false),
        _ => return None,
    })
}
fn parse_options(ts: &[(usize, usize, String)]) -> Result<Vec<OptionValue>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ts.len() {
        let token = &ts[i].2;
        let (mut name, mut value) = token
            .split_once('=')
            .map_or((token.as_str(), None), |(a, b)| (a, Some(b.to_string())));
        if option_kind(name).is_none() && token.len() > 2 {
            let short = token.get(..2).unwrap_or("");
            if matches!(short, "-r" | "-c" | "-e" | "-i" | "-f" | "-C") {
                name = short;
                value = Some(token[2..].into());
            }
        }
        let (takes, _) =
            option_kind(name).ok_or_else(|| format!("unsupported requirements option {name}"))?;
        if takes && value.is_none() {
            i += 1;
            value = Some(
                ts.get(i)
                    .filter(|t| !t.2.starts_with('-'))
                    .ok_or_else(|| format!("missing value for {name}"))?
                    .2
                    .clone(),
            );
        }
        if !takes && value.is_some() {
            return Err(format!("{name} does not take a value"));
        }
        if value.as_ref().is_some_and(String::is_empty) {
            return Err(format!("empty value for {name}"));
        }
        if name == "--hash"
            && !value.as_ref().is_some_and(|v| {
                v.split_once(':').is_some_and(|(a, b)| {
                    !a.is_empty() && !b.is_empty() && b.bytes().all(|c| c.is_ascii_hexdigit())
                })
            })
        {
            return Err("hash must be algorithm:hex-digest".into());
        }
        if matches!(name, "--config-settings" | "-C")
            && !value
                .as_ref()
                .is_some_and(|v| v.split_once('=').is_some_and(|(key, _)| !key.is_empty()))
        {
            return Err("config setting must be KEY=VALUE".into());
        }
        out.push(OptionValue {
            name: name.into(),
            value,
        });
        i += 1;
    }
    Ok(out)
}
fn classify(value: &str) -> Target {
    let lower = value.to_ascii_lowercase();
    if ["git+", "hg+", "svn+", "bzr+"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        Target::Vcs(value.into())
    } else if lower.contains("://") || lower.starts_with("file:") {
        Target::Url(value.into())
    } else if [
        ".whl",
        ".zip",
        ".tar.gz",
        ".tar.bz2",
        ".tar.xz",
        ".tgz",
        ".tar",
        ".tbz",
        ".txz",
        ".tlz",
        ".tar.lz",
        ".tar.lzma",
    ]
    .iter()
    .any(|s| lower.ends_with(s))
    {
        Target::Archive(value.into())
    } else {
        Target::LocalPath(value.into())
    }
}
fn is_unnamed(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.contains('/')
        || value.contains('\\')
        || value.starts_with('.')
        || value.starts_with('~')
        || value.contains("://")
        || lower.starts_with("file:")
        || [
            ".whl",
            ".zip",
            ".tar.gz",
            ".tar.bz2",
            ".tar.xz",
            ".tgz",
            ".tar",
            ".tbz",
            ".txz",
            ".tlz",
            ".tar.lz",
            ".tar.lzma",
        ]
        .iter()
        .any(|s| lower.ends_with(s))
}
fn validate_budget(s: &str) -> Result<(), String> {
    if s.len() > MAX_LINE {
        return Err("logical line exceeds 64 KiB syntax budget".into());
    }
    let mut quote = None;
    let mut depth = 0usize;
    for c in s.chars() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if c == '(' {
            depth += 1;
            if depth > 64 {
                return Err("marker nesting exceeds 64 levels".into());
            }
        } else if c == ')' {
            depth = depth.saturating_sub(1);
        }
    }
    Ok(())
}
/// Shared bounded named validation for file parsing and the compatibility adapter.
pub fn parse_named(s: &str) -> Result<NamedRequirement<RawUrl>, String> {
    validate_budget(s)?;
    NamedRequirement::<RawUrl>::from_str(s)
        .map_err(|error| format!("invalid PEP 508 requirement: {error}"))
}
fn parse_line(s: &str) -> Result<Syntax, String> {
    validate_budget(s)?;
    let ts = tokens(s)?;
    if s.starts_with('-') {
        let opts = parse_options(&ts)?;
        if let Some(edit) = opts
            .iter()
            .find(|o| matches!(o.name.as_str(), "-e" | "--editable"))
        {
            if opts
                .iter()
                .filter(|o| matches!(o.name.as_str(), "-e" | "--editable"))
                .count()
                != 1
                || opts.iter().any(|o| {
                    !matches!(o.name.as_str(), "-e" | "--editable")
                        && !option_kind(&o.name).is_some_and(|(_, p)| p)
                })
            {
                return Err("editable target cannot be combined with global options".into());
            }
            return Ok(Syntax::Requirement {
                target: classify(edit.value.as_deref().expect("valued option")),
                requirement: edit.value.clone().expect("valued option"),
                editable: true,
                options: opts
                    .into_iter()
                    .filter(|o| !matches!(o.name.as_str(), "-e" | "--editable"))
                    .collect(),
            });
        }
        if opts
            .iter()
            .any(|o| option_kind(&o.name).is_some_and(|(_, p)| p))
        {
            return Err("per-requirement option has no requirement".into());
        }
        return Ok(Syntax::Options(opts));
    }
    let split = ts
        .iter()
        .position(|t| s[t.0..t.1].starts_with('-'))
        .unwrap_or(ts.len());
    let options = parse_options(&ts[split..])?;
    if options
        .iter()
        .any(|o| !option_kind(&o.name).is_some_and(|(_, p)| p))
    {
        return Err("global option attached to requirement".into());
    }
    let end = ts.get(split).map_or(s.len(), |t| t.0);
    let req = s[..end].trim_end();
    // Prefer PEP 508: a named direct reference also contains slash characters.
    let target = match parse_named(req) {
        Ok(named) => Target::Named(named),
        Err(_) if is_unnamed(req) && ts[..split].len() == 1 => classify(&ts[0].2),
        Err(error) => return Err(format!("invalid PEP 508 requirement: {error}")),
    };
    Ok(Syntax::Requirement {
        target,
        requirement: req.into(),
        editable: false,
        options,
    })
}
