//! Optional Rename families and supplemental source-backed references.
use crate::{
    refactor2::{self, FileOp},
    rename::{self, RenameOptions, RenamePlan},
};
use rustpy_core::{TextRange, TextSize};
use rustpy_semantic::{
    db::{Analysis, FileInfo},
    edit_helpers::{ancestor_at, rng, Edit},
    infer::Inferer,
    scope::{BindingId, BindingKind},
    types::Type,
    usages::{self, Symbol, Usage},
};
use rustpy_syntax::ast::{self, AstNode};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HierarchyChoice {
    Declaration,
    Overrides,
    #[default]
    EntireHierarchy,
}
#[derive(Clone, Debug)]
pub struct FamilyOptions {
    pub base: RenameOptions,
    pub hierarchy: HierarchyChoice,
    pub search_references: bool,
    pub dynamic_strings: bool,
    pub text_files: bool,
    pub related_tests: bool,
    pub similar_variables: bool,
    pub rename_inheritors: bool,
    pub rename_matching_file: bool,
    pub django: bool,
    pub reference_scope: Option<BTreeSet<PathBuf>>,
}
impl Default for FamilyOptions {
    fn default() -> Self {
        Self {
            base: Default::default(),
            hierarchy: Default::default(),
            search_references: true,
            dynamic_strings: true,
            text_files: false,
            related_tests: false,
            similar_variables: false,
            rename_inheritors: false,
            rename_matching_file: false,
            django: true,
            reference_scope: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct FamilyPlan {
    pub primary: RenamePlan,
    pub extra: Vec<FileOp>,
    pub snapshots: BTreeMap<PathBuf, String>,
    pub notes: Vec<String>,
}
pub fn target(db: &Analysis, symbol: &Symbol) -> Option<(Arc<FileInfo>, TextRange)> {
    match symbol {
        Symbol::Name { def, .. } => {
            let origin = usages::name_origin(&Inferer::new(db), def);
            Some((origin.file.clone(), origin.range()))
        }
        Symbol::Member {
            decl: Some((file, range)),
            ..
        } => Some((file.clone(), *range)),
        _ => None,
    }
}
pub fn hierarchy_targets(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    paths: &[PathBuf],
    choice: HierarchyChoice,
) -> Vec<(Arc<FileInfo>, TextRange)> {
    if choice == HierarchyChoice::Declaration {
        return Vec::new();
    }
    let Some(symbol) = usages::symbol_at(db, file, offset) else {
        return Vec::new();
    };
    let Some((source, range)) = target(db, &symbol) else {
        return Vec::new();
    };
    let Some(function) = ancestor_at::<ast::FuncDef>(&source.parse.syntax_node(), range.start())
        .filter(|function| function.is_method())
    else {
        return Vec::new();
    };
    let parameter = source
        .scopes
        .binding_at(range.start())
        .filter(|(_, binding)| {
            matches!(
                binding.kind,
                BindingKind::Param | BindingKind::StarParam | BindingKind::StarStarParam
            )
        })
        .and_then(|_| function.params())
        .and_then(|params| {
            params
                .params()
                .filter(|parameter| parameter.name().is_some())
                .position(|parameter| {
                    parameter
                        .name()
                        .is_some_and(|name| name.text_range() == range)
                })
        });
    let at = function
        .name()
        .map(|name| name.text_range().start())
        .unwrap_or(range.start());
    let mut locations = Vec::new();
    if choice == HierarchyChoice::EntireHierarchy {
        locations.extend(rustpy_semantic::hierarchy::super_methods(db, &source, at));
    }
    let mut roots = vec![(source.clone(), at)];
    for location in &locations {
        if let Some(owner) = db.file_for_path(&location.path) {
            roots.push((owner, location.range.start()));
        }
    }
    for (owner, offset) in roots {
        locations.extend(rustpy_semantic::hierarchy::implementations(
            db, &owner, offset, paths,
        ));
    }
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for location in locations {
        let Some(owner) = db.file_for_path(&location.path) else {
            continue;
        };
        if usages::is_library_file(db, &owner) {
            continue;
        }
        let Some(method) =
            ancestor_at::<ast::FuncDef>(&owner.parse.syntax_node(), location.range.start())
        else {
            continue;
        };
        let range = if let Some(index) = parameter {
            method
                .params()
                .and_then(|params| {
                    params
                        .params()
                        .filter(|parameter| parameter.name().is_some())
                        .nth(index)
                })
                .and_then(|parameter| parameter.name())
                .map(|name| name.text_range())
        } else {
            method.name().map(|name| name.text_range())
        };
        if let Some(range) = range {
            if seen.insert((owner.path.clone(), range.start(), range.end()))
                && !(owner.id == source.id && Some(range) == range_of_symbol(&symbol))
            {
                result.push((owner, range));
            }
        }
    }
    result
}
fn range_of_symbol(symbol: &Symbol) -> Option<TextRange> {
    match symbol {
        Symbol::Name { def, .. } => Some(def.range()),
        Symbol::Member {
            decl: Some((_, range)),
            ..
        } => Some(*range),
        _ => None,
    }
}
pub fn literal_name_range(
    file: &FileInfo,
    expression: &ast::Expr,
    name: &str,
) -> Option<TextRange> {
    let ast::Expr::String(value) = expression else {
        return None;
    };
    if value.value() != name {
        return None;
    }
    let text = expression.syntax().text().to_string();
    let start = text.find(['\'', '"'])?;
    let quote = text.as_bytes()[start];
    let size = if text.as_bytes().get(start..start + 3) == Some(&[quote, quote, quote]) {
        3
    } else {
        1
    };
    if text.len() < start + size * 2 || &text[start + size..text.len() - size] != name {
        return None;
    }
    let _ = file;
    Some(TextRange::new(
        expression.syntax().text_range().start() + TextSize::new((start + size) as u32),
        expression.syntax().text_range().end() - TextSize::new(size as u32),
    ))
}
fn builtin(db: &Analysis, file: &Arc<FileInfo>, callee: &ast::Expr, names: &[&str]) -> bool {
    let typ = Inferer::new(db).type_of_expr(file, callee);
    let Some(def) = typ.signature().and_then(|signature| signature.def.as_ref()) else {
        return false;
    };
    names.contains(&def.name.as_str())
        && db
            .module("builtins")
            .is_some_and(|owner| owner.id == def.file)
}
pub fn semantic_strings(db: &Analysis, symbol: &Symbol, paths: &[PathBuf]) -> Vec<Usage> {
    let Some((source, range)) = target(db, symbol) else {
        return Vec::new();
    };
    let name = symbol.name();
    let mut result = Vec::new();
    for path in paths {
        if !matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("py" | "pyi")
        ) {
            continue;
        }
        let Some(file) = db.file_for_path(path) else {
            continue;
        };
        for call in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
        {
            let Some(callee) = call.callee() else {
                continue;
            };
            if !builtin(
                db,
                &file,
                &callee,
                &["getattr", "setattr", "hasattr", "delattr"],
            ) {
                continue;
            }
            let Some(args) = call.arg_list() else {
                continue;
            };
            let mut values = args.positional();
            let (Some(object), Some(key)) = (values.next(), values.next()) else {
                continue;
            };
            if usages::member_declaration(db, &Inferer::new(db).type_of_expr(&file, &object), name)
                .is_some_and(|(owner, member)| owner.id == source.id && member == range)
            {
                if let Some(range) = literal_name_range(&file, &key, name) {
                    result.push(usages::usage_at(&file, range, false, false));
                }
            }
        }
        for assignment in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::AssignStmt::cast)
        {
            let targets = assignment.targets();
            if targets.len() != 1 {
                continue;
            }
            let ast::Expr::Name(variable) = &targets[0] else {
                continue;
            };
            let module_export = variable.name_text() == "__all__"
                && file.id == source.id
                && source
                    .scopes
                    .binding_at(range.start())
                    .is_some_and(|(_, binding)| binding.scope == source.scopes.module_scope());
            let class_metadata = matches!(
                variable.name_text().as_str(),
                "__slots__" | "__match_args__"
            ) && ancestor_at::<ast::ClassDef>(
                assignment.syntax(),
                assignment.syntax().text_range().start(),
            )
            .is_some_and(|class| {
                range.start() >= class.syntax().text_range().start()
                    && range.end() <= class.syntax().text_range().end()
                    && source.id == file.id
            });
            if !module_export && !class_metadata {
                continue;
            }
            let Some(value) = assignment.value() else {
                continue;
            };
            let values: Vec<_> = match value {
                ast::Expr::List(list) => list.elements().collect(),
                ast::Expr::Tuple(tuple) => tuple.elements().collect(),
                value => vec![value],
            };
            for value in values {
                if let Some(range) = literal_name_range(&file, &value, name) {
                    result.push(usages::usage_at(&file, range, false, false));
                }
            }
        }
    }
    result
}
fn merge_primary(into: &mut RenamePlan, other: RenamePlan) {
    into.code.extend(other.code);
    into.dynamic.extend(other.dynamic);
    into.text.extend(other.text);
    into.conflicts.extend(other.conflicts);
}
fn dedup(usages: &mut Vec<Usage>) {
    usages.sort_by_key(|usage| (usage.path.clone(), usage.range.start(), usage.range.end()));
    usages.dedup_by(|a, b| a.path == b.path && a.range == b.range);
}
fn read_text(db: &Analysis, path: &Path) -> Result<String, String> {
    if let Some(file) = db.cached_file_for_path(path) {
        return Ok(file.text.to_string());
    }
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() > 1024 * 1024 {
        return Err(format!(
            "{} exceeds the supplemental rename size limit",
            path.display()
        ));
    }
    let mut text = String::new();
    file.take(1024 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() > 1024 * 1024 {
        return Err("Supplemental rename source grew beyond its limit".into());
    }
    Ok(text)
}
fn words(text: &str, word: &str) -> Vec<TextRange> {
    text.match_indices(word)
        .filter(|(start, _)| {
            text[..*start]
                .chars()
                .next_back()
                .is_none_or(|ch| !rustpy_core::is_ident_continue(ch))
                && text[*start + word.len()..]
                    .chars()
                    .next()
                    .is_none_or(|ch| !rustpy_core::is_ident_continue(ch))
        })
        .map(|(start, _)| rng(start, start + word.len()))
        .collect()
}
pub fn plan(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    new_name: &str,
    paths: &[PathBuf],
    root: &Path,
    options: &FamilyOptions,
) -> Result<FamilyPlan, String> {
    let symbol = usages::symbol_at(db, file, offset).ok_or("Choose a declaration or reference")?;
    let mut primary = rename::rename_plan(db, file, offset, new_name, paths, options.base)?;
    // The generic usage query intentionally includes override-family usages.
    // Declaration-only Rename must narrow that query back to the exact member
    // lookup target, including inherited clients but excluding real overrides.
    if options.hierarchy == HierarchyChoice::Declaration {
        if let Some((owner, range)) = target(db, &symbol) {
            let method = ancestor_at::<ast::FuncDef>(&owner.parse.syntax_node(), range.start())
                .filter(|function| {
                    function.is_method()
                        && function
                            .name()
                            .is_some_and(|name| name.text_range() == range)
                });
            if method.is_some() {
                primary.code.retain(|usage| {
                    let Some(source) = db.file_for_path(&usage.path) else {
                        return false;
                    };
                    usages::symbol_at(db, &source, usage.range.start())
                        .and_then(|symbol| target(db, &symbol))
                        .is_some_and(|(source, selected)| {
                            source.id == owner.id && selected == range
                        })
                });
            }
        }
    }
    let mut extra_edits = BTreeMap::<PathBuf, Vec<Edit>>::new();
    let mut notes = Vec::new();
    let related = hierarchy_targets(db, file, offset, paths, options.hierarchy);
    for (owner, range) in related {
        let other = rename::rename_plan(db, &owner, range.start(), new_name, paths, options.base)?;
        if other.old_name == primary.old_name {
            merge_primary(&mut primary, other);
        } else {
            if !other.conflicts.is_empty() {
                return Err("A related parameter has a conflicting new name".into());
            }
            for (path, edits) in rename::rename_edits(
                other.code.iter().filter(|usage| {
                    usage.is_definition
                        || (options.search_references
                            && options
                                .reference_scope
                                .as_ref()
                                .is_none_or(|scope| scope.contains(&usage.path)))
                }),
                new_name,
            ) {
                extra_edits.entry(path).or_default().extend(edits);
            }
        }
    }
    if options.dynamic_strings {
        primary.code.extend(semantic_strings(db, &symbol, paths));
    }
    if !options.search_references {
        primary.code.retain(|usage| usage.is_definition);
        primary.dynamic.clear();
        notes.push("Search for references is disabled: only declarations are renamed".into());
    }
    if let Some(scope) = &options.reference_scope {
        primary
            .code
            .retain(|usage| usage.is_definition || scope.contains(&usage.path));
        primary.dynamic.retain(|usage| scope.contains(&usage.path));
        notes.push(
            "References are limited to the chosen scope; declarations remain included".into(),
        );
    }
    if options.text_files {
        let mut bytes = 0;
        let mut count = 0;
        for path in paths {
            if options
                .reference_scope
                .as_ref()
                .is_some_and(|scope| !scope.contains(path))
            {
                continue;
            }
            if !matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some(
                    "txt"
                        | "md"
                        | "rst"
                        | "html"
                        | "htm"
                        | "jinja"
                        | "jinja2"
                        | "j2"
                        | "json"
                        | "xml"
                        | "toml"
                        | "yaml"
                        | "yml"
                        | "ini"
                        | "cfg"
                        | "csv"
                        | "sql"
                        | "css"
                        | "js"
                        | "ts"
                        | "sh"
                )
            ) && path.extension().is_some()
            {
                continue;
            }
            if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("py" | "pyi")
            ) {
                continue;
            }
            count += 1;
            if count > 512 {
                return Err(
                    "Supplemental text rename supports at most 512 files; choose a smaller scope"
                        .into(),
                );
            }
            let text = read_text(db, path)?;
            bytes += text.len();
            if bytes > 8 * 1024 * 1024 {
                return Err(
                    "Supplemental text rename exceeds 8 MiB; choose a smaller scope".into(),
                );
            }
            for range in words(&text, &primary.old_name) {
                extra_edits
                    .entry(path.clone())
                    .or_default()
                    .push((range, new_name.to_owned()));
            }
        }
    }
    if options.django && options.search_references {
        for (path, edits) in crate::rename_framework::related_edits(db, &symbol, new_name, paths)? {
            if options
                .reference_scope
                .as_ref()
                .is_none_or(|scope| scope.contains(&path))
            {
                extra_edits.entry(path).or_default().extend(edits);
            }
        }
    }
    let mut moves = Vec::new();
    if options.related_tests || options.similar_variables || options.rename_inheritors {
        related_symbols(
            db,
            &symbol,
            new_name,
            paths,
            options,
            &mut extra_edits,
            &mut moves,
        )?;
    }
    if options.rename_matching_file {
        if let Some((owner, _)) = target(db, &symbol) {
            let old_stem = rustpy_semantic::edit_helpers::to_snake(symbol.name());
            if owner.path.file_stem().and_then(|stem| stem.to_str()) == Some(&old_stem)
                && owner
                    .path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    == Some("py")
            {
                let stem = rustpy_semantic::edit_helpers::to_snake(new_name);
                moves.push(FileOp::Move(
                    owner.path.clone(),
                    owner.path.with_file_name(format!("{stem}.py")),
                ));
                notes.push(
                    "Matching module imports will be updated in the combined transaction".into(),
                );
            }
        }
    }
    dedup(&mut primary.code);
    dedup(&mut primary.dynamic);
    dedup(&mut primary.text);
    primary.text.retain(|usage| {
        !primary
            .code
            .iter()
            .any(|code| code.path == usage.path && code.range == usage.range)
    });
    let mut snapshots = BTreeMap::new();
    for path in primary
        .code
        .iter()
        .chain(&primary.dynamic)
        .chain(&primary.text)
        .map(|usage| &usage.path)
        .chain(extra_edits.keys())
        .chain(moves.iter().filter_map(|operation| {
            if let FileOp::Move(path, _) = operation {
                Some(path)
            } else {
                None
            }
        }))
    {
        if !snapshots.contains_key(path) {
            snapshots.insert(path.clone(), read_text(db, path)?);
        }
    }
    if !moves.is_empty() {
        for path in paths.iter().filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("py" | "pyi")
            )
        }) {
            if let Some(file) = db.file_for_path(path) {
                snapshots
                    .entry(path.clone())
                    .or_insert_with(|| file.text.to_string());
            }
        }
    }
    let mut extra = extra_edits
        .into_iter()
        .map(|(path, edits)| FileOp::Edit(path, edits))
        .collect::<Vec<_>>();
    extra.extend(moves);
    let _ = root;
    Ok(FamilyPlan {
        primary,
        extra,
        snapshots,
        notes,
    })
}

fn related_symbols(
    db: &Analysis,
    symbol: &Symbol,
    new_name: &str,
    paths: &[PathBuf],
    options: &FamilyOptions,
    edits: &mut BTreeMap<PathBuf, Vec<Edit>>,
    moves: &mut Vec<FileOp>,
) -> Result<(), String> {
    let Some((source, range)) = target(db, symbol) else {
        return Ok(());
    };
    let old = symbol.name();
    let old_snake = rustpy_semantic::edit_helpers::to_snake(old);
    let new_snake = rustpy_semantic::edit_helpers::to_snake(new_name);
    let related = rustpy_search::usages::find_usages(db, &source, range.start(), paths);
    let inference = Inferer::new(db);
    let class_type = source
        .scopes
        .binding_at(range.start())
        .map(|(id, _)| inference.type_of_binding(&source, id));
    let selected_class = class_type.as_ref().and_then(Type::class_ref);
    let mut auxiliary = Vec::new();
    if options.rename_inheritors && matches!(class_type, Some(Type::Class(_, _))) {
        for location in
            rustpy_semantic::hierarchy::implementations(db, &source, range.start(), paths)
        {
            if let Some(file) = db.file_for_path(&location.path) {
                if let Some(class) =
                    ancestor_at::<ast::ClassDef>(&file.parse.syntax_node(), location.range.start())
                {
                    let name = class.name_text();
                    if let Some(prefix) = name.strip_suffix(old) {
                        if !prefix.is_empty() {
                            auxiliary.push((
                                file,
                                class.name().unwrap().text_range(),
                                format!("{prefix}{new_name}"),
                            ));
                        }
                    }
                }
            }
        }
    }
    for path in paths {
        if !matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("py" | "pyi")
        ) {
            continue;
        }
        let Some(file) = db.file_for_path(path) else {
            continue;
        };
        if usages::is_library_file(db, &file) {
            continue;
        }
        if options.related_tests
            && related
                .iter()
                .any(|usage| usage.path == *path && !usage.dynamic && !usage.non_code)
        {
            for function in file
                .parse
                .syntax_node()
                .descendants()
                .filter_map(ast::FuncDef::cast)
                .filter(|function| function.name_text().starts_with("test_"))
            {
                if !related.iter().any(|usage| {
                    usage.path == *path
                        && function.syntax().text_range().contains_range(usage.range)
                }) {
                    continue;
                }
                let name = function.name_text();
                let pieces = name.split('_').collect::<Vec<_>>();
                let prefix = format!("test_{old_snake}");
                let renamed = if name == prefix || name.starts_with(&format!("{prefix}_")) {
                    name.replacen(&prefix, &format!("test_{new_snake}"), 1)
                } else if pieces.last().copied() == Some(old_snake.as_str()) {
                    format!("{}_{new_snake}", pieces[..pieces.len() - 1].join("_"))
                } else {
                    continue;
                };
                auxiliary.push((file.clone(), function.name().unwrap().text_range(), renamed));
            }
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                let renamed = if stem == format!("test_{old_snake}") {
                    Some(format!("test_{new_snake}"))
                } else if stem == format!("{old_snake}_test") {
                    Some(format!("{new_snake}_test"))
                } else {
                    None
                };
                if let Some(stem) = renamed {
                    moves.push(FileOp::Move(
                        path.clone(),
                        path.with_file_name(format!(
                            "{stem}.{}",
                            path.extension().unwrap().to_string_lossy()
                        )),
                    ));
                }
            }
        }
        if options.similar_variables {
            if let Some(class) = selected_class {
                for (index, binding) in
                    file.scopes
                        .bindings
                        .iter()
                        .enumerate()
                        .filter(|(_, binding)| {
                            matches!(
                                binding.kind,
                                BindingKind::Assignment
                                    | BindingKind::Annotation
                                    | BindingKind::Param
                            )
                        })
                {
                    let name = binding.name.as_str();
                    let renamed = if name == old_snake {
                        new_snake.clone()
                    } else if name == format!("{old_snake}s") {
                        format!("{new_snake}s")
                    } else {
                        continue;
                    };
                    fn contains(
                        typ: &Type,
                        class: &rustpy_semantic::types::ClassRef,
                        depth: usize,
                        remaining: &mut usize,
                    ) -> bool {
                        if depth > 16 || *remaining == 0 {
                            return false;
                        }
                        *remaining -= 1;
                        match typ {
                            Type::Instance(owner, args) | Type::Class(owner, args) => {
                                owner == class
                                    || args
                                        .iter()
                                        .take(64)
                                        .any(|arg| contains(arg, class, depth + 1, remaining))
                            }
                            Type::Union(arms) | Type::Tuple(arms) => arms
                                .iter()
                                .take(64)
                                .any(|arg| contains(arg, class, depth + 1, remaining)),
                            _ => false,
                        }
                    }
                    if contains(
                        &inference.type_of_binding(&file, BindingId(index as u32)),
                        class,
                        0,
                        &mut 256,
                    ) {
                        auxiliary.push((file.clone(), binding.range, renamed));
                    }
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    for (file, range, new) in auxiliary {
        if !seen.insert((file.path.clone(), range.start(), range.end())) {
            continue;
        }
        let plan = rename::rename_plan(db, &file, range.start(), &new, paths, Default::default())?;
        if !plan.conflicts.is_empty() {
            return Err(format!(
                "Related rename '{}' conflicts; disable related names or resolve the conflict",
                plan.old_name
            ));
        }
        for (path, changes) in rename::rename_edits(
            plan.code.iter().filter(|usage| {
                usage.is_definition
                    || (options.search_references
                        && options
                            .reference_scope
                            .as_ref()
                            .is_none_or(|scope| scope.contains(&usage.path)))
            }),
            &new,
        ) {
            edits.entry(path).or_default().extend(changes);
        }
    }
    Ok(())
}

/// Build one snapshot-bound transaction, composing optional module renames
/// after symbol edits in an isolated analysis. Import edits are translated back
/// to the original source, then physical moves run through common Undo.
pub fn transaction(
    db: &Analysis,
    plan: &FamilyPlan,
    selected: Vec<(PathBuf, TextRange)>,
    paths: &[PathBuf],
    root: &Path,
) -> Result<Vec<FileOp>, String> {
    let mut edits = BTreeMap::<PathBuf, Vec<Edit>>::new();
    for (path, range) in selected {
        edits
            .entry(path)
            .or_default()
            .push((range, plan.primary.new_name.clone()));
    }
    let mut requested = Vec::new();
    for operation in &plan.extra {
        match operation {
            FileOp::Edit(path, changes) => edits
                .entry(path.clone())
                .or_default()
                .extend(changes.clone()),
            FileOp::Move(from, to) => requested.push((from.clone(), to.clone())),
            _ => return Err("Unsupported supplemental rename operation".into()),
        }
    }
    let mut originals = plan.snapshots.clone();
    if !requested.is_empty() {
        for path in paths.iter().filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("py" | "pyi")
            )
        }) {
            if let Some(file) = db.file_for_path(path) {
                originals
                    .entry(path.clone())
                    .or_insert_with(|| file.text.to_string());
            }
        }
    }
    let probe = Analysis::for_project(root);
    let mut current = originals.clone();
    for (path, text) in &current {
        if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("py" | "pyi")
        ) {
            probe.set_file_text(path, text);
        }
    }
    for (path, mut changes) in edits {
        changes.sort_by_key(|(range, _)| (range.start(), range.end()));
        changes.dedup();
        if changes.windows(2).any(|pair| {
            pair[0].0.end() > pair[1].0.start()
                || (pair[0].0 == pair[1].0 && pair[0].1 != pair[1].1)
        }) {
            return Err("Related renames overlap with different replacements".into());
        }
        let original = current
            .get(&path)
            .ok_or("Rename source snapshot is missing")?;
        let updated = rustpy_semantic::edit_helpers::try_apply_edits(original, &changes)
            .map_err(|error| error.to_string())?;
        probe.set_file_text(&path, &updated);
        current.insert(path, updated);
    }
    let mut moves = Vec::new();
    let mut seen = BTreeSet::new();
    for (from, to) in requested {
        if !seen.insert(from.clone()) {
            continue;
        }
        if to.exists() || current.contains_key(&to) {
            return Err(format!("{} already exists", to.display()));
        }
        let mut pair = vec![(from.clone(), to.clone())];
        if let Some(extension @ ("py" | "pyi")) =
            from.extension().and_then(|extension| extension.to_str())
        {
            let other = if extension == "py" { "pyi" } else { "py" };
            let counterpart = from.with_extension(other);
            if current.contains_key(&counterpart) && !seen.contains(&counterpart) {
                seen.insert(counterpart.clone());
                pair.push((counterpart, to.with_extension(other)));
            }
        }
        for (from, to) in pair {
            if to.exists() || current.contains_key(&to) {
                return Err(format!("{} already exists", to.display()));
            }
            let catalog = current.keys().cloned().collect::<Vec<_>>();
            let operations = refactor2::move_module(&probe, root, &from, &to, &catalog)?;
            for operation in operations {
                match operation {
                    FileOp::Edit(path, changes) => {
                        let text = current.get(&path).ok_or("Module rename snapshot missing")?;
                        let updated =
                            rustpy_semantic::edit_helpers::try_apply_edits(text, &changes)
                                .map_err(|error| error.to_string())?;
                        probe.set_file_text(&path, &updated);
                        current.insert(path, updated);
                    }
                    FileOp::Move(from, to) => {
                        let text = current
                            .remove(&from)
                            .ok_or("Module rename source missing")?;
                        probe.set_file_text(&from, "");
                        probe.set_file_text(&to, &text);
                        current.insert(to.clone(), text);
                        moves.push((from, to));
                    }
                    _ => return Err("Unexpected module rename operation".into()),
                }
            }
        }
    }
    let mut operations = Vec::new();
    for (path, original) in originals {
        let mut final_path = path.clone();
        for (from, to) in &moves {
            if &final_path == from {
                final_path = to.clone();
            }
        }
        if let Some(final_text) = current.get(&final_path) {
            if &original != final_text {
                operations.push(FileOp::Edit(
                    path,
                    vec![(rng(0, original.len()), final_text.clone())],
                ));
            }
        }
    }
    operations.extend(moves.into_iter().map(|(from, to)| FileOp::Move(from, to)));
    Ok(operations)
}

/// Module/file consistency uses the same import relocation planner as Move,
/// including a paired source/stub file and package-relative imports.
pub fn module_rename(
    db: &Analysis,
    root: &Path,
    source: &Path,
    new: &str,
    paths: &[PathBuf],
) -> Result<Vec<FileOp>, String> {
    rename::check_identifier(new)?;
    let source = if matches!(
        source.file_name().and_then(|name| name.to_str()),
        Some("__init__.py" | "__init__.pyi")
    ) {
        source.parent().ok_or("Package has no parent")?
    } else {
        source
    };
    if source.is_dir() {
        return refactor2::move_module(db, root, source, &source.with_file_name(new), paths);
    }
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .ok_or("Module has no Python extension")?;
    if !matches!(extension, "py" | "pyi") {
        return Err("Rename Module requires Python source or stub".into());
    }
    let destination = source.with_file_name(format!("{new}.{extension}"));
    let mut snapshots = BTreeMap::new();
    for path in paths.iter().filter(|path| {
        matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("py" | "pyi")
        )
    }) {
        if let Some(file) = db.file_for_path(path) {
            snapshots.insert(path.clone(), file.text.to_string());
        }
    }
    if !snapshots.contains_key(source) {
        snapshots.insert(source.to_owned(), read_text(db, source)?);
    }
    let other = source.with_extension(if extension == "py" { "pyi" } else { "py" });
    if other.exists() || db.cached_file_for_path(&other).is_some() {
        snapshots.insert(other.clone(), read_text(db, &other)?);
    }
    let plan = FamilyPlan {
        primary: RenamePlan::default(),
        extra: vec![FileOp::Move(source.to_owned(), destination)],
        snapshots,
        notes: vec![],
    };
    transaction(db, &plan, vec![], paths, root)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup(files: &[(&str, &str)]) -> (Analysis, Vec<PathBuf>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let paths = files
            .iter()
            .map(|(name, source)| {
                let path = PathBuf::from(format!("/proj/{name}"));
                db.set_file_text(&path, source);
                path
            })
            .collect();
        (db, paths)
    }
    fn planned(
        files: &[(&str, &str)],
        needle: &str,
        new: &str,
        options: FamilyOptions,
    ) -> (FamilyPlan, Vec<FileOp>) {
        let (db, paths) = setup(files);
        let file = db.file_for_path(&paths[0]).unwrap();
        let at = TextSize::new(files[0].1.find(needle).unwrap() as u32);
        let plan = plan(&db, &file, at, new, &paths, Path::new("/proj"), &options).unwrap();
        let selected = plan
            .primary
            .code
            .iter()
            .map(|usage| (usage.path.clone(), usage.range))
            .collect();
        let operations = transaction(&db, &plan, selected, &paths, Path::new("/proj")).unwrap();
        (plan, operations)
    }
    fn output(operations: &[FileOp], name: &str, source: &str) -> String {
        let edits = operations
            .iter()
            .filter_map(|operation| match operation {
                FileOp::Edit(path, edits) if path.ends_with(name) => Some(edits),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        rustpy_semantic::edit_helpers::apply_edits(source, &edits)
    }
    #[test]
    fn hierarchy_prompt_changes_related_methods_without_unrelated_names() {
        let source = "class Base:\n    def run(self): pass\nclass Child(Base):\n    def run(self): pass\nclass Other:\n    def run(self): pass\nBase().run()\nChild().run()\nOther().run()\n";
        let (_, operations) = planned(
            &[("main.py", source)],
            "run(self)",
            "execute",
            FamilyOptions::default(),
        );
        let result = output(&operations, "main.py", source);
        assert_eq!(result.matches("def execute(").count(), 2, "{result}");
        assert!(
            result.contains("Base().execute()") && result.contains("Child().execute()"),
            "{result}"
        );
        assert!(
            result.contains("class Other:\n    def run(") && result.contains("Other().run()"),
            "{result}"
        );
        let (_, operations) = planned(
            &[("main.py", source)],
            "run(self)",
            "execute",
            FamilyOptions {
                hierarchy: HierarchyChoice::Declaration,
                ..Default::default()
            },
        );
        let declaration_only = output(&operations, "main.py", source);
        assert!(declaration_only.contains("class Child(Base):\n    def run("));
        assert!(declaration_only.contains("Base().execute()"));
        assert!(declaration_only.contains("Child().run()"));
        assert!(declaration_only.contains("Other().run()"));
    }
    #[test]
    fn hierarchy_parameter_names_and_keyword_clients_have_their_own_replacements() {
        let source = "class Base:\n    def run(self, value: int): pass\nclass Child(Base):\n    def run(self, item: int): pass\nBase().run(value=1)\nChild().run(item=2)\n";
        let (_, operations) = planned(
            &[("main.py", source)],
            "value: int",
            "amount",
            FamilyOptions::default(),
        );
        let result = output(&operations, "main.py", source);
        assert!(
            result.contains("run(self, amount: int)") && !result.contains("item: int"),
            "{result}"
        );
        assert!(
            result.contains("Base().run(amount=1)") && result.contains("Child().run(amount=2)"),
            "{result}"
        );
    }
    #[test]
    fn canonical_dynamic_references_keep_receiver_identity_and_fake_builtin_clear() {
        let source = "class Box:\n    __slots__ = ('value',)\n    __match_args__ = ('value',)\n    def __init__(self): self.value = 1\nclass Other:\n    def __init__(self): self.value = 2\nbox = Box()\ngetattr(box, 'value')\nsetattr(box, 'value', 3)\ngetattr(Other(), 'value')\ndef fake(getattr):\n    return getattr(box, 'value')\n";
        let (_, operations) = planned(
            &[("main.py", source)],
            "value = 1",
            "size",
            FamilyOptions::default(),
        );
        let result = output(&operations, "main.py", source);
        assert!(
            result.contains("__slots__ = ('size',)")
                && result.contains("__match_args__ = ('size',)"),
            "{result}"
        );
        assert!(
            result.contains("getattr(box, 'size')") && result.contains("setattr(box, 'size', 3)"),
            "{result}"
        );
        assert!(
            result.contains("getattr(Other(), 'value')")
                && result.contains("return getattr(box, 'value')"),
            "{result}"
        );
    }
    #[test]
    fn reference_checkbox_and_scope_retain_definitions_without_other_clients() {
        let source = "def run(): pass\nrun()\n";
        let (plan, operations) = planned(
            &[
                ("main.py", source),
                ("client.py", "from main import run\nrun()\n"),
            ],
            "run():",
            "execute",
            FamilyOptions {
                search_references: false,
                ..Default::default()
            },
        );
        assert!(plan.primary.code.iter().all(|usage| usage.is_definition));
        let result = output(&operations, "main.py", source);
        assert!(
            result.contains("def execute():") && result.ends_with("run()\n"),
            "{result}"
        );
        assert!(!operations
            .iter()
            .any(|op| matches!(op,FileOp::Edit(path,_) if path.ends_with("client.py"))));
        let mut scope = BTreeSet::new();
        scope.insert(PathBuf::from("/proj/main.py"));
        let (plan, _) = planned(
            &[
                ("main.py", source),
                ("client.py", "from main import run\nrun()\n"),
            ],
            "run():",
            "execute",
            FamilyOptions {
                reference_scope: Some(scope),
                ..Default::default()
            },
        );
        assert!(plan
            .primary
            .code
            .iter()
            .all(|usage| usage.is_definition || usage.path.ends_with("main.py")));
    }
    #[test]
    fn text_occurrences_are_opt_in_whole_words_and_never_binary_files() {
        let source = "value = 1\nprint(value)\n";
        let documentation = "value values value_1 (value)\n";
        let (_, plain) = planned(
            &[("main.py", source), ("README.md", documentation)],
            "value =",
            "amount",
            FamilyOptions::default(),
        );
        assert!(!plain
            .iter()
            .any(|op| matches!(op,FileOp::Edit(path,_) if path.ends_with("README.md"))));
        let (_, operations) = planned(
            &[
                ("main.py", source),
                ("README.md", documentation),
                ("data.bin", "value"),
            ],
            "value =",
            "amount",
            FamilyOptions {
                text_files: true,
                ..Default::default()
            },
        );
        assert_eq!(
            output(&operations, "README.md", documentation),
            "amount values value_1 (amount)\n"
        );
        assert!(!operations
            .iter()
            .any(|op| matches!(op,FileOp::Edit(path,_) if path.ends_with("data.bin"))));
    }
    #[test]
    fn related_class_names_require_real_type_and_test_references() {
        let source = "class User: pass\nclass CachedUser(User): pass\nuser = User()\nusers = [User()]\nother = 'text'\n";
        let test = "from main import User\ndef test_user():\n    return User()\n";
        let (plan, operations) = planned(
            &[
                ("main.py", source),
                ("test_user.py", test),
                ("unrelated.py", "def test_user(): return 1\n"),
            ],
            "User: pass",
            "Account",
            FamilyOptions {
                related_tests: true,
                similar_variables: true,
                rename_inheritors: true,
                ..Default::default()
            },
        );
        let result = output(&operations, "main.py", source);
        assert!(
            result.contains("class CachedAccount(Account)")
                && result.contains("account = Account()"),
            "{result}"
        );
        assert!(output(&operations, "test_user.py", test).contains("def test_account():"));
        assert!(plan.extra.iter().any(|op|matches!(op,FileOp::Move(from,to) if from.ends_with("test_user.py")&&to.ends_with("test_account.py"))));
        assert!(!operations
            .iter()
            .any(|op| matches!(op,FileOp::Edit(path,_) if path.ends_with("unrelated.py"))));
    }
    #[test]
    fn matching_module_and_stub_moves_share_original_coordinate_import_edits() {
        let source = "class User: pass\n";
        let stub = "class User: ...\n";
        let client = "from user import User\nvalue = User()\n";
        let (_, operations) = planned(
            &[
                ("user.py", source),
                ("user.pyi", stub),
                ("client.py", client),
            ],
            "User: pass",
            "Account",
            FamilyOptions {
                rename_matching_file: true,
                ..Default::default()
            },
        );
        assert!(operations.iter().any(|op|matches!(op,FileOp::Move(from,to) if from.ends_with("user.py")&&to.ends_with("account.py"))));
        assert!(operations.iter().any(|op|matches!(op,FileOp::Move(from,to) if from.ends_with("user.pyi")&&to.ends_with("account.pyi"))));
        assert!(output(&operations, "client.py", client).contains("from account import Account"));
        assert!(output(&operations, "user.pyi", stub).contains("class Account:"));
    }
    #[test]
    fn protocol_names_are_protected_in_dialog_and_inplace_checks() {
        let source = "class Box:\n    def __len__(self): return 1\n    def size(self): return 1\n";
        let (db, paths) = setup(&[("main.py", source)]);
        let file = db.file_for_path(&paths[0]).unwrap();
        assert!(rename::rename_plan(
            &db,
            &file,
            TextSize::new(source.find("__len__").unwrap() as u32),
            "length",
            &paths,
            Default::default()
        )
        .is_err());
        assert!(rename::rename_check(
            &db,
            &file,
            TextSize::new(source.find("size(self)").unwrap() as u32)
        )
        .unwrap()
        .check("__len__")
        .is_err());
    }
}
