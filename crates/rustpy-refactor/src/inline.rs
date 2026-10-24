//! Binding-aware Inline plans. Evaluation remains in Python, not in the IDE:
//! an immediately invoked lambda owns the original argument binding, so each
//! argument is evaluated once and keyword/default/star semantics are retained.
use crate::{change_signature, refactor2::MultiFileRefactoring};
use rustpy_core::{TextRange, TextSize};
use rustpy_search::usages::{find_usages, function_call_sites};
use rustpy_semantic::{
    db::{Analysis, FileInfo},
    edit_helpers::{self, ancestor_at, node_with_range, statement_removals, Edit},
    infer::Inferer,
    scope::{BindingKind, ScopeKind},
    usages::{self, Symbol},
};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind,
};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineOptions {
    pub this_only: bool,
    pub keep_declaration: bool,
}
#[derive(Clone, Debug)]
pub struct InlineModel {
    pub name: String,
    pub kind: &'static str,
    pub supports_this: bool,
}
fn origin(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Result<(Arc<FileInfo>, TextRange, BindingKind, String), String> {
    let symbol = usages::symbol_at(db, file, offset)
        .ok_or("Place the caret on a variable, parameter, function or property")?;
    match symbol {
        Symbol::Name { def, name } => {
            let def = usages::name_origin(&Inferer::new(db), &def);
            if matches!(def.binding().kind, BindingKind::Import(_)) {
                let candidates: Vec<_> = rustpy_semantic::features::definition_at(db, file, offset)
                    .into_iter()
                    .filter_map(|location| {
                        let owner = db.file_for_path(&location.path)?;
                        let (_, binding) = owner.scopes.binding_at(location.range.start())?;
                        (!matches!(binding.kind, BindingKind::Import(_))).then(|| {
                            (
                                owner.clone(),
                                binding.range,
                                binding.kind.clone(),
                                binding.name.to_string(),
                            )
                        })
                    })
                    .collect();
                if candidates.len() == 1 {
                    return Ok(candidates.into_iter().next().unwrap());
                }
                return Err("The imported Inline target is ambiguous".into());
            }
            Ok((
                def.file.clone(),
                def.range(),
                def.binding().kind.clone(),
                name,
            ))
        }
        Symbol::Member {
            decl: Some((owner, range)),
            name,
            ..
        } => {
            let (_, binding) = owner
                .scopes
                .binding_at(range.start())
                .ok_or("Cannot locate member declaration")?;
            Ok((owner.clone(), range, binding.kind.clone(), name))
        }
        _ => Err("The declaration is ambiguous or unavailable".into()),
    }
}
pub fn model(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Result<InlineModel, String> {
    let (owner, range, kind, name) = origin(db, file, offset)?;
    if usages::is_library_file(db, &owner) || owner.is_stub {
        return Err("Navigate to writable source before inlining".into());
    }
    let kind = match kind {
        BindingKind::Function => {
            if ancestor_at::<ast::FuncDef>(&owner.parse.syntax_node(), range.start())
                .is_some_and(|f| f.has_decorator("property"))
            {
                "property"
            } else {
                "function / method"
            }
        }
        BindingKind::Param => "parameter",
        BindingKind::Assignment => {
            if owner
                .scopes
                .scope(owner.scopes.binding_at(range.start()).unwrap().1.scope)
                .kind
                == ScopeKind::Module
            {
                "constant"
            } else {
                "local variable"
            }
        }
        _ => return Err("This declaration cannot be inlined".into()),
    };
    Ok(InlineModel {
        name,
        kind,
        supports_this: kind != "parameter",
    })
}
pub fn plan(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    files: &[PathBuf],
    options: InlineOptions,
) -> Result<MultiFileRefactoring, String> {
    let (owner, range, kind, name) = origin(db, file, offset)?;
    model(db, file, offset)?;
    if !options.keep_declaration && !options.this_only && !matches!(kind, BindingKind::Param) {
        let exports = owner
            .scopes
            .bindings_in(owner.scopes.module_scope(), "__all__");
        for id in exports {
            let binding = owner.scopes.binding(*id);
            let Some(value) = binding
                .value
                .and_then(|r| node_with_range(&owner.parse.syntax_node(), r))
                .and_then(ast::Expr::cast)
            else {
                return Err("Dynamic exports require keeping the declaration".into());
            };
            if !matches!(value, ast::Expr::List(_) | ast::Expr::Tuple(_))
                || value
                    .syntax()
                    .descendants()
                    .filter_map(ast::StringExpr::cast)
                    .any(|s| s.value() == name)
            {
                return Err("The declaration is exported by __all__; keep its declaration".into());
            }
        }
    }
    let mut result = match kind {
        BindingKind::Assignment => inline_value(db, file, offset, &owner, range, files, options)?,
        BindingKind::Param => inline_parameter(db, &owner, range, files)?,
        BindingKind::Function => inline_function(db, file, offset, &owner, range, files, options)?,
        _ => return Err("Unsupported Inline target".into()),
    };
    result.title = format!("Inline {name}");
    if result
        .changes
        .iter()
        .flat_map(|(_, edits)| edits)
        .map(|(_, text)| text.len())
        .sum::<usize>()
        > 8 * 1024 * 1024
    {
        return Err("Inline expansion exceeds the 8 MiB edit budget".into());
    }
    for (_, edits) in &mut result.changes {
        edits.sort_by_key(|(r, _)| (r.start(), r.end()));
        edits.dedup();
        for pair in edits.windows(2) {
            if pair[0].0.end() > pair[1].0.start() {
                return Err("Inline replacements overlap (recursive or nested invocations); inline one invocation first".into());
            }
        }
    }
    Ok(result)
}
fn add(
    changes: &mut BTreeMap<PathBuf, Vec<Edit>>,
    file: &FileInfo,
    range: TextRange,
    text: String,
) {
    changes
        .entry(file.path.clone())
        .or_default()
        .push((range, text));
}
fn immutable(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Literal(_) | ast::Expr::String(_) | ast::Expr::ConcatString(_) => true,
        ast::Expr::Tuple(t) => t.elements().all(|e| immutable(&e)),
        ast::Expr::Paren(p) => p.inner().is_some_and(|e| immutable(&e)),
        ast::Expr::Unary(u) => u.operand().is_some_and(|e| immutable(&e)),
        _ => false,
    }
}
fn inline_value(
    db: &Analysis,
    caller: &Arc<FileInfo>,
    offset: TextSize,
    owner: &Arc<FileInfo>,
    range: TextRange,
    files: &[PathBuf],
    options: InlineOptions,
) -> Result<MultiFileRefactoring, String> {
    let root = owner.parse.syntax_node();
    let (id, binding) = owner
        .scopes
        .binding_at(range.start())
        .ok_or("Missing variable binding")?;
    if owner
        .scopes
        .bindings_in(binding.scope, &binding.name)
        .iter()
        .filter(|id| owner.scopes.binding(**id).is_definition())
        .count()
        != 1
    {
        return Err("The variable is assigned more than once".into());
    }
    let declaration = node_with_range(&root, binding.node_range).ok_or("Missing assignment")?;
    let value = if let Some(assign) = ast::AssignStmt::cast(declaration.clone()) {
        if assign.targets().len() != 1 || !matches!(assign.targets()[0], ast::Expr::Name(_)) {
            return Err("Inline needs one simple variable target".into());
        }
        assign.value()
    } else {
        ast::AnnAssignStmt::cast(declaration.clone()).and_then(|a| a.value())
    }
    .ok_or("Declaration has no value")?;
    let local = owner.scopes.scope(binding.scope).kind != ScopeKind::Module;
    let mut usages = find_usages(db, owner, range.start(), files);
    let imports = paired_imports(db, owner, range, files);
    for (file, id) in &imports {
        for reference in file.scopes.references_to(*id) {
            usages.push(usages::usage_at(
                file,
                reference.range,
                reference.is_write,
                false,
            ));
        }
    }
    usages.sort_by_key(|usage| (usage.path.clone(), usage.range.start(), usage.range.end()));
    usages.dedup_by(|a, b| a.path == b.path && a.range == b.range);
    if usages.iter().any(|u| {
        u.dynamic
            || (u.is_write
                && (u.path != owner.path || u.range != binding.range)
                && db.file_for_path(&u.path).is_none_or(|file| {
                    ancestor_at::<ast::ImportAlias>(&file.parse.syntax_node(), u.range.start())
                        .is_none()
                }))
    }) {
        return Err("A dynamic or writing usage prevents safe Inline".into());
    }
    let reads: Vec<_> = usages
        .iter()
        .filter(|u| !u.is_definition && !u.is_write)
        .filter(|usage| {
            db.file_for_path(&usage.path).is_none_or(|file| {
                ancestor_at::<ast::ImportAlias>(&file.parse.syntax_node(), usage.range.start())
                    .is_none()
            })
        })
        .collect();
    let selected: Vec<_> = reads
        .iter()
        .copied()
        .filter(|u| {
            !options.this_only || (u.path == caller.path && u.range.contains_inclusive(offset))
        })
        .collect();
    if selected.is_empty() {
        return Err("Place the caret on the usage to inline, or choose all usages".into());
    }
    if selected
        .len()
        .saturating_mul(usize::from(value.text_range().len()).saturating_add(16))
        > 8 * 1024 * 1024
    {
        return Err("Inline value expansion exceeds 8 MiB".into());
    }
    let repeatable = immutable(&value);
    if !repeatable {
        if !local || reads.len() != 1 || options.keep_declaration {
            return Err("Inlining this initializer could repeat or reorder side effects; use a single adjacent read without keeping the declaration".into());
        }
        let read = reads[0];
        let node = node_with_range(&root, read.range).ok_or("Missing variable read")?;
        let statement = edit_helpers::stmt_of(&node).ok_or("Missing read statement")?;
        let direct = match &statement {
            ast::Stmt::Return(r) => r.value().is_some_and(|e| e.text_range() == read.range),
            ast::Stmt::Assign(a) => a.value().is_some_and(|e| e.text_range() == read.range),
            ast::Stmt::Expr(e) => e.expr().is_some_and(|e| e.text_range() == read.range),
            _ => false,
        };
        let gap = &owner.text[usize::from(declaration.text_range().end())
            ..usize::from(statement.text_range().start())];
        if !direct || !gap.trim().is_empty() {
            return Err("The initializer has effects or mutable dependencies; its single read must be the next direct expression".into());
        }
    }
    let mut changes = BTreeMap::new();
    for usage in selected {
        let target = db
            .file_for_path(&usage.path)
            .ok_or("A usage file is unavailable")?;
        let node =
            node_with_range(&target.parse.syntax_node(), usage.range).ok_or("A usage changed")?;
        let expr = node
            .ancestors()
            .find_map(ast::Expr::cast)
            .ok_or("A usage is not an expression")?;
        if usage.path == owner.path && usage.range.start() < binding.range.start() {
            return Err("A read precedes the initializer".into());
        }
        // Immutable constants can cross modules; an imported alias is replaced
        // at its value reads, never by deleting or rewriting unrelated imports.
        add(
            &mut changes,
            &target,
            expr.text_range(),
            format!("({})", value.syntax().text()),
        );
    }
    if !options.keep_declaration && !options.this_only {
        for (file, id) in &imports {
            remove_import_usage(file, file.scopes.binding(*id).range, &mut changes);
        }
        for usage in usages.iter().filter(|u| !u.is_definition) {
            if let Some(file) = db.file_for_path(&usage.path) {
                remove_import_usage(&file, usage.range, &mut changes);
            }
        }
        for (r, s) in statement_removals(&owner.text, &[declaration]) {
            add(&mut changes, owner, r, s)
        }
    } else if !options.keep_declaration && reads.len() == 1 {
        for (r, s) in statement_removals(&owner.text, &[declaration]) {
            add(&mut changes, owner, r, s)
        }
    }
    let _ = id;
    Ok(MultiFileRefactoring {
        title: String::new(),
        changes: changes.into_iter().collect(),
    })
}

/// Translate acyclic statement/return trees into a Python expression, keeping
/// branch evaluation lazy. Unsupported control transfers become conflicts.
fn suite(statements: &[ast::Stmt], fallback: &str, budget: &mut usize) -> Result<String, String> {
    let result = suite_inner(statements, fallback, budget)?;
    if result.len() > 262_144 {
        return Err("Inline expansion exceeds 256 KiB".into());
    }
    Ok(result)
}
fn suite_inner(
    statements: &[ast::Stmt],
    fallback: &str,
    budget: &mut usize,
) -> Result<String, String> {
    if *budget == 0 {
        return Err("Inline body exceeds its structural budget".into());
    }
    *budget -= 1;
    let Some((first, rest)) = statements.split_first() else {
        return Ok(fallback.into());
    };
    match first {
        ast::Stmt::Return(r)=>Ok(r.value().map(|e|e.syntax().text().to_string()).unwrap_or_else(||"None".into())),
        ast::Stmt::Pass(_)=>suite(rest,fallback,budget),
        ast::Stmt::Expr(s)=>{let e=s.expr().ok_or("Missing expression")?;if matches!(e,ast::Expr::String(_)|ast::Expr::ConcatString(_)){return suite(rest,fallback,budget)}let after=suite(rest,fallback,budget)?;Ok(format!("(({}), ({after}))[-1]",e.syntax().text()))},
        ast::Stmt::Assign(s)=>{let targets=s.targets();if targets.len()!=1||!matches!(targets[0],ast::Expr::Name(_)){return Err("Inline cannot preserve this destructuring/chained assignment".into())}let value=s.value().ok_or("Missing assigned value")?;let after=suite(rest,fallback,budget)?;Ok(format!("(({} := {}), ({after}))[-1]",targets[0].syntax().text(),value.syntax().text()))},
        ast::Stmt::AnnAssign(s)=>{let target=s.target().ok_or("Missing annotated target")?;if !matches!(target,ast::Expr::Name(_)){return Err("Inline cannot rewrite an annotated attribute".into())}let Some(value)=s.value()else{return suite(rest,fallback,budget)};let after=suite(rest,fallback,budget)?;Ok(format!("(({} := {}), ({after}))[-1]",target.syntax().text(),value.syntax().text()))},
        ast::Stmt::If(s)=>{let after=suite(rest,fallback,budget)?;let mut otherwise=s.else_clause().and_then(|e|e.body()).map(|b|suite(&b.statements().collect::<Vec<_>>(),&after,budget)).transpose()?.unwrap_or(after.clone());let branches:Vec<_>=s.elif_clauses().collect();for branch in branches.into_iter().rev(){let body=branch.body().ok_or("Missing elif body")?;let result=suite(&body.statements().collect::<Vec<_>>(),&after,budget)?;otherwise=format!("({result} if {} else {otherwise})",branch.condition().ok_or("Missing condition")?.syntax().text());}let result=suite(&s.body().ok_or("Missing if body")?.statements().collect::<Vec<_>>(),&after,budget)?;Ok(format!("({result} if {} else {otherwise})",s.condition().ok_or("Missing condition")?.syntax().text()))},
        _=>Err("This body contains a loop, try/with, nested scope, raise or another control transfer that cannot be safely expressed inline".into()),
    }
}

fn rewrite_expression(
    expression: &str,
    names: &BTreeMap<String, String>,
    super_value: Option<&str>,
) -> Result<String, String> {
    let parsed = rustpy_syntax::parse(expression);
    if !parsed.errors().is_empty() {
        return Err("Inline expression cannot be represented safely".into());
    }
    let root = parsed.syntax_node();
    let mut edits = Vec::new();
    let mut covered = Vec::new();
    if let Some(value) = super_value {
        for call in root.descendants().filter_map(ast::CallExpr::cast) {
            if matches!(call.callee(),Some(ast::Expr::Name(ref n)) if n.name_text()=="super")
                && call
                    .arg_list()
                    .is_some_and(|args| args.args().next().is_none())
            {
                covered.push(call.text_range());
                edits.push((call.text_range(), value.into()));
            }
        }
    }
    for name in root.descendants().filter_map(ast::NameExpr::cast) {
        if covered.iter().any(|r| r.contains_range(name.text_range())) {
            continue;
        }
        if let Some(value) = names.get(&name.name_text()) {
            edits.push((name.text_range(), value.clone()));
        }
    }
    if expression
        .len()
        .saturating_add(edits.iter().map(|(_, text)| text.len()).sum::<usize>())
        > 8 * 1024 * 1024
    {
        return Err("Inline dependency expansion exceeds 8 MiB".into());
    }
    edit_helpers::try_apply_edits(expression, &edits)
}
fn same_definition(
    left: &rustpy_semantic::infer::Definition,
    right: &rustpy_semantic::infer::Definition,
) -> bool {
    left.file.id == right.file.id && left.binding == right.binding
}
fn adapt_body(
    db: &Analysis,
    owner: &Arc<FileInfo>,
    function: &ast::FuncDef,
    target: &Arc<FileInfo>,
    at: TextSize,
    body: &str,
    changes: &mut BTreeMap<PathBuf, Vec<Edit>>,
) -> Result<String, String> {
    let inf = Inferer::new(db);
    let function_scope = owner
        .scopes
        .scope_for_node(function.text_range())
        .ok_or("Missing function scope")?;
    let block = function.body().ok_or("Missing function body")?;
    let locals: HashSet<_> = owner
        .scopes
        .bindings
        .iter()
        .filter(|binding| {
            block.text_range().contains_range(binding.range) || binding.scope == function_scope
        })
        .map(|binding| binding.name.to_string())
        .collect();
    let target_scope = target.scopes.scope_at(at);
    let mut names = BTreeMap::new();
    let mut module_alias = None;
    for reference in owner.scopes.references.iter().filter(|r| {
        block.text_range().contains_range(r.range)
            && !r.is_write
            && !locals.contains(r.name.as_str())
    }) {
        let name = reference.name.as_str();
        if matches!(name, "locals" | "globals" | "eval" | "exec" | "__class__") {
            return Err("Frame/closure introspection cannot be relocated by Inline".into());
        }
        let source = inf
            .resolve_name(owner, reference.scope, name)
            .ok_or("An inline dependency cannot be resolved")?;
        if let Some(actual) = inf.resolve_name(target, target_scope, name) {
            if same_definition(&source, &actual) {
                continue;
            }
        }
        // Reuse an existing import alias if it actually denotes the dependency.
        let alias = target.scopes.bindings.iter().find_map(|binding| {
            inf.resolve_name(target, target_scope, &binding.name)
                .filter(|actual| same_definition(&source, actual))
                .map(|_| binding.name.to_string())
        });
        if let Some(alias) = alias {
            names.insert(name.into(), alias);
            continue;
        }
        if owner.scopes.scope(source.binding().scope).kind != ScopeKind::Module
            || source.file.id != owner.id
            || owner.id == target.id
        {
            return Err(format!(
                "Moving '{name}' would capture another binding or a closure cell"
            ));
        }
        let module = owner
            .module_name
            .as_ref()
            .filter(|name| !name.is_empty())
            .ok_or("The defining module has no importable name")?;
        let alias = module_alias
            .get_or_insert_with(|| {
                let mut n = 0;
                loop {
                    let candidate = format!("_inline_module_{n}");
                    if !target.scopes.bindings.iter().any(|b| b.name == candidate) {
                        break candidate;
                    }
                    n += 1
                }
            })
            .clone();
        names.insert(name.into(), format!("{alias}.{name}"));
        let imports = changes.entry(target.path.clone()).or_default();
        let text = format!("import {module} as {alias}\n");
        if !imports.iter().any(|(_, existing)| existing == &text) {
            imports.push((
                TextRange::empty(rustpy_semantic::import_placement::import_insert_offset(
                    target,
                )),
                text,
            ));
        }
    }
    let super_value = if body.contains("super") {
        let uses_super = owner
            .scopes
            .references
            .iter()
            .any(|r| block.text_range().contains_range(r.range) && r.name == "super");
        if uses_super {
            let actual = inf
                .resolve_name(owner, function_scope, "super")
                .ok_or("Cannot resolve super")?;
            if actual.file.module_name.as_deref() != Some("builtins")
                || !actual.file.path.starts_with("<typeshed>")
            {
                return Err("A shadowed super() cannot be rewritten".into());
            }
            let class = function
                .syntax()
                .ancestors()
                .skip(1)
                .find_map(ast::ClassDef::cast)
                .ok_or("super() needs a class method")?;
            let receiver = function
                .params()
                .and_then(|p| p.plain_params().next())
                .map(|p| p.name_text())
                .ok_or("super() needs a receiver")?;
            let class_token = class.name().ok_or("Missing class name")?;
            let (_, class_binding) = owner
                .scopes
                .binding_at(class_token.text_range().start())
                .ok_or("Missing defining class binding")?;
            if owner.scopes.scope(class_binding.scope).kind != ScopeKind::Module
                || owner
                    .scopes
                    .bindings_in(class_binding.scope, &class_binding.name)
                    .len()
                    != 1
            {
                return Err(
                    "A nested/rebound defining class prevents explicit super relocation".into(),
                );
            }
            let class_name = if owner.id == target.id {
                let actual = inf
                    .resolve_name(target, target_scope, &class_binding.name)
                    .ok_or("Defining class is unavailable at this call")?;
                if actual.file.id != owner.id || actual.range() != class_binding.range {
                    return Err("The defining class is shadowed at this call".into());
                }
                class.name_text()
            } else {
                let module = owner.module_name.as_ref().ok_or("Missing class module")?;
                let alias = "_inline_super_owner";
                if target.scopes.bindings.iter().any(|b| b.name == alias) {
                    return Err("Inline super owner name is already used".into());
                }
                let import = format!("import {module} as {alias}\n");
                let imports = changes.entry(target.path.clone()).or_default();
                if !imports.iter().any(|(_, text)| text == &import) {
                    imports.push((
                        TextRange::empty(rustpy_semantic::import_placement::import_insert_offset(
                            target,
                        )),
                        import,
                    ));
                }
                format!("{alias}.{}", class.name_text())
            };
            Some(format!("super({class_name}, {receiver})"))
        } else {
            None
        }
    } else {
        None
    };
    rewrite_expression(body, &names, super_value.as_deref())
}
fn allowed_decorators(
    db: &Analysis,
    file: &Arc<FileInfo>,
    function: &ast::FuncDef,
) -> Result<(), String> {
    let inf = Inferer::new(db);
    for decorator in function.decorators() {
        let expr = decorator.expr().ok_or("Missing decorator")?;
        if !matches!(expr, ast::Expr::Name(_)) {
            return Err(
                "Qualified/compound decorators require preserving their descriptor binding".into(),
            );
        }
        let ty = inf.type_of_expr(file, &expr);
        let rustpy_semantic::types::Type::Class(class, _) = ty else {
            return Err("An unknown decorator may replace the function".into());
        };
        if !matches!(
            class.name.as_str(),
            "staticmethod" | "classmethod" | "property"
        ) || db.file(class.file).is_none_or(|f| {
            f.module_name.as_deref() != Some("builtins") || !f.path.starts_with("<typeshed>")
        }) {
            return Err("A transforming decorator prevents Inline".into());
        }
    }
    Ok(())
}
fn remove_import_usage(
    file: &FileInfo,
    range: TextRange,
    changes: &mut BTreeMap<PathBuf, Vec<Edit>>,
) -> bool {
    let Some(alias) = ancestor_at::<ast::ImportAlias>(&file.parse.syntax_node(), range.start())
    else {
        return false;
    };
    let Some(statement) = alias.syntax().parent().and_then(ast::ImportFromStmt::cast) else {
        return false;
    };
    if statement.aliases().count() == 1 {
        for (r, s) in statement_removals(&file.text, &[statement.syntax().clone()]) {
            add(changes, file, r, s)
        }
    } else {
        add(
            changes,
            file,
            edit_helpers::element_with_comma(&file.text, alias.syntax()),
            String::new(),
        )
    }
    true
}
fn inline_function(
    db: &Analysis,
    caller: &Arc<FileInfo>,
    offset: TextSize,
    owner: &Arc<FileInfo>,
    range: TextRange,
    files: &[PathBuf],
    options: InlineOptions,
) -> Result<MultiFileRefactoring, String> {
    let function = ancestor_at::<ast::FuncDef>(&owner.parse.syntax_node(), range.start())
        .ok_or("Missing function")?;
    allowed_decorators(db, owner, &function)?;
    method_authority(db, owner, &function, files)?;
    if function.is_async() || function.type_params().is_some() {
        return Err("Async/generic function frame semantics cannot be represented by a synchronous inline expression".into());
    }
    let block = function.body().ok_or("Missing function body")?;
    if usize::from(block.text_range().len()) > 262_144 {
        return Err("Inline body exceeds 256 KiB".into());
    }
    if block.syntax().descendants().any(|node| {
        matches!(
            node.kind(),
            SyntaxKind::YIELD_EXPR
                | SyntaxKind::YIELD_FROM_EXPR
                | SyntaxKind::AWAIT_EXPR
                | SyntaxKind::GLOBAL_STMT
                | SyntaxKind::NONLOCAL_STMT
                | SyntaxKind::FUNC_DEF
                | SyntaxKind::CLASS_DEF
                | SyntaxKind::LAMBDA_EXPR
                | SyntaxKind::LIST_COMP
                | SyntaxKind::DICT_COMP
                | SyntaxKind::SET_COMP
                | SyntaxKind::GENERATOR_EXPR
        )
    }) {
        return Err(
            "Generators, nested scopes and global/nonlocal writers cannot be relocated".into(),
        );
    }
    let mut signature = change_signature::signature_model(owner, range.start())?;
    for parameter in &mut signature.params {
        parameter.annotation = None;
        if let Some(default) = &parameter.default {
            let parse = rustpy_syntax::parse(default);
            let expr = parse
                .syntax_node()
                .descendants()
                .find_map(ast::Expr::cast)
                .ok_or("Malformed default")?;
            if !immutable(&expr) {
                return Err("A mutable or evaluated default must keep its original identity; Inline cannot rebuild it".into());
            }
        }
    }
    let body = suite(&block.statements().collect::<Vec<_>>(), "None", &mut 512)?;
    let property = function.has_decorator("property");
    let mut calls = function_call_sites(db, owner, &function, files);
    if property {
        // A property is a descriptor access, not a CallExpr. Its selected or
        // all statically resolved read accesses are lowered with receiver once.
        let usages = find_usages(db, owner, range.start(), files);
        if usages.len().saturating_mul(
            body.len()
                .saturating_add(signature.render().len())
                .saturating_add(128),
        ) > 8 * 1024 * 1024
        {
            return Err("Property expansion exceeds 8 MiB".into());
        }
        let mut changes = BTreeMap::new();
        let mut count = 0;
        for usage in usages.iter().filter(|u| !u.is_definition) {
            if usage.dynamic || usage.is_write {
                return Err(
                    "A writing/dynamic property usage requires keeping the descriptor".into(),
                );
            }
            if options.this_only
                && (usage.path != caller.path || !usage.range.contains_inclusive(offset))
            {
                continue;
            }
            let target = db
                .file_for_path(&usage.path)
                .ok_or("Missing property usage")?;
            let Some(attribute) =
                ancestor_at::<ast::AttributeExpr>(&target.parse.syntax_node(), usage.range.start())
            else {
                continue;
            };
            let receiver = attribute.object().ok_or("Missing property receiver")?;
            let expression = adapt_body(
                db,
                owner,
                &function,
                &target,
                attribute.text_range().start(),
                &body,
                &mut changes,
            )?;
            add(
                &mut changes,
                &target,
                attribute.text_range(),
                format!(
                    "(lambda {}: {expression})({})",
                    signature.render(),
                    receiver.syntax().text()
                ),
            );
            count += 1;
        }
        if count == 0 {
            return Err("No resolved property read is selected".into());
        }
        if !options.keep_declaration && !options.this_only {
            for (r, s) in statement_removals(&owner.text, &[function.syntax().clone()]) {
                add(&mut changes, owner, r, s)
            }
        }
        return Ok(MultiFileRefactoring {
            title: String::new(),
            changes: changes.into_iter().collect(),
        });
    }
    if calls.len() > 2048 {
        return Err("Inline affects more than 2048 invocations; inline a smaller scope".into());
    }
    let all_calls = calls.clone();
    if options.this_only {
        calls.retain(|site| {
            site.file.path == caller.path && site.call.text_range().contains_inclusive(offset)
        });
        calls.sort_by_key(|site| site.call.text_range().len());
        calls.truncate(1);
    }
    if calls.is_empty() {
        return Err("No resolved invocation is selected".into());
    }
    if calls
        .iter()
        .map(|site| {
            usize::from(site.call.text_range().len())
                .saturating_add(body.len())
                .saturating_add(signature.render().len())
                .saturating_add(128)
        })
        .sum::<usize>()
        > 8 * 1024 * 1024
    {
        return Err("Inline invocation expansion exceeds 8 MiB".into());
    }
    let mut changes = BTreeMap::new();
    for site in &calls {
        if site.unresolved {
            return Err("An unknown receiver prevents Inline".into());
        }
        if site.file.id == owner.id && function.text_range().contains_range(site.call.text_range())
        {
            return Err("Recursive invocations cannot be inlined as a finite expression".into());
        }
        let callee = site.call.callee().ok_or("Missing callee")?;
        let mut acquisition = None;
        let receiver = if site.implicit != 0 {
            let ast::Expr::Attribute(attribute) = &callee else {
                return Err("Cannot preserve this implicit receiver".into());
            };
            let object = attribute.object().ok_or("Missing receiver")?;
            let super_object = if let ast::Expr::Call(super_call) = &object {
                super_call.callee().is_some_and(|callee|matches!(Inferer::new(db).type_of_expr(&site.file,&callee),rustpy_semantic::types::Type::Class(ref class,_) if class.name=="super" && db.file(class.file).is_some_and(|f|f.module_name.as_deref()==Some("builtins") && f.path.starts_with("<typeshed>"))))
            } else {
                false
            };
            if super_object {
                let mut index = 0;
                let fresh = loop {
                    let candidate = format!("_inline_super_{index}");
                    if !owner.text.contains(&candidate) && !site.file.text.contains(&candidate) {
                        break candidate;
                    }
                    index += 1
                };
                acquisition = Some(format!("({fresh} := {})", object.syntax().text()));
                Some(format!(
                    "{fresh}.{}",
                    if function.has_decorator("classmethod") {
                        "__self_class__"
                    } else {
                        "__self__"
                    }
                ))
            } else {
                if function.has_decorator("classmethod") {
                    let ty = Inferer::new(db).type_of_expr(&site.file, &object);
                    if !matches!(ty, rustpy_semantic::types::Type::Class(_, _)) {
                        return Err(
                            "Inline a classmethod through its class to preserve cls binding".into(),
                        );
                    }
                }
                Some(object.syntax().text().to_string())
            }
        } else {
            None
        };
        if usize::from(site.call.text_range().len()) > 262_144 {
            return Err("Inline invocation exceeds 256 KiB".into());
        }
        let arguments = site.call.arg_list().ok_or("Missing arguments")?;
        let raw = arguments.syntax().text().to_string();
        let raw = raw
            .trim()
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(raw.trim());
        let actuals = match receiver {
            Some(receiver) if raw.trim().is_empty() => receiver,
            Some(receiver) => format!("{receiver}, {raw}"),
            None => raw.into(),
        };
        let expression = adapt_body(
            db,
            owner,
            &function,
            &site.file,
            site.call.text_range().start(),
            &body,
            &mut changes,
        )?;
        let mut replacement = format!("(lambda {}: {expression})({actuals})", signature.render());
        if site.implicit == 0 {
            if let ast::Expr::Attribute(attribute) = &callee {
                let receiver = attribute.object().ok_or("Missing attribute receiver")?;
                replacement = format!("({}, {replacement})[-1]", receiver.syntax().text());
            }
        }
        if let Some(acquisition) = acquisition {
            replacement = format!("({acquisition}, {replacement})[-1]");
        }
        add(
            &mut changes,
            &site.file,
            site.call.text_range(),
            replacement,
        );
    }
    if !options.keep_declaration && !options.this_only {
        for (file, id) in paired_imports(db, owner, range, files) {
            if file.scopes.references_to(id).iter().any(|reference| {
                !all_calls.iter().any(|site| {
                    site.file.path == file.path
                        && site.call.text_range().contains_range(reference.range)
                })
            }) {
                return Err("An imported callable escapes; keep its declaration".into());
            }
            remove_import_usage(&file, file.scopes.binding(id).range, &mut changes);
        }
        for usage in find_usages(db, owner, range.start(), files)
            .iter()
            .filter(|u| !u.is_definition)
        {
            if all_calls.iter().any(|site| {
                site.file.path == usage.path && site.call.text_range().contains_range(usage.range)
            }) {
                continue;
            }
            let target = db
                .file_for_path(&usage.path)
                .ok_or("Missing function usage")?;
            if !remove_import_usage(&target, usage.range, &mut changes) {
                return Err(
                    "The function escapes as a value or has non-call usages; keep its declaration"
                        .into(),
                );
            }
        }
        for (r, s) in statement_removals(&owner.text, &[function.syntax().clone()]) {
            add(&mut changes, owner, r, s)
        }
    }
    Ok(MultiFileRefactoring {
        title: String::new(),
        changes: changes.into_iter().collect(),
    })
}

fn inline_parameter(
    db: &Analysis,
    owner: &Arc<FileInfo>,
    range: TextRange,
    files: &[PathBuf],
) -> Result<MultiFileRefactoring, String> {
    let root = owner.parse.syntax_node();
    let (id, binding) = owner
        .scopes
        .binding_at(range.start())
        .ok_or("Missing parameter")?;
    let function =
        ancestor_at::<ast::FuncDef>(&root, range.start()).ok_or("Missing parameter owner")?;
    if owner
        .scopes
        .bindings_in(binding.scope, &binding.name)
        .iter()
        .any(|other| *other != id && owner.scopes.binding(*other).is_definition())
    {
        return Err("The parameter is reassigned in its body".into());
    }
    allowed_decorators(db, owner, &function)?;
    let mut model = change_signature::signature_model(owner, range.start())?;
    let index = model
        .params
        .iter()
        .position(|p| p.name == binding.name)
        .ok_or("Parameter not found")?;
    if model.implicit_first && index == 0 {
        return Err("The implicit receiver cannot be inlined".into());
    }
    let parameter = model.params[index].clone();
    if matches!(
        parameter.kind,
        change_signature::ParamKind::VarArgs | change_signature::ParamKind::KwArgs
    ) {
        return Err("A variadic parameter has no single constant value".into());
    }
    let calls = function_call_sites(db, owner, &function, files);
    let mut value = None;
    let mut changes = BTreeMap::new();
    for site in &calls {
        if site.unresolved {
            return Err("An unresolved invocation prevents removing the parameter".into());
        }
        let list = site.args().ok_or("Missing arguments")?;
        if list
            .syntax()
            .descendants_with_tokens()
            .any(|element| element.kind() == SyntaxKind::COMMENT)
        {
            return Err("Inlining this parameter would discard argument comments".into());
        }
        let args: Vec<_> = list.args().collect();
        if args.iter().any(|a| {
            matches!(
                a,
                ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_) | ast::AnyArg::Generator(_)
            )
        }) {
            return Err("Expanded arguments prevent proving a common parameter value".into());
        }
        let position = model
            .params
            .iter()
            .take(index)
            .filter(|p| {
                matches!(
                    p.kind,
                    change_signature::ParamKind::Normal
                        | change_signature::ParamKind::PositionalOnly
                )
            })
            .count()
            .checked_sub(site.implicit);
        let keyword=args.iter().filter(|_|parameter.kind!=change_signature::ParamKind::PositionalOnly).find(|arg|matches!(arg,ast::AnyArg::Keyword(k) if k.name().is_some_and(|name|name.text()==parameter.name)));
        let positional = position
            .and_then(|position| {
                args.iter()
                    .filter(|arg| matches!(arg, ast::AnyArg::Positional(_)))
                    .nth(position)
            })
            .filter(|_| {
                matches!(
                    parameter.kind,
                    change_signature::ParamKind::Normal
                        | change_signature::ParamKind::PositionalOnly
                )
            });
        if keyword.is_some() && positional.is_some() {
            return Err("The parameter is supplied twice".into());
        }
        let argument = keyword.or(positional);
        let source = argument
            .and_then(|a| a.value())
            .map(|e| e.syntax().text().to_string())
            .or_else(|| parameter.default.clone())
            .ok_or("A call does not supply the parameter")?;
        let parsed = rustpy_syntax::parse(&source);
        let expression = parsed
            .syntax_node()
            .descendants()
            .find_map(ast::Expr::cast)
            .ok_or("Missing parameter value")?;
        if !immutable(&expression) {
            return Err("The parameter is supplied an effectful or mutable expression".into());
        }
        let source = source.trim().to_string();
        if value.as_ref().is_some_and(|value| value != &source) {
            return Err("Callers supply different values; this parameter cannot be inlined".into());
        }
        value = Some(source);
        if let Some(argument) = argument {
            add(
                &mut changes,
                &site.file,
                edit_helpers::element_with_comma(&site.file.text, argument.syntax()),
                String::new(),
            );
        }
    }
    let value = value
        .or(parameter.default.clone())
        .ok_or("No common actual/default value can be proved")?;
    let parsed = rustpy_syntax::parse(&value);
    if parsed
        .syntax_node()
        .descendants()
        .find_map(ast::Expr::cast)
        .is_none_or(|e| !immutable(&e))
    {
        return Err("The default is not immutable".into());
    }
    if owner
        .scopes
        .references_to(id)
        .len()
        .saturating_mul(value.len().saturating_add(8))
        > 8 * 1024 * 1024
    {
        return Err("Inline parameter expansion exceeds 8 MiB".into());
    }
    for reference in owner.scopes.references_to(id) {
        if reference.is_write {
            return Err("The parameter is reassigned or deleted".into());
        }
        add(&mut changes, owner, reference.range, format!("({value})"));
    }
    model.params.remove(index);
    model.propagate_overrides = false;
    let list = function.params().ok_or("Missing parameter list")?;
    if list
        .syntax()
        .descendants_with_tokens()
        .any(|n| n.kind() == SyntaxKind::COMMENT)
    {
        return Err("Removing this parameter would discard comments".into());
    }
    add(
        &mut changes,
        owner,
        TextRange::new(
            list.text_range().start() + TextSize::new(1),
            list.text_range().end() - TextSize::new(1),
        ),
        model.render(),
    );
    // Escaping callable values are not statically enumerable invocation sites.
    for usage in find_usages(
        db,
        owner,
        function
            .name()
            .ok_or("Missing function name")?
            .text_range()
            .start(),
        files,
    )
    .iter()
    .filter(|u| !u.is_definition)
    {
        if calls
            .iter()
            .any(|s| s.file.path == usage.path && s.call.text_range().contains_range(usage.range))
        {
            continue;
        }
        let target = db.file_for_path(&usage.path).ok_or("Missing usage file")?;
        if ancestor_at::<ast::ImportAlias>(&target.parse.syntax_node(), usage.range.start())
            .is_none()
        {
            return Err(
                "The callable escapes or has dynamic usages; removing its parameter is unsafe"
                    .into(),
            );
        }
    }
    Ok(MultiFileRefactoring {
        title: String::new(),
        changes: changes.into_iter().collect(),
    })
}

fn method_authority(
    db: &Analysis,
    file: &Arc<FileInfo>,
    function: &ast::FuncDef,
    files: &[PathBuf],
) -> Result<(), String> {
    if !function.is_method() {
        return Ok(());
    }
    let class = function
        .syntax()
        .ancestors()
        .skip(1)
        .find_map(ast::ClassDef::cast)
        .ok_or("Missing class")?;
    let inf = Inferer::new(db);
    let reference = rustpy_semantic::types::ClassRef {
        file: file.id,
        range: class.text_range(),
        name: class.name_text().into(),
    };
    let owners = inf.mro(&reference);
    if owners.len() > 32 {
        return Err("Class hierarchy exceeds Inline budget".into());
    }
    for owner in owners.iter() {
        let source = db.file(owner.file).ok_or("Class source unavailable")?;
        if source.path.starts_with("<typeshed>")
            && source.module_name.as_deref() == Some("builtins")
            && owner.name == "object"
        {
            continue;
        }
        let declaration = node_with_range(&source.parse.syntax_node(), owner.range)
            .and_then(ast::ClassDef::cast)
            .ok_or("Unknown class hierarchy")?;
        if declaration.decorators().next().is_some()
            || declaration.arguments().is_some_and(|args| {
                args.args()
                    .any(|arg| !matches!(arg, ast::AnyArg::Positional(_)))
            })
        {
            return Err("A class decorator/metaclass can replace method lookup".into());
        }
        for base in declaration.bases() {
            if !matches!(base, ast::Expr::Name(_) | ast::Expr::Attribute(_))
                || !matches!(
                    inf.type_of_expr(&source, &base),
                    rustpy_semantic::types::Type::Class(_, _)
                )
            {
                return Err("A dynamic/unresolved base prevents safe method Inline".into());
            }
        }
        let info = inf.class_info(owner).ok_or("Missing class metadata")?;
        if ["__getattribute__", "__getattr__", "__get__"]
            .iter()
            .any(|name| info.members.contains_key(*name))
        {
            return Err("Custom attribute/descriptor lookup prevents method Inline".into());
        }
    }
    let name = function.name_text();
    let mut remaining = 200_000usize;
    for path in files {
        let Some(source) = db.file_for_path(path) else {
            continue;
        };
        for node in source.parse.syntax_node().descendants() {
            if remaining == 0 {
                return Err("Method replacement scan exceeds the Inline budget".into());
            }
            remaining -= 1;
            let targets = if let Some(a) = ast::AssignStmt::cast(node.clone()) {
                a.targets()
            } else if let Some(a) = ast::AnnAssignStmt::cast(node.clone()) {
                a.target().into_iter().collect()
            } else if let Some(a) = ast::AugAssignStmt::cast(node.clone()) {
                a.target().into_iter().collect()
            } else if let Some(a) = ast::DelStmt::cast(node.clone()) {
                a.targets()
            } else if let Some(a) = ast::ForStmt::cast(node.clone()) {
                a.target().into_iter().collect()
            } else if let Some(a) = ast::WithStmt::cast(node.clone()) {
                a.items().filter_map(|item| item.target()).collect()
            } else if let Some(a) = ast::CompFor::cast(node) {
                a.target().into_iter().collect()
            } else {
                continue;
            };
            if targets
                .iter()
                .flat_map(|e| {
                    e.syntax()
                        .descendants()
                        .filter_map(ast::AttributeExpr::cast)
                })
                .any(|attribute| attribute.attr_text() == name)
            {
                return Err("A visible method replacement prevents Inline".into());
            }
        }
    }
    Ok(())
}

fn paired_imports(
    db: &Analysis,
    owner: &Arc<FileInfo>,
    range: TextRange,
    files: &[PathBuf],
) -> Vec<(Arc<FileInfo>, rustpy_semantic::scope::BindingId)> {
    let mut out = Vec::new();
    for path in files {
        let Some(file) = db.file_for_path(path) else {
            continue;
        };
        for (index, binding) in file
            .scopes
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, binding)| matches!(binding.kind, BindingKind::Import(_)))
        {
            let locations =
                rustpy_semantic::features::definition_at(db, &file, binding.range.start());
            if locations
                .iter()
                .any(|location| location.path == owner.path && location.range == range)
            {
                out.push((
                    file.clone(),
                    rustpy_semantic::scope::BindingId(index as u32),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup(source: &str) -> (Analysis, Arc<FileInfo>, Vec<PathBuf>) {
        let path = PathBuf::from("/proj/main.py");
        let db = Analysis::for_project(std::path::Path::new("/proj"));
        db.set_file_text(&path, source);
        let file = db.file_for_path(&path).unwrap();
        (db, file, vec![path])
    }
    fn at(source: &str, needle: &str) -> TextSize {
        TextSize::new(source.find(needle).unwrap() as u32)
    }
    fn output(file: &FileInfo, plan: &MultiFileRefactoring) -> String {
        edit_helpers::try_apply_edits(
            &file.text,
            plan.changes
                .iter()
                .find(|(path, _)| path == &file.path)
                .map(|(_, edits)| edits.as_slice())
                .unwrap_or_default(),
        )
        .unwrap()
    }
    #[test]
    fn variable_this_only_keep_and_all_remove_are_distinct() {
        let source = "def f():\n    token = (1, 'π')\n    print(token)\n    return token\n";
        let (db, file, files) = setup(source);
        let one = plan(
            &db,
            &file,
            at(source, "return token") + TextSize::new(7),
            &files,
            InlineOptions {
                this_only: true,
                keep_declaration: true,
            },
        )
        .unwrap();
        let text = output(&file, &one);
        assert!(text.contains("token = (1, 'π')"));
        assert!(text.contains("print(token)"));
        assert!(text.contains("return ((1, 'π'))"));
        let all = plan(
            &db,
            &file,
            at(source, "token ="),
            &files,
            InlineOptions::default(),
        )
        .unwrap();
        let text = output(&file, &all);
        assert!(!text.contains("token"));
        assert!(rustpy_syntax::parse(&text).errors().is_empty());
    }
    #[test]
    fn side_effectful_initializer_cannot_be_duplicated_or_moved_past_calls() {
        let source = "def f():\n    token = produce()\n    print(token)\n    return token\n";
        let (db, file, files) = setup(source);
        assert!(plan(
            &db,
            &file,
            at(source, "token ="),
            &files,
            InlineOptions::default()
        )
        .is_err());
        let source = "def f():\n    token = produce()\n    return token\n";
        let (db, file, files) = setup(source);
        let result = plan(
            &db,
            &file,
            at(source, "token ="),
            &files,
            InlineOptions::default(),
        )
        .unwrap();
        let text = output(&file, &result);
        assert_eq!(text.matches("produce()").count(), 1);
        assert!(text.contains("return (produce())"));
    }
    #[test]
    fn function_args_default_keyword_star_and_side_effects_are_bound_once() {
        let source="def combine(first, second=2, *rest, flag=True, **extras):\n    return first + second\nresult = combine(produce(), flag=False, **options)\n";
        let (db, file, files) = setup(source);
        let result = plan(
            &db,
            &file,
            at(source, "combine(produce"),
            &files,
            InlineOptions {
                this_only: true,
                keep_declaration: true,
            },
        )
        .unwrap();
        let text = output(&file, &result);
        assert_eq!(text.matches("produce()").count(), 1);
        assert!(text.contains("lambda first, second=2, *rest, flag=True, **extras:"));
        assert!(text.contains("flag=False, **options"));
        assert!(rustpy_syntax::parse(&text).errors().is_empty());
    }
    #[test]
    fn conditional_returns_and_local_assignments_keep_lazy_branch_effects() {
        let source="def prepare():\n    print('prepare')\n    return 1\ndef accept(value):\n    print('accept')\n    return value\ndef reject(value):\n    print('reject')\n    return value\ndef choose(flag):\n    value = prepare()\n    if flag:\n        return accept(value)\n    return reject(value)\nanswer = choose(True)\n";
        let (db, file, files) = setup(source);
        let result = plan(
            &db,
            &file,
            at(source, "choose(True)"),
            &files,
            InlineOptions::default(),
        )
        .unwrap();
        let text = output(&file, &result);
        assert!(text.contains("value := prepare()"));
        assert!(text.contains("if flag else"));
        assert!(!text.contains("def choose"));
        let parsed = rustpy_syntax::parse(&text);
        // Count invocations, excluding the now-explicit helper declaration.
        assert_eq!(
            parsed
                .syntax_node()
                .descendants()
                .filter_map(ast::CallExpr::cast)
                .filter(|call| call.callee().is_some_and(|callee| {
                    matches!(callee, ast::Expr::Name(name) if name.name_text() == "prepare")
                }))
                .count(),
            1
        );
        assert!(parsed.errors().is_empty());
    }
    #[test]
    fn mutable_defaults_recursion_and_escaping_callables_are_conflicts() {
        for source in [
            "def f(value=[]):\n    return value\nf()\n",
            "def f(x):\n    return f(x)\nf(1)\n",
            "def f(x):\n    return x\ncallback=f\nf(1)\n",
        ] {
            let (db, file, files) = setup(source);
            assert!(plan(
                &db,
                &file,
                at(source, "def f") + TextSize::new(4),
                &files,
                InlineOptions::default()
            )
            .is_err());
        }
    }
    #[test]
    fn ordinary_method_and_property_inline_capture_receiver_once() {
        for source in ["class Box:\n    def size(self, extra=1):\n        return self.value + extra\nanswer = Box().size(2)\n","class Box:\n    @property\n    def size(self):\n        return self.value\nanswer = Box().size\n"] {let(db,file,files)=setup(source);let offset=TextSize::new(source.rfind("size").unwrap() as u32);let result=plan(&db,&file,offset,&files,InlineOptions{this_only:true,keep_declaration:true}).unwrap();let text=output(&file,&result);assert_eq!(text.matches("Box()").count(),1);assert!(text.contains("lambda self"));assert!(rustpy_syntax::parse(&text).errors().is_empty());}
    }
    #[test]
    fn parameter_common_constant_updates_body_signature_and_calls() {
        let source="def scale(value, factor=2):\n    return value * factor\na = scale(3)\nb = scale(4, factor=2)\n";
        let (db, file, files) = setup(source);
        let result = plan(
            &db,
            &file,
            at(source, "factor=2"),
            &files,
            InlineOptions::default(),
        )
        .unwrap();
        let text = output(&file, &result);
        assert!(text.contains("def scale(value):"));
        assert!(text.contains("value * (2)"));
        assert!(text.contains("scale(4)"));
        assert!(rustpy_syntax::parse(&text).errors().is_empty());
        let different = source.replace("scale(4, factor=2)", "scale(4, factor=3)");
        let (db, file, files) = setup(&different);
        assert!(plan(
            &db,
            &file,
            at(&different, "factor=2"),
            &files,
            InlineOptions::default()
        )
        .is_err());
    }
    #[test]
    fn free_binding_capture_and_unknown_descriptor_are_refused() {
        let source="offset = 1\ndef f(value):\n    return value + offset\ndef caller(offset):\n    return f(2)\n";
        let (db, file, files) = setup(source);
        assert!(plan(
            &db,
            &file,
            at(source, "f(2)"),
            &files,
            InlineOptions {
                this_only: true,
                keep_declaration: true
            }
        )
        .is_err());
        let source="class Box:\n    def __getattribute__(self, name):\n        return replacement\n    def f(self):\n        return 1\nBox().f()\n";
        let (db, file, files) = setup(source);
        assert!(plan(
            &db,
            &file,
            at(source, "Box().f") + TextSize::new(6),
            &files,
            InlineOptions::default()
        )
        .is_err());
    }
    #[test]
    fn nested_parentheses_preserve_literal_repeatability_and_effect_rejection() {
        let source = "def f():\n    token = (((1)))\n    return token + token\n";
        let (db, file, files) = setup(source);
        let result = plan(
            &db,
            &file,
            at(source, "token ="),
            &files,
            InlineOptions::default(),
        )
        .unwrap();
        let changed = output(&file, &result);
        assert!(!changed.contains("token"));
        assert_eq!(changed.matches("(((1)))").count(), 2);
        assert!(rustpy_syntax::parse(&changed).errors().is_empty());
        assert_eq!(
            file.text.as_ref(),
            source,
            "Planning must not mutate the captured source"
        );
        let source = "def f():\n    token = ((produce()))\n    return token + token\n";
        let (db, file, files) = setup(source);
        assert!(plan(
            &db,
            &file,
            at(source, "token ="),
            &files,
            InlineOptions::default()
        )
        .is_err());
        assert_eq!(file.text.as_ref(), source);
    }
}
