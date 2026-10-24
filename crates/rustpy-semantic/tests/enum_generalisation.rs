use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

/// Type of the last expression statement, possibly nested in a function body.
fn result(source: &str) -> String {
    let db = Analysis::for_project(Path::new("/enum-generalisation"));
    let id = db.set_file_text(Path::new("/enum-generalisation/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let statement = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::ExprStmt::cast)
        .last()
        .expect("expression statement");
    Inferer::new(&db)
        .type_of_expr(&file, &statement.expr().unwrap())
        .to_string()
}

#[test]
fn mixin_derived_and_metaclass_enums_have_literal_members() {
    for (source, expected) in [
        ("from enum import Enum\nclass C(str, Enum):\n A='a'\nC.A\n", "Literal[C.A]"),
        ("from enum import Enum\nclass C(str, Enum):\n A='a'\nC.A.value\n", "Literal[\"a\"]"),
        ("from enum import Enum\nclass C(str, Enum):\n A='a'\nC.A.upper()\n", "str"),
        ("from enum import Enum\nclass C(int, Enum):\n A=1\nC.A.value\n", "Literal[1]"),
        ("from enum import Enum\nclass B(Enum):\n def f(self)->int: return 1\nclass C(B):\n A=1\nC.A\n", "Literal[C.A]"),
        ("from enum import Enum\nclass B(Enum):\n def f(self)->int: return 1\nclass C(B):\n A=1\nC.A.f()\n", "int"),
        ("from enum import EnumMeta\nclass C(metaclass=EnumMeta):\n A=1\nC.A\n", "Literal[C.A]"),
        ("from enum import EnumMeta\nclass M(EnumMeta): pass\nclass C(metaclass=M):\n A=1\nC.A\n", "Literal[C.A]"),
        ("from enum import ReprEnum\nclass C(ReprEnum):\n A=1\nC.A\n", "Literal[C.A]"),
        ("import enum\n@enum.unique\nclass C(enum.Enum):\n A=1\nC.A\n", "Literal[C.A]"),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
}

#[test]
fn non_literal_member_values_keep_member_types() {
    for (source, expected) in [
        ("from enum import Enum\nclass C(Enum):\n A=(1,2)\nC.A\n", "Literal[C.A]"),
        ("from enum import Enum\nclass C(Enum):\n A=(1,2)\nC.A.value\n", "tuple[int, int]"),
        ("from enum import Enum\nclass C(Enum):\n A=1.5\nC.A._value_\n", "float"),
        ("from enum import Enum\nclass C(Enum):\n A=object()\nC.A.value\n", "object"),
        ("from enum import Enum\nclass C(Enum):\n A=(1,2)\nC.A.name\n", "Literal[\"A\"]"),
        ("from enum import Enum, auto\nclass C(Enum):\n A=(1,2)\n B=auto()\nC.B.value\n", "int"),
        ("from enum import Enum, auto\nclass C(Enum):\n A=auto()\nC.A.value\n", "Literal[1]"),
        ("from enum import StrEnum, auto\nclass C(StrEnum):\n A=auto()\nC.A.value\n", "Literal[\"a\"]"),
        // CPython converts Enum's integer auto() through the str mixin ('1'
        // from 3.12; the raw int before), so only `str` is version-independent.
        ("from enum import Enum, auto\nclass C(str, Enum):\n A=auto()\nC.A.value\n", "str"),
        ("from enum import Enum\nclass P(Enum):\n E=(1.0,2.0)\n def __init__(self, mass: float, radius: float):\n  self.mass=mass\n  self.radius=radius\nP.E\n", "Literal[P.E]"),
        ("from enum import Enum\nclass P(Enum):\n E=(1.0,2.0)\n def __init__(self, mass: float, radius: float):\n  self.mass=mass\n  self.radius=radius\nP.E.mass\n", "float"),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
}

#[test]
fn non_members_keep_their_own_types() {
    for (source, expected) in [
        (
            "from enum import Enum\nclass C(Enum):\n A=1\n def f(self)->int: return 1\nC.f\n",
            "(self: C) -> int",
        ),
        (
            "from enum import Enum\nclass C(Enum):\n A=1\n class N: pass\nC.N\n",
            "type[N]",
        ),
        (
            "from enum import Enum, nonmember\nclass C(Enum):\n A=1\n B=nonmember(3)\nC.B\n",
            "Literal[3]",
        ),
        (
            "from enum import Enum, member\nclass C(Enum):\n A=1\n B=member(3)\nC.B\n",
            "Literal[C.B]",
        ),
        (
            "from enum import Enum, member\nclass C(Enum):\n A=1\n B=member(3)\nC.B.value\n",
            "Literal[3]",
        ),
        (
            "from enum import Enum\nclass C(Enum):\n A=1\n f=lambda self: 1\nC.A\n",
            "Literal[C.A]",
        ),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
    // `_ignore_` names, private names and descriptors are not members.
    let source = "from enum import Enum\nclass D:\n def __get__(self, obj:object, owner:type)->str: return ''\nclass C(Enum):\n _ignore_=['B']\n A=1\n B=2\n __p=3\n d=D()\n Z=4\ndef f(c: C):\n if c is C.A:\n  return\n c\n";
    assert_eq!(result(source), "Literal[C.Z]");
}

#[test]
fn functional_enums_and_typed_dicts_are_classes() {
    for (source, expected) in [
        ("from enum import Enum\nColor=Enum('Color','A B')\nColor.A\n", "Literal[Color.A]"),
        ("from enum import Enum\nColor=Enum('Color',['A','B'])\nColor.B.value\n", "Literal[2]"),
        ("from enum import Enum\nColor=Enum('Color',[('A',5),('B',6)])\nColor.B.value\n", "Literal[6]"),
        ("from enum import Enum\nColor=Enum('Color',{'A':1})\nColor.A.value\n", "Literal[1]"),
        ("from enum import IntEnum\nColor=IntEnum('Color','A B')\nColor.B.value\n", "Literal[2]"),
        ("from enum import Flag\nColor=Flag('Color','A B C')\nColor.C.value\n", "Literal[4]"),
        ("from enum import Enum\nColor=Enum('Color','A B')\nColor\n", "type[Color]"),
        ("from enum import Enum\nColor=Enum('Color','A B')\nColor(1)\n", "Color"),
        ("from enum import Enum\nColor=Enum('Color','A B')\nColor['A']\n", "Color"),
        ("from enum import Enum\nColor=Enum('Color','A B')\nfor m in Color:\n pass\nm\n", "Color"),
        ("from enum import Enum\nColor=Enum('Color','A B')\ndef f(c: Color):\n if c is Color.A:\n  return\n c\n", "Literal[Color.B]"),
        ("from typing import TypedDict\nTD=TypedDict('TD',{'a':int},total=False)\nTD(a=1)\n", "TD"),
        ("from typing import TypedDict\nTD=TypedDict('TD',{'a':int},total=False)\ndef f(x:TD):\n x\n", "TD"),
        ("from typing import TypedDict\nTD=TypedDict('TD',{'a':int},total=False)\ndef f(x:TD):\n x['a']\n", "int"),
        ("from typing import TypedDict\nTD=TypedDict('TD',{'a':int},total=False)\ndef f(x:TD):\n x.get('a')\n", "int | None"),
        ("from typing import TypedDict\nclass TD(TypedDict):\n a:int\n b:str\ndef f(x:TD):\n x['b']\n", "str"),
        ("from typing import TypedDict\nclass TD(TypedDict):\n a:int\ndef f(x:TD):\n x.get('a', '')\n", "int | Literal[\"\"]"),
        ("from typing import TypedDict\nclass TD(TypedDict):\n a:int\nx:TD={'a':1}\nx['a']\n", "int"),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
    let db = Analysis::for_project(Path::new("/enum-generalisation-td"));
    let id = db.set_file_text(
        Path::new("/enum-generalisation-td/m.py"),
        "from typing import TypedDict\nTD=TypedDict('TD',{'a':int,'b':str},total=False)\nTD\n",
    );
    let file = db.file(id).unwrap();
    let statement = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::ExprStmt::cast)
        .last()
        .unwrap();
    let infer = Inferer::new(&db);
    let Type::Class(class, _) = infer.type_of_expr(&file, &statement.expr().unwrap()) else {
        panic!("functional TypedDict class")
    };
    let fields = infer.typed_dict_fields(&class, &[]).expect("schema");
    let fields: Vec<_> = fields
        .iter()
        .map(|(name, value, required, _)| format!("{name}:{value}:{required}"))
        .collect();
    assert_eq!(fields, ["a:int:false", "b:str:false"]);
}

#[test]
fn enum_class_operations_and_narrowing_produce_members() {
    for (source, expected) in [
        ("from enum import Enum\nclass C(Enum):\n A=1\n B=2\nfor m in C:\n pass\nm\n", "C"),
        ("from enum import Enum\nclass C(Enum):\n A=1\nC['A']\n", "C"),
        ("from enum import Enum\nclass C(Enum):\n A=1\nC(1)\n", "C"),
        ("from enum import Enum\nclass C(Enum):\n A=(1,2)\n B=(3,4)\ndef f(c: C):\n if c is C.A:\n  return\n c\n", "Literal[C.B]"),
        ("from enum import Enum\nclass C(str, Enum):\n A='a'\n B='b'\ndef f(c: C):\n if c is C.A:\n  return\n c\n", "Literal[C.B]"),
        ("from enum import Enum\nclass C(Enum):\n A=1\n B=2\n def f(self)->int: return 1\ndef g(c: C):\n if c is C.A:\n  return\n c\n", "Literal[C.B]"),
        ("from enum import Enum\nclass C(Enum):\n A=(1,2)\n B=(1,2)\n Z=3\ndef f(c: C):\n if c is C.A:\n  return\n c\n", "Literal[C.Z]"),
        ("from enum import Enum\nclass C(Enum):\n A=object()\n B=object()\ndef f(c: C):\n match c:\n  case C.A: pass\n  case _:\n   c\n", "Literal[C.B]"),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
}

#[test]
fn exhausted_enum_domains_narrow_to_never() {
    for source in [
        "from enum import Enum\nclass C(Enum):\n A=1\ndef f(c: C):\n if c is C.A:\n  return\n c\n",
        "from enum import Enum\nclass C(Enum):\n A=(1,2)\n B=object()\ndef f(c: C):\n if c is C.A:\n  return\n if c is C.B:\n  return\n c\n",
        "from enum import Enum\nclass C(str, Enum):\n A='a'\ndef f(c: C):\n if c == C.A:\n  pass\n else:\n  c\n",
        "from enum import Enum\nclass C(Enum):\n A=1\n B=2\ndef f(c: C):\n match c:\n  case C.A: pass\n  case C.B: pass\n  case _:\n   c\n",
        "from enum import Enum\nclass C(Enum):\n A=1\n B=2\ndef f(c: C):\n match c:\n  case C.A | C.B: pass\n  case _:\n   c\n",
        "from enum import Enum\nColor=Enum('Color','A B')\ndef f(c: Color):\n match c:\n  case Color.A | Color.B: pass\n  case _:\n   c\n",
    ] {
        assert_eq!(result(source), "Never", "{source}");
    }
    // A non-exhaustive match keeps the declared or residual type.
    assert_eq!(
        result("from enum import Enum\nclass C(Enum):\n A=1\n B=2\ndef f(c: C):\n match c:\n  case C.A: pass\n  case _:\n   c\n"),
        "Literal[C.B]"
    );
}

#[test]
fn enum_classes_are_iterables_of_members() {
    assert_eq!(
        result("from enum import Enum\nclass C(Enum):\n A=1\nlist(C)\n"),
        "list[C]"
    );
    assert_eq!(
        result("from enum import Enum\nColor=Enum('Color','A B')\nset(Color)\n"),
        "set[Color]"
    );
}
