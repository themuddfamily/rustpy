use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn query(source: &str, expression: &str, marker: &str) -> Type {
    let db = Analysis::for_project(Path::new("/functools-types"));
    let id = db.set_file_text(Path::new("/functools-types/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let needle = format!("{expression} #{marker}");
    assert_eq!(source.matches(&needle).count(), 1);
    let offset = source.find(&needle).unwrap();
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|node| {
            usize::from(node.text_range().start()) == offset
                && usize::from(node.text_range().len()) == expression.len()
        })
        .unwrap();
    Inferer::new(&db).type_of_expr(&file, &node)
}
#[test]
fn partial_preserves_bound_arguments_required_parameters_and_keyword_overrides() {
    let source="from functools import partial\ndef combine(left:int,right:str,*,enabled:bool)->tuple[int,str]:return left,right\np=partial(combine,1,right='saved')\np(enabled=True) #complete\np(right='new',enabled=False) #override\np() #required\np.args #args\np.func #func\n";
    assert_eq!(
        query(source, "p(enabled=True)", "complete").to_string(),
        "tuple[int, str]"
    );
    assert_eq!(
        query(source, "p(right='new',enabled=False)", "override").to_string(),
        "tuple[int, str]"
    );
    assert!(matches!(query(source, "p()", "required"), Type::Unknown));
    assert!(matches!(query(source,"p.args","args"),Type::Tuple(items) if items.len()==1));
    assert!(matches!(query(source, "p.func", "func"), Type::Function(_)));
    let generic="from functools import partial\ndef identity[T](value:T)->T:return value\np=partial(identity,value=1)\np() #saved\np(value='text') #override\n";
    assert_eq!(query(generic, "p()", "saved").to_string(), "int");
    assert_eq!(
        query(generic, "p(value='text')", "override").to_string(),
        "str"
    );
}
#[test]
fn partial_uses_original_overload_order_and_positional_only_keyword_destinations() {
    let source="from functools import partial\nfrom typing import overload,Literal\n@overload\ndef choose(value:Literal[0])->bytes:...\n@overload\ndef choose(value:str)->str:...\ndef choose(value):return value\np=partial(choose)\np(0) #integer\np('text') #text\ndef named(value:int,/,**keywords:str)->bytes:return b'yes'\nq=partial(named,1,value='forwarded')\nq(value='overridden') #posonly\n";
    assert_eq!(query(source, "p(0)", "integer").to_string(), "bytes");
    assert_eq!(query(source, "p('text')", "text").to_string(), "str");
    assert_eq!(
        query(source, "q(value='overridden')", "posonly").to_string(),
        "bytes"
    );
}
#[test]
fn wraps_and_update_wrapper_retain_actual_callable_and_expose_wrapped_metadata() {
    let source="from functools import wraps,update_wrapper\ndef original(value:int)->int:return value\n@wraps(original)\ndef wrapper(value:str)->str:return value\nwrapper('text') #actual\nwrapper.__wrapped__(1) #wrapped\ndef other(value:bytes)->bytes:return value\nupdated=update_wrapper(other,original)\nupdated(b'text') #updated\nupdated.__wrapped__(1) #metadata\n";
    assert_eq!(
        query(source, "wrapper('text')", "actual").to_string(),
        "str"
    );
    assert_eq!(
        query(source, "wrapper.__wrapped__(1)", "wrapped").to_string(),
        "int"
    );
    assert_eq!(
        query(source, "updated(b'text')", "updated").to_string(),
        "bytes"
    );
    assert_eq!(
        query(source, "updated.__wrapped__(1)", "metadata").to_string(),
        "int"
    );
}
#[test]
fn wraps_preserves_function_descriptor_binding_and_shadowed_helpers_keep_ordinary_types() {
    let source="from functools import wraps\ndef original(value:int)->int:return value\nclass Wrapper:\n    @wraps(original)\n    def method(self,value:str)->str:return value\nWrapper().method('text') #bound\ndef partial(value:int)->str:return 'ordinary'\npartial(1) #shadow\n";
    assert_eq!(
        query(source, "Wrapper().method('text')", "bound").to_string(),
        "str"
    );
    assert_eq!(query(source, "partial(1)", "shadow").to_string(), "str");
}
