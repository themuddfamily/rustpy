//! Finds the identifiers that a Python file *declares*, from its tokens.
//!
//! The spell checker only reports a name where it is declared —
//! a typo is fixed by renaming it there — so names of library APIs that the
//! file merely uses (`np.linspace`, `os.getcwd`) are never reported. Declared
//! are: `def`/`class` names and their (type) parameters, assignment and
//! annotation targets including `self.x`/`cls.x` attributes, `for` and
//! comprehension targets, `as` targets of `import`/`with`/`except`/`case`,
//! `lambda` parameters, walrus targets and `type` aliases.

use rustpy_syntax::{SyntaxKind, SyntaxKind::*, Token};

/// A significant token: an index into the token list, with every f-/t-string
/// collapsed into one `STRING` atom.
#[derive(Copy, Clone)]
struct Sig {
    kind: SyntaxKind,
    index: usize,
}

struct Ctx<'a> {
    tokens: &'a [Token],
    text: &'a str,
    declared: Vec<bool>,
}

impl Ctx<'_> {
    fn text_of(&self, sig: Sig) -> &str {
        let range = self.tokens[sig.index].range;
        &self.text[usize::from(range.start())..usize::from(range.end())]
    }

    fn is_ident(&self, sig: Sig, name: &str) -> bool {
        sig.kind == IDENT && self.text_of(sig) == name
    }

    fn declare(&mut self, sig: Sig) {
        if sig.kind == IDENT {
            self.declared[sig.index] = true;
        }
    }
}

/// For every token of `tokens` (the output of `rustpy_syntax::tokenize(text)`),
/// whether it is an `IDENT` that declares a name.
pub(crate) fn declared_identifiers(tokens: &[Token], text: &str) -> Vec<bool> {
    let mut ctx = Ctx {
        tokens,
        text,
        declared: vec![false; tokens.len()],
    };
    let sig = significant(tokens);
    let mut start = 0;
    while start < sig.len() {
        let mut end = start;
        let mut depth = 0usize;
        while end < sig.len() {
            match sig[end].kind {
                NEWLINE | INDENT | DEDENT | EOF => break,
                SEMICOLON if depth == 0 => break,
                L_PAREN | L_BRACK | L_BRACE => depth += 1,
                R_PAREN | R_BRACK | R_BRACE => depth = depth.saturating_sub(1),
                _ => {}
            }
            end += 1;
        }
        statement(&mut ctx, &sig[start..end]);
        start = end + 1;
    }
    ctx.declared
}

fn significant(tokens: &[Token]) -> Vec<Sig> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut fstring_depth = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        let kind = token.kind;
        if fstring_depth > 0 {
            match kind {
                FSTRING_START | TSTRING_START => fstring_depth += 1,
                FSTRING_END | TSTRING_END => fstring_depth -= 1,
                // An unterminated f-string ends with its line.
                NEWLINE => {
                    fstring_depth = 0;
                    out.push(Sig { kind, index });
                }
                _ => {}
            }
            continue;
        }
        match kind {
            WHITESPACE | COMMENT | NL => {}
            FSTRING_START | TSTRING_START => {
                fstring_depth = 1;
                out.push(Sig {
                    kind: STRING,
                    index,
                });
            }
            _ => out.push(Sig { kind, index }),
        }
    }
    out
}

/// Bracket depth of every token; an opening bracket and its closing bracket
/// get the depth outside them.
fn depths(toks: &[Sig]) -> Vec<usize> {
    let mut depth = 0usize;
    toks.iter()
        .map(|t| match t.kind {
            L_PAREN | L_BRACK | L_BRACE => {
                depth += 1;
                depth - 1
            }
            R_PAREN | R_BRACK | R_BRACE => {
                depth = depth.saturating_sub(1);
                depth
            }
            _ => depth,
        })
        .collect()
}

fn statement(ctx: &mut Ctx, toks: &[Sig]) {
    let all_depths = depths(toks);
    let (mut toks, mut depth) = (toks, &all_depths[..]);
    // A compound statement can have its body on the same line
    // (`if x: y = 1`). The header's `:` is at depth 0, so the depths computed
    // for the whole line hold for the body too. A loop, not recursion:
    // `else: else: …` must not exhaust the stack.
    let (first, head) = loop {
        let first = toks
            .iter()
            .position(|t| t.kind != ASYNC_KW)
            .unwrap_or(toks.len());
        let Some(&head) = toks.get(first) else { return };
        let compound = matches!(
            head.kind,
            DEF_KW
                | CLASS_KW
                | IF_KW
                | ELIF_KW
                | ELSE_KW
                | WHILE_KW
                | FOR_KW
                | TRY_KW
                | EXCEPT_KW
                | FINALLY_KW
                | WITH_KW
        ) || soft_compound(ctx, toks, depth, first);
        if !compound {
            break (first, head);
        }
        let colon = header_colon(toks, depth, first);
        let end = colon.unwrap_or(toks.len());
        expressions(ctx, &toks[..end], &depth[..end]);
        if matches!(head.kind, DEF_KW | CLASS_KW) {
            def_or_class(ctx, &toks[first..end], &depth[first..end]);
        }
        let Some(colon) = colon else { return };
        (toks, depth) = (&toks[colon + 1..], &depth[colon + 1..]);
    };
    expressions(ctx, toks, depth);
    match head.kind {
        IMPORT_KW | FROM_KW | GLOBAL_KW | NONLOCAL_KW | DEL_KW | RETURN_KW | RAISE_KW
        | ASSERT_KW | PASS_KW | BREAK_KW | CONTINUE_KW | YIELD_KW | AWAIT_KW | AT => {}
        _ => {
            if !type_alias(ctx, &toks[first..], &depth[first..]) {
                assignment(ctx, &toks[first..], &depth[first..]);
            }
        }
    }
}

/// `match subject:` / `case pattern:` (soft keywords).
fn soft_compound(ctx: &Ctx, toks: &[Sig], depth: &[usize], first: usize) -> bool {
    let head = toks[first];
    if !(ctx.is_ident(head, "match") || ctx.is_ident(head, "case")) {
        return false;
    }
    let starts_subject = toks.get(first + 1).is_some_and(|t| {
        matches!(
            t.kind,
            IDENT
                | L_PAREN
                | L_BRACK
                | L_BRACE
                | STRING
                | INT_NUMBER
                | FLOAT_NUMBER
                | COMPLEX_NUMBER
                | MINUS
                | STAR
                | TILDE
                | NONE_KW
                | TRUE_KW
                | FALSE_KW
                | NOT_KW
                | AWAIT_KW
                | LAMBDA_KW
        )
    });
    starts_subject && header_colon(toks, depth, first).is_some()
}

/// The `:` that ends a compound statement header (skipping lambda colons).
fn header_colon(toks: &[Sig], depth: &[usize], from: usize) -> Option<usize> {
    let mut lambdas = 0;
    for k in from..toks.len() {
        if depth[k] != 0 {
            continue;
        }
        match toks[k].kind {
            LAMBDA_KW => lambdas += 1,
            COLON if lambdas > 0 => lambdas -= 1,
            COLON => return Some(k),
            _ => {}
        }
    }
    None
}

/// Longest `for` target list, `lambda` parameter list or `as` target looked
/// at, in significant tokens. Real code stays far below it; together with
/// the stops at keywords that cannot occur in these constructs, it keeps the
/// work linear on pathological input (`for lambda as for …`).
const MAX_SCAN: usize = 256;

/// Declarations that can occur inside any expression: walrus targets,
/// `for` targets (statements and comprehensions), `lambda` parameters and
/// `as` targets.
fn expressions(ctx: &mut Ctx, toks: &[Sig], depth: &[usize]) {
    for k in 0..toks.len() {
        let d = depth[k];
        let limit = toks.len().min(k + 1 + MAX_SCAN);
        // The end of the construct that starts after `k`: the first token
        // that closes the bracket `k` is in, or that is at the same depth and
        // satisfies `stop`.
        let end = |stop: fn(SyntaxKind) -> bool| {
            (k + 1..limit)
                .find(|&j| depth[j] < d || (depth[j] == d && stop(toks[j].kind)))
                .unwrap_or(limit)
        };
        match toks[k].kind {
            COLON_EQ if k > 0 => ctx.declare(toks[k - 1]),
            FOR_KW => {
                let end = end(|kind| matches!(kind, IN_KW | FOR_KW | COLON | LAMBDA_KW | AS_KW));
                if end < limit && toks[end].kind == IN_KW && depth[end] == d {
                    targets(ctx, &toks[k + 1..end]);
                }
            }
            LAMBDA_KW => {
                for j in k + 1..end(|kind| matches!(kind, COLON | FOR_KW | AS_KW | LAMBDA_KW)) {
                    if toks[j].kind == IDENT
                        && depth[j] == d
                        && matches!(toks[j - 1].kind, LAMBDA_KW | COMMA | STAR | STAR_STAR)
                    {
                        ctx.declare(toks[j]);
                    }
                }
            }
            AS_KW => {
                let end = end(|kind| {
                    matches!(
                        kind,
                        COMMA | COLON | IF_KW | EQ | FOR_KW | IN_KW | LAMBDA_KW | AS_KW
                    )
                });
                targets(ctx, &toks[k + 1..end]);
            }
            _ => {}
        }
    }
}

/// `def name[T](params)` / `class Name[T](bases)`: the name, the type
/// parameters and (for functions) the parameters.
fn def_or_class(ctx: &mut Ctx, toks: &[Sig], depth: &[usize]) {
    let is_def = toks[0].kind == DEF_KW;
    let Some(&name) = toks.get(1).filter(|t| t.kind == IDENT) else {
        return;
    };
    ctx.declare(name);
    let mut k = 2;
    if toks.get(k).is_some_and(|t| t.kind == L_BRACK) {
        k = parameters(ctx, toks, depth, k);
    }
    if is_def && toks.get(k).is_some_and(|t| t.kind == L_PAREN) {
        parameters(ctx, toks, depth, k);
    }
}

/// Declares the names directly inside the bracket group opened at `open`
/// that start a parameter (after the bracket, `,`, `*` or `**`); returns the
/// index after the closing bracket.
fn parameters(ctx: &mut Ctx, toks: &[Sig], depth: &[usize], open: usize) -> usize {
    let inner = depth[open] + 1;
    let mut k = open + 1;
    while k < toks.len() && depth[k] >= inner {
        if toks[k].kind == IDENT
            && depth[k] == inner
            && (k - 1 == open || matches!(toks[k - 1].kind, COMMA | STAR | STAR_STAR))
        {
            ctx.declare(toks[k]);
        }
        k += 1;
    }
    k + 1
}

/// `type Alias[T] = ...`
fn type_alias(ctx: &mut Ctx, toks: &[Sig], depth: &[usize]) -> bool {
    let is_alias = toks.len() >= 3
        && ctx.is_ident(toks[0], "type")
        && toks[1].kind == IDENT
        && matches!(toks[2].kind, EQ | L_BRACK);
    if is_alias {
        ctx.declare(toks[1]);
        if toks[2].kind == L_BRACK {
            parameters(ctx, toks, depth, 2);
        }
    }
    is_alias
}

/// Assignment (`a = b = value`) and annotation (`a: int = value`) targets of
/// a simple statement.
fn assignment(ctx: &mut Ctx, toks: &[Sig], depth: &[usize]) {
    let mut equals = Vec::new();
    let mut annotation = None;
    let mut lambdas = 0;
    for k in 0..toks.len() {
        if depth[k] != 0 {
            continue;
        }
        match toks[k].kind {
            LAMBDA_KW => lambdas += 1,
            COLON if lambdas > 0 => lambdas -= 1,
            COLON if annotation.is_none() && equals.is_empty() => annotation = Some(k),
            EQ if lambdas == 0 => equals.push(k),
            _ => {}
        }
    }
    let ends = if equals.is_empty() {
        annotation.into_iter().collect()
    } else {
        equals
    };
    let mut start = 0;
    for end in ends {
        let target_end = annotation.filter(|&a| start == 0 && a < end).unwrap_or(end);
        targets(ctx, &toks[start..target_end]);
        start = end + 1;
    }
}

/// Declares the names bound by a target list: plain names, names inside
/// tuple/list unpacking and starred names, and `self.x`/`cls.x` attributes.
/// Names that are called, subscripted or have attributes taken are not
/// targets (`a[i] = 1`, `f(x).y = 1`, `obj.attr = 1`).
fn targets(ctx: &mut Ctx, toks: &[Sig]) {
    // One entry per open bracket: whether it unpacks (`(a, b) = ...`) rather
    // than calls or subscripts.
    let mut brackets: Vec<bool> = Vec::new();
    for k in 0..toks.len() {
        let prev = k.checked_sub(1).map(|p| toks[p].kind);
        match toks[k].kind {
            L_PAREN | L_BRACK => {
                let after_atom = matches!(
                    prev,
                    Some(
                        IDENT
                            | R_PAREN
                            | R_BRACK
                            | R_BRACE
                            | STRING
                            | INT_NUMBER
                            | FLOAT_NUMBER
                            | COMPLEX_NUMBER
                            | NONE_KW
                            | TRUE_KW
                            | FALSE_KW
                            | ELLIPSIS
                    )
                );
                brackets.push(!after_atom);
            }
            L_BRACE => brackets.push(false),
            R_PAREN | R_BRACK | R_BRACE => {
                brackets.pop();
            }
            IDENT if brackets.iter().all(|&unpacks| unpacks) => {
                if toks
                    .get(k + 1)
                    .is_some_and(|t| matches!(t.kind, DOT | L_PAREN | L_BRACK))
                {
                    continue;
                }
                let attribute_of_self = || {
                    k >= 2
                        && (ctx.is_ident(toks[k - 2], "self") || ctx.is_ident(toks[k - 2], "cls"))
                        && (k < 3 || toks[k - 3].kind != DOT)
                };
                if prev != Some(DOT) || attribute_of_self() {
                    ctx.declare(toks[k]);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared names of `src`, in order.
    fn declared(src: &str) -> Vec<String> {
        let (tokens, _) = rustpy_syntax::tokenize(src);
        let flags = declared_identifiers(&tokens, src);
        tokens
            .iter()
            .zip(flags)
            .filter(|(_, d)| *d)
            .map(|(t, _)| src[usize::from(t.range.start())..usize::from(t.range.end())].to_string())
            .collect()
    }

    #[test]
    fn functions_classes_and_parameters() {
        assert_eq!(
            declared("def f(a, b=c, *args, d: int = e(g=1), **kw) -> T:\n    pass\n"),
            ["f", "a", "b", "args", "d", "kw"]
        );
        assert_eq!(
            declared("async def g(self, /, x, *, y=lambda p, q: p): ...\n"),
            ["g", "self", "x", "y", "p", "q"]
        );
        assert_eq!(
            declared("def h[T, *Ts, **P](x: T) -> T: ...\n"),
            ["h", "T", "Ts", "P", "x"]
        );
        assert_eq!(
            declared("class C(Base, metaclass=Meta):\n    pass\n"),
            ["C"]
        );
        assert_eq!(declared("class D[T: int]: x: T\n"), ["D", "T", "x"]);
        assert_eq!(declared("@decorator(arg=1)\ndef f(): pass\n"), ["f"]);
    }

    #[test]
    fn assignments_and_annotations() {
        assert_eq!(declared("a = b = f(c, d=e)\n"), ["a", "b"]);
        assert_eq!(
            declared("x, (y, *z) = [p, q] = value\n"),
            ["x", "y", "z", "p", "q"]
        );
        assert_eq!(declared("count: int = 0\nname: str\n"), ["count", "name"]);
        assert_eq!(
            declared("self.total = 0\ncls.registry: dict = {}\n"),
            ["total", "registry"]
        );
        assert_eq!(
            declared("obj.attr = 1\nself.a.b = 2\nitems[key] = 3\nf(x).y = 4\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            declared("total += 1\nprint(a == b)\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            declared("handler = lambda event, *rest: event\n"),
            ["handler", "event", "rest"]
        );
        assert_eq!(declared("x = 1; y = 2\n"), ["x", "y"]);
        assert_eq!(
            declared("type Alias[T] = list[T]\ntype = 3\n"),
            ["Alias", "T", "type"]
        );
    }

    #[test]
    fn loops_with_except_import() {
        assert_eq!(
            declared("for i, (key, value) in items():\n    pass\n"),
            ["i", "key", "value"]
        );
        assert_eq!(
            declared("squares = [n * n for n in numbers if n]\n"),
            ["squares", "n"]
        );
        assert_eq!(
            declared("with open(p) as handle, lock:\n    pass\n"),
            ["handle"]
        );
        assert_eq!(
            declared("with (open(a) as first, open(b) as second): pass\n"),
            ["first", "second"]
        );
        assert_eq!(
            declared("try:\n    pass\nexcept ValueError as error:\n    pass\n"),
            ["error"]
        );
        assert_eq!(
            declared("import numpy as np, os.path\nfrom x import (y as z, w)\n"),
            ["np", "z"]
        );
        assert_eq!(
            declared("if (n := len(items)) > 10: limit = n\n"),
            ["n", "limit"]
        );
        assert_eq!(
            declared("match command:\n    case Point() as point:\n        pass\n"),
            ["point"]
        );
        assert_eq!(declared("match = re.match(p, s)\n"), ["match"]);
    }

    #[test]
    fn references_are_not_declarations() {
        assert_eq!(
            declared("print(recieve_data)\nobj.method(arg)\nreturn value\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            declared("foo(recieve=1)\ndel x\nglobal y\n"),
            Vec::<String>::new()
        );
        // Names inside f-strings are uses, even next to `=` (`{x=}`).
        assert_eq!(declared("s = f'{x=} {y:{w}}'\n"), ["s"]);
        // An unterminated f-string does not swallow the next line.
        assert_eq!(declared("s = f'abc\nt = 1\n"), ["s", "t"]);
    }
}
