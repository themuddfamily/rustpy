use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;
fn tail(source: &str, expected: usize) {
    let db = Analysis::for_project(Path::new("/with-analysis"));
    let id = db.set_file_text(Path::new("/with-analysis/m.py"), source);
    let file = db.file(id).unwrap();
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    let count = problems
        .iter()
        .filter(|p| {
            p.diagnostic.code == "PyUnboundLocalVariable"
                && &source
                    [usize::from(p.diagnostic.range.start())..usize::from(p.diagnostic.range.end())]
                    == "marker"
        })
        .count();
    assert_eq!(count, expected, "{source} {problems:#?}");
}
#[test]
fn exception_does_not_suppress_baseexception() {
    tail(
        "from contextlib import suppress\ndef f():\n    with suppress(Exception):\n        raise BaseException\n    print(marker)\n    marker=1\n",
        0,
    );
}
#[test]
fn exception_does_not_suppress_systemexit() {
    tail(
        "from contextlib import suppress\ndef f():\n    with suppress(Exception):\n        raise SystemExit\n    print(marker)\n    marker=1\n",
        0,
    );
}
#[test]
fn exception_does_not_suppress_keyboardinterrupt() {
    tail(
        "from contextlib import suppress\ndef f():\n    with suppress(Exception):\n        raise KeyboardInterrupt\n    print(marker)\n    marker=1\n",
        0,
    );
}
#[test]
fn exception_does_not_suppress_generatorexit() {
    tail(
        "from contextlib import suppress\ndef f():\n    with suppress(Exception):\n        raise GeneratorExit\n    print(marker)\n    marker=1\n",
        0,
    );
}
