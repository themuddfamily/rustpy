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
fn exact_tuple_slices_preserve_container_and_python_signed_bounds() {
    check("value=(1,'text',True)\ntail=value[1:]\nreverse=value[::-1]\nempty=value[:0]\nclipped=value[-20:20:2]\ntail #tail\nreverse #reverse\nempty #empty\nclipped #clipped\n","(1,'text',True)","tuple[int, str, bool]",&[("tail #tail","tuple[str, bool]"),("reverse #reverse","tuple[bool, str, int]"),("empty #empty","tuple[()]"),("clipped #clipped","tuple[int, bool]")]);
}
#[test]
fn actual_slice_argument_selects_custom_getitem_overload() {
    check("from typing import overload\nclass Picker:\n @overload\n def __getitem__(self,key:int)->int:...\n @overload\n def __getitem__(self,key:slice)->str:...\n def __getitem__(self,key:int|slice)->int|str:return 'text'\nobj=Picker()\nindexed=obj[0]\nsliced=obj[1:]\nindexed #index\nsliced #slice\n","Picker()","Picker",&[("indexed #index","int"),("sliced #slice","str")]);
}
#[test]
fn async_iteration_uses_async_protocol_instead_of_sync_protocol() {
    check("from typing import AsyncIterator\nasync def use(stream:AsyncIterator[str]):\n stream #producer\n async for element in stream:\n  element #item\n","stream", "AsyncIterator[str]", &[("element #item","str")]);
}
#[test]
fn user_callable_named_type_is_not_the_builtin_type_factory() {
    check(
        "def type(value:int)->str:return 'custom'\nresult=type(1)\nresult #custom\n",
        "type",
        "(value: int) -> str",
        &[("result #custom", "str")],
    );
    check(
        "result=type(1)\nresult #builtin\n",
        "1",
        "Literal[1]",
        &[("result #builtin", "type[int]")],
    );
}
