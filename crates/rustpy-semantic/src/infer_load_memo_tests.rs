//! Count-based reuse of load types (names, attributes, calls); no timing
//! thresholds.
use super::*;
use std::path::Path;

/// A function that tests and conditionally rebinds `value` `n` times in a row,
/// the shape of many standard-library functions (`if isinstance(x, str):
/// x = …`). Every condition reads `value` as narrowed by all earlier ones.
fn rebinding_chain(n: usize, condition: &str) -> String {
    let mut source = String::from("def check(x) -> bool:\n    return bool(x)\n\n\ndef f(value):\n");
    for _ in 0..n {
        source.push_str(&format!("    if {condition}:\n        value = value\n"));
    }
    source.push_str("    return value\n");
    source
}

fn returned(file: &Arc<FileInfo>) -> ast::Expr {
    file.parse
        .syntax_node()
        .descendants()
        .filter_map(ast::ReturnStmt::cast)
        .last()
        .and_then(|statement| statement.value())
        .expect("return value")
}

fn name_computations(n: usize, condition: &str) -> usize {
    let db = Analysis::for_project(Path::new("/load-memo-chain"));
    let id = db.set_file_text(
        Path::new("/load-memo-chain/main.py"),
        &rebinding_chain(n, condition),
    );
    let file = db.file(id).unwrap();
    let inf = Inferer::new(&db);
    inf.type_of_expr(&file, &returned(&file));
    inf.name_computations.get()
}

/// Runs `work` allowed at most `budget` load inferences in all sessions,
/// returning how many it used: exponential re-inference fails fast.
fn within_load_budget(budget: usize, work: impl FnOnce()) -> usize {
    LOAD_BUDGET.with(|left| left.set(budget));
    work();
    let used = LOAD_BUDGET.with(|left| budget - left.replace(usize::MAX));
    eprintln!("used {used} of {budget} load inferences");
    used
}

#[test]
fn conditional_rebinding_chains_infer_each_name_load_once() {
    for condition in ["isinstance(value, int)", "check(value)", "value"] {
        let mut small = 0;
        let mut large = 0;
        // Contextual sessions (one per checked argument) still repeat some
        // work, polynomially; without reuse the row was exponential and ran
        // through any budget (forty conditions took hours).
        within_load_budget(400_000, || small = name_computations(6, condition));
        within_load_budget(400_000, || large = name_computations(12, condition));
        // Each of the 2n + 1 loads of `value` is inferred about once here.
        assert!(large <= 4 * 12 + 8, "{condition}: {large} name inferences");
        assert!(large <= 3 * small, "{condition}: {small} then {large}");
    }
}

#[test]
fn load_memo_rejects_replaced_sources_and_keeps_contextual_sessions_apart() {
    let db = Analysis::for_project(Path::new("/load-memo-authority"));
    let path = Path::new("/load-memo-authority/main.py");
    let id = db.set_file_text(path, "value = 'a'\nvalue\n");
    let file = db.file(id).unwrap();
    let load = |file: &Arc<FileInfo>| {
        file.parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .last()
            .map(ast::Expr::Name)
            .unwrap()
    };
    let inf = Inferer::new(&db);
    let expression = load(&file);
    assert_eq!(inf.type_of_expr(&file, &expression).to_string(), "str");
    let computed = inf.name_computations.get();
    assert_eq!(inf.type_of_expr(&file, &expression).to_string(), "str");
    assert_eq!(inf.name_computations.get(), computed, "reused");
    inf.expected_context.active.set(true);
    inf.type_of_expr(&file, &expression);
    assert!(
        inf.name_computations.get() > computed,
        "a contextual session keeps its own facts"
    );
    inf.expected_context.active.set(false);
    db.set_file_text(path, "value = 1.5\nvalue\n");
    let replacement = db.file(id).unwrap();
    let changed = load(&replacement);
    assert_eq!(changed.text_range(), expression.text_range());
    inf.type_of_expr(&replacement, &changed);
    let before = inf.name_computations.get();
    inf.type_of_expr(&file, &expression);
    assert!(
        inf.name_computations.get() > before,
        "a retired FileInfo cannot read its memo"
    );
}

fn chain_computations(n: usize) -> usize {
    let db = Analysis::for_project(Path::new("/load-memo-chain"));
    let source = format!(
        "def f(x):\n    return User.query{}\n",
        ".filter(x)".repeat(n)
    );
    let id = db.set_file_text(Path::new("/load-memo-chain/main.py"), &source);
    let file = db.file(id).unwrap();
    let inf = Inferer::new(&db);
    inf.type_of_expr(&file, &returned(&file));
    inf.member_and_call_computations.get()
}

#[test]
fn method_chains_on_unknown_receivers_infer_each_link_once() {
    // `User` is unresolved, so every link is a call on an unknown receiver
    // that several rules inspect; each used to infer the whole chain below
    // it again (eight links took minutes in `rustpy inspect`).
    let mut short = 0;
    let mut long = 0;
    within_load_budget(20_000, || short = chain_computations(12));
    within_load_budget(20_000, || long = chain_computations(24));
    assert!(long <= 4 * 24 + 8, "{long} attribute and call inferences");
    assert!(long <= 3 * short, "{short} then {long}");
}
