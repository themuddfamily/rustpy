//! Impossible constant branches must neither record reads nor publish exit edges.
use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;

fn absent_reads(source: &str, expected_load_count: usize) {
    let db = Analysis::for_project(Path::new("/cfg-constant-dead-paths"));
    let id = db.set_file_text(Path::new("/cfg-constant-dead-paths/m.py"), source);
    let file = db.file(id).unwrap();
    let loads: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|r| r.name == "marker" && !r.is_write)
        .collect();
    assert_eq!(loads.len(), expected_load_count, "nonvacuous source loads");
    for reference in loads {
        let info = flow::flow_of(&file, flow::flow_scope(&file, reference.scope).unwrap());
        assert!(
            !info.refs.contains_key(&reference.range.start()),
            "impossible branch/exit read: {source}"
        );
    }
}

#[test]
fn false_if_cannot_record_read_or_break_infinite_loop() {
    // Intentionally infinite: semantic-only fixture, never execute f().
    absent_reads("def f():\n    marker=1\n    while True:\n        if False:\n            print(marker)\n            marker=2\n            break\n    return marker\n", 2);
}

#[test]
fn false_elif_cannot_record_read_or_break_infinite_loop() {
    // Both branches are false; neither branch can break the loop.
    absent_reads("def f():\n    marker=1\n    while True:\n        if False:\n            pass\n        elif False:\n            print(marker)\n            break\n    return marker\n", 2);
}

#[test]
fn false_while_does_not_visit_body_read() {
    absent_reads(
        "def f():\n    marker=1\n    while False:\n        print(marker)\n",
        1,
    );
}
