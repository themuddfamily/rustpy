//! Generated ordering dunders and class-form NamedTuple tuple behaviour must
//! not produce operator/subscript false positives.
use rustpy_analysis::inspections::{self, InspectionOptions, Problem};
use rustpy_semantic::db::Analysis;
use std::path::Path;
fn inspect(source: &str) -> Vec<Problem> {
    let db = Analysis::for_project(Path::new("/synthesized-ordering"));
    let id = db.set_file_text(Path::new("/synthesized-ordering/m.py"), source);
    inspections::inspect(&db, &db.file(id).unwrap(), &InspectionOptions::default())
}
fn has(problems: &[Problem], message: &str) -> bool {
    problems
        .iter()
        .any(|p| p.diagnostic.code == "PyTypeChecker" && p.diagnostic.message.contains(message))
}
#[test]
fn generated_ordering_methods_support_comparisons() {
    let p = inspect(
        "import attr\nfrom dataclasses import dataclass\nfrom functools import total_ordering\n@dataclass(order=True)\nclass V:\n    a: int\nV(1) < V(2)\nV(1) >= V(2)\nclass SubV(V):\n    pass\nSubV(1) <= SubV(2)\n@total_ordering\nclass W:\n    def __init__(self, v: int) -> None:\n        self.v = v\n    def __eq__(self, other: object) -> bool:\n        return True\n    def __lt__(self, other: 'W') -> bool:\n        return True\nW(1) > W(2)\nW(1) >= W(2)\nW(1) <= W(2)\n@attr.s\nclass A:\n    x = attr.ib()\nA(1) <= A(2)\n",
    );
    assert!(!has(&p, "Comparison"), "{p:?}");
}
#[test]
fn unordered_dataclass_comparison_is_still_reported() {
    let p = inspect(
        "from dataclasses import dataclass\n@dataclass\nclass U:\n    a: int\nU(1) < U(2)\n@dataclass(order=True, eq=False)\nclass E:\n    a: int\n",
    );
    assert!(has(&p, "Comparison '<'"), "{p:?}");
}
#[test]
fn class_namedtuple_indexing_and_unpacking_are_tuple_operations() {
    let p = inspect(
        "from typing import NamedTuple\nclass Point(NamedTuple):\n    x: int\n    y: str\np = Point(1, 'a')\nfirst: int = p[0]\nlast: str = p[-1]\nhead = p[:1]\na, b = p\nn: int = a + len(p)\nfor item in p:\n    print(item)\n",
    );
    assert!(
        !has(&p, "not subscriptable") && !has(&p, "Expected type") && !has(&p, "not iterable"),
        "{p:?}"
    );
}
