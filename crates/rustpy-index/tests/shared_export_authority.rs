use rustpy_index::{extract, DunderAll};

#[test]
fn module_constants_and_list_alias_mutations_define_exports() {
    let source = "NAME = 'Public'\nBASE = [NAME]\n__all__: list[str] = BASE + ['_explicit']\nalias = __all__\nalias.append('Later')\n";
    assert_eq!(
        extract(source).dunder_all,
        Some(DunderAll {
            names: vec!["Public".into(), "_explicit".into(), "Later".into()],
            exact: true,
        })
    );
}

#[test]
fn invalid_list_tuple_concatenation_cannot_claim_exact_exports() {
    let all = extract("__all__ = ['Public'] + ('Other',)\n")
        .dunder_all
        .expect("an export assignment was present");
    assert!(!all.exact, "Python cannot concatenate a list and tuple");
}

#[test]
fn bare_decorator_callback_invalidates_previous_export_authority() {
    let source = "__all__ = ['Old']\ndef mutate(fn):\n    global __all__\n    __all__ = ['New']\n    return fn\n@mutate\ndef decorated():\n    pass\n";
    let all = extract(source).dunder_all.expect("exports were assigned");
    assert!(!all.exact, "a decorator runs during module loading");
}

#[test]
fn executing_class_body_callback_invalidates_previous_export_authority() {
    let source = "__all__ = ['Old']\ndef mutate():\n    global __all__\n    __all__ = ['New']\nclass C:\n    mutate()\n";
    let all = extract(source).dunder_all.expect("exports were assigned");
    assert!(!all.exact, "class bodies execute during module loading");
}
