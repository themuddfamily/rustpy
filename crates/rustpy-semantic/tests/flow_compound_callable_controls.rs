//! Source-only proposals: premise failures must be reported separately from Flow gaps.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, call_text: &str, never: Option<bool>, terminal: bool) {
    let db = Analysis::for_project(Path::new("/remaining-producer"));
    let id = db.set_file_text(Path::new("/remaining-producer/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == call_text)
        .unwrap();
    if let Some(expected) = never {
        let ty = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(call));
        assert_eq!(
            matches!(ty, Type::Never),
            expected,
            "Inferer premise BEFORE Flow: {ty}; {source}"
        );
    }
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
        "Flow completion: {source}"
    );
}

#[test]
fn named_value_capture_precedes_argument_rebinding() {
    check("from typing import Never\ndef stop(value)->Never:raise RuntimeError\ndef normal(value)->int:return 1\ndef f():\n    marker=1\n    (callback:=stop)((callback:=normal))\n    return marker\n","(callback:=stop)((callback:=normal))",Some(true),true);
}
#[test]
fn shadowed_super_cannot_supply_intrinsic_authority() {
    check("from typing import Never\nclass Base:\n    def stop(self)->Never:raise RuntimeError\nclass Replacement:\n    def stop(self)->int:return 1\ndef super():return Replacement()\nclass Child(Base):\n    def run(self):\n        marker=1\n        super().stop()\n        return marker\n","super().stop()",Some(false),false);
}
#[test]
fn replacement_field_store_retires_nested_receiver_authority() {
    check("from typing import Never\nclass Inner:\n    def stop(self)->Never:raise RuntimeError\nclass Replacement:\n    def stop(self)->int:return 1\nclass Holder:\n    def __init__(self):self.child=Inner()\ndef f(holder:Holder):\n    marker=1\n    holder.child=Replacement()\n    holder.child.stop()\n    return marker\n","holder.child.stop()",None,false);
}
#[test]
fn property_receiver_is_not_plain_initialized_storage() {
    check("from typing import Never\nclass Inner:\n    def stop(self)->Never:raise RuntimeError\nclass Holder:\n    @property\n    def child(self)->Inner:return Inner()\ndef f(holder:Holder):\n    marker=1\n    holder.child.stop()\n    return marker\n","holder.child.stop()",Some(true),false);
}

#[test]
fn setter_interception_cannot_certify_initialized_field() {
    check("from typing import Never\nclass Inner:\n    def stop(self)->Never:raise RuntimeError\nclass Replacement:\n    def stop(self)->int:return 1\nclass Holder:\n    def __init__(self):self.child=Inner()\n    def __setattr__(self,name,value):object.__setattr__(self,name,Replacement())\ndef f(holder:Holder):\n    marker=1\n    holder.child.stop()\n    return marker\n","holder.child.stop()",None,false);
}
#[test]
fn inner_initializer_class_replacement_retires_receiver_proof() {
    check("from typing import Never\nclass Replacement:\n    def stop(self)->int:return 1\nclass Inner:\n    def __init__(self):object.__setattr__(self,'__class__',Replacement)\n    def stop(self)->Never:raise RuntimeError\nclass Holder:\n    def __init__(self):self.child=Inner()\ndef f(holder:Holder):\n    marker=1\n    holder.child.stop()\n    return marker\n","holder.child.stop()",Some(true),false);
}
