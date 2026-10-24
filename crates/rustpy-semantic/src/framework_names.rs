//! Route and template names shared by Python framework strings and template
//! tags: Django `path(name=…)` declarations, Flask endpoints and the template
//! argument of render calls. Source-derived only; runtime catalogue names are
//! merged by the caller.
use crate::{
    db::{Analysis, FileInfo},
    features::Location,
    frameworks::origin,
};
use rustpy_core::TextSize;
use rustpy_syntax::ast::{self, AstNode};
use std::sync::Arc;

/// A named route declaration.
#[derive(Clone, Debug)]
pub struct Declaration {
    pub name: String,
    pub location: Location,
}

fn string(expr: &ast::Expr) -> Option<String> {
    if let ast::Expr::String(string) = expr.clone().unparen() {
        Some(string.value())
    } else {
        None
    }
}

fn sources(db: &Analysis, marker: &str) -> Vec<Arc<FileInfo>> {
    db.files_with_limit(4096)
        .unwrap_or_default()
        .into_iter()
        .filter(|file| !file.is_stub && file.text.contains(marker))
        .collect()
}

/// `path()/re_path()/url()` declarations with a literal `name=`. A module-level
/// literal `app_name` also contributes the namespaced `app:name` spelling.
pub fn django_urls(db: &Analysis) -> Vec<Declaration> {
    django_urls_in(sources(db, "django"))
}
/// [`django_urls`] over explicitly gathered sources (e.g. unloaded `urls.py`).
pub fn django_urls_in(sources: impl IntoIterator<Item = Arc<FileInfo>>) -> Vec<Declaration> {
    let mut out = Vec::new();
    for source in sources {
        let root = source.parse.syntax_node();
        let namespace = root
            .children()
            .filter_map(ast::AssignStmt::cast)
            .find(|assignment| {
                assignment
                    .targets()
                    .iter()
                    .any(|target| matches!(target, ast::Expr::Name(name) if name.name_text() == "app_name"))
            })
            .and_then(|assignment| assignment.value())
            .and_then(|value| string(&value))
            .filter(|name| !name.is_empty());
        for call in root
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .take(4096)
        {
            if !call
                .callee()
                .and_then(|callee| origin(&source, &callee))
                .is_some_and(|identity| {
                    matches!(
                        identity.as_str(),
                        "django.urls.path" | "django.urls.re_path" | "django.conf.urls.url"
                    )
                })
            {
                continue;
            }
            let Some(value) = call
                .arg_list()
                .and_then(|args| {
                    args.keywords()
                        .find(|keyword| keyword.name_text() == "name")
                })
                .and_then(|keyword| keyword.value())
            else {
                continue;
            };
            let Some(name) = string(&value) else {
                continue;
            };
            let location = Location {
                path: source.path.clone(),
                range: value.syntax().text_range(),
                is_stub: false,
            };
            if let Some(namespace) = &namespace {
                out.push(Declaration {
                    name: format!("{namespace}:{name}"),
                    location: location.clone(),
                });
            }
            out.push(Declaration { name, location });
        }
    }
    out
}

/// Flask view endpoints (blueprint-qualified), the names `url_for` accepts.
pub fn flask_endpoints(db: &Analysis) -> Vec<Declaration> {
    flask_endpoints_in(sources(db, "flask"))
}
/// [`flask_endpoints`] over explicitly gathered sources.
pub fn flask_endpoints_in(sources: impl IntoIterator<Item = Arc<FileInfo>>) -> Vec<Declaration> {
    let mut out = Vec::new();
    for source in sources {
        for (name, range) in crate::frameworks::flask_names(&source) {
            out.push(Declaration {
                name,
                location: Location {
                    path: source.path.clone(),
                    range,
                    is_stub: false,
                },
            });
        }
    }
    out
}

/// The declaration of `name`; a namespaced Django name falls back to its
/// last component when the namespace comes from an `include()` elsewhere.
pub fn find(declarations: &[Declaration], name: &str) -> Option<Location> {
    declarations
        .iter()
        .find(|declaration| declaration.name == name)
        .or_else(|| {
            let (_, short) = name.rsplit_once(':')?;
            declarations
                .iter()
                .find(|declaration| declaration.name == short)
        })
        .map(|declaration| declaration.location.clone())
}

/// When `offset` is inside the template-name string argument of a Django or
/// Flask render call, the offset where that string's content starts.
pub fn template_argument(file: &FileInfo, offset: TextSize) -> Option<TextSize> {
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::StringExpr::cast)
        .find(|string| string.syntax().text_range().contains_inclusive(offset))?;
    let call = node.syntax().ancestors().find_map(ast::CallExpr::cast)?;
    let identity = call.callee().and_then(|callee| origin(file, &callee))?;
    let position = match identity.as_str() {
        "django.shortcuts.render" | "django.template.response.TemplateResponse" => 1,
        "django.shortcuts.render_to_response"
        | "django.template.loader.render_to_string"
        | "django.template.loader.get_template"
        | "django.template.loader.select_template"
        | "django.template.response.SimpleTemplateResponse"
        | "flask.render_template"
        | "flask.stream_template" => 0,
        _ => return None,
    };
    let range = node.syntax().text_range();
    let args = call.arg_list()?;
    let positional = args
        .args()
        .filter_map(|arg| match arg {
            ast::AnyArg::Positional(arg) => arg.expr(),
            _ => None,
        })
        .nth(position);
    let keyword = args
        .keywords()
        .find(|keyword| keyword.name_text() == "template_name")
        .and_then(|keyword| keyword.value());
    let argument = |expr: Option<ast::Expr>| {
        expr.is_some_and(|expr| expr.unparen().syntax().text_range() == range)
    };
    if !argument(positional) && !argument(keyword) {
        return None;
    }
    let text = &file.text[usize::from(range.start())..usize::from(range.end())];
    let quote = text.find(['\'', '"'])?;
    let delimiter = &text[quote..quote + 1];
    let width = if text[quote..].starts_with(&delimiter.repeat(3)) && text.len() >= quote + 6 {
        3
    } else {
        1
    };
    let start = range.start() + TextSize::new((quote + width) as u32);
    (offset >= start && offset <= range.end()).then_some(start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::ModuleResolver;
    use std::path::Path;

    fn db(files: &[(&str, &str)]) -> Analysis {
        let db = Analysis::new(ModuleResolver::new(vec![], vec![], (3, 13)));
        for (path, text) in files {
            db.set_file_text(Path::new(path), text);
        }
        db
    }

    #[test]
    fn django_urls_resolve_plain_and_app_namespaced_names() {
        let urls = "from django.urls import path, re_path\napp_name = 'blog'\nurlpatterns = [path('', view, name='index'), re_path(r'^x$', view, name=\"detail\")]\n";
        let db = db(&[("/p/blog/urls.py", urls)]);
        let declarations = django_urls(&db);
        let names: Vec<_> = declarations.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["blog:index", "index", "blog:detail", "detail"]);
        let location = find(&declarations, "blog:detail").unwrap();
        assert_eq!(location.path, Path::new("/p/blog/urls.py"));
        assert_eq!(
            &urls[usize::from(location.range.start())..usize::from(location.range.end())],
            "\"detail\""
        );
        // A namespace declared by an include() still reaches the short name.
        assert!(find(&declarations, "other:index").is_some());
        assert!(find(&declarations, "missing").is_none());
    }

    #[test]
    fn flask_endpoints_include_blueprint_prefixes() {
        let app = "from flask import Flask, Blueprint\napp = Flask(__name__)\nauth = Blueprint('auth', __name__)\n@app.route('/')\ndef index(): pass\n@auth.route('/login')\ndef login(): pass\n";
        let db = db(&[("/p/app.py", app)]);
        let endpoints = flask_endpoints(&db);
        let names: Vec<_> = endpoints.iter().map(|d| d.name.as_str()).collect();
        assert!(
            names.contains(&"index") && names.contains(&"auth.login"),
            "{names:?}"
        );
        let location = find(&endpoints, "auth.login").unwrap();
        assert_eq!(
            &app[usize::from(location.range.start())..usize::from(location.range.end())],
            "login"
        );
    }

    #[test]
    fn template_argument_recognises_render_calls_only_at_the_template_slot() {
        let source = "from django.shortcuts import render\nfrom django.template.loader import render_to_string\nfrom django.template.response import TemplateResponse\nfrom flask import render_template\ndef v(request):\n    render(request, 'blog/in')\n    render_to_string('mail/')\n    TemplateResponse(request, template_name='x')\n    render_template('ho')\n    render(request, 'a', {'k': 'not'})\n";
        let db = db(&[("/p/views.py", source)]);
        let file = db.file_for_path(Path::new("/p/views.py")).unwrap();
        let at = |needle: &str, delta: usize| {
            TextSize::new((source.find(needle).unwrap() + delta) as u32)
        };
        for (needle, quote) in [("'blog/in'", 0), ("'mail/'", 0), ("'x'", 0), ("'ho'", 0)] {
            let caret = at(needle, needle.len() - 1);
            assert_eq!(
                template_argument(&file, caret),
                Some(at(needle, quote + 1)),
                "{needle}"
            );
        }
        assert_eq!(template_argument(&file, at("'not'", 2)), None);
    }
}
