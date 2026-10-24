//! Additive captured-authority controls; original public Never-premise tests unchanged.
use rustpy_semantic::{
    db::Analysis,
    flow::{flow_of_with_db, flow_scope},
};
fn tail(source: &str, module: Option<&str>, reachable: bool) {
    let directory = tempfile::TempDir::with_prefix("rustpy-meta-controls").unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, source).unwrap();
    if let Some(module) = module {
        std::fs::write(directory.path().join("meta.py"), module).unwrap();
    }
    let db = Analysis::for_project(directory.path());
    let _ = db.module("typing");
    if module.is_some() {
        let _ = db.module("meta");
    }
    let snapshot = db
        .snapshot(
            std::slice::from_ref(&path),
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    let db = snapshot.analysis();
    let file = db.file_for_path(&path).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let flow = flow_of_with_db(db, &file, flow_scope(&file, reference.scope).unwrap());
    assert_eq!(
        flow.refs.contains_key(&reference.range.start()),
        reachable,
        "{source}"
    );
}
const META: &str = "from __future__ import annotations\nfrom typing import Never\nclass Meta(type):\n    def __call__(cls)->Never: raise RuntimeError\n";
const TAIL: &str = "def f():\n    marker=1\n    C()\n    marker\n";
#[test]
fn local_and_captured_project_metaclass_aliases_terminate() {
    tail(
        &format!("{META}Alias=Meta\nclass C(metaclass=Alias): pass\n{TAIL}"),
        None,
        false,
    );
    tail(
        &format!("from meta import Meta as Alias\nclass C(metaclass=Alias): pass\n{TAIL}"),
        Some(META),
        false,
    );
}
#[test]
fn mutated_metaclass_member_retires_terminal_authority() {
    tail(
        &format!("{META}Meta.__call__=lambda cls:None\nclass C(metaclass=Meta): pass\n{TAIL}"),
        None,
        true,
    );
}
#[test]
fn unknown_namespace_factory_and_shadowed_type_remain_reachable() {
    let prepared = "from __future__ import annotations\nfrom typing import Never\nclass Meta(type):\n    def __prepare__(name,bases): return {}\n    def __call__(cls)->Never: raise RuntimeError\n";
    tail(
        &format!("{prepared}class C(metaclass=Meta): pass\n{TAIL}"),
        None,
        true,
    );
    tail(
        &format!(
            "{}class C(metaclass=Meta): pass\n{TAIL}",
            META.replace("class Meta(type):", "type=object\nclass Meta(type):")
        ),
        None,
        true,
    );
}
