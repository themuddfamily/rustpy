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
fn exit_true_does_not_suppress_return() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return True\ndef f():\n    marker=1\n    with C():\n        return\n    return marker\n",
        false,
    );
}
#[test]
fn exit_true_preserves_break() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return True\ndef f():\n    marker=1\n    while True:\n        with C():\n            break\n    return marker\n",
        true,
    );
}
#[test]
fn exit_true_preserves_continue() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return True\ndef f():\n    marker=1\n    while True:\n        with C():\n            continue\n    return marker\n",
        false,
    );
}
#[test]
fn failed_enter_does_not_invoke_own_true_exit() {
    tail(
        "from typing import NoReturn\nclass C:\n    def __enter__(self)->NoReturn:\n        raise RuntimeError\n    def __exit__(self,*args):\n        return True\ndef f():\n    marker=1\n    with C():\n        pass\n    return marker\n",
        false,
    );
}
#[test]
fn exit_never_cancels_normal_body() {
    tail(
        "from typing import NoReturn\nclass C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args)->NoReturn:\n        raise RuntimeError\ndef f():\n    marker=1\n    with C():\n        pass\n    return marker\n",
        false,
    );
}
#[test]
fn outer_true_exit_can_suppress_inner_enter_failure() {
    tail(
        "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return True\nfrom typing import NoReturn\nclass D:\n    def __enter__(self)->NoReturn:\n        raise RuntimeError\n    def __exit__(self,*args):\n        return False\ndef f():\n    marker=1\n    with C(), D():\n        pass\n    return marker\n",
        true,
    );
}
