use rustpy_highlight::{highlight_python, HighlightTag};
#[test]
fn maintained_local_variable_attribute_is_emitted_for_assignment_and_read() {
    let source = "def work():\n    value_π=1\n    return value_π\n";
    let spans = highlight_python(source);
    let locals: Vec<_> = spans
        .iter()
        .filter(|span| span.tag == HighlightTag::LocalVariable)
        .map(|span| &source[usize::from(span.range.start())..usize::from(span.range.end())])
        .collect();
    assert_eq!(locals, ["value_π", "value_π"]);
}
#[test]
fn scoped_shadowed_builtin_uses_local_attribute_without_changing_global_builtin() {
    let source = "def work():\n    len=1\n    return len\nprint(len([]))\n";
    let spans = highlight_python(source);
    let locals: Vec<_> = spans
        .iter()
        .filter(|span| span.tag == HighlightTag::LocalVariable)
        .map(|span| &source[usize::from(span.range.start())..usize::from(span.range.end())])
        .collect();
    assert_eq!(locals, ["len", "len"]);
    let builtins: Vec<_> = spans
        .iter()
        .filter(|span| span.tag == HighlightTag::Builtin)
        .map(|span| &source[usize::from(span.range.start())..usize::from(span.range.end())])
        .collect();
    assert_eq!(builtins, ["print", "len"]);
}
