use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, producer: &str, premise: &str, reads: &[(&str, &str)]) {
    let db = Analysis::for_project(Path::new("/inference-row222"));
    let id = db.set_file_text(Path::new("/inference-row222/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let inferer = Inferer::new(&db);
    let expression = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|expr| expr.syntax().text() == producer)
        .unwrap();
    assert_eq!(
        inferer.type_of_expr(&file, &expression).to_string(),
        premise,
        "actual maintained producer premise"
    );
    for (needle, expected) in reads {
        let offset = source.find(needle).unwrap();
        assert_eq!(source.matches(needle).count(), 1);
        let name = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|node| usize::from(node.text_range().start()) == offset)
            .unwrap();
        assert_eq!(
            inferer
                .type_of_expr(&file, &ast::Expr::Name(name))
                .to_string(),
            *expected,
            "{needle}: {source}"
        );
    }
}

#[test]
fn qualified_builtin_type_alias_retirement_uses_the_actual_member_name() {
    check(
        "class Holder:\n reflect=type\nresult=Holder.reflect(1)\nresult #valid\n",
        "1",
        "Literal[1]",
        &[("result #valid", "type[int]")],
    );
    check("class Holder:\n reflect=type\ndef replacement(value:int)->str:return 'made'\nHolder.reflect=replacement\nresult=Holder.reflect(1)\nresult #retired\n","1","Literal[1]",&[("result #retired","Unknown")]);
}
