use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::{
    db::Analysis,
    flow,
    infer::Inferer,
    types::{LiteralValue, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, truth_slot: bool, terminal: bool) {
    let db = Analysis::for_project(Path::new("/chain-operation-consumer"));
    let id = db.set_file_text(Path::new("/chain-operation-consumer/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let constructor = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|e| e.syntax().text() == "C()")
        .unwrap();
    let inf = Inferer::new(&db);
    let receiver = inf.type_of_expr(&file, &constructor);
    let method = inf.lookup_member(&receiver, "__lt__").unwrap();
    let result = inf.call_result(&method, &[Type::Literal(LiteralValue::Int(0))], &[]);
    let invoked = if truth_slot {
        inf.call_result(&inf.lookup_member(&result, "__bool__").unwrap(), &[], &[])
    } else {
        result
    };
    assert!(!invoked.is_unknown());
    assert_eq!(
        matches!(invoked, Type::Never),
        terminal,
        "actual invoked slot BEFORE consumer: {source}"
    );
    let reads: Vec<_> = ["later", "missing"]
        .into_iter()
        .map(|name| {
            file.scopes
                .references
                .iter()
                .find(|r| r.name == name && !r.is_write)
                .unwrap()
        })
        .collect();
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, reads[0].scope).unwrap());
    let actual: Vec<_> = inspections::inspect(&db, &file, &InspectionOptions::default())
        .into_iter()
        .filter(|p| p.diagnostic.code == "PyUnboundLocalVariable")
        .map(|p| {
            (
                p.diagnostic.code.to_string(),
                usize::from(p.diagnostic.range.start()),
                usize::from(p.diagnostic.range.end()),
            )
        })
        .collect();
    let expected = if terminal {
        Vec::new()
    } else {
        reads
            .iter()
            .map(|r| {
                (
                    "PyUnboundLocalVariable".to_owned(),
                    usize::from(r.range.start()),
                    usize::from(r.range.end()),
                )
            })
            .collect()
    };
    assert_eq!(
        actual, expected,
        "actual exact later-operand/tail diagnostics; semantic reads={:?}; {source}",
        info.refs
    );
    assert_eq!(
        !info.refs.contains_key(&reads[0].range.start()),
        terminal,
        "semantic operation must skip actual later operand iff terminal: {source}"
    );
    if terminal {
        assert!(
            !info.refs.contains_key(&reads[1].range.start()),
            "terminal tail: {source}"
        );
    }
    // The inspection intentionally reports both unbound reads; semantic Flow
    // may stop after the first definite NameError in the normal control.
}
#[test]
fn first_comparison_invocation_stops_before_later_operand() {
    for (ret, body, terminal) in [
        ("Never", "raise RuntimeError", true),
        ("bool", "return True", false),
    ] {
        let source = format!(
            "from typing import Never\nclass C:\n    def __lt__(self,other:int)->{ret}:{body}\ndef f():\n    items=C()\n    items < 0 < later\n    print(missing)\n    later=1\n    missing=1\n"
        );
        check(&source, false, terminal);
    }
}
#[test]
fn intermediate_comparison_truth_stops_before_later_operand() {
    for (ret, body, terminal) in [
        ("Never", "raise RuntimeError", true),
        ("bool", "return True", false),
    ] {
        let source = format!(
            "from typing import Never\nclass Truth:\n    def __bool__(self)->{ret}:{body}\nclass C:\n    def __lt__(self,other:int)->Truth:return Truth()\ndef f():\n    items=C()\n    items < 0 < later\n    print(missing)\n    later=1\n    missing=1\n"
        );
        check(&source, true, terminal);
    }
}
