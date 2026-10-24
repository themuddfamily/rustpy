//! Transactional Move planners. Project traversal and edit application remain
//! in the existing shared refactoring infrastructure.
use crate::refactor2::{self, FileOp};
use rustpy_core::{TextRange, TextSize};
use rustpy_semantic::{
    db::{Analysis, FileInfo},
    edit_helpers::{
        after_last_line, ancestor_at, apply_edits, deletion_range, indent_at, is_identifier,
        line_start, reindent, rng, slice, stmt_lines, Edit,
    },
    infer::Inferer,
    scope::BindingKind,
    types::Type,
};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind, SyntaxNode,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone, Debug)]
pub struct MoveOptions {
    pub dependencies: Vec<String>,
    pub move_usages: bool,
    pub keep_reexports: bool,
    pub prefer_from_imports: bool,
}
impl Default for MoveOptions {
    fn default() -> Self {
        Self {
            dependencies: Vec::new(),
            move_usages: true,
            keep_reexports: true,
            prefer_from_imports: true,
        }
    }
}
/// Direct and transitive source-module dependencies available in the chooser.
pub fn dependencies(file: &FileInfo, offset: TextSize) -> Result<Vec<String>, String> {
    let (initial, name) = if let Some(function) =
        ancestor_at::<ast::FuncDef>(&file.parse.syntax_node(), offset)
            .filter(|function| function.is_method())
    {
        (function.syntax().clone(), function.name_text())
    } else {
        refactor2::top_level_def_at(file, offset)?
    };
    let mut pending = vec![initial];
    let mut names = BTreeSet::from([name.clone()]);
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        for reference in file
            .scopes
            .references
            .iter()
            .filter(|reference| node.text_range().contains_range(reference.range))
        {
            let Some(binding) = reference.resolved.map(|id| file.scopes.binding(id)) else {
                continue;
            };
            if binding.scope != file.scopes.module_scope()
                || !matches!(
                    binding.kind,
                    BindingKind::Function
                        | BindingKind::Class
                        | BindingKind::Assignment
                        | BindingKind::Annotation
                        | BindingKind::TypeAlias
                )
                || !names.insert(binding.name.to_string())
            {
                continue;
            }
            if let Ok((declaration, dependency)) =
                refactor2::top_level_def_at(file, binding.range.start())
            {
                result.push(dependency);
                pending.push(declaration);
            }
        }
    }
    result.sort();
    Ok(result)
}
fn declaration(file: &FileInfo, name: &str) -> Result<SyntaxNode, String> {
    let bindings = file.scopes.bindings_in(file.scopes.module_scope(), name);
    if bindings.len() != 1 {
        return Err(format!(
            "'{name}' is rebound or ambiguous; split its declarations before moving"
        ));
    }
    refactor2::top_level_def_at(file, file.scopes.binding(bindings[0]).range.start())
        .map(|(node, _)| node)
}
fn exported(file: &FileInfo) -> Result<Option<(TextRange, Vec<String>)>, String> {
    let bindings = file
        .scopes
        .bindings_in(file.scopes.module_scope(), "__all__");
    if bindings.is_empty() {
        return Ok(None);
    }
    if bindings.len() != 1 {
        return Err(
            "Dynamic or repeatedly assigned __all__ must be resolved before moving exports".into(),
        );
    }
    let binding = file.scopes.binding(bindings[0]);
    let range = binding.value.ok_or("__all__ has no literal value")?;
    let expression = file
        .parse
        .syntax_node()
        .descendants()
        .find_map(|node| {
            ast::Expr::cast(node).filter(|expression| expression.syntax().text_range() == range)
        })
        .ok_or("Cannot locate __all__")?;
    let elements: Vec<_> = match expression {
        ast::Expr::List(list) => list.elements().collect(),
        ast::Expr::Tuple(tuple) => tuple.elements().collect(),
        _ => return Err("Only literal list/tuple __all__ can be updated automatically".into()),
    };
    if file
        .parse
        .syntax_node()
        .descendants_with_tokens()
        .any(|token| {
            token.kind() == SyntaxKind::COMMENT && range.contains_range(token.text_range())
        })
    {
        return Err("__all__ contains comments; edit its exports explicitly before moving".into());
    }
    let mut names = Vec::new();
    for expression in elements {
        match expression {
            ast::Expr::String(value) => names.push(value.value()),
            _ => return Err("Computed __all__ entries require a manual export decision".into()),
        }
    }
    Ok(Some((range, names)))
}
fn render_exports(names: &[String]) -> String {
    format!(
        "[{}]",
        names
            .iter()
            .map(|name| format!("{name:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
fn add_import(edits: &mut Vec<Edit>, file: &FileInfo, imports: BTreeSet<String>) {
    if !imports.is_empty() {
        let at = refactor2::import_insert_offset(file);
        edits.push((
            rng(at, at),
            imports
                .into_iter()
                .map(|import| format!("{import}\n"))
                .collect(),
        ));
    }
}
fn fresh(file: &FileInfo, stem: &str) -> String {
    let mut name = stem.to_string();
    let mut index = 2;
    while file
        .scopes
        .bindings
        .iter()
        .any(|binding| binding.name == name)
    {
        name = format!("{stem}{index}");
        index += 1;
    }
    name
}
/// Move one declaration and explicitly selected dependencies in a single plan;
/// dependency edges inside the group stay local and cannot create source↔target
/// imports. Old module compatibility exports are an explicit UI choice.
pub fn move_symbols(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    target: &Path,
    root: &Path,
    paths: &[PathBuf],
    options: &MoveOptions,
) -> Result<Vec<FileOp>, String> {
    if target == file.path {
        return Err("Choose another module".into());
    }
    let (_, name) = refactor2::top_level_def_at(file, offset)?;
    let mut names = BTreeSet::from([name]);
    names.extend(options.dependencies.iter().cloned());
    let mut declarations = names
        .iter()
        .map(|name| declaration(file, name))
        .collect::<Result<Vec<_>, _>>()?;
    declarations.sort_by_key(|node| node.text_range().start());
    if declarations.iter().any(|node| {
        node.descendants().any(|node| {
            matches!(
                node.kind(),
                SyntaxKind::GLOBAL_STMT | SyntaxKind::NONLOCAL_STMT
            )
        })
    }) {
        return Err("A moved declaration writes an explicit global/nonlocal namespace; resolve those bindings before moving modules".into());
    }
    let ranges: Vec<_> = declarations.iter().map(|node| node.text_range()).collect();
    let source_module =
        refactor2::module_name_for(db, root, &file.path).ok_or("Source is outside import roots")?;
    let target_module = refactor2::module_name_for(db, root, target)
        .ok_or("Destination is outside import roots")?;
    if !target_module.split('.').all(is_identifier)
        || !rustpy_syntax::parse(&format!("import {target_module}\n"))
            .errors()
            .is_empty()
    {
        return Err("Destination module components must be identifiers".into());
    }
    let target_file = db.file_for_path(target);
    if target.exists() && target_file.is_none() {
        return Err("Load the existing destination before moving into it".into());
    }
    let mut needed = BTreeSet::new();
    let mut module_dependencies = Vec::new();
    let mut moved_edits = Vec::new();
    let mut source_alias = fresh(file, "_moved_dependencies");
    if let Some(destination) = &target_file {
        while destination
            .scopes
            .resolve(destination.scopes.module_scope(), &source_alias)
            .is_some()
        {
            source_alias.push('_');
        }
    }

    for reference in file.scopes.references.iter().filter(|reference| {
        ranges
            .iter()
            .any(|range| range.contains_range(reference.range))
    }) {
        let Some(binding) = reference.resolved.map(|id| file.scopes.binding(id)) else {
            continue;
        };
        if binding.scope != file.scopes.module_scope() || names.contains(binding.name.as_str()) {
            continue;
        }
        if let Some(destination) = &target_file {
            if destination
                .scopes
                .resolve(destination.scopes.module_scope(), &binding.name)
                .is_some()
                && (options.prefer_from_imports || matches!(binding.kind, BindingKind::Import(_)))
            {
                let wanted = if let BindingKind::Import(import) = &binding.kind {
                    refactor2::import_statement_for(import, &source_module, file.is_package_init())
                } else {
                    format!("from {source_module} import {}", binding.name)
                };
                if !refactor2::top_level_statements(destination)
                    .iter()
                    .any(|statement| statement.syntax().text().to_string().trim() == wanted)
                {
                    return Err(format!(
                        "Dependency '{}' already has a different binding in destination",
                        binding.name
                    ));
                }
            }
        }
        match &binding.kind {
            BindingKind::Import(import) => {
                needed.insert(refactor2::import_statement_for(
                    import,
                    &source_module,
                    file.is_package_init(),
                ));
            }
            BindingKind::Function
            | BindingKind::Class
            | BindingKind::Assignment
            | BindingKind::Annotation
            | BindingKind::TypeAlias => {
                module_dependencies.push(binding.range);
                if options.prefer_from_imports {
                    needed.insert(format!("from {source_module} import {}", binding.name));
                } else {
                    needed.insert(format!("import {source_module} as {source_alias}"));
                    moved_edits.push((reference.range, format!("{source_alias}.{}", binding.name)));
                }
            }
            _ => {}
        }
    }
    let keep = options.keep_reexports || !options.move_usages;
    let source_exports = exported(file)?;
    let moved_exports: Vec<_> = source_exports
        .as_ref()
        .map(|(_, exports)| {
            exports
                .iter()
                .filter(|name| names.contains(name.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let mut changes = BTreeMap::<PathBuf, Vec<Edit>>::new();
    let source_edits = changes.entry(file.path.clone()).or_default();
    for node in &declarations {
        source_edits.push((deletion_range(&file.text, node.text_range()), String::new()));
    }
    let mut source_imports = BTreeSet::new();
    for name in &names {
        let id = file.scopes.bindings_in(file.scopes.module_scope(), name)[0];
        if keep
            || file.scopes.references_to(id).iter().any(|reference| {
                !ranges
                    .iter()
                    .any(|range| range.contains_range(reference.range))
            })
        {
            source_imports.insert(format!("from {target_module} import {name}"));
        }
    }
    if !keep {
        if let Some((range, exports)) = &source_exports {
            source_edits.push((
                *range,
                render_exports(
                    &exports
                        .iter()
                        .filter(|name| !names.contains(name.as_str()))
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
            ));
        }
    }
    if !source_imports.is_empty() && !module_dependencies.is_empty() {
        return Err("Move the source-module dependencies together, or remove compatibility reexports and remaining source uses, to avoid a circular import".into());
    }
    add_import(source_edits, file, source_imports);
    let inference = Inferer::new(db);
    for attribute in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::AttributeExpr::cast)
        .filter(|attribute| {
            names.contains(&attribute.attr_text())
                && ranges
                    .iter()
                    .any(|range| range.contains_range(attribute.syntax().text_range()))
        })
    {
        if attribute.object().is_some_and(
            |object| matches!(inference.type_of_expr(file,&object),Type::Module(id) if id==file.id),
        ) {
            moved_edits.push((attribute.syntax().text_range(), attribute.attr_text()));
        }
    }
    let symbol_text = declarations
        .iter()
        .map(|node| {
            let range = stmt_lines(&file.text, node.text_range());
            let edits = moved_edits
                .iter()
                .filter(|(edit, _)| range.contains_range(*edit))
                .map(|(edit, text)| {
                    (
                        TextRange::new(edit.start() - range.start(), edit.end() - range.start()),
                        text.clone(),
                    )
                })
                .collect::<Vec<_>>();
            apply_edits(slice(&file.text, range), &edits)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut create = None;
    if let Some(destination) = &target_file {
        for name in &names {
            if destination
                .scopes
                .resolve(destination.scopes.module_scope(), name)
                .is_some()
            {
                return Err(format!("'{name}' already exists in destination"));
            }
        }
        let edits = changes.entry(target.to_owned()).or_default();
        add_import(edits, destination, needed);
        let at = destination.text.len();
        edits.push((
            rng(at, at),
            format!(
                "{}{}",
                if destination.text.trim().is_empty() {
                    ""
                } else {
                    "\n\n"
                },
                symbol_text
            ),
        ));
        if !moved_exports.is_empty() {
            if let Some((range, mut exports)) = exported(destination)? {
                for name in &moved_exports {
                    if !exports.contains(name) {
                        exports.push(name.clone());
                    }
                }
                edits.push((range, render_exports(&exports)));
            }
        }
    } else {
        let mut text = needed
            .into_iter()
            .map(|import| format!("{import}\n"))
            .collect::<String>();
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&symbol_text);
        if !moved_exports.is_empty() {
            text.push_str(&format!(
                "\n\n__all__ = {}\n",
                render_exports(&moved_exports)
            ));
        }
        create = Some(FileOp::Create(target.to_owned(), text));
    }
    let files = refactor2::load_files(db, paths);
    if options.move_usages {
        for other in &files {
            if other.path == target {
                continue;
            }
            let edits = changes.entry(other.path.clone()).or_default();
            let mut imports = BTreeSet::new();
            for statement in other
                .parse
                .syntax_node()
                .descendants()
                .filter_map(ast::ImportFromStmt::cast)
            {
                let module = if statement.level() > 0 {
                    refactor2::absolute_module(
                        &refactor2::module_name_for(db, root, &other.path).unwrap_or_default(),
                        other.is_package_init(),
                        statement.level(),
                        &statement
                            .module()
                            .map(|module| module.path())
                            .unwrap_or_default(),
                    )
                } else {
                    statement.module_path()
                };
                if module != source_module {
                    continue;
                }
                if statement.is_star() {
                    if !keep {
                        return Err("A star import still exposes the old module; retain compatibility reexports or replace that import first".into());
                    }
                    continue;
                }
                let moved: Vec<_> = statement
                    .aliases()
                    .filter(|alias| names.contains(&alias.name()))
                    .map(|alias| alias.syntax().text().to_string())
                    .collect();
                if moved.is_empty() {
                    continue;
                }
                if statement
                    .syntax()
                    .descendants_with_tokens()
                    .any(|token| token.kind() == SyntaxKind::COMMENT)
                {
                    return Err(
                        "An affected import contains comments; split that import before moving"
                            .into(),
                    );
                }
                let remaining: Vec<_> = statement
                    .aliases()
                    .filter(|alias| !names.contains(&alias.name()))
                    .map(|alias| alias.syntax().text().to_string())
                    .collect();
                let indent = indent_at(&other.text, statement.syntax().text_range().start().into());
                let mut replacement = Vec::new();
                if !remaining.is_empty() {
                    replacement.push(format!(
                        "from {} import {}",
                        statement.module_path(),
                        remaining.join(", ")
                    ));
                }
                replacement.push(format!("from {target_module} import {}", moved.join(", ")));
                edits.push((
                    statement.syntax().text_range(),
                    replacement.join(&format!("\n{indent}")),
                ));
            }
            // Module-qualified accesses are resolved, not text matched: a local
            // same-spelled object or receiver never gets redirected.
            let inference = Inferer::new(db);
            let alias = fresh(other, "_moved_module");
            for attribute in other
                .parse
                .syntax_node()
                .descendants()
                .filter_map(ast::AttributeExpr::cast)
                .filter(|attribute| names.contains(&attribute.attr_text()))
            {
                if other.path == file.path
                    && ranges
                        .iter()
                        .any(|range| range.contains_range(attribute.syntax().text_range()))
                {
                    continue;
                }
                let Some(object) = attribute.object() else {
                    continue;
                };
                if matches!(inference.type_of_expr(other,&object),Type::Module(id) if id==file.id) {
                    edits.push((
                        attribute.syntax().text_range(),
                        format!("{alias}.{}", attribute.attr_text()),
                    ));
                    imports.insert(format!("import {target_module} as {alias}"));
                }
            }
            add_import(edits, other, imports);
        }
    }
    let mut result = finish(changes)?;
    if let Some(create) = create {
        result.push(create);
    }
    Ok(result)
}
fn finish(changes: BTreeMap<PathBuf, Vec<Edit>>) -> Result<Vec<FileOp>, String> {
    let mut result = Vec::new();
    for (path, mut edits) in changes {
        edits.sort_by_key(|(range, _)| (range.start(), range.end()));
        edits.dedup();
        if edits
            .windows(2)
            .any(|pair| pair[0].0.end() > pair[1].0.start())
        {
            return Err(format!("Overlapping Move edits in {}", path.display()));
        }
        if !edits.is_empty() {
            result.push(FileOp::Edit(path, edits));
        }
    }
    Ok(result)
}

fn exact_receiver(db: &Analysis, file: &Arc<FileInfo>, object: &ast::Expr) -> bool {
    match Inferer::new(db).type_of_expr(file, object) {
        Type::Class(_, _) => match object {
            ast::Expr::Name(name) => file
                .scopes
                .reference_at(name.syntax().text_range().start())
                .and_then(|reference| reference.resolved)
                .is_some_and(|id| {
                    matches!(
                        file.scopes.binding(id).kind,
                        BindingKind::Class | BindingKind::Import(_)
                    )
                }),
            ast::Expr::Attribute(attribute) => attribute.object().is_some_and(|object| {
                matches!(
                    Inferer::new(db).type_of_expr(file, &object),
                    Type::Module(_)
                )
            }),
            _ => false,
        },
        Type::Instance(_, _) => {
            let value = if let ast::Expr::Name(name) = object {
                let Some(reference) = file.scopes.reference_at(name.syntax().text_range().start())
                else {
                    return false;
                };
                let Some(binding) = reference.resolved.map(|id| file.scopes.binding(id)) else {
                    return false;
                };
                if !matches!(binding.kind, BindingKind::Assignment)
                    || file.scopes.bindings_in(binding.scope, &binding.name).len() != 1
                {
                    return false;
                }
                let Some(range) = binding.value else {
                    return false;
                };
                file.parse.syntax_node().descendants().find_map(|node| {
                    ast::Expr::cast(node)
                        .filter(|expression| expression.syntax().text_range() == range)
                })
            } else {
                Some(object.clone())
            };
            value.is_some_and(|value|matches!(value,ast::Expr::Call(call) if call.callee().is_some_and(|callee|matches!(Inferer::new(db).type_of_expr(file,&callee),Type::Class(_, _)))))
        }
        _ => false,
    }
}

fn captured_default_edits(
    function: &ast::FuncDef,
    model: &crate::change_signature::SignatureModel,
    owner: &str,
) -> Vec<Edit> {
    let mut index = 0;
    let mut edits = Vec::new();
    if let Some(parameters) = function.params() {
        for parameter in parameters.params() {
            let Some(value) = parameter.default() else {
                continue;
            };
            let Some(name) = parameter.name().map(|name| name.text().to_string()) else {
                continue;
            };
            let kind = model
                .original
                .iter()
                .find(|slot| slot.name == name)
                .map(|slot| slot.kind);
            let expression = if kind == Some(crate::change_signature::ParamKind::KeywordOnly) {
                format!("{owner}.__kwdefaults__[{name:?}]")
            } else {
                let expression = format!("{owner}.__defaults__[{index}]");
                index += 1;
                expression
            };
            edits.push((value.syntax().text_range(), expression));
        }
    }
    edits
}

fn forwarding_arguments(model: &crate::change_signature::SignatureModel) -> String {
    model
        .params
        .iter()
        .map(|parameter| match parameter.kind {
            crate::change_signature::ParamKind::VarArgs => format!("*{}", parameter.name),
            crate::change_signature::ParamKind::KwArgs => format!("**{}", parameter.name),
            crate::change_signature::ParamKind::KeywordOnly => {
                format!("{}={}", parameter.name, parameter.name)
            }
            _ => parameter.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}
/// Make Function. The forwarding descriptor is deliberately retained: method
/// values, unknown receivers, subclass dispatch and external clients still have
/// a correct callable while resolved direct calls can use the new function.
pub fn method_to_function(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    name: &str,
    paths: &[PathBuf],
    root: &Path,
    move_usages: bool,
) -> Result<Vec<FileOp>, String> {
    let function = ancestor_at::<ast::FuncDef>(&file.parse.syntax_node(), offset)
        .filter(|function| function.is_method())
        .ok_or("Place the caret in a method")?;
    if !is_identifier(name)
        || !rustpy_syntax::parse(&format!("def {name}():\n    pass\n"))
            .errors()
            .is_empty()
    {
        return Err("Choose a valid function name".into());
    }
    if file
        .scopes
        .resolve(file.scopes.module_scope(), name)
        .is_some()
    {
        return Err(format!("'{name}' already exists at module level"));
    }
    for decorator in function.decorators() {
        let text = decorator.syntax().text().to_string();
        if !matches!(
            text.trim().trim_start_matches('@'),
            "staticmethod" | "classmethod"
        ) {
            return Err(
                "Custom/property/overload decorators must be resolved before making a function"
                    .into(),
            );
        }
    }
    if function.syntax().descendants().any(|node| {
        if let Some(attribute) = ast::AttributeExpr::cast(node.clone()) {
            if attribute.attr_text().starts_with("__") && !attribute.attr_text().ends_with("__") {
                return true;
            }
        }
        if let Some(call) = ast::CallExpr::cast(node.clone()) {
            if matches!(call.callee(),Some(ast::Expr::Name(name)) if name.name_text()=="super")
                && call
                    .arg_list()
                    .is_some_and(|arguments| arguments.args().next().is_none())
            {
                return true;
            }
        }
        matches!(ast::Expr::cast(node),Some(ast::Expr::Name(name)) if name.name_text()=="__class__")
    }) {
        return Err("Class-private names, __class__ and zero-argument super() need their lexical class context".into());
    }
    let source_class = function
        .syntax()
        .parent()
        .and_then(|block| block.parent())
        .and_then(ast::ClassDef::cast)
        .ok_or("Missing source class")?;
    if source_class
        .syntax()
        .parent()
        .is_none_or(|parent| parent.kind() != SyntaxKind::MODULE)
        || source_class.decorators().next().is_some()
    {
        return Err(
            "Nested/decorated classes need their lexical context before making a function".into(),
        );
    }
    if function.name_text().starts_with("__") && !function.name_text().ends_with("__") {
        return Err("Rename the private method before making it a function".into());
    }
    let header_end = function
        .body()
        .map(|body| body.syntax().text_range().start())
        .unwrap_or(function.syntax().text_range().end());
    if file.scopes.references.iter().any(|reference| {
        reference.range.start() < header_end
            && function
                .syntax()
                .text_range()
                .contains_range(reference.range)
            && reference.resolved.is_some_and(|id| {
                file.scopes.scope(file.scopes.binding(id).scope).kind
                    == rustpy_semantic::scope::ScopeKind::Class
            })
    }) {
        return Err("Signature annotations/defaults capture class-local names; qualify them before making a function".into());
    }
    let body = function.body().ok_or("Method has no body")?;
    let first = body.statements().next().ok_or("Method has no body")?;
    let last = body.statements().last().unwrap();
    let generator = crate::refactor::has_descendant(
        body.syntax(),
        &[SyntaxKind::YIELD_EXPR, SyntaxKind::YIELD_FROM_EXPR],
        &[
            SyntaxKind::FUNC_DEF,
            SyntaxKind::CLASS_DEF,
            SyntaxKind::LAMBDA_EXPR,
        ],
    );
    if generator && function.is_async() {
        return Err("An async generator cannot use a forwarding yield-from bridge".into());
    }
    let model = crate::change_signature::signature_model(
        file,
        function
            .name()
            .ok_or("Missing method name")?
            .text_range()
            .start(),
    )?;
    let receiver = model
        .original
        .first()
        .map(|parameter| parameter.name.as_str());
    let class_method = function.has_decorator("classmethod");
    let static_method = function.has_decorator("staticmethod");
    let ind = indent_at(&file.text, first.syntax().text_range().start().into());
    let call = format!("{name}({})", forwarding_arguments(&model));
    let call = if generator {
        format!("(yield from {call})")
    } else if function.is_async() {
        format!("await {call}")
    } else {
        call
    };
    let mut edits = BTreeMap::<PathBuf, Vec<Edit>>::new();
    let source_edits = edits.entry(file.path.clone()).or_default();
    let replace = TextRange::new(
        TextSize::new(line_start(&file.text, first.syntax().text_range().start().into()) as u32),
        stmt_lines(&file.text, last.syntax().text_range()).end(),
    );
    let doc = if function.docstring().is_some() {
        slice(
            &file.text,
            stmt_lines(&file.text, first.syntax().text_range()),
        )
        .to_string()
    } else {
        String::new()
    };
    source_edits.push((replace, format!("{doc}{ind}return {call}\n")));
    let name_token = function.name().unwrap();
    let start = line_start(&file.text, name_token.text_range().start().into());
    let range = TextRange::new(
        TextSize::new(start as u32),
        stmt_lines(&file.text, function.syntax().text_range()).end(),
    );
    let mut header_edits = captured_default_edits(
        &function,
        &model,
        &format!(
            "{}.{}{}",
            source_class.name_text(),
            function.name_text(),
            if class_method { ".__func__" } else { "" }
        ),
    );
    header_edits.push((name_token.text_range(), name.to_owned()));
    let local_edits = header_edits
        .into_iter()
        .map(|(edit, text)| {
            (
                TextRange::new(edit.start() - range.start(), edit.end() - range.start()),
                text,
            )
        })
        .collect::<Vec<_>>();
    let extracted = reindent(
        &apply_edits(slice(&file.text, range), &local_edits),
        &indent_at(&file.text, name_token.text_range().start().into()),
        "",
    );
    let top = function
        .syntax()
        .ancestors()
        .filter(|node| {
            node.parent()
                .is_some_and(|parent| parent.kind() == SyntaxKind::MODULE)
        })
        .last()
        .ok_or("Cannot locate enclosing class")?;
    let at = after_last_line(&file.text, top.text_range().end().into());
    source_edits.push((rng(at, at), format!("\n\n{extracted}")));
    if move_usages {
        for site in rustpy_search::usages::function_call_sites(db, file, &function, paths) {
            if site.unresolved
                || function
                    .syntax()
                    .text_range()
                    .contains_range(site.call.syntax().text_range())
                    && site.file.id == file.id
            {
                continue;
            }
            let Some(callee) = site.call.callee() else {
                continue;
            };
            let Some(arguments) = site.args() else {
                continue;
            };
            if let ast::Expr::Attribute(attribute) = &callee {
                if attribute
                    .object()
                    .is_none_or(|object| !exact_receiver(db, &site.file, &object))
                {
                    continue;
                }
            }
            if static_method {
                if let ast::Expr::Attribute(attribute) = &callee {
                    if !matches!(attribute.object(), Some(ast::Expr::Name(_))) {
                        continue;
                    }
                }
            }
            let mut prefix = String::new();
            if site.implicit > 0 && !static_method {
                let ast::Expr::Attribute(attribute) = &callee else {
                    continue;
                };
                let Some(object) = attribute.object() else {
                    continue;
                };
                if class_method
                    && !matches!(
                        Inferer::new(db).type_of_expr(&site.file, &object),
                        Type::Class(_, _)
                    )
                {
                    continue;
                }
                if receiver.is_none() {
                    return Err("Implicit receiver has no parameter".into());
                }
                prefix = object.syntax().text().to_string();
            }
            let mut imports = BTreeSet::new();
            let target = if site.file.id == file.id {
                name.to_owned()
            } else {
                let module = refactor2::module_name_for(db, root, &file.path)
                    .ok_or("Cannot name the method's source module")?;
                let alias = fresh(&site.file, "_moved_function");
                imports.insert(format!("from {module} import {name} as {alias}"));
                alias
            };
            let here = edits.entry(site.file.path.clone()).or_default();
            here.push((callee.syntax().text_range(), target));
            if !prefix.is_empty() {
                let open = arguments
                    .syntax()
                    .first_token()
                    .ok_or("Incomplete call arguments")?
                    .text_range()
                    .end();
                let separator = if arguments.args().next().is_some() {
                    ", "
                } else {
                    ""
                };
                here.push((TextRange::empty(open), format!("{prefix}{separator}")));
            }
            add_import(here, &site.file, imports);
        }
    }
    finish(edits)
}

/// Make Method Static: retain a plain-function compatibility bridge and move
/// its implementation into the selected class. Parameters remain explicit;
/// no arbitrary callable is given an invented implicit receiver.
pub fn function_to_static(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    target: &Arc<FileInfo>,
    class_offset: TextSize,
    project: crate::ProjectEditContext<'_>,
    move_usages: bool,
) -> Result<Vec<FileOp>, String> {
    let crate::ProjectEditContext { root, files: paths } = project;
    let (declaration, name) = refactor2::top_level_def_at(file, offset)?;
    let function = ast::FuncDef::cast(declaration).ok_or("Select a top-level function")?;
    if function.decorators().next().is_some() {
        return Err("Resolve function decorators before making a static method".into());
    }
    let class = ancestor_at::<ast::ClassDef>(&target.parse.syntax_node(), class_offset)
        .ok_or("Choose a destination class")?;
    if class
        .syntax()
        .parent()
        .is_none_or(|parent| parent.kind() != SyntaxKind::MODULE)
    {
        return Err("Choose a top-level destination class".into());
    }
    if target
        .scopes
        .scope_for_node(class.syntax().text_range())
        .is_some_and(|scope| !target.scopes.bindings_in(scope, &name).is_empty())
    {
        return Err(format!("Class already has a method '{name}'"));
    }
    let body = function.body().ok_or("Function has no body")?;
    let first = body.statements().next().ok_or("Function has no body")?;
    let last = body.statements().last().unwrap();
    let generator = crate::refactor::has_descendant(
        body.syntax(),
        &[SyntaxKind::YIELD_EXPR, SyntaxKind::YIELD_FROM_EXPR],
        &[
            SyntaxKind::FUNC_DEF,
            SyntaxKind::CLASS_DEF,
            SyntaxKind::LAMBDA_EXPR,
        ],
    );
    if generator && function.is_async() {
        return Err("An async generator cannot use a forwarding yield-from bridge".into());
    }
    let model = crate::change_signature::signature_model(
        file,
        function.name().unwrap().text_range().start(),
    )?;
    let mut changes = BTreeMap::<PathBuf, Vec<Edit>>::new();
    let mut imports = BTreeSet::new();
    let target_class = class.name_text();
    let qualifier = if target.id == file.id {
        target_class.clone()
    } else {
        let module = refactor2::module_name_for(db, root, &target.path)
            .ok_or("Destination class is outside import roots")?;
        let alias = fresh(file, "_moved_class");
        imports.insert(format!("from {module} import {target_class} as {alias}"));
        alias
    };
    let call = format!("{qualifier}.{name}({})", forwarding_arguments(&model));
    let call = if generator {
        format!("(yield from {call})")
    } else if function.is_async() {
        format!("await {call}")
    } else {
        call
    };
    let ind = indent_at(&file.text, first.syntax().text_range().start().into());
    let doc = if function.docstring().is_some() {
        slice(
            &file.text,
            stmt_lines(&file.text, first.syntax().text_range()),
        )
        .to_string()
    } else {
        String::new()
    };
    let lazy = imports
        .into_iter()
        .map(|import| format!("{ind}{import}\n"))
        .collect::<String>();
    changes.entry(file.path.clone()).or_default().push((
        TextRange::new(
            TextSize::new(
                line_start(&file.text, first.syntax().text_range().start().into()) as u32,
            ),
            stmt_lines(&file.text, last.syntax().text_range()).end(),
        ),
        format!("{doc}{lazy}{ind}return {call}\n"),
    ));
    let class_body = class.body().ok_or("Destination class has no body")?;
    let anchor = class_body
        .statements()
        .last()
        .ok_or("Destination class has no statements")?;
    let indentation = indent_at(&target.text, anchor.syntax().text_range().start().into());
    let defaults_owner = if target.id == file.id {
        if model
            .original
            .iter()
            .any(|parameter| parameter.default.is_some())
            && class.syntax().text_range().start() < function.syntax().text_range().end()
        {
            return Err(
                "Move the destination class below the function before transferring default objects"
                    .into(),
            );
        }
        name.clone()
    } else {
        let alias = fresh(target, "_original_function");
        let module =
            refactor2::module_name_for(db, root, &file.path).ok_or("Source has no module")?;
        if model
            .original
            .iter()
            .any(|parameter| parameter.default.is_some())
        {
            add_import(
                changes.entry(target.path.clone()).or_default(),
                target,
                BTreeSet::from([format!("from {module} import {name} as {alias}")]),
            );
        }
        alias
    };
    let symbol_range = stmt_lines(&file.text, function.syntax().text_range());
    let edits = captured_default_edits(&function, &model, &defaults_owner)
        .into_iter()
        .map(|(range, text)| {
            (
                TextRange::new(
                    range.start() - symbol_range.start(),
                    range.end() - symbol_range.start(),
                ),
                text,
            )
        })
        .collect::<Vec<_>>();
    let mut method = reindent(
        &apply_edits(slice(&file.text, symbol_range), &edits),
        "",
        &indentation,
    );
    if !method.ends_with('\n') {
        method.push('\n');
    }
    changes.entry(target.path.clone()).or_default().push((
        TextRange::empty(stmt_lines(&target.text, anchor.syntax().text_range()).end()),
        format!("\n{indentation}@staticmethod\n{method}"),
    ));
    if target.id != file.id {
        let source_module = refactor2::module_name_for(db, root, &file.path)
            .ok_or("Source is outside import roots")?;
        let mut needed = BTreeSet::new();
        for reference in file.scopes.references.iter().filter(|reference| {
            function
                .syntax()
                .text_range()
                .contains_range(reference.range)
        }) {
            let Some(binding) = reference.resolved.map(|id| file.scopes.binding(id)) else {
                continue;
            };
            if binding.scope != file.scopes.module_scope() || binding.name.as_str() == name {
                continue;
            }
            if target
                .scopes
                .resolve(target.scopes.module_scope(), &binding.name)
                .is_some()
            {
                return Err(format!(
                    "Dependency '{}' collides in the destination module",
                    binding.name
                ));
            }
            match &binding.kind {
                BindingKind::Import(import) => {
                    needed.insert(refactor2::import_statement_for(
                        import,
                        &source_module,
                        file.is_package_init(),
                    ));
                }
                _ => {
                    needed.insert(format!("from {source_module} import {}", binding.name));
                }
            }
        }
        add_import(
            changes.entry(target.path.clone()).or_default(),
            target,
            needed,
        );
    }
    if move_usages {
        for site in rustpy_search::usages::function_call_sites(db, file, &function, paths) {
            if site.unresolved
                || site.file.id == file.id
                    && function
                        .syntax()
                        .text_range()
                        .contains_range(site.call.syntax().text_range())
            {
                continue;
            }
            let Some(callee) = site.call.callee() else {
                continue;
            };
            // Keep the old bridge for effectful qualified receivers.
            if let ast::Expr::Attribute(attribute) = &callee {
                if !matches!(attribute.object(), Some(ast::Expr::Name(_))) {
                    continue;
                }
            }
            let mut imports = BTreeSet::new();
            let qualifier = if site.file.id == target.id {
                target_class.clone()
            } else {
                let module = refactor2::module_name_for(db, root, &target.path)
                    .ok_or("Destination has no module")?;
                let alias = fresh(&site.file, "_moved_class");
                imports.insert(format!("from {module} import {target_class} as {alias}"));
                alias
            };
            let here = changes.entry(site.file.path.clone()).or_default();
            here.push((callee.syntax().text_range(), format!("{qualifier}.{name}")));
            add_import(here, &site.file, imports);
        }
    }
    finish(changes)
}

/// Move an instance implementation to the class of one required positional
/// parameter. The original method becomes a descriptor-correct delegate;
/// arbitrary callers do not need their argument evaluation order rewritten.
pub fn method_via_parameter(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    parameter: &str,
    name: &str,
) -> Result<Vec<FileOp>, String> {
    let function = ancestor_at::<ast::FuncDef>(&file.parse.syntax_node(), offset)
        .filter(|function| function.is_method())
        .ok_or("Choose an instance method")?;
    if function.decorators().next().is_some() {
        return Err("Resolve decorators before transferring an instance method".into());
    }
    if !is_identifier(name)
        || !rustpy_syntax::parse(&format!("def {name}():\n    pass\n"))
            .errors()
            .is_empty()
    {
        return Err("Choose a valid method name".into());
    }
    let mut model = crate::change_signature::signature_model(
        file,
        function.name().unwrap().text_range().start(),
    )?;
    let index = model
        .params
        .iter()
        .position(|slot| slot.name == parameter)
        .filter(|index| *index > 0)
        .ok_or("Choose a receiver parameter after self")?;
    if model.params[index].default.is_some()
        || model.params[index].kind != crate::change_signature::ParamKind::Normal
    {
        return Err("The receiver must be a required ordinary positional parameter".into());
    }
    let signature = Inferer::new(db).signature(file, &function, None);
    let receiver = signature
        .params
        .get(index)
        .map(|slot| &slot.ty)
        .ok_or("Cannot resolve receiver parameter")?;
    let Type::Instance(class, _) = receiver else {
        return Err("Annotate the receiver parameter with its destination class".into());
    };
    let destination = db
        .file(class.file)
        .ok_or("Destination class is unavailable")?;
    let target = rustpy_semantic::edit_helpers::node_with_range(
        &destination.parse.syntax_node(),
        class.range,
    )
    .and_then(ast::ClassDef::cast)
    .ok_or("Destination is not a source class")?;
    if destination.is_stub
        || destination
            .scopes
            .scope_for_node(target.syntax().text_range())
            .is_some_and(|scope| !destination.scopes.bindings_in(scope, name).is_empty())
    {
        return Err("Destination is read-only or already has that method".into());
    }
    let source_class = function
        .syntax()
        .parent()
        .and_then(|block| block.parent())
        .and_then(ast::ClassDef::cast)
        .ok_or("Missing source class")?;
    if class.file == file.id && class.range == source_class.syntax().text_range() {
        return Err("Choose a parameter of another class".into());
    }
    if function.syntax().descendants().any(|node| {
        ast::AttributeExpr::cast(node.clone()).is_some_and(|attribute|attribute.attr_text().starts_with("__")&&!attribute.attr_text().ends_with("__")) || ast::CallExpr::cast(node.clone()).is_some_and(|call|matches!(call.callee(),Some(ast::Expr::Name(name)) if name.name_text()=="super")) || matches!(ast::Expr::cast(node),Some(ast::Expr::Name(name)) if name.name_text()=="__class__")
    }){return Err("Class-private names and super() require the source class context".into());}
    if destination.id != file.id
        && file
            .scopes
            .references
            .iter()
            .filter(|reference| {
                function
                    .syntax()
                    .text_range()
                    .contains_range(reference.range)
            })
            .any(|reference| {
                reference
                    .resolved
                    .is_some_and(|id| file.scopes.binding(id).scope == file.scopes.module_scope())
            })
    {
        return Err("Move source-module dependencies to the destination first, or keep the classes in one module".into());
    }
    let body = function.body().ok_or("Missing method body")?;
    let first = body.statements().next().ok_or("Empty method body")?;
    let last = body.statements().last().unwrap();
    let generator = crate::refactor::has_descendant(
        body.syntax(),
        &[SyntaxKind::YIELD_EXPR, SyntaxKind::YIELD_FROM_EXPR],
        &[
            SyntaxKind::FUNC_DEF,
            SyntaxKind::CLASS_DEF,
            SyntaxKind::LAMBDA_EXPR,
        ],
    );
    if generator && function.is_async() {
        return Err("An async generator cannot delegate yield from".into());
    }
    let args = model
        .params
        .iter()
        .enumerate()
        .filter(|(slot, _)| *slot != index)
        .map(|(_, slot)| match slot.kind {
            crate::change_signature::ParamKind::VarArgs => format!("*{}", slot.name),
            crate::change_signature::ParamKind::KwArgs => format!("**{}", slot.name),
            crate::change_signature::ParamKind::KeywordOnly => {
                format!("{}={}", slot.name, slot.name)
            }
            _ => slot.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let call = format!("{parameter}.{name}({args})");
    let call = if generator {
        format!("(yield from {call})")
    } else if function.is_async() {
        format!("await {call}")
    } else {
        call
    };
    let mut changes = BTreeMap::<PathBuf, Vec<Edit>>::new();
    let ind = indent_at(&file.text, first.syntax().text_range().start().into());
    changes.entry(file.path.clone()).or_default().push((
        TextRange::new(
            TextSize::new(
                line_start(&file.text, first.syntax().text_range().start().into()) as u32,
            ),
            stmt_lines(&file.text, last.syntax().text_range()).end(),
        ),
        format!("{ind}return {call}\n"),
    ));
    let mut receiver = model.params.remove(index);
    receiver.annotation = None;
    model.params.insert(0, receiver);
    model.implicit_first = false;
    crate::change_signature::validate(&model)?;
    let target_body = target.body().ok_or("Destination class has no body")?;
    let anchor = target_body
        .statements()
        .last()
        .ok_or("Destination class has no statements")?;
    let indent = indent_at(
        &destination.text,
        anchor.syntax().text_range().start().into(),
    );
    let body_indent = format!("{indent}    ");
    let copied = reindent(
        slice(
            &file.text,
            TextRange::new(
                TextSize::new(
                    line_start(&file.text, first.syntax().text_range().start().into()) as u32,
                ),
                stmt_lines(&file.text, last.syntax().text_range()).end(),
            ),
        ),
        &ind,
        &body_indent,
    );
    let annotation = function
        .return_annotation()
        .map(|value| format!(" -> {}", value.syntax().text()))
        .unwrap_or_default();
    changes.entry(destination.path.clone()).or_default().push((
        TextRange::empty(stmt_lines(&destination.text, anchor.syntax().text_range()).end()),
        format!(
            "\n{indent}{}def {name}({}){annotation}:\n{copied}",
            if function.is_async() { "async " } else { "" },
            model.render()
        ),
    ));
    finish(changes)
}

pub fn attribute_to_class(file: &Arc<FileInfo>, offset: TextSize) -> Result<Vec<FileOp>, String> {
    let assignment = ancestor_at::<ast::AssignStmt>(&file.parse.syntax_node(), offset)
        .ok_or("Choose a self.attribute assignment")?;
    let targets = assignment.targets();
    if targets.len() != 1 {
        return Err("Choose one attribute assignment".into());
    }
    let ast::Expr::Attribute(attribute) = &targets[0] else {
        return Err("Choose a self.attribute assignment".into());
    };
    let function = ancestor_at::<ast::FuncDef>(
        assignment.syntax(),
        assignment.syntax().text_range().start(),
    )
    .filter(|function| function.is_method())
    .ok_or("Choose an instance field assignment")?;
    let receiver = function
        .params()
        .and_then(|params| params.plain_params().next())
        .map(|parameter| parameter.name_text())
        .ok_or("Missing instance parameter")?;
    if function.decorators().next().is_some()
        || !matches!(attribute.object(),Some(ast::Expr::Name(name)) if name.name_text()==receiver)
    {
        return Err("Choose an undecorated instance method's own field".into());
    }
    let class = function
        .syntax()
        .parent()
        .and_then(|node| node.parent())
        .and_then(ast::ClassDef::cast)
        .ok_or("Missing source class")?;
    if class.decorators().next().is_some()
        || class
            .syntax()
            .children()
            .any(|node| node.kind() == SyntaxKind::ARG_LIST)
        || class.methods().any(|method| {
            matches!(
                method.name_text().as_str(),
                "__getattribute__" | "__getattr__" | "__setattr__" | "__delattr__"
            )
        })
        || class
            .syntax()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .any(|name| name.name_text() == "__slots__")
    {
        return Err(
            "Custom class storage/attribute behavior must be resolved before promoting a field"
                .into(),
        );
    }
    let value = assignment.value().ok_or("Missing field value")?;
    if !matches!(value, ast::Expr::Literal(_) | ast::Expr::String(_)) {
        return Err("Only immutable literal defaults can become shared class variables".into());
    }
    let class_scope = file
        .scopes
        .scope_for_node(class.syntax().text_range())
        .ok_or("Missing class scope")?;
    if !file
        .scopes
        .bindings_in(class_scope, &attribute.attr_text())
        .is_empty()
    {
        return Err("The class already defines that name".into());
    }
    let body = class.body().ok_or("Class has no body")?;
    let first = body.statements().next().ok_or("Class has no statements")?;
    let at = line_start(&file.text, first.syntax().text_range().start().into());
    let indent = indent_at(&file.text, first.syntax().text_range().start().into());
    let function_body = function.body().ok_or("Method has no body")?;
    let replacement = if function_body.statements().count() == 1 {
        format!(
            "{}pass\n",
            indent_at(&file.text, assignment.syntax().text_range().start().into())
        )
    } else {
        String::new()
    };
    Ok(vec![FileOp::Edit(
        file.path.clone(),
        vec![
            (
                rng(at, at),
                format!(
                    "{indent}{} = {}\n",
                    attribute.attr_text(),
                    value.syntax().text()
                ),
            ),
            (
                stmt_lines(&file.text, assignment.syntax().text_range()),
                replacement,
            ),
        ],
    )])
}

/// Compose a source-only method conversion with the existing group/module
/// planner. Each resulting edit still addresses an original file snapshot.
pub fn make_function_in_module(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    name: &str,
    target: &Path,
    project: crate::ProjectEditContext<'_>,
    options: &MoveOptions,
) -> Result<Vec<FileOp>, String> {
    let crate::ProjectEditContext { root, files: paths } = project;
    let first = method_to_function(db, file, offset, name, paths, root, options.move_usages)?;
    if target == file.path {
        return Ok(first);
    }
    let probe = Analysis::for_project(root);
    let mut originals = BTreeMap::new();
    for path in paths.iter().chain(std::iter::once(&file.path)) {
        if let Some(source) = db.file_for_path(path) {
            originals.insert(path.clone(), source.text.to_string());
            probe.set_file_text(path, &source.text);
        }
    }
    for operation in first {
        match operation {
            FileOp::Edit(path, edits) => {
                let current = probe
                    .file_for_path(&path)
                    .ok_or("Missing conversion snapshot")?;
                probe.set_file_text(&path, &apply_edits(&current.text, &edits));
            }
            _ => return Err("Unexpected method conversion file operation".into()),
        }
    }
    let source = probe
        .file_for_path(&file.path)
        .ok_or("Missing converted function")?;
    let declaration = source
        .parse
        .syntax_node()
        .children()
        .filter_map(ast::FuncDef::cast)
        .find(|function| function.name_text() == name)
        .ok_or("Cannot locate converted function")?;
    let second = move_symbols(
        &probe,
        &source,
        declaration.name().unwrap().text_range().start(),
        target,
        root,
        paths,
        options,
    )?;
    let mut created = Vec::new();
    for operation in second {
        match operation {
            FileOp::Edit(path, edits) => {
                let current = probe.file_for_path(&path).ok_or("Missing move snapshot")?;
                probe.set_file_text(&path, &apply_edits(&current.text, &edits));
            }
            other => created.push(other),
        }
    }
    let mut result = Vec::new();
    for (path, original) in originals {
        if let Some(current) = probe.file_for_path(&path) {
            if current.text.as_ref() != original {
                result.push(FileOp::Edit(
                    path,
                    vec![(rng(0, original.len()), current.text.to_string())],
                ));
            }
        }
    }
    result.extend(created);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Project {
        db: Analysis,
        paths: Vec<PathBuf>,
    }
    impl Project {
        fn new(sources: &[(&str, &str)]) -> Self {
            let db = Analysis::for_project(Path::new("/project"));
            let paths = sources
                .iter()
                .map(|(name, text)| {
                    let path = PathBuf::from(format!("/project/{name}"));
                    db.set_file_text(&path, text);
                    path
                })
                .collect();
            Self { db, paths }
        }
        fn file(&self, name: &str) -> Arc<FileInfo> {
            self.db
                .file_for_path(Path::new(&format!("/project/{name}")))
                .unwrap()
        }
        fn result(&self, operations: &[FileOp], name: &str) -> String {
            let path = PathBuf::from(format!("/project/{name}"));
            let mut text = self
                .db
                .file_for_path(&path)
                .map(|file| file.text.to_string())
                .unwrap_or_default();
            for operation in operations {
                match operation {
                    FileOp::Edit(file, edits) if file == &path => text = apply_edits(&text, edits),
                    FileOp::Create(file, created) if file == &path => text = created.clone(),
                    _ => {}
                }
            }
            text
        }
    }
    fn at(source: &str, needle: &str) -> TextSize {
        TextSize::new(source.find(needle).unwrap() as u32)
    }
    #[test]
    fn move_variables_dependency_closure_exports_and_resolved_qualified_usages() {
        let source = "__all__ = ['run', 'VALUE']\nVALUE = 3\ndef helper(value):\n    return value + VALUE\ndef run(value):\n    return helper(value)\n";
        let project = Project::new(&[
            ("source.py", source),
            (
                "client.py",
                "import source as origin\nfrom source import run as execute, VALUE\norigin.run(1)\nexecute(2)\n",
            ),
        ]);
        let file = project.file("source.py");
        assert_eq!(
            dependencies(&file, at(source, "def run")).unwrap(),
            vec!["VALUE", "helper"]
        );
        let operations = move_symbols(
            &project.db,
            &file,
            at(source, "def run"),
            Path::new("/project/destination.py"),
            Path::new("/project"),
            &project.paths,
            &MoveOptions {
                dependencies: vec!["VALUE".into(), "helper".into()],
                keep_reexports: false,
                ..Default::default()
            },
        )
        .unwrap();
        let source = project.result(&operations, "source.py");
        let target = project.result(&operations, "destination.py");
        let client = project.result(&operations, "client.py");
        assert!(source.contains("__all__ = []"));
        assert!(!source.contains("def run"));
        assert!(target.contains("VALUE = 3"));
        assert!(target.contains("return helper(value)"));
        assert!(!target.contains("from source import"));
        assert!(target.contains("__all__ = [\"run\", \"VALUE\"]"));
        assert!(client.contains("from destination import run as execute, VALUE"));
        assert!(client.contains("_moved_module.run(1)"));
        assert!(client.contains("execute(2)"));
    }
    #[test]
    fn compatibility_exports_and_star_clients_remain_valid_while_explicit_moves_retarget() {
        let source = "__all__=['run']\ndef run():\n    return 1\n";
        let project = Project::new(&[
            ("source.py", source),
            ("client.py", "from source import *\nrun()\n"),
        ]);
        let file = project.file("source.py");
        let operations = move_symbols(
            &project.db,
            &file,
            at(source, "def run"),
            Path::new("/project/new.py"),
            Path::new("/project"),
            &project.paths,
            &Default::default(),
        )
        .unwrap();
        assert!(project
            .result(&operations, "source.py")
            .contains("from new import run"));
        assert!(project
            .result(&operations, "source.py")
            .contains("__all__=['run']"));
        assert_eq!(
            project.result(&operations, "client.py"),
            "from source import *\nrun()\n"
        );
        assert!(move_symbols(
            &project.db,
            &file,
            at(source, "def run"),
            Path::new("/project/new.py"),
            Path::new("/project"),
            &project.paths,
            &MoveOptions {
                keep_reexports: false,
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("star import"));
    }
    #[test]
    fn relative_multi_imports_keep_nonmoved_names_relative_and_move_aliases_absolutely() {
        let project = Project::new(&[
            ("pkg/__init__.py", ""),
            ("pkg/source.py", "def run():\n    return 1\nkeep=2\n"),
            (
                "pkg/client.py",
                "from .source import run as execute, keep\nexecute()\n",
            ),
        ]);
        let file = project.file("pkg/source.py");
        let operations = move_symbols(
            &project.db,
            &file,
            at(&file.text, "def run"),
            Path::new("/project/pkg/new.py"),
            Path::new("/project"),
            &project.paths,
            &Default::default(),
        )
        .unwrap();
        let client = project.result(&operations, "pkg/client.py");
        assert!(client.contains("from .source import keep"));
        assert!(client.contains("from pkg.new import run as execute"));
    }
    #[test]
    fn method_to_function_preserves_receiver_wrapper_and_actual_bound_and_unbound_calls() {
        let source = "class C:\n    def run(self, value, *, flag=False):\n        return value if flag else self\nc=C()\nc.run(1)\nC.run(c,2)\ndef unknown(x):\n    return x.run(3)\n";
        let project = Project::new(&[("main.py", source)]);
        let file = project.file("main.py");
        let operations = method_to_function(
            &project.db,
            &file,
            at(source, "def run"),
            "run_function",
            &project.paths,
            Path::new("/project"),
            true,
        )
        .unwrap();
        let result = project.result(&operations, "main.py");
        assert!(result.contains("return run_function(self, value, flag=flag)"));
        assert!(result
            .contains("def run_function(self, value, *, flag=C.run.__kwdefaults__[\"flag\"]):"));
        assert!(result.contains("run_function(c, 1)"));
        assert!(result.contains("run_function(c,2)"));
        assert!(result.contains("x.run(3)"));
        assert!(rustpy_syntax::parse(&result).errors().is_empty());
    }
    #[test]
    fn moving_callable_defaults_reuses_original_objects_instead_of_repeating_factories() {
        let source = "class C:\n    def run(self, values=[]):\n        return values\n";
        let project = Project::new(&[("main.py", source)]);
        let file = project.file("main.py");
        let operations = method_to_function(
            &project.db,
            &file,
            at(source, "def run"),
            "run_function",
            &project.paths,
            Path::new("/project"),
            false,
        )
        .unwrap();
        let result = project.result(&operations, "main.py");
        assert!(result.contains("def run(self, values=[]):"));
        assert!(result.contains("def run_function(self, values=C.run.__defaults__[0]):"));
        assert_eq!(result.matches("values=[]").count(), 1);
        assert!(rustpy_syntax::parse(&result).errors().is_empty());
    }
    #[test]
    fn make_static_method_keeps_function_values_and_updates_resolved_callers() {
        let source = "def calculate(value):\n    return value + 1\nclass C:\n    pass\ncalculate(1)\ncallback=calculate\n";
        let project = Project::new(&[("main.py", source)]);
        let file = project.file("main.py");
        let operations = function_to_static(
            &project.db,
            &file,
            at(source, "def calculate"),
            &file,
            at(source, "class C"),
            crate::ProjectEditContext {
                root: Path::new("/project"),
                files: &project.paths,
            },
            true,
        )
        .unwrap();
        let result = project.result(&operations, "main.py");
        assert!(result.contains("return C.calculate(value)"));
        assert!(result.contains("@staticmethod\n    def calculate(value):"));
        assert!(result.contains("C.calculate(1)"));
        assert!(result.contains("callback=calculate"));
        assert!(rustpy_syntax::parse(&result).errors().is_empty());
    }
    #[test]
    fn method_moves_to_typed_parameter_class_with_original_receiver_as_explicit_input() {
        let source = "class Target:\n    pass\nclass Source:\n    def send(self, destination: Target, value):\n        return destination, self, value\n";
        let project = Project::new(&[("main.py", source)]);
        let file = project.file("main.py");
        let operations = method_via_parameter(
            &project.db,
            &file,
            at(source, "def send"),
            "destination",
            "accept",
        )
        .unwrap();
        let result = project.result(&operations, "main.py");
        assert!(result.contains("def accept(destination, self, value):"));
        assert!(result.contains("return destination.accept(self, value)"));
        assert!(rustpy_syntax::parse(&result).errors().is_empty());
    }
    #[test]
    fn field_promotion_and_lexical_class_conflicts_are_explicit() {
        let source = "class C:\n    def __init__(self):\n        self.value = 1\n";
        let project = Project::new(&[("main.py", source)]);
        let file = project.file("main.py");
        let operations = attribute_to_class(&file, at(source, "self.value")).unwrap();
        let result = project.result(&operations, "main.py");
        assert!(result.contains("class C:\n    value = 1"));
        assert!(result.contains("def __init__(self):\n        pass"));
        let source = "class C:\n    def run(self):\n        return super().run()\n";
        let project = Project::new(&[("main.py", source)]);
        let file = project.file("main.py");
        assert!(method_to_function(
            &project.db,
            &file,
            at(source, "def run"),
            "run_function",
            &project.paths,
            Path::new("/project"),
            true
        )
        .unwrap_err()
        .contains("super"));
    }
}
