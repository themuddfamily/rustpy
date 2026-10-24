//! Standard protocol invocation, consumption, and exhaustion are distinct events.
use rustpy_semantic::{
    db::Analysis,
    flow,
    infer::Inferer,
    types::{LiteralValue, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, slot: &str, never: bool, terminal: bool) {
    let db = Analysis::for_project(Path::new("/standard-protocol-audit"));
    let id = db.set_file_text(Path::new("/standard-protocol-audit/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let receivers: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|expression| expression.syntax().text() == "C()")
        .collect();
    assert_eq!(
        receivers.len(),
        1,
        "unique actual constructed protocol receiver"
    );
    let inf = Inferer::new(&db);
    let receiver = inf.type_of_expr(&file, &receivers[0]);
    assert!(
        !receiver.is_unknown(),
        "actual receiver BEFORE Flow: {source}"
    );
    let method = inf
        .lookup_member(&receiver, slot)
        .expect("actual maintained protocol slot");
    let arguments = if matches!(slot, "__getitem__" | "__lt__") {
        vec![Type::Literal(LiteralValue::Int(0))]
    } else {
        Vec::new()
    };
    let result = inf.call_result(&method, &arguments, &[]);
    assert_eq!(
        matches!(result, Type::Never),
        never,
        "actual slot producer BEFORE Flow: {result}; {source}"
    );
    assert!(!result.is_unknown(), "normal slot result cannot be Unknown");
    tail(&db, &file, terminal);
}
fn tail(db: &Analysis, file: &rustpy_semantic::db::FileInfo, terminal: bool) {
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(db, file, flow::flow_scope(file, reference.scope).unwrap());
    assert_eq!(
        !info.refs.contains_key(&reference.range.start()),
        terminal,
        "{}",
        file.text
    );
}
#[test]
fn intrinsic_len_iter_acquisition_and_returning_controls() {
    for (slot, operation, ret, body, never) in [
        ("__len__", "len(items)", "Never", "raise RuntimeError", true),
        ("__len__", "len(items)", "int", "return 0", false),
        (
            "__iter__",
            "iter(items)",
            "Never",
            "raise RuntimeError",
            true,
        ),
        ("__iter__", "iter(items)", "'C'", "return self", false),
    ] {
        let source = format!("from typing import Never\nclass C:\n    def {slot}(self)->{ret}:{body}\n    def __next__(self)->int:raise StopIteration\ndef f():\n    marker=1\n    items=C()\n    {operation}\n    return marker\n");
        check(&source, slot, never, never);
    }
}
#[test]
fn next_without_default_is_not_next_with_exhaustion_default() {
    for (operation, ret, body, never, terminal) in [
        ("next(items)", "Never", "raise StopIteration", true, true),
        ("next(items,0)", "Never", "raise StopIteration", true, false),
        ("next(items)", "int", "return 1", false, false),
    ] {
        let source = format!("from typing import Never\nclass C:\n    def __iter__(self)->'C':return self\n    def __next__(self)->{ret}:{body}\ndef f():\n    marker=1\n    items=C()\n    {operation}\n    return marker\n");
        check(&source, "__next__", never, terminal);
    }
}
#[test]
fn builtin_import_aliases_and_local_shadow_do_not_share_authority() {
    for (prefix, operation, terminal) in [
        ("from builtins import len as size\n", "size(items)", true),
        ("import builtins as b\n", "b.len(items)", true),
        ("def len(value)->int:return 0\n", "len(items)", false),
        ("def size(value)->int:return 0\n", "size(items)", false),
    ] {
        let source = format!("from typing import Never\n{prefix}class C:\n    def __len__(self)->Never:raise RuntimeError\ndef f():\n    marker=1\n    items=C()\n    {operation}\n    return marker\n");
        // Shadow controls establish the known but uninvoked slot contract.
        check(&source, "__len__", true, terminal);
    }
}
#[test]
fn async_iteration_acquisition_and_exhaustion_are_distinct() {
    for (ret, body, never, terminal) in [
        ("Never", "raise RuntimeError", true, true),
        ("'C'", "return self", false, false),
    ] {
        let source = format!("from typing import Never\nclass C:\n    def __aiter__(self)->{ret}:{body}\n    async def __anext__(self)->Never:raise StopAsyncIteration\nasync def f():\n    marker=1\n    items=C()\n    async for value in items:\n        pass\n    return marker\n");
        check(&source, "__aiter__", never, terminal);
    }
    // A genuine awaited Never result can be StopAsyncIteration; async-for
    // consumes it and completes normally instead of invoking a terminal tail.
    let source = "from typing import Never\nclass C:\n    def __aiter__(self)->'C':return self\n    async def __anext__(self)->Never:raise StopAsyncIteration\nasync def probe(value:C):\n    await value.__anext__()\nasync def f():\n    marker=1\n    items=C()\n    async for value in items:\n        pass\n    return marker\n";
    let db = Analysis::for_project(Path::new("/async-exhaustion-audit"));
    let id = db.set_file_text(Path::new("/async-exhaustion-audit/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let awaits: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::AwaitExpr::cast)
        .collect();
    assert_eq!(awaits.len(), 1);
    let result = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Await(awaits[0].clone()));
    assert!(
        matches!(result, Type::Never),
        "actual awaited producer BEFORE Flow: {result}"
    );
    tail(&db, &file, false);
}
fn compare_truth(source: &str, terminal: bool) {
    let db = Analysis::for_project(Path::new("/comparison-truth-audit"));
    let id = db.set_file_text(Path::new("/comparison-truth-audit/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let expressions: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|e| e.syntax().text() == "C()")
        .collect();
    assert_eq!(expressions.len(), 1);
    let inf = Inferer::new(&db);
    let receiver = inf.type_of_expr(&file, &expressions[0]);
    let method = inf.lookup_member(&receiver, "__lt__").unwrap();
    let result = inf.call_result(&method, &[Type::Literal(LiteralValue::Int(0))], &[]);
    assert!(
        !result.is_unknown(),
        "actual comparison producer BEFORE Flow"
    );
    let truth = inf.lookup_member(&result, "__bool__").unwrap();
    assert!(
        matches!(inf.call_result(&truth, &[], &[]), Type::Never),
        "actual comparison-result truth contract BEFORE Flow"
    );
    tail(&db, &file, terminal);
}
#[test]
fn comparison_result_truth_is_consumed_only_where_python_tests_it() {
    let prefix = "from typing import Never\nclass Truth:\n    def __bool__(self)->Never:raise RuntimeError\nclass C:\n    def __lt__(self,other:int)->Truth:return Truth()\n";
    for (statement, terminal) in [
        ("items < 0", false),
        ("if items < 0:\n        pass", true),
        ("while items < 0:\n        break", true),
        ("not (items < 0)", true),
        ("items < 0 < 1", true),
    ] {
        compare_truth(&format!("{prefix}def f():\n    marker=1\n    items=C()\n    {statement}\n    return marker\n"), terminal);
    }
}
#[test]
fn comparison_invocation_never_and_normal_result_controls() {
    for (ret, body, never) in [
        ("Never", "raise RuntimeError", true),
        ("bool", "return False", false),
    ] {
        let source = format!("from typing import Never\nclass C:\n    def __lt__(self,other:int)->{ret}:{body}\ndef f():\n    marker=1\n    items=C()\n    items < 0\n    return marker\n");
        check(&source, "__lt__", never, never);
    }
}
#[test]
fn structural_unpack_acquisition_and_required_elements_differ_from_for_exhaustion() {
    for (ret, body, statement, never, terminal) in [
        (
            "Never",
            "raise RuntimeError",
            "first,second=items",
            true,
            true,
        ),
        ("'C'", "return self", "first,second=items", false, true),
        ("'C'", "return self", "*rest,=items", false, false),
    ] {
        let source = format!("from typing import Never\nclass C:\n    def __iter__(self)->{ret}:{body}\n    def __next__(self)->Never:raise StopIteration\ndef f():\n    marker=1\n    items=C()\n    {statement}\n    return marker\n");
        check(
            &source,
            if never { "__iter__" } else { "__next__" },
            true,
            terminal,
        );
    }
    let source="class C:\n    def __init__(self):self.count=0\n    def __iter__(self)->'C':return self\n    def __next__(self)->int:\n        self.count+=1\n        if self.count>2:raise StopIteration\n        return self.count\ndef f():\n    marker=1\n    items=C()\n    first,second=items\n    return marker\n";
    check(source, "__next__", false, false);
}
#[test]
fn structural_sequence_exhaustion_has_zero_or_positive_required_arity() {
    for (statement, terminal) in [("first,=items", true), ("*rest,=items", false)] {
        let source=format!("from typing import Never\nclass C:\n    def __getitem__(self,index:int)->Never:raise IndexError\ndef f():\n    marker=1\n    items=C()\n    {statement}\n    return marker\n");
        check(&source, "__getitem__", true, terminal);
    }
}
#[test]
fn builtin_operand_rebinding_and_explicit_slot_mutation_remain_normal() {
    let source="from typing import Never\nclass C:\n    def __len__(self)->Never:raise RuntimeError\nclass Normal:\n    def __len__(self)->int:return 0\ndef f():\n    marker=1\n    items=C()\n    len((items:=Normal()))\n    return marker\n";
    // The known old slot is not invoked after the argument changes receiver.
    check(source, "__len__", true, false);
    let source="from typing import Never\nclass C:\n    def __len__(self)->Never:raise RuntimeError\ndef normal(self)->int:return 0\ndef f():\n    marker=1\n    items=C()\n    C.__len__=normal\n    len(items)\n    return marker\n";
    // A replaced slot has no genuine Never premise; require conservative tail.
    let db = Analysis::for_project(Path::new("/builtin-slot-mutation-audit"));
    let id = db.set_file_text(Path::new("/builtin-slot-mutation-audit/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    tail(&db, &file, false);
}
