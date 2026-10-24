//! RustPy IDE — Python code formatter (roadmap §2.9 "Reformat Code").
//!
//! Two engines:
//!
//! * the `black` module: a port of Black 26's stable style. Compatibility with
//!   [`FormatOptions::black()`] is measured against the pinned corpus; explicit
//!   safety refusals, unsupported modes and safe divergences remain reported.
//!   Line length, `QuoteStyle::Preserve` and `magic_trailing_comma` map to
//!   Black's `-l`, `-S` and `-C`; other indent widths are supported too.
//!   Used whenever the options are Black-compatible (not single quotes,
//!   default blank-line counts).
//! * The token-based formatter below (tokenize + parse → logical items →
//!   emit with normalized whitespace, quotes and blank lines, right-hand
//!   splits), used for the remaining option combinations.
//!
//! Either way the result must parse and have the same semantic fingerprint
//! as the input ([`semantic_fingerprint`], which also covers `# type:`
//! comments the way Black's AST check does); otherwise formatting fails with
//! [`FormatError::Unsafe`] and the code is left alone. Like Black's safe mode,
//! the Black engine also refuses output that another formatting pass would
//! change, so the inputs Black refuses are refused here as well.
//!
//! Compatibility is measured with the differential harness
//! `examples/black_diff.rs` (see its docs): it formats a fixed sample of
//! files with both Black and this crate and reports identical, idempotent
//! and AST-equal counts plus the most frequent kinds of first differences:
//!
//! ```text
//! cargo run -p rustpy-format --example black_diff -- --black <venv>/bin/black
//! ```

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::decode_string_literal;
use rustpy_syntax::lexer::Token;
use rustpy_syntax::rowan::NodeOrToken;
use rustpy_syntax::SyntaxKind::{self, *};
use rustpy_syntax::SyntaxNode;

pub mod candidate;
pub mod fragments;
pub mod imports;
pub mod python_style;
pub mod style_policy;

mod black;
mod equiv;
mod logical_range;
pub use logical_range::{format_logical_ranges, LogicalRangeEdits};

/// Preferred string quote style.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum QuoteStyle {
    Double,
    Single,
    Preserve,
}

/// Plain-text markers in actual Python comments; never matched inside strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatterTags {
    pub off: String,
    pub on: String,
}
impl FormatterTags {
    fn is_off(&self, comment: &str) -> bool {
        !self.off.is_empty() && comment.contains(&self.off)
    }
    fn is_on(&self, comment: &str) -> bool {
        !self.on.is_empty() && comment.contains(&self.on)
    }
    pub fn has_start(&self, source: &str) -> bool {
        let (tokens, _) = rustpy_syntax::tokenize(source);
        tokens
            .iter()
            .any(|t| t.kind == COMMENT && self.is_off(&source[t.range]))
    }
}

/// Tab indentation distinguishes syntactic block columns from alignment.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TabIndent {
    pub width: usize,
    pub smart: bool,
}
impl TabIndent {
    pub fn render(self, columns: usize, block_columns: usize) -> String {
        let width = self.width.clamp(1, 256);
        let tab_columns = if self.smart {
            block_columns.min(columns)
        } else {
            columns
        };
        let mut out = "\t".repeat(tab_columns / width);
        out.push_str(&" ".repeat(columns - (tab_columns / width) * width));
        out
    }
}

/// Explicit Python hanging-indent policy; absent keeps existing formatter style.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ContinuationIndent {
    pub width: usize,
    pub parameters: bool,
    pub arguments: bool,
    pub collections: bool,
}
impl ContinuationIndent {
    pub fn step(self, parameters: bool, arguments: bool, normal: usize) -> usize {
        let enabled = if parameters {
            self.parameters
        } else if arguments {
            self.arguments
        } else {
            self.collections
        };
        if enabled {
            self.width.clamp(1, 256)
        } else {
            normal
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    pub indent: usize,
    pub continuation_indent: Option<ContinuationIndent>,
    pub tabs: Option<TabIndent>,
    pub formatter_tags: Option<FormatterTags>,
    pub max_line_length: usize,
    pub quote: QuoteStyle,
    /// A trailing comma inside brackets forces one element per line.
    pub magic_trailing_comma: bool,
    pub blank_lines_top_level: usize,
    pub blank_lines_in_class: usize,
    pub python: rustpy_settings::python_style::PythonStyle,
    /// Changed source spans as `(zero_based_start_line, line_count)` in the
    /// original input, before formatting changes any line counts. `None`
    /// formats all lines; an empty selection formats none.
    pub changed_lines: Option<Vec<(usize, usize)>>,
    pub keep_line_breaks: bool,
    pub keep_simple_blocks: bool,
    pub keep_first_column_comments: bool,
}

impl FormatOptions {
    /// Black defaults: 88 columns, double quotes, magic trailing commas.
    pub fn black() -> Self {
        Self {
            indent: 4,
            continuation_indent: None,
            tabs: None,
            formatter_tags: None,
            max_line_length: 88,
            quote: QuoteStyle::Double,
            magic_trailing_comma: true,
            blank_lines_top_level: 2,
            blank_lines_in_class: 1,
            python: Default::default(),
            changed_lines: None,
            keep_line_breaks: false,
            keep_simple_blocks: false,
            keep_first_column_comments: false,
        }
    }

    /// PEP 8-style IDE defaults: 120 columns, quotes preserved.
    pub fn pep8() -> Self {
        Self {
            indent: 4,
            continuation_indent: None,
            tabs: None,
            formatter_tags: None,
            max_line_length: 120,
            quote: QuoteStyle::Preserve,
            magic_trailing_comma: true,
            blank_lines_top_level: 2,
            blank_lines_in_class: 1,
            python: Default::default(),
            changed_lines: None,
            keep_line_breaks: false,
            keep_simple_blocks: false,
            keep_first_column_comments: false,
        }
    }
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self::black()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    #[error("cannot format: {0}")]
    SyntaxError(String),
    /// The formatted code would not be equivalent to the input (a formatter
    /// bug); the source is left untouched instead.
    #[error("cannot format safely: {0}")]
    Unsafe(String),
}

/// Formats a whole module. Files with syntax errors are left untouched (`Err`).
///
/// Black-compatible options (any indent width, double or preserved quotes,
/// Black's blank-line counts) use the port of Black's algorithm in the
/// `black` module (including `# fmt: off` / `# fmt: on` / `# fmt: skip`);
/// other combinations (single quotes, custom blank lines) use the simpler
/// token-based formatter below. Like Black, the output uses the line
/// separator that ends the first line of `src`.
pub fn format_source(src: &str, opts: &FormatOptions) -> Result<String, FormatError> {
    let newline = newline_style(src);
    let normalized = normalize_newlines(src);
    let out = format_stable_lf(&normalized, opts)?;
    Ok(if newline == "\n" {
        out
    } else {
        out.replace('\n', newline)
    })
}

/// Public formatting publishes only a fixed point. Custom CST options use a
/// bounded convergence pass; oscillating policies leave the source untouched.
fn format_stable_lf(source: &str, options: &FormatOptions) -> Result<String, FormatError> {
    // Changed-line coordinates belong to the original document. Never move the
    // selection by feeding an intermediate document back through those ranges.
    if let Some(lines) = &options.changed_lines {
        let mut whole = options.clone();
        whole.changed_lines = None;
        let formatted = format_stable_lf(source, &whole)?;
        let selected = style_policy::changed_lines(source, &formatted, lines)?;
        let second = format_stable_lf(&selected, &whole)?;
        if style_policy::changed_lines(&selected, &second, lines)? != selected {
            return Err(FormatError::Unsafe(
                "changed-line formatting did not reach a stable selection".into(),
            ));
        }
        check_equivalent(&rustpy_syntax::parse(source).syntax_node(), &selected)?;
        return Ok(selected);
    }
    let mut current = source.to_owned();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..4 {
        let output = format_lf(&current, options)?;
        if !comments_preserved(&current, &output) {
            return Err(FormatError::Unsafe(
                "formatter policy would lose or duplicate a comment".into(),
            ));
        }
        if output == current {
            return Ok(output);
        }
        // The Black path already performs its own assert_stable. Post-policies
        // need the complete pipeline check because they run after that engine.
        let black_only = uses_black_engine(options)
            && !options.keep_line_breaks
            && !options.keep_simple_blocks
            && !options.keep_first_column_comments
            && options.changed_lines.is_none()
            && !options
                .formatter_tags
                .as_ref()
                .is_some_and(|tags| tags.has_start(&current));
        if black_only {
            return Ok(output);
        }
        if !seen.insert(current) {
            return Err(FormatError::Unsafe(
                "formatter options cycle instead of reaching a fixed point".into(),
            ));
        }
        current = output;
    }
    Err(FormatError::Unsafe(
        "formatter options did not stabilize within four passes".into(),
    ))
}

/// Black's choice of output line separator (`decode_bytes`): the one ending
/// the first line (a lone `\r` inside that line means old Mac endings).
fn newline_style(src: &str) -> &'static str {
    let first = match src.find('\n') {
        Some(i) => &src[..=i],
        None => src,
    };
    if let Some(body) = first.strip_suffix("\r\n") {
        if body.contains('\r') {
            "\r"
        } else {
            "\r\n"
        }
    } else if let Some(body) = first.strip_suffix('\n') {
        if body.contains('\r') {
            "\r"
        } else {
            "\n"
        }
    } else if first.contains('\r') {
        "\r"
    } else {
        "\n"
    }
}

/// [`format_source`] on text with `\n` line endings, producing `\n` endings.
fn format_lf(src: &str, opts: &FormatOptions) -> Result<String, FormatError> {
    // A UTF-8 byte-order mark stays in front of the formatted code, as with
    // Black (which reads and writes such files as `utf-8-sig`).
    if let Some(code) = src.strip_prefix('\u{feff}') {
        return Ok(format!("\u{feff}{}", format_lf(code, opts)?));
    }
    let parse = rustpy_syntax::parse(src);
    if let Some(e) = parse.errors().first() {
        let start = usize::from(e.range.start()).min(src.len());
        let line = src[..start].matches('\n').count() + 1;
        return Err(FormatError::SyntaxError(format!(
            "line {line}: {}",
            e.message
        )));
    }
    let (tokens, _) = rustpy_syntax::tokenize(src);
    let has_custom_tags = opts.formatter_tags.as_ref().is_some_and(|tags| {
        tokens
            .iter()
            .any(|t| t.kind == COMMENT && tags.is_off(&src[t.range]))
    });
    let black_mode = (uses_black_engine(opts) && !has_custom_tags).then(|| black::Mode {
        line_length: opts.max_line_length,
        string_normalization: opts.quote == QuoteStyle::Double,
        magic_trailing_comma: opts.magic_trailing_comma,
        indent: opts.indent.max(1),
    });
    let out = if let Some(mode) = &black_mode {
        black::format_str(src, mode)
            .map_err(|e| FormatError::SyntaxError(format!("line {}: {}", e.line, e.message)))?
    } else {
        let root = parse.syntax_node();
        let items = Scanner::new(src, &tokens, &root, opts).scan();
        Emitter::new(opts).emit(&items)
    };
    if out != src {
        check_equivalent(&parse.syntax_node(), &out)?;
        // Black's `assert_stable`: one more pass must leave the result alone.
        if black_mode.is_some_and(|mode| !black::is_stable(&out, &mode)) {
            return Err(FormatError::Unsafe(
                "formatting the result again would change it (Black refuses such files too)".into(),
            ));
        }
    }
    let out = python_style::finish(src, out, opts)?;
    let out = style_policy::keep(src, out, opts)?;
    if let Some(lines) = &opts.changed_lines {
        style_policy::changed_lines(src, &out, lines)
    } else {
        Ok(out)
    }
}

/// Formats the file and returns the edits that turn `src` into the formatted
/// text (empty when nothing changes). The file is formatted as a whole, so
/// blank-line rules stay consistent; when `range` does not cover the whole
/// file, only the changed lines that touch it are returned (Reformat Code on a
/// selection). Edits use the original source's byte offsets and line endings.
/// Selections that cannot be reformatted without changing unselected lines or
/// the program are refused.
pub fn format_range(
    src: &str,
    range: TextRange,
    opts: &FormatOptions,
) -> Result<Vec<(TextRange, String)>, FormatError> {
    let normalized = normalize_newlines(src);
    // A CRLF pair becomes one LF. Offsets inside the pair map to the start
    // of that LF, so a selection ending there never includes the next line.
    let crlf: Vec<(usize, usize)> = src
        .match_indices("\r\n")
        .enumerate()
        .map(|(removed, (original, _))| (original, original - removed))
        .collect();
    let to_normalized = |off: TextSize| {
        let off = usize::from(off).min(src.len());
        TextSize::new((off - crlf.partition_point(|&(original, _)| original < off)) as u32)
    };
    let normalized_range = TextRange::new(to_normalized(range.start()), to_normalized(range.end()));
    let mut edits = format_range_lf(&normalized, normalized_range, opts)?;
    if normalized != src {
        let original_offset = |off: TextSize| {
            let off = usize::from(off);
            TextSize::new((off + crlf.partition_point(|&(_, normalized)| normalized < off)) as u32)
        };
        let newline = newline_style(src);
        for (range, text) in &mut edits {
            *range = TextRange::new(original_offset(range.start()), original_offset(range.end()));
            if newline != "\n" {
                *text = text.replace('\n', newline);
            }
        }
    }
    if !edits.is_empty() {
        // Whole-file formatting was checked by format_lf, but a subset of its
        // edits can change a list's contents or break a block's indentation.
        // Check the actual source the caller will obtain, after EOL mapping.
        let applied = rustpy_syntax::printer::apply_text_edits(src, &edits)
            .map_err(|error| FormatError::Unsafe(error.to_string()))?;
        let applied = normalize_newlines(&applied);
        let before =
            rustpy_syntax::parse(normalized.strip_prefix('\u{feff}').unwrap_or(&normalized));
        check_equivalent(
            &before.syntax_node(),
            applied.strip_prefix('\u{feff}').unwrap_or(&applied),
        )?;
    }
    Ok(edits)
}

fn format_range_lf(
    src: &str,
    range: TextRange,
    opts: &FormatOptions,
) -> Result<Vec<(TextRange, String)>, FormatError> {
    let formatted = format_stable_lf(src, opts)?;
    if formatted == src {
        return Ok(Vec::new());
    }
    let whole = usize::from(range.start()) == 0 && usize::from(range.end()) >= src.len();
    if !whole {
        return edits_touching(src, &formatted, range);
    }
    let prefix = src
        .bytes()
        .zip(formatted.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let prefix = floor_char_boundary(src, prefix);
    let max_suffix = src.len().min(formatted.len()) - prefix;
    let suffix = src
        .bytes()
        .rev()
        .zip(formatted.bytes().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(max_suffix);
    let suffix = suffix_char_boundary(src, suffix);
    let old_end = src.len() - suffix;
    let new_end = formatted.len() - suffix;
    Ok(vec![(
        TextRange::new(TextSize::new(prefix as u32), TextSize::new(old_end as u32)),
        formatted[prefix..new_end].to_string(),
    )])
}

/// The line hunks of the diff from `src` to `formatted` whose source lines
/// touch the lines of `range`, as edits of `src`.
fn edits_touching(
    src: &str,
    formatted: &str,
    range: TextRange,
) -> Result<Vec<(TextRange, String)>, FormatError> {
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(src.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line_of = |off: usize| line_starts.partition_point(|&s| s <= off).saturating_sub(1);
    let first = line_of(usize::from(range.start()).min(src.len()));
    // A selection ending at a line start does not include that line.
    let end = usize::from(range.end()).min(src.len());
    let last = if end > usize::from(range.start()) && line_starts.contains(&end) {
        line_of(end).saturating_sub(1)
    } else {
        line_of(end)
    };
    let new_lines: Vec<&str> = formatted.split_inclusive('\n').collect();
    let old_lines: Vec<&str> = src.split_inclusive('\n').collect();
    let byte_of_line = |l: usize| line_starts.get(l).copied().unwrap_or(src.len());
    let mut edits = Vec::new();
    let touches = |old: &std::ops::Range<usize>| {
        if old.is_empty() {
            old.start >= first && old.start <= last + 1
        } else {
            old.start <= last && old.end > first
        }
    };
    // `old` source lines replaced by the `new` formatted lines.
    let mut push = |old: std::ops::Range<usize>, new: std::ops::Range<usize>| {
        // Insertions count when they sit next to the selection's lines.
        if touches(&old) {
            let text: String = new_lines[new.start..new.end.min(new_lines.len())].concat();
            edits.push((
                TextRange::new(
                    TextSize::new(byte_of_line(old.start) as u32),
                    TextSize::new(byte_of_line(old.end) as u32),
                ),
                text,
            ));
        }
    };
    // Independent equal-length changes can stay within selected lines. A
    // split or collapse is one transformation: never return only its deletions
    // or its opening line, and never pull unselected source lines into it.
    let diff = similar::TextDiff::from_lines(src, formatted);
    let mut deleted: Vec<usize> = Vec::new();
    let mut inserted: Vec<usize> = Vec::new();
    let mut old_pos = 0usize;
    let mut incomplete = false;
    let mut flush = |deleted: &mut Vec<usize>, inserted: &mut Vec<usize>, at: usize| {
        // Diff runs may also contain unrelated blank-line removal. Peel off
        // complete line transformations before considering a split/collapse.
        let mut paired = 0;
        while deleted.len() != inserted.len()
            && paired < deleted.len().min(inserted.len())
            && significant_tokens(old_lines[deleted[paired]])
                == significant_tokens(new_lines[inserted[paired]])
        {
            let old = deleted[paired];
            let new = inserted[paired];
            push(old..old + 1, new..new + 1);
            paired += 1;
        }
        deleted.drain(..paired);
        inserted.drain(..paired);
        // The following statement may share the same diff run as a collapse.
        // Align independent trailing lines too, so its whitespace does not
        // prevent formatting a completely selected preceding statement.
        while deleted.len() != inserted.len() && !deleted.is_empty() && !inserted.is_empty() {
            let old = *deleted.last().unwrap();
            let new = *inserted.last().unwrap();
            if significant_tokens(old_lines[old]) != significant_tokens(new_lines[new]) {
                break;
            }
            push(old..old + 1, new..new + 1);
            deleted.pop();
            inserted.pop();
        }
        if deleted.len() == inserted.len() {
            for k in 0..deleted.len() {
                push(deleted[k]..deleted[k] + 1, inserted[k]..inserted[k] + 1);
            }
        } else {
            let old =
                deleted.first().copied().unwrap_or(at)..deleted.last().map_or(at, |last| last + 1);
            if touches(&old) && !old.is_empty() && (old.start < first || old.end > last + 1) {
                incomplete = true;
            } else {
                let new = inserted.first().copied().unwrap_or(0)
                    ..inserted.last().map_or(0, |last| last + 1);
                push(old, new);
            }
        }
        deleted.clear();
        inserted.clear();
    };
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Equal => {
                flush(&mut deleted, &mut inserted, old_pos);
                old_pos = change.old_index().map_or(old_pos, |i| i + 1);
            }
            similar::ChangeTag::Delete => {
                if let Some(i) = change.old_index() {
                    deleted.push(i);
                    old_pos = i + 1;
                }
            }
            similar::ChangeTag::Insert => {
                if let Some(i) = change.new_index() {
                    inserted.push(i);
                }
            }
        }
    }
    flush(&mut deleted, &mut inserted, old_pos);
    drop(flush);
    if incomplete {
        return Err(FormatError::Unsafe("formatting the selection requires changes outside its lines; select the complete statement".into()));
    }
    // Pure deletions and insertions at the same place must not overlap.
    edits.sort_by_key(|(r, _): &(TextRange, String)| (r.start(), r.end()));
    Ok(edits)
}

/// Refuses output that does not parse or is not equivalent to the input.
fn check_equivalent(before: &SyntaxNode, out: &str) -> Result<(), FormatError> {
    if !comments_preserved(&before.text().to_string(), out) {
        return Err(FormatError::Unsafe(
            "formatting would lose or duplicate a comment".into(),
        ));
    }

    let after = rustpy_syntax::parse(out);
    if let Some(e) = after.errors().first() {
        return Err(FormatError::Unsafe(format!(
            "output does not parse: {}",
            e.message
        )));
    }
    let (a, b) = (
        equiv::fingerprint(before),
        equiv::fingerprint(&after.syntax_node()),
    );
    if a != b {
        let i = a
            .iter()
            .zip(&b)
            .position(|(x, y)| x != y)
            .unwrap_or(a.len().min(b.len()));
        return Err(FormatError::Unsafe(format!(
            "the program would change near {:?} / {:?}",
            a.get(i),
            b.get(i)
        )));
    }
    Ok(())
}

/// Preserve the ordered ledger of complete normalized source comments. Black
/// can coalesce adjacent ordinary comments using exactly two spaces, so lexer
/// token counts alone are not a measure of lost payloads. Each original fragment
/// must still be consumed once, whole and in order; directives cannot coalesce.
/// Strings containing `#` are never included in the ledger.
pub fn comments_preserved(source: &str, formatted: &str) -> bool {
    fn payloads(source: &str) -> Vec<String> {
        let (tokens, _) = rustpy_syntax::tokenize(source);
        tokens
            .iter()
            .filter(|token| token.kind == COMMENT)
            .map(|token| black::normalized_comment(&source[token.range]))
            .collect()
    }
    fn directive(comment: &str) -> bool {
        if comment.starts_with("#!") {
            return true;
        }
        let lower = comment.to_ascii_lowercase();
        // A coding cookie may have an editor wrapper, e.g. -*- coding: utf-8 -*-.
        if lower.contains("coding:") || lower.contains("coding=") {
            return true;
        }
        lower.split(['#', ';']).any(|part| {
            let body = part.trim_start();
            [
                "fmt:",
                "yapf:",
                "isort:",
                "type:",
                "noqa",
                "nosec",
                "noinspection",
                "pylint:",
                "pyright:",
                "mypy:",
                "ruff:",
            ]
            .iter()
            .any(|prefix| body.starts_with(prefix))
        })
    }
    let before = payloads(source);
    let after = payloads(formatted);
    let mut consumed = 0;
    for comment in &after {
        if before.get(consumed) == Some(comment) {
            consumed += 1;
            continue;
        }
        let mut rest = comment.as_str();
        let mut fragments = 0;
        while let Some(fragment) = before.get(consumed) {
            if directive(fragment) {
                return false;
            }
            let Some(suffix) = rest.strip_prefix(fragment.as_str()) else {
                return false;
            };
            rest = suffix;
            consumed += 1;
            fragments += 1;
            if rest.is_empty() {
                break;
            }
            let Some(suffix) = rest.strip_prefix("  ") else {
                return false;
            };
            rest = suffix;
        }
        if !rest.is_empty() || fragments < 2 {
            return false;
        }
    }
    consumed == before.len()
}

/// Semantic fingerprint (see the `equiv` module): equal fingerprints mean the two
/// sources are the same program up to formatting.
pub fn semantic_fingerprint(src: &str) -> Option<Vec<String>> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let parse = rustpy_syntax::parse(&normalize_newlines(src));
    if !parse.errors().is_empty() {
        return None;
    }
    Some(equiv::fingerprint(&parse.syntax_node()))
}

fn uses_black_engine(opts: &FormatOptions) -> bool {
    opts.python == rustpy_settings::python_style::PythonStyle::default()
        && opts.tabs.is_none()
        && opts.continuation_indent.is_none()
        && opts.quote != QuoteStyle::Single
        && opts.blank_lines_top_level == 2
        && opts.blank_lines_in_class == 1
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn suffix_char_boundary(s: &str, mut n: usize) -> usize {
    while n > 0 && !s.is_char_boundary(s.len() - n) {
        n -= 1;
    }
    n
}

fn normalize_newlines(src: &str) -> String {
    if src.contains('\r') {
        src.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        src.to_string()
    }
}

// ---------------------------------------------------------------------------
// Scanning: tokens → logical items

/// Role of a `:` token (decides the whitespace around it).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ColonRole {
    /// Opens a block (`if x:`); statements after it on the same line are split off.
    Block,
    /// Slice colon with simple operands: no spaces (`a[1:2]`).
    Slice,
    /// Slice colon with complex operands: spaces like a binary operator (`a[x + 1 :]`).
    SliceSpaced,
    /// Dict item, annotation, lambda, …: no space before, one after.
    Other,
}

#[derive(Clone, Debug)]
struct Tok {
    text: String,
    kind: SyntaxKind,
    parent: SyntaxKind,
    colon: ColonRole,
    /// Prefix operator (unary minus, star unpacking, decorator `@`): no space after.
    prefix: bool,
    /// `=` without surrounding spaces (keyword argument / unannotated default).
    tight_eq: bool,
    /// `**` hugging simple operands (`x**2`).
    hug_power: bool,
    space_before: bool,
    /// A trailing comma may be added when this opening bracket is exploded.
    comma_ok: bool,
    /// The source has a trailing comma before this bracket's closing token.
    magic: bool,
    /// Physical source line (for comment-preserving rendering).
    src_line: usize,
    is_comment: bool,
    annotation: bool,
}

#[derive(Clone, Debug, Default)]
struct LineAttrs {
    is_def: bool,
    is_decorator: bool,
    block_opener: bool,
    /// `def f(): ...` one-liner.
    is_stub: bool,
    is_docstring: bool,
    is_import: bool,
    is_class: bool,
    /// `else`, `elif`, `except`, `finally`, `case` header.
    is_continuation_clause: bool,
    /// Name of the defined function/class (overload stubs are kept together).
    def_name: Option<String>,
}

#[derive(Clone, Debug)]
enum Item {
    Line {
        level: usize,
        toks: Vec<Tok>,
        trailing_comment: Option<String>,
        attrs: LineAttrs,
        /// Keep the physical line structure (comments inside brackets).
        preserve: bool,
    },
    Comment {
        level: usize,
        text: String,
    },
    Verbatim {
        level: usize,
        text: String,
        preserve_tail: bool,
    },
    Blank {
        count: usize,
    },
}

struct Scanner<'a> {
    src: &'a str,
    tokens: &'a [Token],
    root: &'a SyntaxNode,
    opts: &'a FormatOptions,
    line_starts: Vec<usize>,
    level: usize,
    indent_cols: Vec<usize>,
    /// Extra levels for statements after a block colon on the same line (`if x: y`).
    virtual_levels: usize,
    depth: usize,
    cur: Vec<Tok>,
    cur_trailing: Option<String>,
    cur_preserve: bool,
    cur_skip: bool,
    /// Part of the current logical line was already flushed (`;` or `if x: y`).
    partial_line: bool,
    pending_blank: usize,
    after_comment_nl: bool,
    items: Vec<Item>,
}

fn token_line(line_starts: &[usize], offset: usize) -> usize {
    match line_starts.binary_search(&offset) {
        Ok(i) => i,
        Err(i) => i - 1,
    }
}

fn indent_width(text: &str) -> usize {
    let mut col = 0;
    for c in text.chars() {
        match c {
            '\t' => col = (col / 8 + 1) * 8,
            _ => col += 1,
        }
    }
    col
}

const BLOCK_PARENTS: &[SyntaxKind] = &[
    FUNC_DEF,
    CLASS_DEF,
    IF_STMT,
    ELIF_CLAUSE,
    ELSE_CLAUSE,
    WHILE_STMT,
    FOR_STMT,
    TRY_STMT,
    EXCEPT_CLAUSE,
    FINALLY_CLAUSE,
    WITH_STMT,
    MATCH_STMT,
    CASE_CLAUSE,
];

impl<'a> Scanner<'a> {
    fn new(
        src: &'a str,
        tokens: &'a [Token],
        root: &'a SyntaxNode,
        opts: &'a FormatOptions,
    ) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            src,
            tokens,
            root,
            opts,
            line_starts,
            level: 0,
            indent_cols: vec![0],
            virtual_levels: 0,
            depth: 0,
            cur: Vec::new(),
            cur_trailing: None,
            cur_preserve: false,
            cur_skip: false,
            partial_line: false,
            pending_blank: 0,
            after_comment_nl: false,
            items: Vec::new(),
        }
    }

    /// Tree token kind (soft keywords become `MATCH_KW` etc.), parent kind and parent node.
    fn tree_info(&self, t: &Token) -> (SyntaxKind, SyntaxKind, SyntaxKind, Option<SyntaxNode>) {
        let (kind, parent) = match self.root.covering_element(t.range) {
            NodeOrToken::Token(tok) => (tok.kind(), tok.parent()),
            NodeOrToken::Node(n) => (t.kind, Some(n)),
        };
        let (pk, gk) = match &parent {
            Some(p) => (p.kind(), p.parent().map(|g| g.kind()).unwrap_or(MODULE)),
            None => (MODULE, MODULE),
        };
        (kind, pk, gk, parent)
    }

    fn text(&self, t: &Token) -> &'a str {
        &self.src[t.range]
    }

    fn line_of(&self, t: &Token) -> usize {
        token_line(&self.line_starts, t.range.start().into())
    }

    fn line_end(&self, line: usize) -> usize {
        if line + 1 < self.line_starts.len() {
            self.line_starts[line + 1]
        } else {
            self.src.len()
        }
    }

    fn next_significant(&self, mut i: usize) -> Option<usize> {
        while i < self.tokens.len() {
            if !matches!(self.tokens[i].kind, WHITESPACE | COMMENT | NL) {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Indentation level of the standalone comment at token `i` (column `col`).
    /// Comments take the level of the following line; before a dedent the
    /// column decides which of the closing blocks the comment belongs to.
    fn comment_level(&self, i: usize, col: usize) -> usize {
        let mut j = i + 1;
        while j < self.tokens.len() && matches!(self.tokens[j].kind, COMMENT | NL | WHITESPACE) {
            j += 1;
        }
        match self.tokens.get(j).map(|t| t.kind) {
            Some(INDENT) => self.level + 1,
            Some(DEDENT) | Some(EOF) | None => {
                let dedents = self.tokens[j.min(self.tokens.len())..]
                    .iter()
                    .take_while(|t| t.kind == DEDENT)
                    .count();
                let by_column = self
                    .indent_cols
                    .iter()
                    .skip(1)
                    .filter(|c| **c <= col)
                    .count();
                by_column.clamp(self.level.saturating_sub(dedents), self.level)
            }
            _ => self.level,
        }
    }

    fn comment_column(&self, t: &Token) -> usize {
        let line = self.line_of(t);
        indent_width(&self.src[self.line_starts[line]..usize::from(t.range.start())])
    }

    fn make_tok(&self, t: &Token, text: String, kind: SyntaxKind, parent: SyntaxKind) -> Tok {
        Tok {
            text,
            kind,
            parent,
            colon: ColonRole::Other,
            prefix: false,
            tight_eq: false,
            hug_power: false,
            space_before: false,
            comma_ok: false,
            magic: false,
            src_line: self.line_of(t),
            is_comment: false,
            annotation: self
                .root
                .covering_element(t.range)
                .parent()
                .is_some_and(|node| {
                    node.ancestors().any(|node| {
                        matches!(
                            node.kind(),
                            ANNOTATION | RETURN_ANNOTATION | TYPE_ALIAS_STMT
                        )
                    })
                }),
        }
    }

    fn scan(mut self) -> Vec<Item> {
        let tokens = self.tokens;
        let mut i = 0;
        while i < tokens.len() {
            let t = &tokens[i];
            match t.kind {
                WHITESPACE | EOF => {}
                COMMENT => {
                    if self
                        .opts
                        .formatter_tags
                        .as_ref()
                        .is_some_and(|tags| tags.is_off(self.text(t)))
                    {
                        i = self.scan_verbatim_region(i);
                        continue;
                    }
                    if self.cur.is_empty() && self.depth == 0 {
                        if is_fmt_off(self.text(t)) {
                            i = self.scan_verbatim_region(i);
                            continue;
                        }
                        let level = self.comment_level(i, self.comment_column(t));
                        let text = if self.opts.python.space_after_comment {
                            normalize_comment(self.text(t), t.range.start() == TextSize::new(0))
                        } else {
                            self.text(t).trim_end().into()
                        };
                        self.flush_blank();
                        self.items.push(Item::Comment { level, text });
                        self.after_comment_nl = true;
                    } else if self.depth == 0 {
                        if is_fmt_skip(self.text(t)) {
                            self.cur_skip = true;
                        }
                        self.cur_trailing = Some(if self.opts.python.space_after_comment {
                            normalize_comment(self.text(t), false)
                        } else {
                            self.text(t).trim_end().into()
                        });
                    } else {
                        self.cur_preserve = true;
                        let mut tok = self.make_tok(
                            t,
                            if self.opts.python.space_after_comment {
                                normalize_comment(self.text(t), false)
                            } else {
                                self.text(t).trim_end().into()
                            },
                            COMMENT,
                            MODULE,
                        );
                        tok.is_comment = true;
                        self.cur.push(tok);
                    }
                }
                NL => {
                    if self.depth == 0 && self.cur.is_empty() {
                        if self.after_comment_nl {
                            self.after_comment_nl = false;
                        } else {
                            self.pending_blank += 1;
                        }
                    }
                }
                NEWLINE => {
                    self.flush_line();
                    self.virtual_levels = 0;
                    self.depth = 0;
                    self.partial_line = false;
                }
                INDENT => {
                    self.level += 1;
                    self.indent_cols.push(indent_width(self.text(t)));
                }
                DEDENT => {
                    self.level = self.level.saturating_sub(1);
                    self.indent_cols.pop();
                }
                SEMICOLON if self.depth == 0 => {
                    self.partial_line = true;
                    self.flush_line();
                }
                FSTRING_START | TSTRING_START => {
                    let end_kind = if t.kind == FSTRING_START {
                        FSTRING_END
                    } else {
                        TSTRING_END
                    };
                    let mut nest = 0usize;
                    let mut j = i;
                    let mut end = t.range.end();
                    while j < tokens.len() {
                        let k = tokens[j].kind;
                        if k == t.kind {
                            nest += 1;
                        } else if k == end_kind {
                            nest -= 1;
                            if nest == 0 {
                                end = tokens[j].range.end();
                                break;
                            }
                        }
                        j += 1;
                    }
                    let (_, parent, _, _) = self.tree_info(t);
                    let text = self.src[usize::from(t.range.start())..usize::from(end)].to_string();
                    let tok = self.make_tok(t, text, STRING, parent);
                    self.cur.push(tok);
                    i = j + 1;
                    continue;
                }
                _ => {
                    let (kind, parent, _, pnode) = self.tree_info(t);
                    let text = self.text(t);
                    let mut tok = self.make_tok(t, text.to_string(), kind, parent);
                    match kind {
                        L_PAREN | L_BRACK | L_BRACE => self.depth += 1,
                        R_PAREN | R_BRACK | R_BRACE => self.depth = self.depth.saturating_sub(1),
                        COLON => {
                            tok.colon = if BLOCK_PARENTS.contains(&parent) {
                                ColonRole::Block
                            } else if parent == SLICE_EXPR {
                                if pnode.as_ref().map(slice_is_complex).unwrap_or(false) {
                                    ColonRole::SliceSpaced
                                } else {
                                    ColonRole::Slice
                                }
                            } else {
                                ColonRole::Other
                            };
                        }
                        STRING => {
                            let style = python_style::string_quote(pnode.as_ref(), self.opts);
                            tok.text = normalize_string(text, style);
                        }
                        INT_NUMBER | FLOAT_NUMBER | COMPLEX_NUMBER => {
                            tok.text = normalize_number(text)
                        }
                        MINUS | PLUS | TILDE => {
                            tok.prefix = matches!(parent, UNARY_EXPR | MATCH_VALUE_PATTERN);
                        }
                        STAR | STAR_STAR => {
                            tok.prefix = matches!(
                                parent,
                                STAR_EXPR
                                    | DOUBLE_STAR_EXPR
                                    | STAR_ARG
                                    | STAR_STAR_ARG
                                    | STAR_PARAM
                                    | STAR_STAR_PARAM
                                    | STAR_SEPARATOR
                                    | DICT_UNPACK
                                    | MATCH_STAR_PATTERN
                                    | MATCH_MAPPING_REST
                                    | TYPE_VAR_TUPLE_PARAM
                                    | PARAM_SPEC_PARAM
                                    | IMPORT_FROM_STMT
                            );
                            if kind == STAR_STAR && parent == BINARY_EXPR {
                                tok.hug_power =
                                    pnode.as_ref().map(power_is_simple).unwrap_or(false);
                            }
                        }
                        AT => tok.prefix = parent == DECORATOR,
                        EQ => {
                            tok.tight_eq = parent == KEYWORD_ARG
                                || parent == MATCH_KEYWORD_PATTERN
                                || (parent == PARAM
                                    && !pnode.as_ref().map(param_has_annotation).unwrap_or(false));
                        }
                        _ => {}
                    }
                    let is_block_colon = tok.colon == ColonRole::Block && self.depth == 0;
                    self.cur.push(tok);
                    if is_block_colon {
                        if let Some(ni) = self.next_significant(i + 1) {
                            let nk = tokens[ni].kind;
                            if !matches!(nk, NEWLINE | EOF) {
                                // `def f(): ...` / `class A: ...` stubs stay on one line.
                                let is_def = matches!(
                                    self.cur.first().map(|f| f.kind),
                                    Some(DEF_KW | CLASS_KW | ASYNC_KW)
                                );
                                let stub = is_def
                                    && !self.opts.python.force_after_colon
                                    && nk == ELLIPSIS
                                    && self
                                        .next_significant(ni + 1)
                                        .map(|m| matches!(tokens[m].kind, NEWLINE | EOF))
                                        .unwrap_or(true);
                                if !stub {
                                    self.partial_line = true;
                                    self.flush_line();
                                    self.virtual_levels += 1;
                                }
                            }
                        }
                    }
                }
            }
            i += 1;
        }
        self.flush_line();
        self.flush_blank();
        self.items
    }

    /// Copies everything from the `# fmt: off` line through the `# fmt: on`
    /// line verbatim; returns the index of the first token after the region.
    fn scan_verbatim_region(&mut self, i: usize) -> usize {
        let start_line = self
            .cur
            .first()
            .map_or_else(|| self.line_of(&self.tokens[i]), |t| t.src_line);
        // An inline marker opts out its whole logical statement, including
        // an already-emitted one-line-suite header on the same physical line.
        while self.items.last().is_some_and(|item| matches!(item, Item::Line {toks, ..} if toks.first().is_some_and(|t| t.src_line >= start_line))) {
            self.items.pop();
        }
        self.cur.clear();
        self.cur_trailing = None;
        self.cur_preserve = false;
        self.cur_skip = false;
        self.partial_line = false;
        self.virtual_levels = 0;
        let first_comment = self.text(&self.tokens[i]);
        let mut black_active = is_fmt_off(first_comment);
        let mut custom_active = self
            .opts
            .formatter_tags
            .as_ref()
            .is_some_and(|tags| tags.is_off(first_comment));
        let mut custom_region = custom_active;
        let level = self.comment_level(i, self.comment_column(&self.tokens[i]));
        let mut j = i + 1;
        let mut end_line = self.line_starts.len() - 1;
        let mut found = None;
        while j < self.tokens.len() {
            let tj = &self.tokens[j];
            match tj.kind {
                INDENT => {
                    self.level += 1;
                    self.indent_cols.push(indent_width(self.text(tj)));
                }
                DEDENT => {
                    self.level = self.level.saturating_sub(1);
                    self.indent_cols.pop();
                }
                COMMENT => {
                    let comment = self.text(tj);
                    if is_fmt_off(comment) {
                        black_active = true;
                    }
                    if is_fmt_on(comment) {
                        black_active = false;
                    }
                    if let Some(tags) = &self.opts.formatter_tags {
                        if tags.is_off(comment) {
                            custom_active = true;
                            custom_region = true;
                        }
                        if tags.is_on(comment) {
                            custom_active = false;
                        }
                    }
                    if !black_active && !custom_active && self.depth == 0 {
                        end_line = self.line_of(tj);
                        found = Some(j);
                        break;
                    }
                }
                L_PAREN | L_BRACK | L_BRACE => self.depth += 1,
                R_PAREN | R_BRACK | R_BRACE => self.depth = self.depth.saturating_sub(1),
                NEWLINE if !black_active && !custom_active && self.depth == 0 => {
                    end_line = self.line_of(tj);
                    found = Some(j);
                    break;
                }
                _ => {}
            }
            j += 1;
        }
        let start = self.line_starts[start_line];
        let end = self.line_end(end_line);
        self.flush_blank();
        let mut text = self.src[start..end].to_string();
        if !custom_region && !text.ends_with('\n') {
            text.push('\n');
        }
        self.items.push(Item::Verbatim {
            level,
            text,
            preserve_tail: custom_region && end == self.src.len(),
        });
        self.after_comment_nl = true;
        found.map(|f| f + 1).unwrap_or(self.tokens.len())
    }

    fn flush_blank(&mut self) {
        if self.pending_blank > 0 {
            self.items.push(Item::Blank {
                count: self.pending_blank,
            });
            self.pending_blank = 0;
        }
    }

    fn flush_line(&mut self) {
        let trailing = self.cur_trailing.take();
        let preserve = std::mem::take(&mut self.cur_preserve);
        let skip = std::mem::take(&mut self.cur_skip);
        if self.cur.is_empty() {
            return;
        }
        self.flush_blank();
        let toks = std::mem::take(&mut self.cur);
        let level = self.level + self.virtual_levels;
        let first = &toks[0];
        let last = toks.last().unwrap();
        if skip && !self.partial_line {
            // `# fmt: skip`: keep the statement as written (re-indented).
            let start = self.line_starts[first.src_line];
            let end = self.line_end(last.src_line);
            let mut text = render_indent(
                level * self.opts.indent,
                level * self.opts.indent,
                self.opts,
            );
            text.push_str(self.src[start..end].trim_start());
            if !text.ends_with('\n') {
                text.push('\n');
            }
            self.items.push(Item::Verbatim {
                level,
                text,
                preserve_tail: false,
            });
            return;
        }
        let mut attrs = LineAttrs::default();
        attrs.is_decorator = first.kind == AT && first.parent == DECORATOR;
        attrs.is_def = matches!(first.kind, DEF_KW | CLASS_KW)
            || (first.kind == ASYNC_KW && toks.get(1).map(|t| t.kind == DEF_KW).unwrap_or(false));
        attrs.is_continuation_clause = matches!(
            first.kind,
            ELSE_KW | ELIF_KW | EXCEPT_KW | FINALLY_KW | CASE_KW
        );
        attrs.is_import = matches!(first.kind, IMPORT_KW | FROM_KW);
        attrs.is_class = first.kind == CLASS_KW;
        attrs.block_opener = last.kind == COLON && last.colon == ColonRole::Block;
        attrs.is_stub = attrs.is_def && last.kind == ELLIPSIS;
        if attrs.is_def {
            attrs.def_name = toks
                .iter()
                .skip_while(|t| !matches!(t.kind, DEF_KW | CLASS_KW))
                .nth(1)
                .map(|t| t.text.clone());
        }
        attrs.is_docstring = toks.len() == 1 && first.kind == STRING && first.parent == STRING_EXPR;
        let toks = prepare_line(toks, self.opts);
        self.items.push(Item::Line {
            level,
            toks,
            trailing_comment: trailing,
            attrs,
            preserve,
        });
    }
}

/// Black's "simple" slice operand: names, literals and unary operators thereof.
fn slice_is_complex(slice: &SyntaxNode) -> bool {
    fn simple(n: &SyntaxNode) -> bool {
        match n.kind() {
            NAME_EXPR | LITERAL_EXPR | STRING_EXPR => true,
            UNARY_EXPR => n.children().all(|c| simple(&c)),
            _ => false,
        }
    }
    slice.children().any(|c| !simple(&c))
}

/// Black's "simple power operand": names, numbers, attribute chains, unary minus of those.
fn power_is_simple(binary: &SyntaxNode) -> bool {
    fn simple(n: &SyntaxNode) -> bool {
        match n.kind() {
            NAME_EXPR | LITERAL_EXPR => true,
            ATTRIBUTE_EXPR | UNARY_EXPR => n.children().all(|c| simple(&c)),
            _ => false,
        }
    }
    binary.children().all(|c| simple(&c))
}

fn param_has_annotation(param: &SyntaxNode) -> bool {
    param.children().any(|c| c.kind() == ANNOTATION)
}

fn fmt_directive(text: &str) -> &str {
    text.trim_start_matches('#').trim()
}

fn is_fmt_off(text: &str) -> bool {
    matches!(
        fmt_directive(text),
        "fmt: off" | "fmt:off" | "yapf: disable"
    )
}

fn is_fmt_on(text: &str) -> bool {
    matches!(fmt_directive(text), "fmt: on" | "fmt:on" | "yapf: enable")
}

fn is_fmt_skip(text: &str) -> bool {
    matches!(fmt_directive(text), "fmt: skip" | "fmt:skip")
}

/// `#comment` → `# comment`, trailing whitespace removed; shebangs, `#!`,
/// `#:` and `##` style comments are kept as written.
fn normalize_comment(text: &str, at_file_start: bool) -> String {
    let text = text.trim_end();
    if at_file_start && text.starts_with("#!") {
        return text.to_string();
    }
    let body = &text[1..];
    if body.is_empty() || body.starts_with([' ', '!', ':', '#']) {
        return text.to_string();
    }
    format!("# {body}")
}

/// Normalizes the quotes and prefix of a (non-f) string literal.
fn normalize_string(text: &str, style: QuoteStyle) -> String {
    let target = match style {
        QuoteStyle::Double => '"',
        QuoteStyle::Single => '\'',
        QuoteStyle::Preserve => return text.to_string(),
    };
    let other = if target == '"' { '\'' } else { '"' };
    let prefix_len = text.chars().take_while(|c| c.is_ascii_alphabetic()).count();
    let (prefix, body) = text.split_at(prefix_len);
    // Black: lowercase prefixes (except `R`), drop the redundant `u`.
    let prefix: String = prefix
        .chars()
        .filter(|c| !matches!(c, 'u' | 'U'))
        .map(|c| if c == 'R' { c } else { c.to_ascii_lowercase() })
        .collect();
    let triple = body.starts_with("'''") || body.starts_with("\"\"\"");
    let quote_len = if triple { 3 } else { 1 };
    if body.len() < 2 * quote_len {
        return text.to_string();
    }
    let q = body.chars().next().unwrap();
    if q == target {
        return format!("{prefix}{body}");
    }
    let inner = &body[quote_len..body.len() - quote_len];
    let raw = prefix.contains(['r', 'R']);
    if triple || raw {
        // Escapes cannot be rewritten here: only swap when the body has no target quote.
        if inner.contains(target) || inner.ends_with('\\') {
            return format!("{prefix}{body}");
        }
        let quotes: String = std::iter::repeat_n(target, quote_len).collect();
        return format!("{prefix}{quotes}{inner}{quotes}");
    }
    let mut new_inner = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    let mut unescaped_target = 0usize;
    let mut escaped_other = 0usize;
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(n) if n == other => {
                    escaped_other += 1;
                    new_inner.push(other);
                }
                Some(n) => {
                    new_inner.push('\\');
                    new_inner.push(n);
                }
                None => new_inner.push('\\'),
            }
        } else if c == target {
            unescaped_target += 1;
            new_inner.push('\\');
            new_inner.push(c);
        } else {
            new_inner.push(c);
        }
    }
    // Prefer the target quote unless it would add more escapes than it removes.
    if unescaped_target > escaped_other {
        return format!("{prefix}{body}");
    }
    format!("{prefix}{target}{new_inner}{target}")
}

/// Lowercase radix prefixes / exponent / imaginary suffix, uppercase hex digits.
fn normalize_number(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("0x") {
        return format!("0x{}", rest.to_ascii_uppercase());
    }
    lower
}

// ---------------------------------------------------------------------------
// Whitespace model & bracket pairing

const OPEN: &[SyntaxKind] = &[L_PAREN, L_BRACK, L_BRACE];
const CLOSE: &[SyntaxKind] = &[R_PAREN, R_BRACK, R_BRACE];

fn is_atom_end(k: SyntaxKind) -> bool {
    matches!(
        k,
        IDENT
            | MATCH_KW
            | CASE_KW
            | TYPE_KW
            | UNDERSCORE_KW
            | INT_NUMBER
            | FLOAT_NUMBER
            | COMPLEX_NUMBER
            | STRING
            | R_PAREN
            | R_BRACK
            | R_BRACE
            | NONE_KW
            | TRUE_KW
            | FALSE_KW
            | ELLIPSIS
    )
}

/// Keywords that take a space on both sides (soft keywords only in keyword position).
fn is_kw(t: &Tok) -> bool {
    match t.kind {
        MATCH_KW => t.parent == MATCH_STMT,
        CASE_KW => t.parent == CASE_CLAUSE,
        TYPE_KW => t.parent == TYPE_ALIAS_STMT,
        UNDERSCORE_KW | NONE_KW | TRUE_KW | FALSE_KW => false,
        k => k.is_keyword(),
    }
}

fn is_binary_op(k: SyntaxKind) -> bool {
    matches!(
        k,
        PLUS | MINUS
            | STAR
            | SLASH
            | SLASH_SLASH
            | PERCENT
            | AT
            | STAR_STAR
            | SHL
            | SHR
            | AMP
            | PIPE
            | CARET
            | LT
            | GT
            | LT_EQ
            | GT_EQ
            | EQ_EQ
            | NOT_EQ
            | COLON_EQ
            | ARROW
            | EQ
    ) || k.is_augmented_assign_op()
}

/// Whether a space goes between two adjacent tokens of a logical line.
fn default_space_between(prev: &Tok, cur: &Tok) -> bool {
    if cur.is_comment {
        return true;
    }
    if prev.is_comment {
        return false;
    }
    let (pk, ck) = (prev.kind, cur.kind);
    if CLOSE.contains(&ck) || OPEN.contains(&pk) {
        return false;
    }
    if matches!(ck, COMMA | SEMICOLON) {
        return false;
    }
    if pk == COMMA {
        return true;
    }
    if ck == STAR && cur.parent == EXCEPT_CLAUSE {
        return false; // except*
    }
    if pk == STAR && prev.parent == EXCEPT_CLAUSE {
        return true;
    }
    if ck == DOT {
        return is_kw(prev) || pk == INT_NUMBER; // `from . import`, `1 .real`
    }
    if pk == DOT {
        return is_kw(cur); // `from . import x`
    }
    if ck == COLON {
        return cur.colon == ColonRole::SliceSpaced && pk != COLON;
    }
    if pk == COLON {
        return prev.colon != ColonRole::Slice;
    }
    if (ck == EQ && cur.tight_eq) || (pk == EQ && prev.tight_eq) {
        return false;
    }
    if (ck == STAR_STAR && cur.hug_power) || (pk == STAR_STAR && prev.hug_power) {
        return false;
    }
    if prev.prefix {
        return false;
    }
    if ck == L_PAREN || ck == L_BRACK {
        return is_kw(prev) || !is_atom_end(pk);
    }
    if ck == L_BRACE {
        return pk != DOT;
    }
    if is_binary_op(ck) || is_binary_op(pk) {
        return true;
    }
    true
}

/// Computes inter-token spacing, bracket pairing and trailing-comma eligibility.
fn prepare_line(mut toks: Vec<Tok>, opts: &FormatOptions) -> Vec<Tok> {
    for i in 1..toks.len() {
        let sb = python_style::space(
            &toks[i - 1],
            &toks[i],
            opts,
            default_space_between(&toks[i - 1], &toks[i]),
        );
        toks[i].space_before = sb;
    }
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..toks.len() {
        if OPEN.contains(&toks[i].kind) {
            stack.push(i);
        } else if CLOSE.contains(&toks[i].kind) {
            let Some(o) = stack.pop() else { continue };
            let inner = i - o - 1;
            let mut commas = 0usize;
            let mut has_for = false;
            let mut d = 0usize;
            for t in &toks[o + 1..i] {
                if OPEN.contains(&t.kind) {
                    d += 1;
                } else if CLOSE.contains(&t.kind) {
                    d = d.saturating_sub(1);
                } else if d == 0 && t.kind == COMMA && !has_for {
                    commas += 1;
                } else if d == 0 && t.kind == FOR_KW {
                    has_for = true;
                }
            }
            let last_is_comma = inner > 0 && toks[i - 1].kind == COMMA;
            let comma_ok = inner > 0
                && match toks[o].parent {
                    ARG_LIST
                    | PARAM_LIST
                    | LIST_EXPR
                    | SET_EXPR
                    | DICT_EXPR
                    | IMPORT_FROM_STMT
                    | MATCH_SEQUENCE_PATTERN
                    | MATCH_MAPPING_PATTERN
                    | MATCH_CLASS_PATTERN => !(has_for && commas == 0),
                    // A one-tuple's comma is syntax, not a trailing comma.
                    TUPLE_EXPR => commas >= 2 || (commas == 1 && !last_is_comma),
                    _ => false,
                };
            let magic = opts.magic_trailing_comma && comma_ok && last_is_comma;
            toks[o].comma_ok = comma_ok;
            toks[o].magic = magic;
            toks[i].magic = magic;
        }
    }
    toks
}

// ---------------------------------------------------------------------------
// Emission

struct PrevLine {
    level: usize,
    block_opener: bool,
    is_def: bool,
    is_class: bool,
    is_decorator: bool,
    is_stub: bool,
    /// Module or class docstring: exactly one blank line follows.
    is_docstring: bool,
    class_docstring: bool,
    is_import: bool,
    is_comment: bool,
    /// Comment block directly attached to the following def/class.
    attached: bool,
    def_name: Option<String>,
}

impl PrevLine {
    fn plain(level: usize) -> Self {
        Self {
            level,
            block_opener: false,
            is_def: false,
            is_class: false,
            is_decorator: false,
            is_stub: false,
            is_docstring: false,
            class_docstring: false,
            is_import: false,
            is_comment: false,
            attached: false,
            def_name: None,
        }
    }
}

struct Emitter<'a> {
    opts: &'a FormatOptions,
    out: String,
    prev: Option<PrevLine>,
    pending_blank: usize,
    /// Levels of the def/class headers whose bodies are still open.
    def_stack: Vec<usize>,
    seen_statement: bool,
}

impl<'a> Emitter<'a> {
    fn new(opts: &'a FormatOptions) -> Self {
        Self {
            opts,
            out: String::new(),
            prev: None,
            pending_blank: 0,
            def_stack: Vec::new(),
            seen_statement: false,
        }
    }

    fn def_required(&self, level: usize) -> usize {
        if level == 0 {
            self.opts.blank_lines_top_level
        } else {
            self.opts.blank_lines_in_class
        }
    }

    /// Pops the defs/classes whose body ends before a line at `level` and
    /// returns the blank lines they require after them (Black's `previous_defs`).
    fn pop_defs(&mut self, level: usize, attrs: &LineAttrs) -> Option<usize> {
        let mut required = None;
        while let Some(&top) = self.def_stack.last() {
            if top < level {
                break;
            }
            self.def_stack.pop();
            // A dependent clause (`else:` / `except:`) after an indented def
            // is not really top-level: one blank line.
            required = Some(if level > 0 || (top > 0 && attrs.is_continuation_clause) {
                self.opts.blank_lines_in_class
            } else {
                self.opts.blank_lines_top_level
            });
        }
        required
    }

    /// Number of blank lines to emit before an item at `level`.
    fn blank_lines_before(&mut self, level: usize, attrs: &LineAttrs, for_comment: bool) -> usize {
        let popped = self.pop_defs(level, attrs);
        let Some(prev) = &self.prev else { return 0 };
        if prev.is_decorator && prev.level == level {
            return 0;
        }
        if prev.block_opener && prev.level + 1 == level {
            // Start of a block: no blank lines in def/class bodies; a nested
            // def/class under another opener keeps its separation
            // (`if TYPE_CHECKING:` + blank + `def ...`).
            if prev.is_def {
                return 0;
            }
            if prev.is_class && attrs.is_def {
                return self.opts.python.blank_lines.before_first_method;
            }
            if prev.is_class && attrs.is_docstring {
                return self.opts.python.blank_lines.before_class_docstring;
            }
            if (attrs.is_def || attrs.is_decorator) && !for_comment {
                return self.def_required(level);
            }
            return self.pending_blank.min(1);
        }
        if prev.is_comment && prev.attached {
            return 0;
        }
        let cap =
            if attrs.is_def || attrs.is_class || attrs.is_decorator || prev.is_def || prev.is_class
            {
                self.opts.python.blank_lines.max_declarations
            } else {
                self.opts.python.blank_lines.max_code
            };
        let mut required = popped.unwrap_or(0);
        if attrs.is_def || attrs.is_decorator {
            required = required.max(self.def_required(level));
        }
        if prev.is_docstring && prev.level == level {
            required = required.max(if prev.class_docstring {
                self.opts.python.blank_lines.after_class_docstring
            } else {
                1
            });
        }
        if attrs.is_import && !prev.is_import {
            required = required.max(self.opts.python.blank_lines.before_imports);
        }
        if prev.is_import && !attrs.is_import && prev.level == level {
            required = required.max(if level == 0 {
                self.opts.python.blank_lines.after_imports
            } else {
                self.opts.python.blank_lines.after_local_imports
            });
        }
        // Black: dummy implementations (overload stubs) directly followed by a
        // decorator or by a def of the same name stay together.
        if prev.is_stub && prev.level == level && self.pending_blank == 0 {
            let same_name =
                attrs.is_def && attrs.def_name.is_some() && attrs.def_name == prev.def_name;
            if attrs.is_decorator || same_name {
                return 0;
            }
        }
        required.max(self.pending_blank.min(cap))
    }

    fn push_blank_lines(&mut self, n: usize) {
        if self.prev.is_some() {
            for _ in 0..n {
                self.out.push('\n');
            }
        }
        self.pending_blank = 0;
    }

    fn emit(mut self, items: &[Item]) -> String {
        for (idx, item) in items.iter().enumerate() {
            match item {
                Item::Blank { count } => self.pending_blank += count,
                Item::Verbatim { level, text, .. } => {
                    let n = self.blank_lines_before(*level, &LineAttrs::default(), false);
                    self.push_blank_lines(n);
                    self.out.push_str(text);
                    self.prev = Some(PrevLine::plain(*level));
                    self.seen_statement = true;
                }
                Item::Comment { level, text } => {
                    // A comment block directly followed by a def/class at the
                    // same level belongs to it: blank lines go before the block.
                    let attached = items[idx + 1..]
                        .iter()
                        .find(|it| !matches!(it, Item::Comment { .. }))
                        .map(|it| match it {
                            Item::Line {
                                level: l, attrs, ..
                            } => (attrs.is_def || attrs.is_decorator) && *l == *level,
                            _ => false,
                        })
                        .unwrap_or(false);
                    let prev_same_level_comment = self
                        .prev
                        .as_ref()
                        .map(|p| p.is_comment && p.level == *level)
                        .unwrap_or(false);
                    let n = if prev_same_level_comment && self.pending_blank == 0 {
                        0
                    } else {
                        let attrs = LineAttrs {
                            is_def: attached,
                            ..LineAttrs::default()
                        };
                        self.blank_lines_before(*level, &attrs, true)
                    };
                    self.push_blank_lines(n);
                    self.out.push_str(&render_indent(
                        level * self.opts.indent,
                        level * self.opts.indent,
                        self.opts,
                    ));
                    self.out.push_str(text);
                    self.out.push('\n');
                    self.prev = Some(PrevLine {
                        is_comment: true,
                        attached,
                        ..PrevLine::plain(*level)
                    });
                }
                Item::Line {
                    level,
                    toks,
                    trailing_comment,
                    attrs,
                    preserve,
                } => {
                    let n = self.blank_lines_before(*level, attrs, false);
                    self.push_blank_lines(n);
                    let lines = if *preserve {
                        render_preserved(toks, *level, self.opts)
                    } else {
                        split_line(toks, *level * self.opts.indent, self.opts, false)
                    };
                    let count = lines.len();
                    for (i, l) in lines.into_iter().enumerate() {
                        let prefix = l.bytes().take_while(|b| *b == b' ').count();
                        self.out.push_str(&render_indent(
                            prefix,
                            level * self.opts.indent,
                            self.opts,
                        ));
                        self.out.push_str(&l[prefix..]);
                        if i + 1 == count {
                            if let Some(c) = trailing_comment {
                                self.out.push_str(
                                    &" ".repeat(self.opts.python.spaces_before_comment.min(16)),
                                );
                                self.out.push_str(c);
                            }
                        }
                        self.out.push('\n');
                    }
                    if attrs.is_def {
                        self.def_stack.push(*level);
                    }
                    let class_docstring = attrs.is_docstring
                        && self.prev.as_ref().is_some_and(|previous| {
                            previous.is_class
                                && previous.block_opener
                                && previous.level + 1 == *level
                        });
                    let is_docstring = attrs.is_docstring
                        && ((*level == 0 && !self.seen_statement)
                            || self
                                .prev
                                .as_ref()
                                .map(|p| p.is_class && p.block_opener && p.level + 1 == *level)
                                .unwrap_or(false));
                    self.seen_statement = true;
                    self.prev = Some(PrevLine {
                        level: *level,
                        block_opener: attrs.block_opener,
                        is_def: attrs.is_def,
                        is_class: attrs.is_class,
                        is_decorator: attrs.is_decorator,
                        is_stub: attrs.is_stub,
                        is_docstring,
                        class_docstring,
                        is_import: attrs.is_import,
                        is_comment: false,
                        attached: false,
                        def_name: attrs.def_name.clone(),
                    });
                }
            }
        }
        let preserve_tail = items.last().is_some_and(|item| {
            matches!(
                item,
                Item::Verbatim {
                    preserve_tail: true,
                    ..
                }
            )
        });
        if !preserve_tail {
            while self.out.ends_with("\n\n") {
                self.out.pop();
            }
            if !self.out.is_empty() && !self.out.ends_with('\n') {
                self.out.push('\n');
            }
        }
        self.out
    }
}

/// Renders tokens on one line (no indentation).
fn render(toks: &[Tok]) -> String {
    let mut s = String::new();
    for (i, t) in toks.iter().enumerate() {
        if i > 0 && t.space_before {
            s.push(' ');
        }
        s.push_str(&t.text);
    }
    s
}

/// A rendered line fits when its first and last physical lines are short
/// enough (multi-line strings in between don't count).
fn fits(s: &str, max: usize) -> bool {
    let first = s.split('\n').next().unwrap_or("");
    let last = s.rsplit('\n').next().unwrap_or("");
    black::str_width(first) <= max && black::str_width(last) <= max
}

fn render_indent(columns: usize, block_columns: usize, opts: &FormatOptions) -> String {
    opts.tabs.map_or_else(
        || " ".repeat(columns),
        |tabs| tabs.render(columns, block_columns),
    )
}

/// Lines containing comments inside brackets keep their physical line
/// structure; only indentation and spacing are normalized.
fn render_preserved(toks: &[Tok], level: usize, opts: &FormatOptions) -> Vec<String> {
    let base = level * opts.indent;
    let mut lines = Vec::new();
    let mut cur = " ".repeat(base);
    let mut cur_line = toks[0].src_line;
    let mut steps = Vec::new();
    let mut first_in_line = true;
    for (index, t) in toks.iter().enumerate() {
        if t.src_line != cur_line {
            lines.push(cur.trim_end().to_string());
            let count = if CLOSE.contains(&t.kind) {
                steps.len().saturating_sub(1)
            } else {
                steps.len()
            };
            cur = " ".repeat(base + steps.iter().take(count).sum::<usize>());
            cur_line = t.src_line;
            first_in_line = true;
        }
        if !first_in_line {
            if t.is_comment {
                cur.push_str("  ");
            } else if t.space_before {
                cur.push(' ');
            }
        }
        cur.push_str(&t.text);
        first_in_line = false;
        if OPEN.contains(&t.kind) {
            steps.push(continuation_step(toks, index, opts));
        } else if CLOSE.contains(&t.kind) {
            steps.pop();
        }
    }
    lines.push(cur.trim_end().to_string());
    lines
}

/// Matching bracket pairs at depth 0 of `toks` (non-empty bodies only).
fn top_level_pairs(toks: &[Tok]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut stack = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if OPEN.contains(&t.kind) {
            stack.push(i);
        } else if CLOSE.contains(&t.kind) {
            if let Some(o) = stack.pop() {
                if stack.is_empty() && i > o + 1 {
                    pairs.push((o, i));
                }
            }
        }
    }
    pairs
}

fn continuation_step(toks: &[Tok], open: usize, opts: &FormatOptions) -> usize {
    if !opts.python.indents.in_parentheses {
        return 0;
    }
    let Some(policy) = opts.continuation_indent else {
        return opts.indent;
    };
    let parameters = toks[open].kind == L_PAREN
        && toks.iter().position(|t| t.kind == L_PAREN) == Some(open)
        && toks.iter().take(2).any(|t| t.kind == DEF_KW);
    let arguments = toks[open].kind == L_PAREN
        && open > 0
        && matches!(toks[open - 1].kind, IDENT | R_PAREN | R_BRACK | R_BRACE);
    policy.step(parameters, arguments, opts.indent)
}

/// Black-style split of an over-long (or magic-comma) line: content inside
/// brackets is split at delimiters first (commas, then comprehension
/// keywords, ternaries, boolean/comparison/arithmetic operators), otherwise
/// the rightmost bracket is exploded ("right-hand split").
fn split_line(toks: &[Tok], indent_cols: usize, opts: &FormatOptions, inside: bool) -> Vec<String> {
    let pad = " ".repeat(indent_cols);
    let one = format!("{pad}{}", render(toks));
    let has_magic = toks.iter().any(|t| {
        (t.magic || python_style::wrap(t, opts).mode == "always") && OPEN.contains(&t.kind)
    });
    if fits(&one, opts.max_line_length) && !has_magic {
        return vec![one];
    }
    if inside {
        if let Some(lines) = delimiter_split(toks, indent_cols, opts, false) {
            return lines;
        }
    }
    let pairs = top_level_pairs(toks);
    if pairs.is_empty() {
        if !inside {
            if let Some(wrapped) = wrap_unbracketed(toks, opts) {
                return split_line(&wrapped, indent_cols, opts, false);
            }
        }
        return vec![one];
    }
    // Prefer a magic-comma bracket, otherwise the rightmost bracket whose head fits.
    let (o, c) = pairs
        .iter()
        .rev()
        .find(|(o, _)| toks[*o].magic)
        .copied()
        .or_else(|| {
            pairs
                .iter()
                .rev()
                .find(|(o, _)| {
                    fits(
                        &format!("{pad}{}", render(&toks[..=*o])),
                        opts.max_line_length,
                    )
                })
                .copied()
        })
        .unwrap_or(*pairs.last().unwrap());
    let rule = python_style::wrap(&toks[o], opts);
    if rule.mode == "never" {
        return vec![one];
    }
    let head = format!("{pad}{}", render(&toks[..=o]));
    let body = &toks[o + 1..c];
    let tail = &toks[c..];
    let inner = if rule.align {
        head.chars().count().min(4096)
    } else {
        indent_cols + continuation_step(toks, o, opts)
    };
    let mut lines = vec![head];
    let body_has_magic = body.iter().any(|t| t.magic && OPEN.contains(&t.kind));
    let body_toks = if toks[o].comma_ok && body.last().map(|t| t.kind == COMMA).unwrap_or(false) {
        &body[..body.len() - 1]
    } else {
        body
    };
    let body_line = format!("{}{}", " ".repeat(inner), render(body_toks));
    if !toks[o].magic
        && !body_has_magic
        && !matches!(rule.mode.as_str(), "always" | "chop")
        && fits(&body_line, opts.max_line_length)
    {
        lines.push(body_line);
    } else if let Some(body_lines) = delimiter_split(body, inner, opts, toks[o].comma_ok) {
        lines.extend(body_lines);
    } else {
        lines.extend(split_line(body, inner, opts, true));
    }
    // The closing bracket and whatever follows it (`) -> int:`, `).method(...)`).
    if !rule.newline_after_open && lines.len() > 1 {
        let first = lines.remove(1);
        if body.first().is_some_and(|token| token.space_before) {
            lines[0].push(' ');
        }
        lines[0].push_str(first.trim_start());
    }
    let closing_pad = if opts.python.indents.hanging_closing {
        " ".repeat(inner)
    } else {
        pad.clone()
    };
    let tail_line = format!("{closing_pad}{}", render(tail));
    if !rule.closing_on_newline && !lines.is_empty() {
        if tail.first().is_some_and(|token| token.space_before) {
            lines.last_mut().unwrap().push(' ');
        }
        lines.last_mut().unwrap().push_str(tail_line.trim_start());
        return lines;
    }
    if fits(&tail_line, opts.max_line_length) || tail.len() <= 1 {
        lines.push(tail_line);
    } else {
        lines.extend(split_line(tail, indent_cols, opts, inside));
    }
    lines
}

/// Give a long free expression an implicit-continuation context rather than
/// leaving every binary/Boolean line unwrapped outside existing brackets.
fn wrap_unbracketed(tokens: &[Tok], options: &FormatOptions) -> Option<Vec<Tok>> {
    let mut start = 0;
    let mut end = tokens.len();
    if matches!(tokens.first()?.kind, IF_KW | ELIF_KW | WHILE_KW) {
        start = 1;
        if tokens.last()?.kind == COLON {
            end -= 1;
        }
    } else if tokens.first()?.kind == RETURN_KW {
        start = 1;
    } else if let Some(index) = tokens
        .iter()
        .position(|token| token.kind == EQ || token.kind.is_augmented_assign_op())
    {
        start = index + 1;
    } else if !tokens.iter().any(|token| {
        matches!(
            token.parent,
            BINARY_EXPR | BOOL_OP_EXPR | COMPARE_OP | CONDITIONAL_EXPR
        )
    }) {
        return None;
    }
    if start >= end || tokens[start..end].iter().any(|token| token.is_comment) {
        return None;
    }
    if split_delimiters(&tokens[start..end], options).len() < 2 {
        return None;
    }
    let mut opening = tokens[start].clone();
    opening.kind = L_PAREN;
    opening.parent = PAREN_EXPR;
    opening.text = "(".into();
    opening.magic = false;
    opening.prefix = false;
    opening.comma_ok = false;
    let mut closing = opening.clone();
    closing.kind = R_PAREN;
    closing.text = ")".into();
    let mut wrapped = tokens[..start].to_vec();
    wrapped.push(opening);
    wrapped.extend_from_slice(&tokens[start..end]);
    wrapped.push(closing);
    wrapped.extend_from_slice(&tokens[end..]);
    Some(prepare_line(wrapped, options))
}

/// Splits bracket contents one element per line: at depth-0 commas (adding
/// a trailing comma where the grammar allows it), else at the loosest
/// delimiter class present. `None` when there is nothing to split at.
fn delimiter_split(
    body: &[Tok],
    indent_cols: usize,
    opts: &FormatOptions,
    comma_ok: bool,
) -> Option<Vec<String>> {
    let elements = split_elements(body);
    let ends_with_comma = body.last().map(|t| t.kind == COMMA).unwrap_or(false);
    if elements.len() > 1 || ends_with_comma {
        let n = elements.len();
        let mut lines = Vec::new();
        for (i, el) in elements.iter().enumerate() {
            let had_comma = el.last().map(|t| t.kind == COMMA).unwrap_or(false);
            let core = if had_comma { &el[..el.len() - 1] } else { el };
            if core.is_empty() {
                continue;
            }
            let mut el_lines = split_line(core, indent_cols, opts, true);
            if had_comma || i + 1 < n || (comma_ok && opts.python.multiline_trailing_comma) {
                el_lines.last_mut().unwrap().push(',');
            }
            lines.extend(el_lines);
        }
        return Some(lines);
    }
    let chunks = split_delimiters(body, opts);
    if chunks.len() > 1 {
        return Some(
            chunks
                .into_iter()
                .flat_map(|c| split_line(c, indent_cols, opts, true))
                .collect(),
        );
    }
    None
}

/// Black's delimiter priorities (lower splits first), for a split *before* `t`.
fn delimiter_rank(prev: Option<&Tok>, t: &Tok) -> Option<u8> {
    let prev_kind = prev.map(|p| p.kind);
    let prev_parent = prev.map(|p| p.parent);
    match t.kind {
        // Comprehension clauses: `for` / `async for` / `if`.
        FOR_KW if t.parent == COMP_FOR && prev_kind != Some(ASYNC_KW) => Some(0),
        ASYNC_KW if t.parent == COMP_FOR => Some(0),
        IF_KW if t.parent == COMP_IF => Some(0),
        // Ternary.
        IF_KW | ELSE_KW if t.parent == CONDITIONAL_EXPR => Some(1),
        // Boolean operators.
        AND_KW | OR_KW if t.parent == BOOL_OP_EXPR => Some(2),
        // Implicit string concatenation.
        STRING if prev_kind == Some(STRING) => Some(3),
        // Comparisons (`not in` / `is not` split before their first token).
        _ if t.parent == COMPARE_OP && prev_parent != Some(COMPARE_OP) => Some(4),
        PIPE | CARET | AMP | SHL | SHR if t.parent == BINARY_EXPR => Some(5),
        PLUS | MINUS if t.parent == BINARY_EXPR => Some(6),
        STAR | SLASH | SLASH_SLASH | PERCENT | AT if t.parent == BINARY_EXPR => Some(7),
        STAR_STAR if t.parent == BINARY_EXPR => Some(8),
        _ => None,
    }
}

/// Splits `body` before every depth-0 delimiter of the loosest class present.
fn split_delimiters<'a>(body: &'a [Tok], opts: &FormatOptions) -> Vec<&'a [Tok]> {
    let mut depth = 0usize;
    let mut positions: Vec<(usize, u8)> = Vec::new();
    for (i, t) in body.iter().enumerate() {
        if OPEN.contains(&t.kind) {
            depth += 1;
            continue;
        }
        if CLOSE.contains(&t.kind) {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth != 0 || i == 0 {
            continue;
        }
        if let Some(rank) = delimiter_rank(body.get(i - 1), t) {
            let after = opts.python.space("operator_at_line_end", false);
            let point = if after
                && matches!(
                    t.kind,
                    PLUS | MINUS
                        | STAR
                        | SLASH
                        | SLASH_SLASH
                        | PIPE
                        | AMP
                        | CARET
                        | SHL
                        | SHR
                        | STAR_STAR
                        | AND_KW
                        | OR_KW
                ) {
                i + 1
            } else {
                i
            };
            if point < body.len() {
                positions.push((point, rank));
            }
        }
    }
    let Some(best) = positions.iter().map(|(_, r)| *r).min() else {
        return vec![body];
    };
    let mut chunks = Vec::new();
    let mut start = 0;
    for (i, r) in positions {
        if r == best && i > start {
            chunks.push(&body[start..i]);
            start = i;
        }
    }
    chunks.push(&body[start..]);
    chunks
}

/// Splits bracket contents at depth-0 commas, keeping each comma with its
/// element (commas in a comprehension's target list do not separate elements).
fn split_elements(body: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut in_comprehension = false;
    for (i, t) in body.iter().enumerate() {
        if OPEN.contains(&t.kind) {
            depth += 1;
        } else if CLOSE.contains(&t.kind) {
            depth = depth.saturating_sub(1);
        } else if t.kind == FOR_KW && depth == 0 {
            in_comprehension = true;
        } else if t.kind == COMMA && depth == 0 && !in_comprehension {
            out.push(&body[start..=i]);
            start = i + 1;
        }
    }
    if start < body.len() {
        out.push(&body[start..]);
    }
    out
}

/// Code tokens with normalized texts (strings decoded, numbers normalized),
/// used to verify that formatting preserves the program.
pub fn significant_tokens(src: &str) -> Vec<(SyntaxKind, String)> {
    let (tokens, _) = rustpy_syntax::tokenize(src);
    let all: Vec<(SyntaxKind, String)> = tokens
        .iter()
        .filter(|t| {
            !matches!(
                t.kind,
                WHITESPACE | NL | COMMENT | NEWLINE | INDENT | DEDENT | SEMICOLON
            )
        })
        .map(|t| {
            let text = &src[t.range];
            let norm = match t.kind {
                STRING => decode_string_literal(text),
                INT_NUMBER | FLOAT_NUMBER | COMPLEX_NUMBER => normalize_number(text),
                _ => text.to_string(),
            };
            (t.kind, norm)
        })
        .collect();
    // Trailing commas before a closing bracket are optional syntax (the
    // formatter adds and removes them); one-tuples are covered by unit tests.
    let mut out = Vec::with_capacity(all.len());
    for (i, tok) in all.iter().enumerate() {
        if tok.0 == COMMA
            && all
                .get(i + 1)
                .map(|n| CLOSE.contains(&n.0))
                .unwrap_or(false)
        {
            continue;
        }
        out.push(tok.clone());
    }
    out
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod range_tests {
    use super::*;

    #[test]
    fn selection_printer_keeps_unicode_comments_crlf_and_untouched_spacing() {
        let source = "# header π🙂\r\nx=1  # untouched\r\ny=2 # selected λ\r\nz=3\r\n";
        let start = source.find("y=2").unwrap() as u32;
        let edits = format_range(
            source,
            TextRange::new(start.into(), (start + 3).into()),
            &FormatOptions::pep8(),
        )
        .unwrap();
        let output = rustpy_syntax::printer::apply_text_edits(source, &edits).unwrap();
        assert_eq!(
            output,
            "# header π🙂\r\nx=1  # untouched\r\ny = 2  # selected λ\r\nz=3\r\n"
        );
        assert!(rustpy_syntax::parse(&output).ok());
    }

    #[test]
    fn reformat_selection_changes_only_the_selected_lines() {
        let src = "x=1\ny=2\n\n\n\n\ndef f( a ):\n    return a\n";
        let opts = FormatOptions::pep8();
        // Select line 2 (`y=2`) only.
        let start = src.find("y=2").unwrap() as u32;
        let edits = format_range(
            src,
            TextRange::new(TextSize::new(start), TextSize::new(start + 3)),
            &opts,
        )
        .unwrap();
        let mut out = src.to_string();
        for (r, t) in edits.iter().rev() {
            out.replace_range(usize::from(r.start())..usize::from(r.end()), t);
        }
        assert!(out.starts_with("x=1\ny = 2\n"), "{out:?}");
        assert!(
            out.contains("def f( a ):"),
            "outside the selection is untouched: {out:?}"
        );
        // The whole file: everything.
        let all = format_range(
            src,
            TextRange::new(TextSize::new(0), TextSize::new(src.len() as u32)),
            &opts,
        )
        .unwrap();
        assert_eq!(all.len(), 1);
    }
}

#[cfg(test)]
mod continuation_policy_tests {
    use super::*;
    fn options(arguments: bool, collections: bool) -> FormatOptions {
        FormatOptions {
            indent: 2,
            continuation_indent: Some(ContinuationIndent {
                width: 6,
                parameters: true,
                arguments,
                collections,
            }),
            ..FormatOptions::pep8()
        }
    }
    #[test]
    fn actual_formatter_parameter_policy_preserves_suite_indent_and_fingerprint() {
        let source = "def f(alpha, beta,):\n    pass\n";
        let formatted = format_source(source, &options(false, false)).unwrap();
        assert_eq!(formatted, "def f(\n      alpha,\n      beta,\n):\n  pass\n");
        assert_eq!(
            semantic_fingerprint(source),
            semantic_fingerprint(&formatted)
        );
        assert_eq!(
            format_source(&formatted, &options(false, false)).unwrap(),
            formatted
        );
    }
    #[test]
    fn actual_formatter_calls_collections_and_preserved_comments_use_correct_steps() {
        for (source, fragment) in [
            ("value = f(alpha, beta,)\n", "\n      alpha,"),
            ("value = [alpha, beta,]\n", "\n      alpha,"),
            (
                "def f(\n    alpha,  # π🙂\n):\n    pass\n",
                "\n      alpha,  # π🙂",
            ),
        ] {
            let out = format_source(source, &options(true, true)).unwrap();
            assert!(out.contains(fragment), "{out}");
            assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&out));
        }
        let out = format_source("value = f(alpha, beta,)\n", &options(false, false)).unwrap();
        assert!(out.contains("\n  alpha,"), "{out}");
    }
}

#[cfg(test)]
mod smart_tabs_tests {
    use super::*;
    #[test]
    fn tabs_distinguish_block_columns_from_alignment() {
        assert_eq!(
            TabIndent {
                width: 4,
                smart: true
            }
            .render(10, 4),
            "\t      "
        );
        assert_eq!(
            TabIndent {
                width: 4,
                smart: false
            }
            .render(10, 4),
            "\t\t  "
        );
        assert_eq!(
            TabIndent {
                width: 4,
                smart: true
            }
            .render(10, 0),
            "          "
        );
    }
    #[test]
    fn actual_formatter_uses_tabs_for_suites_and_spaces_for_smart_alignment() {
        let source = "def f():\n    return call(alpha, beta,)\n";
        for smart in [false, true] {
            let options = FormatOptions {
                tabs: Some(TabIndent { width: 4, smart }),
                continuation_indent: Some(ContinuationIndent {
                    width: 4,
                    parameters: true,
                    arguments: true,
                    collections: true,
                }),
                ..FormatOptions::pep8()
            };
            let out = format_source(source, &options).unwrap();
            let inner = if smart { "\t    " } else { "\t\t" };
            assert_eq!(
                out,
                format!("def f():\n\treturn call(\n{inner}alpha,\n{inner}beta,\n\t)\n")
            );
            assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&out));
            assert_eq!(format_source(&out, &options).unwrap(), out);
        }
    }
    #[test]
    fn tab_formatting_keeps_multiline_string_content_and_control_regions_exact() {
        let source = "def f():\n    text = \"\"\"π\n  keep\t🙂\n    \"\"\"\n    return text\n\n# fmt: off\nuntidy = [ 1,2 ]\n# fmt: on\n";
        let options = FormatOptions {
            tabs: Some(TabIndent {
                width: 4,
                smart: true,
            }),
            ..FormatOptions::pep8()
        };
        let out = format_source(source, &options).unwrap();
        assert!(out.contains("π\n  keep\t🙂\n    \"\"\""), "{out}");
        assert!(
            out.contains("# fmt: off\nuntidy = [ 1,2 ]\n# fmt: on\n"),
            "{out}"
        );
        assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&out));
    }
}

#[cfg(test)]
mod formatter_control_tags_tests {
    use super::*;
    fn options(off: &str, on: &str) -> FormatOptions {
        FormatOptions {
            formatter_tags: Some(FormatterTags {
                off: off.into(),
                on: on.into(),
            }),
            ..FormatOptions::pep8()
        }
    }
    #[test]
    fn configured_default_and_custom_comment_regions_are_byte_preserved() {
        for (off, on) in [("@formatter:off", "@formatter:on"), ("KEEP π", "RESUME π")] {
            let source = format!("before=1\n# {off}\nuntidy = [ 1,2 ]\n# {on}\nafter=2\n");
            let out = format_source(&source, &options(off, on)).unwrap();
            assert_eq!(
                out,
                format!("before = 1\n# {off}\nuntidy = [ 1,2 ]\n# {on}\nafter = 2\n")
            );
            assert_eq!(semantic_fingerprint(&source), semantic_fingerprint(&out));
            assert_eq!(format_source(&out, &options(off, on)).unwrap(), out);
        }
    }
    #[test]
    fn strings_disabled_empty_and_unterminated_markers_obey_authority() {
        let source = "text = '@formatter:off'\ny=2\n";
        assert!(!options("@formatter:off", "@formatter:on")
            .formatter_tags
            .unwrap()
            .has_start(source));
        assert!(
            format_source(source, &options("@formatter:off", "@formatter:on"))
                .unwrap()
                .contains("y = 2\n")
        );
        let controlled = "# KEEP\nuntidy = [ 1,2 ]\n# RESUME\n";
        assert!(format_source(controlled, &FormatOptions::pep8())
            .unwrap()
            .contains("untidy = [1, 2]"));
        assert!(format_source(controlled, &options("", "RESUME"))
            .unwrap()
            .contains("untidy = [1, 2]"));
        for tail in ["", "\n\n"] {
            let source = format!("before=1\n# KEEP\nuntidy = [ 1,2 ]{tail}");
            assert_eq!(
                format_source(&source, &options("KEEP", "RESUME")).unwrap(),
                format!("before = 1\n# KEEP\nuntidy = [ 1,2 ]{tail}")
            );
        }
    }
    #[test]
    fn inline_bracket_and_one_line_suite_markers_protect_whole_logical_statement() {
        for source in [
            "before=1 # KEEP\nuntidy = [ 1,2 ]\n# RESUME\nafter=2\n",
            "values = [ # KEEP\n  1,2, # RESUME\n]\nafter=2\n",
            "def f(): value=1 # KEEP\n# RESUME\nafter=2\n",
        ] {
            let out = format_source(source, &options("KEEP", "RESUME")).unwrap();
            let prefix = source.split("after=2").next().unwrap();
            assert!(out.starts_with(prefix), "{out}");
            assert!(out.ends_with("after = 2\n"), "{out}");
            assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&out));
        }
    }
    #[test]
    fn black_and_custom_controls_remain_independently_active() {
        let source = "# fmt: off\none= 1\n# KEEP\n# fmt: on\ntwo= 2\n# RESUME\nafter=3\n";
        let out = format_source(source, &options("KEEP", "RESUME")).unwrap();
        assert_eq!(
            out,
            "# fmt: off\none= 1\n# KEEP\n# fmt: on\ntwo= 2\n# RESUME\nafter = 3\n"
        );
    }
    #[test]
    fn selected_range_does_not_mutate_protected_region_or_other_unselected_bytes() {
        let source = "before=1\n# KEEP\nuntidy = [ 1,2 ]\n# RESUME\nafter=2\n";
        let start = source.find("untidy").unwrap();
        let end = source.find("# RESUME").unwrap();
        assert!(format_range(
            source,
            TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
            &options("KEEP", "RESUME")
        )
        .unwrap()
        .is_empty());
    }
}
