//! Runtime reaching definitions must distinguish annotation declarations from stores.
use rustpy_semantic::{db::Analysis, flow, scope::BindingKind};
use std::path::Path;
fn reaches(source: &str, name: &str) -> (Vec<BindingKind>, bool) {
    let analysis = Analysis::for_project(Path::new("/symbol_annotation_fixture"));
    let id = analysis.set_file_text(Path::new("/symbol_annotation_fixture/module.py"), source);
    let file = analysis.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|reference| reference.name == name && !reference.is_write)
        .unwrap();
    let scope = flow::flow_scope(&file, reference.scope).unwrap();
    let info = flow::flow_of(&file, scope);
    let reached = info
        .refs
        .get(&reference.range.start())
        .expect("actual load is recorded");
    (
        reached
            .iter()
            .filter_map(|reach| {
                reach
                    .binding
                    .map(|binding| file.scopes.binding(binding).kind.clone())
            })
            .collect(),
        file.scopes
            .scope(scope)
            .bindings
            .get(name)
            .unwrap()
            .iter()
            .any(|binding| file.scopes.binding(*binding).kind == BindingKind::Annotation),
    )
}
#[test]
fn bare_annotation_retains_prior_runtime_value_in_module_class_and_function() {
    for source in [
        "value=1\nvalue:int\nvalue\n",
        "class C:\n    value=1\n    value:int\n    value\n",
        "def f():\n    value=1\n    value:int\n    return value\n",
    ] {
        let (bindings, annotation) = reaches(source, "value");
        assert!(annotation, "declaration must remain in symbol table");
        assert_eq!(bindings, vec![BindingKind::Assignment], "{source}");
    }
}
#[test]
fn bare_local_annotation_does_not_initialize_but_annotated_store_does() {
    let (bindings, annotation) = reaches("def f():\n    value:int\n    return value\n", "value");
    assert!(annotation);
    assert!(bindings.is_empty(), "annotation-only local is unbound");
    let (bindings, _) = reaches("def f():\n    value:int=1\n    return value\n", "value");
    assert_eq!(bindings, vec![BindingKind::Assignment]);
}

#[test]
fn parenthesized_annotation_names_are_not_loaded_or_initialized() {
    for (source, expected) in [
        (
            "value=1\n(value):int\nvalue\n",
            vec![BindingKind::Assignment],
        ),
        (
            "class C:\n    value=1\n    (value):int\n    value\n",
            vec![BindingKind::Assignment],
        ),
        (
            "def f():\n    value=1\n    (value):int\n    return value\n",
            vec![BindingKind::Assignment],
        ),
        ("def f():\n    (value):int\n    return value\n", vec![]),
    ] {
        let analysis = Analysis::for_project(Path::new("/parenthesized_annotation_fixture"));
        let id = analysis.set_file_text(
            Path::new("/parenthesized_annotation_fixture/module.py"),
            source,
        );
        let file = analysis.file(id).unwrap();
        let references: Vec<_> = file
            .scopes
            .references
            .iter()
            .filter(|reference| reference.name == "value" && !reference.is_write)
            .collect();
        assert_eq!(
            references.len(),
            1,
            "only the final runtime load is a reference: {source}"
        );
        let scope = flow::flow_scope(&file, references[0].scope).unwrap();
        let info = flow::flow_of(&file, scope);
        let annotation_target =
            rustpy_core::TextSize::from((source.find("(value)").unwrap() + 1) as u32);
        assert!(
            !info.refs.contains_key(&annotation_target),
            "annotation target is not a runtime load"
        );
        assert!(
            !info.targets.contains_key(&annotation_target),
            "annotation target is not a runtime store"
        );
        let (bindings, annotation) = reaches(source, "value");
        assert!(annotation);
        assert_eq!(bindings, expected, "{source}");
    }
}
