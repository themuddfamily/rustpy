use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(parameters: &str, call: &str, expected_never: bool, extra: &str) {
    let source = format!("from typing import overload,Never\n{extra}@overload\ndef stop({})->Never: ...\n@overload\ndef stop({})->str: ...\ndef stop(*args,**kwargs): return 'ok'\n{call}\n",parameters.replace("TYPE","int"),parameters.replace("TYPE","str"));
    let db = Analysis::for_project(Path::new("/kwargs-contract"));
    let id = db.set_file_text(Path::new("/kwargs-contract/m.py"), &source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let expression = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| {
            call.callee()
                .is_some_and(|callee| callee.syntax().text() == "stop")
        })
        .unwrap();
    let inf = Inferer::new(&db);
    let Type::Overloaded(signatures) = inf.type_of_expr(&file, &expression.callee().unwrap())
    else {
        panic!("mixed actual overloads required")
    };
    assert_eq!(signatures.len(), 2);
    let result = inf.type_of_expr(&file, &ast::Expr::Call(expression));
    assert_eq!(
        matches!(result, Type::Never),
        expected_never,
        "actual result {result}: {source}"
    );
    if !expected_never {
        assert_eq!(result.to_string(), "str");
    }
}
#[test]
fn known_kwargs_int_selects_never() {
    check("**values:TYPE", "stop(count=1)", true, "");
}
#[test]
fn known_kwargs_str_selects_returning() {
    check("**values:TYPE", "stop(count='ok')", false, "");
}
#[test]
fn normal_keyword_consumed_before_kwargs_values() {
    check(
        "label:str,**values:TYPE",
        "stop(label='name',count=1)",
        true,
        "",
    );
    check(
        "label:str,**values:TYPE",
        "stop(label='name',count='ok')",
        false,
        "",
    );
}
#[test]
fn keyword_only_consumed_before_kwargs_values() {
    check(
        "*,label:str,**values:TYPE",
        "stop(label='name',count=1)",
        true,
        "",
    );
    check(
        "*,label:str,**values:TYPE",
        "stop(label='name',count='ok')",
        false,
        "",
    );
}
#[test]
fn positional_only_name_remains_a_kwargs_entry() {
    check(
        "label:str,/,**values:TYPE",
        "stop('name',label=1)",
        true,
        "",
    );
    check(
        "label:str,/,**values:TYPE",
        "stop('name',label='ok')",
        false,
        "",
    );
}
#[test]
fn callback_result_type_is_checked_as_kwargs_value() {
    check(
        "**values:TYPE",
        "stop(count=obtain())",
        false,
        "def obtain()->str: return 'ok'\n",
    );
}
