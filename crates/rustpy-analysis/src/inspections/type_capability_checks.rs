//! Write-only capabilities do not change ordinary member/value reads.
use super::Sink;
use rustpy_core::Severity;
use rustpy_semantic::{db::FileInfo, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::sync::Arc;

pub(super) fn run(infer: &Inferer<'_>, file: &Arc<FileInfo>, sink: &mut Sink) {
    if file.text.len() > 1024 * 1024 {
        return;
    }
    let mut remaining = 16384usize;
    for node in file.parse.syntax_node().descendants() {
        if remaining == 0 {
            break;
        }
        remaining -= 1;
        if let Some(statement) = ast::AssignStmt::cast(node.clone()) {
            let mut children = statement
                .syntax()
                .children()
                .filter_map(ast::Expr::cast)
                .peekable();
            while let Some(target) = children.next() {
                if children.peek().is_some() {
                    check_target(infer, file, &target, false, sink, &mut remaining, 0);
                }
            }
        } else if let Some(statement) = ast::AnnAssignStmt::cast(node.clone()) {
            if statement.value().is_some() {
                if let Some(target) = statement.target() {
                    check_target(infer, file, &target, false, sink, &mut remaining, 0);
                }
            }
        } else if let Some(statement) = ast::AugAssignStmt::cast(node.clone()) {
            if let Some(target) = statement.target() {
                check_target(infer, file, &target, false, sink, &mut remaining, 0);
            }
        } else if let Some(statement) = ast::DelStmt::cast(node.clone()) {
            for target in statement.syntax().children().filter_map(ast::Expr::cast) {
                check_target(infer, file, &target, true, sink, &mut remaining, 0);
            }
        } else if let Some(statement) = ast::ForStmt::cast(node.clone()) {
            if let Some(target) = statement.target() {
                check_target(infer, file, &target, false, sink, &mut remaining, 0);
            }
        } else if let Some(item) = ast::WithItem::cast(node.clone()) {
            if let Some(target) = item.target() {
                check_target(infer, file, &target, false, sink, &mut remaining, 0);
            }
        } else if let Some(clause) = ast::CompFor::cast(node.clone()) {
            if let Some(target) = clause.target() {
                check_target(infer, file, &target, false, sink, &mut remaining, 0);
            }
        }
        if let Some(named) = ast::NamedExpr::cast(node.clone()) {
            if let Some(target) = named.target() {
                check_target(
                    infer,
                    file,
                    &ast::Expr::Name(target),
                    false,
                    sink,
                    &mut remaining,
                    0,
                );
            }
        }
        if let Some(class) = ast::ClassDef::cast(node.clone()) {
            for (range, message) in infer.class_declaration_errors(file, &class) {
                sink.push(
                    range,
                    Severity::Warning,
                    "PyTypeChecker",
                    message,
                    Vec::new(),
                );
            }
        }
        if let Some(call) = ast::CallExpr::cast(node) {
            if let Some(message) = infer.typed_dict_mutating_call_error(file, &call) {
                sink.push(
                    call.text_range(),
                    Severity::Warning,
                    "PyTypeChecker",
                    message,
                    Vec::new(),
                );
            }
            let Some(callee) = call.callee() else {
                continue;
            };
            let callable = infer.type_of_expr(file, &callee);
            let Some(signature) = callable.signature() else {
                continue;
            };
            let Some(definition) = &signature.def else {
                continue;
            };
            if !matches!(definition.name.as_str(), "isinstance" | "issubclass") {
                continue;
            }
            let Some(owner) = infer.db.file(definition.file) else {
                continue;
            };
            if !owner.is_stub || owner.path != std::path::Path::new("<typeshed>/builtins.pyi") {
                continue;
            }
            let Some(arguments) = call.arg_list() else {
                continue;
            };
            let Some(target) = arguments.positional().nth(1) else {
                continue;
            };
            let ty = infer.type_of_expr(file, &target);
            if let Some(message) =
                infer.runtime_class_check_error(&ty, definition.name == "issubclass")
            {
                sink.push(
                    target.text_range(),
                    Severity::Warning,
                    "PyTypeChecker",
                    message,
                    Vec::new(),
                );
            }
        }
    }
}

fn check_target(
    infer: &Inferer<'_>,
    file: &Arc<FileInfo>,
    target: &ast::Expr,
    delete: bool,
    sink: &mut Sink,
    remaining: &mut usize,
    depth: usize,
) {
    if depth >= 16 || *remaining == 0 {
        return;
    }
    *remaining -= 1;
    if matches!(
        target.clone().unparen(),
        ast::Expr::Name(_) | ast::Expr::Attribute(_)
    ) {
        if let Some(message) = infer.final_write_error(file, target, delete) {
            sink.push(
                target.text_range(),
                Severity::Warning,
                "PyTypeChecker",
                message,
                Vec::new(),
            );
            return;
        }
    }
    match target.clone().unparen() {
        ast::Expr::Attribute(attribute) => {
            let Some(object) = attribute.object() else {
                return;
            };
            let name = attribute.attr_text();
            if name.len() > 256 {
                return;
            }
            let receiver = infer.type_of_expr(file, &object);
            if let Some(message) = infer.member_write_error(&receiver, &name, delete) {
                sink.push(
                    target.text_range(),
                    Severity::Warning,
                    "PyTypeChecker",
                    message,
                    Vec::new(),
                );
            }
        }
        ast::Expr::Subscript(subscript) => {
            let (Some(object), Some(index)) = (subscript.object(), subscript.index_expr()) else {
                return;
            };
            let receiver = infer.type_of_expr(file, &object);
            let key = infer.typed_dict_key(file, &index);
            let error = if let Some(key) = key {
                infer.typed_dict_write_error(&receiver, &key, delete)
            } else {
                infer.typed_dict_unknown_write_error(&receiver, delete)
            };
            if let Some(message) = error {
                sink.push(
                    target.text_range(),
                    Severity::Warning,
                    "PyTypeChecker",
                    message,
                    Vec::new(),
                );
            }
        }
        ast::Expr::Tuple(tuple) => {
            for item in tuple.elements().take(257) {
                check_target(infer, file, &item, delete, sink, remaining, depth + 1);
            }
        }
        ast::Expr::List(list) => {
            for item in list.elements().take(257) {
                check_target(infer, file, &item, delete, sink, remaining, depth + 1);
            }
        }
        ast::Expr::Star(star) => {
            if let Some(item) = star.expr() {
                check_target(infer, file, &item, delete, sink, remaining, depth + 1);
            }
        }
        _ => {}
    }
}
