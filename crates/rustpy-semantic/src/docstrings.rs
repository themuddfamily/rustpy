//! Docstring tools (roadmap §2.5 Quick Documentation, §2.6 docstring format):
//! format detection, parsing of Plain / reStructuredText / Epytext / Google /
//! NumPy docstrings, stub generation and updating for functions, consistency
//! problems between a docstring and the signature, and Markdown rendering.

use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;
use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};

use crate::db::{Analysis, FileInfo};

mod signature;
pub mod type_information;
pub(crate) use signature::parameter_name_ranges;
pub use signature::synchronize_signature;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DocstringFormat {
    Plain,
    #[default]
    ReStructuredText,
    Epytext,
    Google,
    NumPy,
}

impl DocstringFormat {
    pub fn label(self) -> &'static str {
        match self {
            DocstringFormat::Plain => "Plain",
            DocstringFormat::ReStructuredText => "reStructuredText",
            DocstringFormat::Epytext => "Epytext",
            DocstringFormat::Google => "Google",
            DocstringFormat::NumPy => "NumPy",
        }
    }

    pub fn all() -> [DocstringFormat; 5] {
        [
            DocstringFormat::Plain,
            DocstringFormat::ReStructuredText,
            DocstringFormat::Epytext,
            DocstringFormat::Google,
            DocstringFormat::NumPy,
        ]
    }
}

/// Structured view of a docstring.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedDocstring {
    pub format: DocstringFormat,
    pub summary: String,
    pub description: String,
    /// `(name, type, description)`.
    pub params: Vec<(String, Option<String>, String)>,
    /// `(type, description)`.
    pub returns: Option<(Option<String>, String)>,
    /// `(exception, description)`.
    pub raises: Vec<(String, String)>,
    pub examples: Vec<String>,
    /// `(name, description)`.
    pub attributes: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

pub(crate) static REST_FIELD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*:(param|parameter|arg|argument|key|keyword|kwarg|kwparam|type|return|returns|rtype|raise|raises|except|exception|ivar|var|cvar|attr|attribute|yield|yields|ytype)\b").unwrap()
});
pub(crate) static EPY_FIELD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*@(param|type|return|rtype|raise|raises|except|exception|ivar|cvar|keyword|kwarg|attention|note|see|sort|summary|undocumented|yield|ytype)\b").unwrap()
});
pub(crate) static GOOGLE_SECTION: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(\s*)(Args|Arguments|Parameters|Params|Keyword Args|Keyword Arguments|Kwargs|Other Parameters|Returns|Return|Yields|Yield|Raises|Raise|Except|Exceptions|Attributes|Example|Examples|Note|Notes|See Also|Todo|References|Warning|Warnings|Methods):\s*$").unwrap()
});
pub(crate) static NUMPY_SECTION: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*(Parameters|Other Parameters|Returns|Yields|Raises|Warns|Attributes|Methods|Examples|Example|Notes|Note|See Also|References|Warnings)\s*$").unwrap()
});
static FIELD_LINE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(\s*)[:@](\w+)(?:\s+([^:]+?))?\s*:\s?(.*)$").unwrap());
// Python names use Unicode XID boundaries, including combining continuations.
// These shared patterns also drive rendered doc blocks, not only popup Markdown.
pub(crate) static GOOGLE_ARG: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(\*{0,2}[_\p{XID_Start}][\p{XID_Continue}]*)\s*(?:\(([^)]*)\))?\s*:\s*(.*)$")
        .unwrap()
});
pub(crate) static GOOGLE_RAISE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^([_\p{XID_Start}][\p{XID_Continue}]*(?:\.[_\p{XID_Start}][\p{XID_Continue}]*)*)\s*:\s*(.*)$").unwrap()
});
static ROLE: Lazy<Regex> = Lazy::new(|| Regex::new(r":[a-zA-Z_.]+:`([^`]+)`").unwrap());
static LINK: Lazy<Regex> = Lazy::new(|| Regex::new(r"`([^`<]+?)\s*<([^>]+)>`_").unwrap());

pub(crate) fn is_underline(line: &str, ch: char) -> bool {
    let t = line.trim();
    t.len() >= 3 && t.chars().all(|c| c == ch)
}

/// Format of a single docstring (`Plain` when it carries no markup).
pub fn detect_one(doc: &str) -> DocstringFormat {
    let lines = clean(doc);
    for (i, l) in lines.iter().enumerate() {
        if REST_FIELD.is_match(l) {
            return DocstringFormat::ReStructuredText;
        }
        if EPY_FIELD.is_match(l) {
            return DocstringFormat::Epytext;
        }
        if NUMPY_SECTION.is_match(l)
            && lines
                .get(i + 1)
                .is_some_and(|n| is_underline(n, '-') || is_underline(n, '='))
        {
            return DocstringFormat::NumPy;
        }
    }
    if lines.iter().any(|l| GOOGLE_SECTION.is_match(l)) {
        return DocstringFormat::Google;
    }
    DocstringFormat::Plain
}

/// Module-level guess from the existing docstrings in `text` (the most common
/// marked-up format; `Plain` when none has markup).
pub fn detect(text: &str) -> DocstringFormat {
    let parse = rustpy_syntax::parse(text);
    let root = parse.syntax_node();
    let mut counts: [usize; 5] = [0; 5];
    let mut tally = |doc: Option<ast::StringLike>| {
        if let Some(d) = doc {
            let f = detect_one(&d.value());
            counts[f as usize] += 1;
        }
    };
    if let Some(m) = ast::Module::cast(root.clone()) {
        tally(m.docstring());
    }
    for n in root.descendants() {
        match n.kind() {
            SyntaxKind::FUNC_DEF => tally(ast::FuncDef::cast(n).and_then(|f| f.docstring())),
            SyntaxKind::CLASS_DEF => tally(
                ast::ClassDef::cast(n)
                    .and_then(|c| c.body())
                    .and_then(|b| b.docstring()),
            ),
            _ => {}
        }
    }
    let order = [
        DocstringFormat::ReStructuredText,
        DocstringFormat::Google,
        DocstringFormat::NumPy,
        DocstringFormat::Epytext,
    ];
    let mut best = DocstringFormat::Plain;
    let mut best_n = 0;
    for f in order {
        if counts[f as usize] > best_n {
            best_n = counts[f as usize];
            best = f;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// `inspect.cleandoc`: strips the common indentation and surrounding blank lines.
pub fn clean(doc: &str) -> Vec<String> {
    let mut lines: Vec<String> = doc
        .replace("\r\n", "\n")
        .split('\n')
        .map(|l| l.trim_end().to_string())
        .collect();
    if let Some(first) = lines.first_mut() {
        *first = first.trim_start().to_string();
    }
    let indent = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    for l in lines.iter_mut().skip(1) {
        *l = strip_indent(l, indent).to_string();
    }
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
}

fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

/// `l` without its first `n` bytes of leading whitespace. Never splits a
/// character: indentation made of multi-byte whitespace (U+3000, …) is cut
/// at a character boundary.
fn strip_indent(l: &str, n: usize) -> &str {
    let mut cut = 0;
    for (i, c) in l.char_indices() {
        if i >= n || !c.is_whitespace() {
            break;
        }
        cut = i + c.len_utf8();
    }
    &l[cut..]
}

/// Summary (first paragraph) and the index of the first line after it.
fn split_summary(lines: &[String], is_marker: &dyn Fn(usize) -> bool) -> (String, usize) {
    let mut summary: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim().is_empty() || is_marker(i) {
            break;
        }
        summary.push(lines[i].trim());
        i += 1;
    }
    (summary.join(" "), i)
}

/// Doctest blocks (`>>>` lines until a blank line) as examples; returns the remaining lines.
fn extract_doctests(lines: &[String]) -> (Vec<String>, Vec<String>) {
    let mut examples = Vec::new();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with(">>>") {
            let mut block = Vec::new();
            while i < lines.len() && !lines[i].trim().is_empty() {
                block.push(lines[i].trim().to_string());
                i += 1;
            }
            examples.push(block.join("\n"));
        } else {
            rest.push(lines[i].clone());
            i += 1;
        }
    }
    (examples, rest)
}

fn join_desc(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// reST (`:param x:`) and Epytext (`@param x:`) field lists.
fn parse_fields(lines: &[String], out: &mut ParsedDocstring) {
    let is_marker = |i: usize| {
        FIELD_LINE.is_match(&lines[i])
            && (REST_FIELD.is_match(&lines[i]) || EPY_FIELD.is_match(&lines[i]))
    };
    let (summary, start) = split_summary(lines, &is_marker);
    out.summary = summary;
    let mut description: Vec<String> = Vec::new();
    #[derive(Clone, Copy, PartialEq)]
    enum Target {
        Param(usize),
        ParamType(usize),
        Return,
        ReturnType,
        Raise(usize),
        Attr(usize),
        Ignore,
    }
    let mut current: Option<(Target, usize)> = None;
    let mut i = start;
    while i < lines.len() {
        let line = &lines[i];
        if is_marker(i) {
            let caps = FIELD_LINE.captures(line).unwrap();
            let field_indent = caps.get(1).map(|m| m.as_str().len()).unwrap_or(0);
            let field = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let arg = caps
                .get(3)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let desc = caps
                .get(4)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let target = match field {
                "param" | "parameter" | "arg" | "argument" | "key" | "keyword" | "kwarg"
                | "kwparam" => {
                    // `:param int x:` → type + name.
                    let (ty, name) = match arg.rsplit_once(' ') {
                        Some((t, n)) => (Some(t.trim().to_string()), n.trim().to_string()),
                        None => (None, arg.clone()),
                    };
                    let name = name.trim_start_matches('*').to_string();
                    let idx = match out.params.iter().position(|p| p.0 == name) {
                        Some(idx) => {
                            if ty.is_some() {
                                out.params[idx].1 = ty;
                            }
                            idx
                        }
                        None => {
                            out.params.push((name, ty, String::new()));
                            out.params.len() - 1
                        }
                    };
                    out.params[idx].2 = desc.clone();
                    Target::Param(idx)
                }
                "type" => {
                    let name = arg.trim_start_matches('*').to_string();
                    let idx = match out.params.iter().position(|p| p.0 == name) {
                        Some(idx) => idx,
                        None => {
                            out.params.push((name, None, String::new()));
                            out.params.len() - 1
                        }
                    };
                    out.params[idx].1 = Some(desc.clone());
                    Target::ParamType(idx)
                }
                "return" | "returns" | "yield" | "yields" => {
                    let r = out.returns.get_or_insert((None, String::new()));
                    r.1 = desc.clone();
                    Target::Return
                }
                "rtype" | "ytype" => {
                    let r = out.returns.get_or_insert((None, String::new()));
                    r.0 = Some(desc.clone());
                    Target::ReturnType
                }
                "raise" | "raises" | "except" | "exception" => {
                    out.raises.push((arg.clone(), desc.clone()));
                    Target::Raise(out.raises.len() - 1)
                }
                "ivar" | "var" | "cvar" | "attr" | "attribute" => {
                    out.attributes.push((arg.clone(), desc.clone()));
                    Target::Attr(out.attributes.len() - 1)
                }
                _ => Target::Ignore,
            };
            current = Some((target, field_indent));
            i += 1;
            continue;
        }
        if line.trim().is_empty() {
            if current.is_none() {
                description.push(String::new());
            }
            current = None;
            i += 1;
            continue;
        }
        match current {
            Some((target, field_indent)) if indent_of(line) > field_indent => {
                let extra = line.trim().to_string();
                let append = |s: &mut String| {
                    if !s.is_empty() {
                        s.push(' ');
                    }
                    s.push_str(&extra);
                };
                match target {
                    Target::Param(idx) => append(&mut out.params[idx].2),
                    Target::ParamType(idx) => {
                        let t = out.params[idx].1.get_or_insert_with(String::new);
                        append(t);
                    }
                    Target::Return => {
                        append(&mut out.returns.get_or_insert((None, String::new())).1)
                    }
                    Target::ReturnType => {
                        let r = out.returns.get_or_insert((None, String::new()));
                        append(r.0.get_or_insert_with(String::new));
                    }
                    Target::Raise(idx) => append(&mut out.raises[idx].1),
                    Target::Attr(idx) => append(&mut out.attributes[idx].1),
                    Target::Ignore => {}
                }
            }
            _ => {
                // A field continuation ends at unindented prose. Otherwise a
                // later indented paragraph can corrupt the preceding type.
                current = None;
                description.push(line.clone());
            }
        }
        i += 1;
    }
    let (examples, rest) = extract_doctests(&description);
    out.examples.extend(examples);
    out.description = trim_blank_lines(&rest).join("\n");
}

fn trim_blank_lines(lines: &[String]) -> Vec<String> {
    let mut v: Vec<String> = lines.to_vec();
    while v.first().is_some_and(|l| l.trim().is_empty()) {
        v.remove(0);
    }
    while v.last().is_some_and(|l| l.trim().is_empty()) {
        v.pop();
    }
    v
}

/// Splits a section body into entries: an entry starts at the smallest indent
/// of the block; deeper lines continue it.
fn section_entries(body: &[String]) -> Vec<(String, Vec<String>)> {
    let base = body
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| indent_of(l))
        .min()
        .unwrap_or(0);
    let mut entries: Vec<(String, Vec<String>)> = Vec::new();
    for l in body {
        if l.trim().is_empty() {
            if let Some(e) = entries.last_mut() {
                e.1.push(String::new());
            }
            continue;
        }
        if indent_of(l) <= base {
            entries.push((l.trim().to_string(), Vec::new()));
        } else if let Some(e) = entries.last_mut() {
            e.1.push(l.trim().to_string());
        } else {
            entries.push((l.trim().to_string(), Vec::new()));
        }
    }
    entries
}

pub(crate) fn looks_like_type(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && !s.contains(' ')
        || s.chars()
            .all(|c| c.is_alphanumeric() || " _.,[]|()".contains(c))
            && s.split_whitespace().count() <= 3
            && !s.ends_with('.')
}

fn parse_google(lines: &[String], out: &mut ParsedDocstring) {
    let is_marker = |i: usize| GOOGLE_SECTION.is_match(&lines[i]);
    let (summary, start) = split_summary(lines, &is_marker);
    out.summary = summary;
    let mut description: Vec<String> = Vec::new();
    let mut i = start;
    // Collect sections as (name, body lines).
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    while i < lines.len() {
        if let Some(c) = GOOGLE_SECTION.captures(&lines[i]) {
            let name = c.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
            let header_indent = c.get(1).map(|m| m.as_str().len()).unwrap_or(0);
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() {
                let l = &lines[i];
                if GOOGLE_SECTION.is_match(l) && indent_of(l) <= header_indent {
                    break;
                }
                if !l.trim().is_empty() && indent_of(l) <= header_indent {
                    break;
                }
                body.push(l.clone());
                i += 1;
            }
            sections.push((name, body));
            continue;
        }
        description.push(lines[i].clone());
        i += 1;
    }
    for (name, body) in sections {
        let body = trim_blank_lines(&body);
        match name.as_str() {
            "Args" | "Arguments" | "Parameters" | "Params" | "Keyword Args"
            | "Keyword Arguments" | "Kwargs" | "Other Parameters" => {
                for (head, cont) in section_entries(&body) {
                    if let Some(c) = GOOGLE_ARG.captures(&head) {
                        let pname = c
                            .get(1)
                            .map(|m| m.as_str().trim_start_matches('*').to_string())
                            .unwrap_or_default();
                        let ty = c
                            .get(2)
                            .map(|m| m.as_str().trim().to_string())
                            .map(|t| {
                                t.trim_end_matches(", optional")
                                    .trim_end_matches(",optional")
                                    .to_string()
                            })
                            .filter(|t| !t.is_empty());
                        let mut desc = c
                            .get(3)
                            .map(|m| m.as_str().trim().to_string())
                            .unwrap_or_default();
                        let more = join_desc(&cont);
                        if !more.is_empty() {
                            if !desc.is_empty() {
                                desc.push(' ');
                            }
                            desc.push_str(&more);
                        }
                        out.params.push((pname, ty, desc));
                    }
                }
            }
            "Returns" | "Return" | "Yields" | "Yield" => {
                let text = join_desc(&body);
                let (ty, desc) = match type_information::typed_entry(&text) {
                    Some((t, d))
                        if type_information::normalize(t)
                            .is_some_and(|ty| looks_like_type(&ty)) =>
                    {
                        (Some(t.trim().to_string()), d.trim().to_string())
                    }
                    _ => (None, text),
                };
                out.returns = Some((ty, desc));
            }
            "Raises" | "Raise" | "Except" | "Exceptions" => {
                for (head, cont) in section_entries(&body) {
                    if let Some(c) = GOOGLE_RAISE.captures(&head) {
                        let mut desc = c
                            .get(2)
                            .map(|m| m.as_str().trim().to_string())
                            .unwrap_or_default();
                        let more = join_desc(&cont);
                        if !more.is_empty() {
                            if !desc.is_empty() {
                                desc.push(' ');
                            }
                            desc.push_str(&more);
                        }
                        out.raises.push((
                            c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default(),
                            desc,
                        ));
                    } else {
                        out.raises.push((head, join_desc(&cont)));
                    }
                }
            }
            "Attributes" | "Methods" => {
                for (head, cont) in section_entries(&body) {
                    if let Some(c) = GOOGLE_ARG.captures(&head) {
                        let mut desc = c
                            .get(3)
                            .map(|m| m.as_str().trim().to_string())
                            .unwrap_or_default();
                        let more = join_desc(&cont);
                        if !more.is_empty() {
                            if !desc.is_empty() {
                                desc.push(' ');
                            }
                            desc.push_str(&more);
                        }
                        out.attributes.push((
                            c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default(),
                            desc,
                        ));
                    } else {
                        out.attributes.push((head, join_desc(&cont)));
                    }
                }
            }
            "Example" | "Examples" => {
                let dedented = dedent(&body);
                if !dedented.trim().is_empty() {
                    out.examples.push(dedented);
                }
            }
            other => {
                description.push(String::new());
                description.push(format!("{other}: {}", join_desc(&body)));
            }
        }
    }
    let (examples, rest) = extract_doctests(&description);
    out.examples.extend(examples);
    out.description = trim_blank_lines(&rest).join("\n");
}

fn dedent(lines: &[String]) -> String {
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| indent_of(l))
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| strip_indent(l, indent).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_numpy(lines: &[String], out: &mut ParsedDocstring) {
    let is_header = |i: usize| {
        NUMPY_SECTION.is_match(&lines[i])
            && lines
                .get(i + 1)
                .is_some_and(|n| is_underline(n, '-') || is_underline(n, '='))
    };
    let (summary, start) = split_summary(lines, &is_header);
    out.summary = summary;
    let mut description: Vec<String> = Vec::new();
    let mut i = start;
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    while i < lines.len() {
        if is_header(i) {
            let name = lines[i].trim().to_string();
            let mut body = Vec::new();
            i += 2;
            while i < lines.len() && !is_header(i) {
                body.push(lines[i].clone());
                i += 1;
            }
            sections.push((name, body));
            continue;
        }
        description.push(lines[i].clone());
        i += 1;
    }
    for (name, body) in sections {
        let body = trim_blank_lines(&body);
        match name.as_str() {
            "Parameters" | "Other Parameters" => {
                for (head, cont) in section_entries(&body) {
                    let (pname, ty) = match head.split_once(" : ") {
                        Some((n, t)) => (n.trim().to_string(), Some(t.trim().to_string())),
                        None => match head.split_once(':') {
                            Some((n, t)) if !n.trim().contains(' ') => {
                                (n.trim().to_string(), Some(t.trim().to_string()))
                            }
                            _ => (head.trim().to_string(), None),
                        },
                    };
                    let names: Vec<String> = pname
                        .split(',')
                        .map(|n| n.trim().trim_start_matches('*').to_string())
                        .filter(|n| !n.is_empty())
                        .collect();
                    for n in names {
                        out.params.push((
                            n,
                            ty.clone().filter(|t| !t.is_empty()),
                            join_desc(&cont),
                        ));
                    }
                }
            }
            "Returns" | "Yields" => {
                let entries = section_entries(&body);
                if let Some((head, cont)) = entries.first() {
                    let entry_type = |head: &str| {
                        head.split_once(':')
                            .filter(|(name, _)| {
                                !name.trim().is_empty()
                                    && name.trim().chars().all(rustpy_core::is_ident_continue)
                            })
                            .map_or(head, |(_, ty)| ty)
                            .trim()
                            .to_string()
                    };
                    let ty = if entries.len() == 1 {
                        entry_type(head)
                    } else {
                        format!(
                            "tuple[{}]",
                            entries
                                .iter()
                                .map(|(head, _)| entry_type(head))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    out.returns = Some((Some(ty).filter(|t| !t.is_empty()), join_desc(cont)));
                }
            }
            "Raises" | "Warns" => {
                for (head, cont) in section_entries(&body) {
                    out.raises.push((head.trim().to_string(), join_desc(&cont)));
                }
            }
            "Attributes" | "Methods" => {
                for (head, cont) in section_entries(&body) {
                    let n = head.split(" : ").next().unwrap_or(&head).trim().to_string();
                    out.attributes.push((n, join_desc(&cont)));
                }
            }
            "Examples" | "Example" => {
                let text = dedent(&body);
                if !text.trim().is_empty() {
                    out.examples.push(text);
                }
            }
            other => {
                description.push(String::new());
                description.push(format!("{other}: {}", join_desc(&body)));
            }
        }
    }
    let (examples, rest) = extract_doctests(&description);
    out.examples.extend(examples);
    out.description = trim_blank_lines(&rest).join("\n");
}

fn parse_plain(lines: &[String], out: &mut ParsedDocstring) {
    let (summary, start) = split_summary(lines, &|_| false);
    out.summary = summary;
    let (examples, rest) = extract_doctests(&lines[start.min(lines.len())..]);
    out.examples = examples;
    let (params, rest) = clinic_params(&rest);
    out.params = params;
    out.description = trim_blank_lines(&rest).join("\n");
}

/// A documented parameter: `(name, type, description)`.
type ParamDoc = (String, Option<String>, String);

/// Parameters listed the way builtins and C modules document them (Argument
/// Clinic: `print`, `str.split`, `int.to_bytes`): after a blank line, each
/// name alone on a line with its description indented below it.
///
/// ```text
///   sep
///     The separator used to split the string.
///   maxsplit
///     Maximum number of splits.
/// ```
///
/// Returns the parameters and the lines outside such lists.
fn clinic_params(lines: &[String]) -> (Vec<ParamDoc>, Vec<String>) {
    let blank = |i: usize| lines.get(i).is_none_or(|l| l.trim().is_empty());
    let deeper = |i: usize, k: usize| {
        lines
            .get(i)
            .is_some_and(|l| !l.trim().is_empty() && indent_of(l) > k)
    };
    // A parameter name (lowercase, unlike section titles such as `Returns`)
    // at indentation `k`, followed directly by its description.
    let item_at = |i: usize, k: usize| {
        let line = &lines[i];
        let name = line.trim().trim_start_matches('*');
        indent_of(line) == k
            && line.trim().len() - name.len() <= 2
            && name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && deeper(i + 1, k)
    };
    let mut params = Vec::new();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let k = indent_of(&lines[i]);
        if !((i == 0 || blank(i - 1)) && item_at(i, k)) {
            rest.push(lines[i].clone());
            i += 1;
            continue;
        }
        while i < lines.len() && item_at(i, k) {
            let name = lines[i].trim().to_string();
            i += 1;
            let mut desc: Vec<String> = Vec::new();
            // The description, including blank lines between its paragraphs.
            while deeper(i, k) || (blank(i) && i < lines.len() && deeper(i + 1, k)) {
                desc.push(lines[i].clone());
                i += 1;
            }
            params.push((name, None, join_desc(&desc)));
        }
    }
    (params, rest)
}

/// Parses `doc` in the given format.
pub fn parse_with(doc: &str, format: DocstringFormat) -> ParsedDocstring {
    let lines = clean(doc);
    let mut out = ParsedDocstring {
        format,
        ..Default::default()
    };
    match format {
        DocstringFormat::ReStructuredText | DocstringFormat::Epytext => {
            parse_fields(&lines, &mut out)
        }
        DocstringFormat::Google => parse_google(&lines, &mut out),
        DocstringFormat::NumPy => parse_numpy(&lines, &mut out),
        DocstringFormat::Plain => parse_plain(&lines, &mut out),
    }
    out
}

/// Parses `doc`, detecting its format.
pub fn parse(doc: &str) -> ParsedDocstring {
    parse_with(doc, detect_one(doc))
}

// ---------------------------------------------------------------------------
// Markdown rendering (Quick Documentation)
// ---------------------------------------------------------------------------

/// Inline markup → Markdown: roles, double backticks, reST links.
fn inline_md(s: &str) -> String {
    let s = ROLE.replace_all(s, "`$1`").to_string();
    let s = LINK.replace_all(&s, "[$1]($2)").to_string();
    s.replace("``", "`")
}

/// Paragraph text → Markdown, turning `::` literal blocks and doctests into code fences.
fn body_md(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let literal = l.trim_end().ends_with("::");
        if literal {
            let head = l.trim_end().trim_end_matches("::").trim_end();
            if !head.is_empty() {
                out.push(format!("{}:", inline_md(head)));
            }
            i += 1;
            while i < lines.len() && lines[i].trim().is_empty() {
                i += 1;
            }
            let mut block = Vec::new();
            while i < lines.len() && (lines[i].trim().is_empty() || indent_of(lines[i]) > 0) {
                block.push(lines[i].to_string());
                i += 1;
            }
            let block: Vec<String> = trim_blank_lines(&block);
            out.push(String::new());
            out.push("```".into());
            out.push(dedent(&block));
            out.push("```".into());
            out.push(String::new());
            continue;
        }
        if l.trim_start().starts_with(">>>") {
            let mut block = Vec::new();
            while i < lines.len() && !lines[i].trim().is_empty() {
                block.push(lines[i].trim().to_string());
                i += 1;
            }
            out.push(String::new());
            out.push("```python".into());
            out.push(block.join("\n"));
            out.push("```".into());
            out.push(String::new());
            continue;
        }
        out.push(inline_md(l));
        i += 1;
    }
    out.join("\n")
}

/// Renders a docstring as Markdown for the Quick Documentation popup. A
/// `Plain` hint lets the docstring's own markup decide.
pub fn render_markdown(doc: &str, format: DocstringFormat) -> String {
    let fmt = match (format, detect_one(doc)) {
        (DocstringFormat::Plain, detected) => detected,
        (_, DocstringFormat::Plain) => format,
        (_, detected) => detected,
    };
    let p = parse_with(doc, fmt);
    let mut out: Vec<String> = Vec::new();
    if !p.summary.is_empty() {
        out.push(inline_md(&p.summary));
        out.push(String::new());
    }
    if !p.description.is_empty() {
        out.push(body_md(&p.description));
        out.push(String::new());
    }
    if !p.params.is_empty() {
        out.push("**Parameters**".into());
        out.push(String::new());
        for (name, ty, desc) in &p.params {
            let mut line = format!("- `{name}`");
            if let Some(t) = ty {
                line.push_str(&format!(" (*{}*)", inline_md(t)));
            }
            if !desc.is_empty() {
                line.push_str(&format!(" – {}", inline_md(desc)));
            }
            out.push(line);
        }
        out.push(String::new());
    }
    if let Some((ty, desc)) = &p.returns {
        out.push("**Returns**".into());
        out.push(String::new());
        let mut line = String::new();
        if let Some(t) = ty {
            line.push_str(&format!("*{}*", inline_md(t)));
        }
        if !desc.is_empty() {
            if !line.is_empty() {
                line.push_str(" – ");
            }
            line.push_str(&inline_md(desc));
        }
        out.push(line);
        out.push(String::new());
    }
    if !p.raises.is_empty() {
        out.push("**Raises**".into());
        out.push(String::new());
        for (exc, desc) in &p.raises {
            let mut line = format!("- `{exc}`");
            if !desc.is_empty() {
                line.push_str(&format!(" – {}", inline_md(desc)));
            }
            out.push(line);
        }
        out.push(String::new());
    }
    if !p.attributes.is_empty() {
        out.push("**Attributes**".into());
        out.push(String::new());
        for (name, desc) in &p.attributes {
            let mut line = format!("- `{name}`");
            if !desc.is_empty() {
                line.push_str(&format!(" – {}", inline_md(desc)));
            }
            out.push(line);
        }
        out.push(String::new());
    }
    if !p.examples.is_empty() {
        out.push("**Examples**".into());
        out.push(String::new());
        for ex in &p.examples {
            out.push("```python".into());
            out.push(ex.clone());
            out.push("```".into());
            out.push(String::new());
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

// ---------------------------------------------------------------------------
// Generation & updating
// ---------------------------------------------------------------------------

/// Signature facts used by the generators.
struct FuncFacts {
    /// `(name as written incl. stars, annotation text)` without `self`/`cls`.
    params: Vec<(String, Option<String>)>,
    /// Return annotation text, or `Some(None)` when the body returns values.
    returns: Option<Option<String>>,
    raises: Vec<String>,
    /// Indentation of the body statements.
    body_indent: String,
}

fn func_at(root: &SyntaxNode, offset: TextSize) -> Option<ast::FuncDef> {
    let tok = root
        .token_at_offset(offset)
        .right_biased()
        .or_else(|| root.token_at_offset(offset).left_biased())?;
    let node = tok
        .parent_ancestors()
        .find(|n| n.kind() == SyntaxKind::FUNC_DEF)?;
    ast::FuncDef::cast(node)
}

fn facts_for(file: &FileInfo, f: &ast::FuncDef) -> Option<FuncFacts> {
    let text: &str = &file.text;
    let body = f.body()?;
    let first = body.statements().next()?;
    let first_tok = first.syntax().first_token().map(|t| {
        if t.kind().is_trivia() {
            t.next_token().unwrap_or(t)
        } else {
            t
        }
    })?;
    let start = usize::from(first_tok.text_range().start());
    let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let body_indent = text[line_start..start].to_string();
    if !body_indent.chars().all(|c| c == ' ' || c == '\t') || body_indent.is_empty() {
        // Body on the `def` line; not supported.
        return None;
    }
    let is_method = f.is_method() && !f.has_decorator("staticmethod");
    let mut params = Vec::new();
    if let Some(list) = f.params() {
        for (i, p) in list.params().enumerate() {
            let Some(name) = p.name() else { continue };
            let name = name.text().to_string();
            if i == 0 && is_method && (name == "self" || name == "cls") {
                continue;
            }
            let written = match &p {
                ast::AnyParam::Star(_) => format!("*{name}"),
                ast::AnyParam::StarStar(_) => format!("**{name}"),
                _ => name,
            };
            let ann = p.annotation().map(|a| a.syntax().text().to_string());
            params.push((written, ann));
        }
    }
    let ann = f.return_annotation().map(|a| a.syntax().text().to_string());
    let returns = match ann {
        Some(a) if a == "None" => None,
        Some(a) => Some(Some(a)),
        None => {
            if returns_value(f) {
                Some(None)
            } else {
                None
            }
        }
    };
    let mut raises: Vec<String> = Vec::new();
    for n in f
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::RAISE_STMT)
    {
        if n.ancestors()
            .skip(1)
            .find(|a| a.kind() == SyntaxKind::FUNC_DEF)
            .as_ref()
            != Some(f.syntax())
        {
            continue;
        }
        let Some(r) = ast::RaiseStmt::cast(n) else {
            continue;
        };
        let Some(e) = r.exception() else { continue };
        let name = match &e {
            ast::Expr::Call(c) => c.callee().map(|c| c.syntax().text().to_string()),
            other => Some(other.syntax().text().to_string()),
        };
        if let Some(n) = name.filter(|n| {
            n.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        }) {
            if !raises.contains(&n) {
                raises.push(n);
            }
        }
    }
    Some(FuncFacts {
        params,
        returns,
        raises,
        body_indent,
    })
}

/// Whether `f` itself (not nested functions) has `return <value>`.
fn returns_value(f: &ast::FuncDef) -> bool {
    f.syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::RETURN_STMT)
        .any(|n| {
            let owner = n
                .ancestors()
                .skip(1)
                .find(|a| a.kind() == SyntaxKind::FUNC_DEF);
            owner.as_ref() == Some(f.syntax())
                && ast::ReturnStmt::cast(n).and_then(|r| r.value()).is_some()
        })
}

fn strip_stars(name: &str) -> &str {
    name.trim_start_matches('*')
}

/// Smart Keys options for new documentation stub fields.
/// Type placeholders are empty editable slots, not inferred or fabricated types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StubOptions {
    pub type_placeholders: bool,
    /// Include a return-type field independently of parameter type placeholders.
    pub return_type: bool,
}

fn parameter_lines(
    format: DocstringFormat,
    name: &str,
    annotation: Option<&str>,
    options: StubOptions,
) -> Vec<String> {
    match format {
        DocstringFormat::ReStructuredText | DocstringFormat::Epytext => {
            let prefix = if format == DocstringFormat::Epytext {
                "@"
            } else {
                ":"
            };
            let mut lines = vec![format!("{prefix}param {}: ", strip_stars(name))];
            if options.type_placeholders {
                lines.push(format!(
                    "{prefix}type {}: {}",
                    strip_stars(name),
                    annotation.unwrap_or("")
                ));
            }
            lines
        }
        DocstringFormat::Google => vec![match annotation {
            Some(annotation) => format!("    {name} ({annotation}): "),
            None if options.type_placeholders => format!("    {name} (): "),
            None => format!("    {name}: "),
        }],
        DocstringFormat::NumPy => vec![
            match annotation {
                Some(annotation) => format!("{name} : {annotation}"),
                None if options.type_placeholders => format!("{name} : "),
                None => name.to_string(),
            },
            "    ".into(),
        ],
        DocstringFormat::Plain => vec![format!(":param {}: ", strip_stars(name))],
    }
}

/// Lines (without indentation) of a docstring stub, including the quotes.
fn stub_lines(format: DocstringFormat, facts: &FuncFacts, options: StubOptions) -> Vec<String> {
    let mut lines: Vec<String> = vec!["\"\"\"".into(), String::new()];
    match format {
        DocstringFormat::Plain => {}
        DocstringFormat::ReStructuredText => {
            if !facts.params.is_empty() || facts.returns.is_some() || !facts.raises.is_empty() {
                lines.push(String::new());
            }
            for (name, annotation) in &facts.params {
                lines.extend(parameter_lines(
                    format,
                    name,
                    annotation.as_deref(),
                    options,
                ));
            }
            if let Some(annotation) = &facts.returns {
                lines.push(":return: ".into());
                if options.type_placeholders || options.return_type {
                    lines.push(format!(":rtype: {}", annotation.as_deref().unwrap_or("")));
                }
            }
            for r in &facts.raises {
                lines.push(format!(":raises {r}: "));
            }
        }
        DocstringFormat::Epytext => {
            if !facts.params.is_empty() || facts.returns.is_some() || !facts.raises.is_empty() {
                lines.push(String::new());
            }
            for (name, annotation) in &facts.params {
                lines.extend(parameter_lines(
                    format,
                    name,
                    annotation.as_deref(),
                    options,
                ));
            }
            if let Some(annotation) = &facts.returns {
                lines.push("@return: ".into());
                if options.type_placeholders || options.return_type {
                    lines.push(format!("@rtype: {}", annotation.as_deref().unwrap_or("")));
                }
            }
            for r in &facts.raises {
                lines.push(format!("@raise {r}: "));
            }
        }
        DocstringFormat::Google => {
            if !facts.params.is_empty() {
                lines.push(String::new());
                lines.push("Args:".into());
                for (name, annotation) in &facts.params {
                    lines.extend(parameter_lines(
                        format,
                        name,
                        annotation.as_deref(),
                        options,
                    ));
                }
            }
            if let Some(ret) = &facts.returns {
                lines.push(String::new());
                lines.push("Returns:".into());
                match ret {
                    Some(t) => lines.push(format!("    {t}: ")),
                    None if options.type_placeholders => lines.push("    : ".into()),
                    None => lines.push("    ".into()),
                }
            }
            if !facts.raises.is_empty() {
                lines.push(String::new());
                lines.push("Raises:".into());
                for r in &facts.raises {
                    lines.push(format!("    {r}: "));
                }
            }
        }
        DocstringFormat::NumPy => {
            if !facts.params.is_empty() {
                lines.push(String::new());
                lines.push("Parameters".into());
                lines.push("----------".into());
                for (name, annotation) in &facts.params {
                    lines.extend(parameter_lines(
                        format,
                        name,
                        annotation.as_deref(),
                        options,
                    ));
                }
            }
            if let Some(ret) = &facts.returns {
                lines.push(String::new());
                lines.push("Returns".into());
                lines.push("-------".into());
                lines.push(ret.clone().unwrap_or_default());
                lines.push("    ".into());
            }
            if !facts.raises.is_empty() {
                lines.push(String::new());
                lines.push("Raises".into());
                lines.push("------".into());
                for r in &facts.raises {
                    lines.push(r.clone());
                    lines.push("    ".into());
                }
            }
        }
    }
    lines.push("\"\"\"".into());
    lines
}

/// Generates a docstring stub for the function at `func_offset` in `format`.
/// Returns the insertion offset (start of the first body statement) and the
/// text; the caret belongs right after the opening quotes (offset 3 in the
/// text). An empty docstring (`""""""`, e.g. just typed with auto-closed
/// quotes) is replaced by the stub instead. `None` when the function already
/// has a docstring or the body is on the `def` line.
pub fn generate(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    format: DocstringFormat,
) -> Option<(TextRange, String)> {
    generate_with_options(db, file, func_offset, format, StubOptions::default())
}

/// Option-aware version of [`generate`]; Plain format never receives type slots.
pub fn generate_with_options(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    format: DocstringFormat,
    options: StubOptions,
) -> Option<(TextRange, String)> {
    let func_offset = crate::clamp_offset(file, func_offset);
    let _ = db;
    let root = file.parse.syntax_node();
    let f = func_at(&root, func_offset)?;
    let empty_doc = match f.docstring() {
        Some(doc) if doc.value().trim().is_empty() => Some(doc.syntax().text_range()),
        Some(_) => return None,
        None => None,
    };
    let facts = facts_for(file, &f)?;
    let body = f.body()?;
    let first = body.statements().next()?;
    let first_tok = first.syntax().first_token().map(|t| {
        if t.kind().is_trivia() {
            t.next_token().unwrap_or(t)
        } else {
            t
        }
    })?;
    let at = first_tok.text_range().start();
    let indent = &facts.body_indent;
    let lines = stub_lines(format, &facts, options);
    let mut text = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            text.push('\n');
            // The summary line (index 1) keeps its indentation so the caret lands ready to type.
            if !l.is_empty() || i == 1 {
                text.push_str(indent);
            }
        }
        text.push_str(l);
    }
    if let Some(range) = empty_doc {
        return Some((range, text));
    }
    text.push('\n');
    text.push_str(indent);
    Some((TextRange::empty(at), text))
}

/// Adds the parameters missing from the existing docstring of the function at
/// `func_offset` (in the docstring's own format when it has one, else
/// `format`). `None` when nothing is missing or there is no docstring.
pub fn update(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    format: DocstringFormat,
) -> Option<(TextRange, String)> {
    update_with_options(db, file, func_offset, format, StubOptions::default())
}

/// Option-aware version of [`update`]; Plain format never receives type slots.
pub fn update_with_options(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    format: DocstringFormat,
    options: StubOptions,
) -> Option<(TextRange, String)> {
    let parameter_edit = update_parameters_with_options(db, file, func_offset, format, options);
    if !options.return_type {
        return parameter_edit;
    }
    let function = func_at(&file.parse.syntax_node(), func_offset)?;
    let doc = function.docstring()?;
    let original_range = doc.syntax().text_range();
    let mut candidate = file.text.to_string();
    if let Some((range, text)) = &parameter_edit {
        candidate.replace_range(usize::from(range.start())..usize::from(range.end()), text);
    }
    let candidate = file.with_candidate_text(candidate);
    let function = func_at(&candidate.parse.syntax_node(), func_offset)?;
    let doc = function.docstring()?;
    let facts = facts_for(&candidate, &function)?;
    let selected = match detect_one(&doc.value()) {
        DocstringFormat::Plain => format,
        selected => selected,
    };
    let prefix = match selected {
        DocstringFormat::ReStructuredText => ":",
        DocstringFormat::Epytext => "@",
        _ => return parameter_edit,
    };
    let Some(annotation) = facts.returns else {
        return parameter_edit;
    };
    let range = doc.syntax().text_range();
    let raw = &candidate.text[usize::from(range.start())..usize::from(range.end())];
    if raw
        .lines()
        .any(|line| line.trim_start().starts_with(&format!("{prefix}rtype:")))
    {
        return parameter_edit;
    }
    let quote = if raw.ends_with("\"\"\"") {
        "\"\"\""
    } else if raw.ends_with("'''") {
        "'''"
    } else {
        return parameter_edit;
    };
    let closing = raw.len() - quote.len();
    let line_start = raw[..closing].rfind('\n').map_or(0, |at| at + 1);
    let own_line = raw[line_start..closing]
        .chars()
        .all(|c| c == ' ' || c == '\t');
    let insertion = if own_line { line_start } else { closing };
    let mut replacement = raw.to_string();
    let fields = format!(
        "{}{}{prefix}rtype: {}\n{}",
        if own_line { "" } else { "\n\n" },
        facts.body_indent,
        annotation.as_deref().unwrap_or(""),
        if own_line {
            ""
        } else {
            facts.body_indent.as_str()
        }
    );
    replacement.insert_str(insertion, &fields);
    Some((original_range, replacement))
}

fn update_parameters_with_options(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    format: DocstringFormat,
    options: StubOptions,
) -> Option<(TextRange, String)> {
    let func_offset = crate::clamp_offset(file, func_offset);
    let _ = db;
    let root = file.parse.syntax_node();
    let f = func_at(&root, func_offset)?;
    let doc = f.docstring()?;
    let facts = facts_for(file, &f)?;
    let value = doc.value();
    let fmt = match detect_one(&value) {
        DocstringFormat::Plain => format,
        other => other,
    };
    let parsed = parse_with(&value, fmt);
    let documented: Vec<&str> = parsed.params.iter().map(|p| p.0.as_str()).collect();
    let missing: Vec<&(String, Option<String>)> = facts
        .params
        .iter()
        .filter(|(n, _)| !documented.contains(&strip_stars(n)))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let text: &str = &file.text;
    let node_range = doc.syntax().text_range();
    let raw = &text[usize::from(node_range.start())..usize::from(node_range.end())];
    let indent = facts.body_indent.clone();
    let raw_lines: Vec<&str> = raw.split('\n').collect();
    let single_line = raw_lines.len() == 1;
    // Lines to insert (without indentation).
    let new_lines: Vec<String> = missing
        .iter()
        .flat_map(|(name, annotation)| parameter_lines(fmt, name, annotation.as_deref(), options))
        .collect();
    if single_line {
        // `"""Summary."""` → multi-line with a field block.
        let quote_len = if raw.ends_with("\"\"\"") || raw.ends_with("'''") {
            3
        } else {
            1
        };
        let at = node_range.end() - TextSize::new(quote_len as u32);
        let mut t = String::from("\n");
        let (section_head, section_tail): (Vec<String>, Vec<String>) = match fmt {
            DocstringFormat::Google => (vec!["Args:".into()], Vec::new()),
            DocstringFormat::NumPy => (vec!["Parameters".into(), "----------".into()], Vec::new()),
            _ => (Vec::new(), Vec::new()),
        };
        for l in section_head
            .iter()
            .chain(new_lines.iter())
            .chain(section_tail.iter())
        {
            t.push('\n');
            t.push_str(&indent);
            t.push_str(l);
        }
        t.push('\n');
        t.push_str(&indent);
        return Some((TextRange::empty(at), t));
    }
    // Multi-line: find where to insert (line index within the raw docstring).
    let is_param_line = |l: &str| match fmt {
        DocstringFormat::ReStructuredText => {
            l.trim_start().starts_with(":param") || l.trim_start().starts_with(":type")
        }
        DocstringFormat::Epytext => {
            l.trim_start().starts_with("@param") || l.trim_start().starts_with("@type")
        }
        _ => false,
    };
    let closing_idx = raw_lines.len() - 1;
    let mut insert_idx: Option<usize> = None;
    let mut extra_head: Vec<String> = Vec::new();
    match fmt {
        DocstringFormat::ReStructuredText | DocstringFormat::Epytext => {
            if let Some(last) = raw_lines.iter().rposition(|l| is_param_line(l)) {
                // Skip continuation lines of the last field.
                let mut idx = last + 1;
                while idx < closing_idx
                    && !raw_lines[idx].trim().is_empty()
                    && !raw_lines[idx].trim_start().starts_with([':', '@'])
                {
                    idx += 1;
                }
                insert_idx = Some(idx);
            } else if let Some(ret) = raw_lines.iter().position(|l| {
                l.trim_start().starts_with(":return")
                    || l.trim_start().starts_with(":rtype")
                    || l.trim_start().starts_with("@return")
                    || l.trim_start().starts_with(":raise")
                    || l.trim_start().starts_with("@raise")
            }) {
                insert_idx = Some(ret);
            }
        }
        DocstringFormat::Google => {
            if let Some(head) = raw_lines.iter().position(|l| {
                GOOGLE_SECTION.captures(l).is_some_and(|c| {
                    matches!(
                        c.get(2).map(|m| m.as_str()),
                        Some("Args" | "Arguments" | "Parameters" | "Params")
                    )
                })
            }) {
                let header_indent = indent_of(raw_lines[head]);
                let mut idx = head + 1;
                while idx < closing_idx
                    && (raw_lines[idx].trim().is_empty()
                        || indent_of(raw_lines[idx]) > header_indent)
                    && !(raw_lines[idx].trim().is_empty()
                        && idx + 1 < closing_idx
                        && indent_of(raw_lines[idx + 1]) <= header_indent)
                {
                    idx += 1;
                }
                // Back over trailing blank lines inside the section.
                while idx > head + 1 && raw_lines[idx - 1].trim().is_empty() {
                    idx -= 1;
                }
                insert_idx = Some(idx);
            } else {
                extra_head.push("Args:".into());
                if let Some(sec) = raw_lines.iter().position(|l| GOOGLE_SECTION.is_match(l)) {
                    insert_idx = Some(sec);
                }
            }
        }
        DocstringFormat::NumPy => {
            if let Some(head) = raw_lines
                .iter()
                .position(|l| l.trim() == "Parameters" || l.trim() == "Other Parameters")
            {
                let mut idx = head + 2;
                while idx < closing_idx
                    && !(raw_lines[idx].trim().is_empty()
                        && raw_lines
                            .get(idx + 1)
                            .is_some_and(|n| NUMPY_SECTION.is_match(n)))
                    && !NUMPY_SECTION.is_match(raw_lines[idx])
                {
                    idx += 1;
                }
                while idx > head + 2 && raw_lines[idx - 1].trim().is_empty() {
                    idx -= 1;
                }
                insert_idx = Some(idx);
            } else {
                extra_head.push("Parameters".into());
                extra_head.push("----------".into());
                if let Some(sec) = raw_lines.iter().enumerate().position(|(i, l)| {
                    NUMPY_SECTION.is_match(l)
                        && raw_lines.get(i + 1).is_some_and(|n| is_underline(n, '-'))
                }) {
                    insert_idx = Some(sec);
                }
            }
        }
        DocstringFormat::Plain => {}
    }
    let (idx, before_closing) = match insert_idx {
        Some(i) => (i, false),
        None => (closing_idx, true),
    };
    let line_offset: usize = raw_lines[..idx].iter().map(|l| l.len() + 1).sum();
    let at = node_range.start() + TextSize::new(line_offset as u32);
    let prev_blank = idx > 0 && raw_lines[idx - 1].trim().is_empty();
    let next_is_section = !before_closing
        && idx < closing_idx
        && (GOOGLE_SECTION.is_match(raw_lines[idx]) || NUMPY_SECTION.is_match(raw_lines[idx]));
    let mut t = String::new();
    // A new section, or fields appended at the end, start after a blank line.
    if !prev_blank
        && (!extra_head.is_empty()
            || (before_closing
                && matches!(
                    fmt,
                    DocstringFormat::ReStructuredText | DocstringFormat::Epytext
                )))
    {
        t.push('\n');
    }
    for l in extra_head.iter().chain(new_lines.iter()) {
        if !l.is_empty() {
            t.push_str(&indent);
        }
        t.push_str(l);
        t.push('\n');
    }
    if next_is_section {
        t.push('\n');
    }
    Some((TextRange::empty(at), t))
}

// ---------------------------------------------------------------------------
// Consistency problems
// ---------------------------------------------------------------------------

/// Mismatches between docstrings and signatures: documented parameters that
/// do not exist, undocumented parameters (when the docstring documents at
/// least one), and documented return values for functions returning nothing.
pub fn docstring_problems(db: &Analysis, file: &Arc<FileInfo>) -> Vec<(TextRange, String)> {
    if !db.analyze_doc_comments() {
        return Vec::new();
    }
    let root = file.parse.syntax_node();
    let text: &str = &file.text;
    let mut out = Vec::new();
    for node in root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::FUNC_DEF)
    {
        let Some(f) = ast::FuncDef::cast(node) else {
            continue;
        };
        let Some(doc) = f.docstring() else { continue };
        let value = doc.value();
        let parsed = parse(&value);
        if parsed.format == DocstringFormat::Plain {
            continue;
        }
        let doc_range = doc.syntax().text_range();
        let raw = &text[usize::from(doc_range.start())..usize::from(doc_range.end())];
        let is_method = f.is_method() && !f.has_decorator("staticmethod");
        let mut actual: Vec<(String, TextRange)> = Vec::new();
        if let Some(list) = f.params() {
            for (i, p) in list.params().enumerate() {
                let Some(tok) = p.name() else { continue };
                let name = tok.text().to_string();
                if i == 0 && is_method && (name == "self" || name == "cls") {
                    continue;
                }
                actual.push((name, tok.text_range()));
            }
        }
        for (name, _, _) in &parsed.params {
            if !actual.iter().any(|(n, _)| n == name) {
                let range = parameter_name_ranges(raw, name)
                    .first()
                    .map(|range| *range + doc_range.start())
                    .unwrap_or(doc_range);
                out.push((
                    range,
                    format!("Parameter '{name}' is documented but not declared"),
                ));
            }
        }
        if !parsed.params.is_empty() {
            for (name, range) in &actual {
                if !parsed.params.iter().any(|(n, _, _)| n == name) {
                    out.push((*range, format!("Missing parameter '{name}' in docstring")));
                }
            }
        }
        if let Some((documented, _)) = &parsed.returns {
            let ann = f.return_annotation().map(|a| a.syntax().text().to_string());
            let returns_nothing = match ann.as_deref() {
                Some("None") => true,
                Some(_) => false,
                None => !returns_value(&f) && !crate::infer::has_own_yield(&f),
            };
            let documents_none = documented
                .as_deref()
                .and_then(type_information::normalize)
                .is_some_and(|ty| ty == "None");
            if returns_nothing && !documents_none {
                let range = raw
                    .find(":return")
                    .or_else(|| raw.find("@return"))
                    .or_else(|| raw.find("Returns"))
                    .map(|i| {
                        TextRange::at(
                            doc_range.start() + TextSize::new(i as u32),
                            TextSize::new(7),
                        )
                    })
                    .unwrap_or(doc_range);
                out.push((
                    range,
                    "Docstring documents a return value, but the function does not return anything"
                        .into(),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn setup(src: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        (db, f)
    }

    fn apply(src: &str, edit: &(TextRange, String)) -> String {
        let mut s = src.to_string();
        s.replace_range(
            usize::from(edit.0.start())..usize::from(edit.0.end()),
            &edit.1,
        );
        s
    }

    #[test]
    fn builtin_parameter_lists_render_as_parameters() {
        // `print.__doc__` after `inspect.cleandoc`, as the interpreter dump has it.
        let print = "Prints the values to a stream, or to sys.stdout by default.\n\nsep\n  string inserted between values, default a space.\nend\n  string appended after the last value, default a newline.\nfile\n  a file-like object (stream); defaults to the current sys.stdout.\nflush\n  whether to forcibly flush the stream.";
        let p = parse(print);
        assert_eq!(p.format, DocstringFormat::Plain);
        let names: Vec<&str> = p.params.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(names, vec!["sep", "end", "file", "flush"]);
        assert_eq!(
            p.params[0].2,
            "string inserted between values, default a space."
        );
        assert!(p.description.is_empty(), "{:?}", p.description);
        let md = render_markdown(print, DocstringFormat::Plain);
        assert!(md.contains("**Parameters**\n\n- `sep` – string inserted between values, default a space.\n- `end` –"), "{md}");
        // Indented by two with paragraphs inside a description; the text after
        // the list stays the description.
        let split = "Return a list of the substrings in the string, using sep as the separator string.\n\n  sep\n    The separator used to split the string.\n\n    When set to None (the default value), will split on any whitespace\n    character and will discard empty strings from the result.\n  maxsplit\n    Maximum number of splits.\n    -1 (the default value) means no limit.\n\nSplitting starts at the front of the string and works to the end.";
        let p = parse(split);
        assert_eq!(p.params, vec![("sep".to_string(), None, "The separator used to split the string. When set to None (the default value), will split on any whitespace character and will discard empty strings from the result.".to_string()), ("maxsplit".to_string(), None, "Maximum number of splits. -1 (the default value) means no limit.".to_string())]);
        assert_eq!(
            p.description,
            "Splitting starts at the front of the string and works to the end."
        );
        let to_bytes = "Return an array of bytes representing an integer.\n\nlength\n  Length of bytes object to use.\nbyteorder\n  The byte order used to represent the integer.\n*args\n  More.";
        assert_eq!(
            parse(to_bytes)
                .params
                .iter()
                .map(|x| x.0.as_str())
                .collect::<Vec<_>>(),
            vec!["length", "byteorder", "*args"]
        );
        // Not parameter lists: a section title, a wrapped sentence, an indented
        // literal block after a sentence.
        for doc in [
            "Summary.\n\nReturns\n    The value.",
            "Summary.\n\nSomething that wraps onto a\nsecond\n    indented line.",
            "Summary.\n\nRun it like this::\n\n    tool --all",
        ] {
            let p = parse(doc);
            assert!(p.params.is_empty(), "{doc:?} → {:?}", p.params);
        }
    }

    #[test]
    fn detects_formats() {
        assert_eq!(
            detect_one("Summary.\n\n:param x: the x\n:return: y\n"),
            DocstringFormat::ReStructuredText
        );
        assert_eq!(
            detect_one("Summary.\n\n@param x: the x\n@return: y\n"),
            DocstringFormat::Epytext
        );
        assert_eq!(
            detect_one("Summary.\n\nArgs:\n    x (int): the x\n\nReturns:\n    int: y\n"),
            DocstringFormat::Google
        );
        assert_eq!(
            detect_one("Summary.\n\nParameters\n----------\nx : int\n    the x\n"),
            DocstringFormat::NumPy
        );
        assert_eq!(detect_one("Just a summary."), DocstringFormat::Plain);
        let module = "\"\"\"Module.\"\"\"\n\ndef f(x):\n    \"\"\"F.\n\n    Args:\n        x: the x\n    \"\"\"\n\ndef g(y):\n    \"\"\"G.\n\n    Args:\n        y: the y\n    \"\"\"\n";
        assert_eq!(detect(module), DocstringFormat::Google);
        assert_eq!(
            detect("def f():\n    '''Plain.'''\n"),
            DocstringFormat::Plain
        );
    }

    #[test]
    fn parses_rest_and_epytext() {
        let doc = "\n    Compute things.\n\n    Longer description\n    on two lines.\n\n    :param int x: the x value\n        continued here\n    :param y: the y\n    :type y: str\n    :returns: the sum\n    :rtype: float\n    :raises ValueError: when bad\n    :ivar count: number\n    ";
        let p = parse(doc);
        assert_eq!(p.format, DocstringFormat::ReStructuredText);
        assert_eq!(p.summary, "Compute things.");
        assert_eq!(p.description, "Longer description\non two lines.");
        assert_eq!(
            p.params,
            vec![
                (
                    "x".to_string(),
                    Some("int".to_string()),
                    "the x value continued here".to_string()
                ),
                (
                    "y".to_string(),
                    Some("str".to_string()),
                    "the y".to_string()
                )
            ]
        );
        assert_eq!(
            p.returns,
            Some((Some("float".to_string()), "the sum".to_string()))
        );
        assert_eq!(
            p.raises,
            vec![("ValueError".to_string(), "when bad".to_string())]
        );
        assert_eq!(
            p.attributes,
            vec![("count".to_string(), "number".to_string())]
        );
        let e = parse("Summary.\n\n@param x: the x\n@type x: int\n@return: result\n@rtype: bool\n@raise KeyError: missing\n");
        assert_eq!(e.format, DocstringFormat::Epytext);
        assert_eq!(
            e.params,
            vec![(
                "x".to_string(),
                Some("int".to_string()),
                "the x".to_string()
            )]
        );
        assert_eq!(
            e.returns,
            Some((Some("bool".to_string()), "result".to_string()))
        );
        assert_eq!(
            e.raises,
            vec![("KeyError".to_string(), "missing".to_string())]
        );
    }

    #[test]
    fn parses_google_and_numpy() {
        let g = "Fetch a thing.\n\nMore text.\n\nArgs:\n    url (str): where to fetch\n        from, really.\n    retries: how many\n    *args: extra\n\nReturns:\n    bytes: the payload\n\nRaises:\n    IOError: on failure\n\nAttributes:\n    size (int): bytes\n\nExamples:\n    >>> fetch('x')\n    b''\n";
        let p = parse(g);
        assert_eq!(p.format, DocstringFormat::Google);
        assert_eq!(p.summary, "Fetch a thing.");
        assert_eq!(p.description, "More text.");
        assert_eq!(
            p.params,
            vec![
                (
                    "url".to_string(),
                    Some("str".to_string()),
                    "where to fetch from, really.".to_string()
                ),
                ("retries".to_string(), None, "how many".to_string()),
                ("args".to_string(), None, "extra".to_string())
            ]
        );
        assert_eq!(
            p.returns,
            Some((Some("bytes".to_string()), "the payload".to_string()))
        );
        assert_eq!(
            p.raises,
            vec![("IOError".to_string(), "on failure".to_string())]
        );
        assert_eq!(
            p.attributes,
            vec![("size".to_string(), "bytes".to_string())]
        );
        assert_eq!(p.examples, vec![">>> fetch('x')\nb''".to_string()]);
        let n = "Add numbers.\n\nParameters\n----------\na : int\n    first\nb : int, optional\n    second\n\nReturns\n-------\nint\n    the sum\n\nRaises\n------\nValueError\n    if negative\n\nExamples\n--------\n>>> add(1, 2)\n3\n";
        let p = parse(n);
        assert_eq!(p.format, DocstringFormat::NumPy);
        assert_eq!(
            p.params,
            vec![
                (
                    "a".to_string(),
                    Some("int".to_string()),
                    "first".to_string()
                ),
                (
                    "b".to_string(),
                    Some("int, optional".to_string()),
                    "second".to_string()
                )
            ]
        );
        assert_eq!(
            p.returns,
            Some((Some("int".to_string()), "the sum".to_string()))
        );
        assert_eq!(
            p.raises,
            vec![("ValueError".to_string(), "if negative".to_string())]
        );
        assert_eq!(p.examples, vec![">>> add(1, 2)\n3".to_string()]);
    }

    #[test]
    fn generates_stubs_in_every_format() {
        let src = "class C:\n    def run(self, x: int, name, *args, **kw) -> bool:\n        if x < 0:\n            raise ValueError(\"neg\")\n        return True\n";
        let (db, f) = setup(src);
        let at = TextSize::new(src.find("def run").unwrap() as u32 + 4);
        let (r, t) = generate(&db, &f, at, DocstringFormat::ReStructuredText).unwrap();
        assert_eq!(usize::from(r.start()), src.find("if x < 0").unwrap());
        let out = apply(src, &(r, t));
        assert!(out.contains("        \"\"\"\n        \n\n        :param x: \n        :param name: \n        :param args: \n        :param kw: \n        :return: \n        :raises ValueError: \n        \"\"\"\n        if x < 0:"), "{out}");
        let (r, t) = generate(&db, &f, at, DocstringFormat::Google).unwrap();
        let out = apply(src, &(r, t));
        assert!(out.contains("        Args:\n            x (int): \n            name: \n            *args: \n            **kw: \n\n        Returns:\n            bool: \n\n        Raises:\n            ValueError: \n        \"\"\""), "{out}");
        let (r, t) = generate(&db, &f, at, DocstringFormat::NumPy).unwrap();
        let out = apply(src, &(r, t));
        assert!(out.contains("        Parameters\n        ----------\n        x : int\n            \n        name\n            \n"), "{out}");
        assert!(
            out.contains("        Returns\n        -------\n        bool\n            \n"),
            "{out}"
        );
        let (_, t) = generate(&db, &f, at, DocstringFormat::Epytext).unwrap();
        assert!(
            t.contains("@param x: ")
                && t.contains("@return: ")
                && t.contains("@raise ValueError: ")
        );
        let (_, t) = generate(&db, &f, at, DocstringFormat::Plain).unwrap();
        assert_eq!(t, "\"\"\"\n        \n        \"\"\"\n        ");
        // Re-parse the generated reST stub: it must round-trip the parameters.
        let (r, t) = generate(&db, &f, at, DocstringFormat::ReStructuredText).unwrap();
        let out = apply(src, &(r, t));
        let (db2, f2) = setup(&out);
        assert!(generate(&db2, &f2, at, DocstringFormat::ReStructuredText).is_none());
        let probs = docstring_problems(&db2, &f2);
        assert!(probs.is_empty(), "{probs:?}");
    }

    #[test]
    fn fills_an_empty_docstring() {
        // `"""` + Enter with auto-closed quotes leaves an empty docstring behind.
        let src = "def f(a):\n    \"\"\"\n    \"\"\"\n";
        let (db, f) = setup(src);
        let at = TextSize::new(src.find("\"\"\"").unwrap() as u32 + 3);
        let (r, t) = generate(&db, &f, at, DocstringFormat::ReStructuredText).unwrap();
        assert_eq!(
            &src[r], "\"\"\"\n    \"\"\"",
            "the empty docstring is replaced"
        );
        assert_eq!(
            apply(src, &(r, t)),
            "def f(a):\n    \"\"\"\n    \n\n    :param a: \n    \"\"\"\n"
        );
        let (db2, f2) = setup("def f(a):\n    \"\"\"Doc.\"\"\"\n");
        assert!(
            generate(&db2, &f2, at, DocstringFormat::ReStructuredText).is_none(),
            "a real docstring is kept"
        );
    }

    #[test]
    fn updates_missing_parameters() {
        let src = "def f(a, b, c):\n    \"\"\"Do it.\n\n    :param a: first\n    :return: nothing\n    \"\"\"\n    return a\n";
        let (db, f) = setup(src);
        let at = TextSize::new(src.find("def f").unwrap() as u32 + 4);
        let edit = update(&db, &f, at, DocstringFormat::ReStructuredText).unwrap();
        let out = apply(src, &edit);
        assert!(
            out.contains(
                "    :param a: first\n    :param b: \n    :param c: \n    :return: nothing\n"
            ),
            "{out}"
        );
        let (db2, f2) = setup(&out);
        assert!(update(&db2, &f2, at, DocstringFormat::ReStructuredText).is_none());
        // Google section.
        let g = "def f(a, b):\n    \"\"\"Do it.\n\n    Args:\n        a: first\n\n    Returns:\n        int: x\n    \"\"\"\n    return a\n";
        let (db, f) = setup(g);
        let edit = update(&db, &f, at, DocstringFormat::Google).unwrap();
        let out = apply(g, &edit);
        assert!(
            out.contains("    Args:\n        a: first\n        b: \n\n    Returns:"),
            "{out}"
        );
        // Single-line docstring becomes multi-line.
        let s = "def f(a):\n    \"\"\"Do it.\"\"\"\n    return a\n";
        let (db, f) = setup(s);
        let edit = update(&db, &f, at, DocstringFormat::ReStructuredText).unwrap();
        let out = apply(s, &edit);
        assert_eq!(
            out,
            "def f(a):\n    \"\"\"Do it.\n\n    :param a: \n    \"\"\"\n    return a\n"
        );
        // NumPy without a Parameters section.
        let n = "def f(a):\n    \"\"\"Do it.\n\n    Returns\n    -------\n    int\n        x\n    \"\"\"\n    return a\n";
        let (db, f) = setup(n);
        let edit = update(&db, &f, at, DocstringFormat::NumPy).unwrap();
        let out = apply(n, &edit);
        assert!(
            out.contains("    Parameters\n    ----------\n    a\n        \n\n    Returns\n"),
            "{out}"
        );
    }

    #[test]
    fn reports_docstring_problems() {
        let src = "def f(a, b):\n    \"\"\"Do.\n\n    :param a: first\n    :param zzz: ghost\n    :return: value\n    \"\"\"\n    print(a, b)\n\ndef ok(x) -> int:\n    \"\"\"Fine.\n\n    :param x: x\n    :return: x\n    \"\"\"\n    return x\n";
        let (db, f) = setup(src);
        let probs = docstring_problems(&db, &f);
        let msgs: Vec<&str> = probs.iter().map(|p| p.1.as_str()).collect();
        assert!(
            msgs.contains(&"Parameter 'zzz' is documented but not declared"),
            "{msgs:?}"
        );
        assert!(
            msgs.contains(&"Missing parameter 'b' in docstring"),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.starts_with("Docstring documents a return value")),
            "{msgs:?}"
        );
        assert_eq!(msgs.len(), 3);
        let ghost = probs.iter().find(|p| p.1.contains("zzz")).unwrap();
        assert_eq!(&src[ghost.0], "zzz");
        let b = probs.iter().find(|p| p.1.contains("'b'")).unwrap();
        assert_eq!(&src[b.0], "b");
    }

    #[test]
    fn renders_markdown() {
        let doc = "Compute ``x`` with :func:`helper`.\n\nExample usage::\n\n    compute(1)\n\n:param int x: the value\n:return: the result\n:raises ValueError: when bad\n";
        let md = render_markdown(doc, DocstringFormat::Plain);
        assert!(md.starts_with("Compute `x` with `helper`."), "{md}");
        assert!(md.contains("```\ncompute(1)\n```"), "{md}");
        assert!(
            md.contains("**Parameters**\n\n- `x` (*int*) – the value"),
            "{md}"
        );
        assert!(md.contains("**Returns**\n\nthe result"), "{md}");
        assert!(
            md.contains("**Raises**\n\n- `ValueError` – when bad"),
            "{md}"
        );
        let g = "Summary.\n\nArgs:\n    a (int): first\n\nReturns:\n    int: the sum\n\nExamples:\n    >>> f(1)\n    1\n";
        let md = render_markdown(g, DocstringFormat::Google);
        assert!(md.contains("- `a` (*int*) – first"), "{md}");
        assert!(md.contains("*int* – the sum"), "{md}");
        assert!(md.contains("```python\n>>> f(1)\n1\n```"), "{md}");
        let plain = "Just text.\n\n>>> f()\n2\n";
        let md = render_markdown(plain, DocstringFormat::ReStructuredText);
        assert!(md.contains("```python\n>>> f()\n2\n```"), "{md}");
    }

    #[test]
    fn type_slots_generate_in_every_structured_format_without_guessing_types() {
        let src = "class C:\n    def run(\n        self, item, count: int = 1, *args, **kwargs\n    ):\n        return item\n";
        let (db, file) = setup(src);
        let at = TextSize::new(src.find("def run").unwrap() as u32 + 4);
        let options = StubOptions {
            type_placeholders: true,
            return_type: false,
        };
        for format in [
            DocstringFormat::ReStructuredText,
            DocstringFormat::Epytext,
            DocstringFormat::Google,
            DocstringFormat::NumPy,
        ] {
            let edit = generate_with_options(&db, &file, at, format, options).unwrap();
            let output = apply(src, &edit);
            let (out_db, out_file) = setup(&output);
            let function = func_at(&out_file.parse.syntax_node(), at).unwrap();
            let parsed = parse_with(&function.docstring().unwrap().value(), format);
            assert_eq!(
                parsed
                    .params
                    .iter()
                    .map(|param| param.0.as_str())
                    .collect::<Vec<_>>(),
                vec!["item", "count", "args", "kwargs"],
                "{format:?}: {output}"
            );
            // Field-based formats retain an explicit empty :type/@type value;
            // Google/NumPy normalize their empty inline type slot to None.
            let empty_type = match format {
                DocstringFormat::ReStructuredText | DocstringFormat::Epytext => Some(""),
                DocstringFormat::Google | DocstringFormat::NumPy => None,
                DocstringFormat::Plain => unreachable!(),
            };
            for index in [0, 2, 3] {
                assert_eq!(
                    parsed.params[index].1.as_deref(),
                    empty_type,
                    "{format:?}: unknown types must remain exactly empty"
                );
            }
            assert_eq!(parsed.params[1].1.as_deref(), Some("int"));
            assert!(generate_with_options(&out_db, &out_file, at, format, options).is_none());
            assert!(
                update_with_options(&out_db, &out_file, at, format, options).is_none(),
                "generated parameters must not duplicate"
            );
            match format {
                DocstringFormat::ReStructuredText => {
                    assert!(output.contains(":type item: \n"));
                    assert!(output.contains(":type count: int\n"));
                    assert!(output.contains(":type args: \n"));
                    assert!(output.contains(":type kwargs: \n"));
                    assert!(output.contains(":rtype: \n"));
                    assert!(!output.contains(":param self:"));
                }
                DocstringFormat::Epytext => {
                    assert!(output.contains("@type item: \n"));
                    assert!(output.contains("@type count: int\n"));
                    assert!(output.contains("@rtype: \n"));
                }
                DocstringFormat::Google => {
                    assert!(output.contains("item (): \n"));
                    assert!(output.contains("count (int): \n"));
                    assert!(output.contains("*args (): \n"));
                    assert!(output.contains("**kwargs (): \n"));
                    assert!(output.contains("Returns:\n            : \n"));
                }
                DocstringFormat::NumPy => {
                    assert!(output.contains("item : \n"));
                    assert!(output.contains("count : int\n"));
                    assert!(output.contains("*args : \n"));
                    assert!(output.contains("**kwargs : \n"));
                    assert!(output.contains("Returns\n        -------\n\n            \n"));
                }
                DocstringFormat::Plain => unreachable!(),
            }
            assert!(!output.contains("TODO") && !output.contains("Any"));
        }
    }

    #[test]
    fn type_slot_option_preserves_default_and_plain_output_and_annotated_return() {
        let src = "def f(value: list[str] = None) -> tuple[int, str]:\n    return 1, 'x'\n";
        let (db, file) = setup(src);
        let at = TextSize::new(5);
        for format in [
            DocstringFormat::Plain,
            DocstringFormat::ReStructuredText,
            DocstringFormat::Epytext,
            DocstringFormat::Google,
            DocstringFormat::NumPy,
        ] {
            assert_eq!(
                generate(&db, &file, at, format),
                generate_with_options(&db, &file, at, format, StubOptions::default())
            );
            let (_, enabled) = generate_with_options(
                &db,
                &file,
                at,
                format,
                StubOptions {
                    type_placeholders: true,
                    return_type: false,
                },
            )
            .unwrap();
            if format == DocstringFormat::Plain {
                assert_eq!(generate(&db, &file, at, format).unwrap().1, enabled);
            } else {
                assert!(enabled.contains("list[str]"), "{format:?}: {enabled}");
                assert!(enabled.contains("tuple[int, str]"), "{format:?}: {enabled}");
            }
        }
    }

    #[test]
    fn no_return_type_slot_for_none_bare_return_or_only_nested_return() {
        for src in [
            "def f(value) -> None:\n    return\n",
            "def f(value):\n    return\n",
            "def f(value):\n    def inner():\n        return value\n    inner()\n",
        ] {
            let (db, file) = setup(src);
            for format in [
                DocstringFormat::ReStructuredText,
                DocstringFormat::Epytext,
                DocstringFormat::Google,
                DocstringFormat::NumPy,
            ] {
                let (_, stub) = generate_with_options(
                    &db,
                    &file,
                    TextSize::new(5),
                    format,
                    StubOptions {
                        type_placeholders: true,
                        return_type: false,
                    },
                )
                .unwrap();
                assert!(
                    !stub.contains("rtype") && !stub.contains("Returns"),
                    "{stub}"
                );
            }
        }
    }

    #[test]
    fn updating_missing_parameters_adds_slots_without_rewriting_user_types_or_description() {
        let src = "def f(x: int, extra):\n    \"\"\"Summary.\n\n    :param x: kept description\n    :type x: float\n    :return: kept result\n    :rtype: str\n    \"\"\"\n    return x\n";
        let (db, file) = setup(src);
        let edit = update_with_options(
            &db,
            &file,
            TextSize::new(5),
            DocstringFormat::Google,
            StubOptions {
                type_placeholders: true,
                return_type: false,
            },
        )
        .unwrap();
        let output = apply(src, &edit);
        assert!(
            output.contains(":param extra: \n    :type extra: \n"),
            "{output}"
        );
        assert!(output.contains(":param x: kept description\n    :type x: float"));
        assert!(output.contains(":return: kept result\n    :rtype: str"));
        assert_eq!(output.matches(":param x:").count(), 1);
        let (db, file) = setup(&output);
        assert!(update_with_options(
            &db,
            &file,
            TextSize::new(5),
            DocstringFormat::Google,
            StubOptions {
                type_placeholders: true,
                return_type: false
            }
        )
        .is_none());
    }

    #[test]
    fn empty_docstring_replacement_excludes_receiver_but_preserves_static_parameter() {
        let src = "class C:\n    @classmethod\n    def f(cls, name):\n        \"\"\"\n        \"\"\"\n        return name\n";
        let (db, file) = setup(src);
        let at = TextSize::new(src.find("\"\"\"").unwrap() as u32 + 3);
        let edit = generate_with_options(
            &db,
            &file,
            at,
            DocstringFormat::Google,
            StubOptions {
                type_placeholders: true,
                return_type: false,
            },
        )
        .unwrap();
        assert!(!edit.0.is_empty());
        let output = apply(src, &edit);
        assert!(output.contains("name (): "));
        assert!(!output.contains("cls ()"));
        assert_eq!(output.matches("\"\"\"").count(), 2);
        let src = "class C:\n    @staticmethod\n    def f(self):\n        return self\n";
        let (db, file) = setup(src);
        let (_, stub) = generate_with_options(
            &db,
            &file,
            TextSize::new(src.find("def f").unwrap() as u32 + 4),
            DocstringFormat::Google,
            StubOptions {
                type_placeholders: true,
                return_type: false,
            },
        )
        .unwrap();
        assert!(stub.contains("self (): "));
    }
    #[test]
    fn field_types_stop_at_unindented_prose_but_keep_real_continuations() {
        for (format, marker) in [
            (DocstringFormat::ReStructuredText, ":"),
            (DocstringFormat::Epytext, "@"),
        ] {
            let text = format!("Summary.\n\n{marker}rtype: int\nArg:\n    value: input\n");
            let parsed = parse_with(&text, format);
            assert_eq!(parsed.returns.unwrap().0.as_deref(), Some("int"));
            assert!(parsed.description.contains("Arg:"));
            assert!(parsed.description.contains("value: input"));
            let continuation = format!("Summary.\n\n{marker}rtype: dict[\n    str,\n    int]\n");
            let parsed = parse_with(&continuation, format);
            assert_eq!(
                parsed.returns.unwrap().0.as_deref(),
                Some("dict[ str, int]")
            );
        }
    }
}
