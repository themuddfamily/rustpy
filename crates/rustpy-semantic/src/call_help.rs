//! Call-site help (roadmap §2.4 "parameter info", keyword-argument
//! completion): the call around the caret, its candidate signatures
//! (overloads, constructors, conditional definitions) and the one the
//! arguments typed so far select.

use std::sync::Arc;

use rustpy_core::TextSize;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;
use smol_str::SmolStr;

use crate::db::FileInfo;
use crate::features::{CompletionItem, CompletionKind};
use crate::infer::Inferer;
use crate::types::{ParamKind, Signature, Type};

/// The argument the caret is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurrentArg {
    /// The n-th positional argument (0-based).
    Positional(usize),
    /// A keyword argument `name=…`.
    Keyword(String),
    /// After `*args`/`**kwargs` or keyword arguments: no positional slot.
    Unknown,
}

/// A call enclosing the caret.
pub struct CallContext {
    pub call: ast::CallExpr,
    /// Name shown for the callee (`greet`, `join`).
    pub name: String,
    pub signatures: Vec<Arc<Signature>>,
    /// Positional argument types (the argument at the caret included).
    pub positional: Vec<Type>,
    pub keywords: Vec<(SmolStr, Type)>,
    pub current: CurrentArg,
    /// Positional arguments before the caret's argument.
    pub positional_before: usize,
    /// Index into `positional` of the caret's argument, when positional.
    current_index: Option<usize>,
}

/// Every signature a value of type `ty` can be called with.
pub fn signatures_of(inf: &Inferer<'_>, ty: &Type) -> Vec<Arc<Signature>> {
    signatures_rec(inf, ty, 0)
}

fn signatures_rec(inf: &Inferer<'_>, ty: &Type, depth: usize) -> Vec<Arc<Signature>> {
    if depth > 4 {
        return Vec::new();
    }
    match ty {
        Type::Function(s) | Type::BoundMethod(s, _) | Type::Callable(s) => vec![s.clone()],
        Type::Overloaded(v) => v.clone(),
        Type::Class(c, _) => match inf.constructor(c) {
            Some(Type::Overloaded(v)) => v,
            Some(t) => t.signature().cloned().into_iter().collect(),
            None => Vec::new(),
        },
        Type::Instance(..) => inf
            .lookup_member(ty, "__call__")
            .map(|m| signatures_rec(inf, &m, depth + 1))
            .unwrap_or_default(),
        // Conditionally defined functions: every definition.
        Type::Union(ts) => {
            let mut out: Vec<Arc<Signature>> = Vec::new();
            for t in ts {
                for s in signatures_rec(inf, t, depth + 1) {
                    if !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// The call whose parentheses enclose `offset`, with its arguments.
pub fn call_context(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Option<CallContext> {
    let root = file.parse.syntax_node();
    let mut tok = root
        .token_at_offset(offset)
        .left_biased()
        .or_else(|| root.token_at_offset(offset).right_biased())?;
    // Trivia (spaces, newlines) attaches to the enclosing statement; step back to code.
    while tok.kind().is_trivia()
        || matches!(
            tok.kind(),
            SyntaxKind::NEWLINE | SyntaxKind::INDENT | SyntaxKind::DEDENT
        )
    {
        tok = tok.prev_token()?;
    }
    let arg_list = tok.parent_ancestors().find(|n| {
        n.kind() == SyntaxKind::ARG_LIST
            && n.first_token()
                .is_some_and(|t| t.kind() == SyntaxKind::L_PAREN && t.text_range().end() <= offset)
            && n.last_token()
                .map(|t| t.kind() != SyntaxKind::R_PAREN || t.text_range().start() >= offset)
                .unwrap_or(true)
    })?;
    call_context_in(inf, file, &arg_list, offset)
}

/// [`call_context`] for a known argument list (`arg_list` must enclose `offset`).
pub fn call_context_in(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    arg_list: &rustpy_syntax::SyntaxNode,
    offset: TextSize,
) -> Option<CallContext> {
    let call = ast::CallExpr::cast(arg_list.parent()?)?;
    let callee = call.callee()?;
    let name = match &callee {
        ast::Expr::Name(n) => n.name_text(),
        ast::Expr::Attribute(a) => a.attr_text(),
        other => other.syntax().text().to_string(),
    };
    // Only identical rendered contracts are shown once. LiteralString and str
    // annotations remain distinct in the lookup.
    let mut labels: Vec<String> = Vec::new();
    let mut signatures = signatures_of(inf, &inf.type_of_expr(file, &callee));
    signatures.retain(|s| {
        let label = signature_label(&name, s).0;
        let new = !labels.contains(&label);
        labels.push(label);
        new
    });
    if signatures.is_empty() {
        return None;
    }
    let args: Vec<ast::AnyArg> = ast::ArgList::cast(arg_list.clone())?.args().collect();
    // The caret's slot: commas before it.
    let slot = arg_list
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == SyntaxKind::COMMA && t.text_range().end() <= offset)
        .count();
    let mut positional = Vec::new();
    let mut keywords = Vec::new();
    let mut current = None;
    let mut current_index = None;
    let mut positional_before = 0;
    let mut no_slot = false;
    for (i, a) in args.iter().enumerate() {
        let here = i == slot;
        match a {
            ast::AnyArg::Positional(_) | ast::AnyArg::Generator(_) => {
                if here {
                    current = Some(if no_slot {
                        CurrentArg::Unknown
                    } else {
                        CurrentArg::Positional(positional.len())
                    });
                    current_index = Some(positional.len());
                } else if i < slot {
                    positional_before += 1;
                }
                positional.push(
                    a.value()
                        .map(|e| inf.type_of_expr(file, &e))
                        .unwrap_or(Type::Unknown),
                );
            }
            ast::AnyArg::Keyword(k) => {
                if here {
                    current = Some(CurrentArg::Keyword(k.name_text()));
                }
                if i < slot {
                    no_slot = true;
                }
                keywords.push((
                    SmolStr::new(k.name_text()),
                    k.value()
                        .map(|e| inf.type_of_expr(file, &e))
                        .unwrap_or(Type::Unknown),
                ));
            }
            ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_) => {
                if here {
                    current = Some(CurrentArg::Unknown);
                }
                if i < slot {
                    no_slot = true;
                }
            }
        }
    }
    let current = match current {
        Some(c) => c,
        None if no_slot => CurrentArg::Unknown,
        None => {
            // An empty slot after the last argument.
            current_index = Some(positional.len());
            positional.push(Type::Unknown);
            CurrentArg::Positional(positional_before)
        }
    };
    Some(CallContext {
        call,
        name,
        signatures,
        positional,
        keywords,
        current,
        positional_before,
        current_index,
    })
}

impl CallContext {
    /// Index of the signature the arguments select: the first that accepts
    /// them (missing arguments are fine while typing).
    pub fn active_signature(&self, inf: &Inferer<'_>) -> usize {
        // Once a keyword argument is typed, no further positional ones follow.
        let positional_done = !self.keywords.is_empty();
        if let Some(i) = self
            .signatures
            .iter()
            .position(|s| could_fit(inf, s, &self.positional, &self.keywords, positional_done))
        {
            return i;
        }
        // The argument being typed may not be complete yet.
        if let Some(ci) = self.current_index {
            let mut pos = self.positional.clone();
            pos[ci] = Type::Unknown;
            if let Some(i) = self
                .signatures
                .iter()
                .position(|s| could_fit(inf, s, &pos, &self.keywords, positional_done))
            {
                return i;
            }
        }
        0
    }

    /// Index of the parameter of `sig` the caret's argument fills.
    pub fn active_param(&self, sig: &Signature) -> Option<usize> {
        match &self.current {
            CurrentArg::Keyword(k) => sig
                .params
                .iter()
                .position(|p| p.name == k.as_str() && p.kind != ParamKind::PositionalOnly)
                .or_else(|| sig.params.iter().position(|p| p.kind == ParamKind::KwArgs)),
            CurrentArg::Positional(n) => positional_slot(sig, *n),
            CurrentArg::Unknown => {
                // The next parameter that can still be passed by keyword.
                let filled = self.filled(sig);
                sig.params.iter().enumerate().position(|(i, p)| {
                    matches!(p.kind, ParamKind::Normal | ParamKind::KeywordOnly)
                        && !filled.contains(&i)
                })
            }
        }
    }

    /// The parameter of the selected signature that the caret's argument
    /// fills, with the type its value should have (for `**kwargs:
    /// Unpack[TD]`, the type of the TypedDict key).
    pub fn expected_parameter(&self, inf: &Inferer<'_>) -> Option<(SmolStr, Type)> {
        let sig = self.signatures.get(self.active_signature(inf))?;
        let p = sig.params.get(self.active_param(sig)?)?;
        if p.kind == ParamKind::KwArgs {
            let keys = typed_dict_keys(inf, &p.ty);
            if !keys.is_empty() {
                let CurrentArg::Keyword(k) = &self.current else {
                    return None;
                };
                return keys.into_iter().find(|(n, _)| n == k.as_str());
            }
        }
        Some((p.name.clone(), p.ty.clone()))
    }

    /// Parameters of `sig` already given (positionally before the caret, or by keyword).
    fn filled(&self, sig: &Signature) -> Vec<usize> {
        let mut out: Vec<usize> = (0..self.positional_before)
            .filter_map(|n| positional_slot(sig, n))
            .filter(|i| sig.params[*i].kind != ParamKind::VarArgs)
            .collect();
        for (k, _) in &self.keywords {
            if let Some(i) = sig.params.iter().position(|p| p.name == *k) {
                out.push(i);
            }
        }
        out
    }
}

/// Parameter receiving the n-th positional argument.
fn positional_slot(sig: &Signature, n: usize) -> Option<usize> {
    let mut seen = 0;
    for (i, p) in sig.params.iter().enumerate() {
        match p.kind {
            ParamKind::PositionalOnly | ParamKind::Normal => {
                if seen == n {
                    return Some(i);
                }
                seen += 1;
            }
            ParamKind::VarArgs => return Some(i),
            ParamKind::KeywordOnly | ParamKind::KwArgs => {}
        }
    }
    None
}

/// Whether `sig` accepts the arguments given so far (missing ones allowed,
/// except positional-only ones once `positional_done`).
fn could_fit(
    inf: &Inferer<'_>,
    sig: &Signature,
    pos: &[Type],
    kw: &[(SmolStr, Type)],
    positional_done: bool,
) -> bool {
    if positional_done
        && sig
            .params
            .iter()
            .filter(|p| p.kind == ParamKind::PositionalOnly)
            .skip(pos.len())
            .any(|p| !p.has_default)
    {
        return false;
    }
    let mut slots = sig
        .params
        .iter()
        .filter(|p| matches!(p.kind, ParamKind::PositionalOnly | ParamKind::Normal));
    let varargs = sig.params.iter().find(|p| p.kind == ParamKind::VarArgs);
    for a in pos {
        let ok = match slots.next().or(varargs) {
            Some(p) => inf.is_compatible(a, &p.ty),
            None => false,
        };
        if !ok {
            return false;
        }
    }
    let kwargs = sig.params.iter().any(|p| p.kind == ParamKind::KwArgs);
    kw.iter().all(|(name, t)| {
        match sig.params.iter().find(|p| {
            p.name == *name && matches!(p.kind, ParamKind::Normal | ParamKind::KeywordOnly)
        }) {
            Some(p) => inf.is_compatible(t, &p.ty),
            None => kwargs,
        }
    })
}

/// The overload a complete call resolves to (falls back like [`CallContext::active_signature`]).
pub fn select_for_call(
    inf: &Inferer<'_>,
    sigs: &[Arc<Signature>],
    pos: &[Type],
    kw: &[(SmolStr, Type)],
) -> Option<Arc<Signature>> {
    sigs.iter()
        .find(|s| inf.arguments_fit(s, pos, kw))
        .or_else(|| sigs.iter().find(|s| could_fit(inf, s, pos, kw, true)))
        .or_else(|| sigs.first())
        .cloned()
}

/// `name(params) -> ret` with the byte range of every parameter.
pub fn signature_label(name: &str, sig: &Signature) -> (String, Vec<(usize, usize)>) {
    let mut label = format!("{name}(");
    let mut params = Vec::new();
    let mut seen_kw_sep = false;
    for (i, p) in sig.params.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
        }
        if p.kind == ParamKind::KeywordOnly
            && !seen_kw_sep
            && !sig.params[..i].iter().any(|q| q.kind == ParamKind::VarArgs)
        {
            label.push_str("*, ");
            seen_kw_sep = true;
        }
        let start = label.len();
        label.push_str(&param_text(p));
        params.push((start, label.len()));
        if p.kind == ParamKind::PositionalOnly
            && sig
                .params
                .get(i + 1)
                .map(|q| q.kind != ParamKind::PositionalOnly)
                .unwrap_or(true)
        {
            label.push_str(", /");
        }
    }
    label.push(')');
    if !sig.ret.is_unknown() {
        label.push_str(&format!(" -> {}", display_type(&sig.ret)));
    }
    (label, params)
}

/// Type text for parameter info: a union of literals reads
/// `Literal["r", "w", "a", "x", …]` rather than `Literal["r"] | Literal["w"] | …`
/// (`open()`'s modes alone are dozens of literals).
pub fn display_type(ty: &Type) -> String {
    const SHOWN: usize = 4;
    let Type::Union(ts) = ty else {
        return ty.to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    let mut literals: Vec<String> = Vec::new();
    let mut slot = None;
    for t in ts {
        if let Type::Literal(_) = t {
            slot.get_or_insert_with(|| {
                parts.push(String::new());
                parts.len() - 1
            });
            let text = t.to_string();
            literals.push(text["Literal[".len()..text.len() - 1].to_string());
        } else {
            parts.push(t.to_string());
        }
    }
    if let Some(i) = slot {
        let mut values: Vec<&str> = literals.iter().take(SHOWN).map(String::as_str).collect();
        if literals.len() > SHOWN {
            values.push("…");
        }
        parts[i] = format!("Literal[{}]", values.join(", "));
    }
    parts.join(" | ")
}

/// `name: type = default` as shown in parameter info and hover.
pub fn param_text(p: &crate::types::Param) -> String {
    let mut text = match p.kind {
        ParamKind::VarArgs => format!("*{}", p.name),
        ParamKind::KwArgs => format!("**{}", p.name),
        _ => p.name.to_string(),
    };
    if !p.ty.is_unknown() {
        text.push_str(&format!(": {}", display_type(&p.ty)));
    }
    if let Some(d) = &p.default_text {
        text.push_str(if p.ty.is_unknown() { "=" } else { " = " });
        text.push_str(d);
    }
    text
}

/// `name=` items for the parameters a call can still take by keyword, when
/// the caret starts an argument of that call (after its `(` or one of its
/// own `,`s: not inside a list, tuple, dict, subscript or lambda nested in
/// the arguments, where a keyword argument would be invalid).
pub fn keyword_argument_items(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    offset: TextSize,
    prefix_start: usize,
) -> Vec<CompletionItem> {
    let text: &str = &file.text;
    let before = text[..prefix_start.min(text.len())].trim_end();
    if !before.ends_with(['(', ',']) {
        return Vec::new();
    }
    // Editing the name of an existing `name=value`.
    let after = &text[usize::from(offset).min(text.len())..];
    let rest = after
        .trim_start_matches(|c: char| rustpy_core::is_ident_continue(c))
        .trim_start();
    if rest.starts_with('=') && !rest.starts_with("==") {
        return Vec::new();
    }
    let Some(ctx) = call_context(inf, file, offset) else {
        return Vec::new();
    };
    let separator = file
        .parse
        .syntax_node()
        .token_at_offset(TextSize::new(before.len() as u32 - 1))
        .right_biased();
    let own_list = ctx.call.arg_list().map(|l| l.syntax().clone());
    if separator.and_then(|t| t.parent()) != own_list {
        return Vec::new();
    }
    let active = ctx.active_signature(inf);
    let used: Vec<SmolStr> = ctx.keywords.iter().map(|(k, _)| k.clone()).collect();
    let mut order: Vec<usize> = vec![active];
    order.extend((0..ctx.signatures.len()).filter(|i| *i != active));
    let mut seen: Vec<SmolStr> = Vec::new();
    let mut out = Vec::new();
    for si in order {
        let sig = &ctx.signatures[si];
        let filled: Vec<usize> = (0..ctx.positional_before)
            .filter_map(|n| positional_slot(sig, n))
            .collect();
        for (i, p) in sig.params.iter().enumerate() {
            let mut push = |name: &SmolStr, ty: &Type| {
                if used.contains(name) || seen.contains(name) {
                    return;
                }
                seen.push(name.clone());
                let detail = if ty.is_unknown() {
                    String::new()
                } else {
                    display_type(ty)
                };
                out.push(CompletionItem {
                    label: SmolStr::new(format!("{name}=")),
                    kind: CompletionKind::Parameter,
                    detail,
                    insert: format!("{name}="),
                    caret_offset: None,
                    priority: 0,
                });
            };
            match p.kind {
                ParamKind::Normal | ParamKind::KeywordOnly => {
                    if filled.contains(&i) || (i == 0 && matches!(p.name.as_str(), "self" | "cls"))
                    {
                        continue;
                    }
                    push(&p.name, &p.ty);
                }
                // `**kwargs: Unpack[TD]`: the TypedDict's keys.
                ParamKind::KwArgs => {
                    for (k, t) in typed_dict_keys(inf, &p.ty) {
                        push(&k, &t);
                    }
                    // Documented keyword-only options of an open **kwargs
                    // contract remain suggestions, not a fabricated signature.
                    if let Some(doc) = &sig.docstring {
                        for (name, _, _) in crate::docstrings::parse(doc).params {
                            if sig.params.iter().any(|parameter| parameter.name == name)
                                || !name.chars().next().is_some_and(rustpy_core::is_ident_start)
                                || !name.chars().all(rustpy_core::is_ident_continue)
                            {
                                continue;
                            }
                            push(&SmolStr::new(name), &Type::Unknown);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Keys of a TypedDict type with their value types.
fn typed_dict_keys(inf: &Inferer<'_>, ty: &Type) -> Vec<(SmolStr, Type)> {
    let ty = match ty {
        Type::Unpack(operand) => operand.as_ref(),
        other => other,
    };
    let Type::Instance(class, arguments) = ty else {
        return Vec::new();
    };
    inf.typed_dict_fields(class, arguments)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, value, _, _)| (name, value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::db::Analysis;
    use crate::features;

    fn setup(src: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        (db, f)
    }

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn keyword_argument_completion() {
        let src = "def greet(name: str, excited: bool = False, *, loud=1, **kw): ...\ngreet(na\n";
        let (db, f) = setup(src);
        let items = features::complete(&db, &f, TextSize::new(src.len() as u32 - 1));
        assert_eq!(items[0].label, "name=", "{:?}", labels(&items));
        assert_eq!(items[0].kind, CompletionKind::Parameter);
        assert_eq!(items[0].detail, "str");
        // Supplied parameters (by position or keyword) are not offered again; `**kw` is skipped.
        let src = "def greet(name: str, excited: bool = False, *, loud=1, **kw): ...\ngreet('a', loud=2, \n";
        let (db, f) = setup(src);
        let items = features::complete(&db, &f, TextSize::new(src.len() as u32 - 1));
        let l = labels(&items);
        assert!(l.contains(&"excited="), "{l:?}");
        assert!(
            !l.contains(&"name=")
                && !l.contains(&"loud=")
                && !l.contains(&"kw=")
                && !l.contains(&"self="),
            "{l:?}"
        );
        // Not when editing an existing keyword, nor outside argument starts.
        let src = "def greet(name: str): ...\ngreet(na=1)\n";
        let (db, f) = setup(src);
        let off = TextSize::new(src.find("na=").unwrap() as u32 + 2);
        assert!(!labels(&features::complete(&db, &f, off)).contains(&"name="));
        let src = "def greet(name: str): ...\nx = greet(1) + na\n";
        let (db, f) = setup(src);
        assert!(!labels(&features::complete(
            &db,
            &f,
            TextSize::new(src.len() as u32 - 1)
        ))
        .contains(&"name="));
        // Constructors and methods (no `self=`).
        let src = "class P:\n    def __init__(self, width: int, height: int): ...\n    def area(self, scale: float): ...\nP(\nP(1, 2).area(\n";
        let (db, f) = setup(src);
        let off = TextSize::new(src.find("P(\n").unwrap() as u32 + 2);
        let l: Vec<String> = features::complete(&db, &f, off)
            .iter()
            .filter(|i| i.kind == CompletionKind::Parameter)
            .map(|i| i.label.to_string())
            .collect();
        assert_eq!(l, vec!["height=", "width="]);
        let items = features::complete(&db, &f, TextSize::new(src.len() as u32 - 1));
        assert!(
            labels(&items).contains(&"scale=") && !labels(&items).contains(&"self="),
            "{:?}",
            labels(&items)
        );
        // `**kwargs: Unpack[TD]` offers the TypedDict keys.
        let src = "from typing import TypedDict, Unpack\nclass Opts(TypedDict):\n    color: str\n    size: int\ndef draw(**kw: Unpack[Opts]): ...\ndraw(\n";
        let (db, f) = setup(src);
        let l = labels(&features::complete(
            &db,
            &f,
            TextSize::new(src.len() as u32 - 1),
        ))
        .into_iter()
        .filter(|l| l.ends_with('='))
        .map(str::to_string)
        .collect::<Vec<_>>();
        assert_eq!(l, vec!["color=", "size="]);
    }

    #[test]
    fn keyword_arguments_only_where_the_call_takes_an_argument() {
        let keywords = |src: &str| -> Vec<String> {
            let (db, f) = setup(src);
            features::complete(&db, &f, TextSize::new(src.trim_end().len() as u32))
                .into_iter()
                .filter(crate::features::is_keyword_argument)
                .map(|i| i.label.to_string())
                .collect()
        };
        // Inside a list, tuple, subscript or dict nested in the arguments.
        for src in [
            "xs = [1]\na = 1\nsorted([a, k\n",
            "s = 1\nprint(1, (2, s\n",
            "x = [1]\ns = 1\nprint(x[1, s\n",
            "k = 1\nmax({1: 2, k\n",
            "s = 1\nprint(1, [s\n",
            "s = 1\nprint(1, {s\n",
        ] {
            assert_eq!(keywords(src), Vec::<String>::new(), "{src:?}");
        }
        // Lambda parameters in a keyword argument's value.
        assert_eq!(
            keywords("xs = [1]\nsorted(xs, key=lambda a, r\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            keywords("xs = [1]\nsorted(xs, key=lambda r\n"),
            Vec::<String>::new()
        );
        // The call's own argument starts still offer them, also after a nested display.
        assert_eq!(keywords("xs = [1]\nsorted(xs, k\n"), vec!["key="]);
        assert_eq!(keywords("sorted([1, 2], k\n"), vec!["key="]);
        assert!(keywords("print((1, 2), s\n").contains(&"sep=".to_string()));
        assert!(keywords("print(\n").contains(&"sep=".to_string()));
        // A nested call's own arguments.
        assert_eq!(keywords("xs = [1]\nprint(sorted(xs, k\n"), vec!["key="]);
    }

    #[test]
    fn parameter_info_lists_overloads_and_selects_by_arguments() {
        let src = "range(0, 10, 2)\n";
        let (db, f) = setup(src);
        let h = features::signature_help(&db, &f, TextSize::new(src.find("2)").unwrap() as u32))
            .unwrap();
        assert!(h.signatures.len() >= 2, "{:?}", h.signatures);
        assert_eq!(h.active_signature, 1);
        let s = &h.signatures[1];
        let active = s.active.map(|a| &s.label[s.params[a].0..s.params[a].1]);
        assert!(active.is_some_and(|a| a.starts_with("step")), "{s:?}");
        // The selected signature's fields mirror the active one.
        assert_eq!(h.label, s.label);
        // A single argument picks `range(stop)`.
        let src = "range(5)\n";
        let (db, f) = setup(src);
        let h = features::signature_help(&db, &f, TextSize::new(6)).unwrap();
        assert_eq!(h.active_signature, 0);
        // Keyword arguments map by name, not by comma count.
        let src = "def f(a, b, c=1, *, d=2): ...\nf(1, d=3, \n";
        let (db, f2) = setup(src);
        let h = features::signature_help(&db, &f2, TextSize::new(src.len() as u32 - 1)).unwrap();
        let s = &h.signatures[0];
        assert_eq!(
            s.active.map(|a| &s.label[s.params[a].0..s.params[a].1]),
            Some("b")
        );
        let off = TextSize::new(src.find("d=3").unwrap() as u32 + 2);
        let h = features::signature_help(&db, &f2, off).unwrap();
        let s = &h.signatures[0];
        assert_eq!(
            s.active.map(|a| &s.label[s.params[a].0..s.params[a].1]),
            Some("d: int = 2")
        );
        let _ = f;
    }

    #[test]
    fn stdlib_overloads_select_by_typed_arguments() {
        let sig_help = |src: &str| {
            let (db, f) = setup(src);
            features::signature_help(&db, &f, TextSize::new(src.len() as u32 - 1)).unwrap()
        };
        // A keyword argument ends the positional ones: `max(iterable, *, key=…)`,
        // not `max(arg1, arg2, *args, key=…)`.
        let h = sig_help("max([1, 2], key=\n");
        assert!(h.label.starts_with("max(iterable"), "{}", h.label);
        let h = sig_help("max(1, 2, \n");
        assert!(h.label.starts_with("max(arg1"), "{}", h.label);
        // `int(x, base)` once a second argument starts.
        let h = sig_help("int('12', \n");
        assert_eq!(
            h.active.map(|a| &h.label[h.params[a].0..h.params[a].1]),
            Some("base: SupportsIndex")
        );
        // LiteralString is now a real contract, distinct from ordinary str.
        let h = sig_help("', '.join(\n");
        assert_eq!(h.signatures.len(), 2, "{:?}", h.signatures);
        assert!(h
            .signatures
            .iter()
            .any(|signature| signature.label.contains("LiteralString")));
        assert!(h
            .signatures
            .iter()
            .any(|signature| !signature.label.contains("LiteralString")
                && signature.label.contains("str")));
        assert_ne!(h.signatures[0].label, h.signatures[1].label);
        assert_eq!(display_type(&Type::LiteralString), "LiteralString");
        let h = sig_help("from typing import LiteralString\ndef require(value:LiteralString)->LiteralString: return value\nrequire(\n");
        assert_eq!(h.label, "require(value: LiteralString) -> LiteralString");
    }

    #[test]
    fn literal_unions_read_compactly_in_labels() {
        use crate::types::LiteralValue;
        let lit = |s: &str| Type::Literal(LiteralValue::Str(s.into()));
        let modes = Type::Union(vec![
            lit("r"),
            lit("w"),
            lit("a"),
            lit("x"),
            lit("rb"),
            Type::None,
        ]);
        assert_eq!(
            display_type(&modes),
            "Literal[\"r\", \"w\", \"a\", \"x\", …] | None"
        );
        let ints = Type::Union(vec![
            Type::Literal(LiteralValue::Int(-1)),
            Type::Literal(LiteralValue::Int(1)),
        ]);
        assert_eq!(display_type(&ints), "Literal[-1, 1]");
        assert_eq!(display_type(&Type::None), "None");
    }

    #[test]
    fn keyword_items_rank_above_a_local_of_the_same_name() {
        let src = "def greet(name: str): ...\nname = 'x'\nnavy = 1\ngreet(na\n";
        let (db, f) = setup(src);
        let items = features::complete(&db, &f, TextSize::new(src.len() as u32 - 1));
        assert_eq!(&labels(&items)[..3], &["name=", "name", "navy"]);
        // The UI's filter keeps that order.
        let filtered = crate::completion2::filter_items(items, "na", &Default::default());
        assert_eq!(&labels(&filtered)[..2], &["name=", "name"]);
    }

    #[test]
    fn smart_completion_expects_the_selected_overloads_parameter() {
        let src = "from typing import overload\n@overload\ndef conv(x: int, base: int) -> int: ...\n@overload\ndef conv(text: str, enc: str) -> str: ...\ndef conv(*a): ...\nlabel = 'x'\ncount = 1\nconv('a', )\n";
        let (db, f) = setup(src);
        let off = TextSize::new(src.find("conv('a', )").unwrap() as u32 + 10);
        let (t, ctx) = crate::completion2::expected_type(&db, &f, off).unwrap();
        assert_eq!(t.to_string(), "str");
        assert_eq!(
            ctx,
            crate::completion2::ExpectedContext::Argument("conv".into(), "enc".into())
        );
        let items = crate::completion2::smart_complete(&db, &f, off);
        let l = labels(&items);
        let pos = |name: &str| {
            l.iter()
                .position(|x| *x == name)
                .unwrap_or_else(|| panic!("{name} missing: {l:?}"))
        };
        assert!(pos("label") < pos("count"), "{l:?}");
        // Keyword items are offered by smart completion too, after the type matches.
        assert!(pos("enc=") < pos("count"), "{l:?}");
        // Keyword values map by name, and `**kw: Unpack[TD]` by key.
        let src = "from typing import TypedDict, Unpack\nclass Opts(TypedDict):\n    color: str\n    size: int\ndef draw(n: int, **kw: Unpack[Opts]): ...\ndraw(1, size=)\ndraw(size=1, n=)\n";
        let (db, f) = setup(src);
        let (t, ctx) = crate::completion2::expected_type(
            &db,
            &f,
            TextSize::new(src.find("size=)").unwrap() as u32 + 5),
        )
        .unwrap();
        assert_eq!(
            (t.to_string(), ctx),
            (
                "int".to_string(),
                crate::completion2::ExpectedContext::Argument("draw".into(), "size".into())
            )
        );
        let (t, _) = crate::completion2::expected_type(
            &db,
            &f,
            TextSize::new(src.find("n=)").unwrap() as u32 + 2),
        )
        .unwrap();
        assert_eq!(t.to_string(), "int");
    }

    #[test]
    fn inlay_hints_and_hover_use_the_selected_overload() {
        let src = "from typing import overload\n@overload\ndef conv(x: int, base: int) -> int: ...\n@overload\ndef conv(text: str, enc: str) -> str: ...\ndef conv(*a): ...\nconv('a', 'utf8')\nconv(1, base=2)\n";
        let (db, f) = setup(src);
        let hints: Vec<String> = features::inlay_hints(&db, &f, true, false)
            .into_iter()
            .map(|h| h.label)
            .collect();
        assert!(
            hints.contains(&"text:".to_string()) && hints.contains(&"enc:".to_string()),
            "{hints:?}"
        );
        let off = TextSize::new(src.find("base=2").unwrap() as u32 + 1);
        let h = features::hover_at(&db, &f, off).unwrap();
        assert_eq!(h.signature, "base: int");
        assert_eq!(h.kind, Some("parameter"));
    }
}
