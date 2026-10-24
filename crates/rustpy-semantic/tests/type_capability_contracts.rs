use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn expression(source: &str) -> Type {
    let db = Analysis::for_project(Path::new("/capability-contracts"));
    let id = db.set_file_text(Path::new("/capability-contracts/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(statement) = module.statements().last().unwrap() else {
        panic!("expression")
    };
    Inferer::new(&db).type_of_expr(&file, &statement.expr().unwrap())
}

#[test]
fn literal_string_preserves_safe_operations_without_accepting_general_strings() {
    for body in [
        "safe",
        "safe + 'suffix'",
        "safe.upper()",
        "safe[1:]",
        "f'{safe}'",
    ] {
        let value = expression(&format!("safe='literal'\n{body}\n"));
        assert!(
            matches!(value, Type::LiteralStringValue | Type::Literal(_)),
            "{body}: {value:?}"
        );
    }
    let source =
        "from typing import LiteralString\ndef use(safe:LiteralString, ordinary:str): pass\n";
    let db = Analysis::for_project(Path::new("/capability-compatibility"));
    let id = db.set_file_text(
        Path::new("/capability-compatibility/m.py"),
        &format!("{source}def values(safe:LiteralString, ordinary:str):\n    safe\n    ordinary\n"),
    );
    let file = db.file(id).unwrap();
    let names: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .filter(|name| {
            matches!(
                name.syntax().text().to_string().as_str(),
                "safe" | "ordinary"
            )
        })
        .collect();
    let infer = Inferer::new(&db);
    let safe = infer.type_of_expr(&file, &ast::Expr::Name(names[names.len() - 2].clone()));
    let ordinary = infer.type_of_expr(&file, &ast::Expr::Name(names[names.len() - 1].clone()));
    assert!(matches!(safe, Type::LiteralString));
    assert_eq!(ordinary.to_string(), "str");
    assert!(infer.is_compatible(&safe, &ordinary));
    assert!(!infer.is_compatible(&ordinary, &safe));
}

#[test]
fn readonly_dictionary_fields_are_covariant_but_mutable_fields_are_invariant() {
    let source = "from typing import TypedDict\nfrom typing_extensions import ReadOnly\nclass ReadInt(TypedDict):\n    value:ReadOnly[int]\nclass ReadObject(TypedDict):\n    value:ReadOnly[object]\nclass WriteInt(TypedDict):\n    value:int\nclass WriteObject(TypedDict):\n    value:object\ndef check(a:ReadInt,b:ReadObject,c:WriteInt,d:WriteObject):\n    a\n    b\n    c\n    d\n";
    let db = Analysis::for_project(Path::new("/dictionary-capabilities"));
    let id = db.set_file_text(Path::new("/dictionary-capabilities/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let infer = Inferer::new(&db);
    let values: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::ExprStmt::cast)
        .map(|statement| infer.type_of_expr(&file, &statement.expr().unwrap()))
        .collect();
    assert_eq!(values.len(), 4);
    assert!(infer.is_compatible(&values[0], &values[1]));
    assert!(infer.is_compatible(&values[2], &values[0]));
    assert!(!infer.is_compatible(&values[0], &values[2]));
    assert!(!infer.is_compatible(&values[2], &values[3]));
    assert!(!infer.is_compatible(&values[3], &values[2]));
}

#[test]
fn declared_literal_string_contract_and_inferred_value_stay_distinct() {
    let prefix = "from typing import LiteralString\ndef provide()->LiteralString: return 'safe'\n";
    let Type::Function(signature) = expression(&format!("{prefix}provide\n")) else {
        panic!("actual callable signature");
    };
    assert!(matches!(signature.ret, Type::LiteralString));
    let inferred = expression(&format!("{prefix}provide()\n"));
    assert!(matches!(inferred, Type::LiteralStringValue));
    assert_eq!(inferred.to_string(), "str");
}
#[test]
fn canonical_typing_readonly_is_available_in_actual_python313_world() {
    let path = Path::new("/readonly313/m.py");
    let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
        vec![Path::new("/readonly313").to_path_buf()],
        Vec::new(),
        (3, 13),
    ));
    let source = "from typing import TypedDict, ReadOnly\nclass Settings(TypedDict):\n    value:ReadOnly[int]\ndef f(value:Settings):\n    value\n";
    let file = db.file(db.set_file_text(path, source)).unwrap();
    assert!(file.parse.errors().is_empty());
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .filter(|name| name.syntax().text() == "value")
        .last()
        .unwrap();
    let infer = Inferer::new(&db);
    let Type::Instance(class, arguments) = infer.type_of_expr(&file, &ast::Expr::Name(name)) else {
        panic!("actual declared dictionary receiver");
    };
    let fields = infer.typed_dict_fields(&class, &arguments).unwrap();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].0, "value");
    assert!(fields[0].3);
}
