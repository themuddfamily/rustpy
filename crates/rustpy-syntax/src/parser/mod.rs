//! Event-based recursive-descent Python parser producing a lossless `rowan`
//! tree with error recovery (roadmap §2.1).
//!
//! The grammar lives in [`stmt`], [`expr`] and [`pattern`]; this module holds
//! the parser machinery: token cursor, events, markers and the tree sink.

mod expr;
mod pattern;
mod stmt;
#[cfg(test)]
mod tests;

use std::hash::{Hash, Hasher};

use rowan::{GreenNode, GreenNodeData, GreenToken, NodeOrToken};
use rustc_hash::{FxHashMap, FxHasher};
use rustpy_core::{Diagnostic, TextRange, TextSize};

use crate::lexer::{tokenize, Token};
use crate::syntax_kind::SyntaxKind::{self, *};
use crate::syntax_kind::SyntaxNode;

/// Result of parsing: the green tree plus syntax diagnostics (lexer + parser).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parse {
    green: GreenNode,
    errors: Vec<Diagnostic>,
}

impl Parse {
    /// Assembles a parse result from its parts (used by incremental reparsing).
    pub(crate) fn new(green: GreenNode, errors: Vec<Diagnostic>) -> Parse {
        Parse { green, errors }
    }
    pub fn syntax_node(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }
    pub fn green(&self) -> &GreenNode {
        &self.green
    }
    pub fn errors(&self) -> &[Diagnostic] {
        &self.errors
    }
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Parses a whole module.
pub fn parse(text: &str) -> Parse {
    let (tokens, mut errors) = tokenize(text);
    let mut p = Parser::new(text, &tokens);
    stmt::module(&mut p);
    let (events, parse_errors) = p.finish();
    errors.extend(parse_errors);
    let green = Sink::new(text, &tokens, events).build(&mut errors);
    errors.sort_by_key(|d| d.range.start());
    Parse { green, errors }
}

/// Parses a single expression (for evaluate-expression dialogs, injections…).
pub fn parse_expression(text: &str) -> Parse {
    let (tokens, mut errors) = tokenize(text);
    let mut p = Parser::new(text, &tokens);
    let m = p.start();
    expr::expressions(&mut p);
    while !p.at(EOF) {
        if p.at(NEWLINE) {
            p.bump_any();
        } else {
            p.push_error("unexpected token after expression");
            let e = p.start();
            p.bump_any();
            e.complete(&mut p, ERROR);
        }
    }
    m.complete(&mut p, MODULE);
    let (events, parse_errors) = p.finish();
    errors.extend(parse_errors);
    let green = Sink::new(text, &tokens, events).build(&mut errors);
    errors.sort_by_key(|d| d.range.start());
    Parse { green, errors }
}

/// The statement list a run of statements belongs to (see [`parse_statement_run`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum StatementList {
    /// The module body (`stmt::module` loop).
    Module,
    /// The body of an indented `BLOCK` (`stmt::block` loop).
    Block,
}

/// Why [`parse_statement_run`] did not stop exactly at its stop token.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum RunMismatch {
    /// A statement consumed the stop token.
    Overran,
    /// A `DEDENT` ended the block before the stop token.
    EndedEarly,
}

/// Incremental-reparse entry point: runs the statement loop of `list` over
/// `tokens` (lexed from `text`), stopping at the loop head once the next
/// significant token is `tokens[stop]` (the end of input when
/// `stop == tokens.len()`). Returns a `MODULE`/`BLOCK` node built from
/// `tokens[..stop]` whose children are the parsed elements, plus the parser
/// errors; an error if the loop did not stop exactly before `tokens[stop]`.
pub(crate) fn parse_statement_run(
    text: &str,
    tokens: &[Token],
    list: StatementList,
    stop: usize,
    ctx: RunContext,
) -> Result<(GreenNode, Vec<Diagnostic>), RunMismatch> {
    debug_assert!(stop <= tokens.len());
    let mut p = Parser::new(text, tokens);
    p.depth = ctx.depth;
    p.max_depth = ctx.depth;
    let stop_sig = p.sig.partition_point(|&i| i < stop);
    let m = p.start();
    while p.pos < stop_sig {
        match list {
            StatementList::Module => stmt::module_item(&mut p),
            StatementList::Block if p.at(DEDENT) => return Err(RunMismatch::EndedEarly),
            StatementList::Block => stmt::block_item(&mut p),
        }
    }
    if p.pos != stop_sig {
        return Err(RunMismatch::Overran);
    }
    m.complete(
        &mut p,
        if list == StatementList::Module {
            MODULE
        } else {
            BLOCK
        },
    );
    // The full parse may start the run up to `slack` deeper: it must not reach
    // the nesting limit either.
    if p.max_depth + ctx.slack > MAX_DEPTH {
        return Err(RunMismatch::Overran);
    }
    let (events, mut errors) = p.finish();
    let mut sink = Sink::new(text, &tokens[..stop], events);
    sink.depth_offset = ctx.tree_depth.saturating_sub(1);
    let green = sink.build(&mut errors);
    Ok((green, errors))
}

/// Parser state a full parse has at the start of a statement run (see
/// [`parse_statement_run`]), so a run is parsed exactly as it would be in place.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct RunContext {
    /// Nesting budget ([`Parser::enter`]) used by the enclosing blocks.
    pub depth: u32,
    /// How much more budget the full parse may have used at the run start
    /// (blocks entered through an unexpected indent have no BLOCK node).
    pub slack: u32,
    /// Depth of the statement list's node in the full tree (the root is 1).
    pub tree_depth: usize,
}

// ---------------------------------------------------------------------------
// Token sets

/// A compact set of [`SyntaxKind`]s (only token kinds, all < 128).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TokenSet(u128);

impl TokenSet {
    pub const EMPTY: TokenSet = TokenSet(0);

    pub const fn new(kinds: &[SyntaxKind]) -> TokenSet {
        let mut bits = 0u128;
        let mut i = 0;
        while i < kinds.len() {
            bits |= 1u128 << (kinds[i] as u16);
            i += 1;
        }
        TokenSet(bits)
    }

    pub const fn union(self, other: TokenSet) -> TokenSet {
        TokenSet(self.0 | other.0)
    }

    pub const fn contains(&self, kind: SyntaxKind) -> bool {
        (kind as u16) < 128 && self.0 & (1u128 << (kind as u16)) != 0
    }
}

// ---------------------------------------------------------------------------
// Events & markers

#[derive(Debug, Clone)]
enum Event {
    Start {
        kind: SyntaxKind,
        forward_parent: Option<u32>,
    },
    Finish,
    Token {
        kind: SyntaxKind,
    },
}

const TOMBSTONE: SyntaxKind = SyntaxKind::__LAST;

#[must_use = "a marker must be completed or abandoned"]
pub struct Marker {
    pos: u32,
}

impl Marker {
    pub fn complete(self, p: &mut Parser<'_>, kind: SyntaxKind) -> CompletedMarker {
        let idx = self.pos as usize;
        match &mut p.events[idx] {
            Event::Start { kind: slot, .. } => *slot = kind,
            _ => unreachable!(),
        }
        p.events.push(Event::Finish);
        CompletedMarker {
            pos: self.pos,
            kind,
        }
    }

    pub fn abandon(self, p: &mut Parser<'_>) {
        let idx = self.pos as usize;
        if idx == p.events.len() - 1 {
            if let Some(Event::Start {
                kind: TOMBSTONE,
                forward_parent: None,
            }) = p.events.pop()
            {
                return;
            }
            unreachable!()
        }
        // Leave a tombstone that the sink ignores.
    }
}

#[derive(Copy, Clone, Debug)]
pub struct CompletedMarker {
    pos: u32,
    kind: SyntaxKind,
}

impl CompletedMarker {
    /// Starts a new node *before* this completed node, making it a child.
    pub fn precede(self, p: &mut Parser<'_>) -> Marker {
        let new = p.start();
        if let Event::Start { forward_parent, .. } = &mut p.events[self.pos as usize] {
            *forward_parent = Some(new.pos - self.pos);
        }
        new
    }

    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }
}

/// A snapshot for speculative parsing (backtracking on soft keywords).
pub struct Checkpoint {
    pos: usize,
    events: usize,
    errors: usize,
}

// ---------------------------------------------------------------------------
// Parser

pub struct Parser<'a> {
    text: &'a str,
    /// Indices into the full token list for non-trivia tokens.
    sig: Vec<usize>,
    tokens: &'a [Token],
    pos: usize,
    events: Vec<Event>,
    errors: Vec<Diagnostic>,
    /// Token position seen by the last [`Parser::step`] call, and how many
    /// consecutive calls saw it.
    stall_pos: usize,
    stall_steps: u32,
    /// Current nesting of recursive grammar rules (see [`Parser::enter`]).
    depth: u32,
    /// Highest `depth` reached (statement runs check it against their slack).
    max_depth: u32,
    /// Set by `from __future__ import barry_as_FLUFL`, which makes `<>` the
    /// inequality operator (and `!=` an error), like in CPython.
    barry_as_flufl: bool,
}

/// Consecutive [`Parser::step`] calls without progress that mean a loop is stuck.
const STALL_LIMIT: u32 = 10_000;

/// Budget for the nesting of recursive grammar rules, in units of roughly
/// 100–130 bytes of stack (about 1 MiB in total, half of a default thread
/// stack). Each kind of nesting is charged by its stack cost: see the
/// `*_COST` constants. Valid Python stays far below the budget because CPython
/// gives up first: at 200 nested brackets, 100 indentation levels and about
/// 6000 chained unary operators / conditional expressions. Deeper input gets
/// a diagnostic instead of overflowing the stack.
const MAX_DEPTH: u32 = 8_000;

/// A recursive expression rule: unary operators, `not`, `**`, lambda bodies,
/// conditional branches, patterns.
pub(crate) const EXPR_COST: u32 = 1;
/// A nested format spec or an indented block.
pub(crate) const BLOCK_COST: u32 = 2;
/// Brackets, strings and calls/subscripts: the recursion re-enters the whole
/// operator-precedence chain, which costs about eight times more stack.
pub(crate) const BRACKET_COST: u32 = 8;

impl<'a> Parser<'a> {
    fn new(text: &'a str, tokens: &'a [Token]) -> Self {
        let sig = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.kind.is_trivia())
            .map(|(i, _)| i)
            .collect();
        Self {
            text,
            sig,
            tokens,
            pos: 0,
            events: Vec::with_capacity(tokens.len() * 2),
            errors: Vec::new(),
            stall_pos: usize::MAX,
            stall_steps: 0,
            depth: 0,
            max_depth: 0,
            barry_as_flufl: false,
        }
    }

    fn finish(self) -> (Vec<Event>, Vec<Diagnostic>) {
        (self.events, self.errors)
    }

    // ----- lookahead ------------------------------------------------------

    fn token(&self, n: usize) -> Option<&Token> {
        self.sig.get(self.pos + n).map(|&i| &self.tokens[i])
    }

    pub fn nth(&self, n: usize) -> SyntaxKind {
        self.token(n).map(|t| t.kind).unwrap_or(EOF)
    }

    pub fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    pub fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    pub fn nth_at(&self, n: usize, kind: SyntaxKind) -> bool {
        self.nth(n) == kind
    }

    pub fn at_ts(&self, set: TokenSet) -> bool {
        set.contains(self.current())
    }

    /// Text of the n-th upcoming significant token.
    pub fn nth_text(&self, n: usize) -> &'a str {
        self.token(n).map(|t| &self.text[t.range]).unwrap_or("")
    }

    pub fn at_ident(&self, text: &str) -> bool {
        self.at(IDENT) && self.nth_text(0) == text
    }

    pub fn nth_at_ident(&self, n: usize, text: &str) -> bool {
        self.nth_at(n, IDENT) && self.nth_text(n) == text
    }

    /// Byte offset where the current token starts (or EOF).
    pub fn current_start(&self) -> TextSize {
        self.token(0)
            .map(|t| t.range.start())
            .unwrap_or_else(|| TextSize::new(self.text.len() as u32))
    }

    pub fn current_range(&self) -> TextRange {
        self.token(0)
            .map(|t| t.range)
            .unwrap_or_else(|| TextRange::empty(TextSize::new(self.text.len() as u32)))
    }

    /// End offset of the previously consumed token.
    pub fn prev_end(&self) -> TextSize {
        if self.pos == 0 {
            return TextSize::new(0);
        }
        self.tokens[self.sig[self.pos - 1]].range.end()
    }

    // ----- consuming --------------------------------------------------------

    pub fn start(&mut self) -> Marker {
        let pos = self.events.len() as u32;
        self.events.push(Event::Start {
            kind: TOMBSTONE,
            forward_parent: None,
        });
        Marker { pos }
    }

    /// Consumes the current token, re-tagging it as `kind` (for soft keywords).
    pub fn bump_as(&mut self, kind: SyntaxKind) {
        if self.pos < self.sig.len() {
            self.pos += 1;
            self.events.push(Event::Token { kind });
        }
    }

    pub fn bump_any(&mut self) {
        let k = self.current();
        if k != EOF {
            self.bump_as(k);
        }
    }

    pub fn bump(&mut self, kind: SyntaxKind) {
        debug_assert!(
            self.at(kind),
            "expected {kind:?} but at {:?}",
            self.current()
        );
        self.bump_any();
    }

    pub fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump_any();
            true
        } else {
            false
        }
    }

    pub fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        let what = kind
            .text()
            .map(|t| format!("'{t}'"))
            .unwrap_or_else(|| format!("{kind:?}").to_lowercase());
        self.push_error(format!("expected {what}"));
        false
    }

    /// Records an error at the current token without consuming anything.
    pub fn push_error(&mut self, msg: impl Into<String>) {
        let range = if self.at(EOF) || self.at(NEWLINE) || self.at(DEDENT) {
            TextRange::empty(self.prev_end())
        } else {
            self.current_range()
        };
        // Avoid duplicate errors at the same position.
        if self
            .errors
            .last()
            .is_some_and(|e| e.range.start() == range.start())
        {
            return;
        }
        self.errors.push(Diagnostic::error(range, "syntax", msg));
    }

    /// Records an error and wraps the current token in an ERROR node (if any).
    pub fn error_and_bump(&mut self, msg: impl Into<String>) {
        self.push_error(msg);
        if !self.at(EOF) && !self.at(NEWLINE) && !self.at(DEDENT) && !self.at(INDENT) {
            let m = self.start();
            self.bump_any();
            m.complete(self, ERROR);
        }
    }

    /// Skips tokens until one in `recovery` (not consumed), wrapping them in an ERROR node.
    pub fn error_recover(&mut self, msg: impl Into<String>, recovery: TokenSet) {
        self.push_error(msg);
        if self.at(EOF) || self.at_ts(recovery) {
            return;
        }
        let m = self.start();
        while !self.at(EOF) && !self.at_ts(recovery) {
            self.bump_any();
        }
        m.complete(self, ERROR);
    }

    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            pos: self.pos,
            events: self.events.len(),
            errors: self.errors.len(),
        }
    }

    pub fn rewind(&mut self, cp: Checkpoint) {
        self.pos = cp.pos;
        self.events.truncate(cp.events);
        self.errors.truncate(cp.errors);
    }

    /// Called at the head of every grammar loop. Guards against loops that
    /// make no progress (which would be a parser bug): after
    /// [`STALL_LIMIT`] iterations at the same token, that token is skipped
    /// as an error so that parsing always terminates.
    pub fn step(&mut self) -> bool {
        if self.pos != self.stall_pos {
            self.stall_pos = self.pos;
            self.stall_steps = 0;
            return true;
        }
        self.stall_steps += 1;
        if self.stall_steps > STALL_LIMIT {
            // Every loop stops at EOF, so being stuck there is impossible.
            assert!(!self.at(EOF), "parser stuck at end of input");
            self.stall_steps = 0;
            self.push_error("internal error: the parser made no progress here");
            let m = self.start();
            self.bump_any();
            m.complete(self, ERROR);
        }
        true
    }

    /// Enters a recursive grammar rule that needs `cost` units of the nesting
    /// budget ([`MAX_DEPTH`]). Returns `false` (after reporting an error) when
    /// the input is nested too deeply to parse safely; the caller must then
    /// not recurse. Every successful `enter` must be paired with a
    /// [`Parser::leave`] of the same cost.
    pub fn enter(&mut self, cost: u32) -> bool {
        if self.depth + cost > MAX_DEPTH {
            self.push_error("too many nested expressions or blocks");
            return false;
        }
        self.depth += cost;
        self.max_depth = self.max_depth.max(self.depth);
        true
    }

    /// [`Parser::enter`] for expressions and patterns: when the input is
    /// nested too deeply, the rest of the logical line is skipped (as one
    /// ERROR node) so that absurd nesting yields one diagnostic and a
    /// shallow tree instead of an error per remaining level.
    pub fn enter_expr(&mut self, cost: u32) -> bool {
        if self.enter(cost) {
            return true;
        }
        if !self.at(NEWLINE) && !self.at(EOF) {
            let m = self.start();
            while !self.at(NEWLINE) && !self.at(EOF) {
                self.bump_any();
            }
            m.complete(self, ERROR);
        }
        false
    }

    pub fn leave(&mut self, cost: u32) {
        self.depth -= cost;
    }

    pub fn errors_len(&self) -> usize {
        self.errors.len()
    }

    /// Reports the spelling of an inequality operator (at the current
    /// `NOT_EQ` token) that is invalid in this module.
    pub fn check_not_eq_spelling(&mut self) {
        match (self.nth_text(0), self.barry_as_flufl) {
            ("<>", false) => self.push_error("use '!=' instead of '<>'"),
            ("!=", true) => self.push_error("with Barry as BDFL, use '<>' instead of '!='"),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Green tree builder

type GreenElement = NodeOrToken<GreenNode, GreenToken>;

/// Builds green trees like `rowan::GreenNodeBuilder` and deduplicates tokens
/// and small nodes the same way, but keeps the hash of every interned node:
/// rowan's interner re-hashes whole subtrees whenever its table grows, which
/// is quadratic on deep trees such as long attribute or call chains.
#[derive(Default)]
struct TreeBuilder<'t> {
    /// Open nodes: kind and index of their first child in `children`.
    parents: Vec<(SyntaxKind, usize)>,
    /// Children of the open nodes with their interning hash (0: not interned).
    children: Vec<(u64, GreenElement)>,
    tokens: FxHashMap<(SyntaxKind, &'t str), (u64, GreenToken)>,
    /// Interned nodes by hash; a bucket only holds more than one node on a
    /// hash collision.
    nodes: FxHashMap<u64, Vec<GreenNode>>,
}

impl<'t> TreeBuilder<'t> {
    fn token(&mut self, kind: SyntaxKind, text: &'t str) {
        let (hash, token) = self
            .tokens
            .entry((kind, text))
            .or_insert_with(|| {
                let mut h = FxHasher::default();
                (kind, text).hash(&mut h);
                (h.finish() | 1, GreenToken::new(kind.into(), text))
            })
            .clone();
        self.children.push((hash, NodeOrToken::Token(token)));
    }

    fn start_node(&mut self, kind: SyntaxKind) {
        self.parents.push((kind, self.children.len()));
    }

    fn finish_node(&mut self) {
        let (kind, first) = self.parents.pop().expect("finish_node without start_node");
        let children = &self.children[first..];
        // Like rowan: only nodes with at most three interned children are interned.
        let hash = if children.len() <= 3 && children.iter().all(|(h, _)| *h != 0) {
            let mut h = FxHasher::default();
            kind.hash(&mut h);
            for (child, _) in children {
                child.hash(&mut h);
            }
            h.finish() | 1
        } else {
            0
        };
        let node = if hash == 0 {
            GreenNode::new(kind.into(), self.children.drain(first..).map(|(_, e)| e))
        } else {
            let bucket = self.nodes.entry(hash).or_default();
            match bucket
                .iter()
                .find(|n| n.kind() == kind.into() && same_children(n, &self.children[first..]))
            {
                Some(n) => {
                    let n = n.clone();
                    self.children.truncate(first);
                    n
                }
                None => {
                    let n =
                        GreenNode::new(kind.into(), self.children.drain(first..).map(|(_, e)| e));
                    bucket.push(n.clone());
                    n
                }
            }
        };
        self.children.push((hash, NodeOrToken::Node(node)));
    }

    fn finish(mut self) -> GreenNode {
        assert!(self.parents.is_empty(), "unfinished nodes");
        match self.children.pop() {
            Some((_, NodeOrToken::Node(root))) if self.children.is_empty() => root,
            _ => panic!("the tree must have exactly one root node"),
        }
    }
}

/// Whether `node` has exactly these children (by identity: they are interned).
fn same_children(node: &GreenNodeData, children: &[(u64, GreenElement)]) -> bool {
    node.children().len() == children.len()
        && node
            .children()
            .zip(children)
            .all(|(a, (_, b))| match (a, b) {
                (NodeOrToken::Node(a), NodeOrToken::Node(b)) => std::ptr::eq(a, &**b),
                (NodeOrToken::Token(a), NodeOrToken::Token(b)) => std::ptr::eq(a, &**b),
                _ => false,
            })
}

// ---------------------------------------------------------------------------
// Sink: events → green tree

/// Depth limit of the syntax tree. Dropping or walking a rowan tree recurses
/// once per level (about 80 bytes of stack each), and some inputs nest
/// without any parser recursion — a 50 000-term `a + a + …` chain or
/// `a.b.b.b…` is parsed by a loop — so their trees could be deep enough to
/// overflow a 2 MiB thread stack when dropped. Nodes below the limit are not
/// opened: their tokens are attached to the node at the limit (the tree stays
/// lossless) and one error is reported. CPython 3.12 itself rejects such
/// chains beyond about 10 000 terms.
const MAX_TREE_DEPTH: usize = 12_000;

struct Sink<'a> {
    text: &'a str,
    tokens: &'a [Token],
    token_pos: usize,
    events: Vec<Event>,
    builder: TreeBuilder<'a>,
    /// Depth of the built root's parent in the full tree (statement runs).
    depth_offset: usize,
}

impl<'a> Sink<'a> {
    fn new(text: &'a str, tokens: &'a [Token], events: Vec<Event>) -> Self {
        Self {
            text,
            tokens,
            token_pos: 0,
            events,
            builder: TreeBuilder::default(),
            depth_offset: 0,
        }
    }

    /// Builds the tree; a diagnostic is added to `errors` if it had to be
    /// flattened (see [`MAX_TREE_DEPTH`]).
    fn build(mut self, errors: &mut Vec<Diagnostic>) -> GreenNode {
        let mut forward_parents: Vec<SyntaxKind> = Vec::new();
        // Number of builder nodes currently open.
        let mut depth = 0usize;
        // For every node started by the events: whether it was opened in the
        // builder (`false`: flattened away by the depth limit).
        let mut opened: Vec<bool> = Vec::new();
        let mut flattened_at: Option<TextRange> = None;
        let events = std::mem::take(&mut self.events);
        let mut events: Vec<Option<Event>> = events.into_iter().map(Some).collect();
        let last = events.len().saturating_sub(1);
        for i in 0..events.len() {
            let Some(ev) = events[i].take() else { continue };
            match ev {
                Event::Start {
                    kind: TOMBSTONE,
                    forward_parent: None,
                } => {}
                Event::Start {
                    kind,
                    forward_parent,
                } => {
                    forward_parents.push(kind);
                    let mut idx = i;
                    let mut fp = forward_parent;
                    while let Some(fwd) = fp {
                        idx += fwd as usize;
                        fp = match events[idx].take() {
                            Some(Event::Start {
                                kind,
                                forward_parent,
                            }) => {
                                if kind != TOMBSTONE {
                                    forward_parents.push(kind);
                                }
                                forward_parent
                            }
                            _ => unreachable!(),
                        };
                    }
                    // Leading trivia belongs to the parent node (the root has none).
                    if depth > 0 {
                        self.eat_trivia();
                    }
                    for kind in forward_parents.drain(..).rev() {
                        if depth + self.depth_offset < MAX_TREE_DEPTH {
                            self.builder.start_node(kind);
                            depth += 1;
                            opened.push(true);
                        } else {
                            let here = self.tokens.get(self.token_pos).map_or_else(
                                || TextRange::empty(TextSize::new(self.text.len() as u32)),
                                |t| t.range,
                            );
                            flattened_at.get_or_insert(here);
                            opened.push(false);
                        }
                    }
                }
                Event::Finish => {
                    if i == last {
                        // Everything left (trailing trivia, stray tokens) belongs to the root.
                        while self.token_pos < self.tokens.len() {
                            let k = self.tokens[self.token_pos].kind;
                            self.push_token(k);
                        }
                    }
                    if opened.pop().expect("every Finish event has a Start") {
                        self.builder.finish_node();
                        depth -= 1;
                    }
                }
                Event::Token { kind } => {
                    self.eat_trivia();
                    self.push_token(kind);
                }
            }
        }
        debug_assert_eq!(
            self.token_pos,
            self.tokens.len(),
            "all tokens must be consumed"
        );
        if let Some(range) = flattened_at {
            errors.push(Diagnostic::error(
                range,
                "syntax",
                "too many nested expressions or blocks",
            ));
        }
        self.builder.finish()
    }

    fn eat_trivia(&mut self) {
        while let Some(t) = self.tokens.get(self.token_pos) {
            if !t.kind.is_trivia() {
                break;
            }
            self.push_token(t.kind);
        }
    }

    fn push_token(&mut self, kind: SyntaxKind) {
        let t = self.tokens[self.token_pos];
        self.builder.token(kind, &self.text[t.range]);
        self.token_pos += 1;
    }
}

/// Debug rendering of a syntax tree (used by tests): one node/token per line.
pub fn debug_tree(node: &SyntaxNode) -> String {
    fn go(
        el: rowan::NodeOrToken<SyntaxNode, crate::syntax_kind::SyntaxToken>,
        depth: usize,
        out: &mut String,
    ) {
        let pad = "  ".repeat(depth);
        match el {
            rowan::NodeOrToken::Node(n) => {
                out.push_str(&format!("{pad}{:?}@{:?}\n", n.kind(), n.text_range()));
                for c in n.children_with_tokens() {
                    go(c, depth + 1, out);
                }
            }
            rowan::NodeOrToken::Token(t) => {
                if !t.kind().is_trivia() {
                    out.push_str(&format!(
                        "{pad}{:?}@{:?} {:?}\n",
                        t.kind(),
                        t.text_range(),
                        t.text()
                    ));
                }
            }
        }
    }
    let mut s = String::new();
    go(rowan::NodeOrToken::Node(node.clone()), 0, &mut s);
    s
}

/// Compact S-expression rendering without ranges or trivia (for assertions).
pub fn sexp(node: &SyntaxNode) -> String {
    fn go(el: rowan::NodeOrToken<SyntaxNode, crate::syntax_kind::SyntaxToken>, out: &mut String) {
        match el {
            rowan::NodeOrToken::Node(n) => {
                out.push('(');
                out.push_str(&format!("{:?}", n.kind()));
                for c in n.children_with_tokens() {
                    if let rowan::NodeOrToken::Token(t) = &c {
                        if t.kind().is_trivia() || matches!(t.kind(), NEWLINE | INDENT | DEDENT) {
                            continue;
                        }
                    }
                    out.push(' ');
                    go(c, out);
                }
                out.push(')');
            }
            rowan::NodeOrToken::Token(t) => out.push_str(t.text()),
        }
    }
    let mut s = String::new();
    go(rowan::NodeOrToken::Node(node.clone()), &mut s);
    s
}
