use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

#[test]
fn replaced_source_module_member_does_not_lend_stale_never_alias_contract() {
    let db = Analysis::for_project(Path::new("/never-member-alias"));
    db.set_file_text(
        Path::new("/never-member-alias/helper.py"),
        "from typing import Never\ndef impossible(value: Never) -> None: pass\n",
    );
    let id = db.set_file_text(Path::new("/never-member-alias/m.py"), "import helper\ndef ordinary(value: int) -> None: pass\nhelper.impossible = ordinary\nfrom helper import impossible as different\ndifferent(1)\n");
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let invocation = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|node| node.syntax().text() == "different(1)")
        .unwrap();
    let infer = Inferer::new(&db);
    let callee = infer.type_of_expr(&file, &invocation.callee().unwrap());
    // Record the stale physical producer: this is not a runtime post-store type claim.
    let signature = callee.signature().unwrap();
    assert_eq!(signature.def.as_ref().unwrap().name, "impossible");
    assert!(matches!(signature.params[0].ty, Type::Never));
    let actual = infer.type_of_expr(
        &file,
        &invocation.arg_list().unwrap().positional().next().unwrap(),
    );
    assert!(matches!(
        actual,
        Type::Literal(rustpy_semantic::types::LiteralValue::Int(1))
    ));
    assert!(
        inspections::inspect(&db, &file, &InspectionOptions::default())
            .into_iter()
            .all(|problem| problem.diagnostic.code != "PyTypeChecker")
    );
}
