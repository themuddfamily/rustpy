//! Additional class namespace/control oracles; original nine stay unchanged.
use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
fn inspect(source: &str, test: impl FnOnce(&rustpy_semantic::db::FileInfo, &flow::FlowInfo)) {
    let db = Analysis::for_project(Path::new("/class-role-controls"));
    let id = db.set_file_text(Path::new("/class-role-controls/m.py"), source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    test(&file, &info);
}
#[test]
fn class_type_parameter_body_does_not_read_later_outer_fastlocal() {
    inspect(
        "def f():\n    marker=1\n    class C[T]:\n        value=T\n    T=0\n    return marker\n",
        |file, info| {
            let marker = file
                .scopes
                .references
                .iter()
                .find(|r| r.name == "marker" && !r.is_write)
                .unwrap();
            let parameter = file
                .scopes
                .references
                .iter()
                .find(|r| r.name == "T" && !r.is_write)
                .unwrap();
            assert!(info.refs.contains_key(&marker.range.start()));
            assert!(
                !info.refs.contains_key(&parameter.range.start()),
                "annotation-scope T never resolves to outer fastlocal"
            );
        },
    );
}
#[test]
fn class_load_name_exit_before_own_store_uses_builtin() {
    inspect("def f():\n    marker=1\n    class C:\n        exit()\n        exit=lambda:None\n    return marker\n", |file, info| {
        let marker = file.scopes.references.iter().find(|r| r.name=="marker" && !r.is_write).unwrap();
        assert!(!info.refs.contains_key(&marker.range.start()), "captured ordinary builtin call terminates class construction");
    });
}
#[test]
fn class_cell_read_filters_unbound_normal_alternative() {
    inspect("def f(flag):\n    marker=1\n    if flag:\n        value=1\n    class C:\n        seen=value\n    marker=value\n    return marker\n", |file, info| {
        let mut values:Vec<_> = file.scopes.references.iter().filter(|r|r.name=="value" && !r.is_write).collect();
        values.sort_by_key(|r|r.range.start());
        assert_eq!(values.len(),2);
        assert!(info.refs[&values[0].range.start()].iter().any(|r|r.binding.is_none()), "class read retains missing-cell diagnostic alternative");
        let normal = &info.refs[&values[1].range.start()];
        assert!(!normal.is_empty(), "successful class construction has an actual surviving cell binding");
        let surviving = file.scopes.bindings.iter().enumerate().find(|(_, binding)| binding.name == "value" && binding.is_definition()).unwrap();
        let expected = rustpy_semantic::scope::BindingId(surviving.0 as u32);
        assert!(normal.iter().all(|reach| reach.binding == Some(expected)), "normal paths retain the exact conditional value assignment");
        assert!(info.refs[&values[1].range.start()].iter().all(|r|r.binding.is_some()), "successful class construction retains only bound cell paths");
    });
}
