//! Structural Search & Replace ("Search Structurally", roadmap §2.7).
//!
//! A pattern is Python source with placeholders:
//!
//! * `$name$` matches exactly one node: an expression, a whole statement (when
//!   the placeholder is a statement of its own), an identifier (in name
//!   positions such as `obj.$attr$` or `def $f$`), a parameter, an import
//!   alias or a dotted module name.
//! * `$name...$` written as a list element (`f($args...$)`, `def g($params...$)`,
//!   `[$items...$]`, or a statement in a block) matches zero or more elements.
//!
//! Matching ignores whitespace, comments and indentation; a placeholder that
//! occurs twice must bind the same text. Placeholders may carry constraints
//! (regex on the matched text, element counts, node kind) through
//! [`PatternOptions`]. The replacement template reuses the placeholders.

use std::cell::Cell;
use std::collections::HashMap;

use regex::Regex;
use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::rowan::NodeOrToken;
use rustpy_syntax::{parse, Parse, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

use rustpy_semantic::FileInfo;
use serde::{Deserialize, Serialize};

mod filters;
pub use filters::{library_key, load_library, save_library, Template};

/// Restricts what a placeholder may bind to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKindFilter {
    Expression,
    Statement,
    Identifier,
    Literal,
    Call,
    String,
}

/// Per-placeholder constraints.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Constraint {
    /// Regular expression the matched source text must satisfy.
    pub regex: Option<String>,
    /// Minimum number of elements for `$name...$` placeholders.
    pub min: usize,
    /// Maximum number of elements for `$name...$` placeholders.
    pub max: usize,
    pub kind: Option<NodeKindFilter>,
    /// Regex matched against the inferred Python expression type, never source spelling.
    pub type_regex: Option<String>,
    /// Bounded predicate: text comparisons/methods and count comparisons joined by && / ||.
    pub script: Option<String>,
}

impl Default for Constraint {
    fn default() -> Self {
        Self {
            regex: None,
            min: 0,
            max: usize::MAX,
            kind: None,
            type_regex: None,
            script: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PatternOptions {
    pub constraints: HashMap<String, Constraint>,
}

/// A match in a file: the matched range plus what each placeholder bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub range: TextRange,
    /// `(placeholder name, bound source range)` in binding order.
    pub bindings: Vec<(String, TextRange)>,
}

#[derive(Clone, Debug)]
struct Placeholder {
    name: String,
    variadic: bool,
}

/// A compiled pattern.
#[derive(Clone, Debug)]
pub struct Pattern {
    source: String,
    parse: Parse,
    /// Mangled identifier → placeholder.
    placeholders: HashMap<String, Placeholder>,
    is_expression: bool,
    constraints: HashMap<String, Constraint>,
    regexes: HashMap<String, Regex>,
    type_regexes: HashMap<String, Regex>,
    scripts: HashMap<String, filters::Script>,
}

const PLACEHOLDER_RE: &str = r"\$([A-Za-z_][A-Za-z0-9_]*)(\.\.\.)?\$";

fn placeholder_regex() -> &'static Regex {
    static RE: once_cell::sync::OnceCell<Regex> = once_cell::sync::OnceCell::new();
    RE.get_or_init(|| Regex::new(PLACEHOLDER_RE).expect("valid placeholder regex"))
}

fn mangle(name: &str, variadic: bool) -> String {
    if variadic {
        format!("__ssrv_{name}__")
    } else {
        format!("__ssr_{name}__")
    }
}

/// Compiles a pattern. Errors on syntax errors in the pattern or invalid constraints.
pub fn compile(pattern: &str, opts: &PatternOptions) -> Result<Pattern, String> {
    if pattern.len() > 65_536 || opts.constraints.len() > 64 {
        return Err("Structural template exceeds the 64 KiB / 64 filter limit".into());
    }
    let source = pattern.trim_end().to_string();
    if source.trim().is_empty() {
        return Err("The pattern is empty".into());
    }
    let mut placeholders = HashMap::new();
    let rewritten = placeholder_regex()
        .replace_all(&source, |caps: &regex::Captures| {
            let name = caps[1].to_string();
            let variadic = caps.get(2).is_some();
            let m = mangle(&name, variadic);
            placeholders.insert(m.clone(), Placeholder { name, variadic });
            m
        })
        .into_owned();
    // Dedent the pattern so a snippet copied from inside a block parses.
    let rewritten = dedent(&rewritten);
    let parsed = parse(&rewritten);
    if let Some(e) = parsed.errors().first() {
        return Err(format!("Pattern syntax error: {}", e.message));
    }
    let root = parsed.syntax_node();
    let stmts: Vec<SyntaxNode> = root
        .children()
        .filter(|n| n.kind() != SyntaxKind::ERROR)
        .collect();
    if stmts.is_empty() {
        return Err("The pattern contains no statement or expression".into());
    }
    // A lone `$x$` is a statement pattern (matching every expression would be noise).
    let lone_placeholder = stmts.len() == 1 && {
        let mut toks = significant_tokens(&stmts[0]);
        let first = toks.next();
        toks.next().is_none()
            && first.is_some_and(|t| {
                t.kind() == SyntaxKind::IDENT && placeholders.contains_key(t.text())
            })
    };
    let is_expression = stmts.len() == 1
        && stmts[0].kind() == SyntaxKind::EXPR_STMT
        && stmts[0].children().count() == 1
        && !lone_placeholder;
    let mut regexes = HashMap::new();
    if placeholders.len() > 64 {
        return Err("At most 64 placeholders are supported".into());
    }
    let mut type_regexes = HashMap::new();
    let mut scripts = HashMap::new();
    for (name, c) in &opts.constraints {
        if c.regex.as_ref().is_some_and(|s| s.len() > 4096)
            || c.type_regex.as_ref().is_some_and(|s| s.len() > 4096)
        {
            return Err("Filter regex exceeds 4096 bytes".into());
        }
        if !placeholders.values().any(|p| &p.name == name) {
            return Err(format!("Unknown placeholder: {name}"));
        }
        if let Some(value) = &c.type_regex {
            type_regexes.insert(
                name.clone(),
                regex::RegexBuilder::new(value)
                    .size_limit(262_144)
                    .dfa_size_limit(262_144)
                    .build()
                    .map_err(|e| format!("{name}: {e}"))?,
            );
        }
        if let Some(value) = &c.script {
            scripts.insert(name.clone(), filters::Script::compile(value)?);
        }
        if let Some(r) = &c.regex {
            let re = regex::RegexBuilder::new(r)
                .size_limit(262_144)
                .dfa_size_limit(262_144)
                .build()
                .map_err(|e| format!("Invalid regex for ${name}$: {e}"))?;
            regexes.insert(name.clone(), re);
        }
        if c.min > c.max {
            return Err(format!("Invalid count range for ${name}$"));
        }
    }
    Ok(Pattern {
        source,
        parse: parsed,
        placeholders,
        is_expression,
        constraints: opts.constraints.clone(),
        regexes,
        type_regexes,
        scripts,
    })
}

impl Pattern {
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Placeholder names in order of first appearance.
    pub fn placeholder_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        for caps in placeholder_regex().captures_iter(&self.source) {
            let n = caps[1].to_string();
            if !out.contains(&n) {
                out.push(n);
            }
        }
        out
    }

    /// True when the pattern is a single expression (matched against expressions anywhere).
    pub fn is_expression(&self) -> bool {
        self.is_expression
    }

    fn roots(&self) -> Vec<SyntaxNode> {
        let root = self.parse.syntax_node();
        let stmts: Vec<SyntaxNode> = root
            .children()
            .filter(|n| n.kind() != SyntaxKind::ERROR)
            .collect();
        if self.is_expression {
            vec![stmts[0]
                .first_child()
                .expect("expression statement has a child")]
        } else {
            stmts
        }
    }

    fn placeholder_of(&self, tok: &SyntaxToken) -> Option<&Placeholder> {
        if tok.kind() != SyntaxKind::IDENT {
            return None;
        }
        self.placeholders.get(tok.text())
    }

    /// A node whose only significant token is a placeholder identifier
    /// (containers such as blocks and argument lists never count).
    fn bare_placeholder(&self, node: &SyntaxNode) -> Option<&Placeholder> {
        if is_block_like(node.kind()) || is_list_like(node.kind()) {
            return None;
        }
        let mut toks = significant_tokens(node);
        let first = toks.next()?;
        if toks.next().is_some() {
            return None;
        }
        self.placeholder_of(&first)
    }

    fn constraint(&self, name: &str) -> Option<&Constraint> {
        self.constraints.get(name)
    }

    fn regex_ok(&self, name: &str, text: &str) -> bool {
        self.regexes.get(name).is_none_or(|re| re.is_match(text))
    }
}

fn dedent(text: &str) -> String {
    let indent = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    if indent == 0 {
        return text.to_string();
    }
    text.lines()
        .map(|l| {
            if l.len() >= indent {
                &l[indent..]
            } else {
                l.trim_start()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_block_like(kind: SyntaxKind) -> bool {
    matches!(kind, SyntaxKind::MODULE | SyntaxKind::BLOCK)
}

fn is_list_like(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        ARG_LIST
            | PARAM_LIST
            | TUPLE_EXPR
            | LIST_EXPR
            | SET_EXPR
            | DICT_EXPR
            | SLICE_LIST
            | IMPORT_FROM_STMT
            | IMPORT_STMT
            | GLOBAL_STMT
            | NONLOCAL_STMT
            | DEL_STMT
            | WITH_STMT
            | MATCH_SEQUENCE_PATTERN
            | MATCH_CLASS_PATTERN
            | MATCH_MAPPING_PATTERN
    )
}

/// Children that take part in matching: nodes, plus tokens that are neither
/// trivia nor separators of the parent kind.
fn significant_children(node: &SyntaxNode) -> Vec<SyntaxElement> {
    let kind = node.kind();
    let block = is_block_like(kind);
    let list = is_list_like(kind);
    node.children_with_tokens()
        .filter(|e| match e {
            NodeOrToken::Token(t) => {
                let k = t.kind();
                let separator = (block
                    && matches!(
                        k,
                        SyntaxKind::NEWLINE | SyntaxKind::INDENT | SyntaxKind::DEDENT
                    ))
                    || (list && k == SyntaxKind::COMMA);
                !(k.is_trivia() || k == SyntaxKind::EOF || separator)
            }
            NodeOrToken::Node(n) => n.kind() != SyntaxKind::ERROR,
        })
        .collect()
}

fn significant_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| {
            !t.kind().is_trivia()
                && !matches!(
                    t.kind(),
                    SyntaxKind::NEWLINE | SyntaxKind::INDENT | SyntaxKind::DEDENT | SyntaxKind::EOF
                )
        })
}

/// Range of a node without its trailing newline / dedent / trivia tokens.
fn trimmed_range(node: &SyntaxNode) -> TextRange {
    let end = significant_tokens(node)
        .last()
        .map(|t| t.text_range().end())
        .unwrap_or(node.text_range().end());
    TextRange::new(
        node.text_range().start(),
        end.max(node.text_range().start()),
    )
}

fn elem_range(e: &SyntaxElement) -> TextRange {
    match e {
        NodeOrToken::Node(n) => trimmed_range(n),
        NodeOrToken::Token(t) => t.text_range(),
    }
}

/// Whitespace-insensitive text of a node, used for placeholder consistency.
fn normalized(node: &SyntaxNode) -> String {
    significant_tokens(node)
        .map(|t| t.text().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalized_range(elems: &[SyntaxElement]) -> String {
    let mut parts = Vec::new();
    for e in elems {
        match e {
            NodeOrToken::Node(n) => parts.push(normalized(n)),
            NodeOrToken::Token(t) => parts.push(t.text().to_string()),
        }
    }
    parts.join(" ")
}

#[derive(Clone, Debug)]
struct Binding {
    name: String,
    text: String,
    range: TextRange,
}

#[derive(Clone, Debug, Default)]
struct Bindings(Vec<Binding>);

impl Bindings {
    fn bind(
        &mut self,
        pat: &Pattern,
        name: &str,
        text: String,
        range: TextRange,
        source: &str,
        count: usize,
    ) -> bool {
        if let Some(c) = pat.constraint(name) {
            if count < c.min || count > c.max {
                return false;
            }
        }
        let slice = source
            .get(usize::from(range.start())..usize::from(range.end()))
            .unwrap_or("");
        if !pat.regex_ok(name, slice)
            || pat
                .scripts
                .get(name)
                .is_some_and(|s| !s.evaluate(slice, count))
        {
            return false;
        }
        if let Some(existing) = self.0.iter().find(|b| b.name == name) {
            return existing.text == text;
        }
        self.0.push(Binding {
            name: name.to_string(),
            text,
            range,
        });
        true
    }
}

fn kind_filter_ok(filter: Option<NodeKindFilter>, target: &SyntaxNode) -> bool {
    let Some(f) = filter else { return true };
    let k = target.kind();
    match f {
        NodeKindFilter::Expression => k.is_expression(),
        NodeKindFilter::Statement => k.is_statement(),
        NodeKindFilter::Identifier => k == SyntaxKind::NAME_EXPR,
        NodeKindFilter::Literal => matches!(
            k,
            SyntaxKind::LITERAL_EXPR | SyntaxKind::STRING_EXPR | SyntaxKind::CONCAT_STRING_EXPR
        ),
        NodeKindFilter::Call => k == SyntaxKind::CALL_EXPR,
        NodeKindFilter::String => matches!(
            k,
            SyntaxKind::STRING_EXPR | SyntaxKind::FSTRING_EXPR | SyntaxKind::CONCAT_STRING_EXPR
        ),
    }
}

/// Kinds for which a bare placeholder binds the whole target node of the same kind.
fn whole_node_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        PARAM | IMPORT_ALIAS | DOTTED_NAME | WITH_ITEM | DECORATOR | DICT_ITEM
    )
}

struct Matcher<'a> {
    remaining: Cell<usize>,
    depth: Cell<usize>,
    pat: &'a Pattern,
    source: &'a str,
}

struct MatchFrame<'a>(&'a Cell<usize>);
impl Drop for MatchFrame<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl Matcher<'_> {
    fn enter(&self) -> Option<MatchFrame<'_>> {
        if self.remaining.get() == 0 || self.depth.get() >= 128 {
            self.remaining.set(0);
            return None;
        }
        self.remaining.set(self.remaining.get() - 1);
        self.depth.set(self.depth.get() + 1);
        Some(MatchFrame(&self.depth))
    }
    fn match_node(&self, p: &SyntaxNode, t: &SyntaxNode, b: &mut Bindings) -> bool {
        let Some(_frame) = self.enter() else {
            return false;
        };
        if let Some(ph) = self.pat.bare_placeholder(p) {
            let ok = match p.kind() {
                SyntaxKind::NAME_EXPR => t.kind().is_expression(),
                SyntaxKind::EXPR_STMT => t.kind().is_statement(),
                k if whole_node_kind(k) => t.kind() == k,
                _ => false,
            };
            if ok {
                let filter = self.pat.constraint(&ph.name).and_then(|c| c.kind);
                if !kind_filter_ok(filter, t) {
                    return false;
                }
                return b.bind(
                    self.pat,
                    &ph.name,
                    normalized(t),
                    trimmed_range(t),
                    self.source,
                    1,
                );
            }
            // Wrapper kinds (e.g. `ARG(NAME_EXPR(ph))`) fall through to structural matching.
        }
        if p.kind() != t.kind() {
            return false;
        }
        let pc = significant_children(p);
        let mut tc = significant_children(t);
        // A pattern without decorators matches decorated definitions too.
        let has_decorators =
            |elems: &[SyntaxElement]| elems.iter().any(|e| e.kind() == SyntaxKind::DECORATORS);
        if !has_decorators(&pc) && has_decorators(&tc) {
            tc.retain(|e| e.kind() != SyntaxKind::DECORATORS);
        }
        self.match_seq_from(&pc, &tc, 0, 0, b, false).is_some()
    }

    fn match_token(&self, p: &SyntaxToken, t: &SyntaxToken, b: &mut Bindings) -> bool {
        if let Some(ph) = self.pat.placeholder_of(p) {
            if t.kind() != SyntaxKind::IDENT && !t.kind().is_keyword() {
                return false;
            }
            let filter = self.pat.constraint(&ph.name).and_then(|c| c.kind);
            if matches!(filter, Some(f) if f != NodeKindFilter::Identifier) {
                return false;
            }
            return b.bind(
                self.pat,
                &ph.name,
                t.text().to_string(),
                t.text_range(),
                self.source,
                1,
            );
        }
        if p.kind() != t.kind() {
            return false;
        }
        match p.kind() {
            SyntaxKind::INDENT | SyntaxKind::DEDENT | SyntaxKind::NEWLINE => true,
            _ => p.text() == t.text(),
        }
    }

    fn variadic_of(&self, e: &SyntaxElement) -> Option<&Placeholder> {
        let NodeOrToken::Node(n) = e else { return None };
        self.pat.bare_placeholder(n).filter(|ph| ph.variadic)
    }

    /// Matches pattern elements `ps[i..]` against target elements `ts[j..]`.
    /// Returns the index just past the last consumed target element. With
    /// `allow_tail` the target may continue after the pattern ends.
    fn match_seq_from(
        &self,
        ps: &[SyntaxElement],
        ts: &[SyntaxElement],
        i: usize,
        j: usize,
        b: &mut Bindings,
        allow_tail: bool,
    ) -> Option<usize> {
        let _frame = self.enter()?;
        if i == ps.len() {
            return if allow_tail || j == ts.len() {
                Some(j)
            } else {
                None
            };
        }
        if let Some(ph) = self.variadic_of(&ps[i]) {
            // Only nodes can be swallowed; count how many consecutive nodes follow.
            let mut max = 0;
            while j + max < ts.len() && ts[j + max].as_node().is_some() {
                max += 1;
            }
            let (lo, hi) = self
                .pat
                .constraint(&ph.name)
                .map(|c| (c.min, c.max.min(max)))
                .unwrap_or((0, max));
            if lo > hi {
                return None;
            }
            // Longest first so a trailing `$rest...$` swallows everything it can.
            for k in (lo..=hi).rev() {
                let mut trial = b.clone();
                let elems = &ts[j..j + k];
                let range = match (elems.first(), elems.last()) {
                    (Some(f), Some(l)) => {
                        TextRange::new(f.text_range().start(), elem_range(l).end())
                    }
                    _ => {
                        let pos = ts
                            .get(j)
                            .map(|e| e.text_range().start())
                            .or_else(|| ts.last().map(|e| e.text_range().end()))
                            .unwrap_or(TextSize::new(0));
                        TextRange::empty(pos)
                    }
                };
                if !trial.bind(
                    self.pat,
                    &ph.name,
                    normalized_range(elems),
                    range,
                    self.source,
                    k,
                ) {
                    continue;
                }
                if let Some(end) = self.match_seq_from(ps, ts, i + 1, j + k, &mut trial, allow_tail)
                {
                    *b = trial;
                    return Some(end);
                }
            }
            return None;
        }
        let t = ts.get(j)?;
        let ok = match (&ps[i], t) {
            (NodeOrToken::Node(pn), NodeOrToken::Node(tn)) => self.match_node(pn, tn, b),
            (NodeOrToken::Token(pt), NodeOrToken::Token(tt)) => self.match_token(pt, tt, b),
            _ => false,
        };
        if !ok {
            return None;
        }
        self.match_seq_from(ps, ts, i + 1, j + 1, b, allow_tail)
    }
}

fn finish(b: Bindings, range: TextRange) -> Match {
    Match {
        range,
        bindings: b.0.into_iter().map(|b| (b.name, b.range)).collect(),
    }
}

/// Searches a parsed tree whose source text is `source`.
pub fn search_tree(pattern: &Pattern, root: &SyntaxNode, source: &str) -> Vec<Match> {
    if !pattern.type_regexes.is_empty() {
        return Vec::new(); // Typed constraints require the semantic entry point.
    }
    search_tree_untyped(pattern, root, source)
}

fn search_tree_untyped(pattern: &Pattern, root: &SyntaxNode, source: &str) -> Vec<Match> {
    if source.len() > 8 * 1024 * 1024 {
        return Vec::new();
    }
    let m = Matcher {
        remaining: Cell::new(200_000),
        depth: Cell::new(0),
        pat: pattern,
        source,
    };
    let roots = pattern.roots();
    let mut out = Vec::new();
    if pattern.is_expression {
        let proot = &roots[0];
        for node in root.descendants().filter(|n| n.kind().is_expression()) {
            let mut b = Bindings::default();
            if m.match_node(proot, &node, &mut b) {
                out.push(finish(b, node.text_range()));
            }
        }
    } else {
        let ps: Vec<SyntaxElement> = roots.iter().cloned().map(NodeOrToken::Node).collect();
        for block in root.descendants().filter(|n| is_block_like(n.kind())) {
            let ts = significant_children(&block);
            for j in 0..ts.len() {
                let mut b = Bindings::default();
                if let Some(end) = m.match_seq_from(&ps, &ts, 0, j, &mut b, true) {
                    if end > j {
                        let range = TextRange::new(
                            ts[j].text_range().start(),
                            elem_range(&ts[end - 1]).end(),
                        );
                        out.push(finish(b, range));
                    }
                }
            }
        }
    }
    // Outermost first for equal starts, so replacement keeps the enclosing match.
    if m.remaining.get() == 0 {
        return Vec::new();
    }
    out.truncate(10_000);
    out.sort_by_key(|m| (m.range.start(), std::cmp::Reverse(m.range.end())));
    out
}

/// Searches a file of the analysis.
pub fn search(pattern: &Pattern, file: &FileInfo) -> Vec<Match> {
    search_tree(pattern, &file.parse.syntax_node(), &file.text)
}

/// Searches with actual inferred expression types; unknown/non-expression captures fail closed.
pub fn search_with_analysis(
    pattern: &Pattern,
    db: &rustpy_semantic::Analysis,
    file: &std::sync::Arc<FileInfo>,
) -> Vec<Match> {
    use rustpy_syntax::ast::{AstNode, Expr};
    let root = file.parse.syntax_node();
    let matches = search_tree_untyped(pattern, &root, &file.text);
    if pattern.type_regexes.is_empty() {
        return matches;
    }
    let mut expressions = HashMap::new();
    for (index, node) in root.descendants().take(100_001).enumerate() {
        if index == 100_000 {
            return Vec::new();
        }
        if let Some(expr) = Expr::cast(node) {
            expressions
                .entry(expr.syntax().text_range())
                .or_insert(expr);
        }
    }
    let inf = rustpy_semantic::infer::Inferer::new(db);
    let mut queries = 4096usize;
    let mut out = Vec::new();
    for matched in matches {
        let mut accepted = true;
        for (name, regex) in &pattern.type_regexes {
            let Some(remaining) = queries.checked_sub(1) else {
                return Vec::new();
            };
            queries = remaining;
            let Some((_, range)) = matched.bindings.iter().find(|(n, _)| n == name) else {
                accepted = false;
                break;
            };
            let Some(expr) = expressions.get(range) else {
                accepted = false;
                break;
            };
            let ty = inf.type_of_expr(file, expr);
            use rustpy_semantic::types::{LiteralValue, Type};
            let nominal = match &ty {
                Type::Literal(LiteralValue::Int(_)) => Some("int"),
                Type::Literal(LiteralValue::Str(_)) => Some("str"),
                Type::Literal(LiteralValue::Bytes(_)) => Some("bytes"),
                Type::Literal(LiteralValue::Bool(_)) => Some("bool"),
                Type::Literal(LiteralValue::EnumMember { class, .. }) => Some(class.name.as_str()),
                _ => None,
            };
            let display = bounded_type_display(&ty);
            if matches!(ty, Type::Unknown | Type::Any)
                || !display.as_deref().is_some_and(|text| {
                    regex.is_match(text) || nominal.is_some_and(|name| regex.is_match(name))
                })
            {
                accepted = false;
                break;
            }
        }
        if accepted {
            out.push(matched);
        }
    }
    out
}

fn bounded_type_display(ty: &rustpy_semantic::types::Type) -> Option<String> {
    struct Output(String);
    impl std::fmt::Write for Output {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            if self.0.len().saturating_add(text.len()) > 4096 {
                return Err(std::fmt::Error);
            }
            self.0.push_str(text);
            Ok(())
        }
    }
    let mut output = Output(String::new());
    std::fmt::write(&mut output, format_args!("{ty}")).ok()?;
    Some(output.0)
}

/// Replacement edits from semantic matches, retaining the ordinary overlap policy.
pub fn replace_with_analysis(
    pattern: &Pattern,
    replacement: &str,
    db: &rustpy_semantic::Analysis,
    file: &std::sync::Arc<FileInfo>,
) -> Vec<(TextRange, String)> {
    let mut end = TextSize::new(0);
    search_with_analysis(pattern, db, file)
        .into_iter()
        .filter_map(|m| {
            if m.range.start() < end {
                return None;
            }
            end = m.range.end();
            Some((m.range, expand_replacement(replacement, &m, &file.text)))
        })
        .collect()
}

/// Parses `text` and searches it.
pub fn search_text(pattern: &Pattern, text: &str) -> Vec<Match> {
    if text.len() > 8 * 1024 * 1024 {
        return Vec::new();
    }
    let parsed = parse(text);
    search_tree(pattern, &parsed.syntax_node(), text)
}

/// Expands a replacement template for one match (unknown placeholders are kept verbatim).
pub fn expand_replacement(replacement: &str, m: &Match, source: &str) -> String {
    placeholder_regex()
        .replace_all(replacement, |caps: &regex::Captures| {
            let name = &caps[1];
            match m.bindings.iter().find(|(n, _)| n == name) {
                Some((_, r)) => source
                    .get(usize::from(r.start())..usize::from(r.end()))
                    .unwrap_or("")
                    .to_string(),
                None => caps[0].to_string(),
            }
        })
        .into_owned()
}

/// Replacement edits (non-overlapping, outermost matches win) for a parsed tree.
pub fn replace_tree(
    pattern: &Pattern,
    replacement: &str,
    root: &SyntaxNode,
    source: &str,
) -> Vec<(TextRange, String)> {
    let mut out: Vec<(TextRange, String)> = Vec::new();
    let mut last_end: Option<TextSize> = None;
    for m in search_tree(pattern, root, source) {
        if last_end.is_some_and(|e| m.range.start() < e) {
            continue;
        }
        out.push((m.range, expand_replacement(replacement, &m, source)));
        last_end = Some(m.range.end());
    }
    out
}

/// Replacement edits for a file of the analysis.
pub fn replace(pattern: &Pattern, replacement: &str, file: &FileInfo) -> Vec<(TextRange, String)> {
    replace_tree(pattern, replacement, &file.parse.syntax_node(), &file.text)
}

/// Replacement edits for raw text.
pub fn replace_text(pattern: &Pattern, replacement: &str, text: &str) -> Vec<(TextRange, String)> {
    let parsed = parse(text);
    replace_tree(pattern, replacement, &parsed.syntax_node(), text)
}

/// Applies non-overlapping edits to `text`.
pub fn apply_edits(text: &str, edits: &[(TextRange, String)]) -> String {
    let mut edits: Vec<&(TextRange, String)> = edits.iter().collect();
    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start()));
    let mut out = text.to_string();
    for (r, s) in edits {
        let (a, b) = (usize::from(r.start()), usize::from(r.end()));
        if a <= b && b <= out.len() {
            out.replace_range(a..b, s);
        }
    }
    out
}

/// Ready-made search templates: `(name, pattern)`.
pub fn builtin_templates() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Method call", "$obj$.$method$($args...$)"),
        ("Function call", "$func$($args...$)"),
        ("Assignment", "$x$ = $value$"),
        ("Augmented assignment", "$x$ += $value$"),
        ("Binary expression", "$a$ + $b$"),
        ("Comparison with None", "$x$ == None"),
        ("Print call", "print($args...$)"),
        ("Return statement", "return $value$"),
        (
            "If with else",
            "if $cond$:\n    $then...$\nelse:\n    $otherwise...$",
        ),
        ("For loop", "for $item$ in $iterable$:\n    $body...$"),
        ("While loop", "while $cond$:\n    $body...$"),
        (
            "Try/except",
            "try:\n    $body...$\nexcept $exc$:\n    $handler...$",
        ),
        (
            "Function definition",
            "def $name$($params...$):\n    $body...$",
        ),
        (
            "Decorated function",
            "@$decorator$\ndef $name$($params...$):\n    $body...$",
        ),
        ("Class with base", "class $name$($base$):\n    $body...$"),
        ("Lambda", "lambda $params...$: $body$"),
        ("From import", "from $module$ import $names...$"),
        ("With statement", "with $ctx$ as $name$:\n    $body...$"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(p: &str) -> Pattern {
        compile(p, &PatternOptions::default()).unwrap_or_else(|e| panic!("{p:?}: {e}"))
    }

    fn texts(text: &str, ms: &[Match]) -> Vec<String> {
        ms.iter()
            .map(|m| text[usize::from(m.range.start())..usize::from(m.range.end())].to_string())
            .collect()
    }

    fn bound<'a>(text: &'a str, m: &Match, name: &str) -> &'a str {
        let (_, r) = m
            .bindings
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no binding {name}"));
        &text[usize::from(r.start())..usize::from(r.end())]
    }

    #[test]
    fn expression_pattern_matches_subexpressions() {
        let text = "x = 1 + 2\ny = foo(a) + b\nz = a - b\n";
        let ms = search_text(&pat("$a$ + $b$"), text);
        assert_eq!(texts(text, &ms), vec!["1 + 2", "foo(a) + b"]);
        assert_eq!(bound(text, &ms[0], "a"), "1");
        assert_eq!(bound(text, &ms[0], "b"), "2");
        assert_eq!(bound(text, &ms[1], "a"), "foo(a)");
    }

    #[test]
    fn statement_pattern() {
        let text = "a = 1\nb += 2\nc = f(x)\nif t:\n    d = 3\n";
        let ms = search_text(&pat("$x$ = $value$"), text);
        assert_eq!(texts(text, &ms), vec!["a = 1", "c = f(x)", "d = 3"]);
    }

    #[test]
    fn placeholder_consistency() {
        let text = "a == a\na == b\nf(x) == f(x)\n";
        let ms = search_text(&pat("$x$ == $x$"), text);
        assert_eq!(texts(text, &ms), vec!["a == a", "f(x) == f(x)"]);
    }

    #[test]
    fn variadic_arguments_and_counts() {
        let text = "print()\nprint(1)\nprint(1, 2, k=3)\nlog(1)\n";
        let ms = search_text(&pat("print($args...$)"), text);
        assert_eq!(
            texts(text, &ms),
            vec!["print()", "print(1)", "print(1, 2, k=3)"]
        );
        assert_eq!(bound(text, &ms[2], "args"), "1, 2, k=3");
        assert_eq!(bound(text, &ms[0], "args"), "");
        let mut opts = PatternOptions::default();
        opts.constraints.insert(
            "args".into(),
            Constraint {
                max: 1,
                ..Default::default()
            },
        );
        let p = compile("print($args...$)", &opts).unwrap();
        assert_eq!(
            texts(text, &search_text(&p, text)),
            vec!["print()", "print(1)"]
        );
        let mut opts = PatternOptions::default();
        opts.constraints.insert(
            "args".into(),
            Constraint {
                min: 2,
                ..Default::default()
            },
        );
        let p = compile("print($args...$)", &opts).unwrap();
        assert_eq!(
            texts(text, &search_text(&p, text)),
            vec!["print(1, 2, k=3)"]
        );
    }

    #[test]
    fn method_call_template() {
        let text = "self.items.append(x)\nfoo(1)\nobj.run()\n";
        let ms = search_text(&pat("$obj$.$method$($args...$)"), text);
        assert_eq!(texts(text, &ms), vec!["self.items.append(x)", "obj.run()"]);
        assert_eq!(bound(text, &ms[0], "obj"), "self.items");
        assert_eq!(bound(text, &ms[0], "method"), "append");
        assert_eq!(bound(text, &ms[0], "args"), "x");
    }

    #[test]
    fn nested_blocks_ignore_whitespace_and_comments() {
        let text = "def f(items):\n    # leading comment\n    for  it in items :  # trailing\n\n        if it:\n            handle(it)\n            log(it)\n    return 1\n";
        let ms = search_text(&pat("if $c$:\n    $body...$"), text);
        assert_eq!(ms.len(), 1);
        assert_eq!(bound(text, &ms[0], "c"), "it");
        assert_eq!(
            bound(text, &ms[0], "body"),
            "handle(it)\n            log(it)"
        );
        let ms = search_text(&pat("for $x$ in $xs$:\n    $body...$"), text);
        assert_eq!(ms.len(), 1);
        assert_eq!(bound(text, &ms[0], "xs"), "items");
    }

    #[test]
    fn regex_constraint() {
        let text = "print(1)\nlog(2)\nprint_debug(3)\n";
        let mut opts = PatternOptions::default();
        opts.constraints.insert(
            "func".into(),
            Constraint {
                regex: Some("^print$".into()),
                ..Default::default()
            },
        );
        let p = compile("$func$($args...$)", &opts).unwrap();
        assert_eq!(texts(text, &search_text(&p, text)), vec!["print(1)"]);
        let mut opts = PatternOptions::default();
        opts.constraints.insert(
            "func".into(),
            Constraint {
                regex: Some("^print".into()),
                ..Default::default()
            },
        );
        let p = compile("$func$($args...$)", &opts).unwrap();
        assert_eq!(
            texts(text, &search_text(&p, text)),
            vec!["print(1)", "print_debug(3)"]
        );
    }

    #[test]
    fn kind_constraint() {
        let text = "f(1)\nf(x)\nf('s')\n";
        let mut opts = PatternOptions::default();
        opts.constraints.insert(
            "a".into(),
            Constraint {
                kind: Some(NodeKindFilter::Literal),
                ..Default::default()
            },
        );
        let p = compile("f($a$)", &opts).unwrap();
        assert_eq!(texts(text, &search_text(&p, text)), vec!["f(1)", "f('s')"]);
    }

    #[test]
    fn replace_round_trip() {
        let text = "if a == None:\n    pass\nb = c == None\n";
        let p = pat("$x$ == None");
        let edits = replace_text(&p, "$x$ is None", text);
        assert_eq!(edits.len(), 2);
        assert_eq!(
            apply_edits(text, &edits),
            "if a is None:\n    pass\nb = c is None\n"
        );
        // Variadic placeholders in the replacement keep the original argument text.
        let text = "print(1, 2)\nprint()\n";
        let edits = replace_text(&pat("print($args...$)"), "logger.info($args...$)", text);
        assert_eq!(
            apply_edits(text, &edits),
            "logger.info(1, 2)\nlogger.info()\n"
        );
    }

    #[test]
    fn replace_keeps_outermost_match() {
        let text = "v = a + b + c\n";
        let edits = replace_text(&pat("$a$ + $b$"), "add($a$, $b$)", text);
        assert_eq!(apply_edits(text, &edits), "v = add(a + b, c)\n");
    }

    #[test]
    fn statement_sequence() {
        let text =
            "def f():\n    tmp = compute()\n    return tmp\n\ndef g():\n    a = 1\n    return b\n";
        let ms = search_text(&pat("$x$ = $v$\nreturn $x$"), text);
        assert_eq!(ms.len(), 1);
        assert_eq!(texts(text, &ms), vec!["tmp = compute()\n    return tmp"]);
        assert_eq!(bound(text, &ms[0], "v"), "compute()");
    }

    #[test]
    fn statement_placeholder_matches_any_statement() {
        let text = "x = 1\nif c:\n    pass\nreturn 2\n";
        let ms = search_text(&pat("$s$"), text);
        // Three top-level statements plus the nested `pass`.
        assert_eq!(
            texts(text, &ms),
            vec!["x = 1", "if c:\n    pass", "pass", "return 2"]
        );
    }

    #[test]
    fn identifier_placeholders_in_definitions() {
        let text = "@dec\ndef f(a: int = 1, *args):\n    return a\n\ndef g():\n    pass\n\nclass K(Base):\n    x = 1\n";
        let ms = search_text(&pat("def $name$($params...$):\n    $body...$"), text);
        assert_eq!(ms.len(), 2);
        assert_eq!(bound(text, &ms[0], "name"), "f");
        assert_eq!(bound(text, &ms[0], "params"), "a: int = 1, *args");
        assert_eq!(bound(text, &ms[1], "params"), "");
        let ms = search_text(&pat("@$d$\ndef $name$($params...$):\n    $body...$"), text);
        assert_eq!(ms.len(), 1);
        assert_eq!(bound(text, &ms[0], "d"), "dec");
        let ms = search_text(&pat("class $name$($base$):\n    $body...$"), text);
        assert_eq!(ms.len(), 1);
        assert_eq!(bound(text, &ms[0], "base"), "Base");
    }

    #[test]
    fn import_and_with_templates() {
        let text = "from os import path, sep\nfrom os.path import join\nwith open(f) as fh:\n    data = fh.read()\n";
        let ms = search_text(&pat("from $module$ import $names...$"), text);
        assert_eq!(ms.len(), 2);
        assert_eq!(bound(text, &ms[0], "names"), "path, sep");
        assert_eq!(bound(text, &ms[1], "module"), "os.path");
        let ms = search_text(&pat("with $ctx$ as $name$:\n    $body...$"), text);
        assert_eq!(ms.len(), 1);
        assert_eq!(bound(text, &ms[0], "ctx"), "open(f)");
        assert_eq!(bound(text, &ms[0], "name"), "fh");
    }

    #[test]
    fn builtin_templates_compile() {
        for (name, p) in builtin_templates() {
            compile(p, &PatternOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn errors() {
        assert!(compile("", &PatternOptions::default()).is_err());
        assert!(compile("def (", &PatternOptions::default()).is_err());
        let mut opts = PatternOptions::default();
        opts.constraints.insert(
            "x".into(),
            Constraint {
                regex: Some("(".into()),
                ..Default::default()
            },
        );
        assert!(compile("$x$", &opts).is_err());
    }

    #[test]
    fn placeholder_names_and_flags() {
        let p = pat("$obj$.$method$($args...$)");
        assert_eq!(p.placeholder_names(), vec!["obj", "method", "args"]);
        assert!(p.is_expression());
        assert!(!pat("$x$ = 1").is_expression());
    }
}
