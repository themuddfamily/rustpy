use rustpy_semantic::{db::Analysis, template_context, TextSize};
use std::path::Path;
#[test]
fn canonical_render_alias_and_class_view_link_templates_and_physical_source_names() {
    let db = Analysis::for_project(Path::new("/related-symbols"));
    let path = Path::new("/related-symbols/views.py");
    let source="from django.shortcuts import render as page_render\nfrom django.views.generic import TemplateView\ndef page(request):\n    return page_render(request,'page.html')\nclass Other(TemplateView):\n    template_name='other.html'\n";
    let file = db.file(db.set_file_text(path, source)).unwrap();
    assert!(file.parse.errors().is_empty());
    let templates = vec![
        Path::new("/related-symbols/templates/page.html").to_path_buf(),
        Path::new("/related-symbols/templates/other.html").to_path_buf(),
    ];
    for (needle, expected) in [("def page", "page.html"), ("class Other", "other.html")] {
        let offset = TextSize::new((source.find(needle).unwrap() + 5) as u32);
        let locations = template_context::related_locations(
            &db,
            &file,
            offset,
            &[path.to_path_buf()],
            &templates,
        );
        assert_eq!(locations.len(), 1);
        assert!(locations[0].path.ends_with(expected));
    }
    let template = db
        .file(db.set_file_text(&templates[0], "<p>page</p>\n"))
        .unwrap();
    let locations = template_context::related_locations(
        &db,
        &template,
        0.into(),
        &[path.to_path_buf()],
        &templates,
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].path, path);
    assert_eq!(&source[locations[0].range], "page");
}
#[test]
fn rebound_render_and_unrelated_same_named_function_do_not_fabricate_framework_links() {
    let db = Analysis::for_project(Path::new("/related-symbol-controls"));
    let path = Path::new("/related-symbol-controls/views.py");
    let template = Path::new("/related-symbol-controls/templates/page.html").to_path_buf();
    for source in ["def render(request,name): return None\ndef page(request): return render(request,'page.html')\n","from django.shortcuts import render\nrender=lambda request,name: None\ndef page(request): return render(request,'page.html')\n"] {
        let file=db.file(db.set_file_text(path,source)).unwrap();assert!(file.parse.errors().is_empty());
        assert!(template_context::related_locations(&db,&file,0.into(),&[path.to_path_buf()],std::slice::from_ref(&template)).is_empty());
    }
}
