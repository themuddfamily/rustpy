use rustpy_core::TextSize;
use rustpy_semantic::{completion2::smart_complete, db::Analysis, scope::BindingKind};
use std::path::Path;
#[test]
fn called_nonlocal_writer_retires_local_and_parameter_await_suggestions() {
    for setup in [
        "async def run():\n    pending=fetch()\n",
        "async def run(pending:Coroutine[object,object,str]):\n",
    ] {
        let source = format!(
            "from typing import Coroutine\nasync def fetch()->str:return 'ok'\ndef use(value:str):pass\n{setup}    def reset():\n        nonlocal pending\n        pending=1\n    reset()\n    use()\n"
        );
        let db = Analysis::for_project(Path::new("/smart-captured-cell"));
        let id = db.set_file_text(Path::new("/smart-captured-cell/m.py"), &source);
        let file = db.file(id).unwrap();
        assert!(file.parse.errors().is_empty());
        let nested = file
            .scopes
            .scopes
            .iter()
            .find(|s| s.name.as_deref() == Some("reset"))
            .unwrap();
        assert!(nested.nonlocals.iter().any(|n| n == "pending"));
        let writes = nested.bindings.get("pending").unwrap();
        assert!(
            writes.iter().any(|id| matches!(
                file.scopes.binding(*id).kind,
                BindingKind::Assignment
            ) && file.scopes.binding(*id).value.is_some_and(
                |range| &source[usize::from(range.start())..usize::from(range.end())] == "1"
            )),
            "actual nested cell store BEFORE completion"
        );
        assert!(source.contains("    reset()\n    use()"));
        let offset = TextSize::new((source.rfind("use()").unwrap() + 4) as u32);
        assert!(!smart_complete(&db, &file, offset)
            .iter()
            .any(|i| i.insert == "await pending"));
    }
}
