//! The line tokenizer: TextMate's matching model (as implemented by
//! vscode-textmate) over a scope stack carried from line to line.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crate::grammar::{name_scopes, CaptureRule, Grammar, PatternId, RuleId, RuleKind};
use crate::matcher::{resolve_back_references, Groups, Hit, Matcher, SearchCtx};

/// Lines longer than this (in bytes) are not tokenized: they get a single
/// token with the scopes at their start, and the state is left unchanged
/// (vscode-textmate's default limit is 20 000 characters).
pub const MAX_LINE_LENGTH: usize = 20_000;
/// Deepest scope stack the tokenizer builds.
const MAX_STACK_DEPTH: usize = 512;
/// Deepest nesting of captures retokenized with their own patterns.
const MAX_CAPTURE_DEPTH: usize = 16;

/// A tokenized span of a line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Token {
    /// Byte range in the line.
    pub range: Range<usize>,
    /// Scopes from the outermost (the grammar's scope name) to the innermost.
    pub scopes: Vec<String>,
}

impl Token {
    /// The highlight tag for this token's scopes; see
    /// [`crate::scope_to_tag`].
    pub fn tag(&self) -> Option<&'static str> {
        crate::scope::scope_to_tag(&self.scopes)
    }
}

/// The tokenizer state between lines: the stack of open begin/end and
/// begin/while rules with their scopes.
///
/// [`StackState::default`] is the state before the first line. States can be
/// compared to detect when re-tokenizing after an edit has converged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct StackState {
    /// The grammar that produced the frames (0 before the first line).
    grammar: u64,
    frames: Vec<Frame>,
}

impl StackState {
    /// The state before the first line.
    pub fn new() -> StackState {
        StackState::default()
    }

    /// Whether no line has been tokenized with this state yet.
    pub fn is_initial(&self) -> bool {
        self.frames.is_empty()
    }

    /// Number of open rules (the grammar's root included).
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// The scopes in effect at the end of the last tokenized line (empty
    /// before the first line).
    pub fn scopes(&self) -> Vec<String> {
        self.frames
            .iter()
            .flat_map(|f| f.name_scopes.iter().chain(&f.content_scopes))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Frame {
    rule: RuleId,
    /// Scopes from the rule's `name`.
    name_scopes: Vec<String>,
    /// Scopes from the rule's `contentName`.
    content_scopes: Vec<String>,
    /// The `end`/`while` pattern with `begin` back-references substituted.
    end: Option<String>,
    /// The `begin` match reached the end of its line, so `\G` matches at the
    /// start of the next one.
    begin_captured_eol: bool,
}

/// Tokenizes one line with `grammar`, continuing from `state` (which is
/// updated for the next line).
///
/// `line` should not contain line breaks, except an optional trailing `\n` /
/// `\r\n`, which is not tokenized. Token ranges are byte offsets into `line`;
/// together the tokens cover the line without gaps. A state produced by a
/// different grammar is treated as the initial state.
pub fn tokenize_line(grammar: &Grammar, line: &str, state: &mut StackState) -> Vec<Token> {
    let content = line
        .strip_suffix('\n')
        .map_or(line, |l| l.strip_suffix('\r').unwrap_or(l));
    let fresh = state.grammar != grammar.id
        || state.frames.is_empty()
        || state.frames.iter().any(|f| grammar.rule(f.rule).is_none());
    if fresh {
        state.grammar = grammar.id;
        state.frames = vec![Frame {
            rule: grammar.root,
            name_scopes: vec![grammar.scope_name().to_string()],
            content_scopes: Vec::new(),
            end: None,
            begin_captured_eol: false,
        }];
    }
    if content.len() > MAX_LINE_LENGTH {
        log::debug!("textmate: line of {} bytes not tokenized", content.len());
        return vec![Token {
            range: 0..content.len(),
            scopes: state.scopes(),
        }];
    }
    // Like vscode-textmate, match against the line plus its newline.
    let mut text = String::with_capacity(content.len() + 1);
    text.push_str(content);
    text.push('\n');
    let mut work = Work {
        base: Vec::new(),
        frames: state
            .frames
            .drain(..)
            .map(|frame| WorkFrame {
                frame,
                enter_pos: None,
                anchor_pos: None,
            })
            .collect(),
    };
    let mut out = LineTokens::default();
    tokenize_string(grammar, &text, fresh, 0, &mut work, &mut out, true, 0);
    state.frames = work.frames.into_iter().map(|f| f.frame).collect();
    out.tokens
        .into_iter()
        .filter(|t| t.range.start < content.len())
        .map(|mut t| {
            t.range.end = t.range.end.min(content.len());
            t
        })
        .collect()
}

struct WorkFrame {
    frame: Frame,
    /// Line position where the rule was entered (`None` for earlier lines).
    enter_pos: Option<usize>,
    /// The `\G` anchor in effect when the rule was entered.
    anchor_pos: Option<usize>,
}

/// The scope stack while a line is tokenized.
struct Work {
    /// Scopes below the first frame (used when retokenizing a capture).
    base: Vec<String>,
    frames: Vec<WorkFrame>,
}

impl Work {
    /// Scopes of `frames[..len]`, with the content scopes of the last of
    /// them only when `content` is set.
    fn scopes_upto(&self, len: usize, content: bool) -> Vec<String> {
        let mut out = self.base.clone();
        for (i, f) in self.frames.iter().take(len).enumerate() {
            out.extend(f.frame.name_scopes.iter().cloned());
            if content || i + 1 < len {
                out.extend(f.frame.content_scopes.iter().cloned());
            }
        }
        out
    }

    fn scopes(&self) -> Vec<String> {
        self.scopes_upto(self.frames.len(), true)
    }

    /// Whether a rule just pushed at `pos` is already open at that position
    /// (a grammar looping without consuming anything).
    fn pushed_same_rule(&self, pos: usize) -> bool {
        let Some((top, below)) = self.frames.split_last() else {
            return false;
        };
        below
            .iter()
            .rev()
            .take_while(|f| f.enter_pos == Some(pos))
            .any(|f| f.frame.rule == top.frame.rule)
    }
}

#[derive(Default)]
struct LineTokens {
    tokens: Vec<Token>,
    last_end: usize,
}

impl LineTokens {
    fn produce(&mut self, end: usize, scopes: impl FnOnce() -> Vec<String>) {
        if end <= self.last_end {
            return;
        }
        self.tokens.push(Token {
            range: self.last_end..end,
            scopes: scopes(),
        });
        self.last_end = end;
    }

    fn produce_scopes(&mut self, scopes: &[String], end: usize) {
        self.produce(end, || scopes.to_vec());
    }
}

/// Caches each pattern's last search on one haystack, like vscode-oniguruma:
/// a match found from `from` is still the leftmost one for any later search
/// position up to its start.
struct Scanner<'t> {
    text: &'t str,
    cache: HashMap<PatternId, (usize, Option<Hit>)>,
}

impl<'t> Scanner<'t> {
    fn new(text: &'t str) -> Scanner<'t> {
        Scanner {
            text,
            cache: HashMap::new(),
        }
    }

    /// The pattern's leftmost match from `pos`, without capture groups.
    fn search(
        &mut self,
        grammar: &Grammar,
        id: PatternId,
        pos: usize,
        ctx: SearchCtx,
    ) -> Option<Hit> {
        let matcher = grammar.pattern(id)?;
        let cacheable = !matcher.uses_a() && !matcher.uses_g();
        if cacheable {
            if let Some((from, hit)) = self.cache.get(&id) {
                if *from <= pos && hit.as_ref().is_none_or(|h| h.raw_start >= pos) {
                    return hit.clone();
                }
            }
        }
        let hit = matcher.search(self.text, pos, ctx, false);
        if cacheable {
            self.cache.insert(id, (pos, hit.clone()));
        }
        hit
    }
}

/// What matched: the innermost rule's `end`, or a rule to enter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Candidate {
    End,
    Rule(RuleId),
}

/// A candidate's pattern (back-referencing ends are compiled per match).
enum PatternRef<'g> {
    Static(&'g Matcher),
    Dynamic(Arc<Matcher>),
}

impl std::ops::Deref for PatternRef<'_> {
    type Target = Matcher;

    fn deref(&self) -> &Matcher {
        match self {
            PatternRef::Static(m) => m,
            PatternRef::Dynamic(m) => m,
        }
    }
}

/// Keeps the leftmost hit (ties go to the earlier candidate); returns `true`
/// when no later candidate can win.
fn consider<'g>(
    best: &mut Option<(Candidate, Hit, PatternRef<'g>)>,
    candidate: Candidate,
    found: Option<(Hit, PatternRef<'g>)>,
    pos: usize,
) -> bool {
    let Some((hit, pattern)) = found else {
        return false;
    };
    if best.as_ref().is_some_and(|(_, b, _)| hit.start >= b.start) {
        return false;
    }
    let done = hit.start == pos;
    *best = Some((candidate, hit, pattern));
    done
}

fn begin_pattern(grammar: &Grammar, id: RuleId) -> Option<PatternId> {
    match &grammar.rule(id)?.kind {
        RuleKind::Match { pattern, .. } => Some(*pattern),
        RuleKind::BeginEnd { begin, .. } | RuleKind::BeginWhile { begin, .. } => Some(*begin),
        RuleKind::IncludeOnly { .. } => None,
    }
}

/// The earliest match among the innermost rule's `end` and its patterns,
/// with its capture groups.
fn find_best(
    grammar: &Grammar,
    scanner: &mut Scanner<'_>,
    work: &Work,
    pos: usize,
    ctx: SearchCtx,
) -> Option<(Candidate, Hit)> {
    let top = work.frames.last()?;
    let rule = grammar.rule(top.frame.rule)?;
    let end = match &rule.kind {
        RuleKind::BeginEnd {
            end,
            apply_end_last,
            ..
        } => Some((end, *apply_end_last)),
        _ => None,
    };
    let end_hit = |scanner: &mut Scanner<'_>| match (&top.frame.end, end) {
        (Some(source), _) => {
            let matcher = grammar.dynamic_matcher(source)?;
            let hit = matcher.search(scanner.text, pos, ctx, false)?;
            Some((hit, PatternRef::Dynamic(matcher)))
        }
        (None, Some((end, _))) => {
            let id = end.pattern?;
            Some((
                scanner.search(grammar, id, pos, ctx)?,
                PatternRef::Static(grammar.pattern(id)?),
            ))
        }
        (None, None) => None,
    };
    let mut best = None;
    let decided = matches!(end, Some((_, false)))
        && consider(&mut best, Candidate::End, end_hit(scanner), pos);
    if !decided {
        let mut decided = false;
        for &id in grammar.candidates(top.frame.rule) {
            let found = begin_pattern(grammar, id).and_then(|p| {
                Some((
                    scanner.search(grammar, p, pos, ctx)?,
                    PatternRef::Static(grammar.pattern(p)?),
                ))
            });
            if consider(&mut best, Candidate::Rule(id), found, pos) {
                decided = true;
                break;
            }
        }
        if !decided && matches!(end, Some((_, true))) {
            consider(&mut best, Candidate::End, end_hit(scanner), pos);
        }
    }
    // Only the winner's capture groups are computed: searching again from
    // its start finds the same match.
    let (candidate, hit, pattern) = best?;
    let hit = pattern
        .search(scanner.text, hit.raw_start, ctx, true)
        .unwrap_or(hit);
    Some((candidate, hit))
}

/// Tokenizes `text[line_pos..]` (vscode-textmate's `_tokenizeString`).
#[allow(clippy::too_many_arguments)]
fn tokenize_string(
    grammar: &Grammar,
    text: &str,
    mut is_first_line: bool,
    mut line_pos: usize,
    work: &mut Work,
    out: &mut LineTokens,
    check_while: bool,
    depth: usize,
) {
    let line_len = text.len();
    let mut scanner = Scanner::new(text);
    let mut anchor = None;
    if check_while {
        (line_pos, anchor, is_first_line) =
            check_while_conditions(grammar, text, is_first_line, line_pos, work, out, depth);
    }
    let max_steps = line_len.saturating_mul(4).saturating_add(1024);
    for _ in 0..max_steps {
        let ctx = SearchCtx {
            allow_a: is_first_line,
            g_pos: (anchor == Some(line_pos)).then_some(line_pos),
        };
        let Some((candidate, hit)) = find_best(grammar, &mut scanner, work, line_pos, ctx) else {
            out.produce(line_len, || work.scopes());
            return;
        };
        let Hit {
            start, end, groups, ..
        } = hit;
        let advanced = end > line_pos;
        match candidate {
            Candidate::End => {
                out.produce(start, || work.scopes());
                // End captures do not get the rule's contentName.
                let base = work.scopes_upto(work.frames.len(), false);
                let captures = work
                    .frames
                    .last()
                    .and_then(|f| grammar.rule(f.frame.rule))
                    .map(|r| match &r.kind {
                        RuleKind::BeginEnd { end_captures, .. } => end_captures.as_slice(),
                        _ => &[],
                    });
                handle_captures(
                    grammar,
                    text,
                    is_first_line,
                    &base,
                    out,
                    captures.unwrap_or(&[]),
                    &groups,
                    depth,
                );
                out.produce_scopes(&base, end);
                let Some(popped) = work.frames.pop() else {
                    return;
                };
                anchor = popped.anchor_pos;
                if !advanced && popped.enter_pos == Some(line_pos) {
                    // Entered and left without consuming anything: keep the
                    // rule and give up on the rest of the line.
                    work.frames.push(popped);
                    out.produce(line_len, || work.scopes());
                    return;
                }
                if work.frames.is_empty() {
                    out.produce(line_len, || work.base.clone());
                    return;
                }
            }
            Candidate::Rule(id) => {
                let Some(rule) = grammar.rule(id) else { return };
                out.produce(start, || work.scopes());
                if work.frames.len() >= MAX_STACK_DEPTH {
                    log::debug!("textmate: scope stack too deep; rest of line not tokenized");
                    out.produce(line_len, || work.scopes());
                    return;
                }
                work.frames.push(WorkFrame {
                    frame: Frame {
                        rule: id,
                        name_scopes: name_scopes(rule.name.as_ref(), text, &groups),
                        content_scopes: Vec::new(),
                        end: None,
                        begin_captured_eol: end == line_len,
                    },
                    enter_pos: Some(line_pos),
                    anchor_pos: anchor,
                });
                match &rule.kind {
                    RuleKind::BeginEnd {
                        begin_captures,
                        end: end_pattern,
                        ..
                    }
                    | RuleKind::BeginWhile {
                        begin_captures,
                        while_: end_pattern,
                        ..
                    } => {
                        let base = work.scopes();
                        handle_captures(
                            grammar,
                            text,
                            is_first_line,
                            &base,
                            out,
                            begin_captures,
                            &groups,
                            depth,
                        );
                        out.produce_scopes(&base, end);
                        anchor = Some(end);
                        let content = name_scopes(rule.content_name.as_ref(), text, &groups);
                        let resolved = end_pattern
                            .back_refs
                            .then(|| resolve_back_references(&end_pattern.source, text, &groups));
                        if let Some(top) = work.frames.last_mut() {
                            top.frame.content_scopes = content;
                            top.frame.end = resolved;
                        }
                        if !advanced && work.pushed_same_rule(line_pos) {
                            work.frames.pop();
                            out.produce(line_len, || work.scopes());
                            return;
                        }
                    }
                    RuleKind::Match { captures, .. } => {
                        let base = work.scopes();
                        handle_captures(
                            grammar,
                            text,
                            is_first_line,
                            &base,
                            out,
                            captures,
                            &groups,
                            depth,
                        );
                        out.produce_scopes(&base, end);
                        work.frames.pop();
                        if !advanced {
                            // Neither consuming nor pushing: vscode-textmate
                            // leaves the enclosing rule and the line.
                            if work.frames.len() > 1 {
                                work.frames.pop();
                            }
                            out.produce(line_len, || work.scopes());
                            return;
                        }
                    }
                    RuleKind::IncludeOnly { .. } => {
                        // Never a candidate; be defensive.
                        work.frames.pop();
                        out.produce(line_len, || work.scopes());
                        return;
                    }
                }
            }
        }
        if end > line_pos {
            line_pos = end;
            is_first_line = false;
        }
    }
    log::debug!("textmate: step limit reached; rest of line not tokenized");
    out.produce(line_len, || work.scopes());
}

/// At the start of a line every open begin/while rule (outermost first) must
/// match its `while` pattern, which is tokenized; the first that does not is
/// closed together with everything inside it. Returns the new
/// `(line_pos, anchor, is_first_line)`.
fn check_while_conditions(
    grammar: &Grammar,
    text: &str,
    mut is_first_line: bool,
    mut line_pos: usize,
    work: &mut Work,
    out: &mut LineTokens,
    depth: usize,
) -> (usize, Option<usize>, bool) {
    let mut anchor = work
        .frames
        .last()
        .is_some_and(|f| f.frame.begin_captured_eol)
        .then_some(0);
    let while_frames: Vec<usize> = (0..work.frames.len())
        .filter(|&i| {
            matches!(
                grammar.rule(work.frames[i].frame.rule).map(|r| &r.kind),
                Some(RuleKind::BeginWhile { .. })
            )
        })
        .collect();
    for index in while_frames {
        let Some(RuleKind::BeginWhile {
            while_,
            while_captures,
            ..
        }) = grammar.rule(work.frames[index].frame.rule).map(|r| &r.kind)
        else {
            continue;
        };
        let ctx = SearchCtx {
            allow_a: is_first_line,
            g_pos: (anchor == Some(line_pos)).then_some(line_pos),
        };
        let hit = match &work.frames[index].frame.end {
            Some(source) => grammar
                .dynamic_matcher(source)
                .and_then(|m| m.search(text, line_pos, ctx, true)),
            None => while_
                .pattern
                .and_then(|p| grammar.pattern(p))
                .and_then(|m| m.search(text, line_pos, ctx, true)),
        };
        let Some(hit) = hit else {
            work.frames.truncate(index);
            break;
        };
        let scopes = work.scopes_upto(index + 1, true);
        out.produce_scopes(&scopes, hit.start);
        handle_captures(
            grammar,
            text,
            is_first_line,
            &scopes,
            out,
            while_captures,
            &hit.groups,
            depth,
        );
        out.produce_scopes(&scopes, hit.end);
        anchor = Some(hit.end);
        if hit.end > line_pos {
            line_pos = hit.end;
            is_first_line = false;
        }
    }
    (line_pos, anchor, is_first_line)
}

/// Produces the tokens of a match's capture groups (vscode-textmate's
/// `handleCaptures`): nested captures stack their scopes, and a capture with
/// `patterns` is tokenized with them.
#[allow(clippy::too_many_arguments)]
fn handle_captures(
    grammar: &Grammar,
    text: &str,
    is_first_line: bool,
    base: &[String],
    out: &mut LineTokens,
    captures: &[Option<CaptureRule>],
    groups: &Groups,
    depth: usize,
) {
    let Some(&Some((_, max_end))) = groups.first() else {
        return;
    };
    // `(scopes, end)` of the enclosing named captures.
    let mut open: Vec<(Vec<String>, usize)> = Vec::new();
    for (rule, span) in captures.iter().zip(groups) {
        let (Some(rule), Some((start, end))) = (rule, *span) else {
            continue;
        };
        if end <= start {
            continue;
        }
        if start > max_end {
            break;
        }
        // Captures in a trailing look-ahead may reach past the match.
        let end = end.min(max_end);
        if end <= start {
            continue;
        }
        while let Some((scopes, open_end)) = open.last() {
            if *open_end > start {
                break;
            }
            out.produce_scopes(scopes, *open_end);
            open.pop();
        }
        let parent: Vec<String> = open
            .last()
            .map_or_else(|| base.to_vec(), |(scopes, _)| scopes.clone());
        out.produce_scopes(&parent, start);
        if let (Some(retokenize), true) = (rule.retokenize, depth < MAX_CAPTURE_DEPTH) {
            let name = name_scopes(rule.name.as_ref(), text, groups);
            let content = name_scopes(rule.content_name.as_ref(), text, groups);
            let mut sub = Work {
                base: parent,
                frames: vec![WorkFrame {
                    frame: Frame {
                        rule: retokenize,
                        name_scopes: name,
                        content_scopes: content,
                        end: None,
                        begin_captured_eol: false,
                    },
                    enter_pos: Some(start),
                    anchor_pos: None,
                }],
            };
            let first = is_first_line && start == 0;
            tokenize_string(
                grammar,
                &text[..end],
                first,
                start,
                &mut sub,
                out,
                false,
                depth + 1,
            );
            continue;
        }
        if let Some(name) = &rule.name {
            let mut scopes = parent;
            name.push_scopes(text, groups, &mut scopes);
            open.push((scopes, end));
        }
    }
    while let Some((scopes, end)) = open.pop() {
        out.produce_scopes(&scopes, end);
    }
}
