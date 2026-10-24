use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
fn check(source: &str, modules: &[(&str, &str)], callee: &str, never: bool, reachable: bool) {
    let directory = tempfile::TempDir::with_prefix("rustpy-qualified-factory-contract").unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, source).unwrap();
    for (name, text) in modules {
        std::fs::write(directory.path().join(format!("{name}.py")), text).unwrap();
    }
    let db = Analysis::for_project(directory.path());
    let _ = db.module("typing");
    // Freeze the actual dependency source too, rather than asking the frozen
    // resolver to read a previously unadmitted project file during inference.
    for (name, text) in modules {
        let module = db
            .module(name)
            .expect("actual project module must resolve before snapshot");
        assert_eq!(module.path, directory.path().join(format!("{name}.py")));
        assert_eq!(module.text.as_ref(), *text);
    }
    let snapshot = db
        .snapshot(
            std::slice::from_ref(&path),
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    let db = snapshot.analysis();
    let file = db.file_for_path(&path).unwrap();
    let calls: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter(|call| {
            call.callee()
                .is_some_and(|expr| expr.syntax().text() == callee)
        })
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "unique real OUTER factory-result invocation"
    );
    let result =
        Inferer::new(db).type_of_expr(&file, &ast::Expr::Call(calls.into_iter().next().unwrap()));
    assert_eq!(
        matches!(result, Type::Never),
        never,
        "actual public Inferer premise BEFORE flow: {result}; {source}"
    );
    let references: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|reference| reference.name == "marker" && !reference.is_write)
        .collect();
    assert_eq!(references.len(), 1, "real unique assigned tail read");
    let reference = references[0];
    let info = flow::flow_of_with_db(db, &file, flow::flow_scope(&file, reference.scope).unwrap());
    assert_eq!(
        info.refs.contains_key(&reference.range.start()),
        reachable,
        "actual normal completion after returned callable: {source}"
    );
}
const STOP: &str =
    "from typing import Callable,NoReturn\ndef stop()->NoReturn:\n    raise RuntimeError\n";
#[test]
fn qualified_imported_module_factory_result_terminates() {
    let module = format!("{STOP}def factory()->Callable[[],NoReturn]:\n    return stop\n");
    check(
        "import factories as lib\ndef f():\n    marker=1\n    lib.factory()()\n    return marker\n",
        &[("factories", &module)],
        "lib.factory()",
        true,
        false,
    );
}
#[test]
fn intrinsic_static_factory_result_terminates() {
    check(&format!("{STOP}class C:\n    @staticmethod\n    def factory()->Callable[[],NoReturn]:\n        return stop\ndef f():\n    marker=1\n    C.factory()()\n    return marker\n"),&[],"C.factory()",true,false);
}
#[test]
fn intrinsic_class_factory_result_terminates() {
    check(&format!("{STOP}class C:\n    @classmethod\n    def factory(cls)->Callable[[],NoReturn]:\n        return stop\ndef f():\n    marker=1\n    C.factory()()\n    return marker\n"),&[],"C.factory()",true,false);
}
#[test]
fn ordinary_instance_factory_result_terminates() {
    check(&format!("{STOP}class C:\n    def factory(self)->Callable[[],NoReturn]:\n        return stop\ndef f():\n    c=C()\n    marker=1\n    c.factory()()\n    return marker\n"),&[],"c.factory()",true,false);
}
#[test]
fn returning_intrinsic_static_factory_result_keeps_flow() {
    check("from typing import Callable\ndef stop()->int:\n    return 1\nclass C:\n    @staticmethod\n    def factory()->Callable[[],int]:\n        return stop\ndef f():\n    marker=1\n    C.factory()()\n    return marker\n",&[],"C.factory()",false,true);
}
#[test]
fn shadowed_noreturn_marker_cannot_make_factory_terminal() {
    check("from typing import Callable\nclass NoReturn:\n    pass\ndef stop()->NoReturn:\n    return NoReturn()\nclass C:\n    @staticmethod\n    def factory()->Callable[[],NoReturn]:\n        return stop\ndef f():\n    marker=1\n    C.factory()()\n    return marker\n",&[],"C.factory()",false,true);
}
