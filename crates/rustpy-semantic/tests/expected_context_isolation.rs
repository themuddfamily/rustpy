use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn explicit_queries_do_not_poison_ordinary_binding_or_lambda_caches() {
    let db = Analysis::for_project(Path::new("/context-order"));
    let id=db.set_file_text(Path::new("/context-order/m.py"),"from typing import Callable\nstr_fn:Callable[[str],str]\nint_fn:Callable[[int],int]\nfn=lambda value:value\n");
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let root = file.parse.syntax_node();
    let lambda = root.descendants().find_map(ast::LambdaExpr::cast).unwrap();
    let annotations: Vec<_> = root
        .descendants()
        .filter_map(ast::AnnAssignStmt::cast)
        .filter_map(|assign| assign.annotation())
        .collect();
    let inf = Inferer::new(&db);
    let str_expected = inf.annotation_type(&file, &annotations[0]);
    let int_expected = inf.annotation_type(&file, &annotations[1]);
    let expression = ast::Expr::Lambda(lambda);
    let before = inf.type_of_expr(&file, &expression);
    let str_result = inf.type_of_expr_with_expected(&file, &expression, &str_expected);
    let int_result = inf.type_of_expr_with_expected(&file, &expression, &int_expected);
    let after = inf.type_of_expr(&file, &expression);
    let Type::Function(s) = str_result else {
        panic!("str lambda")
    };
    let Type::Function(i) = int_result else {
        panic!("int lambda")
    };
    assert_eq!(s.params[0].ty.to_string(), "str");
    assert_eq!(s.ret.to_string(), "str");
    assert_eq!(i.params[0].ty.to_string(), "int");
    assert_eq!(i.ret.to_string(), "int");
    assert_eq!(
        before, after,
        "contextual derived types must not publish into ordinary caches"
    );
}
