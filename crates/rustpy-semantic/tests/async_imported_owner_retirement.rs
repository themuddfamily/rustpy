use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
#[test]
fn imported_async_slot_write_is_not_hidden_by_the_callers_clean_module() {
    let directory = tempfile::TempDir::with_prefix("async-owner-retirement").unwrap();
    let reader="class Reader:\n def __aiter__(self)->'Reader':return self\n async def __anext__(self)->str:...\ndef replacement(self)->int:return 1\nReader.__anext__=replacement\n";
    let main="from reader import Reader\nasync def use(stream:Reader):\n stream #premise\n async for item in stream:\n  item #result\n";
    std::fs::write(directory.path().join("reader.py"), reader).unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, main).unwrap();
    let db = Analysis::for_project(directory.path());
    let id = db.set_file_text(&path, main);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let inferer = Inferer::new(&db);
    let read = |needle: &str| {
        let offset = main.find(needle).unwrap();
        let name = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|name| usize::from(name.text_range().start()) == offset)
            .unwrap();
        inferer.type_of_expr(&file, &ast::Expr::Name(name))
    };
    let receiver = read("stream #premise");
    assert_eq!(receiver.to_string(), "Reader");
    let owner = db.file(receiver.class_ref().unwrap().file).unwrap();
    assert_eq!(
        owner.path,
        dunce::canonicalize(directory.path().join("reader.py")).unwrap()
    );
    assert_eq!(owner.text.as_ref(), reader);
    let method = inferer.lookup_member(&receiver, "__anext__").unwrap();
    assert!(
        method.signature().unwrap().def.is_some(),
        "actual maintained old nominal method premise"
    );
    assert_eq!(read("item #result"), Type::Unknown);
}
