//! Import intentions share binding-aware edits with the actual Alt+Enter menu.
use super::*;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum ImportRequest {
    Alias {
        binding: rustpy_semantic::scope::BindingId,
    },
    Create {
        path: PathBuf,
        package: bool,
    },
    Install {
        distribution: String,
    },
}

fn statement(file: &FileInfo, offset: TextSize) -> Option<ast::Stmt> {
    token_at(&file.parse.syntax_node(), offset)?
        .parent_ancestors()
        .find_map(|node| {
            let stmt = ast::Stmt::cast(node)?;
            matches!(stmt, ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_)).then_some(stmt)
        })
}
fn aliases(stmt: &ast::Stmt) -> Vec<ast::ImportAlias> {
    match stmt {
        ast::Stmt::Import(i) => i.aliases().collect(),
        ast::Stmt::ImportFrom(i) => i.aliases().collect(),
        _ => Vec::new(),
    }
}
fn alias_at(
    file: &FileInfo,
    offset: TextSize,
) -> Option<(ast::ImportAlias, rustpy_semantic::scope::BindingId)> {
    let stmt = statement(file, offset)?;
    let aliases = aliases(&stmt);
    let alias = aliases
        .iter()
        .find(|a| a.text_range().contains_inclusive(offset))
        .or_else(|| (aliases.len() == 1).then(|| &aliases[0]))?
        .clone();
    let id = alias_binding(file, &alias)?;
    Some((alias, id))
}
/// Rename only this import binding's references; collisions refuse the operation.
pub fn alias_import(
    file: &Arc<FileInfo>,
    id: rustpy_semantic::scope::BindingId,
    name: &str,
) -> Option<Intention> {
    if !valid_identifier(name) {
        return None;
    }
    let binding = file.scopes.binding(id);
    if !matches!(binding.kind, BindingKind::Import(_)) {
        return None;
    }
    // references_to groups lexical same-name definitions. Without a unique
    // defining binding it cannot establish ownership across reassignment.
    if file
        .scopes
        .bindings_in(binding.scope, binding.name.as_str())
        .iter()
        .filter(|other| file.scopes.binding(**other).is_definition())
        .any(|other| *other != id)
    {
        return None;
    }
    if file
        .scopes
        .resolve(binding.scope, name)
        .is_some_and(|ids| ids.iter().any(|other| *other != id))
    {
        return None;
    }
    // Simulate the new lexical binding for every existing use of the alias,
    // including scopes that never referenced the old import (e.g. print()).
    let mut proposed = file.scopes.clone();
    let scope = &mut proposed.scopes[binding.scope.0 as usize];
    if let Some(ids) = scope.bindings.get_mut(binding.name.as_str()) {
        ids.retain(|other| *other != id);
    }
    scope.bindings.entry(name.into()).or_default().push(id);
    for reference in &file.scopes.references {
        if reference.name == name
            && reference.resolved != Some(id)
            && proposed
                .resolve(reference.scope, name)
                .is_some_and(|ids| ids.contains(&id))
        {
            return None;
        }
    }
    // A nonlocal declaration is itself a binding contract; do not leave a
    // declaration of the old name after removing that enclosing binding.
    if file.scopes.scopes.iter().enumerate().any(|(index, scope)| {
        scope.nonlocals.contains(&binding.name)
            && file
                .scopes
                .resolve(
                    rustpy_semantic::scope::ScopeId(index as u32),
                    binding.name.as_str(),
                )
                .is_some_and(|ids| ids.contains(&id))
    }) {
        return None;
    }
    let alias = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::ImportAlias::cast)
        .find(|alias| alias.text_range().contains_range(binding.range))?;
    // Aliasing an unaliased dotted import changes the bound root. Its qualified
    // references need a structural conversion rather than a blind token rename.
    let mut edits = vec![(alias.text_range(), format!("{} as {name}", alias.name()))];
    for reference in file.scopes.references_to(id) {
        if file
            .scopes
            .resolve(reference.scope, name)
            .is_some_and(|ids| ids.iter().any(|other| *other != id))
        {
            return None;
        }

        let range = if alias.alias().is_none() && alias.name().contains('.') {
            let token = token_at(&file.parse.syntax_node(), reference.range.start())?;
            token
                .parent_ancestors()
                .filter_map(ast::Expr::cast)
                .find(|expr| {
                    expr.syntax()
                        .text()
                        .to_string()
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect::<String>()
                        == alias.name()
                })?
                .text_range()
        } else {
            reference.range
        };
        edits.push((range, name.into()));
    }
    Some(Intention {
        title: format!("Import '{}' as '{name}'", alias.name()),
        edits,
    })
}
fn valid_identifier(name: &str) -> bool {
    let (tokens, diagnostics) = rustpy_syntax::tokenize(name);
    diagnostics.is_empty()
        && tokens.iter().any(|token| {
            token.kind == SyntaxKind::IDENT
                && token.range == TextRange::new(TextSize::new(0), TextSize::new(name.len() as u32))
        })
}
/// Non-text actions are typed; no path or command is reconstructed from labels.
pub fn requests(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Vec<(String, ImportRequest)> {
    let mut out = Vec::new();
    if let Some((alias, id)) = alias_at(file, offset) {
        out.push((
            format!("Import '{}' as…", alias.name()),
            ImportRequest::Alias { binding: id },
        ));
    }
    let Some(stmt) = statement(file, offset) else {
        return out;
    };
    let (mut module, level) = match &stmt {
        ast::Stmt::ImportFrom(i) => (i.module().map(|m| m.path()).unwrap_or_default(), i.level()),
        ast::Stmt::Import(i) => {
            let Some(alias) = i
                .aliases()
                .find(|alias| alias.text_range().contains_inclusive(offset))
                .or_else(|| i.aliases().next())
            else {
                return out;
            };
            (alias.name(), 0)
        }
        _ => return out,
    };
    let mut missing = if level > 0 {
        db.resolver
            .resolve_relative(&file.path, level, &module)
            .is_none()
    } else {
        db.module(&module).is_none()
    };
    if let ast::Stmt::ImportFrom(import) = &stmt {
        if let Some(alias) = import
            .aliases()
            .find(|alias| alias.text_range().contains_inclusive(offset))
            .or_else(|| import.aliases().next())
        {
            let name = alias.name();
            if valid_identifier(&name) && !missing {
                let source = if level == 0 {
                    db.module(&module)
                } else {
                    db.resolver
                        .resolve_relative(&file.path, level, &module)
                        .and_then(|source| source.path().and_then(|path| db.file_for_path(path)))
                };
                if source.is_some_and(|source| {
                    !source
                        .scopes
                        .scope(source.scopes.module_scope())
                        .bindings
                        .contains_key(name.as_str())
                }) {
                    let candidate = if module.is_empty() {
                        name
                    } else {
                        format!("{module}.{name}")
                    };
                    missing = if level > 0 {
                        db.resolver
                            .resolve_relative(&file.path, level, &candidate)
                            .is_none()
                    } else {
                        db.module(&candidate).is_none()
                    };
                    module = candidate;
                }
            }
        }
    }
    if module.is_empty() || !module.split('.').all(valid_identifier) {
        return out;
    }
    if missing {
        let base = if level > 0 {
            rustpy_semantic::module::ModuleResolver::relative_package_dir(&file.path, level)
        } else {
            db.resolver
                .roots
                .iter()
                .find(|root| file.path.starts_with(root))
                .cloned()
        };
        if let Some(base) = base {
            let path = module.split('.').fold(base, |path, part| path.join(part));
            if !path.with_extension("py").exists() && !path.join("__init__.py").exists() {
                out.push((
                    format!("Create module '{module}'"),
                    ImportRequest::Create {
                        path: path.with_extension("py"),
                        package: false,
                    },
                ));
                out.push((
                    format!("Create package '{module}'"),
                    ImportRequest::Create {
                        path: path.join("__init__.py"),
                        package: true,
                    },
                ));
            }
        }
        if level == 0 {
            let root = module.split('.').next().unwrap();
            let dist =
                rustpy_packages::distribution_for_module(root).unwrap_or_else(|| root.to_string());
            out.push((
                format!("Install package '{dist}' (interpreter/version…)"),
                ImportRequest::Install { distribution: dist },
            ));
        }
    }
    out
}

/// Line-based rewrites require the import to own its physical lines.
pub(super) fn isolated_statement(file: &FileInfo, stmt: &ast::Stmt) -> bool {
    let start = usize::from(stmt.text_range().start());
    let end = usize::from(stmt.text_range().end());
    let line = line_start(&file.text, start);
    let end_line = file.text[end..]
        .find('\n')
        .map_or(file.text.len(), |offset| end + offset);
    file.text[line..start].trim().is_empty() && file.text[end..end_line].trim().is_empty()
}

pub fn at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Intention> {
    let mut out = Vec::new();
    let ranges = crate::inspections::unused_import_ranges_with(db, file);
    let optimized = rustpy_format::imports::optimize_imports(file, &ranges);
    if !optimized.is_empty() && apply_edits(&file.text, &optimized) != file.text.as_ref() {
        out.push(Intention {
            title: "Optimize imports".into(),
            edits: optimized,
        });
    }
    if let Some(stmt) = statement(file, offset) {
        let all = aliases(&stmt);
        if let ast::Stmt::Import(import) = &stmt {
            if all.len() > 1 && isolated_statement(file, &stmt) {
                let indent = indent_at(&file.text, usize::from(stmt.text_range().start()));
                let replacement = all
                    .iter()
                    .map(|a| format!("import {}", a.syntax().text()))
                    .collect::<Vec<_>>()
                    .join(&format!("\n{indent}"));
                out.push(Intention {
                    title: "Split import statement".into(),
                    edits: vec![(import.text_range(), replacement)],
                });
            }
        }
        if let ast::Stmt::ImportFrom(import) = &stmt {
            if !import.is_star() {
                {
                    let module = import
                        .module()
                        .map(|module| module.path())
                        .unwrap_or_default();
                    let level = import.level();
                    let package = file
                        .module_name
                        .as_deref()
                        .map(|name| {
                            let mut p: Vec<_> = name.split('.').collect();
                            if !file.is_package_init() {
                                p.pop();
                            }
                            p
                        })
                        .unwrap_or_default();
                    if level > 0 && level <= package.len() {
                        let mut parts = package[..package.len() + 1 - level].to_vec();
                        if !module.is_empty() {
                            parts.push(&module)
                        }
                        let absolute = parts.join(".");
                        out.push(Intention {
                            title: "Convert relative import to absolute".into(),
                            edits: vec![(
                                import.text_range(),
                                format!(
                                    "from {absolute} import {}",
                                    all.iter()
                                        .map(|a| a.syntax().text().to_string())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            )],
                        });
                    } else if level == 0 && module != "__future__" && !package.is_empty() {
                        let target: Vec<_> = module.split('.').collect();
                        let common = package
                            .iter()
                            .zip(&target)
                            .take_while(|(a, b)| a == b)
                            .count();
                        if common > 0 {
                            let relative = format!(
                                "{}{}",
                                ".".repeat(package.len() - common + 1),
                                target[common..].join(".")
                            );
                            out.push(Intention {
                                title: "Convert absolute import to relative".into(),
                                edits: vec![(
                                    import.text_range(),
                                    format!(
                                        "from {relative} import {}",
                                        all.iter()
                                            .map(|a| a.syntax().text().to_string())
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ),
                                )],
                            });
                        }
                    }
                    if module != "__future__"
                        && isolated_statement(file, &stmt)
                        && !all.is_empty()
                        && all.iter().all(|a| {
                            a.alias().is_none()
                                && matches!(
                                    a.name().as_str(),
                                    "annotations"
                                        | "division"
                                        | "absolute_import"
                                        | "print_function"
                                        | "unicode_literals"
                                        | "generator_stop"
                                        | "barry_as_FLUFL"
                                        | "nested_scopes"
                                        | "generators"
                                        | "with_statement"
                                )
                        })
                    {
                        let at=ast::Module::cast(file.parse.syntax_node()).and_then(|m|m.statements().find(|s|!matches!(s,ast::Stmt::Expr(e) if matches!(e.expr(),Some(ast::Expr::String(_))))).map(|s|s.text_range().start())).unwrap_or(TextSize::new(file.text.len() as u32));
                        out.push(Intention {
                            title: "Change to from __future__ import".into(),
                            edits: vec![
                                (stmt_lines(&file.text, stmt.text_range()), String::new()),
                                (
                                    TextRange::empty(at),
                                    format!(
                                        "from __future__ import {}\n",
                                        all.iter().map(|a| a.name()).collect::<Vec<_>>().join(", ")
                                    ),
                                ),
                            ],
                        });
                    }
                    merge_same_module(file, import, &module, &mut out);
                }
            }
        }
        import_locally(file, &stmt, &mut out);
    }
    export_intention(db, file, offset, &mut out);
    out
}
fn merge_same_module(
    file: &FileInfo,
    selected: &ast::ImportFromStmt,
    module: &str,
    out: &mut Vec<Intention>,
) {
    let Some(parent) = selected.syntax().parent() else {
        return;
    };
    let peers: Vec<_> = parent
        .children()
        .filter_map(ast::ImportFromStmt::cast)
        .filter(|i| {
            i.level() == selected.level()
                && !i.is_star()
                && i.module().is_some_and(|m| m.path() == module)
        })
        .collect();
    if peers
        .iter()
        .any(|peer| !isolated_statement(file, &ast::Stmt::ImportFrom(peer.clone())))
    {
        return;
    }
    if peers.len() < 2
        || peers
            .iter()
            .any(|i| slice(&file.text, stmt_lines(&file.text, i.text_range())).contains('#'))
    {
        return;
    }
    // Only adjacent imports: never move bindings across executable code.
    let start = peers.first().unwrap().text_range().start();
    let end = peers.last().unwrap().text_range().end();
    if parent.children().filter_map(ast::Stmt::cast).any(|s| {
        s.text_range().start() >= start
            && s.text_range().end() <= end
            && !matches!(s, ast::Stmt::ImportFrom(i) if i.level()==selected.level() && i.module().is_some_and(|m|m.path()==module))
    }) {
        return;
    }
    let mut names = Vec::new();
    for peer in &peers {
        for alias in peer.aliases() {
            let text = alias.syntax().text().to_string();
            if !names.contains(&text) {
                names.push(text)
            }
        }
    }
    let prefix = ".".repeat(selected.level());
    let mut edits = vec![(
        peers[0].text_range(),
        format!("from {prefix}{module} import {}", names.join(", ")),
    )];
    edits.extend(
        peers
            .iter()
            .skip(1)
            .map(|peer| (stmt_lines(&file.text, peer.text_range()), String::new())),
    );
    out.push(Intention {
        title: format!("Merge imports from {prefix}{module}"),
        edits,
    });
}
fn import_locally(file: &FileInfo, stmt: &ast::Stmt, out: &mut Vec<Intention>) {
    if !isolated_statement(file, stmt) {
        return;
    }
    if stmt
        .syntax()
        .ancestors()
        .any(|n| n.kind() == SyntaxKind::FUNC_DEF)
    {
        return;
    }
    let mut destination = None;
    let exports = rustpy_semantic::imports::dunder_all(file);
    for alias in aliases(stmt) {
        let name = alias
            .alias()
            .map(|token| token.text().to_string())
            .unwrap_or_else(|| {
                alias
                    .name()
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .to_string()
            });
        if exports.contains(&name) {
            return;
        }
        let Some(id) = alias_binding(file, &alias) else {
            return;
        };
        for r in file.scopes.references_to(id) {
            let Some(function) = file
                .parse
                .syntax_node()
                .token_at_offset(r.range.start())
                .right_biased()
                .and_then(|t| t.parent_ancestors().find_map(ast::FuncDef::cast))
            else {
                return;
            };
            // Defaults/decorators execute at definition time, not in the body.
            let Some(body) = function.body() else { return };
            if !body.text_range().contains_range(r.range) {
                return;
            }
            if destination
                .as_ref()
                .is_some_and(|f: &ast::FuncDef| f.text_range() != function.text_range())
            {
                return;
            }
            destination = Some(function);
        }
    }
    let Some(function) = destination else { return };
    let Some(body) = function.body() else { return };
    let Some(first) = body.statements().next() else {
        return;
    };
    if !isolated_statement(file, &first) {
        return;
    }
    if let Some(scope) = file
        .scopes
        .scopes
        .iter()
        .find(|scope| scope.range == function.text_range())
    {
        if aliases(stmt)
            .iter()
            .filter_map(|alias| alias_binding(file, alias))
            .any(|id| {
                let name = &file.scopes.binding(id).name;
                scope.globals.contains(name) || scope.nonlocals.contains(name)
            })
        {
            return;
        }
    }

    let mut at = first.text_range().start();
    if let ast::Stmt::Expr(expr) = &first {
        if matches!(expr.expr(), Some(ast::Expr::String(_))) {
            at = body
                .statements()
                .nth(1)
                .map(|s| s.text_range().start())
                .unwrap_or(first.text_range().end());
        }
    }
    let indent = indent_at(&file.text, usize::from(first.text_range().start()));
    if indent.is_empty() {
        return;
    }
    out.push(Intention {
        title: "Import locally".into(),
        edits: vec![
            (stmt_lines(&file.text, stmt.text_range()), String::new()),
            (
                TextRange::empty(at),
                format!("{}\n{indent}", stmt.syntax().text()),
            ),
        ],
    });
}
fn export_intention(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    out: &mut Vec<Intention>,
) {
    let root = file.parse.syntax_node();
    let Some(token) = token_at(&root, offset) else {
        return;
    };
    let name = token.text();
    let Some(bindings) = file
        .scopes
        .scope(file.scopes.module_scope())
        .bindings
        .get(name)
    else {
        return;
    };
    let is_module_name = bindings
        .iter()
        .any(|id| file.scopes.binding(*id).range == token.text_range())
        || file
            .scopes
            .reference_at(token.text_range().start())
            .is_some_and(|reference| {
                reference.range == token.text_range()
                    && reference.resolved.is_some_and(|id| bindings.contains(&id))
            });
    if !is_module_name {
        return;
    }
    if name == "__all__" {
        let exported = rustpy_semantic::imports::dunder_all_with(db, file);
        let missing: Vec<_> = file
            .scopes
            .scope(file.scopes.module_scope())
            .bindings
            .keys()
            .filter(|name| {
                !name.starts_with('_')
                    && valid_identifier(name)
                    && !exported.iter().any(|exported| exported == name.as_str())
            })
            .map(|name| name.to_string())
            .collect();
        if !missing.is_empty() {
            out.push(Intention {
                title: "Update __all__ with public module names".into(),
                edits: vec![(
                    TextRange::new(
                        TextSize::new(file.text.len() as u32),
                        TextSize::new(file.text.len() as u32),
                    ),
                    format!(
                        "\n__all__ = [*__all__, {}]\n",
                        missing
                            .iter()
                            .map(|name| format!("{name:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )],
            });
        }
        return;
    }
    if !valid_identifier(name)
        || file
            .scopes
            .scope(file.scopes.module_scope())
            .bindings
            .get(name)
            .is_none()
    {
        return;
    }
    let exports = rustpy_semantic::imports::dunder_all_with(db, file);
    if exports.iter().any(|n| n == name) {
        return;
    }
    let mut names = exports;
    names.push(name.into());
    let replacement = format!(
        "__all__ = [{}]",
        names
            .iter()
            .map(|n| format!("{n:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    // Append to the existing binding rather than replacing dynamic export code.
    let has_all = file
        .scopes
        .scope(file.scopes.module_scope())
        .bindings
        .contains_key("__all__");
    let at = if has_all {
        TextSize::new(file.text.len() as u32)
    } else {
        rustpy_semantic::import_placement::import_insert_offset(file)
    };
    let value = if has_all {
        format!("\n__all__ = [*__all__, {name:?}]\n")
    } else {
        format!("{replacement}\n")
    };
    out.push(Intention {
        title: format!("Add '{name}' to __all__"),
        edits: vec![(TextRange::empty(at), value)],
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn setup(source: &str, path: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/project"));
        let id = db.set_file_text(Path::new(path), source);
        let file = db.file(id).unwrap();
        (db, file)
    }
    fn transform(source: &str, path: &str, needle: &str, title: &str) -> String {
        let (db, file) = setup(source, path);
        let at = TextSize::new(source.find(needle).unwrap() as u32);
        let intention = crate::intentions::intentions_at(&db, &file, at)
            .into_iter()
            .find(|i| i.title.starts_with(title))
            .unwrap_or_else(|| panic!("{title} unavailable"));
        apply_edits(source, &intention.edits)
    }
    #[test]
    fn aliases_follow_binding_and_preserve_shadowed_parameters() {
        let source = "from pathlib import Path\ndef f(Path): return Path\nvalue = Path('x')\n";
        let (_, file) = setup(source, "/project/main.py");
        let (_, id) = alias_at(&file, TextSize::new(22)).unwrap();
        let intention = alias_import(&file, id, "FilePath").unwrap();
        assert_eq!(apply_edits(source,&intention.edits),"from pathlib import Path as FilePath\ndef f(Path): return Path\nvalue = FilePath('x')\n");
        for invalid in [
            "class",
            "elif",
            "except",
            "import",
            "a b",
            "name; pass",
            "",
            "1name",
        ] {
            assert!(
                alias_import(&file, id, invalid).is_none(),
                "invalid alias {invalid:?}"
            );
        }
        for soft in ["match", "case", "type", "_"] {
            let edited = apply_edits(source, &alias_import(&file, id, soft).unwrap().edits);
            assert!(
                rustpy_syntax::parse(&edited).errors().is_empty(),
                "valid soft-keyword alias {soft}"
            );
        }
        let (_, file) = setup("import os\nother=1\nprint(os.name)\n", "/project/main.py");
        let (_, id) = alias_at(&file, TextSize::new(8)).unwrap();
        assert!(alias_import(&file, id, "other").is_none());
    }
    #[test]
    fn dotted_import_alias_replaces_actual_qualified_receiver() {
        let source = "import os.path\nprint(os.path.join('a','b'))\n";
        let (_, file) = setup(source, "/project/main.py");
        let (_, id) = alias_at(&file, TextSize::new(8)).unwrap();
        assert_eq!(
            apply_edits(source, &alias_import(&file, id, "paths").unwrap().edits),
            "import os.path as paths\nprint(paths.join('a','b'))\n"
        );
    }
    #[test]
    fn split_merge_and_relative_conversions_preserve_aliases() {
        assert_eq!(
            transform(
                "import os as system, sys\n",
                "/project/pkg/main.py",
                "os",
                "Split import"
            ),
            "import os as system\nimport sys\n"
        );
        assert_eq!(
            transform(
                "from os import path\nfrom os import sep as separator\nprint(path,separator)\n",
                "/project/main.py",
                "path",
                "Merge imports"
            ),
            "from os import path, sep as separator\nprint(path,separator)\n"
        );
        assert_eq!(
            transform(
                "from pkg.helpers import value as v\n",
                "/project/pkg/main.py",
                "helpers",
                "Convert absolute import"
            ),
            "from .helpers import value as v\n"
        );
        assert_eq!(
            transform(
                "from ..helpers import value\n",
                "/project/pkg/sub/main.py",
                "helpers",
                "Convert relative import"
            ),
            "from pkg.helpers import value\n"
        );
    }
    #[test]
    fn local_import_retains_docstring_and_refuses_multiple_consumers() {
        let source = "import os\n\ndef f():\n    \"\"\"Doc.\"\"\"\n    return os.name\n";
        assert_eq!(
            transform(source, "/project/main.py", "os", "Import locally"),
            "\ndef f():\n    \"\"\"Doc.\"\"\"\n    import os\n    return os.name\n"
        );
        let source = "import os\ndef f(): return os.name\ndef g(): return os.name\n";
        let (db, file) = setup(source, "/project/main.py");
        assert!(!at(&db, &file, TextSize::new(8))
            .iter()
            .any(|i| i.title == "Import locally"));
    }
    #[test]
    fn export_update_preserves_existing_tuple_and_future_is_before_imports() {
        let source = "__all__ = ('old',)\nold=1\nnew=2\n";
        let updated = transform(source, "/project/main.py", "new=", "Add 'new'");
        assert!(updated.ends_with("__all__ = [*__all__, \"new\"]\n"));
        assert_eq!(
            transform(
                "\"\"\"Doc\"\"\"\nimport os\nfrom compat import annotations\n",
                "/project/main.py",
                "annotations",
                "Change to from __future__"
            ),
            "\"\"\"Doc\"\"\"\nfrom __future__ import annotations\nimport os\n"
        );
    }
    #[test]
    fn missing_import_offers_typed_create_and_interpreter_install_requests() {
        let (db, file) = setup("import unknown_package.module\n", "/project/main.py");
        let choices = requests(&db, &file, TextSize::new(12));
        assert!(choices.iter().any(|(_,request)|matches!(request,ImportRequest::Create{path,package:false} if path==Path::new("/project/unknown_package/module.py"))));
        assert!(choices.iter().any(|(_,request)|matches!(request,ImportRequest::Create{path,package:true} if path==Path::new("/project/unknown_package/module/__init__.py"))));
        assert!(choices.iter().any(|(_,request)|matches!(request,ImportRequest::Install{distribution} if distribution=="unknown_package")));
    }
}

#[cfg(test)]
mod whole_item_tests {
    use super::*;
    use std::path::Path;
    fn source(text: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/project"));
        let id = db.set_file_text(Path::new("/project/main.py"), text);
        let file = db.file(id).unwrap();
        (db, file)
    }
    #[test]
    fn unresolved_choices_offer_project_and_stdlib_from_imports() {
        let (db, file) = source("Path('π')\n");
        let choices = crate::inspections::import_fixes(
            &db,
            &file,
            "Path",
            &[("Path".into(), "tools.paths".into())],
        );
        for module in ["pathlib", "tools.paths"] {
            let choice = choices
                .iter()
                .find(|choice| choice.title == format!("Import 'from {module} import Path'"))
                .unwrap();
            assert_eq!(
                apply_edits(&file.text, &choice.edits),
                format!("from {module} import Path\nPath('π')\n")
            );
        }
    }
    #[test]
    fn optimize_and_update_all_use_shared_usage_facts() {
        let (db, file) = source("import sys\nimport os\nprint(os.name)\n");
        let optimized = at(&db, &file, TextSize::new(8))
            .into_iter()
            .find(|i| i.title == "Optimize imports")
            .unwrap();
        let result = apply_edits(&file.text, &optimized.edits);
        assert!(result.contains("import os"));
        assert!(!result.contains("import sys"));
        let (db, file) = source("__all__ = ['old']\nold=1\nnew=2\n_hidden=3\n");
        let update = at(&db, &file, TextSize::new(2))
            .into_iter()
            .find(|i| i.title.starts_with("Update __all__"))
            .unwrap();
        let result = apply_edits(&file.text, &update.edits);
        assert!(result.contains("[*__all__, \"new\"]"));
        assert!(!result.contains("\"_hidden\""));
    }
    #[test]
    fn alias_refuses_new_capture_in_a_consumer_scope() {
        let (_, file) = source("import os\ndef f(local): return os.name\n");
        let (_, id) = alias_at(&file, TextSize::new(8)).unwrap();
        assert!(alias_import(&file, id, "local").is_none());
    }
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    use std::path::Path;
    fn setup(text: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/project"));
        let id = db.set_file_text(Path::new("/project/main.py"), text);
        let file = db.file(id).unwrap();
        (db, file)
    }
    fn has(text: &str, needle: &str, title: &str) -> bool {
        let (db, file) = setup(text);
        crate::intentions::intentions_at(
            &db,
            &file,
            TextSize::new(text.find(needle).unwrap() as u32),
        )
        .iter()
        .any(|intention| intention.title.starts_with(title))
    }
    #[test]
    fn new_alias_cannot_capture_existing_builtin_or_unresolved_reads() {
        for source in [
            "import os\ndef f(): return os.name\ndef g(): print('x')\n",
            "import os\ndef f(): return os.name\ndef g(): return pending\n",
        ] {
            let (_, file) = setup(source);
            let (_, id) = alias_at(&file, TextSize::new(8)).unwrap();
            let alias = if source.contains("print(") {
                "print"
            } else {
                "pending"
            };
            assert!(alias_import(&file, id, alias).is_none());
        }
        let (_, file) = setup("import os\ndef f(): return os.name\ndef g(print): return print\n");
        let (_, id) = alias_at(&file, TextSize::new(8)).unwrap();
        assert!(
            alias_import(&file, id, "print").is_some(),
            "unrelated local parameter remains local"
        );
    }
    #[test]
    fn shared_line_and_inline_suite_edits_are_not_offered() {
        assert!(!has(
            "import os; sentinel=1\ndef f():\n    return os.name\n",
            "import os",
            "Import locally"
        ));
        assert!(!has(
            "from os import path; from os import sep\nprint(path,sep)\n",
            "path;",
            "Merge imports"
        ));
        assert!(!has(
            "def f(): import os, sys; return os.name\n",
            "import os",
            "Split import"
        ));
        assert!(!has(
            "def f(): import os; return os.name\n",
            "import os",
            "Move import to top"
        ));
        assert!(!has(
            "import os\ndef f(): return os.name\n",
            "import os",
            "Import locally"
        ));
        assert!(!has(
            "from compat import annotations; sentinel=1\n",
            "annotations",
            "Change to from __future__"
        ));
    }
    #[test]
    fn local_import_does_not_cross_global_or_nonlocal_binding_contracts() {
        assert!(!has(
            "import os\ndef f():\n    global os\n    return os.name\n",
            "import os",
            "Import locally"
        ));
        assert!(!has("def outer():\n    import os\n    def inner():\n        nonlocal os\n        return os.name\n","import os","Import locally"));
        let (_,file)=setup("def outer():\n    import os\n    def inner():\n        nonlocal os\n        return os.name\n");
        let at = TextSize::new(file.text.find("import os").unwrap() as u32 + 8);
        let (_, id) = alias_at(&file, at).unwrap();
        assert!(alias_import(&file, id, "system").is_none());
    }
    #[test]
    fn update_all_requires_the_actual_module_binding_or_reference() {
        for source in [
            "value=1\nprint(module.__all__)\n",
            "__all__=[]\nvalue=1\nprint(module.__all__)\n",
            "value=1\ndef f():\n    __all__=[]\n    return __all__\n",
        ] {
            let at = if source.contains("module.__all__") {
                source.find("module.__all__").unwrap() + 7
            } else {
                source.rfind("__all__").unwrap()
            };
            let (db, file) = setup(source);
            assert!(
                !crate::intentions::intentions_at(&db, &file, TextSize::new(at as u32))
                    .iter()
                    .any(|i| i.title.starts_with("Update __all__"))
            );
        }
        assert!(has(
            "__all__=[]\nvalue=1\nprint(__all__)\n",
            "__all__=[]",
            "Update __all__"
        ));
    }
    #[test]
    fn alias_refuses_old_name_rebinding_and_repeated_import_aliases() {
        for source in [
            "import os\nprint(os.name)\nos=1\nprint(os)\n",
            "import pathlib as thing\nimport os as thing\nprint(thing.name)\n",
        ] {
            let (_, file) = setup(source);
            let at = if source.contains(" as thing") {
                source.find("thing").unwrap()
            } else {
                source.find("os").unwrap()
            };
            let (_, id) = alias_at(&file, TextSize::new(at as u32)).unwrap();
            assert!(alias_import(&file, id, "system").is_none());
        }
        let source = "import os\nprint(os.name)\n";
        let (_, file) = setup(source);
        let (_, id) = alias_at(&file, TextSize::new(8)).unwrap();
        let intention = alias_import(&file, id, "system").unwrap();
        assert_eq!(
            apply_edits(source, &intention.edits),
            "import os as system\nprint(system.name)\n"
        );
    }
}
