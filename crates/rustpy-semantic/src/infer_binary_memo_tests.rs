//! Count-based reuse and authority controls; no timing thresholds.
use super::*;
use std::path::Path;

fn binaries(file: &Arc<FileInfo>) -> Vec<ast::Expr> {
    file.parse
        .syntax_node()
        .descendants()
        .filter_map(ast::BinaryExpr::cast)
        .map(ast::Expr::Binary)
        .collect()
}

#[test]
fn all_deep_binary_prefixes_reuse_completed_operator_results() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let db = Analysis::for_project(Path::new("/binary-memo-count"));
            let path = Path::new("/binary-memo-count/main.py");
            let id = db.set_file_text(path, &format!("result = {}\n", vec!["1"; 9500].join(" + ")));
            let file = db.file(id).unwrap();
            let expressions = binaries(&file);
            assert_eq!(expressions.len(), 9499);
            let inf = Inferer::new(&db);
            assert_eq!(
                inf.type_of_expr(&file, &expressions[0]).to_string(),
                "Literal[9500]"
            );
            let initial = inf.binary_applications.get();
            for expression in &expressions {
                let terms = usize::from(expression.text_range().len()).div_ceil(4);
                assert_eq!(
                    inf.type_of_expr(&file, expression).to_string(),
                    format!("Literal[{terms}]")
                );
            }
            // Lazy dependency discovery may retire the first captured epoch. It
            // permits another linear pass, never re-inference of every prefix.
            assert!(inf.binary_applications.get() <= initial + 2 * expressions.len());
            let settled = inf.binary_applications.get();
            for expression in expressions.iter().rev() {
                inf.type_of_expr(&file, expression);
            }
            assert_eq!(inf.binary_applications.get(), settled);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn memo_rejects_replaced_source_dependency_epochs_and_contextual_fallbacks() {
    let db = Analysis::for_project(Path::new("/binary-memo-authority"));
    let path = Path::new("/binary-memo-authority/main.py");
    let id = db.set_file_text(path, "result = 1 + 2\n");
    let file = db.file(id).unwrap();
    let expression = binaries(&file).remove(0);
    let inf = Inferer::new(&db);
    assert_eq!(
        inf.type_of_expr(&file, &expression).to_string(),
        "Literal[3]"
    );
    assert_eq!(
        inf.type_of_expr(&file, &expression).to_string(),
        "Literal[3]"
    );
    let before = inf.binary_applications.get();
    inf.expected_context.active.set(true);
    assert_eq!(
        inf.type_of_expr(&file, &expression).to_string(),
        "Literal[3]"
    );
    assert!(
        inf.binary_applications.get() > before,
        "an isolated contextual fallback must bypass ordinary memo facts"
    );
    inf.expected_context.active.set(false);
    let before = inf.binary_applications.get();
    db.set_file_text(
        Path::new("/binary-memo-authority/dependency.py"),
        "changed = True\n",
    );
    assert_eq!(
        inf.type_of_expr(&file, &expression).to_string(),
        "Literal[3]"
    );
    assert!(
        inf.binary_applications.get() > before,
        "dependency changes retire captured results"
    );
    db.set_file_text(path, "result = 4 + 5\n");
    let replacement = db.file(id).unwrap();
    assert!(!Arc::ptr_eq(&file, &replacement));
    let changed = binaries(&replacement).remove(0);
    assert_eq!(changed.text_range(), expression.text_range());
    assert_eq!(
        inf.type_of_expr(&replacement, &changed).to_string(),
        "Literal[9]"
    );
    let before = inf.binary_applications.get();
    assert_eq!(
        inf.type_of_expr(&file, &expression).to_string(),
        "Literal[3]"
    );
    assert!(
        inf.binary_applications.get() > before,
        "a retired FileInfo cannot read or republish its memo"
    );
}

#[test]
fn memo_retains_reflected_operator_and_flow_site_results() {
    let db = Analysis::for_project(Path::new("/binary-memo-protocol"));
    let path = Path::new("/binary-memo-protocol/main.py");
    let id = db.set_file_text(path, "class Left:\n def __add__(self, other: object) -> int: ...\nclass Right(Left):\n def __radd__(self, other: object) -> str: ...\nleft = Left()\nright = Right()\nleft + right\ndef add(value: int | str):\n if isinstance(value, str):\n  value + value\n else:\n  value + value\n");
    let file = db.file(id).unwrap();
    let inf = Inferer::new(&db);
    // The annotation's union is also binary syntax; these are specifically
    // runtime additions, with identical text at two distinct narrowed sites.
    let expressions: Vec<_> = binaries(&file)
        .into_iter()
        .filter(|expression| {
            matches!(expression, ast::Expr::Binary(binary)
            if binary.op().is_some_and(|op| op.kind() == SyntaxKind::PLUS))
        })
        .collect();
    assert_eq!(expressions.len(), 3);
    for (expression, expected) in expressions.iter().zip(["str", "str", "int"]) {
        let uncached = Inferer::new(&db).type_of_expr(&file, expression);
        assert_eq!(
            uncached.to_string(),
            expected,
            "fresh inference at {:?}",
            expression.text_range()
        );
        for _ in 0..2 {
            let actual = inf.type_of_expr(&file, expression);
            assert_eq!(actual.to_string(), expected);
            assert_eq!(
                actual,
                uncached,
                "memoized and fresh inference must agree at {:?}",
                expression.text_range()
            );
        }
    }
}
