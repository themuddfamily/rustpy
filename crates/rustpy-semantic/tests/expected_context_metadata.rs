use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn source_file(source: &str) -> (Analysis, std::sync::Arc<rustpy_semantic::db::FileInfo>) {
    let db = Analysis::for_project(Path::new("/expected-metadata"));
    let id = db.set_file_text(Path::new("/expected-metadata/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    (db, file)
}
#[test]
fn unsupported_parameter_spec_context_preserves_actual_callable() {
    let (db,file)=source_file("from typing import Callable,ParamSpec,TypeVar\nP=ParamSpec('P')\nR=TypeVar('R')\ndef relay(fn:Callable[P,R])->Callable[P,R]:return fn\ndef target(value:int)->str:return ''\nrelay(target)\n");
    let root = file.parse.syntax_node();
    let relay = root
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .find(|f| f.name_text() == "relay")
        .unwrap();
    let target = root
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|name| name.name_text() == "target")
        .unwrap();
    let inf = Inferer::new(&db);
    let expected = inf.signature(&file, &relay, None).params[0].ty.clone();
    let expression = ast::Expr::Name(target);
    let ordinary = inf.type_of_expr(&file, &expression);
    let contextual = inf.type_of_expr_with_expected(&file, &expression, &expected);
    assert_eq!(
        contextual, ordinary,
        "opaque expected metadata cannot erase a known callable"
    );
    let signature = contextual.signature().unwrap();
    assert_eq!(signature.params[0].ty.to_string(), "int");
    assert_eq!(signature.ret.to_string(), "str");
}
#[test]
fn parameter_spec_call_argument_keeps_shape_through_parent_adapter() {
    let (db,file)=source_file("from typing import Callable,ParamSpec,TypeVar\nP=ParamSpec('P')\nR=TypeVar('R')\ndef relay(fn:Callable[P,R])->Callable[P,R]:return fn\ndef target(*,value:str)->int:return 1\nrelay(target)\n");
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == "relay(target)")
        .unwrap();
    let result = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(call));
    let signature = result.signature().unwrap();
    assert_eq!(signature.params.len(), 1);
    assert_eq!(signature.params[0].name.as_str(), "value");
    assert_eq!(signature.params[0].ty.to_string(), "str");
    assert_eq!(signature.ret.to_string(), "int");
}
#[test]
fn unsupported_tuple_pack_context_preserves_actual_ordered_tuple() {
    let (db,file)=source_file("from typing import TypeVarTuple,Unpack\nTs=TypeVarTuple('Ts')\ndef middle(value:tuple[int,Unpack[Ts],bytes])->tuple[Unpack[Ts]]:...\nmiddle((1,'x',b'end'))\n");
    let root = file.parse.syntax_node();
    let function = root
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .find(|f| f.name_text() == "middle")
        .unwrap();
    let tuple = root
        .descendants()
        .filter_map(ast::TupleExpr::cast)
        .find(|t| t.syntax().text() == "(1,'x',b'end')")
        .unwrap();
    let inf = Inferer::new(&db);
    let expected = inf.signature(&file, &function, None).params[0].ty.clone();
    let expression = ast::Expr::Tuple(tuple);
    let ordinary = inf.type_of_expr(&file, &expression);
    let contextual = inf.type_of_expr_with_expected(&file, &expression, &expected);
    assert_eq!(contextual, ordinary);
    let Type::Tuple(elements) = contextual else {
        panic!("actual tuple required")
    };
    assert_eq!(elements.len(), 3);
}
#[test]
fn uninformative_expected_types_preserve_concrete_call_result() {
    let (db, file) = source_file("def actual(value:int)->int:return value\nactual(1)\n");
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|c| c.syntax().text() == "actual(1)")
        .unwrap();
    let expression = ast::Expr::Call(call);
    let inf = Inferer::new(&db);
    assert_eq!(
        inf.type_of_expr_with_expected(&file, &expression, &Type::Unknown)
            .to_string(),
        "int"
    );
    assert_eq!(
        inf.type_of_expr_with_expected(&file, &expression, &Type::Any)
            .to_string(),
        "int"
    );
}
