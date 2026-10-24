use rustpy_syntax::{exports, parse};
#[test]
fn inert_public_reexport_classes_preserve_exact_names() {
    let value = exports::evaluate(
        &parse("__all__=['DataFrame']\nclass DataFrame:pass\nclass Hidden:pass\n").syntax_node(),
    )
    .unwrap();
    assert!(value.exact);
    assert_eq!(value.names, ["DataFrame"]);
    let source="__all__=['C']\nclass C:\n    \"docs\"\n    label='π'\n    def method(self, x=1):\n        mutate()\n";
    assert!(
        exports::evaluate(&parse(source).syntax_node())
            .unwrap()
            .exact
    );
}
#[test]
fn class_execution_and_definition_hooks_remain_unknown() {
    for body in [
        "class C:\n    mutate()",
        "class C:\n    global __all__\n    __all__=['new']",
        "class C(Base):pass",
        "class C(metaclass=Meta):pass",
        "@mutate\nclass C:pass",
        "class C:\n    @mutate\n    def method(self):pass",
        "class C:\n    def method(self, x=mutate()):pass",
    ] {
        assert!(
            !exports::evaluate(&parse(&format!("__all__=['old']\n{body}\n")).syntax_node())
                .unwrap()
                .exact,
            "{body}"
        );
    }
}
