//! Rendered documentation comments (roadmap §1.1 "Render docs" and reader
//! mode): turns a docstring into display blocks — paragraphs with inline
//! markup, section headings, parameter / return / exception fields, list
//! items and code — for reStructuredText, Epytext, Google, NumPy and plain
//! (or Markdown) docstrings.
//!
//! Unlike [`crate::docstrings::parse`], which extracts facts for code
//! generation, this keeps the order of the docstring's parts and the source
//! line every block starts on, so an editor can draw the blocks in place of
//! the docstring and map a click on them back to the source.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::docstrings::{
    self, DocstringFormat, EPY_FIELD, GOOGLE_ARG, GOOGLE_RAISE, GOOGLE_SECTION, NUMPY_SECTION,
    REST_FIELD,
};

/// Inline style of a run of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanStyle {
    Text,
    Emphasis,
    Strong,
    Code,
}

/// A run of text in one style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
    /// Hyperlink target (`` `text <url>`_ ``, `[text](url)`, bare URLs).
    pub link: Option<String>,
}

impl Span {
    fn new(text: impl Into<String>, style: SpanStyle) -> Self {
        Self {
            text: text.into(),
            style,
            link: None,
        }
    }
}

/// What a block shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// Running text, reflowed to the available width.
    Paragraph(Vec<Span>),
    /// A section title: "Parameters", "Returns", "Note", a reST section title, …
    Heading(String),
    /// A parameter, attribute, exception or return value: `name (type) – description`.
    /// `name` is empty for a return value described by its type only.
    Field {
        name: String,
        ty: Option<String>,
        desc: Vec<Span>,
    },
    /// A bullet (`•`) or enumerated (`1.`) list item.
    Item { marker: String, spans: Vec<Span> },
    /// A definition-list term; its definition follows one level deeper.
    Term(Vec<Span>),
    /// Preformatted text: literal blocks, doctests, examples.
    Code { text: String, python: bool },
    /// Image resource, resolved only by the owning documentation consumer.
    Image { target: String, alt: String },
}

/// One display block of a rendered docstring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocBlock {
    pub kind: BlockKind,
    /// Nesting depth: fields under their heading, section bodies, nested lists, block quotes.
    pub depth: u8,
    /// Line of the string literal the block starts on (0 = the line of the opening quotes).
    pub line: usize,
}

/// A docstring line: `(text, line of the string literal it comes from)`.
type Line = (String, usize);

/// Blocks of the docstring written as the string literal `literal` (prefix,
/// quotes and escape sequences as they appear in the source).
pub fn render_literal(literal: &str) -> Vec<DocBlock> {
    render_lines(clean_lines(literal_lines(literal)))
}

/// Blocks of an already decoded docstring value (line numbers count from its first line).
pub fn render_text(doc: &str) -> Vec<DocBlock> {
    render_lines(clean_lines(
        doc.split('\n')
            .enumerate()
            .map(|(i, l)| (l.strip_suffix('\r').unwrap_or(l).to_string(), i))
            .collect(),
    ))
}

/// The text of `spans` without markup (for types and plain-text uses).
pub fn plain_text(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

fn render_lines(lines: Vec<Line>) -> Vec<DocBlock> {
    if lines.is_empty() {
        return Vec::new();
    }
    let joined = lines
        .iter()
        .map(|l| l.0.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    match docstrings::detect_one(&joined) {
        DocstringFormat::ReStructuredText => fields_doc(&lines, false),
        DocstringFormat::Epytext => fields_doc(&lines, true),
        DocstringFormat::Google => google_doc(&lines),
        DocstringFormat::NumPy => numpy_doc(&lines),
        DocstringFormat::Plain => body(&lines, 0, false),
    }
}

// ---------------------------------------------------------------------------
// Literal → lines
// ---------------------------------------------------------------------------

/// Lines of a string literal's value: prefix and quotes removed and escape
/// sequences decoded (unless raw), each with its source line.
fn literal_lines(literal: &str) -> Vec<Line> {
    let prefix_len = literal
        .bytes()
        .take_while(|b| b.is_ascii_alphabetic())
        .count();
    let raw = literal[..prefix_len].to_ascii_lowercase().contains('r');
    let body = &literal[prefix_len..];
    let quote = ["\"\"\"", "'''", "\"", "'"]
        .into_iter()
        .find(|q| body.starts_with(q))
        .unwrap_or("");
    let mut inner = &body[quote.len()..];
    if !quote.is_empty() && inner.len() >= quote.len() && inner.ends_with(quote) {
        inner = &inner[..inner.len() - quote.len()];
    }
    let mut out: Vec<Line> = Vec::new();
    // A line continued with a trailing backslash (non-raw strings): text and first source line.
    let mut cont: Option<Line> = None;
    for (src, source_line) in inner.split('\n').enumerate() {
        let source_line = source_line.strip_suffix('\r').unwrap_or(source_line);
        let (mut text, start) = cont.take().unwrap_or((String::new(), src));
        if raw {
            text.push_str(source_line);
            out.push((text, start));
            continue;
        }
        let trailing = source_line.len() - source_line.trim_end_matches('\\').len();
        let continued = trailing % 2 == 1;
        let part = if continued {
            &source_line[..source_line.len() - 1]
        } else {
            source_line
        };
        let decoded = decode_escapes(part);
        let mut pieces = decoded.split('\n');
        text.push_str(pieces.next().unwrap_or(""));
        for piece in pieces {
            out.push((std::mem::take(&mut text), start));
            text.push_str(piece);
        }
        if continued {
            cont = Some((text, start));
        } else {
            out.push((text, start));
        }
    }
    if let Some(line) = cont {
        out.push(line);
    }
    out
}

/// The escape sequences that matter for display; others stay as written.
fn decode_escapes(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('\'') => out.push('\''),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `inspect.cleandoc` on numbered lines: tabs expanded, trailing whitespace
/// removed, the common indentation of the lines after the first stripped,
/// blank lines at both ends dropped.
fn clean_lines(mut lines: Vec<Line>) -> Vec<Line> {
    for l in &mut lines {
        if l.0.contains('\t') {
            l.0 = expand_tabs(&l.0);
        }
        let end = l.0.trim_end().len();
        l.0.truncate(end);
    }
    if let Some(first) = lines.first_mut() {
        first.0 = first.0.trim_start().to_string();
    }
    let indent = lines
        .iter()
        .skip(1)
        .filter(|l| !l.0.is_empty())
        .map(|l| indent_of(&l.0))
        .min()
        .unwrap_or(0);
    for l in lines.iter_mut().skip(1) {
        let cut = indent.min(indent_of(&l.0));
        l.0.drain(..cut);
    }
    let leading = lines.iter().take_while(|l| l.0.is_empty()).count();
    lines.drain(..leading);
    while lines.last().is_some_and(|l| l.0.is_empty()) {
        lines.pop();
    }
    lines
}

fn expand_tabs(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut col = 0;
    for c in s.chars() {
        if c == '\t' {
            let n = 8 - col % 8;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// Leading ASCII spaces (the lines are tab-expanded), so slicing by it is safe.
fn indent_of(s: &str) -> usize {
    s.bytes().take_while(|b| *b == b' ').count()
}

fn is_blank(l: &Line) -> bool {
    l.0.trim().is_empty()
}

/// Index of the first line at or after `start` that is not blank and not
/// indented deeper than `indent` (the end of an indented block).
fn block_end(lines: &[Line], start: usize, indent: usize) -> usize {
    let mut j = start;
    while j < lines.len() && (is_blank(&lines[j]) || indent_of(&lines[j].0) > indent) {
        j += 1;
    }
    j
}

/// The lines' text with their common indentation and blank edges removed.
fn dedent_text(lines: &[Line]) -> String {
    let indent = lines
        .iter()
        .filter(|l| !is_blank(l))
        .map(|l| indent_of(&l.0))
        .min()
        .unwrap_or(0);
    let mut out: Vec<&str> = lines
        .iter()
        .map(|l| if is_blank(l) { "" } else { &l.0[indent..] })
        .collect();
    let leading = out.iter().take_while(|l| l.is_empty()).count();
    out.drain(..leading);
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

fn block(kind: BlockKind, depth: u8, line: usize) -> DocBlock {
    DocBlock { kind, depth, line }
}

// ---------------------------------------------------------------------------
// Free text (summary, descriptions, section bodies, plain docstrings)
// ---------------------------------------------------------------------------

static DIRECTIVE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\.\.\s+([A-Za-z][\w:.+-]*)::\s*(.*)$").unwrap());
static CITATION: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\.\.\s+\[([^\]\s]+)\]\s*(.*)$").unwrap());
static DIRECTIVE_OPTION: Lazy<Regex> = Lazy::new(|| Regex::new(r"^:[\w-]+:").unwrap());
static ENUMERATOR: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(\d{1,3}|#|[a-zA-Z])([.)])\s+").unwrap());
static MD_HEADING: Lazy<Regex> = Lazy::new(|| Regex::new(r"^#{1,6}\s+(.+?)\s*#*$").unwrap());

/// A reST section adornment: one repeated punctuation character, 3+ times.
fn is_adornment(s: &str) -> bool {
    let t = s.trim();
    let mut chars = t.chars();
    let Some(c) = chars.next() else { return false };
    t.chars().count() >= 3 && "=-~^\"'`*+#:.<>_".contains(c) && chars.all(|d| d == c)
}

/// The opening fence of a Markdown code block.
fn fence_of(t: &str) -> Option<&'static str> {
    ["```", "~~~"].into_iter().find(|f| t.starts_with(f))
}

/// A list item's marker (as displayed) and the item text.
fn list_marker(t: &str) -> Option<(String, &str)> {
    for bullet in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = t.strip_prefix(bullet) {
            return Some(("•".to_string(), rest.trim_start()));
        }
    }
    if matches!(t, "-" | "*" | "+" | "•") {
        return Some(("•".to_string(), ""));
    }
    let m = ENUMERATOR.captures(t)?;
    let label = m.get(1)?.as_str();
    // A single letter only enumerates like `a.` / `b)`, not sentences such as "I. e."
    if label.len() == 1
        && label.chars().all(|c| c.is_ascii_alphabetic())
        && m.get(2)?.as_str() == "."
        && label.chars().all(|c| c.is_ascii_uppercase())
    {
        return None;
    }
    let marker = format!(
        "{}{}",
        if label == "#" { "•" } else { label },
        if label == "#" { "" } else { m.get(2)?.as_str() }
    );
    Some((marker, &t[m.get(0)?.end()..]))
}

/// A reST simple-table border (`=====  =====`, two columns or more) or a
/// grid-table rule (`+-----+-----+`).
fn is_table_border(t: &str) -> bool {
    let t = t.trim();
    if t.len() >= 3 && t.starts_with('+') && t.ends_with('+') {
        return t.chars().all(|c| matches!(c, '+' | '-' | '=')) && t.contains(['-', '=']);
    }
    let mut columns = 0;
    for part in t.split(' ').filter(|p| !p.is_empty()) {
        if !part.chars().all(|c| c == '=') {
            return false;
        }
        columns += 1;
    }
    columns >= 2
}

/// Where the tables of a block of lines end, answered from lookups built once
/// for the block: a docstring with thousands of table borders would otherwise
/// search to its end for each of them.
struct TableEnds {
    /// The first blank line at or after each line (the number of lines: none).
    next_blank: Vec<usize>,
    /// The first simple-table bottom border (a border followed by a blank line
    /// or the end) at or after each line.
    next_bottom: Vec<Option<usize>>,
}

impl TableEnds {
    fn new(lines: &[Line]) -> Self {
        let n = lines.len();
        let mut next_blank = vec![n; n + 1];
        let mut next_bottom = vec![None; n + 1];
        for j in (0..n).rev() {
            next_blank[j] = if is_blank(&lines[j]) {
                j
            } else {
                next_blank[j + 1]
            };
            let bottom = is_table_border(&lines[j].0) && lines.get(j + 1).is_none_or(is_blank);
            next_bottom[j] = if bottom { Some(j) } else { next_bottom[j + 1] };
        }
        Self {
            next_blank,
            next_bottom,
        }
    }

    /// The end (exclusive) of the table starting at border line `start`: a grid
    /// table runs to the next blank line, a simple table to its bottom border
    /// (a border followed by a blank line or the end; blank lines may separate rows).
    fn end(&self, lines: &[Line], start: usize) -> usize {
        let first_blank = self.next_blank[start];
        if lines[start].0.trim().starts_with('+') {
            return first_blank;
        }
        self.next_bottom[start + 1].map_or(first_blank, |j| j + 1)
    }
}

/// The text of a Markdown block-quote line (`> text`), not a doctest prompt.
fn quote_text(t: &str) -> Option<&str> {
    if t.starts_with(">>>") {
        return None;
    }
    if t == ">" {
        return Some("");
    }
    t.strip_prefix("> ")
}

/// The title of a GitHub-style alert (`[!NOTE]`, `[!WARNING]`, …) opening a quote.
fn alert_title(t: &str) -> Option<&'static str> {
    let kind = t.trim().strip_prefix("[!")?.strip_suffix(']')?;
    Some(match kind.to_ascii_uppercase().as_str() {
        "NOTE" => "Note",
        "TIP" => "Tip",
        "IMPORTANT" => "Important",
        "WARNING" => "Warning",
        "CAUTION" => "Caution",
        _ => return None,
    })
}

/// The title of a Hugging Face doc-builder `<Tip>` / `<Tip warning={true}>` line.
fn tip_title(t: &str) -> Option<&'static str> {
    match t.replace(' ', "").as_str() {
        "<Tip>" => Some("Tip"),
        "<Tipwarning={true}>" => Some("Warning"),
        _ => None,
    }
}

/// Whether line `i` starts a construct that ends a running paragraph.
fn starts_construct(lines: &[Line], i: usize) -> bool {
    let t = lines[i].0.trim();
    t.starts_with(">>>")
        || fence_of(t).is_some()
        || list_marker(t).is_some()
        || t == ".."
        || t.starts_with(".. ")
        || is_table_border(t)
        || quote_text(t).is_some()
        || tip_title(t).is_some()
        || t == "</Tip>"
        || (i + 1 < lines.len() && is_adornment(&lines[i + 1].0) && !is_adornment(t))
}

/// Nesting depth of blocks (indented blocks, block quotes, admonitions,
/// `<Tip>`s) shown as such; deeper text shows as written, preformatted. Real
/// docstrings nest a few levels; a crafted one could otherwise nest
/// thousands deep and overflow the stack.
const MAX_DEPTH: u8 = 12;

/// Blocks of free text: paragraphs, literal blocks (`::`), doctests, fenced
/// code, lists, reST directives and section titles, Markdown headings and
/// indented blocks.
fn body(lines: &[Line], depth: u8, epy: bool) -> Vec<DocBlock> {
    if depth >= MAX_DEPTH {
        return preformatted(lines, depth);
    }
    let mut out = Vec::new();
    let base = lines
        .iter()
        .filter(|l| !is_blank(l))
        .map(|l| indent_of(&l.0))
        .min()
        .unwrap_or(0);
    let mut tables: Option<TableEnds> = None;
    let mut i = 0;
    while i < lines.len() {
        if is_blank(&lines[i]) {
            i += 1;
            continue;
        }
        let (text, src) = (&lines[i].0, lines[i].1);
        let ind = indent_of(text);
        let t = text.trim();
        if ind > base {
            // Indented block (reST block quote / definition): nested one level.
            let end = block_end(lines, i, base);
            out.extend(body(&lines[i..end], depth + 1, epy));
            i = end;
            continue;
        }
        if let Some(image) = t
            .strip_prefix("![")
            .and_then(|rest| rest.split_once("]("))
            .and_then(|(alt, target)| target.strip_suffix(')').map(|target| (alt, target)))
        {
            out.push(block(
                BlockKind::Image {
                    target: image.1.trim().into(),
                    alt: image.0.into(),
                },
                depth,
                src,
            ));
            i += 1;
            continue;
        }
        if let Some(fence) = fence_of(t) {
            let lang = t[fence.len()..].trim().to_ascii_lowercase();
            let mut j = i + 1;
            while j < lines.len() && !lines[j].0.trim().starts_with(fence) {
                j += 1;
            }
            out.push(block(
                BlockKind::Code {
                    text: dedent_text(&lines[i + 1..j]),
                    python: matches!(
                        lang.as_str(),
                        "" | "python" | "py" | "pycon" | "python3" | "ipython"
                    ),
                },
                depth,
                src,
            ));
            i = (j + 1).min(lines.len());
            continue;
        }
        if t.starts_with(">>>") {
            let mut j = i;
            while j < lines.len() && !is_blank(&lines[j]) {
                j += 1;
            }
            out.push(block(
                BlockKind::Code {
                    text: dedent_text(&lines[i..j]),
                    python: true,
                },
                depth,
                src,
            ));
            i = j;
            continue;
        }
        if is_table_border(t) {
            // A table: preformatted, as laid out in the source.
            let end = tables
                .get_or_insert_with(|| TableEnds::new(lines))
                .end(lines, i);
            out.push(block(
                BlockKind::Code {
                    text: dedent_text(&lines[i..end]),
                    python: false,
                },
                depth,
                src,
            ));
            i = end;
            continue;
        }
        if quote_text(t).is_some() {
            // A Markdown block quote; `> [!NOTE]` & co. make it a titled GitHub alert.
            let mut quoted: Vec<Line> = Vec::new();
            let mut j = i;
            while let Some(q) = lines.get(j).and_then(|l| quote_text(l.0.trim_start())) {
                quoted.push((q.to_string(), lines[j].1));
                j += 1;
            }
            let title = quoted.first().and_then(|q| alert_title(&q.0));
            if let Some(title) = title {
                out.push(block(BlockKind::Heading(title.to_string()), depth, src));
            }
            out.extend(body(
                &quoted[usize::from(title.is_some())..],
                depth + 1,
                epy,
            ));
            i = j;
            continue;
        }
        if let Some(title) = tip_title(t) {
            // Hugging Face doc-builder `<Tip>` … `</Tip>`.
            let end = (i + 1..lines.len())
                .find(|&j| lines[j].0.trim() == "</Tip>")
                .unwrap_or(lines.len());
            out.push(block(BlockKind::Heading(title.to_string()), depth, src));
            out.extend(body(&lines[i + 1..end], depth + 1, epy));
            i = (end + 1).min(lines.len());
            continue;
        }
        if t == "</Tip>" {
            i += 1;
            continue;
        }
        if t == ".." || t.starts_with(".. ") {
            let end = block_end(lines, i + 1, ind);
            directive(&lines[i], &lines[i + 1..end], depth, epy, &mut out);
            i = end;
            continue;
        }
        if is_adornment(t) {
            // Overline + title + underline, or a transition line.
            if i + 2 < lines.len() && !is_blank(&lines[i + 1]) && is_adornment(&lines[i + 2].0) {
                out.push(block(
                    BlockKind::Heading(lines[i + 1].0.trim().to_string()),
                    depth,
                    lines[i + 1].1,
                ));
                i += 3;
            } else {
                i += 1;
            }
            continue;
        }
        if i + 1 < lines.len() && is_adornment(&lines[i + 1].0) && indent_of(&lines[i + 1].0) == ind
        {
            out.push(block(BlockKind::Heading(t.to_string()), depth, src));
            i += 2;
            continue;
        }
        if let Some(h) = MD_HEADING.captures(t).and_then(|c| c.get(1)) {
            out.push(block(
                BlockKind::Heading(h.as_str().to_string()),
                depth,
                src,
            ));
            i += 1;
            continue;
        }
        if let Some((marker, first)) = list_marker(t) {
            let mut parts = vec![first.to_string()];
            let mut j = i + 1;
            while j < lines.len() {
                let l = &lines[j].0;
                // A nested item (deeper, with its own marker) becomes an indented block.
                if is_blank(&lines[j])
                    || indent_of(l) < ind
                    || (indent_of(l) == ind && starts_construct(lines, j))
                    || (indent_of(l) > ind && list_marker(l.trim()).is_some())
                {
                    break;
                }
                parts.push(l.trim().to_string());
                j += 1;
            }
            out.push(block(
                BlockKind::Item {
                    marker,
                    spans: inline(&parts.join(" "), epy),
                },
                depth,
                src,
            ));
            i = j;
            continue;
        }
        // A paragraph: the following lines at the same indentation.
        let mut parts = vec![t.to_string()];
        let mut j = i + 1;
        while j < lines.len()
            && !is_blank(&lines[j])
            && indent_of(&lines[j].0) == ind
            && !starts_construct(lines, j)
        {
            parts.push(lines[j].0.trim().to_string());
            j += 1;
        }
        let mut para = parts.join(" ");
        let literal = para.ends_with("::");
        // One line right above a deeper indented block: a definition-list term.
        let term = j == i + 1
            && !literal
            && lines
                .get(j)
                .is_some_and(|l| !is_blank(l) && indent_of(&l.0) > ind);
        if literal {
            // reST: "Text::" shows "Text:", "Text ::" and a lone "::" show nothing.
            para.truncate(para.len() - 2);
            if para.ends_with(char::is_whitespace) || para.is_empty() {
                para = para.trim_end().to_string();
            } else {
                para.push(':');
            }
        }
        if term {
            out.push(block(BlockKind::Term(inline(&para, epy)), depth, src));
        } else if !para.is_empty() {
            out.push(block(BlockKind::Paragraph(inline(&para, epy)), depth, src));
        }
        if literal {
            let mut k = j;
            while k < lines.len() && is_blank(&lines[k]) {
                k += 1;
            }
            if k < lines.len() && indent_of(&lines[k].0) > ind {
                let end = block_end(lines, k, ind);
                let text = dedent_text(&lines[k..end]);
                let python = text.starts_with(">>>");
                out.push(block(BlockKind::Code { text, python }, depth, lines[k].1));
                j = end;
            }
        }
        i = j;
    }
    out
}

/// `lines` as one preformatted block, as written (text nested too deeply).
fn preformatted(lines: &[Line], depth: u8) -> Vec<DocBlock> {
    let Some(first) = lines.iter().find(|l| !is_blank(l)) else {
        return Vec::new();
    };
    vec![block(
        BlockKind::Code {
            text: dedent_text(lines),
            python: false,
        },
        depth,
        first.1,
    )]
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// A reST directive (`.. note::`, `.. code-block:: python`, `.. versionadded:: 1.2`, …).
/// Comments and targets show nothing; directives without a rendering of
/// their own show their content as indented text.
fn directive(head: &Line, content: &[Line], depth: u8, epy: bool, out: &mut Vec<DocBlock>) {
    if let Some(c) = CITATION.captures(head.0.trim()) {
        // A citation or footnote (`.. [1] Author, Title.`, NumPy "References"): a list item.
        let mut text = c[2].trim().to_string();
        for l in content.iter().filter(|l| !is_blank(l)) {
            text.push(' ');
            text.push_str(l.0.trim());
        }
        out.push(block(
            BlockKind::Item {
                marker: format!("[{}]", &c[1]),
                spans: inline(text.trim(), epy),
            },
            depth,
            head.1,
        ));
        return;
    }
    let Some(c) = DIRECTIVE.captures(head.0.trim()) else {
        return;
    };
    let full_name = c[1].to_ascii_lowercase();
    // Sphinx's Python domain may be named explicitly (`.. py:function::`).
    let name = full_name.strip_prefix("py:").unwrap_or(&full_name);
    let arg = c[2].trim().to_string();
    // Directive options (`:linenos:`) precede the content.
    let mut start = 0;
    while start < content.len()
        && (is_blank(&content[start]) || DIRECTIVE_OPTION.is_match(content[start].0.trim()))
    {
        start += 1;
    }
    let options = &content[..start];
    let content = &content[start..];
    let first_line = content.first().map(|l| l.1).unwrap_or(head.1);
    match name {
        "code-block" | "code" | "sourcecode" | "testcode" | "testsetup" | "testcleanup"
        | "doctest" | "ipython" | "jupyter-execute" | "plot" => {
            let python = matches!(name, "doctest" | "plot" | "testsetup" | "testcleanup")
                || matches!(
                    arg.to_ascii_lowercase().as_str(),
                    "" | "python" | "py" | "pycon" | "python3" | "ipython"
                );
            if !content.is_empty() {
                out.push(block(
                    BlockKind::Code {
                        text: dedent_text(content),
                        python,
                    },
                    depth,
                    first_line,
                ));
            }
        }
        // Preformatted text: program output, CSV tables, raw markup, grammars.
        "testoutput" | "parsed-literal" | "csv-table" | "raw" | "productionlist" => {
            if name == "csv-table" && !arg.is_empty() {
                out.push(block(BlockKind::Heading(arg.clone()), depth, head.1));
            }
            if !content.is_empty() {
                out.push(block(
                    BlockKind::Code {
                        text: dedent_text(content),
                        python: false,
                    },
                    depth,
                    first_line,
                ));
            }
        }
        "math" => {
            let mut text = arg.clone();
            let rest = dedent_text(content);
            if !rest.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&rest);
            }
            out.push(block(
                BlockKind::Code {
                    text,
                    python: false,
                },
                depth,
                head.1,
            ));
        }
        "image" | "figure" => {
            let alt = options
                .iter()
                .find_map(|line| line.0.trim().strip_prefix(":alt:").map(str::trim))
                .unwrap_or(&arg)
                .to_string();
            out.push(block(BlockKind::Image { target: arg, alt }, depth, head.1));
            if name == "figure" {
                let caption: Vec<_> = content
                    .iter()
                    .filter(|line| !line.0.trim().starts_with(':'))
                    .cloned()
                    .collect();
                out.extend(body(&caption, depth + 1, epy));
            }
        }
        "rubric" => out.push(block(BlockKind::Heading(arg), depth, head.1)),
        "centered" => out.push(block(
            BlockKind::Paragraph(inline(&arg, epy)),
            depth,
            head.1,
        )),
        "note" | "warning" | "tip" | "hint" | "important" | "caution" | "attention" | "danger"
        | "error" | "seealso" | "todo" | "admonition" => {
            let title = match name {
                "seealso" => "See also".to_string(),
                "admonition" => arg.clone(),
                other => title_case(other),
            };
            out.push(block(BlockKind::Heading(title), depth, head.1));
            let mut text: Vec<Line> = Vec::new();
            if name != "admonition" && !arg.is_empty() {
                // The text may start on the directive line; its continuation lines are indented.
                let indent = content
                    .iter()
                    .filter(|l| !is_blank(l))
                    .map(|l| indent_of(&l.0))
                    .min()
                    .unwrap_or(0);
                text.push((format!("{}{arg}", " ".repeat(indent)), head.1));
            }
            text.extend(content.iter().cloned());
            out.extend(body(&text, depth + 1, epy));
        }
        "versionadded" | "versionchanged" | "deprecated" | "versionremoved"
        | "deprecated-removed" => {
            let label = match name {
                "versionadded" => "New in version",
                "versionchanged" => "Changed in version",
                "deprecated" | "deprecated-removed" => "Deprecated since version",
                _ => "Removed in version",
            };
            let split = |s: &str| {
                s.split_once(char::is_whitespace)
                    .map(|(v, r)| (v.to_string(), r.trim().to_string()))
                    .unwrap_or((s.to_string(), String::new()))
            };
            let (version, mut rest) = split(&arg);
            let mut title = format!("{label} {version}").trim_end().to_string();
            if name == "deprecated-removed" && !rest.is_empty() {
                // `.. deprecated-removed:: 3.12 3.14`: deprecated, and the version that removes it.
                let (removed, more) = split(&rest);
                title.push_str(&format!(", will be removed in version {removed}"));
                rest = more;
            }
            let mut more: Vec<String> = Vec::new();
            if !rest.is_empty() {
                more.push(rest);
            }
            more.extend(
                content
                    .iter()
                    .filter(|l| !is_blank(l))
                    .map(|l| l.0.trim().to_string()),
            );
            let mut spans = vec![Span::new(title, SpanStyle::Emphasis)];
            if more.is_empty() {
                spans.push(Span::new(".", SpanStyle::Text));
            } else {
                spans.push(Span::new(": ", SpanStyle::Text));
                spans.extend(inline(&more.join(" "), epy));
            }
            out.push(block(BlockKind::Paragraph(merge(spans)), depth, head.1));
        }
        // A list of documented objects (the numpy / scipy API listings): one per line.
        "autosummary" => {
            for l in content.iter().filter(|l| !is_blank(l)) {
                out.push(block(
                    BlockKind::Item {
                        marker: "•".to_string(),
                        spans: vec![Span::new(role_display(l.0.trim()), SpanStyle::Code)],
                    },
                    depth,
                    l.1,
                ));
            }
        }
        // A titled block: the title, then the content.
        "topic" | "sidebar" | "table" | "list-table" => {
            if !arg.is_empty() {
                out.push(block(BlockKind::Heading(arg), depth, head.1));
            }
            out.extend(body(content, depth + 1, epy));
        }
        // A Python object (`.. function:: f(x)`, `.. autoclass:: Foo`): its signature, then its description.
        n if is_object_description(n) => {
            if !arg.is_empty() {
                out.push(block(
                    BlockKind::Term(vec![Span::new(arg, SpanStyle::Code)]),
                    depth,
                    head.1,
                ));
            }
            out.extend(body(content, depth + 1, epy));
        }
        // Index entries and the table of contents show nothing in rendered documentation.
        "index" | "toctree" => {}
        // Any other directive (`container`, `glossary`, `only`, `legacy`, …): its content, as indented text.
        _ => out.extend(body(content, depth + 1, epy)),
    }
}

/// Sphinx directives that describe a Python object (`.. attribute:: name`,
/// `.. autofunction:: pkg.f`), with the Python domain's `py:` prefix removed.
fn is_object_description(name: &str) -> bool {
    matches!(
        name,
        "function"
            | "method"
            | "classmethod"
            | "staticmethod"
            | "class"
            | "attribute"
            | "data"
            | "exception"
            | "property"
            | "decorator"
            | "decoratormethod"
            | "type"
    ) || (name.starts_with("auto") && name != "autosummary")
}

// ---------------------------------------------------------------------------
// reStructuredText / Epytext field lists
// ---------------------------------------------------------------------------

static FIELD: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s*([:@])([A-Za-z_][\w-]*)(?:\s+([^:]+?))?\s*:(.*)$").unwrap());

struct Entry {
    name: String,
    ty: Option<String>,
    desc: String,
    line: usize,
}

fn entry(name: &str, ty: Option<String>, desc: &str, line: usize) -> Entry {
    Entry {
        name: name.to_string(),
        ty,
        desc: desc.to_string(),
        line,
    }
}

/// Field-list groups in the order Sphinx shows them.
#[derive(Default)]
struct Fields {
    params: Vec<Entry>,
    keywords: Vec<Entry>,
    returns: Option<Entry>,
    yields: Option<Entry>,
    raises: Vec<Entry>,
    variables: Vec<Entry>,
    /// Other fields (`:note:`, `@see`, …) as `(title, entry)`.
    other: Vec<(String, Entry)>,
}

/// A field-list line: `(field name, argument, description)`.
fn field_line(line: &str, epy: bool) -> Option<(String, String, String)> {
    let c = FIELD.captures(line)?;
    let marker = c.get(1)?.as_str();
    let name = c.get(2)?.as_str().to_string();
    if marker == "@" && !(epy && EPY_FIELD.is_match(line)) {
        return None;
    }
    let known = REST_FIELD.is_match(line) || EPY_FIELD.is_match(line);
    // `:class:` followed by a backquote is a role, not a field; unknown names need a space after the colon.
    if !known
        && marker == ":"
        && !c
            .get(4)
            .is_some_and(|m| m.as_str().is_empty() || m.as_str().starts_with(char::is_whitespace))
    {
        return None;
    }
    let arg = c
        .get(3)
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default();
    let desc = c
        .get(4)
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default();
    Some((name, arg, desc))
}

fn strip_stars(name: &str) -> &str {
    name.trim_start_matches('*')
}

/// `s` without reST backslash escapes (`\*\*kwargs` → `**kwargs`).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' {
            chars.next().unwrap_or('\\')
        } else {
            c
        });
    }
    out
}

impl Fields {
    fn add(&mut self, field: &str, arg: &str, desc: &str, line: usize, epy: bool) {
        let arg = unescape(arg);
        let arg = arg.as_str();
        let typed_name = |arg: &str| -> (String, Option<String>) {
            match arg.rsplit_once(' ') {
                Some((t, n)) => (n.trim().to_string(), Some(type_text(t, epy))),
                None => (arg.to_string(), None),
            }
        };
        match field {
            "param" | "parameter" | "arg" | "argument" | "key" | "keyword" | "kwarg"
            | "kwparam" => {
                let (name, ty) = typed_name(arg);
                let group = if matches!(field, "key" | "keyword" | "kwarg" | "kwparam") {
                    &mut self.keywords
                } else {
                    &mut self.params
                };
                match group
                    .iter_mut()
                    .find(|e| strip_stars(&e.name) == strip_stars(&name))
                {
                    Some(e) => {
                        e.desc = desc.to_string();
                        e.ty = ty.or(e.ty.take());
                    }
                    None => group.push(entry(&name, ty, desc, line)),
                }
            }
            "type" => {
                let ty = Some(type_text(desc, epy));
                match self
                    .params
                    .iter_mut()
                    .chain(self.keywords.iter_mut())
                    .find(|e| strip_stars(&e.name) == strip_stars(arg))
                {
                    Some(e) => e.ty = ty,
                    None => self.params.push(entry(arg, ty, "", line)),
                }
            }
            "return" | "returns" => {
                self.returns
                    .get_or_insert_with(|| entry("", None, "", line))
                    .desc = desc.to_string()
            }
            "rtype" => {
                self.returns
                    .get_or_insert_with(|| entry("", None, "", line))
                    .ty = Some(type_text(desc, epy))
            }
            "yield" | "yields" => {
                self.yields
                    .get_or_insert_with(|| entry("", None, "", line))
                    .desc = desc.to_string()
            }
            "ytype" => {
                self.yields
                    .get_or_insert_with(|| entry("", None, "", line))
                    .ty = Some(type_text(desc, epy))
            }
            "raise" | "raises" | "except" | "exception" => {
                self.raises.push(entry(arg, None, desc, line))
            }
            "ivar" | "var" | "cvar" | "attr" | "attribute" => {
                let (name, ty) = typed_name(arg);
                self.variables.push(entry(&name, ty, desc, line));
            }
            "vartype" => {
                let ty = Some(type_text(desc, epy));
                match self.variables.iter_mut().find(|e| e.name == arg) {
                    Some(e) => e.ty = ty,
                    None => self.variables.push(entry(arg, ty, "", line)),
                }
            }
            "meta" | "sort" | "undocumented" => {}
            other => {
                let title = match other {
                    "see" | "seealso" => "See also".to_string(),
                    other => title_case(&other.replace(['_', '-'], " ")),
                };
                self.other.push((title, entry(arg, None, desc, line)));
            }
        }
    }

    /// Continuation text for the field added last.
    fn extend_last(&mut self, field: &str, more: &str) {
        let target = match field {
            "param" | "parameter" | "arg" | "argument" => {
                self.params.last_mut().map(|e| &mut e.desc)
            }
            "key" | "keyword" | "kwarg" | "kwparam" => {
                self.keywords.last_mut().map(|e| &mut e.desc)
            }
            "type" | "rtype" | "ytype" | "vartype" | "meta" | "sort" | "undocumented" => None,
            "return" | "returns" => self.returns.as_mut().map(|e| &mut e.desc),
            "yield" | "yields" => self.yields.as_mut().map(|e| &mut e.desc),
            "raise" | "raises" | "except" | "exception" => {
                self.raises.last_mut().map(|e| &mut e.desc)
            }
            "ivar" | "var" | "cvar" | "attr" | "attribute" => {
                self.variables.last_mut().map(|e| &mut e.desc)
            }
            _ => self.other.last_mut().map(|(_, e)| &mut e.desc),
        };
        if let Some(desc) = target {
            if !desc.is_empty() {
                desc.push(' ');
            }
            desc.push_str(more);
        }
    }

    fn into_blocks(self, epy: bool, out: &mut Vec<DocBlock>) {
        let section = |out: &mut Vec<DocBlock>, title: &str, entries: Vec<Entry>| {
            let Some(first) = entries.first() else { return };
            out.push(block(BlockKind::Heading(title.to_string()), 0, first.line));
            for e in entries {
                out.push(block(
                    BlockKind::Field {
                        name: e.name,
                        ty: e.ty,
                        desc: inline(&e.desc, epy),
                    },
                    1,
                    e.line,
                ));
            }
        };
        section(out, "Parameters", self.params);
        section(out, "Keyword Arguments", self.keywords);
        section(out, "Returns", self.returns.into_iter().collect());
        section(out, "Yields", self.yields.into_iter().collect());
        section(out, "Raises", self.raises);
        section(out, "Variables", self.variables);
        for (title, e) in self.other {
            out.push(block(BlockKind::Heading(title), 0, e.line));
            let text = if e.name.is_empty() {
                e.desc
            } else {
                format!("{} {}", e.name, e.desc)
            };
            if !text.trim().is_empty() {
                out.push(block(
                    BlockKind::Paragraph(inline(text.trim(), epy)),
                    1,
                    e.line,
                ));
            }
        }
    }
}

/// A type written in a field (`:class:`int``, `C{int}`) as plain text.
fn type_text(t: &str, epy: bool) -> String {
    plain_text(&inline(t.trim(), epy))
}

/// reStructuredText and Epytext docstrings: free text, then field lists.
fn fields_doc(lines: &[Line], epy: bool) -> Vec<DocBlock> {
    let mut text: Vec<Line> = Vec::new();
    let mut fields = Fields::default();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].0.trim_start().starts_with(".. ") {
            // A directive: its options (`:type:`, `:toctree:`) and content are its own, not fields.
            let end = block_end(lines, i + 1, indent_of(&lines[i].0));
            text.extend_from_slice(&lines[i..end]);
            i = end;
            continue;
        }
        let Some((name, arg, desc)) = field_line(&lines[i].0, epy) else {
            text.push(lines[i].clone());
            i += 1;
            continue;
        };
        let field_indent = indent_of(&lines[i].0);
        fields.add(&name, &arg, &desc, lines[i].1, epy);
        let mut j = i + 1;
        loop {
            // Continuation lines up to the next field or a blank line; after a blank
            // line, a paragraph indented deeper than the field still belongs to it.
            if j < lines.len() && is_blank(&lines[j]) {
                let mut k = j;
                while k < lines.len() && is_blank(&lines[k]) {
                    k += 1;
                }
                if k < lines.len()
                    && indent_of(&lines[k].0) > field_indent
                    && field_line(&lines[k].0, epy).is_none()
                {
                    j = k;
                } else {
                    break;
                }
            }
            if j >= lines.len()
                || field_line(&lines[j].0, epy).is_some()
                || (indent_of(&lines[j].0) < field_indent)
            {
                break;
            }
            fields.extend_last(&name, lines[j].0.trim());
            j += 1;
        }
        i = j;
    }
    let mut out = body(&text, 0, epy);
    fields.into_blocks(epy, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Google and NumPy sections
// ---------------------------------------------------------------------------

/// Entries of a section body: a line at the body's smallest indentation starts
/// an entry, deeper lines continue it. `(head, continuation, line)`.
fn entries(lines: &[Line]) -> Vec<(String, Vec<String>, usize)> {
    let base = lines
        .iter()
        .filter(|l| !is_blank(l))
        .map(|l| indent_of(&l.0))
        .min()
        .unwrap_or(0);
    let mut out: Vec<(String, Vec<String>, usize)> = Vec::new();
    for l in lines.iter().filter(|l| !is_blank(l)) {
        if indent_of(&l.0) <= base || out.is_empty() {
            out.push((l.0.trim().to_string(), Vec::new(), l.1));
        } else if let Some(e) = out.last_mut() {
            e.1.push(l.0.trim().to_string());
        }
    }
    out
}

fn joined(head: &str, cont: &[String]) -> String {
    let mut s = head.trim().to_string();
    for c in cont {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(c);
    }
    s
}

#[derive(Clone, Copy, PartialEq)]
enum Section {
    /// `name (type): desc` / `name : type` entries.
    Params,
    /// One return value: `type: desc` / `name : type`.
    Returns,
    /// `Exception: desc` entries.
    Raises,
    /// `name : desc` cross references (NumPy "See Also").
    SeeAlso,
    /// Free text (examples, notes, warnings, …).
    Text,
}

fn section_kind(name: &str) -> Section {
    match name {
        "Args" | "Arguments" | "Parameters" | "Params" | "Keyword Args" | "Keyword Arguments"
        | "Kwargs" | "Other Parameters" | "Attributes" | "Methods" | "Receives" => Section::Params,
        "Returns" | "Return" | "Yields" | "Yield" => Section::Returns,
        "Raises" | "Raise" | "Except" | "Exceptions" | "Warns" => Section::Raises,
        "See Also" => Section::SeeAlso,
        _ => Section::Text,
    }
}

/// The title a section is shown with.
fn section_title(name: &str) -> &str {
    match name {
        "Args" | "Arguments" | "Params" => "Parameters",
        "Keyword Args" | "Kwargs" => "Keyword Arguments",
        "Return" => "Returns",
        "Yield" => "Yields",
        "Raise" | "Except" | "Exceptions" => "Raises",
        "Example" => "Examples",
        other => other,
    }
}

/// Google `name (type): description`.
fn google_field(head: &str, cont: &[String]) -> Option<BlockKind> {
    let c = GOOGLE_ARG.captures(head)?;
    let ty = c
        .get(2)
        .map(|m| type_text(m.as_str(), false))
        .filter(|t| !t.is_empty());
    Some(BlockKind::Field {
        name: c[1].to_string(),
        ty,
        desc: inline(&joined(c.get(3).map_or("", |m| m.as_str()), cont), false),
    })
}

/// NumPy `name : type` (or a bare `name`).
fn numpy_field(head: &str, cont: &[String]) -> BlockKind {
    let (name, ty) = match head.split_once(" : ") {
        Some((n, t)) => (n.trim(), Some(type_text(t, false))),
        None => match head.split_once(':') {
            Some((n, t)) if !n.trim().contains(' ') && !t.trim().is_empty() => {
                (n.trim(), Some(type_text(t, false)))
            }
            _ => (head.trim().trim_end_matches(':'), None),
        },
    };
    BlockKind::Field {
        name: unescape(name),
        ty: ty.filter(|t| !t.is_empty()),
        desc: inline(&joined("", cont), false),
    }
}

fn section_blocks(name: &str, line: usize, lines: &[Line], numpy: bool, out: &mut Vec<DocBlock>) {
    out.push(block(
        BlockKind::Heading(section_title(name).to_string()),
        0,
        line,
    ));
    let kind = section_kind(name);
    if kind == Section::Text {
        out.extend(body(lines, 1, false));
        return;
    }
    if kind == Section::Returns && !numpy {
        // Google: `type: description` on the first line, or plain text.
        let es = entries(lines);
        let first = es.first().map(|e| e.0.clone()).unwrap_or_default();
        match first.split_once(':') {
            Some((t, d)) if docstrings::looks_like_type(t) && !t.trim().is_empty() => {
                let mut desc = joined(d, &es[0].1);
                for e in &es[1..] {
                    if !desc.is_empty() {
                        desc.push(' ');
                    }
                    desc.push_str(&joined(&e.0, &e.1));
                }
                out.push(block(
                    BlockKind::Field {
                        name: String::new(),
                        ty: Some(type_text(t, false)),
                        desc: inline(&desc, false),
                    },
                    1,
                    es[0].2,
                ));
            }
            _ => out.extend(body(lines, 1, false)),
        }
        return;
    }
    for (head, cont, l) in entries(lines) {
        let kind = match kind {
            Section::Params if numpy => numpy_field(&head, &cont),
            Section::Params => match google_field(&head, &cont) {
                Some(k) => k,
                None => BlockKind::Paragraph(inline(&joined(&head, &cont), false)),
            },
            Section::Returns => match head.split_once(" : ") {
                Some((n, t)) => BlockKind::Field {
                    name: unescape(n.trim()),
                    ty: Some(type_text(t, false)),
                    desc: inline(&joined("", &cont), false),
                },
                None => BlockKind::Field {
                    name: String::new(),
                    ty: Some(type_text(&head, false)),
                    desc: inline(&joined("", &cont), false),
                },
            },
            Section::Raises => match GOOGLE_RAISE.captures(&head).filter(|_| !numpy) {
                Some(c) => BlockKind::Field {
                    name: c[1].to_string(),
                    ty: None,
                    desc: inline(&joined(&c[2], &cont), false),
                },
                None => BlockKind::Field {
                    name: head.trim().to_string(),
                    ty: None,
                    desc: inline(&joined("", &cont), false),
                },
            },
            Section::SeeAlso | Section::Text => {
                match head.split_once(" : ").or_else(|| head.split_once(": ")) {
                    Some((n, d)) => BlockKind::Field {
                        name: n.trim().to_string(),
                        ty: None,
                        desc: inline(&joined(d, &cont), false),
                    },
                    None => BlockKind::Field {
                        name: head.trim().to_string(),
                        ty: None,
                        desc: inline(&joined("", &cont), false),
                    },
                }
            }
        };
        out.push(block(kind, 1, l));
    }
}

/// Google style: free text and `Section:` headers with indented bodies.
fn google_doc(lines: &[Line]) -> Vec<DocBlock> {
    let mut out = Vec::new();
    let mut text: Vec<Line> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(c) = GOOGLE_SECTION.captures(&lines[i].0) else {
            text.push(lines[i].clone());
            i += 1;
            continue;
        };
        let header_indent = c.get(1).map_or(0, |m| m.as_str().len());
        let name = c[2].to_string();
        out.extend(body(&std::mem::take(&mut text), 0, false));
        let end = block_end(lines, i + 1, header_indent);
        section_blocks(&name, lines[i].1, &lines[i + 1..end], false, &mut out);
        i = end;
    }
    out.extend(body(&text, 0, false));
    out
}

/// NumPy style: free text and underlined section headers.
fn numpy_doc(lines: &[Line]) -> Vec<DocBlock> {
    let is_header = |i: usize| {
        NUMPY_SECTION.is_match(&lines[i].0)
            && lines.get(i + 1).is_some_and(|n| {
                docstrings::is_underline(&n.0, '-') || docstrings::is_underline(&n.0, '=')
            })
    };
    let mut out = Vec::new();
    let mut text: Vec<Line> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !is_header(i) {
            text.push(lines[i].clone());
            i += 1;
            continue;
        }
        out.extend(body(&std::mem::take(&mut text), 0, false));
        let name = lines[i].0.trim().to_string();
        let mut end = i + 2;
        while end < lines.len() && !is_header(end) {
            end += 1;
        }
        section_blocks(
            &name,
            lines[i].1,
            &lines[(i + 2).min(end)..end],
            true,
            &mut out,
        );
        i = end;
    }
    out.extend(body(&text, 0, false));
    out
}

// ---------------------------------------------------------------------------
// Inline markup
// ---------------------------------------------------------------------------

/// An interpreted-text role, ``:role:`target` ``. Role names are short: a
/// bound keeps a crafted run like `:a-:a-:a-…` cheap at each of its colons.
static ROLE_AT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^:((?:[A-Za-z][0-9A-Za-z_.+-]{0,40}:){0,3}[A-Za-z][0-9A-Za-z_.+-]{0,40}):`([^`]+)`",
    )
    .unwrap()
});
static REF_TARGET: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(?s)(.*?)\s*<([^<>]+)>$").unwrap());
/// Hugging Face doc-builder cross-reference: ``[`Class`]``, ``[`~module.func`]``.
static MD_CODE_REF: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\[`([^`\]]+)`\]").unwrap());

/// Nesting depth of inline markup (`**…**`, link texts, Epytext `B{…}`)
/// parsed as such; deeper markup shows as written.
const MAX_INLINE_DEPTH: u8 = 6;

/// reST inline markup may start after whitespace, an opening bracket/quote or a dash.
fn start_ok(text: &str, i: usize) -> bool {
    text[..i]
        .chars()
        .next_back()
        .is_none_or(|c| c.is_whitespace() || "'\"([{<-/:‘“".contains(c))
}

/// Whether the `len`-byte marker at byte `p` can close inline markup:
/// preceded by a non-space (not an escaping backslash) and followed by the
/// end, a space or punctuation.
fn closes(text: &str, p: usize, len: usize) -> bool {
    let before_ok = text[..p]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace() && c != '\\');
    let after_ok = text[p + len..]
        .chars()
        .next()
        .is_none_or(|c| c.is_whitespace() || "'\")]}>-/:.,;!?\\’”".contains(c));
    before_ok && after_ok
}

/// "None" in the lookup tables of [`Scan`].
const NONE: u32 = u32::MAX;

fn at(table: &[u32], i: usize) -> Option<usize> {
    table
        .get(i)
        .copied()
        .filter(|&p| p != NONE)
        .map(|p| p as usize)
}

/// For each byte of `text`, the first character at or after it that satisfies `pred`.
fn next_char_table(text: &str, pred: impl Fn(char) -> bool) -> Vec<u32> {
    let mut out = vec![NONE; text.len() + 1];
    let (mut next, mut end) = (NONE, text.len());
    for (p, c) in text.char_indices().rev() {
        if pred(c) {
            next = p as u32;
        }
        out[p..end].fill(next);
        end = p;
    }
    out
}

/// Lookups over the text of one paragraph, each built on first use, that keep
/// its inline-markup scan linear: a crafted paragraph of thousands of `*`, `[`
/// or `B{` that never close would otherwise be searched to its end once for
/// each of them.
struct Scan<'a> {
    text: &'a str,
    /// For `*` and `**`: the closer that a search starting at a byte finds (see [`Scan::markup_end`]).
    closers: [Option<Vec<u32>>; 2],
    /// The next `]` at or after a byte.
    next_bracket: Option<Vec<u32>>,
    /// The next `)` at or after a byte.
    next_paren: Option<Vec<u32>>,
    /// The next whitespace character at or after a byte.
    next_space: Option<Vec<u32>>,
    /// The `}` that closes the `{` at a byte.
    brace_close: Option<Vec<u32>>,
}

impl<'a> Scan<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            closers: [None, None],
            next_bracket: None,
            next_paren: None,
            next_space: None,
            brace_close: None,
        }
    }

    /// Where the `*` / `**` markup opened at byte `i` closes: the first marker
    /// after the content's first character that [`closes`] it. Returns the
    /// content length; `None` when the content starts with a space or nothing closes it.
    fn markup_end(&mut self, i: usize, marker: &str) -> Option<usize> {
        let text = self.text;
        let start = i + marker.len();
        let first = text[start..].chars().next()?;
        if first.is_whitespace() {
            return None;
        }
        let table = self.closers[marker.len() - 1].get_or_insert_with(|| {
            // Backwards: a search from byte `q` takes the marker at `q` when it
            // closes, else continues after it (markers found do not overlap);
            // without a marker at `q` it continues at `q + 1`.
            let (b, m) = (text.as_bytes(), marker.len());
            let mut out = vec![NONE; b.len() + 1];
            for q in (0..b.len()).rev() {
                out[q] = if b[q..].starts_with(marker.as_bytes()) {
                    if closes(text, q, m) {
                        q as u32
                    } else {
                        out[q + m]
                    }
                } else {
                    out[q + 1]
                };
            }
            out
        });
        at(table, start + first.len_utf8()).map(|p| p - start)
    }

    fn next_bracket(&mut self, i: usize) -> Option<usize> {
        let text = self.text;
        at(
            self.next_bracket
                .get_or_insert_with(|| next_char_table(text, |c| c == ']')),
            i,
        )
    }

    fn next_space(&mut self, i: usize) -> Option<usize> {
        let text = self.text;
        at(
            self.next_space
                .get_or_insert_with(|| next_char_table(text, char::is_whitespace)),
            i,
        )
    }

    /// A Markdown link `[text](url)` at byte `i`: its text, its target and its length.
    fn md_link(&mut self, i: usize) -> Option<(&'a str, &'a str, usize)> {
        let text = self.text;
        let close = self.next_bracket(i + 1)?;
        if close == i + 1 || !text[close + 1..].starts_with('(') {
            return None;
        }
        let url = close + 2;
        let end = at(
            self.next_paren
                .get_or_insert_with(|| next_char_table(text, |c| c == ')')),
            url,
        )?;
        if end == url || self.next_space(url).is_some_and(|sp| sp < end) {
            return None;
        }
        Some((&text[i + 1..close], &text[url..end], end + 1 - i))
    }

    /// The length of a reST citation or footnote reference at byte `i`: `[1]_`, `[#]_`, `[Smith2020]_`.
    fn citation_ref(&mut self, i: usize) -> Option<usize> {
        let close = self.next_bracket(i + 1)?;
        let label_ok = close > i + 1 && self.next_space(i + 1).is_none_or(|sp| sp > close);
        (label_ok && self.text[close + 1..].starts_with('_')).then_some(close + 2 - i)
    }

    /// The byte of the `}` matching the `{` at byte `open` (Epytext `B{…}`).
    fn brace_close(&mut self, open: usize) -> Option<usize> {
        let text = self.text;
        let table = self.brace_close.get_or_insert_with(|| {
            let mut out = vec![NONE; text.len()];
            let mut open: Vec<usize> = Vec::new();
            for (p, b) in text.bytes().enumerate() {
                match b {
                    b'{' => open.push(p),
                    b'}' => {
                        if let Some(o) = open.pop() {
                            out[o] = p as u32;
                        }
                    }
                    _ => {}
                }
            }
            out
        });
        at(table, open)
    }
}

/// What an interpreted-text role shows: `title <target>` shows the title, `~a.b.c` shows `c`.
fn role_display(content: &str) -> String {
    let mut c = content.trim();
    if let Some(m) = REF_TARGET.captures(c) {
        let title = m.get(1).map_or("", |t| t.as_str()).trim();
        if !title.is_empty() {
            return title.to_string();
        }
        c = m.get(2).map_or(c, |t| t.as_str()).trim();
    }
    let c = c.trim_start_matches('!');
    match c.strip_prefix('~') {
        Some(path) => path.rsplit('.').next().unwrap_or(path).to_string(),
        None => c.trim_start_matches('.').to_string(),
    }
}

/// How an interpreted-text role shows: references to Python objects
/// (`:class:`, `:func:`, …) and literals as code; references to documents,
/// labels and glossary terms as text; `:pep:` / `:rfc:` as "PEP 8" / "RFC 2616".
fn role_span(role: &str, content: &str) -> Span {
    let name = role.rsplit(':').next().unwrap_or(role).to_ascii_lowercase();
    match name.as_str() {
        "ref" | "doc" | "term" | "numref" | "abbr" | "dfn" | "guilabel" | "menuselection"
        | "download" | "title-reference" | "title" | "t" => {
            Span::new(role_display(content), SpanStyle::Text)
        }
        "emphasis" => Span::new(content, SpanStyle::Emphasis),
        "strong" | "command" | "program" => Span::new(content, SpanStyle::Strong),
        "pep" | "rfc" => Span::new(
            format!(
                "{} {}",
                name.to_ascii_uppercase(),
                content.split('#').next().unwrap_or(content).trim()
            ),
            SpanStyle::Text,
        ),
        "class" | "func" | "function" | "meth" | "method" | "mod" | "module" | "attr" | "data"
        | "obj" | "exc" | "exception" => {
            let target = REF_TARGET
                .captures(content)
                .map(|m| m[2].trim().to_string())
                .unwrap_or_else(|| content.trim().to_string());
            let target = target.trim_start_matches(['~', '.']);
            Span {
                text: role_display(content),
                style: SpanStyle::Code,
                link: (!target.starts_with('!')
                    && target.len() <= 1024
                    && target.split('.').all(|part| {
                        !part.is_empty() && part.chars().all(|c| c == '_' || c.is_alphanumeric())
                    }))
                .then(|| format!("python:{target}")),
            }
        }
        _ => Span::new(role_display(content), SpanStyle::Code),
    }
}

/// Whether a link target may be followed from rendered documentation: web and
/// mail links only, so a docstring cannot link `file:`, `javascript:` or other
/// handlers (the editor opens links with the system's URL opener).
pub fn looks_like_url(s: &str) -> bool {
    let scheme =
        |p: &str| s.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p)) && s.len() > p.len();
    (scheme("http://") || scheme("https://") || scheme("mailto:"))
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Merges neighbouring spans of the same style and link.
fn merge(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans {
        if s.text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.style == s.style && last.link == s.link => {
                last.text.push_str(&s.text)
            }
            _ => out.push(s),
        }
    }
    out
}

/// Inline markup of one paragraph: ``` ``literal`` ```, `` `default role` ``,
/// `` :role:`target` ``, `*emphasis*`, `**strong**`, reST and Markdown links,
/// bare URLs, backslash escapes, and Epytext `B{…}` / `I{…}` / `C{…}` / `U{…}`.
pub fn inline(text: &str, epy: bool) -> Vec<Span> {
    inline_at(text, epy, 0)
}

fn inline_at(text: &str, epy: bool, depth: u8) -> Vec<Span> {
    // Markup inside markup, down to a limit (deeper text shows as written).
    let nested = |inner: &str| {
        if depth + 1 < MAX_INLINE_DEPTH {
            inline_at(inner, epy, depth + 1)
        } else {
            vec![Span::new(inner, SpanStyle::Text)]
        }
    };
    let mut scan = Scan::new(text);
    let mut out: Vec<Span> = Vec::new();
    let mut plain = String::new();
    let flush = |plain: &mut String, out: &mut Vec<Span>| {
        if !plain.is_empty() {
            out.push(Span::new(std::mem::take(plain), SpanStyle::Text));
        }
    };
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let Some(c) = rest.chars().next() else { break };
        if c == '\\' {
            if let Some(next) = rest[1..]
                .chars()
                .next()
                .filter(|n| "*`_|\\:[]{}".contains(*n))
            {
                plain.push(next);
                i += 1 + next.len_utf8();
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix("``") {
            if let Some(end) = after.find("``") {
                if end > 0 {
                    flush(&mut plain, &mut out);
                    out.push(Span::new(&after[..end], SpanStyle::Code));
                    i += end + 4;
                    continue;
                }
            }
        }
        if c == ':' && start_ok(text, i) {
            if let Some(m) = ROLE_AT.captures(rest) {
                flush(&mut plain, &mut out);
                out.push(role_span(&m[1], &m[2]));
                i += m[0].len();
                continue;
            }
        }
        if c == '`' {
            if let Some(end) = rest[1..].find('`').filter(|e| *e > 0) {
                let inner = &rest[1..1 + end];
                let mut len = end + 2;
                let reference = if rest[len..].starts_with("__") {
                    len += 2;
                    true
                } else if rest[len..].starts_with('_') {
                    len += 1;
                    true
                } else {
                    false
                };
                flush(&mut plain, &mut out);
                if reference {
                    match REF_TARGET.captures(inner) {
                        Some(m) => {
                            let target = m[2].trim().to_string();
                            let title = m.get(1).map_or("", |t| t.as_str()).trim();
                            let shown = if title.is_empty() {
                                target.clone()
                            } else {
                                title.to_string()
                            };
                            out.push(Span {
                                text: shown,
                                style: SpanStyle::Text,
                                link: Some(target).filter(|t| looks_like_url(t)),
                            });
                        }
                        None => out.push(Span::new(inner, SpanStyle::Text)),
                    }
                } else {
                    out.push(Span::new(role_display(inner), SpanStyle::Code));
                }
                i += len;
                continue;
            }
        }
        if rest.starts_with("**") && start_ok(text, i) {
            if let Some(end) = scan.markup_end(i, "**") {
                flush(&mut plain, &mut out);
                for mut s in nested(&rest[2..2 + end]) {
                    if s.style == SpanStyle::Text || s.style == SpanStyle::Emphasis {
                        s.style = SpanStyle::Strong;
                    }
                    out.push(s);
                }
                i += end + 4;
                continue;
            }
        }
        if c == '*' && !rest.starts_with("**") && start_ok(text, i) {
            if let Some(end) = scan
                .markup_end(i, "*")
                .filter(|e| !rest[1 + e..].starts_with("**"))
            {
                flush(&mut plain, &mut out);
                out.push(Span::new(&rest[1..1 + end], SpanStyle::Emphasis));
                i += end + 2;
                continue;
            }
        }
        if c == '[' {
            if let Some((label, target, len)) = scan.md_link(i) {
                // Shows its text; only web and mail targets link.
                flush(&mut plain, &mut out);
                let link = looks_like_url(target).then(|| target.to_string());
                out.extend(nested(label).into_iter().map(|s| Span {
                    link: link.clone(),
                    ..s
                }));
                i += len;
                continue;
            }
            if let Some(m) = MD_CODE_REF.captures(rest) {
                flush(&mut plain, &mut out);
                out.push(Span::new(role_display(&m[1]), SpanStyle::Code));
                i += m[0].len();
                continue;
            }
            if let Some(len) = scan.citation_ref(i) {
                // A citation or footnote reference: `[1]_` shows `[1]`.
                plain.push_str(&rest[..len - 1]);
                i += len;
                continue;
            }
        }
        if (rest.starts_with("http://") || rest.starts_with("https://")) && start_ok(text, i) {
            let end = rest
                .find(|ch: char| ch.is_whitespace() || "<>\"'`".contains(ch))
                .unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';', ':', ')', ']', '!', '?']);
            if url.len() > "https://".len() {
                flush(&mut plain, &mut out);
                out.push(Span {
                    text: url.to_string(),
                    style: SpanStyle::Text,
                    link: Some(url.to_string()),
                });
                i += url.len();
                continue;
            }
        }
        if epy && rest.len() > 2 && rest.as_bytes()[1] == b'{' && "BICLUMEX".contains(c) {
            if let Some(close) = scan.brace_close(i + 1) {
                let inner = &text[i + 2..close];
                flush(&mut plain, &mut out);
                match c {
                    'B' => out.extend(nested(inner).into_iter().map(|mut s| {
                        s.style = SpanStyle::Strong;
                        s
                    })),
                    'I' => out.extend(nested(inner).into_iter().map(|mut s| {
                        if s.style == SpanStyle::Text {
                            s.style = SpanStyle::Emphasis;
                        }
                        s
                    })),
                    'U' => {
                        // `U{text <url>}` or `U{url}`; only web and mail targets link.
                        let target = REF_TARGET
                            .captures(inner)
                            .map(|m| m[2].trim().to_string())
                            .unwrap_or_else(|| inner.trim().to_string());
                        out.push(Span {
                            text: role_display(inner),
                            style: SpanStyle::Text,
                            link: looks_like_url(&target).then_some(target),
                        });
                    }
                    'X' | 'E' => out.push(Span::new(inner, SpanStyle::Text)),
                    _ => out.push(Span::new(role_display(inner), SpanStyle::Code)),
                }
                i = close + 1;
                continue;
            }
        }
        // Runs of spaces show as one, as in rendered reST / Markdown.
        if !(c == ' ' && plain.ends_with(' ')) {
            plain.push(c);
        }
        i += c.len_utf8();
    }
    flush(&mut plain, &mut out);
    merge(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(blocks: &[DocBlock]) -> Vec<String> {
        blocks
            .iter()
            .map(|b| {
                let pad = "  ".repeat(b.depth as usize);
                match &b.kind {
                    BlockKind::Paragraph(s) => format!("{pad}P {}", show(s)),
                    BlockKind::Heading(h) => format!("{pad}H {h}"),
                    BlockKind::Field { name, ty, desc } => format!(
                        "{pad}F {name} ({}) {}",
                        ty.clone().unwrap_or_default(),
                        show(desc)
                    ),
                    BlockKind::Item { marker, spans } => format!("{pad}I {marker} {}", show(spans)),
                    BlockKind::Term(s) => format!("{pad}T {}", show(s)),
                    BlockKind::Image { target, alt } => format!("IMG {alt} {target}"),
                    BlockKind::Code { text, python } => format!(
                        "{pad}C{} {}",
                        if *python { "py" } else { "" },
                        text.replace('\n', "⏎")
                    ),
                }
            })
            .collect()
    }

    /// Spans as text with `*emph*`, `**strong**`, `` `code` `` and `[text](url)` markers.
    fn show(spans: &[Span]) -> String {
        spans
            .iter()
            .map(|s| {
                let t = match s.style {
                    SpanStyle::Text => s.text.clone(),
                    SpanStyle::Emphasis => format!("*{}*", s.text),
                    SpanStyle::Strong => format!("**{}**", s.text),
                    SpanStyle::Code => format!("`{}`", s.text),
                };
                match &s.link {
                    Some(url) => format!("[{t}]({url})"),
                    None => t,
                }
            })
            .collect()
    }

    #[test]
    fn google_sections_become_headings_and_fields() {
        let doc = "\"\"\"Fetch rows from a table.\n\n    Retrieves rows pertaining to the given keys.\n\n    Args:\n        table (Table): An open table.\n        keys: A sequence of strings\n            representing the keys.\n        *args: Extra.\n\n    Returns:\n        dict: A mapping of keys to rows.\n\n    Raises:\n        IOError: An error occurred.\n\n    Note:\n        Rows are cached.\n    \"\"\"";
        let blocks = render_literal(doc);
        assert_eq!(
            kinds(&blocks),
            vec![
                "P Fetch rows from a table.",
                "P Retrieves rows pertaining to the given keys.",
                "H Parameters",
                "  F table (Table) An open table.",
                "  F keys () A sequence of strings representing the keys.",
                "  F *args () Extra.",
                "H Returns",
                "  F  (dict) A mapping of keys to rows.",
                "H Raises",
                "  F IOError () An error occurred.",
                "H Note",
                "  P Rows are cached.",
            ]
        );
        // Source lines: the summary is on the opening line, `Args:` on line 4.
        let lines: Vec<usize> = blocks.iter().map(|b| b.line).collect();
        assert_eq!(lines, vec![0, 2, 4, 5, 6, 8, 10, 11, 13, 14, 16, 17]);
    }

    #[test]
    fn rest_fields_are_grouped_like_sphinx() {
        let doc = "\"\"\"\n    Compute *things*.\n\n    :param int x: the x value\n        continued here\n    :param y: the y\n    :type y: :class:`str`\n    :keyword verbose: chatty\n    :returns: the sum\n    :rtype: float\n    :raises ValueError: when bad\n    :ivar count: number\n    :meta private:\n    \"\"\"";
        assert_eq!(
            kinds(&render_literal(doc)),
            vec![
                "P Compute *things*.",
                "H Parameters",
                "  F x (int) the x value continued here",
                "  F y (str) the y",
                "H Keyword Arguments",
                "  F verbose () chatty",
                "H Returns",
                "  F  (float) the sum",
                "H Raises",
                "  F ValueError () when bad",
                "H Variables",
                "  F count () number",
            ]
        );
    }

    #[test]
    fn epytext_fields_and_markup() {
        let doc = "'''Summary with B{bold} and C{code}.\n\n@param x: the I{x}\n@type x: int\n@return: result\n@rtype: bool\n'''";
        assert_eq!(
            kinds(&render_literal(doc)),
            vec![
                "P Summary with **bold** and `code`.",
                "H Parameters",
                "  F x (int) the *x*",
                "H Returns",
                "  F  (bool) result"
            ]
        );
    }

    #[test]
    fn numpy_sections() {
        let doc = "\"\"\"Add numbers.\n\nParameters\n----------\na, b : int\n    The numbers.\nscale : float, optional\n\nReturns\n-------\nint\n    The sum.\n\nSee Also\n--------\nsubtract : Take away.\n\nExamples\n--------\n>>> add(1, 2)\n3\n\"\"\"";
        assert_eq!(
            kinds(&render_literal(doc)),
            vec![
                "P Add numbers.",
                "H Parameters",
                "  F a, b (int) The numbers.",
                "  F scale (float, optional) ",
                "H Returns",
                "  F  (int) The sum.",
                "H See Also",
                "  F subtract () Take away.",
                "H Examples",
                "  Cpy >>> add(1, 2)⏎3",
            ]
        );
    }

    #[test]
    fn plain_text_lists_code_and_titles() {
        let doc = "\"\"\"Module title\n============\n\nSome **intro** text\nwrapped over lines.\n\n- first item\n  continued\n- second item\n\n1. one\n2. two\n\nExample::\n\n    x = 1\n    y = 2\n\n```python\nprint('md')\n```\n\n.. note:: Be careful\n   with this.\n\n.. versionadded:: 3.2\n\n.. autosummary::\n   hidden\n\"\"\"";
        assert_eq!(
            kinds(&render_literal(doc)),
            vec![
                "H Module title",
                "P Some **intro** text wrapped over lines.",
                "I • first item continued",
                "I • second item",
                "I 1. one",
                "I 2. two",
                "P Example:",
                "C x = 1⏎y = 2",
                "Cpy print('md')",
                "H Note",
                "  P Be careful with this.",
                "P *New in version 3.2*.",
                "I • `hidden`",
            ]
        );
    }

    #[test]
    fn indented_blocks_nest_and_doctests_are_code() {
        let blocks = render_text(
            "Summary.\n\nterm\n    The definition.\n\n>>> f(1)\n2\n\nLiteral ::\n\n    raw text\n",
        );
        assert_eq!(
            kinds(&blocks),
            vec![
                "P Summary.",
                "T term",
                "  P The definition.",
                "Cpy >>> f(1)⏎2",
                "P Literal",
                "C raw text"
            ]
        );
    }

    #[test]
    fn inline_markup() {
        assert_eq!(
            show(&inline(
                "Use ``x`` or :func:`~pkg.mod.run` with *care* and **force**.",
                false
            )),
            "Use `x` or [`run`](python:pkg.mod.run) with *care* and **force**."
        );
        assert_eq!(
            show(&inline(
                "See `the docs <https://example.org>`_ and :class:`Thing <pkg.Thing>`.",
                false
            )),
            "See [the docs](https://example.org) and [`Thing`](python:pkg.Thing)."
        );
        assert_eq!(
            show(&inline(
                "Visit https://python.org. Or [here](http://x.y/z).",
                false
            )),
            "Visit [https://python.org](https://python.org). Or [here](http://x.y/z)."
        );
        // Not markup: stars inside words, unmatched stars, `*args`, escaped characters.
        assert_eq!(
            show(&inline(
                "2*3*4 and *args, **kwargs; a \\*literal\\* star",
                false
            )),
            "2*3*4 and *args, **kwargs; a *literal* star"
        );
        assert_eq!(
            show(&inline("A `default role` value", false)),
            "A `default role` value"
        );
        // A role is not a field: `:class:` followed by a backquote.
        assert!(field_line(":class:`Foo` is great", false).is_none());
        assert!(field_line(":param x: y", false).is_some());
        assert!(field_line(":custom field: text", false).is_some());
    }

    #[test]
    fn literal_decoding_and_line_numbers() {
        // Escapes are decoded in normal strings, kept in raw ones; a `\n` escape splits a line.
        let blocks = render_literal(
            "\"\"\"Tab\\tsep \\\"q\\\" path C:\\\\dir\n\n    Second.\\n\\n    Third.\n    \"\"\"",
        );
        // (The tab shows as one space: runs of spaces collapse in running text.)
        assert_eq!(
            kinds(&blocks),
            vec!["P Tab sep \"q\" path C:\\dir", "P Second.", "P Third."]
        );
        assert_eq!(
            blocks.iter().map(|b| b.line).collect::<Vec<_>>(),
            vec![0, 2, 2]
        );
        let raw = render_literal("r'''Regex \\d+ here.'''");
        assert_eq!(kinds(&raw), vec!["P Regex \\d+ here."]);
        // Unterminated (being typed) and single-quoted docstrings.
        assert_eq!(
            kinds(&render_literal("\"\"\"Still typing")),
            vec!["P Still typing"]
        );
        assert_eq!(kinds(&render_literal("'One line.'")), vec!["P One line."]);
        assert!(render_literal("\"\"\"   \"\"\"").is_empty());
    }

    #[test]
    fn non_ascii_whitespace_does_not_panic() {
        let doc = "\"\"\"Summary.\n\u{3000}\u{3000}全角の字下げ\n  two spaces\n\u{a0}nbsp\n\"\"\"";
        let blocks = render_literal(doc);
        assert!(!blocks.is_empty());
        assert!(!docstrings::clean("x\n  a\n\u{3000}b").is_empty());
    }

    #[test]
    fn tables_quotes_and_alerts() {
        let doc = "Summary.\n\n====  ===========\nbeta  Window shape\n====  ===========\n0     Rectangular\n\n5     Hamming\n====  ===========\n\n+---+---+\n| a | b |\n+---+---+\n\n> [!TIP]\n> Use **this**.\n> More text.\n\n> Just a quote.\n\n<Tip warning={true}>\n\nCareful.\n\n</Tip>\n\n>>> doctest()\n";
        assert_eq!(
            kinds(&render_text(doc)),
            vec![
                "P Summary.",
                "C ====  ===========⏎beta  Window shape⏎====  ===========⏎0     Rectangular⏎⏎5     Hamming⏎====  ===========",
                "C +---+---+⏎| a | b |⏎+---+---+",
                "H Tip",
                "  P Use **this**. More text.",
                "  P Just a quote.",
                "H Warning",
                "  P Careful.",
                "Cpy >>> doctest()",
            ]
        );
        // A table right below a paragraph line ends the paragraph.
        assert_eq!(
            kinds(&render_text(
                "Values:\n=====  =====\na      b\n=====  =====\n"
            )),
            vec!["P Values:", "C =====  =====⏎a      b⏎=====  ====="]
        );
    }

    #[test]
    fn markup_in_types_and_doc_builder_references() {
        let doc = "Do.\n\nArgs:\n    filter (`str` or `Iterable`, *optional*):\n        Tags; see [`list_models`] and [`~hub.HfApi`].\n\nReturns:\n    `Iterable[SpaceInfo]`: the [*spaces*](https://hf.co/spaces).\n";
        assert_eq!(
            kinds(&render_text(doc)),
            vec![
                "P Do.",
                "H Parameters",
                "  F filter (str or Iterable, optional) Tags; see `list_models` and `HfApi`.",
                "H Returns",
                "  F  (Iterable[SpaceInfo]) the [*spaces*](https://hf.co/spaces)."
            ]
        );
        let numpy = "Do.\n\nParameters\n----------\nx : :class:`numpy.ndarray`\n    Input.\n\nReturns\n-------\n:class:`float`\n    Result.\n";
        assert_eq!(
            kinds(&render_text(numpy)),
            vec![
                "P Do.",
                "H Parameters",
                "  F x (numpy.ndarray) Input.",
                "H Returns",
                "  F  (float) Result."
            ]
        );
    }

    #[test]
    fn roles_escapes_and_spacing() {
        assert_eq!(show(&inline("See :ref:`the guide <guide>`, :pep:`484`, :rfc:`2616#section-1` and :py:meth:`~a.B.c`.", false)), "See the guide, PEP 484, RFC 2616 and [`c`](python:a.B.c).");
        assert_eq!(
            show(&inline("One.  Two   three ``a  b``.", false)),
            "One. Two three `a  b`."
        );
        let doc = "r'''Send.\n\n:param \\*\\*kwargs: Extra.\n:type \\*\\*kwargs: dict\n'''";
        assert_eq!(
            kinds(&render_literal(doc)),
            vec!["P Send.", "H Parameters", "  F **kwargs (dict) Extra."]
        );
    }

    #[test]
    fn definition_list_terms() {
        let blocks = render_text("Summary.\n\nOptions:\n  width (default: 70)\n    the maximum width\n  tabsize\n    spaces per tab\n");
        assert_eq!(
            kinds(&blocks),
            vec![
                "P Summary.",
                "T Options:",
                "  T width (default: 70)",
                "    P the maximum width",
                "  T tabsize",
                "    P spaces per tab"
            ]
        );
    }

    #[test]
    fn citations_and_their_references() {
        let doc = "Do it, see [1]_ and [Smith2020]_.\n\nReferences\n----------\n.. [1] C. W. Clenshaw, *Chebyshev series*,\n       1962.\n.. [Smith2020] Another.\n";
        assert_eq!(
            kinds(&render_text(doc)),
            vec![
                "P Do it, see [1] and [Smith2020].",
                "H References",
                "  I [1] C. W. Clenshaw, *Chebyshev series*, 1962.",
                "  I [Smith2020] Another."
            ]
        );
    }

    /// The searches [`Scan`] answers from lookups, as they were written before:
    /// each one runs to the end of the text.
    fn naive_markup_end(rest: &str, open: usize, marker: &str) -> Option<usize> {
        let content = &rest[open..];
        if content.starts_with(char::is_whitespace) {
            return None;
        }
        let mut from = content.chars().next().map_or(0, char::len_utf8);
        while let Some(p) = content[from..].find(marker).map(|p| p + from) {
            let before_ok = content[..p]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_whitespace() && c != '\\');
            let after = &content[p + marker.len()..];
            let after_ok = after
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || "'\")]}>-/:.,;!?\\’”".contains(c));
            if before_ok && after_ok {
                return Some(p);
            }
            from = p + marker.len();
        }
        None
    }

    fn naive_brace_close(s: &str) -> Option<usize> {
        let mut depth = 0usize;
        for (i, c) in s.char_indices() {
            match c {
                '{' => depth += 1,
                '}' if depth == 0 => return Some(i),
                '}' => depth -= 1,
                _ => {}
            }
        }
        None
    }

    #[test]
    fn linear_lookups_agree_with_the_plain_searches() {
        let md_link = Regex::new(r"^\[([^\]]+)\]\(([^)\s]+)\)").unwrap();
        let citation = Regex::new(r"^\[[^\]\s]+\]_").unwrap();
        let alphabet = [
            '*', '*', 'a', ' ', '\\', '.', '[', ']', '(', ')', '_', '{', '}', 'é', '\n', ':',
        ];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..4000 {
            let len = (next() % 28) as usize;
            let text: String = (0..len)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect();
            let mut scan = Scan::new(&text);
            for (i, _) in text.char_indices() {
                let rest = &text[i..];
                for marker in ["*", "**"] {
                    if rest.starts_with(marker) {
                        assert_eq!(
                            scan.markup_end(i, marker),
                            naive_markup_end(rest, marker.len(), marker),
                            "{marker} at {i} of {text:?}"
                        );
                    }
                }
                if rest.starts_with('[') {
                    let expected = md_link.captures(rest).map(|m| {
                        (
                            m.get(1).unwrap().as_str(),
                            m.get(2).unwrap().as_str(),
                            m[0].len(),
                        )
                    });
                    assert_eq!(scan.md_link(i), expected, "link at {i} of {text:?}");
                    assert_eq!(
                        scan.citation_ref(i),
                        citation.find(rest).map(|m| m.len()),
                        "citation at {i} of {text:?}"
                    );
                }
                if let Some(after) = rest.strip_prefix('{') {
                    assert_eq!(
                        scan.brace_close(i),
                        naive_brace_close(after).map(|c| i + 1 + c),
                        "brace at {i} of {text:?}"
                    );
                }
            }
        }
    }

    /// Runs `f` on a thread with a 1 MB stack: deep recursion would overflow it.
    fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(f)
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn deeply_nested_docstrings_render_flat_below_a_limit() {
        let staircase: String = (0..300)
            .map(|k| format!("{}step {k}\n", " ".repeat(k)))
            .collect();
        let admonitions: String = (0..300)
            .map(|k| format!("{}.. note::\n\n", "   ".repeat(k)))
            .collect::<String>()
            + &"   ".repeat(300)
            + "deep\n";
        let docs = [
            // One line of 300 quote markers, and of 30 000 (a 60 KB line).
            format!("Summary.\n\n{}x\n", "> ".repeat(300)),
            format!("Summary.\n\n{}x\n", "> ".repeat(30_000)),
            format!("Summary.\n\n{}x\n", "<Tip>\n\n".repeat(300)),
            format!("Summary.\n\n{staircase}"),
            format!("Summary.\n\n{admonitions}"),
        ];
        for doc in docs {
            let blocks = on_small_stack(move || render_text(&doc));
            assert!(
                blocks.iter().all(|b| b.depth <= MAX_DEPTH),
                "nesting is capped"
            );
            // The innermost text is still there, as preformatted text at the deepest level.
            let last = blocks.last().unwrap();
            assert!(
                matches!(&last.kind, BlockKind::Code { text, python: false } if text.ends_with('x') || text.ends_with("step 299") || text.ends_with("deep")),
                "{:?}",
                last.kind
            );
            assert_eq!(last.depth, MAX_DEPTH);
        }
        // Inline markup nested 30 000 deep (Epytext `B{…}`, `**`, link texts) stays on the stack too.
        let spans = on_small_stack(|| {
            inline(
                &format!("{}x{}", "B{".repeat(30_000), "}".repeat(30_000)),
                true,
            )
        });
        assert!(
            plain_text(&spans).contains('x') && spans.iter().all(|s| s.style == SpanStyle::Strong)
        );
        let spans = on_small_stack(|| {
            inline(
                &format!(
                    "{}x{}",
                    "[".repeat(30_000),
                    "](https://e.org)".repeat(30_000)
                ),
                false,
            )
        });
        assert!(plain_text(&spans).contains('x'));
    }

    /// Asserts that `f` returns within `limit` (the quadratic scans this guards
    /// against took tens of seconds on these inputs).
    fn quickly<T>(what: &str, limit_ms: u64, f: impl FnOnce() -> T) -> T {
        let t = std::time::Instant::now();
        let out = f();
        assert!(
            t.elapsed() < std::time::Duration::from_millis(limit_ms),
            "{what} took {:?}",
            t.elapsed()
        );
        out
    }

    #[test]
    fn crafted_paragraphs_render_in_linear_time() {
        let cases = [
            ("unclosed emphasis", "*a ".repeat(40_000), false),
            ("unclosed strong", "**a ".repeat(30_000), false),
            ("unclosed links", "[a ".repeat(40_000), false),
            ("unclosed citations", "[a".repeat(60_000), false),
            (
                "link texts sharing one bracket",
                "[a ".repeat(40_000) + "](x y",
                false,
            ),
            ("colons like roles", ":a-".repeat(40_000), false),
            ("unclosed Epytext", "B{a ".repeat(30_000), true),
        ];
        for (what, text, epy) in cases {
            let spans = quickly(what, 2000, || inline(&text, epy));
            // Nothing closes: the paragraph shows as written (runs of spaces collapsed).
            assert_eq!(plain_text(&spans), text, "{what}");
        }
        let doc = format!("Summary.\n\n{}", "*a ".repeat(40_000));
        assert_eq!(
            quickly("a docstring paragraph", 2000, || render_text(&doc)).len(),
            2
        );
    }

    #[test]
    fn crafted_docstrings_render_in_linear_time() {
        // Thousands of simple tables without a bottom border, leading blank
        // lines, a Google "Returns" section of thousands of entries.
        let tables = format!("Summary.\n\n{}", "==  ==\nx\n\n".repeat(20_000));
        assert_eq!(
            quickly("tables", 3000, || render_text(&tables)).len(),
            20_001
        );
        let blank = format!("\"\"\"{}Summary.\"\"\"", "\n".repeat(100_000));
        let blocks = quickly("leading blank lines", 3000, || render_literal(&blank));
        assert_eq!(kinds(&blocks), vec!["P Summary."]);
        assert_eq!(blocks[0].line, 100_000);
        let returns = format!(
            "Do.\n\nReturns:\n    int: the value\n{}",
            "    more\n".repeat(30_000)
        );
        let blocks = quickly("returns", 3000, || render_text(&returns));
        assert!(
            matches!(&blocks.last().unwrap().kind, BlockKind::Field { desc, .. } if plain_text(desc).len() == "the value".len() + 30_000 * " more".len())
        );
    }

    #[test]
    fn directives_without_their_own_rendering_show_their_content() {
        let doc = "Summary.\n\n.. autosummary::\n   :toctree: generated/\n\n   firwin\n   ~scipy.signal.remez\n\n.. plot::\n\n   import matplotlib.pyplot as plt\n   plt.plot([1, 2])\n\n.. testoutput::\n\n   3\n\n.. container:: topic\n\n   Container prose.\n\n.. legacy:: 2.0\n\n   Legacy prose.\n\n.. list-table:: Sizes\n\n   * - small\n     - 1\n\n.. glossary::\n\n   term\n      Its meaning.\n\n.. attribute:: name\n   :type: str\n\n   The name.\n\n.. py:function:: f(x)\n\n   Does f.\n\n.. autoclass:: pkg.Thing\n\n.. index:: single: hidden entry\n\n.. deprecated-removed:: 3.12 3.14\n   Use g.\n";
        assert_eq!(
            kinds(&render_text(doc)),
            vec![
                "P Summary.",
                "I • `firwin`",
                "I • `remez`",
                "Cpy import matplotlib.pyplot as plt⏎plt.plot([1, 2])",
                "C 3",
                "  P Container prose.",
                "  P Legacy prose.",
                "H Sizes",
                "  I • - small",
                "    I • 1",
                "  T term",
                "    P Its meaning.",
                "T `name`",
                "  P The name.",
                "T `f(x)`",
                "  P Does f.",
                "T `pkg.Thing`",
                "P *Deprecated since version 3.12, will be removed in version 3.14*: Use g.",
            ]
        );
    }

    #[test]
    fn links_only_to_web_and_mail_targets() {
        assert_eq!(
            show(&inline(
                "[docs](https://example.org/guide), [mail](mailto:a@b.org) and [up](HTTPS://E.ORG)",
                false
            )),
            "[docs](https://example.org/guide), [mail](mailto:a@b.org) and [up](HTTPS://E.ORG)"
        );
        // Other targets show their text without a link.
        assert_eq!(
            show(&inline(
                "[passwd](file:///etc/passwd), [x](javascript:alert%281%29), [rel](guide.md)",
                false
            )),
            "passwd, x, rel"
        );
        assert_eq!(
            show(&inline("See `secrets <file:///etc/shadow>`_.", false)),
            "See secrets."
        );
        assert_eq!(
            show(&inline(
                "U{Python <https://python.org>} and U{file:///etc/passwd}",
                true
            )),
            "[Python](https://python.org) and file:///etc/passwd"
        );
        assert!(looks_like_url("https://example.org/a?b=c") && looks_like_url("mailto:x@y.org"));
        assert!(
            !looks_like_url("https://")
                && !looks_like_url("ftp://example.org")
                && !looks_like_url("https://a b")
                && !looks_like_url("smb://host/share")
        );
    }

    #[test]
    fn google_returns_without_type_is_text() {
        let doc = "Do it.\n\nReturns:\n    The value, or None when\n    nothing was found.\n";
        assert_eq!(
            kinds(&render_text(doc)),
            vec![
                "P Do it.",
                "H Returns",
                "  P The value, or None when nothing was found."
            ]
        );
    }
}

#[cfg(test)]
mod documentation_resource_tests {
    use super::*;
    #[test]
    fn roles_preserve_display_and_exact_semantic_targets_without_external_handlers() {
        let spans = inline(
            ":class:`Thing <pkg.Thing>` and :py:meth:`~pkg.Thing.call` and :class:`!Hidden`",
            false,
        );
        let links: Vec<_> = spans
            .iter()
            .filter_map(|span| span.link.as_deref())
            .collect();
        assert_eq!(links, vec!["python:pkg.Thing", "python:pkg.Thing.call"]);
        assert!(plain_text(&spans).contains("Thing and call and Hidden"));
        assert!(inline(":class:`bad <javascript:alert(1)>`", false)
            .iter()
            .all(|span| span.link.is_none()));
    }
    #[test]
    fn rst_markdown_images_keep_source_line_alt_and_neighboring_admonition_code() {
        let blocks=render_text("Summary.\n\n.. image:: images/π.png\n   :alt: Diagram π\n\n.. note:: Read carefully.\n\n```python\nvalue = 1\n```\n\n![Other](other.png)\n");
        assert!(blocks.iter().any(|block|block.line==2&&matches!(&block.kind,BlockKind::Image{target,alt}if target=="images/π.png"&&alt=="Diagram π")));
        assert!(blocks.iter().any(|block|matches!(&block.kind,BlockKind::Image{target,alt}if target=="other.png"&&alt=="Other")));
        assert!(blocks
            .iter()
            .any(|block| matches!(&block.kind,BlockKind::Heading(title)if title=="Note")));
        assert!(blocks.iter().any(
            |block| matches!(&block.kind,BlockKind::Code{python:true,text}if text=="value = 1")
        ));
    }
}
