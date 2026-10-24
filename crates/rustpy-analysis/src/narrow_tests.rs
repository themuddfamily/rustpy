use rustpy_semantic::Analysis;
use std::path::Path;

#[test]
fn type_checks_use_narrowed_and_reaching_types() {
    let run = |src: &str| -> Vec<String> {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        crate::inspections::inspect(&db, &file, &Default::default())
            .into_iter()
            .filter(|p| p.diagnostic.code == "PyTypeChecker")
            .map(|p| p.diagnostic.message)
            .collect()
    };
    // No "got 'int | None'" inside a None check.
    let narrowed = run(
        "def g(n: int) -> int:\n    return n\n\n\ndef f(x: int | None):\n    if x is not None:\n        g(x)\n",
    );
    assert!(narrowed.is_empty(), "{narrowed:?}");
    // A rebound name is checked with the binding that reaches the call.
    let m = run("def g(n: int) -> int:\n    return n\n\n\nv = 1\ng(v)\nv = 'a'\ng(v)\n");
    assert_eq!(
        m,
        vec!["Expected type 'int', got 'str' instead".to_string()]
    );
}
