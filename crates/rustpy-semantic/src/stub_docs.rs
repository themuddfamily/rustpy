//! Documentation for definitions that come from stubs (roadmap §2.6 "Quick
//! Documentation"). Stubs carry no docstrings, so the docstring of the same
//! qualified name is taken from the interpreter: the docstrings it reported
//! (`runtime_docs`: builtins and compiled modules such as `math`, `time` or
//! `itertools`), else its Python sources (pure-Python modules such as `re`,
//! `json` or `subprocess`, and the runtime package of a PEP 561 stub package).

use rustpy_core::TextRange;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;

use crate::db::Analysis;
use crate::types::Type;

/// Docstring for a symbol of type `ty` defined in a stub, taken from the
/// interpreter's docstrings or from the runtime source of the same module.
pub fn doc_for_type(db: &Analysis, ty: &Type) -> Option<String> {
    let (file_id, range) = definition_of(ty)?;
    let file = db.file(file_id)?;
    if !file.is_stub {
        return None;
    }
    let module = file.module_name.as_deref()?;
    let qualname = match range {
        Some(r) => qualname_at(&file.parse.syntax_node(), r)?,
        None => Vec::new(),
    };
    // An identically named project stub is not the interpreter's builtin/
    // stdlib module. Its own source docs remain eligible below.
    if crate::usages::is_library_file(db, &file) {
        if let Some(doc) = db.runtime_docs().get(module, &qualname) {
            return Some(doc.to_string());
        }
    }
    source_docstring(db, module, &qualname)
}

/// Where the function, class or module `ty` is defined: its file and the
/// range of the `def` / `class` statement (`None` for a module).
pub(crate) fn definition_of(ty: &Type) -> Option<(rustpy_core::FileId, Option<TextRange>)> {
    match ty {
        Type::Function(sig) | Type::Callable(sig) | Type::BoundMethod(sig, _) => {
            sig.def.as_ref().map(|d| (d.file, Some(d.range)))
        }
        Type::Overloaded(sigs) => sigs
            .iter()
            .find_map(|s| s.def.as_ref())
            .map(|d| (d.file, Some(d.range))),
        Type::Class(c, _) => Some((c.file, Some(c.range))),
        Type::Module(fid) => Some((*fid, None)),
        _ => None,
    }
}

/// `["Class", "method"]` for the class or function node with `range`.
pub(crate) fn qualname_at(
    root: &rustpy_syntax::SyntaxNode,
    range: TextRange,
) -> Option<Vec<String>> {
    let el = root.covering_element(range);
    let start = match el {
        rustpy_syntax::rowan::NodeOrToken::Node(n) => n,
        rustpy_syntax::rowan::NodeOrToken::Token(t) => t.parent()?,
    };
    let node = start.ancestors().find(|a| {
        a.text_range() == range && matches!(a.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF)
    })?;
    let mut parts: Vec<String> = Vec::new();
    for a in node.ancestors() {
        match a.kind() {
            SyntaxKind::FUNC_DEF => parts.push(ast::FuncDef::cast(a)?.name_text()),
            SyntaxKind::CLASS_DEF => parts.push(ast::ClassDef::cast(a)?.name_text()),
            _ => {}
        }
    }
    parts.reverse();
    Some(parts)
}

/// Definitions reachable at the top of a block: `if`/`try`/`with` bodies
/// are searched too (platform-specific definitions).
fn definitions(stmts: impl Iterator<Item = ast::Stmt>, out: &mut Vec<ast::Stmt>) {
    for s in stmts {
        match &s {
            ast::Stmt::If(i) => {
                let blocks = i
                    .body()
                    .into_iter()
                    .chain(i.elif_clauses().filter_map(|e| e.body()))
                    .chain(i.else_clause().and_then(|e| e.body()));
                for b in blocks {
                    definitions(b.statements(), out);
                }
            }
            ast::Stmt::Try(t) => {
                let blocks = t
                    .body()
                    .into_iter()
                    .chain(t.except_clauses().filter_map(|e| e.body()))
                    .chain(t.else_clause().and_then(|e| e.body()))
                    .chain(t.finally_clause().and_then(|f| f.body()));
                for b in blocks {
                    definitions(b.statements(), out);
                }
            }
            ast::Stmt::With(w) => {
                if let Some(b) = w.body() {
                    definitions(b.statements(), out);
                }
            }
            ast::Stmt::ClassDef(_) | ast::Stmt::FuncDef(_) => out.push(s),
            _ => {}
        }
    }
}

/// The selected provider's physical source counterpart precedes interpreter
/// fallback. In particular, a project stub must not borrow an unrelated
/// interpreter module's docs. Analysis owns overlays and snapshot admission.
fn source_parse(db: &Analysis, module: &str) -> Option<rustpy_syntax::Parse> {
    if let Some(selected) = db.module(module) {
        if !selected.path.starts_with("<typeshed>") {
            if !selected.is_stub {
                return Some(selected.parse.clone());
            }
            let sibling = selected.path.with_extension("py");
            if let Some(source) = db.file_for_path(&sibling) {
                return Some(source.parse.clone());
            }
            // Only interpreter libraries may fall back to the interpreter's
            // source search. A project-only stub has no such authority.
            if !crate::usages::is_library_file(db, &selected) {
                return None;
            }
        }
    }
    let path = db.resolver.resolve_source(module)?;
    db.file_for_path(&path).map(|file| file.parse.clone())
}

/// Whether the interpreter's Python source of `module` binds `name` at module
/// level: names typeshed leaves out (`urllib.request.proxy_bypass_environment`,
/// `socket._GLOBAL_DEFAULT_TIMEOUT`) still exist at run time.
pub fn source_defines(db: &Analysis, module: &str, name: &str) -> bool {
    let Some(parse) = source_parse(db, module) else {
        return false;
    };
    let scopes = crate::scope::build(&parse);
    !scopes.bindings_in(scopes.module_scope(), name).is_empty()
}

/// Docstring of `qualname` (empty: the module) in the Python source of
/// `module` found on the interpreter's search paths.
pub fn source_docstring(db: &Analysis, module: &str, qualname: &[String]) -> Option<String> {
    let parse = source_parse(db, module)?;
    let root = ast::Module::cast(parse.syntax_node())?;
    if qualname.is_empty() {
        return root.docstring().map(|d| d.value());
    }
    let mut scope: Vec<ast::Stmt> = Vec::new();
    definitions(root.statements(), &mut scope);
    let mut doc = None;
    for part in qualname {
        let found = scope.iter().find_map(|s| match s {
            ast::Stmt::ClassDef(c) if c.name_text() == *part => {
                Some((c.docstring().map(|d| d.value()), c.body()))
            }
            ast::Stmt::FuncDef(f) if f.name_text() == *part => {
                Some((f.docstring().map(|d| d.value()), None))
            }
            _ => None,
        })?;
        doc = found.0;
        scope.clear();
        if let Some(body) = found.1 {
            definitions(body.statements(), &mut scope);
        }
    }
    doc
}

#[cfg(test)]
mod documentation_source_tests {
    use super::*;
    #[test]
    fn stub_counterpart_documentation_uses_open_source_overlay() {
        let root = tempfile::TempDir::with_prefix("rustpy-doc-source").unwrap();
        let source = root.path().join("library.py");
        std::fs::write(
            &source,
            "\"\"\"Disk module.\"\"\"\ndef call():\n    \"\"\"Disk function.\"\"\"\n    pass\n",
        )
        .unwrap();
        std::fs::write(root.path().join("library.pyi"), "def call() -> None: ...\n").unwrap();
        let db = Analysis::for_project(root.path());
        assert_eq!(
            source_docstring(&db, "library", &["call".into()]).as_deref(),
            Some("Disk function.")
        );
        db.set_file_text(&source,"\"\"\"Unsaved module.\"\"\"\ndef call():\n    \"\"\"Unsaved function.\"\"\"\n    pass\n");
        assert_eq!(
            source_docstring(&db, "library", &[]).as_deref(),
            Some("Unsaved module.")
        );
        assert_eq!(
            source_docstring(&db, "library", &["call".into()]).as_deref(),
            Some("Unsaved function.")
        );
        assert!(std::fs::read_to_string(&source)
            .unwrap()
            .contains("Disk function."));
        let snapshot = db
            .snapshot(
                &[source.clone(), root.path().join("library.pyi")],
                &rustpy_core::CancellationToken::new(),
            )
            .unwrap();
        db.set_file_text(&source, "def call():\n    'Later function.'\n    pass\n");
        assert_eq!(
            source_docstring(snapshot.analysis(), "library", &["call".into()]).as_deref(),
            Some("Unsaved function.")
        );
        assert!(snapshot.complete());
        assert_eq!(
            source_docstring(&db, "library", &["call".into()]).as_deref(),
            Some("Later function.")
        );
    }
    #[test]
    fn project_shadow_stub_does_not_borrow_interpreter_standard_library_docs() {
        let root = tempfile::TempDir::with_prefix("rustpy-doc-shadow").unwrap();
        let path = root.path().join("math.pyi");
        std::fs::write(&path, "def sqrt(value: int) -> int: ...\n").unwrap();
        let db = Analysis::for_project(root.path());
        db.set_runtime_docs(std::sync::Arc::new(
            crate::runtime_docs::RuntimeDocs::from_json(
                r#"{"format":1,"docs":{"math.sqrt":"Interpreter square root documentation"}}"#,
            ),
        ));
        let source = "from math import sqrt\nsqrt(1)\n";
        let id = db.set_file_text(&root.path().join("main.py"), source);
        let file = db.file(id).unwrap();
        let info = crate::features::hover_at(
            &db,
            &file,
            rustpy_core::TextSize::new(source.rfind("sqrt(").unwrap() as u32),
        )
        .unwrap();
        assert_eq!(info.docstring, None);
    }
    #[test]
    fn generated_native_stub_docstring_literals_reach_module_class_and_method_hover() {
        let db = Analysis::for_project(std::path::Path::new("/project"));
        let stub = "'Native C module documentation.'\nfrom typing import Any\nclass Native:\n    'Native class documentation.'\n    def read(self, /) -> Any:\n        'Read native values; unicode π.'\n        ...\n";
        db.set_file_text(std::path::Path::new("/project/native.pyi"), stub);
        let source = "import native\nvalue = native.Native()\nvalue.read()\n";
        let id = db.set_file_text(std::path::Path::new("/project/main.py"), source);
        let file = db.file(id).unwrap();
        for (needle, expected) in [
            ("native\n", "Native C module documentation."),
            ("Native()", "Native class documentation."),
            ("read()", "Read native values; unicode π."),
        ] {
            let info = crate::features::hover_at(
                &db,
                &file,
                rustpy_core::TextSize::new(source.find(needle).unwrap() as u32),
            )
            .unwrap();
            assert_eq!(info.docstring.as_deref(), Some(expected), "{needle}");
        }
    }
}
