use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
fn tail(source: &str, expected: bool) {
    let db = Analysis::for_project(Path::new("/with-cfg"));
    let id = db.set_file_text(Path::new("/with-cfg/m.py"), source);
    let file = db.file(id).unwrap();
    let refs: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|r| r.name == "marker" && !r.is_write)
        .collect();
    assert_eq!(refs.len(), 1);
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, refs[0].scope).unwrap());
    assert_eq!(
        info.refs.contains_key(&refs[0].range.start()),
        expected,
        "{source}"
    );
}
#[test]
fn suppress_matching_raise() {
    tail(
        "from contextlib import suppress\ndef f():\n    marker=1\n    with suppress(ValueError):\n        raise ValueError\n    return marker\n",
        true,
    );
}
#[test]
fn suppress_other_raise() {
    tail(
        "from contextlib import suppress\ndef f():\n    marker=1\n    with suppress(ValueError):\n        raise RuntimeError\n    return marker\n",
        false,
    );
}
#[test]
fn never_enter() {
    tail(
        "from typing import NoReturn\nclass C:\n    def __enter__(self)->NoReturn:\n        raise RuntimeError\n    def __exit__(self,*args):\n        return False\ndef f():\n    marker=1\n    with C() as value:\n        pass\n    return marker\n",
        false,
    );
}
#[test]
fn ordinary_false_exit() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return False\ndef f():\n    marker=1\n    with C():\n        pass\n    return marker\n",
        true,
    );
}
#[test]
fn false_exit_retains_raise() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return False\ndef f():\n    marker=1\n    with C():\n        raise ValueError\n    return marker\n",
        false,
    );
}
#[test]
fn unknown_exit_allows_suppressed_tail() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return flag\ndef f():\n    marker=1\n    with C():\n        raise ValueError\n    return marker\n",
        true,
    );
}
