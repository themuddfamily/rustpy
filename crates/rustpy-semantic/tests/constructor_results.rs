//! Constructor result authority through maintained public APIs; baseline fixtures.
use rustpy_semantic::{
    db::Analysis,
    features::{complete, hover_at},
    infer::Inferer,
};
use rustpy_syntax::{
    ast::{self, AstNode},
    TextSize,
};
use std::path::Path;
fn inferred(source: &str) -> String {
    let db = Analysis::for_project(Path::new("/constructor-fixture"));
    let id = db.set_file_text(Path::new("/constructor-fixture/main.py"), source);
    let file = db.file(id).unwrap();
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(last) = module.statements().last().unwrap() else {
        panic!("expression fixture")
    };
    Inferer::new(&db)
        .type_of_expr(&file, &last.expr().unwrap())
        .to_string()
}
fn consumers(source: &str) {
    let db = Analysis::for_project(Path::new("/constructor-fixture"));
    let text = format!("{source}value\nvalue.\n");
    let id = db.set_file_text(Path::new("/constructor-fixture/main.py"), &text);
    let file = db.file(id).unwrap();
    let offset = text.rfind("value\n").unwrap();
    assert_eq!(
        hover_at(&db, &file, TextSize::new(offset as u32))
            .unwrap()
            .signature,
        "value: Result"
    );
    let items = complete(&db, &file, TextSize::new((text.len() - 1) as u32));
    assert!(items.iter().any(|item| item.label == "result_member"));
    assert!(!items.iter().any(|item| item.label == "factory_member"));
}

#[test]
fn constructor_foreign_new() {
    let source = "class Result:\n    result_member: str\nclass Factory:\n    factory_member: int\n    def __new__(cls) -> Result:\n        return Result()\nvalue = Factory()\nvalue\n";
    assert_eq!(inferred(source), "Result");
    consumers(source);
}

#[test]
fn constructor_metaclass_call() {
    let source = "class Result:\n    result_member: str\nclass Meta(type):\n    def __call__(cls, value: int) -> Result:\n        return Result()\nclass Factory(metaclass=Meta):\n    factory_member: int\nvalue = Factory(1)\nvalue\n";
    assert_eq!(inferred(source), "Result");
    consumers(source);
}

#[test]
fn constructor_ordinary_init() {
    let source = "class Ordinary:\n    def __init__(self, value: int) -> None:\n        self.value = value\nOrdinary(1)\n";
    assert_eq!(inferred(source), "Ordinary");
}

#[test]
fn constructor_self_new() {
    let source = "from typing import Self\nclass Base:\n    def __new__(cls) -> Self:\n        return super().__new__(cls)\nclass Child(Base):\n    child_member: int\nChild()\n";
    assert_eq!(inferred(source), "Child");
}

#[test]
fn constructor_foreign_new_skips_init() {
    let source = "class Result: pass\nclass Factory:\n    def __new__(cls, value: int) -> Result:\n        return Result()\n    def __init__(self, forbidden: str, required: str) -> None: pass\nFactory(1)\n";
    assert_eq!(inferred(source), "Result");
}

#[test]
fn constructor_union_new() {
    let source = "class Result: pass\nclass Factory:\n    def __new__(cls, flag: bool) -> 'Factory | Result': ...\nFactory(True)\n";
    let result = inferred(source);
    assert!(
        result.contains("Factory") && result.contains("Result") && result.contains(" | "),
        "{result}"
    );
}

#[test]
fn constructor_inherited_metaclass() {
    let source = "class Meta(type):\n    def __call__(cls) -> str: return 'value'\nclass Base(metaclass=Meta): pass\nclass Child(Base): pass\nChild()\n";
    assert_eq!(inferred(source), "str");
}

#[test]
fn constructor_recursive_new() {
    let source = "class Factory:\n    def __new__(cls): return cls()\nFactory()\n";
    assert_eq!(inferred(source), "Unknown");
}
