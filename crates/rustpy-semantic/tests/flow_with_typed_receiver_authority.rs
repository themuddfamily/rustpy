use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
#[test]
fn typed_receiver_exit_can_raise_after_completed_body_store() {
    let source="class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args)->bool:\n        return False\nclass D(C):\n    def __exit__(self,*args)->bool:\n        raise RuntimeError\ndef f(c:C):\n    marker=1\n    try:\n        with c:\n            marker=2\n    except RuntimeError:\n        return marker\n";
    let db = Analysis::for_project(Path::new("/typed-context-authority"));
    let id = db.set_file_text(Path::new("/typed-context-authority/m.py"), source);
    let file = db.file(id).unwrap();
    let read = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, read.scope).unwrap());
    let binding = file
        .scopes
        .bindings
        .iter()
        .enumerate()
        .find(|(_, b)| {
            b.name == "marker"
                && source[usize::from(b.node_range.start())..usize::from(b.node_range.end())]
                    .contains("marker=2")
        })
        .unwrap();
    let expected = rustpy_semantic::scope::BindingId(binding.0 as u32);
    assert!(
        info.refs[&read.range.start()]
            .iter()
            .any(|r| r.binding == Some(expected)),
        "valid subtype exit may raise after actual body store"
    );
}
