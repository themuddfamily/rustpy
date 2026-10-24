//! Control-flow intentions offered through the shared editor/LSP intention route.
use super::*;
use rustpy_semantic::{
    infer::Inferer,
    types::{LiteralValue, Type},
};
fn offer(out: &mut Vec<Intention>, range: TextRange, title: &str, replacement: String) {
    out.push(Intention {
        title: title.into(),
        edits: vec![(range, replacement)],
    });
}
fn text(file: &FileInfo, expr: &ast::Expr) -> String {
    slice(&file.text, expr.text_range()).to_string()
}
fn canonical(file: &FileInfo, offset: TextSize, name: &str) -> bool {
    file.scopes
        .resolve(file.scopes.scope_at(offset), name)
        .is_none()
        && file
            .scopes
            .scopes
            .iter()
            .all(|scope| scope.star_imports.is_empty())
}
fn import_name(
    file: &FileInfo,
    offset: TextSize,
    module: &str,
    name: &str,
) -> (String, Option<Edit>) {
    let scope = file.scopes.scope_at(offset);
    for (index, binding) in file.scopes.bindings.iter().enumerate() {
        if matches!(&binding.kind,BindingKind::Import(info) if info.level==0&&info.module==module&&info.name.as_deref()==Some(name))
            && binding.range.start() < offset
            && file
                .scopes
                .resolve(scope, binding.name.as_str())
                .is_some_and(|ids| ids.len() == 1 && ids[0].0 as usize == index)
        {
            return (binding.name.to_string(), None);
        }
    }
    let used = if canonical(file, offset, name) {
        name.into()
    } else {
        fresh(file, &format!("_{name}"))
    };
    let alias = if used == name {
        String::new()
    } else {
        format!(" as {used}")
    };
    (
        used,
        Some((
            TextRange::empty(rustpy_semantic::import_placement::import_insert_offset(
                file,
            )),
            format!("from {module} import {name}{alias}\n"),
        )),
    )
}
fn same(a: &ast::Expr, b: &ast::Expr) -> bool {
    a.syntax().text() == b.syntax().text()
}
fn simple(expr: &ast::Expr) -> bool {
    matches!(
        expr.clone().unparen(),
        ast::Expr::Name(_) | ast::Expr::Literal(_) | ast::Expr::String(_)
    )
}
fn primitive(db: &Analysis, file: &Arc<FileInfo>, expr: &ast::Expr, names: &[&str]) -> bool {
    let inf = Inferer::new(db);
    match inf.type_of_expr(file, expr) {
        Type::Literal(LiteralValue::Int(_)) => names.contains(&"int"),
        Type::Literal(LiteralValue::Str(_)) => names.contains(&"str"),
        Type::Literal(LiteralValue::Bool(_)) => names.contains(&"bool"),
        Type::Instance(class, _) => names.iter().any(|name| {
            inf.builtin_class(name)
                .is_some_and(|builtin| builtin == class)
        }),
        Type::Tuple(_) => names.contains(&"tuple"),
        _ => false,
    }
}
fn statement_at(file: &FileInfo, offset: TextSize) -> Option<ast::Stmt> {
    token_at(&file.parse.syntax_node(), offset)?
        .parent_ancestors()
        .find_map(ast::Stmt::cast)
}
fn isolated(file: &FileInfo, stmt: &ast::Stmt) -> bool {
    let start = usize::from(stmt.text_range().start());
    let end = usize::from(stmt.text_range().end());
    let last = file.text[end..]
        .find('\n')
        .map_or(file.text.len(), |n| end + n);
    file.text[line_start(&file.text, start)..start]
        .trim()
        .is_empty()
        && (file.text[..end].ends_with('\n') || file.text[end..last].trim().is_empty())
}
fn body_text(file: &FileInfo, body: &ast::Block, destination: &str) -> Option<String> {
    let (range, indent) = block_lines(&file.text, body)?;
    let raw_start = usize::from(body.text_range().start());
    let start = if file.text[raw_start..].starts_with("\r\n") {
        raw_start + 2
    } else if file.text[raw_start..].starts_with('\n') {
        raw_start + 1
    } else {
        line_start(&file.text, raw_start)
    };
    let start = start.min(usize::from(range.start()));
    let end = usize::from(body.text_range().end())
        .max(usize::from(range.end()))
        .min(file.text.len());
    let content = file.text[start..end].trim_end_matches([' ', '\t']);
    let mut result = reindent(content, &indent, destination);
    if !result.ends_with('\n') {
        result.push('\n');
    }
    Some(result)
}
fn rewrite_statement(
    file: &FileInfo,
    stmt: &ast::Stmt,
    out: &mut Vec<Intention>,
    title: &str,
    replacement: String,
) {
    if isolated(file, stmt) {
        offer(
            out,
            stmt_lines(&file.text, stmt.text_range()),
            title,
            replacement,
        )
    }
}
fn append_clause(
    file: &FileInfo,
    stmt: &ast::Stmt,
    title: &str,
    clause: &str,
    out: &mut Vec<Intention>,
) {
    if !isolated(file, stmt) {
        return;
    }
    let indent = indent_at(&file.text, usize::from(stmt.text_range().start()));
    let end = stmt_lines(&file.text, stmt.text_range()).end();
    offer(
        out,
        TextRange::empty(end),
        title,
        format!(
            "{}{indent}{clause}\n{indent}    pass\n",
            if file.text[..usize::from(end)].ends_with('\n') {
                ""
            } else {
                "\n"
            }
        ),
    );
}

fn branch_actions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    statement: &ast::Stmt,
    out: &mut Vec<Intention>,
) {
    let indent = indent_at(&file.text, usize::from(statement.text_range().start()));
    if let ast::Stmt::If(if_) = statement {
        if if_.else_clause().is_none() {
            append_clause(file, statement, "Add else branch", "else:", out);
            append_clause(file, statement, "Add elif branch", "elif False:", out);
        } else if let Some(else_) = if_.else_clause() {
            if isolated(file, statement) {
                let at = line_start(&file.text, usize::from(else_.text_range().start()));
                offer(
                    out,
                    TextRange::empty(TextSize::new(at as u32)),
                    "Add elif branch",
                    format!("{indent}elif False:\n{indent}    pass\n"),
                );
            }
        }
        if let Some(body) = if_.body() {
            if let Some(unwrapped) = body_text(file, &body, &indent) {
                rewrite_statement(file, statement, out, "Unwrap if", unwrapped)
            }
            if let Some(last) = body.statements().last() {
                if matches!(
                    last,
                    ast::Stmt::Return(_)
                        | ast::Stmt::Raise(_)
                        | ast::Stmt::Break(_)
                        | ast::Stmt::Continue(_)
                ) && if_.elif_clauses().next().is_none()
                {
                    if let Some(else_) = if_.else_clause() {
                        if let Some(else_body) = else_.body() {
                            if let Some(unwrapped) = body_text(file, &else_body, &indent) {
                                let header_end =
                                    line_start(&file.text, usize::from(else_.text_range().start()));
                                let start = usize::from(statement.text_range().start());
                                rewrite_statement(
                                    file,
                                    statement,
                                    out,
                                    "Remove redundant else",
                                    format!(
                                        "{indent}{}{}",
                                        &file.text[start..header_end],
                                        unwrapped
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
        if let Some(else_body) = if_.else_clause().and_then(|clause| clause.body()) {
            if let Some(unwrapped) = body_text(file, &else_body, &indent) {
                rewrite_statement(file, statement, out, "Unwrap else", unwrapped)
            }
        }
        if_chain_to_match(db, file, if_, out);
    }
    match statement {
        ast::Stmt::For(for_) => {
            if let Some(body) = for_.body() {
                if let Some(body) = body_text(file, &body, &indent) {
                    rewrite_statement(file, statement, out, "Unwrap for", body)
                }
            }
            if for_.else_clause().is_none() {
                append_clause(file, statement, "Add else branch", "else:", out)
            }
            range_to_enumerate(db, file, for_, out);
            aggregates(file, for_, out);
            dict_iteration(db, file, for_, out);
        }
        ast::Stmt::While(while_) => {
            if let Some(body) = while_.body() {
                if let Some(body) = body_text(file, &body, &indent) {
                    rewrite_statement(file, statement, out, "Unwrap while", body)
                }
            }
            if while_.else_clause().is_none() {
                append_clause(file, statement, "Add else branch", "else:", out)
            }
            while_condition(file, while_, out);
        }
        ast::Stmt::With(with) => {
            if let Some(body) = with.body() {
                if let Some(body) = body_text(file, &body, &indent) {
                    rewrite_statement(file, statement, out, "Unwrap with", body)
                }
            }
            with_actions(file, with, out);
        }
        ast::Stmt::Try(try_) => {
            if let Some(body) = try_.body() {
                if let Some(body) = body_text(file, &body, &indent) {
                    rewrite_statement(file, statement, out, "Unwrap try", body)
                }
            }
            let clause = if try_.except_clauses().any(|clause| clause.is_star()) {
                "except* Exception:"
            } else {
                "except Exception:"
            };
            if let Some(finally) = try_
                .else_clause()
                .map(|clause| clause.text_range())
                .or_else(|| try_.finally_clause().map(|clause| clause.text_range()))
            {
                if isolated(file, statement) {
                    let at = line_start(&file.text, usize::from(finally.start()));
                    offer(
                        out,
                        TextRange::empty(TextSize::new(at as u32)),
                        "Add except clause",
                        format!("{indent}{clause}\n{indent}    pass\n"),
                    );
                }
            } else {
                append_clause(file, statement, "Add except clause", clause, out);
            }
            suppress(file, try_, out);
        }
        ast::Stmt::Match(match_) => match_to_if(file, match_, out),
        ast::Stmt::Assert(assert) => {
            if let Some(condition) = assert.test() {
                if canonical(file, statement.text_range().start(), "AssertionError") {
                    let message = assert
                        .message()
                        .map(|message| text(file, &message))
                        .unwrap_or_default();
                    rewrite_statement(
                        file,
                        statement,
                        out,
                        "Convert assert to if raise",
                        format!(
                            "{indent}if {}:\n{indent}    raise AssertionError({message})\n",
                            negate(&file.text, &condition)
                        ),
                    );
                }
            }
        }
        ast::Stmt::Assign(assign) => {
            let targets = assign.targets();
            if targets.len() == 1 {
                if let (Some(ast::Expr::Name(target)), Some(ast::Expr::Binary(value))) =
                    (targets.first().cloned(), assign.value())
                {
                    if let (Some(left), Some(right), Some(op)) =
                        (value.lhs(), value.rhs(), value.op())
                    {
                        if same(&ast::Expr::Name(target.clone()), &left)
                            && primitive(db, file, &left, &["int", "float", "complex"])
                            && matches!(
                                op.kind(),
                                SyntaxKind::PLUS
                                    | SyntaxKind::MINUS
                                    | SyntaxKind::STAR
                                    | SyntaxKind::SLASH
                                    | SyntaxKind::SLASH_SLASH
                                    | SyntaxKind::PERCENT
                                    | SyntaxKind::STAR_STAR
                                    | SyntaxKind::AMP
                                    | SyntaxKind::PIPE
                                    | SyntaxKind::CARET
                                    | SyntaxKind::SHL
                                    | SyntaxKind::SHR
                            )
                        {
                            offer(
                                out,
                                assign.text_range(),
                                "Replace assignment with augmented assignment",
                                format!(
                                    "{} {}= {}",
                                    target.name_text(),
                                    op.text(),
                                    text(file, &right)
                                ),
                            );
                        }
                    }
                }
            }
        }
        ast::Stmt::AugAssign(assign) => {
            if let (Some(ast::Expr::Name(target)), Some(value), Some(op)) =
                (assign.target(), assign.value(), assign.op())
            {
                if primitive(
                    db,
                    file,
                    &ast::Expr::Name(target.clone()),
                    &["int", "float", "complex"],
                ) {
                    let operator = op.text().trim_end_matches('=');
                    offer(
                        out,
                        assign.text_range(),
                        "Expand augmented assignment",
                        format!(
                            "{} = {} {operator} ({})",
                            target.name_text(),
                            target.name_text(),
                            text(file, &value)
                        ),
                    );
                }
            }
        }
        ast::Stmt::FuncDef(function) => function_lambda(file, function, out),
        ast::Stmt::ClassDef(class) => dataclass(file, class, out),
        _ => {}
    }
    if !matches!(
        statement,
        ast::Stmt::FuncDef(_)
            | ast::Stmt::ClassDef(_)
            | ast::Stmt::Import(_)
            | ast::Stmt::ImportFrom(_)
    ) && isolated(file, statement)
    {
        let body = format!("{indent}    ");
        let range = stmt_lines(&file.text, statement.text_range());
        let source = reindent(slice(&file.text, range), &indent, &body);
        for (title, header, tail) in [
            ("Surround with if", "if True:", String::new()),
            (
                "Surround with while",
                "while True:",
                format!("{body}break\n"),
            ),
            ("Surround with for", "for _item in (None,):", String::new()),
            (
                "Surround with try except",
                "try:",
                format!("{indent}except Exception:\n{body}raise\n"),
            ),
        ] {
            offer(
                out,
                range,
                title,
                format!("{indent}{header}\n{source}{tail}"),
            );
        }
        {
            let (name, import) = import_name(
                file,
                statement.text_range().start(),
                "contextlib",
                "nullcontext",
            );
            let mut edits = vec![(range, format!("{indent}with {name}():\n{source}"))];
            if let Some(import) = import {
                edits.push(import);
            }
            out.push(Intention {
                title: "Surround with with".into(),
                edits,
            });
        }
    }
}
fn comparison_values(file: &FileInfo, condition: ast::Expr) -> Option<(String, Vec<String>)> {
    let mut expressions = vec![condition];
    if let ast::Expr::BoolOp(or) = expressions[0].clone().unparen() {
        if !or.is_or() {
            return None;
        }
        expressions = or.operands().collect();
    }
    let mut name = None;
    let mut values = Vec::new();
    for expr in expressions {
        let ast::Expr::Compare(compare) = expr.unparen() else {
            return None;
        };
        let (left, op, right) = single_compare(&compare)?;
        if op != "==" && op != "is" {
            return None;
        }
        let ast::Expr::Name(subject) = left.unparen() else {
            return None;
        };
        if !matches!(
            right.clone().unparen(),
            ast::Expr::Literal(_) | ast::Expr::String(_)
        ) {
            return None;
        }
        let subject = subject.name_text();
        if name.as_ref().is_some_and(|name| name != &subject) {
            return None;
        }
        name = Some(subject);
        let value = text(file, &right);
        if op == "is" && !matches!(value.as_str(), "None" | "True" | "False") {
            return None;
        }
        values.push(if op == "==" && value == "True" {
            "1".into()
        } else if op == "==" && value == "False" {
            "0".into()
        } else {
            value
        });
    }
    Some((name?, values))
}
fn if_chain_to_match(
    db: &Analysis,
    file: &Arc<FileInfo>,
    if_: &ast::IfStmt,
    out: &mut Vec<Intention>,
) {
    let Some(condition) = if_.condition() else {
        return;
    };
    let Some((subject, values)) = comparison_values(file, condition.clone()) else {
        return;
    };
    let Some(left) = condition
        .syntax()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|name| name.name_text() == subject)
        .map(ast::Expr::Name)
    else {
        return;
    };
    if !primitive(db, file, &left, &["int", "str", "bool"]) {
        return;
    }
    let indent = indent_at(&file.text, usize::from(if_.text_range().start()));
    let case_indent = format!("{indent}    ");
    let body_indent = format!("{case_indent}    ");
    let Some(body) = if_
        .body()
        .and_then(|body| body_text(file, &body, &body_indent))
    else {
        return;
    };
    let mut result = format!(
        "{indent}match {subject}:\n{case_indent}case {}:\n{body}",
        values.join(" | ")
    );
    for elif in if_.elif_clauses() {
        let Some((name, values)) = elif.condition().and_then(|c| comparison_values(file, c)) else {
            return;
        };
        if name != subject {
            return;
        }
        let Some(body) = elif
            .body()
            .and_then(|body| body_text(file, &body, &body_indent))
        else {
            return;
        };
        result.push_str(&format!(
            "{case_indent}case {}:\n{body}",
            values.join(" | ")
        ));
    }
    if let Some(body) = if_
        .else_clause()
        .and_then(|clause| clause.body())
        .and_then(|body| body_text(file, &body, &body_indent))
    {
        result.push_str(&format!("{case_indent}case _:\n{body}"));
    }
    rewrite_statement(
        file,
        &ast::Stmt::If(if_.clone()),
        out,
        "Convert if chain to match",
        result,
    );
}
fn match_to_if(file: &FileInfo, match_: &ast::MatchStmt, out: &mut Vec<Intention>) {
    let Some(subject) = match_.subject() else {
        return;
    };
    if !matches!(subject, ast::Expr::Name(_)) {
        return;
    }
    let subject = text(file, &subject);
    let indent = indent_at(&file.text, usize::from(match_.text_range().start()));
    let body_indent = format!("{indent}    ");
    let mut result = String::new();
    for (index, case) in match_.cases().enumerate() {
        if case.guard().is_some() {
            return;
        }
        let Some(pattern) = case.pattern() else {
            return;
        };
        let pattern_text = pattern.text().to_string();
        let condition = if pattern_text.trim() == "_" {
            if index == 0 {
                return;
            }
            None
        } else {
            let values: Vec<String> = if pattern.kind() == SyntaxKind::MATCH_OR_PATTERN {
                pattern
                    .children()
                    .filter(|node| node.kind().is_pattern())
                    .map(|node| node.text().to_string())
                    .collect()
            } else {
                vec![pattern_text]
            };
            for value in &values {
                let parsed = rustpy_syntax::parse_expression(value);
                if !parsed.errors().is_empty()
                    || !parsed
                        .syntax_node()
                        .descendants()
                        .filter_map(ast::Expr::cast)
                        .next()
                        .is_some_and(|expr| {
                            matches!(
                                expr,
                                ast::Expr::Literal(_) | ast::Expr::String(_) | ast::Expr::Unary(_)
                            )
                        })
                {
                    return;
                }
            }
            Some(
                values
                    .iter()
                    .map(|value| {
                        format!(
                            "{subject} {} {value}",
                            if matches!(value.as_str(), "None" | "True" | "False") {
                                "is"
                            } else {
                                "=="
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" or "),
            )
        };
        let Some(body) = case
            .body()
            .and_then(|body| body_text(file, &body, &body_indent))
        else {
            return;
        };
        match condition {
            Some(condition) => result.push_str(&format!(
                "{indent}{} {condition}:\n{body}",
                if index == 0 { "if" } else { "elif" }
            )),
            None => result.push_str(&format!("{indent}else:\n{body}")),
        }
    }
    if !result.is_empty() {
        rewrite_statement(
            file,
            &ast::Stmt::Match(match_.clone()),
            out,
            "Convert match to if",
            result,
        )
    }
}
fn while_condition(file: &FileInfo, while_: &ast::WhileStmt, out: &mut Vec<Intention>) {
    if while_.else_clause().is_some() || while_.condition().is_none_or(|c| text(file, &c) != "True")
    {
        return;
    }
    let Some(body) = while_.body() else { return };
    let mut statements = body.statements();
    let Some(ast::Stmt::If(first)) = statements.next() else {
        return;
    };
    if !is_plain_if(&first)
        || first
            .body()
            .and_then(|b| single_stmt(&b))
            .is_none_or(|s| !matches!(s, ast::Stmt::Break(_)))
    {
        return;
    }
    let Some(condition) = first.condition() else {
        return;
    };
    let indent = indent_at(&file.text, usize::from(while_.text_range().start()));
    let body_indent = format!("{indent}    ");
    let remaining: Vec<_> = statements.collect();
    if remaining.is_empty() {
        return;
    }
    let start = stmt_lines(&file.text, remaining[0].text_range()).start();
    let end = stmt_lines(&file.text, remaining.last().unwrap().text_range()).end();
    let content = slice(&file.text, TextRange::new(start, end));
    if !content.starts_with(&body_indent) {
        return;
    }
    rewrite_statement(
        file,
        &ast::Stmt::While(while_.clone()),
        out,
        "Replace while True with condition",
        format!(
            "{indent}while {}:\n{content}",
            negate(&file.text, &condition)
        ),
    );
}
fn with_actions(file: &FileInfo, with: &ast::WithStmt, out: &mut Vec<Intention>) {
    let items: Vec<_> = with.items().collect();
    let indent = indent_at(&file.text, usize::from(with.text_range().start()));
    let prefix = if with.is_async() {
        "async with"
    } else {
        "with"
    };
    if items.len() > 1 {
        let mut result = String::new();
        let mut current = indent.clone();
        for item in &items {
            result.push_str(&format!("{current}{prefix} {}:\n", item.syntax().text()));
            current.push_str("    ");
        }
        if let Some(body) = with
            .body()
            .and_then(|body| body_text(file, &body, &current))
        {
            result.push_str(&body);
            rewrite_statement(
                file,
                &ast::Stmt::With(with.clone()),
                out,
                "Split with statements",
                result,
            )
        }
    }
    if let Some(ast::Stmt::With(inner)) = with.body().and_then(|body| single_stmt(&body)) {
        if inner.is_async() != with.is_async() {
            return;
        }
        let all = items
            .iter()
            .map(|i| i.syntax().text().to_string())
            .chain(inner.items().map(|i| i.syntax().text().to_string()))
            .collect::<Vec<_>>();
        if let Some(body) = inner
            .body()
            .and_then(|body| body_text(file, &body, &format!("{indent}    ")))
        {
            rewrite_statement(
                file,
                &ast::Stmt::With(with.clone()),
                out,
                "Join with statements",
                format!("{indent}{prefix} {}:\n{body}", all.join(", ")),
            )
        }
    }
}
fn suppress(file: &FileInfo, try_: &ast::TryStmt, out: &mut Vec<Intention>) {
    let handlers: Vec<_> = try_.except_clauses().collect();
    if handlers.len() != 1
        || handlers[0].is_star()
        || handlers[0].name().is_some()
        || try_.else_clause().is_some()
        || try_.finally_clause().is_some()
    {
        return;
    }
    if handlers[0]
        .body()
        .and_then(|body| single_stmt(&body))
        .is_none_or(|stmt| !matches!(stmt, ast::Stmt::Pass(_)))
    {
        return;
    }
    let Some(exception) = handlers[0].exception_type() else {
        return;
    };
    let exceptions = if let ast::Expr::Tuple(tuple) = exception.clone().unparen() {
        tuple.elements().collect::<Vec<_>>()
    } else {
        vec![exception]
    };
    if exceptions.is_empty()||exceptions.iter().any(|exception|!matches!(exception,ast::Expr::Name(name) if matches!(name.name_text().as_str(),"Exception"|"BaseException"|"ValueError"|"TypeError"|"KeyError"|"IndexError"|"OSError"|"RuntimeError"|"AttributeError"|"ImportError"|"ZeroDivisionError")&&canonical(file,try_.text_range().start(),&name.name_text()))){return}
    let (name, import) = import_name(file, try_.text_range().start(), "contextlib", "suppress");
    let indent = indent_at(&file.text, usize::from(try_.text_range().start()));
    let Some(body) = try_
        .body()
        .and_then(|body| body_text(file, &body, &format!("{indent}    ")))
    else {
        return;
    };
    let before = out.len();
    rewrite_statement(
        file,
        &ast::Stmt::Try(try_.clone()),
        out,
        "Convert try except to with suppress",
        format!(
            "{indent}with {name}({}):\n{body}",
            exceptions
                .iter()
                .map(|e| text(file, e))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    if out.len() > before {
        if let Some(import) = import {
            out.last_mut().unwrap().edits.push(import);
        }
    }
}

fn function_lambda(file: &FileInfo, function: &ast::FuncDef, out: &mut Vec<Intention>) {
    if function.is_async()
        || function.decorators().next().is_some()
        || function.return_annotation().is_some()
        || function.type_params().is_some()
        || function.is_method()
    {
        return;
    }
    let Some(params) = function.params() else {
        return;
    };
    if params
        .syntax()
        .descendants()
        .any(|node| node.kind() == SyntaxKind::ANNOTATION)
    {
        return;
    }
    let Some(ast::Stmt::Return(return_)) = function.body().and_then(|body| single_stmt(&body))
    else {
        return;
    };
    let Some(value) = return_.value() else { return };
    let raw = params.syntax().text().to_string();
    let parameters = raw.trim_start_matches('(').trim_end_matches(')');
    let indent = indent_at(&file.text, usize::from(function.text_range().start()));
    rewrite_statement(
        file,
        &ast::Stmt::FuncDef(function.clone()),
        out,
        "Convert function to lambda",
        format!(
            "{indent}{} = lambda {parameters}: {}\n",
            function.name_text(),
            text(file, &value)
        ),
    );
}
fn dataclass(file: &FileInfo, class: &ast::ClassDef, out: &mut Vec<Intention>) {
    if class.decorators().next().is_some() || class.arguments().is_some() {
        return;
    }
    let Some(body) = class.body() else { return };
    if body.statements().any(|statement| {
        !matches!(
            statement,
            ast::Stmt::AnnAssign(_) | ast::Stmt::Pass(_) | ast::Stmt::Expr(_)
        )
    }) {
        return;
    }
    let indent = indent_at(&file.text, usize::from(class.text_range().start()));
    let (name, import) = import_name(file, class.text_range().start(), "dataclasses", "dataclass");
    let mut edits = vec![(
        TextRange::empty(class.text_range().start()),
        format!("@{name}\n{indent}"),
    )];
    if let Some(import) = import {
        edits.push(import);
    }
    out.push(Intention {
        title: "Convert class to dataclass".into(),
        edits,
    });
}
fn writes_names(body: &ast::Block, names: &[&str]) -> bool {
    for node in body.syntax().descendants() {
        let targets: Vec<_> = match node.kind() {
            SyntaxKind::ASSIGN_STMT => ast::AssignStmt::cast(node).unwrap().targets(),
            SyntaxKind::ANN_ASSIGN_STMT => ast::AnnAssignStmt::cast(node)
                .unwrap()
                .target()
                .into_iter()
                .collect(),
            SyntaxKind::AUG_ASSIGN_STMT => ast::AugAssignStmt::cast(node)
                .unwrap()
                .target()
                .into_iter()
                .collect(),
            SyntaxKind::FOR_STMT => ast::ForStmt::cast(node)
                .unwrap()
                .target()
                .into_iter()
                .collect(),
            _ => continue,
        };
        if targets.iter().any(|target| {
            target
                .syntax()
                .descendants()
                .filter_map(ast::NameExpr::cast)
                .any(|name| names.iter().any(|wanted| name.name_text() == *wanted))
        }) {
            return true;
        }
    }
    false
}
fn no_effects(body: &ast::Block) -> bool {
    !body.syntax().descendants().any(|node| {
        matches!(
            node.kind(),
            SyntaxKind::CALL_EXPR
                | SyntaxKind::FUNC_DEF
                | SyntaxKind::CLASS_DEF
                | SyntaxKind::LAMBDA_EXPR
                | SyntaxKind::AWAIT_EXPR
                | SyntaxKind::YIELD_EXPR
                | SyntaxKind::DEL_STMT
        )
    })
}
fn fresh(file: &FileInfo, base: &str) -> String {
    for index in 0..100 {
        let name = if index == 0 {
            base.into()
        } else {
            format!("{base}{index}")
        };
        if !file
            .scopes
            .bindings
            .iter()
            .any(|binding| binding.name == name)
            && !file
                .scopes
                .references
                .iter()
                .any(|reference| reference.name == name)
        {
            return name;
        }
    }
    format!("_{base}_item")
}
fn range_to_enumerate(
    db: &Analysis,
    file: &Arc<FileInfo>,
    for_: &ast::ForStmt,
    out: &mut Vec<Intention>,
) {
    if for_.is_async() {
        return;
    }
    let Some(ast::Expr::Name(index)) = for_.target() else {
        return;
    };
    let Some(ast::Expr::Call(range)) = for_.iter() else {
        return;
    };
    if range
        .callee()
        .is_none_or(|callee| text(file, &callee) != "range")
        || !canonical(file, for_.text_range().start(), "range")
        || !canonical(file, for_.text_range().start(), "len")
        || !canonical(file, for_.text_range().start(), "enumerate")
    {
        return;
    }
    let args = range
        .arg_list()
        .map(|args| args.args().collect::<Vec<_>>())
        .unwrap_or_default();
    if args.len() != 1 {
        return;
    }
    let Some(ast::Expr::Call(len)) = args[0].value() else {
        return;
    };
    if len
        .callee()
        .is_none_or(|callee| text(file, &callee) != "len")
    {
        return;
    }
    let args = len
        .arg_list()
        .map(|args| args.args().collect::<Vec<_>>())
        .unwrap_or_default();
    if args.len() != 1 {
        return;
    }
    let Some(ast::Expr::Name(sequence)) = args[0].value() else {
        return;
    };
    let Some(body) = for_.body() else { return };
    if !no_effects(&body)
        || !primitive(
            db,
            file,
            &ast::Expr::Name(sequence.clone()),
            &["list", "tuple", "str", "bytes"],
        )
    {
        return;
    }
    if body
        .syntax()
        .descendants()
        .filter_map(ast::AssignStmt::cast)
        .flat_map(|assign| assign.targets())
        .any(|target| {
            target
                .syntax()
                .descendants()
                .filter_map(ast::NameExpr::cast)
                .any(|name| {
                    name.name_text() == sequence.name_text()
                        || name.name_text() == index.name_text()
                })
        })
    {
        return;
    }
    if writes_names(&body, &[&sequence.name_text(), &index.name_text()]) {
        return;
    }
    let value = fresh(file, "item");
    let mut edits = vec![
        (
            for_.target().unwrap().text_range(),
            format!("{}, {value}", index.name_text()),
        ),
        (
            for_.iter().unwrap().text_range(),
            format!("enumerate({})", sequence.name_text()),
        ),
    ];
    for node in body
        .syntax()
        .descendants()
        .filter_map(ast::SubscriptExpr::cast)
    {
        if node
            .object()
            .is_some_and(|object| text(file, &object) == sequence.name_text())
            && node
                .index_expr()
                .is_some_and(|expr| text(file, &expr) == index.name_text())
        {
            if is_assignment_target(node.syntax()) {
                return;
            }
            edits.push((node.text_range(), value.clone()));
        }
    }
    out.push(Intention {
        title: "Replace range len with enumerate".into(),
        edits,
    });
}
fn preceding_assignment(file: &FileInfo, for_: &ast::ForStmt) -> Option<ast::AssignStmt> {
    let statement = previous_statement(for_.syntax())?;
    let ast::Stmt::Assign(assignment) = statement else {
        return None;
    };
    if !isolated(file, &ast::Stmt::Assign(assignment.clone())) {
        return None;
    }
    Some(assignment)
}
fn previous_statement(node: &SyntaxNode) -> Option<ast::Stmt> {
    let mut previous = node.prev_sibling();
    while let Some(node) = previous {
        if let Some(statement) = ast::Stmt::cast(node.clone()) {
            return Some(statement);
        }
        previous = node.prev_sibling();
    }
    None
}
pub(crate) fn aggregates(file: &FileInfo, for_: &ast::ForStmt, out: &mut Vec<Intention>) {
    if for_.is_async() || for_.else_clause().is_some() {
        return;
    }
    let Some(before) = preceding_assignment(file, for_) else {
        return;
    };
    let targets = before.targets();
    if targets.len() != 1 {
        return;
    }
    let ast::Expr::Name(target) = targets[0].clone() else {
        return;
    };
    let Some(initial) = before.value() else {
        return;
    };
    let Some(body) = for_.body() else { return };
    let Some(statement) = single_stmt(&body) else {
        return;
    };
    let Some(iter) = for_.iter() else { return };
    let Some(item) = for_.target() else { return };
    let loop_names: Vec<_> = item
        .syntax()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .map(|name| name.name_text())
        .collect();
    if file.scopes.references.iter().any(|reference| {
        reference.range.start() > for_.text_range().end()
            && loop_names
                .iter()
                .any(|name| name == reference.name.as_str())
    }) {
        return;
    }
    let tail = format!("for {} in {}", text(file, &item), text(file, &iter));
    let (function, element) = match statement {
        ast::Stmt::AugAssign(assignment)
            if text(file, &initial) == "0"
                && assignment
                    .target()
                    .is_some_and(|expr| same(&expr, &targets[0]))
                && assignment
                    .op()
                    .is_some_and(|op| op.kind() == SyntaxKind::PLUS_EQ) =>
        {
            let Some(value) = assignment.value() else {
                return;
            };
            ("sum", text(file, &value))
        }
        ast::Stmt::If(if_) if is_plain_if(&if_) => {
            let Some(body) = if_.body() else { return };
            let statements: Vec<_> = body.statements().collect();
            if statements.len() != 2 || !matches!(statements[1], ast::Stmt::Break(_)) {
                return;
            }
            let ast::Stmt::Assign(assign) = &statements[0] else {
                return;
            };
            if assign.targets().len() != 1 || !same(&assign.targets()[0], &targets[0]) {
                return;
            }
            let Some(value) = assign.value() else { return };
            let Some(condition) = if_.condition() else {
                return;
            };
            match (text(file, &initial).as_str(), text(file, &value).as_str()) {
                ("False", "True") => ("any", text(file, &condition)),
                ("True", "False") => ("all", negate(&file.text, &condition)),
                _ => return,
            }
        }
        _ => return,
    };
    let parsed = rustpy_syntax::parse_expression(&element);
    if parsed
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .any(|name| name.name_text() == target.name_text())
    {
        return;
    }
    if !canonical(file, for_.text_range().start(), function) {
        return;
    }
    let indent = indent_at(&file.text, usize::from(for_.text_range().start()));
    let range = TextRange::new(
        stmt_lines(&file.text, before.text_range()).start(),
        stmt_lines(&file.text, for_.text_range()).end(),
    );
    offer(
        out,
        range,
        &format!("Replace loop with {function}"),
        format!(
            "{indent}{} = {function}({element} {tail})\n",
            target.name_text()
        ),
    );
}
fn dict_iteration(
    db: &Analysis,
    file: &Arc<FileInfo>,
    for_: &ast::ForStmt,
    out: &mut Vec<Intention>,
) {
    let Some(iter) = for_.iter() else { return };
    if let ast::Expr::Call(call) = iter.clone() {
        if let Some(ast::Expr::Attribute(attribute)) = call.callee() {
            if attribute.attr_text() == "keys"
                && call
                    .arg_list()
                    .is_some_and(|args| args.args().next().is_none())
            {
                if let Some(object) = attribute.object() {
                    if primitive(db, file, &object, &["dict"]) {
                        offer(
                            out,
                            call.text_range(),
                            "Replace dict keys iteration",
                            text(file, &object),
                        );
                    }
                }
            }
        }
    }
    let Some(ast::Expr::Name(dictionary)) = Some(iter.clone().unparen()) else {
        return;
    };
    if !primitive(db, file, &iter, &["dict"]) {
        return;
    }
    let Some(ast::Expr::Name(key)) = for_.target() else {
        return;
    };
    let Some(body) = for_.body() else { return };
    if !no_effects(&body) {
        return;
    }
    if writes_names(&body, &[&dictionary.name_text(), &key.name_text()]) {
        return;
    }
    let value = fresh(file, "value");
    let mut edits = Vec::new();
    for node in body
        .syntax()
        .descendants()
        .filter_map(ast::SubscriptExpr::cast)
    {
        if node
            .object()
            .is_some_and(|object| text(file, &object) == dictionary.name_text())
            && node
                .index_expr()
                .is_some_and(|expr| text(file, &expr) == key.name_text())
        {
            if is_assignment_target(node.syntax()) {
                return;
            }
            edits.push((node.text_range(), value.clone()));
        }
    }
    if edits.is_empty() {
        return;
    }
    edits.push((
        for_.target().unwrap().text_range(),
        format!("{}, {value}", key.name_text()),
    ));
    edits.push((
        iter.text_range(),
        format!("{}.items()", dictionary.name_text()),
    ));
    out.push(Intention {
        title: "Iterate over dict items".into(),
        edits,
    });
}
fn expression_actions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    expr: &ast::Expr,
    out: &mut Vec<Intention>,
) {
    let source = text(file, expr);
    let at = expr.text_range().start();
    if !matches!(expr, ast::Expr::Paren(_))
        && !is_assignment_target(expr.syntax())
        && !matches!(expr, ast::Expr::Tuple(_))
    {
        offer(
            out,
            expr.text_range(),
            "Add parentheses",
            format!("({source})"),
        );
    }
    match expr.clone().unparen() {
        ast::Expr::Not(not) => {
            if let Some(operand) = not.operand() {
                if let ast::Expr::BoolOp(boolean) = operand.clone().unparen() {
                    let operands: Vec<_> = boolean.operands().collect();
                    offer(
                        out,
                        expr.text_range(),
                        "Apply De Morgan laws",
                        operands
                            .iter()
                            .map(|e| format!("({})", negate(&file.text, e)))
                            .collect::<Vec<_>>()
                            .join(if boolean.is_or() { " and " } else { " or " }),
                    );
                } else if let ast::Expr::Compare(compare) = operand.unparen() {
                    if let Some((left, op, right)) = single_compare(&compare) {
                        if let Some(negated) = negated_op(&op) {
                            offer(
                                out,
                                expr.text_range(),
                                "Simplify negated comparison",
                                format!("{} {negated} {}", text(file, &left), text(file, &right)),
                            );
                        }
                    }
                }
            }
        }
        ast::Expr::BoolOp(boolean) => {
            let values: Vec<_> = boolean.operands().collect();
            if values.len() == 2 {
                let first = text(file, &values[0]);
                let replacement = match (first.as_str(), boolean.is_or()) {
                    ("True", false) | ("False", true) => Some(text(file, &values[1])),
                    ("True", true) => Some("True".into()),
                    ("False", false) => Some("False".into()),
                    _ => None,
                };
                if let Some(replacement) = replacement {
                    offer(
                        out,
                        expr.text_range(),
                        "Simplify boolean expression",
                        replacement,
                    )
                }
                if !boolean.is_or() {
                    if let (ast::Expr::Compare(a), ast::Expr::Compare(b)) =
                        (values[0].clone().unparen(), values[1].clone().unparen())
                    {
                        if let (Some((left, op, middle)), Some((again, second, right))) =
                            (single_compare(&a), single_compare(&b))
                        {
                            if same(&middle, &again)
                                && simple(&middle)
                                && primitive(db, file, &middle, &["int", "float", "str"])
                                && !matches!(op.as_str(), "in" | "not in")
                                && !matches!(second.as_str(), "in" | "not in")
                            {
                                offer(
                                    out,
                                    expr.text_range(),
                                    "Replace with chained comparison",
                                    format!(
                                        "{} {op} {} {second} {}",
                                        text(file, &left),
                                        text(file, &middle),
                                        text(file, &right)
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            if boolean.is_or() {
                if let Some((subject, values)) = comparison_values(file, expr.clone()) {
                    if values.len() > 1 {
                        let first = boolean.operands().next().unwrap();
                        if let ast::Expr::Compare(compare) = first.unparen() {
                            if let Some((left, _, _)) = single_compare(&compare) {
                                if primitive(db, file, &left, &["int", "str", "bool"]) {
                                    offer(
                                        out,
                                        expr.text_range(),
                                        "Use in instead of chained equality",
                                        format!("{subject} in ({},)", values.join(", ")),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        ast::Expr::Compare(compare) => {
            if let Some((left, op, right)) = single_compare(&compare) {
                if matches!(op.as_str(), "==" | "!=")
                    && (text(file, &left) == "None" || text(file, &right) == "None")
                {
                    offer(
                        out,
                        expr.text_range(),
                        "Use identity comparison for None",
                        format!(
                            "{} {} {}",
                            text(file, &left),
                            if op == "==" { "is" } else { "is not" },
                            text(file, &right)
                        ),
                    );
                }
                if matches!(op.as_str(), "is" | "is not")
                    && matches!(
                        right.clone().unparen(),
                        ast::Expr::Literal(_) | ast::Expr::String(_)
                    )
                    && !matches!(text(file, &right).as_str(), "None" | "True" | "False")
                {
                    offer(
                        out,
                        expr.text_range(),
                        "Use equality comparison for literal",
                        format!(
                            "{} {} {}",
                            text(file, &left),
                            if op == "is" { "==" } else { "!=" },
                            text(file, &right)
                        ),
                    );
                }
                if let ast::Expr::Call(call) = left.clone() {
                    let args = call
                        .arg_list()
                        .map(|args| args.args().collect::<Vec<_>>())
                        .unwrap_or_default();
                    if args.len() == 1 {
                        if let Some(value) = args[0].value() {
                            if call
                                .callee()
                                .is_some_and(|callee| text(file, &callee) == "len")
                                && text(file, &right) == "0"
                                && matches!(op.as_str(), "==" | "!=")
                                && canonical(file, at, "len")
                                && (op == "==" || canonical(file, at, "bool"))
                                && primitive(
                                    db,
                                    file,
                                    &value,
                                    &["list", "tuple", "str", "bytes", "set", "dict"],
                                )
                            {
                                offer(
                                    out,
                                    expr.text_range(),
                                    "Replace empty length comparison",
                                    format!(
                                        "{}({})",
                                        if op == "==" { "not " } else { "bool" },
                                        text(file, &value)
                                    ),
                                );
                            }
                            if call
                                .callee()
                                .is_some_and(|callee| text(file, &callee) == "type")
                                && matches!(op.as_str(), "==" | "is")
                                && simple(&right)
                                && canonical(file, at, "type")
                                && canonical(file, at, "isinstance")
                            {
                                offer(
                                    out,
                                    expr.text_range(),
                                    "Replace type comparison with isinstance",
                                    format!(
                                        "isinstance({}, {})",
                                        text(file, &value),
                                        text(file, &right)
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
        ast::Expr::Conditional(conditional) => {
            if let (Some(value), Some(condition), Some(fallback)) = (
                conditional.true_expr(),
                conditional.condition(),
                conditional.false_expr(),
            ) {
                if same(&value, &condition)
                    && matches!(condition, ast::Expr::Name(_))
                    && primitive(
                        db,
                        file,
                        &condition,
                        &[
                            "int", "float", "str", "bool", "list", "tuple", "dict", "set",
                        ],
                    )
                {
                    offer(
                        out,
                        expr.text_range(),
                        "Replace conditional with or",
                        format!("{} or ({})", text(file, &value), text(file, &fallback)),
                    );
                }
                if let (ast::Expr::Subscript(subscript), ast::Expr::Compare(compare)) =
                    (value, condition.unparen())
                {
                    if let Some((key, op, dictionary)) = single_compare(&compare) {
                        if op == "in"
                            && simple(&key)
                            && simple(&dictionary)
                            && primitive(db, file, &dictionary, &["dict"])
                            && matches!(fallback, ast::Expr::Literal(_) | ast::Expr::String(_))
                            && subscript
                                .object()
                                .is_some_and(|object| same(&object, &dictionary))
                            && subscript
                                .index_expr()
                                .is_some_and(|index| same(&index, &key))
                        {
                            offer(
                                out,
                                expr.text_range(),
                                "Replace with dict get",
                                format!(
                                    "{}.get({}, {})",
                                    text(file, &dictionary),
                                    text(file, &key),
                                    text(file, &fallback)
                                ),
                            );
                        }
                    }
                }
            }
        }
        ast::Expr::Generator(generator) => {
            let raw = generator.syntax().text().to_string();
            let raw = raw.trim();
            let body = if raw.starts_with('(') && raw.ends_with(')') {
                &raw[1..raw.len() - 1]
            } else {
                raw
            };
            offer(
                out,
                expr.text_range(),
                "Convert generator expression to list comprehension",
                format!("[{body}]"),
            );
        }
        ast::Expr::Call(call) => collection_literal(file, &call, out),
        _ => {}
    }
}
fn collection_literal(file: &FileInfo, call: &ast::CallExpr, out: &mut Vec<Intention>) {
    let Some(ast::Expr::Name(name)) = call.callee() else {
        return;
    };
    let name = name.name_text();
    if !matches!(name.as_str(), "list" | "tuple" | "set")
        || !canonical(file, call.text_range().start(), &name)
    {
        return;
    }
    let Some(args) = call.arg_list() else { return };
    let args: Vec<_> = args.args().collect();
    let values = if args.is_empty() {
        Vec::new()
    } else if args.len() == 1 {
        let Some(expr) = args[0].value() else { return };
        match expr.unparen() {
            ast::Expr::List(list) => list.elements().collect::<Vec<_>>(),
            ast::Expr::Tuple(tuple) => tuple.elements().collect::<Vec<_>>(),
            ast::Expr::Set(set) if name == "set" => set.elements().collect::<Vec<_>>(),
            _ => return,
        }
    } else {
        return;
    };
    let content = values
        .iter()
        .map(|expr| text(file, expr))
        .collect::<Vec<_>>()
        .join(", ");
    let replacement = match name.as_str() {
        "list" => format!("[{content}]"),
        "tuple" => {
            if values.is_empty() {
                "()".into()
            } else {
                format!("({content},)")
            }
        }
        "set" => {
            if values.is_empty() {
                return;
            } else {
                format!("{{{content}}}")
            }
        }
        _ => return,
    };
    offer(
        out,
        call.text_range(),
        &format!("Convert {name} call to literal"),
        replacement,
    );
}
pub fn at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Intention> {
    let mut out = Vec::new();
    if let Some(statement) = statement_at(file, offset) {
        branch_actions(db, file, &statement, &mut out)
    }
    if let Some(token) = token_at(&file.parse.syntax_node(), offset) {
        for node in token.parent_ancestors() {
            if let Some(expr) = ast::Expr::cast(node.clone()) {
                expression_actions(db, file, &expr, &mut out)
            }
            if ast::Stmt::can_cast(node.kind()) {
                break;
            }
        }
    }
    // Legacy Python-2 print is intentionally handled before parsing recovery
    // can turn its operands into unrelated expression statements.
    let start = line_start(&file.text, usize::from(offset));
    let end = file.text[start..]
        .find('\n')
        .map_or(file.text.len(), |n| start + n);
    let line = &file.text[start..end];
    let content = line.trim_start();
    if let Some(values) = content.strip_prefix("print ") {
        if !values.starts_with(['(', '>']) && canonical(file, offset, "print") {
            let indent = &line[..line.len() - content.len()];
            let (values, suffix) = if let Some(value) = values.trim_end().strip_suffix(',') {
                (value, ", end=' '")
            } else {
                (values, "")
            };
            offer(
                &mut out,
                TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
                "Replace print statement with function call",
                format!("{indent}print({values}{suffix})"),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn setup(source: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/project"));
        let id = db.set_file_text(Path::new("/project/main.py"), source);
        let file = db.file(id).unwrap();
        (db, file)
    }
    fn choices(source: &str, needle: &str) -> Vec<Intention> {
        let (db, file) = setup(source);
        crate::intentions::intentions_at(
            &db,
            &file,
            TextSize::new(source.find(needle).unwrap() as u32),
        )
    }
    fn apply(source: &str, needle: &str, title: &str) -> String {
        let choices = choices(source, needle);
        let intention = choices
            .iter()
            .find(|i| i.title == title)
            .unwrap_or_else(|| {
                panic!(
                    "{title} unavailable: {:?}",
                    choices.iter().map(|i| &i.title).collect::<Vec<_>>()
                )
            });
        let result = apply_edits(source, &intention.edits);
        assert!(
            rustpy_syntax::parse(&result).errors().is_empty(),
            "invalid output: {result}"
        );
        result
    }
    #[test]
    fn if_match_roundtrip_preserves_literal_cases_and_fallback() {
        let source="kind: int = 1\nif kind == 1:\n    result='one'\nelif kind == 2:\n    result='two'\nelse:\n    result='other'\nafter=1\n";
        let matched = apply(source, "if kind", "Convert if chain to match");
        assert!(matched.contains("match kind:\n    case 1:"));
        assert!(matched.contains("    case _:"));
        assert!(matched.ends_with("after=1\n"));
        let restored = apply(&matched, "match kind", "Convert match to if");
        assert!(restored.contains("if kind == 1:"));
        assert!(restored.contains("elif kind == 2:"));
        assert!(restored.contains("else:\n    result='other'"));
        assert!(!choices("if fetch() == 1:\n    pass\n", "if")
            .iter()
            .any(|i| i.title == "Convert if chain to match"));
    }
    #[test]
    fn match_strings_containing_pipe_and_singletons_are_not_split_as_text() {
        let source="match name:\n    case 'a|b':\n        result=1\n    case None:\n        result=2\n    case _:\n        result=3\n";
        let result = apply(source, "match", "Convert match to if");
        assert!(result.contains("name == 'a|b'"));
        assert!(result.contains("name is None"));
        assert!(!choices(
            "match value:\n    case [first, second]:\n        pass\n",
            "match"
        )
        .iter()
        .any(|i| i.title == "Convert match to if"));
    }
    #[test]
    fn leading_break_while_and_redundant_else_keep_following_statements() {
        let source = "while True:\n    if finished:\n        break\n    value += 1\nafter=1\n";
        assert_eq!(
            apply(source, "while True", "Replace while True with condition"),
            "while not finished:\n    value += 1\nafter=1\n"
        );
        let source = "def f(flag):\n    if flag:\n        return 1\n    else:\n        return 2\n";
        assert_eq!(
            apply(source, "if flag", "Remove redundant else"),
            "def f(flag):\n    if flag:\n        return 1\n    return 2\n"
        );
    }
    #[test]
    fn unwrap_and_surround_providers_preserve_body_indentation() {
        for (source, needle, title) in [
            (
                "if flag:\n    value=1\nelse:\n    value=2\n",
                "if",
                "Unwrap if",
            ),
            (
                "if flag:\n    value=1\nelse:\n    value=2\n",
                "else",
                "Unwrap else",
            ),
            ("for item in items:\n    value=1\n", "for", "Unwrap for"),
            ("while flag:\n    value=1\n", "while", "Unwrap while"),
            ("with manager:\n    value=1\n", "with", "Unwrap with"),
            (
                "try:\n    value=1\nexcept Exception:\n    pass\n",
                "try",
                "Unwrap try",
            ),
        ] {
            let result = apply(source, needle, title);
            assert!(result.starts_with("value="));
        }
        for title in [
            "Surround with if",
            "Surround with while",
            "Surround with for",
            "Surround with try except",
            "Surround with with",
        ] {
            let result = apply("value=1\nafter=2\n", "value", title);
            assert!(result.contains("    value=1\n"));
            assert!(result.ends_with("after=2\n"));
        }
        assert!(!choices("first=1; value=2\n", "value")
            .iter()
            .any(|i| i.title.starts_with("Surround")));
    }
    #[test]
    fn join_and_split_with_keep_context_acquisition_and_reverse_exit_order() {
        let source = "with first() as a, second(a) as b:\n    use(a,b)\n";
        let split = apply(source, "with", "Split with statements");
        assert_eq!(
            split,
            "with first() as a:\n    with second(a) as b:\n        use(a,b)\n"
        );
        assert_eq!(apply(&split, "with first", "Join with statements"), source);
        let source =
            "async def f():\n    async with first() as a, second() as b:\n        use(a,b)\n";
        assert!(apply(source, "async with", "Split with statements")
            .contains("        async with second() as b:"));
    }
    #[test]
    fn assert_clauses_and_suppress_are_real_intentions() {
        assert_eq!(
            apply(
                "assert valid, 'π'\n",
                "assert",
                "Convert assert to if raise"
            ),
            "if not valid:\n    raise AssertionError('π')\n"
        );
        assert!(apply("if flag:\n    pass", "if", "Add else branch").contains("pass\nelse:\n"));
        assert!(apply(
            "if flag:\n    pass\nelse:\n    pass\n",
            "if",
            "Add elif branch"
        )
        .contains("elif False:\n    pass\nelse:"));
        assert!(apply(
            "try:\n    value=1\nfinally:\n    done=1\n",
            "try",
            "Add except clause"
        )
        .contains("except Exception:\n    pass\nfinally:"));
        let result = apply(
            "try:\n    value=load()\nexcept ValueError:\n    pass\n",
            "try",
            "Convert try except to with suppress",
        );
        assert!(result.contains("from contextlib import suppress\n"));
        assert!(result.contains("with suppress(ValueError):\n    value=load()"));
        assert!(!choices(
            "try:\n    load()\nexcept ValueError:\n    recover()\n",
            "try"
        )
        .iter()
        .any(|i| i.title == "Convert try except to with suppress"));
    }
    #[test]
    fn range_len_enumerate_and_dictionary_iteration_have_receiver_guards() {
        let source = "items=[1,2]\ntotal=0\nfor i in range(len(items)):\n    total += items[i]\n";
        let result = apply(source, "for i", "Replace range len with enumerate");
        assert!(result.contains("for i, item in enumerate(items):"));
        assert!(result.contains("total += item"));
        assert!(!choices(
            "items=[1,2]\nfor i in range(len(items)):\n    items.append(i)\n",
            "for i"
        )
        .iter()
        .any(|i| i.title == "Replace range len with enumerate"));
        let source = "mapping={'a':1}\nfor key in mapping.keys():\n    total=key\n";
        assert!(
            apply(source, "for key", "Replace dict keys iteration").contains("for key in mapping:")
        );
        let source = "mapping={'a':1}\nfor key in mapping:\n    total=mapping[key]\n";
        let result = apply(source, "for key", "Iterate over dict items");
        assert!(result.contains("for key, value in mapping.items():"));
        assert!(result.contains("total=value"));
    }
    #[test]
    fn loops_to_any_all_sum_refuse_live_loop_variable_and_accumulator_dependencies() {
        for (source,function) in [
            ("found=False\nfor item in items:\n    if item:\n        found=True\n        break\n","any"),
            ("valid=True\nfor item in items:\n    if not item:\n        valid=False\n        break\n","all"),
            ("total=0\nfor item in items:\n    total += item\n","sum"),
        ]{let result=apply(source,"for item",&format!("Replace loop with {function}"));assert!(result.contains(&format!("{function}(")));}
        assert!(!choices(
            "total=0\nfor item in items:\n    total += total + item\n",
            "for item"
        )
        .iter()
        .any(|i| i.title == "Replace loop with sum"));
        assert!(!choices(
            "total=0\nfor item in items:\n    total += item\nprint(item)\n",
            "for item"
        )
        .iter()
        .any(|i| i.title == "Replace loop with sum"));
        assert!(apply(
            "value=(item * 2 for item in items)\n",
            "item *",
            "Convert generator expression to list comprehension"
        )
        .contains("[item * 2 for item in items]"));
    }
    #[test]
    fn boolean_demorgan_comparison_and_membership_routes() {
        assert!(apply(
            "value=not (left and right)\n",
            "not",
            "Apply De Morgan laws"
        )
        .contains("(not left) or (not right)"));
        assert!(apply(
            "value=True and other\n",
            "True",
            "Simplify boolean expression"
        )
        .contains("value=other"));
        assert!(apply(
            "value=not (left == right)\n",
            "not",
            "Simplify negated comparison"
        )
        .contains("left != right"));
        assert!(apply(
            "middle: int=2\nvalue=1 < middle and middle < 4\n",
            "1 <",
            "Replace with chained comparison"
        )
        .contains("1 < middle < 4"));
        assert!(apply(
            "kind: int=1\nvalue=kind == 1 or kind == 2\n",
            "kind ==",
            "Use in instead of chained equality"
        )
        .contains("kind in (1, 2,)"));
        assert!(apply(
            "value=None\nanswer=value == None\n",
            "value ==",
            "Use identity comparison for None"
        )
        .contains("value is None"));
        assert!(apply(
            "answer=value is 17\n",
            "value is",
            "Use equality comparison for literal"
        )
        .contains("value == 17"));
    }
    #[test]
    fn emptiness_type_dict_get_and_conditional_or_have_real_builtin_guards() {
        assert!(apply(
            "items=[]\nempty=len(items) == 0\n",
            "len",
            "Replace empty length comparison"
        )
        .contains("not (items)"));
        assert!(
            !choices("def len(x): return 0\nempty=len(items)==0\n", "len(items)")
                .iter()
                .any(|i| i.title == "Replace empty length comparison")
        );
        assert!(apply(
            "answer=type(value) == int\n",
            "type",
            "Replace type comparison with isinstance"
        )
        .contains("isinstance(value, int)"));
        assert!(apply(
            "mapping={'key':1}\nkey='key'\nvalue=mapping[key] if key in mapping else 0\n",
            "mapping[key]",
            "Replace with dict get"
        )
        .contains("mapping.get(key, 0)"));
        assert!(!choices(
            "mapping={}\nvalue=mapping[key] if key in mapping else fallback()\n",
            "mapping[key]"
        )
        .iter()
        .any(|i| i.title == "Replace with dict get"));
        assert!(apply(
            "name: str='a'\nvalue=name if name else 'fallback'\n",
            "name if",
            "Replace conditional with or"
        )
        .contains("name or ('fallback')"));
    }
    #[test]
    fn function_lambda_literals_augmented_assignment_and_parentheses() {
        assert!(apply(
            "def compute(value=1):\n    return value+1\n",
            "def",
            "Convert function to lambda"
        )
        .contains("compute = lambda value=1: value+1"));
        assert!(!choices("@decorate\ndef f():\n    return 1\n", "def")
            .iter()
            .any(|i| i.title == "Convert function to lambda"));
        for (source, title, expected) in [
            (
                "value=list((1,2))\n",
                "Convert list call to literal",
                "[1, 2]",
            ),
            (
                "value=tuple([1,2])\n",
                "Convert tuple call to literal",
                "(1, 2,)",
            ),
            (
                "value=set([1,2])\n",
                "Convert set call to literal",
                "{1, 2}",
            ),
        ] {
            let needle = source.split_once('=').unwrap().1.split_once('(').unwrap().0;
            assert!(apply(source, needle, title).contains(expected));
        }
        assert!(apply(
            "value: int=1\nvalue=value+1\n",
            "value=value",
            "Replace assignment with augmented assignment"
        )
        .contains("value += 1"));
        assert!(apply(
            "value: int=1\nvalue += 1\n",
            "value +=",
            "Expand augmented assignment"
        )
        .contains("value = value + (1)"));
        assert!(apply("value=name\n", "name", "Add parentheses").contains("value=(name)"));
        assert!(apply(
            "print 'π', value\n",
            "print",
            "Replace print statement with function call"
        )
        .contains("print('π', value)"));
    }
    #[test]
    fn dataclass_provider_and_existing_comprehension_method_routes_remain_registered() {
        let result = apply(
            "class Point:\n    x: int\n    y: int\n",
            "class",
            "Convert class to dataclass",
        );
        assert!(result.contains("from dataclasses import dataclass"));
        assert!(result.contains("@dataclass\nclass Point:"));
        assert!(choices(
            "values=[]\nfor item in items:\n    values.append(item * 2)\n",
            "for item"
        )
        .iter()
        .any(|i| i.title.contains("comprehension")));
        assert!(
            choices("class C:\n    def f(self,x):\n        return x\n", "def f")
                .iter()
                .any(|i| i.title == "Make method static")
        );
        assert!(choices("@staticmethod\ndef unused():\n    pass\n", "def")
            .iter()
            .all(|i| i.title != "Convert static method to function"));
    }
}
