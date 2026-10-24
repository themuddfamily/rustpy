//! Reference/name checks complementary to semantic unresolved references and the
//! existing flow, member-visibility, unused-import and method inspections.
use super::{full_lines, ts, InspectionOptions, QuickFix, Sink};
use rustpy_core::{Severity, TextRange};
use rustpy_semantic::{
    db::{Analysis, FileInfo},
    infer::Inferer,
    scope::{BindingId, BindingKind, ScopeId, ScopeKind},
    types::{ClassRef, LiteralValue, Type},
};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind as K, SyntaxNode,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    sync::Arc,
};

pub(super) fn run(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    options: &InspectionOptions,
    sink: &mut Sink,
) {
    if options.unused_variables {
        unused_declarations(inf, file, sink);
    }
    if options.probable_bugs {
        early_globals(inf, file, sink);
        overwritten_values(file, sink);
        for node in file.parse.syntax_node().descendants() {
            if let Some(call) = ast::CallExpr::cast(node.clone()) {
                string_reference(inf, file, &call, sink);
                super_arguments(inf, file, &call, sink);
            }
            if let Some(name) = ast::NameExpr::cast(node) {
                self_outside(inf, file, &name, sink);
            }
        }
        imports(inf.db, file, options.unused_imports, options.pep8, sink);
    }
    if options.code_smells {
        all_contract(inf, file, sink);
    }
    if options.pep8 {
        lexical_style(file, sink);
    }
}
fn reads(file: &FileInfo, read: &HashSet<BindingId>, scope: ScopeId, name: &str) -> bool {
    file.scopes
        .bindings_in(scope, name)
        .iter()
        .any(|id| read.contains(id))
}
/// The outermost node spanning exactly `range` (the first one in preorder),
/// found from the covering element instead of a whole-file walk per scope.
fn node_spanning(root: &SyntaxNode, range: TextRange) -> Option<SyntaxNode> {
    if !root.text_range().contains_range(range) {
        return None;
    }
    let start = match root.covering_element(range) {
        rustpy_syntax::rowan::NodeOrToken::Node(node) => node,
        rustpy_syntax::rowan::NodeOrToken::Token(token) => token.parent()?,
    };
    start
        .ancestors()
        .filter(|node| node.text_range() == range)
        .last()
}
fn unused_declarations(inf: &Inferer<'_>, file: &Arc<FileInfo>, sink: &mut Sink) {
    let root = file.parse.syntax_node();
    // Bindings some load resolves to, collected once (not per binding).
    let read: HashSet<BindingId> = file
        .scopes
        .references
        .iter()
        .filter(|r| !r.is_write)
        .filter_map(|r| r.resolved)
        .collect();
    for (index, scope) in file.scopes.scopes.iter().enumerate() {
        if !matches!(
            scope.kind,
            ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Comprehension
        ) {
            continue;
        }
        let sid = ScopeId(index as u32);
        let owner = node_spanning(&root, scope.range);
        let function = owner.clone().and_then(ast::FuncDef::cast);
        let method = function.as_ref().is_some_and(|f| {
            f.syntax()
                .parent()
                .and_then(|b| b.parent())
                .is_some_and(|c| c.kind() == K::CLASS_DEF)
        });
        let signature_only = function.as_ref().is_some_and(|function| {
            function.decorators().next().is_some()
                || function.body().is_none_or(|body| {
                    body.statements().all(|statement| match statement {
                        ast::Stmt::Pass(_) | ast::Stmt::Raise(_) => true,
                        ast::Stmt::Expr(expr) => matches!(
                            expr.expr(),
                            Some(ast::Expr::String(_)) | Some(ast::Expr::Literal(_))
                        ),
                        _ => false,
                    })
                })
        });
        let override_method = if let Some(f) = &function {
            if let Some(c) = f
                .syntax()
                .parent()
                .and_then(|b| b.parent())
                .and_then(ast::ClassDef::cast)
            {
                let class = ClassRef {
                    file: file.id,
                    range: c.text_range(),
                    name: c.name_text().into(),
                };
                inf.has_unknown_base(&class)
                    || inf.mro(&class).iter().skip(1).any(|base| {
                        inf.class_info(base)
                            .is_some_and(|info| info.members.contains_key(f.name_text().as_str()))
                    })
            } else {
                false
            }
        } else {
            false
        };
        let first = function
            .as_ref()
            .and_then(|f| f.params())
            .and_then(|p| p.params().next())
            .and_then(|p| p.name())
            .map(|n| n.text_range());
        for (name, ids) in &scope.bindings {
            if name.starts_with('_')
                || scope.globals.contains(name)
                || scope.nonlocals.contains(name)
                || reads(file, &read, sid, name)
            {
                continue;
            }
            let Some(binding) = ids.first().map(|id| file.scopes.binding(*id)) else {
                continue;
            };
            let (code, what) = match binding.kind {
                BindingKind::Param | BindingKind::StarParam | BindingKind::StarStarParam
                    if !signature_only
                        && !override_method
                        && !(method && first == Some(binding.range)) =>
                {
                    ("PyUnusedParameter", "Parameter")
                }
                BindingKind::Function => ("PyUnusedLocalDefinition", "Local function"),
                BindingKind::Class => ("PyUnusedLocalDefinition", "Local class"),
                BindingKind::For
                | BindingKind::With
                | BindingKind::Except
                | BindingKind::CompTarget => ("PyUnusedLocal", "Local variable"),
                _ => continue,
            };
            sink.push(
                binding.range,
                Severity::WeakWarning,
                code,
                format!("{what} '{name}' is not used"),
                Vec::new(),
            );
        }
    }
}
fn eager_module_reference(file: &FileInfo, scope: ScopeId) -> bool {
    let mut current = Some(scope);
    while let Some(id) = current {
        let scope = file.scopes.scope(id);
        if matches!(
            scope.kind,
            ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Comprehension
        ) {
            return false;
        }
        current = scope.parent;
    }
    true
}
fn early_globals(inf: &Inferer<'_>, file: &FileInfo, sink: &mut Sink) {
    let module = file.scopes.module_scope();
    for reference in &file.scopes.references {
        if reference.is_write || !eager_module_reference(file, reference.scope) {
            continue;
        }
        let element = file.parse.syntax_node().covering_element(reference.range);
        let parent = match element {
            rustpy_syntax::rowan::NodeOrToken::Node(node) => Some(node),
            rustpy_syntax::rowan::NodeOrToken::Token(token) => token.parent(),
        };
        if parent.is_some_and(|node| {
            node.ancestors().any(|ancestor| {
                matches!(
                    ancestor.kind(),
                    K::ANNOTATION | K::RETURN_ANNOTATION | K::TYPE_PARAM_LIST
                )
            })
        }) {
            continue;
        }
        if inf.db.builtins().is_some_and(|builtins| {
            builtins
                .scopes
                .scope(builtins.scopes.module_scope())
                .bindings
                .contains_key(reference.name.as_str())
        }) {
            continue;
        }
        let Some(id) = reference.resolved else {
            continue;
        };
        let binding = file.scopes.binding(id);
        if binding.scope != module {
            continue;
        }
        let earliest = file
            .scopes
            .bindings_in(module, &reference.name)
            .iter()
            .map(|id| file.scopes.binding(*id))
            .filter(|b| b.is_definition())
            .min_by_key(|b| b.range.start());
        if earliest.is_some_and(|b| {
            reference.range.start() < b.range.start()
                || b.value
                    .is_some_and(|value| value.contains_range(reference.range))
                || (matches!(b.kind, BindingKind::Class | BindingKind::Function)
                    && b.node_range.contains_range(reference.range))
        }) {
            sink.push(
                reference.range,
                Severity::Warning,
                "PyGlobalUsedBeforeAssignment",
                format!(
                    "Global name '{}' is used before its definition",
                    reference.name
                ),
                Vec::new(),
            );
        }
    }
}
fn overwritten_values(file: &FileInfo, sink: &mut Sink) {
    for scope in &file.scopes.scopes {
        for (name, ids) in &scope.bindings {
            if name.starts_with('_')
                || scope.globals.contains(name)
                || scope.nonlocals.contains(name)
            {
                continue;
            }
            let definitions: Vec<_> = ids
                .iter()
                .map(|id| file.scopes.binding(*id))
                .filter(|b| b.is_definition())
                .collect();
            for pair in definitions.windows(2) {
                let (old, new) = (pair[0], pair[1]);
                if !matches!(
                    old.kind,
                    BindingKind::Assignment
                        | BindingKind::Import(_)
                        | BindingKind::For
                        | BindingKind::With
                ) || !matches!(new.kind, BindingKind::Assignment)
                {
                    continue;
                }
                // Conditional/nested blocks are alternatives, not a guaranteed
                // overwrite. RHS reads of the next assignment still use old.
                let root = file.parse.syntax_node();
                // Import bindings own an alias and with-target bindings own a
                // WITH_ITEM. Compare their executing statements, rather than
                // treating those nested binding nodes as sibling statements.
                let statement = |range| {
                    rustpy_semantic::edit_helpers::node_with_range(&root, range)
                        .and_then(|node| rustpy_semantic::edit_helpers::stmt_of(&node))
                        .map(|statement| statement.syntax().clone())
                };
                let old_node = statement(old.node_range);
                let new_node = statement(new.node_range);
                let target_overwrite = matches!(old.kind, BindingKind::For | BindingKind::With)
                    && old_node
                        .as_ref()
                        .zip(new_node.as_ref())
                        .is_some_and(|(a, b)| {
                            // A conditional/nested block may keep the acquired
                            // value; only an immediate suite write guarantees the
                            // overwrite before the next read.
                            b.parent().and_then(|body| body.parent()).as_ref() == Some(a)
                        });
                if !target_overwrite
                    && !old_node
                        .zip(new_node)
                        .is_some_and(|(a, b)| a.parent() == b.parent())
                {
                    continue;
                }
                let used = file.scopes.references.iter().any(|r| {
                    !r.is_write
                        && r.name == *name
                        && r.range.start() > old.range.end()
                        && r.range.start() < new.node_range.end()
                        && (r.scope == old.scope || r.resolved.is_some_and(|id| ids.contains(&id)))
                });
                if used {
                    continue;
                }
                let (code, message) = if matches!(old.kind, BindingKind::Import(_)) {
                    (
                        "PyImportShadowed",
                        format!("Assignment shadows imported name '{name}' before it is used"),
                    )
                } else if target_overwrite {
                    (
                        "PyLoopOrWithVariableAssignment",
                        format!(
                            "Loop/with variable '{name}' is reassigned before its acquired value is used"
                        ),
                    )
                } else if scope.kind == ScopeKind::Function {
                    (
                        "PyRedeclaration",
                        format!("Value of '{name}' is redeclared without being used"),
                    )
                } else {
                    continue;
                };
                sink.push(old.range, Severity::WeakWarning, code, message, Vec::new());
            }
        }
    }
}
fn canonical_builtin(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    callee: &ast::Expr,
    name: &str,
) -> bool {
    let mut scope = Some(file.scopes.scope_at(callee.text_range().start()));
    let mut star = false;
    while let Some(id) = scope {
        let current = file.scopes.scope(id);
        star |= !current.star_imports.is_empty();
        scope = current.parent;
    }
    if let ast::Expr::Name(n) = callee {
        if !star
            && n.name_text() == name
            && file
                .scopes
                .resolve(file.scopes.scope_at(n.text_range().start()), name)
                .is_none()
        {
            return true;
        }
    }
    let value = inf.type_of_expr(file, callee);
    if let Type::Class(class, _) = &value {
        return class.name == name
            && inf.db.file(class.file).is_some_and(|f| {
                f.is_stub
                    && f.path.starts_with("<typeshed>")
                    && f.module_name.as_deref() == Some("builtins")
            });
    }
    value
        .signature()
        .and_then(|s| s.def.as_ref())
        .is_some_and(|d| {
            d.name == name
                && inf.db.file(d.file).is_some_and(|f| {
                    f.is_stub
                        && f.path.starts_with("<typeshed>")
                        && f.module_name.as_deref() == Some("builtins")
                })
        })
}
fn closed_type(inf: &Inferer<'_>, ty: &Type) -> bool {
    match ty {
        Type::Class(c, _) | Type::Instance(c, _) | Type::SelfType(c) => {
            !inf.has_unknown_base(c)
                && !inf.mro(c).iter().any(|base| {
                    inf.class_info(base).is_some_and(|info| {
                        info.members.contains_key("__getattr__")
                            || (base.name != "object"
                                && info.members.contains_key("__getattribute__"))
                    })
                })
        }
        Type::Module(id) => inf.db.file(*id).is_some_and(|f| {
            !f.is_stub
                && !f
                    .scopes
                    .scope(f.scopes.module_scope())
                    .bindings
                    .contains_key("__getattr__")
        }),
        Type::Literal(LiteralValue::EnumMember { class, .. }) => {
            closed_type(inf, &Type::Instance(class.clone(), Vec::new()))
        }
        Type::Literal(_) | Type::None => true,
        _ => false,
    }
}
fn string_reference(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    call: &ast::CallExpr,
    sink: &mut Sink,
) {
    let Some(callee) = call.callee() else { return };
    let builtin = if canonical_builtin(inf, file, &callee, "getattr") {
        "getattr"
    } else if canonical_builtin(inf, file, &callee, "delattr") {
        "delattr"
    } else {
        return;
    };
    let Some(args) = call.arg_list() else { return };
    let args: Vec<_> = args.args().collect();
    if args.len() != 2
        || args
            .iter()
            .any(|a| !matches!(a, ast::AnyArg::Positional(_)))
    {
        return;
    }
    let (Some(object), Some(ast::Expr::String(name))) =
        (args[0].value(), args[1].value().map(|v| v.unparen()))
    else {
        return;
    };
    if name.is_bytes() || name.syntax().text().len() > rustpy_core::TextSize::new(4096) {
        return;
    }
    let Ok(value) = rustpy_syntax::string_literal::text(&name.raw_text()) else {
        return;
    };
    let ty = inf.type_of_expr(file, &object);
    if !closed_type(inf, &ty) || inf.lookup_member(&ty, &value).is_some() {
        return;
    }
    sink.push(
        name.content_range(),
        Severity::Warning,
        "PyUnresolvedReferences",
        format!(
            "Cannot find attribute '{value}' used by {builtin}() in '{}'",
            ty
        ),
        Vec::new(),
    );
}
fn super_arguments(inf: &Inferer<'_>, file: &Arc<FileInfo>, call: &ast::CallExpr, sink: &mut Sink) {
    let Some(callee) = call.callee() else { return };
    if !canonical_builtin(inf, file, &callee, "super") {
        return;
    }
    let Some(args) = call.arg_list() else { return };
    let args: Vec<_> = args.args().collect();
    if args.is_empty() {
        return;
    }
    if args.len() > 2
        || args
            .iter()
            .any(|a| !matches!(a, ast::AnyArg::Positional(_)))
    {
        sink.push(
            call.text_range(),
            Severity::Error,
            "PySuperArguments",
            "super() expects zero, one or two positional arguments",
            Vec::new(),
        );
        return;
    }
    let Some(first) = args[0].value() else { return };
    let first = inf.type_of_expr(file, &first);
    let Type::Class(base, _) = first else {
        if matches!(
            first,
            Type::None
                | Type::Literal(_)
                | Type::Instance(_, _)
                | Type::Tuple(_)
                | Type::Function(_)
                | Type::Callable(_)
                | Type::BoundMethod(_, _)
                | Type::Module(_)
        ) {
            sink.push(
                call.text_range(),
                Severity::Error,
                "PySuperArguments",
                "The first argument to super() must be a class",
                Vec::new(),
            );
        }
        return;
    };
    if args.len() == 2 {
        let Some(receiver) = args[1].value() else {
            return;
        };
        let receiver = inf.type_of_expr(file, &receiver);
        if receiver == Type::None {
            sink.push(
                call.text_range(),
                Severity::Error,
                "PySuperArguments",
                "The second argument to super() must be an instance or subtype of the first",
                Vec::new(),
            );
        }
        if let Some(class) = receiver.class_ref() {
            if !inf.has_unknown_base(class) && !inf.mro(class).contains(&base) {
                sink.push(
                    call.text_range(),
                    Severity::Error,
                    "PySuperArguments",
                    "The second argument to super() must be an instance or subtype of the first",
                    Vec::new(),
                );
            }
        }
    }
}
fn self_outside(inf: &Inferer<'_>, file: &Arc<FileInfo>, name: &ast::NameExpr, sink: &mut Sink) {
    if name.name_text() != "self" {
        return;
    }
    let scope = file.scopes.scope_at(name.text_range().start());
    if file.scopes.resolve(scope, "self").is_some()
        || inf.resolve_name(file, scope, "self").is_some()
    {
        return;
    }
    sink.push(
        name.text_range(),
        Severity::Warning,
        "PySelfOutsideMethod",
        "'self' is only implicitly meaningful as a declared method parameter",
        Vec::new(),
    );
}
fn all_contract(inf: &Inferer<'_>, file: &Arc<FileInfo>, sink: &mut Sink) {
    let root = file.parse.syntax_node();
    for binding in &file.scopes.bindings {
        if binding.name != "__all__" || binding.scope != file.scopes.module_scope() {
            continue;
        }
        let Some(range) = binding.value else { continue };
        let Some(value) =
            rustpy_semantic::edit_helpers::node_with_range(&root, range).and_then(ast::Expr::cast)
        else {
            continue;
        };
        let value = value.unparen();
        match value.clone() {
            ast::Expr::List(_) | ast::Expr::Tuple(_) => {
                for element in value.syntax().children().filter_map(ast::Expr::cast) {
                    if matches!(element.clone().unparen(), ast::Expr::String(string) if string.is_bytes())
                        || matches!(
                            element.clone().unparen(),
                            ast::Expr::Literal(_)
                                | ast::Expr::Dict(_)
                                | ast::Expr::List(_)
                                | ast::Expr::Tuple(_)
                                | ast::Expr::Set(_)
                        )
                    {
                        sink.push(
                            element.text_range(),
                            Severity::Warning,
                            "PyInvalidAll",
                            "__all__ must contain only strings",
                            Vec::new(),
                        );
                    }
                }
            }
            ast::Expr::String(_)
            | ast::Expr::Literal(_)
            | ast::Expr::Dict(_)
            | ast::Expr::Set(_) => sink.push(
                value.text_range(),
                Severity::Warning,
                "PyInvalidAll",
                "__all__ must be a list or tuple of strings",
                Vec::new(),
            ),
            _ => {}
        }
    }
    // The shared evaluator handles aliases, concatenation and imported exports;
    // never inspect unrelated descendant strings as declarations of exports.
    let Some(exports) = rustpy_semantic::imports::evaluated_exports(inf.db, file) else {
        return;
    };
    if !file
        .scopes
        .scope(file.scopes.module_scope())
        .star_imports
        .is_empty()
        || file
            .scopes
            .scope(file.scopes.module_scope())
            .bindings
            .contains_key("__getattr__")
    {
        return;
    }
    for name in exports.names {
        if inf.resolve_in_module(file, &name, 0).is_some() {
            continue;
        }
        if let Some(binding) = file
            .scopes
            .scope(file.scopes.module_scope())
            .bindings
            .get("__all__")
            .and_then(|ids| ids.last())
            .map(|id| file.scopes.binding(*id))
        {
            sink.push(
                root.descendants()
                    .filter_map(ast::StringExpr::cast)
                    .find(|string| {
                        string.value() == name
                            && string.syntax().ancestors().any(|node| {
                                ast::AssignStmt::cast(node).is_some_and(|a| {
                                    a.targets().iter().any(
                                    |t| matches!(t,ast::Expr::Name(n) if n.name_text()=="__all__"),
                                )
                                })
                            })
                    })
                    .map(|s| s.text_range())
                    .unwrap_or(binding.range),
                Severity::Warning,
                "PyUnresolvedReferences",
                format!("Unresolved reference '{name}' in __all__"),
                Vec::new(),
            );
        }
    }
}

/// Typeshed stubs, module-context copies and other synthesized files.
fn virtual_path(path: &Path) -> bool {
    path.to_str().is_some_and(|p| p.starts_with('<'))
}

/// `PyModuleNotOnPath`: a module file that no project, interpreter or stub
/// root contains, i.e. one found only relative to the importing file (such an
/// import fails when the code runs as part of the project or from sys.path).
fn outside_import_roots(db: &Analysis, target: &FileInfo) -> bool {
    let resolver = &db.resolver;
    !virtual_path(&target.path)
        && target.module_origin.is_none()
        && !(resolver.roots.is_empty() && resolver.search_paths.is_empty())
        && !resolver.is_on_import_path(&target.path)
}

/// `PyImportShadowsModule`: an absolute import whose top-level module resolves
/// to a project file that hides the standard-library or installed module of
/// the same name (a local `random.py` captures `import random`).
fn shadowing_imports(db: &Analysis, file: &FileInfo, node: &SyntaxNode, sink: &mut Sink) {
    let tops: Vec<_> = if let Some(import) = ast::ImportStmt::cast(node.clone()) {
        import.aliases().filter_map(|a| a.name_token()).collect()
    } else if let Some(import) = ast::ImportFromStmt::cast(node.clone()) {
        if import.level() > 0 {
            return;
        }
        import
            .module()
            .and_then(|m| m.first_token())
            .into_iter()
            .collect()
    } else {
        return;
    };
    let own_top = file
        .module_name
        .as_deref()
        .and_then(|m| m.split('.').next());
    let resolver = db.resolver.for_path(&file.path);
    for token in tops {
        let top = token.text();
        if own_top == Some(top) || top == "__future__" {
            continue;
        }
        let Some(target) = db.import_module(top) else {
            continue;
        };
        if virtual_path(&target.path) || target.module_origin.is_some() || target.id == file.id {
            continue;
        }
        let Some(root) = db.resolver.project_root_of(&target.path) else {
            continue;
        };
        let library = if rustpy_semantic::typeshed::is_stdlib(top)
            && rustpy_semantic::typeshed::module_available(top, resolver.python_version)
        {
            "standard library"
        } else if resolver
            .resolve_source(top)
            .is_some_and(|source| source != target.path && !source.starts_with(root))
        {
            "installed"
        } else {
            continue;
        };
        let shown = target.path.strip_prefix(root).unwrap_or(&target.path);
        sink.push(
            token.text_range(),
            Severity::Warning,
            "PyImportShadowsModule",
            format!(
                "Import '{top}' resolves to project file '{}', which shadows the {library} module '{top}'",
                shown.display()
            ),
            Vec::new(),
        );
    }
}

/// `PyImportShadowsModule` in a package `__init__`: an import that binds the
/// name of one of the package's own submodules to something else, so that
/// `package.name` changes once the submodule itself is imported.
fn init_submodule_rebinds(db: &Analysis, file: &FileInfo, node: &SyntaxNode, sink: &mut Sink) {
    let Some(package_dir) = file.path.parent() else {
        return;
    };
    let submodule_of = |name: &str| -> Option<Arc<FileInfo>> {
        let sub = db.module_relative(file, 1, name)?;
        let first = sub
            .path
            .strip_prefix(package_dir)
            .ok()?
            .components()
            .next()?;
        let first = Path::new(first.as_os_str());
        let owned = first.as_os_str() == name
            || first.file_stem().and_then(|s| s.to_str()) == Some(name)
                && first.extension().is_some_and(|e| e == "py" || e == "pyi");
        (owned && sub.id != file.id && !virtual_path(&sub.path)).then_some(sub)
    };
    let same = |a: Option<Arc<FileInfo>>, b: &FileInfo| {
        a.is_some_and(|a| a.id == b.id || a.path == b.path)
    };
    let mut report = |alias: &ast::ImportAlias, bound: &str| {
        let Some(token) = alias.alias().or_else(|| alias.name_token()) else {
            return;
        };
        let package = file.module_name.clone().unwrap_or_else(|| {
            package_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        sink.push(
            token.text_range(),
            Severity::Warning,
            "PyImportShadowsModule",
            format!("Import binds '{bound}', shadowing the submodule '{package}.{bound}' of this package"),
            Vec::new(),
        );
    };
    if let Some(import) = ast::ImportStmt::cast(node.clone()) {
        for alias in import.aliases() {
            let Some(bound) = alias.bound_name() else {
                continue;
            };
            let Some(sub) = submodule_of(&bound) else {
                continue;
            };
            // `import pkg.sub as sub` binds the submodule itself.
            let name = if alias.alias().is_some() {
                alias.name()
            } else {
                bound.clone()
            };
            if !same(db.module_relative(file, 0, &name), &sub) {
                report(&alias, &bound);
            }
        }
    } else if let Some(import) = ast::ImportFromStmt::cast(node.clone()) {
        if import.is_star() {
            return;
        }
        let level = import.level();
        let module = import.module().map(|m| m.path()).unwrap_or_default();
        let Some(source) = db.module_relative(file, level, &module) else {
            return;
        };
        for alias in import.aliases() {
            let Some(bound) = alias.bound_name() else {
                continue;
            };
            let Some(sub) = submodule_of(&bound) else {
                continue;
            };
            let name = alias.name();
            let full = if module.is_empty() {
                name.clone()
            } else {
                format!("{module}.{name}")
            };
            // `from . import sub` binds the submodule; `from .sub import sub`
            // is the deliberate re-export idiom.
            if same(db.module_relative(file, level, &full), &sub)
                || name == bound && same(Some(source.clone()), &sub)
            {
                continue;
            }
            report(&alias, &bound);
        }
    }
}

fn import_targets(db: &Analysis, file: &FileInfo, node: &SyntaxNode) -> Vec<Arc<FileInfo>> {
    if let Some(import) = ast::ImportStmt::cast(node.clone()) {
        return import
            .aliases()
            .filter_map(|a| db.import_module(&a.name()))
            .collect();
    }
    let Some(import) = ast::ImportFromStmt::cast(node.clone()) else {
        return Vec::new();
    };
    let name = import.module().map(|m| m.path()).unwrap_or_default();
    let target = if import.level() == 0 {
        db.import_module(&name)
    } else {
        db.module_relative(file, import.level(), &name)
    };
    let mut targets = Vec::new();
    if let Some(target) = target {
        // `from pkg import child` may import a submodule if the package does
        // not already supply that attribute. Follow that actual import edge.
        let inf = Inferer::new(db);
        for alias in import.aliases() {
            if inf.resolve_in_module(&target, &alias.name(), 0).is_none() {
                if let Some(module) = target
                    .module_name
                    .as_ref()
                    .and_then(|m| db.import_module(&format!("{m}.{}", alias.name())))
                {
                    targets.push(module);
                }
            }
        }
        targets.push(target);
    }
    targets
}
fn import_cycle(db: &Analysis, start: &Arc<FileInfo>, target: &Arc<FileInfo>) -> bool {
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([(target.clone(), 0usize)]);
    let mut edges = 0;
    while let Some((file, depth)) = queue.pop_front() {
        if file.id == start.id {
            return true;
        }
        if file.is_stub || depth >= 16 || !seen.insert(file.id) {
            continue;
        }
        if seen.len() > 64 {
            return false;
        }
        for node in file
            .parse
            .syntax_node()
            .children()
            .filter(|n| matches!(n.kind(), K::IMPORT_STMT | K::IMPORT_FROM_STMT))
        {
            for next in import_targets(db, &file, &node) {
                edges += 1;
                if edges > 256 {
                    return false;
                }
                queue.push_back((next, depth + 1));
            }
        }
    }
    false
}
fn imports(db: &Analysis, file: &Arc<FileInfo>, unused: bool, order: bool, sink: &mut Sink) {
    let root = file.parse.syntax_node();
    let mut duplicates: HashMap<(ScopeId, String, String), TextRange> = HashMap::new();
    for binding in &file.scopes.bindings {
        let BindingKind::Import(import) = &binding.kind else {
            continue;
        };
        let key = (
            binding.scope,
            format!(
                "{}:{}:{}",
                import.level,
                import.module,
                import.name.as_deref().unwrap_or("")
            ),
            binding.name.to_string(),
        );
        if let Some(prior) = duplicates.insert(key, binding.node_range) {
            let same_block = rustpy_semantic::edit_helpers::node_with_range(&root, prior)
                .zip(rustpy_semantic::edit_helpers::node_with_range(
                    &root,
                    binding.node_range,
                ))
                .is_some_and(|(a, b)| {
                    let a = a
                        .ancestors()
                        .find(|n| matches!(n.kind(), K::IMPORT_STMT | K::IMPORT_FROM_STMT));
                    let b = b
                        .ancestors()
                        .find(|n| matches!(n.kind(), K::IMPORT_STMT | K::IMPORT_FROM_STMT));
                    a.zip(b)
                        .is_some_and(|(a, b)| a == b || a.next_sibling().as_ref() == Some(&b))
                });
            if same_block {
                let edits = super::remove_import_fix(file, &binding.node_range);
                sink.push(
                    binding.range,
                    Severity::WeakWarning,
                    "PyDuplicateImport",
                    format!("Redundant import of '{}'", binding.name),
                    if edits.is_empty() {
                        Vec::new()
                    } else {
                        vec![QuickFix {
                            title: "Remove redundant import".into(),
                            edits,
                        }]
                    },
                );
            }
        }
    }
    for node in root
        .descendants()
        .filter(|n| matches!(n.kind(), K::IMPORT_STMT | K::IMPORT_FROM_STMT))
    {
        let scope = file.scopes.scope_at(node.text_range().start());
        if matches!(
            file.scopes.scope(scope).kind,
            ScopeKind::Function | ScopeKind::Class
        ) {
            sink.push(node.text_range(),Severity::Info,"PyLocalImport","Import inside a function or class; consider module-level imports unless lazy loading is intentional",Vec::new());
        }
        let targets = import_targets(db, file, &node);
        if targets
            .iter()
            .any(|target| target.id == file.id || target.path == file.path)
        {
            sink.push(
                node.text_range(),
                Severity::Warning,
                "PyImportSelf",
                "Import resolves to its containing file",
                Vec::new(),
            );
        } else if targets.iter().any(|target| import_cycle(db, file, target)) {
            sink.push(
                node.text_range(),
                Severity::WeakWarning,
                "PyCyclicImport",
                "Cyclic import dependency (partially initialized module may be observed)",
                Vec::new(),
            );
        }
        if let Some(target) = targets
            .iter()
            .find(|target| outside_import_roots(db, target))
        {
            // Namespace directories are represented by a virtual `__init__.py`.
            let named = if target.is_package_init() {
                target.path.parent()
            } else {
                Some(target.path.as_path())
            };
            let shown = target
                .module_name
                .clone()
                .or_else(|| {
                    named
                        .and_then(Path::file_stem)
                        .map(|s| s.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            sink.push(
                node.text_range(),
                Severity::Warning,
                "PyModuleNotOnPath",
                format!(
                    "Module '{shown}' is not in the project and not on sys.path ({})",
                    target.path.display()
                ),
                Vec::new(),
            );
        }
        shadowing_imports(db, file, &node, sink);
        if file.is_package_init() && file.scopes.scope(scope).kind == ScopeKind::Module {
            init_submodule_rebinds(db, file, &node, sink);
        }
        if let Some(import) = ast::ImportFromStmt::cast(node.clone()) {
            if import.is_star() {
                sink.push(
                    node.text_range(),
                    Severity::WeakWarning,
                    "PyWildcardImport",
                    "Wildcard import obscures which names are defined",
                    Vec::new(),
                );
                if unused && !file.is_package_init() {
                    if let Some(target) = targets.last() {
                        let exports = rustpy_semantic::imports::evaluated_exports(db, target);
                        if exports.as_ref().is_some_and(|e| !e.exact)
                            || target
                                .scopes
                                .scope(target.scopes.module_scope())
                                .bindings
                                .contains_key("__getattr__")
                        {
                            continue;
                        }
                        let names: HashSet<String> = exports
                            .map(|e| e.names)
                            .unwrap_or_else(|| {
                                target
                                    .scopes
                                    .scope(target.scopes.module_scope())
                                    .bindings
                                    .keys()
                                    .filter(|n| !n.starts_with('_'))
                                    .map(ToString::to_string)
                                    .collect()
                            })
                            .into_iter()
                            .collect();
                        let forwarded = rustpy_semantic::imports::dunder_all_with(db, file);
                        let used = file
                            .scopes
                            .references
                            .iter()
                            .any(|r| !r.is_write && names.contains(r.name.as_str()))
                            || forwarded.iter().any(|n| names.contains(n));
                        if !used {
                            sink.push(
                                node.text_range(),
                                Severity::Warning,
                                "PyUnusedImports",
                                "Unused wildcard import",
                                vec![QuickFix {
                                    title: "Remove unused wildcard import".into(),
                                    edits: vec![(
                                        full_lines(&file.text, node.text_range()),
                                        String::new(),
                                    )],
                                }],
                            );
                        }
                    }
                }
            }
        }
    }
    if order {
        let edits = rustpy_format::imports::optimize_imports(file, &[]);
        if !edits.is_empty() {
            let range = edits[0].0;
            sink.push(
                range,
                Severity::WeakWarning,
                "PyUnsortedImports",
                "Imports are not arranged in the configured import style",
                vec![QuickFix {
                    title: "Optimize imports".into(),
                    edits,
                }],
            );
        }
    }
}
fn lexical_style(file: &FileInfo, sink: &mut Sink) {
    let mut depth = 0usize;
    let root = file.parse.syntax_node();
    let tokens: Vec<_> = root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .collect();
    for (index, token) in tokens.iter().enumerate() {
        match token.kind() {
            K::L_PAREN | K::L_BRACK | K::L_BRACE => depth = depth.saturating_add(1),
            K::R_PAREN | K::R_BRACK | K::R_BRACE => depth = depth.saturating_sub(1),
            K::SEMICOLON => {
                let trailing = tokens
                    .iter()
                    .skip(index + 1)
                    .find(|t| !t.kind().is_trivia())
                    .is_none_or(|t| matches!(t.kind(), K::NEWLINE | K::EOF | K::DEDENT));
                sink.push(
                    token.text_range(),
                    Severity::WeakWarning,
                    "PyUnnecessarySemicolon",
                    if trailing {
                        "Unnecessary trailing semicolon"
                    } else {
                        "Separate statements onto different lines instead of using a semicolon"
                    },
                    if trailing {
                        vec![QuickFix {
                            title: "Remove unnecessary semicolon".into(),
                            edits: vec![(token.text_range(), String::new())],
                        }]
                    } else {
                        Vec::new()
                    },
                );
            }
            K::WHITESPACE
                if depth > 0
                    && (token.text().contains("\\\n") || token.text().contains("\\\r\n")) =>
            {
                let offset = token.text().find('\\').unwrap_or(0);
                let start = usize::from(token.text_range().start()) + offset;
                // Remove only the slash, preserving the newline, indentation,
                // comments, and Unicode byte offsets of implicit line joining.
                let range = TextRange::new(ts(start), ts(start + 1));
                sink.push(
                    range,
                    Severity::WeakWarning,
                    "PyUnnecessaryBackslash",
                    "Backslash is unnecessary inside parentheses, brackets or braces",
                    vec![QuickFix {
                        title: "Remove unnecessary backslash".into(),
                        edits: vec![(range, String::new())],
                    }],
                );
            }
            _ => {}
        }
    }
}
