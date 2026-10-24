//! Authority/evaluation hazards for finite receiver and declared-await leaves.
use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
fn check(source: &str, terminal: bool) {
    let db = Analysis::for_project(Path::new("/receiver-await-hazards"));
    let id = db.set_file_text(Path::new("/receiver-await-hazards/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert_eq!(
        !info.refs.contains_key(&reference.range.start()),
        terminal,
        "{source}"
    );
}
#[test]
fn declared_await_alias_retains_old_value_and_rebinding_retires_new_value() {
    check("from typing import Awaitable,Never\nasync def f(pending:Awaitable[Never],normal:Awaitable[int]):\n    marker=1\n    alias=pending\n    pending=normal\n    await alias\n    return marker\n", true);
    check("from typing import Awaitable,Never\nasync def f(pending:Awaitable[Never],normal:Awaitable[int]):\n    marker=1\n    pending=normal\n    await pending\n    return marker\n", false);
}
#[test]
fn fake_awaitable_head_and_unknown_branch_preserve_normal_path() {
    check("from typing import Never\nclass Awaitable:pass\nasync def f(pending:Awaitable[Never]):\n    marker=1\n    await pending\n    return marker\n", false);
    check("from typing import Awaitable,Never\nasync def f(flag:bool,pending:Awaitable[Never],normal:Awaitable[int]):\n    marker=1\n    value=pending if flag else normal\n    await value\n    return marker\n", false);
}
#[test]
fn fake_property_and_replaced_property_preserve_normal_path() {
    check("from typing import Never\ndef property(function):return function\nclass C:\n    @property\n    def terminal(self)->Never:raise RuntimeError\ndef f():\n    marker=1\n    c=C()\n    c.terminal\n    return marker\n", false);
    check("from typing import Never\nclass C:\n    @property\n    def terminal(self)->Never:raise RuntimeError\nC.terminal=1\ndef f():\n    marker=1\n    c=C()\n    c.terminal\n    return marker\n", false);
}
#[test]
fn super_zeroarg_control_and_explicit_receiver_rebinding() {
    check("from typing import Never\nclass Base:\n    def stop(self)->Never:raise RuntimeError\nclass Child(Base):\n    def run(self):\n        marker=1\n        super().stop()\n        return marker\n", true);
    check("from typing import Never\nclass Base:\n    def stop(self)->Never:raise RuntimeError\nclass Child(Base):\n    def run(self,other):\n        marker=1\n        self=other\n        super(Child,self).stop()\n        return marker\n", false);
}
#[test]
fn nested_initialized_field_mutation_and_custom_new_are_not_exact() {
    check("from typing import Never\nclass Leaf:\n    def stop(self)->Never:raise RuntimeError\nclass Middle:\n    def __init__(self):self.child=Leaf()\nclass Outer:\n    def __init__(self):self.middle=Middle()\ndef replace(outer,other):outer.middle=other\ndef f():\n    marker=1\n    outer=Outer()\n    outer.middle.child.stop()\n    return marker\n", false);
    check("from typing import Never\nclass Leaf:\n    def __new__(cls):return object()\n    def stop(self)->Never:raise RuntimeError\nclass Middle:\n    def __init__(self):self.child=Leaf()\nclass Outer:\n    def __init__(self):self.middle=Middle()\ndef f():\n    marker=1\n    outer=Outer()\n    outer.middle.child.stop()\n    return marker\n", false);
}
