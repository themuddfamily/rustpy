use rustpy_semantic::{db::Analysis, hierarchy, TextSize};
use std::path::Path;

#[test]
fn callee_queries_resolve_references_and_exclude_unexecuted_nested_bodies() {
    let db = Analysis::for_project(Path::new("/hierarchy-queries"));
    let source="def direct(): pass\ndef hidden(): pass\ndef outer():\n    direct()\n    def inner():\n        hidden()\n    class Nested:\n        def method(self): hidden()\nouter()\n";
    let file = db
        .file(db.set_file_text(Path::new("/hierarchy-queries/main.py"), source))
        .unwrap();
    assert!(file.parse.errors().is_empty());
    for offset in [
        source.find("def outer").unwrap() + 4,
        source.rfind("outer()").unwrap(),
    ] {
        let offset = TextSize::new(offset as u32);
        let root = hierarchy::callable_root(&db, &file, offset).unwrap();
        assert_eq!(&source[root.range], "outer");
        let nodes = hierarchy::callees(&db, &file, offset);
        assert_eq!(
            nodes
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            ["direct"]
        );
    }
}

#[test]
fn method_hierarchy_at_a_reference_and_scoped_subtypes_keep_physical_targets() {
    let db = Analysis::for_project(Path::new("/hierarchy-scope"));
    let path = Path::new("/hierarchy-scope/base.py");
    let source="class Base:\n    def run(self): pass\nclass Local(Base):\n    def run(self): pass\ndef use(value: Base):\n    value.run()\n";
    let file = db.file(db.set_file_text(path, source)).unwrap();
    let child = Path::new("/hierarchy-scope/child.py");
    db.set_file_text(
        child,
        "from base import Base\nclass Child(Base):\n    def run(self): pass\n",
    );
    let at = TextSize::new((source.find("value.run").unwrap() + 6) as u32);
    let root = hierarchy::method_hierarchy(&db, &file, at, &[path.into(), child.into()]).unwrap();
    assert_eq!(&source[root.range], "run");
    assert_eq!(root.children.len(), 2);
    let base = TextSize::new((source.find("class Base").unwrap() + 6) as u32);
    let local = hierarchy::subtypes(&db, &file, base, &[path.into()]).unwrap();
    assert_eq!(local.children.len(), 1);
    assert_eq!(local.children[0].name, "Local");
    assert!(hierarchy::supertypes(
        &db,
        &file,
        TextSize::new((source.find("class Local").unwrap() + 6) as u32)
    )
    .unwrap()
    .children
    .iter()
    .any(|node| node.name == "Base"));
}
