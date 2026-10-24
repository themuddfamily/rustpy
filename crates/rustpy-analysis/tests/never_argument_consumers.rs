use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_core::TextRange;
use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn with_file(
    source: &str,
    check: impl FnOnce(&Analysis, &std::sync::Arc<rustpy_semantic::db::FileInfo>),
) {
    let db = Analysis::for_project(Path::new("/never-argument-consumer"));
    let id = db.set_file_text(Path::new("/never-argument-consumer/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    check(&db, &file);
}
fn call(file: &rustpy_semantic::db::FileInfo, text: &str) -> ast::CallExpr {
    file.parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == text)
        .unwrap()
}
fn type_diagnostics(
    db: &Analysis,
    file: &std::sync::Arc<rustpy_semantic::db::FileInfo>,
) -> Vec<TextRange> {
    let mut ranges: Vec<_> = inspections::inspect(db, file, &InspectionOptions::default())
        .into_iter()
        .filter(|problem| problem.diagnostic.code == "PyTypeChecker")
        .map(|problem| problem.diagnostic.range)
        .collect();
    ranges.sort_by_key(|range| range.start());
    ranges
}
#[test]
fn canonical_assert_never_and_user_never_parameters_reject_reachable_concrete_arguments() {
    let source = "from typing import Never, assert_never\ndef own(value:Never)->Never: raise AssertionError\ndef canonical(valueπ:int):\n assert_never(valueπ)\ndef custom(value:str):\n own(value)\n";
    with_file(source, |db, file| {
        let infer = Inferer::new(db);
        let mut expected = Vec::new();
        for (text, argument_class, canonical) in [
            ("assert_never(valueπ)", "int", true),
            ("own(value)", "str", false),
        ] {
            let call = call(file, text);
            let callee = infer.type_of_expr(file, &call.callee().unwrap());
            let signature = callee.signature().unwrap();
            assert_eq!(signature.params.len(), 1);
            assert!(matches!(signature.params[0].ty, Type::Never));
            assert!(matches!(signature.ret, Type::Never));
            let definition = signature.def.as_ref().unwrap();
            if canonical {
                let owner = db.file(definition.file).unwrap();
                assert!(owner.is_stub);
                assert_eq!(owner.path, Path::new("<typeshed>/typing.pyi"));
                assert_eq!(definition.name, "assert_never");
            } else {
                assert_eq!(definition.name, "own");
            }
            let argument = call.arg_list().unwrap().positional().next().unwrap();
            let argument_type = infer.type_of_expr(file, &argument);
            let Type::Instance(class, _) = &argument_type else {
                panic!("actual concrete argument premise required");
            };
            assert_eq!(class.name, argument_class);
            assert!(!infer.is_compatible(&argument_type, &signature.params[0].ty));
            expected.push(argument.text_range());
        }
        expected.sort_by_key(|range| range.start());
        assert_eq!(type_diagnostics(db, file), expected);
    });
}
#[test]
fn compatible_returning_and_unknown_arguments_keep_existing_noise_policy() {
    let source = "from typing import Any,Never\ndef own(value:Never)->Never:raise AssertionError\ndef returning(value:int)->int:return value\ndef unknown(value:Any):\n own(value)\nreturning(1)\n";
    with_file(source, |db, file| {
        let infer = Inferer::new(db);
        let unknown = call(file, "own(value)");
        let argument = unknown.arg_list().unwrap().positional().next().unwrap();
        assert!(matches!(infer.type_of_expr(file, &argument), Type::Any));
        let callee = infer.type_of_expr(file, &unknown.callee().unwrap());
        assert!(matches!(
            callee.signature().unwrap().params[0].ty,
            Type::Never
        ));
        let ordinary = call(file, "returning(1)");
        let callee = infer.type_of_expr(file, &ordinary.callee().unwrap());
        assert_eq!(callee.signature().unwrap().params[0].ty.to_string(), "int");
        assert!(type_diagnostics(db, file).is_empty());
    });
}
#[test]
fn source_spelled_never_and_assert_never_are_ordinary_user_contracts() {
    let source =
        "class Never:pass\ndef assert_never(value:Never)->None:pass\nassert_never(Never())\n";
    with_file(source, |db, file| {
        let infer = Inferer::new(db);
        let invocation = call(file, "assert_never(Never())");
        let callee = infer.type_of_expr(file, &invocation.callee().unwrap());
        let signature = callee.signature().unwrap();
        let Type::Instance(class, _) = &signature.params[0].ty else {
            panic!("ordinary project class required");
        };
        assert_eq!(class.name, "Never");
        assert!(!matches!(signature.params[0].ty, Type::Never));
        assert!(matches!(signature.ret, Type::None));
        assert!(type_diagnostics(db, file).is_empty());
    });
}
