use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn diagnostics(source: &str) -> Vec<(String, usize)> {
    let db = Analysis::for_project(Path::new("/captured-callable-inspection"));
    let id = db.set_file_text(Path::new("/captured-callable-inspection/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    inspections::inspect(&db, &file, &InspectionOptions::default())
        .into_iter()
        .filter(|p| {
            matches!(
                p.diagnostic.code.as_str(),
                "PyTypeChecker" | "PyArgumentList"
            )
        })
        .map(|p| {
            (
                p.diagnostic.code.to_string(),
                source[..usize::from(p.diagnostic.range.start())]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count()
                    + 1,
            )
        })
        .collect()
}
#[test]
fn captured_paramspec_and_concatenate_preserve_exact_native_errors() {
    let source = "from typing import Callable, ParamSpec, TypeVar, Concatenate\nP = ParamSpec('P')\nR = TypeVar('R')\ndef relay(fn: Callable[P, R]) -> Callable[P, R]: return fn\ndef remove(fn: Callable[Concatenate[int, P], R]) -> Callable[P, R]: ...\ndef target(context: int, /, value: str, *, flag: bool) -> str: return value\nforwarded = relay(target)\nreduced = remove(target)\nforwarded(1, 'π', flag=True)\nreduced('π', flag=True)\nforwarded('bad', 'π', flag=True)\nreduced(1, flag=True)\n";
    assert_eq!(
        diagnostics(source),
        [("PyTypeChecker".into(), 11), ("PyTypeChecker".into(), 12)]
    );
}
#[test]
fn explicit_callable_annotation_checks_known_parameters() {
    assert!(diagnostics(
        "from typing import Callable\ndef f(stop:Callable[[int],str]):\n stop(1)\n"
    )
    .is_empty());
    assert_eq!(
        diagnostics(
            "from typing import Callable\ndef f(stop:Callable[[int],str]):\n stop('bad')\n"
        ),
        [("PyTypeChecker".into(), 3)]
    );
}
#[test]
fn unresolved_paramspec_and_ellipsis_remain_conservative() {
    for annotation in ["Callable[P,int]", "Callable[...,int]"] {
        let source=format!("from typing import Callable,ParamSpec\nP=ParamSpec('P')\ndef f(stop:{annotation}):\n stop('unknown',other=1)\n");
        assert!(diagnostics(&source).is_empty(), "{annotation}");
    }
}
#[test]
fn callable_annotation_unknown_argument_type_does_not_create_warning() {
    assert!(diagnostics("from typing import Callable,Any\ndef f(stop:Callable[[int],str], value:Any):\n stop(value)\n").is_empty());
}
