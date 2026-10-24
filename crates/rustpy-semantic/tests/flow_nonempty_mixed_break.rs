use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
#[test]
fn first_iteration_break_definition_survives_later_terminal_condition() {
    let source = "from typing import Callable, Never\ndef stop()->Never: raise RuntimeError\ndef f(flag:Callable[[],bool]):\n result=0\n for target in (1,2):\n  if flag():\n   result=1\n   break\n  flag=stop\n return result\n";
    let db = Analysis::for_project(Path::new("/mixed-break"));
    let id = db.set_file_text(Path::new("/mixed-break/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let reads: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|read| read.name == "result" && !read.is_write)
        .collect();
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, read.scope).unwrap());
    let reaches = info
        .refs
        .get(&read.range.start())
        .expect("first break reaches tail");
    assert!(!reaches.is_empty());
    let first_break_store = source.find("result=1").unwrap();
    assert!(
        reaches
            .iter()
            .any(|reach| reach.binding.is_some_and(|id| usize::from(
                file.scopes.binding(id).range.start()
            ) == first_break_store)),
        "actual first break store retained: {reaches:?}"
    );
}
