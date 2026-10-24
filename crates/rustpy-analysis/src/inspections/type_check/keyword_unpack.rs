//! PEP692 projection of canonical Unpack[TypedDict] into keyword-only fields.
use super::*;
use rustc_hash::{FxHashMap, FxHashSet};
use rustpy_semantic::scope::ScopeId;
use rustpy_semantic::types::ClassRef;
use std::path::Path;

pub(super) fn expand(inf: &Inferer<'_>, checkable: &mut Checkable) -> Result<bool, ()> {
    let original_signature = checkable.sig.clone();
    let original_annotations = checkable.annotated.clone();
    match expand_known(inf, checkable) {
        Ok(expanded) => Ok(expanded),
        Err(()) => {
            let definition = original_signature.def.as_ref().ok_or(())?;
            let file = inf.db.file(definition.file).ok_or(())?;
            let function = rustpy_semantic::edit_helpers::node_with_range(
                &file.parse.syntax_node(),
                definition.range,
            )
            .and_then(ast::FuncDef::cast)
            .ok_or(())?;
            if function.decorators().next().is_some()
                || function
                    .syntax()
                    .ancestors()
                    .filter_map(ast::ClassDef::cast)
                    .any(|class| {
                        class.arguments().is_some_and(|arguments| {
                            arguments.keywords().any(|keyword| {
                                keyword
                                    .name()
                                    .is_some_and(|name| name.text() == "metaclass")
                            })
                        })
                    })
            {
                return Err(());
            }
            // Unknown keyword schemas do not erase independently known ordinary
            // parameter contracts. Retain the original binder, with only the
            // unproven **kwargs type checking disabled.
            checkable.sig = original_signature;
            checkable.annotated = original_annotations;
            let kwargs: Vec<_> = checkable
                .sig
                .params
                .iter()
                .filter(|parameter| parameter.kind == ParamKind::KwArgs)
                .map(|parameter| parameter.name.to_string())
                .collect();
            checkable.annotated.retain(|name| !kwargs.contains(name));
            Ok(false)
        }
    }
}
fn expand_known(inf: &Inferer<'_>, checkable: &mut Checkable) -> Result<bool, ()> {
    let Some(kwargs) = checkable
        .sig
        .params
        .iter()
        .find(|parameter| parameter.kind == ParamKind::KwArgs)
    else {
        return Ok(false);
    };
    let Some(definition) = &checkable.sig.def else {
        return Ok(false);
    };
    let file = inf.db.file(definition.file).ok_or(())?;
    let function =
        rustpy_semantic::edit_helpers::node_with_range(&file.parse.syntax_node(), definition.range)
            .and_then(ast::FuncDef::cast)
            .ok_or(())?;
    let annotation = function
        .params()
        .ok_or(())?
        .params()
        .find(|parameter| {
            matches!(parameter, ast::AnyParam::StarStar(_))
                && parameter
                    .name()
                    .is_some_and(|name| name.text() == kwargs.name.as_str())
        })
        .and_then(|parameter| parameter.annotation());
    let Some(annotation) = annotation else {
        return Ok(false);
    };
    let normalized = normalized_annotation(&annotation).ok_or(())?;
    let ast::Expr::Subscript(subscript) = normalized.unparen() else {
        return Ok(false);
    };
    let scope = file
        .scopes
        .scope_for_node(function.text_range())
        .and_then(|scope| file.scopes.scope(scope).parent)
        .unwrap_or(file.scopes.module_scope());
    let head = subscript.object().ok_or(())?;
    if !canonical(
        inf,
        &file,
        scope,
        &head,
        annotation.text_range().start(),
        &["Unpack"],
    ) {
        // A lookalike Unpack is never authority for a TypedDict field contract.
        return if matches!(head.clone().unparen(), ast::Expr::Name(name) if name.name_text() == "Unpack")
            || matches!(head.clone().unparen(), ast::Expr::Attribute(attribute) if attribute.attr_text() == "Unpack")
            || matches!(head.clone().unparen(), ast::Expr::Name(name) if inf.resolve_name(&file, scope, &name.name_text()).map(|definition|inf.follow_imports(definition)).is_some_and(|definition|definition.binding().name=="Unpack"))
        {
            Err(())
        } else {
            Ok(false)
        };
    }
    for decorator in function.decorators() {
        let expression = decorator.expr().ok_or(())?;
        let factory = matches!(expression.clone().unparen(), ast::Expr::Call(_));
        let head = match expression.clone().unparen() {
            ast::Expr::Call(call) => call.callee().ok_or(())?,
            other => other,
        };
        let before = expression.text_range().start();
        if factory
            && !canonical_in(
                inf,
                &file,
                scope,
                &head,
                before,
                &["lru_cache", "cache"],
                &["functools"],
            )
        {
            return Err(());
        }
        if !canonical_builtin(
            inf,
            &file,
            scope,
            &head,
            before,
            &["staticmethod", "classmethod"],
        ) && !canonical(
            inf,
            &file,
            scope,
            &head,
            before,
            &["overload", "final", "override"],
        ) && !canonical_in(
            inf,
            &file,
            scope,
            &head,
            before,
            &["lru_cache", "cache"],
            &["functools"],
        ) && !canonical_in(
            inf,
            &file,
            scope,
            &head,
            before,
            &["abstractmethod"],
            &["abc"],
        ) {
            return Err(());
        }
    }
    if function
        .syntax()
        .ancestors()
        .filter_map(ast::ClassDef::cast)
        .any(|class| {
            class.arguments().is_some_and(|arguments| {
                arguments.keywords().any(|keyword| {
                    keyword
                        .name()
                        .is_some_and(|name| name.text() == "metaclass")
                })
            })
        })
    {
        return Err(());
    }
    let operand = subscript.index_expr().ok_or(())?;
    if matches!(operand.clone().unparen(), ast::Expr::Tuple(_)) {
        return Err(());
    }
    let ty = match operand.clone().unparen() {
        ast::Expr::Name(name) => {
            let definition = inf
                .resolve_name(&file, scope, &name.name_text())
                .ok_or(())?;
            match inf.type_of_binding(&definition.file, definition.binding) {
                Type::Class(class, arguments) => Type::Instance(class, arguments),
                other => other,
            }
        }
        _ => inf.annotation_type(&file, &operand),
    };
    let Type::Instance(class, arguments) = ty else {
        return Err(());
    };
    if !arguments.is_empty() {
        return Err(());
    } // Generic field substitution needs its own captured provider.
    let mut required = FxHashMap::default();
    validate_typed_dict(
        inf,
        &class,
        &mut FxHashSet::default(),
        &mut required,
        0,
        &mut 256,
    )?;
    let (fields, kind) = inf.generated_constructor(&class).ok_or(())?;
    if kind != SynthKind::TypedDict
        || fields.params.len() != required.len()
        || fields.params.len() > 128
    {
        return Err(());
    }
    let mut signature = (*checkable.sig).clone();
    signature
        .params
        .retain(|parameter| parameter.kind != ParamKind::KwArgs);
    checkable
        .annotated
        .retain(|name| name != kwargs.name.as_str());
    for field in &fields.params {
        if signature.params.iter().any(|parameter| {
            parameter.name == field.name && parameter.kind != ParamKind::PositionalOnly
        }) {
            return Err(());
        }
        let mut field = field.clone();
        field.kind = ParamKind::KeywordOnly;
        field.has_default = !*required.get(field.name.as_str()).ok_or(())?;
        checkable.annotated.push(field.name.to_string());
        signature.params.push(field);
    }
    checkable.sig = Arc::new(signature);
    Ok(true)
}
fn canonical(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    scope: ScopeId,
    expression: &ast::Expr,
    before: rustpy_core::TextSize,
    names: &[&str],
) -> bool {
    canonical_in(
        inf,
        file,
        scope,
        expression,
        before,
        names,
        &["typing", "typing_extensions"],
    )
}
fn canonical_builtin(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    scope: ScopeId,
    expression: &ast::Expr,
    before: rustpy_core::TextSize,
    names: &[&str],
) -> bool {
    canonical_in(inf, file, scope, expression, before, names, &["builtins"])
}
fn canonical_in(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    scope: ScopeId,
    expression: &ast::Expr,
    before: rustpy_core::TextSize,
    names: &[&str],
    modules: &[&str],
) -> bool {
    let definition = match expression.clone().unparen() {
        ast::Expr::Name(name) => {
            if file
                .scopes
                .resolve(scope, &name.name_text())
                .is_some_and(|ids| {
                    ids.len() != 1 || file.scopes.binding(ids[0]).range.start() >= before
                })
            {
                return false;
            }
            inf.resolve_name(file, scope, &name.name_text())
        }
        ast::Expr::Attribute(attribute) => {
            let Some(ast::Expr::Name(name)) = attribute.object().map(ast::Expr::unparen) else {
                return false;
            };
            if module_member_written(file, &name.name_text(), &attribute.attr_text()) {
                return false;
            }
            let Some(ids) = file.scopes.resolve(scope, &name.name_text()) else {
                return false;
            };
            if ids.len() != 1 || file.scopes.binding(ids[0]).range.start() >= before {
                return false;
            }
            let Type::Module(id) = inf.type_of_binding(file, ids[0]) else {
                return false;
            };
            let Some(module) = inf.db.file(id) else {
                return false;
            };
            inf.resolve_name(
                &module,
                module.scopes.module_scope(),
                &attribute.attr_text(),
            )
        }
        _ => None,
    };
    definition
        .map(|definition| inf.follow_imports(definition))
        .is_some_and(|definition| {
            !module_member_written(file, "", definition.binding().name.as_str())
                && definition.file.is_stub
                && modules.iter().any(|module| {
                    definition.file.module_name.as_deref() == Some(*module)
                        && definition.file.path
                            == Path::new(&format!("<typeshed>/{}.pyi", module.replace('.', "/")))
                })
                && names.contains(&definition.binding().name.as_str())
        })
}
fn validate_typed_dict(
    inf: &Inferer<'_>,
    class: &ClassRef,
    active: &mut FxHashSet<ClassRef>,
    required: &mut FxHashMap<String, bool>,
    depth: u8,
    budget: &mut usize,
) -> Result<(), ()> {
    if depth >= 16 || *budget == 0 || !active.insert(class.clone()) {
        return Err(());
    }
    *budget -= 1;
    let file = inf.db.file(class.file).ok_or(())?;
    let node =
        rustpy_semantic::edit_helpers::node_with_range(&file.parse.syntax_node(), class.range)
            .and_then(ast::ClassDef::cast)
            .ok_or(())?;
    if node.decorators().next().is_some() {
        return Err(());
    }
    let mut total = true;
    if let Some(arguments) = node.arguments() {
        for keyword in arguments.keywords() {
            if keyword.name().is_none_or(|name| name.text() != "total") {
                return Err(());
            }
            total = match keyword.value().map(ast::Expr::unparen) {
                Some(ast::Expr::Literal(literal))
                    if literal.kind() == Some(SyntaxKind::TRUE_KW) =>
                {
                    true
                }
                Some(ast::Expr::Literal(literal))
                    if literal.kind() == Some(SyntaxKind::FALSE_KW) =>
                {
                    false
                }
                _ => return Err(()),
            };
        }
    }
    let class_scope = file.scopes.scope_for_node(class.range).ok_or(())?;
    let defining = file.scopes.scope(class_scope).parent.unwrap_or(class_scope);
    let mut has_base = false;
    for base in node.bases() {
        has_base = true;
        if canonical(
            inf,
            &file,
            defining,
            &base,
            base.text_range().start(),
            &["TypedDict"],
        ) {
            continue;
        }
        let Type::Class(parent, _) = inf.type_of_expr(&file, &base) else {
            return Err(());
        };
        validate_typed_dict(inf, &parent, active, required, depth + 1, budget)?;
    }
    if !has_base {
        return Err(());
    }
    for statement in node.body().ok_or(())?.statements() {
        if *budget == 0 || required.len() >= 128 {
            return Err(());
        }
        *budget -= 1;
        match statement {
            ast::Stmt::AnnAssign(assignment) => {
                if assignment.value().is_some() {
                    return Err(());
                }
                let Some(ast::Expr::Name(name)) = assignment.target().map(ast::Expr::unparen)
                else {
                    return Err(());
                };
                let annotation = assignment.annotation().ok_or(())?;
                let is_required = field_required(
                    inf,
                    &file,
                    class_scope,
                    &annotation,
                    annotation.text_range().start(),
                    total,
                    0,
                )?;
                if required.insert(name.name_text(), is_required).is_some() {
                    return Err(());
                }
            }
            ast::Stmt::Pass(_) => {}
            ast::Stmt::Expr(expression)
                if matches!(expression.expr(), Some(ast::Expr::String(_))) => {}
            _ => return Err(()),
        }
    }
    active.remove(class);
    Ok(())
}
fn field_required(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    scope: ScopeId,
    annotation: &ast::Expr,
    before: rustpy_core::TextSize,
    total: bool,
    depth: u8,
) -> Result<bool, ()> {
    if depth >= 8 {
        return Err(());
    }
    let normalized = normalized_annotation(annotation).ok_or(())?;
    let ast::Expr::Subscript(subscript) = normalized.unparen() else {
        return Ok(total);
    };
    let head = subscript.object().ok_or(())?;
    if canonical(inf, file, scope, &head, before, &["Required"]) {
        return Ok(true);
    }
    if canonical(inf, file, scope, &head, before, &["NotRequired"]) {
        return Ok(false);
    }
    if canonical(inf, file, scope, &head, before, &["ReadOnly"]) {
        return field_required(
            inf,
            file,
            scope,
            &subscript.index_expr().ok_or(())?,
            before,
            total,
            depth + 1,
        );
    }
    if canonical(inf, file, scope, &head, before, &["Annotated"]) {
        let Some(ast::Expr::Tuple(tuple)) = subscript.index_expr().map(ast::Expr::unparen) else {
            return Err(());
        };
        let mut elements = tuple.elements();
        let first = elements.next().ok_or(())?;
        if elements.next().is_none() {
            return Err(());
        }
        return field_required(inf, file, scope, &first, before, total, depth + 1);
    }
    Ok(total)
}

fn normalized_annotation(annotation: &ast::Expr) -> Option<ast::Expr> {
    if let ast::Expr::String(string) = annotation.clone().unparen() {
        let raw = string.syntax().text().to_string();
        if raw.len() > 4096 {
            return None;
        }
        let rustpy_syntax::string_literal::Decoded::Text(text, _) =
            rustpy_syntax::string_literal::string(&raw).ok()?
        else {
            return None;
        };
        let parsed = rustpy_syntax::parse_expression(&text);
        if !parsed.errors().is_empty() {
            return None;
        }
        parsed.syntax_node().children().find_map(ast::Expr::cast)
    } else {
        Some(annotation.clone())
    }
}
fn module_member_written(file: &FileInfo, _module: &str, member: &str) -> bool {
    // Namespace aliases and compound target stores are conservatively retired.
    // A member-named store through any receiver may be an alias; without a
    // separate receiver proof, declining authority is safer than retaining it.
    let mut budget = 8192usize;
    for node in file.parse.syntax_node().descendants() {
        if budget == 0 {
            return true;
        }
        budget -= 1;
        let Some(statement) = ast::Stmt::cast(node) else {
            continue;
        };
        let targets = match statement {
            ast::Stmt::Assign(assignment) => assignment.targets(),
            ast::Stmt::AnnAssign(assignment) => assignment.target().into_iter().collect(),
            ast::Stmt::AugAssign(assignment) => assignment.target().into_iter().collect(),
            ast::Stmt::Del(deletion) => deletion.targets(),
            _ => Vec::new(),
        };
        for target in targets {
            for node in target.syntax().descendants() {
                if budget == 0 {
                    return true;
                }
                budget -= 1;
                if ast::AttributeExpr::cast(node)
                    .is_some_and(|attribute| attribute.attr_text() == member)
                {
                    return true;
                }
            }
        }
    }
    false
}
pub(super) fn check_stars(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    arguments: &ast::ArgList,
    checkable: &Checkable,
    sink: &mut Sink,
) {
    let mut supplied: FxHashMap<String, (Type, bool)> = FxHashMap::default();
    let mut uncertain = false;
    let mut had_star = false;
    let explicit: FxHashSet<String> = arguments
        .args()
        .filter_map(|argument| {
            if let ast::AnyArg::Keyword(keyword) = argument {
                Some(keyword.name_text())
            } else {
                None
            }
        })
        .collect();
    for argument in arguments.args() {
        match argument {
            ast::AnyArg::StarStar(star) => {
                had_star = true;
                let Some(expression) = ast::AnyArg::StarStar(star).value() else {
                    uncertain = true;
                    continue;
                };
                let ty = inf.type_of_expr(file, &expression);
                if matches!(ty, Type::Unknown | Type::Any) {
                    uncertain = true;
                    continue;
                }
                let Type::Instance(class, _) = ty else {
                    uncertain = true;
                    continue;
                };
                let mut required = FxHashMap::default();
                if validate_typed_dict(
                    inf,
                    &class,
                    &mut FxHashSet::default(),
                    &mut required,
                    0,
                    &mut 256,
                )
                .is_err()
                {
                    if class.name == "dict"
                        && inf.db.file(class.file).is_some_and(|file| {
                            file.is_stub && file.path == Path::new("<typeshed>/builtins.pyi")
                        })
                    {
                        sink.push(
                            expression.text_range(),
                            Severity::Error,
                            TYPES,
                            "Expected a TypedDict for Unpack keyword arguments",
                            Vec::new(),
                        );
                    }
                    uncertain = true;
                    continue;
                }
                let Some((signature, SynthKind::TypedDict)) = inf.generated_constructor(&class)
                else {
                    uncertain = true;
                    continue;
                };
                for parameter in &signature.params {
                    let is_required = required
                        .get(parameter.name.as_str())
                        .copied()
                        .unwrap_or(false);
                    if is_required
                        && (explicit.contains(parameter.name.as_str())
                            || supplied
                                .get(parameter.name.as_str())
                                .is_some_and(|(_, required)| *required))
                    {
                        sink.push(
                            expression.text_range(),
                            Severity::Error,
                            ARGS,
                            format!("Multiple values for keyword '{}'", parameter.name),
                            Vec::new(),
                        );
                    }
                    supplied
                        .entry(parameter.name.to_string())
                        .and_modify(|(ty, guaranteed)| {
                            *ty = Type::union(vec![ty.clone(), parameter.ty.clone()]);
                            *guaranteed |= is_required;
                        })
                        .or_insert((parameter.ty.clone(), is_required));
                    if let Some(expected) = checkable.sig.params.iter().find(|expected| {
                        expected.name == parameter.name
                            && expected.kind != ParamKind::PositionalOnly
                    }) {
                        if !inf.is_compatible(&parameter.ty, &expected.ty) {
                            sink.push(
                                expression.text_range(),
                                Severity::Error,
                                TYPES,
                                format!("Incompatible keyword field '{}'", parameter.name),
                                Vec::new(),
                            );
                        }
                    }
                }
            }
            ast::AnyArg::Star(_) => uncertain = true,
            _ => {}
        }
    }
    if had_star && !uncertain {
        let missing: Vec<_> = checkable
            .sig
            .params
            .iter()
            .filter(|parameter| {
                parameter.kind == ParamKind::KeywordOnly
                    && !parameter.has_default
                    && !explicit.contains(parameter.name.as_str())
                    && !supplied
                        .get(parameter.name.as_str())
                        .is_some_and(|(_, required)| *required)
            })
            .map(|parameter| parameter.name.to_string())
            .collect();
        if !missing.is_empty() {
            sink.push(
                arguments.text_range(),
                Severity::Error,
                ARGS,
                format!("Required keyword fields unfilled: {}", missing.join(", ")),
                Vec::new(),
            );
        }
    }
}
