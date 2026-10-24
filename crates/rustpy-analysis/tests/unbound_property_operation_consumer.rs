use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn inspected_ranges(
    db: &Analysis,
    file: &std::sync::Arc<rustpy_semantic::db::FileInfo>,
) -> Vec<(String, usize, usize)> {
    inspections::inspect(db, file, &InspectionOptions::default())
        .into_iter()
        .filter(|problem| problem.diagnostic.code == "PyUnboundLocalVariable")
        .map(|problem| {
            (
                problem.diagnostic.code.to_string(),
                usize::from(problem.diagnostic.range.start()),
                usize::from(problem.diagnostic.range.end()),
            )
        })
        .collect()
}
const NATIVE: &str = r#"from typing import Never, Awaitable
class Base:
    def stop(self) -> Never: raise RuntimeError
class Child(Base):
    def run(self):
        super(Child, self).stop()
        return super_missing
        super_missing = 1
class Middle:
    def __init__(self): self.child = Base()
class Outer:
    def __init__(self): self.middle = Middle()
def nested_case():
    outer = Outer()
    outer.middle.child.stop()
    return nested_missing
    nested_missing = 1
class Getter:
    @property
    def terminal(self) -> Never: raise RuntimeError
def property_case():
    getter = Getter()
    getter.terminal
    return property_missing
    property_missing = 1
async def await_case(pending: Awaitable[Never]):
    await pending
    return await_missing
    await_missing = 1
async def returning_case(pending: Awaitable[int]):
    await pending
    return ordinary_missing
    ordinary_missing = 1
"#;
#[test]
fn exact_native_property_source_has_semantic_terminal_point_before_inspection() {
    let db = Analysis::for_project(Path::new("/operation-consumer-native"));
    let id = db.set_file_text(Path::new("/operation-consumer-native/m.py"), NATIVE);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let attribute = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::AttributeExpr::cast)
        .find(|expression| expression.syntax().text() == "getter.terminal")
        .unwrap();
    let ty = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Attribute(attribute.clone()));
    assert!(
        matches!(ty, Type::Never),
        "public getter premise BEFORE Flow: {ty}"
    );
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "property_missing" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert!(
        info.terminal_calls.contains(&attribute.text_range()),
        "actual property semantic terminal point"
    );
    assert!(
        !info.refs.contains_key(&reference.range.start()),
        "semantic tail unreachable BEFORE inspection"
    );
    let start = NATIVE.rfind("return ordinary_missing").unwrap() + "return ".len();
    assert_eq!(
        inspected_ranges(&db, &file),
        vec![(
            "PyUnboundLocalVariable".into(),
            start,
            start + "ordinary_missing".len()
        )],
        "whole native input exact inspection ID/range"
    );
}
#[test]
fn minimal_property_read_completion_reaches_actual_inspection_consumer() {
    for (ret, body, terminal) in [
        ("Never", "raise RuntimeError", true),
        ("int", "return 1", false),
    ] {
        let source=format!("from typing import Never\nclass C:\n    @property\n    def terminal(self)->{ret}:{body}\ndef f():\n    c=C()\n    c.terminal\n    print(x)\n    x=1\n");
        let db = Analysis::for_project(Path::new("/operation-consumer-small"));
        let id = db.set_file_text(Path::new("/operation-consumer-small/m.py"), &source);
        let file = db.file(id).unwrap();
        assert!(file.parse.errors().is_empty());
        let attribute = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::AttributeExpr::cast)
            .find(|expression| expression.syntax().text() == "c.terminal")
            .unwrap();
        let ty = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Attribute(attribute));
        assert_eq!(
            matches!(ty, Type::Never),
            terminal,
            "public getter producer BEFORE inspection"
        );
        assert!(!ty.is_unknown());
        let start = source.rfind("print(x)").unwrap() + "print(".len();
        let expected = if terminal {
            vec![]
        } else {
            vec![("PyUnboundLocalVariable".into(), start, start + 1)]
        };
        assert_eq!(inspected_ranges(&db, &file), expected, "{source}");
    }
}
