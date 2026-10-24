use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn check(source: &str, expected: &str) {
    let db = Analysis::for_project(Path::new("/class-decorator-replacement"));
    let id = db.set_file_text(Path::new("/class-decorator-replacement/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let calls: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter(|call| call.syntax().text() == "Box()")
        .collect();
    assert_eq!(calls.len(), 1);
    let actual = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(calls[0].clone()));
    assert_eq!(actual.to_string(), expected, "{source}");
}

#[test]
fn compound_annotated_augmented_and_deleted_attrs_members_retire_model_authority() {
    let prefix="import attr\nfrom typing import Callable\ndef replacement(cls)->Callable[[],str]:raise RuntimeError\n";
    for store in [
        "(attr.s, other)=(replacement,0)",
        "[attr.s,*other]=[replacement,0]",
        "attr.s:object=replacement",
        "attr.s+=replacement",
        "del attr.s",
    ] {
        check(
            &format!(
                "{prefix}{store}\n@attr.s\nclass Box:\n    value=attr.ib(default=1)\nvalue=Box()\n"
            ),
            "Unknown",
        );
    }
}

#[test]
fn canonical_typing_namespace_member_retirement_declines_identity() {
    let prefix="import typing\nfrom typing import Callable\ndef replacement(cls)->Callable[[],str]:raise RuntimeError\n";
    for store in [
        "typing.final=replacement",
        "(typing.final, other)=(replacement,0)",
        "typing.final:object=replacement",
        "typing.final+=replacement",
        "del typing.final",
    ] {
        check(
            &format!("{prefix}{store}\n@typing.final\nclass Box:pass\nvalue=Box()\n"),
            "Unknown",
        );
    }
}

#[test]
fn relative_attrs_import_and_scan_exhaustion_do_not_claim_external_model_identity() {
    check(
        "from .attrs import define\n@define\nclass Box:\n    value:int=1\nvalue=Box()\n",
        "Unknown",
    );
    let source = format!(
        "import attr\n{}@attr.s\nclass Box:\n    value=attr.ib(default=1)\nvalue=Box()\n",
        "filler=0\n".repeat(17_000)
    );
    check(&source, "Unknown");
}

#[test]
fn canonical_identity_function_call_is_not_an_identity_decorator_factory() {
    check("from typing import final,Callable\ndef replacement(cls)->Callable[[],str]:raise RuntimeError\n@final(replacement)\nclass Box:pass\nvalue=Box()\n", "str");
    check(
        "from typing import final\n@final()\nclass Box:pass\nvalue=Box()\n",
        "Unknown",
    );
    check("from typing import dataclass_transform\n@dataclass_transform\nclass Box:pass\nvalue=Box()\n", "Unknown");
}
