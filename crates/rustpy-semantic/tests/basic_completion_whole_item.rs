//! Public Basic-completion providers and metadata needed by actual lookup acceptance.
use rustpy_core::TextSize;
use rustpy_semantic::{
    db::Analysis,
    features::{self, CompletionItem, CompletionKind},
};
fn project(
    source: &str,
    path: &str,
    files: &[(&str, &str)],
) -> (
    tempfile::TempDir,
    Analysis,
    std::sync::Arc<rustpy_semantic::FileInfo>,
    TextSize,
) {
    let root = tempfile::TempDir::with_prefix("rustpy-basic-completion").unwrap();
    for (relative, text) in files {
        let path = root.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let at = source.find("$0").expect("one actual caret marker");
    assert_eq!(source.matches("$0").count(), 1);
    let text = source.replacen("$0", "", 1);
    let path = root.path().join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    let db = Analysis::for_project(root.path());
    let file = db.file_for_path(&path).unwrap();
    (root, db, file, TextSize::new(at as u32))
}
fn items(source: &str) -> Vec<CompletionItem> {
    let (_root, db, file, at) = project(source, "main.py", &[]);
    features::complete(&db, &file, at)
}
fn has(items: &[CompletionItem], name: &str) -> bool {
    items.iter().any(|item| item.label == name)
}
#[test]
fn lexical_names_builtins_parameters_and_control_keywords_use_their_context() {
    assert!(has(
        &items("outside=1\ndef run(parameter):\n    par$0"),
        "parameter"
    ));
    assert!(has(&items("le$0"), "len"));
    assert!(!has(&items("ret$0"), "return"));
    assert!(has(&items("def run():\n    ret$0"), "return"));
    assert!(has(&items("if True:\n    pass\nel$0"), "else"));
    assert!(!has(&items("el$0"), "else"));
    assert!(has(&items("for value $0"), "in"));
    assert!(has(&items("with open('name') $0"), "as"));
    assert!(has(&items("async $0"), "def"));
    assert!(has(&items("mat$0"), "match"));
    assert!(!has(&items("bre$0"), "break"));
    assert!(has(&items("for value in [1]:\n    bre$0"), "break"));
}
#[test]
fn actual_receiver_members_include_properties_other_methods_super_and_unittest() {
    let source="class Base:\n    def inherited(self): pass\nclass Child(Base):\n    @property\n    def title(self)->str: return 'title'\n    def populate(self): self.from_other_method=1\n    def use(self):\n        self.$0";
    let found = items(source);
    assert!(has(&found, "from_other_method"));
    assert!(found
        .iter()
        .any(|item| item.label == "title" && item.kind == CompletionKind::Property));
    assert!(has(&items("class Base:\n    def inherited(self): pass\nclass Child(Base):\n    def use(self):\n        super().inh$0"),"inherited"));
    assert!(has(&items("import unittest\nclass Case(unittest.TestCase):\n    def test_value(self):\n        self.assertEq$0"),"assertEqual"));
    assert!(has(&items("class Child:\n    @classmethod\n    def populate(cls): cls.from_class_method=1\n    @classmethod\n    def use(cls):\n        cls.from_class$0"),"from_class_method"));
}
#[test]
fn modules_relative_imports_and_future_features_offer_only_the_right_namespace() {
    assert!(has(&items("import js$0"), "json"));
    assert!(has(
        &items("from collections import defaul$0"),
        "defaultdict"
    ));
    let (_root, db, file, at) = project(
        "from . import sib$0",
        "pkg/main.py",
        &[("pkg/__init__.py", ""), ("pkg/sibling.py", "value=1\n")],
    );
    assert!(has(&features::complete(&db, &file, at), "sibling"));
    let found = items("from __future__ import ann$0");
    assert!(has(&found, "annotations"));
    assert!(!has(&found, "all_feature_names"));
}
#[test]
fn dunder_and_decorator_candidates_have_valid_insertion_and_import_plans() {
    let found = items("class Owner:\n    def __ent$0");
    assert!(found
        .iter()
        .any(|item| item.label == "__enter__" && item.insert == "__enter__(self):"));
    let (_root, db, file, at) = project("@datacl$0\nclass Owner: pass\n", "main.py", &[]);
    let context = rustpy_semantic::basic_completion::complete(&db, &file, at).unwrap();
    let index = context
        .items
        .iter()
        .position(|item| item.label == "dataclass")
        .unwrap();
    assert_eq!(
        context.imports[index],
        Some(("dataclasses".into(), "dataclass".into()))
    );
    assert_eq!(context.items[index].insert, "dataclass");
}
#[test]
fn dictionary_literal_and_typed_dictionary_keys_replace_inside_the_quotes() {
    for source in ["config={'host':1,'port':2}\nconfig['ho$0']","from typing import TypedDict\nclass Config(TypedDict):\n    host:str\n    port:int\nconfig:Config\nconfig['ho$0']"] {
        let (_root,db,file,at)=project(source,"main.py",&[]);
        let context=rustpy_semantic::basic_completion::complete(&db,&file,at).unwrap();
        assert!(context.replace_segment);assert_eq!(&file.text[usize::from(context.prefix_start)..usize::from(at)],"ho");assert!(has(&context.items,"host"));assert!(!has(&context.items,"port"));
    }
    assert!(!has(&items("class TypedDict: pass\nclass Fake(TypedDict):\n    invented:str\nconfig:Fake\nconfig['inv$0']"),"invented"));
}
#[test]
fn declared_literal_strings_all_exports_enum_members_and_fstrings_are_contextual() {
    assert!(has(
        &items(
            "from typing import Literal\ndef read(mode:Literal['read','write']): pass\nread('r$0')"
        ),
        "read"
    ));
    assert!(!has(
        &items(
            "from typing import Literal\ndef read(mode:Literal['read','write']): pass\nread('r$0')"
        ),
        "write"
    ));
    assert!(has(
        &items("def helper(): pass\nclass Public: pass\n__all__=['hel$0']"),
        "helper"
    ));
    assert!(has(
        &items("from enum import Enum\nclass Color(Enum):\n    RED=1\n    BLUE=2\nColor.RE$0"),
        "RED"
    ));
    assert!(has(
        &items("value='text'\nresult=f'{value.upp$0}'"),
        "upper"
    ));
}
#[test]
fn annotations_type_comments_and_suppressions_keep_import_metadata_and_code_spans() {
    for source in ["value: Opt$0", "value=None # type: Opt$0"] {
        let (_root, db, file, at) = project(source, "main.py", &[]);
        let context = rustpy_semantic::basic_completion::complete(&db, &file, at).unwrap();
        let index = context
            .items
            .iter()
            .position(|item| item.label == "Optional")
            .unwrap();
        assert_eq!(
            context.imports[index],
            Some(("typing".into(), "Optional".into()))
        );
        assert!(context.import_classes);
    }
    let (_root, db, file, at) = project("class Optional: pass\nvalue: Opt$0", "main.py", &[]);
    let context = rustpy_semantic::basic_completion::complete(&db, &file, at).unwrap();
    let index = context
        .items
        .iter()
        .position(|item| item.label == "Optional")
        .unwrap();
    assert!(context.imports[index].is_none());
    assert!(has(&items("import os # noqa: F4$0"), "F401"));
    let (_root, db, file, at) = project("value=1 # type: ignore[arg-t$0]", "main.py", &[]);
    let context = rustpy_semantic::basic_completion::complete(&db, &file, at).unwrap();
    assert!(has(&context.items, "arg-type"));
    assert_eq!(
        &file.text[usize::from(context.prefix_start)..usize::from(at)],
        "arg-t"
    );
}
#[test]
fn pytest_parameter_lookup_uses_local_and_ancestor_fixtures_and_aliases() {
    let files = [
        (
            "conftest.py",
            "import pytest\n@pytest.fixture(name='database')\ndef db_connection(): return 1\n",
        ),
        (
            "tests/conftest.py",
            "from pytest import fixture as provided\n@provided\ndef service(): return 'ok'\n",
        ),
        ("fixturelib.py", "def fixture(function): return function\n"),
    ];
    let (_root, db, file, at) =
        project("def test_read(dat$0): pass\n", "tests/test_case.py", &files);
    assert!(has(&features::complete(&db, &file, at), "database"));
    let (_root, db, file, at) =
        project("def test_read(ser$0): pass\n", "tests/test_case.py", &files);
    assert!(has(&features::complete(&db, &file, at), "service"));
    assert!(has(&items("def test_read(tmp_p$0): pass\n"), "tmp_path"));
    let (_root,db,file,at)=project("from fixturelib import fixture\n@fixture\ndef fake_fixture(): pass\ndef test_read(fake$0): pass\n","tests/test_case.py",&files);
    assert!(!has(&features::complete(&db, &file, at), "fake_fixture"));
}
#[test]
fn keyword_arguments_include_canonical_unpack_fields_and_documented_kwargs_options() {
    assert!(has(
        &items("def run(value:int,*,enabled:bool=False):pass\nrun(1,en$0)"),
        "enabled="
    ));
    assert!(has(&items("from typing import TypedDict,Unpack\nclass Options(TypedDict):\n    enabled:bool\ndef run(**kwargs:Unpack[Options]): pass\nrun(en$0)"),"enabled="));
    assert!(has(&items("def run(**kwargs):\n    \"\"\":param timeout: Maximum wait.\n    :type timeout: float\n    \"\"\"\n    pass\nrun(tim$0)"),"timeout="));
    assert!(!has(&items("class TypedDict: pass\nclass Options(TypedDict):\n    enabled:bool\ndef run(**kwargs:Options):pass\nrun(en$0)"),"enabled="));
}
#[test]
fn class_import_filter_runs_before_the_result_cap() {
    let source = (0..250)
        .map(|number| format!("def Thing{number}(): pass\n"))
        .collect::<String>()
        + "class ThingWanted: pass\n";
    let (_root, db, file, at) = project("Thing$0", "main.py", &[("providers.pyi", &source)]);
    let path = file.path.parent().unwrap().join("providers.pyi");
    let found =
        rustpy_semantic::completion2::class_name_complete_with(&db, &file, at, &[path], &[]);
    assert!(found
        .iter()
        .any(|candidate| candidate.module == "providers" && candidate.name == "ThingWanted"));
    assert!(found
        .iter()
        .all(|candidate| candidate.item.kind == CompletionKind::Class));
}

#[test]
fn class_receiver_attributes_exclude_nested_scopes_and_fake_classmethod() {
    let found=items("class Owner:\n    @classmethod\n    def configure(cls):\n        cls.shared = 1\n        def nested():\n            cls.hidden = 1\n    def method(self):\n        self.visible = 1\n        def nested():\n            self.pollution = 1\nOwner.sha$0");
    assert!(has(&found, "shared"));
    let found=items("class Owner:\n    def method(self):\n        self.visible = 1\n        def nested():\n            self.pollution = 1\nOwner().$0");
    assert!(has(&found, "visible"));
    assert!(!has(&found, "pollution"));
    let found=items("def classmethod(f): return f\nclass Owner:\n    @classmethod\n    def configure(cls): cls.fabricated = 1\nOwner.fab$0");
    assert!(!has(&found, "fabricated"));
}

#[test]
fn static_framework_render_contexts_follow_real_imports_and_unsaved_sources() {
    let (root,db,_file,_at)=project("value=1\n$0","main.py",&[
        ("views.py","from django.shortcuts import render as page\ndef view(request):\n    data={'customer':1}\n    return page(request, 'shop/detail.html', data)\n"),
        ("app.py","from flask import render_template\ndef view(): return render_template('shop/detail.html', invoice=1)\n"),
        ("jinja.py","from jinja2 import Environment\nenv=Environment()\npage=env.get_template('shop/detail.html')\npage.render(report=1)\n"),
        ("fake.py","def render_template(*args,**kwargs): pass\nrender_template('shop/detail.html', fabricated=1)\n"),
    ]);
    let files = ["views.py", "app.py", "jinja.py", "fake.py"].map(|name| root.path().join(name));
    let template = root.path().join("templates/shop/detail.html");
    let names = rustpy_semantic::template_context::names(&db, &files, &template);
    assert!(names.iter().any(|name| name == "customer"));
    assert!(names.iter().any(|name| name == "invoice"));
    assert!(names.iter().any(|name| name == "report"));
    assert!(!names.iter().any(|name| name == "fabricated"));
    db.set_file_text(
        &files[1],
        "from flask import render_template\nrender_template('shop/detail.html', edited=1)\n",
    );
    let names = rustpy_semantic::template_context::names(&db, &files, &template);
    assert!(names.iter().any(|name| name == "edited"));
    assert!(!names.iter().any(|name| name == "invoice"));
    db.set_file_text(&files[1],"import flask\nflask.render_template=object\nflask.render_template('shop/detail.html', retired=1)\n");
    assert!(
        !rustpy_semantic::template_context::names(&db, &files, &template)
            .iter()
            .any(|name| name == "retired")
    );
}

#[test]
fn template_member_completion_infers_render_context_values_and_loop_elements() {
    use rustpy_semantic::template_context::{member_path, members, Step};
    let (root,db,_file,_at)=project("value=1\n$0","main.py",&[
        ("models.py","class Author:\n    name: str = ''\nclass Book:\n    title: str = ''\n    author: Author = Author()\n    def summary(self) -> str: return ''\n"),
        ("app.py","from flask import render_template\nfrom models import Book\ndef view():\n    books: list[Book] = []\n    return render_template('shop/list.html', featured=Book(), books=books)\n"),
        ("views.py","from django.shortcuts import render\nfrom models import Book\ndef view(request):\n    return render(request, 'shop/detail.html', {'book': Book()})\n"),
    ]);
    let files = ["app.py", "views.py"].map(|name| root.path().join(name));
    let list = root.path().join("templates/shop/list.html");
    let labels = |items: Vec<rustpy_semantic::features::CompletionItem>| {
        items
            .into_iter()
            .map(|item| (item.label.to_string(), item.insert))
            .collect::<Vec<_>>()
    };
    let found = labels(members(&db, &files, &list, "featured", &[]));
    assert!(found.iter().any(|(label, _)| label == "title"));
    assert!(found
        .iter()
        .any(|(label, insert)| label == "summary" && insert == "summary()"));
    assert!(!found.iter().any(|(label, _)| label.starts_with('_')));
    let found = labels(members(
        &db,
        &files,
        &list,
        "featured",
        &[Step::Attribute("author".into())],
    ));
    assert!(found.iter().any(|(label, _)| label == "name"));
    let found = labels(members(&db, &files, &list, "books", &[Step::Iterate]));
    assert!(found.iter().any(|(label, _)| label == "title"));
    // Django renders call methods without parentheses.
    let detail = root.path().join("templates/shop/detail.html");
    let found = labels(members(&db, &files, &detail, "book", &[]));
    assert!(found
        .iter()
        .any(|(label, insert)| label == "summary" && insert == "summary"));
    assert!(members(&db, &files, &detail, "missing", &[]).is_empty());

    let text = "{% for b in books %}{% for a in b.authors %}{{ a.na";
    let (start, root_name, steps) = member_path(text, text.len()).unwrap();
    assert_eq!(&text[start..], "na");
    assert_eq!(root_name, "books");
    assert_eq!(
        steps,
        [
            Step::Iterate,
            Step::Attribute("authors".into()),
            Step::Iterate
        ]
    );
    let text = "{% for b in books %}{% endfor %}{{ b.";
    assert_eq!(member_path(text, text.len()).unwrap().1, "b");
    let text = "{{ featured.author.";
    let (_, root_name, steps) = member_path(text, text.len()).unwrap();
    assert_eq!(
        (root_name.as_str(), steps),
        ("featured", vec![Step::Attribute("author".into())])
    );
    assert!(member_path("{{ call().", 10).is_none());
    assert!(member_path("{{ loop.", 8).is_none());
    assert!(member_path("{{ featured }}", 12).is_none());
}
