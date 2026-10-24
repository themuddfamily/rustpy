//! Import resolution regression tests (roadmap §2.2 "Module index"): source
//! roots, project-local virtual environments, PEP 561 stub-only packages,
//! compiled extension modules, relative-import completion and module
//! lookups that recover after packages are installed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustpy_core::TextSize;

use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::features;
use rustpy_semantic::module::ModuleResolver;

fn touch(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A project at `<tmp>/proj` with a `site-packages` search path.
fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::TempDir::with_prefix("rustpy-imports").unwrap();
    let root = dir.path().join("proj");
    let site = dir.path().join("site-packages");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&site).unwrap();
    for (rel, text) in files {
        let base = if let Some(r) = rel.strip_prefix("site:") {
            site.join(r)
        } else {
            root.join(rel)
        };
        touch(&base, text);
    }
    (dir, root, site)
}

fn open(db: &Analysis, path: &Path) -> Arc<FileInfo> {
    let text = std::fs::read_to_string(path).unwrap();
    let id = db.set_file_text(path, &text);
    db.file(id).unwrap()
}

fn unresolved(db: &Analysis, file: &Arc<FileInfo>) -> Vec<String> {
    features::diagnostics(db, file)
        .into_iter()
        .filter(|d| d.code == "PyUnresolvedReferences" || d.code == "syntax")
        .map(|d| d.message)
        .collect()
}

fn labels_at(db: &Analysis, file: &Arc<FileInfo>, offset: usize) -> Vec<String> {
    features::complete(db, file, TextSize::new(offset as u32))
        .into_iter()
        .map(|i| i.label.to_string())
        .collect()
}

#[test]
fn compiled_modules_and_stub_packages_produce_no_false_errors() {
    let src = "import ujson\nimport lxml.etree\nfrom lxml import etree, objectify\nimport requests\nfrom requests import get\n\nujson.dumps({})\netree.fromstring('<a/>')\nlxml.etree.tostring(None)\nrequests.get('u')\nget('u')\n";
    let (_dir, root, site) = project(&[
        ("main.py", src),
        ("site:ujson.cpython-312-x86_64-linux-gnu.so", ""),
        ("site:lxml/__init__.py", ""),
        ("site:lxml/etree.cpython-312-x86_64-linux-gnu.so", ""),
        (
            "site:requests/__init__.py",
            "def get(url, params=None, **kwargs):\n    '''Sends a GET request.'''\n",
        ),
        (
            "site:requests-stubs/__init__.pyi",
            "def get(url: str, params: object = ..., **kwargs: object) -> int: ...\n",
        ),
    ]);
    let db = Analysis::new(ModuleResolver::new(
        vec![root.clone()],
        vec![site.clone()],
        (3, 12),
    ));
    let f = open(&db, &root.join("main.py"));
    assert_eq!(
        unresolved(&db, &f),
        vec!["Cannot find reference 'objectify' in 'lxml'".to_string()]
    );
    // The compiled module is a read-only placeholder stub.
    let etree = db.module("lxml.etree").unwrap();
    assert!(
        etree.is_stub && etree.path.starts_with("<typeshed>"),
        "{}",
        etree.path.display()
    );
    assert_eq!(etree.module_name.as_deref(), Some("lxml.etree"));
    // The stub package types the runtime package (`-> int` only exists in the stub).
    let inf = rustpy_semantic::infer::Inferer::new(&db);
    let module = db.module("requests").unwrap();
    assert!(module.path.starts_with(site.join("requests-stubs")));
    let get = inf.resolve_in_module(&module, "get", 0).unwrap();
    let ty = inf.type_of_binding(&get.file, get.binding).to_string();
    assert!(ty.contains("int"), "{ty}");
}

#[test]
fn source_roots_name_modules_for_auto_import() {
    let (_dir, root, _site) = project(&[
        ("src/app/models.py", "class User:\n    pass\n"),
        ("src/app/__init__.py", ""),
        ("main.py", ""),
    ]);
    let src_root = root.join("src");
    let db = Analysis::new(ModuleResolver::new(
        vec![root.clone(), src_root.clone()],
        vec![],
        (3, 12),
    ));
    let models = db.file_for_path(&src_root.join("app/models.py")).unwrap();
    assert_eq!(models.module_name.as_deref(), Some("app.models"));
    // "Add import" offers the importable name, not `src.app.models`.
    let main = db.set_file_text(&root.join("main.py"), "User()\n");
    let main = db.file(main).unwrap();
    let symbols = vec![(
        "User".to_string(),
        db.resolver
            .module_name_of(&src_root.join("app/models.py"))
            .unwrap(),
    )];
    let problems = crate::inspections::inspect(
        &db,
        &main,
        &crate::inspections::InspectionOptions {
            project_symbols: symbols.into(),
            ..Default::default()
        },
    );
    let titles: Vec<String> = problems
        .iter()
        .flat_map(|p| p.fixes.iter().map(|f| f.title.clone()))
        .collect();
    assert!(
        titles
            .iter()
            .any(|t| t.contains("from app.models import User")),
        "{titles:?}"
    );
    assert!(!titles.iter().any(|t| t.contains("src.app")), "{titles:?}");
}

#[test]
fn relative_from_completes_package_modules() {
    let (_dir, root, _site) = project(&[
        ("pkg/__init__.py", ""),
        ("pkg/util.py", ""),
        ("pkg/models.py", ""),
        ("pkg/sub/__init__.py", ""),
        ("pkg/sub/deep.py", ""),
        ("pkg/sub/inner.py", ""),
        ("pkg/_private.py", ""),
        ("ns/a.py", ""),
        ("ns/b.py", ""),
    ]);
    let db = Analysis::for_project(&root);
    let check = |path: &str, src: &str| -> Vec<String> {
        let id = db.set_file_text(&root.join(path), src);
        let f = db.file(id).unwrap();
        labels_at(&db, &f, src.len())
    };
    let l = check("pkg/sub/inner.py", "from .");
    assert!(l.contains(&"deep".to_string()), "{l:?}");
    let l = check("pkg/sub/inner.py", "from ..");
    assert!(
        l.contains(&"util".to_string())
            && l.contains(&"models".to_string())
            && l.contains(&"sub".to_string()),
        "{l:?}"
    );
    let l = check("pkg/sub/inner.py", "from ..sub.");
    assert!(l.contains(&"deep".to_string()), "{l:?}");
    let l = check("pkg/sub/inner.py", "from ..ut");
    assert!(l.contains(&"util".to_string()), "{l:?}");
    let l = check("pkg/util.py", "from .mo");
    assert_eq!(l.first().map(String::as_str), Some("models"), "{l:?}");
    // `from . import <caret>` in a namespace package (no `__init__.py`).
    let l = check("ns/a.py", "from . import ");
    assert!(l.contains(&"b".to_string()), "{l:?}");
    let l = check("pkg/util.py", "from . import ");
    assert!(
        l.contains(&"models".to_string()) && !l.contains(&"_private".to_string()),
        "{l:?}"
    );
}

#[test]
fn module_lookups_recover_after_installing_a_package() {
    let (_dir, root, site) = project(&[("main.py", "import fancylib\n")]);
    let db = Analysis::new(ModuleResolver::new(
        vec![root.clone()],
        vec![site.clone()],
        (3, 12),
    ));
    let f = open(&db, &root.join("main.py"));
    assert_eq!(
        unresolved(&db, &f),
        vec!["No module named 'fancylib'".to_string()]
    );
    touch(&site.join("fancylib/__init__.py"), "X = 1\n");
    assert_eq!(unresolved(&db, &f).len(), 1, "failed lookups are cached");
    db.invalidate_modules();
    assert!(unresolved(&db, &f).is_empty());
}

#[test]
fn project_venv_packages_are_libraries_with_short_names() {
    let (_dir, root, _site) = project(&[
        (
            ".venv/lib/python3.12/site-packages/fancylib/__init__.py",
            "def fetch(url):\n    '''Fetches.'''\n",
        ),
        ("src/own/__init__.py", "def mine():\n    pass\n"),
        (
            "main.py",
            "from fancylib import fetch\nfrom own import mine\nfetch('u')\nmine()\n",
        ),
    ]);
    let venv_site = root.join(".venv/lib/python3.12/site-packages");
    // An editable install puts the project's own `src` on the search path.
    let db = Analysis::new(ModuleResolver::new(
        vec![root.clone()],
        vec![venv_site.clone(), root.join("src")],
        (3, 12),
    ));
    let lib = db.module("fancylib").unwrap();
    assert_eq!(
        lib.module_name.as_deref(),
        Some("fancylib"),
        "not `.venv.lib.python3.12.site-packages.fancylib`"
    );
    let main = open(&db, &root.join("main.py"));
    assert!(
        unresolved(&db, &main).is_empty(),
        "{:?}",
        unresolved(&db, &main)
    );
    let src = main.text.to_string();
    let files = vec![root.join("main.py"), root.join("src/own/__init__.py")];
    let err = rustpy_refactor::rename::rename(
        &db,
        &main,
        TextSize::new(src.find("fetch('u')").unwrap() as u32),
        "get",
        &files,
    )
    .unwrap_err();
    assert!(err.contains("library"), "{err}");
    // The project's own sources stay renameable.
    assert!(rustpy_refactor::rename::rename(
        &db,
        &main,
        TextSize::new(src.find("mine()").unwrap() as u32),
        "ours",
        &files
    )
    .is_ok());
}

#[test]
fn module_getattr_provides_attributes() {
    let (_dir, root, _site) = project(&[
        (
            "lazy.py",
            "from typing import Any\n\ndef __getattr__(name: str) -> int: ...\n",
        ),
        (
            "main.py",
            "import lazy\nlazy.anything.bit_length()\nlazy.other\n",
        ),
    ]);
    let db = Analysis::for_project(&root);
    let f = open(&db, &root.join("main.py"));
    assert!(unresolved(&db, &f).is_empty(), "{:?}", unresolved(&db, &f));
    let inf = rustpy_semantic::infer::Inferer::new(&db);
    let lazy = db.module("lazy").unwrap();
    assert_eq!(
        inf.lookup_member(&rustpy_semantic::types::Type::Module(lazy.id), "anything")
            .map(|t| t.to_string())
            .as_deref(),
        Some("int")
    );
}
