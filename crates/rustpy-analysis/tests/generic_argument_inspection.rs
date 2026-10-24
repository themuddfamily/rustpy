use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;
fn check(source: &str, expected: usize) {
    let db = Analysis::for_project(Path::new("/generic-inspection"));
    let id = db.set_file_text(Path::new("/generic-inspection/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    assert_eq!(
        problems
            .iter()
            .filter(|p| p.diagnostic.code == "PyTypeChecker")
            .count(),
        expected,
        "{source}; {problems:#?}"
    );
}
#[test]
fn known_bound_rejects_wrong_argument_and_accepts_int() {
    let prefix =
        "from typing import TypeVar\nT=TypeVar('T',bound=int)\ndef consume(value:T):pass\n";
    check(&format!("{prefix}consume('bad')\n"), 1);
    check(&format!("{prefix}consume(1)\n"), 0);
}
#[test]
fn ordered_constraints_reject_outside_and_accept_both() {
    let prefix = "from typing import TypeVar\nT=TypeVar('T',int,str)\ndef consume(value:T):pass\n";
    check(&format!("{prefix}consume(3.14)\n"), 1);
    check(&format!("{prefix}consume(1)\nconsume('ok')\n"), 0);
}
#[test]
fn canonical_factory_alias_retains_bound() {
    check("from typing import TypeVar as TV\nT=TV('T',bound=int)\ndef consume(value:T):pass\nconsume('bad')\n",1);
}
#[test]
fn pep695_known_bound_reaches_inspection() {
    check("def consume[T:int](value:T):pass\nconsume('bad')\n", 1);
    check("def consume[T:int](value:T):pass\nconsume(1)\n", 0);
}
#[test]
fn unknown_and_unbounded_stay_conservative() {
    check("from typing import TypeVar,Any\nT=TypeVar('T')\nA=TypeVar('A',bound=Any)\ndef consume(value:T):pass\ndef any_bound(value:A):pass\nconsume('ok')\nany_bound([])\n",0);
    check("from typing import TypeVar\nT=TypeVar('T',bound=Missing)\ndef consume(value:T):pass\nconsume('ok')\n",0);
}
#[test]
fn fake_factory_does_not_create_bound_authority() {
    check("def TypeVar(*args,**kwargs):return object\nT=TypeVar('T',bound=int)\ndef consume(value:T):pass\nconsume('ok')\n",0);
}

#[test]
fn ordinary_seventeen_member_union_retains_existing_warning() {
    let declarations = (0..17)
        .map(|index| format!("class C{index}:pass\n"))
        .collect::<String>();
    let union = (0..17)
        .map(|index| format!("C{index}"))
        .collect::<Vec<_>>()
        .join("|");
    check(
        &format!("{declarations}def consume(value:{union}):pass\nconsume(1)\n"),
        1,
    );
}
