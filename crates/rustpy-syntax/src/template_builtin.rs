//! Additional builtin grammar and static body checks; no template evaluation.
use crate::{
    template::{Block, Dialect, Kind as RegionKind, Region},
    template_expression::{self, Context, Kind},
};
use std::{collections::HashSet, ops::Range};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidExpression,
    MissingArgument,
    InvalidOption,
    InvalidBinding,
    InvalidCall,
    InvalidBody,
    DuplicateBlock,
    InvalidInheritance,
    DuplicateOption,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub error: Error,
}
#[derive(Debug)]
pub struct Validation {
    pub recognized: bool,
    pub diagnostics: Vec<Diagnostic>,
}
fn trim(s: &str, r: Range<usize>) -> Range<usize> {
    let text = &s[r.clone()];
    let left = text.len() - text.trim_start().len();
    r.start + left..r.start + text.trim_end().len().max(left)
}
/// Separator offsets outside quoted literals and nested delimiters.
fn split(s: &str, r: Range<usize>, separator: Option<char>) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = r.start;
    let mut quote = None;
    let mut escaped = false;
    let mut depth = 0usize;
    for (offset, c) in s[r.clone()].char_indices() {
        let offset = r.start + offset;
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
            continue;
        }
        if matches!(c, '\'' | '"') {
            quote = Some(c);
            continue;
        }
        if matches!(c, '(' | '[' | '{') {
            depth += 1;
        } else if matches!(c, ')' | ']' | '}') {
            depth = depth.saturating_sub(1);
        }
        if depth == 0 && separator.map_or_else(|| c.is_whitespace(), |s| s == c) {
            let part = trim(s, start..offset);
            if separator.is_some() || !part.is_empty() {
                out.push(part);
            }
            start = offset + c.len_utf8();
        }
    }
    let last = trim(s, start..r.end);
    if separator.is_some() || !last.is_empty() {
        out.push(last);
    }
    out
}
fn name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || unicode_ident::is_xid_start(c))
        && chars.all(|c| c == '_' || unicode_ident::is_xid_continue(c))
}
fn trans_variables(s: &str, mut r: Range<usize>) -> Vec<Range<usize>> {
    if let Some(quote @ ('\'' | '"')) = s[r.clone()].chars().next() {
        let mut escaped = false;
        for (offset, c) in s[r.clone()].char_indices().skip(1) {
            if escaped {
                escaped = false;
                continue;
            }
            if c == '\\' {
                escaped = true;
                continue;
            }
            if c == quote {
                r = trim(s, r.start + offset + c.len_utf8()..r.end);
                break;
            }
        }
    }
    let mut out = Vec::new();
    let mut trimming = false;
    let parts = split(s, r, Some(','));
    let count = parts.len();
    for (index, mut part) in parts.into_iter().enumerate() {
        let words = split(s, part.clone(), None);
        if let Some(first) = words.first() {
            let flag = matches!(&s[first.clone()], "trimmed" | "notrimmed");
            if !trimming && flag && !s[part.clone()].contains('=') {
                trimming = true;
                part = trim(s, first.end..part.end);
            } else if !trimming && flag && words.len() > 1 && out.is_empty() {
                // A flag followed by the first variable has no comma in Jinja.
                trimming = true;
                part = trim(s, first.end..part.end);
            }
        }
        if !part.is_empty() || (out.is_empty() && index + 1 < count) {
            out.push(part);
        }
    }
    out
}
struct Validator<'a> {
    s: &'a str,
    dialect: Dialect,
    out: Vec<Diagnostic>,
}
impl Validator<'_> {
    fn error(&mut self, r: Range<usize>, error: Error) {
        self.out.push(Diagnostic { range: r, error });
    }
    fn expr(&mut self, r: Range<usize>) -> Option<Kind> {
        let r = trim(self.s, r);
        let ast = template_expression::parse(&self.s[r.clone()], self.dialect, Context::Output);
        for e in &ast.diagnostics {
            self.error(
                r.start + e.range.start..r.start + e.range.end,
                Error::InvalidExpression,
            );
        }
        if ast.diagnostics.is_empty() {
            ast.root.map(|id| ast.nodes[id].kind.clone())
        } else {
            None
        }
    }
    fn binding(&mut self, r: Range<usize>) {
        if !name(&self.s[r.clone()]) {
            self.error(r, Error::InvalidBinding);
        }
    }
    fn delegate(&mut self, tag: &str, r: Range<usize>) {
        let source = format!("{tag} {}", &self.s[r.clone()]);
        let offset = tag.len() + 1;
        for e in crate::template_statement::validate(&source, self.dialect).diagnostics {
            self.error(
                r.start + e.range.start.saturating_sub(offset)
                    ..r.start + e.range.end.saturating_sub(offset),
                Error::InvalidOption,
            );
        }
    }
    fn assignment(&mut self, r: Range<usize>) {
        self.delegate("set", r);
    }
    fn signature(&mut self, r: Range<usize>) {
        let source = format!("macro __caller{}", &self.s[r.clone()]);
        let prefix = "macro __caller".len();
        for e in crate::template_statement::validate(&source, Dialect::Jinja).diagnostics {
            self.error(
                r.start + e.range.start.saturating_sub(prefix)
                    ..r.start + e.range.end.saturating_sub(prefix),
                Error::InvalidBinding,
            );
        }
    }
    fn call(&mut self, r: Range<usize>) {
        let mut value = r.clone();
        if self.s[r.clone()].starts_with('(') {
            let mut depth = 0usize;
            let mut quote = None;
            let mut escaped = false;
            let mut closing = None;
            for (offset, c) in self.s[r.clone()].char_indices() {
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
                    continue;
                }
                if matches!(c, '\'' | '"') {
                    quote = Some(c);
                } else if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        closing = Some(r.start + offset + 1);
                        break;
                    }
                }
            }
            let Some(end) = closing else {
                self.error(r, Error::InvalidBinding);
                return;
            };
            self.signature(r.start..end);
            value = trim(self.s, end..r.end);
        }
        if !matches!(self.expr(value.clone()), Some(Kind::Call { .. })) {
            self.error(value, Error::InvalidCall);
        }
    }
    fn filter(&mut self, r: Range<usize>) {
        let source = format!("subject|{}", &self.s[r.clone()]);
        let prefix = 8;
        let ast = template_expression::parse(&source, self.dialect, Context::Output);
        for e in &ast.diagnostics {
            self.error(
                r.start + e.range.start.saturating_sub(prefix)
                    ..(r.start + e.range.end.saturating_sub(prefix)).min(r.end),
                Error::InvalidExpression,
            );
        }
        if ast.diagnostics.is_empty() {
            let mut current = ast.root;
            let mut filters = 0;
            let mut valid = false;
            while let Some(id) = current {
                match &ast.nodes[id].kind {
                    Kind::Filter { value, .. } => {
                        filters += 1;
                        current = Some(*value);
                    }
                    Kind::Name if ast.nodes[id].range == (0..7) => {
                        valid = filters > 0;
                        break;
                    }
                    _ => break,
                }
            }
            if !valid {
                self.error(r, Error::InvalidOption);
            }
        }
    }

    fn with_jinja(&mut self, r: Range<usize>) {
        if r.is_empty() {
            return;
        }
        for part in split(self.s, r.clone(), Some(',')) {
            if part.is_empty() {
                self.error(part, Error::MissingArgument);
            } else {
                let p = split(self.s, part.clone(), Some('='));
                if p.len() < 2 {
                    self.error(part, Error::InvalidBinding);
                } else {
                    // Unlike set, with does not permit namespace attribute targets.
                    if self.s[p[0].clone()].contains('.') {
                        self.error(p[0].clone(), Error::InvalidBinding);
                    }
                    self.assignment(part);
                }
            }
        }
    }
    fn expression_words(&mut self, r: Range<usize>, minimum: usize) {
        let words = split(self.s, r.clone(), None);
        if words.len() < minimum {
            self.error(r, Error::MissingArgument);
        }
        for w in words {
            self.expr(w);
        }
    }
    fn expression_as(&mut self, r: Range<usize>, required: bool) {
        let mut words = split(self.s, r.clone(), None);
        if words.len() >= 2 && &self.s[words[words.len() - 2].clone()] == "as" {
            self.binding(words.pop().unwrap());
            words.pop();
        } else if required {
            self.error(r.clone(), Error::InvalidOption);
        }
        if words.len() != 1 {
            self.error(r, Error::MissingArgument);
        } else {
            self.expr(words[0].clone());
        }
    }
    fn cycle(&mut self, r: Range<usize>) {
        let mut words = split(self.s, r.clone(), None);
        if words.len() == 1 {
            self.binding(words[0].clone());
            return;
        }
        if words.last().is_some_and(|w| &self.s[w.clone()] == "silent") {
            words.pop();
            if words.len() < 2 || &self.s[words[words.len() - 2].clone()] != "as" {
                self.error(r.clone(), Error::InvalidOption);
            }
        }
        if words.len() >= 2 && &self.s[words[words.len() - 2].clone()] == "as" {
            self.binding(words.pop().unwrap());
            words.pop();
        }
        if words.len() < 2 {
            self.error(r, Error::MissingArgument);
        }
        for w in words {
            self.expr(w);
        }
    }
    fn regroup(&mut self, r: Range<usize>) {
        let words = split(self.s, r.clone(), None);
        if words.len() != 5
            || &self.s[words[1].clone()] != "by"
            || &self.s[words[3].clone()] != "as"
        {
            self.error(r, Error::InvalidOption);
            return;
        }
        self.expr(words[0].clone());
        self.expr(words[2].clone());
        self.binding(words[4].clone());
    }
    fn translate(&mut self, r: Range<usize>) {
        let words = split(self.s, r.clone(), None);
        let Some(first) = words.first() else {
            self.error(r, Error::MissingArgument);
            return;
        };
        self.expr(first.clone());
        let mut at = 1;
        let mut seen = HashSet::new();
        while at < words.len() {
            let option = &self.s[words[at].clone()];
            if !seen.insert(option) {
                self.error(words[at].clone(), Error::DuplicateOption);
            }
            match option {
                "noop" => at += 1,
                "context" | "as" => {
                    let Some(value) = words.get(at + 1) else {
                        self.error(words[at].clone(), Error::MissingArgument);
                        break;
                    };
                    if option == "context" {
                        self.expr(value.clone());
                    } else {
                        self.binding(value.clone());
                    }
                    at += 2;
                }
                _ => {
                    self.error(words[at].clone(), Error::InvalidOption);
                    break;
                }
            }
        }
    }
    fn django_blocktranslate(&mut self, r: Range<usize>) {
        let words = split(self.s, r.clone(), None);
        let mut at = 0;
        let mut seen = HashSet::new();
        while at < words.len() {
            let option = &self.s[words[at].clone()];
            if !seen.insert(option) {
                self.error(words[at].clone(), Error::DuplicateOption);
            }
            match option {
                "trimmed" => at += 1,
                "context" | "asvar" => {
                    if let Some(value) = words.get(at + 1) {
                        if option == "context" {
                            self.expr(value.clone());
                        } else {
                            self.binding(value.clone());
                        }
                        at += 2;
                    } else {
                        self.error(words[at].clone(), Error::MissingArgument);
                        break;
                    }
                }
                "with" | "count" => {
                    let start = at + 1;
                    at = start;
                    while at < words.len()
                        && !matches!(
                            &self.s[words[at].clone()],
                            "trimmed" | "context" | "asvar" | "with" | "count"
                        )
                    {
                        at += 1;
                    }
                    if at == start {
                        self.error(words[start - 1].clone(), Error::MissingArgument);
                    } else {
                        let value = words[start].start..words[at - 1].end;
                        if option == "count" {
                            let delegated = format!("with {}", &self.s[value.clone()]);
                            if crate::template_statement::validate(&delegated, Dialect::Django)
                                .bindings
                                .len()
                                != 1
                            {
                                self.error(value.clone(), Error::InvalidBinding);
                            }
                        }
                        self.delegate("with", value);
                    }
                }
                _ => {
                    self.error(words[at].clone(), Error::InvalidOption);
                    break;
                }
            }
        }
    }
    fn trans_jinja(&mut self, r: Range<usize>) {
        let mut seen = HashSet::new();
        for part in trans_variables(self.s, r) {
            let pieces = split(self.s, part.clone(), Some('='));
            let target = pieces[0].clone();
            self.binding(target.clone());
            if !seen.insert(&self.s[target.clone()]) {
                self.error(target, Error::DuplicateOption);
            }
            if pieces.len() > 1 {
                self.expr(pieces[1].start..part.end);
            }
        }
    }
}
pub fn validate(source: &str, dialect: Dialect) -> Validation {
    let all = trim(source, 0..source.len());
    let text = crate::template::statement_tag(&source[all.clone()], dialect);
    if text.is_empty() {
        return Validation {
            recognized: false,
            diagnostics: Vec::new(),
        };
    }
    let args = trim(source, all.start + text.len()..all.end);
    let mut v = Validator {
        s: source,
        dialect,
        out: Vec::new(),
    };
    let mut recognized = true;
    match (dialect, text) {
        (Dialect::Jinja, "call") => v.call(args),
        (Dialect::Jinja, "filter") | (Dialect::Django, "filter") => v.filter(args),
        (Dialect::Jinja, "with") => v.with_jinja(args),
        (Dialect::Jinja, "autoescape") => {
            v.expr(args);
        }
        (Dialect::Jinja, "trans") => v.trans_jinja(args),
        (Dialect::Jinja, "do" | "print") => {
            v.expr(args);
        }
        (Dialect::Django, "autoescape") => {
            if !matches!(&source[args.clone()], "on" | "off") {
                v.error(args, Error::InvalidOption);
            }
        }
        (Dialect::Django, "cycle") => v.cycle(args),
        (Dialect::Django, "regroup") => v.regroup(args),
        (Dialect::Django, "ifchanged") => v.expression_words(args, 0),
        (Dialect::Django, "translate" | "trans") => v.translate(args),
        (Dialect::Django, "blocktranslate" | "blocktrans") => v.django_blocktranslate(args),
        (Dialect::Django, "now" | "static") => v.expression_as(args, false),
        (Dialect::Django, "get_static_prefix" | "get_media_prefix") => {
            let words = split(source, args.clone(), None);
            if !words.is_empty() {
                if words.len() == 2 && &source[words[0].clone()] == "as" {
                    v.binding(words[1].clone());
                } else {
                    v.error(args, Error::InvalidOption);
                }
            }
        }
        (Dialect::Django, "firstof") => {
            let mut r = args.clone();
            let words = split(source, args, None);
            if words.len() >= 2 && &source[words[words.len() - 2].clone()] == "as" {
                v.binding(words.last().unwrap().clone());
                r.end = words[words.len() - 2].start;
            }
            v.expression_words(r, 1);
        }
        (Dialect::Django, "resetcycle") => {
            if !args.is_empty() {
                v.binding(args);
            }
        }
        (Dialect::Django, "templatetag") => {
            if !matches!(
                &source[args.clone()],
                "openblock"
                    | "closeblock"
                    | "openvariable"
                    | "closevariable"
                    | "openbrace"
                    | "closebrace"
                    | "opencomment"
                    | "closecomment"
            ) {
                v.error(args, Error::InvalidOption);
            }
        }
        (Dialect::Django, "widthratio") => {
            let words = split(source, args.clone(), None);
            if words.len() == 3 || words.len() == 5 && &source[words[3].clone()] == "as" {
                for w in words.iter().take(3) {
                    v.expr(w.clone());
                }
                if words.len() == 5 {
                    v.binding(words[4].clone());
                }
            } else {
                v.error(args, Error::MissingArgument);
            }
        }
        (Dialect::Jinja, "pluralize") => {
            if !args.is_empty() {
                let w = split(source, args.clone(), None);
                if w.len() == 1 {
                    v.binding(w[0].clone());
                } else {
                    v.error(args, Error::InvalidOption);
                }
            }
        }
        (Dialect::Django, "plural") => {
            if !args.is_empty() {
                v.error(args, Error::InvalidOption);
            }
        }
        (_, "else") | (Dialect::Django, "empty") => {
            if !args.is_empty() {
                v.error(args, Error::InvalidOption);
            }
        }
        (Dialect::Django, "spaceless" | "debug" | "csrf_token")
        | (Dialect::Jinja, "raw" | "break" | "continue") => {
            if !args.is_empty() {
                v.error(args, Error::InvalidOption);
            }
        }
        (_, "endblock") => {
            if !args.is_empty() {
                let w = split(source, args.clone(), None);
                if w.len() != 1 {
                    v.error(args, Error::InvalidOption);
                }
            }
        }
        (
            Dialect::Jinja,
            "endif" | "endfor" | "endset" | "endmacro" | "endcall" | "endfilter" | "endwith"
            | "endautoescape" | "endtrans" | "endraw",
        )
        | (
            Dialect::Django,
            "endif" | "endfor" | "endwith" | "endfilter" | "endautoescape" | "endspaceless"
            | "endcomment" | "endifchanged" | "endblocktranslate" | "endblocktrans",
        ) => {
            if !args.is_empty() {
                v.error(args, Error::InvalidOption);
            }
        }
        _ => recognized = false,
    }
    Validation {
        recognized,
        diagnostics: v.out,
    }
}
/// Static constraints on already parsed source regions. Does not infer execution.
pub fn validate_document(
    source: &str,
    regions: &[Region],
    blocks: &[Block],
    dialect: Dialect,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut names = HashSet::new();
    let mut nontext = false;
    let mut required = HashSet::new();
    let mut translation = HashSet::new();
    for (index, block) in blocks.iter().enumerate() {
        let region = &regions[block.opener];
        let words = split(source, region.content.clone(), None);
        if words.first().is_some_and(|r| &source[r.clone()] == "block") {
            if let Some(name) = words.get(1) {
                if !names.insert(&source[name.clone()]) {
                    out.push(Diagnostic {
                        range: name.clone(),
                        error: Error::DuplicateBlock,
                    });
                }
            }
            if dialect == Dialect::Jinja
                && words
                    .iter()
                    .skip(2)
                    .any(|r| &source[r.clone()] == "required")
            {
                required.insert(index);
            }
        }
        if words.first().is_some_and(|r| {
            matches!(
                &source[r.clone()],
                "trans" | "blocktranslate" | "blocktrans"
            )
        }) {
            translation.insert(index);
        }
    }
    let mut cycles = HashSet::new();
    let mut any_cycle = false;
    let mut invalid = HashSet::new();
    for (region_index, region) in regions.iter().enumerate() {
        if dialect == Dialect::Django && region.kind == RegionKind::Statement {
            let text = &source[region.content.clone()];
            let tag = text.split_whitespace().next().unwrap_or("");
            if matches!(tag, "cycle" | "resetcycle") {
                let words = split(source, region.content.clone(), None);
                let args = &words[1..];
                if tag == "cycle" {
                    if args.len() == 1 {
                        let target = &source[args[0].clone()];
                        if !cycles.contains(target) {
                            out.push(Diagnostic {
                                range: args[0].clone(),
                                error: Error::InvalidBinding,
                            });
                        }
                    } else {
                        any_cycle = true;
                        let length = args.len()
                            - usize::from(
                                args.last().is_some_and(|r| &source[r.clone()] == "silent"),
                            );
                        if length >= 2 && &source[args[length - 2].clone()] == "as" {
                            cycles.insert(&source[args[length - 1].clone()]);
                        }
                    }
                } else if let Some(target) = args.first() {
                    if !cycles.contains(&source[target.clone()]) {
                        out.push(Diagnostic {
                            range: target.clone(),
                            error: Error::InvalidBinding,
                        });
                    }
                } else if !any_cycle {
                    out.push(Diagnostic {
                        range: region.content.clone(),
                        error: Error::InvalidBinding,
                    });
                }
            }
            if tag == "extends" && nontext {
                out.push(Diagnostic {
                    range: region.content.clone(),
                    error: Error::InvalidInheritance,
                });
            }
            if matches!(tag, "comment" | "verbatim")
                || crate::template_statement::validate(text, dialect)
                    .kind
                    .is_some()
                || validate(text, dialect).recognized
            {
                nontext = true;
            }
        } else if region.kind == RegionKind::Expression {
            nontext = true;
        }
        if let Some(parent) = region.parent {
            if blocks[parent].end == Some(region_index) {
                continue;
            }
            if required.contains(&parent)
                && region.kind != RegionKind::Comment
                && !(region.kind == RegionKind::Text
                    && source[region.range.clone()].trim().is_empty())
                && invalid.insert(parent)
            {
                out.push(Diagnostic {
                    range: region.range.clone(),
                    error: Error::InvalidBody,
                });
            }
            if translation.contains(&parent) && region.kind == RegionKind::Statement {
                let text = &source[region.content.clone()];
                let mut branch = text.split_whitespace();
                let tag = branch.next().unwrap_or("");
                if matches!(tag, "plural" | "pluralize") {
                    let opener = &regions[blocks[parent].opener];
                    let words = split(source, opener.content.clone(), None);
                    if dialect == Dialect::Django {
                        if !words.iter().any(|r| &source[r.clone()] == "count") {
                            out.push(Diagnostic {
                                range: region.content.clone(),
                                error: Error::InvalidBody,
                            });
                        }
                    } else {
                        let mut declared = HashSet::new();
                        let header =
                            words.first().map_or(opener.content.end, |r| r.end)..opener.content.end;
                        for part in trans_variables(source, header) {
                            let before = split(source, part, Some('=')).into_iter().next().unwrap();
                            if name(&source[before.clone()]) {
                                declared.insert(&source[before]);
                            }
                        }
                        let selected = branch.next();
                        let implicit_body_counter = regions
                            [blocks[parent].opener + 1..region_index]
                            .iter()
                            .any(|r| r.parent == Some(parent) && r.kind == RegionKind::Expression);
                        if selected.map_or(declared.is_empty() && !implicit_body_counter, |n| {
                            !declared.contains(n)
                        }) {
                            out.push(Diagnostic {
                                range: region.content.clone(),
                                error: Error::InvalidBody,
                            });
                        }
                    }
                }
            }
            if translation.contains(&parent) {
                let valid = match region.kind {
                    RegionKind::Text | RegionKind::Comment => true,
                    RegionKind::Expression if dialect == Dialect::Django => true,
                    RegionKind::Expression => {
                        let ast = template_expression::parse(
                            &source[region.content.clone()],
                            dialect,
                            Context::Output,
                        );
                        ast.diagnostics.is_empty()
                            && ast
                                .root
                                .is_some_and(|id| matches!(ast.nodes[id].kind, Kind::Name))
                    }
                    RegionKind::Statement => source[region.content.clone()]
                        .split_whitespace()
                        .next()
                        .is_some_and(|t| {
                            matches!(
                                t,
                                "plural"
                                    | "pluralize"
                                    | "endtrans"
                                    | "endblocktranslate"
                                    | "endblocktrans"
                            )
                        }),
                    RegionKind::RawText => false,
                };
                if !valid {
                    out.push(Diagnostic {
                        range: region.range.clone(),
                        error: Error::InvalidBody,
                    });
                }
            }
        }
    }
    out
}
