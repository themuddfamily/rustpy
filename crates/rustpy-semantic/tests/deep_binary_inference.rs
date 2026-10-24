//! Preserve exact operator results when binary syntax exceeds native call depth.
use rustpy_semantic::{infer::Inferer, types::Type, Analysis};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn inferred_type(expression: &str) -> Type {
    let db = Analysis::for_project(Path::new("/deep-binary-inference"));
    let id = db.set_file_text(
        Path::new("/deep-binary-inference/main.py"),
        &format!("result = {expression}\n"),
    );
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let binary = file
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::BinaryExpr::cast)
        .unwrap();
    Inferer::new(&db).type_of_expr(&file, &ast::Expr::Binary(binary))
}

fn inferred(expression: &str) -> String {
    inferred_type(expression).to_string()
}

#[test]
fn deep_binary_chain_retains_exact_result_on_the_cli_worker_stack() {
    // Match the existing CLI worker's stack, rather than enlarging it to hide
    // recursive inference. A successful Unknown/Any result is not sufficient.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            assert_eq!(inferred(&vec!["1"; 9500].join(" + ")), "Literal[9500]");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn iterative_binary_evaluation_preserves_association_and_precedence() {
    assert_eq!(inferred("15 - 4 - 2 * 3"), "Literal[5]");
    assert_eq!(inferred("15 - (4 - 2 * 3)"), "Literal[17]");
    // String operations retain literal-derived provenance while their ordinary
    // display type is str. A plain str result would lose that safety contract.
    assert_eq!(inferred_type("'a' + 'b' + 'c'"), Type::LiteralStringValue);
}
