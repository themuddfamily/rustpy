use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn diagnostics(call: &str) -> Vec<String> {
    let source = format!("from typing import TypedDict,Unpack,NotRequired\nclass Movie(TypedDict):\n name:str\n year:int\n note:NotRequired[str]\ndef consume(**kwargs:Unpack[Movie])->None:...\n{call}\n");
    let db = Analysis::for_project(Path::new("/unpack-keyword-inspection"));
    let id = db.set_file_text(Path::new("/unpack-keyword-inspection/m.py"), &source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    inspections::inspect(&db, &file, &InspectionOptions::default())
        .into_iter()
        .filter(|p| {
            matches!(
                p.diagnostic.code.as_str(),
                "PyArgumentList" | "PyTypeChecker"
            )
        })
        .map(|p| p.diagnostic.code.to_string())
        .collect()
}
#[test]
fn unpack_typed_dict_accepts_required_and_optional_fields() {
    assert!(diagnostics("consume(name='x',year=2026)").is_empty());
    assert!(diagnostics("consume(name='x',year=2026,note='ok')").is_empty());
}
#[test]
fn unpack_typed_dict_reports_missing_required_field() {
    let codes = diagnostics("consume(name='x')");
    assert_eq!(codes, ["PyArgumentList"]);
}
#[test]
fn unpack_typed_dict_reports_wrong_field_type() {
    let codes = diagnostics("consume(name='x',year='bad')");
    assert_eq!(codes, ["PyTypeChecker"]);
}
#[test]
fn unpack_typed_dict_reports_unexpected_keyword() {
    let codes = diagnostics("consume(name='x',year=2026,extra=1)");
    assert_eq!(codes, ["PyArgumentList"]);
}
