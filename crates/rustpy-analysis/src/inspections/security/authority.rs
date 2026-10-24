//! Security identities come from bindings, never a bare callee suffix.
use rustpy_semantic::{db::FileInfo, infer::Inferer, scope::BindingKind, types::Type};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind as K,
};
use std::sync::Arc;

pub(in crate::inspections) fn identity(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    expr: &ast::Expr,
) -> Option<String> {
    resolve(inf, file, expr, expr.text_range().start(), 0)
}
fn resolve(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    expr: &ast::Expr,
    cutoff: rustpy_core::TextSize,
    depth: usize,
) -> Option<String> {
    if depth > 12 {
        return None;
    }
    match expr.clone().unparen() {
        ast::Expr::Attribute(a) => {
            if !intact(file, &a.attr_text(), cutoff) {
                return None;
            }
            let object = a.object()?;
            if let Some(owner) = resolve(inf, file, &object, cutoff, depth + 1) {
                return Some(format!("{owner}.{}", a.attr_text()));
            }
            let Type::Instance(class, _) = inf.type_of_expr(file, &object) else {
                return None;
            };
            let owner = inf.db.file(class.file)?;
            if rustpy_semantic::usages::is_library_file(inf.db, &owner) {
                if !intact(
                    &owner,
                    &a.attr_text(),
                    rustpy_core::TextSize::new(owner.text.len().min(u32::MAX as usize) as u32),
                ) {
                    return None;
                }
                return Some(format!(
                    "{}.{}.{}",
                    owner.module_name.as_ref()?,
                    class.name,
                    a.attr_text()
                ));
            }
            None
        }
        ast::Expr::Name(n) => {
            let scope = file.scopes.scope_at(n.text_range().start());
            if let Some(ids) = file.scopes.resolve(scope, &n.name_text()) {
                let candidates: Vec<_> = ids
                    .iter()
                    .map(|id| file.scopes.binding(*id))
                    .filter(|b| b.range.start() < cutoff)
                    .collect();
                let binding = *candidates.last()?;
                // Conditional declarations do not establish a unique callable.
                let node = rustpy_semantic::edit_helpers::node_with_range(
                    &file.parse.syntax_node(),
                    binding.node_range,
                )?;
                if node
                    .ancestors()
                    .skip(1)
                    .take_while(|a| !matches!(a.kind(), K::FUNC_DEF | K::MODULE))
                    .any(|a| {
                        matches!(
                            a.kind(),
                            K::IF_STMT | K::TRY_STMT | K::FOR_STMT | K::WHILE_STMT | K::WITH_STMT
                        )
                    })
                {
                    return None;
                }
                match &binding.kind {
                    BindingKind::Import(i) if i.level == 0 => {
                        let module = i.module.to_string();
                        if let Some(owner) = inf.db.module_from(file, &module) {
                            // Project modules are not the framework, but a
                            // virtual environment inside the project is.
                            if !rustpy_semantic::usages::is_library_file(inf.db, &owner) {
                                return None;
                            }
                        }
                        Some(if let Some(name) = &i.name {
                            format!("{module}.{name}")
                        } else if i.alias.is_some() {
                            module
                        } else {
                            module.split('.').next()?.to_string()
                        })
                    }
                    BindingKind::Assignment => {
                        let value = rustpy_semantic::edit_helpers::node_with_range(
                            &file.parse.syntax_node(),
                            binding.value?,
                        )?;
                        resolve(
                            inf,
                            file,
                            &ast::Expr::cast(value)?,
                            binding.range.start(),
                            depth + 1,
                        )
                    }
                    _ => None,
                }
            } else {
                if file
                    .scopes
                    .scopes
                    .iter()
                    .any(|scope| !scope.star_imports.is_empty())
                {
                    return None;
                }
                let definition = inf.resolve_name(file, scope, &n.name_text())?;
                let definition = inf.follow_imports(definition);
                if definition.file.is_stub
                    && definition.file.path.starts_with("<typeshed>")
                    && definition.file.module_name.as_deref() == Some("builtins")
                {
                    Some(format!("builtins.{}", definition.binding().name))
                } else {
                    None
                }
            }
        }
        _ => None,
    }
}
pub(super) fn intact(file: &FileInfo, name: &str, cutoff: rustpy_core::TextSize) -> bool {
    let mut remaining = 20000;
    // Statements only: expressions, parameter lists and decorators hold no
    // assignment statements, and library modules (FastAPI's documented
    // `Annotated[...]` parameters) are mostly expression nodes.
    let mut pending = vec![file.parse.syntax_node()];
    while let Some(node) = pending.pop() {
        if remaining == 0 {
            return false;
        }
        remaining -= 1;
        if node.text_range().start() >= cutoff {
            continue;
        }
        pending.extend(node.children().filter(|child| {
            !ast::Expr::can_cast(child.kind())
                && !matches!(
                    child.kind(),
                    K::PARAM_LIST | K::DECORATOR | K::TYPE_PARAM_LIST
                )
        }));
        let target = match node.kind() {
            K::ASSIGN_STMT => ast::AssignStmt::cast(node.clone()).map(|s| s.targets()),
            K::ANN_ASSIGN_STMT => {
                ast::AnnAssignStmt::cast(node.clone()).map(|s| s.target().into_iter().collect())
            }
            K::AUG_ASSIGN_STMT => {
                ast::AugAssignStmt::cast(node.clone()).map(|s| s.target().into_iter().collect())
            }
            K::DEL_STMT => Some(node.children().filter_map(ast::Expr::cast).collect()),
            _ => None,
        };
        if target.is_some_and(|targets| {
            targets.iter().any(|t| {
                t.syntax()
                    .descendants()
                    .filter_map(ast::AttributeExpr::cast)
                    .any(|a| a.attr_text() == name)
            })
        }) {
            return false;
        }
    }
    true
}
/// Typed DB-API cursor/socket receivers require a real imported nominal owner.
pub(super) fn receiver_module(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    expr: &ast::Expr,
) -> Option<String> {
    let ty = inf.type_of_expr(file, expr);
    let Type::Instance(class, _) = ty else {
        return None;
    };
    let owner = inf.db.file(class.file)?;
    if !rustpy_semantic::usages::is_library_file(inf.db, &owner) {
        return None;
    }
    owner.module_name.clone()
}
