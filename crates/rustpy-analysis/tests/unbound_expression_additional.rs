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
fn pep695_alias_value_and_bound_remain_lazy() {
    check("    type Alias[T: die()] = die()\n", true);
}
#[test]
fn pep695_function_and_class_bounds_remain_lazy() {
    check("    def inner[T: die()]():\n        pass\n", true);
    check("    class Inner[T: die()]:\n        pass\n", true);
}
#[test]
fn implicit_function_and_class_decorators_terminate() {
    check("    @die\n    def inner():\n        pass\n", false);
    check("    @die\n    class Inner:\n        pass\n", false);
}
#[test]
fn defaults_precede_all_function_annotations() {
    check(
        "    def inner(first: marker, second=die()):\n        pass\n",
        false,
    );
    check(
        "    def inner(first: die(), second=marker):\n        pass\n",
        true,
    );
}
#[test]
fn future_annotations_do_not_evaluate_calls() {
    unbound(
        "from __future__ import annotations\nfrom typing import NoReturn\ndef die()->NoReturn:\n    raise RuntimeError\ndef f():\n    def inner(x: die())->die():\n        pass\n    print(marker)\n    marker=1\n",
        "marker",
        1,
    );
}
#[test]
fn comprehension_store_operands_are_evaluated_before_filters() {
    for body in [
        "    values=[None for die().field in flag if marker]\n",
        "    values=[None for sink[die()] in flag if marker]\n",
    ] {
        let source = format!(
            "from typing import NoReturn\ndef die()->NoReturn:\n    raise RuntimeError\ndef f(flag,sink):\n{body}    print(marker)\n    marker=1\n"
        );
        unbound(&source, "marker", 1);
        let db = Analysis::for_project(Path::new("/analysis-expression-completions"));
        let id = db.set_file_text(Path::new("/analysis-expression-completions/m.py"), &source);
        let file = db.file(id).unwrap();
        let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
        let start = source.rfind("print(marker)").unwrap() + "print(".len();
        let ranges: Vec<_> = problems
            .iter()
            .filter(|problem| {
                problem.diagnostic.code == "PyUnboundLocalVariable"
                    && &source[usize::from(problem.diagnostic.range.start())
                        ..usize::from(problem.diagnostic.range.end())]
                        == "marker"
            })
            .map(|problem| {
                (
                    usize::from(problem.diagnostic.range.start()),
                    usize::from(problem.diagnostic.range.end()),
                )
            })
            .collect();
        assert_eq!(
            ranges,
            vec![(start, start + "marker".len())],
            "{source}\n{problems:#?}"
        );
    }
}
#[test]
fn decorated_containing_class_does_not_prove_terminal_method() {
    unbound(
        "from typing import NoReturn\ndef replace(cls):\n    class Replacement:\n        def stop(self): return None\n    return Replacement\n@replace\nclass C:\n    def stop(self)->NoReturn: raise RuntimeError\ndef f():\n    C().stop()\n    print(marker)\n    marker=1\n",
        "marker",
        1,
    );
}
#[test]
fn instance_lookup_override_does_not_prove_terminal_method() {
    unbound(
        "from typing import NoReturn\nclass C:\n    def __getattribute__(self,name): return lambda: None\n    def stop(self)->NoReturn: raise RuntimeError\ndef f():\n    C().stop()\n    print(marker)\n    marker=1\n",
        "marker",
        1,
    );
}
#[test]
fn inherited_lookup_override_retires_base_method_authority() {
    unbound(
        "from typing import NoReturn\nclass Base:\n    def stop(self)->NoReturn: raise RuntimeError\nclass C(Base):\n    def __getattribute__(self,name): return lambda: None\ndef f():\n    C().stop()\n    print(marker)\n    marker=1\n",
        "marker",
        1,
    );
}
#[test]
fn ordinary_genuine_method_remains_terminal() {
    unbound(
        "from typing import NoReturn\nclass C:\n    def stop(self)->NoReturn: raise RuntimeError\ndef f():\n    C().stop()\n    pass\n    print(marker)\n    marker=1\n",
        "marker",
        0,
    );
}

#[test]
fn replacement_constructor_does_not_prove_terminal_method() {
    unbound(
        "from typing import NoReturn\nclass R:\n    def stop(self): return None\nclass C:\n    def __new__(cls): return R()\n    def stop(self)->NoReturn: raise RuntimeError\ndef f():\n    C().stop()\n    print(marker)\n    marker=1\n",
        "marker",
        1,
    );
}
