use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn result(source: &str, expression: &str) -> Type {
    let db = Analysis::for_project(Path::new("/function-decorator-controls"));
    let id = db.set_file_text(Path::new("/function-decorator-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|node| node.syntax().text() == expression)
        .last()
        .unwrap();
    Inferer::new(&db).type_of_expr(&file, &node)
}

#[test]
fn stacked_replacements_apply_bottom_up_and_factories_use_their_returned_callable() {
    let source = "from typing import Callable\ndef first(fn:Callable[[int],int])->Callable[[int],str]:...\ndef second(fn:Callable[[int],str])->Callable[[int],bytes]:...\ndef factory()->Callable[[Callable[[int],int]],Callable[[int],str]]:...\n@second\n@first\ndef stacked(value:int)->int:...\n@factory()\ndef made(value:int)->int:...\nstacked(1)\nmade(1)\n";
    assert_eq!(result(source, "stacked(1)").to_string(), "bytes");
    assert_eq!(result(source, "made(1)").to_string(), "str");
}

#[test]
fn unknown_decorators_do_not_retain_the_undecorated_return_promise() {
    let source = "def replace(fn):...\n@replace\ndef decorated(value:int)->int:...\ndecorated(1)\n";
    assert!(result(source, "decorated(1)").is_unknown());
    let method = "def replace(fn):...\nclass C:\n @replace\n def slot(self)->int:...\nC().slot()\n";
    assert!(result(method, "C().slot()").is_unknown());
}

#[test]
fn canonical_metadata_and_descriptors_keep_their_existing_value_contracts() {
    let source = "from typing import final, overload\n@final\ndef ordinary(value:int)->str:...\nclass C:\n @property\n def value(self)->str:...\n @staticmethod\n def static(value:int)->str:...\n @classmethod\n def method(cls,value:int)->str:...\nordinary(1)\nC().value\nC.static(1)\nC.method(1)\n";
    for expression in ["ordinary(1)", "C().value", "C.static(1)", "C.method(1)"] {
        assert_eq!(
            result(source, expression).to_string(),
            "str",
            "{expression}"
        );
    }
}

#[test]
fn local_metadata_spelling_is_an_actual_replacement_not_a_canonical_identity() {
    let source = "from typing import Callable\ndef final(fn:Callable[[int],int])->Callable[[int],str]:...\n@final\ndef decorated(value:int)->int:...\ndecorated(1)\n";
    assert_eq!(result(source, "decorated(1)").to_string(), "str");
}

#[test]
fn arbitrary_decorator_values_and_recursive_dependencies_remain_conservative() {
    let source =
        "def replace(fn)->int:...\n@replace\ndef decorated(value:int)->str:...\ndecorated\n";
    assert_eq!(result(source, "decorated").to_string(), "int");
    let recursive = "@decorated\ndef decorated(value:int)->str:...\ndecorated(1)\n";
    assert!(result(recursive, "decorated(1)").is_unknown());
}

#[test]
fn context_manager_projection_requires_canonical_iterator_family() {
    for source in [
        "from contextlib import contextmanager\n@contextmanager\ndef bad()->list[str]:...\nbad()\n",
        "from contextlib import contextmanager\nfrom typing import Generic,TypeVar\nT=TypeVar('T')\nclass Generator(Generic[T]):...\n@contextmanager\ndef bad()->Generator[str]:...\nbad()\n",
    ] {
        let Type::Instance(class, arguments) = result(source, "bad()") else {
            panic!("context manager value required");
        };
        assert_eq!(class.name, "_GeneratorContextManager");
        assert!(arguments.first().unwrap().is_unknown());
    }
    let source = "from contextlib import contextmanager\nfrom typing import Iterator\n@contextmanager\ndef good()->Iterator[str]:...\ngood()\n";
    let Type::Instance(class, arguments) = result(source, "good()") else {
        panic!("context manager value required");
    };
    assert_eq!(class.name, "_GeneratorContextManager");
    assert_eq!(arguments[0].to_string(), "str");
}

#[test]
fn decorator_stack_budget_is_enforced_for_function_and_method_paths() {
    for count in [32, 33] {
        let decorators = "@final\n".repeat(count);
        let source =
            format!("from typing import final\n{decorators}def value()->int:...\nvalue()\n");
        let actual = result(&source, "value()");
        assert_eq!(actual.is_unknown(), count > 32);
        let decorators = " @final\n".repeat(count);
        let source = format!("from typing import final\nclass C:\n{decorators} def value(self)->int:...\nC().value()\n");
        let actual = result(&source, "C().value()");
        assert_eq!(actual.is_unknown(), count > 32);
    }
}
