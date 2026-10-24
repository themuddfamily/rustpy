use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn warnings(source: &str) -> Vec<String> {
    let db = Analysis::for_project(Path::new("/capability-inspections"));
    let id = db.set_file_text(Path::new("/capability-inspections/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    inspections::inspect(&db, &file, &InspectionOptions::default())
        .into_iter()
        .filter(|problem| problem.diagnostic.code == "PyTypeChecker")
        .map(|problem| format!("{problem:?}"))
        .collect()
}

#[test]
fn readonly_initialization_is_valid_but_later_mutations_are_checked() {
    let prefix = "from typing import TypedDict, NotRequired\nfrom typing_extensions import ReadOnly\nclass Settings(TypedDict):\n    fixed:ReadOnly[int]\n    mutable:int\n    optional:NotRequired[int]\n";
    assert!(warnings(&format!(
        "{prefix}value:Settings={{'fixed':1,'mutable':2}}\n"
    ))
    .is_empty());
    for statement in [
        "value['fixed']=2",
        "del value['fixed']",
        "del value['mutable']",
        "value.update(fixed=2)",
    ] {
        let problems = warnings(&format!(
            "{prefix}def change(value:Settings):\n    {statement}\n"
        ));
        assert_eq!(problems.len(), 1, "{statement}: {problems:#?}");
    }
    assert!(warnings(&format!(
        "{prefix}def change(value:Settings):\n    value['mutable']=2\n    del value['optional']\n"
    ))
    .is_empty());
}

#[test]
fn literal_string_argument_checks_distinguish_safe_and_general_values() {
    let prefix = "from typing import LiteralString\ndef require(value:LiteralString): pass\n";
    assert!(warnings(&format!(
        "{prefix}safe='literal'\nrequire(safe.upper()+'suffix')\n"
    ))
    .is_empty());
    let problems = warnings(&format!(
        "{prefix}def unsafe(value:str):\n    require(value)\n"
    ));
    assert_eq!(problems.len(), 1, "{problems:#?}");
}

#[test]
fn member_capabilities_reach_actual_assignment_inspections() {
    for (source, fragment) in [
        ("from dataclasses import dataclass\n@dataclass(frozen=True)\nclass C:\n value:int\nc=C(1)\nc.value=2\n", "frozen"),
        ("class C:\n __slots__=('value',)\n value:int\nc=C()\nc.other=2\n", "__slots__"),
        ("class C:\n @property\n def value(self)->int:return 1\nc=C()\nc.value=2\n", "setter"),
        ("from typing import Final\nvalue:Final[int]=1\nvalue=2\n", "final name"),
    ] {
        let problems=warnings(source);
        assert!(problems.iter().any(|problem|problem.contains(fragment)),"{source}\n{problems:#?}");
    }
    assert!(warnings(
        "from dataclasses import dataclass\n@dataclass\nclass C:\n value:int\nc=C(1)\nc.value=2\n"
    )
    .is_empty());
}

#[test]
fn protocol_runtime_marker_reaches_actual_builtin_call_inspection() {
    let prefix="from typing import Protocol,runtime_checkable\nclass P(Protocol):\n def value(self)->int:...\n@runtime_checkable\nclass R(Protocol):\n def value(self)->int:...\n";
    let problems = warnings(&format!(
        "{prefix}def check(value:object):\n return isinstance(value,P)\n"
    ));
    assert!(
        problems
            .iter()
            .any(|problem| problem.contains("runtime_checkable")),
        "{problems:#?}"
    );
    assert!(warnings(&format!(
        "{prefix}def check(value:object):\n return isinstance(value,R)\n"
    ))
    .is_empty());
}
