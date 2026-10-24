use rustpy_core::TextSize;
use rustpy_semantic::{
    completion2::{expected_type, smart_complete},
    db::Analysis,
    infer::Inferer,
    types::Type,
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

#[test]
fn callable_expected_type_offers_an_editable_lambda_and_not_for_scalar_context() {
    let db = Analysis::for_project(Path::new("/smart-missing-forms"));
    let source =
        "from typing import Callable\ndef use(callback:Callable[[str],int])->None:pass\nuse()\n";
    let id = db.set_file_text(Path::new("/smart-missing-forms/m.py"), source);
    let file = db.file(id).unwrap();
    let offset = TextSize::new((source.rfind("use()").unwrap() + 4) as u32);
    let (expected, _) =
        expected_type(&db, &file, offset).expect("actual callable parameter context");
    let signature = expected.signature().expect("actual callable metadata");
    assert_eq!(signature.params.len(), 1);
    assert_eq!(signature.params[0].ty.to_string(), "str");
    assert_eq!(signature.ret.to_string(), "int");
    let items = smart_complete(&db, &file, offset);
    let item = items
        .iter()
        .find(|item| item.insert.starts_with("lambda "))
        .expect("editable lambda proposal");
    let insertion = item.insert.as_str();
    let colon = insertion.find(':').expect("lambda body separator");
    let caret = item.caret_offset.expect("editable lambda body caret");
    assert!(caret > colon && caret <= insertion.len());
    assert!(insertion.is_char_boundary(caret));
    // Supply a real expression for the unfinished editable body, then verify
    // the generated header is syntactically a one-parameter lambda.
    let completed = format!("{} 0\n", &insertion[..=colon]);
    let skeleton_id = db.set_file_text(Path::new("/smart-missing-forms/lambda.py"), &completed);
    let skeleton = db.file(skeleton_id).unwrap();
    assert!(skeleton.parse.errors().is_empty(), "{completed}");
    let lambda = skeleton
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::LambdaExpr::cast)
        .expect("actual generated lambda AST");
    assert_eq!(lambda.params().unwrap().plain_params().count(), 1);
    assert!(lambda.body().is_some());
    let source = "def use(value:int)->None:pass\nuse()\n";
    let id = db.set_file_text(Path::new("/smart-missing-forms/m.py"), source);
    let file = db.file(id).unwrap();
    let offset = TextSize::new((source.rfind("use()").unwrap() + 4) as u32);
    assert!(!smart_complete(&db, &file, offset)
        .iter()
        .any(|item| item.insert.starts_with("lambda ")));
}

#[test]
fn async_expected_result_offers_awaited_matching_variable_but_sync_scope_does_not() {
    for asynchronous in [true, false] {
        let db = Analysis::for_project(Path::new("/smart-missing-forms"));
        let header = if asynchronous {
            "async def run():"
        } else {
            "def run():"
        };
        let source=format!("async def fetch()->str:return 'ok'\ndef use(value:str)->None:pass\n{header}\n    pending=fetch()\n    wrong=1\n    use()\n");
        let id = db.set_file_text(Path::new("/smart-missing-forms/m.py"), &source);
        let file = db.file(id).unwrap();
        let offset = TextSize::new((source.rfind("use()").unwrap() + 4) as u32);
        let (expected, _) =
            expected_type(&db, &file, offset).expect("actual result parameter context");
        assert_eq!(expected.to_string(), "str");
        let fetch = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .find(|call| call.syntax().text() == "fetch()")
            .expect("real coroutine producer");
        let inf = Inferer::new(&db);
        let producer = inf.type_of_expr(&file, &ast::Expr::Call(fetch));
        let pending_name = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|name| name.name_text() == "pending")
            .expect("actual initialized pending binding");
        let pending = inf.type_of_expr(&file, &ast::Expr::Name(pending_name));
        assert_eq!(
            pending, producer,
            "the suggested variable retains the actual producer type"
        );
        let Type::Instance(class, arguments) = &pending else {
            panic!("actual async producer: {pending}");
        };
        assert_eq!(class.name.as_str(), "Coroutine");
        assert_eq!(arguments.len(), 3);
        assert_eq!(arguments[2].to_string(), "str");
        assert_eq!(inf.await_type(&pending).to_string(), "str");
        let items = smart_complete(&db, &file, offset);
        let awaited = items.iter().any(|item| item.insert == "await pending");
        assert_eq!(
            awaited, asynchronous,
            "only legal async scope offers wrapping"
        );
        assert!(!items.iter().any(|item| item.insert == "await wrong"));
    }
}
