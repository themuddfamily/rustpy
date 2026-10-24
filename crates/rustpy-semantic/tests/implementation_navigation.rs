use rustpy_semantic::{db::Analysis, features, hierarchy, TextSize};
use std::path::Path;

#[test]
fn methods_at_references_and_declarations_share_override_and_super_targets() {
    let db = Analysis::for_project(Path::new("/implementation-navigation"));
    let path = Path::new("/implementation-navigation/main.py");
    let source = "from abc import ABC, abstractmethod\nclass Base(ABC):\n    @abstractmethod\n    def run(self): pass\nclass Middle(Base):\n    def run(self): pass\nclass Child(Middle):\n    def run(self): pass\nclass Unrelated:\n    def run(self): pass\ndef use(value:Base, child:Child):\n    value.run()\n    child.run()\n";
    let file = db.file(db.set_file_text(path, source)).unwrap();
    assert!(file.parse.errors().is_empty());
    let files = vec![path.to_path_buf()];
    let at =
        |needle: &str, delta: usize| TextSize::new((source.find(needle).unwrap() + delta) as u32);
    let declared = hierarchy::implementations(&db, &file, at("def run", 4), &files);
    let referenced = hierarchy::implementations(&db, &file, at("value.run", 6), &files);
    assert_eq!(declared, referenced);
    assert_eq!(referenced.len(), 2);
    for location in &referenced {
        assert_eq!(&source[location.range], "run");
        assert!(location.range.start() < at("class Unrelated", 0));
    }
    let supers = hierarchy::super_methods(&db, &file, at("child.run", 6));
    assert_eq!(supers.len(), 2);
    assert!(supers[0].range.start() > supers[1].range.start());
    let subclasses = hierarchy::implementations(&db, &file, at("class Base", 6), &files);
    assert_eq!(subclasses.len(), 2);
    assert!(subclasses
        .iter()
        .any(|location| &source[location.range] == "Child"));
    assert!(subclasses
        .iter()
        .any(|location| &source[location.range] == "Middle"));
}

#[test]
fn type_navigation_preserves_all_overload_results_and_union_members() {
    let db = Analysis::for_project(Path::new("/type-navigation"));
    let path = Path::new("/type-navigation/main.py");
    let source = "from typing import overload\nclass First: pass\nclass Second: pass\n@overload\ndef choose(value:int)->First: ...\n@overload\ndef choose(value:str)->Second: ...\ndef choose(value): pass\ndef use(value:First|Second|None):\n    value\nchoose\n";
    let file = db.file(db.set_file_text(path, source)).unwrap();
    assert!(file.parse.errors().is_empty());
    for needle in ["    value\n", "choose\n"] {
        let offset = TextSize::new(
            (source.rfind(needle).unwrap() + if needle.starts_with(' ') { 5 } else { 1 }) as u32,
        );
        let targets = features::type_definition_at(&db, &file, offset);
        let mut names: Vec<_> = targets
            .iter()
            .map(|location| source[location.range].to_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["First", "Second"]);
    }
}

#[test]
fn nested_functions_and_same_named_data_are_not_overriding_methods() {
    let db = Analysis::for_project(Path::new("/method-identity"));
    let path = Path::new("/method-identity/main.py");
    let source = "class Base:\n    def run(self): pass\nclass Data(Base):\n    run=1\nclass Child(Base):\n    def run(self):\n        def nested(): pass\n        nested()\n";
    let file = db.file(db.set_file_text(path, source)).unwrap();
    assert!(file.parse.errors().is_empty());
    let results = hierarchy::implementations(
        &db,
        &file,
        TextSize::new(source.find("def run").unwrap() as u32 + 5),
        &[path.to_path_buf()],
    );
    assert_eq!(results.len(), 1);
    assert_eq!(&source[results[0].range], "run");
    assert!(results[0].range.start() > TextSize::new(source.find("class Child").unwrap() as u32));
    let nested = TextSize::new(source.find("def nested").unwrap() as u32 + 5);
    assert!(hierarchy::super_methods(&db, &file, nested).is_empty());
}

#[test]
fn same_spelled_local_and_parameter_references_are_not_method_declarations() {
    let db = Analysis::for_project(Path::new("/physical-method-name"));
    let path = Path::new("/physical-method-name/main.py");
    let source = "class Base:\n    def run(self): pass\n    def execute(self): pass\nclass Middle(Base):\n    def run(self):\n        run = 1\n        return run\n    def execute(self, execute:int):\n        return execute\nclass Child(Middle):\n    def run(self): pass\n    def execute(self): pass\n";
    let file = db.file(db.set_file_text(path, source)).unwrap();
    assert!(file.parse.errors().is_empty());
    for needle in ["return run", "return execute"] {
        let offset = TextSize::new((source.find(needle).unwrap() + "return ".len()) as u32);
        assert!(
            hierarchy::implementations(&db, &file, offset, &[path.to_path_buf()]).is_empty(),
            "{needle}"
        );
    }
    let declaration = TextSize::new(
        (source.find("    def run(self):\n        run").unwrap() + "    def ".len()) as u32,
    );
    let targets = hierarchy::implementations(&db, &file, declaration, &[path.to_path_buf()]);
    assert_eq!(targets.len(), 1);
    assert!(targets[0].range.start() > TextSize::new(source.find("class Child").unwrap() as u32));
}

#[test]
fn super_method_targets_exclude_data_but_preserve_method_and_property_kinds() {
    let db = Analysis::for_project(Path::new("/super-member-kinds"));
    let path = Path::new("/super-member-kinds/main.py");
    let source = "class Base:\n    data = 1\n    def method(self): pass\n    @property\n    def value(self): return 1\n    @staticmethod\n    def static(): pass\n    @classmethod\n    def factory(cls): pass\nclass Child(Base):\n    def data(self): pass\n    def method(self): pass\n    @property\n    def value(self): return 2\n    @staticmethod\n    def static(): pass\n    @classmethod\n    def factory(cls): pass\n";
    let file = db.file(db.set_file_text(path, source)).unwrap();
    assert!(file.parse.errors().is_empty());
    for (name, expected) in [
        ("data", 0),
        ("method", 1),
        ("value", 1),
        ("static", 1),
        ("factory", 1),
    ] {
        let needle = format!("def {name}(");
        let offset = TextSize::new((source.rfind(&needle).unwrap() + 4) as u32);
        let targets = hierarchy::super_methods(&db, &file, offset);
        assert_eq!(targets.len(), expected, "{name}");
        for target in targets {
            assert_eq!(&source[target.range], name);
            assert!(
                target.range.start() < TextSize::new(source.find("class Child").unwrap() as u32)
            );
        }
    }
}
