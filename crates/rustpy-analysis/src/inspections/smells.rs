//! Code smells and semantic errors from the "Python" inspection group:
//! redeclarations, unreachable code, shadowed builtins, simplifiable
//! expressions, mutable defaults, method parameter problems, misplaced
//! `return`/`yield`/`await`/`break`, argument and parameter list errors,
//! duplicate dict keys, and more.

use std::collections::HashSet;
use std::sync::Arc;

use rustpy_core::{Severity, TextRange};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};
use smol_str::SmolStr;

use super::{body_insert_point, full_lines, indent_at, line_start, ts, QuickFix, Sink};
use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::infer::Inferer;
use rustpy_semantic::scope::{BindingKind, ScopeKind};
use rustpy_semantic::types::Type;

pub(super) fn run(inf: &Inferer<'_>, file: &Arc<FileInfo>, sink: &mut Sink) {
    let db = inf.db;
    let text: &str = &file.text;
    let root = file.parse.syntax_node();
    redeclarations(file, &root, sink);
    shadowing_builtins(db, file, sink);
    future_imports(file, sink);
    for node in root.descendants() {
        match node.kind() {
            SyntaxKind::BLOCK | SyntaxKind::MODULE => unreachable_code(&node, sink),
            SyntaxKind::GLOBAL_STMT => global_stmt(file, text, &node, sink),
            SyntaxKind::NONLOCAL_STMT => nonlocal_stmt(file, &node, sink),
            SyntaxKind::NOT_EXPR => negated_in(text, &node, sink),
            SyntaxKind::BOOL_OP_EXPR => chained_comparison(text, &node, sink),
            SyntaxKind::PAREN_EXPR => redundant_parens(text, &node, sink),
            SyntaxKind::COMPARE_EXPR => {
                len_zero(text, &node, sink);
                is_literal(&node, sink);
            }
            SyntaxKind::CALL_EXPR => comprehension_in_call(text, &node, sink),
            SyntaxKind::PARAM_LIST => param_list(&node, sink),
            SyntaxKind::ARG_LIST => arg_list(&node, sink),
            SyntaxKind::EXCEPT_CLAUSE => bare_except(&node, sink),
            SyntaxKind::ASSERT_STMT => assert_tuple(text, &node, sink),
            SyntaxKind::FUNC_DEF => method_checks(text, &node, sink),
            SyntaxKind::CLASS_DEF => missing_super_init(inf, file, text, &node, sink),
            SyntaxKind::DICT_EXPR => dict_duplicates(&node, sink),
            SyntaxKind::SET_EXPR => set_duplicates(&node, sink),
            SyntaxKind::IF_STMT | SyntaxKind::WHILE_STMT | SyntaxKind::ELIF_CLAUSE => {
                function_in_condition(file, &node, sink)
            }
            SyntaxKind::EXPR_STMT => statement_effect(text, &node, sink),
            SyntaxKind::RAISE_STMT => raise_not_implemented(&node, sink),
            SyntaxKind::YIELD_EXPR | SyntaxKind::YIELD_FROM_EXPR => yield_outside(&node, sink),
            SyntaxKind::RETURN_STMT => return_outside(&node, sink),
            SyntaxKind::AWAIT_EXPR => await_outside(&node, sink),
            SyntaxKind::BREAK_STMT | SyntaxKind::CONTINUE_STMT => loop_control(&node, sink),
            _ => {}
        }
    }
}

fn node_text(text: &str, n: &SyntaxNode) -> String {
    text[usize::from(n.text_range().start())..usize::from(n.text_range().end())].to_string()
}

fn expr_text(text: &str, e: &ast::Expr) -> String {
    node_text(text, e.syntax())
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn single_compare(c: &ast::CompareExpr) -> Option<(ast::Expr, String, ast::Expr)> {
    let ops: Vec<ast::CompareOp> = c.ops().collect();
    let operands: Vec<ast::Expr> = c.operands().collect();
    if ops.len() != 1 || operands.len() != 2 {
        return None;
    }
    Some((operands[0].clone(), ops[0].text(), operands[1].clone()))
}

fn decorator_names(f: &ast::FuncDef) -> Vec<String> {
    f.decorators().filter_map(|d| d.name_text()).collect()
}

// ---------------------------------------------------------------------------
// Scope-level checks

/// `def f` / `class C` defined twice in the same block without a use in between.
fn redeclarations(file: &Arc<FileInfo>, root: &SyntaxNode, sink: &mut Sink) {
    for scope in &file.scopes.scopes {
        for (name, ids) in &scope.bindings {
            let defs: Vec<_> = ids
                .iter()
                .map(|id| file.scopes.binding(*id))
                .filter(|b| matches!(b.kind, BindingKind::Function | BindingKind::Class))
                .collect();
            for pair in defs.windows(2) {
                let (b1, b2) = (pair[0], pair[1]);
                let n1 = rustpy_semantic::edit_helpers::node_with_range(root, b1.node_range);
                let n2 = rustpy_semantic::edit_helpers::node_with_range(root, b2.node_range);
                let (Some(n1), Some(n2)) = (n1, n2) else {
                    continue;
                };
                if n1.parent() != n2.parent() {
                    continue;
                }
                let skip = |n: &SyntaxNode| {
                    ast::FuncDef::cast(n.clone())
                        .map(|f| decorator_names(&f))
                        .unwrap_or_default()
                        .iter()
                        .any(|d| {
                            d.ends_with(".setter")
                                || d.ends_with(".getter")
                                || d.ends_with(".deleter")
                                || d.ends_with("overload")
                                || d.ends_with("register")
                                || d.contains("dispatch")
                        })
                };
                if skip(&n1) || skip(&n2) {
                    continue;
                }
                let between = TextRange::new(b1.node_range.end(), b2.node_range.start());
                let used = file.scopes.references.iter().any(|r| {
                    r.resolved.is_some_and(|rid| ids.contains(&rid))
                        && between.contains_range(r.range)
                });
                if used {
                    continue;
                }
                sink.push(
                    b2.range,
                    Severity::Warning,
                    "PyRedeclaration",
                    format!("Redeclared '{name}' defined above without usage"),
                    Vec::new(),
                );
            }
        }
    }
}

const SITE_BUILTINS: &[&str] = &["exit", "quit", "copyright", "credits", "license", "help"];

fn shadowing_builtins(db: &Analysis, file: &Arc<FileInfo>, sink: &mut Sink) {
    let Some(builtins) = db.builtins() else {
        return;
    };
    let names: HashSet<SmolStr> = builtins
        .scopes
        .scope(builtins.scopes.module_scope())
        .bindings
        .keys()
        .cloned()
        .collect();
    let mut seen: HashSet<(rustpy_semantic::scope::ScopeId, SmolStr)> = HashSet::new();
    for b in &file.scopes.bindings {
        if !matches!(
            b.kind,
            BindingKind::Param
                | BindingKind::Assignment
                | BindingKind::Function
                | BindingKind::Class
                | BindingKind::For
                | BindingKind::With
                | BindingKind::Walrus
                | BindingKind::Annotation
        ) {
            continue;
        }
        if file.scopes.scope(b.scope).kind == ScopeKind::Class
            || b.name.starts_with('_')
            || !names.contains(&b.name)
            || SITE_BUILTINS.contains(&b.name.as_str())
        {
            continue;
        }
        if !seen.insert((b.scope, b.name.clone())) {
            continue;
        }
        sink.push(
            b.range,
            Severity::WeakWarning,
            "PyShadowingBuiltins",
            format!("Shadows built-in name '{}'", b.name),
            Vec::new(),
        );
    }
}

fn future_imports(file: &Arc<FileInfo>, sink: &mut Sink) {
    let root = file.parse.syntax_node();
    let Some(module) = ast::Module::cast(root) else {
        return;
    };
    let mut only_prelude = true;
    for (i, stmt) in module.statements().enumerate() {
        match &stmt {
            ast::Stmt::ImportFrom(f) if f.module_path() == "__future__" => {
                if !only_prelude {
                    sink.push(
                        stmt.text_range(),
                        Severity::Error,
                        "PyFromFutureImport",
                        "from __future__ imports must occur at the beginning of the file",
                        Vec::new(),
                    );
                }
            }
            ast::Stmt::Expr(e) if i == 0 && matches!(e.expr(), Some(ast::Expr::String(_))) => {}
            _ => only_prelude = false,
        }
    }
}

// ---------------------------------------------------------------------------
// Statement checks

fn unreachable_code(block: &SyntaxNode, sink: &mut Sink) {
    let stmts: Vec<SyntaxNode> = block
        .children()
        .filter(|c| ast::Stmt::can_cast(c.kind()))
        .collect();
    for (i, s) in stmts.iter().enumerate() {
        if matches!(
            s.kind(),
            SyntaxKind::RETURN_STMT
                | SyntaxKind::RAISE_STMT
                | SyntaxKind::BREAK_STMT
                | SyntaxKind::CONTINUE_STMT
        ) {
            if let (Some(next), Some(last)) = (stmts.get(i + 1), stmts.last()) {
                sink.push(
                    TextRange::new(next.text_range().start(), last.text_range().end()),
                    Severity::Warning,
                    "PyUnreachableCode",
                    "This code is unreachable",
                    Vec::new(),
                );
            }
            return;
        }
    }
}

fn global_stmt(file: &Arc<FileInfo>, text: &str, node: &SyntaxNode, sink: &mut Sink) {
    let Some(g) = ast::GlobalStmt::cast(node.clone()) else {
        return;
    };
    let scope = super::scope_of_node(file, node);
    if file.scopes.scope(scope).kind == ScopeKind::Module {
        let fix = QuickFix {
            title: "Remove 'global' statement".into(),
            edits: vec![(full_lines(text, node.text_range()), String::new())],
        };
        sink.push(
            node.text_range(),
            Severity::WeakWarning,
            "PyGlobalAtModuleLevel",
            "'global' statement at module level has no effect",
            vec![fix],
        );
        return;
    }
    let module = file.scopes.scope(file.scopes.module_scope());
    for n in g.names() {
        let defined = module.bindings.get(n.text()).is_some_and(|ids| {
            ids.iter()
                .any(|id| file.scopes.binding(*id).is_definition())
        });
        if !defined {
            sink.push(
                n.text_range(),
                Severity::Warning,
                "PyGlobalUndefined",
                format!(
                    "Global variable '{}' is undefined at the module level",
                    n.text()
                ),
                Vec::new(),
            );
        }
    }
}

fn nonlocal_stmt(file: &Arc<FileInfo>, node: &SyntaxNode, sink: &mut Sink) {
    let Some(g) = ast::NonlocalStmt::cast(node.clone()) else {
        return;
    };
    let scope = super::scope_of_node(file, node);
    let sc = file.scopes.scope(scope);
    if sc.kind == ScopeKind::Module {
        sink.push(
            node.text_range(),
            Severity::Error,
            "PyNonlocalDeclaration",
            "nonlocal declaration not allowed at module level",
            Vec::new(),
        );
        return;
    }
    for n in g.names() {
        let mut cur = sc.parent;
        let mut found = false;
        while let Some(s) = cur {
            let p = file.scopes.scope(s);
            if matches!(p.kind, ScopeKind::Function | ScopeKind::Lambda)
                && p.bindings.get(n.text()).is_some_and(|ids| {
                    ids.iter()
                        .any(|id| file.scopes.binding(*id).is_definition())
                })
            {
                found = true;
                break;
            }
            if p.kind == ScopeKind::Module {
                break;
            }
            cur = p.parent;
        }
        if !found {
            sink.push(
                n.text_range(),
                Severity::Error,
                "PyNonlocalDeclaration",
                format!("No binding for nonlocal '{}' found", n.text()),
                Vec::new(),
            );
        }
    }
}

fn yield_outside(node: &SyntaxNode, sink: &mut Sink) {
    let Some(owner) = node.ancestors().skip(1).find(|a| {
        matches!(
            a.kind(),
            SyntaxKind::FUNC_DEF
                | SyntaxKind::LAMBDA_EXPR
                | SyntaxKind::CLASS_DEF
                | SyntaxKind::MODULE
                | SyntaxKind::LIST_COMP
                | SyntaxKind::SET_COMP
                | SyntaxKind::DICT_COMP
                | SyntaxKind::GENERATOR_EXPR
        )
    }) else {
        return;
    };
    let msg = match owner.kind() {
        SyntaxKind::FUNC_DEF | SyntaxKind::LAMBDA_EXPR => return,
        SyntaxKind::LIST_COMP
        | SyntaxKind::SET_COMP
        | SyntaxKind::DICT_COMP
        | SyntaxKind::GENERATOR_EXPR => "'yield' inside comprehension",
        _ => "'yield' outside function",
    };
    sink.push(
        node.text_range(),
        Severity::Error,
        "PyYieldOutsideFunction",
        msg,
        Vec::new(),
    );
}

fn return_outside(node: &SyntaxNode, sink: &mut Sink) {
    let Some(owner) = node.ancestors().skip(1).find(|a| {
        matches!(
            a.kind(),
            SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::MODULE
        )
    }) else {
        return;
    };
    if owner.kind() != SyntaxKind::FUNC_DEF {
        sink.push(
            node.text_range(),
            Severity::Error,
            "PyReturnOutsideFunction",
            "'return' outside function",
            Vec::new(),
        );
    }
}

fn await_outside(node: &SyntaxNode, sink: &mut Sink) {
    let Some(owner) = node.ancestors().skip(1).find(|a| {
        matches!(
            a.kind(),
            SyntaxKind::FUNC_DEF
                | SyntaxKind::LAMBDA_EXPR
                | SyntaxKind::CLASS_DEF
                | SyntaxKind::MODULE
        )
    }) else {
        return;
    };
    let msg = match owner.kind() {
        SyntaxKind::FUNC_DEF => {
            if ast::FuncDef::cast(owner.clone()).is_some_and(|f| f.is_async()) {
                return;
            }
            "'await' outside async function"
        }
        SyntaxKind::LAMBDA_EXPR => "'await' outside async function",
        _ => "'await' outside function",
    };
    let kw = node
        .first_token()
        .map(|t| t.text_range())
        .unwrap_or(node.text_range());
    sink.push(kw, Severity::Error, "PyAwaitOutsideAsync", msg, Vec::new());
}

fn loop_control(node: &SyntaxNode, sink: &mut Sink) {
    let mut child = node.clone();
    for anc in node.ancestors().skip(1) {
        match anc.kind() {
            SyntaxKind::FUNC_DEF
            | SyntaxKind::LAMBDA_EXPR
            | SyntaxKind::CLASS_DEF
            | SyntaxKind::MODULE => break,
            SyntaxKind::FOR_STMT | SyntaxKind::WHILE_STMT if child.kind() == SyntaxKind::BLOCK => {
                return
            }
            _ => {}
        }
        child = anc;
    }
    let what = if node.kind() == SyntaxKind::BREAK_STMT {
        "break"
    } else {
        "continue"
    };
    sink.push(
        node.text_range(),
        Severity::Error,
        "PyLoopControlOutsideLoop",
        format!("'{what}' outside loop"),
        Vec::new(),
    );
}

fn statement_effect(text: &str, node: &SyntaxNode, sink: &mut Sink) {
    let Some(stmt) = ast::ExprStmt::cast(node.clone()) else {
        return;
    };
    let Some(expr) = stmt.expr() else { return };
    let inner = expr.clone().unparen();
    let no_effect = match &inner {
        ast::Expr::Literal(l) => l.kind() != Some(SyntaxKind::ELLIPSIS),
        ast::Expr::Name(_)
        | ast::Expr::Compare(_)
        | ast::Expr::Binary(_)
        | ast::Expr::BoolOp(_)
        | ast::Expr::Unary(_)
        | ast::Expr::Not(_)
        | ast::Expr::Attribute(_)
        | ast::Expr::Subscript(_)
        | ast::Expr::Conditional(_)
        | ast::Expr::Lambda(_)
        | ast::Expr::List(_)
        | ast::Expr::Dict(_)
        | ast::Expr::Set(_)
        | ast::Expr::Tuple(_)
        | ast::Expr::ListComp(_)
        | ast::Expr::SetComp(_)
        | ast::Expr::DictComp(_)
        | ast::Expr::Generator(_) => true,
        _ => false,
    };
    if !no_effect
        || inner.syntax().descendants().any(|d| {
            matches!(
                d.kind(),
                SyntaxKind::AWAIT_EXPR
                    | SyntaxKind::YIELD_EXPR
                    | SyntaxKind::YIELD_FROM_EXPR
                    | SyntaxKind::NAMED_EXPR
                    | SyntaxKind::CALL_EXPR
            )
        })
    {
        return;
    }
    let range = node.text_range();
    let lines = full_lines(text, range);
    let alone = text[usize::from(lines.start())..usize::from(range.start())]
        .trim()
        .is_empty();
    let fixes = if alone {
        vec![QuickFix {
            title: "Remove statement".into(),
            edits: vec![(lines, String::new())],
        }]
    } else {
        Vec::new()
    };
    sink.push(
        range,
        Severity::Warning,
        "PyStatementEffect",
        "Statement seems to have no effect",
        fixes,
    );
}

fn raise_not_implemented(node: &SyntaxNode, sink: &mut Sink) {
    let Some(r) = ast::RaiseStmt::cast(node.clone()) else {
        return;
    };
    let Some(e) = r.exception() else { return };
    let name = match e.clone().unparen() {
        ast::Expr::Name(n) => Some(n),
        ast::Expr::Call(c) => match c.callee() {
            Some(ast::Expr::Name(n)) => Some(n),
            _ => None,
        },
        _ => None,
    };
    let Some(n) = name.filter(|n| n.name_text() == "NotImplemented") else {
        return;
    };
    let fix = QuickFix {
        title: "Replace with 'NotImplementedError'".into(),
        edits: vec![(n.text_range(), "NotImplementedError".into())],
    };
    sink.push(
        n.text_range(),
        Severity::Warning,
        "PyRaiseNotImplemented",
        "'NotImplemented' is not an exception; use 'NotImplementedError'",
        vec![fix],
    );
}

fn bare_except(node: &SyntaxNode, sink: &mut Sink) {
    let Some(c) = ast::ExceptClause::cast(node.clone()) else {
        return;
    };
    if c.exception_type().is_some() || c.is_star() {
        return;
    }
    let Some(kw) = node
        .first_token()
        .filter(|t| t.kind() == SyntaxKind::EXCEPT_KW)
    else {
        return;
    };
    let fix = QuickFix {
        title: "Replace with 'except Exception:'".into(),
        edits: vec![(TextRange::empty(kw.text_range().end()), " Exception".into())],
    };
    sink.push(
        kw.text_range(),
        Severity::WeakWarning,
        "PyBroadException",
        "Too broad exception clause",
        vec![fix],
    );
}

fn assert_tuple(text: &str, node: &SyntaxNode, sink: &mut Sink) {
    let Some(a) = ast::AssertStmt::cast(node.clone()) else {
        return;
    };
    let Some(ast::Expr::Tuple(t)) = a.test() else {
        return;
    };
    let elems: Vec<ast::Expr> = t.elements().collect();
    if elems.is_empty() {
        return;
    }
    let mut fixes = Vec::new();
    if elems.len() <= 2 && a.message().is_none() {
        let repl = elems
            .iter()
            .map(|e| expr_text(text, e))
            .collect::<Vec<_>>()
            .join(", ");
        fixes.push(QuickFix {
            title: "Remove parentheses".into(),
            edits: vec![(t.text_range(), repl)],
        });
    }
    sink.push(
        t.text_range(),
        Severity::Warning,
        "PyAssertTuple",
        "Assertion is always true, perhaps remove parentheses?",
        fixes,
    );
}

// ---------------------------------------------------------------------------
// Expression checks

fn negated_in(text: &str, node: &SyntaxNode, sink: &mut Sink) {
    let Some(n) = ast::NotExpr::cast(node.clone()) else {
        return;
    };
    let Some(operand) = n.operand() else { return };
    let ast::Expr::Compare(c) = operand.unparen() else {
        return;
    };
    let Some((l, op, r)) = single_compare(&c) else {
        return;
    };
    let new_op = match op.as_str() {
        "in" => "not in",
        "is" => "is not",
        _ => return,
    };
    let repl = format!("{} {new_op} {}", expr_text(text, &l), expr_text(text, &r));
    let fix = QuickFix {
        title: format!("Replace with '{repl}'"),
        edits: vec![(node.text_range(), repl.clone())],
    };
    sink.push(
        node.text_range(),
        Severity::WeakWarning,
        "PyNegatedIn",
        format!(
            "'{}' can be simplified to '{repl}'",
            norm(&node_text(text, node))
        ),
        vec![fix],
    );
}

fn chained_comparison(text: &str, node: &SyntaxNode, sink: &mut Sink) {
    let Some(b) = ast::BoolOpExpr::cast(node.clone()) else {
        return;
    };
    if b.is_or() {
        return;
    }
    let operands: Vec<ast::Expr> = b.operands().collect();
    if operands.len() != 2 {
        return;
    }
    let (ast::Expr::Compare(c1), ast::Expr::Compare(c2)) =
        (operands[0].clone().unparen(), operands[1].clone().unparen())
    else {
        return;
    };
    let (Some((a, op1, b1)), Some((b2, op2, c))) = (single_compare(&c1), single_compare(&c2))
    else {
        return;
    };
    let lt = |o: &str| matches!(o, "<" | "<=");
    let gt = |o: &str| matches!(o, ">" | ">=");
    if !((lt(&op1) && lt(&op2)) || (gt(&op1) && gt(&op2))) {
        return;
    }
    if norm(&expr_text(text, &b1)) != norm(&expr_text(text, &b2)) {
        return;
    }
    let repl = format!(
        "{} {op1} {} {op2} {}",
        expr_text(text, &a),
        expr_text(text, &b1),
        expr_text(text, &c)
    );
    let fix = QuickFix {
        title: format!("Replace with '{repl}'"),
        edits: vec![(node.text_range(), repl)],
    };
    sink.push(
        node.text_range(),
        Severity::WeakWarning,
        "PyChainedComparisons",
        "Simplify chained comparison",
        vec![fix],
    );
}

fn redundant_parens(text: &str, node: &SyntaxNode, sink: &mut Sink) {
    let Some(p) = ast::ParenExpr::cast(node.clone()) else {
        return;
    };
    let Some(inner) = p.inner() else { return };
    if !matches!(
        inner,
        ast::Expr::Name(_)
            | ast::Expr::Literal(_)
            | ast::Expr::String(_)
            | ast::Expr::FString(_)
            | ast::Expr::Attribute(_)
            | ast::Expr::Call(_)
            | ast::Expr::Subscript(_)
            | ast::Expr::List(_)
            | ast::Expr::Dict(_)
            | ast::Expr::Set(_)
            | ast::Expr::ListComp(_)
            | ast::Expr::SetComp(_)
            | ast::Expr::DictComp(_)
    ) {
        return;
    }
    if node_text(text, node).contains('\n')
        || node
            .ancestors()
            .any(|a| a.kind() == SyntaxKind::FSTRING_REPLACEMENT)
    {
        return;
    }
    let Some(parent) = node.parent() else { return };
    let ok = match parent.kind() {
        SyntaxKind::IF_STMT | SyntaxKind::WHILE_STMT | SyntaxKind::ELIF_CLAUSE => {
            parent
                .children()
                .find(|c| ast::Expr::can_cast(c.kind()))
                .as_ref()
                == Some(node)
        }
        SyntaxKind::RETURN_STMT
        | SyntaxKind::EXPR_STMT
        | SyntaxKind::ARG
        | SyntaxKind::KEYWORD_ARG
        | SyntaxKind::PAREN_EXPR
        | SyntaxKind::ASSERT_STMT => true,
        SyntaxKind::ASSIGN_STMT | SyntaxKind::AUG_ASSIGN_STMT => {
            parent.children().last().as_ref() == Some(node)
        }
        SyntaxKind::FOR_STMT => ast::ForStmt::cast(parent.clone())
            .and_then(|f| f.iter())
            .is_some_and(|i| i.syntax() == node),
        SyntaxKind::WITH_ITEM => ast::WithItem::cast(parent.clone())
            .and_then(|w| w.context_expr())
            .is_some_and(|i| i.syntax() == node),
        _ => false,
    };
    if !ok {
        return;
    }
    let start: usize = node.text_range().start().into();
    let prev_is_word = text[..start]
        .chars()
        .last()
        .is_some_and(|c| c.is_alphanumeric() || c == '_');
    let repl = format!(
        "{}{}",
        if prev_is_word { " " } else { "" },
        expr_text(text, &inner)
    );
    let fix = QuickFix {
        title: "Remove redundant parentheses".into(),
        edits: vec![(node.text_range(), repl)],
    };
    sink.push(
        node.text_range(),
        Severity::WeakWarning,
        "PyRedundantParentheses",
        "Redundant parentheses",
        vec![fix],
    );
}

fn len_zero(text: &str, node: &SyntaxNode, sink: &mut Sink) {
    let Some(c) = ast::CompareExpr::cast(node.clone()) else {
        return;
    };
    let Some((l, op, r)) = single_compare(&c) else {
        return;
    };
    let ast::Expr::Call(call) = l.clone().unparen() else {
        return;
    };
    if !matches!(call.callee(), Some(ast::Expr::Name(n)) if n.name_text() == "len") {
        return;
    }
    let Some(args) = call.arg_list() else { return };
    let all: Vec<ast::AnyArg> = args.args().collect();
    if all.len() != 1 || !matches!(all[0], ast::AnyArg::Positional(_)) {
        return;
    }
    let Some(arg) = all[0].value() else { return };
    let rhs = match r.clone().unparen() {
        ast::Expr::Literal(l) if l.kind() == Some(SyntaxKind::INT_NUMBER) => {
            l.token().map(|t| t.text().to_string()).unwrap_or_default()
        }
        _ => return,
    };
    let negate = match (op.as_str(), rhs.as_str()) {
        ("==", "0") | ("<=", "0") | ("<", "1") => true,
        ("!=", "0") | (">", "0") | (">=", "1") => false,
        _ => return,
    };
    // Only in boolean contexts.
    let mut parent = node.parent();
    while let Some(p) = &parent {
        if p.kind() != SyntaxKind::PAREN_EXPR {
            break;
        }
        parent = p.parent();
    }
    let Some(p) = parent else { return };
    if !matches!(
        p.kind(),
        SyntaxKind::IF_STMT
            | SyntaxKind::WHILE_STMT
            | SyntaxKind::ELIF_CLAUSE
            | SyntaxKind::CONDITIONAL_EXPR
            | SyntaxKind::COMP_IF
            | SyntaxKind::NOT_EXPR
            | SyntaxKind::BOOL_OP_EXPR
            | SyntaxKind::ASSERT_STMT
    ) {
        return;
    }
    let x = expr_text(text, &arg);
    let repl = if negate { format!("not {x}") } else { x };
    let fix = QuickFix {
        title: format!("Replace with '{repl}'"),
        edits: vec![(node.text_range(), repl.clone())],
    };
    sink.push(
        node.text_range(),
        Severity::WeakWarning,
        "PyLenZeroCheck",
        format!(
            "'{}' can be simplified to '{repl}'",
            norm(&node_text(text, node))
        ),
        vec![fix],
    );
}

fn is_literal(node: &SyntaxNode, sink: &mut Sink) {
    let Some(c) = ast::CompareExpr::cast(node.clone()) else {
        return;
    };
    let operands: Vec<ast::Expr> = c.operands().collect();
    let is_lit = |e: &ast::Expr| match e.clone().unparen() {
        ast::Expr::Literal(l) => matches!(
            l.kind(),
            Some(SyntaxKind::INT_NUMBER | SyntaxKind::FLOAT_NUMBER | SyntaxKind::COMPLEX_NUMBER)
        ),
        ast::Expr::String(_) | ast::Expr::ConcatString(_) | ast::Expr::FString(_) => true,
        _ => false,
    };
    for (i, op) in c.ops().enumerate() {
        let t = op.text();
        if t != "is" && t != "is not" {
            continue;
        }
        let (Some(l), Some(r)) = (operands.get(i), operands.get(i + 1)) else {
            continue;
        };
        if is_lit(l) || is_lit(r) {
            let repl = if t == "is" { "==" } else { "!=" };
            let fix = QuickFix {
                title: format!("Replace with '{repl}'"),
                edits: vec![(op.text_range(), repl.into())],
            };
            sink.push(
                op.text_range(),
                Severity::Warning,
                "PyIsLiteral",
                format!("'{t}' with a literal; did you mean '{repl}'?"),
                vec![fix],
            );
        }
    }
}

fn comprehension_in_call(text: &str, node: &SyntaxNode, sink: &mut Sink) {
    let Some(call) = ast::CallExpr::cast(node.clone()) else {
        return;
    };
    let Some(ast::Expr::Name(n)) = call.callee() else {
        return;
    };
    let fn_name = n.name_text();
    if !matches!(fn_name.as_str(), "list" | "set" | "dict") {
        return;
    }
    let Some(args) = call.arg_list() else { return };
    let all: Vec<ast::AnyArg> = args.args().collect();
    if all.len() != 1 {
        return;
    }
    let arg = match &all[0] {
        ast::AnyArg::Positional(p) => p.expr().map(|e| e.unparen()),
        ast::AnyArg::Generator(g) => Some(ast::Expr::Generator(g.clone())),
        _ => None,
    };
    let Some(arg) = arg else { return };
    let gen_body = |g: &ast::GeneratorExpr| {
        let t = expr_text(text, &ast::Expr::Generator(g.clone()));
        let t = t.trim();
        if t.starts_with('(') && t.ends_with(')') {
            t[1..t.len() - 1].trim().to_string()
        } else {
            t.to_string()
        }
    };
    let repl = match (fn_name.as_str(), &arg) {
        ("list", ast::Expr::ListComp(_))
        | ("set", ast::Expr::SetComp(_))
        | ("dict", ast::Expr::DictComp(_)) => expr_text(text, &arg),
        ("list", ast::Expr::Generator(g)) => format!("[{}]", gen_body(g)),
        ("set", ast::Expr::Generator(g)) => format!("{{{}}}", gen_body(g)),
        ("set", ast::Expr::ListComp(_)) => {
            let t = expr_text(text, &arg);
            format!(
                "{{{}}}",
                t.trim().trim_start_matches('[').trim_end_matches(']')
            )
        }
        _ => return,
    };
    let fix = QuickFix {
        title: "Replace with comprehension".into(),
        edits: vec![(node.text_range(), repl)],
    };
    sink.push(
        n.text_range(),
        Severity::WeakWarning,
        "PyComprehensionInCollectionCall",
        format!("Unnecessary '{fn_name}()' call around a comprehension"),
        vec![fix],
    );
}

fn function_in_condition(file: &Arc<FileInfo>, node: &SyntaxNode, sink: &mut Sink) {
    let cond = match node.kind() {
        SyntaxKind::IF_STMT => ast::IfStmt::cast(node.clone()).and_then(|i| i.condition()),
        SyntaxKind::WHILE_STMT => ast::WhileStmt::cast(node.clone()).and_then(|w| w.condition()),
        _ => ast::ElifClause::cast(node.clone()).and_then(|e| e.condition()),
    };
    let Some(cond) = cond else { return };
    let mut names: Vec<ast::NameExpr> = Vec::new();
    fn collect(e: &ast::Expr, out: &mut Vec<ast::NameExpr>) {
        match e.clone().unparen() {
            ast::Expr::Name(n) => out.push(n),
            ast::Expr::BoolOp(b) => b.operands().for_each(|o| collect(&o, out)),
            ast::Expr::Not(n) => {
                if let Some(x) = n.operand() {
                    collect(&x, out);
                }
            }
            _ => {}
        }
    }
    collect(&cond, &mut names);
    for n in names {
        let Some(r) = file.scopes.reference_at(n.text_range().start()) else {
            continue;
        };
        let Some(bid) = r.resolved else { continue };
        let b = file.scopes.binding(bid);
        if b.kind != BindingKind::Function || file.scopes.bindings_in(b.scope, &b.name).len() != 1 {
            continue;
        }
        sink.push(
            n.text_range(),
            Severity::WeakWarning,
            "PyFunctionInCondition",
            format!(
                "Function '{}' is always true in a condition; did you mean to call it?",
                b.name
            ),
            Vec::new(),
        );
    }
}

fn literal_key(e: &ast::Expr) -> Option<String> {
    match e.clone().unparen() {
        ast::Expr::Literal(l) => {
            let t = l.token()?;
            match t.kind() {
                SyntaxKind::INT_NUMBER | SyntaxKind::FLOAT_NUMBER | SyntaxKind::COMPLEX_NUMBER => {
                    Some(format!("n:{}", t.text().replace('_', "").to_lowercase()))
                }
                SyntaxKind::TRUE_KW | SyntaxKind::FALSE_KW | SyntaxKind::NONE_KW => {
                    Some(format!("k:{}", t.text()))
                }
                _ => None,
            }
        }
        ast::Expr::String(s) => Some(format!("s:{}:{}", s.is_bytes(), s.value())),
        ast::Expr::Unary(u) => {
            let op = u.op()?.text().to_string();
            let inner = literal_key(&u.operand()?)?;
            Some(format!("{op}{inner}"))
        }
        _ => None,
    }
}

fn dict_duplicates(node: &SyntaxNode, sink: &mut Sink) {
    let Some(d) = ast::DictExpr::cast(node.clone()) else {
        return;
    };
    let mut seen: HashSet<String> = HashSet::new();
    for item in d.items() {
        let Some(k) = item.key() else { continue };
        let Some(key) = literal_key(&k) else { continue };
        if !seen.insert(key) {
            sink.push(
                k.text_range(),
                Severity::Warning,
                "PyDictDuplicateKeys",
                format!("Dictionary contains duplicate keys '{}'", k.syntax().text()),
                Vec::new(),
            );
        }
    }
}

fn set_duplicates(node: &SyntaxNode, sink: &mut Sink) {
    let Some(s) = ast::SetExpr::cast(node.clone()) else {
        return;
    };
    let mut seen: HashSet<String> = HashSet::new();
    for e in s.elements() {
        let Some(key) = literal_key(&e) else { continue };
        if !seen.insert(key) {
            sink.push(
                e.text_range(),
                Severity::WeakWarning,
                "PySetDuplicateItems",
                format!("Set contains duplicate items '{}'", e.syntax().text()),
                Vec::new(),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter and argument lists

fn param_list(node: &SyntaxNode, sink: &mut Sink) {
    let mut seen_default = false;
    let mut after_star = false;
    let mut bare_star: Option<SyntaxNode> = None;
    let mut names: HashSet<String> = HashSet::new();
    let mut params_after_bare_star = 0usize;
    for c in node.children() {
        match c.kind() {
            SyntaxKind::PARAM => {
                let Some(p) = ast::Param::cast(c.clone()) else {
                    continue;
                };
                let name = p.name_text();
                if !names.insert(name.clone()) {
                    sink.push(
                        p.name().map(|t| t.text_range()).unwrap_or(c.text_range()),
                        Severity::Error,
                        "PyParameterList",
                        format!("Duplicate parameter '{name}'"),
                        Vec::new(),
                    );
                }
                if bare_star.is_some() {
                    params_after_bare_star += 1;
                }
                if p.default().is_some() {
                    seen_default = true;
                } else if seen_default && !after_star {
                    sink.push(
                        c.text_range(),
                        Severity::Error,
                        "PyParameterList",
                        "Non-default parameter follows default parameter",
                        Vec::new(),
                    );
                }
            }
            SyntaxKind::STAR_PARAM | SyntaxKind::STAR_STAR_PARAM => {
                after_star = true;
                if let Some(t) = c
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .find(|t| t.kind() == SyntaxKind::IDENT)
                {
                    if !names.insert(t.text().to_string()) {
                        sink.push(
                            t.text_range(),
                            Severity::Error,
                            "PyParameterList",
                            format!("Duplicate parameter '{}'", t.text()),
                            Vec::new(),
                        );
                    }
                }
            }
            SyntaxKind::STAR_SEPARATOR => {
                after_star = true;
                bare_star = Some(c.clone());
            }
            _ => {}
        }
    }
    if let Some(star) = bare_star {
        if params_after_bare_star == 0 {
            sink.push(
                star.text_range(),
                Severity::Error,
                "PyParameterList",
                "Named arguments must follow bare '*'",
                Vec::new(),
            );
        }
    }
}

fn arg_list(node: &SyntaxNode, sink: &mut Sink) {
    let mut seen_keyword = false;
    let mut seen_star_star = false;
    let mut names: HashSet<String> = HashSet::new();
    for c in node.children() {
        match c.kind() {
            SyntaxKind::ARG => {
                if seen_star_star {
                    sink.push(
                        c.text_range(),
                        Severity::Error,
                        "PyArgumentList",
                        "Positional argument after keyword argument unpacking",
                        Vec::new(),
                    );
                } else if seen_keyword {
                    sink.push(
                        c.text_range(),
                        Severity::Error,
                        "PyArgumentList",
                        "Positional argument after keyword argument",
                        Vec::new(),
                    );
                }
            }
            SyntaxKind::KEYWORD_ARG => {
                seen_keyword = true;
                if let Some(k) = ast::KeywordArg::cast(c.clone()) {
                    let name = k.name_text();
                    if !names.insert(name.clone()) {
                        sink.push(
                            k.name().map(|t| t.text_range()).unwrap_or(c.text_range()),
                            Severity::Error,
                            "PyArgumentList",
                            format!("Keyword argument repeated: '{name}'"),
                            Vec::new(),
                        );
                    }
                }
            }
            SyntaxKind::STAR_ARG => {
                if seen_star_star {
                    sink.push(
                        c.text_range(),
                        Severity::Error,
                        "PyArgumentList",
                        "Iterable argument unpacking follows keyword argument unpacking",
                        Vec::new(),
                    );
                }
            }
            SyntaxKind::STAR_STAR_ARG => seen_star_star = true,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Methods and classes

fn mutable_default(e: &ast::Expr) -> bool {
    match e.clone().unparen() {
        ast::Expr::List(_)
        | ast::Expr::Dict(_)
        | ast::Expr::Set(_)
        | ast::Expr::ListComp(_)
        | ast::Expr::DictComp(_)
        | ast::Expr::SetComp(_) => true,
        ast::Expr::Call(c) => match c.callee() {
            Some(ast::Expr::Name(n)) => matches!(
                n.name_text().as_str(),
                "list"
                    | "dict"
                    | "set"
                    | "bytearray"
                    | "deque"
                    | "defaultdict"
                    | "OrderedDict"
                    | "Counter"
            ),
            Some(ast::Expr::Attribute(a)) => matches!(
                a.attr_text().as_str(),
                "deque" | "defaultdict" | "OrderedDict" | "Counter"
            ),
            _ => false,
        },
        _ => false,
    }
}

fn method_checks(text: &str, node: &SyntaxNode, sink: &mut Sink) {
    let Some(f) = ast::FuncDef::cast(node.clone()) else {
        return;
    };
    let name = f.name_text();
    let own = |n: &SyntaxNode| {
        !n.ancestors().take_while(|a| a != node).any(|a| {
            matches!(
                a.kind(),
                SyntaxKind::FUNC_DEF | SyntaxKind::LAMBDA_EXPR | SyntaxKind::CLASS_DEF
            )
        })
    };
    // Mutable default arguments (any function).
    if let Some(params) = f.params() {
        for p in params.plain_params() {
            let Some(d) = p.default() else { continue };
            if !mutable_default(&d) {
                continue;
            }
            let mut fixes = Vec::new();
            if let Some((at, indent)) = f.body().and_then(|b| body_insert_point(text, &b)) {
                let pname = p.name_text();
                let init = format!(
                    "{indent}if {pname} is None:\n{indent}    {pname} = {}\n",
                    expr_text(text, &d)
                );
                fixes.push(QuickFix {
                    title: "Replace with None and initialize in the body".into(),
                    edits: vec![
                        (d.text_range(), "None".into()),
                        (TextRange::empty(ts(at)), init),
                    ],
                });
            }
            sink.push(
                d.text_range(),
                Severity::Warning,
                "PyDefaultArgument",
                "Default argument value is mutable",
                fixes,
            );
        }
    }
    if !f.is_method() {
        return;
    }
    let decos = decorator_names(&f);
    let is_static = decos.iter().any(|d| d == "staticmethod");
    let is_class_method = decos.iter().any(|d| d == "classmethod");
    let params: Vec<ast::AnyParam> = f.params().map(|p| p.params().collect()).unwrap_or_default();
    let first_plain = params.first().and_then(|p| match p {
        ast::AnyParam::Plain(x) => Some(x.clone()),
        _ => None,
    });
    if !is_static && params.is_empty() {
        let mut fixes = Vec::new();
        if let Some(open) = f
            .params()
            .and_then(|p| p.syntax().first_token())
            .filter(|t| t.kind() == SyntaxKind::L_PAREN)
        {
            fixes.push(QuickFix {
                title: "Add 'self' parameter".into(),
                edits: vec![(TextRange::empty(open.text_range().end()), "self".into())],
            });
        }
        let range = f
            .name()
            .map(|t| t.text_range())
            .unwrap_or(node.text_range());
        sink.push(
            range,
            Severity::Error,
            "PyMethodParameters",
            "Method must have a first parameter, usually called 'self'",
            fixes,
        );
        return;
    }
    if !is_static {
        if let Some(first) = &first_plain {
            let fname = first.name_text();
            let expected = if is_class_method
                || matches!(
                    name.as_str(),
                    "__new__" | "__init_subclass__" | "__class_getitem__"
                ) {
                "cls"
            } else {
                "self"
            };
            let ok = fname == expected
                || fname == "_"
                || matches!(fname.as_str(), "mcs" | "mcls" | "metacls" | "cls");
            if !ok {
                sink.push(
                    first.text_range(),
                    Severity::WeakWarning,
                    "PyMethodParameters",
                    format!("Usually first parameter of a method is named '{expected}'"),
                    Vec::new(),
                );
            }
        }
    }
    // __init__ returning a value.
    if name == "__init__" {
        if let Some(body) = f.body() {
            for r in body
                .syntax()
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::RETURN_STMT && own(n))
                .filter_map(ast::ReturnStmt::cast)
            {
                if r.value().is_some_and(|v| !matches!(v.unparen(), ast::Expr::Literal(l) if l.kind() == Some(SyntaxKind::NONE_KW))) {
                    sink.push(r.text_range(), Severity::Error, "PyReturnFromInit", "Cannot return a value from __init__", Vec::new());
                }
            }
        }
    }
    // Method may be static.
    if decos.is_empty() && !name.starts_with("__") {
        if let (Some(first), Some(body)) = (&first_plain, f.body()) {
            let fname = first.name_text();
            if fname == "self" {
                let uses_self = body
                    .syntax()
                    .descendants()
                    .any(|n| n.kind() == SyntaxKind::NAME_EXPR && n.text() == "self")
                    || body.syntax().descendants().any(|n| {
                        n.kind() == SyntaxKind::CALL_EXPR
                            && n.text().to_string().starts_with("super(")
                    });
                let stmts: Vec<ast::Stmt> = body.statements().filter(|s| !matches!(s, ast::Stmt::Expr(e) if matches!(e.expr(), Some(ast::Expr::String(_)) | Some(ast::Expr::Literal(_))))).collect();
                let trivial = stmts.is_empty() || stmts.iter().all(|s| matches!(s, ast::Stmt::Pass(_)) || matches!(s, ast::Stmt::Raise(r) if r.exception().is_some_and(|e| e.syntax().text().to_string().starts_with("NotImplemented"))));
                if !uses_self && !trivial {
                    let mut fixes = Vec::new();
                    let def_start: usize = node.text_range().start().into();
                    let at = line_start(text, def_start);
                    let indent = indent_at(text, def_start);
                    let next_start = params.get(1).map(|p| p.syntax().text_range().start());
                    let remove = match next_start {
                        Some(n) => TextRange::new(first.text_range().start(), n),
                        None => first.text_range(),
                    };
                    fixes.push(QuickFix {
                        title: "Make method static".into(),
                        edits: vec![
                            (TextRange::empty(ts(at)), format!("{indent}@staticmethod\n")),
                            (remove, String::new()),
                        ],
                    });
                    sink.push(
                        f.name()
                            .map(|t| t.text_range())
                            .unwrap_or(node.text_range()),
                        Severity::WeakWarning,
                        "PyMethodMayBeStatic",
                        format!("Method '{name}' may be 'static'"),
                        fixes,
                    );
                }
            }
        }
    }
}

fn missing_super_init(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    text: &str,
    node: &SyntaxNode,
    sink: &mut Sink,
) {
    let Some(class) = ast::ClassDef::cast(node.clone()) else {
        return;
    };
    let bases = class.bases();
    if bases.is_empty() {
        return;
    }
    let Some(init) = class.methods().find(|m| m.name_text() == "__init__") else {
        return;
    };
    let Some(body) = init.body() else { return };
    let body_text = node_text(text, body.syntax());
    if body_text.contains("super(") || body_text.contains(".__init__(") {
        return;
    }
    let skip_names = [
        "NamedTuple",
        "TypedDict",
        "Protocol",
        "Generic",
        "ABC",
        "object",
        "Enum",
        "IntEnum",
        "StrEnum",
        "Flag",
        "type",
    ];
    for base in &bases {
        if matches!(base, ast::Expr::Call(_) | ast::Expr::Subscript(_)) {
            continue;
        }
        let Type::Class(c, _) = inf.type_of_expr(file, base) else {
            continue;
        };
        if skip_names.contains(&c.name.as_str()) {
            continue;
        }
        let Some(info) = inf.class_info(&c) else {
            continue;
        };
        if info.is_protocol || info.is_enum_like || info.is_dataclass {
            continue;
        }
        let defines_init = inf
            .mro(&c)
            .iter()
            .take_while(|k| k.name != "object")
            .any(|k| {
                inf.class_info(k).is_some_and(|i| {
                    i.members.get("__init__").is_some_and(|m| {
                        matches!(m.kind, rustpy_semantic::classes::MemberKind::Method)
                    })
                })
            });
        if !defines_init {
            continue;
        }
        let mut fixes = Vec::new();
        if let Some((at, indent)) = body_insert_point(text, &body) {
            fixes.push(QuickFix {
                title: "Add 'super().__init__()' call".into(),
                edits: vec![(
                    TextRange::empty(ts(at)),
                    format!("{indent}super().__init__()\n"),
                )],
            });
        }
        let range = init
            .name()
            .map(|t| t.text_range())
            .unwrap_or(init.text_range());
        sink.push(
            range,
            Severity::WeakWarning,
            "PyMissingConstructor",
            "Call to __init__ of super class is missed",
            fixes,
        );
        return;
    }
}
