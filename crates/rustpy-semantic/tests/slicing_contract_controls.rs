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
fn builtin_slices_and_ordinary_tuple_index_preserve_actual_types() {
    check(
        "value=[1,2]\nresult=value[1:]\nresult #list\n",
        "[1,2]",
        "list[int]",
        &[("result #list", "list[int]")],
    );
    check(
        "value='text'\nresult=value[::-1]\nresult #str\n",
        "'text'",
        "Literal[\"text\"]",
        &[("result #str", "str")],
    );
    check(
        "value=b'text'\nresult=value[1:]\nresult #bytes\n",
        "b'text'",
        "Literal[b\"text\"]",
        &[("result #bytes", "bytes")],
    );
    check(
        "value=(1,'text')\nresult=value[1]\nresult #index\n",
        "(1,'text')",
        "tuple[int, str]",
        &[("result #index", "str")],
    );
}
#[test]
fn invalid_builtin_bounds_do_not_invalidate_custom_slice_objects() {
    check("value=(1,'text')\nzero=value[::0]\nbad=value['wrong':]\nnone=value[None:None:None]\nzero #zero\nbad #bad\nnone #none\n","(1,'text')","tuple[int, str]",&[("zero #zero","Unknown"),("bad #bad","Unknown"),("none #none","tuple[int, str]")]);
    check(
        "value=[1,2]\nresult=value[::0]\nresult #zero\n",
        "[1,2]",
        "list[int]",
        &[("result #zero", "Unknown")],
    );
    check("class Accept:\n def __getitem__(self,index:slice)->str:return 'accepted'\nobj=Accept()\nresult=obj[::0]\nresult #custom\n","Accept()","Accept",&[("result #custom","str")]);
}
#[test]
fn dynamic_and_multiple_indices_keep_sequence_shape_and_real_custom_dispatch() {
    check(
        "def use(value:tuple[int,str],start:int):\n result=value[start:]\n result #dynamic\n",
        "value",
        "tuple[int, str]",
        &[("result #dynamic", "tuple[int | str]")],
    );
    check("class Accept:\n def __getitem__(self,index:tuple[slice,int])->str:return 'accepted'\nobj=Accept()\nresult=obj[:1,0]\nresult #multi\n","Accept()","Accept",&[("result #multi","str")]);
    check(
        "value=(1,'text')\nresult=value[:1,0]\nresult #invalid_multi\n",
        "(1,'text')",
        "tuple[int, str]",
        &[("result #invalid_multi", "Unknown")],
    );
}
