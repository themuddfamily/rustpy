use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn expression(source: &str, text: &str) -> Type {
    let db = Analysis::for_project(Path::new("/paramspec-return-controls"));
    let id = db.set_file_text(Path::new("/paramspec-return-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let expressions: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|e| e.syntax().text() == text)
        .collect();
    assert_eq!(expressions.len(), 1, "exact unique expression {text}");
    Inferer::new(&db).type_of_expr(&file, &expressions[0])
}
#[test]
fn input_only_paramspec_preserves_concrete_never_and_normal_return() {
    for parameter in ["P", "Concatenate[int,P]"] {
        let source=format!("from typing import Callable,Concatenate,ParamSpec,Never\nP=ParamSpec('P')\ndef f(stop:Callable[{parameter},Never]):\n    stop(0)\ndef g(normal:Callable[{parameter},int]):\n    normal(0)\n");
        assert!(matches!(expression(&source, "stop(0)"), Type::Never));
        assert_eq!(expression(&source, "normal(0)").to_string(), "int");
    }
}
#[test]
fn validated_existing_factory_options_preserve_canonical_contracts() {
    for options in [
        "bound=str",
        "bound=object",
        "bound=None,covariant=True",
        "covariant=False,contravariant=False",
    ] {
        let source=format!("from typing import Callable,ParamSpec,Never\nP=ParamSpec('P',{options})\ndef f(stop:Callable[P,Never]):\n    stop()\n");
        let ty = expression(&source, "stop()");
        assert!(matches!(ty, Type::Never), "{options}: {ty}");
    }
}
#[test]
fn invalid_unknown_and_duplicate_options_cannot_grant_contracts() {
    for options in [
        "unknown=True",
        "bound=missing",
        "covariant=1",
        "covariant=True,contravariant=True",
        "bound=str,bound=object",
        "default=missing",
    ] {
        let source=format!("from typing import Callable,ParamSpec,Never\nP=ParamSpec('P',{options})\ndef f(stop:Callable[P,Never]):\n    stop()\n");
        assert!(expression(&source, "stop()").is_unknown(), "{options}");
    }
}
#[test]
fn unresolved_returned_callable_shape_still_refuses_fake_expansion() {
    let source="from typing import Callable,ParamSpec\nP=ParamSpec('P')\ndef unresolved()->Callable[P,int]:...\nunresolved()\n";
    assert!(expression(source, "unresolved()").is_unknown());
}
