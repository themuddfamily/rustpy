//! `typing`/`collections` aliases resolve to their real classes, so their own
//! members are not reported as unresolved attribute references.
use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn unresolved_attributes(source: &str) -> Vec<String> {
    let db = Analysis::for_project(Path::new("/collection-aliases"));
    let id = db.set_file_text(Path::new("/collection-aliases/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    inspections::inspect(&db, &file, &InspectionOptions::default())
        .into_iter()
        .filter(|problem| {
            problem.diagnostic.code == "PyUnresolvedReferences"
                && problem
                    .diagnostic
                    .message
                    .starts_with("Unresolved attribute reference")
        })
        .map(|problem| problem.diagnostic.message)
        .collect()
}

#[test]
fn collection_alias_members_are_resolved() {
    let source = "import collections\nfrom collections import Counter\nfrom typing import Deque, OrderedDict, DefaultDict\nimport typing\n\n\ndef use(d: Deque[int], o: OrderedDict[str, int], dd: DefaultDict[str, int], tc: typing.Counter[str], cc: collections.Counter[str]):\n    Counter('abc').most_common(1)\n    Counter().most_common()\n    tc.most_common()\n    cc.elements()\n    d.appendleft(1)\n    d.rotate(1)\n    o.move_to_end('k')\n    dd.default_factory\n";
    assert_eq!(unresolved_attributes(source), Vec::<String>::new());
}

#[test]
fn genuinely_missing_collection_members_are_still_reported() {
    let source = "from typing import Counter, Deque\n\n\ndef use(c: Counter[str], d: Deque[int]):\n    c.not_a_counter_method()\n    d.not_a_deque_method()\n";
    let problems = unresolved_attributes(source);
    assert_eq!(problems.len(), 2, "{problems:#?}");
    assert!(
        problems[0].contains("not_a_counter_method"),
        "{problems:#?}"
    );
    assert!(problems[1].contains("not_a_deque_method"), "{problems:#?}");
}
