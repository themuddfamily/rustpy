use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};

fn imported(owner: &str, main: &str, member: &str, result: &str) {
    let directory = tempfile::TempDir::with_prefix("async-type-visible-authority").unwrap();
    std::fs::write(directory.path().join("owner.py"), owner).unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, main).unwrap();
    let db = Analysis::for_project(directory.path());
    let id = db.set_file_text(&path, main);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let inferer = Inferer::new(&db);
    let read = |needle: &str| {
        assert_eq!(main.matches(needle).count(), 1);
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
    let receiver = read("receiver #premise");
    let reference = receiver.class_ref().unwrap();
    let source = db.file(reference.file).unwrap();
    assert_eq!(
        source.path,
        dunce::canonicalize(directory.path().join("owner.py")).unwrap()
    );
    assert_eq!(source.text.as_ref(), owner);
    let old = inferer.lookup_member(&receiver, member).unwrap();
    if member == "reflect" {
        let Type::Class(class, _) = old else {
            panic!("actual retained builtin callee premise");
        };
        assert_eq!(class.name, "type");
        assert_eq!(
            db.file(class.file).unwrap().path,
            std::path::Path::new("<typeshed>/builtins.pyi")
        );
    } else {
        let declaration = source
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::FuncDef::cast)
            .find(|function| {
                function.name_text() == member
                    && reference.range.contains_range(function.text_range())
            })
            .expect("actual class-owned physical async method");
        assert!(declaration.is_async());
        let raw = inferer.signature(&source, &declaration, Some(reference));
        assert!(raw.def.is_some(), "physical declared method premise");
        let Type::Instance(coroutine, arguments) = &raw.ret else {
            panic!("actual raw async return premise");
        };
        assert_eq!(coroutine.name, "Coroutine");
        assert_eq!(arguments.len(), 3);
        assert_eq!(arguments[2].to_string(), "str");
        if declaration.decorators().next().is_some() {
            assert!(
                !old.signature().is_some_and(|signature| {
                    signature.def.as_ref().is_some_and(|definition| {
                        definition.file == source.id && definition.range == declaration.text_range()
                    })
                }),
                "transformed replacement must not retain physical async promise"
            );
        } else {
            assert!(
                old.signature().unwrap().def.is_some(),
                "clean physical method remains declaration-backed"
            );
        }
    }
    assert_eq!(read("result #result").to_string(), result);
}

#[test]
fn unknown_decorator_cannot_authorize_the_retained_async_slot_signature() {
    imported("def replace(fn):return lambda self:42\nclass Reader:\n def __aiter__(self)->'Reader':return self\n @replace\n async def __anext__(self)->str:...\n",
        "from owner import Reader\nasync def use(receiver:Reader):\n receiver #premise\n async for result in receiver:\n  result #result\n", "__anext__", "Unknown");
    imported("class Reader:\n def __aiter__(self)->'Reader':return self\n async def __anext__(self)->str:...\n",
        "from owner import Reader\nasync def use(receiver:Reader):\n receiver #premise\n async for result in receiver:\n  result #result\n", "__anext__", "str");
}

#[test]
fn imported_qualified_alias_checks_the_actual_owner_namespace() {
    for (tail, expected) in [
        ("", "type[int]"),
        (
            "def replacement(value:int)->str:return 'made'\nHolder.reflect=replacement\n",
            "Unknown",
        ),
    ] {
        imported(&format!("class Holder:\n reflect=type\n{tail}"),
            "from owner import Holder\nreceiver=Holder\nreceiver #premise\nresult=receiver.reflect(1)\nresult #result\n", "reflect", expected);
    }
}
