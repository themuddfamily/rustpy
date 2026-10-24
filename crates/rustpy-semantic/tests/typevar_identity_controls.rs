use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn result(source: &str) -> String {
    let db = Analysis::for_project(Path::new("/typevar-contracts"));
    let id = db.set_file_text(Path::new("/typevar-contracts/m.py"), source);
    let file = db.file(id).unwrap();
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(statement) = module.statements().last().unwrap() else {
        panic!("expression")
    };
    Inferer::new(&db)
        .type_of_expr(&file, &statement.expr().unwrap())
        .to_string()
}
fn compatible(source: &str) -> bool {
    let db = Analysis::for_project(Path::new("/typevar-compatible"));
    let id = db.set_file_text(Path::new("/typevar-compatible/m.py"), source);
    let file = db.file(id).unwrap();
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let inf = Inferer::new(&db);
    let annotations: Vec<_> = module
        .statements()
        .filter_map(|statement| match statement {
            ast::Stmt::AnnAssign(a) => a.annotation(),
            _ => None,
        })
        .collect();
    assert_eq!(annotations.len(), 1);
    let function = module
        .statements()
        .find_map(|statement| match statement {
            ast::Stmt::FuncDef(function) if function.name_text() == "accept" => Some(function),
            _ => None,
        })
        .unwrap();
    let signature = inf.signature(&file, &function, None);
    assert_eq!(signature.params.len(), 1);
    inf.is_compatible(
        &inf.annotation_type(&file, &annotations[0]),
        &signature.params[0].ty,
    )
}
#[test]
fn canonical_import_factory_and_variable_aliases_keep_constraints() {
    assert_eq!(result("from typing import TypeVar as TV\nFactory=TV\nT=Factory('T',str,bytes)\nAlias=T\nclass Text(str):pass\ndef identity(value:Alias)->Alias:return value\nidentity(Text())\n"),"str");
}
#[test]
fn shadowed_factory_is_an_ordinary_call() {
    assert_eq!(
        result("def TypeVar(name,*constraints):return 1\nT=TypeVar('T',str,bytes)\nT\n"),
        "int"
    );
}
#[test]
fn repeated_constraint_occurrences_select_one_consistent_constraint() {
    assert_eq!(result("from typing import TypeVar\nT=TypeVar('T',str,bytes)\nclass Text(str):pass\ndef same(a:T,b:T)->T:return a\nsame(Text(),'literal')\n"),"str");
}
#[test]
fn conflicting_repeated_constraints_do_not_return_union_as_solution() {
    assert_eq!(result("from typing import TypeVar\nT=TypeVar('T',str,bytes)\ndef same(a:T,b:T)->T:return a\nsame('text',b'bytes')\n"),"Unknown");
}
#[test]
fn pep695_bound_keeps_valid_subtype() {
    assert_eq!(result("class Text(str):pass\ndef identity[T:str](value:T)->T:return value\nidentity(Text())\n"),"Text");
}
#[test]
fn pep695_constraints_promote_subtype() {
    assert_eq!(result("class Text(str):pass\ndef identity[T:(str,bytes)](value:T)->T:return value\nidentity(Text())\n"),"str");
}
#[test]
fn inherited_generic_arguments_use_declared_variance() {
    let prefix="from typing import TypeVar,Generic\nT=TypeVar('T',covariant=True)\nclass Animal:pass\nclass Dog(Animal):pass\nclass Box(Generic[T]):pass\nclass Derived(Box[T]):pass\n";
    assert!(compatible(&format!(
        "{prefix}argument:Derived[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Derived[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
}
#[test]
fn imported_typevars_with_same_display_name_keep_distinct_solutions() {
    let db = Analysis::for_project(Path::new("/typevar-identities"));
    db.set_file_text(
        Path::new("/typevar-identities/a.py"),
        "from typing import TypeVar\nT=TypeVar('T',str,bytes)\n",
    );
    db.set_file_text(
        Path::new("/typevar-identities/b.py"),
        "from typing import TypeVar\nT=TypeVar('T',bound=int)\n",
    );
    let id=db.set_file_text(Path::new("/typevar-identities/main.py"),"from a import T as TextVar\nfrom b import T as NumberVar\ndef pair(a:TextVar,b:NumberVar)->tuple[TextVar,NumberVar]:return a,b\npair('text',1)\n");
    let file = db.file(id).unwrap();
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(statement) = module.statements().last().unwrap() else {
        panic!("expression")
    };
    assert_eq!(
        Inferer::new(&db)
            .type_of_expr(&file, &statement.expr().unwrap())
            .to_string(),
        "tuple[str, int]"
    );
}
