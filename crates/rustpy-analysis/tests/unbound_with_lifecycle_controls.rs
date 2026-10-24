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
fn exit_true_does_not_suppress_return() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return True\ndef f():\n    with C():\n        return\n    print(marker)\n    marker=1\n",
        0,
    );
}
#[test]
fn exit_true_preserves_break() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return True\ndef f():\n    while True:\n        with C():\n            break\n    print(marker)\n    marker=1\n",
        1,
    );
}
#[test]
fn exit_true_preserves_continue() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return True\ndef f():\n    while True:\n        with C():\n            continue\n    print(marker)\n    marker=1\n",
        0,
    );
}
#[test]
fn failed_enter_does_not_invoke_own_true_exit() {
    tail(
        "from typing import NoReturn\nclass C:\n    def __enter__(self)->NoReturn:\n        raise RuntimeError\n    def __exit__(self,*args):\n        return True\ndef f():\n    with C():\n        pass\n    print(marker)\n    marker=1\n",
        0,
    );
}
#[test]
fn exit_never_cancels_normal_body() {
    tail(
        "from typing import NoReturn\nclass C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args)->NoReturn:\n        raise RuntimeError\ndef f():\n    with C():\n        pass\n    print(marker)\n    marker=1\n",
        0,
    );
}
#[test]
fn outer_true_exit_can_suppress_inner_enter_failure() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return True\nfrom typing import NoReturn\nclass D:\n    def __enter__(self)->NoReturn:\n        raise RuntimeError\n    def __exit__(self,*args):\n        return False\ndef f():\n    with C(), D():\n        pass\n    print(marker)\n    marker=1\n",
        1,
    );
}
