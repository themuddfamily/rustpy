use rustpy_highlight::{highlight_python, HighlightTag};
#[test]
fn explicit_class_nonlocal_keeps_enclosing_function_cell_role() {
    let source = "def outer():\n len=1\n class C:\n  nonlocal len\n  len=2\n  before=len([])\n";
    let offset = source.find("len([])").unwrap();
    let spans = highlight_python(source);
    assert_eq!(
        spans
            .iter()
            .find(|span| usize::from(span.range.start()) == offset)
            .map(|span| span.tag),
        Some(HighlightTag::LocalVariable)
    );
}
