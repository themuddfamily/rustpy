//! Captured Safe Delete plans. A plan contains every definition/call/import edit
//! and the unresolved usages which require an explicit Delete Anyway decision.
use crate::{
    change_signature, refactor,
    refactor2::{self, FileOp},
};
use rustpy_core::{TextRange, TextSize};
use rustpy_semantic::{
    db::{Analysis, FileInfo},
    edit_helpers::{ancestor_at, node_with_range, statement_removals, token_at},
    scope::BindingKind,
    usages::{self, Symbol},
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Symbol,
    Attribute,
    Parameter,
    File,
    Module,
}
#[derive(Clone, Debug)]
pub struct Plan {
    pub name: String,
    pub target: PathBuf,
    pub kind: Kind,
    /// Usages left dangling, not the calls/imports this plan rewrites.
    pub blockers: Vec<usages::Usage>,
    pub warnings: Vec<String>,
    pub operations: Vec<FileOp>,
    pub updated_calls: usize,
    pub sources: Vec<Arc<FileInfo>>,
    pub project_files: Vec<PathBuf>,
}
impl Plan {
    pub fn needs_force(&self) -> bool {
        !self.blockers.is_empty() || !self.warnings.is_empty()
    }
    /// All sources, including callers which did not need an edit, are query
    /// dependencies: adding a new usage invalidates the confirmation too.
    pub fn validate_sources(&self, db: &Analysis, files: &[PathBuf]) -> Result<(), String> {
        let mut files = files.to_vec();
        files.sort();
        files.dedup();
        if files != self.project_files {
            return Err("Project files changed; rebuild Safe Delete".into());
        }
        for source in &self.sources {
            if db
                .file(source.id)
                .is_none_or(|current| !Arc::ptr_eq(&current, source))
            {
                return Err(format!(
                    "{} changed; rebuild Safe Delete",
                    source.path.display()
                ));
            }
            if let Some(revision) = source.disk_revision {
                let current = rustpy_core::vfs::Vfs::new()
                    .read(
                        &source.path,
                        rustpy_core::vfs::ReadLimits {
                            max_file_bytes: 8 * 1024 * 1024,
                            max_cached_bytes: 8 * 1024 * 1024,
                            max_cached_files: 1,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                if current.revision != revision {
                    return Err(format!(
                        "{} changed on disk; rebuild Safe Delete",
                        source.path.display()
                    ));
                }
            }
        }
        Ok(())
    }
}
fn candidates(db: &Analysis, files: &[PathBuf]) -> Result<Vec<Arc<FileInfo>>, String> {
    if files.len() > 4096 {
        return Err("Safe Delete supports at most 4096 candidate files per project scope".into());
    }
    let mut total = 0usize;
    let mut result = Vec::new();
    for path in files {
        if !text_candidate(path) {
            continue;
        }
        let Some(file) = db.file_for_path(path) else {
            continue;
        };
        total = total.saturating_add(file.text.len());
        if file.text.len() > 8 * 1024 * 1024 || total > 32 * 1024 * 1024 {
            return Err("Safe Delete source budget exceeded (8 MiB/file, 32 MiB total)".into());
        }
        result.push(file);
    }
    Ok(result)
}
fn text_candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext,
                "py" | "pyi"
                    | "pyw"
                    | "txt"
                    | "md"
                    | "rst"
                    | "json"
                    | "toml"
                    | "yaml"
                    | "yml"
                    | "ini"
                    | "cfg"
                    | "html"
                    | "htm"
                    | "xml"
                    | "js"
                    | "ts"
                    | "css"
                    | "sql"
                    | "ipynb"
            )
        })
}
fn writable_source(file: &FileInfo) -> Result<(), String> {
    if file.path.starts_with("<typeshed>") || file.path.starts_with("<python-stubs>") {
        return Err("Bundled/library definitions are read-only; navigate to project source".into());
    }
    if std::fs::metadata(&file.path).is_ok_and(|metadata| metadata.permissions().readonly()) {
        return Err(format!("{} is read-only", file.path.display()));
    }
    Ok(())
}
fn project_list(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = files.to_vec();
    paths.sort();
    paths.dedup();
    paths
}
fn normalize_ops(
    mut edits: BTreeMap<PathBuf, Vec<(TextRange, String)>>,
    delete: Option<PathBuf>,
) -> Result<Vec<FileOp>, String> {
    let mut result = Vec::new();
    for (path, items) in &mut edits {
        items.sort_by_key(|(range, _)| (range.start(), range.end()));
        items.dedup();
        if items
            .windows(2)
            .any(|pair| pair[0].0.end() > pair[1].0.start())
        {
            return Err(format!("Safe Delete changes overlap in {}", path.display()));
        }
        if !items.is_empty() {
            result.push(FileOp::Edit(path.clone(), std::mem::take(items)));
        }
    }
    if let Some(path) = delete {
        result.push(FileOp::Delete(path));
    }
    Ok(result)
}
fn dedup_usages(usages: &mut Vec<usages::Usage>) {
    usages.sort_by(|a, b| {
        (&a.path, a.range.start(), a.range.end()).cmp(&(&b.path, b.range.start(), b.range.end()))
    });
    usages.dedup_by(|a, b| a.path == b.path && a.range == b.range);
}

pub fn symbol(
    db: &Analysis,
    input: &Arc<FileInfo>,
    offset: TextSize,
    files: &[PathBuf],
) -> Result<Plan, String> {
    let mut sources = candidates(db, files)?;
    let selected = usages::symbol_at(db, input, offset);
    let (owner, at) = match &selected {
        // At an import's actual declaration, delete that alias/re-export, not
        // the declaration in its provider module.
        _ if input
            .scopes
            .binding_at(offset)
            .is_some_and(|(_, binding)| matches!(binding.kind, BindingKind::Import(_))) =>
        {
            (input.clone(), offset)
        }
        Some(Symbol::Name { def, .. }) => (def.file.clone(), def.binding().range.start()),
        Some(Symbol::Member {
            decl: Some((file, range)),
            ..
        }) => (file.clone(), range.start()),
        _ => return Err("The symbol has no unique physical declaration".into()),
    };
    writable_source(&owner)?;
    if !sources.iter().any(|source| source.id == owner.id) {
        sources.push(owner.clone());
    }
    if let Some((binding, definition)) = owner.scopes.binding_at(at).filter(|(_, binding)| {
        matches!(
            binding.kind,
            BindingKind::Param | BindingKind::StarParam | BindingKind::StarStarParam
        )
    }) {
        return parameter(
            db,
            &owner,
            binding,
            definition.range.start(),
            files,
            sources,
        );
    }
    let selected = usages::symbol_at(db, &owner, at);
    let name = if let Some((_, binding)) = owner
        .scopes
        .binding_at(at)
        .filter(|(_, binding)| matches!(binding.kind, BindingKind::Import(_)))
    {
        binding.name.to_string()
    } else {
        selected
            .as_ref()
            .ok_or("Declaration no longer resolves")?
            .name()
            .to_owned()
    };
    let force = refactor::safe_delete_anyway(db, &owner, at, files)?;
    let mut edits: BTreeMap<_, _> = force.changes.into_iter().collect();
    let mut ignored = Vec::<TextRange>::new();
    let mut kind = Kind::Symbol;
    if let Some(Symbol::Member {
        owner: receiver, ..
    }) = &selected
    {
        if let Some(class) = receiver.class_ref().filter(|class| class.file == owner.id) {
            kind = Kind::Attribute;
            let class_node = node_with_range(&owner.parse.syntax_node(), class.range)
                .ok_or("Member class changed")?;
            let mut declarations = Vec::new();
            for usage in rustpy_search::usages::find_usages(db, &owner, at, files)
                .into_iter()
                .filter(|usage| {
                    usage.path == owner.path
                        && !usage.dynamic
                        && (usage.is_write || usage.is_definition)
                })
            {
                let Some(token) = token_at(&owner.parse.syntax_node(), usage.range.start()) else {
                    continue;
                };
                let Some(statement) = token.parent_ancestors().find(|node| {
                    matches!(
                        node.kind(),
                        SyntaxKind::ASSIGN_STMT
                            | SyntaxKind::ANN_ASSIGN_STMT
                            | SyntaxKind::AUG_ASSIGN_STMT
                            | SyntaxKind::FUNC_DEF
                            | SyntaxKind::CLASS_DEF
                    )
                }) else {
                    continue;
                };
                if statement
                    .ancestors()
                    .skip(1)
                    .find(|node| node.kind() == SyntaxKind::CLASS_DEF)
                    .as_ref()
                    != Some(&class_node)
                {
                    continue;
                }
                if let Some(assign) = ast::AssignStmt::cast(statement.clone()) {
                    if assign.targets().len() != 1
                        || !matches!(
                            &assign.targets()[0],
                            ast::Expr::Name(_) | ast::Expr::Attribute(_)
                        )
                    {
                        return Err("A field shares a multiple/destructuring assignment; separate that assignment before deleting it".into());
                    }
                }
                if !declarations
                    .iter()
                    .any(|node: &SyntaxNode| node.text_range() == statement.text_range())
                {
                    declarations.push(statement);
                }
            }
            if !declarations.is_empty() {
                ignored = declarations.iter().map(|node| node.text_range()).collect();
                edits.insert(
                    owner.path.clone(),
                    statement_removals(&owner.text, &declarations),
                );
            }
        }
    }
    let mut blockers = match refactor::safe_delete(db, &owner, at, files) {
        Ok(_) => Vec::new(),
        Err(refactor::SafeDeleteError::Used { usages, .. }) => usages,
        Err(error) => return Err(error.to_string()),
    };
    blockers.retain(|usage| {
        !(usage.path == owner.path
            && ignored
                .iter()
                .any(|range| range.contains_range(usage.range)))
    });
    // Imports which are actually removed need no Delete Anyway confirmation;
    // value uses and dynamic matches still do.
    blockers.retain(|usage| {
        !edits.get(&usage.path).is_some_and(|changes| {
            changes
                .iter()
                .any(|(range, _)| range.contains_range(usage.range))
        })
    });
    let mut removed = BTreeMap::<PathBuf, BTreeSet<String>>::new();
    removed
        .entry(owner.path.clone())
        .or_default()
        .insert(name.clone());
    collect_removed_import_bindings(&sources, &edits, &mut removed);
    remove_static_exports(&sources, &removed, &mut edits)?;
    dedup_usages(&mut blockers);
    let operations = normalize_ops(edits, None)?;
    Ok(Plan {
        name,
        target: owner.path.clone(),
        kind,
        blockers,
        warnings: Vec::new(),
        operations,
        updated_calls: 0,
        sources,
        project_files: project_list(files),
    })
}

fn parameter(
    db: &Analysis,
    file: &Arc<FileInfo>,
    binding: rustpy_semantic::scope::BindingId,
    at: TextSize,
    files: &[PathBuf],
    sources: Vec<Arc<FileInfo>>,
) -> Result<Plan, String> {
    let function = ancestor_at::<ast::FuncDef>(&file.parse.syntax_node(), at)
        .ok_or("Lambda parameters cannot be changed through a function signature")?;
    let mut model = change_signature::signature_model(file, at)?;
    let name = file.scopes.binding(binding).name.to_string();
    let index = model
        .original
        .iter()
        .position(|parameter| parameter.name == name)
        .ok_or("Parameter is not in the original signature")?;
    if model.implicit_first && index == 0 {
        return Err("The implicit self/cls receiver cannot be deleted".into());
    }
    model
        .params
        .retain(|parameter| parameter.origin != Some(index));
    model.propagate_overrides = true;
    let (changed, summary) = change_signature::delete_parameter_anyway(db, file, &model, files)?;
    let mut blockers: Vec<_> = file
        .scopes
        .references_to(binding)
        .into_iter()
        .map(|reference| usages::usage_at(file, reference.range, reference.is_write, false))
        .collect();
    let mut warnings = Vec::new();
    for (usage, reason) in &summary.skipped {
        blockers.push(usage.clone());
        warnings.push(format!(
            "{}:{}: {reason}",
            usage.path.display(),
            usage.line + 1
        ));
    }
    // Reuse the exact argument binding/caller edits. Any removed effectful
    // expression requires explicit consent instead of silently dropping work.
    for (path, changes) in &changed.changes {
        let Some(source) = sources
            .iter()
            .find(|source| &source.path == path)
            .cloned()
            .or_else(|| db.file_for_path(path))
        else {
            continue;
        };
        for call in source
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
        {
            let range = call.syntax().text_range();
            if changes.iter().any(|(edit, replacement)| {
                edit.contains_range(range)
                    && !replacement.contains(&call.syntax().text().to_string())
            }) {
                let usage = usages::usage_at(&source, range, false, false);
                warnings.push(format!(
                    "Deleting an argument also removes evaluation of {}:{}",
                    path.display(),
                    usage.line + 1
                ));
                blockers.push(usage);
            }
        }
    }
    if let Some(name_token) = function.name() {
        for usage in
            rustpy_search::usages::find_usages(db, file, name_token.text_range().start(), files)
                .into_iter()
                .filter(|usage| !usage.is_definition && !usage.non_code)
        {
            let Some(source) = db.file_for_path(&usage.path) else {
                continue;
            };
            let Some(token) = token_at(&source.parse.syntax_node(), usage.range.start()) else {
                continue;
            };
            if token
                .parent_ancestors()
                .any(|node| node.kind() == SyntaxKind::IMPORT_ALIAS)
            {
                continue;
            }
            let invoked = token
                .parent_ancestors()
                .find_map(ast::CallExpr::cast)
                .and_then(|call| call.callee())
                .is_some_and(|callee| callee.syntax().text_range().contains_range(usage.range));
            if !invoked {
                blockers.push(usage);
                warnings.push(
                    "The function is used as a value; indirect callers may need updating".into(),
                );
            }
        }
    }
    warnings.sort();
    warnings.dedup();
    dedup_usages(&mut blockers);
    let edits = changed.changes.into_iter().collect();
    Ok(Plan {
        name: format!("{} · parameter {name}", model.func_name),
        target: file.path.clone(),
        kind: Kind::Parameter,
        blockers,
        warnings,
        operations: normalize_ops(edits, None)?,
        updated_calls: summary.updated_calls,
        sources,
        project_files: project_list(files),
    })
}

fn decoded(node: &SyntaxNode) -> Option<String> {
    match rustpy_syntax::string_literal::string(&node.text().to_string()).ok()? {
        rustpy_syntax::string_literal::Decoded::Text(value, _) => Some(value),
        _ => None,
    }
}
fn collect_removed_import_bindings(
    sources: &[Arc<FileInfo>],
    edits: &BTreeMap<PathBuf, Vec<(TextRange, String)>>,
    removed: &mut BTreeMap<PathBuf, BTreeSet<String>>,
) {
    for source in sources {
        let Some(changes) = edits.get(&source.path) else {
            continue;
        };
        let Ok(updated) = refactor::try_apply_edits(&source.text, changes) else {
            continue;
        };
        let parsed = rustpy_syntax::parse(&updated);
        let retained: BTreeSet<String> = parsed
            .syntax_node()
            .descendants()
            .filter_map(ast::ImportAlias::cast)
            .map(|alias| {
                alias
                    .alias()
                    .map(|token| token.text().to_string())
                    .unwrap_or_else(|| alias.name().split('.').next().unwrap_or("").to_owned())
            })
            .collect();
        for binding in &source.scopes.bindings {
            if matches!(binding.kind, BindingKind::Import(_))
                && !retained.contains(binding.name.as_str())
                && changes
                    .iter()
                    .any(|(range, _)| range.contains_range(binding.range))
            {
                removed
                    .entry(source.path.clone())
                    .or_default()
                    .insert(binding.name.to_string());
            }
        }
    }
}
fn remove_static_exports(
    sources: &[Arc<FileInfo>],
    removed: &BTreeMap<PathBuf, BTreeSet<String>>,
    edits: &mut BTreeMap<PathBuf, Vec<(TextRange, String)>>,
) -> Result<(), String> {
    for source in sources {
        let Some(names) = removed.get(&source.path) else {
            continue;
        };
        for assign in source
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::AssignStmt::cast)
        {
            if !assign
                .targets()
                .iter()
                .any(|target| matches!(target,ast::Expr::Name(name)if name.name_text()=="__all__"))
            {
                continue;
            }
            let Some(value) = assign.value() else {
                continue;
            };
            let elements: Vec<_> = match value.clone().unparen() {
                ast::Expr::List(list) => list.elements().collect(),
                ast::Expr::Tuple(tuple) => tuple.elements().collect(),
                _ => continue,
            };
            let kept: Vec<_> = elements
                .iter()
                .filter(|element| {
                    decoded(element.syntax()).is_none_or(|name| !names.contains(&name))
                })
                .map(|element| element.syntax().text().to_string())
                .collect();
            if kept.len() == elements.len() {
                continue;
            }
            let replacement = if matches!(value.clone().unparen(), ast::Expr::List(_)) {
                format!("[{}]", kept.join(", "))
            } else {
                format!(
                    "({}{})",
                    kept.join(", "),
                    if kept.len() == 1 { "," } else { "" }
                )
            };
            edits
                .entry(source.path.clone())
                .or_default()
                .push((value.syntax().text_range(), replacement));
        }
    }
    Ok(())
}

pub fn path(db: &Analysis, root: &Path, target: &Path, files: &[PathBuf]) -> Result<Plan, String> {
    if target == root
        || !target.starts_with(root)
        || !target.exists()
        || target.starts_with("<typeshed>")
    {
        return Err("Choose an existing project child path".into());
    }
    let sources = candidates(db, files)?;
    let module = target.is_dir()
        || target.extension().is_some_and(|extension| {
            matches!(
                extension.to_str(),
                Some("py" | "pyi" | "pyw" | "so" | "pyd")
            )
        });
    let mut blockers = if module {
        refactor2::module_import_sites(db, root, target, files)
    } else {
        Vec::new()
    };
    let mut edits = BTreeMap::<PathBuf, Vec<(TextRange, String)>>::new();
    let mut removed = BTreeMap::<PathBuf, BTreeSet<String>>::new();
    let cleanup = !(target
        .extension()
        .is_some_and(|extension| extension == "pyi")
        && target.with_extension("py").exists());
    if module && cleanup {
        let module_path = if target.is_dir() {
            target.join("__init__.py")
        } else {
            target.into()
        };
        if let Some(name) = db.resolver.module_name_of(&module_path) {
            let matches = |module: &str| {
                (module == name
                    || module
                        .strip_prefix(name.as_str())
                        .is_some_and(|suffix| suffix.starts_with('.')))
                    && db.module(module).is_some_and(|selected| {
                        selected.path == target
                            || selected.path.starts_with(target)
                            || selected.path == module_path
                    })
            };
            for source in &sources {
                if source.path == target || source.path.starts_with(target) {
                    continue;
                }
                let mut delete_statements = Vec::new();
                let mut changes = Vec::new();
                for statement in source.parse.syntax_node().descendants().filter(|node| {
                    matches!(
                        node.kind(),
                        SyntaxKind::IMPORT_STMT | SyntaxKind::IMPORT_FROM_STMT
                    )
                }) {
                    let aliases: Vec<_> = statement
                        .children()
                        .filter_map(ast::ImportAlias::cast)
                        .collect();
                    let from = ast::ImportFromStmt::cast(statement.clone());
                    let absolute = from.as_ref().map(|from| {
                        absolute_module(
                            db,
                            source,
                            from.level(),
                            &from
                                .module()
                                .map(|module| module.path())
                                .unwrap_or_default(),
                        )
                    });
                    let remove: Vec<_> = aliases
                        .iter()
                        .filter(|alias| {
                            if let Some(from) = &absolute {
                                matches(from) || matches(&format!("{from}.{}", alias.name()))
                            } else {
                                matches(&alias.name())
                            }
                        })
                        .collect();
                    let whole = absolute.as_ref().is_some_and(|module| matches(module));
                    if !whole && remove.is_empty() {
                        continue;
                    }
                    for alias in &remove {
                        removed.entry(source.path.clone()).or_default().insert(
                            alias
                                .alias()
                                .map(|token| token.text().into())
                                .unwrap_or_else(|| {
                                    alias.name().split('.').next().unwrap_or("").into()
                                }),
                        );
                    }
                    if whole || remove.len() == aliases.len() {
                        delete_statements.push(statement);
                    } else {
                        let keep: Vec<_> = aliases
                            .iter()
                            .filter(|alias| {
                                !remove.iter().any(|removed| {
                                    removed.syntax().text_range() == alias.syntax().text_range()
                                })
                            })
                            .map(|alias| alias.syntax().text().to_string())
                            .collect();
                        let replacement = if let Some(from) = &from {
                            format!("from {} import {}", from.module_path(), keep.join(", "))
                        } else {
                            format!("import {}", keep.join(", "))
                        };
                        changes.push((statement.text_range(), replacement));
                    }
                }
                changes.extend(statement_removals(&source.text, &delete_statements));
                if !changes.is_empty() {
                    edits.insert(source.path.clone(), changes);
                }
            }
        }
    }
    // File references are separate from module names and never authorize
    // deleting a similarly named definition/import in an unrelated module.
    for source in &sources {
        if source.path == target || source.path.starts_with(target) {
            continue;
        }
        for string in source
            .parse
            .syntax_node()
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::STRING_EXPR)
        {
            let Some(value) = decoded(&string) else {
                continue;
            };
            if value.len() > 8192 || value.contains(['\0', '\n', '\r']) {
                continue;
            }
            let literal = Path::new(&value);
            let paths = if literal.is_absolute() {
                vec![literal.to_path_buf()]
            } else {
                vec![
                    source.path.parent().unwrap_or(root).join(literal),
                    root.join(literal),
                ]
            };
            if paths.into_iter().any(|path| {
                rustpy_core::vfs::normalize_path(&path) == rustpy_core::vfs::normalize_path(target)
            }) {
                let mut usage = usages::usage_at(source, string.text_range(), false, false);
                usage.non_code = true;
                blockers.push(usage);
            }
        }
    }
    // Imports being cleaned still appear in the usages preview: deleting a
    // module is an explicit destructive choice, even when its aliases vanish.
    remove_static_exports(&sources, &removed, &mut edits)?;
    dedup_usages(&mut blockers);
    let operations = normalize_ops(edits, Some(target.into()))?;
    Ok(Plan {
        name: target
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        target: target.into(),
        kind: if module { Kind::Module } else { Kind::File },
        blockers,
        warnings: if cleanup {
            Vec::new()
        } else {
            vec!["Only the stub is deleted; the runtime source and its imports remain".into()]
        },
        operations,
        updated_calls: 0,
        sources,
        project_files: project_list(files),
    })
}
fn absolute_module(db: &Analysis, file: &FileInfo, level: usize, written: &str) -> String {
    if level == 0 {
        return written.into();
    }
    let Some(module) = file
        .module_name
        .clone()
        .or_else(|| db.resolver.module_name_of(&file.path))
    else {
        return String::new();
    };
    let mut parts: Vec<_> = module.split('.').collect();
    if !file.is_package_init() {
        parts.pop();
    }
    for _ in 1..level {
        if parts.pop().is_none() {
            return String::new();
        }
    }
    if !written.is_empty() {
        parts.extend(written.split('.'));
    }
    parts.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Project {
        db: Analysis,
        files: Vec<PathBuf>,
    }
    impl Project {
        fn new(inputs: &[(&str, &str)]) -> Self {
            let db = Analysis::for_project(Path::new("/project"));
            let files = inputs
                .iter()
                .map(|(name, text)| {
                    let path = PathBuf::from(format!("/project/{name}"));
                    db.set_file_text(&path, text);
                    path
                })
                .collect();
            Self { db, files }
        }
        fn plan(&self, name: &str, needle: &str) -> Plan {
            let file = self
                .db
                .file_for_path(Path::new(&format!("/project/{name}")))
                .unwrap();
            symbol(
                &self.db,
                &file,
                TextSize::new(file.text.find(needle).unwrap() as u32),
                &self.files,
            )
            .unwrap()
        }
        fn output(&self, plan: &Plan, name: &str) -> String {
            let path = PathBuf::from(format!("/project/{name}"));
            let original = self.db.file_for_path(&path).unwrap();
            let edits = plan
                .operations
                .iter()
                .find_map(|op| match op {
                    FileOp::Edit(p, e) if p == &path => Some(e),
                    _ => None,
                })
                .unwrap();
            refactor::try_apply_edits(&original.text, edits).unwrap()
        }
    }
    #[test]
    fn parameter_delete_updates_positional_keyword_and_preserves_other_arguments() {
        let p = Project::new(&[(
            "a.py",
            "def f(unused, keep):\n    return keep\na=f(1, 2)\nb=f(keep=3, unused=4)\n",
        )]);
        let plan = p.plan("a.py", "unused");
        assert_eq!(plan.kind, Kind::Parameter);
        assert!(!plan.needs_force());
        let output = p.output(&plan, "a.py");
        assert!(output.contains("def f(keep)"), "{output}");
        assert!(output.contains("f(2)"), "{output}");
        assert!(output.contains("f(keep=3)"), "{output}");
        assert_eq!(plan.updated_calls, 2);
    }
    #[test]
    fn used_parameter_and_effectful_removed_argument_require_explicit_force() {
        let p = Project::new(&[(
            "a.py",
            "def f(value, keep):\n    return value + keep\nf(side_effect(), 2)\n",
        )]);
        let plan = p.plan("a.py", "value");
        assert!(plan.needs_force());
        assert!(!plan.blockers.is_empty());
        assert!(plan
            .warnings
            .iter()
            .any(|message| message.contains("evaluation")));
        assert!(p.output(&plan, "a.py").contains("return value + keep"));
    }
    #[test]
    fn stale_unchanged_caller_dependency_invalidates_preview() {
        let p = Project::new(&[
            ("a.py", "def unused():\n    pass\n"),
            ("b.py", "value = 1\n"),
        ]);
        let plan = p.plan("a.py", "unused");
        plan.validate_sources(&p.db, &p.files).unwrap();
        p.db.set_file_text(
            Path::new("/project/b.py"),
            "from a import unused\nunused()\n",
        );
        assert!(plan.validate_sources(&p.db, &p.files).is_err());
    }
    #[test]
    fn removing_one_import_keeps_other_import_and_its_export() {
        let p = Project::new(&[
            ("a.py", "def unused():\n    pass\ndef keep():\n    pass\n"),
            (
                "b.py",
                "from a import unused, keep\n__all__ = ['unused', 'keep']\n",
            ),
        ]);
        let plan = p.plan("a.py", "unused");
        let output = p.output(&plan, "b.py");
        assert!(output.contains("keep"));
        assert!(!output.contains("unused"), "{output}");
        assert!(rustpy_syntax::parse(&output).errors().is_empty());
    }
    #[test]
    fn attribute_definition_deletes_store_but_keeps_read_in_preview() {
        let p=Project::new(&[("a.py","class C:\n    def __init__(self):\n        self.value = 1\n    def read(self):\n        return self.value\n")]);
        let plan = p.plan("a.py", "value =");
        assert_eq!(plan.kind, Kind::Attribute);
        assert!(plan.needs_force());
        let output = p.output(&plan, "a.py");
        assert!(!output.contains("self.value ="), "{output}");
        assert!(output.contains("return self.value"));
        assert!(rustpy_syntax::parse(&output).errors().is_empty());
    }
    #[test]
    fn deleting_module_cleans_import_aliases_exports_and_captures_resource_uses() {
        let temp = tempfile::TempDir::with_prefix("safe-delete").unwrap();
        let root = temp.path();
        let target = root.join("removed.py");
        let caller = root.join("main.py");
        std::fs::write(&target, "value=1\n").unwrap();
        let source="import removed, os\nfrom removed import value\n__all__=['removed','value','os']\nresource='removed.py'\n";
        std::fs::write(&caller, source).unwrap();
        let db = Analysis::for_project(root);
        let plan = path(&db, root, &target, &[target.clone(), caller.clone()]).unwrap();
        assert!(plan.needs_force());
        assert!(plan.blockers.iter().any(|usage| usage.non_code));
        let edits = plan
            .operations
            .iter()
            .find_map(|op| match op {
                FileOp::Edit(p, e) if p == &caller => Some(e),
                _ => None,
            })
            .unwrap();
        let output = refactor::try_apply_edits(source, edits).unwrap();
        assert!(output.contains("import os"), "{output}");
        assert!(!output.contains("from removed"), "{output}");
        assert!(output.contains("['os']"), "{output}");
        assert!(plan.operations.contains(&FileOp::Delete(target)));
    }
}
