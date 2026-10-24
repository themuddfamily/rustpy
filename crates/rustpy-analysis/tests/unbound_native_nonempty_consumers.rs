use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::{
    db::Analysis,
    flow,
    infer::Inferer,
    scope::BindingKind,
    types::{LiteralValue, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
const FOR_SOURCE: &str = "def accepts_int(value: int) -> None:\n    pass\n\ndef accepts_str(value: str) -> None:\n    pass\n\ndef nonempty_target() -> None:\n    for target in (1,):\n        pass\n    accepts_int(target)  # NATIVE: for-valid\n    accepts_str(target)  # NATIVE: for-wrong\n";
const EAGER_SOURCE: &str = "def accepts_int(value: int) -> None:\n    pass\n\ndef accepts_str(value: str) -> None:\n    pass\n\ndef nonempty_eager() -> None:\n    values = [(assigned := 1) for item in (0,)]\n    accepts_int(assigned)  # NATIVE: eager-valid\n    accepts_str(assigned)  # NATIVE: eager-wrong\n\ndef empty_eager() -> None:\n    values = [(missing := 1) for item in ()]\n    accepts_int(missing)  # NATIVE: eager-empty-unbound\n";

fn check(source: &str, guaranteed: &str, expected_kind: BindingKind, empty: Option<&str>) {
    let db = Analysis::for_project(Path::new("/native-nonempty-consumer"));
    let id = db.set_file_text(Path::new("/native-nonempty-consumer/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let offset = source.find(&format!("accepts_int({guaranteed})")).unwrap() + "accepts_int(".len();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| usize::from(r.range.start()) == offset)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    let reaches = info
        .refs
        .get(&reference.range.start())
        .expect("actual semantic read point BEFORE inspection");
    assert!(!reaches.is_empty());
    assert!(
        reaches.iter().all(|reach| reach
            .binding
            .is_some_and(|id| file.scopes.binding(id).kind == expected_kind)),
        "actual guaranteed BindingId kind BEFORE inspection: {reaches:?}"
    );
    let expression = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|n| n.text_range() == reference.range)
        .unwrap();
    let inf = Inferer::new(&db);
    let actual_bindings = inf
        .reaching_bindings(&file, &expression)
        .expect("actual Inferer cached-flow guarantee BEFORE inspection");
    assert!(!actual_bindings.is_empty());
    assert!(actual_bindings
        .iter()
        .all(|id| file.scopes.binding(*id).kind == expected_kind));
    let ty = inf.type_of_expr(&file, &ast::Expr::Name(expression));
    assert!(
        matches!(ty, Type::Literal(LiteralValue::Int(1)))
            || matches!(ty,Type::Instance(ref class,_) if class.name=="int"),
        "actual guaranteed read type BEFORE inspection: {ty}"
    );
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
    let expected = empty
        .map(|name| {
            let start =
                source.rfind(&format!("accepts_int({name})")).unwrap() + "accepts_int(".len();
            vec![(
                "PyUnboundLocalVariable".to_owned(),
                start,
                start + name.len(),
            )]
        })
        .unwrap_or_default();
    assert_eq!(
        actual, expected,
        "actual inspection must consume guaranteed nonempty binding while preserving empty control"
    );
}
#[test]
fn exact_native_nonempty_for_guarantee_is_consumed_by_inspection() {
    check(FOR_SOURCE, "target", BindingKind::For, None);
}
#[test]
fn exact_native_eager_walrus_guarantee_is_consumed_while_empty_remains_unbound() {
    check(
        EAGER_SOURCE,
        "assigned",
        BindingKind::Walrus,
        Some("missing"),
    );
}
