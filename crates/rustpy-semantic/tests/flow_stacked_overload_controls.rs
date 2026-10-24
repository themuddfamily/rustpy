//! Negative authority controls for canonical stacked descriptor overloads.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn reachable(source: &str) {
    let db = Analysis::for_project(Path::new("/stacked-overload-controls"));
    let id = db.set_file_text(Path::new("/stacked-overload-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(
        file.parse.errors().is_empty(),
        "{source}\n{:?}",
        file.parse.errors()
    );
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert!(
        info.refs.contains_key(&reference.range.start()),
        "unknown/returning stacked descriptor cannot erase tail: {source}"
    );
}
fn source(
    descriptor: &str,
    extra: &str,
    second_return: &str,
    implementation_descriptor: &str,
    implementation_body: &str,
) -> String {
    let first_parameter = if descriptor == "classmethod" {
        "cls,"
    } else {
        ""
    };
    format!("from typing import overload,NoReturn\n{extra}class C:\n    @overload\n    @{descriptor}\n    def stop({first_parameter}value:int)->NoReturn: ...\n    @overload\n    @{descriptor}\n    def stop({first_parameter}value:str)->{second_return}: ...\n{implementation_descriptor}    def stop({first_parameter}value:object)->NoReturn:\n        {implementation_body}\ndef f():\n    marker=1\n    C.stop(0)\n    return marker\n")
}
fn selected_stacked(source: &str, inferred_never: Option<bool>, terminal: bool) {
    let db = Analysis::for_project(Path::new("/stacked-selected-contract"));
    let id = db.set_file_text(Path::new("/stacked-selected-contract/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let inferer = Inferer::new(&db);
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| {
            call.callee()
                .is_some_and(|callee| callee.syntax().text() == "C.stop")
        })
        .unwrap();
    let Type::Overloaded(signatures) = inferer.type_of_expr(&file, &call.callee().unwrap()) else {
        panic!("actual stacked mixed overload set required: {source}")
    };
    assert_eq!(signatures.len(), 2);
    assert!(signatures
        .iter()
        .any(|signature| matches!(signature.ret, Type::Never)));
    assert!(signatures
        .iter()
        .any(|signature| !matches!(signature.ret, Type::Never)));
    if let Some(expected) = inferred_never {
        let result = inferer.type_of_expr(&file, &ast::Expr::Call(call));
        assert_eq!(
            matches!(result, Type::Never),
            expected,
            "public selected call premise: {result}; {source}"
        );
    }
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert_eq!(
        !info.refs.contains_key(&reference.range.start()),
        terminal,
        "selected stacked contract: {source}"
    );
}
#[test]
fn mixed_stacked_overload_selects_known_branch_and_retains_unknown_path() {
    for descriptor in ["staticmethod", "classmethod"] {
        // This ORIGINAL input makes a declared NoReturn promise for int,
        // although the implementation violates that promise by returning value.
        // Flow follows the same declared selected signature as public inference.
        let original = source(
            descriptor,
            "",
            "str",
            &format!("    @{descriptor}\n"),
            "return value",
        );
        selected_stacked(&original, Some(true), true);
        selected_stacked(
            &original.replace("C.stop(0)", "C.stop('returning')"),
            Some(false),
            false,
        );
        selected_stacked(
            &original
                .replace("def f():", "def f(value):")
                .replace("C.stop(0)", "C.stop(value)"),
            None,
            false,
        );
    }
}
#[test]
fn shadowed_descriptor_name_has_no_intrinsic_authority() {
    for descriptor in ["staticmethod", "classmethod"] {
        reachable(&source(
            descriptor,
            &format!("def {descriptor}(function): return lambda value:None\n"),
            "NoReturn",
            &format!("    @{descriptor}\n"),
            "raise RuntimeError",
        ));
    }
}
#[test]
fn unknown_implementation_decorator_retires_stacked_contract() {
    for descriptor in ["staticmethod", "classmethod"] {
        reachable(&source(
            descriptor,
            "def replace(function): return lambda *args:None\n",
            "NoReturn",
            &format!("    @{descriptor}\n    @replace\n"),
            "raise RuntimeError",
        ));
    }
}
#[test]
fn inconsistent_descriptor_or_generator_cannot_prove_terminal_invocation() {
    reachable(
        &source(
            "staticmethod",
            "",
            "NoReturn",
            "    @classmethod\n",
            "return None",
        )
        .replace("def stop(value:object)", "def stop(cls,value:object)"),
    );
    reachable(&source(
        "staticmethod",
        "",
        "NoReturn",
        "    @staticmethod\n",
        "yield 1",
    ));
}
#[test]
fn original_stacked_int_input_has_public_selected_never_premise() {
    use rustpy_semantic::{infer::Inferer, types::Type};
    use rustpy_syntax::ast::{self, AstNode};
    for descriptor in ["staticmethod", "classmethod"] {
        let source = source(
            descriptor,
            "",
            "str",
            &format!("    @{descriptor}\n"),
            "return value",
        );
        let db = Analysis::for_project(Path::new("/stacked-original-premise"));
        let id = db.set_file_text(Path::new("/stacked-original-premise/m.py"), &source);
        let file = db.file(id).unwrap();
        assert!(file.parse.errors().is_empty(), "{source}");
        let inferer = Inferer::new(&db);
        let call = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .find(|call| {
                call.callee()
                    .is_some_and(|callee| callee.syntax().text() == "C.stop")
            })
            .unwrap();
        let Type::Overloaded(signatures) = inferer.type_of_expr(&file, &call.callee().unwrap())
        else {
            panic!("actual original mixed overload set required: {source}")
        };
        assert_eq!(signatures.len(), 2);
        assert!(signatures
            .iter()
            .any(|signature| matches!(signature.ret, Type::Never)));
        assert!(signatures
            .iter()
            .any(|signature| !matches!(signature.ret, Type::Never)));
        let result = inferer.type_of_expr(&file, &ast::Expr::Call(call));
        assert!(
            matches!(result, Type::Never),
            "original C.stop(0) public result: {result}; {source}"
        );
    }
}
