use rustpy_highlight::{highlight_python, HighlightTag};
#[test]
fn module_and_class_acquisitions_bind_targets_before_body() {
    for source in [
        "for len in range(len([])):\n print(len)\n",
        "class C:\n for len in range(len([])):\n  print(len)\n",
        "with open('name') as len:\n print(len)\n",
        "class C:\n with open('name') as len:\n  print(len)\n",
    ] {
        let spans = highlight_python(source);
        let tags: Vec<_> = source
            .match_indices("len")
            .map(|(offset, _)| {
                spans
                    .iter()
                    .find(|s| usize::from(s.range.start()) == offset)
                    .map(|s| s.tag)
            })
            .collect();
        assert_ne!(tags[0], Some(HighlightTag::Builtin), "target: {source}");
        if source.contains("range") {
            assert_eq!(
                tags[1],
                Some(HighlightTag::Builtin),
                "iterable evaluates before store: {source}"
            );
        }
        assert_ne!(
            *tags.last().unwrap(),
            Some(HighlightTag::Builtin),
            "body sees binding: {source}"
        );
    }
}
