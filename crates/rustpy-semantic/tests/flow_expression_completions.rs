//! Public-consumer expression completion regressions, source preparation only.
use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;

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
fn assignment_rhs_terminates() {
    check("    value=die()\n", false);
}

#[test]
fn if_condition_terminates() {
    check("    if die():\n        pass\n", false);
}

#[test]
fn for_iterable_terminates() {
    check("    for value in die():\n        pass\n", false);
}

#[test]
fn with_context_terminates() {
    check("    with die():\n        pass\n", false);
}

#[test]
fn nested_argument_terminates() {
    check("    sink(1, die(), marker)\n", false);
}

#[test]
fn match_subject_terminates() {
    check(
        "    match die():\n        case _:\n            pass\n",
        false,
    );
}

#[test]
fn match_guard_terminates() {
    check(
        "    match 1:\n        case _ if die():\n            pass\n",
        false,
    );
}

#[test]
fn false_and_does_not_execute_terminal_rhs() {
    check("    value=False and die()\n", true);
}

#[test]
fn true_or_does_not_execute_terminal_rhs() {
    check("    value=True or die()\n", true);
}

#[test]
fn true_and_executes_terminal_rhs() {
    check("    value=True and die()\n", false);
}

#[test]
fn false_or_executes_terminal_rhs() {
    check("    value=False or die()\n", false);
}

#[test]
fn conditional_unselected_terminal_branch_is_not_evaluated() {
    check("    value=die() if False else 1\n", true);
}

#[test]
fn conditional_selected_terminal_branch_terminates() {
    check("    value=die() if True else 1\n", false);
}

#[test]
fn conditional_unknown_has_normal_alternative() {
    check("    value=die() if flag else 1\n", true);
}

#[test]
fn both_conditional_branches_terminate() {
    check("    value=die() if flag else die()\n", false);
}

#[test]
fn call_arguments_stop_at_first_terminal_argument() {
    let source = "from typing import NoReturn\ndef die() -> NoReturn:\n    raise RuntimeError\ndef f(sink):\n    first=1\n    last=2\n    sink(first, die(), last)\n";
    let db = Analysis::for_project(Path::new("/expression-order"));
    let id = db.set_file_text(Path::new("/expression-order/m.py"), source);
    let file = db.file(id).unwrap();
    let first = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "first" && !r.is_write)
        .unwrap();
    let last = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "last" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, first.scope).unwrap());
    assert!(
        info.refs.contains_key(&first.range.start()),
        "earlier argument is evaluated"
    );
    assert!(
        !info.refs.contains_key(&last.range.start()),
        "later argument is not evaluated"
    );
}
