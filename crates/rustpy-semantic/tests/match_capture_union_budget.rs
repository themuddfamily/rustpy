//! Positional class-pattern captures on a union subject keep their projection
//! when the class's authority is proved once per class rather than per arm.
use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn capture(source: &str, marker: &str) -> String {
    let db = Analysis::for_project(Path::new("/match-capture-union"));
    let id = db.set_file_text(Path::new("/match-capture-union/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let offset = source.find(marker).unwrap();
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|node| usize::from(node.text_range().start()) == offset)
        .unwrap();
    Inferer::new(&db)
        .type_of_expr(&file, &ast::Expr::Name(node))
        .to_string()
}

const TRANSFORM_BASE: &str = "from typing import dataclass_transform\n@dataclass_transform()\nclass ModelBase:\n    def __init_subclass__(cls, **kwargs): pass\nclass Point(ModelBase):\n    x: int\n    y: str\n";

#[test]
fn transform_base_model_positional_captures_survive_optional_subject() {
    for subject in ["Point", "Point | None", "None | Point", "Optional[Point]"] {
        let source = format!(
            "from typing import Optional\n{TRANSFORM_BASE}def use(p: {subject}):\n    match p:\n        case Point(a, b):\n            a #first\n            b #second\n"
        );
        assert_eq!(capture(&source, "a #first"), "int", "{subject}");
        assert_eq!(capture(&source, "b #second"), "str", "{subject}");
    }
}

#[test]
fn transform_base_model_captures_in_nested_and_keyword_positions() {
    let source = format!(
        "{TRANSFORM_BASE}class Line(ModelBase):\n    start: Point\n    end: Point | None\ndef use(line: Line | None):\n    match line:\n        case Line(Point(a, b), Point(x=c, y=d)):\n            a #first\n            d #last\n"
    );
    assert_eq!(capture(&source, "a #first"), "int");
    assert_eq!(capture(&source, "d #last"), "str");
}

#[test]
fn wide_union_of_distinct_classes_stays_bounded() {
    // Many distinct model classes each need their own proof; the shared budget
    // still declines instead of growing with the union width.
    let mut source = String::from(TRANSFORM_BASE);
    let mut names = Vec::new();
    for index in 0..30 {
        source.push_str(&format!("class M{index}(ModelBase):\n    x: int\n"));
        names.push(format!("M{index}"));
    }
    source.push_str(&format!(
        "def use(p: {} | Point):\n    match p:\n        case Point(a, b):\n            a #first\n",
        names.join(" | ")
    ));
    let started = std::time::Instant::now();
    let _ = capture(&source, "a #first");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}
