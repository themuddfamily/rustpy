//! Additive actual-inspection checks; the original ten CFG fixtures stay exact.
use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn unbound(source: &str, name: &str, count: usize) {
    let db = Analysis::for_project(Path::new("/analysis-completion-controls"));
    let id = db.set_file_text(Path::new("/analysis-completion-controls/m.py"), source);
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
    assert_eq!(actual, count, "{source}\n{problems:#?}");
}

#[test]
fn finally_continue_replaces_break_and_finally_break_replaces_continue() {
    // This infinite source is inspected only, never executed.
    unbound(
        "def f():\n    while True:\n        try:\n            break\n        finally:\n            continue\n    print(x)\n    x=1\n",
        "x",
        0,
    );
    unbound(
        "def f():\n    while True:\n        try:\n            continue\n        finally:\n            x=1\n            break\n    return x\n",
        "x",
        0,
    );
}

#[test]
fn finally_return_and_raise_override_every_pending_reason() {
    for reason in ["break", "continue", "return 0", "raise RuntimeError"] {
        for replacement in ["return 1", "raise ValueError"] {
            let source = format!(
                "def f():\n    while True:\n        try:\n            {reason}\n        finally:\n            {replacement}\n    print(x)\n    x=1\n"
            );
            unbound(&source, "x", 0);
        }
    }
}

#[test]
fn return_value_is_evaluated_before_finally_assignment() {
    unbound(
        "def f():\n    try:\n        return x\n    finally:\n        x=1\n",
        "x",
        1,
    );
}

#[test]
fn finally_reads_meet_all_completion_inputs_once() {
    unbound(
        "def f(flag):\n    try:\n        if flag:\n            x=1\n            return 0\n        raise RuntimeError\n    finally:\n        print(x)\n",
        "x",
        1,
    );
}

#[test]
fn actual_inner_caught_exception_does_not_escape_outer_try() {
    unbound(
        "def f():\n    try:\n        try:\n            raise RuntimeError\n        except:\n            x=1\n    except:\n        return x\n    return x\n",
        "x",
        0,
    );
}

#[test]
fn exception_target_deleted_before_outer_finally_on_break() {
    unbound(
        "def f():\n    while True:\n        try:\n            try:\n                raise RuntimeError\n            except RuntimeError as x:\n                break\n        finally:\n            print(x)\n",
        "x",
        1,
    );
}

#[test]
fn grouped_as_or_patterns_and_constant_guards_partition_actual_paths() {
    for pattern in [
        "(_)",
        "(_ as whole)",
        "(0 | _) as whole",
        "_ as whole if True",
    ] {
        let source = format!(
            "def f(subject):\n    match subject:\n        case {pattern}:\n            x=1\n    return x\n"
        );
        unbound(&source, "x", 0);
    }
    unbound(
        "def f(subject):\n    match subject:\n        case _ if False:\n            print(x)\n        case _:\n            x=1\n    return x\n",
        "x",
        0,
    );
}

#[test]
fn true_elif_and_false_while_do_not_publish_dead_breaks_to_finally() {
    unbound(
        "def f():\n    while True:\n        try:\n            if False:\n                break\n            elif False:\n                print(x)\n                break\n            elif True:\n                x=1\n                break\n            else:\n                print(x)\n        finally:\n            pass\n    return x\n",
        "x",
        0,
    );
    unbound(
        "def f():\n    while True:\n        try:\n            while False:\n                print(x)\n                break\n            x=1\n            break\n        finally:\n            pass\n    return x\n",
        "x",
        0,
    );
}

#[test]
fn pure_irrefutable_match_does_not_fabricate_pre_assignment_exception() {
    unbound(
        "def f(subject):\n    try:\n        match subject:\n            case (_ as whole) if True:\n                x=1\n    finally:\n        print(x)\n",
        "x",
        0,
    );
}

#[test]
fn failed_guard_keeps_captures_of_actual_irrefutable_pattern() {
    unbound(
        "def f(subject):\n    match subject:\n        case _ as whole if False:\n            pass\n    return whole\n",
        "whole",
        0,
    );
    unbound(
        "def f(subject, flag):\n    match subject:\n        case (_ as whole) if flag:\n            pass\n    return whole\n",
        "whole",
        0,
    );
}
