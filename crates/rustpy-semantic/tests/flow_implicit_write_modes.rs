//! STORE/DELETE/annotation are not descriptor or item READ operations.
use rustpy_semantic::{
    db::Analysis,
    flow,
    infer::Inferer,
    types::{LiteralValue, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, member: &str, arguments: &[Type], never: bool, terminal: bool) {
    let db = Analysis::for_project(Path::new("/implicit-write-audit"));
    let id = db.set_file_text(Path::new("/implicit-write-audit/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let references: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|r| r.name == "value" && !r.is_write)
        .collect();
    assert_eq!(references.len(), 1, "actual unique operation receiver");
    let receiver = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|name| name.text_range() == references[0].range)
        .unwrap();
    let inf = Inferer::new(&db);
    let ty = inf.type_of_expr(&file, &ast::Expr::Name(receiver));
    let method = inf
        .lookup_member(&ty, member)
        .expect("actual maintained selected slot");
    let result = if member == "field" {
        method
    } else {
        inf.call_result(&method, arguments, &[])
    };
    assert_eq!(
        matches!(result, Type::Never),
        never,
        "maintained slot/read-type premise BEFORE Flow: {result}; {source}"
    );
    assert!(
        !result.is_unknown(),
        "normal slot must have a concrete declaration"
    );
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert_eq!(
        !info.refs.contains_key(&reference.range.start()),
        terminal,
        "completion in actual access mode: {source}"
    );
}
#[test]
fn declared_never_setters_and_deleters_stop_after_actual_invocation() {
    let index = Type::Literal(LiteralValue::Int(0));
    let item = Type::Literal(LiteralValue::Int(1));
    let name = Type::Literal(LiteralValue::Str("field".into()));
    for (slot, parameters, operation, arguments) in [
        (
            "__setitem__",
            "self,key:int,item:object",
            "value[0]=1",
            vec![index.clone(), item.clone()],
        ),
        ("__delitem__", "self,key:int", "del value[0]", vec![index]),
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
            let source=format!("from typing import Never\nclass C:\n    def {slot}({parameters})->{ret}:{body}\ndef f():\n    marker=1\n    value=C()\n    {operation}\n    return marker\n");
            check(&source, slot, &arguments, terminal, terminal);
        }
    }
}
#[test]
fn never_item_reader_is_not_invoked_by_store_delete_or_bare_annotation() {
    let source="from typing import Never\nclass C:\n    def __getitem__(self,key:int)->Never:raise RuntimeError\n    def __setitem__(self,key:int,item:object)->None:return None\n    def __delitem__(self,key:int)->None:return None\n";
    for operation in ["value[0]=1", "del value[0]", "value[0]:int"] {
        let program = format!(
            "{source}def f():\n    marker=1\n    value=C()\n    {operation}\n    return marker\n"
        );
        // An item READ would be Never, but this operation is not a READ.
        check(
            &program,
            "__getitem__",
            &[Type::Literal(LiteralValue::Int(0))],
            true,
            false,
        );
    }
}
#[test]
fn never_property_reader_is_not_invoked_by_setter_deleter_or_annotation() {
    let source="from typing import Never\nclass C:\n    @property\n    def field(self)->Never:raise RuntimeError\n    @field.setter\n    def field(self,item:object)->None:return None\n    @field.deleter\n    def field(self)->None:return None\n";
    for operation in ["value.field=1", "del value.field", "value.field:object"] {
        let program = format!(
            "{source}def f():\n    marker=1\n    value=C()\n    {operation}\n    return marker\n"
        );
        // The maintained property READ type is Never; a WRITE is not its getter.
        check(&program, "field", &[], true, false);
    }
}
#[test]
fn assignment_rhs_rebinding_precedes_target_receiver_evaluation() {
    let source="from typing import Never\nclass Stop:\n    def __setitem__(self,key:int,item:object)->Never:raise RuntimeError\nclass Normal:\n    def __setitem__(self,key:int,item:object)->None:return None\n";
    for (old, new, terminal) in [("Stop", "Normal", false), ("Normal", "Stop", true)] {
        let program=format!("{source}def f():\n    marker=1\n    value={old}()\n    value[0]=(value:={new}())\n    return marker\n");
        check(
            &program,
            "__setitem__",
            &[Type::Literal(LiteralValue::Int(0)), Type::Any],
            terminal,
            terminal,
        );
    }
}
