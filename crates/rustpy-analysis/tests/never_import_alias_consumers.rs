use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_core::TextRange;
use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
const NATIVE: &str = "from typing import Any, Never, assert_never as canonical_assert_never\n\ndef needs_never(value: Never) -> None: pass\ndef returns_int(value: int) -> int: return value\ndef assert_never(value: int) -> None: pass\n\ndef canonical_wrong() -> None:\n    observed = 1\n    canonical_assert_never(observed)  # NATIVE: canonical-wrong\n\ndef declared_wrong() -> None:\n    observed = \"text\"\n    needs_never(observed)  # NATIVE: declared-wrong\n\ndef any_declared_control(value: Any) -> None:\n    needs_never(value)  # NATIVE: any-declared-clear\n\ndef any_canonical_control(value: Any) -> None:\n    canonical_assert_never(value)  # NATIVE: any-canonical-clear\n\ndef ordinary_returning_control() -> int:\n    observed = returns_int(1)  # NATIVE: returning-clear\n    return observed\n\ndef fake_spelling_control() -> None:\n    observed = 1\n    assert_never(observed)  # NATIVE: fake-clear\n";
fn call(file: &rustpy_semantic::db::FileInfo, text: &str) -> ast::CallExpr {
    file.parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter(|node| node.syntax().text() == text)
        .last()
        .unwrap()
}
fn findings(db: &Analysis, file: &std::sync::Arc<rustpy_semantic::db::FileInfo>) -> Vec<TextRange> {
    let mut ranges: Vec<_> = inspections::inspect(db, file, &InspectionOptions::default())
        .into_iter()
        .filter(|problem| problem.diagnostic.code == "PyTypeChecker")
        .map(|problem| problem.diagnostic.range)
        .collect();
    ranges.sort_by_key(|range| range.start());
    ranges
}
#[test]
fn exact_native_import_alias_keeps_canonical_never_argument_contract() {
    let db = Analysis::for_project(Path::new("/never-import-alias"));
    let id = db.set_file_text(Path::new("/never-import-alias/m.py"), NATIVE);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let infer = Inferer::new(&db);
    let mut expected = Vec::new();
    for (text, kind, canonical) in [
        ("canonical_assert_never(observed)", "int", true),
        ("needs_never(observed)", "str", false),
    ] {
        let invocation = call(&file, text);
        let callee = infer.type_of_expr(&file, &invocation.callee().unwrap());
        let signature = callee.signature().unwrap();
        assert!(matches!(signature.params[0].ty, Type::Never));
        if canonical {
            assert!(matches!(signature.ret, Type::Never));
            let definition = signature.def.as_ref().unwrap();
            let owner = db.file(definition.file).unwrap();
            assert!(owner.is_stub);
            assert_eq!(owner.path, Path::new("<typeshed>/typing.pyi"));
            assert_eq!(definition.name, "assert_never");
        }
        let argument = invocation.arg_list().unwrap().positional().next().unwrap();
        let actual = infer.type_of_expr(&file, &argument);
        match &actual {
            Type::Instance(class, _) => assert_eq!(class.name, kind),
            Type::LiteralStringValue => assert_eq!(kind, "str"),
            _ => panic!("actual native concrete read premise"),
        }
        assert!(!infer.is_compatible(&actual, &signature.params[0].ty));
        expected.push(argument.text_range());
    }
    expected.sort_by_key(|range| range.start());
    assert_eq!(findings(&db, &file), expected);
}
#[test]
fn stable_imported_user_function_alias_uses_its_actual_declared_signature() {
    let db = Analysis::for_project(Path::new("/never-import-alias"));
    db.set_file_text(Path::new("/never-import-alias/helper.py"),"from typing import Never\ndef impossible(value:Never)->None:pass\ndef returning(value:int)->int:return value\n");
    let id = db.set_file_text(
        Path::new("/never-import-alias/m.py"),
        "from helper import impossible as different, returning as valid\ndifferent(1)\nvalid(1)\n",
    );
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let infer = Inferer::new(&db);
    let invocation = call(&file, "different(1)");
    let callee = infer.type_of_expr(&file, &invocation.callee().unwrap());
    let signature = callee.signature().unwrap();
    assert_eq!(signature.def.as_ref().unwrap().name, "impossible");
    assert!(matches!(signature.params[0].ty, Type::Never));
    assert!(matches!(signature.ret, Type::None));
    let argument = invocation.arg_list().unwrap().positional().next().unwrap();
    assert_eq!(findings(&db, &file), vec![argument.text_range()]);
}
#[test]
fn rebound_ambiguous_and_source_spelled_aliases_do_not_borrow_canonical_never() {
    for (source,text,ambiguous) in [
  ("from typing import assert_never as alias\ndef ordinary(value:int)->None:pass\nalias=ordinary\nalias(1)\n","alias(1)",false),
  ("from typing import assert_never as original\ndef ordinary(value:int)->None:pass\ndef use(flag:bool,value:int):\n if flag: alias=original\n else: alias=ordinary\n alias(value)\n","alias(value)",true),
  ("def canonical_assert_never(value:int)->None:pass\ncanonical_assert_never(1)\n","canonical_assert_never(1)",false),
 ] {
  let db=Analysis::for_project(Path::new("/never-import-alias"));let id=db.set_file_text(Path::new("/never-import-alias/m.py"),source);let file=db.file(id).unwrap();assert!(file.parse.errors().is_empty());
  let infer=Inferer::new(&db);let invocation=call(&file,text);let callee=infer.type_of_expr(&file,&invocation.callee().unwrap());
  if ambiguous {let Type::Union(arms)=callee else {panic!("actual ambiguous callables required");};assert_eq!(arms.len(),2);}
  else {assert_eq!(callee.signature().unwrap().params[0].ty.to_string(),"int");}
  assert!(findings(&db,&file).is_empty());
 }
}

#[test]
fn nested_global_writer_retires_import_alias_authority_even_if_signature_is_stale() {
    let source = "from typing import assert_never as alias\ndef ordinary(value:int)->None:pass\ndef replace():\n global alias\n alias=ordinary\nreplace()\nalias(1)\n";
    let db = Analysis::for_project(Path::new("/never-import-alias"));
    let id = db.set_file_text(Path::new("/never-import-alias/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    assert!(file
        .scopes
        .bindings
        .iter()
        .any(|binding| binding.name == "alias"
            && binding.kind == rustpy_semantic::scope::BindingKind::Global));
    // This is an authority refusal, not a claim that arbitrary callback bodies
    // were executed by inference or that the post-call value is known.
    assert!(findings(&db, &file).is_empty());
}
