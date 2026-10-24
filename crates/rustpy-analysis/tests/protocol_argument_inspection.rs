use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;
#[test]
fn protocol_method_incompatibility_reaches_actual_argument_inspection() {
    let source = "from typing import Protocol\nclass P(Protocol):\n    def run(self,value:int)->str: ...\nclass C:\n    def run(self,value:str)->int: return 1\ndef consume(value:P): pass\nconsume(C())\n";
    let db = Analysis::for_project(Path::new("/protocol-inspection"));
    let id = db.set_file_text(Path::new("/protocol-inspection/m.py"), source);
    let file = db.file(id).unwrap();
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    assert_eq!(
        problems
            .iter()
            .filter(|problem| problem.diagnostic.code == "PyTypeChecker")
            .count(),
        1,
        "{problems:#?}"
    );
}
