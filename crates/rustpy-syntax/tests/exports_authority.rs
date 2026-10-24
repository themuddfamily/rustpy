use rustpy_syntax::{exports, parse};
fn exact(source: &str) -> bool {
    exports::evaluate(&parse(source).syntax_node())
        .unwrap()
        .exact
}
#[test]
fn annotation_store_and_loop_headers_retire_exact_authority() {
    for tail in [
        "__all__[0]: str = 'new'",
        "for __all__ in values:\n    pass",
        "for (__all__, other) in values:\n    pass",
    ] {
        assert!(!exact(&format!("__all__=['old']\n{tail}\n")), "{tail}");
    }
}
#[test]
fn unknown_calls_can_mutate_global_or_exposed_alias_authority() {
    for tail in [
        "mutate(values=__all__)",
        "helper.mutate(__all__)",
        "mutate(*__all__)",
        "mutate(**options)",
        "mutate()",
        "x = mutate()",
        "__all__.append(mutate())",
    ] {
        assert!(!exact(&format!("__all__=['old']\n{tail}\n")), "{tail}");
    }
    assert!(!exact(
        "__all__=['old']\ndef mutate():\n    global __all__\n    __all__=['new']\nmutate()\n"
    ));
    assert!(!exact("NAME='old'\nmutate()\n__all__=[NAME]\n"));
}
#[test]
fn inert_bodies_and_known_list_methods_preserve_exactness() {
    assert!(exact("__all__=['old']\ndef never_called():\n    mutate()\n__all__.append('new')\n__all__.extend(['last'])\n__all__.remove('old')\n"));
    assert!(!exact("__all__=['old']\n@decorate()\ndef f():\n    pass\n"));
    assert!(!exact("__all__=['old']\ndef f(x=mutate()):\n    pass\n"));
    assert!(exact("__all__=['old']\nmutate()\n__all__=['fresh']\n"));
}

#[test]
fn implicit_decorators_and_class_execution_retire_global_authority() {
    for definition in [
        "@mutate\ndef f():\n    pass",
        "@helper.mutate\ndef f():\n    pass",
        "class C:\n    mutate()",
        "class C:\n    global __all__\n    __all__=['new']",
        "class C(Base):\n    pass",
        "@mutate\nclass C:\n    pass",
    ] {
        assert!(
            !exact(&format!("__all__=['old']\n{definition}\n")),
            "{definition}"
        );
    }
    assert!(exact(
        "__all__=['old']\ndef mutate():\n    global __all__\n    __all__=['new']\n"
    ));
}
