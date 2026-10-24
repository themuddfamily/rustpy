//! A known implicit truth test mutates a captured callable before branch selection.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn known_bool_side_effect_cannot_fabricate_conditional_noreturn() {
    let source = SOURCE;
    let db = Analysis::for_project(Path::new("/truthiness-control"));
    let id = db.set_file_text(Path::new("/truthiness-control/main.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == "(stop if flag else stop)()")
        .unwrap();
    // Static return inference alone cannot erase actual pre-acquisition side effects.
    let inferred = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(call));
    assert!(
        matches!(inferred, Type::Never),
        "public inference premise BEFORE flow: {inferred}"
    );
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert!(
        info.refs.contains_key(&reference.range.start()),
        "known __bool__ rebinds stop before callee selection; return marker remains reachable"
    );
}
const SOURCE: &str = r#"from typing import Never
def halt()->Never: raise RuntimeError
def normal()->int: return 1
def f():
    stop=halt
    class Flag:
        def __bool__(self):
            nonlocal stop
            stop=normal
            return True
    flag=Flag()
    marker=1
    (stop if flag else stop)()
    return marker
"#;
