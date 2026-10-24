use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn types(source: &str, operand: &str, expression: &str) -> (Type, Type) {
    let db = Analysis::for_project(Path::new("/operator-resource-controls"));
    let id = db.set_file_text(Path::new("/operator-resource-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let expression_at = |text: &str| {
        file.parse
            .syntax_node()
            .descendants()
            .filter_map(ast::Expr::cast)
            .filter(|node| node.syntax().text() == text)
            .last()
            .unwrap()
    };
    let infer = Inferer::new(&db);
    (
        infer.type_of_expr(&file, &expression_at(operand)),
        infer.type_of_expr(&file, &expression_at(expression)),
    )
}
fn union_source(count: usize) -> String {
    let mut source = String::new();
    for index in 0..count {
        source.push_str(&format!(
            "class C{index}:\n def __add__(self,other:object)->int:...\n"
        ));
    }
    let union = (0..count)
        .map(|index| format!("C{index}"))
        .collect::<Vec<_>>()
        .join("|");
    source.push_str(&format!(
        "def f(left:{union},right:{union}):\n return left+right\n"
    ));
    source
}
#[test]
fn union_width_and_cartesian_work_refuse_whole_results_without_partial_known_prefixes() {
    for count in [2, 32, 65] {
        let source = union_source(count);
        let (operand, actual) = types(&source, "left", "left+right");
        let Type::Union(values) = operand else {
            panic!("actual union premise required");
        };
        assert_eq!(values.len(), count);
        if count == 2 {
            assert_eq!(actual.to_string(), "int");
        } else {
            assert!(
                actual.is_unknown(),
                "width/work refusal must be whole: {actual}"
            );
        }
    }
}
#[test]
fn chain_length_and_raw_operator_text_are_bounded_before_result_join() {
    for pairs in [2, 33] {
        let expression = (0..=pairs).map(|_| "value").collect::<Vec<_>>().join(" < ");
        let source =
            format!("class C:\n def __lt__(self,other:object)->str:...\nvalue=C()\n{expression}\n");
        let (_, actual) = types(&source, "value", &expression);
        if pairs == 2 {
            assert_eq!(actual.to_string(), "str");
        } else {
            assert!(actual.is_unknown(), "oversized whole chain must decline");
        }
    }
    let expression = format!("value not{}in [value]", " ".repeat(1024));
    let source = format!("value=1\n{expression}\n");
    let (_, actual) = types(&source, "value", &expression);
    assert!(
        actual.is_unknown(),
        "operator text budget must precede normalization"
    );
}
#[test]
fn oversized_declared_result_union_does_not_escape_the_result_budget() {
    let mut source = String::new();
    for index in 0..65 {
        source.push_str(&format!("class R{index}:...\n"));
    }
    let union = (0..65)
        .map(|index| format!("R{index}"))
        .collect::<Vec<_>>()
        .join("|");
    source.push_str(&format!("class C:\n def __add__(self,other:object)->{union}:...\nvalue=C()\nvalue.__add__(0)\nvalue+0\n"));
    let (slot, actual) = types(&source, "value.__add__(0)", "value+0");
    let Type::Union(values) = slot else {
        panic!("actual declared result union required");
    };
    assert_eq!(values.len(), 65);
    assert!(actual.is_unknown());
}

#[test]
fn owned_generic_input_is_admitted_before_widening_or_cloning() {
    for width in [2, 65] {
        let elements = vec!["int"; width].join(",");
        let source = format!("from typing import Generic,TypeVar\nT=TypeVar('T')\nclass Box(Generic[T]):\n def __add__(self,other:object)->int:...\ndef f(value:Box[tuple[{elements}]]):\n value.__add__(0)\n return value+0\n");
        let (input, actual) = types(&source, "value", "value+0");
        let Type::Instance(class, arguments) = input else {
            panic!("actual generic input required");
        };
        assert_eq!(class.name, "Box");
        let Type::Tuple(values) = &arguments[0] else {
            panic!("actual owned tuple argument required");
        };
        assert_eq!(values.len(), width);
        let (_, slot) = types(&source, "value", "value.__add__(0)");
        assert_eq!(slot.to_string(), "int");
        if width == 2 {
            assert_eq!(actual.to_string(), "int");
        } else {
            assert!(
                actual.is_unknown(),
                "oversized owned input must decline before copy"
            );
        }
    }
}

#[test]
fn owned_generic_result_is_admitted_before_union_equality_or_join() {
    for width in [2, 65] {
        let elements = vec!["int"; width].join(",");
        let source = format!("from typing import Generic,TypeVar\nT=TypeVar('T')\nclass Box(Generic[T]):...\nclass C:\n def __add__(self,other:object)->Box[tuple[{elements}]]:...\nvalue=C()\nvalue.__add__(0)\nvalue+0\n");
        let (slot, actual) = types(&source, "value.__add__(0)", "value+0");
        let Type::Instance(class, arguments) = slot else {
            panic!("actual declared generic result required");
        };
        assert_eq!(class.name, "Box");
        let Type::Tuple(values) = &arguments[0] else {
            panic!("actual result tuple argument required");
        };
        assert_eq!(values.len(), width);
        if width == 2 {
            let Type::Instance(actual_class, actual_arguments) = actual else {
                panic!("operator generic result required");
            };
            assert_eq!(actual_class.name, "Box");
            let Type::Tuple(actual_values) = &actual_arguments[0] else {
                panic!("operator tuple argument required");
            };
            assert_eq!(actual_values.len(), 2);
            assert!(actual_values.iter().all(|value| value.to_string() == "int"));
        } else {
            assert!(
                actual.is_unknown(),
                "oversized owned result must decline before join"
            );
        }
    }
}
