//! Incremental reparsing (roadmap §2 "Incremental re-parse on edit").
//!
//! [`reparse`] turns the parse of `old_text` into the parse of `new_text`
//! after one edit, reusing every subtree the edit cannot affect. The result is
//! always identical to `parse(new_text)`: same green tree (trivia included)
//! and same diagnostics, in the same order. The tests in `reparse/tests.rs`
//! check this against full parses under tens of thousands of exhaustive and
//! pseudo-random edits of real files. [`reparse_with_kind`] also reports
//! which of three strategies produced the result:
//!
//! 1. [`ReparseKind::Token`]: the edit lies inside one identifier, number,
//!    string, comment or mid-line whitespace token, and the new text relexes
//!    as one token of the same kind that cannot merge with its neighbours.
//!    Only that leaf is replaced (`SyntaxToken::replace_with`). The parser
//!    looks at token kinds only, plus the text of a few soft keywords and
//!    conversion letters (those are excluded), so the tree shape cannot change.
//! 2. [`ReparseKind::Block`]: a run of whole statements of the innermost
//!    statement list around the edit (an indented `BLOCK`, else the `MODULE`)
//!    is relexed and reparsed alone with that list's own statement loop, then
//!    spliced into the list. The run starts and ends at statements that begin
//!    a line at the list's indentation. It grows when a bracket, string or
//!    clause continues past its end, or when the statement before it depends
//!    on the edit; failing that, the enclosing lists are tried. The private
//!    `reparse_run` and `attempt` document why the result is exact.
//! 3. [`ReparseKind::Full`]: everything else, e.g. an edit that opens a
//!    string or bracket running to the end of the file, or one that changes
//!    how the enclosing blocks are indented, and runs that would span the
//!    whole module (or most of a large file) anyway.

use std::sync::atomic::{AtomicU64, Ordering};

use rowan::{GreenNodeData, GreenToken, GreenTokenData, NodeOrToken, TokenAtOffset};
use rustpy_core::{Diagnostic, TextRange, TextSize};

use crate::lexer::{tokenize, Token};
use crate::parser::{
    parse, parse_statement_run, Parse, RunContext, RunMismatch, StatementList, BLOCK_COST,
};
use crate::syntax_kind::SyntaxKind::{self, *};
use crate::syntax_kind::{SyntaxNode, SyntaxToken};

#[cfg(test)]
mod tests;

/// Which strategy [`reparse_with_kind`] used.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ReparseKind {
    /// One token was relexed and replaced in place (also reported for no-op edits).
    Token,
    /// A run of whole statements of one block (or of the module) was reparsed and spliced in.
    Block,
    /// The whole text was parsed from scratch.
    Full,
}

/// Process-wide counts of the strategies taken by [`reparse`] / [`reparse_with_kind`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ReparseStats {
    pub token: u64,
    pub block: u64,
    pub full: u64,
}

static TOKEN_COUNT: AtomicU64 = AtomicU64::new(0);
static BLOCK_COUNT: AtomicU64 = AtomicU64::new(0);
static FULL_COUNT: AtomicU64 = AtomicU64::new(0);

/// Snapshot of how often each [`ReparseKind`] was taken since program start.
pub fn reparse_stats() -> ReparseStats {
    ReparseStats {
        token: TOKEN_COUNT.load(Ordering::Relaxed),
        block: BLOCK_COUNT.load(Ordering::Relaxed),
        full: FULL_COUNT.load(Ordering::Relaxed),
    }
}

/// Reparses after replacing `edit_range` of `old_text` (whose parse is `old`)
/// by `inserted`, giving `new_text`. The result equals `parse(new_text)`.
///
/// `old` must come from [`parse`] or `reparse` of `old_text`. Inconsistent
/// arguments that are cheap to detect (lengths, bounds, the inserted slice)
/// fall back to a full parse of `new_text`. For several edits, call once per
/// edit in order, or merge them into one covering edit (see [`diff_edit`]).
pub fn reparse(
    old: &Parse,
    old_text: &str,
    edit_range: TextRange,
    inserted: &str,
    new_text: &str,
) -> Parse {
    reparse_with_kind(old, old_text, edit_range, inserted, new_text).0
}

/// Like [`reparse`], also telling which strategy was taken (for logging).
pub fn reparse_with_kind(
    old: &Parse,
    old_text: &str,
    edit_range: TextRange,
    inserted: &str,
    new_text: &str,
) -> (Parse, ReparseKind) {
    let incremental = Edit::new(old, old_text, edit_range, inserted, new_text).and_then(|edit| {
        if edit.deleted() == inserted {
            return Some((old.clone(), ReparseKind::Token));
        }
        reparse_token(old, &edit)
            .map(|p| (p, ReparseKind::Token))
            .or_else(|| reparse_statements(old, &edit).map(|p| (p, ReparseKind::Block)))
    });
    let (parse, kind) = incremental.unwrap_or_else(|| (parse(new_text), ReparseKind::Full));
    debug_assert_eq!(usize::from(parse.green().text_len()), new_text.len());
    let counter = match kind {
        ReparseKind::Token => &TOKEN_COUNT,
        ReparseKind::Block => &BLOCK_COUNT,
        ReparseKind::Full => &FULL_COUNT,
    };
    counter.fetch_add(1, Ordering::Relaxed);
    (parse, kind)
}

/// [`reparse_with_kind`] for callers that only have both texts. The edit is
/// derived with [`diff_edit`], a linear `memcmp` that is far cheaper than parsing.
pub fn reparse_text(old: &Parse, old_text: &str, new_text: &str) -> (Parse, ReparseKind) {
    let (range, inserted) = diff_edit(old_text, new_text);
    reparse_with_kind(old, old_text, range, inserted, new_text)
}

/// The single edit turning `old_text` into `new_text`: the range of
/// `old_text` between their longest common prefix and suffix, and the text of
/// `new_text` that replaces it. Boundaries never split a UTF-8 character.
pub fn diff_edit<'a>(old_text: &str, new_text: &'a str) -> (TextRange, &'a str) {
    let (a, b) = (old_text.as_bytes(), new_text.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !old_text.is_char_boundary(prefix) || !new_text.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let max_suffix = a.len().min(b.len()) - prefix;
    let mut suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    while !old_text.is_char_boundary(a.len() - suffix)
        || !new_text.is_char_boundary(b.len() - suffix)
    {
        suffix -= 1;
    }
    (
        TextRange::new(offset(prefix), offset(a.len() - suffix)),
        &new_text[prefix..b.len() - suffix],
    )
}

// ---------------------------------------------------------------------------
// Edit bookkeeping

/// A validated edit: `old_text[start..old_end]` was replaced by `inserted`.
struct Edit<'a> {
    old_text: &'a str,
    new_text: &'a str,
    start: usize,
    old_end: usize,
    inserted: &'a str,
}

impl<'a> Edit<'a> {
    fn new(
        old: &Parse,
        old_text: &'a str,
        range: TextRange,
        inserted: &'a str,
        new_text: &'a str,
    ) -> Option<Edit<'a>> {
        let (start, old_end) = (usize::from(range.start()), usize::from(range.end()));
        let consistent = old_end <= old_text.len()
            && old_text.is_char_boundary(start)
            && old_text.is_char_boundary(old_end)
            && usize::from(old.green().text_len()) == old_text.len()
            && new_text.len() == old_text.len() - (old_end - start) + inserted.len()
            && new_text.get(start..start + inserted.len()) == Some(inserted);
        if !consistent {
            return None;
        }
        debug_assert!(
            new_text[..start] == old_text[..start]
                && new_text[start + inserted.len()..] == old_text[old_end..],
            "new_text must be old_text with the edit applied"
        );
        Some(Edit {
            old_text,
            new_text,
            start,
            old_end,
            inserted,
        })
    }

    fn range(&self) -> TextRange {
        TextRange::new(offset(self.start), offset(self.old_end))
    }

    fn deleted(&self) -> &'a str {
        &self.old_text[self.start..self.old_end]
    }

    /// Maps an old offset at or after the edit's end into `new_text`.
    fn to_new(&self, old_offset: usize) -> usize {
        debug_assert!(old_offset >= self.old_end);
        old_offset - self.old_end + self.start + self.inserted.len()
    }

    /// An old diagnostic located after the edit, moved into `new_text`.
    fn shifted(&self, d: &Diagnostic) -> Diagnostic {
        let (s, e) = (usize::from(d.range.start()), usize::from(d.range.end()));
        Diagnostic {
            range: TextRange::new(offset(self.to_new(s)), offset(self.to_new(e))),
            ..d.clone()
        }
    }
}

fn offset(n: usize) -> TextSize {
    TextSize::new(u32::try_from(n).expect("text larger than 4 GiB"))
}

fn kind_of(raw: rowan::SyntaxKind) -> SyntaxKind {
    SyntaxKind::from_raw(raw.0)
}

/// The kind the lexer gives a token that the parser may have re-tagged
/// (soft keywords are lexed as `IDENT`).
fn lexed_kind(kind: SyntaxKind) -> SyntaxKind {
    match kind {
        MATCH_KW | CASE_KW | TYPE_KW | UNDERSCORE_KW => IDENT,
        k => k,
    }
}

/// Lexes `text` alone: `Some((kind, has_errors))` if it is exactly one token
/// (the zero-width `NEWLINE`/`DEDENT`s the lexer appends at EOF aside).
fn single_token(text: &str) -> Option<(SyntaxKind, bool)> {
    let (tokens, diags) = tokenize(text);
    let mut real = tokens
        .iter()
        .filter(|t| !(t.range.is_empty() && matches!(t.kind, NEWLINE | DEDENT)));
    let first = real.next()?;
    (real.next().is_none() && first.range == TextRange::up_to(TextSize::of(text)))
        .then_some((first.kind, !diags.is_empty()))
}

/// True if some diagnostic of the sorted list `errors` starts in `lo..=hi`.
fn any_error_starting_in(errors: &[Diagnostic], lo: TextSize, hi: TextSize) -> bool {
    let i = errors.partition_point(|e| e.range.start() < lo);
    errors.get(i).is_some_and(|e| e.range.start() <= hi)
}

// ---------------------------------------------------------------------------
// Strategy 1: relex a single token

/// Identifier texts the parser (or lexer recovery) inspects: soft keywords and
/// f-string conversion letters. Changing an identifier to or from one of these
/// may change the tree, so such edits are not handled at token level.
const TEXT_SENSITIVE_IDENTS: &[&str] = &["match", "case", "type", "_", "r", "s", "a"];

fn reparse_token(old: &Parse, edit: &Edit<'_>) -> Option<Parse> {
    // Unbalanced brackets: the lexer's recovery reads words and indentation
    // anywhere in the file, so even a one-token edit can change it.
    if old.errors().iter().any(|e| is_bracket_error(&e.message)) {
        return None;
    }
    let root = old.syntax_node();
    let range = edit.range();
    let candidates = if range.is_empty() {
        match root.token_at_offset(range.start()) {
            TokenAtOffset::None => Vec::new(),
            TokenAtOffset::Single(t) => vec![t],
            TokenAtOffset::Between(left, right) => vec![left, right],
        }
    } else {
        match root.covering_element(range) {
            NodeOrToken::Token(t) => vec![t],
            NodeOrToken::Node(_) => Vec::new(),
        }
    };
    candidates.iter().find_map(|t| relex_token(old, edit, t))
}

fn relex_token(old: &Parse, edit: &Edit<'_>, token: &SyntaxToken) -> Option<Parse> {
    let kind = token.kind();
    let number = matches!(kind, INT_NUMBER | FLOAT_NUMBER | COMPLEX_NUMBER);
    if !(number || matches!(kind, IDENT | STRING | COMMENT | WHITESPACE)) {
        return None;
    }
    let range = token.text_range();
    let (start, end) = (usize::from(range.start()), usize::from(range.end()));
    if edit.start < start || edit.old_end > end {
        return None;
    }
    let old_tok = token.text();
    let new_tok = format!(
        "{}{}{}",
        &old_tok[..edit.start - start],
        edit.inserted,
        &old_tok[edit.old_end - start..]
    );
    match kind {
        IDENT
            if TEXT_SENSITIVE_IDENTS.contains(&old_tok)
                || TEXT_SENSITIVE_IDENTS.contains(&new_tok.as_str()) =>
        {
            return None
        }
        // Quotes, newlines and escapes can re-pair string delimiters.
        STRING
            if [edit.deleted(), edit.inserted]
                .iter()
                .any(|s| s.contains(['\'', '"', '\n', '\r', '\\'])) =>
        {
            return None
        }
        WHITESPACE => {
            // Indentation (and anything after a line continuation) is structural.
            let at_line_start =
                start == 0 || matches!(edit.old_text.as_bytes()[start - 1], b'\n' | b'\r');
            let blanks = |s: &str| !s.is_empty() && s.bytes().all(|b| b == b' ' || b == b'\t');
            if at_line_start || !blanks(old_tok) || !blanks(&new_tok) {
                return None;
            }
        }
        _ => {}
    }
    // One token of the same kind, without lexer errors…
    if single_token(&new_tok) != Some((kind, false)) {
        return None;
    }
    // …that cannot extend into the following character (`b` + `"…"`, `1` + `e5`)…
    let new_end = edit.to_new(end);
    if let Some(next) = edit.new_text[new_end..].chars().next() {
        if number && (next.is_alphanumeric() || next == '_' || next == '.') {
            return None;
        }
        let mut joined = new_tok.clone();
        joined.push(next);
        if single_token(&joined).is_some() {
            return None;
        }
    }
    // …and, when it grows at the front, cannot be absorbed by the previous token.
    if edit.start == start && start > 0 && !matches!(kind, COMMENT | WHITESPACE) {
        let prev = edit.old_text[..start].chars().next_back()?;
        // A quote right before a string can pair up with its opening quote
        // (`''` followed by `'x'` lexes as the triple-quoted `'''x'...`).
        if prev.is_alphanumeric() || matches!(prev, '_' | '.' | '\\' | '\'' | '"') {
            return None;
        }
    }
    // Diagnostics touching the token may depend on its extent: leave them to a reparse.
    if old
        .errors()
        .iter()
        .any(|e| e.range.start() <= range.end() && e.range.end() >= range.start())
    {
        return None;
    }
    let green = token.replace_with(GreenToken::new(kind.into(), &new_tok));
    let errors = old
        .errors()
        .iter()
        .map(|e| {
            if e.range.start() > range.end() {
                edit.shifted(e)
            } else {
                e.clone()
            }
        })
        .collect();
    Some(Parse::new(green, errors))
}

// ---------------------------------------------------------------------------
// Strategy 2: reparse a run of statements of one statement list

/// Outcome of trying one statement list.
enum Run {
    Done(Parse),
    /// Not applicable to this list; an enclosing list may still work.
    Skip,
    /// Give up incremental reparsing (a full parse is as cheap).
    Abandon,
}

/// Outcome of reparsing one choice of run boundaries.
enum Attempt {
    Done(Parse),
    /// The start boundary does not work; an earlier one may.
    EarlierStart,
    /// The end boundary does not work; a later one may.
    LaterEnd,
    Skip,
    Abandon,
}

/// Runs larger than this, and larger than half the file, are not worth it.
const MIN_ABANDON_BYTES: usize = 64 * 1024;

/// Boundary choices tried per statement list before giving up on it.
const MAX_ATTEMPTS: usize = 8;

fn reparse_statements(old: &Parse, edit: &Edit<'_>) -> Option<Parse> {
    // State a statement run cannot reconstruct from the tree: trees flattened
    // by the depth limits, `<>` after `from __future__ import barry_as_FLUFL`,
    // and the lexer's recovery from brackets that are never closed (it looks
    // at the first word and indentation of later lines across the file).
    // Over-indented lines (and dedents to no outer level) leave levels on the
    // lexer's indentation stack that no BLOCK shows, so a run lexed on its own
    // could see different INDENT/DEDENT tokens.
    if old.errors().iter().any(|e| {
        e.message.starts_with("too many nested")
            || e.message.starts_with("unexpected indent")
            || e.message.starts_with("unindent does not match")
    }) || edit.new_text.contains("barry_as_FLUFL")
    {
        return None;
    }
    if old.errors().iter().any(|e| is_bracket_error(&e.message)) {
        return None;
    }
    let root = old.syntax_node();
    let mut node = match root.covering_element(edit.range()) {
        NodeOrToken::Node(n) => Some(n),
        NodeOrToken::Token(t) => t.parent(),
    };
    while let Some(n) = node {
        if matches!(n.kind(), MODULE | BLOCK) {
            match reparse_run(old, edit, &n) {
                Run::Done(p) => return Some(p),
                Run::Abandon => return None,
                Run::Skip => {}
            }
        }
        node = n.parent();
    }
    None
}

type GreenElementRef<'a> = NodeOrToken<&'a GreenNodeData, &'a GreenTokenData>;

/// A child of a statement list where a statement starts a logical line at the
/// list's indentation. There the parser is at the head of the list's
/// statement loop and the lexer is in its line-start state (see [`reparse_run`]).
#[derive(Copy, Clone)]
struct Boundary<'a> {
    /// Child index in the list.
    idx: usize,
    /// Start of the statement (its first token).
    start: TextSize,
    /// Start of the statement's line.
    line_start: TextSize,
    /// Kind of the previous significant token (`NEWLINE`, `INDENT` or `DEDENT`;
    /// `None` at the start of the module).
    prev: Option<SyntaxKind>,
    /// The statement's first token.
    first: &'a GreenTokenData,
}

/// Where the reparsed run ends.
enum RunEnd<'a> {
    /// Before the statement `Boundary`.
    Statement(Boundary<'a>),
    /// At the closing `DEDENT` of an indented block, located at the start of
    /// the (unchanged) first statement of a later, less indented line.
    Dedent { line_start: TextSize },
    /// At the closing `DEDENT` of an indented block that runs to EOF.
    BlockEof,
    /// At the end of the module (EOF).
    ModuleEof,
}

/// A statement list (the module, or an indented block) and its children.
struct List<'g> {
    module: bool,
    kids: Vec<GreenElementRef<'g>>,
    /// Text range of each child.
    ranges: Vec<TextRange>,
    /// Index of the first statement (after a block's `NEWLINE` … `INDENT`).
    first_item: usize,
    /// Index of a block's closing `DEDENT` (`kids.len()` for the module).
    end_item: usize,
    /// A block's `INDENT` text and column.
    level: Option<(&'g str, usize)>,
}

impl<'g> List<'g> {
    /// `None` for a block that is not an indented suite (`if x: pass`).
    fn new(node: &SyntaxNode, green: &'g GreenNodeData) -> Option<List<'g>> {
        let module = node.kind() == MODULE;
        let kids: Vec<GreenElementRef<'g>> = green.children().collect();
        let mut ranges = Vec::with_capacity(kids.len());
        let mut at = node.text_range().start();
        for kid in &kids {
            let len = kid.text_len();
            ranges.push(TextRange::at(at, len));
            at += len;
        }
        // Indented block framing: NEWLINE, blank/comment lines, INDENT, items…, DEDENT.
        let (first_item, end_item, level) = if module {
            (0, kids.len(), None)
        } else {
            let indent_idx = kids
                .iter()
                .position(|k| !kind_of(k.kind()).is_trivia() && kind_of(k.kind()) != NEWLINE)?;
            let NodeOrToken::Token(indent) = kids[indent_idx] else {
                return None;
            };
            let framed = kind_of(kids[0].kind()) == NEWLINE
                && kind_of(indent.kind()) == INDENT
                && kind_of(kids[kids.len() - 1].kind()) == DEDENT
                && indent_idx + 1 < kids.len() - 1;
            let col = indent_col(indent.text()).filter(|_| framed)?;
            (indent_idx + 1, kids.len() - 1, Some((indent.text(), col)))
        };
        Some(List {
            module,
            kids,
            ranges,
            first_item,
            end_item,
            level,
        })
    }

    fn col(&self) -> usize {
        self.level.map_or(0, |(_, col)| col)
    }

    /// The run boundary at child `i`, if it is one (see [`reparse_run`]).
    fn boundary(&self, old_text: &str, i: usize) -> Option<Boundary<'g>> {
        if !matches!(self.kids[i], NodeOrToken::Node(_)) {
            return None;
        }
        let first = green_first_token(self.kids[i])?;
        if matches!(kind_of(first.kind()), INDENT | DEDENT | NEWLINE) {
            return None;
        }
        let prev = TokensBefore::new(&self.kids, i)
            .map(|t| kind_of(t.kind()))
            .find(|k| !k.is_trivia());
        match prev {
            Some(NEWLINE | INDENT | DEDENT) => {}
            None if self.module => {}
            _ => return None,
        }
        if !indentation_processed(TokensBefore::new(&self.kids, i)) {
            return None;
        }
        let start = self.ranges[i].start();
        let s = usize::from(start);
        let ls = line_start(old_text, s);
        if indent_col(&old_text[ls..s]) != Some(self.col()) {
            return None;
        }
        Some(Boundary {
            idx: i,
            start,
            line_start: offset(ls),
            prev,
            first,
        })
    }

    /// A run end at the end of the list, for an edit ending at `ee`.
    fn end_of_list(&self, old_text: &str, ee: TextSize) -> Option<RunEnd<'g>> {
        if self.module {
            return Some(RunEnd::ModuleEof);
        }
        let be = usize::from(self.ranges[self.end_item].start());
        if be == old_text.len() {
            return Some(RunEnd::BlockEof);
        }
        // The isolated lex closes the list's level (and those above it) at the
        // end of the body; the full lex does the same only on a line indented
        // less than the list. (The parser can end a block on a line at its own
        // column: unexpected-indent recovery eats one DEDENT for several levels.)
        let ls = line_start(old_text, be);
        if offset(ls) <= ee || indent_col(&old_text[ls..be]).is_none_or(|col| col >= self.col()) {
            return None;
        }
        Some(RunEnd::Dedent {
            line_start: offset(ls),
        })
    }
}

/// Reparses a run of statements `list.children()[k..j]` around the edit.
///
/// The run starts at a [`Boundary`] `k` before the edit and ends at a later
/// boundary `j` (or at the end of the list) after the edit. The edit touches
/// neither the first character of `k` (nor its indentation) nor the line of
/// `j`. A boundary is a statement node whose first token is the first token
/// of a line, has the list's indentation, and follows a `NEWLINE`, `INDENT` or
/// `DEDENT`. Its line must also be one whose indentation the lexer processed,
/// so not one following a backslash continuation. Then, as for the old text,
/// so for the new one:
///
/// * the parser is at the head of the list's statement loop. Unexpected-indent
///   recovery loops only see deeper lines, because a line at the list's
///   column pops their level first. A `DEDENT` reaching the loop head would
///   have ended the list.
/// * the lexer is in its line-start state: bottom mode, no open bracket,
///   `current_indent` and the top of the indentation stack are the list's
///   level, pushed by the list's own `INDENT`, whose text we reuse. A block
///   ends no later than its level is popped, so that level is on the stack.
///   Lower levels only matter on lines that dedent below the list, which end
///   the list and are rejected because a `DEDENT` reaches the loop head.
///
/// The nearest boundaries are tried first. If the run cannot end at `j` (a
/// bracket, string or clause carries over) a later `j` is tried, doubling the
/// run each time, so retries cost O(final run). If the statement before `k`
/// depends on the edit, an earlier `k` is tried. See [`attempt`] for one try.
fn reparse_run(old: &Parse, edit: &Edit<'_>, node: &SyntaxNode) -> Run {
    let green = node.green();
    let Some(list) = List::new(node, &green) else {
        return Run::Skip;
    };
    let old_text = edit.old_text;
    let (es, ee) = (offset(edit.start), offset(edit.old_end));

    // Start candidates, nearest first: boundaries whose first character and
    // indentation the edit leaves in place. A module run may also start at 0.
    let upto = list
        .ranges
        .partition_point(|r| r.start() <= es)
        .min(list.end_item);
    let mut starts = (list.first_item..upto)
        .rev()
        .filter_map(|i| list.boundary(old_text, i))
        .filter(|b| b.start < es || starts_line_content(edit.new_text, edit.start));
    let mut start = starts.next();
    if start.is_none() && !list.module {
        return Run::Skip;
    }

    // End candidates, nearest first: boundaries whose line (including the line
    // break before it) the edit leaves in place, then the end of the list.
    // `async` is never an end: `@deco` + `async def` needs two tokens of lookahead.
    let from = list.ranges.partition_point(|r| r.start() <= ee);
    let mut ends = (from..list.end_item)
        .filter_map(|i| list.boundary(old_text, i))
        .filter(|b| b.line_start > ee && kind_of(b.first.kind()) != ASYNC_KW);
    let mut end = match ends.next() {
        Some(b) => RunEnd::Statement(b),
        None => match list.end_of_list(old_text, ee) {
            Some(end) => end,
            None => return Run::Skip,
        },
    };

    for _ in 0..MAX_ATTEMPTS {
        match attempt(old, edit, node, &list, start.as_ref(), &end) {
            Attempt::Done(parse) => return Run::Done(parse),
            Attempt::Skip => return Run::Skip,
            Attempt::Abandon => return Run::Abandon,
            Attempt::EarlierStart => {
                start = starts.next();
                if start.is_none() && !list.module {
                    return Run::Skip;
                }
            }
            Attempt::LaterEnd => {
                let RunEnd::Statement(j) = end else {
                    return Run::Skip;
                };
                let fs = start.map_or(TextSize::new(0), |b| b.start);
                let min_start = j.start + (j.start - fs);
                end = loop {
                    match ends.next() {
                        Some(b) if b.start < min_start => {}
                        Some(b) => break RunEnd::Statement(b),
                        None => match list.end_of_list(old_text, ee) {
                            Some(end) => break end,
                            None => return Run::Skip,
                        },
                    }
                };
            }
        }
    }
    Run::Skip
}

/// Reparses the run from `start` (or the beginning of the module) to `end`.
///
/// The run's new text is lexed alone, preceded by the list's `INDENT` text
/// and followed by a copy of `j`'s first token. That sentinel gives the run's
/// last statement the same lookahead as the full parse, e.g. for `else` /
/// `except` continuations. The run is then parsed with the list's own loop
/// ([`parse_statement_run`]), which must stop exactly before the sentinel (or
/// the list's closing `DEDENT`). Further checks guarantee that the full parse
/// of `new_text` would produce the same elements for the run and leave
/// everything outside it untouched:
///
/// * the lexer reaches the end of the run at a line boundary (the last
///   significant token is a real `NEWLINE`, followed only by blank/comment
///   lines), so nothing (bracket, string, continuation) carries into `j`;
/// * the lookahead of the statement before `k` (`elif`/`else`/`except`/
///   `finally`) sees the same decision, and an unclosed-bracket recovery that
///   inspected `k`'s line still sees the same first word;
/// * no old diagnostic starts at `k`, or between `j`'s line start and `j`.
///   Those may stem from the parser's lookahead, which reports at the next
///   token (or at the zero-width closing `DEDENT`), and the parser
///   de-duplicates errors by start offset. So diagnostics before the run are
///   kept, the run's are replaced, and later ones are shifted, with no
///   interaction across the boundaries;
/// * at the end of a block, the old and new runs close the same indentation
///   levels, so the DEDENTs that close enclosing lists carry over.
fn attempt(
    old: &Parse,
    edit: &Edit<'_>,
    node: &SyntaxNode,
    list: &List<'_>,
    start: Option<&Boundary<'_>>,
    end: &RunEnd<'_>,
) -> Attempt {
    let old_text = edit.old_text;
    let k_idx = start.map_or(0, |b| b.idx);
    let fs = start.map_or(TextSize::new(0), |b| b.start);
    let (j_idx, fe) = match end {
        RunEnd::Statement(b) => (b.idx, b.start),
        RunEnd::ModuleEof => (list.kids.len(), TextSize::of(old_text)),
        RunEnd::Dedent { .. } | RunEnd::BlockEof => {
            (list.end_item, list.ranges[list.end_item].start())
        }
    };
    // A run over the whole module, or over most of a large file, is no
    // cheaper than (and is reported as) a full parse.
    let whole_module = matches!(end, RunEnd::ModuleEof)
        && list.kids[..k_idx]
            .iter()
            .all(|k| matches!(k, NodeOrToken::Token(_)));
    let run_len = usize::from(fe - fs);
    if whole_module || (run_len > MIN_ABANDON_BYTES && run_len * 2 > old_text.len()) {
        return Attempt::Abandon;
    }

    // Diagnostics at the run's boundaries may come from outside the run.
    let errors = old.errors();
    if start.is_some() && any_error_starting_in(errors, fs, fs) {
        return Attempt::EarlierStart;
    }
    match end {
        RunEnd::Statement(b) if any_error_starting_in(errors, b.line_start, fe) => {
            return Attempt::LaterEnd
        }
        RunEnd::Dedent { line_start } if any_error_starting_in(errors, *line_start, fe) => {
            return Attempt::Skip
        }
        RunEnd::BlockEof if any_error_starting_in(errors, fe, fe) => return Attempt::Skip,
        _ => {}
    }

    let prefix = list.level.map_or("", |(text, _)| text);
    if matches!(end, RunEnd::Dedent { .. } | RunEnd::BlockEof) {
        // The DEDENTs at the list's end that close enclosing lists are kept
        // from the old tree. That is right only if, in the old text too, the
        // list's closing DEDENT closed the list's own level, i.e. the old run
        // left no indentation level without a consumer (error recovery can
        // leave some: an unmatched dedent pushes a level without an INDENT,
        // and unexpected-indent recovery eats one DEDENT for nested levels).
        // The old run lexed alone ends with one DEDENT per level it leaves open.
        let consumed = TokensBefore::new(&list.kids, list.end_item)
            .take_while(|t| kind_of(t.kind()) == DEDENT)
            .count();
        let (old_tokens, _) =
            tokenize(&[prefix, &old_text[usize::from(fs)..usize::from(fe)]].concat());
        if old_tokens
            .iter()
            .rev()
            .take_while(|t| t.kind == DEDENT)
            .count()
            != consumed + 1
        {
            return Attempt::Skip;
        }
    }

    // Lex the run alone: [INDENT text] + new run text + [sentinel = first token of `j`].
    let body = &edit.new_text[usize::from(fs)..edit.to_new(usize::from(fe))];
    let sentinel = match end {
        RunEnd::Statement(b) => b.first.text(),
        _ => "",
    };
    let text = [prefix, body, sentinel].concat();
    let (tokens, lex_errors) = tokenize(&text);
    // A bracket left open in the run is cut off where the whole file's lexer
    // would cut it only if the rest of the file has balanced brackets (its
    // recovery passes look at every bracket still open at the end).
    if lex_errors
        .iter()
        .any(|d| d.message.ends_with("was never closed"))
        && old.errors().iter().any(|e| is_bracket_error(&e.message))
    {
        return Attempt::Skip;
    }
    let base = TextSize::of(prefix);
    let body_end = base + TextSize::of(body);
    let first = if list.level.is_some() {
        if tokens.first().map(|t| (t.kind, t.range)) != Some((INDENT, TextRange::up_to(base))) {
            return Attempt::Skip;
        }
        1
    } else {
        0
    };
    let stop = match end {
        RunEnd::Statement(b) => {
            let Some(i) = tokens
                .iter()
                .position(|t| t.range.start() >= body_end && t.kind != DEDENT)
            else {
                return Attempt::LaterEnd;
            };
            if tokens[i].range != TextRange::at(body_end, TextSize::of(sentinel))
                || tokens[i].kind != lexed_kind(kind_of(b.first.kind()))
            {
                return Attempt::LaterEnd;
            }
            i
        }
        RunEnd::Dedent { .. } | RunEnd::BlockEof => match tokens.last() {
            Some(t) if t.kind == DEDENT && t.range == TextRange::empty(body_end) => {
                tokens.len() - 1
            }
            _ => return Attempt::Skip,
        },
        RunEnd::ModuleEof => tokens.len(),
    };
    if first > stop {
        return Attempt::Skip;
    }
    if matches!(end, RunEnd::Statement(_) | RunEnd::Dedent { .. })
        && !ends_at_line_boundary(&text, &tokens[first..stop], body_end)
    {
        return if matches!(end, RunEnd::Statement(_)) {
            Attempt::LaterEnd
        } else {
            Attempt::Skip
        };
    }
    if let Some(b) = start {
        if !same_lookahead(old, edit, b, &tokens[first..stop]) {
            return Attempt::EarlierStart;
        }
    }

    let list_kind = if list.module {
        StatementList::Module
    } else {
        StatementList::Block
    };
    // Blocks entered through an unexpected indent use nesting budget without a
    // BLOCK node: the run may start up to that much deeper in the full parse.
    let slack = old
        .errors()
        .iter()
        .filter(|e| e.message.starts_with("unexpected indent"))
        .count() as u32
        * BLOCK_COST;
    let ctx = RunContext {
        depth: node.ancestors().filter(|a| a.kind() == BLOCK).count() as u32 * BLOCK_COST,
        slack,
        tree_depth: node.ancestors().count(),
    };
    let (run, parse_errors) =
        match parse_statement_run(&text, &tokens[first..], list_kind, stop - first, ctx) {
            Ok(run) => run,
            Err(RunMismatch::Overran) if matches!(end, RunEnd::Statement(_)) => {
                return Attempt::LaterEnd
            }
            Err(_) => return Attempt::Skip,
        };

    // Diagnostics: kept before the run, the run's own, shifted after it. The
    // run's parser errors lie in the run (lookahead ones at the sentinel).
    if parse_errors
        .iter()
        .any(|d| d.range.start() < base || d.range.start() > body_end)
    {
        return Attempt::Skip;
    }
    let to_eof = matches!(end, RunEnd::BlockEof | RunEnd::ModuleEof);
    let mut own: Vec<Diagnostic> = lex_errors
        .into_iter()
        .filter(|d| d.range.start() >= base && (to_eof || d.range.start() < body_end))
        .collect();
    own.extend(parse_errors);
    own.sort_by_key(|d| d.range.start()); // stable: lexer errors first, as in `parse`
    let kept = errors.partition_point(|e| e.range.start() < fs);
    let after = errors.partition_point(|e| e.range.start() <= fe);
    let mut new_errors: Vec<Diagnostic> =
        Vec::with_capacity(kept + own.len() + errors.len() - after);
    new_errors.extend_from_slice(&errors[..kept]);
    new_errors.extend(own.into_iter().map(|d| Diagnostic {
        range: d.range - base + fs,
        ..d
    }));
    new_errors.extend(errors[after..].iter().map(|e| edit.shifted(e)));

    let new_list = node
        .green()
        .splice_children(k_idx..j_idx, run.children().map(|c| c.to_owned()));
    Attempt::Done(Parse::new(node.replace_with(new_list), new_errors))
}

/// Lexer diagnostics about unbalanced brackets.
fn is_bracket_error(message: &str) -> bool {
    message.ends_with("was never closed")
        || message.starts_with("unmatched '")
        || message == "f-string: unmatched bracket"
}

/// Checks that the parser and lexer decisions made *before* the run start
/// `b`, which look at `b`'s first token or line, are the same for the new text.
fn same_lookahead(old: &Parse, edit: &Edit<'_>, b: &Boundary<'_>, new_tokens: &[Token]) -> bool {
    // The statement before `b` may peek at its first token: clause keywords
    // continue `if`/`while`/`for`/`try`. A decorator also continues with `@`,
    // `def`, `class` or `async def`. But a dangling decorator that sees `b`'s
    // first token reports an error at `b`, which the caller rejects, and one
    // that sees a DEDENT does not depend on `b`. Nothing else looks past a
    // NEWLINE, and after an `INDENT` (first statement of a block) nothing peeks.
    const PEEKED: &[SyntaxKind] = &[ELIF_KW, ELSE_KW, EXCEPT_KW, FINALLY_KW];
    if matches!(b.prev, Some(NEWLINE | DEDENT)) {
        let o1 = lexed_kind(kind_of(b.first.kind()));
        let n1 = new_tokens.iter().map(|t| t.kind).find(|k| !k.is_trivia());
        if Some(o1) != n1 && (PEEKED.contains(&o1) || n1.is_none_or(|k| PEEKED.contains(&k))) {
            return false;
        }
    }
    // The lexer's unclosed-bracket recovery decides from the first word of the
    // next non-blank line (`Lexer::unclosed_bracket_recovery`, whose word lists
    // contain only keywords and soft keywords) and from a leading `@`. If it
    // closed a bracket by looking at `b`'s line, that decision must not flip.
    let recovered_before = old
        .errors()
        .iter()
        .take_while(|e| e.range.start() < b.start)
        .any(|e| e.message.ends_with("was never closed"));
    if recovered_before {
        let s = usize::from(b.start);
        let (old_line, new_line) = (&edit.old_text[s..], &edit.new_text[s..]);
        let (ow, nw) = (first_word(old_line), first_word(new_line));
        let keywordish = |w: &str| {
            SyntaxKind::from_keyword(w).is_some() || SyntaxKind::from_soft_keyword(w).is_some()
        };
        if old_line.starts_with('@') != new_line.starts_with('@')
            || (ow != nw && (keywordish(ow) || keywordish(nw)))
        {
            return false;
        }
    }
    true
}

/// True if the lexed run ends at a line boundary: the last significant token
/// before the trailing zero-width `DEDENT`s is a real `NEWLINE`, followed only
/// by blank lines, comments and indentation.
fn ends_at_line_boundary(text: &str, tokens: &[Token], body_end: TextSize) -> bool {
    let mut rest = tokens;
    while let [init @ .., last] = rest {
        if last.kind != DEDENT || last.range != TextRange::empty(body_end) {
            break;
        }
        rest = init;
    }
    for t in rest.iter().rev() {
        match t.kind {
            NL | COMMENT => {}
            WHITESPACE if !text[t.range].contains('\\') => {}
            NEWLINE => return !t.range.is_empty(),
            _ => return false,
        }
    }
    false
}

/// True if the lexer processed the indentation of the line of the token that
/// follows `before` (the tokens preceding it, in reverse order). That means
/// the token is the first on its line, and the line follows a logical line
/// break, a blank or comment line, or the start of the file. A line after a
/// backslash continuation does not qualify.
fn indentation_processed<'a>(mut before: impl Iterator<Item = &'a GreenTokenData>) -> bool {
    // Zero-width DEDENTs sit between the indentation and the first token.
    let mut t = before.next();
    while t.is_some_and(|t| kind_of(t.kind()) == DEDENT) {
        t = before.next();
    }
    let Some(t) = t else { return true };
    match kind_of(t.kind()) {
        INDENT | NEWLINE | NL => true,
        WHITESPACE => {
            t.text()
                .bytes()
                .all(|b| matches!(b, b' ' | b'\t' | b'\x0c'))
                && before
                    .next()
                    .is_none_or(|p| matches!(kind_of(p.kind()), NEWLINE | NL))
        }
        _ => false,
    }
}

/// The tokens of `kids[..end]` in reverse text order (descending into nodes).
struct TokensBefore<'k, 'a> {
    kids: &'k [GreenElementRef<'a>],
    next_kid: usize,
    stack: Vec<GreenElementRef<'a>>,
}

impl<'k, 'a> TokensBefore<'k, 'a> {
    fn new(kids: &'k [GreenElementRef<'a>], end: usize) -> Self {
        TokensBefore {
            kids,
            next_kid: end,
            stack: Vec::new(),
        }
    }
}

impl<'a> Iterator for TokensBefore<'_, 'a> {
    type Item = &'a GreenTokenData;

    fn next(&mut self) -> Option<&'a GreenTokenData> {
        loop {
            let el = match self.stack.pop() {
                Some(el) => el,
                None if self.next_kid > 0 => {
                    self.next_kid -= 1;
                    self.kids[self.next_kid]
                }
                None => return None,
            };
            match el {
                NodeOrToken::Token(t) => return Some(t),
                NodeOrToken::Node(n) => self.stack.extend(n.children()),
            }
        }
    }
}

/// First token (in text order) of a green element.
fn green_first_token(el: GreenElementRef<'_>) -> Option<&GreenTokenData> {
    match el {
        NodeOrToken::Token(t) => Some(t),
        NodeOrToken::Node(n) => n.children().find_map(green_first_token),
    }
}

/// Offset of the start of the line containing `pos`.
fn line_start(text: &str, pos: usize) -> usize {
    text[..pos].rfind(['\n', '\r']).map_or(0, |i| i + 1)
}

/// Indentation column of `ws` with the lexer's rules (tabs to multiples of 8,
/// form feed resets); `None` if `ws` is not pure indentation.
fn indent_col(ws: &str) -> Option<usize> {
    let mut col = 0;
    for c in ws.chars() {
        match c {
            ' ' => col += 1,
            '\t' => col = (col / 8 + 1) * 8,
            '\x0c' => col = 0,
            _ => return None,
        }
    }
    Some(col)
}

/// True if `text[pos..]` starts with a character that makes its line a code
/// line (not blank, comment, or continuation) without changing its indentation.
fn starts_line_content(text: &str, pos: usize) -> bool {
    text[pos..]
        .chars()
        .next()
        .is_some_and(|c| !matches!(c, ' ' | '\t' | '\x0c' | '\n' | '\r' | '#' | '\\'))
}

/// The word the lexer's bracket recovery reads at the start of a line.
fn first_word(line: &str) -> &str {
    let end = line
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(line.len());
    &line[..end]
}
