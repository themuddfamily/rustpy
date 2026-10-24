//! Compiler-context restrictions which the lossless, recovery-oriented parser
//! intentionally leaves to inspections. No execution or inferred types needed.
use super::Sink;
use rustpy_core::Severity;
use rustpy_semantic::{db::FileInfo, scope::BindingKind};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind as K, SyntaxNode,
};
use std::collections::HashSet;

fn error(node: &SyntaxNode, message: impl Into<String>, sink: &mut Sink) {
    sink.push(
        node.text_range(),
        Severity::Error,
        "PySyntaxError",
        message,
        Vec::new(),
    );
}

pub(super) fn run(file: &FileInfo, sink: &mut Sink) {
    for binding in &file.scopes.bindings {
        if binding.name == "__debug__" && binding.is_definition() {
            sink.push(
                binding.range,
                Severity::Error,
                "PySyntaxError",
                "Cannot bind __debug__",
                Vec::new(),
            );
        }
    }
    for node in file.parse.syntax_node().descendants() {
        match node.kind() {
            K::FOR_STMT | K::WITH_STMT | K::COMP_FOR => {
                let asynchronous = node
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .any(|t| t.kind() == K::ASYNC_KW);
                if node.kind() == K::FOR_STMT {
                    if let Some(target) = ast::ForStmt::cast(node.clone()).and_then(|s| s.target())
                    {
                        target_check(&target, false, false, sink);
                    }
                }
                if node.kind() == K::COMP_FOR {
                    if let Some(target) = ast::CompFor::cast(node.clone()).and_then(|s| s.target())
                    {
                        target_check(&target, false, false, sink);
                    }
                }
                if node.kind() == K::WITH_STMT {
                    if let Some(stmt) = ast::WithStmt::cast(node.clone()) {
                        for item in stmt.items() {
                            if let Some(target) = item.target() {
                                target_check(&target, false, false, sink);
                            }
                        }
                    }
                }
                if asynchronous && !async_owner(&node) {
                    error(&node, "'async for/with' outside async function", sink);
                }
            }
            K::TRY_STMT => except_order(&node, sink),
            K::IMPORT_FROM_STMT => future_context(&node, sink),
            K::GLOBAL_STMT | K::NONLOCAL_STMT => declaration(file, &node, sink),
            K::ASSIGN_STMT => {
                if let Some(stmt) = ast::AssignStmt::cast(node.clone()) {
                    for target in stmt.targets() {
                        target_check(&target, false, false, sink);
                    }
                }
            }
            K::ANN_ASSIGN_STMT => {
                if let Some(stmt) = ast::AnnAssignStmt::cast(node.clone()) {
                    if let Some(target) = stmt.target() {
                        target_check(&target, false, true, sink);
                    }
                }
            }
            K::AUG_ASSIGN_STMT => {
                if let Some(stmt) = ast::AugAssignStmt::cast(node.clone()) {
                    if let Some(target) = stmt.target() {
                        target_check(&target, false, true, sink);
                    }
                }
            }
            K::DEL_STMT => {
                if let Some(stmt) = ast::DelStmt::cast(node.clone()) {
                    for target in stmt.targets() {
                        target_check(&target, true, false, sink);
                    }
                }
            }
            K::NAMED_EXPR => {
                if let Some(target) = ast::NamedExpr::cast(node.clone()).and_then(|s| s.target()) {
                    if target.name_text() == "__debug__" {
                        error(&node, "Cannot assign to __debug__", sink);
                    }
                }
            }
            K::PARAM_LIST => parameter_separators(&node, sink),
            K::ARG_LIST => generator_arguments(&node, sink),
            K::MATCH_STMT => match_cases(&node, sink),
            K::STAR_EXPR | K::DOUBLE_STAR_EXPR => star_context(&node, sink),
            K::CALL_EXPR => class_cell(file, &node, sink),
            K::NAME_EXPR => {
                if let Some(name) = ast::NameExpr::cast(node.clone()) {
                    if name.name_text() == "__class__"
                        && !has_class(&node)
                        && file
                            .scopes
                            .resolve(file.scopes.scope_at(node.text_range().start()), "__class__")
                            .is_none()
                    {
                        sink.push(
                            node.text_range(),
                            Severity::Warning,
                            "PyClassCell",
                            "__class__ cell is only available in a class method",
                            Vec::new(),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}
fn async_owner(node: &SyntaxNode) -> bool {
    let owner = node.ancestors().skip(1).find(|a| {
        matches!(
            a.kind(),
            K::FUNC_DEF | K::LAMBDA_EXPR | K::CLASS_DEF | K::MODULE
        )
    });
    owner
        .and_then(ast::FuncDef::cast)
        .is_some_and(|f| f.is_async())
}
fn has_class(node: &SyntaxNode) -> bool {
    node.ancestors().skip(1).any(|n| n.kind() == K::CLASS_DEF)
}
fn except_order(node: &SyntaxNode, sink: &mut Sink) {
    let clauses: Vec<_> = node
        .children()
        .filter_map(ast::ExceptClause::cast)
        .collect();
    let mut star = None;
    for (i, c) in clauses.iter().enumerate() {
        if c.exception_type().is_none() && i + 1 < clauses.len() {
            error(c.syntax(), "Default 'except:' must be last", sink);
        }
        if c.is_star() && c.exception_type().is_none() {
            error(c.syntax(), "'except*' requires an exception type", sink);
        }
        if star.is_some_and(|s| s != c.is_star()) {
            error(
                c.syntax(),
                "Cannot mix 'except' and 'except*' in the same try",
                sink,
            );
        }
        star = Some(c.is_star());
        // Return/break/continue cannot transfer control out of an except* block.
        if c.is_star() {
            if let Some(body) = c.body() {
                for n in body.syntax().descendants().filter(|n| {
                    matches!(n.kind(), K::RETURN_STMT | K::BREAK_STMT | K::CONTINUE_STMT)
                }) {
                    if n.ancestors()
                        .take_while(|a| a != body.syntax())
                        .any(|a| matches!(a.kind(), K::FUNC_DEF | K::LAMBDA_EXPR | K::CLASS_DEF))
                    {
                        continue;
                    }
                    if matches!(n.kind(), K::BREAK_STMT | K::CONTINUE_STMT)
                        && n.ancestors()
                            .take_while(|a| a != body.syntax())
                            .any(|a| matches!(a.kind(), K::FOR_STMT | K::WHILE_STMT))
                    {
                        continue;
                    }
                    error(
                        &n,
                        "'return', 'break' and 'continue' cannot leave an except* block",
                        sink,
                    );
                }
            }
        }
    }
}
fn declaration(file: &FileInfo, node: &SyntaxNode, sink: &mut Sink) {
    let scope = file.scopes.scope_at(node.text_range().start());
    let sc = file.scopes.scope(scope);
    for token in node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == K::IDENT)
    {
        let name = token.text();
        let earlier_read = file.scopes.references.iter().any(|r| {
            r.scope == scope && r.name == name && r.range.start() < node.text_range().start()
        });
        let earlier_binding = sc.bindings.get(name).is_some_and(|ids| {
            ids.iter().any(|id| {
                let b = file.scopes.binding(*id);
                b.range.start() < node.text_range().start() && b.is_definition()
            })
        });
        if earlier_read || earlier_binding {
            error(
                node,
                format!(
                    "Name '{name}' is used or assigned prior to {} declaration",
                    if node.kind() == K::GLOBAL_STMT {
                        "global"
                    } else {
                        "nonlocal"
                    }
                ),
                sink,
            );
        }
        if sc.globals.iter().any(|n| n == name) && sc.nonlocals.iter().any(|n| n == name) {
            error(
                node,
                format!("Name '{name}' is both global and nonlocal"),
                sink,
            );
        }
        if sc.bindings.get(name).is_some_and(|ids| {
            ids.iter().any(|id| {
                matches!(
                    file.scopes.binding(*id).kind,
                    BindingKind::Param | BindingKind::StarParam | BindingKind::StarStarParam
                )
            })
        }) {
            error(
                node,
                format!("Parameter '{name}' cannot be global or nonlocal"),
                sink,
            );
        }
    }
}
fn target_check(expr: &ast::Expr, delete: bool, single: bool, sink: &mut Sink) {
    target_inner(expr, delete, single, sink, 0);
}
fn target_inner(expr: &ast::Expr, delete: bool, single: bool, sink: &mut Sink, depth: usize) {
    if depth > 128 {
        return;
    }
    let expr = expr.clone().unparen();
    match &expr {
        ast::Expr::Name(n) => {
            if n.name_text() == "__debug__" {
                error(
                    n.syntax(),
                    if delete {
                        "Cannot delete __debug__"
                    } else {
                        "Cannot assign to __debug__"
                    },
                    sink,
                );
            }
        }
        ast::Expr::Attribute(_) | ast::Expr::Subscript(_) => {}
        ast::Expr::Tuple(_) | ast::Expr::List(_) if !single => {
            let elements: Vec<_> = expr
                .syntax()
                .children()
                .filter_map(ast::Expr::cast)
                .collect();
            if elements
                .iter()
                .filter(|e| matches!(e, ast::Expr::Star(_)))
                .count()
                > 1
            {
                error(
                    expr.syntax(),
                    "Multiple starred expressions in assignment",
                    sink,
                );
            }
            for element in elements {
                match element {
                    ast::Expr::Star(ref s) if !delete => {
                        for child in s.syntax().children().filter_map(ast::Expr::cast) {
                            target_inner(&child, false, false, sink, depth + 1);
                        }
                    }
                    _ => target_inner(&element, delete, false, sink, depth + 1),
                }
            }
        }
        _ => error(
            expr.syntax(),
            if delete {
                "Cannot delete this expression"
            } else {
                "Cannot assign to this expression"
            },
            sink,
        ),
    }
}
fn parameter_separators(node: &SyntaxNode, sink: &mut Sink) {
    let mut slash = false;
    let mut star = false;
    let mut kwargs = false;
    let mut positional = 0;
    for child in node.children() {
        match child.kind() {
            K::SLASH_SEPARATOR => {
                if slash || star || positional == 0 {
                    error(
                        &child,
                        "'/' must follow positional parameters and precede '*'",
                        sink,
                    );
                }
                slash = true;
            }
            K::STAR_PARAM | K::STAR_SEPARATOR => {
                if star || kwargs {
                    error(
                        &child,
                        "Multiple or misplaced '*' parameter separators",
                        sink,
                    );
                }
                star = true;
            }
            K::STAR_STAR_PARAM => {
                if kwargs {
                    error(&child, "Multiple '**' parameters", sink);
                }
                kwargs = true;
                star = true;
            }
            K::PARAM => {
                if kwargs {
                    error(&child, "Parameters cannot follow '**' parameter", sink);
                }
                if !star {
                    positional += 1;
                }
            }
            _ => {}
        }
    }
}
fn irrefutable(node: &SyntaxNode) -> bool {
    irrefutable_inner(node, 0)
}
fn irrefutable_inner(node: &SyntaxNode, depth: usize) -> bool {
    if depth > 128 {
        return false;
    }
    match node.kind() {
        K::MATCH_CAPTURE_PATTERN | K::MATCH_WILDCARD_PATTERN => true,
        K::MATCH_GROUP_PATTERN | K::MATCH_AS_PATTERN => node
            .children()
            .find(|n| n.kind().is_pattern())
            .is_some_and(|n| irrefutable_inner(&n, depth + 1)),
        K::MATCH_OR_PATTERN => node
            .children()
            .filter(|n| n.kind().is_pattern())
            .any(|n| irrefutable_inner(&n, depth + 1)),
        _ => false,
    }
}
fn match_cases(node: &SyntaxNode, sink: &mut Sink) {
    let Some(stmt) = ast::MatchStmt::cast(node.clone()) else {
        return;
    };
    let cases: Vec<_> = stmt.cases().collect();
    for (i, case) in cases.iter().enumerate() {
        let Some(pattern) = case.pattern() else {
            continue;
        };
        if i + 1 < cases.len() && case.guard().is_none() && irrefutable(&pattern) {
            error(
                &pattern,
                "Irrefutable pattern is only allowed for the last case",
                sink,
            );
        }
        for n in pattern.descendants() {
            if n.kind() == K::MATCH_SEQUENCE_PATTERN
                && n.children()
                    .filter(|n| n.kind() == K::MATCH_STAR_PATTERN)
                    .count()
                    > 1
            {
                error(&n, "Multiple starred names in sequence pattern", sink);
            }
            if n.kind() == K::MATCH_MAPPING_PATTERN {
                let mut keys = HashSet::new();
                let mut rest = false;
                for child in n.children() {
                    if child.kind() == K::MATCH_MAPPING_REST {
                        if rest {
                            error(&child, "Multiple '**' names in mapping pattern", sink);
                        }
                        rest = true;
                    }
                    if child.kind() == K::MATCH_MAPPING_ITEM {
                        if rest {
                            error(&child, "Mapping rest pattern must be last", sink);
                        }
                        if mapping_key(&child).is_some_and(|key| !keys.insert(key)) {
                            error(&child, "Duplicate key in mapping pattern", sink);
                        }
                    }
                }
            }
            if n.kind() == K::MATCH_OR_PATTERN {
                let arms: Vec<_> = n.children().filter(|n| n.kind().is_pattern()).collect();
                for arm in arms.iter().take(arms.len().saturating_sub(1)) {
                    if irrefutable(arm) {
                        error(
                            arm,
                            "Irrefutable alternative makes remaining patterns unreachable",
                            sink,
                        );
                    }
                }
            }
        }
    }
}
fn star_context(node: &SyntaxNode, sink: &mut Sink) {
    let Some(parent) = node.parent() else { return };
    if matches!(
        parent.kind(),
        K::TUPLE_EXPR
            | K::LIST_EXPR
            | K::SET_EXPR
            | K::SLICE_LIST
            | K::TYPE_PARAM
            | K::ANNOTATION
            | K::SUBSCRIPT_EXPR
    ) {
        return;
    }
    // Starred return values require a tuple (a trailing comma), not a bare star.
    error(
        node,
        "Starred expression is not allowed in this position",
        sink,
    );
}
fn class_cell(file: &FileInfo, node: &SyntaxNode, sink: &mut Sink) {
    let Some(call) = ast::CallExpr::cast(node.clone()) else {
        return;
    };
    let Some(ast::Expr::Name(name)) = call.callee() else {
        return;
    };
    if name.name_text() != "super" || call.arg_list().is_some_and(|a| a.args().next().is_some()) {
        return;
    }
    let scope = file.scopes.scope_at(node.text_range().start());
    if file.scopes.resolve(scope, "super").is_some() {
        return;
    }
    let owner = node.ancestors().skip(1).find(|a| {
        matches!(
            a.kind(),
            K::FUNC_DEF | K::LAMBDA_EXPR | K::CLASS_DEF | K::MODULE
        )
    });
    let valid = owner.and_then(ast::FuncDef::cast).is_some_and(|f| {
        has_class(f.syntax())
            && f.params()
                .is_some_and(|p| p.plain_params().next().is_some())
    });
    if !valid {
        sink.push(
            node.text_range(),
            Severity::Warning,
            "PyClassCell",
            "Zero-argument super() requires a class method with a first argument",
            Vec::new(),
        );
    }
}

fn mapping_key(item: &SyntaxNode) -> Option<String> {
    // Numeric/singleton keys are direct tokens; strings are child nodes. Stop
    // at the key colon so captured names in the value never affect equality.
    let mut raw = String::new();
    for element in item.children_with_tokens() {
        if element.kind() == K::COLON {
            break;
        }
        if !element.kind().is_trivia() {
            raw.push_str(&element.to_string());
        }
    }
    if raw.len() > 4096 {
        return None;
    }
    if let Ok(value) = rustpy_syntax::string_literal::string(&raw) {
        return match value {
            rustpy_syntax::string_literal::Decoded::Text(value, _) => Some(format!("text:{value}")),
            rustpy_syntax::string_literal::Decoded::Bytes(value) => {
                Some(format!("bytes:{value:?}"))
            }
            rustpy_syntax::string_literal::Decoded::SurrogateText(value, _) => {
                Some(format!("surrogates:{value:?}"))
            }
        };
    }
    match raw.as_str() {
        "None" => return Some("none".into()),
        "True" => return Some("number:1".into()),
        "False" => return Some("number:0".into()),
        _ => {}
    }
    let clean = raw.replace('_', "");
    let (negative, digits) = clean
        .strip_prefix('-')
        .map_or((false, clean.as_str()), |s| (true, s));
    let (radix, digits) = if let Some(s) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (16, s)
    } else if let Some(s) = digits
        .strip_prefix("0o")
        .or_else(|| digits.strip_prefix("0O"))
    {
        (8, s)
    } else if let Some(s) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        (2, s)
    } else {
        (10, digits)
    };
    if let Ok(value) = i128::from_str_radix(digits, radix) {
        let value = if negative {
            value.checked_neg()?
        } else {
            value
        };
        return Some(format!("number:{value}"));
    }
    if let Ok(value) = clean.parse::<f64>() {
        if value.is_finite() {
            if value.fract() == 0.0 && value.abs() < 1e30 {
                return Some(format!("number:{}", value as i128));
            }
            return Some(format!("float:{value}"));
        }
    }
    None
}
fn future_context(node: &SyntaxNode, sink: &mut Sink) {
    let Some(import) = ast::ImportFromStmt::cast(node.clone()) else {
        return;
    };
    if import.module_path() != "__future__" {
        return;
    }
    if !node.parent().is_some_and(|p| p.kind() == K::MODULE) {
        error(
            node,
            "from __future__ imports must occur at module beginning",
            sink,
        );
    }
    for alias in import.aliases() {
        if !matches!(
            alias.name().as_str(),
            "nested_scopes"
                | "generators"
                | "division"
                | "absolute_import"
                | "with_statement"
                | "print_function"
                | "unicode_literals"
                | "barry_as_FLUFL"
                | "generator_stop"
                | "annotations"
        ) {
            error(
                alias.syntax(),
                format!("Future feature '{}' is not defined", alias.name()),
                sink,
            );
        }
    }
    if import.is_star() {
        error(node, "Future statements do not support import *", sink);
    }
}

fn generator_arguments(node: &SyntaxNode, sink: &mut Sink) {
    let Some(generator) = node.children().find(|n| n.kind() == K::GENERATOR_EXPR) else {
        return;
    };
    if node.children().count() > 1
        || node
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == K::COMMA)
    {
        error(
            &generator,
            "Generator expression must be parenthesized when used with other arguments or a trailing comma",
            sink,
        );
    }
}
