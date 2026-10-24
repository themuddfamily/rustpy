use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
fn query(source: &str, check: impl FnOnce(&rustpy_semantic::db::FileInfo, &flow::FlowInfo)) {
    let db = Analysis::for_project(Path::new("/empty-cfg"));
    let id = db.set_file_text(Path::new("/empty-cfg/m.py"), source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    check(&file, &info);
}
#[test]
fn empty_literals_skip_for_body_and_target_but_execute_else() {
    for literal in ["[]", "()", "{}", "''", "b''"] {
        let source = format!(
            "def f():\n    marker=1\n    for target in {literal}:\n        print(marker)\n    else:\n        print(marker)\n    return target\n"
        );
        query(&source, |file, info| {
            let mut reads: Vec<_> = file
                .scopes
                .references
                .iter()
                .filter(|r| r.name == "marker" && !r.is_write)
                .collect();
            reads.sort_by_key(|r| r.range.start());
            assert_eq!(reads.len(), 2);
            assert!(
                !info.refs.contains_key(&reads[0].range.start()),
                "empty iterable has no body execution: {literal}"
            );
            assert!(
                !info.refs[&reads[1].range.start()].is_empty(),
                "else actually executes"
            );
            let target = file
                .scopes
                .references
                .iter()
                .find(|r| r.name == "target" && !r.is_write)
                .unwrap();
            assert!(
                info.refs[&target.range.start()].is_empty()
                    || info.refs[&target.range.start()]
                        .iter()
                        .all(|r| r.binding.is_none()),
                "zero iterations cannot bind target"
            );
        });
    }
}
#[test]
fn empty_container_conditions_skip_dead_read_and_break() {
    for literal in ["[]", "()", "{}"] {
        let source = format!(
            "def f():\n    marker=1\n    while True:\n        if {literal}:\n            print(marker)\n            break\n    return marker\n"
        );
        query(&source, |file, info| {
            let reads: Vec<_> = file
                .scopes
                .references
                .iter()
                .filter(|r| r.name == "marker" && !r.is_write)
                .collect();
            assert_eq!(reads.len(), 2);
            assert!(
                reads
                    .iter()
                    .all(|r| !info.refs.contains_key(&r.range.start())),
                "dead break cannot publish loop exit: {literal}"
            );
        });
    }
}
#[test]
fn nonempty_and_expanding_iterables_retain_possible_body_execution() {
    for literal in ["[1]", "(1,)", "{'a':1}", "[*items]"] {
        let source = format!(
            "def f(items):\n    marker=1\n    for target in {literal}:\n        print(marker)\n    return marker\n"
        );
        query(&source, |file, info| {
            let r = file
                .scopes
                .references
                .iter()
                .find(|r| r.name == "marker" && !r.is_write)
                .unwrap();
            assert!(info.refs.contains_key(&r.range.start()), "{literal}");
        });
    }
}
