//! Statement grammar: module, simple statements, compound statements,
//! definitions, imports, decorators, parameters, type parameters.

use super::{expr, pattern, Parser, TokenSet, BLOCK_COST};
use crate::syntax_kind::SyntaxKind::{self, *};

const AUG_ASSIGN: TokenSet = TokenSet::new(&[
    PLUS_EQ,
    MINUS_EQ,
    STAR_EQ,
    SLASH_EQ,
    SLASH_SLASH_EQ,
    PERCENT_EQ,
    AT_EQ,
    AMP_EQ,
    PIPE_EQ,
    CARET_EQ,
    SHR_EQ,
    SHL_EQ,
    STAR_STAR_EQ,
]);

pub(super) fn module(p: &mut Parser<'_>) {
    let m = p.start();
    while !p.at(EOF) {
        module_item(p);
    }
    m.complete(p, MODULE);
}

/// One iteration of the module-level statement loop (shared with the
/// incremental reparser, see [`super::parse_statement_run`]).
pub(super) fn module_item(p: &mut Parser<'_>) {
    p.step();
    match p.current() {
        NEWLINE | DEDENT => p.bump_any(),
        INDENT => unexpected_indent(p),
        _ => statement(p),
    }
}

/// Statements up to the DEDENT that closes the current indentation level
/// (which is left for the caller).
fn statements_until_dedent(p: &mut Parser<'_>) {
    while !p.at(EOF) && !p.at(DEDENT) {
        block_item(p);
    }
}

/// An INDENT where no block can start: report it, then parse the
/// over-indented statements as if they were not indented, up to and
/// including the DEDENT that matches the INDENT.
fn unexpected_indent(p: &mut Parser<'_>) {
    p.push_error("unexpected indent");
    if !p.enter(BLOCK_COST) {
        skip_indented(p);
        return;
    }
    p.bump(INDENT);
    statements_until_dedent(p);
    p.eat(DEDENT);
    p.leave(BLOCK_COST);
}

/// Error recovery for blocks nested too deeply: skips everything up to and
/// including the DEDENT that matches the INDENT at the current position.
fn skip_indented(p: &mut Parser<'_>) {
    let m = p.start();
    let mut level = 0usize;
    loop {
        match p.current() {
            EOF => break,
            INDENT => level += 1,
            DEDENT => level = level.saturating_sub(1),
            _ => {}
        }
        p.bump_any();
        if level == 0 {
            break;
        }
    }
    m.complete(p, ERROR);
}

/// Parses an indented block after `:` (or simple statements on the same line).
pub(super) fn block(p: &mut Parser<'_>) {
    let m = p.start();
    if p.at(NEWLINE) {
        p.bump_any();
        if p.at(INDENT) {
            if p.enter(BLOCK_COST) {
                p.bump_any();
                statements_until_dedent(p);
                p.eat(DEDENT);
                p.leave(BLOCK_COST);
            } else {
                skip_indented(p);
            }
        } else {
            p.push_error("expected an indented block");
        }
    } else if p.at(EOF) {
        p.push_error("expected an indented block");
    } else {
        simple_stmts(p);
    }
    m.complete(p, BLOCK);
}

/// One iteration of an indented block's statement loop (shared with the
/// incremental reparser, see [`super::parse_statement_run`]).
pub(super) fn block_item(p: &mut Parser<'_>) {
    p.step();
    match p.current() {
        NEWLINE => p.bump_any(),
        INDENT => unexpected_indent(p),
        _ => statement(p),
    }
}

pub(super) fn statement(p: &mut Parser<'_>) {
    match p.current() {
        DEF_KW => func_def(p, None),
        CLASS_KW => class_def(p, None),
        AT => decorated(p),
        ASYNC_KW => match p.nth(1) {
            DEF_KW => func_def(p, None),
            WITH_KW => with_stmt(p),
            FOR_KW => for_stmt(p),
            _ => {
                p.error_and_bump("expected 'def', 'with' or 'for' after 'async'");
            }
        },
        IF_KW => if_stmt(p),
        WHILE_KW => while_stmt(p),
        FOR_KW => for_stmt(p),
        TRY_KW => try_stmt(p),
        WITH_KW => with_stmt(p),
        IDENT if p.nth_text(0) == "match" && looks_like_match(p) => pattern::match_stmt(p),
        _ => simple_stmts(p),
    }
}

/// `type` is a soft keyword: `type X = ...` / `type X[T] = ...` is a type
/// alias (a simple statement, so it can follow `;` or `class A:`), while
/// `type(x)`, `type = 1` or `type.x` use `type` as a name.
fn at_type_alias(p: &Parser<'_>) -> bool {
    p.at_ident("type") && p.nth_at(1, IDENT) && matches!(p.nth(2), EQ | L_BRACK)
}

/// `match` is a soft keyword: try to parse a match statement, else fall back.
fn looks_like_match(p: &mut Parser<'_>) -> bool {
    // `match` followed by something that cannot start a subject, or by `=`/`.`/`(`-call-ish tokens
    // is not a statement. We speculatively parse `match <subject> :` NEWLINE.
    if matches!(
        p.nth(1),
        EQ | DOT | COLON | NEWLINE | COMMA | SEMICOLON | EOF | R_PAREN | R_BRACK | R_BRACE
    ) || AUG_ASSIGN.contains(p.nth(1))
    {
        return false;
    }
    let cp = p.checkpoint();
    let errors_before = p.errors_len();
    p.bump_as(MATCH_KW);
    let m = p.start();
    expr::star_named_expressions(p);
    m.abandon(p);
    let ok = p.at(COLON) && p.nth_at(1, NEWLINE) && p.errors_len() == errors_before;
    p.rewind(cp);
    ok
}

fn simple_stmts(p: &mut Parser<'_>) {
    loop {
        p.step();
        simple_stmt(p);
        if p.eat(SEMICOLON) {
            if p.at(NEWLINE) || p.at(EOF) {
                break;
            }
            continue;
        }
        break;
    }
    if !p.eat(NEWLINE) && !p.at(EOF) && !p.at(DEDENT) {
        p.error_recover(
            "expected end of statement",
            TokenSet::new(&[NEWLINE, DEDENT]),
        );
        p.eat(NEWLINE);
    }
}

fn simple_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    match p.current() {
        PASS_KW => {
            p.bump_any();
            m.complete(p, PASS_STMT);
        }
        BREAK_KW => {
            p.bump_any();
            m.complete(p, BREAK_STMT);
        }
        CONTINUE_KW => {
            p.bump_any();
            m.complete(p, CONTINUE_STMT);
        }
        RETURN_KW => {
            p.bump_any();
            if !at_stmt_end(p) {
                expr::expect_star_expressions(p);
            }
            m.complete(p, RETURN_STMT);
        }
        RAISE_KW => {
            p.bump_any();
            if !at_stmt_end(p) {
                expr::expect_expression(p);
                if p.eat(FROM_KW) {
                    expr::expect_expression(p);
                }
            }
            m.complete(p, RAISE_STMT);
        }
        GLOBAL_KW | NONLOCAL_KW => {
            let kind = if p.at(GLOBAL_KW) {
                GLOBAL_STMT
            } else {
                NONLOCAL_STMT
            };
            p.bump_any();
            name_list(p);
            m.complete(p, kind);
        }
        DEL_KW => {
            p.bump_any();
            expr::del_targets(p);
            m.complete(p, DEL_STMT);
        }
        ASSERT_KW => {
            p.bump_any();
            expr::expect_expression(p);
            if p.eat(COMMA) {
                expr::expect_expression(p);
            }
            m.complete(p, ASSERT_STMT);
        }
        IMPORT_KW => {
            p.bump_any();
            import_names(p);
            m.complete(p, IMPORT_STMT);
        }
        FROM_KW => {
            p.bump_any();
            import_from_rest(p);
            m.complete(p, IMPORT_FROM_STMT);
        }
        IDENT if at_type_alias(p) => {
            type_alias(p);
            m.complete(p, TYPE_ALIAS_STMT);
        }
        _ => {
            expression_or_assignment(p, m);
        }
    }
}

fn at_stmt_end(p: &Parser<'_>) -> bool {
    matches!(p.current(), NEWLINE | SEMICOLON | EOF | DEDENT)
}

fn expression_or_assignment(p: &mut Parser<'_>, m: super::Marker) {
    if at_stmt_end(p) {
        p.error_and_bump("expected statement");
        m.abandon(p);
        return;
    }
    if !expr::star_expressions_or_yield(p) {
        p.error_recover(
            "expected statement",
            TokenSet::new(&[NEWLINE, SEMICOLON, DEDENT]),
        );
        m.abandon(p);
        return;
    }
    match p.current() {
        EQ => {
            // Chained assignment: a = b = value
            while p.eat(EQ) {
                p.step();
                if at_stmt_end(p) || !expr::star_expressions_or_yield(p) {
                    p.push_error("expected expression after '='");
                    break;
                }
            }
            m.complete(p, ASSIGN_STMT);
        }
        COLON => {
            p.bump_any();
            let a = p.start();
            expr::expect_expression(p);
            a.complete(p, ANNOTATION);
            if p.eat(EQ) && !expr::star_expressions_or_yield(p) {
                p.push_error("expected expression after '='");
            }
            m.complete(p, ANN_ASSIGN_STMT);
        }
        k if AUG_ASSIGN.contains(k) => {
            p.bump_any();
            if at_stmt_end(p) || !expr::star_expressions_or_yield(p) {
                p.push_error("expected expression");
            }
            m.complete(p, AUG_ASSIGN_STMT);
        }
        _ => {
            m.complete(p, EXPR_STMT);
        }
    }
}

fn name_list(p: &mut Parser<'_>) {
    if !p.expect(IDENT) {
        return;
    }
    while p.eat(COMMA) {
        if !p.expect(IDENT) {
            break;
        }
    }
}

fn dotted_name(p: &mut Parser<'_>) -> bool {
    let m = p.start();
    if !p.at(IDENT) {
        m.abandon(p);
        p.push_error("expected module name");
        return false;
    }
    p.bump_any();
    while p.at(DOT) && p.nth_at(1, IDENT) {
        p.bump_any();
        p.bump_any();
    }
    if p.at(DOT) {
        p.error_and_bump("expected name after '.'");
    }
    m.complete(p, DOTTED_NAME);
    true
}

fn import_names(p: &mut Parser<'_>) {
    loop {
        let m = p.start();
        if !dotted_name(p) {
            m.abandon(p);
            break;
        }
        if p.eat(AS_KW) {
            p.expect(IDENT);
        }
        m.complete(p, IMPORT_ALIAS);
        if !p.eat(COMMA) {
            break;
        }
    }
}

fn import_from_rest(p: &mut Parser<'_>) {
    let future = p.at_ident("__future__") && p.nth_at(1, IMPORT_KW);
    // relative level: dots / ellipsis
    if p.at(DOT) || p.at(ELLIPSIS) {
        let m = p.start();
        while p.at(DOT) || p.at(ELLIPSIS) {
            p.bump_any();
        }
        m.complete(p, RELATIVE_LEVEL);
        if p.at(IDENT) {
            dotted_name(p);
        }
    } else if !dotted_name(p) {
        return;
    }
    if !p.expect(IMPORT_KW) {
        return;
    }
    if p.eat(STAR) {
        return;
    }
    let paren = p.eat(L_PAREN);
    loop {
        p.step();
        if paren && p.at(R_PAREN) {
            break;
        }
        let m = p.start();
        if !p.at(IDENT) {
            m.abandon(p);
            p.push_error("expected imported name");
            break;
        }
        if future && p.nth_text(0) == "barry_as_FLUFL" {
            p.barry_as_flufl = true;
        }
        p.bump_any();
        if p.eat(AS_KW) {
            p.expect(IDENT);
        }
        m.complete(p, IMPORT_ALIAS);
        if !p.eat(COMMA) {
            break;
        }
    }
    if paren {
        p.expect(R_PAREN);
    }
}

fn decorated(p: &mut Parser<'_>) {
    let m = p.start();
    let decos = p.start();
    while p.at(AT) {
        p.step();
        let d = p.start();
        p.bump_any();
        expr::expect_named_expression(p);
        if !p.eat(NEWLINE) {
            p.error_recover(
                "expected newline after decorator",
                TokenSet::new(&[NEWLINE, DEF_KW, CLASS_KW, AT, ASYNC_KW]),
            );
            p.eat(NEWLINE);
        }
        d.complete(p, DECORATOR);
        while p.at(NEWLINE) {
            p.bump_any();
        }
    }
    decos.complete(p, DECORATORS);
    match p.current() {
        DEF_KW => func_def(p, Some(m)),
        ASYNC_KW if p.nth_at(1, DEF_KW) => func_def(p, Some(m)),
        CLASS_KW => class_def(p, Some(m)),
        _ => {
            p.push_error("expected 'def' or 'class' after decorator");
            m.complete(p, FUNC_DEF);
        }
    }
}

fn func_def(p: &mut Parser<'_>, m: Option<super::Marker>) {
    let m = m.unwrap_or_else(|| p.start());
    p.eat(ASYNC_KW);
    p.bump(DEF_KW);
    if !p.expect(IDENT) {
        // Try to continue if there's a parameter list.
    }
    if p.at(L_BRACK) {
        type_params(p);
    }
    if p.at(L_PAREN) {
        parameters(p);
    } else {
        p.push_error("expected '('");
    }
    if p.eat(ARROW) {
        let r = p.start();
        expr::expect_expression(p);
        r.complete(p, RETURN_ANNOTATION);
    }
    if !p.expect(COLON) {
        p.error_recover("expected ':'", TokenSet::new(&[COLON, NEWLINE]));
        p.eat(COLON);
    }
    block(p);
    m.complete(p, FUNC_DEF);
}

fn class_def(p: &mut Parser<'_>, m: Option<super::Marker>) {
    let m = m.unwrap_or_else(|| p.start());
    p.bump(CLASS_KW);
    p.expect(IDENT);
    if p.at(L_BRACK) {
        type_params(p);
    }
    if p.at(L_PAREN) {
        expr::arguments(p);
    }
    if !p.expect(COLON) {
        p.error_recover("expected ':'", TokenSet::new(&[COLON, NEWLINE]));
        p.eat(COLON);
    }
    block(p);
    m.complete(p, CLASS_DEF);
}

/// `(` parameters `)` for `def`; `lambda` uses [`lambda_parameters`].
pub(super) fn parameters(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(L_PAREN);
    parameter_list(p, R_PAREN, true);
    p.expect(R_PAREN);
    m.complete(p, PARAM_LIST);
}

pub(super) fn lambda_parameters(p: &mut Parser<'_>) {
    let m = p.start();
    parameter_list(p, COLON, false);
    m.complete(p, PARAM_LIST);
}

fn parameter_list(p: &mut Parser<'_>, end: SyntaxKind, annotations: bool) {
    while !p.at(end) && !p.at(EOF) && !p.at(NEWLINE) {
        p.step();
        match p.current() {
            SLASH => {
                let m = p.start();
                p.bump_any();
                m.complete(p, SLASH_SEPARATOR);
            }
            STAR => {
                if p.nth_at(1, IDENT) {
                    let m = p.start();
                    p.bump_any();
                    p.bump_any();
                    if annotations && p.eat(COLON) {
                        let a = p.start();
                        if !expr::star_expression_or_expression(p) {
                            p.push_error("expected annotation");
                        }
                        a.complete(p, ANNOTATION);
                    }
                    m.complete(p, STAR_PARAM);
                } else {
                    let m = p.start();
                    p.bump_any();
                    m.complete(p, STAR_SEPARATOR);
                }
            }
            STAR_STAR => {
                let m = p.start();
                p.bump_any();
                p.expect(IDENT);
                if annotations && p.eat(COLON) {
                    let a = p.start();
                    expr::expect_expression(p);
                    a.complete(p, ANNOTATION);
                }
                m.complete(p, STAR_STAR_PARAM);
            }
            IDENT => {
                let m = p.start();
                p.bump_any();
                if annotations && p.eat(COLON) {
                    let a = p.start();
                    expr::expect_expression(p);
                    a.complete(p, ANNOTATION);
                }
                if p.eat(EQ) {
                    let d = p.start();
                    expr::expect_expression(p);
                    d.complete(p, DEFAULT_VALUE);
                }
                m.complete(p, PARAM);
            }
            _ => {
                p.error_recover("expected parameter", TokenSet::new(&[COMMA, end, NEWLINE]));
            }
        }
        if !p.eat(COMMA) {
            break;
        }
    }
}

pub(super) fn type_params(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(L_BRACK);
    while !p.at(R_BRACK) && !p.at(EOF) && !p.at(NEWLINE) {
        p.step();
        let t = p.start();
        let kind = match p.current() {
            STAR => {
                p.bump_any();
                TYPE_VAR_TUPLE_PARAM
            }
            STAR_STAR => {
                p.bump_any();
                PARAM_SPEC_PARAM
            }
            _ => TYPE_PARAM,
        };
        if !p.expect(IDENT) {
            t.complete(p, kind);
            p.error_recover(
                "expected type parameter",
                TokenSet::new(&[COMMA, R_BRACK, NEWLINE]),
            );
        } else {
            if kind == TYPE_PARAM && p.eat(COLON) {
                let a = p.start();
                expr::expect_expression(p);
                a.complete(p, ANNOTATION);
            }
            if p.eat(EQ) {
                let d = p.start();
                expr::star_expression_or_expression(p);
                d.complete(p, DEFAULT_VALUE);
            }
            t.complete(p, kind);
        }
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(R_BRACK);
    m.complete(p, TYPE_PARAM_LIST);
}

/// type_alias: "type" NAME [type_params] '=' expression
fn type_alias(p: &mut Parser<'_>) {
    p.bump_as(TYPE_KW);
    p.expect(IDENT);
    if p.at(L_BRACK) {
        type_params(p);
    }
    p.expect(EQ);
    expr::expect_expression(p);
}

fn if_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(IF_KW);
    expr::expect_named_expression(p);
    expect_colon(p);
    block(p);
    while p.at(ELIF_KW) {
        p.step();
        let c = p.start();
        p.bump_any();
        expr::expect_named_expression(p);
        expect_colon(p);
        block(p);
        c.complete(p, ELIF_CLAUSE);
    }
    else_clause(p);
    m.complete(p, IF_STMT);
}

fn else_clause(p: &mut Parser<'_>) {
    if p.at(ELSE_KW) {
        let c = p.start();
        p.bump_any();
        expect_colon(p);
        block(p);
        c.complete(p, ELSE_CLAUSE);
    }
}

pub(super) fn expect_colon(p: &mut Parser<'_>) {
    if !p.eat(COLON) {
        p.error_recover("expected ':'", TokenSet::new(&[COLON, NEWLINE]));
        p.eat(COLON);
    }
}

fn while_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(WHILE_KW);
    expr::expect_named_expression(p);
    expect_colon(p);
    block(p);
    else_clause(p);
    m.complete(p, WHILE_STMT);
}

fn for_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.eat(ASYNC_KW);
    p.bump(FOR_KW);
    expr::star_targets(p);
    if !p.expect(IN_KW) {
        p.error_recover("expected 'in'", TokenSet::new(&[IN_KW, COLON, NEWLINE]));
        p.eat(IN_KW);
    }
    expr::expect_star_expressions(p);
    expect_colon(p);
    block(p);
    else_clause(p);
    m.complete(p, FOR_STMT);
}

fn try_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(TRY_KW);
    expect_colon(p);
    block(p);
    let mut any = false;
    while p.at(EXCEPT_KW) {
        p.step();
        any = true;
        let c = p.start();
        p.bump_any();
        p.eat(STAR);
        if !p.at(COLON) {
            let types = p.start();
            expr::expect_expression(p);
            if p.at(COMMA) {
                // Python 3.14 (PEP 758): `except A, B:` without parentheses.
                while p.eat(COMMA) {
                    p.step();
                    if matches!(p.current(), COLON | AS_KW) || !expr::expression(p) {
                        break;
                    }
                }
                types.complete(p, TUPLE_EXPR);
                if p.at(AS_KW) {
                    p.push_error("multiple exception types must be parenthesized when using 'as'");
                }
            } else {
                types.abandon(p);
            }
            if p.eat(AS_KW) {
                p.expect(IDENT);
            }
        }
        expect_colon(p);
        block(p);
        c.complete(p, EXCEPT_CLAUSE);
    }
    else_clause(p);
    if p.at(FINALLY_KW) {
        any = true;
        let c = p.start();
        p.bump_any();
        expect_colon(p);
        block(p);
        c.complete(p, FINALLY_CLAUSE);
    }
    if !any {
        p.push_error("expected 'except' or 'finally' block");
    }
    m.complete(p, TRY_STMT);
}

fn with_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.eat(ASYNC_KW);
    p.bump(WITH_KW);
    // Parenthesized with-items (3.9+) vs. a parenthesized expression.
    let mut parsed = false;
    if p.at(L_PAREN) {
        let cp = p.checkpoint();
        let errors = p.errors_len();
        p.bump_any();
        with_items(p, true);
        if p.at(R_PAREN) && p.errors_len() == errors && p.nth_at(1, COLON) {
            p.bump_any();
            parsed = true;
        } else {
            p.rewind(cp);
        }
    }
    if !parsed {
        with_items(p, false);
    }
    expect_colon(p);
    block(p);
    m.complete(p, WITH_STMT);
}

fn with_items(p: &mut Parser<'_>, parenthesized: bool) {
    loop {
        p.step();
        if parenthesized && p.at(R_PAREN) {
            break;
        }
        let m = p.start();
        if !expr::expression(p) {
            m.abandon(p);
            p.push_error("expected expression");
            break;
        }
        if p.eat(AS_KW) {
            expr::star_target(p);
        }
        m.complete(p, WITH_ITEM);
        if !p.eat(COMMA) {
            break;
        }
    }
}
