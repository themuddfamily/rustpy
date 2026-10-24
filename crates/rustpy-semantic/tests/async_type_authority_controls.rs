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
fn sync_async_and_comprehension_modes_use_their_actual_slots() {
    check("from typing import Iterator,AsyncIterator\nclass Dual:\n def __iter__(self)->Iterator[int]:...\n def __aiter__(self)->AsyncIterator[str]:...\nasync def use(stream:Dual):\n for sync in stream:\n  sync #sync\n async for asynchronous in stream:\n  asynchronous #async\n items=[part async for part in stream]\n items #comp\n","stream","Dual",&[("sync #sync","int"),("asynchronous #async","str"),("items #comp","list[str]")]);
    check("from typing import AsyncIterator\nasync def use(stream:AsyncIterator[tuple[int,str,str,bool]]):\n async for head,*middle,tail in stream:\n  head #head\n  middle #middle\n  tail #tail\n items=[middle async for head,*middle,tail in stream]\n items #comp\n","stream","AsyncIterator[tuple[int, str, str, bool]]",&[("head #head","int"),("middle #middle","list[str]"),("tail #tail","bool"),("items #comp","list[list[str]]")]);
}
#[test]
fn unknown_or_wrong_async_protocols_do_not_borrow_names_or_sync_fallback() {
    check("from typing import Iterator\nclass SyncOnly:\n def __iter__(self)->Iterator[str]:...\nasync def use(stream:SyncOnly):\n async for item in stream:\n  item #sync_only\n","stream","SyncOnly",&[("item #sync_only","Unknown")]);
    check("from typing import Generic,TypeVar\nT=TypeVar('T')\nclass Awaitable(Generic[T]):pass\nclass Reader:\n def __aiter__(self)->'Reader':return self\n def __anext__(self)->Awaitable[str]:...\nasync def use(stream:Reader):\n async for item in stream:\n  item #fake_awaitable\n","stream","Reader",&[("item #fake_awaitable","Unknown")]);
    check("class Reader:\n def __aiter__(self)->'Reader':return self\n async def __anext__(self)->str:...\ndef replacement(self)->int:return 1\nReader.__anext__=replacement\nasync def use(stream:Reader):\n async for item in stream:\n  item #retired\n","stream","Reader",&[("item #retired","Unknown")]);
}
#[test]
fn type_reflection_requires_captured_builtin_identity() {
    check(
        "from builtins import type as reflect\nresult=reflect(1)\nresult #alias\n",
        "reflect",
        "type[type]",
        &[("result #alias", "type[int]")],
    );
    check("from typing import Callable\ndef use(type:Callable[[int],str]):\n result=type(1)\n result #parameter\n","type","(p0: int, /) -> str",&[("result #parameter","str")]);
    check("class Maker(type):\n def __call__(cls,value:int)->str:return 'made'\nclass Product(metaclass=Maker):pass\nresult=Product(1)\nresult #metaclass\n","Product","type[Product]",&[("result #metaclass","str")]);
    check("import builtins\ndef replacement(value:int)->str:return 'made'\nbuiltins.type=replacement\nresult=builtins.type(1)\nresult #retired\n","1","Literal[1]",&[("result #retired","Unknown")]);
}
