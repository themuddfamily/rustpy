use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn exhausted_union_slice_budget_returns_whole_unknown_not_partial_arms() {
    let arms = (0..32)
        .map(|index| format!("tuple[Literal[{index}],{}]", vec!["str"; 15].join(",")))
        .collect::<Vec<_>>()
        .join(",");
    let source=format!("from typing import Union,Literal\ndef use(value:Union[{arms}],start:int):\n value #premise\n result=value[start:]\n result #result\n");
    let db = Analysis::for_project(Path::new("/slice-budget"));
    let id = db.set_file_text(Path::new("/slice-budget/m.py"), &source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let inferer = Inferer::new(&db);
    let read = |needle: &str| {
        let offset = source.find(needle).unwrap();
        let name = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|name| usize::from(name.text_range().start()) == offset)
            .unwrap();
        inferer.type_of_expr(&file, &ast::Expr::Name(name))
    };
    let Type::Union(arms) = read("value #premise") else {
        panic!("actual union producer premise");
    };
    assert_eq!(arms.len(), 32);
    assert!(arms
        .iter()
        .all(|arm| matches!(arm,Type::Tuple(elements) if elements.len()==16)));
    // An explicit local resource-policy control, not an unlimited-checker
    // conformance assertion. Early arms fit; completing all exceeds work budget.
    assert_eq!(read("result #result"), Type::Unknown);
}
