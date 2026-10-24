use rustpy_core::TextSize;
use rustpy_semantic::{
    completion2::{expected_type, smart_complete},
    db::Analysis,
    infer::Inferer,
    types::{ParamKind, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn expanded_paramspec_unicode_names_do_not_collide_with_positional_skeleton() {
    let source = "from typing import Callable,ParamSpec,TypeVar\nP=ParamSpec('P')\nR=TypeVar('R')\ndef relay(fn:Callable[P,R])->Callable[P,R]:return fn\ndef target(x:str,/,ｖａｌｕｅ1:str)->int:return 1\ncallback=relay(target)\ncallback= \n";
    let db = Analysis::for_project(Path::new("/smart-normal-name"));
    let id = db.set_file_text(Path::new("/smart-normal-name/m.py"), source);
    let file = db.file(id).unwrap();
    let producer = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|c| c.syntax().text() == "relay(target)")
        .unwrap();
    let ty = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(producer));
    assert!(matches!(ty, Type::Callable(_)));
    let signature = ty.signature().unwrap();
    assert_eq!(signature.params.len(), 2);
    assert_eq!(signature.params[0].kind, ParamKind::PositionalOnly);
    assert_eq!(signature.params[1].name.as_str(), "ｖａｌｕｅ1");
    assert_eq!(signature.params[1].kind, ParamKind::Normal);
    assert_eq!(signature.params[1].ty.to_string(), "str");
    let offset = TextSize::new((source.rfind("callback= ").unwrap() + 10) as u32);
    let (expected, _) = expected_type(&db, &file, offset).unwrap();
    assert_eq!(
        expected.signature().unwrap().params[1].name.as_str(),
        "ｖａｌｕｅ1"
    );
    let item = smart_complete(&db, &file, offset)
        .into_iter()
        .find(|i| i.insert.starts_with("lambda "))
        .unwrap();
    assert_eq!(item.insert, "lambda value1_, /, ｖａｌｕｅ1: ");
    assert_eq!(item.caret_offset, Some(item.insert.len()));
    let completed = format!(
        "generated={}0\ngenerated('ok',ｖａｌｕｅ1='ok')\n",
        item.insert
    );
    let id = db.set_file_text(Path::new("/smart-normal-name/generated.py"), &completed);
    let generated = db.file(id).unwrap();
    assert!(generated.parse.errors().is_empty());
    let lambda = generated
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::LambdaExpr::cast)
        .unwrap();
    let params: Vec<_> = lambda.params().unwrap().plain_params().collect();
    assert_eq!(params.len(), 2);
    assert_eq!(params[1].name_text(), "ｖａｌｕｅ1");
    assert!(params[1].syntax().text() == "ｖａｌｕｅ1");
}
