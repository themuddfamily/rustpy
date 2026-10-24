//! Refactoring regression tests from review: Safe Delete of imports and
//! re-exports, Change Signature call sites (generators, inherited
//! constructors, call values), parameter and alias renames, library files,
//! extract-variable occurrences and analysis invalidation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustpy_core::{TextRange, TextSize};

use crate::change_signature::{self, ParamKind, SigParam};
use crate::refactor::{self, apply_edits, Edit, SafeDeleteError};
use crate::rename;
use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::module::ModuleResolver;
use rustpy_semantic::usages;

fn at(src: &str, needle: &str) -> TextSize {
    TextSize::new(
        src.find(needle)
            .unwrap_or_else(|| panic!("needle {needle:?}")) as u32,
    )
}

fn sel(src: &str, needle: &str) -> TextRange {
    let s = src
        .find(needle)
        .unwrap_or_else(|| panic!("needle {needle:?}"));
    TextRange::new(
        TextSize::new(s as u32),
        TextSize::new((s + needle.len()) as u32),
    )
}

/// Files held in memory (editor texts) under `/proj`.
struct Project {
    db: Analysis,
    files: Vec<(PathBuf, String)>,
}

impl Project {
    fn new(files: &[(&str, &str)]) -> Self {
        let db = Analysis::for_project(Path::new("/proj"));
        let files: Vec<(PathBuf, String)> = files
            .iter()
            .map(|(n, t)| (PathBuf::from(format!("/proj/{n}")), t.to_string()))
            .collect();
        for (p, t) in &files {
            db.set_file_text(p, t);
        }
        Self { db, files }
    }

    fn file(&self, name: &str) -> Arc<FileInfo> {
        self.db
            .file_for_path(Path::new(&format!("/proj/{name}")))
            .unwrap()
    }

    fn paths(&self) -> Vec<PathBuf> {
        self.files.iter().map(|(p, _)| p.clone()).collect()
    }

    /// The text of `name` after `changes`.
    fn after(&self, changes: &[(PathBuf, Vec<Edit>)], name: &str) -> String {
        let (path, text) = self
            .files
            .iter()
            .find(|(p, _)| p.ends_with(name))
            .expect("file");
        let edits = changes
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, e)| e.clone())
            .unwrap_or_default();
        apply_edits(text, &edits)
    }
}

/// A project on disk (relative imports and interpreter paths need real files).
struct DiskProject {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl DiskProject {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::TempDir::with_prefix("rustpy-refactor").unwrap();
        let root = dir.path().to_path_buf();
        for (name, text) in files {
            let p = root.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, text).unwrap();
        }
        Self { _dir: dir, root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn py_files(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                let hidden = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'));
                if p.is_dir() && !hidden {
                    walk(&p, out);
                } else if p.extension().and_then(|e| e.to_str()) == Some("py") {
                    out.push(p);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out.sort();
        out
    }

    fn after(&self, changes: &[(PathBuf, Vec<Edit>)], name: &str) -> String {
        let path = self.path(name);
        let text = std::fs::read_to_string(&path).unwrap();
        let edits = changes
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, e)| e.clone())
            .unwrap_or_default();
        apply_edits(&text, &edits)
    }
}

fn rename_edits_to_changes(r: rename::RenameEdits) -> Vec<(PathBuf, Vec<Edit>)> {
    r
}

// ----- Safe Delete -------------------------------------------------------

#[test]
fn delete_anyway_leaves_no_empty_block_behind() {
    let main = "def helper():\n    return 1\n";
    let other = "from typing import TYPE_CHECKING\n\nif TYPE_CHECKING:\n    from main import helper\n\ntry:\n    from main import helper as h\nexcept ImportError:\n    h = None\n\nif TYPE_CHECKING: from main import helper as inline\n\nimport os; from main import helper as semi\nprint(os.sep, h)\n";
    let p = Project::new(&[("main.py", main), ("other.py", other)]);
    let m = refactor::safe_delete_anyway(&p.db, &p.file("main.py"), at(main, "helper"), &p.paths())
        .unwrap();
    assert_eq!(p.after(&m.changes, "main.py"), "");
    assert_eq!(
        p.after(&m.changes, "other.py"),
        "from typing import TYPE_CHECKING\n\nif TYPE_CHECKING:\n    pass\n\ntry:\n    pass\nexcept ImportError:\n    h = None\n\nif TYPE_CHECKING: pass\n\nimport os\nprint(os.sep, h)\n"
    );
}

#[test]
fn delete_anyway_keeps_one_pass_when_a_block_loses_every_statement() {
    let main = "def helper():\n    return 1\n";
    let other = "import typing\n\nif typing.TYPE_CHECKING:\n    from main import helper\n    from main import helper as h2\nelse:\n    helper = h2 = None\n";
    let p = Project::new(&[("main.py", main), ("other.py", other)]);
    let m = refactor::safe_delete_anyway(&p.db, &p.file("main.py"), at(main, "helper"), &p.paths())
        .unwrap();
    assert_eq!(
        p.after(&m.changes, "other.py"),
        "import typing\n\nif typing.TYPE_CHECKING:\n    pass\nelse:\n    helper = h2 = None\n"
    );
}

#[test]
fn delete_anyway_removes_statements_sharing_a_line() {
    let main = "def helper():\n    return 1\n";
    let other = "import os; from main import helper; from main import helper as h\nprint(os.sep)\n";
    let p = Project::new(&[("main.py", main), ("other.py", other)]);
    let m = refactor::safe_delete_anyway(&p.db, &p.file("main.py"), at(main, "helper"), &p.paths())
        .unwrap();
    assert_eq!(
        p.after(&m.changes, "other.py"),
        "import os\nprint(os.sep)\n"
    );
}

#[test]
fn safe_delete_in_a_one_line_block_keeps_the_header() {
    let src = "import sys\n\nif sys.argv: unused = 1\nprint(sys.argv)\n";
    let p = Project::new(&[("m.py", src)]);
    let r = refactor::safe_delete(&p.db, &p.file("m.py"), at(src, "unused"), &p.paths()).unwrap();
    assert_eq!(
        apply_edits(src, &r.edits),
        "import sys\n\nif sys.argv: pass\nprint(sys.argv)\n"
    );
}

#[test]
fn safe_delete_of_a_lone_import_in_a_block_inserts_pass() {
    let src = "from typing import TYPE_CHECKING\n\nif TYPE_CHECKING:\n    import os\nx = 1\n";
    let p = Project::new(&[("m.py", src)]);
    let r = refactor::safe_delete(&p.db, &p.file("m.py"), at(src, "os"), &p.paths()).unwrap();
    assert_eq!(
        apply_edits(src, &r.edits),
        "from typing import TYPE_CHECKING\n\nif TYPE_CHECKING:\n    pass\nx = 1\n"
    );
}

#[test]
fn safe_delete_of_a_reexport_sees_its_importers() {
    let p = DiskProject::new(&[
        (
            "pkg/__init__.py",
            "from .core import Engine\nfrom .core import Spare\n",
        ),
        (
            "pkg/core.py",
            "class Engine:\n    pass\n\n\nclass Spare:\n    pass\n",
        ),
        ("main.py", "from pkg import Engine\n\nEngine()\n"),
        ("app.py", "import pkg\n\npkg.Engine()\n"),
    ]);
    let db = Analysis::for_project(&p.root);
    let files = p.py_files();
    let init = db.file_for_path(&p.path("pkg/__init__.py")).unwrap();
    let text = init.text.to_string();
    match refactor::safe_delete(&db, &init, at(&text, "Engine"), &files) {
        Err(SafeDeleteError::Used { usages, .. }) => {
            let mut where_: Vec<String> = usages
                .iter()
                .map(|u| {
                    format!(
                        "{}:{}",
                        u.path.file_name().unwrap().to_string_lossy(),
                        u.line + 1
                    )
                })
                .collect();
            where_.sort();
            assert_eq!(
                where_,
                vec!["app.py:3", "main.py:1", "main.py:3"],
                "{usages:?}"
            );
        }
        other => panic!("expected usages, got {other:?}"),
    }
    // A re-export nobody imports can go.
    let r = refactor::safe_delete(&db, &init, at(&text, "Spare"), &files).unwrap();
    assert_eq!(apply_edits(&text, &r.edits), "from .core import Engine\n");
    // Delete Anyway also drops the importer's `from pkg import Engine`.
    let m = refactor::safe_delete_anyway(&db, &init, at(&text, "Engine"), &files).unwrap();
    assert_eq!(
        p.after(&m.changes, "pkg/__init__.py"),
        "from .core import Spare\n"
    );
    assert_eq!(p.after(&m.changes, "main.py"), "Engine()\n");
}

// ----- Change Signature ----------------------------------------------------

#[test]
fn change_signature_parenthesizes_a_bare_generator_argument() {
    let src =
        "def total(items):\n    return sum(items)\n\n\ntotal(x for x in range(3))\ntotal([1])\n";
    let p = Project::new(&[("m.py", src)]);
    let f = p.file("m.py");
    let mut model = change_signature::signature_model(&f, at(src, "def total")).unwrap();
    model.params.push(SigParam::added("scale", None, Some("1")));
    let (m, summary) = change_signature::change_signature(&p.db, &f, &model, &p.paths()).unwrap();
    assert_eq!(p.after(&m.changes, "m.py"), "def total(items, scale):\n    return sum(items)\n\n\ntotal((x for x in range(3)), 1)\ntotal([1], 1)\n");
    assert_eq!(summary.updated_calls, 2);
    // A generator that turns into a keyword argument is parenthesized too.
    let mut model = change_signature::signature_model(&f, at(src, "def total")).unwrap();
    model.params[0].kind = ParamKind::KeywordOnly;
    let (m, _) = change_signature::change_signature(&p.db, &f, &model, &p.paths()).unwrap();
    assert_eq!(p.after(&m.changes, "m.py"), "def total(*, items):\n    return sum(items)\n\n\ntotal(items=(x for x in range(3)))\ntotal(items=[1])\n");
    // Unchanged calls keep their bare generator.
    let mut model = change_signature::signature_model(&f, at(src, "def total")).unwrap();
    model.params.push(SigParam::added("scale", Some("1"), None));
    let (m, _) = change_signature::change_signature(&p.db, &f, &model, &p.paths()).unwrap();
    assert_eq!(p.after(&m.changes, "m.py"), "def total(items, scale=1):\n    return sum(items)\n\n\ntotal(x for x in range(3))\ntotal([1])\n");
}

#[test]
fn change_signature_updates_constructor_calls_of_subclasses() {
    let main = "class Box:\n    def __init__(self, size):\n        self.size = size\n\n\nclass Big(Box):\n    pass\n\n\nclass Own(Box):\n    def __init__(self):\n        super().__init__(0)\n\n\nBox(2)\nBig(1)\nOwn()\n";
    let other = "from main import Big\n\n\nclass Huge(Big):\n    pass\n\n\nBig(3)\nHuge(4)\n";
    let p = Project::new(&[("main.py", main), ("other.py", other)]);
    let f = p.file("main.py");
    let mut model = change_signature::signature_model(&f, at(main, "def __init__")).unwrap();
    model
        .params
        .push(SigParam::added("color", None, Some("'red'")));
    let (m, summary) = change_signature::change_signature(&p.db, &f, &model, &p.paths()).unwrap();
    assert_eq!(
        p.after(&m.changes, "main.py"),
        "class Box:\n    def __init__(self, size, color):\n        self.size = size\n\n\nclass Big(Box):\n    pass\n\n\nclass Own(Box):\n    def __init__(self):\n        super().__init__(0, 'red')\n\n\nBox(2, 'red')\nBig(1, 'red')\nOwn()\n"
    );
    assert_eq!(
        p.after(&m.changes, "other.py"),
        "from main import Big\n\n\nclass Huge(Big):\n    pass\n\n\nBig(3, 'red')\nHuge(4, 'red')\n"
    );
    assert_eq!(
        (summary.updated_calls, summary.files),
        (5, 2),
        "{summary:?}"
    );
    assert!(summary.skipped.is_empty(), "{summary:?}");
    let (calls, files, untyped) = change_signature::call_site_counts(&p.db, &f, &model, &p.paths());
    assert_eq!((calls, files, untyped), (5, 2, 0));
}

#[test]
fn change_signature_passes_the_call_value_of_a_new_defaulted_parameter() {
    let lib = "def connect(host, port, timeout=5):\n    return host, port, timeout\n\n\nconnect('a', 1)\nconnect('b', 2, 3)\n";
    let p = Project::new(&[("lib.py", lib)]);
    let f = p.file("lib.py");
    let mut model = change_signature::signature_model(&f, at(lib, "def connect")).unwrap();
    model
        .params
        .push(SigParam::added("retries", Some("3"), Some("0")));
    let (m, summary) = change_signature::change_signature(&p.db, &f, &model, &p.paths()).unwrap();
    assert_eq!(p.after(&m.changes, "lib.py"), "def connect(host, port, timeout=5, retries=3):\n    return host, port, timeout\n\n\nconnect('a', 1, retries=0)\nconnect('b', 2, 3, 0)\n");
    assert_eq!(summary.updated_calls, 2);
    // Without a call value the default covers existing calls.
    let mut model = change_signature::signature_model(&f, at(lib, "def connect")).unwrap();
    model
        .params
        .push(SigParam::added("retries", Some("3"), None));
    let (m, summary) = change_signature::change_signature(&p.db, &f, &model, &p.paths()).unwrap();
    assert_eq!(p.after(&m.changes, "lib.py"), "def connect(host, port, timeout=5, retries=3):\n    return host, port, timeout\n\n\nconnect('a', 1)\nconnect('b', 2, 3)\n");
    assert_eq!(summary.updated_calls, 0);
    // A call that unpacks arguments cannot take the value safely: listed for a manual look.
    let src = "def f(a, b=1):\n    return a\n\n\nargs = (1,)\nf(*args)\n";
    let p = Project::new(&[("u.py", src)]);
    let f = p.file("u.py");
    let mut model = change_signature::signature_model(&f, at(src, "def f")).unwrap();
    model
        .params
        .push(SigParam::added("c", Some("None"), Some("2")));
    let (_, summary) = change_signature::change_signature(&p.db, &f, &model, &p.paths()).unwrap();
    assert_eq!(summary.skipped.len(), 1, "{summary:?}");
}

// ----- Rename: parameters ----------------------------------------------------

#[test]
fn renaming_an_init_parameter_reaches_subclass_calls_in_other_files() {
    let main = "class Box:\n    def __init__(self, size):\n        self.size = size\n\n\nclass Big(Box):\n    pass\n\n\nBig(size=1)\n";
    let other = "from main import Big\n\nBig(size=2)\n";
    let p = Project::new(&[("main.py", main), ("other.py", other)]);
    let r = rename::rename(
        &p.db,
        &p.file("main.py"),
        at(main, "size):"),
        "capacity",
        &p.paths(),
    )
    .unwrap();
    let changes = rename_edits_to_changes(r);
    assert_eq!(p.after(&changes, "main.py"), "class Box:\n    def __init__(self, capacity):\n        self.size = capacity\n\n\nclass Big(Box):\n    pass\n\n\nBig(capacity=1)\n");
    assert_eq!(
        p.after(&changes, "other.py"),
        "from main import Big\n\nBig(capacity=2)\n"
    );
}

#[test]
fn renaming_a_parameter_leaves_unresolved_functions_of_the_same_name_alone() {
    let main = "def greet(name):\n    return name\n\n\nclass Host:\n    def greet(self, name):\n        return name\n";
    let other = "from missing_package import greet\n\ngreet(name='x')\n\n\ndef use(obj, thing):\n    obj.greet(name='y')\n    thing.attr.greet(name='z')\n";
    let p = Project::new(&[("main.py", main), ("other.py", other)]);
    let r = rename::rename(
        &p.db,
        &p.file("main.py"),
        at(main, "name):"),
        "who",
        &p.paths(),
    )
    .unwrap();
    let changes = rename_edits_to_changes(r);
    assert_eq!(
        p.after(&changes, "other.py"),
        other,
        "the function is not a method: untyped receivers are not its calls"
    );
    // A method's keyword through an untyped receiver is still matched by the
    // method name, as a dynamic usage: the Refactoring Preview lists it and
    // renames it when chosen; the plain rename keeps to certain usages.
    let plan = rename::rename_plan(
        &p.db,
        &p.file("main.py"),
        at(main, "name):\n        return"),
        "who",
        &p.paths(),
        rename::RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(
        plan.dynamic
            .iter()
            .map(|u| u.line_text.trim())
            .collect::<Vec<_>>(),
        vec!["obj.greet(name='y')", "thing.attr.greet(name='z')"]
    );
    let changes = rename_edits_to_changes(rename::rename_edits(
        plan.code.iter().chain(&plan.dynamic),
        "who",
    ));
    assert_eq!(p.after(&changes, "other.py"), "from missing_package import greet\n\ngreet(name='x')\n\n\ndef use(obj, thing):\n    obj.greet(who='y')\n    thing.attr.greet(who='z')\n");
    let r = rename::rename(
        &p.db,
        &p.file("main.py"),
        at(main, "name):\n        return"),
        "who",
        &p.paths(),
    )
    .unwrap();
    assert_eq!(p.after(&rename_edits_to_changes(r), "other.py"), other);
}

// ----- Rename: aliases -------------------------------------------------------

#[test]
fn renaming_through_a_redundant_alias_renames_the_alias_too() {
    let main = "def helper():\n    return 1\n";
    let api = "from main import helper as helper\n\nhelper()\n";
    let user = "from api import helper\n\nhelper()\n";
    let p = Project::new(&[("main.py", main), ("api.py", api), ("user.py", user)]);
    let r = rename::rename(
        &p.db,
        &p.file("main.py"),
        at(main, "helper"),
        "assist",
        &p.paths(),
    )
    .unwrap();
    let changes = rename_edits_to_changes(r);
    assert_eq!(
        p.after(&changes, "main.py"),
        "def assist():\n    return 1\n"
    );
    assert_eq!(
        p.after(&changes, "api.py"),
        "from main import assist as assist\n\nassist()\n"
    );
    assert_eq!(
        p.after(&changes, "user.py"),
        "from api import assist\n\nassist()\n"
    );
}

#[test]
fn renaming_an_alias_stays_with_that_alias() {
    let main = "def helper():\n    return 1\n";
    let a = "from main import helper as h\n\nh()\n";
    let b = "from main import helper as h\n\nh()\n";
    let c = "from a import h\n\nh()\n";
    let p = Project::new(&[("main.py", main), ("a.py", a), ("b.py", b), ("c.py", c)]);
    for start in [at(a, "h\n"), at(a, "h()")] {
        let r = rename::rename(&p.db, &p.file("a.py"), start, "hh", &p.paths()).unwrap();
        let changes = rename_edits_to_changes(r);
        assert_eq!(
            p.after(&changes, "a.py"),
            "from main import helper as hh\n\nhh()\n"
        );
        assert_eq!(p.after(&changes, "b.py"), b, "b.py has its own alias");
        assert_eq!(
            p.after(&changes, "c.py"),
            "from a import hh\n\nhh()\n",
            "c.py imports a's alias"
        );
        assert_eq!(p.after(&changes, "main.py"), main);
    }
    // Starting from the importer of the alias renames the alias where it is made.
    let r = rename::rename(&p.db, &p.file("c.py"), at(c, "h()"), "hh", &p.paths()).unwrap();
    let changes = rename_edits_to_changes(r);
    assert_eq!(
        p.after(&changes, "a.py"),
        "from main import helper as hh\n\nhh()\n"
    );
    assert_eq!(p.after(&changes, "c.py"), "from a import hh\n\nhh()\n");
    assert_eq!(p.after(&changes, "b.py"), b);
    // A name imported under an alias of another name is a different local name.
    let d = "from a import h as helper\n\nhelper()\n";
    let p = Project::new(&[("main.py", main), ("a.py", a), ("d.py", d)]);
    let r = rename::rename(
        &p.db,
        &p.file("main.py"),
        at(main, "helper"),
        "assist",
        &p.paths(),
    )
    .unwrap();
    let changes = rename_edits_to_changes(r);
    assert_eq!(
        p.after(&changes, "a.py"),
        "from main import assist as h\n\nh()\n"
    );
    assert_eq!(p.after(&changes, "d.py"), d);
}

// ----- Rename: libraries -----------------------------------------------------

#[test]
fn an_in_project_virtualenv_is_a_library() {
    let p = DiskProject::new(&[
        (
            "main.py",
            "from extlib import fetch\nimport extlib\n\nfetch()\nextlib.fetch()\n",
        ),
        (".venv/pyvenv.cfg", "home = /usr/bin\n"),
        (
            ".venv/lib/python3.12/site-packages/extlib/__init__.py",
            "def fetch():\n    return 1\n",
        ),
        ("src/mypkg/__init__.py", "def tool():\n    return 2\n"),
        ("use.py", "from mypkg import tool\n\ntool()\n"),
    ]);
    // The venv's site-packages and an editable install's `src` are on the interpreter path.
    let site = p.path(".venv/lib/python3.12/site-packages");
    let db = Analysis::new(ModuleResolver::new(
        vec![p.root.clone()],
        vec![site, p.path("src")],
        (3, 12),
    ));
    let files = p.py_files();
    let main = db.file_for_path(&p.path("main.py")).unwrap();
    let text = main.text.to_string();
    let err = rename::rename(&db, &main, at(&text, "fetch()"), "get", &files).unwrap_err();
    assert!(err.contains("library"), "{err}");
    let err = rename::rename(
        &db,
        &main,
        at(&text, "extlib.fetch") + TextSize::new(7),
        "get",
        &files,
    )
    .unwrap_err();
    assert!(err.contains("library"), "{err}");
    // Project code on the interpreter path (an editable install) can still be renamed.
    let use_ = db.file_for_path(&p.path("use.py")).unwrap();
    let text = use_.text.to_string();
    assert!(rename::rename(&db, &use_, at(&text, "tool()"), "instrument", &files).is_ok());
}

// ----- Extract Variable: occurrences ----------------------------------------

#[test]
fn self_mutating_occurrences_are_not_merged() {
    let src = "def f(items):\n    a = items.pop()\n    b = items.pop()\n    return a, b\n";
    let p = Project::new(&[("m.py", src)]);
    assert_eq!(
        refactor::extract_occurrences(&p.file("m.py"), sel(src, "items.pop()"), false).len(),
        1
    );
    let src = "def g(it):\n    a = next(it)\n    b = next(it)\n    return a, b\n";
    let p = Project::new(&[("m.py", src)]);
    let f = p.file("m.py");
    assert_eq!(
        refactor::extract_occurrences(&f, sel(src, "next(it)"), false).len(),
        1
    );
    // The variable must not be called `next`: `next = next(it)` raises UnboundLocalError.
    let r = refactor::extract_variable(&p.db, &f, sel(src, "next(it)"), false).unwrap();
    assert_ne!(r.name, "next");
    assert_eq!(
        apply_edits(src, &r.edits),
        format!(
            "def g(it):\n    {0} = next(it)\n    a = {0}\n    b = next(it)\n    return a, b\n",
            r.name
        )
    );
    // Side-effect-free builtins still merge.
    let src = "def h(items):\n    a = len(items)\n    b = len(items)\n    return a, b\n";
    let p = Project::new(&[("m.py", src)]);
    assert_eq!(
        refactor::extract_occurrences(&p.file("m.py"), sel(src, "len(items)"), false).len(),
        2
    );
}

#[test]
fn a_mutation_through_an_alias_is_a_barrier() {
    let src = "def f(items, x):\n    a = len(items)\n    other = items\n    other.append(x)\n    b = len(items)\n    return a, b\n";
    let p = Project::new(&[("m.py", src)]);
    assert_eq!(
        refactor::extract_occurrences(&p.file("m.py"), sel(src, "len(items)"), false).len(),
        1
    );
    // Rebinding the alias does not change the original.
    let src = "def f(items, x):\n    a = len(items)\n    other = items\n    other = [x]\n    b = len(items)\n    return a, b, other\n";
    let p = Project::new(&[("m.py", src)]);
    assert_eq!(
        refactor::extract_occurrences(&p.file("m.py"), sel(src, "len(items)"), false).len(),
        2
    );
}

#[test]
fn extracted_names_do_not_shadow_names_the_scope_uses() {
    // `next = obj.next` would make `next(it)` above it fail.
    let src = "def f(obj, it):\n    a = next(it)\n    b = obj.next\n    return a, b\n";
    let p = Project::new(&[("m.py", src)]);
    let r =
        refactor::extract_variable(&p.db, &p.file("m.py"), sel(src, "obj.next"), false).unwrap();
    assert_ne!(r.name, "next");
}

#[test]
fn replace_all_is_preselected_only_for_side_effect_free_expressions() {
    let src = "def f(items, k):\n    a = len(items) + 1\n    b = len(items) + 1\n    c = compute(items)\n    d = compute(items)\n    e = 0\n    g = 0\n    return a, b, c, d, e, g\n";
    let p = Project::new(&[("m.py", src)]);
    let f = p.file("m.py");
    assert!(refactor::replace_all_by_default(
        &f,
        sel(src, "len(items) + 1")
    ));
    // An unknown call may have side effects: "All" is offered but not preselected.
    assert_eq!(
        refactor::extract_occurrences(&f, sel(src, "compute(items)"), false).len(),
        2
    );
    assert!(!refactor::replace_all_by_default(
        &f,
        sel(src, "compute(items)")
    ));
    assert!(!refactor::replace_all_by_default(&f, sel(src, "0")));
}

// ----- Analysis invalidation -----------------------------------------------

#[test]
fn batch_invalidation_is_linear_in_the_number_of_cached_files() {
    let dir = tempfile::TempDir::with_prefix("rustpy-invalidate").unwrap();
    let n = 6000;
    let paths: Vec<PathBuf> = (0..n)
        .map(|i| dir.path().join(format!("m{i}.py")))
        .collect();
    for p in &paths {
        std::fs::write(p, "X = 1\n").unwrap();
    }
    let db = Analysis::for_project(dir.path());
    for p in &paths {
        assert!(db.file_for_path(p).is_some());
    }
    // `black .` rewrote every file: one watcher batch.
    for p in &paths {
        std::fs::write(p, "X = 2\n").unwrap();
    }
    let start = std::time::Instant::now();
    db.invalidate_paths(&paths);
    let took = start.elapsed();
    // One pass: milliseconds (the per-path scan took seconds for this many files).
    assert!(
        took < std::time::Duration::from_millis(1500),
        "invalidating {n} paths took {took:?}"
    );
    assert_eq!(&*db.module("m17").unwrap().text, "X = 2\n");
    assert_eq!(&*db.file_for_path(&paths[42]).unwrap().text, "X = 2\n");
}

#[test]
fn batch_invalidation_handles_directories_and_new_files() {
    let dir = tempfile::TempDir::with_prefix("rustpy-invalidate").unwrap();
    std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
    std::fs::write(dir.path().join("pkg/__init__.py"), "").unwrap();
    std::fs::write(dir.path().join("pkg/a.py"), "A = 1\n").unwrap();
    std::fs::write(dir.path().join("top.py"), "T = 1\n").unwrap();
    let db = Analysis::for_project(dir.path());
    assert_eq!(&*db.module("pkg.a").unwrap().text, "A = 1\n");
    assert!(db.module("late").is_none());
    // The package directory was replaced and a module appeared.
    std::fs::write(dir.path().join("pkg/a.py"), "A = 2\n").unwrap();
    std::fs::write(dir.path().join("late.py"), "L = 1\n").unwrap();
    db.invalidate_paths(&[
        dir.path().join("pkg"),
        dir.path().join("late.py"),
        dir.path().join("late.py"),
    ]);
    assert_eq!(&*db.module("pkg.a").unwrap().text, "A = 2\n");
    assert_eq!(&*db.module("late").unwrap().text, "L = 1\n");
    assert_eq!(&*db.module("top").unwrap().text, "T = 1\n");
}

#[test]
fn a_namespace_package_gaining_an_init_is_refreshed_by_its_watcher_event() {
    let dir = tempfile::TempDir::with_prefix("rustpy-ns").unwrap();
    std::fs::create_dir_all(dir.path().join("ns")).unwrap();
    std::fs::write(dir.path().join("ns/m.py"), "").unwrap();
    let db = Analysis::for_project(dir.path());
    assert_eq!(&*db.module("ns").unwrap().text, "");
    let init = dir.path().join("ns/__init__.py");
    std::fs::write(&init, "Z = 4\n").unwrap();
    db.invalidate_path(&init);
    assert_eq!(&*db.module("ns").unwrap().text, "Z = 4\n");
    // The same through a batch.
    std::fs::create_dir_all(dir.path().join("ns2")).unwrap();
    std::fs::write(dir.path().join("ns2/m.py"), "").unwrap();
    assert_eq!(&*db.module("ns2").unwrap().text, "");
    std::fs::write(dir.path().join("ns2/__init__.py"), "W = 5\n").unwrap();
    db.invalidate_paths(&[dir.path().join("ns2/__init__.py")]);
    assert_eq!(&*db.module("ns2").unwrap().text, "W = 5\n");
}

#[test]
fn submodule_imports_in_a_package_are_not_reexports() {
    // `from . import core` in pkg/__init__.py: `from pkg import core` elsewhere
    // loads the submodule by itself, so Safe Delete must not block or edit it.
    let p = DiskProject::new(&[
        ("pkg/__init__.py", "from . import core\n"),
        ("pkg/core.py", "def run():\n    return 1\n"),
        ("main.py", "from pkg import core\ncore.run()\n"),
    ]);
    let db = Analysis::new(ModuleResolver::new(vec![p.root.clone()], vec![], (3, 12)));
    let files = p.py_files();
    let init = db.file_for_path(&p.path("pkg/__init__.py")).unwrap();
    let off = TextSize::new(init.text.find("core").unwrap() as u32);
    if let Some(usages::Symbol::Name { def, .. }) = usages::symbol_at(&db, &init, off) {
        assert!(
            rustpy_search::usages::reexport_usages(&db, &def, &files).is_empty(),
            "submodule imports are not re-exports"
        );
    } else {
        panic!("no symbol at `core`");
    }
}

#[test]
fn class_attributes_used_through_attributes_are_not_deleted_or_inlined() {
    // A class attribute is a member: `self.size` and `Unit.size` use it, so
    // Safe Delete must list them and Inline Variable must not drop the
    // declaration (both only saw bare-name uses in the class body).
    let src = "class Unit:\n    size: float = 1.5\n    unused = 0\n\n    def over(self):\n        return self.size\n\n\nprint(Unit.size)\n";
    let p = Project::new(&[("u.py", src)]);
    match refactor::safe_delete(&p.db, &p.file("u.py"), at(src, "size:"), &p.paths()) {
        Err(SafeDeleteError::Used { usages, .. }) => assert_eq!(usages.len(), 2, "{usages:?}"),
        other => panic!("{:?}", other.map(|r| r.edits)),
    }
    let r = refactor::safe_delete(&p.db, &p.file("u.py"), at(src, "unused"), &p.paths()).unwrap();
    assert_eq!(apply_edits(src, &r.edits), "class Unit:\n    size: float = 1.5\n\n    def over(self):\n        return self.size\n\n\nprint(Unit.size)\n");
    assert!(refactor::inline_variable(&p.db, &p.file("u.py"), at(src, "size:")).is_err());
}
