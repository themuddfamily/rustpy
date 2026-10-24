//! Public-consumer expression completion regressions, source preparation only.
use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;

#[test]
fn all_defaults_execute_before_any_eager_annotations() {
    let source = "from typing import NoReturn\ndef die() -> NoReturn:\n    raise RuntimeError\ndef f():\n    first=1\n    def inner(a: die(), b=first):\n        pass\n";
    let db = Analysis::for_project(Path::new("/header-order"));
    let id = db.set_file_text(Path::new("/header-order/m.py"), source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "first" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert!(
        info.refs.contains_key(&reference.range.start()),
        "default executes before terminal annotation"
    );
}

#[test]
fn future_annotations_do_not_evaluate_function_header_call() {
    let source = "from __future__ import annotations\nfrom typing import NoReturn\ndef die() -> NoReturn:\n    raise RuntimeError\ndef f():\n    marker=1\n    def inner(arg: die()):\n        pass\n    return marker\n";
    let db = Analysis::for_project(Path::new("/future-header"));
    let id = db.set_file_text(Path::new("/future-header/m.py"), source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert!(
        info.refs.contains_key(&reference.range.start()),
        "annotation stays deferred"
    );
}

#[test]
fn comprehension_target_terminates_before_element_load() {
    let source = "from typing import NoReturn\ndef die() -> NoReturn:\n    raise RuntimeError\ndef f(obj):\n    marker=1\n    value=[marker for obj[die()] in [1]]\n    return marker\n";
    let db = Analysis::for_project(Path::new("/comprehension-target"));
    let id = db.set_file_text(Path::new("/comprehension-target/m.py"), source);
    let file = db.file(id).unwrap();
    let mut loads: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|r| r.name == "marker" && !r.is_write)
        .collect();
    loads.sort_by_key(|reference| reference.range.start());
    assert_eq!(loads.len(), 2, "element and tail are actual separate reads");
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, loads[1].scope).unwrap());
    assert!(
        !info.refs.contains_key(&loads[0].range.start()),
        "target runs before element"
    );
}

fn check(body: &str, tail_reachable: bool) {
    let source = format!("from typing import NoReturn\ndef die() -> NoReturn:\n    raise RuntimeError\ndef f(flag, sink):\n    marker=1\n{body}    return marker\n");
    let db = Analysis::for_project(Path::new("/expression-completions"));
    let id = db.set_file_text(Path::new("/expression-completions/m.py"), &source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert_eq!(
        info.refs.contains_key(&reference.range.start()),
        tail_reachable,
        "{source}"
    );
}

#[test]
fn implicit_never_decorator_application_terminates() {
    check("    @die\n    def inner():\n        pass\n", false);
}

#[test]
fn pep695_type_alias_rhs_is_lazy() {
    check("    type Alias = die()\n", true);
}

#[test]
fn pep695_type_parameter_bound_is_lazy() {
    check("    def inner[T: die()](arg: T):\n        pass\n", true);
}

#[test]
fn future_annotations_do_not_evaluate_module_annotated_assignment() {
    let source = "from __future__ import annotations\nfrom typing import NoReturn\ndef die() -> NoReturn:\n    raise RuntimeError\nmarker=1\nvalue: die() = 2\nmarker\n";
    let db = Analysis::for_project(Path::new("/future-module-annotation"));
    let id = db.set_file_text(Path::new("/future-module-annotation/m.py"), source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert!(
        info.refs.contains_key(&reference.range.start()),
        "module annotation stays deferred while store completes"
    );
}
