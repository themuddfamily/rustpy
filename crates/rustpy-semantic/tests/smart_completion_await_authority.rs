use rustpy_core::TextSize;
use rustpy_semantic::{completion2::smart_complete, db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn branch_mixed_pending_does_not_offer_await_from_lexical_last_assignment() {
    let db = Analysis::for_project(Path::new("/smart-await-authority"));
    let source = "async def fetch()->str:return 'ok'\ndef use(value:str):pass\nasync def run(flag:bool):\n    if flag:\n        pending=1\n    else:\n        pending=fetch()\n    pending #actual_read\n    use()\n";
    let id = db.set_file_text(Path::new("/smart-await-authority/m.py"), source);
    let file = db.file(id).unwrap();
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|n| {
            usize::from(n.text_range().start()) == source.find("pending #actual_read").unwrap()
        })
        .unwrap();
    let ty = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name));
    let Type::Union(values) = ty else {
        panic!("actual reaching mixed type: {ty}");
    };
    assert!(values
        .iter()
        .any(|t| matches!(t, Type::Instance(c,a) if c.name == "Coroutine" && a.len()==3)));
    assert!(values.iter().any(|t| matches!(
        t,
        Type::Literal(rustpy_semantic::types::LiteralValue::Int(1))
    ) || matches!(t, Type::Instance(c,_) if c.name == "int")));
    let offset = TextSize::new((source.rfind("use()").unwrap() + 4) as u32);
    assert!(!smart_complete(&db, &file, offset)
        .iter()
        .any(|i| i.insert == "await pending"));
}
#[test]
fn concurrent_future_and_asyncio_future_have_distinct_await_authority() {
    for (module, offered) in [("concurrent.futures", false), ("asyncio", true)] {
        let db = Analysis::for_project(Path::new("/smart-await-authority"));
        let source = format!(
            "from {module} import Future\ndef use(value:str):pass\nasync def run(pending:Future[str]):\n    pending #actual_read\n    use()\n"
        );
        let id = db.set_file_text(Path::new("/smart-await-authority/m.py"), &source);
        let file = db.file(id).unwrap();
        let name = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|n| {
                usize::from(n.text_range().start()) == source.find("pending #actual_read").unwrap()
            })
            .unwrap();
        let inf = Inferer::new(&db);
        let ty = inf.type_of_expr(&file, &ast::Expr::Name(name));
        let Type::Instance(class, args) = &ty else {
            panic!("real Future variable: {ty}");
        };
        assert_eq!(class.name.as_str(), "Future");
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].to_string(), "str");
        assert_eq!(
            inf.lookup_member(&ty, "__await__").is_some(),
            offered,
            "actual maintained protocol authority"
        );
        let offset = TextSize::new((source.rfind("use()").unwrap() + 4) as u32);
        assert_eq!(
            smart_complete(&db, &file, offset)
                .iter()
                .any(|i| i.insert == "await pending"),
            offered
        );
    }
}
