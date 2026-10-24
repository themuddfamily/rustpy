use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
fn check(source: &str, name: &str, visited: bool, bound: bool) {
    let db = Analysis::for_project(Path::new("/nonempty-for"));
    let id = db.set_file_text(Path::new("/nonempty-for/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let reads: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|r| r.name == name && !r.is_write)
        .collect();
    assert_eq!(reads.len(), 1, "unique actual load: {source}");
    let read = reads[0];
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, read.scope).unwrap());
    let reaches = info.refs.get(&read.range.start());
    assert_eq!(
        reaches.is_some(),
        visited,
        "actual load visitation: {source}"
    );
    if visited {
        let reaches = reaches.unwrap();
        if bound {
            assert!(!reaches.is_empty(), "nonvacuous bound reaches: {source}");
        }
        assert_eq!(
            !reaches.is_empty() && reaches.iter().all(|reach| reach.binding.is_some()),
            bound,
            "actual boundness: {source}"
        );
    }
}
#[test]
fn known_nonempty_for_binds_target_on_every_normal_completion() {
    for literal in ["(1,)", "[1, 2]", "'x'", "b'x'"] {
        check(
            &format!("def f():\n for target in {literal}:\n  pass\n return target\n"),
            "target",
            true,
            true,
        );
    }
    check(
        "def f(items):\n target=0\n for target in items:\n  pass\n return target\n",
        "target",
        true,
        true,
    );
}
#[test]
fn required_first_iteration_abrupt_exit_has_no_zero_iteration_tail() {
    for literal in ["(1,)", "[1]"] {
        check(
            &format!(
                "def f():\n marker=1\n for target in {literal}:\n  return 0\n return marker\n"
            ),
            "marker",
            false,
            false,
        );
    }
    check(
        "def f(items):\n marker=1\n for target in items:\n  return 0\n return marker\n",
        "marker",
        true,
        true,
    );
}
#[test]
fn first_iteration_break_and_continue_preserve_target_binding() {
    check(
        "def f():\n for target in (1,):\n  break\n else:\n  return 0\n return target\n",
        "target",
        true,
        true,
    );
    check(
        "def f():\n for target in (1,):\n  continue\n return target\n",
        "target",
        true,
        true,
    );
    check(
        "def f():\n for target in []:\n  pass\n return target\n",
        "target",
        true,
        false,
    );
}
