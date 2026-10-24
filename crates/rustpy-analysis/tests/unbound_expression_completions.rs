//! Actual inspection consumers; these Python functions are inspected, not run.
use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn unbound(source: &str, name: &str, expected: usize) {
    let db = Analysis::for_project(Path::new("/analysis-expression-completions"));
    let id = db.set_file_text(Path::new("/analysis-expression-completions/m.py"), source);
    let file = db.file(id).unwrap();
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    let actual = problems
        .iter()
        .filter(|problem| {
            problem.diagnostic.code == "PyUnboundLocalVariable"
                && &source[usize::from(problem.diagnostic.range.start())
                    ..usize::from(problem.diagnostic.range.end())]
                    == name
        })
        .count();
    assert_eq!(actual, expected, "{source}\n{problems:#?}");
}
fn check(body: &str, reachable: bool) {
    let source = format!(
        "from typing import NoReturn\ndef die() -> NoReturn:\n    raise RuntimeError\ndef f(flag, sink):\n{body}    print(marker)\n    marker=1\n"
    );
    unbound(&source, "marker", usize::from(reachable));
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
fn while_condition_terminates() {
    check("    while die():\n        pass\n", false);
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
    let source = "from typing import NoReturn\ndef die() -> NoReturn:\n    raise RuntimeError\ndef f(sink):\n    sink(first, die(), last)\n    first=1\n    last=2\n";
    unbound(source, "first", 1);
    unbound(source, "last", 0);
}
#[test]
fn lazy_deferred_and_zero_iteration_bodies_do_not_unconditionally_terminate() {
    check("    value=lambda: die()\n", true);
    check("    value=(die() for item in ())\n", true);
    check("    value=[die() for item in ()]\n", true);
    check("    value=[die() for item in flag]\n", true);
}
#[test]
fn lambda_default_and_generator_outer_iterable_are_evaluated_now() {
    check("    value=lambda argument=die(): argument\n", false);
    check("    value=(item for item in die())\n", false);
}
#[test]
fn assert_true_never_evaluates_failure_only_message() {
    unbound(
        "def f():\n    assert True, marker\n    marker=1\n",
        "marker",
        0,
    );
    unbound(
        "def f():\n    assert False, marker\n    marker=1\n",
        "marker",
        1,
    );
}
#[test]
fn parenthesized_bare_local_annotation_never_reads_the_unassigned_name() {
    unbound("def f():\n    (marker): int\n    marker=1\n", "marker", 0);
}
