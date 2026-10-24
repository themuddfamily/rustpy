//! Cross-file route tables: Django URLconfs (with recursive `include()`
//! expansion) and Pyramid `add_route` + `view_config` / `add_view`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rustpy_syntax::ast::{self, AstNode};

use super::Endpoint;

/// 0-based line lookup for byte offsets.
struct Lines(Vec<usize>);

impl Lines {
    fn new(text: &str) -> Self {
        Self(
            text.bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i)
                .collect(),
        )
    }
    fn of(&self, node: &rustpy_syntax::SyntaxNode) -> usize {
        self.at(usize::from(node.text_range().start()))
    }
    fn at(&self, offset: usize) -> usize {
        self.0.partition_point(|&n| n < offset)
    }
}

fn callee_name(call: &ast::CallExpr) -> String {
    call.callee()
        .map(|c| c.syntax().text().to_string())
        .map(|c| c.rsplit('.').next().unwrap_or("").to_string())
        .unwrap_or_default()
}

fn keyword(call: &ast::CallExpr, name: &str) -> Option<ast::Expr> {
    call.arg_list()?
        .keywords()
        .find(|k| k.name_text() == name)?
        .value()
}

fn positional(call: &ast::CallExpr) -> Vec<ast::Expr> {
    call.arg_list()
        .map(|l| l.positional().take(16).collect())
        .unwrap_or_default()
}

fn string(expr: &ast::Expr) -> Option<String> {
    match expr {
        ast::Expr::String(s) => Some(s.value()),
        ast::Expr::Paren(_) => expr
            .syntax()
            .children()
            .find_map(ast::Expr::cast)
            .and_then(|e| string(&e)),
        _ => None,
    }
}

/// Upper-case method names of a `request_method=` value.
fn methods(expr: Option<ast::Expr>) -> Vec<String> {
    let Some(expr) = expr else { return Vec::new() };
    if let Some(value) = string(&expr) {
        return vec![value.to_uppercase()];
    }
    expr.syntax()
        .descendants()
        .filter_map(ast::StringExpr::cast)
        .map(|s| s.value().to_uppercase())
        .take(16)
        .collect()
}

// ---------------------------------------------------------------- Django

enum Target {
    /// Dotted module path segments (relative imports already resolved against the file).
    Module(Vec<String>),
    Patterns(Vec<Entry>),
}

enum Entry {
    Route {
        route: String,
        regex: bool,
        handler: String,
        line: usize,
    },
    Include {
        route: String,
        regex: bool,
        target: Target,
        text: String,
        line: usize,
    },
}

struct UrlConf {
    file: PathBuf,
    /// Path components without the `.py` extension (`…/blog/urls`).
    components: Vec<String>,
    root: bool,
    entries: Vec<Entry>,
}

fn is_route_call(call: &ast::CallExpr) -> bool {
    matches!(callee_name(call).as_str(), "path" | "re_path" | "url")
}

fn components(path: &Path) -> Vec<String> {
    let mut parts: Vec<String> = path
        .with_extension("")
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect();
    if parts.last().is_some_and(|p| p == "__init__") {
        parts.pop();
    }
    parts
}

fn parse_urlconf(file: &Path, text: &str) -> Option<UrlConf> {
    let is_urls = file.file_name().is_some_and(|n| n == "urls.py");
    if text.len() > 2 * 1024 * 1024 || !(is_urls || text.contains("urlpatterns")) {
        return None;
    }
    let parse = rustpy_syntax::parse(text);
    let root = parse.syntax_node();
    let lines = Lines::new(text);
    // Imported module aliases: bound name -> dotted module path.
    let package: Vec<String> = {
        let mut c = components(file);
        if !file.ends_with("__init__.py") {
            c.pop();
        }
        c
    };
    let mut imports: HashMap<String, Vec<String>> = HashMap::new();
    let mut lists: HashMap<String, ast::Expr> = HashMap::new();
    let mut top: Vec<ast::Expr> = Vec::new();
    for statement in root.children().filter_map(ast::Stmt::cast).take(4096) {
        match statement {
            ast::Stmt::ImportFrom(import) => {
                let level = import.level();
                let mut base: Vec<String> = if level > 0 {
                    package[..package.len().saturating_sub(level - 1)].to_vec()
                } else {
                    Vec::new()
                };
                if let Some(module) = import.module() {
                    base.extend(module.path().split('.').map(str::to_string));
                }
                for alias in import.aliases() {
                    if let Some(bound) = alias.bound_name() {
                        let mut path = base.clone();
                        path.push(alias.name());
                        imports.insert(bound, path);
                    }
                }
            }
            ast::Stmt::Import(import) => {
                for alias in import.aliases() {
                    let name = alias.name();
                    if let Some(a) = alias.alias() {
                        imports.insert(
                            a.text().to_string(),
                            name.split('.').map(str::to_string).collect(),
                        );
                    }
                }
            }
            ast::Stmt::Assign(assign) => {
                if let (Some(ast::Expr::Name(target)), Some(value)) =
                    (assign.targets().first().cloned(), assign.value())
                {
                    let name = target.name_text();
                    if name == "urlpatterns" {
                        top.push(value);
                    } else {
                        lists.insert(name, value);
                    }
                }
            }
            ast::Stmt::AugAssign(assign) => {
                if let (Some(ast::Expr::Name(target)), Some(value)) =
                    (assign.target(), assign.value())
                {
                    if target.name_text() == "urlpatterns" {
                        top.push(value);
                    }
                }
            }
            _ => {}
        }
    }
    if top.is_empty() {
        return None;
    }
    let context = Context {
        imports: &imports,
        lists: &lists,
        lines: &lines,
        package: &package,
    };
    let mut entries = Vec::new();
    for value in &top {
        context.entries(value, &mut entries, 0);
    }
    Some(UrlConf {
        file: file.to_path_buf(),
        components: components(file),
        root: is_urls,
        entries,
    })
}

struct Context<'a> {
    imports: &'a HashMap<String, Vec<String>>,
    lists: &'a HashMap<String, ast::Expr>,
    lines: &'a Lines,
    package: &'a [String],
}

impl Context<'_> {
    /// Route calls of a pattern-list expression that are not nested in another route call.
    fn entries(&self, value: &ast::Expr, out: &mut Vec<Entry>, depth: usize) {
        if depth > 8 {
            return;
        }
        let owner = value.syntax().clone();
        for call in value
            .syntax()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .filter(is_route_call)
            .take(4096)
        {
            let nested = call
                .syntax()
                .ancestors()
                .skip(1)
                .take_while(|n| *n != owner)
                .filter_map(ast::CallExpr::cast)
                .any(|c| is_route_call(&c));
            if nested {
                continue;
            }
            let args = positional(&call);
            let Some(route) = args
                .first()
                .cloned()
                .or_else(|| keyword(&call, "route"))
                .and_then(|r| string(&r))
            else {
                continue;
            };
            let Some(view) = args.get(1).cloned().or_else(|| keyword(&call, "view")) else {
                continue;
            };
            let regex = callee_name(&call) != "path";
            let line = self.lines.of(call.syntax());
            match &view {
                ast::Expr::Call(include) if callee_name(include) == "include" => {
                    let Some(target) = positional(include)
                        .first()
                        .and_then(|arg| self.target(arg, depth))
                    else {
                        continue;
                    };
                    out.push(Entry::Include {
                        route,
                        regex,
                        target,
                        text: include.syntax().text().to_string(),
                        line,
                    });
                }
                _ => out.push(Entry::Route {
                    route,
                    regex,
                    handler: view.syntax().text().to_string(),
                    line,
                }),
            }
        }
    }

    fn target(&self, arg: &ast::Expr, depth: usize) -> Option<Target> {
        match arg {
            // `include((patterns, 'app_name'))`
            ast::Expr::Tuple(tuple) => self.target(&tuple.elements().next()?, depth),
            ast::Expr::Paren(_) => {
                self.target(&arg.syntax().children().find_map(ast::Expr::cast)?, depth)
            }
            ast::Expr::String(s) => Some(Target::Module(
                s.value().split('.').map(str::to_string).collect(),
            )),
            ast::Expr::List(_) => {
                let mut entries = Vec::new();
                self.entries(arg, &mut entries, depth + 1);
                Some(Target::Patterns(entries))
            }
            ast::Expr::Name(name) => {
                let name = name.name_text();
                if let Some(list) = self.lists.get(&name) {
                    let mut entries = Vec::new();
                    self.entries(list, &mut entries, depth + 1);
                    return Some(Target::Patterns(entries));
                }
                Some(Target::Module(
                    self.imports.get(&name).cloned().unwrap_or_else(|| {
                        let mut path = self.package.to_vec();
                        path.push(name);
                        path
                    }),
                ))
            }
            ast::Expr::Attribute(_) => {
                let text = arg.syntax().text().to_string();
                let mut parts: Vec<String> =
                    text.split('.').map(|s| s.trim().to_string()).collect();
                if let Some(module) = self.imports.get(&parts[0]) {
                    parts.splice(0..1, module.iter().cloned());
                }
                Some(Target::Module(parts))
            }
            _ => None,
        }
    }
}

struct Piece<'a> {
    route: &'a str,
    regex: bool,
}

fn joined(pieces: &[Piece<'_>]) -> String {
    let regex = pieces.iter().any(|p| p.regex);
    let mut out = String::new();
    for (i, piece) in pieces.iter().enumerate() {
        let mut route = piece.route;
        if i > 0 {
            route = route.trim_start_matches('^');
        }
        if i + 1 < pieces.len() {
            route = route.trim_end_matches('$');
        }
        out.push_str(route);
    }
    if regex && pieces.first().is_some_and(|p| p.regex) {
        out
    } else {
        format!("/{}", out.trim_start_matches('^').trim_start_matches('/'))
    }
}

fn find_conf(confs: &[UrlConf], module: &[String]) -> Option<usize> {
    if module.is_empty() {
        return None;
    }
    confs
        .iter()
        .enumerate()
        .filter(|(_, c)| c.components.ends_with(module))
        .min_by_key(|(_, c)| c.components.len())
        .map(|(i, _)| i)
}

fn expand<'a>(
    confs: &'a [UrlConf],
    conf: usize,
    entries: &'a [Entry],
    prefix: &mut Vec<Piece<'a>>,
    stack: &mut Vec<usize>,
    out: &mut Vec<Endpoint>,
) {
    if prefix.len() > 16 || out.len() > 100_000 {
        return;
    }
    let file = &confs[conf].file;
    for entry in entries {
        match entry {
            Entry::Route {
                route,
                regex,
                handler,
                line,
            } => {
                prefix.push(Piece {
                    route,
                    regex: *regex,
                });
                out.push(Endpoint {
                    framework: "Django",
                    methods: vec!["ANY".into()],
                    path: joined(prefix),
                    handler: handler.clone(),
                    file: file.clone(),
                    line: *line,
                    params: Vec::new(),
                    body: None,
                });
                prefix.pop();
            }
            Entry::Include {
                route,
                regex,
                target,
                text,
                line,
            } => {
                prefix.push(Piece {
                    route,
                    regex: *regex,
                });
                match target {
                    Target::Patterns(nested) => expand(confs, conf, nested, prefix, stack, out),
                    Target::Module(module) => match find_conf(confs, module) {
                        // Cycle: the included URLconf is already being expanded.
                        Some(next) if stack.contains(&next) => {}
                        Some(next) => {
                            stack.push(next);
                            expand(confs, next, &confs[next].entries, prefix, stack, out);
                            stack.pop();
                        }
                        // Unresolved (third-party) URLconf: keep the mount point visible.
                        None => out.push(Endpoint {
                            framework: "Django",
                            methods: vec!["ANY".into()],
                            path: joined(prefix),
                            handler: text.clone(),
                            file: file.clone(),
                            line: *line,
                            params: Vec::new(),
                            body: None,
                        }),
                    },
                }
                prefix.pop();
            }
        }
    }
}

/// Django endpoints of all URLconfs; included URLconfs are expanded under
/// their mount prefixes instead of being listed on their own.
pub(super) fn django(files: &[(&Path, &str)]) -> Vec<Endpoint> {
    let confs: Vec<UrlConf> = files
        .iter()
        .filter_map(|(file, text)| parse_urlconf(file, text))
        .collect();
    let mut included = vec![false; confs.len()];
    fn mark(confs: &[UrlConf], entries: &[Entry], included: &mut [bool], depth: usize) {
        for entry in entries {
            if let Entry::Include { target, .. } = entry {
                match target {
                    Target::Module(module) => {
                        if let Some(i) = find_conf(confs, module) {
                            included[i] = true;
                        }
                    }
                    Target::Patterns(nested) if depth < 8 => {
                        mark(confs, nested, included, depth + 1)
                    }
                    Target::Patterns(_) => {}
                }
            }
        }
    }
    for conf in &confs {
        mark(&confs, &conf.entries, &mut included, 0);
    }
    let mut out = Vec::new();
    for (i, conf) in confs.iter().enumerate() {
        // Roots are urls.py modules nobody includes; an include cycle with no
        // outside root still gets listed from its urls.py members.
        if !conf.root || (included[i] && has_outside_root(&confs, &included, i)) {
            continue;
        }
        let mut stack = vec![i];
        expand(
            &confs,
            i,
            &conf.entries,
            &mut Vec::new(),
            &mut stack,
            &mut out,
        );
    }
    out
}

/// Whether `index` is reachable from some root that is not itself included.
fn has_outside_root(confs: &[UrlConf], included: &[bool], index: usize) -> bool {
    fn reaches(confs: &[UrlConf], entries: &[Entry], index: usize, seen: &mut Vec<usize>) -> bool {
        entries.iter().any(|entry| match entry {
            Entry::Include {
                target: Target::Module(module),
                ..
            } => find_conf(confs, module).is_some_and(|next| {
                next == index
                    || (!seen.contains(&next) && {
                        seen.push(next);
                        reaches(confs, &confs[next].entries, index, seen)
                    })
            }),
            Entry::Include {
                target: Target::Patterns(nested),
                ..
            } => reaches(confs, nested, index, seen),
            Entry::Route { .. } => false,
        })
    }
    confs.iter().enumerate().any(|(i, conf)| {
        i != index
            && conf.root
            && !included[i]
            && reaches(confs, &conf.entries, index, &mut vec![i])
    })
}

// ---------------------------------------------------------------- Pyramid

/// Pyramid routes: `config.add_route(name, pattern)` joined with the views
/// registered for that route name by `@view_config` or `config.add_view`.
pub(super) fn pyramid(files: &[(&Path, &str)]) -> Vec<Endpoint> {
    struct View {
        route: String,
        handler: String,
        methods: Vec<String>,
        file: PathBuf,
        line: usize,
    }
    let mut routes: Vec<(String, String, Vec<String>, PathBuf, usize)> = Vec::new();
    let mut views: Vec<View> = Vec::new();
    for (file, text) in files {
        if text.len() > 2 * 1024 * 1024 || !text.contains("pyramid") {
            continue;
        }
        let parse = rustpy_syntax::parse(text);
        let lines = Lines::new(text);
        for call in parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .take(8192)
        {
            let is_method = matches!(call.callee(), Some(ast::Expr::Attribute(_)));
            match callee_name(&call).as_str() {
                "add_route" if is_method => {
                    let args = positional(&call);
                    let name = args.first().cloned().or_else(|| keyword(&call, "name"));
                    let pattern = args.get(1).cloned().or_else(|| keyword(&call, "pattern"));
                    if let (Some(name), Some(pattern)) = (
                        name.and_then(|n| string(&n)),
                        pattern.and_then(|p| string(&p)),
                    ) {
                        routes.push((
                            name,
                            pattern,
                            methods(keyword(&call, "request_method")),
                            file.to_path_buf(),
                            lines.of(call.syntax()),
                        ));
                    }
                }
                "add_view" if is_method => {
                    let view = positional(&call)
                        .first()
                        .cloned()
                        .or_else(|| keyword(&call, "view"));
                    if let (Some(view), Some(route)) =
                        (view, keyword(&call, "route_name").and_then(|r| string(&r)))
                    {
                        let handler =
                            string(&view).unwrap_or_else(|| view.syntax().text().to_string());
                        views.push(View {
                            route,
                            handler,
                            methods: methods(keyword(&call, "request_method")),
                            file: file.to_path_buf(),
                            line: lines.of(call.syntax()),
                        });
                    }
                }
                "view_config" => {
                    let decorator = call
                        .syntax()
                        .parent()
                        .is_some_and(|p| ast::Decorator::can_cast(p.kind()));
                    let Some(route) = keyword(&call, "route_name")
                        .and_then(|r| string(&r))
                        .filter(|_| decorator)
                    else {
                        continue;
                    };
                    // The decorated function or class.
                    let Some(owner) = call.syntax().ancestors().find(|n| {
                        ast::FuncDef::can_cast(n.kind()) || ast::ClassDef::can_cast(n.kind())
                    }) else {
                        continue;
                    };
                    let (handler, name_line) = if let Some(f) = ast::FuncDef::cast(owner.clone()) {
                        (
                            f.name_text(),
                            f.name()
                                .map(|t| lines.at(usize::from(t.text_range().start()))),
                        )
                    } else if let Some(c) = ast::ClassDef::cast(owner) {
                        (
                            c.name_text(),
                            c.name()
                                .map(|t| lines.at(usize::from(t.text_range().start()))),
                        )
                    } else {
                        continue;
                    };
                    views.push(View {
                        route,
                        handler,
                        methods: methods(keyword(&call, "request_method")),
                        file: file.to_path_buf(),
                        line: name_line.unwrap_or_else(|| lines.of(call.syntax())),
                    });
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for (name, pattern, route_methods, file, line) in &routes {
        let path = if pattern.starts_with('/') {
            pattern.clone()
        } else {
            format!("/{pattern}")
        };
        let mut matched = false;
        for view in views.iter().filter(|v| &v.route == name) {
            matched = true;
            let methods = if !view.methods.is_empty() {
                view.methods.clone()
            } else if !route_methods.is_empty() {
                route_methods.clone()
            } else {
                vec!["ANY".into()]
            };
            out.push(Endpoint {
                framework: "Pyramid",
                methods,
                path: path.clone(),
                handler: view.handler.clone(),
                file: view.file.clone(),
                line: view.line,
                params: Vec::new(),
                body: None,
            });
        }
        if !matched {
            out.push(Endpoint {
                framework: "Pyramid",
                methods: if route_methods.is_empty() {
                    vec!["ANY".into()]
                } else {
                    route_methods.clone()
                },
                path,
                handler: format!("route '{name}'"),
                file: file.clone(),
                line: *line,
                params: Vec::new(),
                body: None,
            });
        }
    }
    out
}
