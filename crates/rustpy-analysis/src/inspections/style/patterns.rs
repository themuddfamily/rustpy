//! Syntax-local style suggestions. Typed boolean/type()/collection conversions
//! remain owned by advanced type-checking; builtin identities are proved here.
use super::*;
use rustpy_semantic::infer::Inferer;
fn push(
    sink: &mut Sink,
    node: &SyntaxNode,
    code: &str,
    message: &str,
    replacement: Option<String>,
) {
    sink.push(
        node.text_range(),
        Severity::WeakWarning,
        code,
        message,
        replacement
            .map(|text| QuickFix {
                title: message.into(),
                edits: vec![(node.text_range(), text)],
            })
            .into_iter()
            .collect(),
    )
}
fn builtin(file: &FileInfo, node: &SyntaxNode, name: &str) -> bool {
    file.scopes
        .resolve(scope_of_node(file, node), name)
        .is_none()
        && file
            .scopes
            .scopes
            .iter()
            .all(|scope| scope.star_imports.is_empty())
}
fn text(expr: &ast::Expr) -> String {
    expr.syntax().text().to_string()
}
fn block_lines(text: &str, block: &ast::Block) -> Option<(TextRange, String)> {
    if !block
        .syntax()
        .children_with_tokens()
        .any(|element| element.kind() == SyntaxKind::INDENT)
    {
        return None;
    }
    let first = block.statements().next()?;
    let last = block.statements().last()?;
    Some((
        TextRange::new(
            full_lines(text, first.text_range()).start(),
            full_lines(text, last.text_range()).end(),
        ),
        indent_at(text, usize::from(first.text_range().start())),
    ))
}
pub(super) fn run(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    config: &StyleInspections,
    sink: &mut Sink,
) {
    let root = file.parse.syntax_node();
    source_rules(file, config, sink);
    for node in root.descendants() {
        match node.kind() {
            SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF => {
                if let Some(parent) = node.ancestors().skip(1).find_map(ast::FuncDef::cast) {
                    let own = file.scopes.scope_for_node(node.text_range());
                    let parent_scope = file.scopes.scope_for_node(parent.text_range());
                    if let (Some(own), Some(parent_scope)) = (own, parent_scope) {
                        let captures = file
                            .scopes
                            .references
                            .iter()
                            .filter(|reference| node.text_range().contains_range(reference.range))
                            .any(|reference| {
                                reference
                                    .resolved
                                    .is_some_and(|id| file.scopes.binding(id).scope == parent_scope)
                            });
                        if !captures && own != parent_scope {
                            push(sink,&node,"PyNestedDefinition","Nested definition has no enclosing-function dependencies and can be moved to module scope",None);
                        }
                    }
                }
            }
            SyntaxKind::CALL_EXPR => {
                let Some(call) = ast::CallExpr::cast(node.clone()) else {
                    continue;
                };
                let (Some(ast::Expr::Attribute(attribute)), Some(args)) =
                    (call.callee(), call.arg_list())
                else {
                    continue;
                };
                let Some(object) = attribute.object() else {
                    continue;
                };
                let ty = inf.type_of_expr(file, &object);
                let dictionary = inf
                    .builtin_class("dict")
                    .is_some_and(|class| ty.class_ref() == Some(&class));
                if attribute.attr_text() == "get"
                    && dictionary
                    && args.args().count() == 2
                    && args.keywords().next().is_none()
                {
                    if let Some(last) = args.positional().nth(1) {
                        if last.syntax().text() == "None" {
                            let range = rustpy_semantic::edit_helpers::element_with_comma(
                                &file.text,
                                last.syntax(),
                            );
                            sink.push(
                                last.text_range(),
                                Severity::WeakWarning,
                                "PyRedundantDictDefault",
                                "dict.get default None is redundant",
                                vec![QuickFix {
                                    title: "Remove redundant None default".into(),
                                    edits: vec![(range, String::new())],
                                }],
                            );
                        }
                    }
                }
                if attribute.attr_text() == "keys" && dictionary && args.args().next().is_none() {
                    let iteration = node.parent().is_some_and(|parent| {
                        matches!(parent.kind(), SyntaxKind::FOR_STMT | SyntaxKind::COMP_FOR)
                    });
                    if iteration {
                        push(
                            sink,
                            &node,
                            "PyDictKeysIteration",
                            "Iterate over the dictionary directly",
                            Some(text(&object)),
                        );
                    }
                }
                if attribute.attr_text() == "has_key"
                    && dictionary
                    && args.args().count() == 1
                    && args.keywords().next().is_none()
                {
                    if let Some(key) = args.positional().next() {
                        push(
                            sink,
                            &node,
                            "PyDeprecatedHasKey",
                            "Replace obsolete has_key with membership",
                            Some(format!("{} in {}", text(&key), text(&object))),
                        );
                    }
                }
            }
            SyntaxKind::PASS_STMT => {
                let Some(block) = node
                    .parent()
                    .filter(|parent| parent.kind() == SyntaxKind::BLOCK)
                else {
                    continue;
                };
                let statements: Vec<_> = block.children().filter_map(ast::Stmt::cast).collect();
                if statements.len() > 1 {
                    let range = full_lines(&file.text, node.text_range());
                    let before = &file.text
                        [usize::from(range.start())..usize::from(node.text_range().start())];
                    let after =
                        &file.text[usize::from(node.text_range().end())..usize::from(range.end())];
                    if before.trim().is_empty() && after.trim().is_empty() {
                        sink.push(
                            node.text_range(),
                            Severity::WeakWarning,
                            "PyUnnecessaryPass",
                            "Unnecessary pass statement",
                            vec![QuickFix {
                                title: "Remove unnecessary pass".into(),
                                edits: vec![(range, String::new())],
                            }],
                        );
                    }
                }
            }
            SyntaxKind::IF_STMT => {
                let Some(if_) = ast::IfStmt::cast(node.clone()) else {
                    continue;
                };
                if builtin(file, &node, "bool") {
                    if let Some(action) =
                        crate::intentions::simplify_bool_return(file, node.text_range().start())
                    {
                        sink.push(
                            node.text_range(),
                            Severity::WeakWarning,
                            "PySimplifyBoolean",
                            "Boolean return expression can be simplified",
                            vec![QuickFix {
                                title: action.title,
                                edits: action.edits,
                            }],
                        );
                    }
                }
                let (Some(body), Some(else_)) = (if_.body(), if_.else_clause()) else {
                    continue;
                };
                let Some(last) = body.statements().last() else {
                    continue;
                };
                if matches!(last, ast::Stmt::Return(_) | ast::Stmt::Raise(_))
                    && if_.elif_clauses().next().is_none()
                {
                    if let Some(other) = else_.body() {
                        if let Some((range, indent)) = block_lines(&file.text, &other) {
                            let parent_indent =
                                indent_at(&file.text, usize::from(if_.text_range().start()));
                            if file.text[usize::from(else_.text_range().start())
                                ..usize::from(other.text_range().start())]
                                .contains('#')
                            {
                                push(
                                    sink,
                                    &node,
                                    "PyNoElseReturn",
                                    "Unnecessary else after terminating branch",
                                    None,
                                );
                                continue;
                            }
                            let replacement = rustpy_semantic::edit_helpers::reindent(
                                &file.text[usize::from(range.start())..usize::from(range.end())],
                                &indent,
                                &parent_indent,
                            );
                            sink.push(
                                else_.text_range(),
                                Severity::WeakWarning,
                                "PyNoElseReturn",
                                "Unnecessary else after terminating branch",
                                vec![QuickFix {
                                    title: "Remove else after return".into(),
                                    edits: vec![(
                                        full_lines(&file.text, else_.text_range()),
                                        replacement,
                                    )],
                                }],
                            );
                        }
                    }
                }
                let (Some(condition), Some(body), Some(other)) = (
                    if_.condition(),
                    if_.body(),
                    if_.else_clause().and_then(|e| e.body()),
                ) else {
                    continue;
                };
                let first = body.statements().collect::<Vec<_>>();
                let second = other.statements().collect::<Vec<_>>();
                if first.len() == 1
                    && second.len() == 1
                    && first[0].syntax().text() == second[0].syntax().text()
                    && matches!(condition, ast::Expr::Name(_) | ast::Expr::Literal(_))
                {
                    push(
                        sink,
                        &node,
                        "PyRedundantIf",
                        "Both branches are identical",
                        None,
                    );
                }
            }
            SyntaxKind::FOR_STMT => {
                let Some(for_) = ast::ForStmt::cast(node.clone()) else {
                    continue;
                };
                if let Some(action) =
                    crate::intentions::loop_to_comprehension(file, node.text_range().start())
                {
                    sink.push(
                        node.text_range(),
                        Severity::WeakWarning,
                        "PyLoopToComprehension",
                        "Loop can be replaced with a comprehension",
                        vec![QuickFix {
                            title: action.title,
                            edits: action.edits,
                        }],
                    );
                }
                let mut actions = Vec::new();
                crate::intentions::control_flow_actions::aggregates(file, &for_, &mut actions);
                for action in actions {
                    sink.push(
                        node.text_range(),
                        Severity::WeakWarning,
                        "PyLoopToComprehension",
                        "Loop can be replaced with any, all or sum",
                        vec![QuickFix {
                            title: action.title,
                            edits: action.edits,
                        }],
                    );
                }
                let (Some(ast::Expr::Name(index)), Some(ast::Expr::Call(range))) =
                    (for_.target(), for_.iter())
                else {
                    continue;
                };
                if range.callee().is_none_or(
                    |e| !matches!(e,ast::Expr::Name(ref name)if name.name_text()=="range"),
                ) || !builtin(file, &node, "range")
                    || !builtin(file, &node, "len")
                {
                    continue;
                }
                let Some(args) = range.arg_list() else {
                    continue;
                };
                if args.args().count() != 1 {
                    continue;
                }
                let Some(ast::Expr::Call(len)) = args.positional().next() else {
                    continue;
                };
                if len.callee().is_none_or(
                    |e| !matches!(e,ast::Expr::Name(ref name)if name.name_text()=="len"),
                ) {
                    continue;
                }
                let Some(args) = len.arg_list() else { continue };
                if args.args().count() != 1 {
                    continue;
                }
                let Some(ast::Expr::Name(sequence)) = args.positional().next() else {
                    continue;
                };
                let Some(body) = for_.body() else { continue };
                if body.syntax().descendants().filter_map(ast::SubscriptExpr::cast).any(|subscript|matches!((subscript.object(),subscript.index_expr()),(Some(ast::Expr::Name(object)),Some(ast::Expr::Name(slot)))if object.name_text()==sequence.name_text()&&slot.name_text()==index.name_text())){push(sink,&node,"PyEnumerate","Use enumerate instead of range(len(...))",None);}
                if let Some(ast::Stmt::Expr(statement)) = body
                    .statements()
                    .next()
                    .filter(|_| body.statements().count() == 1)
                {
                    if matches!(statement.expr(),Some(ast::Expr::Call(ref call))if matches!(call.callee(),Some(ast::Expr::Attribute(ref attr))if attr.attr_text()=="append"))
                    {
                        push(
                            sink,
                            &node,
                            "PyLoopToComprehension",
                            "Loop can be expressed as a comprehension",
                            None,
                        );
                    }
                }
            }
            SyntaxKind::LAMBDA_EXPR => {
                let Some(lambda) = ast::LambdaExpr::cast(node.clone()) else {
                    continue;
                };
                let (Some(params), Some(ast::Expr::Call(call))) = (lambda.params(), lambda.body())
                else {
                    continue;
                };
                let Some(callee) = call.callee() else {
                    continue;
                };
                if !matches!(callee, ast::Expr::Name(_)) {
                    continue;
                }
                let Some(args) = call.arg_list() else {
                    continue;
                };
                let names = params
                    .params()
                    .filter_map(|p| p.name())
                    .map(|name| name.text().to_string())
                    .collect::<Vec<_>>();
                let arguments = args.positional().collect::<Vec<_>>();
                if args.keywords().next().is_none()&&args.args().count()==names.len()&&arguments.iter().zip(&names).all(|(argument,name)|matches!(argument,ast::Expr::Name(n)if n.name_text()==*name))&&!names.iter().any(|name|text(&callee)==*name){push(sink,&node,"PyUnnecessaryLambda","Unnecessary forwarding lambda",None);}
            }
            SyntaxKind::DEL_STMT => {
                if node
                    .children()
                    .filter_map(ast::Expr::cast)
                    .any(|expr| matches!(expr, ast::Expr::Paren(_)))
                {
                    push(
                        sink,
                        &node,
                        "PySourceStyle",
                        "Unnecessary parentheses in del statement",
                        None,
                    );
                }
            }
            SyntaxKind::RAISE_STMT => {
                let mut depth = 0usize;
                let mut comma = false;
                for token in node
                    .descendants_with_tokens()
                    .filter_map(|element| element.into_token())
                {
                    match token.kind() {
                        SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_BRACE => {
                            depth += 1
                        }
                        SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_BRACE => {
                            depth = depth.saturating_sub(1)
                        }
                        SyntaxKind::COMMA if depth == 0 => comma = true,
                        _ => {}
                    }
                }
                if comma {
                    push(
                        sink,
                        &node,
                        "PyDeprecatedRaise",
                        "Deprecated comma form of raising an exception",
                        None,
                    )
                }
            }
            SyntaxKind::BINARY_EXPR => {
                let Some(binary) = ast::BinaryExpr::cast(node.clone()) else {
                    continue;
                };
                if binary.op().is_some_and(|op| op.text() == ">>")
                    && matches!(binary.lhs(),Some(ast::Expr::Name(ref name))if name.name_text()=="print")
                    && builtin(file, &node, "print")
                {
                    push(
                        sink,
                        &node,
                        "PyPrintRedirect",
                        "Python 2 print redirection; use print(..., file=...)",
                        None,
                    );
                }
            }
            _ => {}
        }
    }
}
fn source_rules(file: &Arc<FileInfo>, config: &StyleInspections, sink: &mut Sink) {
    let text = &file.text;
    let first_two = text.lines().take(2).collect::<Vec<_>>();
    let encoding = first_two.iter().any(|line| {
        line.trim_start().starts_with('#') && (line.contains("coding:") || line.contains("coding="))
    });
    if config.mandatory_encoding && !encoding {
        let offset = if text.starts_with("#!") {
            text.find('\n').map_or(text.len(), |n| n + 1)
        } else {
            0
        };
        sink.push(
            TextRange::empty(ts(0)),
            Severity::WeakWarning,
            "PySourceStyle",
            "Mandatory encoding declaration is missing",
            vec![QuickFix {
                title: "Add UTF-8 encoding declaration".into(),
                edits: vec![(
                    TextRange::empty(ts(offset)),
                    format!(
                        "{}# -*- coding: utf-8 -*-\n",
                        if offset > 0 && !text[..offset].ends_with('\n') {
                            "\n"
                        } else {
                            ""
                        }
                    ),
                )],
            }],
        );
    }
    if config.missing_shebang && !text.starts_with("#!") {
        sink.push(
            TextRange::empty(ts(0)),
            Severity::WeakWarning,
            "PySourceStyle",
            "Missing Python shebang",
            vec![QuickFix {
                title: "Add Python shebang".into(),
                edits: vec![(TextRange::empty(ts(0)), "#!/usr/bin/env python3\n".into())],
            }],
        );
    }
    let root = file.parse.syntax_node();
    for token in root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        if ((config.non_ascii_identifiers && token.kind() == SyntaxKind::IDENT)
            || (config.non_ascii_comments && token.kind() == SyntaxKind::COMMENT))
            && !token.text().is_ascii()
        {
            sink.push(
                token.text_range(),
                Severity::WeakWarning,
                "PyNonAscii",
                "Non-ASCII characters in identifier or comment",
                Vec::new(),
            );
        }
    }
    if config.quote_style != "preserve" {
        for string in root.descendants().filter_map(ast::StringExpr::cast) {
            if string.is_bytes() || string.is_raw() {
                continue;
            }
            let raw = string.raw_text();
            let quote = if config.quote_style == "single" {
                '\''
            } else {
                '"'
            };
            if raw.starts_with(quote) || raw.starts_with("\"\"\"") || raw.starts_with("'''") {
                continue;
            }
            if string
                .syntax()
                .ancestors()
                .filter_map(ast::ExprStmt::cast)
                .any(|statement| {
                    statement.syntax().parent().is_some_and(|block| {
                        block
                            .children()
                            .filter_map(ast::Stmt::cast)
                            .next()
                            .is_some_and(|first| first.text_range() == statement.text_range())
                    })
                })
            {
                continue;
            }
            let value = string.value();
            if !value.contains(quote) && !value.contains(['\n', '\r', '\\']) {
                push(
                    sink,
                    string.syntax(),
                    "PySourceStyle",
                    "Inconsistent string quotes",
                    Some(format!("{quote}{value}{quote}")),
                );
            }
        }
    }
    if config.trailing_comma {
        for node in root.descendants().filter(|node| {
            matches!(
                node.kind(),
                SyntaxKind::LIST_EXPR
                    | SyntaxKind::TUPLE_EXPR
                    | SyntaxKind::DICT_EXPR
                    | SyntaxKind::ARG_LIST
            )
        }) {
            if node
                .children()
                .any(|child| child.kind() == SyntaxKind::GENERATOR_EXPR)
            {
                continue;
            }
            if !node.text().contains_char('\n') {
                continue;
            }
            let tokens: Vec<_> = node
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| {
                    !matches!(
                        token.kind(),
                        SyntaxKind::WHITESPACE
                            | SyntaxKind::NL
                            | SyntaxKind::NEWLINE
                            | SyntaxKind::COMMENT
                    )
                })
                .collect();
            if let (Some(close), Some(previous)) = (tokens.last(), tokens.iter().rev().nth(1)) {
                if matches!(
                    close.kind(),
                    SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_BRACE
                ) && !matches!(
                    previous.kind(),
                    SyntaxKind::COMMA
                        | SyntaxKind::L_PAREN
                        | SyntaxKind::L_BRACK
                        | SyntaxKind::L_BRACE
                ) {
                    sink.push(
                        previous.text_range(),
                        Severity::WeakWarning,
                        "PySourceStyle",
                        "Missing trailing comma in multiline collection or call",
                        vec![QuickFix {
                            title: "Add trailing comma".into(),
                            edits: vec![(
                                TextRange::empty(previous.text_range().end()),
                                ",".into(),
                            )],
                        }],
                    );
                }
            }
        }
    }
}
