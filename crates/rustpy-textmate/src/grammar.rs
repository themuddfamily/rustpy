//! Linked grammars: raw rules with every include resolved into an arena of
//! rules. Regexes are compiled lazily, on first use.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::matcher::{self, Groups, Matcher};
use crate::raw::{
    GrammarError, RawCaptures, RawGrammar, RawPattern, RawRule, RawRuleId, Repository,
};
use crate::tokenizer::{StackState, Token};

/// Index of a [`Rule`] in [`Grammar::rules`].
pub(crate) type RuleId = usize;
/// Index of a [`Pattern`] in [`Grammar::patterns`].
pub(crate) type PatternId = usize;

/// Distinguishes grammars so a [`StackState`] is never used with the wrong
/// one.
static NEXT_GRAMMAR_ID: AtomicU64 = AtomicU64::new(1);

/// Back-referencing `end`/`while` patterns compiled per distinct source are
/// cached up to this many entries.
const DYNAMIC_CACHE_LIMIT: usize = 256;

/// A regex compiled on first use.
pub(crate) struct Pattern {
    pub source: String,
    compiled: OnceLock<Result<Matcher, String>>,
}

impl Pattern {
    fn new(source: String) -> Pattern {
        Pattern {
            source,
            compiled: OnceLock::new(),
        }
    }

    fn compiled(&self) -> &Result<Matcher, String> {
        self.compiled.get_or_init(|| {
            let compiled = Matcher::new(&self.source);
            if let Err(e) = &compiled {
                log::debug!(
                    "textmate: skipping unsupported pattern {:?}: {e}",
                    self.source
                );
            }
            compiled
        })
    }

    /// The compiled matcher, or `None` when the pattern is unsupported.
    pub(crate) fn matcher(&self) -> Option<&Matcher> {
        self.compiled().as_ref().ok()
    }
}

/// A `name` / `contentName`, possibly referring to captures (`$1`,
/// `${1:/downcase}`).
#[derive(Debug)]
pub(crate) struct Name {
    raw: String,
    dynamic: bool,
}

impl Name {
    fn new(raw: Option<&str>) -> Option<Name> {
        let raw = raw?.trim();
        (!raw.is_empty()).then(|| Name {
            raw: raw.to_string(),
            dynamic: raw.contains('$'),
        })
    }

    /// Appends the (space separated) scopes this name denotes.
    pub(crate) fn push_scopes(&self, text: &str, groups: &Groups, out: &mut Vec<String>) {
        let resolved = if self.dynamic {
            Cow::Owned(replace_captures(&self.raw, text, groups))
        } else {
            Cow::Borrowed(&self.raw)
        };
        out.extend(resolved.split_whitespace().map(str::to_string));
    }
}

/// Scopes of an optional name (empty when absent).
pub(crate) fn name_scopes(name: Option<&Name>, text: &str, groups: &Groups) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(name) = name {
        name.push_scopes(text, groups, &mut out);
    }
    out
}

/// Substitutes `$n`, `${n:/downcase}` and `${n:/upcase}` like
/// vscode-textmate: leading dots of the captured text are dropped, a group
/// that did not participate yields the empty string and an unknown group
/// number is left as written.
fn replace_captures(raw: &str, text: &str, groups: &Groups) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some((n, command, len)) = capture_reference(rest) else {
            out.push('$');
            rest = &rest[1..];
            continue;
        };
        match groups.get(n) {
            Some(span) => {
                let captured = span
                    .and_then(|(s, e)| text.get(s..e))
                    .unwrap_or("")
                    .trim_start_matches('.');
                match command {
                    Some("downcase") => out.push_str(&captured.to_lowercase()),
                    Some(_) => out.push_str(&captured.to_uppercase()),
                    None => out.push_str(captured),
                }
            }
            None => out.push_str(&rest[..len]),
        }
        rest = &rest[len..];
    }
    out.push_str(rest);
    out
}

/// At `$`: `(group, command, length)` of `$12` or `${12:/upcase}`.
fn capture_reference(s: &str) -> Option<(usize, Option<&'static str>, usize)> {
    let after = &s[1..];
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 {
        return Some((after[..digits].parse().ok()?, None, 1 + digits));
    }
    let inner = after.strip_prefix('{')?;
    let digits = inner.bytes().take_while(u8::is_ascii_digit).count();
    let n = inner[..digits].parse().ok()?;
    for command in ["downcase", "upcase"] {
        let tail = format!(":/{command}}}");
        if inner[digits..].starts_with(&tail) {
            return Some((n, Some(command), 2 + digits + tail.len()));
        }
    }
    None
}

/// What to do with one capture group.
#[derive(Debug)]
pub(crate) struct CaptureRule {
    pub name: Option<Name>,
    pub content_name: Option<Name>,
    /// Tokenize the captured text with this rule's patterns.
    pub retokenize: Option<RuleId>,
}

/// Capture group number → rule.
pub(crate) type Captures = Vec<Option<CaptureRule>>;

/// An `end` / `while` pattern.
#[derive(Debug)]
pub(crate) struct EndPattern {
    pub source: String,
    /// `None` when absent (never matches) or when it has back-references
    /// (compiled per begin match).
    pub pattern: Option<PatternId>,
    pub back_refs: bool,
}

#[derive(Debug)]
pub(crate) enum RuleKind {
    Match {
        pattern: PatternId,
        captures: Captures,
    },
    BeginEnd {
        begin: PatternId,
        begin_captures: Captures,
        end: EndPattern,
        end_captures: Captures,
        apply_end_last: bool,
        patterns: Vec<RuleId>,
    },
    BeginWhile {
        begin: PatternId,
        begin_captures: Captures,
        while_: EndPattern,
        while_captures: Captures,
        patterns: Vec<RuleId>,
    },
    /// Only `patterns` (or an `include`): flattened into the including list.
    IncludeOnly { patterns: Vec<RuleId> },
}

#[derive(Debug)]
pub(crate) struct Rule {
    pub name: Option<Name>,
    pub content_name: Option<Name>,
    pub kind: RuleKind,
    /// Match/begin rules tried inside this rule, include-only rules
    /// flattened (computed on first use).
    candidates: OnceLock<Vec<RuleId>>,
}

impl Rule {
    fn new(name: Option<Name>, content_name: Option<Name>, kind: RuleKind) -> Rule {
        Rule {
            name,
            content_name,
            kind,
            candidates: OnceLock::new(),
        }
    }

    fn patterns(&self) -> &[RuleId] {
        match &self.kind {
            RuleKind::BeginEnd { patterns, .. }
            | RuleKind::BeginWhile { patterns, .. }
            | RuleKind::IncludeOnly { patterns } => patterns,
            RuleKind::Match { .. } => &[],
        }
    }

    fn patterns_mut(&mut self) -> Option<&mut Vec<RuleId>> {
        match &mut self.kind {
            RuleKind::BeginEnd { patterns, .. }
            | RuleKind::BeginWhile { patterns, .. }
            | RuleKind::IncludeOnly { patterns } => Some(patterns),
            RuleKind::Match { .. } => None,
        }
    }
}

/// A pattern of a grammar that cannot be translated for the `regex` crate
/// (it is skipped while tokenizing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedPattern {
    /// The Oniguruma source.
    pub pattern: String,
    /// Why it is unsupported.
    pub reason: String,
}

/// A grammar ready for tokenizing: the root grammar plus every grammar it
/// includes that was available when it was linked.
pub struct Grammar {
    pub(crate) id: u64,
    scope_name: String,
    name: Option<String>,
    file_types: Vec<String>,
    first_line_match: Option<String>,
    pub(crate) rules: Vec<Rule>,
    pub(crate) patterns: Vec<Pattern>,
    pub(crate) root: RuleId,
    dynamic: Mutex<HashMap<String, Option<Arc<Matcher>>>>,
}

impl std::fmt::Debug for Grammar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grammar")
            .field("scope_name", &self.scope_name)
            .field("rules", &self.rules.len())
            .field("patterns", &self.patterns.len())
            .finish()
    }
}

impl Grammar {
    /// Loads a standalone `.tmLanguage.json` grammar (includes of other
    /// grammars are dropped; use a [`crate::Registry`] for those).
    pub fn from_json(src: &str) -> Result<Grammar, GrammarError> {
        Ok(Grammar::from_raw(&RawGrammar::from_json(src)?))
    }

    /// Loads a standalone XML plist (`.tmLanguage`) grammar.
    pub fn from_plist(src: &str) -> Result<Grammar, GrammarError> {
        Ok(Grammar::from_raw(&RawGrammar::from_plist(src)?))
    }

    /// Links a parsed grammar on its own.
    pub fn from_raw(raw: &RawGrammar) -> Grammar {
        link(raw, &|_| None)
    }

    /// The root scope, e.g. `source.rust`.
    pub fn scope_name(&self) -> &str {
        &self.scope_name
    }

    /// The display name, e.g. `Rust`.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// File extensions / names the grammar applies to.
    pub fn file_types(&self) -> &[String] {
        &self.file_types
    }

    /// The `firstLineMatch` regex (Oniguruma syntax), if any.
    pub fn first_line_match(&self) -> Option<&str> {
        self.first_line_match.as_deref()
    }

    /// Tokenizes one line; see [`crate::tokenize_line`].
    pub fn tokenize_line(&self, line: &str, state: &mut StackState) -> Vec<Token> {
        crate::tokenizer::tokenize_line(self, line, state)
    }

    /// Compiles every pattern and returns those that are not supported (they
    /// never match). Back-referencing `end`/`while` patterns are compiled per
    /// match and are not included.
    pub fn unsupported_patterns(&self) -> Vec<UnsupportedPattern> {
        self.patterns
            .iter()
            .filter_map(|p| match p.compiled() {
                Ok(_) => None,
                Err(reason) => Some(UnsupportedPattern {
                    pattern: p.source.clone(),
                    reason: reason.clone(),
                }),
            })
            .collect()
    }

    pub(crate) fn rule(&self, id: RuleId) -> Option<&Rule> {
        self.rules.get(id)
    }

    pub(crate) fn pattern(&self, id: PatternId) -> Option<&Matcher> {
        self.patterns.get(id)?.matcher()
    }

    /// Match/begin rules tried while `id` is the innermost rule, in priority
    /// order (include-only rules flattened, duplicates dropped).
    pub(crate) fn candidates(&self, id: RuleId) -> &[RuleId] {
        let Some(rule) = self.rules.get(id) else {
            return &[];
        };
        rule.candidates.get_or_init(|| {
            let mut out = Vec::new();
            let mut seen = HashSet::new();
            let mut expanded = HashSet::from([id]);
            let mut stack = vec![rule.patterns().iter()];
            while let Some(iter) = stack.last_mut() {
                let Some(&child) = iter.next() else {
                    stack.pop();
                    continue;
                };
                let Some(child_rule) = self.rules.get(child) else {
                    continue;
                };
                match &child_rule.kind {
                    RuleKind::IncludeOnly { patterns } => {
                        if expanded.insert(child) {
                            stack.push(patterns.iter());
                        }
                    }
                    _ => {
                        if seen.insert(child) {
                            out.push(child);
                        }
                    }
                }
            }
            out
        })
    }

    /// A back-referencing `end`/`while` pattern after substitution.
    pub(crate) fn dynamic_matcher(&self, source: &str) -> Option<Arc<Matcher>> {
        if let Some(cached) = self
            .dynamic
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(source)
        {
            return cached.clone();
        }
        let compiled = match Matcher::new(source) {
            Ok(m) => Some(Arc::new(m)),
            Err(e) => {
                log::debug!("textmate: skipping unsupported pattern {source:?}: {e}");
                None
            }
        };
        let mut cache = self.dynamic.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.len() >= DYNAMIC_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(source.to_string(), compiled.clone());
        compiled
    }
}

/// Links `base` with the grammars `lookup` provides (by scope name).
pub(crate) fn link<'a>(
    base: &'a RawGrammar,
    lookup: &'a dyn Fn(&str) -> Option<&'a RawGrammar>,
) -> Grammar {
    let mut linker = Linker {
        grammars: vec![base],
        lookup,
        rules: Vec::new(),
        patterns: Vec::new(),
        pattern_ids: HashMap::new(),
        raw_ids: HashMap::new(),
        self_ids: HashMap::new(),
        jobs: VecDeque::new(),
        pattern_counts: HashMap::new(),
    };
    let root = linker.self_rule(0);
    while let Some(job) = linker.jobs.pop_front() {
        linker.build(job);
    }
    linker.prune_missing();
    Grammar {
        id: NEXT_GRAMMAR_ID.fetch_add(1, Ordering::Relaxed),
        scope_name: base.scope_name.clone(),
        name: base.name.clone(),
        file_types: base.file_types.clone(),
        first_line_match: base.first_line_match.clone(),
        rules: linker.rules,
        patterns: linker.patterns,
        root,
        dynamic: Mutex::new(HashMap::new()),
    }
}

/// A rule to build: its arena slot is allocated when first referenced so
/// recursive includes resolve to it; the body is built from the queue.
struct Job<'a> {
    rule: RuleId,
    grammar: usize,
    source: JobSource,
    /// Repositories visible to `#name` includes, innermost last.
    repos: Vec<&'a Repository>,
}

enum JobSource {
    Raw(RawRuleId),
    /// The grammar's top-level `patterns` (`$self`).
    SelfRule,
}

struct Linker<'a> {
    /// Index 0 is the base grammar (`$base`).
    grammars: Vec<&'a RawGrammar>,
    lookup: &'a dyn Fn(&str) -> Option<&'a RawGrammar>,
    rules: Vec<Rule>,
    patterns: Vec<Pattern>,
    pattern_ids: HashMap<String, PatternId>,
    raw_ids: HashMap<(usize, RawRuleId), RuleId>,
    self_ids: HashMap<usize, RuleId>,
    jobs: VecDeque<Job<'a>>,
    /// Number of `patterns` entries (before resolution) of container rules.
    pattern_counts: HashMap<RuleId, usize>,
}

impl<'a> Linker<'a> {
    fn placeholder(&mut self) -> RuleId {
        self.rules.push(Rule::new(
            None,
            None,
            RuleKind::IncludeOnly {
                patterns: Vec::new(),
            },
        ));
        self.rules.len() - 1
    }

    fn raw_rule(&mut self, grammar: usize, raw: RawRuleId, repos: &[&'a Repository]) -> RuleId {
        if let Some(&id) = self.raw_ids.get(&(grammar, raw)) {
            return id;
        }
        let id = self.placeholder();
        self.raw_ids.insert((grammar, raw), id);
        self.jobs.push_back(Job {
            rule: id,
            grammar,
            source: JobSource::Raw(raw),
            repos: repos.to_vec(),
        });
        id
    }

    fn self_rule(&mut self, grammar: usize) -> RuleId {
        if let Some(&id) = self.self_ids.get(&grammar) {
            return id;
        }
        let id = self.placeholder();
        self.self_ids.insert(grammar, id);
        let repos = vec![&self.grammars[grammar].repository];
        self.jobs.push_back(Job {
            rule: id,
            grammar,
            source: JobSource::SelfRule,
            repos,
        });
        id
    }

    fn grammar_index(&mut self, scope: &str) -> Option<usize> {
        if let Some(i) = self.grammars.iter().position(|g| g.scope_name == scope) {
            return Some(i);
        }
        let found = (self.lookup)(scope)?;
        self.grammars.push(found);
        Some(self.grammars.len() - 1)
    }

    fn pattern(&mut self, source: &str) -> PatternId {
        if let Some(&id) = self.pattern_ids.get(source) {
            return id;
        }
        self.patterns.push(Pattern::new(source.to_string()));
        let id = self.patterns.len() - 1;
        self.pattern_ids.insert(source.to_string(), id);
        id
    }

    fn end_pattern(&mut self, source: Option<&str>) -> EndPattern {
        match source {
            None => EndPattern {
                source: String::new(),
                pattern: None,
                back_refs: false,
            },
            Some(source) => {
                let back_refs = matcher::has_back_references(source);
                let pattern = (!back_refs).then(|| self.pattern(source));
                EndPattern {
                    source: source.to_string(),
                    pattern,
                    back_refs,
                }
            }
        }
    }

    /// Resolves an `include`; `None` when the target does not exist.
    fn resolve(
        &mut self,
        grammar: usize,
        include: &str,
        repos: &[&'a Repository],
    ) -> Option<RuleId> {
        match include {
            "$self" => return Some(self.self_rule(grammar)),
            "$base" => return Some(self.self_rule(0)),
            _ => {}
        }
        if let Some(name) = include.strip_prefix('#') {
            let raw = repos
                .iter()
                .rev()
                .find_map(|repo| repo.get(name).copied())?;
            return Some(self.raw_rule(grammar, raw, repos));
        }
        let (scope, rule) = match include.split_once('#') {
            Some((scope, rule)) => (scope, Some(rule)),
            None => (include, None),
        };
        let target = self.grammar_index(scope)?;
        match rule {
            None => Some(self.self_rule(target)),
            Some(name) => {
                let repo = &self.grammars[target].repository;
                let raw = *repo.get(name)?;
                Some(self.raw_rule(target, raw, &[repo]))
            }
        }
    }

    /// Resolves a `patterns` list; also returns its length before
    /// resolution (disabled rules excluded).
    fn patterns(
        &mut self,
        grammar: usize,
        list: &[RawPattern],
        repos: &[&'a Repository],
    ) -> (Vec<RuleId>, usize) {
        let mut out = Vec::new();
        let mut count = 0;
        for pattern in list {
            match pattern {
                RawPattern::Include(include) => {
                    count += 1;
                    out.extend(self.resolve(grammar, include, repos));
                }
                RawPattern::Rule(raw) => {
                    if self.grammars[grammar]
                        .rules
                        .get(*raw)
                        .is_some_and(|r| r.disabled)
                    {
                        continue;
                    }
                    count += 1;
                    out.push(self.raw_rule(grammar, *raw, repos));
                }
            }
        }
        (out, count)
    }

    fn captures(
        &mut self,
        grammar: usize,
        captures: Option<&RawCaptures>,
        repos: &[&'a Repository],
    ) -> Captures {
        let Some(captures) = captures else {
            return Vec::new();
        };
        let len = captures.last().map_or(0, |(n, _)| n + 1);
        let mut out: Captures = (0..len).map(|_| None).collect();
        for &(n, raw) in captures {
            let Some(rule) = self.grammars[grammar].rules.get(raw) else {
                continue;
            };
            let retokenize = rule
                .patterns
                .is_some()
                .then(|| self.raw_rule(grammar, raw, repos));
            out[n] = Some(CaptureRule {
                name: Name::new(rule.name.as_deref()),
                content_name: Name::new(rule.content_name.as_deref()),
                retokenize,
            });
        }
        out
    }

    fn build(&mut self, job: Job<'a>) {
        let grammar = self.grammars[job.grammar];
        let (rule, count) = match job.source {
            JobSource::SelfRule => {
                let (patterns, count) = self.patterns(job.grammar, &grammar.patterns, &job.repos);
                (
                    Rule::new(None, None, RuleKind::IncludeOnly { patterns }),
                    Some(count),
                )
            }
            JobSource::Raw(raw) => match grammar.rules.get(raw) {
                Some(raw_rule) => self.build_raw(job.grammar, raw_rule, job.repos),
                None => return,
            },
        };
        if let Some(count) = count {
            self.pattern_counts.insert(job.rule, count);
        }
        self.rules[job.rule] = rule;
    }

    fn build_raw(
        &mut self,
        grammar: usize,
        r: &'a RawRule,
        mut repos: Vec<&'a Repository>,
    ) -> (Rule, Option<usize>) {
        if let Some(repo) = &r.repository {
            repos.push(repo);
        }
        let name = Name::new(r.name.as_deref());
        let content_name = Name::new(r.content_name.as_deref());
        if let Some(source) = &r.match_ {
            let pattern = self.pattern(source);
            let captures = self.captures(grammar, r.captures.as_ref(), &repos);
            return (
                Rule::new(name, content_name, RuleKind::Match { pattern, captures }),
                None,
            );
        }
        let Some(begin) = &r.begin else {
            let include;
            let list: &[RawPattern] = match (&r.patterns, &r.include) {
                (Some(patterns), _) => patterns,
                (None, Some(target)) => {
                    include = [RawPattern::Include(target.clone())];
                    &include
                }
                (None, None) => &[],
            };
            let (patterns, count) = self.patterns(grammar, list, &repos);
            return (
                Rule::new(name, content_name, RuleKind::IncludeOnly { patterns }),
                Some(count),
            );
        };
        let begin = self.pattern(begin);
        let (patterns, count) =
            self.patterns(grammar, r.patterns.as_deref().unwrap_or(&[]), &repos);
        let begin_captures = self.captures(
            grammar,
            r.begin_captures.as_ref().or(r.captures.as_ref()),
            &repos,
        );
        let kind = match &r.while_ {
            Some(while_) => RuleKind::BeginWhile {
                begin,
                begin_captures,
                while_: self.end_pattern(Some(while_)),
                while_captures: self.captures(
                    grammar,
                    r.while_captures.as_ref().or(r.captures.as_ref()),
                    &repos,
                ),
                patterns,
            },
            None => RuleKind::BeginEnd {
                begin,
                begin_captures,
                end: self.end_pattern(r.end.as_deref()),
                end_captures: self.captures(
                    grammar,
                    r.end_captures.as_ref().or(r.captures.as_ref()),
                    &repos,
                ),
                apply_end_last: r.apply_end_pattern_last,
                patterns,
            },
        };
        (Rule::new(name, content_name, kind), Some(count))
    }

    /// Like vscode-textmate, drops rules whose `patterns` were all
    /// unresolvable includes (e.g. an embedded language that is not
    /// installed) from the lists that include them, transitively.
    fn prune_missing(&mut self) {
        let mut skip = vec![false; self.rules.len()];
        loop {
            let mut changed = false;
            for (&rule, &count) in &self.pattern_counts {
                if skip[rule] || count == 0 {
                    continue;
                }
                if self.rules[rule].patterns().iter().all(|&child| skip[child]) {
                    skip[rule] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for rule in &mut self.rules {
            if let Some(patterns) = rule.patterns_mut() {
                patterns.retain(|&child| !skip[child]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(spans: &[Option<(usize, usize)>]) -> Groups {
        spans.to_vec()
    }

    #[test]
    fn capture_substitution_in_names() {
        let text = "..Foo bar";
        let g = groups(&[Some((0, 9)), Some((0, 5)), None]);
        assert_eq!(replace_captures("entity.$1.x", text, &g), "entity.Foo.x");
        assert_eq!(replace_captures("a.${1:/downcase}", text, &g), "a.foo");
        assert_eq!(replace_captures("a.${1:/upcase}.$2", text, &g), "a.FOO.");
        assert_eq!(
            replace_captures("a.$7 $ ${x} ${1:/nope}", text, &g),
            "a.$7 $ ${x} ${1:/nope}"
        );
        let name = Name::new(Some(" meta.$1   string ")).unwrap();
        let mut out = Vec::new();
        name.push_scopes("Foo", &groups(&[Some((0, 3)), Some((0, 3))]), &mut out);
        assert_eq!(out, vec!["meta.Foo".to_string(), "string".to_string()]);
        assert!(Name::new(Some("   ")).is_none());
    }

    fn grammar(json: &str) -> Grammar {
        Grammar::from_json(json).unwrap()
    }

    #[test]
    fn includes_resolve_and_flatten() {
        let g = grammar(
            r##"{
                "scopeName": "source.t",
                "patterns": [ { "include": "#a" }, { "include": "$self" }, { "include": "source.missing" }, { "match": "z" } ],
                "repository": {
                    "a": { "patterns": [ { "include": "#b" }, { "match": "x" }, { "include": "#a" } ] },
                    "b": { "match": "y" }
                }
            }"##,
        );
        let candidates: Vec<&str> = g
            .candidates(g.root)
            .iter()
            .map(|&id| match &g.rules[id].kind {
                RuleKind::Match { pattern, .. } => g.patterns[*pattern].source.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(candidates, vec!["y", "x", "z"]);
    }

    #[test]
    fn rules_with_only_missing_includes_are_dropped() {
        let g = grammar(
            r##"{
                "scopeName": "source.t",
                "patterns": [
                    { "begin": "```py", "end": "```", "patterns": [ { "include": "source.python" } ] },
                    { "begin": "```", "end": "```" },
                    { "include": "#outer" }
                ],
                "repository": { "outer": { "patterns": [ { "include": "#inner" } ] }, "inner": { "patterns": [ { "include": "#nope" } ] } }
            }"##,
        );
        let begins: Vec<&str> = g
            .candidates(g.root)
            .iter()
            .map(|&id| match &g.rules[id].kind {
                RuleKind::BeginEnd { begin, .. } => g.patterns[*begin].source.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(begins, vec!["```"]);
    }

    #[test]
    fn identical_sources_share_one_pattern_and_errors_are_listed() {
        let g = grammar(
            r#"{ "scopeName": "s", "patterns": [ { "match": "a" }, { "match": "a" }, { "match": "(.)\\1" } ] }"#,
        );
        assert_eq!(g.patterns.len(), 2);
        let unsupported = g.unsupported_patterns();
        assert_eq!(unsupported.len(), 1);
        assert_eq!(unsupported[0].pattern, r"(.)\1");
    }

    #[test]
    fn grammar_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<Grammar>();
    }
}
