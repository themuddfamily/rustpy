//! Expression grammar with full Python precedence, comprehensions, calls,
//! subscripts/slices, lambdas, f-strings and assignment targets.

use super::{stmt, CompletedMarker, Parser, TokenSet, BLOCK_COST, BRACKET_COST, EXPR_COST};
use crate::syntax_kind::SyntaxKind::{self, *};

const EXPR_FIRST: TokenSet = TokenSet::new(&[
    IDENT,
    INT_NUMBER,
    FLOAT_NUMBER,
    COMPLEX_NUMBER,
    STRING,
    FSTRING_START,
    TSTRING_START,
    L_PAREN,
    L_BRACK,
    L_BRACE,
    MINUS,
    PLUS,
    TILDE,
    NOT_KW,
    LAMBDA_KW,
    AWAIT_KW,
    NONE_KW,
    TRUE_KW,
    FALSE_KW,
    ELLIPSIS,
    STAR,
    STAR_STAR,
    YIELD_KW,
]);

const COMPARISON: TokenSet =
    TokenSet::new(&[LT, GT, LT_EQ, GT_EQ, EQ_EQ, NOT_EQ, IN_KW, NOT_KW, IS_KW]);

pub(super) fn at_expr_start(p: &Parser<'_>) -> bool {
    p.at_ts(EXPR_FIRST)
}

/// Parses an expression, reporting an error if none is present.
pub(super) fn expect_expression(p: &mut Parser<'_>) -> bool {
    if expression(p) {
        true
    } else {
        p.push_error("expected expression");
        false
    }
}

pub(super) fn expect_named_expression(p: &mut Parser<'_>) -> bool {
    if named_expression(p) {
        true
    } else {
        p.push_error("expected expression");
        false
    }
}

pub(super) fn expect_star_expressions(p: &mut Parser<'_>) -> bool {
    if star_expressions(p) {
        true
    } else {
        p.push_error("expected expression");
        false
    }
}

/// star_expressions: a tuple without parentheses or a single expression.
pub(super) fn star_expressions(p: &mut Parser<'_>) -> bool {
    let m = p.start();
    let Some(_) = star_expression(p) else {
        m.abandon(p);
        return false;
    };
    if p.at(COMMA) {
        while p.eat(COMMA) {
            if !at_expr_start(p) {
                break;
            }
            star_expression(p);
        }
        m.complete(p, TUPLE_EXPR);
    } else {
        m.abandon(p);
    }
    true
}

/// Like [`star_expressions`] but also accepts `yield` (assignment RHS / statement).
pub(super) fn star_expressions_or_yield(p: &mut Parser<'_>) -> bool {
    if p.at(YIELD_KW) {
        yield_expr(p);
        return true;
    }
    star_expressions(p)
}

/// star_named_expressions: used by `match` subjects and collection displays.
pub(super) fn star_named_expressions(p: &mut Parser<'_>) -> bool {
    let m = p.start();
    if star_named_expression(p).is_none() {
        m.abandon(p);
        return false;
    }
    if p.at(COMMA) {
        while p.eat(COMMA) {
            if !at_expr_start(p) {
                break;
            }
            star_named_expression(p);
        }
        m.complete(p, TUPLE_EXPR);
    } else {
        m.abandon(p);
    }
    true
}

pub(super) fn expressions(p: &mut Parser<'_>) -> bool {
    star_expressions(p)
}

fn star_expression(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if p.at(STAR) {
        let m = p.start();
        p.bump_any();
        bitwise_or(p);
        return Some(m.complete(p, STAR_EXPR));
    }
    expression_cm(p)
}

fn star_named_expression(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if p.at(STAR) {
        let m = p.start();
        p.bump_any();
        bitwise_or(p);
        return Some(m.complete(p, STAR_EXPR));
    }
    named_expression_cm(p)
}

/// `*expr` or a plain expression (annotations of `*args`, type-param defaults).
pub(super) fn star_expression_or_expression(p: &mut Parser<'_>) -> bool {
    star_expression(p).is_some()
}

/// named_expression: NAME ':=' expression | expression
pub(super) fn named_expression(p: &mut Parser<'_>) -> bool {
    named_expression_cm(p).is_some()
}

fn named_expression_cm(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if p.at(IDENT) && p.nth_at(1, COLON_EQ) {
        let m = p.start();
        let n = p.start();
        p.bump_any();
        n.complete(p, NAME_EXPR);
        p.bump(COLON_EQ);
        if expression_cm(p).is_none() {
            p.push_error("expected expression after ':='");
        }
        return Some(m.complete(p, NAMED_EXPR));
    }
    expression_cm(p)
}

/// expression: disjunction ['if' disjunction 'else' expression] | lambda
pub(super) fn expression(p: &mut Parser<'_>) -> bool {
    expression_cm(p).is_some()
}

/// Runs a recursive rule under the nesting guard of [`Parser::enter`], so
/// that absurdly nested input cannot overflow the stack.
fn nested(
    p: &mut Parser<'_>,
    cost: u32,
    rule: fn(&mut Parser<'_>) -> Option<CompletedMarker>,
) -> Option<CompletedMarker> {
    if !p.enter_expr(cost) {
        return None;
    }
    let result = rule(p);
    p.leave(cost);
    result
}

fn expression_cm(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    // Every nested bracket, lambda body and conditional branch comes through here.
    nested(p, EXPR_COST, expression_inner)
}

fn expression_inner(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if p.at(LAMBDA_KW) {
        return Some(lambda(p));
    }
    let lhs = disjunction(p)?;
    if p.at(IF_KW) {
        let m = lhs.precede(p);
        p.bump_any();
        if disjunction(p).is_none() {
            p.push_error("expected condition");
        }
        if p.expect(ELSE_KW) && expression_cm(p).is_none() {
            p.push_error("expected expression after 'else'");
        }
        return Some(m.complete(p, CONDITIONAL_EXPR));
    }
    Some(lhs)
}

fn lambda(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(LAMBDA_KW);
    stmt::lambda_parameters(p);
    if p.expect(COLON) && expression_cm(p).is_none() {
        p.push_error("expected lambda body");
    }
    m.complete(p, LAMBDA_EXPR)
}

fn disjunction(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut lhs = conjunction(p)?;
    if p.at(OR_KW) {
        let m = lhs.precede(p);
        while p.eat(OR_KW) {
            if conjunction(p).is_none() {
                p.push_error("expected expression after 'or'");
                break;
            }
        }
        lhs = m.complete(p, BOOL_OP_EXPR);
    }
    Some(lhs)
}

fn conjunction(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut lhs = inversion(p)?;
    if p.at(AND_KW) {
        let m = lhs.precede(p);
        while p.eat(AND_KW) {
            if inversion(p).is_none() {
                p.push_error("expected expression after 'and'");
                break;
            }
        }
        lhs = m.complete(p, BOOL_OP_EXPR);
    }
    Some(lhs)
}

fn inversion(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if p.at(NOT_KW) {
        let m = p.start();
        p.bump_any();
        if nested(p, EXPR_COST, inversion).is_none() {
            p.push_error("expected expression after 'not'");
        }
        return Some(m.complete(p, NOT_EXPR));
    }
    comparison(p)
}

fn comparison(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let lhs = bitwise_or(p)?;
    if p.at_ts(COMPARISON) && !(p.at(NOT_KW) && !p.nth_at(1, IN_KW)) {
        let m = lhs.precede(p);
        while p.at_ts(COMPARISON) {
            p.step();
            let op = p.start();
            match p.current() {
                NOT_KW => {
                    if !p.nth_at(1, IN_KW) {
                        op.abandon(p);
                        break;
                    }
                    p.bump_any();
                    p.bump_any();
                }
                IS_KW => {
                    p.bump_any();
                    p.eat(NOT_KW);
                }
                NOT_EQ => {
                    p.check_not_eq_spelling();
                    p.bump_any();
                }
                _ => p.bump_any(),
            }
            op.complete(p, COMPARE_OP);
            if bitwise_or(p).is_none() {
                p.push_error("expected expression after comparison operator");
                break;
            }
        }
        return Some(m.complete(p, COMPARE_EXPR));
    }
    Some(lhs)
}

fn binary_left_assoc(
    p: &mut Parser<'_>,
    ops: TokenSet,
    next: fn(&mut Parser<'_>) -> Option<CompletedMarker>,
) -> Option<CompletedMarker> {
    let mut lhs = next(p)?;
    while p.at_ts(ops) {
        p.step();
        let m = lhs.precede(p);
        p.bump_any();
        if next(p).is_none() {
            p.push_error("expected expression after operator");
        }
        lhs = m.complete(p, BINARY_EXPR);
    }
    Some(lhs)
}

pub(super) fn bitwise_or(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    binary_left_assoc(p, TokenSet::new(&[PIPE]), bitwise_xor)
}

fn bitwise_xor(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    binary_left_assoc(p, TokenSet::new(&[CARET]), bitwise_and)
}

fn bitwise_and(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    binary_left_assoc(p, TokenSet::new(&[AMP]), shift_expr)
}

fn shift_expr(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    binary_left_assoc(p, TokenSet::new(&[SHL, SHR]), sum)
}

fn sum(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    binary_left_assoc(p, TokenSet::new(&[PLUS, MINUS]), term)
}

fn term(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    binary_left_assoc(
        p,
        TokenSet::new(&[STAR, SLASH, SLASH_SLASH, PERCENT, AT]),
        factor,
    )
}

fn factor(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if matches!(p.current(), PLUS | MINUS | TILDE) {
        let m = p.start();
        p.bump_any();
        if nested(p, EXPR_COST, factor).is_none() {
            p.push_error("expected expression after unary operator");
        }
        return Some(m.complete(p, UNARY_EXPR));
    }
    power(p)
}

fn power(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let lhs = await_primary(p)?;
    if p.at(STAR_STAR) {
        let m = lhs.precede(p);
        p.bump_any();
        // `**` is right-associative: `a ** b ** c` recurses.
        if nested(p, EXPR_COST, factor).is_none() {
            p.push_error("expected expression after '**'");
        }
        return Some(m.complete(p, BINARY_EXPR));
    }
    Some(lhs)
}

fn await_primary(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if p.at(AWAIT_KW) {
        let m = p.start();
        p.bump_any();
        if primary(p).is_none() {
            p.push_error("expected expression after 'await'");
        }
        return Some(m.complete(p, AWAIT_EXPR));
    }
    primary(p)
}

fn primary(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut lhs = atom(p)?;
    loop {
        p.step();
        lhs = match p.current() {
            DOT => {
                let m = lhs.precede(p);
                p.bump_any();
                if !p.expect(IDENT) {
                    // `obj.` with missing name: keep node for completion.
                }
                m.complete(p, ATTRIBUTE_EXPR)
            }
            L_PAREN | L_BRACK if p.enter_expr(BRACKET_COST) => {
                let m = lhs.precede(p);
                let kind = if p.at(L_PAREN) {
                    arguments(p);
                    CALL_EXPR
                } else {
                    subscript(p);
                    SUBSCRIPT_EXPR
                };
                p.leave(BRACKET_COST);
                m.complete(p, kind)
            }
            _ => break,
        };
    }
    Some(lhs)
}

/// `(` args `)` — also used for class bases.
pub(super) fn arguments(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(L_PAREN);
    while !p.at(R_PAREN) && !p.at(EOF) {
        p.step();
        if p.at(NEWLINE) || p.at(DEDENT) || p.at(INDENT) {
            break;
        }
        let a = p.start();
        match p.current() {
            STAR => {
                p.bump_any();
                if expression_cm(p).is_none() {
                    p.push_error("expected expression after '*'");
                }
                a.complete(p, STAR_ARG);
            }
            STAR_STAR => {
                p.bump_any();
                if expression_cm(p).is_none() {
                    p.push_error("expected expression after '**'");
                }
                a.complete(p, STAR_STAR_ARG);
            }
            IDENT if p.nth_at(1, EQ) => {
                p.bump_any();
                p.bump_any();
                if expression_cm(p).is_none() {
                    p.push_error("expected expression after '='");
                }
                a.complete(p, KEYWORD_ARG);
            }
            _ => {
                if named_expression_cm(p).is_none() {
                    a.abandon(p);
                    p.error_recover(
                        "expected argument",
                        TokenSet::new(&[COMMA, R_PAREN, NEWLINE]),
                    );
                    if !p.eat(COMMA) {
                        break;
                    }
                    continue;
                }
                if p.at(FOR_KW) || (p.at(ASYNC_KW) && p.nth_at(1, FOR_KW)) {
                    comp_clauses(p);
                    a.complete(p, GENERATOR_EXPR);
                } else {
                    a.complete(p, ARG);
                }
            }
        }
        if !p.eat(COMMA) {
            break;
        }
    }
    if !p.eat(R_PAREN) {
        p.push_error("expected ')'");
    }
    m.complete(p, ARG_LIST);
}

fn subscript(p: &mut Parser<'_>) {
    p.bump(L_BRACK);
    let list = p.start();
    let mut count = 0;
    let mut trailing_comma = false;
    while !p.at(R_BRACK) && !p.at(EOF) && !p.at(NEWLINE) {
        p.step();
        if !slice(p) {
            p.error_recover(
                "expected index or slice",
                TokenSet::new(&[COMMA, R_BRACK, NEWLINE]),
            );
        }
        count += 1;
        trailing_comma = false;
        if !p.eat(COMMA) {
            break;
        }
        trailing_comma = true;
    }
    if count > 1 || trailing_comma {
        list.complete(p, SLICE_LIST);
    } else {
        list.abandon(p);
    }
    if !p.eat(R_BRACK) {
        p.push_error("expected ']'");
    }
}

fn slice(p: &mut Parser<'_>) -> bool {
    let m = p.start();
    let has_lower = if p.at(COLON) {
        false
    } else {
        star_named_expression(p).is_some()
    };
    if !has_lower && !p.at(COLON) {
        m.abandon(p);
        return false;
    }
    if p.at(COLON) {
        p.bump_any();
        if !matches!(p.current(), COLON | COMMA | R_BRACK) {
            expression(p);
        }
        if p.eat(COLON) && !matches!(p.current(), COMMA | R_BRACK) {
            expression(p);
        }
        m.complete(p, SLICE_EXPR);
    } else {
        m.abandon(p);
    }
    true
}

fn atom(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let kind = p.current();
    Some(match kind {
        IDENT => {
            let m = p.start();
            p.bump_any();
            m.complete(p, NAME_EXPR)
        }
        INT_NUMBER | FLOAT_NUMBER | COMPLEX_NUMBER | NONE_KW | TRUE_KW | FALSE_KW | ELLIPSIS => {
            let m = p.start();
            p.bump_any();
            m.complete(p, LITERAL_EXPR)
        }
        STRING | FSTRING_START | TSTRING_START | L_PAREN | L_BRACK | L_BRACE | YIELD_KW => {
            if !p.enter_expr(BRACKET_COST) {
                return None;
            }
            let m = match kind {
                L_PAREN => paren_or_tuple_or_genexp(p),
                L_BRACK => list_or_comp(p),
                L_BRACE => dict_or_set(p),
                YIELD_KW => yield_expr(p),
                _ => strings(p),
            };
            p.leave(BRACKET_COST);
            m
        }
        _ => return None,
    })
}

/// One or more adjacent string literals (implicit concatenation).
pub(super) fn strings(p: &mut Parser<'_>) -> CompletedMarker {
    let outer = p.start();
    let mut count = 0;
    let mut last = None;
    while matches!(p.current(), STRING | FSTRING_START | TSTRING_START) {
        p.step();
        count += 1;
        last = Some(match p.current() {
            STRING => {
                let m = p.start();
                p.bump_any();
                m.complete(p, STRING_EXPR)
            }
            _ => fstring(p),
        });
    }
    if count > 1 {
        outer.complete(p, CONCAT_STRING_EXPR)
    } else {
        outer.abandon(p);
        last.expect("at least one string")
    }
}

fn fstring(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    let end_kind = if p.at(TSTRING_START) {
        TSTRING_END
    } else {
        FSTRING_END
    };
    p.bump_any();
    loop {
        p.step();
        match p.current() {
            FSTRING_MIDDLE | TSTRING_MIDDLE => p.bump_any(),
            L_BRACE => fstring_replacement(p),
            k if k == end_kind => {
                p.bump_any();
                break;
            }
            EOF | NEWLINE => {
                p.push_error("unterminated f-string");
                break;
            }
            _ => {
                p.error_and_bump("unexpected token in f-string");
            }
        }
    }
    m.complete(p, FSTRING_EXPR)
}

fn fstring_replacement(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(L_BRACE);
    if p.at(R_BRACE) {
        p.push_error("f-string: valid expression required before '}'");
    } else if p.at(YIELD_KW) {
        yield_expr(p);
    } else if !star_named_expressions(p) {
        p.error_recover(
            "f-string: expected expression",
            TokenSet::new(&[R_BRACE, BANG, COLON, FSTRING_END, TSTRING_END]),
        );
    }
    // Debug specifier `=`
    p.eat(EQ);
    if p.at(BANG) {
        let c = p.start();
        p.bump_any();
        if p.at(IDENT) && matches!(p.nth_text(0), "r" | "s" | "a") {
            p.bump_any();
        } else {
            p.push_error("f-string: invalid conversion character (expected 'r', 's' or 'a')");
        }
        c.complete(p, FSTRING_CONVERSION);
    }
    if p.at(COLON) {
        let s = p.start();
        p.bump_any();
        loop {
            p.step();
            match p.current() {
                FSTRING_MIDDLE | TSTRING_MIDDLE => p.bump_any(),
                L_BRACE if p.enter_expr(BLOCK_COST) => {
                    fstring_replacement(p);
                    p.leave(BLOCK_COST);
                }
                _ => break,
            }
        }
        s.complete(p, FSTRING_FORMAT_SPEC);
    }
    if !p.eat(R_BRACE) {
        p.push_error("f-string: expected '}'");
    }
    m.complete(p, FSTRING_REPLACEMENT);
}

fn paren_or_tuple_or_genexp(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(L_PAREN);
    if p.eat(R_PAREN) {
        return m.complete(p, TUPLE_EXPR);
    }
    if p.at(YIELD_KW) {
        yield_expr(p);
        p.expect(R_PAREN);
        return m.complete(p, PAREN_EXPR);
    }
    if star_named_expression(p).is_none() {
        p.error_recover("expected expression", TokenSet::new(&[R_PAREN, NEWLINE]));
        p.eat(R_PAREN);
        return m.complete(p, PAREN_EXPR);
    }
    if p.at(FOR_KW) || (p.at(ASYNC_KW) && p.nth_at(1, FOR_KW)) {
        comp_clauses(p);
        if !p.eat(R_PAREN) {
            p.push_error("expected ')'");
        }
        return m.complete(p, GENERATOR_EXPR);
    }
    if p.at(COMMA) {
        while p.eat(COMMA) {
            if p.at(R_PAREN) {
                break;
            }
            if star_named_expression(p).is_none() {
                p.error_recover(
                    "expected expression",
                    TokenSet::new(&[COMMA, R_PAREN, NEWLINE]),
                );
            }
        }
        if !p.eat(R_PAREN) {
            p.push_error("expected ')'");
        }
        return m.complete(p, TUPLE_EXPR);
    }
    if !p.eat(R_PAREN) {
        p.error_recover("expected ')'", TokenSet::new(&[R_PAREN, NEWLINE, DEDENT]));
        p.eat(R_PAREN);
    }
    m.complete(p, PAREN_EXPR)
}

fn list_or_comp(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(L_BRACK);
    if p.eat(R_BRACK) {
        return m.complete(p, LIST_EXPR);
    }
    if star_named_expression(p).is_none() {
        p.error_recover("expected expression", TokenSet::new(&[R_BRACK, NEWLINE]));
        p.eat(R_BRACK);
        return m.complete(p, LIST_EXPR);
    }
    if p.at(FOR_KW) || (p.at(ASYNC_KW) && p.nth_at(1, FOR_KW)) {
        comp_clauses(p);
        if !p.eat(R_BRACK) {
            p.push_error("expected ']'");
        }
        return m.complete(p, LIST_COMP);
    }
    while p.eat(COMMA) {
        p.step();
        if p.at(R_BRACK) {
            break;
        }
        if star_named_expression(p).is_none() {
            p.error_recover(
                "expected expression",
                TokenSet::new(&[COMMA, R_BRACK, NEWLINE]),
            );
        }
    }
    if !p.eat(R_BRACK) {
        p.error_recover("expected ']'", TokenSet::new(&[R_BRACK, NEWLINE, DEDENT]));
        p.eat(R_BRACK);
    }
    m.complete(p, LIST_EXPR)
}

fn dict_or_set(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(L_BRACE);
    if p.eat(R_BRACE) {
        return m.complete(p, DICT_EXPR);
    }
    // First element decides dict vs set.
    if p.at(STAR_STAR) {
        dict_items(p, true);
        if !p.eat(R_BRACE) {
            p.error_recover("expected '}'", TokenSet::new(&[R_BRACE, NEWLINE, DEDENT]));
            p.eat(R_BRACE);
        }
        return m.complete(p, DICT_EXPR);
    }
    let first = p.start();
    if star_named_expression(p).is_none() {
        first.abandon(p);
        p.error_recover("expected expression", TokenSet::new(&[R_BRACE, NEWLINE]));
        p.eat(R_BRACE);
        return m.complete(p, DICT_EXPR);
    }
    if p.at(COLON) {
        p.bump_any();
        if expression_cm(p).is_none() {
            p.push_error("expected value expression");
        }
        first.complete(p, DICT_ITEM);
        if p.at(FOR_KW) || (p.at(ASYNC_KW) && p.nth_at(1, FOR_KW)) {
            comp_clauses(p);
            if !p.eat(R_BRACE) {
                p.push_error("expected '}'");
            }
            return m.complete(p, DICT_COMP);
        }
        if p.eat(COMMA) {
            dict_items(p, false);
        }
        if !p.eat(R_BRACE) {
            p.error_recover("expected '}'", TokenSet::new(&[R_BRACE, NEWLINE, DEDENT]));
            p.eat(R_BRACE);
        }
        return m.complete(p, DICT_EXPR);
    }
    first.abandon(p);
    if p.at(FOR_KW) || (p.at(ASYNC_KW) && p.nth_at(1, FOR_KW)) {
        comp_clauses(p);
        if !p.eat(R_BRACE) {
            p.push_error("expected '}'");
        }
        return m.complete(p, SET_COMP);
    }
    while p.eat(COMMA) {
        p.step();
        if p.at(R_BRACE) {
            break;
        }
        if star_named_expression(p).is_none() {
            p.error_recover(
                "expected expression",
                TokenSet::new(&[COMMA, R_BRACE, NEWLINE]),
            );
        }
    }
    if !p.eat(R_BRACE) {
        p.error_recover("expected '}'", TokenSet::new(&[R_BRACE, NEWLINE, DEDENT]));
        p.eat(R_BRACE);
    }
    m.complete(p, SET_EXPR)
}

fn dict_items(p: &mut Parser<'_>, first: bool) {
    let mut first = first;
    loop {
        p.step();
        if p.at(R_BRACE) || p.at(EOF) || p.at(NEWLINE) {
            break;
        }
        if !first {
            // comma already consumed by caller or previous iteration
        }
        first = false;
        let item = p.start();
        if p.at(STAR_STAR) {
            p.bump_any();
            if bitwise_or(p).is_none() {
                p.push_error("expected expression after '**'");
            }
            item.complete(p, DICT_UNPACK);
        } else {
            if expression_cm(p).is_none() {
                item.abandon(p);
                p.error_recover(
                    "expected dict key",
                    TokenSet::new(&[COMMA, R_BRACE, NEWLINE]),
                );
                if !p.eat(COMMA) {
                    break;
                }
                continue;
            }
            if p.expect(COLON) && expression_cm(p).is_none() {
                p.push_error("expected value expression");
            }
            item.complete(p, DICT_ITEM);
        }
        if !p.eat(COMMA) {
            break;
        }
    }
}

fn comp_clauses(p: &mut Parser<'_>) {
    while p.at(FOR_KW) || (p.at(ASYNC_KW) && p.nth_at(1, FOR_KW)) {
        p.step();
        let c = p.start();
        p.eat(ASYNC_KW);
        p.bump(FOR_KW);
        star_targets(p);
        if p.expect(IN_KW) && disjunction(p).is_none() {
            p.push_error("expected iterable");
        }
        c.complete(p, COMP_FOR);
        while p.at(IF_KW) {
            p.step();
            let i = p.start();
            p.bump_any();
            if disjunction(p).is_none() {
                p.push_error("expected condition");
            }
            i.complete(p, COMP_IF);
        }
    }
}

pub(super) fn yield_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(YIELD_KW);
    if p.eat(FROM_KW) {
        if expression_cm(p).is_none() {
            p.push_error("expected expression after 'yield from'");
        }
        return m.complete(p, YIELD_FROM_EXPR);
    }
    if at_expr_start(p) {
        star_expressions(p);
    }
    m.complete(p, YIELD_EXPR)
}

/// Assignment targets of `for` / comprehension: `a, (b, c), *d`.
pub(super) fn star_targets(p: &mut Parser<'_>) {
    let m = p.start();
    if !star_target(p) {
        m.abandon(p);
        p.push_error("expected target");
        return;
    }
    if p.at(COMMA) {
        while p.eat(COMMA) {
            if p.at(IN_KW) || p.at(EQ) || p.at(R_PAREN) || p.at(R_BRACK) {
                break;
            }
            star_target(p);
        }
        m.complete(p, TUPLE_EXPR);
    } else {
        m.abandon(p);
    }
}

pub(super) fn star_target(p: &mut Parser<'_>) -> bool {
    if p.at(STAR) {
        let m = p.start();
        p.bump_any();
        let ok = bitwise_or(p).is_some();
        m.complete(p, STAR_EXPR);
        return ok;
    }
    bitwise_or(p).is_some()
}

pub(super) fn del_targets(p: &mut Parser<'_>) {
    let m = p.start();
    if bitwise_or(p).is_none() {
        m.abandon(p);
        p.push_error("expected target");
        return;
    }
    if p.at(COMMA) {
        while p.eat(COMMA) {
            if !at_expr_start(p) {
                break;
            }
            bitwise_or(p);
        }
        m.complete(p, TUPLE_EXPR);
    } else {
        m.abandon(p);
    }
}

#[allow(dead_code)]
fn _kind(k: SyntaxKind) -> SyntaxKind {
    k
}
