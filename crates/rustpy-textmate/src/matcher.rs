//! Executes Oniguruma patterns (parsed by [`crate::onig`]) with the `regex`
//! crate.
//!
//! The `regex` crate has no look-around, `\G` or `\K`, so a pattern is
//! translated into a *core* regex plus emulated parts:
//!
//! * `\G` (anchor at the search start) and `\A` (start of the document)
//!   select one of up to four compiled variants, mirroring vscode-textmate's
//!   `compileAG`. `\G` can only match at the search start, so the variant in
//!   which it is empty is only accepted for a match starting there.
//! * `^` becomes `\A`: haystacks are single lines, and Oniguruma's `^` does
//!   not match after the final newline.
//! * A trailing positive look-ahead (`foo(?=\()`, also at the end of a final
//!   unquantified group) is consumed as an internal group and the match is
//!   cut where that group starts, so the regex engine backtracks into the
//!   core as Oniguruma would. A trailing `(?![\w$])` becomes `(?:[^\w$]|\z)`
//!   the same way.
//! * Look-arounds before the first consuming item (`(?<!\.)\bfoo`) are
//!   checked at each candidate start; this is exact.
//! * Any other look-around (outside `*`/`+`/`{n,m}` repetition) becomes an
//!   empty marker group checked at the marker's position after the match.
//!   This is exact when the regex's preferred path passes the checks;
//!   otherwise the search resumes at the next start, so a lower-priority path
//!   at the same start may be missed. A reported match always satisfies
//!   every look-around.
//! * Look-behind bodies are matched against the text before the position
//!   plus the following character, so `$`/`\b` at the position see their
//!   real right-hand context.
//! * `\K` is an internal empty group marking the reported start.
//! * Captures inside positive look-arounds are reported.
//!
//! Subexpression calls and back-references are expanded while parsing (see
//! [`crate::onig::parse`]). Look-arounds inside repetitions are reported as
//! unsupported.

use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};

use crate::onig::{self, Assert, Class, Group, Look, Node};

/// Never matches (an empty class).
const NEVER: &str = r"[^\x00-\x{10FFFF}]";

/// Capture spans by Oniguruma group number (0 = whole match).
pub(crate) type Groups = Vec<Option<(usize, usize)>>;

/// Search context shared by all patterns tried at one scan position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct SearchCtx {
    /// `\A` may match (first line of the document, nothing consumed yet).
    pub allow_a: bool,
    /// Position where `\G` matches, if any.
    pub g_pos: Option<usize>,
}

/// A successful search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    /// Where the underlying match attempt began (before `\K` adjustment);
    /// leftmost-first ordering uses this.
    pub raw_start: usize,
    pub start: usize,
    pub end: usize,
    /// Capture spans by Oniguruma group number. Empty unless captures were
    /// requested.
    pub groups: Groups,
}

/// A compiled pattern.
pub(crate) struct Matcher {
    branches: Vec<Branch>,
    uses_a: bool,
    uses_g: bool,
}

impl std::fmt::Debug for Matcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Matcher")
            .field("branches", &self.branches.len())
            .finish()
    }
}

impl Matcher {
    /// Parses and compiles an Oniguruma pattern.
    pub(crate) fn new(source: &str) -> Result<Matcher, String> {
        let parsed = onig::parse(source)?;
        Matcher::from_node(parsed.node, parsed.groups, false)
    }

    /// `anchored_only`: only [`Matcher::match_at`] will be used.
    fn from_node(node: Node, groups: usize, anchored_only: bool) -> Result<Matcher, String> {
        // Split first: a folded look-ahead's body is itself an alternation,
        // which its own matcher splits into branches rather than folding
        // again.
        let mut alternatives = Vec::new();
        top_branches(node, &mut alternatives);
        let plans = alternatives
            .into_iter()
            .map(|a| plan_branch(fold(a)))
            .collect::<Vec<_>>();
        // Consecutive alternatives whose cores need no markers and that have
        // the same leading look-arounds share one regex: `L·A|L·B` is
        // `L·(A|B)`, as the leading checks hold or fail at the start for both
        // and the alternation keeps their leftmost-first order. Alternatives
        // with markers are searched on their own so that their look-arounds
        // cannot reject a sibling's match.
        let mut branches = Vec::new();
        let mut pending: Option<(Vec<Look>, Vec<Node>)> = None;
        for plan in plans {
            let mergeable = !contains_look(&plan.core);
            if let Some((leading, cores)) = &mut pending {
                if mergeable && *leading == plan.leading {
                    cores.push(plan.core);
                    continue;
                }
            }
            if let Some((leading, cores)) = pending.take() {
                branches.push(Branch::new(alt_of(cores), leading, groups, anchored_only)?);
            }
            if mergeable {
                pending = Some((plan.leading, vec![plan.core]));
            } else {
                branches.push(Branch::new(plan.core, plan.leading, groups, anchored_only)?);
            }
        }
        if let Some((leading, cores)) = pending {
            branches.push(Branch::new(alt_of(cores), leading, groups, anchored_only)?);
        }
        let uses_a = branches.iter().any(|b| b.uses_a);
        let uses_g = branches.iter().any(|b| b.uses_g);
        Ok(Matcher {
            branches,
            uses_a,
            uses_g,
        })
    }

    /// Whether results depend on [`SearchCtx::allow_a`].
    pub(crate) fn uses_a(&self) -> bool {
        self.uses_a
    }

    /// Whether results depend on [`SearchCtx::g_pos`].
    pub(crate) fn uses_g(&self) -> bool {
        self.uses_g
    }

    /// Leftmost match starting at or after `from`.
    pub(crate) fn search(
        &self,
        text: &str,
        from: usize,
        ctx: SearchCtx,
        want_groups: bool,
    ) -> Option<Hit> {
        if from > text.len() || !text.is_char_boundary(from) {
            return None;
        }
        let mut best: Option<Hit> = None;
        for branch in &self.branches {
            if let Some(hit) = branch.search(text, from, ctx, want_groups) {
                if best.as_ref().is_none_or(|b| hit.raw_start < b.raw_start) {
                    let at_from = hit.raw_start == from;
                    best = Some(hit);
                    if at_from {
                        break;
                    }
                }
            }
        }
        best
    }

    /// The match starting exactly at `pos`, if any.
    fn match_at(&self, text: &str, pos: usize, ctx: SearchCtx, want_groups: bool) -> Option<Hit> {
        if pos > text.len() || !text.is_char_boundary(pos) {
            return None;
        }
        let g_on = ctx.g_pos == Some(pos);
        self.branches
            .iter()
            .find_map(|b| b.search_at(text, pos, ctx, want_groups, g_on))
    }
}

fn alt_of(mut nodes: Vec<Node>) -> Node {
    if nodes.len() == 1 {
        nodes.remove(0)
    } else {
        Node::Alt(nodes)
    }
}

fn into_items(node: Node) -> Vec<Node> {
    match node {
        Node::Concat(items) => items,
        Node::Empty => Vec::new(),
        other => vec![other],
    }
}

fn wrap_group(capture: Option<usize>, flags: onig::Flags, node: Node) -> Node {
    if capture.is_none() && flags.is_empty() {
        node
    } else {
        Node::Group(Box::new(Group {
            capture,
            flags,
            node,
        }))
    }
}

/// A chain of groups (`(?i:(…))`) around an alternation with look-arounds
/// in its branches, which become exact leading checks or tails once the
/// alternation is distributed. (Alternations of zero-width branches only are
/// cheaper as one folded look-ahead.)
fn is_alt_group_with_look(node: &Node) -> bool {
    match node {
        Node::Group(g) => match &g.node {
            Node::Alt(branches) => {
                branches.iter().any(contains_look) && !branches.iter().all(zero_width)
            }
            inner @ Node::Group(_) => is_alt_group_with_look(inner),
            _ => false,
        },
        _ => false,
    }
}

/// The alternatives of [`is_alt_group_with_look`], each wrapped in the
/// chain's groups.
fn distribute_groups(node: Node) -> Vec<Node> {
    match node {
        Node::Group(g) => {
            let Group {
                capture,
                flags,
                node,
            } = *g;
            let inner = match node {
                Node::Alt(branches) => branches,
                inner @ Node::Group(_) => distribute_groups(inner),
                other => vec![other],
            };
            inner
                .into_iter()
                .map(|b| wrap_group(capture, flags, b))
                .collect()
        }
        other => vec![other],
    }
}

/// Splits a pattern into top-level alternatives that are planned (and, when
/// they need emulated look-arounds, searched) independently, which keeps
/// their look-arounds exact. Nested non-capturing alternations are
/// flattened; a capturing or flagged group around an alternation with
/// look-arounds in its branches is distributed (`((?=b)|a)` → `((?=b))|(a)`,
/// both numbered as the same group), and so is such an alternation preceded
/// by zero-width items only (`(?<!x)((?<=b)c|a)d` →
/// `(?<!x)((?<=b)c)d|(?<!x)(a)d`): the order in which paths are tried is
/// unchanged.
fn top_branches(node: Node, out: &mut Vec<Node>) {
    match node {
        Node::Alt(branches) => branches.into_iter().for_each(|b| top_branches(b, out)),
        Node::Group(g) if g.capture.is_none() && g.flags.is_empty() => top_branches(g.node, out),
        node if is_alt_group_with_look(&node) => {
            distribute_groups(node)
                .into_iter()
                .for_each(|b| top_branches(b, out));
        }
        Node::Concat(mut items) => {
            // `Z(A|B)C` → `ZAC|ZBC` keeps every path's priority when `Z` is
            // zero-width (whatever `C` is).
            let alt_at = items.iter().position(is_alt_group_with_look);
            let distributable = alt_at.is_some_and(|i| items[..i].iter().all(zero_width));
            match alt_at {
                Some(i) if distributable => {
                    let group = items.remove(i);
                    for b in distribute_groups(group) {
                        let mut seq = items.clone();
                        seq.splice(i..i, into_items(b));
                        top_branches(Node::Concat(seq), out);
                    }
                }
                _ => out.push(Node::Concat(items)),
            }
        }
        other => out.push(other),
    }
}

/// Rewrites each run of two or more consecutive zero-width alternatives
/// containing look-arounds (`(?<=\.\.\.)|(?<!\.)`) into one look-ahead over
/// the run. A marker check follows the regex's preferred path, which among
/// empty alternatives is always the first; as one look-ahead the run is
/// checked exactly, each alternative becoming its own branch.
///
/// Look-around bodies are left alone: they are compiled by their own
/// [`Matcher`], which splits their top-level alternation (folding it here
/// would wrap the same alternation again on every level).
fn fold(node: Node) -> Node {
    match node {
        Node::Group(mut g) => {
            g.node = fold(std::mem::replace(&mut g.node, Node::Empty));
            Node::Group(g)
        }
        Node::Concat(items) => Node::Concat(items.into_iter().map(fold).collect()),
        Node::Alt(branches) => {
            let mut out = Vec::new();
            let mut run = Vec::new();
            for b in branches.into_iter().map(fold) {
                if zero_width(&b) {
                    run.push(b);
                } else {
                    flush_zero_width_run(&mut run, &mut out);
                    out.push(b);
                }
            }
            flush_zero_width_run(&mut run, &mut out);
            alt_of(out)
        }
        Node::Repeat(mut r) => {
            let inner = fold(std::mem::replace(&mut r.node, Node::Empty));
            r.node = if r.min == 0 {
                prune_zero_width(inner)
            } else {
                inner
            };
            if matches!(r.node, Node::Empty) {
                return Node::Empty;
            }
            Node::Repeat(r)
        }
        other => other,
    }
}

/// Inside a repetition that may match zero times, an alternative that never
/// consumes can only produce what skipping the repetition produces: drop
/// such alternatives when they need look-around emulation (only zero-length
/// or look-around captures are lost).
fn prune_zero_width(node: Node) -> Node {
    if zero_width(&node) && contains_look(&node) {
        return Node::Empty;
    }
    match node {
        Node::Group(mut g) => {
            g.node = prune_zero_width(std::mem::replace(&mut g.node, Node::Empty));
            Node::Group(g)
        }
        Node::Alt(branches) => {
            let kept: Vec<Node> = branches
                .into_iter()
                .filter(|b| !(zero_width(b) && contains_look(b)))
                .collect();
            if kept.is_empty() {
                Node::Empty
            } else {
                alt_of(kept)
            }
        }
        other => other,
    }
}

fn flush_zero_width_run(run: &mut Vec<Node>, out: &mut Vec<Node>) {
    if run.len() >= 2 && run.iter().any(contains_look) {
        let node = Node::Alt(std::mem::take(run));
        out.push(Node::Look(Box::new(Look {
            ahead: true,
            negate: false,
            node,
        })));
    } else {
        out.append(run);
    }
}

struct Plan {
    core: Node,
    leading: Vec<Look>,
}

fn plan_branch(node: Node) -> Plan {
    let mut items = into_items(node);
    tailify(&mut items);
    let mut leading = Vec::new();
    peel_leading(&mut items, &mut leading);
    Plan {
        core: Node::Concat(items),
        leading,
    }
}

/// Rewrites the trailing look-arounds of a sequence: the first look-ahead
/// that can be consumed becomes a [`Node::Tail`] (the match is cut where it
/// starts) and the others stay as markers in front of it. Descends into a
/// final group, a final optional repetition, each branch of a final
/// alternation, and the tail's own body.
fn tailify(items: &mut Vec<Node>) {
    let count = items
        .iter()
        .rev()
        .take_while(|n| matches!(n, Node::Look(_)))
        .count();
    if count > 0 {
        let looks: Vec<Look> = items
            .drain(items.len() - count..)
            .filter_map(|n| match n {
                Node::Look(l) => Some(*l),
                _ => None,
            })
            .collect();
        let mut tail = None;
        for look in looks {
            let look = if tail.is_none() && look.ahead {
                match into_tail(look) {
                    Ok(body) => {
                        tail = Some(body);
                        continue;
                    }
                    Err(look) => look,
                }
            } else {
                look
            };
            items.push(Node::Look(Box::new(look)));
        }
        if let Some(body) = tail {
            // The body joins this regex, so fold its own zero-width runs.
            let mut body_items = into_items(fold(body));
            tailify(&mut body_items);
            items.push(Node::Tail(Box::new(Node::Concat(body_items))));
        }
        return;
    }
    let target = match items.last_mut() {
        Some(Node::Group(g)) => &mut g.node,
        Some(Node::Repeat(r)) if r.max == Some(1) => &mut r.node,
        _ => return,
    };
    if !contains_look(target) {
        return;
    }
    let tailified = |node: Node| {
        let mut inner = into_items(node);
        tailify(&mut inner);
        Node::Concat(inner)
    };
    *target = match std::mem::replace(target, Node::Empty) {
        Node::Alt(branches) => Node::Alt(branches.into_iter().map(tailified).collect()),
        other => tailified(other),
    };
}

/// The consumable form of a look-ahead, if it has one: the body of a positive
/// look-ahead (a zero-width body with look-arounds through [`tail_form`]), or
/// `[^x]|\z` for `(?!x)` with a single-character `x`.
fn into_tail(look: Look) -> Result<Node, Look> {
    if look.negate {
        return match negate_single(&look.node) {
            Some(negated) => Ok(Node::Alt(vec![negated, Node::Assert(Assert::TextEnd)])),
            None => Err(look),
        };
    }
    if zero_width(&look.node) && contains_look(&look.node) {
        return tail_form(&look.node).ok_or(look);
    }
    Ok(look.node)
}

/// A consumable equivalent of a zero-width node: look-aheads convert as in
/// [`into_tail`], assertions stay, alternations and non-capturing groups
/// convert branch by branch, and a sequence may be assertions followed by one
/// convertible item.
fn tail_form(node: &Node) -> Option<Node> {
    match node {
        Node::Look(l) if l.ahead => into_tail((**l).clone()).ok(),
        Node::Assert(_) | Node::Empty => Some(node.clone()),
        Node::Alt(branches) => branches
            .iter()
            .map(tail_form)
            .collect::<Option<Vec<_>>>()
            .map(Node::Alt),
        Node::Group(g) if g.capture.is_none() => tail_form(&g.node).map(|n| {
            Node::Group(Box::new(Group {
                capture: None,
                flags: g.flags,
                node: n,
            }))
        }),
        Node::Concat(items) => {
            let (last, init) = items.split_last()?;
            if !init.iter().all(|n| matches!(n, Node::Assert(_))) {
                return None;
            }
            let mut out = init.to_vec();
            out.push(tail_form(last)?);
            Some(Node::Concat(out))
        }
        _ => None,
    }
}

/// Moves look-arounds that precede the first consuming item into `out`
/// (descending into a leading unquantified group).
fn peel_leading(items: &mut Vec<Node>, out: &mut Vec<Look>) {
    let mut i = 0;
    while i < items.len() {
        match &mut items[i] {
            Node::Look(_) => {
                if let Node::Look(look) = items.remove(i) {
                    out.push(*look);
                }
            }
            node if zero_width(node) && !contains_look(node) => i += 1,
            Node::Group(g) if !matches!(g.node, Node::Alt(_)) => {
                let inner = std::mem::replace(&mut g.node, Node::Empty);
                g.node = Node::Concat(into_items(inner));
                if let Node::Concat(inner_items) = &mut g.node {
                    peel_leading(inner_items, out);
                }
                break;
            }
            _ => break,
        }
    }
}

/// `(?!x)` for a single-character `x` as the class `[^x]`.
/// Also handles alternations of single characters (`(?!\(|\.)`); captures in
/// a negative look-ahead are never reported, so capture groups are fine.
fn negate_single(node: &Node) -> Option<Node> {
    match node {
        Node::Class(c) => Some(Node::Class(Class {
            negated: !c.negated,
            items: c.items.clone(),
        })),
        Node::Group(g) if !g.flags.is_empty() => negate_single(&g.node).map(|n| {
            Node::Group(Box::new(Group {
                capture: None,
                flags: g.flags,
                node: n,
            }))
        }),
        other => class_items(other).map(|items| {
            Node::Class(Class {
                negated: true,
                items,
            })
        }),
    }
}

/// Class items matching exactly the characters a single-character node
/// matches.
fn class_items(node: &Node) -> Option<String> {
    match node {
        Node::Char(c) => {
            let mut items = String::new();
            onig::push_class_char(&mut items, *c);
            Some(items)
        }
        Node::Class(c) if c.negated => Some(c.to_bracket()),
        Node::Class(c) => Some(c.items.clone()),
        Node::Group(g) if g.flags.is_empty() => class_items(&g.node),
        Node::Alt(branches) => branches
            .iter()
            .map(class_items)
            .collect::<Option<Vec<_>>>()
            .map(|v| v.concat()),
        _ => None,
    }
}

fn contains_look(node: &Node) -> bool {
    match node {
        Node::Look(_) => true,
        Node::Group(g) => contains_look(&g.node),
        Node::Concat(items) | Node::Alt(items) => items.iter().any(contains_look),
        Node::Repeat(r) => contains_look(&r.node),
        Node::Tail(t) => contains_look(t),
        _ => false,
    }
}

/// Whether `which` occurs in `node` outside look-around bodies (those are
/// compiled separately).
fn contains_assert(node: &Node, which: Assert) -> bool {
    match node {
        Node::Assert(a) => *a == which,
        Node::Group(g) => contains_assert(&g.node, which),
        Node::Concat(items) | Node::Alt(items) => items.iter().any(|n| contains_assert(n, which)),
        Node::Repeat(r) => contains_assert(&r.node, which),
        Node::Tail(t) => contains_assert(t, which),
        _ => false,
    }
}

fn has_any_assert(node: &Node) -> bool {
    match node {
        Node::Assert(_) | Node::Look(_) | Node::Raw(_) | Node::Keep => true,
        Node::Group(g) => has_any_assert(&g.node),
        Node::Concat(items) | Node::Alt(items) => items.iter().any(has_any_assert),
        Node::Repeat(r) => has_any_assert(&r.node),
        Node::Tail(t) => has_any_assert(t),
        _ => false,
    }
}

/// Whether a node never consumes characters.
fn zero_width(node: &Node) -> bool {
    match node {
        Node::Empty | Node::Assert(_) | Node::Look(_) | Node::Keep | Node::Tail(_) => true,
        // Calls / back-references never survive `onig::parse`.
        Node::Char(_)
        | Node::Dot
        | Node::Class(_)
        | Node::Raw(_)
        | Node::Call(_)
        | Node::BackRef(_) => false,
        Node::Group(g) => zero_width(&g.node),
        Node::Concat(items) | Node::Alt(items) => items.iter().all(zero_width),
        Node::Repeat(r) => r.max == Some(0) || zero_width(&r.node),
    }
}

/// Maximum length in characters, if bounded.
fn max_len(node: &Node) -> Option<usize> {
    match node {
        Node::Empty | Node::Assert(_) | Node::Look(_) | Node::Keep => Some(0),
        Node::Char(_) | Node::Dot | Node::Class(_) => Some(1),
        Node::Raw(_) | Node::Tail(_) | Node::Call(_) | Node::BackRef(_) => None,
        Node::Group(g) => max_len(&g.node),
        Node::Concat(items) => items
            .iter()
            .try_fold(0usize, |acc, n| acc.checked_add(max_len(n)?)),
        Node::Alt(items) => items
            .iter()
            .try_fold(0usize, |acc, n| Some(acc.max(max_len(n)?))),
        Node::Repeat(r) => r
            .max
            .and_then(|m| max_len(&r.node)?.checked_mul(m as usize)),
    }
}

fn build_regex(source: &str) -> Result<Regex, String> {
    RegexBuilder::new(source)
        .multi_line(true)
        .build()
        .map_err(|e| {
            let msg = e.to_string();
            msg.lines()
                .last()
                .unwrap_or("invalid regex")
                .trim()
                .to_string()
        })
}

/// Where `\G` may be satisfied by the empty string.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GMode {
    /// `\G` never matches.
    Never,
    /// Before any consuming item (forward patterns).
    AtStart,
    /// After every consuming item (look-behind bodies ending at the position).
    AtEnd,
}

/// Output of translating one core to `regex` syntax.
#[derive(Default)]
struct Emitted {
    source: String,
    /// `(regex group, Oniguruma group)`.
    group_map: Vec<(usize, usize)>,
    /// `(regex group, enclosing Oniguruma groups)` of consumed tails.
    tails: Vec<(usize, Vec<usize>)>,
    /// Regex groups of `\K` markers.
    keeps: Vec<usize>,
    /// `(regex group, look-around)` checked at the marker's position.
    markers: Vec<(usize, Look)>,
}

struct Emitter {
    allow_a: bool,
    g: GMode,
    /// Translating a look-behind body (no `\K`).
    behind: bool,
    next_group: usize,
    enclosing: Vec<usize>,
    /// Nesting inside repetitions that may iterate more than once.
    repeat_depth: usize,
    out: Emitted,
}

fn emit(node: &Node, allow_a: bool, g: GMode, behind: bool) -> Result<Emitted, String> {
    let mut e = Emitter {
        allow_a,
        g,
        behind,
        next_group: 1,
        enclosing: Vec::new(),
        repeat_depth: 0,
        out: Emitted::default(),
    };
    e.node(node, true, true)?;
    Ok(e.out)
}

impl Emitter {
    fn push(&mut self, s: &str) {
        self.out.source.push_str(s);
    }

    fn open_capture(&mut self) -> usize {
        let index = self.next_group;
        self.next_group += 1;
        index
    }

    fn node(&mut self, node: &Node, at_start: bool, at_end: bool) -> Result<(), String> {
        match node {
            Node::Call(_) | Node::BackRef(_) => return Err("unexpanded group reference".into()),
            Node::Empty => self.push("(?:)"),
            Node::Char(c) => {
                let mut buf = [0u8; 4];
                let escaped = regex::escape(c.encode_utf8(&mut buf));
                self.push(&escaped);
            }
            Node::Dot => self.push("."),
            Node::Class(c) => self.push(&c.to_bracket()),
            Node::Raw(r) => self.push(r),
            Node::Assert(a) => {
                let s = match a {
                    Assert::LineStart => r"\A",
                    Assert::LineEnd | Assert::TextEndNewline => "$",
                    Assert::TextStart if self.allow_a => r"\A",
                    Assert::TextStart => NEVER,
                    Assert::TextEnd => r"\z",
                    Assert::WordBoundary => r"\b",
                    Assert::NotWordBoundary => r"\B",
                    Assert::SearchStart => match self.g {
                        GMode::AtStart if at_start => "(?:)",
                        GMode::AtEnd if at_end => "(?:)",
                        _ => NEVER,
                    },
                };
                self.push(s);
            }
            Node::Group(g) => {
                match g.capture {
                    Some(k) => {
                        let index = self.open_capture();
                        self.out.group_map.push((index, k));
                        self.push("(");
                        self.enclosing.push(k);
                        self.flagged(g, at_start, at_end)?;
                        self.enclosing.pop();
                    }
                    None => {
                        self.push("(?:");
                        self.flagged(g, at_start, at_end)?;
                    }
                }
                self.push(")");
            }
            Node::Concat(items) => {
                // `rest_zero[i]`: every item after `i` is zero-width.
                let mut rest_zero = vec![true; items.len()];
                for i in (0..items.len().saturating_sub(1)).rev() {
                    rest_zero[i] = rest_zero[i + 1] && zero_width(&items[i + 1]);
                }
                let mut start = at_start;
                for (item, rest) in items.iter().zip(rest_zero) {
                    self.node(item, start, at_end && rest)?;
                    if !zero_width(item) {
                        start = false;
                    }
                }
            }
            Node::Alt(branches) => {
                self.push("(?:");
                for (i, b) in branches.iter().enumerate() {
                    if i > 0 {
                        self.push("|");
                    }
                    self.node(b, at_start, at_end)?;
                }
                self.push(")");
            }
            Node::Repeat(r) => {
                let iterates = r.max != Some(0) && r.max != Some(1);
                self.repeat_depth += usize::from(iterates);
                self.push("(?:");
                let inner = self.node(&r.node, at_start, at_end);
                self.repeat_depth -= usize::from(iterates);
                inner?;
                self.push(")");
                let q = match (r.min, r.max) {
                    (0, None) => "*".to_string(),
                    (1, None) => "+".to_string(),
                    (0, Some(1)) => "?".to_string(),
                    (n, None) => format!("{{{n},}}"),
                    (n, Some(m)) if n == m => format!("{{{n}}}"),
                    (n, Some(m)) => format!("{{{n},{m}}}"),
                };
                self.push(&q);
                if !r.greedy {
                    self.push("?");
                }
            }
            Node::Look(look) => {
                if self.repeat_depth > 0 {
                    return Err("look-around inside a repetition is not supported".into());
                }
                let index = self.open_capture();
                self.out.markers.push((index, (**look).clone()));
                self.push("()");
            }
            Node::Keep => {
                if self.behind {
                    return Err(r"\K inside look-behind is not supported".into());
                }
                let index = self.open_capture();
                self.out.keeps.push(index);
                self.push("()");
            }
            Node::Tail(body) => {
                let index = self.open_capture();
                self.out.tails.push((index, self.enclosing.clone()));
                self.push("(");
                self.node(body, at_start, false)?;
                self.push(")");
            }
        }
        Ok(())
    }

    /// Group content wrapped in its inline flags, if any.
    fn flagged(&mut self, g: &Group, at_start: bool, at_end: bool) -> Result<(), String> {
        if g.flags.is_empty() {
            return self.node(&g.node, at_start, at_end);
        }
        let flags = g.flags.to_regex();
        self.push("(?");
        self.push(&flags);
        self.push(":");
        self.node(&g.node, at_start, at_end)?;
        self.push(")");
        Ok(())
    }
}

/// Lazily compiled variants indexed by `allow_a as usize | (g_on as usize) << 1`.
type Variants<T> = [OnceLock<Option<T>>; 4];

fn variant_index(allow_a: bool, g_on: bool) -> usize {
    usize::from(allow_a) | (usize::from(g_on) << 1)
}

struct Branch {
    core: Node,
    variants: Variants<Regex>,
    /// Anchored forms for [`Branch::search_at`], indexed by
    /// `variant_index | (after_char as usize) << 2`.
    anchored: [OnceLock<Option<Regex>>; 8],
    uses_a: bool,
    uses_g: bool,
    /// The core regex itself contains `\A` / `\G` (not just a look-around
    /// body); otherwise the variants for them are the same regex.
    core_a: bool,
    core_g: bool,
    /// Number of Oniguruma groups (group 0 excluded).
    groups: usize,
    group_map: Vec<(usize, usize)>,
    tails: Vec<(usize, Vec<usize>)>,
    keeps: Vec<usize>,
    /// `(regex group, check)` for look-arounds left inside the core.
    markers: Vec<(usize, Check)>,
    /// Look-arounds before the first consuming item, checked at the start.
    leading: Vec<Check>,
}

impl Branch {
    /// `anchored_only`: the branch is only matched at given positions (a
    /// look-ahead body), so the unanchored regex is never needed.
    fn new(
        core: Node,
        leading: Vec<Look>,
        groups: usize,
        anchored_only: bool,
    ) -> Result<Branch, String> {
        let emitted = emit(&core, false, GMode::Never, false)?;
        // Compile one form up front: an unsupported pattern fails here.
        let variants: Variants<Regex> = Default::default();
        let anchored: [OnceLock<Option<Regex>>; 8] = Default::default();
        if anchored_only {
            let regex = build_regex(&format!(r"\A(?s:.)(?:{})", emitted.source))?;
            let _ = anchored[1 << 2].set(Some(regex));
        } else {
            let _ = variants[0].set(Some(build_regex(&emitted.source)?));
        }
        let markers = emitted
            .markers
            .into_iter()
            .map(|(index, look)| Ok((index, Check::new(look, groups)?)))
            .collect::<Result<Vec<_>, String>>()?;
        let leading = leading
            .into_iter()
            .map(|l| Check::new(l, groups))
            .collect::<Result<Vec<_>, _>>()?;
        let checks = || leading.iter().chain(markers.iter().map(|(_, c)| c));
        let core_a = contains_assert(&core, Assert::TextStart);
        let core_g = contains_assert(&core, Assert::SearchStart);
        let uses_a = core_a || checks().any(Check::uses_a);
        let uses_g = core_g || checks().any(Check::uses_g);
        Ok(Branch {
            core,
            variants,
            anchored,
            uses_a,
            uses_g,
            core_a,
            core_g,
            groups,
            group_map: emitted.group_map,
            tails: emitted.tails,
            keeps: emitted.keeps,
            markers,
            leading,
        })
    }

    fn regex(&self, allow_a: bool, g_on: bool) -> Option<&Regex> {
        let (allow_a, g_on) = (allow_a && self.core_a, g_on && self.core_g);
        let g = if g_on { GMode::AtStart } else { GMode::Never };
        self.variants[variant_index(allow_a, g_on)]
            .get_or_init(|| {
                emit(&self.core, allow_a, g, false)
                    .ok()
                    .and_then(|e| build_regex(&e.source).ok())
            })
            .as_ref()
    }

    /// The core anchored at the start of the haystack, after one character
    /// (`after_char`, which gives `\b` its left context) or at text start.
    fn anchored_regex(&self, allow_a: bool, g_on: bool, after_char: bool) -> Option<&Regex> {
        let (allow_a, g_on) = (allow_a && self.core_a, g_on && self.core_g);
        let g = if g_on { GMode::AtStart } else { GMode::Never };
        let prefix = if after_char { r"\A(?s:.)" } else { r"\A" };
        self.anchored[variant_index(allow_a, g_on) | (usize::from(after_char) << 2)]
            .get_or_init(|| {
                let emitted = emit(&self.core, allow_a, g, false).ok()?;
                build_regex(&format!("{prefix}(?:{})", emitted.source)).ok()
            })
            .as_ref()
    }

    fn search(&self, text: &str, from: usize, ctx: SearchCtx, want: bool) -> Option<Hit> {
        if from > text.len() {
            return None;
        }
        // `\G` only matches at `from`: try the variant where it is empty there.
        let g_here = self.core_g && ctx.g_pos == Some(from);
        if g_here {
            if let Some(hit) = self.search_at(text, from, ctx, want, true) {
                return Some(hit);
            }
        }
        let re = self.regex(ctx.allow_a, false)?;
        if self.leading.is_empty() && self.markers.is_empty() {
            return self.run(re, text, from, (0, 0), want).map(|(hit, _)| hit);
        }
        // Candidate starts in order: the leftmost core match first, then
        // every later position where the core can start and the leading
        // look-arounds hold (cheap tests first, so failing candidates cost
        // little even when the core's matches are long).
        let mut pos = re.find_at(text, from)?.start();
        loop {
            if self.leading.iter().all(|c| c.holds(text, pos, ctx)) {
                if let Some(hit) = self.search_at(text, pos, ctx, want, false) {
                    return Some(hit);
                }
            }
            loop {
                pos = next_boundary(text, pos)?;
                if self.can_start_at(text, pos, ctx) {
                    break;
                }
            }
        }
    }

    /// Whether the core (ignoring look-arounds) matches at `pos`.
    fn can_start_at(&self, text: &str, pos: usize, ctx: SearchCtx) -> bool {
        let lead = text[..pos].chars().next_back().map_or(0, char::len_utf8);
        self.anchored_regex(ctx.allow_a, false, lead > 0)
            .is_some_and(|re| re.is_match(&text[pos - lead..]))
    }

    /// The match starting exactly at `pos` (a char boundary), found without
    /// scanning past it. `g_on`: `\G` matches at `pos`.
    fn search_at(
        &self,
        text: &str,
        pos: usize,
        ctx: SearchCtx,
        want: bool,
        g_on: bool,
    ) -> Option<Hit> {
        let lead = text
            .get(..pos)?
            .chars()
            .next_back()
            .map_or(0, char::len_utf8);
        let hay_start = pos - lead;
        let re = self.anchored_regex(ctx.allow_a, g_on && self.core_g, lead > 0)?;
        let (mut hit, marks) = self.run(re, &text[hay_start..], 0, (hay_start, lead), want)?;
        self.verify(text, &mut hit, &marks, ctx, want)
            .then_some(hit)
    }

    /// Evaluates the emulated look-arounds of a candidate match.
    fn verify(
        &self,
        text: &str,
        hit: &mut Hit,
        marks: &[Option<usize>],
        ctx: SearchCtx,
        want: bool,
    ) -> bool {
        let groups = &mut hit.groups;
        self.leading
            .iter()
            .all(|c| c.eval(text, hit.raw_start, ctx, want, groups))
            && self
                .markers
                .iter()
                .zip(marks)
                .all(|((_, c), pos)| pos.is_none_or(|pos| c.eval(text, pos, ctx, want, groups)))
    }

    /// Runs a core regex on `hay` (which starts at byte `offset` of the
    /// text) from `from`; returns the hit (in text positions) and the marker
    /// positions. `lead` bytes of context consumed by an
    /// [`Branch::anchored_regex`] precede the real start.
    fn run(
        &self,
        re: &Regex,
        hay: &str,
        from: usize,
        (offset, lead): (usize, usize),
        want: bool,
    ) -> Option<(Hit, Vec<Option<usize>>)> {
        let need_locs =
            want || !self.tails.is_empty() || !self.keeps.is_empty() || !self.markers.is_empty();
        let at = |i: usize| i + offset;
        if !need_locs {
            let m = re.find_at(hay, from)?;
            let raw_start = at(m.start() + lead);
            let hit = Hit {
                raw_start,
                start: raw_start,
                end: at(m.end()),
                groups: Vec::new(),
            };
            return Some((hit, Vec::new()));
        }
        let mut locs = re.capture_locations();
        let m = re.captures_read_at(&mut locs, hay, from)?;
        let raw_start = at(m.start() + lead);
        // Tails are emitted outermost first; the first participating one is
        // where the reported match ends.
        let end = self
            .tails
            .iter()
            .find_map(|(index, _)| locs.get(*index).map(|(s, _)| at(s)))
            .unwrap_or(at(m.end()));
        let start = self
            .keeps
            .iter()
            .filter_map(|index| locs.get(*index).map(|(s, _)| at(s)))
            .max()
            .map_or(raw_start, |k| k.min(end));
        let mut groups = Vec::new();
        if want {
            groups = vec![None; self.groups + 1];
            groups[0] = Some((start, end));
            for (index, k) in &self.group_map {
                if let (Some((s, e)), Some(slot)) = (locs.get(*index), groups.get_mut(*k)) {
                    // Several regex groups may stand for one Oniguruma group
                    // (inlined calls, distributed alternatives): report the
                    // one that completes last, outermost on ties.
                    let span = (at(s), at(e));
                    if slot.is_none_or(|old| span.1 > old.1 || (span.1 == old.1 && span.0 < old.0))
                    {
                        *slot = Some(span);
                    }
                }
            }
            // A group enclosing a consumed look-ahead ends where it starts.
            for (index, enclosing) in &self.tails {
                let Some((cut, _)) = locs.get(*index) else {
                    continue;
                };
                for k in enclosing {
                    if let Some(Some((s, e))) = groups.get_mut(*k) {
                        *e = (*e).min(at(cut));
                        *s = (*s).min(*e);
                    }
                }
            }
        }
        let marks = self
            .markers
            .iter()
            .map(|(index, _)| locs.get(*index).map(|(s, _)| at(s)))
            .collect();
        Some((
            Hit {
                raw_start,
                start,
                end,
                groups,
            },
            marks,
        ))
    }
}

fn next_boundary(text: &str, i: usize) -> Option<usize> {
    text.get(i..)?.chars().next().map(|c| i + c.len_utf8())
}

/// Copies captures found by a positive look-around into `groups`.
fn merge_groups(groups: &mut Groups, found: Groups) {
    for (k, span) in found.into_iter().enumerate().skip(1) {
        if let (Some(span), Some(slot @ None)) = (span, groups.get_mut(k)) {
            *slot = Some(span);
        }
    }
}

/// An emulated look-around evaluated at a position.
struct Check {
    negate: bool,
    kind: CheckKind,
}

enum CheckKind {
    Ahead(Box<Matcher>),
    Behind(Box<Behind>),
}

impl Check {
    fn new(look: Look, groups: usize) -> Result<Check, String> {
        let kind = if look.ahead {
            CheckKind::Ahead(Box::new(Matcher::from_node(look.node, groups, true)?))
        } else {
            CheckKind::Behind(Box::new(Behind::new(look.node, groups)?))
        };
        Ok(Check {
            negate: look.negate,
            kind,
        })
    }

    fn uses_a(&self) -> bool {
        match &self.kind {
            CheckKind::Ahead(m) => m.uses_a,
            CheckKind::Behind(b) => b.uses_a,
        }
    }

    fn uses_g(&self) -> bool {
        match &self.kind {
            CheckKind::Ahead(m) => m.uses_g,
            CheckKind::Behind(b) => b.uses_g,
        }
    }

    /// Whether the look-around holds at `pos` (captures not needed).
    fn holds(&self, text: &str, pos: usize, ctx: SearchCtx) -> bool {
        self.eval(text, pos, ctx, false, &mut Vec::new())
    }

    /// Whether the look-around holds at `pos`; captures of a positive one are
    /// merged into `groups` when `want` is set.
    fn eval(
        &self,
        text: &str,
        pos: usize,
        ctx: SearchCtx,
        want: bool,
        groups: &mut Groups,
    ) -> bool {
        let want = want && !self.negate;
        let found = match &self.kind {
            CheckKind::Ahead(m) => m.match_at(text, pos, ctx, want).map(|h| h.groups),
            CheckKind::Behind(b) => b.match_at(text, pos, ctx, want),
        };
        match found {
            Some(found) if !self.negate => {
                if want {
                    merge_groups(groups, found);
                }
                true
            }
            Some(_) => false,
            None => self.negate,
        }
    }
}

/// A look-behind body, run on the text before the position.
struct Behind {
    node: Node,
    /// `(body followed by one character, body at the end)` per variant.
    variants: Variants<(Regex, Regex)>,
    group_map: Vec<(usize, usize)>,
    /// Nested look-arounds, checked at their positions in the body's match
    /// (exact for fixed-length bodies).
    markers: Vec<(usize, Check)>,
    groups: usize,
    /// Characters to look back when the body is bounded and has no
    /// assertions (otherwise the whole prefix is searched).
    window: Option<usize>,
    uses_a: bool,
    uses_g: bool,
}

impl Behind {
    fn new(node: Node, groups: usize) -> Result<Behind, String> {
        let emitted = emit(&node, false, GMode::Never, true)?;
        let pair = Behind::build(&emitted.source)?;
        let variants: Variants<(Regex, Regex)> = Default::default();
        let _ = variants[0].set(Some(pair));
        let markers = emitted
            .markers
            .into_iter()
            .map(|(index, look)| Ok((index, Check::new(look, groups)?)))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Behind {
            window: if has_any_assert(&node) {
                None
            } else {
                max_len(&node)
            },
            uses_a: contains_assert(&node, Assert::TextStart)
                || markers.iter().any(|(_, c)| c.uses_a()),
            uses_g: contains_assert(&node, Assert::SearchStart)
                || markers.iter().any(|(_, c)| c.uses_g()),
            node,
            variants,
            group_map: emitted.group_map,
            markers,
            groups,
        })
    }

    fn build(body: &str) -> Result<(Regex, Regex), String> {
        Ok((
            build_regex(&format!(r"(?:{body})(?s:.)\z"))?,
            build_regex(&format!(r"(?:{body})\z"))?,
        ))
    }

    fn regexes(&self, allow_a: bool, g_on: bool) -> Option<&(Regex, Regex)> {
        let g = if g_on { GMode::AtEnd } else { GMode::Never };
        self.variants[variant_index(allow_a, g_on)]
            .get_or_init(|| {
                emit(&self.node, allow_a, g, true)
                    .ok()
                    .and_then(|e| Behind::build(&e.source).ok())
            })
            .as_ref()
    }

    /// Captures (empty unless `want`) when the body matches text ending at
    /// `pos`.
    fn match_at(&self, text: &str, pos: usize, ctx: SearchCtx, want: bool) -> Option<Groups> {
        let before = text.get(..pos)?;
        let start = match self.window {
            Some(0) => pos,
            Some(n) => before.char_indices().rev().nth(n - 1).map_or(0, |(i, _)| i),
            None => 0,
        };
        let (with_next, at_end) = self.regexes(
            self.uses_a && ctx.allow_a,
            self.uses_g && ctx.g_pos == Some(pos),
        )?;
        let (re, hay) = match text[pos..].chars().next() {
            Some(c) => (with_next, &text[start..pos + c.len_utf8()]),
            None => (at_end, &text[start..pos]),
        };
        if !want && self.markers.is_empty() {
            return re.is_match(hay).then(Vec::new);
        }
        let mut locs = re.capture_locations();
        re.captures_read(&mut locs, hay)?;
        let mut groups = if want {
            vec![None; self.groups + 1]
        } else {
            Vec::new()
        };
        for (index, k) in &self.group_map {
            if let (Some((s, e)), Some(slot)) = (locs.get(*index), groups.get_mut(*k)) {
                *slot = Some((start + s, start + e));
            }
        }
        for (index, check) in &self.markers {
            if let Some((s, _)) = locs.get(*index) {
                if !check.eval(text, start + s, ctx, want, &mut groups) {
                    return None;
                }
            }
        }
        Some(groups)
    }
}

/// Whether an `end`/`while` pattern refers to `begin` captures (`\1`…).
pub(crate) fn has_back_references(source: &str) -> bool {
    let mut chars = source.chars();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.next().is_some_and(|d| d.is_ascii_digit()) {
            return true;
        }
    }
    false
}

/// Substitutes `\N` in an `end`/`while` pattern with the (escaped) text of
/// `begin` capture `N`, as TextMate does.
pub(crate) fn resolve_back_references(
    source: &str,
    text: &str,
    groups: &[Option<(usize, usize)>],
) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        if chars.peek().is_some_and(|d| d.is_ascii_digit()) {
            let mut n = 0usize;
            while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
                n = n.saturating_mul(10).saturating_add(d as usize);
                chars.next();
            }
            let captured = groups
                .get(n)
                .copied()
                .flatten()
                .and_then(|(s, e)| text.get(s..e))
                .unwrap_or("");
            escape_literal(captured, &mut out);
        } else {
            out.push('\\');
            if let Some(next) = chars.next() {
                out.push(next);
            }
        }
    }
    out
}

/// Escapes text so that it matches literally in an Oniguruma pattern.
pub(crate) fn escape_literal(text: &str, out: &mut String) {
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if c.is_whitespace() || c.is_control() {
            out.push_str(&format!("\\x{{{:X}}}", c as u32));
        } else if c.is_ascii() {
            out.push('\\');
            out.push(c);
        } else {
            out.push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(src: &str) -> Matcher {
        Matcher::new(src).unwrap_or_else(|e| panic!("{src:?}: {e}"))
    }

    /// `(start, end)` of the first match at or after `from`.
    fn find(src: &str, text: &str, from: usize) -> Option<(usize, usize)> {
        m(src)
            .search(text, from, SearchCtx::default(), false)
            .map(|h| (h.start, h.end))
    }

    fn groups(src: &str, text: &str) -> Groups {
        m(src)
            .search(text, 0, SearchCtx::default(), true)
            .expect("match")
            .groups
    }

    #[test]
    fn plain_patterns_and_translations() {
        assert_eq!(find(r"\bif\b", "x if y", 0), Some((2, 4)));
        assert_eq!(find(r"\h+", "zz0aFg", 0), Some((2, 5)));
        assert_eq!(find(r"[[:alpha:]]+", "1 héllo", 0), Some((2, 8)));
        assert_eq!(find(r"a{,2}b", "aaab", 0), Some((1, 4)));
        assert_eq!(find(r"\d++x", "12x", 0), Some((0, 3)));
        assert_eq!(find(r"(?>ab|a)c", "abc", 0), Some((0, 3)));
        assert_eq!(find(r"(?x) a \s b  # comment", "a b", 0), Some((0, 3)));
        assert_eq!(find(r"(?i)select", "SeLeCt", 0), Some((0, 6)));
        // Oniguruma `(?m)` is dot-all.
        assert_eq!(find(r"(?m)a.b", "a\nb", 0), Some((0, 3)));
        assert_eq!(find(r"a.b", "a\nb", 0), None);
        // `$` matches before the line's trailing newline.
        assert_eq!(find(r"x$", "x\n", 0), Some((0, 1)));
        assert_eq!(find(r"\e\[0m", "\x1b[0m", 0), Some((0, 4)));
        assert_eq!(find(r"<", "a<b", 0), Some((1, 2)));
        // Code point lists, possessive intervals, literal `\p`.
        assert_eq!(find(r"\x{41 42}+", "xABBB", 0), Some((1, 5)));
        assert_eq!(find(r"[\x{41 43}]+", "BACD", 0), Some((1, 3)));
        assert_eq!(find(r"[\x{41-43}]+", "ABCD", 0), Some((0, 3)));
        assert_eq!(find(r"a{2,0}", "aaa", 0), Some((0, 2)));
        assert_eq!(find(r"\pL", "pL", 0), Some((0, 2)));
        assert_eq!(find(r"[\s\p]+", "x p ", 0), Some((1, 4)));
        assert_eq!(
            find(r"[^: \p{Cntrl}\p{Surrogate}]+", ": ab\n", 0),
            Some((2, 4))
        );
        assert_eq!(find(r"\p{Surrogate}", "ab", 0), None);
    }

    #[test]
    fn caret_does_not_match_after_the_final_newline() {
        assert_eq!(find(r"^", "ab\n", 1), None);
        assert_eq!(find(r"^\s*$", "  \n", 0), Some((0, 3)));
        assert_eq!(find(r"\*|^\s*$", "ab\n", 1), None);
    }

    #[test]
    fn capture_groups_keep_oniguruma_numbering() {
        assert_eq!(
            groups(r"(a)(?<n>b)(c)?", "ab"),
            vec![Some((0, 2)), Some((0, 1)), Some((1, 2)), None]
        );
        assert_eq!(groups(r"(a|b)", "b"), vec![Some((0, 1)), Some((0, 1))]);
    }

    #[test]
    fn trailing_lookahead_is_exact() {
        assert_eq!(find(r"\w+(?=\()", "foo bar(", 0), Some((4, 7)));
        // Requires backtracking into the core: `ab1` followed by a digit.
        assert_eq!(find(r"\w+(?=\d)", "ab12", 0), Some((0, 3)));
        assert_eq!(find(r"(?=[A-Z])", "abC", 0), Some((2, 2)));
        // Nested in a trailing capture group: the group is clamped.
        assert_eq!(
            groups(r"(\w+(?=:))", "key: v"),
            vec![Some((0, 3)), Some((0, 3))]
        );
        // Captures inside the look-ahead are still reported.
        assert_eq!(groups(r"a(?=(b))", "ab"), vec![Some((0, 1)), Some((1, 2))]);
        // A look-ahead with nested look-arounds still lets the core backtrack.
        assert_eq!(
            find(r"\w+\s*(?=\s+(?!in\b)\w)", "raise ValueError", 0),
            Some((0, 5))
        );
    }

    #[test]
    fn negative_lookahead_single_class_is_exact() {
        assert_eq!(find(r"\d+(?!\.)", "12.5", 0), Some((0, 1)));
        assert_eq!(find(r"foo(?![\w$])", "foobar foo$ foo", 0), Some((12, 15)));
        assert_eq!(find(r"x(?!y)", "x", 0), Some((0, 1)));
    }

    #[test]
    fn leading_lookbehind_and_lookahead() {
        assert_eq!(find(r"(?<=\.)\w+", "a.b", 0), Some((2, 3)));
        assert_eq!(find(r"(?<!\.)\b\w+", ".ab cd", 0), Some((4, 6)));
        assert_eq!(find(r"\b(?<!\.)\w+", ".ab cd", 0), Some((4, 6)));
        assert_eq!(find(r"(?<=^\s*)def", "  def", 0), Some((2, 5)));
        assert_eq!(find(r"(?!if\b)\b\w+", "if x", 0), Some((3, 4)));
        assert_eq!(find(r"(?<=a)|b", "xb", 0), Some((1, 2)));
        assert_eq!(find(r"((?<=\$)\w+)", "a $b", 0), Some((3, 4)));
        assert_eq!(find(r"((?<=\.)\w+|\d)", "1.a", 0), Some((0, 1)));
    }

    #[test]
    fn nested_lookbehind() {
        // The Rust grammar's variable rule: not after `.`, but after `..`.
        let src = r"\b(?<!(?<!\.)\.)[a-z]+\b";
        assert_eq!(find(src, ".y ..z w", 0), Some((5, 6)));
        assert_eq!(find(src, "x.y", 0), Some((0, 1)));
        assert_eq!(find(src, "x.y", 1), None);
    }

    #[test]
    fn anchored_checks_do_not_scan_ahead() {
        // `(?!...)` evaluated at many positions of a long line stays linear.
        let line = "a".repeat(5000) + "\n";
        let m = m(r"(?<![b])a(?!a*c)");
        let t0 = std::time::Instant::now();
        assert_eq!(
            m.search(&line, 0, SearchCtx::default(), false)
                .map(|h| h.start),
            Some(0)
        );
        let mut pos = 0;
        let mut count = 0;
        while let Some(h) = m.search(&line, pos, SearchCtx::default(), false) {
            pos = h.end;
            count += 1;
        }
        assert_eq!(count, 5000);
        assert!(t0.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn lookbehind_sees_the_right_context() {
        assert_eq!(find(r"x(?<=$)", "xy x\n", 0), Some((3, 4)));
        assert_eq!(find(r"(?<=x$)", "x\n", 0), Some((1, 1)));
        assert_eq!(find(r"(?<!x$)\n", "x\n", 0), None);
        assert_eq!(find(r"(?<=a\b)", "ab a.", 0), Some((4, 4)));
    }

    #[test]
    fn markers_check_lookarounds_inside_the_pattern() {
        assert_eq!(find(r"(\{)(\w+)(?<!\\)(\})", r"{a\} {b}", 0), Some((5, 8)));
        assert_eq!(find(r"a(?=b)\w", "ac ab", 0), Some((3, 5)));
        assert_eq!(find(r"(?:x|(?<=\d))y", "ay 1y", 0), Some((4, 5)));
        // Captures inside positive look-arounds are reported.
        assert_eq!(
            groups(r"(?<=(\w))-", "a-"),
            vec![Some((1, 2)), Some((0, 1))]
        );
        assert_eq!(
            groups(r"(?=(['\x22])|(\n))", "ab\n"),
            vec![Some((2, 2)), None, Some((2, 3))]
        );
    }

    #[test]
    fn trailing_negative_lookahead_check() {
        assert_eq!(find(r"\b\w+\b(?!\s*=)", "a = b", 0), Some((4, 5)));
    }

    #[test]
    fn zero_width_alternations_of_lookarounds_are_exact() {
        // The TypeScript grammar's keyword guard.
        let kw = r"(?<![_$[:alnum:]])(?:(?<=\.\.\.)|(?<!\.))(return)(?![_$[:alnum:]])(?:(?=\.\.\.)|(?!\.))";
        assert_eq!(find(kw, "x.return; return y", 0), Some((10, 16)));
        assert_eq!(find(kw, "...return", 0), Some((3, 9)));
        assert_eq!(find(r"(?:\s+|\b|(?=\W)|(?<=\W))x", "a-x", 0), Some((2, 3)));
    }

    #[test]
    fn tails_inside_final_alternations_and_optionals() {
        assert_eq!(
            find(r"(//).*?((?=</script)|$\n?)", "// a </script>\n", 0),
            Some((0, 5))
        );
        assert_eq!(
            find(r"(//).*?((?=</script)|$\n?)", "// a\n", 0),
            Some((0, 5))
        );
        assert_eq!(
            groups(r"(->)((?:.+?(?=\{|$))?)", "-> T {\n"),
            vec![Some((0, 5)), Some((0, 2)), Some((2, 5))]
        );
        // A tail inside a tail: `\s*` backtracks so `(?!\{)` holds.
        assert_eq!(
            find(r"(?=\bif\s*\(\w\)\s*(?!\{))", "if (x) {", 0),
            Some((0, 0))
        );
        assert_eq!(find(r"(?=\bif\s*\(\w\)\s*(?!\{))", "if (x){", 0), None);
    }

    #[test]
    fn zero_width_alternations_become_tails() {
        assert_eq!(
            groups(r"(,)\s*((?!\S)|(?=\/\/))", "a, b"),
            vec![Some((1, 2)), Some((1, 2)), Some((2, 2))]
        );
        assert_eq!(
            groups(r"\s*(?:$|(?=%>)|(;))", "a\";\n"),
            vec![Some((2, 3)), Some((2, 3))]
        );
        // Zero-width alternatives inside an optional group are skipped.
        assert_eq!(
            find(r"([a-z]\w*|(?<=[\])]))?(\[)(?!\])", "a, [x]", 0),
            Some((3, 4))
        );
        // Flag group around a capture group around an alternation.
        let src = r"(?i:((?<=(\+|=))\s*\b([a-z]+)\b(?!(\(|\.))|\b([a-z]+)\b(?=\s*(\+|=))))";
        assert_eq!(find(src, "x -> V + 1", 0), Some((5, 6)));
        assert_eq!(find(r"\w+(?!\(|\.)", "ab( cd", 0), Some((0, 1)));
    }

    #[test]
    fn nested_alternations_are_flattened() {
        let src = r"(?:(?:(?<=\}|%>)\s*(;)|(;))|(?=[;>]))";
        assert_eq!(groups(src, "x;"), vec![Some((1, 2)), None, Some((1, 2))]);
        assert_eq!(groups(src, "} ;"), vec![Some((1, 3)), Some((2, 3)), None]);
        assert_eq!(
            groups(r"(?:(?=\))|(,))|(?=\n)", "a, b)\n"),
            vec![Some((1, 2)), Some((1, 2))]
        );
    }

    #[test]
    fn search_start_anchor() {
        let g = m(r"\G\s*x");
        let at = |pos| SearchCtx {
            allow_a: false,
            g_pos: Some(pos),
        };
        assert_eq!(
            g.search("a  x", 1, at(1), false).map(|h| (h.start, h.end)),
            Some((1, 4))
        );
        assert_eq!(g.search("a  x", 1, SearchCtx::default(), false), None);
        let not_g = m(r"(?!\G)");
        assert_eq!(
            not_g.search("abc", 1, at(1), false).map(|h| h.start),
            Some(2)
        );
        assert_eq!(
            not_g
                .search("abc", 1, SearchCtx::default(), false)
                .map(|h| h.start),
            Some(1)
        );
        let either = m(r"(?:^|\G)#");
        assert_eq!(
            either.search("a#", 1, at(1), false).map(|h| h.start),
            Some(1)
        );
        assert_eq!(either.search("a#", 1, SearchCtx::default(), false), None);
        // `\G` inside a leading look-ahead turned into a tail.
        let tail = m(r"(?=(?>[\t ]++|\G)#)");
        assert_eq!(tail.search("#x", 0, at(0), false).map(|h| h.start), Some(0));
        assert_eq!(tail.search("#x", 0, SearchCtx::default(), false), None);
        // `\G` at the end of a look-behind body.
        let behind = m(r"(?<=\G|')\w+");
        assert_eq!(
            behind
                .search("ab 'cd", 0, at(0), false)
                .map(|h| (h.start, h.end)),
            Some((0, 2))
        );
        assert_eq!(
            behind
                .search("ab 'cd", 0, SearchCtx::default(), false)
                .map(|h| (h.start, h.end)),
            Some((4, 6))
        );
    }

    #[test]
    fn text_start_anchor() {
        let a = m(r"\A#!");
        assert!(a
            .search(
                "#!x",
                0,
                SearchCtx {
                    allow_a: true,
                    g_pos: None
                },
                false
            )
            .is_some());
        assert!(a.search("#!x", 0, SearchCtx::default(), false).is_none());
        assert!(a.uses_a() && !a.uses_g());
    }

    #[test]
    fn keep_moves_the_start() {
        assert_eq!(find(r"foo\Kbar", "foobar", 0), Some((3, 6)));
    }

    #[test]
    fn unsupported_patterns_error_instead_of_panicking() {
        for src in [
            r"(.)\1",
            r"(?:a(?=b))+",
            r"\g<x>",
            r"(?<=(?:a(?=b))+)c",
            r"[z-a]",
            r"(?<=a\K)b",
        ] {
            assert!(Matcher::new(src).is_err(), "{src:?}");
        }
    }

    #[test]
    fn back_references_to_groups_with_few_texts() {
        // Expected values from libonig.
        assert_eq!(
            groups(r#"(['"])x\1"#, "a 'x\" \"x\" 'x'"),
            vec![Some((6, 9)), Some((6, 7))]
        );
        assert_eq!(find(r"(`+)[^`]*\1", "a ``b`` c", 0), Some((2, 7)));
        assert_eq!(find(r"(`+)[^`]*\1", "a `b`` c", 0), Some((2, 5)));
        assert_eq!(
            groups(r"(?<q>`?)ab\k<q>", "`ab` ab"),
            vec![Some((0, 4)), Some((0, 1))]
        );
        assert_eq!(find(r"(a)(b)\k<-1>", "abb", 0), Some((0, 3)));
        assert_eq!(find(r"(?i)(a)\1", "aA", 0), Some((0, 2)));
        assert_eq!(
            groups(r"(a|ab)(c|bcd)\2", "abcdbcd"),
            vec![Some((0, 7)), Some((0, 1)), Some((1, 4))]
        );
        assert_eq!(
            groups(r"(a+)b\1", "aaabaa"),
            vec![Some((1, 6)), Some((1, 3))]
        );
        assert_eq!(find(r"(a{2,})-\1", "aaaa-aaaa", 0), Some((0, 9)));
    }

    #[test]
    fn subexpression_calls_are_inlined() {
        // Expected values from libonig.
        let nested = r"(?<p>\((?:[^()]|\g<p>)*\))";
        assert_eq!(find(nested, "x(a(b)c)", 0), Some((1, 8)));
        // Recursion stops after a few levels (libonig matches all six).
        assert_eq!(find(nested, "((((((x))))))", 0), Some((1, 12)));
        let link = r"(\[)((?<square>[^\[\]\\]|\\.|\[\g<square>*+\])*+)(\])";
        assert_eq!(find(link, "see [a [b] c] x", 0), Some((4, 13)));
        assert_eq!(find(r"\g<+1>(x)", "xx", 0), Some((0, 2)));
        assert_eq!(find(r"(?<x>a)\g<x>", "aa", 0), Some((0, 2)));
        assert_eq!(
            find(r"(?:\((?:[^()]|\g<0>)*\))", "((a)(b))", 0),
            Some((0, 8))
        );
    }

    #[test]
    fn back_reference_substitution() {
        let text = "<<EOS";
        let groups = [Some((0, 5)), Some((2, 5))];
        assert!(has_back_references(r"^\1$"));
        assert!(!has_back_references(r"\\d"));
        assert_eq!(resolve_back_references(r"^\1$", text, &groups), "^EOS$");
        assert_eq!(resolve_back_references(r"\2\\1", text, &groups), r"\\1");
        let mut escaped = String::new();
        escape_literal("a.b *", &mut escaped);
        assert_eq!(escaped, r"a\.b\x{20}\*");
    }

    #[test]
    fn non_char_boundary_positions_are_rejected() {
        assert!(m("x")
            .search("éx", 1, SearchCtx::default(), false)
            .is_none());
        assert_eq!(find("x", "éx", 2), Some((2, 3)));
    }
}
