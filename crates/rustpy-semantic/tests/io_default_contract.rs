use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn open_default_is_a_concrete_canonical_protocol_and_enter_preserves_it() {
    let db = Analysis::for_project(Path::new("/io-default"));
    let id = db.set_file_text(Path::new("/io-default/m.py"), "open('f')\n");
    let file = db.file(id).unwrap();
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::CallExpr::cast)
        .unwrap();
    let inf = Inferer::new(&db);
    let ty = inf.type_of_expr(&file, &ast::Expr::Call(call));
    let Type::Instance(wrapper, arguments) = &ty else {
        panic!("instance: {ty}")
    };
    assert_eq!(wrapper.name, "TextIOWrapper");
    assert_eq!(arguments.len(), 1);
    let Type::Instance(buffer, _) = &arguments[0] else {
        panic!("concrete protocol default: {}", arguments[0])
    };
    assert_eq!(buffer.name, "_WrappedBuffer");
    assert!(inf.class_info(buffer).unwrap().is_protocol);
    let wrapper_file = db.file(wrapper.file).unwrap();
    let buffer_file = db.file(buffer.file).unwrap();
    assert!(wrapper_file.is_stub && buffer_file.is_stub);
    assert_eq!(wrapper_file.path, Path::new("<typeshed>/_io.pyi"));
    assert_eq!(buffer_file.path, wrapper_file.path);
    assert_eq!(wrapper_file.module_name.as_deref(), Some("_io"));
    let scope = wrapper_file.scopes.module_scope();
    let ids = wrapper_file.scopes.bindings_in(scope, "_BufferT_co");
    let declared = inf.type_of_binding(&wrapper_file, *ids.last().unwrap());
    let Type::DeclaredTypeVar(definition) = declared else {
        panic!("declared default metadata")
    };
    assert_eq!(definition.default.as_deref(), Some(&arguments[0]));
    assert_eq!(definition.bound.as_deref(), Some(&arguments[0]));
    let enter = inf.lookup_member(&ty, "__enter__").unwrap();
    assert_eq!(inf.call_result(&enter, &[], &[]), ty);
    assert_eq!(ty.to_string(), "TextIOWrapper[_WrappedBuffer]");
}

#[test]
fn binary_open_default_is_a_concrete_canonical_protocol_and_enter_preserves_it() {
    let db = Analysis::for_project(Path::new("/io-default"));
    let id = db.set_file_text(Path::new("/io-default/m.py"), "open('f', 'rb')\n");
    let file = db.file(id).unwrap();
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::CallExpr::cast)
        .unwrap();
    let inf = Inferer::new(&db);
    let ty = inf.type_of_expr(&file, &ast::Expr::Call(call));
    let Type::Instance(wrapper, arguments) = &ty else {
        panic!("instance: {ty}")
    };
    assert_eq!(wrapper.name, "BufferedReader");
    assert_eq!(arguments.len(), 1);
    let Type::Instance(buffer, _) = &arguments[0] else {
        panic!("concrete protocol default: {}", arguments[0])
    };
    assert_eq!(buffer.name, "_BufferedReaderStream");
    assert!(inf.class_info(buffer).unwrap().is_protocol);
    let wrapper_file = db.file(wrapper.file).unwrap();
    let buffer_file = db.file(buffer.file).unwrap();
    assert!(wrapper_file.is_stub && buffer_file.is_stub);
    assert_eq!(wrapper_file.path, Path::new("<typeshed>/_io.pyi"));
    assert_eq!(buffer_file.path, wrapper_file.path);
    assert_eq!(wrapper_file.module_name.as_deref(), Some("_io"));
    let scope = wrapper_file.scopes.module_scope();
    let ids = wrapper_file
        .scopes
        .bindings_in(scope, "_BufferedReaderStreamT");
    let declared = inf.type_of_binding(&wrapper_file, *ids.last().unwrap());
    let Type::DeclaredTypeVar(definition) = declared else {
        panic!("declared default metadata")
    };
    assert_eq!(definition.default.as_deref(), Some(&arguments[0]));
    assert_eq!(definition.bound.as_deref(), Some(&arguments[0]));
    let enter = inf.lookup_member(&ty, "__enter__").unwrap();
    assert_eq!(inf.call_result(&enter, &[], &[]), ty);
    assert_eq!(ty.to_string(), "BufferedReader[_BufferedReaderStream]");
}
