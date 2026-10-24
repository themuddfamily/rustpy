use rustpy_highlight::{highlight_python, HighlightTag};
fn tags(source: &str, name: &str) -> Vec<Option<HighlightTag>> {
    let spans = highlight_python(source);
    source
        .match_indices(name)
        .map(|(offset, _)| {
            spans
                .iter()
                .find(|s| {
                    usize::from(s.range.start()) == offset
                        && usize::from(s.range.end()) == offset + name.len()
                })
                .map(|s| s.tag)
        })
        .collect()
}
#[test]
fn module_and_class_names_use_dynamic_store_order() {
    for source in [
        "len([])\nlen=1\nlen([])\n",
        "class C:\n len([])\n len=1\n len([])\n",
    ] {
        let actual = tags(source, "len");
        assert_eq!(actual[0], Some(HighlightTag::Builtin));
        assert_ne!(actual[1], Some(HighlightTag::Builtin));
        assert_ne!(actual[2], Some(HighlightTag::Builtin));
    }
}
#[test]
fn explicit_global_store_retires_builtin_only_after_store() {
    let source = "def work():\n global len\n len([])\n len=1\n len([])\n";
    let actual = tags(source, "len");
    // Declaration is context syntax; its color is not a value lookup oracle.
    assert_eq!(actual[1], Some(HighlightTag::Builtin));
    assert_ne!(actual[2], Some(HighlightTag::Builtin));
    assert_ne!(actual[3], Some(HighlightTag::Builtin));
}
#[test]
fn nonlocal_and_fastlocal_bindings_keep_lexical_scope_identity() {
    let source = "def outer():\n value_π=1\n def inner():\n  nonlocal value_π\n  value_π=2\n  return value_π\n return value_π\n";
    let actual = tags(source, "value_π");
    assert_eq!(actual[0], Some(HighlightTag::LocalVariable));
    assert_eq!(actual[2], Some(HighlightTag::LocalVariable));
    assert_eq!(actual[3], Some(HighlightTag::LocalVariable));
    assert_eq!(actual[4], Some(HighlightTag::LocalVariable));
    let source = "def work():\n len([])\n len=1\n";
    assert_eq!(
        tags(source, "len"),
        [
            Some(HighlightTag::LocalVariable),
            Some(HighlightTag::LocalVariable)
        ]
    );
}
