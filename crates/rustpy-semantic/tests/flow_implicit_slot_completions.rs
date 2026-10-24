//! Finite implicit slots: actual maintained producer before completion oracle.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, scope::BindingId, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::{path::Path, sync::Arc};
fn setup(source: &str) -> (Analysis, Arc<rustpy_semantic::db::FileInfo>) {
    let db = Analysis::for_project(Path::new("/implicit-slot-audit"));
    let id = db.set_file_text(Path::new("/implicit-slot-audit/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    (db, file)
}
fn completion(db: &Analysis, file: &Arc<rustpy_semantic::db::FileInfo>, terminal: bool) {
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(db, file, flow::flow_scope(file, reference.scope).unwrap());
    assert_eq!(
        !info.refs.contains_key(&reference.range.start()),
        terminal,
        "{}",
        file.text
    );
}
fn expression(source: &str, text: &str, never: Option<bool>, terminal: bool) {
    let (db, file) = setup(source);
    let selected: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|expression| expression.syntax().text() == text)
        .collect();
    assert_eq!(selected.len(), 1, "unique operation {text}");
    if let Some(never) = never {
        let ty = Inferer::new(&db).type_of_expr(&file, &selected[0]);
        assert_eq!(
            matches!(ty, Type::Never),
            never,
            "actual producer BEFORE Flow: {ty}; {source}"
        );
        assert!(
            !ty.is_unknown(),
            "a returning premise cannot be Unknown: {source}"
        );
    }
    completion(&db, &file, terminal);
}
#[test]
fn unary_declared_never_slots_and_returning_controls() {
    for (operator, slot) in [("-", "__neg__"), ("+", "__pos__"), ("~", "__invert__")] {
        for (ret, body, terminal) in [
            ("Never", "raise RuntimeError", true),
            ("int", "return 1", false),
        ] {
            let source=format!("from typing import Never\nclass C:\n    def {slot}(self)->{ret}:{body}\ndef f():\n    marker=1\n    value=C()\n    {operator}value\n    return marker\n");
            expression(
                &source,
                &format!("{operator}value"),
                Some(terminal),
                terminal,
            );
        }
    }
}
#[test]
fn unary_operand_rebinding_and_member_mutation_remain_observable() {
    let prefix="from typing import Never\nclass Stop:\n    def __neg__(self)->Never:raise RuntimeError\nclass Normal:\n    def __neg__(self)->int:return 1\n";
    expression(&format!("{prefix}def f():\n    marker=1\n    value=Stop()\n    -(value:=Normal())\n    return marker\n"),"-(value:=Normal())",Some(false),false);
    // A call with a concrete Normal result executes before unary dispatch.
    expression(&format!("{prefix}def normalize(value:Stop)->Normal:return Normal()\ndef f():\n    marker=1\n    value=Stop()\n    -normalize(value)\n    return marker\n"),"-normalize(value)",Some(false),false);
    // No static Never premise is claimed for an explicitly replaced slot.
    expression(&format!("{prefix}def normal(self)->int:return 1\ndef f():\n    marker=1\n    value=Stop()\n    Stop.__neg__=normal\n    -value\n    return marker\n"),"-value",None,false);
}
fn augmented(source: &str, never: bool) {
    let (db, file) = setup(source);
    let statements: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::AugAssignStmt::cast)
        .collect();
    assert_eq!(statements.len(), 1, "unique augmented operation");
    let ids: Vec<_> = file
        .scopes
        .bindings
        .iter()
        .enumerate()
        .filter(|(_, binding)| {
            binding.node_range == statements[0].text_range() && binding.name == "value"
        })
        .map(|(index, _)| BindingId(index as u32))
        .collect();
    assert_eq!(ids.len(), 1, "actual augmented definition");
    let ty = Inferer::new(&db).type_of_binding(&file, ids[0]);
    assert_eq!(
        matches!(ty, Type::Never),
        never,
        "actual augmented producer BEFORE Flow: {ty}; {source}"
    );
    assert!(
        !ty.is_unknown(),
        "returning augmented producer cannot be Unknown"
    );
    completion(&db, &file, never);
}
#[test]
fn augmented_slots_preserve_never_and_the_original_target_capture() {
    for (operator, slot) in [("+=", "__iadd__"), ("*=", "__imul__"), ("|=", "__ior__")] {
        for (ret, body, terminal) in [
            ("Never", "raise RuntimeError", true),
            ("'C'", "return self", false),
        ] {
            let source=format!("from typing import Never\nclass C:\n    def {slot}(self,other)->{ret}:{body}\ndef f():\n    marker=1\n    value=C()\n    value {operator} value\n    return marker\n");
            augmented(&source, terminal);
        }
    }
    let prefix="from typing import Never\nclass Stop:\n    def __iadd__(self,other)->Never:raise RuntimeError\nclass Normal:\n    def __iadd__(self,other)->'Normal':return self\n";
    // LOAD_FAST captures the old left value before the RHS rebinds its name.
    augmented(&format!("{prefix}def f():\n    marker=1\n    value=Stop()\n    value+=(value:=Normal())\n    return marker\n"),true);
    augmented(&format!("{prefix}def f():\n    marker=1\n    value=Normal()\n    value+=(value:=Stop())\n    return marker\n"),false);
}
#[test]
fn getitem_never_and_index_side_effects_preserve_receiver_capture() {
    let prefix="from typing import Never\nclass Stop:\n    def __getitem__(self,index:int)->Never:raise RuntimeError\nclass Normal:\n    def __getitem__(self,index:int)->int:return 1\n";
    expression(
        &format!(
            "{prefix}def f():\n    marker=1\n    value=Stop()\n    value[0]\n    return marker\n"
        ),
        "value[0]",
        Some(true),
        true,
    );
    expression(
        &format!(
            "{prefix}def f():\n    marker=1\n    value=Normal()\n    value[0]\n    return marker\n"
        ),
        "value[0]",
        Some(false),
        false,
    );
    expression(&format!("{prefix}def f():\n    marker=1\n    value=Stop()\n    value[(value:=Normal(),0)[1]]\n    return marker\n"),"value[(value:=Normal(),0)[1]]",Some(true),true);
    expression(&format!("{prefix}def f():\n    marker=1\n    value=Normal()\n    value[(value:=Stop(),0)[1]]\n    return marker\n"),"value[(value:=Stop(),0)[1]]",Some(false),false);
}
fn protocol(source: &str, name: &str, member: &str, never: bool, terminal: bool) {
    let (db, file) = setup(source);
    let references: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|reference| reference.name == name && !reference.is_write)
        .collect();
    assert_eq!(
        references.len(),
        1,
        "unique implicit protocol receiver load"
    );
    let selected = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|expression| expression.text_range() == references[0].range)
        .unwrap();
    let inf = Inferer::new(&db);
    let receiver = inf.type_of_expr(&file, &ast::Expr::Name(selected));
    let method = inf
        .lookup_member(&receiver, member)
        .expect("actual maintained slot");
    let arguments = if member == "__getitem__" {
        vec![Type::Literal(rustpy_semantic::types::LiteralValue::Int(0))]
    } else {
        Vec::new()
    };
    let ty = inf.call_result(&method, &arguments, &[]);
    assert_eq!(
        matches!(ty, Type::Never),
        never,
        "actual implicit-slot signature BEFORE Flow: {ty}; {source}"
    );
    assert!(!ty.is_unknown(), "normal slot producer cannot be Unknown");
    completion(&db, &file, terminal);
}
#[test]
fn truth_protocol_never_in_conditions_and_normal_controls() {
    for (member, normal_ret, normal_body) in [
        ("__bool__", "bool", "return False"),
        ("__len__", "int", "return 0"),
    ] {
        for (ret, body, terminal) in [
            ("Never", "raise RuntimeError", true),
            (normal_ret, normal_body, false),
        ] {
            for statement in [
                "if value:\n        pass",
                "while value:\n        break",
                "not value",
            ] {
                let source=format!("from typing import Never\nclass C:\n    def {member}(self)->{ret}:{body}\ndef f():\n    marker=1\n    value=C()\n    {statement}\n    return marker\n");
                protocol(&source, "value", member, terminal, terminal);
            }
        }
    }
}
#[test]
fn iterator_acquisition_never_is_distinct_from_exhaustion() {
    for (ret, body, terminal) in [
        ("Never", "raise RuntimeError", true),
        ("'C'", "return self", false),
    ] {
        let source=format!("from typing import Never\nclass C:\n    def __iter__(self)->{ret}:{body}\n    def __next__(self)->int:raise StopIteration\ndef f():\n    marker=1\n    items=C()\n    for value in items:\n        pass\n    return marker\n");
        protocol(&source, "items", "__iter__", terminal, terminal);
    }
    // A Never __next__ that raises StopIteration is normal FOR_ITER exhaustion;
    // it cannot be used to prove the entire loop terminal from its type alone.
    let source="from typing import Never\nclass C:\n    def __iter__(self)->'C':return self\n    def __next__(self)->Never:raise StopIteration\ndef f():\n    marker=1\n    items=C()\n    for value in items:\n        pass\n    return marker\n";
    protocol(source, "items", "__next__", true, false);
    // Legacy sequence iteration similarly consumes IndexError from __getitem__.
    let source = "from typing import Never\nclass C:\n    def __getitem__(self,index:int)->Never:raise IndexError\ndef f():\n    marker=1\n    items=C()\n    for value in items:\n        pass\n    return marker\n";
    protocol(source, "items", "__getitem__", true, false);
}
