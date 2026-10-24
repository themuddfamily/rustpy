//! Static template render contexts. No application imports or rendering execute.
use crate::{
    db::{Analysis, FileInfo},
    scope::BindingKind,
};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind,
};
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};

fn text(expr: ast::Expr) -> Option<String> {
    match expr.unparen() {
        ast::Expr::String(s) => Some(s.value()),
        _ => None,
    }
}
fn imported(
    file: &FileInfo,
    at: rustpy_core::TextSize,
    expr: &ast::Expr,
) -> Option<(String, String, String)> {
    let mut parts = Vec::new();
    let mut e = expr.clone().unparen();
    while let ast::Expr::Attribute(a) = e {
        parts.push(a.attr_text().to_string());
        e = a.object()?.unparen();
    }
    let ast::Expr::Name(n) = e else { return None };
    let root = n.name_text();
    let ids = file.scopes.resolve(file.scopes.scope_at(at), &root)?;
    if ids.len() != 1 {
        return None;
    }
    let b = file.scopes.binding(ids[0]);
    if b.range.start() > at {
        return None;
    }
    let BindingKind::Import(i) = &b.kind else {
        return None;
    };
    if i.level != 0 {
        return None;
    }
    let mut path = i.name.clone().unwrap_or_default().to_string();
    for p in parts.into_iter().rev() {
        if !path.is_empty() {
            path.push('.')
        }
        path.push_str(&p)
    }
    Some((i.module.to_string(), path, root.to_string()))
}
fn trusted(db: &Analysis, module: &str) -> bool {
    db.module(module).is_none_or(|f| {
        f.path.starts_with("<typeshed>") || !db.resolver.roots.iter().any(|r| f.path.starts_with(r))
    })
}
fn retired(file: &FileInfo, root: &str) -> bool {
    for (count, node) in file.parse.syntax_node().descendants().enumerate() {
        if count > 16384 {
            return true;
        }
        if !matches!(
            node.kind(),
            SyntaxKind::ASSIGN_STMT
                | SyntaxKind::ANN_ASSIGN_STMT
                | SyntaxKind::AUG_ASSIGN_STMT
                | SyntaxKind::DEL_STMT
        ) {
            continue;
        }
        let targets: Vec<ast::Expr> = match node.kind() {
            SyntaxKind::ASSIGN_STMT => ast::AssignStmt::cast(node.clone()).unwrap().targets(),
            SyntaxKind::ANN_ASSIGN_STMT => ast::AnnAssignStmt::cast(node.clone())
                .unwrap()
                .target()
                .into_iter()
                .collect(),
            SyntaxKind::AUG_ASSIGN_STMT => ast::AugAssignStmt::cast(node.clone())
                .unwrap()
                .target()
                .into_iter()
                .collect(),
            _ => ast::DelStmt::cast(node.clone()).unwrap().targets(),
        };
        for target in targets {
            for (count, n) in target.syntax().descendants().take(4097).enumerate() {
                if count == 4096 {
                    return true;
                }
                if let Some(a) = ast::AttributeExpr::cast(n) {
                    if let Some(ast::Expr::Name(n)) = a.object().map(ast::Expr::unparen) {
                        if n.name_text() == root {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}
/// Canonical source-backed imported call identity for static framework tools.
pub fn canonical_import(
    db: &Analysis,
    file: &FileInfo,
    expression: &ast::Expr,
) -> Option<(String, String)> {
    let (module, name, root) =
        imported(file, expression.syntax().text_range().start(), expression)?;
    if !trusted(db, &module) || retired(file, &root) {
        return None;
    }
    // Refactoring must not rewrite a call after any explicit member-store
    // route replaced the imported framework API, including loop/with targets.
    let mut remaining = 16384usize;
    for node in file.parse.syntax_node().descendants() {
        remaining = remaining.checked_sub(1)?;
        let targets: Vec<ast::Expr> = match node.kind() {
            SyntaxKind::FOR_STMT => ast::ForStmt::cast(node.clone())?
                .target()
                .into_iter()
                .collect(),
            SyntaxKind::WITH_ITEM => ast::WithItem::cast(node.clone())?
                .target()
                .into_iter()
                .collect(),
            SyntaxKind::COMP_FOR => ast::CompFor::cast(node.clone())?
                .target()
                .into_iter()
                .collect(),
            _ => continue,
        };
        for target in targets {
            for descendant in target.syntax().descendants() {
                remaining = remaining.checked_sub(1)?;
                if let Some(attribute) = ast::AttributeExpr::cast(descendant) {
                    let mut owner = attribute.object()?.unparen();
                    while let ast::Expr::Attribute(attribute) = owner {
                        owner = attribute.object()?.unparen();
                    }
                    if matches!(owner, ast::Expr::Name(ref name) if name.name_text() == root) {
                        return None;
                    }
                }
            }
        }
    }
    Some((module, name))
}

fn dictionary(
    file: &FileInfo,
    at: rustpy_core::TextSize,
    expr: ast::Expr,
    depth: usize,
    out: &mut dyn FnMut(String, Option<ast::Expr>),
) {
    if depth > 8 {
        return;
    }
    match expr.unparen() {
        ast::Expr::Dict(d) => {
            for pair in d.items().take(256) {
                if let Some(name) = pair.key().and_then(text) {
                    if name.chars().next().is_some_and(rustpy_core::is_ident_start)
                        && name.chars().all(rustpy_core::is_ident_continue)
                    {
                        out(name, pair.value());
                    }
                }
            }
        }
        ast::Expr::Name(n) => {
            if let Some(ids) = file
                .scopes
                .resolve(file.scopes.scope_at(at), &n.name_text())
            {
                if ids.len() == 1 {
                    let b = file.scopes.binding(ids[0]);
                    if b.range.start() < at {
                        if let (BindingKind::Assignment, Some(range)) = (&b.kind, b.value) {
                            if let Some(expr) =
                                file.parse.syntax_node().descendants().find_map(|n| {
                                    ast::Expr::cast(n).filter(|e| e.text_range() == range)
                                })
                            {
                                dictionary(file, at, expr, depth + 1, out)
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}
fn assigned(file: &FileInfo, at: rustpy_core::TextSize, expr: ast::Expr) -> Option<ast::Expr> {
    if let ast::Expr::Name(n) = expr.clone().unparen() {
        let ids = file
            .scopes
            .resolve(file.scopes.scope_at(at), &n.name_text())?;
        if ids.len() != 1 {
            return None;
        }
        let b = file.scopes.binding(ids[0]);
        if b.range.start() > at {
            return None;
        }
        let (BindingKind::Assignment, Some(range)) = (&b.kind, b.value) else {
            return None;
        };
        file.parse
            .syntax_node()
            .descendants()
            .find_map(|n| ast::Expr::cast(n).filter(|e| e.text_range() == range))
    } else {
        Some(expr)
    }
}
fn jinja_template(
    db: &Analysis,
    file: &FileInfo,
    at: rustpy_core::TextSize,
    expr: ast::Expr,
) -> Option<String> {
    let ast::Expr::Call(get) = assigned(file, at, expr)?.unparen() else {
        return None;
    };
    let ast::Expr::Attribute(member) = get.callee()?.unparen() else {
        return None;
    };
    if member.attr_text() != "get_template" {
        return None;
    }
    let object = member.object()?;
    let root = match object.clone().unparen() {
        ast::Expr::Name(n) => Some(n.name_text()),
        _ => None,
    };
    if root.as_ref().is_some_and(|r| retired(file, r)) {
        return None;
    }
    let ast::Expr::Call(environment) = assigned(file, at, object)?.unparen() else {
        return None;
    };
    let (module, name, root) = imported(file, at, &environment.callee()?)?;
    if module != "jinja2" || name != "Environment" || !trusted(db, &module) || retired(file, &root)
    {
        return None;
    }
    get.arg_list()?.positional().next().and_then(text)
}
/// Names from actual render calls whose literal template resolves to this file.
pub fn names(db: &Analysis, files: &[std::path::PathBuf], template: &Path) -> Vec<String> {
    let mut result = BTreeSet::new();
    collect(db, files, template, &mut |name, _, _, _| {
        result.insert(name);
    });
    result.into_iter().collect()
}
/// Render-context entries: name, passed value expression (when literal),
/// its file, and whether the render call is Django's (methods auto-called).
type Sink<'s> = dyn FnMut(String, Option<ast::Expr>, &std::sync::Arc<FileInfo>, bool) + 's;
fn collect(db: &Analysis, files: &[std::path::PathBuf], template: &Path, sink: &mut Sink<'_>) {
    let mut bytes = 0usize;
    for path in files.iter().take(256) {
        let Some(file) = db.file_for_path(path) else {
            continue;
        };
        bytes += file.text.len();
        if bytes > 8 * 1024 * 1024 {
            break;
        }
        let mut retired_roots = HashMap::new();
        for node in file.parse.syntax_node().descendants().take(16384) {
            let Some(call) = ast::CallExpr::cast(node) else {
                continue;
            };
            let Some(callee) = call.callee() else {
                continue;
            };
            let at = call.text_range().start();
            if let ast::Expr::Attribute(member) = callee.clone().unparen() {
                if member.attr_text() == "render" {
                    if let Some(name) = member
                        .object()
                        .and_then(|object| jinja_template(db, &file, at, object))
                    {
                        if template.ends_with(Path::new(&name)) {
                            if let Some(args) = call.arg_list() {
                                for value in args.positional().take(32) {
                                    dictionary(
                                        &file,
                                        at,
                                        value,
                                        0,
                                        &mut |name: String, value: Option<ast::Expr>| {
                                            sink(name, value, &file, false)
                                        },
                                    )
                                }
                                for keyword in args.keywords().take(256) {
                                    if let Some(name) = keyword.name() {
                                        sink(
                                            name.text().to_string(),
                                            keyword.value(),
                                            &file,
                                            false,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let Some((module, name, root)) = imported(&file, at, &callee) else {
                continue;
            };
            if !trusted(db, &module)
                || *retired_roots
                    .entry(root.clone())
                    .or_insert_with(|| retired(&file, &root))
            {
                continue;
            }
            let request_render = module == "django.shortcuts" && name == "render";
            let django =
                request_render || module == "django.template.loader" && name == "render_to_string";
            let flask = matches!(module.as_str(), "flask" | "flask.templating")
                && name == "render_template";
            let jinja = module == "jinja2" && name == "Environment";
            if !django && !flask && !jinja {
                continue;
            }
            let Some(args) = call.arg_list() else {
                continue;
            };
            let positional: Vec<_> = args.positional().take(33).collect();
            if positional.len() > 32 {
                continue;
            }
            if jinja {
                continue;
            } // Environment.get_template is resolved separately below.
            let index = usize::from(request_render);
            let template_name = positional.get(index).cloned().and_then(text).or_else(|| {
                args.keywords()
                    .find(|k| {
                        k.name().is_some_and(|n| {
                            matches!(n.text(), "template_name" | "template_name_or_list")
                        })
                    })
                    .and_then(|k| k.value())
                    .and_then(text)
            });
            let Some(name) = template_name else { continue };
            if !template.ends_with(Path::new(&name)) {
                continue;
            }
            let mut entry =
                |name: String, value: Option<ast::Expr>| sink(name, value, &file, django);
            if django {
                if let Some(context) = positional.get(index + 1) {
                    dictionary(&file, at, context.clone(), 0, &mut entry)
                }
            }
            for keyword in args.keywords().take(256) {
                let Some(key) = keyword.name() else { continue };
                if django {
                    if key.text() == "context" {
                        if let Some(v) = keyword.value() {
                            dictionary(&file, at, v, 0, &mut entry)
                        }
                    }
                } else if !matches!(key.text(), "template_name_or_list") {
                    entry(key.text().to_string(), keyword.value());
                }
            }
            if flask {
                for arg in args.args().take(256) {
                    if let ast::AnyArg::StarStar(star) = arg {
                        if let Some(value) = ast::AnyArg::StarStar(star).value() {
                            dictionary(&file, at, value, 0, &mut entry)
                        }
                    }
                }
            }
        }
    }
}

/// One step from a render-context value towards the completed object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Attribute(String),
    /// The element of an iterable (a `{% for %}` loop variable).
    Iterate,
}
fn dotted(text: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = text.split('.').map(str::to_string).collect();
    (parts.len() <= 16
        && parts.iter().all(|part| {
            part.starts_with(rustpy_core::is_ident_start)
                && part.chars().all(rustpy_core::is_ident_continue)
        }))
    .then_some(parts)
}
/// The dotted template expression completed at `caret` (`{{ obj.fi|` or
/// `{% for x in items %}{{ x.|`): `(member prefix start, root, steps)`.
/// Simple loop variables of enclosing `{% for %}` tags are rewritten to the
/// elements of their dotted iterable, so the root is a render-context name.
pub fn member_path(source: &str, caret: usize) -> Option<(usize, String, Vec<Step>)> {
    let before = source.get(..caret)?;
    let open = match (before.rfind("{{"), before.rfind("{%")) {
        (Some(a), Some(b)) => a.max(b),
        (a, b) => a.or(b)?,
    };
    if before[open..].contains("}}") || before[open..].contains("%}") {
        return None;
    }
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| rustpy_core::is_ident_continue(*c))
        .last()
        .map_or(caret, |(at, _)| at);
    let object = before[..start].strip_suffix('.')?;
    let chain_start = object
        .char_indices()
        .rev()
        .take_while(|(_, c)| rustpy_core::is_ident_continue(*c) || *c == '.')
        .last()
        .map_or(object.len(), |(at, _)| at);
    // Call/subscript results and literals are not statically traced.
    if object[..chain_start].ends_with([')', ']', '\'', '"']) {
        return None;
    }
    let mut parts = dotted(&object[chain_start..])?;
    let mut steps: Vec<Step> = parts.drain(1..).map(Step::Attribute).collect();
    let mut root = parts.pop()?;
    // Enclosing loops, outermost first: (targets, iterable source).
    let mut loops: Vec<(Vec<String>, String)> = Vec::new();
    for segment in before[..open].split("{%").skip(1).take(4096) {
        let Some(end) = segment.find("%}") else {
            continue;
        };
        let body = segment[..end].trim_matches(['-', '+']).trim();
        let mut words = body.split_whitespace();
        match words.next() {
            Some("endfor") => {
                loops.pop();
            }
            Some("for") => {
                let rest = &body[3..];
                let Some(at) = rest.find(" in ") else {
                    loops.push((Vec::new(), String::new()));
                    continue;
                };
                let targets = rest[..at]
                    .split(',')
                    .map(|target| target.trim().to_string())
                    .collect();
                let iterable = rest[at + 4..]
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string();
                loops.push((targets, iterable));
            }
            _ => {}
        }
    }
    let mut index = loops.len();
    while index > 0 {
        index -= 1;
        let (targets, iterable) = &loops[index];
        if targets.len() != 1 || targets[0] != root {
            continue;
        }
        let mut parts = dotted(iterable)?;
        let mut prefix: Vec<Step> = parts.drain(1..).map(Step::Attribute).collect();
        prefix.push(Step::Iterate);
        prefix.extend(steps);
        steps = prefix;
        root = parts.pop()?;
    }
    if root == "loop" {
        return None;
    }
    Some((start, root, steps))
}

/// Members of the value a view passes as render-context `root`, after
/// `steps`, inferred statically from the render call's value expression.
pub fn members(
    db: &Analysis,
    files: &[std::path::PathBuf],
    template: &Path,
    root: &str,
    steps: &[Step],
) -> Vec<crate::features::CompletionItem> {
    members_with(
        db,
        files,
        template,
        root,
        steps,
        crate::infer::Inferer::new(db),
    )
}

/// [`members`] for interactive lookups: inference stops after `budget` or once
/// `token` is cancelled.
pub fn members_bounded(
    db: &Analysis,
    files: &[std::path::PathBuf],
    template: &Path,
    root: &str,
    steps: &[Step],
    budget: std::time::Duration,
    token: Option<rustpy_core::CancellationToken>,
) -> Vec<crate::features::CompletionItem> {
    let inf = crate::infer::Inferer::new(db).with_budget(std::time::Instant::now() + budget, token);
    members_with(db, files, template, root, steps, inf)
}

fn members_with(
    db: &Analysis,
    files: &[std::path::PathBuf],
    template: &Path,
    root: &str,
    steps: &[Step],
    inf: crate::infer::Inferer<'_>,
) -> Vec<crate::features::CompletionItem> {
    let mut out: Vec<crate::features::CompletionItem> = Vec::new();
    collect(db, files, template, &mut |name, value, file, django| {
        if name != root || out.len() >= 512 {
            return;
        }
        let Some(value) = value else { return };
        let mut ty = inf.type_of_expr(file, &value);
        for step in steps {
            ty = match step {
                Step::Attribute(name) => inf
                    .lookup_member(&ty, name)
                    .unwrap_or(crate::types::Type::Unknown),
                Step::Iterate => inf.iter_type(&ty),
            };
        }
        if matches!(ty, crate::types::Type::Unknown | crate::types::Type::Any) {
            return;
        }
        for (name, member) in inf.all_members(&ty).into_iter().take(1024) {
            if name.starts_with('_') || out.iter().any(|item| item.label == name) {
                continue;
            }
            let mut item = crate::features::member_item(&inf, &ty, &name, &member);
            if django {
                // Django templates call methods implicitly and take no arguments.
                item.insert = name.to_string();
                item.caret_offset = None;
            }
            out.push(item);
        }
    });
    out
}

/// Physical render/view links for Related Symbol. Uses the same canonical
/// import and retirement authority as template context inference; no code runs.
pub fn related_locations(
    db: &Analysis,
    file: &std::sync::Arc<FileInfo>,
    offset: rustpy_core::TextSize,
    files: &[std::path::PathBuf],
    templates: &[std::path::PathBuf],
) -> Vec<crate::features::Location> {
    fn render_name(db: &Analysis, file: &FileInfo, call: &ast::CallExpr) -> Option<String> {
        let callee = call.callee()?;
        let at = call.text_range().start();
        if let ast::Expr::Attribute(member) = callee.clone().unparen() {
            if member.attr_text() == "render" {
                if let Some(name) = member
                    .object()
                    .and_then(|object| jinja_template(db, file, at, object))
                {
                    return Some(name);
                }
            }
        }
        let (module, name, root) = imported(file, at, &callee)?;
        if !trusted(db, &module) || retired(file, &root) {
            return None;
        }
        let index = if module == "django.shortcuts" && name == "render" {
            1
        } else if (module == "django.template.loader" && name == "render_to_string")
            || (matches!(module.as_str(), "flask" | "flask.templating")
                && name == "render_template")
        {
            0
        } else {
            return None;
        };
        let args = call.arg_list()?;
        args.positional().nth(index).and_then(text).or_else(|| {
            args.keywords()
                .find(|argument| {
                    argument.name().is_some_and(|name| {
                        matches!(name.text(), "template_name" | "template_name_or_list")
                    })
                })
                .and_then(|argument| argument.value())
                .and_then(text)
        })
    }
    fn sites(
        db: &Analysis,
        file: &FileInfo,
    ) -> Vec<(String, rustpy_core::TextRange, rustpy_core::TextRange)> {
        let mut result = Vec::new();
        for node in file.parse.syntax_node().descendants().take(16384) {
            let Some(call) = ast::CallExpr::cast(node) else {
                continue;
            };
            let Some(name) = render_name(db, file, &call) else {
                continue;
            };
            let owner = call.syntax().ancestors().find_map(ast::FuncDef::cast);
            let range = owner
                .as_ref()
                .and_then(|function| function.name())
                .map(|token| token.text_range())
                .unwrap_or(call.text_range());
            let body = owner
                .map(|function| function.text_range())
                .unwrap_or(call.text_range());
            result.push((name, range, body));
        }
        for node in file.parse.syntax_node().descendants().take(16384) {
            let Some(class) = ast::ClassDef::cast(node) else {
                continue;
            };
            let known = class.bases().iter().any(|base| {
                imported(file, class.text_range().start(), base).is_some_and(
                    |(module, name, root)| {
                        module.starts_with("django.views.generic")
                            && matches!(
                                name.as_str(),
                                "TemplateView"
                                    | "DetailView"
                                    | "ListView"
                                    | "CreateView"
                                    | "UpdateView"
                                    | "DeleteView"
                                    | "FormView"
                            )
                            && trusted(db, &module)
                            && !retired(file, &root)
                    },
                )
            });
            if !known {
                continue;
            }
            let Some(body) = class.body() else {
                continue;
            };
            for statement in body.statements() {
                let value=match statement {
                    ast::Stmt::Assign(assign) if assign.targets().iter().any(|target|matches!(target,ast::Expr::Name(name) if name.name_text()=="template_name"))=>assign.value(),
                    ast::Stmt::AnnAssign(assign) if assign.target().is_some_and(|target|matches!(target,ast::Expr::Name(name) if name.name_text()=="template_name"))=>assign.value(),
                    _=>None,
                };
                let Some(name) = value.and_then(text) else {
                    continue;
                };
                if let Some(token) = class.name() {
                    result.push((name, token.text_range(), class.text_range()));
                }
            }
        }
        result
    }
    let mut result = Vec::new();
    if file
        .path
        .extension()
        .is_some_and(|extension| extension == "py" || extension == "pyi")
    {
        let scope = file
            .parse
            .syntax_node()
            .token_at_offset(offset.min(file.parse.syntax_node().text_range().end()))
            .right_biased()
            .and_then(|token| {
                token.parent_ancestors().find(|node| {
                    matches!(node.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF)
                })
            })
            .map(|node| node.text_range());
        for (name, _, body) in sites(db, file) {
            if scope.is_some_and(|scope| scope != body) {
                continue;
            }
            for path in templates
                .iter()
                .take(4096)
                .filter(|path| path.ends_with(Path::new(&name)))
            {
                let location = crate::features::Location {
                    path: path.clone(),
                    range: rustpy_core::TextRange::empty(0.into()),
                    is_stub: false,
                };
                if !result.contains(&location) {
                    result.push(location);
                }
            }
        }
    } else {
        let mut bytes = 0usize;
        for path in files
            .iter()
            .filter(|path| path.extension().is_some_and(|extension| extension == "py"))
            .take(256)
        {
            let Some(source) = db.file_for_path(path) else {
                continue;
            };
            bytes += source.text.len();
            if bytes > 8 * 1024 * 1024 {
                break;
            }
            for (name, range, _) in sites(db, &source) {
                if file.path.ends_with(Path::new(&name)) {
                    let location = crate::features::Location {
                        path: source.path.clone(),
                        range,
                        is_stub: source.is_stub,
                    };
                    if !result.contains(&location) {
                        result.push(location);
                    }
                }
            }
        }
    }
    result
}
