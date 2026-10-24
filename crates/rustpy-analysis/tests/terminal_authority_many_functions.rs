//! Explicit comparative probe: run this unchanged on both source revisions.
//! Uses the real inspect -> unbound_locals -> Inferer flow consumer. No timing
//! threshold: retain elapsed/result counts and compare equivalent hardware.
use rustpy_analysis::inspections::{inspect, InspectionOptions};
use rustpy_semantic::db::Analysis;

#[test]
#[ignore = "explicit many-function authority cost comparison"]
fn many_function_terminal_authority_actual_inspection_probe() {
    let directory = tempfile::TempDir::with_prefix("rustpy-terminal-many-function-probe").unwrap();
    const FUNCTIONS: usize = 1000;
    const MODULES: usize = 24;
    let padding = format!("# {}\n", "padding".repeat(16_000));
    let mut dependency_bytes = 0;
    for index in 0..MODULES {
        let mut source = padding.clone();
        if index + 1 < MODULES {
            source.push_str(&format!("import dep{}\n", index + 1));
        }
        source.push_str("from typing import Never\ndef stop() -> Never:\n    raise RuntimeError\n");
        dependency_bytes += source.len();
        std::fs::write(directory.path().join(format!("dep{index}.py")), source).unwrap();
    }
    let mut source = String::from("from dep0 import stop\n");
    for index in 0..FUNCTIONS {
        source.push_str(&format!("def function_{index}(flag):\n    if flag:\n        value=1\n    else:\n        stop()\n        pass\n    return value\n"));
    }
    let path = directory.path().join("main.py");
    std::fs::write(&path, &source).unwrap();
    let db = Analysis::for_project(directory.path());
    let file = db.file_for_path(&path).unwrap();
    // Warm only file/module loading; each inspect constructs a fresh Inferer.
    for index in 0..MODULES {
        assert!(db.module(&format!("dep{index}")).is_some());
    }
    assert!(db.module("typing").is_some());
    let options = InspectionOptions {
        pep8: false,
        unused_imports: false,
        unused_variables: false,
        type_checker: false,
        security: false,
        compatibility: false,
        code_smells: false,
        naming: false,
        missing_docstrings: false,
        incorrect_docstrings: false,
        deprecation: false,
        probable_bugs: true,
        ..InspectionOptions::default()
    };
    assert_eq!(
        file.scopes
            .scopes
            .iter()
            .filter(|scope| scope.kind == rustpy_semantic::scope::ScopeKind::Function)
            .count(),
        FUNCTIONS
    );
    for sample in 0..3 {
        let started = std::time::Instant::now();
        let problems = inspect(&db, &file, &options);
        let elapsed = started.elapsed();
        let unbound = problems
            .iter()
            .filter(|problem| problem.diagnostic.code == "PyUnboundLocalVariable")
            .count();
        eprintln!("terminal-authority-probe sample={sample} functions={FUNCTIONS} dependencies={MODULES} source_bytes={} dependency_bytes={dependency_bytes} elapsed_ns={} problems={} unbound={unbound}", source.len(), elapsed.as_nanos(), problems.len());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    }
}
