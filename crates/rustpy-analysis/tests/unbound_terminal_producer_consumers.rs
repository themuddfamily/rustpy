//! Actual inspection consumers for six declared terminal producer families.
use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn check(source: &str, terminal: bool) {
    let db = Analysis::for_project(Path::new("/terminal-analysis-consumers"));
    let id = db.set_file_text(Path::new("/terminal-analysis-consumers/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(
        file.parse.errors().is_empty(),
        "{source}\n{:?}",
        file.parse.errors()
    );
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    let start = source.rfind("print(x)").unwrap() + "print(".len();
    let actual: Vec<_> = problems
        .iter()
        .filter(|problem| problem.diagnostic.code == "PyUnboundLocalVariable")
        .map(|problem| {
            (
                problem.diagnostic.code.to_string(),
                usize::from(problem.diagnostic.range.start()),
                usize::from(problem.diagnostic.range.end()),
            )
        })
        .collect();
    let expected = if terminal {
        Vec::new()
    } else {
        vec![("PyUnboundLocalVariable".to_owned(), start, start + 1)]
    };
    assert_eq!(
        actual, expected,
        "actual consumer IDs/ranges, not inferred type alone: {source}\n{problems:#?}"
    );
}
fn annotated_case(header: &str, annotation: &str, invoke: &str, terminal: bool) {
    check(
        &format!("{header}def f(stop:{annotation}):\n    {invoke}\n    print(x)\n    x=1\n"),
        terminal,
    );
}
#[test]
fn annotated_callable_consumer() {
    let header = "from typing import Annotated,Callable,NoReturn\n";
    annotated_case(
        header,
        "Annotated[Callable[[],NoReturn],'note']",
        "stop()",
        true,
    );
    annotated_case(
        header,
        "Annotated[Callable[[],int],'note']",
        "stop()",
        false,
    );
}
#[test]
fn concatenate_callable_consumer() {
    let header = "from typing import Callable,Concatenate,ParamSpec,NoReturn\nP=ParamSpec('P')\n";
    annotated_case(
        header,
        "Callable[Concatenate[int,P],NoReturn]",
        "stop(1)",
        true,
    );
    annotated_case(header, "Callable[Concatenate[int,P],int]", "stop(1)", false);
}
#[test]
fn object_bounded_paramspec_callable_consumer() {
    let header = "from typing import Callable,ParamSpec,NoReturn\nP=ParamSpec('P',bound=object)\n";
    annotated_case(header, "Callable[P,NoReturn]", "stop()", true);
    annotated_case(header, "Callable[P,int]", "stop()", false);
}
#[test]
fn fixed_tuple_index_callable_consumer() {
    let header="from typing import Callable,NoReturn\ndef f(stops:tuple[Callable[[],NoReturn],Callable[[],int]]):\n";
    check(
        &format!("{header}    stops[0]()\n    print(x)\n    x=1\n"),
        true,
    );
    check(
        &format!("{header}    stops[1]()\n    print(x)\n    x=1\n"),
        false,
    );
}
#[test]
fn all_never_overload_consumer() {
    let prefix="from typing import overload,Never,NoReturn\n@overload\ndef stop(value:int)->Never: ...\n@overload\ndef stop(value:str)->";
    check(&format!("{prefix}NoReturn: ...\ndef stop(value:object)->NoReturn: raise RuntimeError\ndef f(value:object):\n    stop(value)\n    print(x)\n    x=1\n"),true);
    check(&format!("{prefix}str: ...\ndef stop(value:object): return value\ndef f(value:object):\n    stop(value)\n    print(x)\n    x=1\n"),false);
}
#[test]
fn inherited_metaclass_never_consumer() {
    let prefix="from __future__ import annotations\nfrom typing import Never\nclass Meta(type):\n    def __call__(cls)->";
    check(&format!("{prefix}Never: raise RuntimeError\nclass Base(metaclass=Meta): pass\nclass C(Base): pass\ndef f():\n    C()\n    print(x)\n    x=1\n"),true);
    check(&format!("{prefix}int: return 1\nclass Base(metaclass=Meta): pass\nclass C(Base): pass\ndef f():\n    C()\n    print(x)\n    x=1\n"),false);
}
