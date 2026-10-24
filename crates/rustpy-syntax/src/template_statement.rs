//! Known default template statement grammar. Unknown extension tags remain opaque.
use crate::{
    template::Dialect,
    template_expression::{self, Context},
};
use std::ops::Range;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementKind {
    For,
    Set,
    Macro,
    Include,
    Import,
    From,
    With,
    Load,
    Url,
    Block,
    Extends,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    MissingClause,
    InvalidTarget,
    InvalidOption,
    InvalidParameter,
    DuplicateBinding,
    InvalidExpression,
    UnbalancedDelimiter,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub error: Error,
}
#[derive(Debug)]
pub struct Statement {
    pub kind: Option<StatementKind>,
    pub bindings: Vec<Range<usize>>,
    pub expressions: Vec<Range<usize>>,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Clone, Debug)]
struct Token {
    range: Range<usize>,
    depth: usize,
}
fn tokens(src: &str, range: Range<usize>) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = range.start;
    let mut depth = 0usize;
    while i < range.end {
        let c = src[i..range.end].chars().next().unwrap();
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let start = i;
        let before = depth;
        if matches!(c, '\'' | '"') {
            let quote = c;
            i += 1;
            while i < range.end {
                let c = src[i..range.end].chars().next().unwrap();
                i += c.len_utf8();
                if c == '\\' {
                    if let Some(c) = src[i..range.end].chars().next() {
                        i += c.len_utf8();
                    }
                } else if c == quote {
                    break;
                }
            }
        } else if c == '_' || unicode_ident::is_xid_start(c) || c.is_ascii_digit() {
            i += c.len_utf8();
            while let Some(c) = src[i..range.end].chars().next() {
                if c == '_' || unicode_ident::is_xid_continue(c) {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
        } else {
            i += c.len_utf8();
            if matches!(c, '(' | '[' | '{') {
                depth += 1;
            } else if matches!(c, ')' | ']' | '}') {
                depth = depth.saturating_sub(1);
            }
        }
        out.push(Token {
            range: start..i,
            depth: before,
        });
    }
    out
}
fn trim(src: &str, r: Range<usize>) -> Range<usize> {
    let text = &src[r.clone()];
    let left = text.len() - text.trim_start().len();
    r.start + left..r.start + text.trim_end().len().max(left)
}
fn name(src: &str) -> bool {
    let mut chars = src.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || unicode_ident::is_xid_start(c))
        && chars.all(|c| c == '_' || unicode_ident::is_xid_continue(c))
        && !matches!(src, "true" | "false" | "none" | "True" | "False" | "None")
}
struct Validator<'a> {
    src: &'a str,
    dialect: Dialect,
    result: Statement,
}
impl Validator<'_> {
    fn error(&mut self, range: Range<usize>, error: Error) {
        self.result.diagnostics.push(Diagnostic { range, error });
    }
    fn expression(&mut self, r: Range<usize>, context: Context) {
        let r = trim(self.src, r);
        self.result.expressions.push(r.clone());
        let ast = template_expression::parse(&self.src[r.clone()], self.dialect, context);
        for d in ast.diagnostics {
            self.error(
                r.start + d.range.start..r.start + d.range.end,
                Error::InvalidExpression,
            );
        }
    }
    fn target(&mut self, r: Range<usize>, tuple: bool, attribute: bool) {
        let r = trim(self.src, r);
        if self.dialect == Dialect::Django {
            let mut offset = r.start;
            for part in self.src[r.clone()].split(',') {
                let n = trim(self.src, offset..offset + part.len());
                offset += part.len() + 1;
                if n.is_empty()
                    || self.src[n.clone()]
                        .chars()
                        .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '|'))
                {
                    self.error(n, Error::InvalidTarget);
                } else {
                    self.result.bindings.push(n);
                }
            }
            return;
        }
        let t = tokens(self.src, r.clone());
        if attribute
            && t.len() == 3
            && &self.src[t[1].range.clone()] == "."
            && name(&self.src[t[0].range.clone()])
            && name(&self.src[t[2].range.clone()])
        {
            self.result.bindings.push(r);
            return;
        }
        let mut expected = true;
        let mut stack = Vec::new();
        let mut seen = false;
        let mut valid = !t.is_empty();
        for t in t {
            let text = &self.src[t.range.clone()];
            match text {
                "(" if tuple && expected => {
                    stack.push(());
                    seen = false;
                }
                ")" if tuple && !stack.is_empty() && seen => {
                    stack.pop();
                    seen = true;
                    expected = false;
                }
                "," if tuple && !expected => expected = true,
                _ if expected && name(text) => {
                    self.result.bindings.push(t.range);
                    seen = true;
                    expected = false;
                }
                _ => valid = false,
            }
        }
        if !valid || !stack.is_empty() || !seen {
            self.error(r, Error::InvalidTarget);
        }
    }
    fn single_name(&mut self, r: Range<usize>) {
        let r = trim(self.src, r);
        if name(&self.src[r.clone()]) {
            self.result.bindings.push(r);
        } else {
            self.error(r, Error::InvalidTarget);
        }
    }
    fn top(&self, r: Range<usize>, text: &str) -> Option<Range<usize>> {
        tokens(self.src, r)
            .into_iter()
            .find(|t| t.depth == 0 && &self.src[t.range.clone()] == text)
            .map(|t| t.range)
    }
    fn suffix_context(&mut self, r: Range<usize>) -> Range<usize> {
        let t = tokens(self.src, r.clone());
        if t.len() >= 2 {
            let a = &t[t.len() - 2];
            let b = &t[t.len() - 1];
            if a.depth == 0
                && b.depth == 0
                && &self.src[b.range.clone()] == "context"
                && matches!(&self.src[a.range.clone()], "with" | "without")
            {
                return trim(self.src, r.start..a.range.start);
            }
        }
        r
    }
    fn for_statement(&mut self, r: Range<usize>) {
        if self.dialect == Dialect::Django {
            let words = self.django_words(r.clone());
            let reversed = words
                .last()
                .is_some_and(|w| &self.src[w.clone()] == "reversed");
            let needed = if reversed { 4 } else { 3 };
            if words.len() < needed {
                self.error(r, Error::MissingClause);
                return;
            }
            let in_index = words.len() - if reversed { 3 } else { 2 };
            let clause = &words[in_index];
            if &self.src[clause.clone()] != "in" {
                self.error(clause.clone(), Error::MissingClause);
                return;
            }
            self.target(r.start..clause.start, true, false);
            self.expression(words[in_index + 1].clone(), Context::Output);
            return;
        }
        let Some(in_clause) = self.top(r.clone(), "in") else {
            self.error(r, Error::MissingClause);
            return;
        };
        self.target(r.start..in_clause.start, true, false);
        let mut value = trim(self.src, in_clause.end..r.end);
        let t = tokens(self.src, value.clone());
        if let Some(last) = t.last() {
            let modifier = if self.dialect == Dialect::Jinja {
                "recursive"
            } else {
                "reversed"
            };
            if last.depth == 0 && &self.src[last.range.clone()] == modifier {
                let prefix = trim(self.src, value.start..last.range.start);
                if self.dialect == Dialect::Django
                    || !prefix.is_empty()
                        && template_expression::parse(
                            &self.src[prefix.clone()],
                            self.dialect,
                            Context::Output,
                        )
                        .diagnostics
                        .is_empty()
                {
                    value = prefix;
                }
            }
        }
        if self.dialect == Dialect::Jinja {
            if let Some(filter) = self.top(value.clone(), "if") {
                self.expression(value.start..filter.start, Context::Condition);
                self.expression(filter.end..value.end, Context::Output);
                return;
            }
        }
        self.expression(value, Context::Condition);
    }
    fn set_statement(&mut self, r: Range<usize>) {
        if let Some(eq) = self.top(r.clone(), "=") {
            self.target(r.start..eq.start, true, true);
            self.expression(eq.end..r.end, Context::Output);
        } else {
            let target_end = self.top(r.clone(), "|").map_or(r.end, |p| p.start);
            self.single_name(r.start..target_end);
            if target_end < r.end {
                self.expression(r, Context::Output);
            }
        }
    }
    fn macro_statement(&mut self, r: Range<usize>) {
        let t = tokens(self.src, r.clone());
        let Some(first) = t.first() else {
            self.error(r, Error::MissingClause);
            return;
        };
        self.single_name(first.range.clone());
        let Some(open) = t.get(1).filter(|t| &self.src[t.range.clone()] == "(") else {
            self.error(first.range.end..r.end, Error::MissingClause);
            return;
        };
        let Some(close) = t
            .last()
            .filter(|t| &self.src[t.range.clone()] == ")" && t.depth == 1)
        else {
            self.error(open.range.start..r.end, Error::UnbalancedDelimiter);
            return;
        };
        let params = open.range.end..close.range.start;
        let t = tokens(self.src, params.clone());
        let mut start = params.start;
        let mut default = false;
        let separators = t
            .iter()
            .filter(|t| t.depth == 0 && &self.src[t.range.clone()] == ",")
            .map(|t| t.range.clone())
            .chain(std::iter::once(params.end..params.end))
            .collect::<Vec<_>>();
        for comma in separators {
            let parameter_start = start;
            let p = trim(self.src, start..comma.start);
            start = comma.end;
            if p.is_empty() {
                if comma.start != params.end || parameter_start > params.start {
                    self.error(p, Error::InvalidParameter);
                }
                continue;
            }
            let eq = self.top(p.clone(), "=");
            let n = trim(self.src, p.start..eq.as_ref().map_or(p.end, |e| e.start));
            self.single_name(n.clone());
            if let Some(eq) = eq {
                default = true;
                self.expression(eq.end..p.end, Context::Output);
            } else if default {
                self.error(p, Error::InvalidParameter);
            }
        }
    }
    fn include_jinja(&mut self, r: Range<usize>) {
        let mut value = self.suffix_context(r);
        let t = tokens(self.src, value.clone());
        if t.len() >= 2 {
            let a = &t[t.len() - 2];
            let b = &t[t.len() - 1];
            if a.depth == 0
                && b.depth == 0
                && &self.src[a.range.clone()] == "ignore"
                && &self.src[b.range.clone()] == "missing"
            {
                value = trim(self.src, value.start..a.range.start);
            }
        }
        self.expression(value, Context::Output);
    }
    fn import_jinja(&mut self, r: Range<usize>, from: bool) {
        let r = self.suffix_context(r);
        let word = if from { "import" } else { "as" };
        let Some(clause) = self.top(r.clone(), word) else {
            self.error(r, Error::MissingClause);
            return;
        };
        self.expression(r.start..clause.start, Context::Output);
        if !from {
            self.single_name(clause.end..r.end);
            return;
        }
        let t = tokens(self.src, clause.end..r.end);
        let mut at = 0;
        while at < t.len() {
            let item = &t[at];
            if !name(&self.src[item.range.clone()]) || self.src[item.range.clone()].starts_with('_')
            {
                self.error(item.range.clone(), Error::InvalidTarget);
            }
            at += 1;
            let mut binding = item.range.clone();
            if t.get(at)
                .is_some_and(|t| &self.src[t.range.clone()] == "as")
            {
                at += 1;
                if let Some(alias) = t.get(at) {
                    binding = alias.range.clone();
                    self.single_name(binding.clone());
                    at += 1;
                } else {
                    self.error(r.end..r.end, Error::MissingClause);
                    break;
                }
            } else {
                self.result.bindings.push(binding.clone());
            }
            if at < t.len() {
                if &self.src[t[at].range.clone()] != "," {
                    self.error(t[at].range.clone(), Error::InvalidOption);
                    break;
                }
                at += 1;
                if at == t.len() {
                    self.error(r.end..r.end, Error::MissingClause);
                }
            }
        }
        if t.is_empty() {
            self.error(clause.end..r.end, Error::MissingClause);
        }
    }
    fn django_words(&self, r: Range<usize>) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let mut start = r.start;
        let mut quote = None;
        let mut escaped = false;
        for (i, c) in self.src[r.clone()].char_indices() {
            let i = r.start + i;
            if escaped {
                escaped = false;
                continue;
            }
            if quote.is_some() && c == '\\' {
                escaped = true;
                continue;
            }
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
            } else if matches!(c, '\'' | '"') {
                quote = Some(c);
            } else if c.is_whitespace() {
                if start < i {
                    out.push(start..i);
                }
                start = i + c.len_utf8();
            }
        }
        if start < r.end {
            out.push(start..r.end);
        }
        out
    }
    fn assignments_django(&mut self, r: Range<usize>) {
        let words = self.django_words(r.clone());
        if words.is_empty() {
            self.error(r, Error::MissingClause);
            return;
        }
        for word in words {
            if let Some(eq) = self.top(word.clone(), "=") {
                let n = trim(self.src, word.start..eq.start);
                self.single_name(n.clone());
                self.expression(eq.end..word.end, Context::Output);
            } else {
                self.error(word, Error::InvalidOption);
            }
        }
    }
    fn with_django(&mut self, r: Range<usize>) {
        let words = self.django_words(r.clone());
        if words.get(1).is_some_and(|w| &self.src[w.clone()] == "as") {
            let mut at = 0;
            loop {
                if at + 3 > words.len() || &self.src[words[at + 1].clone()] != "as" {
                    self.error(r.clone(), Error::InvalidOption);
                    break;
                }
                self.expression(words[at].clone(), Context::Output);
                self.single_name(words[at + 2].clone());
                at += 3;
                if at == words.len() {
                    break;
                }
                if &self.src[words[at].clone()] != "and" {
                    self.error(words[at].clone(), Error::InvalidOption);
                    break;
                }
                at += 1;
            }
        } else {
            self.assignments_django(r);
        }
    }
    fn include_django(&mut self, r: Range<usize>) {
        let words = self.django_words(r.clone());
        let Some(first) = words.first() else {
            self.error(r, Error::MissingClause);
            return;
        };
        self.expression(first.clone(), Context::Output);
        let mut tail = first.end..r.end;
        let t = self.django_words(tail.clone());
        if let Some(last) = t.last() {
            if &self.src[last.clone()] == "only" {
                tail.end = last.start;
            }
        }
        let tail = trim(self.src, tail);
        if tail.is_empty() {
            return;
        }
        let t = self.django_words(tail.clone());
        if &self.src[t[0].clone()] == "with" {
            self.assignments_django(t[0].end..tail.end);
        } else {
            self.error(tail, Error::InvalidOption);
        }
    }
    fn url_django(&mut self, r: Range<usize>) {
        let mut words = self.django_words(r.clone());
        if words.len() >= 2 && &self.src[words[words.len() - 2].clone()] == "as" {
            let alias = words.pop().unwrap();
            words.pop();
            self.single_name(alias);
        }
        let Some(first) = words.first() else {
            self.error(r, Error::MissingClause);
            return;
        };
        self.expression(first.clone(), Context::Output);
        for word in words.into_iter().skip(1) {
            if let Some(eq) = self.top(word.clone(), "=") {
                let n = word.start..eq.start;
                self.single_name(n.clone());
                self.expression(eq.end..word.end, Context::Output);
            } else {
                self.expression(word, Context::Output);
            }
        }
    }
    fn load_django(&mut self, r: Range<usize>) {
        let words = self.django_words(r.clone());
        if let Some(at) = words.iter().position(|w| &self.src[w.clone()] == "from") {
            if at == 0 || at + 2 != words.len() {
                self.error(r, Error::InvalidOption);
                return;
            }
        } else if words.is_empty() {
            self.error(r, Error::MissingClause);
            return;
        }
        for w in words {
            let text = &self.src[w.clone()];
            if text != "from" && !text.split('.').all(name) {
                self.error(w, Error::InvalidTarget);
            }
        }
    }
}
pub fn validate(source: &str, dialect: Dialect) -> Statement {
    let mut v = Validator {
        src: source,
        dialect,
        result: Statement {
            kind: None,
            bindings: Vec::new(),
            expressions: Vec::new(),
            diagnostics: Vec::new(),
        },
    };
    let whole = trim(source, 0..source.len());
    let tag = crate::template::statement_tag(&source[whole.clone()], dialect);
    let args = trim(source, whole.start + tag.len()..whole.end);
    let kind = match (tag, dialect) {
        ("for", _) => Some(StatementKind::For),
        ("set", Dialect::Jinja) => Some(StatementKind::Set),
        ("macro", Dialect::Jinja) => Some(StatementKind::Macro),
        ("include", _) => Some(StatementKind::Include),
        ("import", Dialect::Jinja) => Some(StatementKind::Import),
        ("from", Dialect::Jinja) => Some(StatementKind::From),
        ("with", Dialect::Django) => Some(StatementKind::With),
        ("load", Dialect::Django) => Some(StatementKind::Load),
        ("url", Dialect::Django) => Some(StatementKind::Url),
        ("block", _) => Some(StatementKind::Block),
        ("extends", _) => Some(StatementKind::Extends),
        _ => None,
    };
    v.result.kind = kind;
    match kind {
        Some(StatementKind::For) => v.for_statement(args),
        Some(StatementKind::Set) => v.set_statement(args),
        Some(StatementKind::Macro) => v.macro_statement(args),
        Some(StatementKind::Include) => {
            if dialect == Dialect::Jinja {
                v.include_jinja(args)
            } else {
                v.include_django(args)
            }
        }
        Some(StatementKind::Import) => v.import_jinja(args, false),
        Some(StatementKind::From) => v.import_jinja(args, true),
        Some(StatementKind::With) => v.with_django(args),
        Some(StatementKind::Load) => v.load_django(args),
        Some(StatementKind::Url) => v.url_django(args),
        Some(StatementKind::Block) => {
            if dialect == Dialect::Django {
                let words = v.django_words(args.clone());
                if words.len() == 1 {
                    v.result.bindings.push(words[0].clone());
                } else {
                    v.error(args, Error::InvalidOption);
                }
            } else {
                let t = tokens(source, args.clone());
                if let Some(n) = t.first() {
                    v.single_name(n.range.clone());
                    let mut at = 1;
                    if t.get(at)
                        .is_some_and(|t| &source[t.range.clone()] == "scoped")
                    {
                        at += 1;
                    }
                    if t.get(at)
                        .is_some_and(|t| &source[t.range.clone()] == "required")
                    {
                        at += 1;
                    }
                    for option in t.iter().skip(at) {
                        v.error(option.range.clone(), Error::InvalidOption);
                    }
                } else {
                    v.error(args, Error::MissingClause);
                }
            }
        }
        Some(StatementKind::Extends) => v.expression(args, Context::Output),
        None => {}
    }
    v.result
}
