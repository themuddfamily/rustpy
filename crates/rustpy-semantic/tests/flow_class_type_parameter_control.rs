//! PEP 695 class header uses its own type parameter, not a later outer local.
use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
#[test]
fn class_type_parameter_base_is_not_an_unbound_outer_local_read() {
    let source = "def f():\n    marker=1\n    class C[T](list[T]):\n        pass\n    T=0\n    return marker\n";
    let db = Analysis::for_project(Path::new("/class-type-parameter"));
    let id = db.set_file_text(Path::new("/class-type-parameter/m.py"), source);
    let file = db.file(id).unwrap();
    let marker = file
        .scopes
        .references
        .iter()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let type_name = file
        .scopes
        .references
        .iter()
        .find(|reference| reference.name == "T" && !reference.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, marker.scope).unwrap());
    assert!(
        info.refs.contains_key(&marker.range.start()),
        "class header normally completes"
    );
    assert!(
        !info.refs.contains_key(&type_name.range.start()),
        "bound annotation-scope T is not outer fast-local T"
    );
}
