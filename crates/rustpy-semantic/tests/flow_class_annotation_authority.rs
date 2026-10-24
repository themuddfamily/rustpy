use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
#[test]
fn eager_annotation_head_from_custom_class_mapping_is_not_canonical() {
    let source="from typing import Callable, NoReturn\nclass Hint:\n    def __class_getitem__(cls,item):\n        return object\nclass Meta(type):\n    @classmethod\n    def __prepare__(mcls,name,bases):\n        return {\"Callable\":Hint}\nclass C(metaclass=Meta):\n    def f(self,stop:Callable[[],NoReturn]):\n        marker=1\n        stop()\n        return marker\n";
    let db = Analysis::for_project(Path::new("/class-annotation-authority"));
    let id = db.set_file_text(Path::new("/class-annotation-authority/m.py"), source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert!(
        info.refs.contains_key(&reference.range.start()),
        "injected annotation head cannot prove terminal stop parameter"
    );
    let reaches = &info.refs[&reference.range.start()];
    assert!(!reaches.is_empty());
    assert!(reaches.iter().all(|r| r.binding.is_some()));
}
