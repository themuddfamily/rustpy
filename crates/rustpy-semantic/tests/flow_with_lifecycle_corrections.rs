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
fn exception_does_not_suppress_baseexception() {
    tail(
        "from contextlib import suppress\ndef f():\n    marker=1\n    with suppress(Exception):\n        raise BaseException\n    return marker\n",
        false,
    );
}
#[test]
fn exception_does_not_suppress_systemexit() {
    tail(
        "from contextlib import suppress\ndef f():\n    marker=1\n    with suppress(Exception):\n        raise SystemExit\n    return marker\n",
        false,
    );
}
#[test]
fn exception_does_not_suppress_keyboardinterrupt() {
    tail(
        "from contextlib import suppress\ndef f():\n    marker=1\n    with suppress(Exception):\n        raise KeyboardInterrupt\n    return marker\n",
        false,
    );
}
#[test]
fn exception_does_not_suppress_generatorexit() {
    tail(
        "from contextlib import suppress\ndef f():\n    marker=1\n    with suppress(Exception):\n        raise GeneratorExit\n    return marker\n",
        false,
    );
}
#[test]
fn unknown_exit_failure_carries_completed_body_store_to_handler() {
    let source = "class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return flag\ndef f():\n    marker=1\n    try:\n        with C():\n            marker=2\n            return\n    except:\n        return marker\n";
    let db = Analysis::for_project(Path::new("/with-exit-failure"));
    let id = db.set_file_text(Path::new("/with-exit-failure/m.py"), source);
    let file = db.file(id).unwrap();
    let read = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, read.scope).unwrap());
    let completed = file
        .scopes
        .bindings
        .iter()
        .enumerate()
        .find(|(_, binding)| {
            binding.name == "marker"
                && source
                    [usize::from(binding.node_range.start())..usize::from(binding.node_range.end())]
                    .contains("marker=2")
        })
        .unwrap();
    let expected = rustpy_semantic::scope::BindingId(completed.0 as u32);
    assert!(
        info.refs[&read.range.start()]
            .iter()
            .any(|reach| reach.binding == Some(expected)),
        "actual exit failure reaches handler AFTER body store"
    );
}
