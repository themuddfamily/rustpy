use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, known: bool, warning: bool) {
    let db = Analysis::for_project(Path::new("/reaching-reconciliation-controls"));
    let id = db.set_file_text(Path::new("/reaching-reconciliation-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let point = source.rfind("print(target)").unwrap() + "print(".len();
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|n| usize::from(n.text_range().start()) == point)
        .unwrap();
    let ids = Inferer::new(&db).reaching_bindings(&file, &name);
    assert_eq!(
        ids.is_some_and(|ids| !ids.is_empty()),
        known,
        "actual current reaching guarantee BEFORE inspection: {source}"
    );
    let actual: Vec<_> = inspections::inspect(&db, &file, &InspectionOptions::default())
        .into_iter()
        .filter(|p| p.diagnostic.code == "PyUnboundLocalVariable")
        .map(|p| {
            (
                p.diagnostic.code.to_string(),
                usize::from(p.diagnostic.range.start()),
                usize::from(p.diagnostic.range.end()),
            )
        })
        .collect();
    let expected = if warning {
        vec![("PyUnboundLocalVariable".to_owned(), point, point + 6)]
    } else {
        vec![]
    };
    assert_eq!(actual, expected, "{source}");
}
#[test]
fn empty_and_unknown_iteration_do_not_fabricate_local_runtime_bindings() {
    for source in [
        "def f():\n    for target in ():\n        pass\n    print(target)\n",
        "def f(values:list[int]):\n    for target in values:\n        pass\n    print(target)\n",
        "def f():\n    values=[(target:=1) for item in ()]\n    print(target)\n",
        "def f(source:list[int]):\n    values=[(target:=1) for item in source]\n    print(target)\n",
    ] {
        check(source, false, true);
    }
}
#[test]
fn parameter_and_function_bindings_are_runtime_values_but_annotations_and_deletes_are_not() {
    check("def f(target:int):\n    print(target)\n", true, false);
    check(
        "def f():\n    def target():pass\n    print(target)\n",
        true,
        false,
    );
    check("def f():\n    target:int\n    print(target)\n", false, true);
    check(
        "def f():\n    target=1\n    del target\n    print(target)\n",
        false,
        true,
    );
}
#[test]
fn globals_nonlocals_and_canonical_terminal_tails_keep_existing_scope_policy() {
    // Outer namespace lookups are not local definite-binding proofs and must
    // retain existing explicit global/nonlocal inspection policy.
    check(
        "target=1\ndef f():\n    global target\n    print(target)\n",
        false,
        false,
    );
    check(
        "def outer():\n    target=1\n    def inner():\n        nonlocal target\n        print(target)\n",
        false,
        false,
    );
    check(
        "from typing import Never\ndef stop()->Never:raise RuntimeError\ndef f():\n    for target in ():\n        pass\n    stop()\n    print(target)\n",
        false,
        false,
    );
}
