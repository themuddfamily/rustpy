//! Endpoints tool window backend: HTTP routes declared with Flask, FastAPI
//! (and Starlette-style routers), Django URLconfs or Pyramid route tables.

mod params;
mod urlconf;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub framework: &'static str,
    /// Upper-case HTTP methods ("GET", "POST", …) or "ANY"/"WS".
    pub methods: Vec<String>,
    pub path: String,
    /// Handler function (or view expression for Django).
    pub handler: String,
    pub file: PathBuf,
    /// 0-based line of the handler definition / URL pattern.
    pub line: usize,
    /// Path parameters of the route template, plus query/body parameters of
    /// FastAPI handler signatures.
    pub params: Vec<EndpointParam>,
    /// JSON request body skeleton derived from the body parameter's model.
    pub body: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamLocation {
    Path,
    Query,
    Body,
}

impl ParamLocation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Query => "query",
            Self::Body => "body",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointParam {
    pub name: String,
    pub location: ParamLocation,
    /// Annotation / converter text as written; may be empty.
    pub annotation: String,
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("valid endpoint regex"))
}

static DECORATOR: OnceLock<Regex> = OnceLock::new();
static CTOR: OnceLock<Regex> = OnceLock::new();
static METHODS: OnceLock<Regex> = OnceLock::new();
static QUOTED: OnceLock<Regex> = OnceLock::new();
static PREFIX_ARG: OnceLock<Regex> = OnceLock::new();
static DEF: OnceLock<Regex> = OnceLock::new();

/// Scans one Python source file (Django includes and Pyramid views are
/// resolved within this file only).
pub fn scan_file(file: &Path, text: &str) -> Vec<Endpoint> {
    let files = [(file, text)];
    let models = params::Models::collect(&files);
    let mut out = scan_routes(file, text, &models);
    out.extend(urlconf::django(&files));
    out.extend(urlconf::pyramid(&files));
    finish(&mut out);
    out
}

/// Fills path parameters not already derived from a handler signature.
fn finish(endpoints: &mut [Endpoint]) {
    for e in endpoints {
        let mut path = params::path_params(&e.path);
        for p in &mut path {
            if let Some(known) = e.params.iter().find(|k| k.name == p.name) {
                if !known.annotation.is_empty() {
                    p.annotation = known.annotation.clone();
                }
            }
        }
        e.params.retain(|k| !path.iter().any(|p| p.name == k.name));
        path.append(&mut e.params);
        e.params = path;
    }
}

/// Decorator and constructor routes of one file (all frameworks but the
/// cross-file Django/Pyramid tables).
fn scan_routes(file: &Path, text: &str, models: &params::Models) -> Vec<Endpoint> {
    let decorator = re(
        &DECORATOR,
        r#"^\s*@\s*([A-Za-z_][\w]*)\.(route|get|post|put|delete|patch|head|options|api_route|websocket)\(\s*(?:rule\s*=\s*|path\s*=\s*)?[rRbBuU]?(['"])(.*?)['"](.*)$"#,
    );
    let ctor = re(
        &CTOR,
        r"^\s*([A-Za-z_]\w*)\s*(?::\s*[\w\.\[\]]+\s*)?=\s*(?:[\w\.]+\.)?(Flask|Blueprint|FastAPI|APIRouter|Quart|Sanic|Starlette|Router|RouteTableDef)\((.*)$",
    );
    let methods_re = re(&METHODS, r"methods\s*=\s*[\[\(\{]([^\]\)\}]*)[\]\)\}]");
    let quoted = re(&QUOTED, r#"['"]([A-Za-z]+)['"]"#);
    let prefix_arg = re(
        &PREFIX_ARG,
        r#"(?:url_prefix|prefix)\s*=\s*['"]([^'"]*)['"]"#,
    );
    let def_re = re(&DEF, r"^\s*(?:async\s+)?def\s+([A-Za-z_]\w*)");

    let lines: Vec<&str> = text.lines().collect();
    // Variables bound to app/router objects: name -> (framework, prefix)
    let mut objects: HashMap<String, (&'static str, String)> = HashMap::new();
    for l in &lines {
        if let Some(c) = ctor.captures(l) {
            let fw = match &c[2] {
                "Flask" | "Blueprint" | "Quart" => "Flask",
                "Sanic" => "Sanic",
                "Starlette" | "Router" => "Starlette",
                "RouteTableDef" => "aiohttp",
                _ => "FastAPI",
            };
            let prefix = prefix_arg
                .captures(&c[3])
                .map(|p| p[1].to_string())
                .unwrap_or_default();
            objects.insert(c[1].to_string(), (fw, prefix));
        }
    }
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if let Some(c) = decorator.captures(l) {
            let obj = c[1].to_string();
            let kind = &c[2];
            let route = c[4].to_string();
            let rest = &c[5];
            let (fw, prefix) = objects.get(&obj).cloned().unwrap_or((
                if matches!(kind, "route") {
                    "Flask"
                } else {
                    "FastAPI"
                },
                String::new(),
            ));
            let methods: Vec<String> = match kind {
                "route" | "api_route" => {
                    let m: Vec<String> = methods_re
                        .captures(rest)
                        .map(|m| {
                            quoted
                                .captures_iter(&m[1])
                                .map(|q| q[1].to_uppercase())
                                .collect()
                        })
                        .unwrap_or_default();
                    if m.is_empty() {
                        vec![if kind == "route" {
                            "GET".into()
                        } else {
                            "ANY".into()
                        }]
                    } else {
                        m
                    }
                }
                "websocket" => vec!["WS".into()],
                k => vec![k.to_uppercase()],
            };
            // The handler is the next `def` after the decorator stack.
            let (handler, line) = lines
                .iter()
                .enumerate()
                .skip(i + 1)
                .take(20)
                .find_map(|(j, l)| def_re.captures(l).map(|d| (d[1].to_string(), j)))
                .unwrap_or_else(|| ("?".into(), i));
            out.push(Endpoint {
                framework: fw,
                methods,
                path: join_route(&prefix, &route),
                handler,
                file: file.to_path_buf(),
                line,
                params: Vec::new(),
                body: None,
            });
        }
    }
    signatures(text, &mut out, models);
    out.extend(additional_routes(file, text));
    out
}

/// Path/query/body parameters from FastAPI handler signatures.
fn signatures(text: &str, endpoints: &mut [Endpoint], models: &params::Models) {
    use rustpy_syntax::ast::{self, AstNode};
    if text.len() > 2 * 1024 * 1024 || !endpoints.iter().any(|e| e.framework == "FastAPI") {
        return;
    }
    let parse = rustpy_syntax::parse(text);
    let functions: Vec<(usize, ast::FuncDef)> = parse
        .syntax_node()
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .take(8192)
        .filter_map(|f| {
            let start = usize::from(f.name()?.text_range().start()).min(text.len());
            Some((text[..start].bytes().filter(|b| *b == b'\n').count(), f))
        })
        .collect();
    for e in endpoints.iter_mut().filter(|e| e.framework == "FastAPI") {
        if let Some((_, function)) = functions
            .iter()
            .find(|(line, f)| *line == e.line && f.name_text() == e.handler)
        {
            e.params = params::path_params(&e.path);
            params::fastapi_signature(function, e, models);
        }
    }
}

fn join_route(prefix: &str, route: &str) -> String {
    if prefix.is_empty() {
        return route.to_string();
    }
    format!(
        "{}/{}",
        prefix.trim_end_matches('/'),
        route.trim_start_matches('/')
    )
    .trim_end_matches('/')
    .to_string()
    .replace("//", "/")
}

/// Scans all files, applying `include_router(x, prefix=…)` / `register_blueprint(x, url_prefix=…)` prefixes by variable name.
pub fn scan_project(files: &[(PathBuf, String)]) -> Vec<Endpoint> {
    let sources: Vec<(&Path, &str)> = files
        .iter()
        .map(|(path, text)| (path.as_path(), text.as_str()))
        .collect();
    let models = params::Models::collect(&sources);
    static INCLUDE: OnceLock<Regex> = OnceLock::new();
    let include = re(
        &INCLUDE,
        r#"\.(?:include_router|register_blueprint)\(\s*(?:[\w\.]+\.)?([A-Za-z_]\w*)\s*,[^)]*?(?:url_prefix|prefix)\s*=\s*['"]([^'"]*)['"]"#,
    );
    let mut mount: HashMap<String, String> = HashMap::new();
    for (_, text) in files {
        for c in include.captures_iter(text) {
            mount.insert(c[1].to_string(), c[2].to_string());
        }
    }
    let mut out = Vec::new();
    for (path, text) in files {
        let mut eps = scan_routes(path, text, &models);
        if !mount.is_empty() {
            // Re-derive the router variable of each decorator to apply its mount prefix.
            let decorator = re(
                &DECORATOR,
                r#"^\s*@\s*([A-Za-z_][\w]*)\.(route|get|post|put|delete|patch|head|options|api_route|websocket)\(\s*(?:rule\s*=\s*|path\s*=\s*)?[rRbBuU]?(['"])(.*?)['"](.*)$"#,
            );
            let lines: Vec<&str> = text.lines().collect();
            for e in &mut eps {
                if e.framework == "Django" {
                    continue;
                }
                let var = (0..=e.line).rev().take(25).find_map(|k| {
                    lines
                        .get(k)
                        .and_then(|l| decorator.captures(l))
                        .map(|c| c[1].to_string())
                });
                if let Some(p) = var.and_then(|v| mount.get(&v)) {
                    e.path = join_route(p, &e.path);
                }
            }
        }
        out.extend(eps);
    }
    out.extend(urlconf::django(&sources));
    out.extend(urlconf::pyramid(&sources));
    finish(&mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.methods.cmp(&b.methods)));
    out
}

/// A request skeleton for the HTTP Client (`.http` syntax).
pub fn http_request(e: &Endpoint, base_url: &str) -> String {
    let method = match e.methods.first().map(String::as_str) {
        Some("ANY") | Some("WS") | None => "GET",
        Some(m) => m,
    };
    // `{id}` (FastAPI) and `<int:id>` (Flask) placeholders become HTTP Client variables.
    let mut path = String::new();
    let mut chars = e.path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                let inner: String = chars.by_ref().take_while(|&c| c != '>').collect();
                let name = inner.rsplit(':').next().unwrap_or("value");
                path.push_str(&format!("{{{{{name}}}}}"));
            }
            '{' => {
                let inner: String = chars.by_ref().take_while(|&c| c != '}').collect();
                let name = inner.split(':').next().unwrap_or("value");
                path.push_str(&format!("{{{{{name}}}}}"));
            }
            c => path.push(c),
        }
    }
    let query = e
        .params
        .iter()
        .filter(|p| p.location == ParamLocation::Query)
        .map(|p| format!("{0}={{{{{0}}}}}", p.name))
        .collect::<Vec<_>>()
        .join("&");
    if !query.is_empty() {
        path.push('?');
        path.push_str(&query);
    }
    let mut req = format!(
        "### {} {}\n{} {}{}\n",
        e.handler,
        e.path,
        method,
        base_url.trim_end_matches('/'),
        path
    );
    if matches!(method, "POST" | "PUT" | "PATCH") || e.body.is_some() {
        req.push_str(&format!(
            "Content-Type: application/json\n\n{}\n",
            e.body.as_deref().unwrap_or("{}")
        ));
    }
    req
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flask_routes_with_methods_and_blueprint_prefix() {
        let src = "from flask import Flask, Blueprint\napp = Flask(__name__)\nbp = Blueprint('api', __name__, url_prefix='/api')\n\n@app.route('/')\ndef index():\n    return 'hi'\n\n@bp.route('/items/<int:item_id>', methods=['GET', 'DELETE'])\n@login_required\ndef item(item_id):\n    pass\n\n@app.post('/login')\ndef login():\n    pass\n";
        let eps = scan_file(Path::new("app.py"), src);
        assert_eq!(eps.len(), 3);
        assert_eq!(
            (
                eps[0].path.as_str(),
                eps[0].methods.clone(),
                eps[0].handler.as_str(),
                eps[0].line
            ),
            ("/", vec!["GET".to_string()], "index", 5)
        );
        assert_eq!(
            (
                eps[1].path.as_str(),
                eps[1].methods.clone(),
                eps[1].handler.as_str()
            ),
            (
                "/api/items/<int:item_id>",
                vec!["GET".to_string(), "DELETE".to_string()],
                "item"
            )
        );
        assert_eq!(
            (eps[2].framework, eps[2].methods[0].as_str()),
            ("Flask", "POST")
        );
    }

    #[test]
    fn fastapi_routers_and_include_prefix() {
        let main = "from fastapi import FastAPI\nfrom . import users\napp = FastAPI()\napp.include_router(users.router, prefix='/users')\n\n@app.get('/health')\nasync def health():\n    return {}\n";
        let users = "from fastapi import APIRouter\nrouter = APIRouter(prefix='/v1')\n\n@router.get('/{user_id}')\nasync def get_user(user_id: int):\n    ...\n\n@router.websocket('/ws')\nasync def ws(sock):\n    ...\n";
        let eps = scan_project(&[
            (PathBuf::from("main.py"), main.into()),
            (PathBuf::from("users.py"), users.into()),
        ]);
        let paths: Vec<(String, String)> = eps
            .iter()
            .map(|e| (e.methods.join(","), e.path.clone()))
            .collect();
        assert_eq!(
            paths,
            vec![
                ("GET".to_string(), "/health".to_string()),
                ("WS".to_string(), "/users/v1/ws".to_string()),
                ("GET".to_string(), "/users/v1/{user_id}".to_string())
            ]
        );
        assert!(eps.iter().all(|e| e.framework == "FastAPI"));
    }

    #[test]
    fn django_urlconf() {
        let src = "from django.urls import path, re_path\nfrom . import views\n\nurlpatterns = [\n    path('articles/<int:year>/', views.year_archive, name='year'),\n    re_path(r'^about/$', views.AboutView.as_view()),\n]\n";
        let eps = scan_file(Path::new("blog/urls.py"), src);
        assert_eq!(eps.len(), 2);
        assert_eq!(
            (eps[0].path.as_str(), eps[0].handler.as_str(), eps[0].line),
            ("/articles/<int:year>/", "views.year_archive", 4)
        );
        assert_eq!(eps[1].handler, "views.AboutView.as_view()");
        assert!(
            scan_file(Path::new("other.py"), src).is_empty(),
            "only urls.py files are URLconfs"
        );
    }

    #[test]
    fn http_request_skeletons() {
        let e = Endpoint {
            framework: "Flask",
            methods: vec!["POST".into()],
            path: "/items/<int:item_id>".into(),
            handler: "item".into(),
            file: PathBuf::new(),
            line: 0,
            params: Vec::new(),
            body: None,
        };
        assert_eq!(http_request(&e, "http://127.0.0.1:5000/"), "### item /items/<int:item_id>\nPOST http://127.0.0.1:5000/items/{{item_id}}\nContent-Type: application/json\n\n{}\n");
        let f = Endpoint {
            framework: "FastAPI",
            methods: vec!["GET".into()],
            path: "/users/{user_id}".into(),
            handler: "get_user".into(),
            file: PathBuf::new(),
            line: 0,
            params: Vec::new(),
            body: None,
        };
        assert!(http_request(&f, "http://localhost:8000")
            .contains("GET http://localhost:8000/users/{{user_id}}\n"));
    }
}

/// Constructor and router-table endpoints for Starlette, Tornado and aiohttp.
fn additional_routes(file: &Path, text: &str) -> Vec<Endpoint> {
    use rustpy_syntax::ast::{self, AstNode};
    if text.len() > 2 * 1024 * 1024 {
        return Vec::new();
    }
    let parse = rustpy_syntax::parse(text);
    let mut out = Vec::new();
    let active = if text.contains("import tornado") || text.contains("from tornado") {
        "Tornado"
    } else if text.contains("import aiohttp") || text.contains("from aiohttp") {
        "aiohttp"
    } else if text.contains("import starlette") || text.contains("from starlette") {
        "Starlette"
    } else {
        return out;
    };
    for call in parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .take(8192)
    {
        let Some(callee) = call.callee() else {
            continue;
        };
        let name = callee.syntax().text().to_string();
        let name = name.rsplit('.').next().unwrap_or("");
        let args = call
            .arg_list()
            .map(|args| {
                args.args()
                    .filter_map(|arg| match arg {
                        ast::AnyArg::Positional(arg) => arg.expr(),
                        _ => None,
                    })
                    .take(64)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let line = text[..usize::from(call.syntax().text_range().start()).min(text.len())]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        if (active == "Starlette" && matches!(name, "Route" | "WebSocketRoute"))
            || (active == "aiohttp"
                && matches!(
                    name,
                    "get" | "post" | "put" | "delete" | "patch" | "head" | "options"
                ))
        {
            let (Some(ast::Expr::String(path)), Some(handler)) = (args.first(), args.get(1)) else {
                continue;
            };
            let mut methods = vec![if name == "WebSocketRoute" {
                "WS".into()
            } else if active == "aiohttp" {
                name.to_uppercase()
            } else {
                "GET".into()
            }];
            if let Some(args) = call.arg_list() {
                if let Some(value) = args
                    .keywords()
                    .find(|keyword| keyword.name_text() == "methods")
                    .and_then(|keyword| keyword.value())
                {
                    let values = value
                        .syntax()
                        .descendants()
                        .filter_map(ast::StringExpr::cast)
                        .map(|string| string.value().to_uppercase())
                        .take(32)
                        .collect::<Vec<_>>();
                    if !values.is_empty() {
                        methods = values;
                    }
                }
            }
            out.push(Endpoint {
                framework: active,
                methods,
                path: path.value(),
                handler: handler.syntax().text().to_string(),
                file: file.into(),
                line,
                params: Vec::new(),
                body: None,
            });
        }
        if active == "Tornado" && name == "Application" {
            for tuple in call
                .syntax()
                .descendants()
                .filter_map(ast::TupleExpr::cast)
                .take(2048)
            {
                let args = tuple.elements().take(3).collect::<Vec<_>>();
                let (Some(ast::Expr::String(path)), Some(handler)) = (args.first(), args.get(1))
                else {
                    continue;
                };
                out.push(Endpoint {
                    framework: "Tornado",
                    methods: vec!["ANY".into()],
                    path: path.value(),
                    handler: handler.syntax().text().to_string(),
                    file: file.into(),
                    line,
                    params: Vec::new(),
                    body: None,
                });
            }
        }
    }
    out
}
#[cfg(test)]
mod additional_framework_fixtures {
    use super::*;
    #[test]
    fn constructor_routes_and_aiohttp_tables_are_actual_endpoint_sources() {
        let routes=scan_file(Path::new("app.py"),"from starlette.routing import Route\nroutes=[Route('/items',items,methods=['POST'])]\n");
        assert_eq!(routes[0].framework, "Starlette");
        assert_eq!(routes[0].methods, vec!["POST"]);
        let routes=scan_file(Path::new("app.py"),"from aiohttp import web\nroutes=web.RouteTableDef()\n@routes.get('/items')\nasync def items(request):pass\n");
        assert_eq!(routes[0].framework, "aiohttp");
    }
}

#[cfg(test)]
mod project_route_tables {
    use super::*;

    fn project(files: &[(&str, &str)]) -> Vec<Endpoint> {
        let files: Vec<(PathBuf, String)> = files
            .iter()
            .map(|(p, t)| (PathBuf::from(p), t.to_string()))
            .collect();
        scan_project(&files)
    }

    #[test]
    fn django_includes_expand_recursively_with_joined_prefixes() {
        let root = "from django.urls import include, path\nfrom blog import urls as blog_urls\nfrom . import views\n\nextra = [\n    path('reports/', views.report),\n]\nurlpatterns = [\n    path('', views.home),\n    path('api/', include('api.urls')),\n    path('blog/', include(blog_urls)),\n    path('extra/', include(extra)),\n    path('inline/', include([path('x/<slug:s>/', views.x)])),\n    path('ns/', include(('shop.urls', 'shop'), namespace='shop')),\n    path('admin/', include('django.contrib.admin.urls')),\n]\n";
        let api = "from django.urls import include, path\nfrom . import views\nurlpatterns = [\n    path('v1/', include('api.v1_urls')),\n]\n";
        let v1 = "from django.urls import include, path\nfrom . import views\nurlpatterns = [\n    path('items/<int:pk>/', views.item),\n    path('loop/', include('api.urls')),\n]\n";
        let blog = "from django.urls import path, re_path\nfrom . import views\nurlpatterns = [\n    re_path(r'^(?P<year>[0-9]{4})/$', views.year),\n]\n";
        let shop = "from django.urls import path\nfrom . import views\napp_name = 'shop'\nurlpatterns = [path('cart/', views.cart)]\n";
        let eps = project(&[
            ("proj/proj/urls.py", root),
            ("proj/api/urls.py", api),
            ("proj/api/v1_urls.py", v1),
            ("proj/blog/urls.py", blog),
            ("proj/shop/urls.py", shop),
        ]);
        let got: Vec<(String, String)> = eps
            .iter()
            .map(|e| (e.path.clone(), e.handler.clone()))
            .collect();
        let want = [
            ("/", "views.home"),
            ("/admin/", "include('django.contrib.admin.urls')"),
            ("/api/v1/items/<int:pk>/", "views.item"),
            ("/blog/(?P<year>[0-9]{4})/$", "views.year"),
            ("/extra/reports/", "views.report"),
            ("/inline/x/<slug:s>/", "views.x"),
            ("/ns/cart/", "views.cart"),
        ];
        assert_eq!(
            got,
            want.iter()
                .map(|(p, h)| (p.to_string(), h.to_string()))
                .collect::<Vec<_>>()
        );
        let item = eps.iter().find(|e| e.handler == "views.item").unwrap();
        assert_eq!(item.file, PathBuf::from("proj/api/v1_urls.py"));
        assert_eq!(item.line, 3);
        assert_eq!(
            (
                item.params[0].name.as_str(),
                item.params[0].annotation.as_str()
            ),
            ("pk", "int")
        );
        assert_eq!(
            eps.iter()
                .find(|e| e.handler == "views.year")
                .unwrap()
                .params[0]
                .name,
            "year"
        );
    }

    #[test]
    fn fastapi_signature_parameters_and_body_skeleton() {
        let models = "from pydantic import BaseModel\nclass Owner(BaseModel):\n    email: str\nclass Item(BaseModel):\n    name: str\n    price: float\n    owner: Owner | None = None\n";
        let main = "from typing import Annotated\nfrom fastapi import FastAPI, Depends, Query, Request\nfrom .models import Item\napp = FastAPI()\n\n@app.put('/items/{item_id}')\nasync def update(item_id: int, item: Item, request: Request, q: str | None = None, db=Depends(get_db), limit: Annotated[int, Query()] = 10):\n    ...\n\n@app.post('/pair')\ndef pair(a: Item, b: Item):\n    ...\n";
        let eps = project(&[("app/models.py", models), ("app/main.py", main)]);
        let update = eps.iter().find(|e| e.handler == "update").unwrap();
        let params: Vec<String> = update
            .params
            .iter()
            .map(|p| format!("{}:{}:{}", p.name, p.location.as_str(), p.annotation))
            .collect();
        assert_eq!(
            params,
            [
                "item_id:path:int",
                "item:body:Item",
                "q:query:str | None",
                "limit:query:int"
            ]
        );
        assert_eq!(
            http_request(update, "http://127.0.0.1:8000"),
            "### update /items/{item_id}\nPUT http://127.0.0.1:8000/items/{{item_id}}?q={{q}}&limit={{limit}}\nContent-Type: application/json\n\n{\n  \"name\": \"string\",\n  \"price\": 0.0,\n  \"owner\": {\n    \"email\": \"string\"\n  }\n}\n"
        );
        let pair = eps.iter().find(|e| e.handler == "pair").unwrap();
        assert!(
            pair.body
                .as_deref()
                .unwrap()
                .starts_with("{\n  \"a\": {\n    \"name\": \"string\""),
            "{:?}",
            pair.body
        );
        // Flask converters become typed path parameters.
        let flask = scan_file(
            Path::new("app.py"),
            "from flask import Flask\napp = Flask(__name__)\n@app.route('/u/<int:uid>')\ndef u(uid):\n    pass\n",
        );
        assert_eq!(flask[0].params[0].annotation, "int");
        assert_eq!(flask[0].params[0].location, ParamLocation::Path);
    }

    #[test]
    fn pyramid_routes_join_view_config_and_add_view() {
        let init = "from pyramid.config import Configurator\ndef main(global_config, **settings):\n    with Configurator(settings=settings) as config:\n        config.add_route('home', '/')\n        config.add_route('item', '/items/{id}')\n        config.add_route('orphan', 'orphan', request_method='POST')\n        config.add_view(home_view, route_name='home')\n        config.scan()\n";
        let views = "from pyramid.view import view_config\n\n@view_config(route_name='item', request_method='GET', renderer='json')\ndef item_get(request):\n    pass\n\n@view_config(route_name='item', request_method=('PUT', 'PATCH'))\ndef item_put(request):\n    pass\n";
        let eps = project(&[("app/__init__.py", init), ("app/views.py", views)]);
        let got: Vec<(String, String, String)> = eps
            .iter()
            .map(|e| (e.methods.join(","), e.path.clone(), e.handler.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("ANY".to_string(), "/".to_string(), "home_view".to_string()),
                ("GET".into(), "/items/{id}".into(), "item_get".into()),
                ("PUT,PATCH".into(), "/items/{id}".into(), "item_put".into()),
                ("POST".into(), "/orphan".into(), "route 'orphan'".into()),
            ]
        );
        assert!(eps.iter().all(|e| e.framework == "Pyramid"));
        let get = &eps[1];
        assert_eq!(
            (get.file.as_path(), get.line),
            (Path::new("app/views.py"), 3)
        );
        assert_eq!(get.params[0].name, "id");
    }
}
