//! Literal false values cannot execute a lazy right operand.
use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
fn lazy_false(value: &str) {
    let source =
        format!("def f():\n    marker=1\n    result={value} and marker\n    return marker\n");
    let db = Analysis::for_project(Path::new("/literal-truth"));
    let id = db.set_file_text(Path::new("/literal-truth/m.py"), &source);
    let file = db.file(id).unwrap();
    let mut loads: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|r| r.name == "marker" && !r.is_write)
        .collect();
    loads.sort_by_key(|r| r.range.start());
    assert_eq!(loads.len(), 2);
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, loads[1].scope).unwrap());
    assert!(
        !info.refs.contains_key(&loads[0].range.start()),
        "false literal never evaluates RHS: {source}"
    );
    assert!(
        info.refs.contains_key(&loads[1].range.start()),
        "normal tail remains reachable"
    );
}
#[test]
fn continued_crlf_only_string_is_false() {
    lazy_false("'\\\r\n'");
}
#[test]
fn continued_crlf_only_bytes_is_false() {
    lazy_false("b'\\\r\n'");
}
