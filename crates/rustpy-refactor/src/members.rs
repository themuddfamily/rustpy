//! Physical-identity member refactorings. All edits refer to captured source;
//! dependencies and remaining member uses are checked before common preview.
use crate::refactor::{self, Refactoring};
use crate::refactor2::{self, MultiFileRefactoring};
use rustpy_core::{TextRange, TextSize};
use rustpy_semantic::{
    db::{Analysis, FileInfo},
    edit_helpers::{self, Edit},
    hierarchy,
    infer::Inferer,
    scope::{BindingKind, ScopeKind},
    types::ClassRef,
};
use rustpy_syntax::ast::{self, AstNode};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::PathBuf,
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    PullUp,
    PushDown,
}
#[derive(Clone, Debug)]
pub struct Member {
    pub name: String,
    pub method: bool,
    pub ranges: Vec<TextRange>,
}
#[derive(Clone, Debug)]
pub struct Selection {
    pub name: String,
    pub abstract_declaration: bool,
}
#[derive(Clone, Debug)]
pub struct Destination {
    pub class: ClassRef,
    pub path: PathBuf,
}
#[derive(Clone, Debug)]
pub struct Catalogue {
    pub source: ClassRef,
    pub file: Arc<FileInfo>,
    pub members: Vec<Member>,
    pub bases: Vec<Destination>,
    pub subclasses: Vec<Destination>,
    pub sources: Vec<Arc<FileInfo>>,
}
fn physical(db: &Analysis, class: &ClassRef) -> Result<(Arc<FileInfo>, ast::ClassDef), String> {
    let file = db.file(class.file).ok_or("Class source disappeared")?;
    let node = edit_helpers::node_with_range(&file.parse.syntax_node(), class.range)
        .and_then(ast::ClassDef::cast)
        .filter(|node| node.name_text() == class.name.as_str())
        .ok_or("Class identity changed")?;
    Ok((file, node))
}
fn writable(file: &FileInfo) -> Result<(), String> {
    if file.is_stub
        || file.path.starts_with("<typeshed>")
        || file.path.starts_with("<python-stubs>")
    {
        return Err(format!(
            "{} is a read-only stub/library",
            file.path.display()
        ));
    }
    if file.path.ancestors().any(|path| {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    }) {
        return Err("Symlinked refactoring targets are unsupported".into());
    }
    if std::fs::metadata(&file.path).is_ok_and(|metadata| metadata.permissions().readonly()) {
        return Err(format!("{} is read-only", file.path.display()));
    }
    Ok(())
}
fn declared(stmt: &ast::Stmt) -> Vec<String> {
    match stmt {
        ast::Stmt::FuncDef(def) => vec![def.name_text()],
        ast::Stmt::ClassDef(def) => vec![def.name_text()],
        ast::Stmt::Assign(assign) => assign
            .targets()
            .iter()
            .filter_map(|target| match target {
                ast::Expr::Name(name) => Some(name.name_text()),
                _ => None,
            })
            .collect(),
        ast::Stmt::AnnAssign(assign) => match assign.target() {
            Some(ast::Expr::Name(name)) => vec![name.name_text()],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}
pub fn catalogue(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    paths: &[PathBuf],
) -> Result<Catalogue, String> {
    if paths.len() > 4096 {
        return Err("Member refactoring project scope exceeds 4096 files".into());
    }
    let source = hierarchy::class_at(db, file, offset).ok_or("Place the caret inside a class")?;
    let (file, node) = physical(db, &source)?;
    writable(&file)?;
    if file.text.len() > 8 * 1024 * 1024 {
        return Err("Member refactoring source exceeds 8 MiB".into());
    }
    let mut total = file.text.len();
    let mut sources = vec![file.clone()];
    for path in paths {
        if let Some(other) = db.file_for_path(path) {
            if sources.iter().any(|f| f.id == other.id) {
                continue;
            }
            if other.text.len() > 8 * 1024 * 1024 {
                return Err("Member refactoring source exceeds 8 MiB".into());
            }
            total = total
                .checked_add(other.text.len())
                .ok_or("Source size overflow")?;
            if total > 32 * 1024 * 1024 {
                return Err("Member refactoring source scope exceeds 32 MiB".into());
            }
            sources.push(other);
        }
    }
    let mut members = BTreeMap::<String, Member>::new();
    for stmt in refactor2::body_statements(&node) {
        for name in declared(&stmt) {
            let member = members.entry(name.clone()).or_insert(Member {
                name,
                method: matches!(stmt, ast::Stmt::FuncDef(_)),
                ranges: Vec::new(),
            });
            member.ranges.push(stmt.syntax().text_range());
            member.method &= matches!(stmt, ast::Stmt::FuncDef(_));
        }
    }
    let inf = Inferer::new(db);
    let mut bases = Vec::new();
    for class in inf.mro(&source).iter().skip(1) {
        if class.name.as_str() == "object"
            && db.file(class.file).is_some_and(|source| {
                source.path.starts_with("<typeshed>")
                    && source.module_name.as_deref() == Some("builtins")
            })
        {
            continue;
        }
        if let Ok((base, _)) = physical(db, class) {
            if writable(&base).is_ok() && (base.id == file.id || paths.contains(&base.path)) {
                bases.push(Destination {
                    class: class.clone(),
                    path: base.path.clone(),
                });
            }
        }
    }
    let subclasses = hierarchy::direct_subclasses(db, &source, &sources)
        .into_iter()
        .filter_map(|class| {
            physical(db, &class).ok().map(|(file, _)| Destination {
                class,
                path: file.path.clone(),
            })
        })
        .collect();
    Ok(Catalogue {
        source,
        file,
        members: members.into_values().collect(),
        bases,
        subclasses,
        sources,
    })
}
impl Catalogue {
    pub fn validate(&self, db: &Analysis) -> Result<(), String> {
        for file in &self.sources {
            if db
                .file(file.id)
                .is_none_or(|actual| !Arc::ptr_eq(&actual, file))
            {
                return Err("Source changed; reopen member refactoring".into());
            }
            if !file.is_open {
                if let Some(revision) = file.disk_revision {
                    let actual = rustpy_core::vfs::Vfs::new()
                        .read(&file.path, rustpy_core::vfs::ReadLimits::default())
                        .map_err(|e| e.to_string())?;
                    if actual.revision != revision {
                        return Err("A closed source changed; reopen member refactoring".into());
                    }
                }
            }
        }
        Ok(())
    }
}
fn push(changes: &mut BTreeMap<PathBuf, Vec<Edit>>, file: &FileInfo, edits: Vec<Edit>) {
    changes.entry(file.path.clone()).or_default().extend(edits);
}
fn finish(
    title: String,
    mut changes: BTreeMap<PathBuf, Vec<Edit>>,
) -> Result<MultiFileRefactoring, String> {
    for (path, edits) in &mut changes {
        edits.sort_by_key(|(range, _)| (range.start(), range.end()));
        let mut merged = Vec::<Edit>::new();
        for (range, text) in std::mem::take(edits) {
            if let Some((previous, content)) = merged.last_mut() {
                if previous.is_empty() && *previous == range {
                    content.push_str(&text);
                    continue;
                }
                if previous.end() > range.start() {
                    return Err(format!("Member changes overlap in {}", path.display()));
                }
            }
            merged.push((range, text));
        }
        *edits = merged;
    }
    Ok(MultiFileRefactoring {
        title,
        changes: changes.into_iter().collect(),
    })
}
fn selected_statements(
    class: &ast::ClassDef,
    selection: &[Selection],
) -> Result<Vec<ast::Stmt>, String> {
    let names: Vec<_> = selection.iter().map(|s| s.name.clone()).collect();
    if names.is_empty() {
        return Err("Select at least one method or attribute".into());
    }
    if names
        .iter()
        .any(|name| name.starts_with("__") && !name.ends_with("__"))
    {
        return Err(
            "Private name-mangled members must be renamed before changing their owning class"
                .into(),
        );
    }
    if names.iter().collect::<HashSet<_>>().len() != names.len() {
        return Err("A member was selected more than once".into());
    }
    let statements = refactor2::member_statements(class, &names)?;
    for statement in &statements {
        if declared(statement).iter().any(|name| !names.contains(name)) {
            return Err(
                "Separate shared attribute assignments before moving one of their targets".into(),
            );
        }
        if let ast::Stmt::Assign(assign) = statement {
            if assign
                .targets()
                .iter()
                .any(|target| !matches!(target, ast::Expr::Name(_)))
            {
                return Err(
                    "Destructuring/member-side-effect assignments must be separated before moving"
                        .into(),
                );
            }
        }
        for node in statement.syntax().descendants() {
            if let Some(name) = ast::NameExpr::cast(node.clone()) {
                if name.name_text() == "__class__" {
                    return Err("Moving __class__ closure references changes their owner; rewrite them explicitly first".into());
                }
            }
            if let Some(call) = ast::CallExpr::cast(node) {
                if matches!(call.callee().map(ast::Expr::unparen),Some(ast::Expr::Name(name)) if name.name_text()=="super")
                {
                    return Err("Moving super() changes MRO dispatch; make its intended delegation explicit first".into());
                }
            }
        }
        for node in statement
            .syntax()
            .descendants()
            .filter_map(ast::AttributeExpr::cast)
        {
            let name = node.attr_text();
            if name.starts_with("__") && !name.ends_with("__") {
                return Err("Private name-mangled attributes must be renamed before changing their owning class".into());
            }
        }
    }
    Ok(statements)
}

/// Import the exact source dependencies into another module. Source globals
/// used only in method bodies get local imports, avoiding child→base cycles.
/// Eager header/attribute dependencies cannot be delayed and are refused when
/// they would import the source class's module back during class construction.
#[derive(Default)]
struct DependencyPlan {
    local_imports: BTreeSet<String>,
    references: Vec<Edit>,
}
fn dependencies(
    db: &Analysis,
    source: &Arc<FileInfo>,
    target: &Arc<FileInfo>,
    statement: &ast::Stmt,
    selection: &[Selection],
    target_class: &ClassRef,
    imports: &mut BTreeSet<String>,
) -> Result<DependencyPlan, String> {
    let inf = Inferer::new(db);
    let module = source
        .module_name
        .clone()
        .or_else(|| db.resolver.module_name_of(&source.path))
        .ok_or("Source has no importable module identity")?;
    let range = statement.syntax().text_range();
    let body = match statement {
        ast::Stmt::FuncDef(function) => function.body().map(|body| body.syntax().text_range()),
        _ => None,
    };
    let reserved: HashSet<_> = source
        .scopes
        .bindings
        .iter()
        .chain(&target.scopes.bindings)
        .map(|binding| binding.name.as_str())
        .collect();
    let alias = (0..=4096)
        .map(|index| {
            if index == 0 {
                "_rustpy_source".to_owned()
            } else {
                format!("_rustpy_source{index}")
            }
        })
        .find(|name| !reserved.contains(name.as_str()))
        .ok_or("Cannot reserve a source module alias")?;
    let mut locals = DependencyPlan::default();
    for reference in source
        .scopes
        .references
        .iter()
        .filter(|reference| range.contains_range(reference.range))
    {
        let Some(id) = reference.resolved else {
            if source.id != target.id {
                let name = edit_helpers::slice(&source.text, reference.range);
                if let Some(destination) =
                    inf.resolve_name(target, target.scopes.module_scope(), name)
                {
                    let same =
                        inf.resolve_name(source, reference.scope, name)
                            .is_some_and(|original| {
                                let original = inf.follow_imports(original);
                                let destination = inf.follow_imports(destination);
                                original.file.id == destination.file.id
                                    && original.binding == destination.binding
                            });
                    if !same {
                        return Err(format!(
                            "Unresolved source name '{name}' would capture a destination binding"
                        ));
                    }
                }
            }
            continue;
        };
        let binding = source.scopes.binding(id);
        if range.contains_range(binding.range) {
            continue;
        }
        let kind = source.scopes.scope(binding.scope).kind;
        if kind != ScopeKind::Module {
            if kind == ScopeKind::Class
                && selection
                    .iter()
                    .any(|selected| selected.name == binding.name.as_str())
            {
                continue;
            }
            return Err(format!("'{}' captures an enclosing/class-local binding; move its dependency too or qualify it",binding.name));
        }
        if body.is_none_or(|body| !body.contains_range(reference.range))
            && inf
                .class_info(target_class)
                .is_some_and(|info| info.members.contains_key(&binding.name))
        {
            return Err(format!(
                "Destination class shadows eager dependency '{}'",
                binding.name
            ));
        }
        if source.id == target.id {
            continue;
        }
        if body.is_some_and(|body| body.contains_range(reference.range)) {
            if reference.is_write {
                return Err(format!(
                    "Moved member writes global '{}'; rewrite that state explicitly",
                    binding.name
                ));
            }
            locals
                .local_imports
                .insert(format!("import {module} as {alias}"));
            locals
                .references
                .push((reference.range, format!("{alias}.{}", binding.name)));
            continue;
        }
        let existing = inf.resolve_name(target, target.scopes.module_scope(), &binding.name);
        if let Some(existing) = existing {
            let original = inf
                .resolve_name(source, binding.scope, &binding.name)
                .ok_or("Dependency no longer resolves")?;
            let original = inf.follow_imports(original);
            let existing = inf.follow_imports(existing);
            if original.file.id == existing.file.id && original.binding == existing.binding {
                continue;
            }
            return Err(format!(
                "Destination name '{}' resolves to a different declaration",
                binding.name
            ));
        }
        if reference.is_write {
            return Err(format!(
                "Moved member writes global '{}'; move/rewrite that state explicitly",
                binding.name
            ));
        }
        let statement_text = match &binding.kind {
            BindingKind::Import(info) => {
                refactor2::import_statement_for(info, &module, source.is_package_init())
            }
            BindingKind::Function
            | BindingKind::Class
            | BindingKind::Assignment
            | BindingKind::Annotation
            | BindingKind::TypeAlias => {
                if body.is_some_and(|body| body.contains_range(reference.range)) {
                    locals
                        .local_imports
                        .insert(format!("import {module} as {alias}"));
                    locals
                        .references
                        .push((reference.range, format!("{alias}.{}", binding.name)));
                    continue;
                }
                return Err(format!("Eager reference '{}' would introduce a source/destination import cycle; extract that dependency into a shared module first",binding.name));
            }
            _ => {
                return Err(format!(
                    "Cannot safely transfer dependency '{}'",
                    binding.name
                ))
            }
        };
        // A class-level symbol with this spelling would shadow an imported
        // annotation/default even if the module itself has no conflicting name.
        if inf
            .class_info(target_class)
            .is_some_and(|info| info.members.contains_key(&binding.name))
        {
            return Err(format!(
                "Destination class shadows dependency '{}'",
                binding.name
            ));
        }
        imports.insert(statement_text);
    }
    Ok(locals)
}
fn add_local_imports(
    text: &str,
    statement: &ast::Stmt,
    locals: &DependencyPlan,
) -> Result<String, String> {
    let original = edit_helpers::stmt_lines(text, statement.syntax().text_range());
    let snippet = edit_helpers::slice(text, original).to_owned();
    if locals.local_imports.is_empty() {
        return Ok(snippet);
    }
    let ast::Stmt::FuncDef(function) = statement else {
        return Err("Only methods can defer imports".into());
    };
    let body = function.body().ok_or("Method has no body")?;
    let statements: Vec<_> = body.statements().collect();
    let first = statements.first().ok_or("Method has no statements")?;
    let def = function.def_token().ok_or("Malformed method")?;
    if edit_helpers::line_start(text, first.syntax().text_range().start().into())
        <= edit_helpers::line_start(text, def.text_range().start().into())
    {
        return Err("Expand one-line methods before moving dependencies".into());
    }
    let doc = matches!(first,ast::Stmt::Expr(e) if matches!(e.expr(),Some(ast::Expr::String(_))|Some(ast::Expr::ConcatString(_))));
    let at = if doc {
        edit_helpers::after_last_line(text, first.syntax().text_range().end().into())
    } else {
        edit_helpers::line_start(text, first.syntax().text_range().start().into())
    };
    if at < usize::from(original.start()) || at > usize::from(original.end()) {
        return Err("One-line methods must be expanded before moving dependencies".into());
    }
    let indent = edit_helpers::indent_at(text, first.syntax().text_range().start().into());
    if indent.is_empty() {
        return Err("Expand one-line methods before moving dependencies".into());
    }
    let imports = locals
        .local_imports
        .iter()
        .map(|line| format!("{indent}{line}\n"))
        .collect::<String>();
    let mut edits: Vec<Edit> = locals
        .references
        .iter()
        .map(|(range, text)| {
            (
                TextRange::new(
                    range.start() - original.start(),
                    range.end() - original.start(),
                ),
                text.clone(),
            )
        })
        .collect();
    edits.push((
        TextRange::empty(TextSize::new((at - usize::from(original.start())) as u32)),
        imports,
    ));
    refactor::try_apply_edits(&snippet, &edits).map_err(|error| error.to_string())
}
fn imported(file: &FileInfo, imports: BTreeSet<String>) -> Vec<Edit> {
    if imports.is_empty() {
        return Vec::new();
    }
    let at = refactor2::import_insert_offset(file);
    vec![(
        edit_helpers::rng(at, at),
        format!(
            "{}{}\n",
            if at > 0 && !file.text[..at].ends_with('\n') {
                "\n"
            } else {
                ""
            },
            imports
                .into_iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>()
        ),
    )]
}

fn abc_alias(db: &Analysis, file: &FileInfo) -> Result<String, String> {
    let module = db
        .module("abc")
        .ok_or("Canonical abc module is unavailable")?;
    if !module.path.starts_with("<typeshed>") || module.module_name.as_deref() != Some("abc") {
        return Err("The project shadows abc; choose the standard-library module before making abstract contracts".into());
    }
    let names: HashSet<_> = file
        .scopes
        .bindings
        .iter()
        .map(|binding| binding.name.as_str())
        .collect();
    for index in 0..=4096 {
        let name = if index == 0 {
            "_rustpy_abc".to_owned()
        } else {
            format!("_rustpy_abc{index}")
        };
        if !names.contains(name.as_str()) {
            return Ok(name);
        }
    }
    Err("Cannot reserve an unshadowed abc alias".into())
}
fn abc_edits(
    db: &Analysis,
    file: &Arc<FileInfo>,
    class: &ast::ClassDef,
    alias: &str,
) -> Result<Vec<Edit>, String> {
    let inf = Inferer::new(db);
    let class_ref = ClassRef {
        file: file.id,
        range: class.syntax().text_range(),
        name: class.name_text().into(),
    };
    let mut has_abc = false;
    for owner in inf.mro(&class_ref).iter() {
        if let Ok((source, node)) = physical(db, owner) {
            if source.module_name.as_deref() == Some("abc")
                && source.path.starts_with("<typeshed>")
                && (owner.name.as_str() == "ABC" || owner.name.as_str() == "ABCMeta")
            {
                has_abc = true;
            }
            for keyword in node
                .arguments()
                .into_iter()
                .flat_map(|arguments| arguments.keywords().collect::<Vec<_>>())
                .filter(|keyword| keyword.name_text() == "metaclass")
            {
                let Some(value) = keyword.value() else {
                    return Err("Unknown metaclass".into());
                };
                let Some(meta) = inf.type_of_expr(&source, &value).class_ref().cloned() else {
                    return Err(
                        "Unknown metaclass cannot acquire an abstract contract safely".into(),
                    );
                };
                if !inf.mro(&meta).iter().any(|base| {
                    db.file(base.file).is_some_and(|f| {
                        f.path.starts_with("<typeshed>")
                            && f.module_name.as_deref() == Some("abc")
                            && base.name.as_str() == "ABCMeta"
                    })
                }) {
                    return Err("A custom metaclass must explicitly derive from canonical abc.ABCMeta before this conversion".into());
                }
                has_abc = true;
            }
        }
    }
    let mut edits = imported(file, BTreeSet::from([format!("import abc as {alias}")]));
    if !has_abc {
        edits.push(
            refactor2::add_base_edit(class, &format!("{alias}.ABC"), true)
                .ok_or("Malformed class header")?,
        );
    }
    Ok(edits)
}
fn abstract_snippet(
    file: &FileInfo,
    function: &ast::FuncDef,
    alias: &str,
    stub: bool,
) -> Result<String, String> {
    let lines = edit_helpers::stmt_lines(&file.text, function.syntax().text_range());
    let mut edits = Vec::new();
    let def = function.def_token().ok_or("Method has no def token")?;
    let at = edit_helpers::line_start(&file.text, def.text_range().start().into());
    let indent = edit_helpers::indent_at(&file.text, def.text_range().start().into());
    if at < usize::from(lines.start()) {
        return Err("Expand one-line class/method suites before abstract conversion".into());
    }
    edits.push((
        TextRange::empty(TextSize::new((at - usize::from(lines.start())) as u32)),
        format!("{indent}@{alias}.abstractmethod\n"),
    ));
    if stub {
        let body = function.body().ok_or("Method has no body")?;
        let statements: Vec<_> = body.statements().collect();
        let first = statements.first().ok_or("Method has no body statements")?;
        let last = statements.last().ok_or("Method has no body statements")?;
        let start =
            edit_helpers::line_start(&file.text, first.syntax().text_range().start().into());
        let end =
            edit_helpers::after_last_line(&file.text, last.syntax().text_range().end().into());
        if start <= usize::from(def.text_range().start()) {
            return Err("Expand one-line methods before abstract conversion".into());
        }
        let body_indent =
            edit_helpers::indent_at(&file.text, first.syntax().text_range().start().into());
        edits.push((
            edit_helpers::rng(
                start - usize::from(lines.start()),
                end - usize::from(lines.start()),
            ),
            format!("{body_indent}raise NotImplementedError\n"),
        ));
    }
    refactor::try_apply_edits(edit_helpers::slice(&file.text, lines), &edits)
        .map_err(|e| e.to_string())
}
fn receiver_dependencies(
    db: &Analysis,
    file: &Arc<FileInfo>,
    source: &ClassRef,
    target: &ClassRef,
    statement: &ast::Stmt,
    selected: &[Selection],
) -> Result<(), String> {
    let inf = Inferer::new(db);
    if inf.class_info(source).is_none() {
        return Err("Source class metadata is unavailable".into());
    }
    for attribute in statement
        .syntax()
        .descendants()
        .filter_map(ast::AttributeExpr::cast)
    {
        if edit_helpers::is_assignment_target(attribute.syntax()) {
            continue;
        }
        let Some(object) = attribute.object() else {
            continue;
        };
        let Some(receiver) = inf.type_of_expr(file, &object).class_ref().cloned() else {
            continue;
        };
        if &receiver != source {
            continue;
        }
        let name = attribute.attr_text();
        if selected.iter().any(|selection| selection.name == name) {
            continue;
        }
        let original = inf.mro(source).iter().find_map(|class| {
            inf.class_info(class)
                .and_then(|info| info.members.get(name.as_str()).cloned())
        });
        let Some(original) = original else {
            continue;
        };
        let destination = inf.mro(target).iter().find_map(|class| {
            inf.class_info(class)
                .and_then(|info| info.members.get(name.as_str()).cloned())
        });
        if destination
            .is_none_or(|member| member.file != original.file || member.range != original.range)
        {
            return Err(format!("Moved member depends on '{}.{}'; select that member too or keep a compatible contract in the destination",source.name,name));
        }
    }
    Ok(())
}
fn push_uses(
    db: &Analysis,
    catalogue: &Catalogue,
    selected: &[Selection],
    statements: &[ast::Stmt],
    targets: &[ClassRef],
) -> Result<(), String> {
    let inf = Inferer::new(db);
    let paths: Vec<_> = catalogue
        .sources
        .iter()
        .map(|source| source.path.clone())
        .collect();
    for selection in selected
        .iter()
        .filter(|selection| !selection.abstract_declaration)
    {
        for sub in &catalogue.subclasses {
            if !targets.contains(&sub.class)
                && inf
                    .class_info(&sub.class)
                    .is_none_or(|info| !info.members.contains_key(selection.name.as_str()))
            {
                return Err(format!("Unselected subclass '{}' still inherits '{}'; select it or retain an abstract declaration",sub.class.name,selection.name));
            }
        }
        let Some(member) = inf
            .class_info(&catalogue.source)
            .and_then(|info| info.members.get(selection.name.as_str()).cloned())
        else {
            continue;
        };
        for usage in
            rustpy_search::usages::find_usages(db, &catalogue.file, member.range.start(), &paths)
        {
            if usage.is_definition
                || (usage.path == catalogue.file.path
                    && statements
                        .iter()
                        .any(|stmt| stmt.syntax().text_range().contains_range(usage.range)))
            {
                continue;
            }
            let Some(file) = db.file_for_path(&usage.path) else {
                return Err("A usage source is unavailable".into());
            };
            let Some(token) =
                edit_helpers::token_at(&file.parse.syntax_node(), usage.range.start())
            else {
                continue;
            };
            let attribute = token.parent_ancestors().find_map(ast::AttributeExpr::cast);
            let safe = attribute
                .and_then(|attribute| attribute.object())
                .map(|receiver| inf.type_of_expr(&file, &receiver))
                .and_then(|ty| ty.class_ref().cloned())
                .is_some_and(|receiver| {
                    inf.mro(&receiver)
                        .iter()
                        .any(|class| targets.contains(class))
                });
            if !safe {
                return Err(format!("{}:{} still uses '{}' through a base/unknown receiver; retain an abstract declaration or update that use first",usage.path.display(),usage.line+1,selection.name));
            }
        }
    }
    Ok(())
}

pub fn plan(
    db: &Analysis,
    catalogue: &Catalogue,
    direction: Direction,
    selection: &[Selection],
    targets: &[ClassRef],
) -> Result<MultiFileRefactoring, String> {
    catalogue.validate(db)?;
    let (file, class) = physical(db, &catalogue.source)?;
    let statements = selected_statements(&class, selection)?;
    if targets.is_empty() {
        return Err("Select a destination class".into());
    }
    if direction == Direction::PullUp && targets.len() != 1 {
        return Err("Pull Up needs exactly one actual superclass".into());
    }
    let allowed = if direction == Direction::PullUp {
        &catalogue.bases
    } else {
        &catalogue.subclasses
    };
    let mut unique = HashSet::new();
    for target in targets {
        if !allowed
            .iter()
            .any(|destination| &destination.class == target)
            || !unique.insert(target.clone())
        {
            return Err(
                "Destination is not a unique superclass/direct subclass of this class".into(),
            );
        }
    }
    for selection in selection
        .iter()
        .filter(|selection| selection.abstract_declaration)
    {
        if !catalogue
            .members
            .iter()
            .any(|member| member.name == selection.name && member.method)
        {
            return Err("Only methods/properties support abstract conversion; attributes must move as concrete declarations".into());
        }
    }
    if direction == Direction::PushDown {
        push_uses(db, catalogue, selection, &statements, targets)?;
    }
    let from = refactor2::class_body_indent(&file.text, &class);
    let mut changes = BTreeMap::new();
    for target in targets {
        let (target_file, target_class) = physical(db, target)?;
        writable(&target_file)?;
        let names = refactor2::defined_names(&target_class);
        for selection in selection {
            if names.contains(&selection.name) {
                return Err(format!(
                    "'{}' in {} already declares '{}'",
                    target.name,
                    target_file.path.display(),
                    selection.name
                ));
            }
        }
        let to = refactor2::class_body_indent(&target_file.text, &target_class);
        let abstract_target = direction == Direction::PullUp
            && selection
                .iter()
                .any(|selection| selection.abstract_declaration);
        let alias = if abstract_target {
            Some(abc_alias(db, &target_file)?)
        } else {
            None
        };
        let mut snippets = Vec::new();
        let mut imports = BTreeSet::new();
        for statement in &statements {
            let abstract_declaration = direction == Direction::PullUp
                && declared(statement).iter().any(|name| {
                    selection
                        .iter()
                        .any(|selection| selection.name == *name && selection.abstract_declaration)
                });
            let text = if abstract_declaration {
                let ast::Stmt::FuncDef(function) = statement else {
                    return Err("Only methods can become abstract".into());
                };
                // Header dependencies remain necessary even when the body is a stub.
                let locals = dependencies(
                    db,
                    &file,
                    &target_file,
                    statement,
                    selection,
                    target,
                    &mut imports,
                )?;
                let _ = locals;
                abstract_snippet(&file, function, alias.as_deref().unwrap_or(""), true)?
            } else {
                receiver_dependencies(db, &file, &catalogue.source, target, statement, selection)?;
                let locals = dependencies(
                    db,
                    &file,
                    &target_file,
                    statement,
                    selection,
                    target,
                    &mut imports,
                )?;
                add_local_imports(&file.text, statement, &locals)?
            };
            snippets.push((
                matches!(statement, ast::Stmt::FuncDef(_) | ast::Stmt::ClassDef(_)),
                edit_helpers::reindent(&text, &from, &to),
            ));
        }
        push(
            &mut changes,
            &target_file,
            refactor2::insert_members(&target_file.text, &target_class, &snippets),
        );
        push(&mut changes, &target_file, imported(&target_file, imports));
        if let Some(alias) = alias {
            push(
                &mut changes,
                &target_file,
                abc_edits(db, &target_file, &target_class, &alias)?,
            );
        }
    }
    let remove: Vec<_> = statements
        .iter()
        .filter(|statement| {
            !declared(statement).iter().any(|name| {
                selection
                    .iter()
                    .any(|selection| selection.name == *name && selection.abstract_declaration)
            })
        })
        .cloned()
        .collect();
    push(
        &mut changes,
        &file,
        refactor2::remove_members(&file.text, &class, &remove),
    );
    if direction == Direction::PushDown
        && selection
            .iter()
            .any(|selection| selection.abstract_declaration)
    {
        let alias = abc_alias(db, &file)?;
        push(&mut changes, &file, abc_edits(db, &file, &class, &alias)?);
        for statement in &statements {
            if !declared(statement).iter().any(|name| {
                selection
                    .iter()
                    .any(|selection| selection.name == *name && selection.abstract_declaration)
            }) {
                continue;
            }
            let ast::Stmt::FuncDef(function) = statement else {
                return Err("Only methods can become abstract".into());
            };
            let text = abstract_snippet(&file, function, &alias, true)?;
            push(
                &mut changes,
                &file,
                vec![(
                    edit_helpers::stmt_lines(&file.text, statement.syntax().text_range()),
                    text,
                )],
            );
        }
    }
    finish(
        format!(
            "{} members {}",
            if direction == Direction::PullUp {
                "Pull"
            } else {
                "Push"
            },
            if direction == Direction::PullUp {
                "up"
            } else {
                "down"
            }
        ),
        changes,
    )
}

/// Make Abstract preserves the implementation (cooperative super calls remain
/// available), adds an innermost decorator and installs canonical ABC authority.
pub fn make_abstract(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Result<Refactoring, String> {
    writable(file)?;
    let function = edit_helpers::ancestor_at::<ast::FuncDef>(&file.parse.syntax_node(), offset)
        .ok_or("Place the caret inside a method")?;
    if !function.is_method() {
        return Err("Only a method can be made abstract".into());
    }
    let class = function
        .syntax()
        .ancestors()
        .skip(1)
        .find_map(ast::ClassDef::cast)
        .ok_or("Method has no owning class")?;
    if function.decorators().any(|decorator| {
        decorator
            .name_text()
            .is_some_and(|name| name == "abstractmethod" || name.ends_with(".abstractmethod"))
    }) {
        return Err("This method already declares an abstract decorator".into());
    }
    let alias = abc_alias(db, file)?;
    let mut edits = abc_edits(db, file, &class, &alias)?;
    edits.push((
        edit_helpers::stmt_lines(&file.text, function.syntax().text_range()),
        abstract_snippet(file, &function, &alias, false)?,
    ));
    let mut changes = finish(
        format!("Make '{}' abstract", function.name_text()),
        BTreeMap::from([(file.path.clone(), edits)]),
    )?;
    Ok(Refactoring {
        title: changes.title,
        edits: changes.changes.remove(0).1,
        rename_ranges: Vec::new(),
        name: function.name_text(),
    })
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
            let db = Analysis::for_project(std::path::Path::new("/project"));
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
                .file_for_path(std::path::Path::new(&format!("/project/{name}")))
                .unwrap()
        }
        fn catalogue(&self, file: &str, class: &str) -> Catalogue {
            let file = self.file(file);
            let offset =
                TextSize::new(file.text.find(&format!("class {class}")).unwrap() as u32 + 6);
            catalogue(&self.db, &file, offset, &self.paths).unwrap()
        }
        fn output(&self, result: &MultiFileRefactoring, name: &str) -> String {
            let file = self.file(name);
            let edits = result
                .changes
                .iter()
                .find(|(path, _)| path == &file.path)
                .unwrap();
            refactor::try_apply_edits(&file.text, &edits.1).unwrap()
        }
    }
    fn selection(name: &str, abstract_declaration: bool) -> Selection {
        Selection {
            name: name.into(),
            abstract_declaration,
        }
    }
    #[test]
    fn pull_method_and_attribute_into_actual_aliased_superclass_with_imports() {
        let p=Project::new(&[("base.py","class Base:\n    pass\n"),("child.py","from base import Base as Parent\nimport math as maths\nclass Child(Parent):\n    scale = 2\n    def size(self):\n        return maths.ceil(self.scale)\n")]);
        let c = p.catalogue("child.py", "Child");
        let result = plan(
            &p.db,
            &c,
            Direction::PullUp,
            &[selection("scale", false), selection("size", false)],
            &[c.bases[0].class.clone()],
        )
        .unwrap();
        let base = p.output(&result, "base.py");
        let child = p.output(&result, "child.py");
        assert!(base.contains("import child as _rustpy_source"), "{base}");
        assert!(base.contains("scale = 2"));
        assert!(base.contains("def size(self)"));
        assert!(!child.contains("def size"));
        assert!(child.contains("pass"));
        assert!(rustpy_syntax::parse(&base).errors().is_empty());
        assert!(rustpy_syntax::parse(&child).errors().is_empty());
    }
    #[test]
    fn abstract_pull_retains_concrete_property_and_uses_unshadowed_abc() {
        let p=Project::new(&[("a.py","abstractmethod = 7\nABC = 9\nclass Base:\n    pass\nclass Child(Base):\n    @property\n    def value(self) -> int:\n        return 1\n")]);
        let c = p.catalogue("a.py", "Child");
        let result = plan(
            &p.db,
            &c,
            Direction::PullUp,
            &[selection("value", true)],
            &[c.bases[0].class.clone()],
        )
        .unwrap();
        let text = p.output(&result, "a.py");
        assert!(text.contains("import abc as _rustpy_abc"), "{text}");
        assert!(text.contains("class Base(_rustpy_abc.ABC)"));
        assert!(
            text.contains("@property\n    @_rustpy_abc.abstractmethod\n    def value"),
            "{text}"
        );
        assert!(text.contains("raise NotImplementedError"));
        assert!(text.contains("return 1"));
    }
    #[test]
    fn push_selected_subclass_requires_retained_contract_for_other_inheritors() {
        let p=Project::new(&[("a.py","class Base:\n    def run(self):\n        return 1\nclass Left(Base):\n    pass\nclass Right(Base):\n    pass\n")]);
        let c = p.catalogue("a.py", "Base");
        let left = c
            .subclasses
            .iter()
            .find(|sub| sub.class.name.as_str() == "Left")
            .unwrap()
            .class
            .clone();
        assert!(plan(
            &p.db,
            &c,
            Direction::PushDown,
            &[selection("run", false)],
            std::slice::from_ref(&left)
        )
        .unwrap_err()
        .contains("Unselected subclass"));
        let result = plan(
            &p.db,
            &c,
            Direction::PushDown,
            &[selection("run", true)],
            &[left],
        )
        .unwrap();
        let text = p.output(&result, "a.py");
        assert_eq!(text.matches("def run").count(), 2);
        assert!(text.contains("class Right(Base):\n    pass"));
        assert!(text.contains("abstractmethod"));
    }
    #[test]
    fn concrete_push_moves_attribute_and_method_to_all_selected_classes() {
        let p=Project::new(&[("a.py","class Base:\n    limit = 1\n    def run(self):\n        return self.limit\nclass Left(Base):\n    pass\nclass Right(Base):\n    pass\n")]);
        let c = p.catalogue("a.py", "Base");
        let targets = c
            .subclasses
            .iter()
            .map(|sub| sub.class.clone())
            .collect::<Vec<_>>();
        let result = plan(
            &p.db,
            &c,
            Direction::PushDown,
            &[selection("limit", false), selection("run", false)],
            &targets,
        )
        .unwrap();
        let text = p.output(&result, "a.py");
        assert_eq!(text.matches("def run").count(), 2);
        assert_eq!(text.matches("limit = 1").count(), 2);
        assert!(text.contains("class Base:\n    pass"), "{text}");
    }
    #[test]
    fn destination_conflicts_and_unselected_receiver_dependencies_are_not_overwritten() {
        let p=Project::new(&[("a.py","class Base:\n    taken = 1\nclass Child(Base):\n    taken = 2\n    needed = 3\n    def run(self):\n        return self.needed\n")]);
        let c = p.catalogue("a.py", "Child");
        let target = [c.bases[0].class.clone()];
        assert!(plan(
            &p.db,
            &c,
            Direction::PullUp,
            &[selection("taken", false)],
            &target
        )
        .unwrap_err()
        .contains("already declares"));
        assert!(plan(
            &p.db,
            &c,
            Direction::PullUp,
            &[selection("run", false)],
            &target
        )
        .unwrap_err()
        .contains("depends on"));
    }
    #[test]
    fn member_source_identity_and_opaque_owner_dependencies_are_guarded() {
        let p=Project::new(&[("a.py","class Base:\n    pass\nclass Child(Base):\n    def run(self):\n        return super().run()\n")]);
        let c = p.catalogue("a.py", "Child");
        assert!(plan(
            &p.db,
            &c,
            Direction::PullUp,
            &[selection("run", false)],
            &[c.bases[0].class.clone()]
        )
        .unwrap_err()
        .contains("super()"));
        p.db.set_file_text(
            std::path::Path::new("/project/a.py"),
            "class Base: pass\nclass Child(Base): pass\n",
        );
        assert!(c.validate(&p.db).is_err());
    }
    #[test]
    fn cross_file_source_helpers_are_imported_lazily_inside_the_moved_method() {
        let p=Project::new(&[("base.py","class Base:\n    pass\n"),("child.py","from base import Base\ndef helper():\n    return 1\nclass Child(Base):\n    def run(self):\n        return helper()\n")]);
        let c = p.catalogue("child.py", "Child");
        let result = plan(
            &p.db,
            &c,
            Direction::PullUp,
            &[selection("run", false)],
            &[c.bases[0].class.clone()],
        )
        .unwrap();
        let text = p.output(&result, "base.py");
        assert!(
            text.contains(
                "def run(self):\n        import child as _rustpy_source\n        return _rustpy_source.helper()"
            ),
            "{text}"
        );
    }
    #[test]
    fn make_abstract_preserves_body_and_puts_abc_before_explicit_object() {
        let p = Project::new(&[(
            "a.py",
            "class C(object):\n    @classmethod\n    def run(cls):\n        return 1\n",
        )]);
        let file = p.file("a.py");
        let result = make_abstract(
            &p.db,
            &file,
            TextSize::new(file.text.find("run").unwrap() as u32),
        )
        .unwrap();
        let text = refactor::try_apply_edits(&file.text, &result.edits).unwrap();
        assert!(text.contains("class C(_rustpy_abc.ABC, object)"), "{text}");
        assert!(text.contains("@classmethod\n    @_rustpy_abc.abstractmethod"));
        assert!(text.contains("return 1"));
    }
}
