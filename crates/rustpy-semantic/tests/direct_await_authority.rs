use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn check(source: &str, receiver_expected: &str, method_result: Option<&str>, expected: &str) {
    let db = Analysis::for_project(Path::new("/row222-direct-await"));
    let id = db.set_file_text(Path::new("/row222-direct-await/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let inferer = Inferer::new(&db);
    let expression = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::AwaitExpr::cast)
        .last()
        .unwrap();
    let receiver = inferer.type_of_expr(&file, &expression.expr().unwrap());
    assert_eq!(
        receiver.to_string(),
        receiver_expected,
        "actual receiver producer premise"
    );
    if source.starts_with("from typing import Awaitable\n") {
        let owner = db.file(receiver.class_ref().unwrap().file).unwrap();
        assert!(owner.is_stub);
        assert_eq!(owner.path, Path::new("<typeshed>/typing.pyi"));
    }
    if let Some(result) = method_result {
        let class = receiver.class_ref().expect("actual receiver class");
        let owner = db.file(class.file).expect("actual physical class owner");
        let declaration = owner
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::FuncDef::cast)
            .find(|function| {
                function.name_text() == "__await__"
                    && class.range.contains_range(function.text_range())
            })
            .expect("actual class-owned physical method");
        let signature = inferer.signature(&owner, &declaration, Some(class));
        assert!(
            signature.def.is_some(),
            "physical maintained method declaration premise"
        );
        assert_eq!(
            signature.ret.to_string(),
            result,
            "actual __await__ declared generator return premise"
        );
    }
    assert_eq!(
        inferer
            .type_of_expr(&file, &ast::Expr::Await(expression))
            .to_string(),
        expected
    );
}

#[test]
fn user_wrapper_spelling_does_not_override_actual_await_protocol_return() {
    check("from typing import Generic,TypeVar,Generator\nT=TypeVar('T')\nclass Awaitable(Generic[T]):\n def __await__(self)->Generator[None,None,int]:...\nasync def use(value:Awaitable[str]):\n return await value\n", "Awaitable[str]", Some("Generator[None, None, int]"), "int");
    check(
        "from typing import Awaitable\nasync def use(value:Awaitable[str]):\n return await value\n",
        "Awaitable[str]",
        None,
        "str",
    );
}

#[test]
fn absent_await_protocol_and_visible_unknown_slot_decorator_stay_unknown() {
    check("from typing import Generic,TypeVar\nT=TypeVar('T')\nclass Awaitable(Generic[T]):pass\nasync def use(value:Awaitable[str]):\n return await value\n", "Awaitable[str]", None, "Unknown");
    check("from typing import Generator\ndef replace(fn):return lambda self:42\nclass Reader:\n @replace\n def __await__(self)->Generator[None,None,int]:...\nasync def use(value:Reader):\n return await value\n", "Reader", Some("Generator[None, None, int]"), "Unknown");
}
