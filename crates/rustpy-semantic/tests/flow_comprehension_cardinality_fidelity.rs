use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(clauses: &str, cardinalities: &[usize], terminal: bool) {
    let head = "from typing import Never\ndef die()->Never: raise RuntimeError\n";
    let element = if terminal { "die()" } else { "(assigned:=1)" };
    let name = if terminal { "marker" } else { "assigned" };
    let source =
        format!("{head}def f():\n marker=1\n values=[{element} {clauses}]\n return {name}\n");
    let db = Analysis::for_project(Path::new("/comprehension-cardinality"));
    let id = db.set_file_text(Path::new("/comprehension-cardinality/m.py"), &source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let actual: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CompFor::cast)
        .map(|clause| {
            let iterator = clause.iter().expect("actual iterable");
            let ast::Expr::Tuple(tuple) = iterator.unparen() else {
                panic!("exact builtin tuple iterable")
            };
            let elements: Vec<_> = tuple.elements().collect();
            assert!(elements
                .iter()
                .all(|element| matches!(element, ast::Expr::Literal(_))));
            elements.len()
        })
        .collect();
    assert_eq!(
        actual, cardinalities,
        "actual nonempty literal counts before Flow"
    );
    let reads: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|read| read.name == name && !read.is_write)
        .collect();
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    let scope = flow::flow_scope(&file, read.scope).unwrap();
    let bindings = file.scopes.bindings_in(scope, name);
    assert_eq!(bindings.len(), 1);
    assert_eq!(read.resolved, Some(bindings[0]));
    if terminal {
        let calls: Vec<_> = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .filter(|call| {
                call.callee()
                    .is_some_and(|callee| callee.syntax().text() == "die")
            })
            .collect();
        assert_eq!(calls.len(), 1);
        assert!(
            matches!(
                Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(calls[0].clone())),
                Type::Never
            ),
            "actual Never producer before Flow"
        );
    }
    let info = flow::flow_of_with_db(&db, &file, scope);
    if terminal {
        assert!(
            !info.refs.contains_key(&read.range.start()),
            "required first element has no empty path"
        );
    } else {
        let reaches = info
            .refs
            .get(&read.range.start())
            .expect("actual tail load");
        assert!(!reaches.is_empty());
        assert!(
            reaches
                .iter()
                .all(|reach| reach.binding == Some(bindings[0])),
            "required first store cannot lose binding through fabricated zero path"
        );
    }
}
#[test]
fn seventeen_known_elements_preserve_mandatory_first_iteration() {
    let tuple = format!("({})", ["0"; 17].join(","));
    let clauses = format!("for item in {tuple}");
    check(&clauses, &[17], false);
    check(&clauses, &[17], true);
}
#[test]
fn sixty_five_nested_iterations_preserve_mandatory_first_element() {
    let outer = format!("({})", ["0"; 5].join(","));
    let inner = format!("({})", ["0"; 13].join(","));
    let clauses = format!("for item in {outer} for other in {inner}");
    check(&clauses, &[5, 13], false);
    check(&clauses, &[5, 13], true);
}
