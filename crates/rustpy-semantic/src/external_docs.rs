//! External Documentation (Shift+F1, roadmap §2.10): the web page that
//! documents the symbol at the caret. The standard library maps to
//! docs.python.org for the interpreter's version (builtins to
//! `functions.html`, `stdtypes.html` and `exceptions.html`), other packages
//! to URL patterns (defaults for well-known packages, overridden by the
//! user's own) or else to their PyPI page. Project code has none.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustpy_core::TextSize;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;

use crate::db::{Analysis, FileInfo};
use crate::infer::Inferer;
use crate::scope::{BindingKind, ScopeId};
use crate::types::{ClassRef, Type};

/// URL patterns of well-known packages, `(module, pattern)`; see
/// [`expand_pattern`] for the placeholders.
pub const DEFAULT_PATTERNS: &[(&str, &str)] = &[
    (
        "numpy",
        "https://numpy.org/doc/stable/reference/generated/{module.name}.{element.qname}.html",
    ),
    (
        "scipy",
        "https://docs.scipy.org/doc/scipy/reference/generated/{module.name}.{element.qname}.html",
    ),
    (
        "pandas",
        "https://pandas.pydata.org/docs/reference/api/{module.name}.{element.qname}.html",
    ),
    (
        "matplotlib",
        "https://matplotlib.org/stable/api/_as_gen/{module.name}.{element.qname}.html",
    ),
    (
        "requests",
        "https://requests.readthedocs.io/en/latest/api/#{module.name}.{element.qname}",
    ),
    (
        "flask",
        "https://flask.palletsprojects.com/en/stable/api/#{module.name}.{element.qname}",
    ),
    (
        "django",
        "https://docs.djangoproject.com/en/stable/search/?q={element.name}",
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    Module,
    Class,
    Function,
    /// A function or attribute of a class (`str.split`).
    Method,
    /// A module-level constant or special form (`math.pi`, `typing.Optional`).
    Attribute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Stdlib,
    /// An installed package (site-packages, other interpreter paths).
    Library,
    Project,
}

/// The symbol a documentation page is looked up for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocTarget {
    /// The public module it is documented under (`os`, `asyncio`, `numpy.linalg`).
    pub module: String,
    /// The module of its definition (`posix`, `asyncio.tasks`, `_io`).
    pub defined_in: String,
    /// `["str", "split"]`; empty for a module.
    pub qualname: Vec<String>,
    pub kind: DocKind,
    /// An exception class, or a member of one.
    pub exception: bool,
    pub origin: Origin,
    /// The file of the definition (finds a library's distribution).
    pub path: PathBuf,
}

/// The External Documentation URL for the symbol at `offset` (`None` for
/// project code and symbols without a page). `python` is the interpreter's
/// version; `user_patterns` are `(module, URL pattern)` pairs that take
/// precedence over [`DEFAULT_PATTERNS`].
pub fn external_doc_url(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    python: (u32, u32),
    user_patterns: &[(String, String)],
) -> Option<String> {
    if let Some(target) = target_at(db, file, offset) {
        return url_for(&target, python, user_patterns);
    }
    let token = crate::features::ident_at(file, offset)?;
    if token.kind() != SyntaxKind::IDENT
        || token.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                SyntaxKind::TYPE_ALIAS_STMT | SyntaxKind::MATCH_STMT | SyntaxKind::CASE_CLAUSE
            )
        })
    {
        return keyword_doc_url(token.text(), python);
    }
    None
}

/// Language-reference documentation for real keyword tokens, also used by
/// Shift+F1 when no callable/class documentation target exists.
pub fn keyword_doc_url(word: &str, (major, minor): (u32, u32)) -> Option<String> {
    let page = match word {
        "if" | "elif" | "else" | "while" | "for" | "try" | "except" | "finally" | "with"
        | "def" | "class" | "async" | "match" | "case" => "compound_stmts",
        "assert" | "pass" | "del" | "return" | "yield" | "raise" | "break" | "continue"
        | "import" | "from" | "global" | "nonlocal" | "type" => "simple_stmts",
        "and" | "or" | "not" | "in" | "is" | "lambda" | "await" => "expressions",
        "True" | "False" | "None" | "as" => "lexical_analysis",
        _ => return None,
    };
    Some(format!(
        "https://docs.python.org/{major}.{minor}/reference/{page}.html"
    ))
}
/// Reject malformed or executable URL schemes before launching a browser.
pub fn pattern_error(module: &str, pattern: &str) -> Option<&'static str> {
    if module.trim().is_empty()
        || !module.trim().split('.').all(|part| {
            part.chars().next().is_some_and(rustpy_core::is_ident_start)
                && part.chars().all(rustpy_core::is_ident_continue)
        })
    {
        return Some("Enter a dotted Python module or package name");
    }
    if pattern.len() > 4096
        || !(pattern.starts_with("https://") || pattern.starts_with("http://"))
        || pattern.chars().any(char::is_whitespace)
    {
        return Some("Use an http:// or https:// URL without whitespace (maximum 4096 bytes)");
    }
    let mut remaining = pattern;
    while let Some(at) = remaining.find('{') {
        if remaining[..at].contains('}') {
            return Some("Unmatched URL placeholder delimiter");
        }
        remaining = &remaining[at..];
        let Some(end) = remaining.find('}') else {
            return Some("Unclosed URL placeholder");
        };
        if !matches!(
            &remaining[..=end],
            "{module.name}"
                | "{module.basename}"
                | "{element.name}"
                | "{element.qname}"
                | "{class.name}"
                | "{function.name}"
                | "{package}"
                | "{python.version}"
        ) {
            return Some("Unknown URL placeholder");
        }
        remaining = &remaining[end + 1..];
    }
    remaining
        .contains('}')
        .then_some("Unmatched URL placeholder delimiter")
}

/// What the identifier at `offset` refers to, as a documentation target.
pub fn target_at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Option<DocTarget> {
    let inf = Inferer::new(db);
    let tok = crate::features::ident_at(file, offset)?;
    if tok.kind() != SyntaxKind::IDENT {
        return None;
    }
    let parent = tok.parent()?;
    let name = tok.text().to_string();
    let scope = file.scopes.scope_at(tok.text_range().start());
    // The symbol's type, and the module the code reached it through
    // (`os.getcwd`, `from os import getcwd`): its public name.
    let (ty, via): (Type, Option<String>) = match parent.kind() {
        SyntaxKind::NAME_EXPR => (
            inf.type_of_expr(file, &ast::Expr::cast(parent.clone())?),
            imported_from(file, scope, &name),
        ),
        SyntaxKind::ATTRIBUTE_EXPR => {
            let attr = ast::AttributeExpr::cast(parent.clone())?;
            if attr.attr().map(|t| t.text_range()) != Some(tok.text_range()) {
                return None;
            }
            let via = match attr.object().map(|o| inf.type_of_expr(file, &o)) {
                Some(Type::Module(m)) => db.file(m).and_then(|f| f.module_name.clone()),
                _ => None,
            };
            (
                inf.type_of_expr(file, &ast::Expr::cast(parent.clone())?),
                via,
            )
        }
        SyntaxKind::DOTTED_NAME => {
            // `import os.path` / `from os.path import …`: the module up to the name.
            let path: Vec<String> = parent
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .filter(|t| {
                    t.kind() == SyntaxKind::IDENT
                        && t.text_range().start() <= tok.text_range().start()
                })
                .map(|t| t.text().to_string())
                .collect();
            let stmt = parent.parent()?;
            let level = stmt
                .children()
                .find(|c| c.kind() == SyntaxKind::RELATIVE_LEVEL)
                .map(|l| {
                    l.children_with_tokens()
                        .map(|t| match t.kind() {
                            SyntaxKind::DOT => 1,
                            SyntaxKind::ELLIPSIS => 3,
                            _ => 0,
                        })
                        .sum()
                })
                .unwrap_or(0);
            let module = if stmt.kind() == SyntaxKind::IMPORT_FROM_STMT {
                db.module_relative(file, level, &path.join("."))?
            } else {
                db.module(&path.join("."))?
            };
            (Type::Module(module.id), None)
        }
        SyntaxKind::IMPORT_ALIAS => {
            let (id, binding) = file.scopes.binding_at(tok.text_range().start())?;
            let via = match &binding.kind {
                BindingKind::Import(info) if info.level == 0 && info.name.is_some() => {
                    Some(info.module.to_string())
                }
                _ => None,
            };
            (inf.type_of_binding(file, id), via)
        }
        SyntaxKind::FUNC_DEF => {
            let f = ast::FuncDef::cast(parent.clone())?;
            let class = parent
                .ancestors()
                .skip(1)
                .take_while(|a| a.kind() != SyntaxKind::FUNC_DEF)
                .find_map(ast::ClassDef::cast)
                .map(|c| ClassRef {
                    file: file.id,
                    range: c.syntax().text_range(),
                    name: c.name_text().into(),
                });
            (inf.function_type(file, &f, class.as_ref()), None)
        }
        SyntaxKind::CLASS_DEF => {
            let c = ast::ClassDef::cast(parent.clone())?;
            (
                Type::Class(
                    ClassRef {
                        file: file.id,
                        range: c.syntax().text_range(),
                        name: c.name_text().into(),
                    },
                    Vec::new(),
                ),
                None,
            )
        }
        _ => return None,
    };
    let Some((def_file, range)) = crate::stub_docs::definition_of(&ty) else {
        // A constant or special form (`math.pi`, `sys.argv`, `typing.Optional`):
        // documented under the module the code took it from.
        let via = via?;
        let module_file = db.module(&via)?;
        let origin = origin_of(db, &module_file, &via);
        let path = documentation_path(db, &module_file, &via, origin);
        return Some(DocTarget {
            module: via.clone(),
            defined_in: via,
            qualname: vec![name],
            kind: DocKind::Attribute,
            exception: false,
            origin,
            path,
        });
    };
    let def_file = db.file(def_file)?;
    let defined_in = def_file.module_name.clone()?;
    let qualname = match range {
        Some(r) => crate::stub_docs::qualname_at(&def_file.parse.syntax_node(), r)?,
        None => Vec::new(),
    };
    let kind = match (&ty, qualname.len()) {
        (Type::Module(_), _) => DocKind::Module,
        (Type::Class(..), 1) => DocKind::Class,
        (_, 1) => DocKind::Function,
        _ => DocKind::Method,
    };
    let exception = match qualname.first() {
        Some(owner) => inf.resolve_in_module(&def_file, owner, 0).map(|d| inf.follow_imports(d)).is_some_and(|d| matches!(inf.type_of_binding(&d.file, d.binding), Type::Class(c, _) if inf.mro(&c).iter().any(|k| k.name == "BaseException"))),
        None => false,
    };
    let origin = origin_of(db, &def_file, &defined_in);
    let module = public_module(db, &inf, &def_file, &defined_in, &qualname, via, origin);
    let path = documentation_path(db, &def_file, &defined_in, origin);
    Some(DocTarget {
        module,
        defined_in,
        qualname,
        kind,
        exception,
        origin,
        path,
    })
}

/// `M` for a name the scope imported with `from M import name`.
fn imported_from(file: &FileInfo, scope: ScopeId, name: &str) -> Option<String> {
    let ids = file.scopes.resolve(scope, name)?;
    ids.iter()
        .rev()
        .find_map(|id| match &file.scopes.binding(*id).kind {
            BindingKind::Import(info) if info.level == 0 && info.name.is_some() => {
                Some(info.module.to_string())
            }
            _ => None,
        })
}

fn top(module: &str) -> &str {
    module.split('.').next().unwrap_or(module)
}

fn is_private(module: &str) -> bool {
    module
        .split('.')
        .any(|p| p.starts_with('_') && !(p.starts_with("__") && p.ends_with("__")))
}

fn documentation_path(db: &Analysis, file: &FileInfo, module: &str, origin: Origin) -> PathBuf {
    if origin == Origin::Library && file.path.starts_with("<typeshed>") {
        if let Some(source) = db.resolver.resolve_source(module) {
            return source;
        }
    }
    file.path.clone()
}

/// Whether the definition comes from the standard library, an installed
/// package or the project.
pub fn origin_of(db: &Analysis, file: &FileInfo, module: &str) -> Origin {
    if file.path.starts_with("<typeshed>") {
        // Third-party bundles and compiled placeholders are library definitions.
        return if file.path.starts_with("<typeshed>/@compiled")
            || !crate::typeshed::is_stdlib(module)
        {
            Origin::Library
        } else {
            Origin::Stdlib
        };
    }
    // A root can itself live below a directory named site-packages. Only
    // components below its deepest matching root identify installed packages
    // (notably a project's own .venv), rather than the project's source.
    let project_root = db
        .resolver
        .roots
        .iter()
        .filter(|r| !r.as_os_str().is_empty() && file.path.starts_with(r))
        .max_by_key(|r| r.components().count());
    let relative = project_root
        .and_then(|r| file.path.strip_prefix(r).ok())
        .unwrap_or(&file.path);
    if relative.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("site-packages" | "dist-packages")
        )
    }) {
        return Origin::Library;
    }
    if project_root.is_some() {
        return Origin::Project;
    }
    if db
        .resolver
        .search_paths
        .iter()
        .any(|p| db.resolver.root_containing(&file.path, p).is_some())
        && !db
            .resolver
            .roots
            .iter()
            .any(|r| db.resolver.root_containing(&file.path, r).is_some())
    {
        // The interpreter's own library directory holds the standard library sources.
        return if crate::typeshed::is_stdlib(top(module)) {
            Origin::Stdlib
        } else {
            Origin::Library
        };
    }
    Origin::Project
}

/// The public name of a standard-library module that implements another
/// (`posix` → `os`, `_io` → `io`, `posixpath` → `os.path`).
fn stdlib_public_module(module: &str) -> String {
    let mapped = match module {
        "posix" | "nt" => "os",
        "posixpath" | "ntpath" | "genericpath" => "os.path",
        "_collections_abc" => "collections.abc",
        "_pydatetime" => "datetime",
        "_pydecimal" => "decimal",
        "_pyio" => "io",
        "_sitebuiltins" => "builtins",
        "_weakrefset" => "weakref",
        "_elementtree" => "xml.etree.ElementTree",
        "_posixsubprocess" => "subprocess",
        "_frozen_importlib" | "_frozen_importlib_external" => "importlib",
        m => match m.strip_prefix('_') {
            Some(public)
                if !m.starts_with("__")
                    && !m.contains('.')
                    && crate::typeshed::is_stdlib(public) =>
            {
                public
            }
            _ => m,
        },
    };
    mapped.to_string()
}

/// The module a symbol is documented under: the public module the code used
/// to reach it, else the shortest package that re-exports the definition
/// (`asyncio` for `asyncio.tasks.sleep`, `json` for `json.decoder.JSONDecodeError`).
fn public_module(
    db: &Analysis,
    inf: &Inferer<'_>,
    def_file: &Arc<FileInfo>,
    defined_in: &str,
    qualname: &[String],
    via: Option<String>,
    origin: Origin,
) -> String {
    if let Some(v) = via {
        let same_kind = match origin {
            Origin::Stdlib => crate::typeshed::is_stdlib(&v),
            _ => top(&v) == top(defined_in),
        };
        if same_kind && !is_private(&v) {
            return v;
        }
    }
    let module = if origin == Origin::Stdlib {
        stdlib_public_module(defined_in)
    } else {
        defined_in.to_string()
    };
    let Some(first) = qualname.first() else {
        return module;
    };
    let parts: Vec<&str> = module.split('.').collect();
    for n in 1..parts.len() {
        let package = parts[..n].join(".");
        if is_private(&package) {
            break;
        }
        let Some(m) = db.module(&package) else {
            continue;
        };
        let Some(def) = inf.resolve_in_module(&m, first, 0) else {
            continue;
        };
        let def = inf.follow_imports(def);
        if crate::stub_docs::definition_of(&inf.type_of_binding(&def.file, def.binding))
            .map(|(f, _)| f)
            == Some(def_file.id)
        {
            return package;
        }
    }
    module
}

/// The page for `target` (`None` for project code).
pub fn url_for(
    target: &DocTarget,
    python: (u32, u32),
    user_patterns: &[(String, String)],
) -> Option<String> {
    // Explicit user mappings apply to modules as well as members, including
    // project-hosted documentation and standard-library mirror overrides.
    if let Some((_, pattern)) = user_patterns
        .iter()
        .filter(|(module, pattern)| {
            pattern_error(module.trim(), pattern.trim()).is_none()
                && (target.module == module.trim()
                    || target.module.starts_with(&format!("{}.", module.trim())))
        })
        .max_by_key(|(module, _)| module.trim().len())
    {
        return Some(expand_pattern(pattern.trim(), target, python));
    }
    match target.origin {
        Origin::Project => None,
        Origin::Stdlib => stdlib_url(target, python),
        Origin::Library => Some(library_url(target, python, user_patterns)),
    }
}

/// docs.python.org: `library/os.html#os.getcwd`, builtins in `functions.html`,
/// `stdtypes.html` (`str.split`) and `exceptions.html`.
fn stdlib_url(t: &DocTarget, (major, minor): (u32, u32)) -> Option<String> {
    // Typing helpers of typeshed itself have no page.
    if top(&t.module) == "_typeshed" {
        return None;
    }
    let base = format!("https://docs.python.org/{major}.{minor}/library/");
    let q = t.qualname.join(".");
    if t.module == "builtins" {
        return Some(match t.kind {
            DocKind::Module => format!("{base}functions.html"),
            _ if t.exception => format!("{base}exceptions.html#{q}"),
            DocKind::Class | DocKind::Function => format!("{base}functions.html#{q}"),
            DocKind::Method => format!("{base}stdtypes.html#{q}"),
            DocKind::Attribute => format!("{base}constants.html#{q}"),
        });
    }
    let page = if t.module == "asyncio" {
        asyncio_page(&t.defined_in).to_string()
    } else {
        t.module.to_lowercase()
    };
    Some(match t.kind {
        DocKind::Module => format!("{base}{page}.html#module-{}", t.module),
        _ => format!("{base}{page}.html#{}.{q}", t.module),
    })
}

/// asyncio is documented on several pages, one per submodule family.
fn asyncio_page(defined_in: &str) -> &'static str {
    match defined_in.strip_prefix("asyncio.").unwrap_or("") {
        "tasks" | "taskgroups" | "timeouts" | "coroutines" => "asyncio-task",
        "events" | "base_events" | "unix_events" | "windows_events" | "selector_events"
        | "proactor_events" => "asyncio-eventloop",
        "streams" => "asyncio-stream",
        "subprocess" => "asyncio-subprocess",
        "queues" => "asyncio-queue",
        "locks" => "asyncio-sync",
        "exceptions" => "asyncio-exceptions",
        "futures" | "base_futures" => "asyncio-future",
        "runners" => "asyncio-runner",
        "protocols" | "transports" => "asyncio-protocol",
        _ => "asyncio",
    }
}

/// A matching URL pattern (the user's first), else the PyPI page of the
/// distribution that installed the package.
fn library_url(t: &DocTarget, python: (u32, u32), user_patterns: &[(String, String)]) -> String {
    let matches =
        |m: &str| !m.is_empty() && (t.module == m || t.module.starts_with(&format!("{m}.")));
    let pattern = user_patterns
        .iter()
        .map(|(m, u)| (m.trim(), u.trim()))
        .find(|(m, u)| matches(m) && pattern_error(m, u).is_none())
        .or_else(|| DEFAULT_PATTERNS.iter().copied().find(|(m, _)| matches(m)));
    match pattern {
        Some((_, url)) if !t.qualname.is_empty() => expand_pattern(url, t, python),
        _ => format!(
            "https://pypi.org/project/{}/",
            distribution_name(&t.path, top(&t.module))
                .unwrap_or_else(|| top(&t.module).to_string())
        ),
    }
}

/// Fills a URL pattern: `{module.name}` (`numpy.linalg`), `{module.basename}`
/// (`linalg`), `{element.name}` (`norm`), `{element.qname}` (`ndarray.reshape`),
/// `{class.name}`, `{function.name}`, `{package}` (`numpy`) and
/// `{python.version}` (`3.12`).
pub fn expand_pattern(pattern: &str, t: &DocTarget, (major, minor): (u32, u32)) -> String {
    let element = t.qualname.last().map(String::as_str).unwrap_or("");
    let class = match t.kind {
        DocKind::Class => element,
        DocKind::Method => t.qualname.first().map(String::as_str).unwrap_or(""),
        _ => "",
    };
    let function = if matches!(t.kind, DocKind::Function | DocKind::Method) {
        element
    } else {
        ""
    };
    pattern
        .replace("{module.name}", &t.module)
        .replace(
            "{module.basename}",
            t.module.rsplit('.').next().unwrap_or(&t.module),
        )
        .replace("{element.qname}", &t.qualname.join("."))
        .replace("{element.name}", element)
        .replace("{class.name}", class)
        .replace("{function.name}", function)
        .replace("{package}", top(&t.module))
        .replace("{python.version}", &format!("{major}.{minor}"))
}

/// The PyPI project that installed the top-level module `top` (`PyYAML` for
/// `yaml`), from the `*.dist-info` / `*.egg-info` metadata beside it.
fn distribution_name(path: &Path, top: &str) -> Option<String> {
    let site = path.ancestors().find(|a| {
        matches!(
            a.file_name().and_then(|n| n.to_str()),
            Some("site-packages" | "dist-packages")
        )
    })?;
    let mut by_record = None;
    for e in std::fs::read_dir(site).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(stem) = name
            .strip_suffix(".dist-info")
            .or_else(|| name.strip_suffix(".egg-info"))
        else {
            continue;
        };
        let project = stem.split('-').next().unwrap_or(stem).to_string();
        let dir = e.path();
        if let Ok(top_level) = std::fs::read_to_string(dir.join("top_level.txt")) {
            if top_level.lines().any(|l| l.trim() == top) {
                return Some(project);
            }
            continue;
        }
        if by_record.is_none() {
            let record = std::fs::read_to_string(dir.join("RECORD")).unwrap_or_default();
            let (package, module) = (format!("{top}/"), format!("{top}.py,"));
            if record
                .lines()
                .any(|l| l.starts_with(&package) || l.starts_with(&module))
            {
                by_record = Some(project);
            }
        }
    }
    by_record
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::ModuleResolver;

    fn url(db: &Analysis, path: &Path, src: &str, at: &str) -> Option<String> {
        let id = db.set_file_text(path, src);
        let f = db.file(id).unwrap();
        let offset = TextSize::new(
            src.find(at)
                .unwrap_or_else(|| panic!("{at:?} not in source")) as u32,
        );
        external_doc_url(db, &f, offset, (3, 12), &[])
    }

    #[test]
    fn standard_library_pages() {
        let db = Analysis::for_project(Path::new("/proj"));
        let p = Path::new("/proj/main.py");
        let src = "import os\nimport io\nimport json\nimport math\nimport asyncio\nimport collections\nfrom os import getcwd\nfrom typing import Optional\n\n\ndef mine():\n    pass\n\n\nprint(len('a b'.split()))\nraise ValueError('x')\nmath.sqrt(2)\nos.path.join('a')\ngetcwd()\nio.StringIO().getvalue()\nasyncio.sleep(1)\ncollections.OrderedDict()\njson.JSONDecodeError\nmine()\nmath.pi\nx: Optional[int] = None\nlocal = 1\nlocal\n";
        let docs = "https://docs.python.org/3.12/library/";
        let expect = |at: &str, page: Option<&str>| {
            assert_eq!(
                url(&db, p, src, at),
                page.map(|s| format!("{docs}{s}")),
                "{at}"
            )
        };
        expect("print(", Some("functions.html#print"));
        expect("len(", Some("functions.html#len"));
        expect("split()", Some("stdtypes.html#str.split"));
        expect("ValueError", Some("exceptions.html#ValueError"));
        expect("sqrt", Some("math.html#math.sqrt"));
        expect("join(", Some("os.path.html#os.path.join"));
        expect("getcwd()", Some("os.html#os.getcwd"));
        expect("StringIO()", Some("io.html#io.StringIO"));
        expect("getvalue", Some("io.html#io.StringIO.getvalue"));
        expect("sleep", Some("asyncio-task.html#asyncio.sleep"));
        expect(
            "OrderedDict",
            Some("collections.html#collections.OrderedDict"),
        );
        expect("JSONDecodeError", Some("json.html#json.JSONDecodeError"));
        expect("json\nimport math", Some("json.html#module-json"));
        // Constants and special forms: under the module they come from.
        expect("pi\n", Some("math.html#math.pi"));
        expect("Optional[", Some("typing.html#typing.Optional"));
        // Project code has no external documentation.
        expect("mine()", None);
        expect("local\n", None);
        // The interpreter's version picks the documentation version.
        let id = db.file_for_path(p).unwrap();
        assert_eq!(
            external_doc_url(
                &db,
                &id,
                TextSize::new(src.find("sqrt").unwrap() as u32),
                (3, 9),
                &[]
            )
            .as_deref(),
            Some("https://docs.python.org/3.9/library/math.html#math.sqrt")
        );
    }

    #[test]
    fn library_patterns_and_pypi() {
        let dir = tempfile::TempDir::with_prefix("rustpy-extdoc").unwrap();
        let root = dir.path().join("proj");
        let site = dir.path().join("venv/lib/python3.12/site-packages");
        let write = |p: &Path, text: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write(
            &site.join("numpy/__init__.py"),
            "from numpy.linalg import norm\n\n\ndef zeros(shape):\n    pass\n",
        );
        write(&site.join("numpy/linalg.py"), "def norm(x):\n    pass\n");
        write(
            &site.join("yaml/__init__.py"),
            "def safe_load(stream):\n    pass\n",
        );
        write(
            &site.join("PyYAML-6.0.1.dist-info/top_level.txt"),
            "_yaml\nyaml\n",
        );
        write(
            &site.join("ujson_like/__init__.py"),
            "def dumps(o):\n    pass\n",
        );
        write(
            &site.join("ujson_like-5.0.dist-info/RECORD"),
            "ujson_like/__init__.py,sha256=x,10\n",
        );
        let db = Analysis::new(ModuleResolver::new(
            vec![root.clone()],
            vec![site.clone()],
            (3, 12),
        ));
        let p = root.join("main.py");
        let src = "import numpy as np\nimport yaml\nimport ujson_like\n\nnp.zeros(3)\nnp.linalg.norm(1)\nyaml.safe_load('')\nujson_like.dumps(1)\nnp\n";
        assert_eq!(
            url(&db, &p, src, "zeros").as_deref(),
            Some("https://numpy.org/doc/stable/reference/generated/numpy.zeros.html")
        );
        assert_eq!(
            url(&db, &p, src, "norm").as_deref(),
            Some("https://numpy.org/doc/stable/reference/generated/numpy.linalg.norm.html")
        );
        assert_eq!(
            url(&db, &p, src, "safe_load").as_deref(),
            Some("https://pypi.org/project/PyYAML/")
        );
        assert_eq!(
            url(&db, &p, src, "dumps").as_deref(),
            Some("https://pypi.org/project/ujson_like/")
        );
        // A module itself: its project page.
        assert_eq!(
            url(&db, &p, src, "np\n").as_deref(),
            Some("https://pypi.org/project/numpy/")
        );
        // The user's patterns come first.
        let id = db.file_for_path(&p).unwrap();
        let mine = vec![(
            "yaml".to_string(),
            "https://pyyaml.org/wiki/{element.name}?py={python.version}".to_string(),
        )];
        assert_eq!(
            external_doc_url(
                &db,
                &id,
                TextSize::new(src.find("safe_load").unwrap() as u32),
                (3, 12),
                &mine
            )
            .as_deref(),
            Some("https://pyyaml.org/wiki/safe_load?py=3.12")
        );
    }

    #[test]
    fn project_roots_win_over_incidental_library_directory_names() {
        for component in ["site-packages", "dist-packages"] {
            let root = PathBuf::from(format!("/audit/{component}/project"));
            let db = Analysis::new(ModuleResolver::new(
                vec![root.clone()],
                vec![root.clone()],
                (3, 12),
            ));
            let path = root.join("main.py");
            let src = "def mine(): pass\nmine()\n";
            assert_eq!(url(&db, &path, src, "mine()"), None);
            let f = db.file_for_path(&path).unwrap();
            assert_eq!(origin_of(&db, &f, "main"), Origin::Project);
            let installed = root.join(".venv/lib/python3.12/site-packages/vendor.py");
            let id = db.set_file_text(&installed, "def installed(): pass\n");
            let library = db.file(id).unwrap();
            assert_eq!(origin_of(&db, &library, "vendor"), Origin::Library);
            let stdlib = db.module("abc").unwrap();
            assert_eq!(origin_of(&db, &stdlib, "abc"), Origin::Stdlib);
        }
    }

    #[test]
    fn private_stdlib_modules_map_to_public_ones() {
        assert_eq!(stdlib_public_module("posix"), "os");
        assert_eq!(stdlib_public_module("_io"), "io");
        assert_eq!(stdlib_public_module("_collections_abc"), "collections.abc");
        assert_eq!(stdlib_public_module("_thread"), "_thread");
        assert_eq!(stdlib_public_module("json.decoder"), "json.decoder");
        assert_eq!(asyncio_page("asyncio.queues"), "asyncio-queue");
    }
    #[test]
    fn configured_module_project_and_stdlib_mirror_urls_use_most_specific_valid_prefix() {
        let mut target = DocTarget {
            module: "project.api".into(),
            defined_in: "project.api".into(),
            qualname: vec![],
            kind: DocKind::Module,
            exception: false,
            origin: Origin::Project,
            path: PathBuf::from("/project/api.py"),
        };
        let patterns = vec![
            (
                "project".into(),
                "https://docs.example/{module.name}/".into(),
            ),
            (
                "project.api".into(),
                "https://api.example/{python.version}/{module.name}.html".into(),
            ),
        ];
        assert_eq!(
            url_for(&target, (3, 13), &patterns).as_deref(),
            Some("https://api.example/3.13/project.api.html")
        );
        assert_eq!(url_for(&target, (3, 13), &[]), None);
        target.module = "math".into();
        target.origin = Origin::Stdlib;
        target.kind = DocKind::Function;
        target.qualname = vec!["sqrt".into()];
        assert_eq!(
            url_for(
                &target,
                (3, 13),
                &[(
                    "math".into(),
                    "https://mirror.example/{element.qname}".into()
                )]
            )
            .as_deref(),
            Some("https://mirror.example/sqrt")
        );
        for bad in [
            "javascript:alert(1)",
            "https://docs.example/{unknown}",
            "https://docs.example/}x/{module.name}",
        ] {
            assert!(pattern_error("math", bad).is_some());
            assert!(url_for(&target, (3, 13), &[("math".into(), bad.into())])
                .unwrap()
                .starts_with("https://docs.python.org/3.13/"));
        }
    }
    #[test]
    fn shift_f1_keyword_reference_does_not_turn_a_shadowed_soft_keyword_into_language_docs() {
        let db = Analysis::for_project(Path::new("/project"));
        let source = "def match(): pass\nmatch()\nwhile True:\n    break\n";
        let id = db.set_file_text(Path::new("/project/main.py"), source);
        let file = db.file(id).unwrap();
        assert!(external_doc_url(
            &db,
            &file,
            TextSize::new(source.find("while").unwrap() as u32),
            (3, 12),
            &[]
        )
        .unwrap()
        .ends_with("/reference/compound_stmts.html"));
        assert_eq!(
            external_doc_url(
                &db,
                &file,
                TextSize::new(source.rfind("match()").unwrap() as u32),
                (3, 12),
                &[]
            ),
            None
        );
    }
}
