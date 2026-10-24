use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;
fn tail(source: &str, expected: usize) {
    let db = Analysis::for_project(Path::new("/with-analysis"));
    let id = db.set_file_text(Path::new("/with-analysis/m.py"), source);
    let file = db.file(id).unwrap();
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    let count = problems
        .iter()
        .filter(|p| {
            p.diagnostic.code == "PyUnboundLocalVariable"
                && &source
                    [usize::from(p.diagnostic.range.start())..usize::from(p.diagnostic.range.end())]
                    == "marker"
        })
        .count();
    assert_eq!(count, expected, "{source} {problems:#?}");
}
#[test]
fn suppress_matching_raise() {
    tail(
        "from contextlib import suppress\ndef f():\n    with suppress(ValueError):\n        raise ValueError\n    print(marker)\n    marker=1\n",
        1,
    );
}
#[test]
fn suppress_other_raise() {
    tail(
        "from contextlib import suppress\ndef f():\n    with suppress(ValueError):\n        raise RuntimeError\n    print(marker)\n    marker=1\n",
        0,
    );
}
#[test]
fn never_enter() {
    tail(
        "from typing import NoReturn\nclass C:\n    def __enter__(self)->NoReturn:\n        raise RuntimeError\n    def __exit__(self,*args):\n        return False\ndef f():\n    with C() as value:\n        pass\n    print(marker)\n    marker=1\n",
        0,
    );
}
#[test]
fn ordinary_false_exit() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return False\ndef f():\n    with C():\n        pass\n    print(marker)\n    marker=1\n",
        1,
    );
}
#[test]
fn false_exit_retains_raise() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return False\ndef f():\n    with C():\n        raise ValueError\n    print(marker)\n    marker=1\n",
        0,
    );
}
#[test]
fn unknown_exit_allows_suppressed_tail() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return flag\ndef f():\n    with C():\n        raise ValueError\n    print(marker)\n    marker=1\n",
        1,
    );
}
