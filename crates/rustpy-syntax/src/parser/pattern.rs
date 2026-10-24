//! `match` statement and pattern grammar (PEP 634).

use super::{expr, stmt, Parser, TokenSet, EXPR_COST};
use crate::syntax_kind::SyntaxKind::*;

pub(super) fn match_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump_as(MATCH_KW);
    expr::star_named_expressions(p);
    stmt::expect_colon(p);
    if !p.eat(NEWLINE) {
        p.push_error("expected newline");
    }
    if !p.eat(INDENT) {
        p.push_error("expected an indented block of case clauses");
        m.complete(p, MATCH_STMT);
        return;
    }
    while !p.at(DEDENT) && !p.at(EOF) {
        p.step();
        if p.at(NEWLINE) {
            p.bump_any();
            continue;
        }
        if p.at_ident("case") {
            case_clause(p);
        } else {
            p.error_recover("expected 'case'", TokenSet::new(&[NEWLINE, DEDENT]));
            p.eat(NEWLINE);
        }
    }
    p.eat(DEDENT);
    m.complete(p, MATCH_STMT);
}

fn case_clause(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump_as(CASE_KW);
    patterns(p);
    if p.at(IF_KW) {
        let g = p.start();
        p.bump_any();
        expr::expect_named_expression(p);
        g.complete(p, GUARD);
    }
    stmt::expect_colon(p);
    stmt::block(p);
    m.complete(p, CASE_CLAUSE);
}

/// Top-level patterns: an open sequence `a, b` becomes a sequence pattern.
fn patterns(p: &mut Parser<'_>) {
    let m = p.start();
    let first = pattern(p);
    if p.at(COMMA) {
        while p.eat(COMMA) {
            if p.at(COLON) || p.at(IF_KW) {
                break;
            }
            pattern(p);
        }
        m.complete(p, MATCH_SEQUENCE_PATTERN);
    } else {
        let _ = first;
        m.abandon(p);
    }
}

/// pattern: or_pattern ['as' NAME]
pub(super) fn pattern(p: &mut Parser<'_>) -> bool {
    // Patterns nest through groups, sequences, mappings and class patterns.
    if !p.enter_expr(EXPR_COST) {
        return false;
    }
    let ok = as_pattern(p);
    p.leave(EXPR_COST);
    ok
}

fn as_pattern(p: &mut Parser<'_>) -> bool {
    let m = p.start();
    if !or_pattern(p) {
        m.abandon(p);
        return false;
    }
    if p.at(AS_KW) {
        p.bump_any();
        if !p.expect(IDENT) {
            // keep going
        }
        m.complete(p, MATCH_AS_PATTERN);
    } else {
        m.abandon(p);
    }
    true
}

fn or_pattern(p: &mut Parser<'_>) -> bool {
    let m = p.start();
    if !closed_pattern(p) {
        m.abandon(p);
        return false;
    }
    if p.at(PIPE) {
        while p.eat(PIPE) {
            if !closed_pattern(p) {
                p.push_error("expected pattern after '|'");
                break;
            }
        }
        m.complete(p, MATCH_OR_PATTERN);
    } else {
        m.abandon(p);
    }
    true
}

fn closed_pattern(p: &mut Parser<'_>) -> bool {
    match p.current() {
        INT_NUMBER | FLOAT_NUMBER | COMPLEX_NUMBER | STRING | FSTRING_START | TSTRING_START
        | MINUS => {
            let m = p.start();
            literal_number_or_string(p);
            m.complete(p, MATCH_VALUE_PATTERN);
        }
        NONE_KW | TRUE_KW | FALSE_KW => {
            let m = p.start();
            p.bump_any();
            m.complete(p, MATCH_SINGLETON_PATTERN);
        }
        IDENT if p.nth_text(0) == "_" && !p.nth_at(1, DOT) && !p.nth_at(1, L_PAREN) => {
            let m = p.start();
            p.bump_as(UNDERSCORE_KW);
            m.complete(p, MATCH_WILDCARD_PATTERN);
        }
        IDENT => {
            let m = p.start();
            p.bump_any();
            let mut dotted = false;
            while p.at(DOT) {
                p.bump_any();
                p.expect(IDENT);
                dotted = true;
            }
            if p.at(L_PAREN) {
                class_pattern_args(p);
                m.complete(p, MATCH_CLASS_PATTERN);
            } else if dotted {
                m.complete(p, MATCH_VALUE_PATTERN);
            } else {
                m.complete(p, MATCH_CAPTURE_PATTERN);
            }
        }
        L_PAREN => {
            let m = p.start();
            p.bump_any();
            if p.eat(R_PAREN) {
                m.complete(p, MATCH_SEQUENCE_PATTERN);
                return true;
            }
            pattern(p);
            if p.at(COMMA) {
                while p.eat(COMMA) {
                    if p.at(R_PAREN) {
                        break;
                    }
                    pattern(p);
                }
                p.expect(R_PAREN);
                m.complete(p, MATCH_SEQUENCE_PATTERN);
            } else {
                p.expect(R_PAREN);
                m.complete(p, MATCH_GROUP_PATTERN);
            }
        }
        L_BRACK => {
            let m = p.start();
            p.bump_any();
            while !p.at(R_BRACK) && !p.at(EOF) && !p.at(NEWLINE) {
                p.step();
                if !pattern(p) {
                    p.error_recover(
                        "expected pattern",
                        TokenSet::new(&[COMMA, R_BRACK, NEWLINE]),
                    );
                }
                if !p.eat(COMMA) {
                    break;
                }
            }
            p.expect(R_BRACK);
            m.complete(p, MATCH_SEQUENCE_PATTERN);
        }
        L_BRACE => mapping_pattern(p),
        STAR => {
            let m = p.start();
            p.bump_any();
            if p.at_ident("_") {
                p.bump_as(UNDERSCORE_KW);
            } else {
                p.expect(IDENT);
            }
            m.complete(p, MATCH_STAR_PATTERN);
        }
        _ => return false,
    }
    true
}

fn literal_number_or_string(p: &mut Parser<'_>) {
    if matches!(p.current(), STRING | FSTRING_START | TSTRING_START) {
        expr::strings(p);
        return;
    }
    p.eat(MINUS);
    if !matches!(p.current(), INT_NUMBER | FLOAT_NUMBER | COMPLEX_NUMBER) {
        p.push_error("expected number");
        return;
    }
    p.bump_any();
    if (p.at(PLUS) || p.at(MINUS)) && p.nth_at(1, COMPLEX_NUMBER) {
        p.bump_any();
        p.bump_any();
    }
}

fn class_pattern_args(p: &mut Parser<'_>) {
    p.bump(L_PAREN);
    while !p.at(R_PAREN) && !p.at(EOF) && !p.at(NEWLINE) {
        p.step();
        let m = p.start();
        if p.at(IDENT) && p.nth_at(1, EQ) {
            p.bump_any();
            p.bump_any();
            if !pattern(p) {
                p.push_error("expected pattern");
            }
            m.complete(p, MATCH_KEYWORD_PATTERN);
        } else if pattern(p) {
            m.complete(p, MATCH_CLASS_ARG);
        } else {
            m.abandon(p);
            p.error_recover(
                "expected pattern",
                TokenSet::new(&[COMMA, R_PAREN, NEWLINE]),
            );
        }
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(R_PAREN);
}

fn mapping_pattern(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(L_BRACE);
    while !p.at(R_BRACE) && !p.at(EOF) && !p.at(NEWLINE) {
        p.step();
        if p.at(STAR_STAR) {
            let r = p.start();
            p.bump_any();
            p.expect(IDENT);
            r.complete(p, MATCH_MAPPING_REST);
        } else {
            let item = p.start();
            // key: literal or value pattern
            match p.current() {
                INT_NUMBER | FLOAT_NUMBER | COMPLEX_NUMBER | STRING | FSTRING_START
                | TSTRING_START | MINUS => literal_number_or_string(p),
                NONE_KW | TRUE_KW | FALSE_KW => p.bump_any(),
                IDENT => {
                    p.bump_any();
                    while p.eat(DOT) {
                        p.expect(IDENT);
                    }
                }
                _ => {
                    item.abandon(p);
                    p.error_recover(
                        "expected mapping key",
                        TokenSet::new(&[COMMA, R_BRACE, NEWLINE]),
                    );
                    if !p.eat(COMMA) {
                        break;
                    }
                    continue;
                }
            }
            stmt::expect_colon(p);
            if !pattern(p) {
                p.push_error("expected pattern");
            }
            item.complete(p, MATCH_MAPPING_ITEM);
        }
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(R_BRACE);
    m.complete(p, MATCH_MAPPING_PATTERN);
}
