use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn result(source: &str) -> Type {
    let db = Analysis::for_project(Path::new("/tuple-pack-controls"));
    let id = db.set_file_text(Path::new("/tuple-pack-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(statement) = module.statements().last().unwrap() else {
        panic!("final expression");
    };
    Inferer::new(&db).type_of_expr(&file, &statement.expr().unwrap())
}
fn tuple(source: &str, expected: &[&str]) {
    let actual = result(source);
    let Type::Tuple(elements) = &actual else {
        panic!("fixed ordered tuple required: {actual:?}");
    };
    assert_eq!(
        elements.iter().map(ToString::to_string).collect::<Vec<_>>(),
        expected
    );
}
const PACK: &str = "from typing import TypeVarTuple,Unpack\nTs=TypeVarTuple('Ts')\ndef pack(*values:Unpack[Ts])->tuple[Unpack[Ts]]:return values\n";
#[test]
fn pep695_pack_keeps_order_and_empty_arity() {
    tuple(
        "def pack[*Ts](*values:*Ts)->tuple[*Ts]:return values\npack(1,'x',True)\n",
        &["int", "str", "bool"],
    );
    tuple(
        "def pack[*Ts](*values:*Ts)->tuple[*Ts]:return values\npack()\n",
        &[],
    );
}
#[test]
fn fixed_star_inputs_expand_and_empty_star_is_not_unknown() {
    tuple(
        &format!("{PACK}pack(*(1,'x'),True)\n"),
        &["int", "str", "bool"],
    );
    tuple(&format!("{PACK}pack(*())\n"), &[]);
}
#[test]
fn unknown_length_star_does_not_capture_empty_pack() {
    assert!(result(&format!("{PACK}values:list[int]=[1]\npack(*values)\n")).is_unknown());
}
#[test]
fn repeated_pack_requires_equal_ordered_sequences() {
    let prefix="from typing import TypeVarTuple,Unpack\nTs=TypeVarTuple('Ts')\ndef pair(left:tuple[Unpack[Ts]],right:tuple[Unpack[Ts]])->tuple[Unpack[Ts]]:...\n";
    tuple(&format!("{prefix}pair((1,'x'),(2,'y'))\n"), &["int", "str"]);
    assert!(result(&format!("{prefix}pair((1,'x'),('y',2))\n")).is_unknown());
}
#[test]
fn fixed_prefix_and_suffix_must_match_and_may_capture_empty() {
    let prefix="from typing import TypeVarTuple,Unpack\nTs=TypeVarTuple('Ts')\ndef middle(value:tuple[int,Unpack[Ts],bytes])->tuple[Unpack[Ts]]:...\n";
    tuple(&format!("{prefix}middle((1,b'end'))\n"), &[]);
    assert!(result(&format!("{prefix}middle(('wrong',True,b'end'))\n")).is_unknown());
    assert!(result(&format!("{prefix}middle((1,True,'wrong'))\n")).is_unknown());
}
#[test]
fn canonical_factory_alias_preserves_pack_identity() {
    tuple("from typing import TypeVarTuple as TVT,Unpack as U\nfactory=TVT\nTs=factory('Ts')\ndef pack(*values:U[Ts])->tuple[U[Ts]]:return values\npack(b'x',1)\n", &["bytes","int"]);
}
#[test]
fn rebound_and_fake_factories_do_not_claim_exact_pack() {
    for source in [
        "from typing import TypeVarTuple,Unpack\nTypeVarTuple=lambda name:object\nTs=TypeVarTuple('Ts')\ndef pack(*values:Unpack[Ts])->tuple[Unpack[Ts]]:return values\npack(1,'x')\n",
        "from typing import Unpack\ndef TypeVarTuple(name):return object\nTs=TypeVarTuple('Ts')\ndef pack(*values:Unpack[Ts])->tuple[Unpack[Ts]]:return values\npack(1,'x')\n",
    ] {
        let actual=result(source);
        assert!(!matches!(&actual, Type::Tuple(values) if values.len()==2 && values[0].to_string()=="int" && values[1].to_string()=="str"), "{actual:?}");
    }
}
#[test]
fn scalar_varargs_union_admission_is_unchanged() {
    let actual = result(
        "from typing import TypeVar\nT=TypeVar('T')\ndef choose(*values:T)->T:...\nchoose(1,'x')\n",
    );
    assert!(
        matches!(&actual, Type::Union(values) if values.len()==2 && values.iter().any(|v|v.to_string()=="int") && values.iter().any(|v|v.to_string()=="str")),
        "{actual:?}"
    );
}
#[test]
fn pack_capture_budget_refuses_without_truncating() {
    let arguments = (0..257).map(|_| "1").collect::<Vec<_>>().join(",");
    assert!(result(&format!("{PACK}pack({arguments})\n")).is_unknown());
}
