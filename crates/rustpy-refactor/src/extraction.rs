//! Extraction choices built on the existing lexical input/output planner.
use crate::refactor::{has_descendant, selected_statements, Edits, Refactoring};
use crate::refactor2::{FileOp, MultiFileRefactoring};
use rustpy_core::{TextRange, TextSize};
use rustpy_semantic::edit_helpers::is_identifier;
use rustpy_semantic::{
    db::{Analysis, FileInfo},
    edit_helpers::{
        after_last_line, indent_at, line_start, normalize, reindent, rng, slice, stmt_lines, ts,
    },
    scope::ScopeKind,
};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind,
};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CallableDestination {
    #[default]
    Automatic,
    Function,
    InstanceMethod,
    StaticMethod,
    ClassMethod,
}
#[derive(Clone, Debug, Default)]
pub struct CallableOptions {
    pub destination: CallableDestination,
    pub replace_duplicates: bool,
}
pub fn extract_callable(
    _db: &Analysis,
    file: &Arc<FileInfo>,
    range: TextRange,
    name: &str,
    options: &CallableOptions,
) -> Result<Refactoring, String> {
    if !is_identifier(name) {
        return Err(format!("'{name}' is not a valid function name"));
    }
    if file
        .scopes
        .bindings
        .iter()
        .any(|binding| binding.name == name)
    {
        return Err(format!("'{name}' is already defined; choose another name"));
    }
    if !rustpy_syntax::parse(&format!("def {name}():\n    pass\n"))
        .errors()
        .is_empty()
    {
        return Err("Choose a valid non-keyword function name".into());
    }
    let text: &str = &file.text;
    let (stmts, block) = selected_statements(file, range)?;
    let first_range = stmts[0].text_range();
    let last_range = stmts.last().unwrap().text_range();
    let sel = TextRange::new(first_range.start(), last_range.end());
    let sel_lines = rng(
        line_start(text, first_range.start().into()),
        usize::from(stmt_lines(text, last_range).end()),
    );
    let func = block.ancestors().find_map(ast::FuncDef::cast);
    let scope_id = file
        .scopes
        .scope_at(first_range.start() + ts(1).min(first_range.len()));
    let scope_kind = file.scopes.scope(scope_id).kind;
    if scope_kind == ScopeKind::Class {
        return Err("Cannot extract statements from a class body".into());
    }
    if block.kind() == SyntaxKind::BLOCK
        && block
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::CLASS_DEF)
    {
        return Err("Cannot extract statements from a class body".into());
    }
    let nested_scopes = [
        SyntaxKind::FUNC_DEF,
        SyntaxKind::LAMBDA_EXPR,
        SyntaxKind::CLASS_DEF,
    ];
    let terminal_return = stmts
        .last()
        .is_some_and(|statement| statement.syntax().kind() == SyntaxKind::RETURN_STMT);
    for (index, st) in stmts.iter().enumerate() {
        let n = st.syntax();
        if has_descendant(n, &[SyntaxKind::RETURN_STMT], &nested_scopes)
            && !(terminal_return && index + 1 == stmts.len())
        {
            return Err("The selection contains a 'return' statement".into());
        }
        if has_descendant(
            n,
            &[SyntaxKind::BREAK_STMT, SyntaxKind::CONTINUE_STMT],
            &[
                SyntaxKind::FUNC_DEF,
                SyntaxKind::LAMBDA_EXPR,
                SyntaxKind::CLASS_DEF,
                SyntaxKind::FOR_STMT,
                SyntaxKind::WHILE_STMT,
            ],
        ) {
            return Err("The selection contains 'break' or 'continue' outside a loop".into());
        }
        if has_descendant(
            n,
            &[SyntaxKind::GLOBAL_STMT, SyntaxKind::NONLOCAL_STMT],
            &nested_scopes,
        ) {
            return Err("The selection contains 'global' or 'nonlocal' declarations".into());
        }
    }
    let contains_yield = stmts.iter().any(|statement| {
        has_descendant(
            statement.syntax(),
            &[SyntaxKind::YIELD_EXPR, SyntaxKind::YIELD_FROM_EXPR],
            &nested_scopes,
        )
    });
    let contains_await = stmts
        .iter()
        .any(|st| has_descendant(st.syntax(), &[SyntaxKind::AWAIT_EXPR], &nested_scopes));
    // Method context.
    let is_method = func.as_ref().is_some_and(|f| f.is_method());
    let first_param = func
        .as_ref()
        .and_then(|f| f.params())
        .and_then(|p| p.plain_params().next())
        .map(|p| p.name_text());
    let receiver = if is_method
        && !func
            .as_ref()
            .is_some_and(|function| function.has_decorator("staticmethod"))
    {
        first_param.clone()
    } else {
        None
    };
    let destination = match options.destination {
        CallableDestination::Automatic => {
            if is_method {
                if func
                    .as_ref()
                    .is_some_and(|function| function.has_decorator("classmethod"))
                {
                    CallableDestination::ClassMethod
                } else if receiver.is_some() {
                    CallableDestination::InstanceMethod
                } else {
                    CallableDestination::StaticMethod
                }
            } else {
                CallableDestination::Function
            }
        }
        other => other,
    };
    if destination != CallableDestination::Function && !is_method {
        return Err("Method extraction needs an enclosing class method".into());
    }
    if destination == CallableDestination::InstanceMethod
        && func.as_ref().is_some_and(|function| {
            function.has_decorator("classmethod") || function.has_decorator("staticmethod")
        })
    {
        return Err(
            "An instance method cannot be called without an instance in this context".into(),
        );
    }
    let self_name = if matches!(
        destination,
        CallableDestination::InstanceMethod | CallableDestination::ClassMethod
    ) {
        receiver.clone()
    } else {
        None
    };
    if destination == CallableDestination::ClassMethod
        && !func
            .as_ref()
            .is_some_and(|function| function.has_decorator("classmethod"))
    {
        return Err("Extract a classmethod from a classmethod context; instance-dependent selections need an instance method".into());
    }
    if contains_yield && func.as_ref().is_some_and(|function| function.is_async()) {
        return Err(
            "An async generator cannot delegate with yield from; extract a smaller block".into(),
        );
    }
    if contains_await && !func.as_ref().is_some_and(|function| function.is_async()) {
        return Err("Await extraction needs an enclosing async function".into());
    }
    if destination==CallableDestination::Function && is_method && stmts.iter().flat_map(|statement|statement.syntax().descendants()).any(|node| {
        if let Some(call)=ast::CallExpr::cast(node.clone()) {return matches!(call.callee(),Some(ast::Expr::Name(name)) if name.name_text()=="super") && call.arg_list().is_some_and(|arguments|arguments.args().next().is_none());}
        ast::AttributeExpr::cast(node).is_some_and(|attribute|attribute.attr_text().starts_with("__")&&!attribute.attr_text().ends_with("__"))
    }) {return Err("Moving zero-argument super() or class-private attributes to a top-level function changes class context".into());}
    let scope = file.scopes.scope(scope_id);
    if destination == CallableDestination::Function
        && file.scopes.references.iter().any(|reference| {
            sel.contains_range(reference.range)
                && reference.resolved.is_some_and(|id| {
                    let binding = file.scopes.binding(id);
                    binding.scope != scope_id
                        && file.scopes.scope(binding.scope).kind != ScopeKind::Module
                })
        })
    {
        return Err("The selection captures an enclosing scope; choose a method or pass captured values explicitly first".into());
    }
    if file.scopes.references.iter().any(|reference| {
        sel.contains_range(reference.range)
            && reference.is_write
            && (scope.globals.iter().any(|name| name == &reference.name)
                || scope.nonlocals.iter().any(|name| name == &reference.name))
    }) || file.scopes.bindings.iter().any(|binding| {
        sel.contains_range(binding.range)
            && (scope.globals.contains(&binding.name) || scope.nonlocals.contains(&binding.name))
    }) {
        return Err("The selection writes an enclosing global/nonlocal binding; include an explicit declaration or extract a smaller block".into());
    }
    // Earliest event per local name inside the selection: (offset, is_read).
    let mut events: BTreeMap<String, (usize, bool)> = BTreeMap::new();
    let mut assigned: HashSet<String> = HashSet::new();
    for r in &file.scopes.references {
        if !sel.contains_range(r.range) {
            continue;
        }
        let Some(bid) = r.resolved else { continue };
        if file.scopes.binding(bid).scope != scope_id {
            continue;
        }
        let off: usize = r.range.start().into();
        let e = events
            .entry(r.name.to_string())
            .or_insert((off, !r.is_write));
        if off < e.0 {
            *e = (off, !r.is_write);
        }
        if r.is_write {
            assigned.insert(r.name.to_string());
        }
    }
    for b in &file.scopes.bindings {
        if b.scope != scope_id || !sel.contains_range(b.range) || !b.is_definition() {
            continue;
        }
        let off: usize = b.range.start().into();
        let e = events.entry(b.name.to_string()).or_insert((off, false));
        if off < e.0 {
            *e = (off, false);
        }
        assigned.insert(b.name.to_string());
    }
    let mut params: Vec<(usize, String)> = events
        .iter()
        .filter(|(n, (_, read))| {
            *read
                && Some(n.as_str()) != self_name.as_deref()
                && !scope.globals.iter().any(|g| g == n.as_str())
        })
        .map(|(n, (off, _))| (*off, n.clone()))
        .collect();
    params.sort();
    let params: Vec<String> = params.into_iter().map(|(_, n)| n).collect();
    // Outputs: assigned in the selection and read afterwards in this scope.
    let mut outputs: Vec<(usize, String)> = Vec::new();
    for n in &assigned {
        if Some(n.as_str()) == self_name.as_deref() {
            continue;
        }
        let ids = file.scopes.bindings_in(scope_id, n);
        let read_after = file.scopes.references.iter().any(|r| {
            r.range.start() >= sel.end()
                && !r.is_write
                && r.resolved.is_some_and(|id| ids.contains(&id))
        });
        if read_after {
            let first_bind = file
                .scopes
                .bindings
                .iter()
                .filter(|b| {
                    b.name.as_str() == n && b.scope == scope_id && sel.contains_range(b.range)
                })
                .map(|b| usize::from(b.range.start()))
                .min()
                .unwrap_or(0);
            outputs.push((first_bind, n.clone()));
        }
    }
    outputs.sort();
    let outputs: Vec<String> = outputs.into_iter().map(|(_, n)| n).collect();
    // An output created only in a conditional branch would become an
    // unconditional helper return (and could raise earlier than the original).
    // Keep that as an explicit conflict rather than manufacturing a value.
    for output in &outputs {
        if !params.contains(output)
            && !stmts.iter().any(|statement| {
                matches!(
                    statement.syntax().kind(),
                    SyntaxKind::ASSIGN_STMT
                        | SyntaxKind::ANN_ASSIGN_STMT
                        | SyntaxKind::AUG_ASSIGN_STMT
                ) && file.scopes.bindings.iter().any(|binding| {
                    binding.scope == scope_id
                        && binding.name.as_str() == output
                        && binding.node_range == statement.syntax().text_range()
                })
            })
        {
            return Err(format!(
                "Output '{output}' is only conditionally assigned; initialize it before the selection or extract a smaller block"
            ));
        }
    }
    // Texts.
    let def_indent = match &func {
        Some(_) if destination == CallableDestination::Function => String::new(),
        Some(f) => indent_at(text, f.text_range().start().into()),
        None => indent_at(text, first_range.start().into()),
    };
    let body_indent = format!("{def_indent}    ");
    let base_indent = indent_at(text, first_range.start().into());
    let is_async = contains_await && func.as_ref().is_some_and(|f| f.is_async());
    let async_kw = if is_async { "async " } else { "" };
    let await_kw = if is_async { "await " } else { "" };
    let static_method = destination == CallableDestination::StaticMethod;
    let mut sig_params: Vec<String> = Vec::new();
    if let Some(s) = &self_name {
        sig_params.push(s.clone());
    }
    sig_params.extend(params.iter().cloned());
    let mut new_fn = String::new();
    if static_method {
        new_fn.push_str(&format!("{def_indent}@staticmethod\n"));
    }
    if destination == CallableDestination::ClassMethod {
        new_fn.push_str(&format!("{def_indent}@classmethod\n"));
    }
    let def_line_off = new_fn.len() + def_indent.len() + async_kw.len() + 4;
    new_fn.push_str(&format!(
        "{def_indent}{async_kw}def {name}({}):\n",
        sig_params.join(", ")
    ));
    new_fn.push_str(&reindent(
        slice(text, sel_lines),
        &base_indent,
        &body_indent,
    ));
    if !new_fn.ends_with('\n') {
        new_fn.push('\n');
    }
    if !outputs.is_empty() && !terminal_return {
        new_fn.push_str(&format!("{body_indent}return {}\n", outputs.join(", ")));
    }
    let (ins_at, prefix, suffix) = match &func {
        Some(f) => {
            let anchor = if destination == CallableDestination::Function {
                f.syntax()
                    .ancestors()
                    .filter(|node| {
                        node.parent()
                            .is_some_and(|parent| parent.kind() == SyntaxKind::MODULE)
                    })
                    .last()
                    .unwrap_or_else(|| f.syntax().clone())
            } else {
                f.syntax().clone()
            };
            let at = after_last_line(text, anchor.text_range().end().into());
            let nl = if at == text.len() && !text.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            (
                at,
                format!("{nl}{}", if is_method { "\n" } else { "\n\n" }),
                String::new(),
            )
        }
        None => (
            usize::from(sel_lines.start()),
            String::new(),
            "\n\n".to_string(),
        ),
    };
    let insert_text = format!("{prefix}{new_fn}{suffix}");
    let class_name = func
        .as_ref()
        .and_then(|f| f.syntax().parent())
        .and_then(|b| b.parent())
        .and_then(ast::ClassDef::cast)
        .map(|c| c.name_text());
    let call_prefix = match (&self_name, static_method, class_name) {
        (Some(s), _, _) => format!("{s}."),
        (None, true, Some(c)) => format!("{c}."),
        _ => String::new(),
    };
    let targets = if terminal_return {
        "return ".to_string()
    } else if outputs.is_empty() {
        String::new()
    } else {
        format!("{} = ", outputs.join(", "))
    };
    let invocation = format!("{await_kw}{call_prefix}{name}({})", params.join(", "));
    let invocation = if contains_yield {
        format!("(yield from {invocation})")
    } else {
        invocation
    };
    let call = format!("{base_indent}{targets}{invocation}\n");
    let call_name_off = base_indent.len()
        + targets.len()
        + if contains_yield {
            "(yield from ".len()
        } else {
            0
        }
        + await_kw.len()
        + call_prefix.len();
    let mut ed = Edits::default();
    ed.add_marked(sel_lines, call, vec![(call_name_off, name.len())]);
    ed.add_marked(
        rng(ins_at, ins_at),
        insert_text.clone(),
        vec![(prefix.len() + def_line_off, name.len())],
    );
    if options.replace_duplicates {
        let width = stmts.len();
        let siblings: Vec<_> = block.children().filter_map(ast::Stmt::cast).collect();
        let mut last_end = TextSize::new(0);
        for group in siblings.windows(width) {
            let candidate = TextRange::new(
                group[0].text_range().start(),
                group.last().unwrap().text_range().end(),
            );
            if candidate.start() < last_end
                || candidate
                    .intersect(sel)
                    .is_some_and(|range| !range.is_empty())
                || normalize(slice(text, candidate)) != normalize(slice(text, sel))
            {
                continue;
            }
            // Same lexical scope and exact identifiers preserve input/output binding;
            // the whole selection is revalidated before replacing each duplicate.
            let candidate_options = CallableOptions {
                replace_duplicates: false,
                ..options.clone()
            };
            let duplicate = extract_callable(_db, file, candidate, name, &candidate_options)?;
            if !duplicate
                .edits
                .iter()
                .any(|(range, text)| range.is_empty() && text == &insert_text)
            {
                continue;
            }

            if let Some((range, replacement)) = duplicate
                .edits
                .iter()
                .find(|(range, _)| !range.is_empty() && range.contains_range(candidate))
            {
                ed.add_marked(
                    *range,
                    replacement.clone(),
                    vec![(call_name_off, name.len())],
                );
                last_end = candidate.end();
            }
        }
    }
    let (edits, rename_ranges) = ed.finish();
    Ok(Refactoring {
        title: format!("Extract method '{name}'"),
        edits,
        rename_ranges,
        name: name.to_string(),
    })
}

/// Introduce a parameter with a definition default and an independently chosen
/// value at existing callers. The old planner supplies expression/name safety;
/// Change Signature owns all binding-aware call updates and override conflicts.
pub fn extract_parameter(
    db: &Analysis,
    file: &Arc<FileInfo>,
    range: TextRange,
    name: Option<&str>,
    default: Option<&str>,
    call_value: Option<&str>,
    project_files: &[PathBuf],
) -> Result<(MultiFileRefactoring, crate::change_signature::ChangeSummary), String> {
    let local = crate::refactor::introduce_parameter(db, file, range)?;
    let expression = crate::refactor::expr_at(file, range).ok_or("Select an expression")?;
    let expression_range = expression.syntax().text_range();
    let mut model = crate::change_signature::signature_model(file, expression_range.start())?;
    let parameter_name = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&local.name);
    if parameter_name != local.name
        && crate::refactor::taken_names(file, expression_range.start()).contains(parameter_name)
    {
        return Err(format!(
            "'{parameter_name}' is already bound in this context"
        ));
    }
    let source_default = expression.syntax().text().to_string();
    let mut parameter = crate::change_signature::SigParam::added(
        parameter_name,
        default.or(Some(&source_default)),
        call_value,
    );
    parameter.kind = crate::change_signature::ParamKind::KeywordOnly;
    let at = model
        .params
        .iter()
        .position(|parameter| parameter.kind == crate::change_signature::ParamKind::KwArgs)
        .unwrap_or(model.params.len());
    model.params.insert(at, parameter);
    let (mut result, summary) =
        crate::change_signature::change_signature(db, file, &model, project_files)?;
    let replacement = (expression_range, parameter_name.to_string());
    if let Some((_, edits)) = result
        .changes
        .iter_mut()
        .find(|(path, _)| path == &file.path)
    {
        if edits.iter().any(|(range, _)| {
            !range.is_empty()
                && range
                    .intersect(expression_range)
                    .is_some_and(|intersection| !intersection.is_empty())
        }) {
            return Err("The extracted expression overlaps a recursive call rewrite; select a smaller expression".into());
        }
        edits.push(replacement);
        edits.sort_by_key(|(range, _)| (range.start(), range.end()));
    } else {
        result.changes.push((file.path.clone(), vec![replacement]));
    }
    result.title = format!("Extract parameter '{parameter_name}'");
    Ok((result, summary))
}

/// Extract Superclass plus the existing import-aware Move Symbol transaction.
/// The intermediate source exists only in an isolated analysis database; edits
/// returned to the UI always refer to the caller's original snapshots.
pub fn extract_superclass_to_module(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    name: &str,
    members: &[String],
    destination: Option<&Path>,
    project: crate::ProjectEditContext<'_>,
) -> Result<Vec<FileOp>, String> {
    let crate::ProjectEditContext {
        root,
        files: project_files,
    } = project;
    let local = crate::refactor2::extract_superclass(db, file, offset, name, members)?;
    let Some(destination) = destination else {
        return Ok(vec![FileOp::Edit(file.path.clone(), local.edits)]);
    };
    move_extracted(db, file, local, name, destination, root, project_files)
}

pub fn extract_callable_to_module(
    db: &Analysis,
    file: &Arc<FileInfo>,
    range: TextRange,
    name: &str,
    options: &CallableOptions,
    destination: &Path,
    project: crate::ProjectEditContext<'_>,
) -> Result<Vec<FileOp>, String> {
    let crate::ProjectEditContext {
        root,
        files: project_files,
    } = project;
    let options = CallableOptions {
        destination: CallableDestination::Function,
        ..options.clone()
    };
    let local = extract_callable(db, file, range, name, &options)?;
    move_extracted(db, file, local, name, destination, root, project_files)
}
fn move_extracted(
    db: &Analysis,
    file: &Arc<FileInfo>,
    local: Refactoring,
    name: &str,
    destination: &Path,
    root: &Path,
    project_files: &[PathBuf],
) -> Result<Vec<FileOp>, String> {
    let updated = crate::refactor::apply_edits(&file.text, &local.edits);
    let probe = Analysis::for_project(root);
    for path in project_files {
        if let Some(source) = db.file_for_path(path) {
            probe.set_file_text(path, &source.text);
        } else if let Ok(text) = std::fs::read_to_string(path) {
            probe.set_file_text(path, &text);
        }
    }
    probe.set_file_text(&file.path, &updated);
    let source = probe
        .file_for_path(&file.path)
        .ok_or("Cannot capture extracted superclass")?;
    let declaration = source
        .parse
        .syntax_node()
        .children()
        .find(|node| {
            ast::ClassDef::cast(node.clone()).is_some_and(|class| class.name_text() == name)
                || ast::FuncDef::cast(node.clone())
                    .is_some_and(|function| function.name_text() == name)
        })
        .ok_or("Extracted declaration must be top-level to move to a module")?;
    let operations = crate::refactor2::move_symbol(
        &probe,
        &source,
        declaration.text_range().start(),
        destination,
        root,
        project_files,
    )?;
    let mut final_source = updated;
    let mut result = Vec::new();
    for operation in operations {
        match operation {
            FileOp::Edit(path, edits) if path == file.path => {
                final_source = crate::refactor::apply_edits(&final_source, &edits);
            }
            other => result.push(other),
        }
    }
    result.insert(
        0,
        FileOp::Edit(
            file.path.clone(),
            vec![(
                TextRange::new(TextSize::new(0), TextSize::new(file.text.len() as u32)),
                final_source,
            )],
        ),
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_semantic::edit_helpers::apply_edits;
    fn selected(source: &str, text: &str) -> TextRange {
        let start = source.find(text).unwrap();
        rng(start, start + text.len())
    }
    fn project(source: &str) -> (Analysis, Arc<FileInfo>) {
        let database = Analysis::for_project(Path::new("/project"));
        database.set_file_text(Path::new("/project/main.py"), source);
        let file = database
            .file_for_path(Path::new("/project/main.py"))
            .unwrap();
        (database, file)
    }
    #[test]
    fn tuple_outputs_inputs_and_exact_duplicates_use_the_actual_lexical_binding_plan() {
        let source = "def run(value):\n    left = value + 1\n    right = value + 2\n    print(left, right)\n    left = value + 1\n    right = value + 2\n    return left, right\n";
        let (database, file) = project(source);
        let selection = selected(source, "left = value + 1\n    right = value + 2");
        let plan = extract_callable(
            &database,
            &file,
            selection,
            "split",
            &CallableOptions {
                replace_duplicates: true,
                ..Default::default()
            },
        )
        .unwrap();
        let result = apply_edits(source, &plan.edits);
        assert_eq!(result.matches("left, right = split(value)").count(), 2);
        assert!(result.contains("def split(value):"));
        assert!(result.contains("return left, right"));
        assert!(rustpy_syntax::parse(&result).errors().is_empty());
    }
    #[test]
    fn final_return_can_be_extracted_but_early_return_and_generator_escape_are_conflicts() {
        let source = "def run(value):\n    result = value + 1\n    return result\n";
        let (database, file) = project(source);
        let plan = extract_callable(
            &database,
            &file,
            selected(source, "result = value + 1\n    return result"),
            "calculate",
            &Default::default(),
        )
        .unwrap();
        let result = apply_edits(source, &plan.edits);
        assert!(result.contains("return calculate(value)"));
        assert!(result.contains("def calculate(value):\n    result = value + 1\n    return result"));
        for (source, selection, reason) in [
            (
                "def run(value):\n    if value:\n        return 1\n    print(value)\n",
                "if value:\n        return 1",
                "return",
            ),
            (
                "async def run(value):\n    yield value\n",
                "yield value",
                "async generator",
            ),
            (
                "def run(value):\n    while value:\n        break\n",
                "break",
                "break",
            ),
            (
                "def run():\n    global value\n    value = 1\n",
                "global value\n    value = 1",
                "global",
            ),
        ] {
            let (database, file) = project(source);
            assert!(extract_callable(
                &database,
                &file,
                selected(source, selection),
                "helper",
                &Default::default()
            )
            .unwrap_err()
            .contains(reason));
        }
    }
    #[test]
    fn async_static_class_and_top_level_destinations_preserve_receivers_and_await() {
        let source = "class Client:\n    async def run(self, value):\n        result = await self.fetch(value)\n        print(result)\n";
        let (database, file) = project(source);
        let range = selected(source, "result = await self.fetch(value)");
        let method =
            extract_callable(&database, &file, range, "fetch_result", &Default::default()).unwrap();
        let result = apply_edits(source, &method.edits);
        assert!(result.contains("result = await self.fetch_result(value)"));
        assert!(result.contains("async def fetch_result(self, value):"));
        let function = extract_callable(
            &database,
            &file,
            range,
            "fetch_result",
            &CallableOptions {
                destination: CallableDestination::Function,
                replace_duplicates: false,
            },
        )
        .unwrap();
        let result = apply_edits(source, &function.edits);
        assert!(result.contains("result = await fetch_result(self, value)"));
        assert!(result.contains("\nasync def fetch_result(self, value):"));
        let source = "class Client:\n    @classmethod\n    def run(cls, value):\n        result = cls(value)\n        print(result)\n";
        let (database, file) = project(source);
        let plan = extract_callable(
            &database,
            &file,
            selected(source, "result = cls(value)"),
            "make",
            &Default::default(),
        )
        .unwrap();
        let result = apply_edits(source, &plan.edits);
        assert!(result.contains("@classmethod\n    def make(cls, value):"));
        assert!(result.contains("result = cls.make(value)"));
        let source = "class Client:\n    @staticmethod\n    def run(value):\n        result = value + 1\n        print(result)\n";
        let (database, file) = project(source);
        let plan = extract_callable(
            &database,
            &file,
            selected(source, "result = value + 1"),
            "calculate",
            &Default::default(),
        )
        .unwrap();
        let result = apply_edits(source, &plan.edits);
        assert!(result.contains("@staticmethod\n    def calculate(value):"));
        assert!(result.contains("result = Client.calculate(value)"));
    }
    #[test]
    fn field_current_method_choice_does_not_move_evaluation_to_constructor() {
        let source = "class C:\n    def __init__(self):\n        pass\n    def run(self):\n        return 10\n";
        let (database, file) = project(source);
        let range = selected(source, "10");
        let init = crate::refactor::introduce_field_at(&database, &file, range, false).unwrap();
        let current = crate::refactor::introduce_field_at(&database, &file, range, true).unwrap();
        let initial = apply_edits(source, &init.edits);
        let local = apply_edits(source, &current.edits);
        assert!(initial.find("self.number = 10").unwrap() < initial.find("def run").unwrap());
        assert!(local.find("self.number = 10").unwrap() > local.find("def run").unwrap());
    }
    #[test]
    fn parameter_extraction_updates_imported_callers_with_independent_call_value_and_default() {
        let (database, file) = project("def run():\n    return 10\n");
        let caller = PathBuf::from("/project/caller.py");
        database.set_file_text(&caller, "from main import run\nrun()\n");
        let (plan, summary) = extract_parameter(
            &database,
            &file,
            selected(&file.text, "10"),
            Some("amount"),
            Some("20"),
            Some("30"),
            &[file.path.clone(), caller.clone()],
        )
        .unwrap();
        let current = apply_edits(
            &file.text,
            &plan
                .changes
                .iter()
                .find(|(path, _)| path == &file.path)
                .unwrap()
                .1,
        );
        assert!(current.contains("def run(*, amount=20):"));
        assert!(current.contains("return amount"));
        let edits = &plan
            .changes
            .iter()
            .find(|(path, _)| path == &caller)
            .unwrap()
            .1;
        assert!(apply_edits("from main import run\nrun()\n", edits).contains("run(amount=30)"));
        assert_eq!(summary.updated_calls, 1);
    }
    #[test]
    fn superclass_can_be_extracted_then_moved_using_one_original_source_transaction() {
        let source = "class Child:\n    def shared(self):\n        return 1\n    def own(self):\n        return 2\n";
        let (database, file) = project(source);
        let destination = Path::new("/project/base.py");
        let operations = extract_superclass_to_module(
            &database,
            &file,
            TextSize::new(6),
            "Base",
            &["shared".into()],
            Some(destination),
            crate::ProjectEditContext {
                root: Path::new("/project"),
                files: std::slice::from_ref(&file.path),
            },
        )
        .unwrap();
        let edited = operations
            .iter()
            .find_map(|operation| match operation {
                FileOp::Edit(path, edits) if path == &file.path => Some(apply_edits(source, edits)),
                _ => None,
            })
            .unwrap();
        assert!(edited.contains("class Child(Base):"));
        assert!(!edited.contains("def shared"));
        assert!(operations.iter().any(|operation|matches!(operation,FileOp::Create(path,text) if path==destination && text.contains("class Base:") && text.contains("def shared"))));
    }
    #[test]
    fn synchronous_generator_extraction_delegates_send_and_return_outputs() {
        let source = "def run(value):\n    received = yield value\n    print(received)\n";
        let (database, file) = project(source);
        let plan = extract_callable(
            &database,
            &file,
            selected(source, "received = yield value"),
            "delegate",
            &Default::default(),
        )
        .unwrap();
        let result = apply_edits(source, &plan.edits);
        assert!(result.contains("received = (yield from delegate(value))"));
        assert!(result
            .contains("def delegate(value):\n    received = yield value\n    return received"));
        assert!(rustpy_syntax::parse(&result).errors().is_empty());
    }
    #[test]
    fn extracted_function_can_move_to_new_module_with_original_caller_import() {
        let source = "def run(value):\n    result = value + 1\n    print(result)\n";
        let (database, file) = project(source);
        let destination = Path::new("/project/calculation.py");
        let operations = extract_callable_to_module(
            &database,
            &file,
            selected(source, "result = value + 1"),
            "calculate",
            &Default::default(),
            destination,
            crate::ProjectEditContext {
                root: Path::new("/project"),
                files: std::slice::from_ref(&file.path),
            },
        )
        .unwrap();
        let edited = operations
            .iter()
            .find_map(|operation| match operation {
                FileOp::Edit(path, edits) if path == &file.path => Some(apply_edits(source, edits)),
                _ => None,
            })
            .unwrap();
        assert!(edited.contains("result = calculate(value)"));
        assert!(!edited.contains("def calculate"));
        assert!(operations.iter().any(|operation|matches!(operation,FileOp::Create(path,text) if path==destination && text.contains("def calculate(value):") && text.contains("return result"))));
        assert!(rustpy_syntax::parse(&edited).errors().is_empty());
    }
}
