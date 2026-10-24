use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, modules: &[(&str, &str)]) {
    let db = Analysis::for_project(Path::new("/imported-annotation-contracts"));
    for (name, text) in modules {
        db.set_file_text(
            &Path::new("/imported-annotation-contracts").join(format!("{name}.py")),
            text,
        );
    }
    let id = db.set_file_text(Path::new("/imported-annotation-contracts/m.py"), source);
    let file = db.file(id).unwrap();
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| {
            call.callee()
                .is_some_and(|callee| callee.syntax().text() == "stop")
        })
        .unwrap();
    let expression = ast::Expr::cast(call.syntax().clone()).unwrap();
    assert!(
        matches!(
            Inferer::new(&db).type_of_expr(&file, &expression),
            Type::Never
        ),
        "maintained Inferer must prove declared Never before Flow: {source}"
    );
    let reference = file
        .scopes
        .references
        .iter()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert!(
        !info.refs.contains_key(&reference.range.start()),
        "actual terminal callable excludes assigned marker tail: {source}"
    );
}
const CONTRACTS: &str = "from typing import Callable,NoReturn\nStop=Callable[[],NoReturn]\n";
#[test]
fn imported_callable_alias_uses_captured_definition() {
    check("from contracts import Stop\ndef f(stop:Stop):\n    marker=1\n    stop()\n    return marker\n",&[("contracts",CONTRACTS)]);
}
#[test]
fn qualified_callable_alias_uses_captured_definition() {
    check(
        "import contracts as c\ndef f(stop:c.Stop):\n    marker=1\n    stop()\n    return marker\n",
        &[("contracts", CONTRACTS)],
    );
}
#[test]
fn canonical_paramspec_literal_options_preserve_terminal_return() {
    for options in [
        "bound=None, covariant=True",
        "bound=None, contravariant=True",
        "covariant=False, contravariant=False",
    ] {
        check(&format!("from typing import Callable,NoReturn,ParamSpec\nP=ParamSpec('P',{options})\ndef f(stop:Callable[P,NoReturn]):\n    marker=1\n    stop()\n    return marker\n"),&[]);
    }
}
