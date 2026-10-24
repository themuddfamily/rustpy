use rustpy_highlight::{highlight_python, HighlightTag};
fn role(source: &str, needle: &str) -> Option<HighlightTag> {
    let offset = source.find(needle).unwrap();
    highlight_python(source)
        .into_iter()
        .find(|span| usize::from(span.range.start()) == offset)
        .map(|span| span.tag)
}
#[test]
fn class_local_lookup_differs_from_free_closure_name_before_store() {
    let assigned = "def outer():\n len=1\n class C:\n  before=len([])\n  len=2\n";
    assert_eq!(role(assigned, "len([])"), Some(HighlightTag::Builtin));
    let free = "def outer():\n len=1\n class C:\n  before=len([])\n";
    assert_eq!(role(free, "len([])"), Some(HighlightTag::LocalVariable));
}
#[test]
fn deferred_recursive_global_does_not_recolor_eager_default_header() {
    let source = "def len(value=len([])):\n return len([])\n";
    let spans = highlight_python(source);
    let header = source.find("len([])").unwrap();
    let body = source.rfind("len([])").unwrap();
    assert_eq!(
        spans
            .iter()
            .find(|span| usize::from(span.range.start()) == header)
            .map(|s| s.tag),
        Some(HighlightTag::Builtin)
    );
    assert_ne!(
        spans
            .iter()
            .find(|span| usize::from(span.range.start()) == body)
            .map(|s| s.tag),
        Some(HighlightTag::Builtin)
    );
}
