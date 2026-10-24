use rustpy_analysis::inspections::{inspect, InspectionOptions};
use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::{collections::BTreeSet, path::Path};
fn check(source: &str, producers: &[(&str, &[&str])], expected_ranges: &[&str]) {
    let db = Analysis::for_project(Path::new("/tuple-subscript-controls"));
    let id = db.set_file_text(Path::new("/tuple-subscript-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let inferer = Inferer::new(&db);
    for (needle, expected) in producers {
        assert_eq!(
            source.matches(needle).count(),
            1,
            "unique producer {needle}"
        );
        let offset = source.find(needle).unwrap();
        let expr = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::SubscriptExpr::cast)
            .find(|expr| usize::from(expr.text_range().start()) == offset)
            .unwrap();
        let ty = inferer.type_of_expr(&file, &ast::Expr::Subscript(expr));
        let values = match ty {
            Type::Union(values) => values,
            value => vec![value],
        };
        let actual: BTreeSet<_> = values.into_iter().map(|value| value.to_string()).collect();
        let expected: BTreeSet<_> = expected.iter().map(|value| value.to_string()).collect();
        assert_eq!(actual, expected, "actual producer {needle}: {source}");
    }
    let problems = inspect(&db, &file, &InspectionOptions::default());
    let actual: Vec<_> = problems
        .iter()
        .filter(|p| p.diagnostic.code == "PyTypeChecker")
        .map(|p| {
            &source[usize::from(p.diagnostic.range.start())..usize::from(p.diagnostic.range.end())]
        })
        .collect();
    assert_eq!(
        actual, expected_ranges,
        "exact diagnostic count/ranges: {problems:#?}\n{source}"
    );
}
#[test]
fn fixed_tuple_literal_indices_report_only_mismatching_elements() {
    let source="def need_int(value:int):pass\ndef need_str(value:str):pass\ndef use(value:tuple[int,str]):\n need_int(value[0])\n need_str(value[1])\n need_int(value[1])\n need_str(value[0])\n";
    check(source, &[], &["value[1]", "value[0]"]);
    // A separate spelling makes the producer locations unique without changing
    // either tuple identity or the argument consumer.
    let source="def need_int(value:int):pass\ndef need_str(value:str):pass\ndef use(value:tuple[int,str]):\n need_int(value[0])\n need_str(value[1])\n";
    check(
        source,
        &[("value[0]", &["int"]), ("value[1]", &["str"])],
        &[],
    );
}
#[test]
fn signed_literal_indices_use_python_tuple_position_and_type() {
    let source="def need_int(value:int):pass\ndef need_str(value:str):pass\ndef use(value:tuple[int,str]):\n need_str(value[-1])\n need_int(value[-2])\n need_int(value[+0])\n need_int(value[-1])\n need_str(value[-2])\n";
    check(
        source,
        &[("value[+0]", &["int"])],
        &["value[-1]", "value[-2]"],
    );
    let source="def need_int(value:int):pass\ndef need_str(value:str):pass\ndef use(value:tuple[int,str]):\n need_str(value[-1])\n need_int(value[-2])\n";
    check(
        source,
        &[("value[-1]", &["str"]), ("value[-2]", &["int"])],
        &[],
    );
}
#[test]
fn unproved_subscript_consumers_do_not_warn_from_inferred_union_or_hook() {
    let source="def need_int(value:int):pass\nclass Hook:\n def __getitem__(self,index:int)->str:return 'text'\ndef use(value:tuple[int,str],index:int,unknown,hook:Hook):\n need_int(value[9])\n need_int(value[-3])\n need_int(value[index])\n need_int(unknown[0])\n need_int(hook[0])\n";
    check(
        source,
        &[
            ("value[9]", &["Unknown"]),
            ("value[-3]", &["Unknown"]),
            ("value[index]", &["int", "str"]),
            ("unknown[0]", &["Unknown"]),
            ("hook[0]", &["str"]),
        ],
        &[],
    );
}
