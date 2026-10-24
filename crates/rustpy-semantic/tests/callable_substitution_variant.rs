use rustpy_semantic::types::{Param, ParamKind, Signature, Type};
use std::{path::Path, sync::Arc};
fn signature() -> Arc<Signature> {
    Arc::new(Signature {
        params: vec![Param {
            name: "value".into(),
            kind: ParamKind::KeywordOnly,
            ty: Type::TypeVar("T".into()),
            has_default: true,
            default_text: Some("None".into()),
        }],
        ret: Type::TypeVar("T".into()),
        is_async: false,
        def: None,
        docstring: Some("contract".into()),
        ret_inferred: false,
    })
}
#[test]
fn callable_substitution_preserves_declared_variant_and_signature_metadata() {
    let original = signature();
    let result =
        Type::Callable(original.clone()).substitute(&|name| (name == "T").then_some(Type::None));
    let Type::Callable(actual) = result else {
        panic!("Callable substitution changed variant: {result}");
    };
    assert_eq!(actual.params.len(), 1);
    assert_eq!(actual.params[0].ty, Type::None);
    assert_eq!(actual.ret, Type::None);
    assert_eq!(actual.params[0].name, original.params[0].name);
    assert_eq!(actual.params[0].kind, original.params[0].kind);
    assert_eq!(actual.params[0].has_default, original.params[0].has_default);
    assert_eq!(
        actual.params[0].default_text,
        original.params[0].default_text
    );
    assert_eq!(actual.def, original.def);
    assert_eq!(actual.docstring, original.docstring);
    assert_eq!(actual.is_async, original.is_async);
    assert_eq!(actual.ret_inferred, original.ret_inferred);
    assert!(matches!(
        Type::Callable(original).substitute(&|_| None),
        Type::Callable(_)
    ));
}
#[test]
fn function_substitution_keeps_function_variant_and_recursive_replacement() {
    let result =
        Type::Function(signature()).substitute(&|name| (name == "T").then_some(Type::None));
    let Type::Function(actual) = result else {
        panic!("Function changed variant: {result}");
    };
    assert_eq!(actual.params[0].ty, Type::None);
    assert_eq!(actual.ret, Type::None);
}

#[test]
fn captured_native_callbacks_preserve_expanded_declared_callable_signatures() {
    use rustpy_semantic::{
        infer::Inferer,
        types::{ParamKind, Type},
    };
    use rustpy_syntax::ast::{self, AstNode};
    let source = "from typing import Callable, ParamSpec, TypeVar, Concatenate\nP = ParamSpec('P')\nR = TypeVar('R')\ndef relay(fn: Callable[P, R]) -> Callable[P, R]: return fn\ndef remove(fn: Callable[Concatenate[int, P], R]) -> Callable[P, R]: ...\ndef target(context: int, /, value: str, *, flag: bool) -> str: return value\nforwarded = relay(target)\nreduced = remove(target)\nforwarded(1, 'π', flag=True)\nreduced('π', flag=True)\nforwarded('bad', 'π', flag=True)\nreduced(1, flag=True)\n";
    let db = rustpy_semantic::db::Analysis::for_project(Path::new("/captured-callable-premise"));
    let id = db.set_file_text(Path::new("/captured-callable-premise/m.py"), source);
    let file = db.file(id).unwrap();
    let inf = Inferer::new(&db);
    for (call_name, expected) in [
        (
            "forwarded",
            vec![
                ("context", ParamKind::PositionalOnly, "int"),
                ("value", ParamKind::Normal, "str"),
                ("flag", ParamKind::KeywordOnly, "bool"),
            ],
        ),
        (
            "reduced",
            vec![
                ("value", ParamKind::Normal, "str"),
                ("flag", ParamKind::KeywordOnly, "bool"),
            ],
        ),
    ] {
        let call = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .find(|call| {
                call.callee()
                    .is_some_and(|callee| callee.syntax().text() == call_name)
            })
            .unwrap();
        let ty = inf.type_of_expr(&file, &call.callee().unwrap());
        let Type::Callable(signature) = ty else {
            panic!("actual captured Callable expected: {ty}");
        };
        assert!(signature.def.is_none());
        assert_eq!(
            signature
                .params
                .iter()
                .map(|p| (p.name.as_str(), p.kind, p.ty.to_string()))
                .collect::<Vec<_>>(),
            expected
                .into_iter()
                .map(|(n, k, t)| (n, k, t.to_owned()))
                .collect::<Vec<_>>()
        );
    }
}
#[test]
fn bound_method_substitution_keeps_receiver_and_signature() {
    let result = Type::BoundMethod(signature(), Box::new(Type::None))
        .substitute(&|name| (name == "T").then_some(Type::None));
    let Type::BoundMethod(actual, receiver) = result else {
        panic!("BoundMethod changed variant: {result}");
    };
    assert_eq!(*receiver, Type::None);
    assert_eq!(actual.params[0].ty, Type::None);
    assert_eq!(actual.ret, Type::None);
}
