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
    let inferer = Inferer::new(&db);
    // Verify that the exercised condition has maintained declaration-backed
    // callable metadata before asking for its branch result.
    let call_offset = source[..offset].rfind("if ").expect("guard condition") + 3;
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.text_range().start() == rustpy_core::TextSize::new(call_offset as u32))
        .unwrap();
    let callable = inferer.type_of_expr(&file, &call.callee().unwrap());
    let declared = match &callable {
        Type::Function(signature) | Type::BoundMethod(signature, _) | Type::Callable(signature) => {
            signature.def.is_some()
        }
        Type::Overloaded(signatures) => {
            !signatures.is_empty() && signatures.iter().all(|signature| signature.def.is_some())
        }
        _ => false,
    };
    assert!(
        declared,
        "actual captured callable premise: {callable:?}\n{source}"
    );
    let actual = inferer.type_of_expr(&file, &ast::Expr::Name(name));
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
fn quoted_and_future_predicate_annotations() {
    let source = "from typing import TypeGuard\ndef guard(value:object)->'TypeGuard[str]':return isinstance(value,str)\ndef use(value:int|str):\n if guard(value):\n  value #yes\n else:\n  value #no\n";
    check(source, "value #yes", &["str"]);
    check(source, "value #no", &["int", "str"]);
    let source = "from __future__ import annotations\nfrom typing_extensions import TypeIs\ndef guard(value:object)->TypeIs[str]:return isinstance(value,str)\ndef use(value:int|str):\n if guard(value):\n  value #yes\n else:\n  value #no\n";
    check(source, "value #yes", &["str"]);
    check(source, "value #no", &["int"]);
    let source = "from typing import Generic,TypeVar\nT=TypeVar('T')\nclass TypeGuard(Generic[T]):pass\ndef guard(value:object)->'TypeGuard[str]':return TypeGuard()\ndef use(value:int|str):\n if guard(value):\n  value #fake\n";
    check(source, "value #fake", &["int", "str"]);
}

#[test]
fn method_predicates_narrow_the_explicit_argument() {
    let source = "from typing_extensions import TypeIs\nclass Checker:\n def guard(self,value:object)->TypeIs[str]:return isinstance(value,str)\ndef use(value:int|str,checker:Checker):\n if checker.guard(value):\n  value #yes\n else:\n  value #no\n";
    check(source, "value #yes", &["str"]);
    check(source, "value #no", &["int"]);
    let source = "from typing_extensions import TypeIs\nclass Checker:\n @staticmethod\n def guard(value:object)->TypeIs[str]:return isinstance(value,str)\ndef use(value:int|str,checker:Checker):\n if Checker.guard(value):\n  value #yes\n else:\n  value #no\n";
    check(source, "value #yes", &["str"]);
    check(source, "value #no", &["int"]);
    let source = "from typing_extensions import TypeIs\nclass Checker:\n @classmethod\n def guard(cls,value:object)->TypeIs[str]:return isinstance(value,str)\ndef use(value:int|str,checker:Checker):\n if Checker.guard(value):\n  value #yes\n else:\n  value #no\n";
    check(source, "value #yes", &["str"]);
    check(source, "value #no", &["int"]);
}

#[test]
fn selected_overload_payload_and_rebound_bool_control() {
    let source = "from typing import overload,Literal,TypeGuard\n@overload\ndef guard(value:object,kind:Literal['text'])->TypeGuard[str]:...\n@overload\ndef guard(value:object,kind:Literal['number'])->TypeGuard[int]:...\ndef guard(value:object,kind:str)->bool:return True\ndef use(value:int|str):\n if guard(value,'text'):\n  value #text\n if guard(value,'number'):\n  value #number\n";
    check(source, "value #text", &["str"]);
    check(source, "value #number", &["int"]);
    let source = "from typing import TypeGuard\ndef guard(value:object)->TypeGuard[str]:return isinstance(value,str)\ndef ordinary(value:object)->bool:return True\nguard=ordinary\ndef use(value:int|str):\n if guard(value):\n  value #rebound\n else:\n  value #ordinary\n";
    check(source, "value #rebound", &["int", "str"]);
    check(source, "value #ordinary", &["int", "str"]);
}

#[test]
fn representable_structural_and_invariant_typeis_intersections() {
    let source = "from typing import Protocol\nfrom typing_extensions import TypeIs\nclass Named(Protocol):\n name:str\nclass Person:\n name:str='person'\nclass Number:\n count:int=1\ndef guard(value:object)->TypeIs[Named]:return hasattr(value,'name')\ndef use(value:Person|Number):\n if guard(value):\n  value #yes\n else:\n  value #no\n";
    check(source, "value #yes", &["Person"]);
    check(source, "value #no", &["Number"]);
    let source = "from typing_extensions import TypeIs\ndef guard(value:list[int]|list[str])->TypeIs[list[int]]:return all(isinstance(item,int) for item in value)\ndef use(value:list[int]|list[str]):\n if guard(value):\n  value #yes\n else:\n  value #no\n";
    check(source, "value #yes", &["list[int]"]);
    check(source, "value #no", &["list[str]"]);
}
