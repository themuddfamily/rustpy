use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, receiver: &str, expected: &str, canonical: bool) {
    let db = Analysis::for_project(Path::new("/direct-await-controls"));
    let id = db.set_file_text(Path::new("/direct-await-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let expressions: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::AwaitExpr::cast)
        .collect();
    assert_eq!(expressions.len(), 1);
    let expression = &expressions[0];
    let inferer = Inferer::new(&db);
    let value = inferer.type_of_expr(&file, &expression.expr().unwrap());
    assert_eq!(value.to_string(), receiver, "actual operand before await");
    if canonical {
        let owner = db.file(value.class_ref().unwrap().file).unwrap();
        assert!(owner.is_stub);
        assert_eq!(owner.path, Path::new("<typeshed>/typing.pyi"));
    }
    let result = inferer.type_of_expr(&file, &ast::Expr::Await(expression.clone()));
    assert_eq!(
        result.to_string(),
        expected,
        "actual awaited protocol result"
    );
}
#[test]
fn canonical_awaitable_and_coroutine_contracts_remain_precise() {
    check(
        "from typing import Awaitable\nasync def use(value:Awaitable[str]):\n return await value\n",
        "Awaitable[str]",
        "str",
        true,
    );
    check("from typing import Coroutine\nasync def use(value:Coroutine[None,None,int]):\n return await value\n","Coroutine[None, None, int]","int",true);
}
#[test]
fn inherited_protocol_and_canonical_final_class_keep_actual_generator_result() {
    check("from typing import Generator\nclass Base:\n def __await__(self)->Generator[None,None,int]:...\nclass Reader(Base):pass\nasync def use(value:Reader):\n return await value\n","Reader","int",false);
    check("from typing import Generator,final\n@final\nclass Reader:\n def __await__(self)->Generator[None,None,int]:...\nasync def use(value:Reader):\n return await value\n","Reader","int",false);
}
#[test]
fn user_generator_spelling_and_absent_future_protocol_are_not_authority() {
    check("from typing import Generic,TypeVar\nY=TypeVar('Y')\nS=TypeVar('S')\nR=TypeVar('R')\nclass Generator(Generic[Y,S,R]):pass\nclass Reader:\n def __await__(self)->Generator[None,None,int]:...\nasync def use(value:Reader):\n return await value\n","Reader","Unknown",false);
    check("from typing import Generic,TypeVar\nT=TypeVar('T')\nclass Future(Generic[T]):pass\nasync def use(value:Future[str]):\n return await value\n","Future[str]","Unknown",false);
}
#[test]
fn visible_slot_replacement_and_unknown_class_decorator_refuse_old_contract() {
    check("from typing import Generator\nclass Reader:\n def __await__(self)->Generator[None,None,int]:...\nReader.__await__=lambda self:42\nasync def use(value:Reader):\n return await value\n","Reader","Unknown",false);
    check("from typing import Generator\ndef replace(cls):return cls\n@replace\nclass Reader:\n def __await__(self)->Generator[None,None,int]:...\nasync def use(value:Reader):\n return await value\n","Reader","Unknown",false);
}
#[test]
fn awaited_never_any_and_unknown_preserve_distinct_results() {
    check("from typing import Awaitable,Never\nasync def use(value:Awaitable[Never]):\n return await value\n","Awaitable[Never]","Never",true);
    check(
        "from typing import Any\nasync def use(value:Any):\n return await value\n",
        "Any",
        "Any",
        false,
    );
    check(
        "async def use(value):\n return await value\n",
        "Unknown",
        "Unknown",
        false,
    );
    // Bottom operand never reaches the awaiting operation itself.
    let source="from typing import Never\ndef die()->Never:raise RuntimeError\nasync def use():\n return await die()\n";
    let db = Analysis::for_project(Path::new("/direct-await-bottom"));
    let id = db.set_file_text(Path::new("/direct-await-bottom/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let expression = file
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::AwaitExpr::cast)
        .unwrap();
    let inf = Inferer::new(&db);
    assert!(matches!(
        inf.type_of_expr(&file, &expression.expr().unwrap()),
        Type::Never
    ));
    assert!(matches!(
        inf.type_of_expr(&file, &ast::Expr::Await(expression)),
        Type::Never
    ));
}

#[test]
fn canonical_future_and_task_use_actual_await_slot_generator_return() {
    check(
        "from asyncio import Future\nasync def use(value:Future[int]):\n return await value\n",
        "Future[int]",
        "int",
        false,
    );
    check(
        "from asyncio import Task\nasync def use(value:Task[str]):\n return await value\n",
        "Task[str]",
        "str",
        false,
    );
}

#[test]
fn ordinary_generator_is_not_awaitable_without_admitted_coroutine_provenance() {
    check("from typing import Generator\nasync def use(value:Generator[None,None,int]):\n return await value\n", "Generator[None, None, int]", "Unknown", true);
}

#[test]
fn actual_synchronous_await_slot_never_is_bottom_before_suspension() {
    let source = "from typing import Never\nclass Reader:\n def __await__(self)->Never:raise RuntimeError\nasync def use(value:Reader):\n return await value\n";
    let db = Analysis::for_project(Path::new("/direct-await-never-slot"));
    let id = db.set_file_text(Path::new("/direct-await-never-slot/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let expression = file
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::AwaitExpr::cast)
        .unwrap();
    let inferer = Inferer::new(&db);
    let receiver = inferer.type_of_expr(&file, &expression.expr().unwrap());
    assert_eq!(receiver.to_string(), "Reader");
    let class = receiver.class_ref().unwrap();
    let owner = db.file(class.file).unwrap();
    let declaration = owner
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .find(|function| {
            function.name_text() == "__await__" && class.range.contains_range(function.text_range())
        })
        .unwrap();
    assert!(
        matches!(
            inferer.signature(&owner, &declaration, Some(class)).ret,
            Type::Never
        ),
        "actual physical declaration premise before transformed slot"
    );
    let slot = inferer
        .lookup_member(&receiver, "__await__")
        .expect("actual owned slot");
    assert!(
        matches!(inferer.call_result(&slot, &[], &[]), Type::Never),
        "actual slot invocation premise before await"
    );
    assert!(matches!(
        inferer.type_of_expr(&file, &ast::Expr::Await(expression)),
        Type::Never
    ));
}
