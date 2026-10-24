use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::{
    db::Analysis,
    flow,
    infer::Inferer,
    types::{LiteralValue, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, slot: &str, args: &[Type], slot_never: bool, terminal: bool) {
    let db = Analysis::for_project(Path::new("/implicit-inspection-consumers"));
    let id = db.set_file_text(Path::new("/implicit-inspection-consumers/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "value" && !r.is_write)
        .unwrap();
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|n| n.text_range() == reference.range)
        .unwrap();
    let inf = Inferer::new(&db);
    let receiver = inf.type_of_expr(&file, &ast::Expr::Name(name));
    let member = inf
        .lookup_member(&receiver, slot)
        .expect("real operation slot");
    let result = if slot == "field" {
        member
    } else {
        inf.call_result(&member, args, &[])
    };
    assert_eq!(
        matches!(result, Type::Never),
        slot_never,
        "actual slot producer BEFORE Flow: {result}; {source}"
    );
    assert!(!result.is_unknown());
    let read = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "missing" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, read.scope).unwrap());
    assert_eq!(
        !info.refs.contains_key(&read.range.start()),
        terminal,
        "semantic completion BEFORE inspection: {source}"
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
    let start = source.rfind("print(missing)").unwrap() + "print(".len();
    let expected = if terminal {
        vec![]
    } else {
        vec![("PyUnboundLocalVariable".to_owned(), start, start + 7)]
    };
    assert_eq!(
        actual, expected,
        "actual inspection IDs/ranges AFTER producer+Flow; terminal_points={:?}; {source}",
        info.terminal_calls
    );
}
#[test]
fn implicit_read_operator_consumer_families() {
    for (slot, params, operation, args) in [
        ("__neg__", "self", "-value", vec![]),
        (
            "__getitem__",
            "self,index:int",
            "value[0]",
            vec![Type::Literal(LiteralValue::Int(0))],
        ),
    ] {
        for (ret, body, terminal) in [
            ("Never", "raise RuntimeError", true),
            ("int", "return 1", false),
        ] {
            let source=format!("from typing import Never\nclass C:\n    def {slot}({params})->{ret}:{body}\ndef f():\n    value=C()\n    {operation}\n    print(missing)\n    missing=1\n");
            check(&source, slot, &args, terminal, terminal);
        }
    }
}
#[test]
fn implicit_truth_consumer_families() {
    for (slot, normal_ret, normal_body) in [
        ("__bool__", "bool", "return False"),
        ("__len__", "int", "return 0"),
    ] {
        for (ret, body, terminal) in [
            ("Never", "raise RuntimeError", true),
            (normal_ret, normal_body, false),
        ] {
            for operation in [
                "if value:\n        pass",
                "while value:\n        break",
                "not value",
            ] {
                let source=format!("from typing import Never\nclass C:\n    def {slot}(self)->{ret}:{body}\ndef f():\n    value=C()\n    {operation}\n    print(missing)\n    missing=1\n");
                check(&source, slot, &[], terminal, terminal);
            }
        }
    }
}
#[test]
fn implicit_store_delete_consumer_families() {
    let i = Type::Literal(LiteralValue::Int(0));
    let item = Type::Literal(LiteralValue::Int(1));
    let name = Type::Literal(LiteralValue::Str("field".into()));
    for (slot, params, operation, args) in [
        (
            "__setitem__",
            "self,index:int,item:object",
            "value[0]=1",
            vec![i.clone(), item.clone()],
        ),
        ("__delitem__", "self,index:int", "del value[0]", vec![i]),
        (
            "__setattr__",
            "self,name:str,item:object",
            "value.field=1",
            vec![name.clone(), item],
        ),
        (
            "__delattr__",
            "self,name:str",
            "del value.field",
            vec![name],
        ),
    ] {
        for (ret, body, terminal) in [
            ("Never", "raise RuntimeError", true),
            ("None", "return None", false),
        ] {
            let source=format!("from typing import Never\nclass C:\n    def {slot}({params})->{ret}:{body}\ndef f():\n    value=C()\n    {operation}\n    print(missing)\n    missing=1\n");
            check(&source, slot, &args, terminal, terminal);
        }
    }
}
#[test]
fn implicit_inplace_and_iteration_acquisition_consumers() {
    for (slot, params, operation, args) in [
        ("__iadd__", "self,other", "value+=value", vec![Type::Any]),
        (
            "__iter__",
            "self",
            "for item in value:\n        pass",
            vec![],
        ),
    ] {
        for (ret, body, terminal) in [
            ("Never", "raise RuntimeError", true),
            ("'C'", "return self", false),
        ] {
            let next = if slot == "__iter__" {
                "    def __next__(self)->int:raise StopIteration\n"
            } else {
                ""
            };
            let source=format!("from typing import Never\nclass C:\n    def {slot}({params})->{ret}:{body}\n{next}def f():\n    value=C()\n    {operation}\n    print(missing)\n    missing=1\n");
            check(&source, slot, &args, terminal, terminal);
        }
    }
}
#[test]
fn read_never_is_not_store_delete_annotation_or_exhaustion() {
    let prefix="from typing import Never\nclass C:\n    def __getitem__(self,index:int)->Never:raise IndexError\n    def __setitem__(self,index:int,item:object)->None:return None\n    def __delitem__(self,index:int)->None:return None\n";
    for operation in [
        "value[0]=1",
        "del value[0]",
        "value[0]:int",
        "for item in value:\n        pass",
    ] {
        let source = format!(
            "{prefix}def f():\n    value=C()\n    {operation}\n    print(missing)\n    missing=1\n"
        );
        check(
            &source,
            "__getitem__",
            &[Type::Literal(LiteralValue::Int(0))],
            true,
            false,
        );
    }
}

#[test]
fn returned_callable_contract_is_not_an_implicit_invocation() {
    for (source,text) in [
        ("from typing import Never\ndef halt()->Never:raise RuntimeError\ndef f():\n    value=halt\n    value\n    print(missing)\n    missing=1\n","value"),
        ("from typing import Never,Callable\ndef halt()->Never:raise RuntimeError\nclass C:\n    @property\n    def callback(self)->Callable[[],Never]:return halt\ndef f():\n    value=C()\n    value.callback\n    print(missing)\n    missing=1\n","value.callback"),
    ] {
        let db=Analysis::for_project(Path::new("/noninvoked-callable-guard"));
        let id=db.set_file_text(Path::new("/noninvoked-callable-guard/m.py"),source);
        let file=db.file(id).unwrap();assert!(file.parse.errors().is_empty());
        let expression=file.parse.syntax_node().descendants().filter_map(ast::Expr::cast)
            .filter(|expression|expression.syntax().text()==text)
            .find(|expression| file.scopes.references.iter().any(|reference|!reference.is_write&&reference.range==expression.text_range()) || matches!(expression,ast::Expr::Attribute(_))).unwrap();
        let ty=Inferer::new(&db).type_of_expr(&file,&expression);
        assert!(matches!(ty,Type::Function(ref signature)|Type::Callable(ref signature)|Type::BoundMethod(ref signature,_) if matches!(signature.ret,Type::Never)),"actual returned callable promise: {ty}");
        let read=file.scopes.references.iter().find(|r|r.name=="missing"&&!r.is_write).unwrap();
        let info=flow::flow_of_with_db(&db,&file,flow::flow_scope(&file,read.scope).unwrap());
        assert!(info.refs.contains_key(&read.range.start()),"merely loading callable completes normally");
        let problems=inspections::inspect(&db,&file,&InspectionOptions::default());
        let ranges:Vec<_>=problems.iter().filter(|problem|problem.diagnostic.code=="PyUnboundLocalVariable").map(|problem|(usize::from(problem.diagnostic.range.start()),usize::from(problem.diagnostic.range.end()))).collect();
        let start=source.rfind("print(missing)").unwrap()+"print(".len();assert_eq!(ranges,[(start,start+7)]);
    }
}
