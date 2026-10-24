use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::{collections::BTreeSet, path::Path};

fn check(source: &str, needle: &str, expected: &[&str]) {
    let db = Analysis::for_project(Path::new("/narrowing-gaps"));
    let id = db.set_file_text(Path::new("/narrowing-gaps/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let offset = source.find(needle).expect("unique selected read");
    assert_eq!(source.matches(needle).count(), 1);
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|name| name.text_range().start() == rustpy_core::TextSize::new(offset as u32))
        .unwrap();
    let actual = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name));
    let values = match actual {
        Type::Union(values) => values,
        value => vec![value],
    };
    let actual: BTreeSet<_> = values.into_iter().map(|value| value.to_string()).collect();
    let expected: BTreeSet<_> = expected.iter().map(|value| value.to_string()).collect();
    assert!(!actual.is_empty());
    assert_eq!(actual, expected, "{source}\n{needle}");
}

#[test]
fn quoted_heads_and_method_decorators_need_actual_canonical_authority() {
    let source="from typing import TypeGuard as RealGuard\nclass Holder:pass\ndef good(value:object)->'RealGuard[str]':return isinstance(value,str)\ndef use(value:int|str):\n if good(value):\n  value #alias\n";
    check(source, "value #alias", &["str"]);
    let source="from typing import Generic,TypeVar\nT=TypeVar('T')\nclass TypeGuard(Generic[T]):pass\ndef fake(value:object)->'TypeGuard[str]':return TypeGuard()\ndef use(value:int|str):\n if fake(value):\n  value #fake\n";
    check(source, "value #fake", &["int", "str"]);
    let source="from typing_extensions import TypeIs\ndef staticmethod(method):return method\nclass Checker:\n @staticmethod\n def guard(value:object)->TypeIs[str]:return isinstance(value,str)\ndef use(value:int|str):\n if Checker.guard(value):\n  value #fake_decorator\n";
    check(source, "value #fake_decorator", &["int", "str"]);
}
#[test]
fn overload_unknown_or_unmatched_selectors_cannot_grant_a_payload() {
    let source="from typing import overload,Literal,TypeGuard\n@overload\ndef guard(value:object,kind:Literal['text'])->TypeGuard[str]:...\n@overload\ndef guard(value:object,kind:Literal['number'])->TypeGuard[int]:...\ndef guard(value:object,kind:str)->bool:return True\ndef use(value:int|str,unknown):\n if guard(value,unknown):\n  value #unknown_selector\n if guard(value,'text'):\n  value #text\n if guard(value,'absent'):\n  value #no_match\n";
    check(source, "value #unknown_selector", &["int", "str"]);
    check(source, "value #text", &["str"]);
    check(source, "value #no_match", &["int", "str"]);
}
