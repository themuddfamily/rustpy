//! Port of `black/nodes.py`: whitespace rules and tree predicates.

use super::strings::{get_string_prefix, has_triple_quotes};
use super::tree::*;

pub fn is_opening(t: u16) -> bool {
    matches!(t, LPAR | LSQB | LBRACE)
}

pub fn is_closing(t: u16) -> bool {
    matches!(t, RPAR | RSQB | RBRACE)
}

pub fn is_bracket(t: u16) -> bool {
    is_opening(t) || is_closing(t)
}

/// `BRACKET[opening]`: the matching closing bracket type.
pub fn closing_of(t: u16) -> u16 {
    match t {
        LPAR => RPAR,
        LSQB => RSQB,
        _ => RBRACE,
    }
}

pub fn is_whitespace_token(t: u16) -> bool {
    matches!(t, DEDENT | INDENT | NEWLINE)
}

pub fn is_statement(t: u16) -> bool {
    matches!(
        t,
        IF_STMT
            | WHILE_STMT
            | FOR_STMT
            | TRY_STMT
            | EXCEPT_CLAUSE
            | WITH_STMT
            | FUNCDEF
            | CLASSDEF
            | MATCH_STMT
            | CASE_BLOCK
    )
}

pub fn is_comparator(t: u16) -> bool {
    matches!(
        t,
        LESS | GREATER | EQEQUAL | NOTEQUAL | LESSEQUAL | GREATEREQUAL
    )
}

pub fn is_math_operator(t: u16) -> bool {
    matches!(
        t,
        VBAR | CIRCUMFLEX
            | AMPER
            | LEFTSHIFT
            | RIGHTSHIFT
            | PLUS
            | MINUS
            | STAR
            | SLASH
            | DOUBLESLASH
            | PERCENT
            | AT
            | TILDE
            | DOUBLESTAR
    )
}

pub fn is_varargs_special(t: u16) -> bool {
    matches!(t, STAR | DOUBLESTAR | SLASH)
}

pub const VARARGS_PARENTS: &[u16] = &[
    ARGLIST,
    ARGUMENT,
    TRAILER,
    TYPEDARGSLIST,
    VARARGSLIST,
    TYPEVARTUPLE,
    PARAMSPEC,
];
pub const UNPACKING_PARENTS: &[u16] = &[
    ATOM,
    DICTSETMAKER,
    LISTMAKER,
    TESTLIST_GEXP,
    TESTLIST_STAR_EXPR,
    SUBJECT_EXPR,
    PATTERN,
];

pub fn is_test_descendant(t: u16) -> bool {
    matches!(
        t,
        TEST | LAMBDEF
            | OR_TEST
            | AND_TEST
            | NOT_TEST
            | COMPARISON
            | STAR_EXPR
            | EXPR
            | XOR_EXPR
            | AND_EXPR
            | SHIFT_EXPR
            | ARITH_EXPR
            | TRAILER
            | TERM
            | POWER
            | NAMEDEXPR_TEST
    )
}

pub fn is_typed_name(t: u16) -> bool {
    matches!(t, TNAME | TNAME_STAR)
}

pub const ASSIGNMENTS: &[&str] = &[
    "=", "+=", "-=", "*=", "@=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=", "**=", "//=", ":",
];

fn always_no_space(t: u16) -> bool {
    is_closing(t) || matches!(t, COMMA | STANDALONE_COMMENT | BANG)
}

/// `whitespace(leaf, complex_subscript)`: the prefix a leaf gets on a line.
pub fn whitespace(tree: &Tree, leaf: Id, complex_subscript: bool) -> String {
    const NO: &str = "";
    const SPACE: &str = " ";
    let t = tree.ty(leaf);
    let v = tree.value(leaf);
    if always_no_space(t) {
        return NO.into();
    }
    if t == COMMENT {
        return "  ".into();
    }
    let Some(p) = tree.parent(leaf) else {
        return SPACE.into();
    };
    let pt = tree.ty(p);
    if t == COLON && !matches!(pt, SUBSCRIPT | SUBSCRIPTLIST | SLICEOP) {
        return NO.into();
    }
    let prev = tree.prev_sibling(leaf);
    match prev {
        None => {
            let prevp = preceding_leaf(tree, Some(p));
            let Some(prevp) = prevp else {
                return NO.into();
            };
            let ppt = tree.ty(prevp);
            if is_opening(ppt) {
                return NO.into();
            }
            if t == COLON {
                if ppt == COLON || (ppt != COMMA && !complex_subscript) {
                    return NO.into();
                }
                return SPACE.into();
            }
            let prevp_parent = tree.parent(prevp);
            if ppt == EQUAL {
                if let Some(pp) = prevp_parent {
                    let ppt2 = tree.ty(pp);
                    if matches!(ppt2, ARGLIST | ARGUMENT | PARAMETERS | VARARGSLIST) {
                        return NO.into();
                    } else if ppt2 == TYPEDARGSLIST {
                        return tree.n(prevp).prefix.clone();
                    }
                }
            } else if ppt == STAR
                && prevp_parent.map(|x| tree.ty(x)) == Some(STAR_EXPR)
                && matches!(
                    prevp_parent.and_then(|x| tree.parent_ty(x)),
                    Some(SUBSCRIPTLIST) | Some(TNAME_STAR)
                )
            {
                return NO.into();
            } else if is_varargs_special(ppt) {
                if is_vararg(tree, prevp, VARARGS_PARENTS, UNPACKING_PARENTS) {
                    return NO.into();
                }
            } else if ppt == COLON {
                if let Some(pp) = prevp_parent {
                    if matches!(tree.ty(pp), SUBSCRIPT | SLICEOP) {
                        return if complex_subscript {
                            SPACE.into()
                        } else {
                            NO.into()
                        };
                    }
                }
            } else if (prevp_parent.map(|x| tree.ty(x)) == Some(FACTOR) && is_math_operator(ppt))
                || (ppt == AT && tree.parent_ty(p) == Some(DECORATOR))
            {
                return NO.into();
            }
        }
        Some(pr) => {
            let prt = tree.ty(pr);
            if is_opening(prt) || prt == BANG {
                return NO.into();
            }
        }
    }
    let prev_ty = prev.map(|x| tree.ty(x));
    match pt {
        PARAMETERS | ARGLIST => {
            if prev_ty != Some(COMMA) {
                return NO.into();
            }
        }
        VARARGSLIST => {
            if prev.is_some() && prev_ty != Some(COMMA) {
                return NO.into();
            }
        }
        TYPEDARGSLIST => {
            let Some(pr) = prev else {
                return NO.into();
            };
            let prt = tree.ty(pr);
            if t == EQUAL {
                if !is_typed_name(prt) {
                    return NO.into();
                }
            } else if prt == EQUAL {
                return tree.n(pr).prefix.clone();
            } else if prt != COMMA {
                return NO.into();
            }
        }
        TNAME | TNAME_STAR => {
            if prev.is_none() {
                let prevp = preceding_leaf(tree, Some(p));
                if prevp.map(|x| tree.ty(x)) != Some(COMMA) {
                    return NO.into();
                }
            }
        }
        TRAILER => {
            if t == LPAR || t == RPAR {
                return NO.into();
            }
            match prev {
                None => {
                    if t == DOT || t == LSQB {
                        return NO.into();
                    }
                }
                Some(_) => {
                    if prev_ty != Some(COMMA) {
                        return NO.into();
                    }
                }
            }
        }
        ARGUMENT => {
            if t == EQUAL {
                return NO.into();
            }
            match prev {
                None => {
                    let prevp = preceding_leaf(tree, Some(p));
                    match prevp {
                        None => return NO.into(),
                        Some(pp) if tree.ty(pp) == LPAR => return NO.into(),
                        _ => {}
                    }
                }
                Some(pr) => {
                    let prt = tree.ty(pr);
                    if prt == EQUAL || is_varargs_special(prt) {
                        return NO.into();
                    }
                }
            }
        }
        DECORATOR => return NO.into(),
        DOTTED_NAME => {
            if prev.is_some() {
                return NO.into();
            }
            let prevp = preceding_leaf(tree, Some(p));
            match prevp {
                None => return NO.into(),
                Some(pp) if matches!(tree.ty(pp), AT | DOT) => return NO.into(),
                _ => {}
            }
        }
        CLASSDEF => {
            if t == LPAR {
                return NO.into();
            }
            if prev_ty == Some(LPAR) {
                return NO.into();
            }
        }
        SUBSCRIPT | SLICEOP => match prev {
            None => {
                if tree.parent_ty(p) == Some(SUBSCRIPTLIST) {
                    return SPACE.into();
                }
                return NO.into();
            }
            Some(pr) => {
                if t == COLONEQUAL || tree.ty(pr) == COLONEQUAL {
                    return SPACE.into();
                } else if !complex_subscript {
                    return NO.into();
                }
            }
        },
        ATOM => {
            if prev.is_some() && t == DOT {
                return NO.into();
            }
        }
        DICTSETMAKER => {
            if prev_ty == Some(DOUBLESTAR) {
                return NO.into();
            }
        }
        FACTOR | STAR_EXPR => match prev {
            None => {
                let prevp = preceding_leaf(tree, Some(p));
                let Some(prevp) = prevp else {
                    return NO.into();
                };
                let ppt = tree.ty(prevp);
                if is_opening(ppt) {
                    return NO.into();
                }
                let ppp = tree.parent(prevp).map(|x| tree.ty(x));
                if (ppt == COLON && matches!(ppp, Some(SUBSCRIPT) | Some(SLICEOP)))
                    || (ppt == EQUAL && ppp == Some(ARGUMENT))
                {
                    return NO.into();
                }
            }
            Some(_) => {
                if matches!(t, NAME | NUMBER | STRING) {
                    return NO.into();
                }
            }
        },
        IMPORT_FROM => {
            if t == DOT {
                if prev_ty == Some(DOT) {
                    return NO.into();
                }
            } else if t == NAME {
                if v == "import" {
                    return SPACE.into();
                }
                if prev_ty == Some(DOT) {
                    return NO.into();
                }
            }
        }
        EXCEPT_CLAUSE if t == STAR => return NO.into(),
        _ => {}
    }
    SPACE.into()
}

/// The first leaf that precedes `node`, if any.
pub fn preceding_leaf(tree: &Tree, node: Option<Id>) -> Option<Id> {
    let mut node = node;
    while let Some(n) = node {
        if let Some(res) = tree.prev_sibling(n) {
            if tree.is_leaf(res) {
                return Some(res);
            }
            return tree.last_leaf(res);
        }
        node = tree.parent(n);
    }
    None
}

/// The child of `ancestor` that contains `descendant`.
pub fn child_towards(tree: &Tree, ancestor: Id, descendant: Id) -> Option<Id> {
    let mut node = Some(descendant);
    while let Some(n) = node {
        if tree.parent(n) == Some(ancestor) {
            return Some(n);
        }
        node = tree.parent(n);
    }
    None
}

pub fn is_vararg(tree: &Tree, leaf: Id, within: &[u16], within2: &[u16]) -> bool {
    if !is_varargs_special(tree.ty(leaf)) {
        return false;
    }
    let Some(mut p) = tree.parent(leaf) else {
        return false;
    };
    if tree.ty(p) == STAR_EXPR {
        match tree.parent(p) {
            Some(pp) => p = pp,
            None => return false,
        }
    }
    let pt = tree.ty(p);
    within.contains(&pt) || within2.contains(&pt)
}

pub fn is_arith_like(tree: &Tree, node: Id) -> bool {
    matches!(tree.ty(node), ARITH_EXPR | SHIFT_EXPR | XOR_EXPR | AND_EXPR)
}

pub fn is_docstring(tree: &Tree, node: Id) -> bool {
    if tree.is_leaf(node) {
        if tree.ty(node) != STRING {
            return false;
        }
        let prefix = get_string_prefix(tree.value(node));
        if prefix.chars().any(|c| "bBfFtT".contains(c)) {
            return false;
        }
    }
    let Some(parent) = tree.parent(node) else {
        return false;
    };
    if tree.ty(parent) == SIMPLE_STMT
        && tree.prev_sibling(parent).is_none()
        && tree.parent_ty(parent) == Some(FILE_INPUT)
    {
        return true;
    }
    if prev_siblings_are(
        tree,
        Some(parent),
        &[None, Some(NEWLINE), Some(INDENT), Some(SIMPLE_STMT)],
    ) {
        return true;
    }
    if prev_siblings_are(
        tree,
        Some(parent),
        &[Some(PARAMETERS), Some(COLON), Some(SIMPLE_STMT)],
    ) {
        return true;
    }
    false
}

pub fn prev_siblings_are(tree: &Tree, node: Option<Id>, tokens: &[Option<u16>]) -> bool {
    let Some((last, rest)) = tokens.split_last() else {
        return true;
    };
    match last {
        None => node.is_none(),
        Some(t) => match node {
            None => false,
            Some(n) => {
                if tree.ty(n) != *t {
                    return false;
                }
                prev_siblings_are(tree, tree.prev_sibling(n), rest)
            }
        },
    }
}

pub fn is_empty_tuple(tree: &Tree, node: Id) -> bool {
    tree.ty(node) == ATOM
        && tree.nchildren(node) == 2
        && tree.ty(tree.child(node, 0)) == LPAR
        && tree.ty(tree.child(node, 1)) == RPAR
}

pub fn unwrap_singleton_parenthesis(tree: &Tree, node: Id) -> Option<Id> {
    let ch = tree.children(node);
    if ch.len() != 3 {
        return None;
    }
    if !(tree.ty(ch[0]) == LPAR && tree.ty(ch[2]) == RPAR) {
        return None;
    }
    Some(ch[1])
}

pub fn is_one_tuple(tree: &Tree, node: Id) -> bool {
    if tree.ty(node) == ATOM {
        let Some(gexp) = unwrap_singleton_parenthesis(tree, node) else {
            return false;
        };
        if tree.ty(gexp) != TESTLIST_GEXP {
            return false;
        }
        return tree.nchildren(gexp) == 2 && tree.ty(tree.child(gexp, 1)) == COMMA;
    }
    matches!(tree.ty(node), TESTLIST | TESTLIST_STAR_EXPR | EXPRLIST)
        && tree.nchildren(node) == 2
        && tree.ty(tree.child(node, 1)) == COMMA
}

fn gexp_of(tree: &Tree, node: Id) -> Option<Id> {
    if tree.ty(node) != ATOM {
        return None;
    }
    let g = unwrap_singleton_parenthesis(tree, node)?;
    if tree.ty(g) != TESTLIST_GEXP {
        return None;
    }
    Some(g)
}

pub fn is_tuple(tree: &Tree, node: Id) -> bool {
    gexp_of(tree, node).is_some()
}

pub fn is_tuple_containing_walrus(tree: &Tree, node: Id) -> bool {
    match gexp_of(tree, node) {
        Some(g) => tree
            .children(g)
            .iter()
            .any(|c| tree.ty(*c) == NAMEDEXPR_TEST),
        None => false,
    }
}

pub fn is_tuple_containing_star(tree: &Tree, node: Id) -> bool {
    match gexp_of(tree, node) {
        Some(g) => tree.children(g).iter().any(|c| tree.ty(*c) == STAR_EXPR),
        None => false,
    }
}

pub fn is_generator(tree: &Tree, node: Id) -> bool {
    match gexp_of(tree, node) {
        Some(g) => tree.children(g).iter().any(|c| tree.ty(*c) == OLD_COMP_FOR),
        None => false,
    }
}

/// True if the content between `opening` and `closing` is a one-sequence.
pub fn is_one_sequence_between(
    tree: &Tree,
    opening: Id,
    closing: Id,
    leaves: &[Id],
    brackets: (u16, u16),
) -> bool {
    if (tree.ty(opening), tree.ty(closing)) != brackets {
        return false;
    }
    let depth = tree.n(closing).bracket_depth + 1;
    let mut opening_index = None;
    if !leaves.is_empty() {
        let (mut left, mut right) = (0usize, leaves.len() - 1);
        while left <= right {
            if leaves[left] == opening {
                opening_index = Some(left);
                break;
            }
            if leaves[right] == opening {
                opening_index = Some(right);
                break;
            }
            left += 1;
            if right == 0 {
                break;
            }
            right -= 1;
        }
    }
    let Some(oi) = opening_index else {
        return false;
    };
    let mut commas = 0;
    for &leaf in &leaves[oi + 1..] {
        if leaf == closing {
            break;
        }
        let n = tree.n(leaf);
        if n.bracket_depth == depth && n.ty == COMMA {
            commas += 1;
            if let Some(p) = n.parent {
                if matches!(tree.ty(p), ARGLIST | TYPEDARGSLIST) {
                    commas += 1;
                    break;
                }
            }
        }
    }
    commas < 2
}

pub fn is_walrus_assignment(tree: &Tree, node: Id) -> bool {
    match unwrap_singleton_parenthesis(tree, node) {
        Some(inner) => tree.ty(inner) == NAMEDEXPR_TEST,
        None => false,
    }
}

fn is_simple_decorator_trailer(tree: &Tree, node: Id, last: bool) -> bool {
    if tree.ty(node) != TRAILER {
        return false;
    }
    let ch = tree.children(node);
    (ch.len() == 2 && tree.ty(ch[0]) == DOT && tree.ty(ch[1]) == NAME)
        || (last && ch.len() == 2 && tree.ty(ch[0]) == LPAR && tree.ty(ch[1]) == RPAR)
        || (last && ch.len() == 3 && tree.ty(ch[0]) == LPAR && tree.ty(ch[2]) == RPAR)
}

pub fn is_simple_decorator_expression(tree: &Tree, node: Id) -> bool {
    if tree.ty(node) == NAME {
        return true;
    }
    if tree.ty(node) == POWER {
        let ch = tree.children(node);
        if !ch.is_empty() {
            return tree.ty(ch[0]) == NAME
                && (ch.len() < 3
                    || ch[1..ch.len() - 1]
                        .iter()
                        .all(|c| is_simple_decorator_trailer(tree, *c, false)))
                && (ch.len() < 2 || is_simple_decorator_trailer(tree, ch[ch.len() - 1], true));
        }
    }
    false
}

pub fn is_yield(tree: &Tree, node: Id) -> bool {
    if tree.ty(node) == YIELD_EXPR {
        return true;
    }
    if tree.ty(node) == NAME && tree.value(node) == "yield" {
        return true;
    }
    if tree.ty(node) != ATOM {
        return false;
    }
    let ch = tree.children(node);
    if ch.len() != 3 {
        return false;
    }
    if tree.ty(ch[0]) == LPAR && tree.ty(ch[2]) == RPAR {
        return is_yield(tree, ch[1]);
    }
    false
}

pub fn is_multiline_string(tree: &Tree, node: Id) -> bool {
    if !tree.is_leaf(node) {
        return false;
    }
    let v = tree.value(node);
    tree.ty(node) == STRING && has_triple_quotes(v) && v.contains('\n')
}

pub fn is_parent_function_or_class(tree: &Tree, node: Id) -> bool {
    matches!(tree.parent_ty(node), Some(FUNCDEF) | Some(CLASSDEF))
}

pub fn is_stub_suite(tree: &Tree, node: Id) -> bool {
    if tree.parent(node).is_some() && !is_parent_function_or_class(tree, node) {
        return false;
    }
    if !tree.prefix(node).trim().is_empty() {
        return false;
    }
    let ch = tree.children(node);
    if ch.len() != 4
        || tree.ty(ch[0]) != NEWLINE
        || tree.ty(ch[1]) != INDENT
        || tree.ty(ch[3]) != DEDENT
    {
        return false;
    }
    if !tree.prefix(ch[3]).trim().is_empty() {
        return false;
    }
    is_stub_body(tree, ch[2])
}

pub fn is_stub_body(tree: &Tree, node: Id) -> bool {
    if tree.is_leaf(node) || tree.ty(node) != SIMPLE_STMT {
        return false;
    }
    let ch = tree.children(node);
    if ch.len() != 2 {
        return false;
    }
    let child = ch[0];
    tree.prefix(child).trim().is_empty()
        && tree.ty(child) == ATOM
        && tree.nchildren(child) == 3
        && tree
            .children(child)
            .iter()
            .all(|l| tree.ty(*l) == DOT && tree.value(*l) == ".")
}

pub fn is_atom_with_invisible_parens(tree: &Tree, node: Id) -> bool {
    if tree.is_leaf(node) || tree.ty(node) != ATOM {
        return false;
    }
    let ch = tree.children(node);
    let (first, last) = (ch[0], ch[ch.len() - 1]);
    tree.is_leaf(first)
        && tree.ty(first) == LPAR
        && tree.value(first).is_empty()
        && tree.is_leaf(last)
        && tree.ty(last) == RPAR
        && tree.value(last).is_empty()
}

pub fn is_import(tree: &Tree, leaf: Id) -> bool {
    let p = tree.parent(leaf);
    let t = tree.ty(leaf);
    let v = tree.value(leaf);
    t == NAME
        && ((v == "import" && p.map(|x| tree.ty(x)) == Some(IMPORT_NAME))
            || (v == "from" && p.map(|x| tree.ty(x)) == Some(IMPORT_FROM)))
}

pub fn is_with_or_async_with_stmt(tree: &Tree, leaf: Id) -> bool {
    (tree.ty(leaf) == NAME && tree.value(leaf) == "with" && tree.parent_ty(leaf) == Some(WITH_STMT))
        || (tree.ty(leaf) == ASYNC
            && tree.next_sibling(leaf).map(|x| tree.ty(x)) == Some(WITH_STMT))
}

pub fn is_async_stmt_or_funcdef(tree: &Tree, leaf: Id) -> bool {
    tree.ty(leaf) == ASYNC && matches!(tree.parent_ty(leaf), Some(ASYNC_STMT) | Some(ASYNC_FUNCDEF))
}

pub fn is_type_comment_string(value: &str) -> bool {
    value.starts_with('#') && value[1..].trim_start().starts_with("type:")
}

pub fn is_type_ignore_comment_string(value: &str) -> bool {
    is_type_comment_string(value)
        && value
            .split_once(':')
            .map(|(_, rest)| rest.trim_start().starts_with("ignore"))
            .unwrap_or(false)
}

pub fn is_type_comment(tree: &Tree, leaf: Id) -> bool {
    matches!(tree.ty(leaf), COMMENT | STANDALONE_COMMENT)
        && is_type_comment_string(tree.value(leaf))
}

pub fn is_type_ignore_comment(tree: &Tree, leaf: Id) -> bool {
    matches!(tree.ty(leaf), COMMENT | STANDALONE_COMMENT)
        && is_type_ignore_comment_string(tree.value(leaf))
}

/// Wraps `child` (a child of `parent`) in an atom with (in)visible parentheses.
pub fn wrap_in_parentheses(
    tree: &mut Tree,
    parent: Id,
    child: Id,
    visible: bool,
    index: Option<usize>,
) {
    let lpar = tree.leaf(LPAR, if visible { "(" } else { "" });
    let rpar = tree.leaf(RPAR, if visible { ")" } else { "" });
    let prefix = tree.prefix(child).to_string();
    tree.set_prefix(child, "");
    match index {
        None => {
            let idx = tree.remove(child).unwrap_or(0);
            let new_child = tree.node(ATOM, vec![lpar, child, rpar]);
            tree.set_prefix(new_child, prefix);
            tree.insert_child(parent, idx, new_child);
        }
        Some(idx) => {
            tree.nm(child).parent = None;
            let new_child = tree.node(ATOM, vec![lpar, child, rpar]);
            tree.set_prefix(new_child, prefix);
            tree.set_child(parent, idx, new_child);
            tree.nm(child).parent = Some(new_child);
            tree.nm(child).index = 1;
        }
    }
}

pub fn ensure_visible(tree: &mut Tree, leaf: Id) {
    match tree.ty(leaf) {
        LPAR => tree.set_value(leaf, "("),
        RPAR => tree.set_value(leaf, ")"),
        _ => {}
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum AnnotationType {
    Return,
    Param,
}

pub fn get_annotation_type(tree: &Tree, leaf: Id) -> Option<AnnotationType> {
    let mut ancestor = tree.parent(leaf);
    while let Some(a) = ancestor {
        if tree.prev_sibling(a).map(|x| tree.ty(x)) == Some(RARROW) {
            return Some(AnnotationType::Return);
        }
        if let Some(ap) = tree.parent(a) {
            if tree.ty(ap) == TNAME {
                return Some(AnnotationType::Param);
            }
        }
        ancestor = tree.parent(a);
    }
    None
}

pub fn is_part_of_annotation(tree: &Tree, leaf: Id) -> bool {
    get_annotation_type(tree, leaf).is_some()
}

pub fn has_sibling_with_type(tree: &Tree, node: Id, ty: u16) -> bool {
    let Some(p) = tree.parent(node) else {
        return false;
    };
    tree.children(p)
        .iter()
        .any(|c| *c != node && tree.ty(*c) == ty)
}
