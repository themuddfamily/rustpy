use rustpy_core::TextSize;
use rustpy_semantic::{
    completion2::{expected_type, smart_complete},
    db::Analysis,
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn finite_callable_headers_cover_zero_two_and_bound_thirty_two_parameters() {
    for count in [0, 2, 32] {
        let db = Analysis::for_project(Path::new("/smart-finite-lambda"));
        let params = vec!["str"; count].join(",");
        let source = format!(
            "from typing import Callable\ndef use(callback:Callable[[{params}],int])->None:pass\nuse()\n"
        );
        let id = db.set_file_text(Path::new("/smart-finite-lambda/m.py"), &source);
        let file = db.file(id).unwrap();
        let offset = TextSize::new((source.rfind("use()").unwrap() + 4) as u32);
        let (expected, _) = expected_type(&db, &file, offset).unwrap();
        assert_eq!(expected.signature().unwrap().params.len(), count);
        let item = smart_complete(&db, &file, offset)
            .into_iter()
            .find(|item| item.insert.starts_with("lambda") && item.insert.contains(':'))
            .expect("finite editable lambda");
        let caret = item.caret_offset.unwrap();
        assert_eq!(caret, item.insert.len());
        assert!(item.insert.ends_with(": "));
        let completed = format!("{}0\n", item.insert);
        let id = db.set_file_text(Path::new("/smart-finite-lambda/generated.py"), &completed);
        let generated = db.file(id).unwrap();
        assert!(generated.parse.errors().is_empty(), "{completed}");
        let lambda = generated
            .parse
            .syntax_node()
            .descendants()
            .find_map(ast::LambdaExpr::cast)
            .unwrap();
        let actual = lambda
            .params()
            .map(|p| p.plain_params().count())
            .unwrap_or(0);
        assert_eq!(actual, count);
    }
}
#[test]
fn unbounded_callable_and_over_budget_headers_do_not_fabricate_parameter_shapes() {
    for args in ["...".to_owned(), format!("[{}]", vec!["str"; 33].join(","))] {
        let db = Analysis::for_project(Path::new("/smart-finite-lambda"));
        let source = format!(
            "from typing import Callable\ndef use(callback:Callable[{args},int])->None:pass\nuse()\n"
        );
        let id = db.set_file_text(Path::new("/smart-finite-lambda/m.py"), &source);
        let file = db.file(id).unwrap();
        let offset = TextSize::new((source.rfind("use()").unwrap() + 4) as u32);
        assert!(!smart_complete(&db, &file, offset)
            .iter()
            .any(|item| item.insert.starts_with("lambda") && item.insert.contains(':')));
    }
}
