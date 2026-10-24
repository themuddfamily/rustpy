//! Class construction is eager; its namespace is distinct from outer locals.
use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;

fn tail(source: &str, name: &str, reachable: bool, binding_lines: &[usize]) {
    let db = Analysis::for_project(Path::new("/class-completions"));
    let id = db.set_file_text(Path::new("/class-completions/m.py"), source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|reference| reference.name == name && !reference.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert_eq!(
        info.refs.contains_key(&reference.range.start()),
        reachable,
        "{source}"
    );
    if reachable {
        let mut lines: Vec<_> = info.refs[&reference.range.start()]
            .iter()
            .map(|reach| {
                let binding = file
                    .scopes
                    .binding(reach.binding.expect("actual reaching store"));
                source[..usize::from(binding.range.start())]
                    .matches('\n')
                    .count()
                    + 1
            })
            .collect();
        lines.sort_unstable();
        lines.dedup();
        assert_eq!(lines, binding_lines, "{source}");
    }
}

#[test]
fn never_call_in_class_body_stops_parent() {
    tail("from typing import NoReturn\ndef die()->NoReturn:\n    raise RuntimeError\ndef f():\n    marker=1\n    class C:\n        die()\n    return marker\n", "marker", false, &[]);
}

#[test]
fn explicit_raise_in_class_body_stops_parent() {
    tail(
        "def f():\n    marker=1\n    class C:\n        raise RuntimeError\n    return marker\n",
        "marker",
        false,
        &[],
    );
}

#[test]
fn normal_class_local_store_does_not_change_function_local() {
    tail(
        "def f():\n    value=1\n    class C:\n        value=2\n    return value\n",
        "value",
        true,
        &[2],
    );
}

#[test]
fn nonlocal_class_store_updates_actual_function_cell() {
    tail("def f():\n    value=1\n    class C:\n        nonlocal value\n        value=2\n    return value\n", "value", true, &[5]);
}

#[test]
fn global_class_store_updates_module_namespace() {
    tail(
        "value=1\nclass C:\n    global value\n    value=2\nvalue\n",
        "value",
        true,
        &[4],
    );
}

#[test]
fn global_class_store_does_not_overwrite_same_named_function_local() {
    tail("value=0\ndef f():\n    value=1\n    class C:\n        global value\n        value=2\n    return value\n", "value", true, &[3]);
}

#[test]
fn class_exception_projects_completed_nonlocal_store_to_outer_handler() {
    tail("def f():\n    value=1\n    try:\n        class C:\n            nonlocal value\n            value=2\n            raise RuntimeError\n    except:\n        return value\n", "value", true, &[2, 6]);
}

#[test]
fn class_own_store_shadows_outer_never_function() {
    tail("from typing import NoReturn\ndef die()->NoReturn:\n    raise RuntimeError\ndef f():\n    marker=1\n    class C:\n        die=lambda: None\n        die()\n    return marker\n", "marker", true, &[5]);
}

#[test]
fn nested_class_preserves_nonlocal_destination() {
    tail("def f():\n    value=1\n    class Outer:\n        class Inner:\n            nonlocal value\n            value=2\n    return value\n", "value", true, &[6]);
}
