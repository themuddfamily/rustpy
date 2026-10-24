//! Public inspection controls: constructor stages use the same call arguments.
use rustpy_analysis::inspections::{inspect, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;
fn codes(source: &str) -> Vec<String> {
    let db = Analysis::for_project(Path::new("/constructor-dual"));
    let id = db.set_file_text(Path::new("/constructor-dual/main.py"), source);
    let file = db.file(id).unwrap();
    inspect(&db, &file, &InspectionOptions::default())
        .into_iter()
        .filter(|p| {
            matches!(
                p.diagnostic.code.as_str(),
                "PyTypeChecker" | "PyArgumentList"
            )
        })
        .map(|p| p.diagnostic.message)
        .collect()
}
#[test]
fn same_instance_checks_new_even_when_init_accepts() {
    let source = "from typing import Self\nclass C:\n    def __new__(cls, value: int) -> Self: return super().__new__(cls)\n    def __init__(self, value: str) -> None: pass\nC('wrong-for-new')\n";
    let errors = codes(source);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("int") && errors[0].contains("str"),
        "{errors:?}"
    );
}
#[test]
fn same_instance_checks_init_even_when_new_accepts() {
    let source = "from typing import Self\nclass C:\n    def __new__(cls, value: int) -> Self: return super().__new__(cls)\n    def __init__(self, value: str) -> None: pass\nC(1)\n";
    let errors = codes(source);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("str") && errors[0].contains("int"),
        "{errors:?}"
    );
}
#[test]
fn matching_new_init_and_foreign_new_are_clean() {
    assert!(codes("from typing import Self\nclass C:\n    def __new__(cls, *, value: int) -> Self: return super().__new__(cls)\n    def __init__(self, *, value: int) -> None: pass\nC(value=1)\n").is_empty());
    assert!(codes("class R: pass\nclass C:\n    def __new__(cls, value: int) -> R: return R()\n    def __init__(self, value: str, extra: str) -> None: pass\nC(1)\n").is_empty());
}

#[test]
fn one_argument_rejected_by_both_stages_is_one_finding() {
    let errors = codes(
        "from typing import Self\nclass C:\n    def __new__(cls, value: int) -> Self: return super().__new__(cls)\n    def __init__(self, value: int) -> None: pass\nC('wrong-for-both')\n",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("int") && errors[0].contains("str"),
        "{errors:?}"
    );
}
