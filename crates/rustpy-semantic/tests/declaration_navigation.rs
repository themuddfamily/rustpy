use rustpy_semantic::{
    db::{Analysis, FileInfo},
    features::{self, Location},
    TextSize,
};
use std::{path::Path, sync::Arc};
fn open(db: &Analysis, path: &Path, text: &str) -> Arc<FileInfo> {
    let id = db.set_file_text(path, text);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{text}");
    file
}
fn at(file: &FileInfo, needle: &str, inside: usize) -> TextSize {
    TextSize::new((file.text.rfind(needle).unwrap() + inside) as u32)
}
fn names(db: &Analysis, locations: Vec<Location>) -> Vec<String> {
    locations
        .into_iter()
        .map(|location| {
            let file = db.file_for_path(&location.path).unwrap();
            file.text[location.range].to_string()
        })
        .collect()
}

#[test]
fn names_attributes_constructors_decorators_keywords_and_super_share_real_declarations() {
    let db = Analysis::for_project(Path::new("/declaration-routes"));
    let file=open(&db,Path::new("/declaration-routes/main.py"),"def decorate(value): return value\nclass Base:\n    def __init__(self, initial:int): pass\n    def run(self): pass\n@decorate\nclass Child(Base):\n    def run(self):\n        super().run()\n        super()\nchild=Child(initial=1)\nchild.run()\n");
    for (needle, inside, expected) in [
        ("@decorate", 2, "decorate"),
        ("Child(initial", 1, "__init__"),
        ("initial=", 2, "initial"),
        ("super().run", 9, "run"),
        ("super()\n", 1, "Base"),
        ("child.run", 7, "run"),
    ] {
        let result = features::definition_at(&db, &file, at(&file, needle, inside));
        assert_eq!(names(&db, result), [expected], "{needle}");
    }
    let declared = features::definition_at(
        &db,
        &file,
        TextSize::new(file.text.find("def decorate").unwrap() as u32 + 4),
    );
    assert_eq!(declared[0].path, file.path);
    assert!(declared[0].range.contains_inclusive(TextSize::new(4)));
}
#[test]
fn canonical_attribute_strings_exports_and_forward_annotations_resolve_but_prose_does_not() {
    let db = Analysis::for_project(Path::new("/string-navigation"));
    let file=open(&db,Path::new("/string-navigation/main.py"),"class Item:\n    field:int\nexported=1\n__all__=['exported']\ndef use(value:'Item'):\n    getattr(value,'field')\n    text='field'\n");
    for (needle, inside, expected) in [
        ("['exported']", 4, "exported"),
        ("'Item'", 2, "Item"),
        ("getattr(value,'field')", 17, "field"),
    ] {
        assert_eq!(
            names(
                &db,
                features::definition_at(&db, &file, at(&file, needle, inside))
            ),
            [expected],
            "{needle}"
        );
    }
    assert!(features::definition_at(&db, &file, at(&file, "text='field'", 8)).is_empty());
    let fake=open(&db,Path::new("/string-navigation/fake.py"),"class Item:\n    field:int\ndef getattr(value,name): return 1\ndef use(value:Item):\n    getattr(value,'field')\n");
    assert!(features::definition_at(&db, &fake, at(&fake, "'field'", 3)).is_empty());
}
#[test]
fn dotted_forward_annotations_choose_the_identifier_at_the_physical_caret() {
    let db = Analysis::for_project(Path::new("/quoted-navigation"));
    open(
        &db,
        Path::new("/quoted-navigation/model.py"),
        "class Item: pass\n",
    );
    let file = open(
        &db,
        Path::new("/quoted-navigation/main.py"),
        "import model\ndef f(value:'list[model.Item]'): pass\n",
    );
    assert_eq!(
        names(
            &db,
            features::definition_at(&db, &file, at(&file, "model.Item", 8))
        ),
        ["Item"]
    );
}
#[test]
fn operators_navigate_actual_dunders_and_offer_both_dispatch_candidates() {
    let db = Analysis::for_project(Path::new("/operator-navigation"));
    let file=open(&db,Path::new("/operator-navigation/main.py"),"class Left:\n    def __add__(self,other): return self\n    def __neg__(self): return self\n    def __lt__(self,other): return True\n    def __getitem__(self,key): return 1\nclass Right:\n    def __radd__(self,other): return self\nleft=Left()\nright=Right()\nleft + right\n-left\nleft < right\nleft[0]\n");
    assert_eq!(
        names(
            &db,
            features::definition_at(&db, &file, at(&file, "left + right", 5))
        ),
        ["__add__", "__radd__"]
    );
    assert_eq!(
        names(
            &db,
            features::definition_at(&db, &file, at(&file, "-left", 0))
        ),
        ["__neg__"]
    );
    assert_eq!(
        names(
            &db,
            features::definition_at(&db, &file, at(&file, "left < right", 5))
        ),
        ["__lt__"]
    );
    assert_eq!(
        names(
            &db,
            features::definition_at(&db, &file, at(&file, "left[0]", 4))
        ),
        ["__getitem__"]
    );
}
#[test]
fn physical_source_stub_counterparts_keep_qualified_members_and_parameters() {
    let db = Analysis::for_project(Path::new("/counterpart-navigation"));
    let stub=open(&db,Path::new("/counterpart-navigation/api.pyi"),"class First:\n    def method(self,value:int)->int: ...\nclass Other:\n    def method(self,value:str)->str: ...\n");
    let source=open(&db,Path::new("/counterpart-navigation/api.py"),"class Other:\n    def method(self,value): return value\nclass First:\n    def method(self,value):\n        return value\n");
    let offset = TextSize::new(stub.text.find("method").unwrap() as u32 + 1);
    let counterpart = features::source_stub_counterpart_at(&db, &stub, offset);
    assert_eq!(counterpart.len(), 1);
    assert_eq!(counterpart[0].path, source.path);
    assert_eq!(&source.text[counterpart[0].range], "method");
    assert_eq!(
        counterpart[0].range.start(),
        TextSize::new(source.text.rfind("method").unwrap() as u32)
    );
    let reverse = features::source_stub_counterpart_at(&db, &source, counterpart[0].range.start());
    assert_eq!(reverse[0].path, stub.path);
    assert_eq!(
        reverse[0].range.start(),
        TextSize::new(stub.text.find("method").unwrap() as u32)
    );
}
#[test]
fn preference_follows_source_without_losing_missing_stub_members_or_typeshed() {
    let temp = tempfile::TempDir::with_prefix("declaration-source-preference").unwrap();
    std::fs::write(
        temp.path().join("api.pyi"),
        "def run(value:int)->int: ...\ndef stub_only()->int: ...\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("api.py"),
        "def run(value):\n    return value\n",
    )
    .unwrap();
    let db = Analysis::for_project(temp.path());
    let file = open(
        &db,
        &temp.path().join("main.py"),
        "from api import run,stub_only\nrun(1)\nstub_only()\nlen([])\n",
    );
    let offset = at(&file, "run(1)", 1);
    let stub = features::definition_at_with_options(&db, &file, offset, false);
    assert!(stub[0].is_stub);
    let source = features::definition_at_with_options(&db, &file, offset, true);
    assert!(!source[0].is_stub);
    assert_eq!(source[0].path, temp.path().join("api.py"));
    assert!(
        features::definition_at_with_options(&db, &file, at(&file, "stub_only()", 2), true)[0]
            .is_stub
    );
    let builtin = features::definition_at_with_options(&db, &file, at(&file, "len([])", 1), true);
    assert!(builtin[0].path.starts_with("<typeshed>"));
    assert_eq!(names(&db, builtin), ["len"]);
}
#[test]
fn imports_and_import_module_string_open_the_module_not_a_guessed_symbol() {
    let temp = tempfile::TempDir::with_prefix("declaration-modules").unwrap();
    std::fs::write(temp.path().join("model.py"), "answer=42\n").unwrap();
    let db = Analysis::for_project(temp.path());
    let file=open(&db,&temp.path().join("main.py"),"import model\nfrom model import answer\nfrom importlib import import_module\nimport_module('model')\n");
    for (needle, inside) in [("import model", 8), ("from model", 6), ("'model'", 3)] {
        let target = features::definition_at(&db, &file, at(&file, needle, inside));
        assert_eq!(target.len(), 1, "{needle}");
        assert_eq!(target[0].path, temp.path().join("model.py"));
        assert!(target[0].range.is_empty());
    }
}

#[test]
fn opening_runtime_source_preserves_authoritative_stub_imports_and_live_source_text() {
    let temp = tempfile::TempDir::with_prefix("declaration-stub-authority").unwrap();
    let stub = temp.path().join("api.pyi");
    let source = temp.path().join("api.py");
    std::fs::write(&stub, "def run(value:int)->str: ...\n").unwrap();
    std::fs::write(&source, "def run(value): return 1\n").unwrap();
    let db = Analysis::for_project(temp.path());
    let selected = db.module("api").unwrap();
    assert!(selected.is_stub);
    let opened = open(&db, &source, "def run(value): return 2\n");
    assert!(!opened.is_stub);
    assert_eq!(db.module("api").unwrap().path, stub);
    assert!(db.file_for_path(&source).unwrap().text.contains("return 2"));
    let file = open(
        &db,
        &temp.path().join("main.py"),
        "from api import run\nrun(1)\n",
    );
    let target = features::definition_at_with_options(&db, &file, at(&file, "run(1)", 1), false);
    assert!(target[0].is_stub);
    let source_target =
        features::definition_at_with_options(&db, &file, at(&file, "run(1)", 1), true);
    assert_eq!(source_target[0].path, source);
    assert!(!source_target[0].is_stub);
    assert!(db.module("api").unwrap().is_stub);
}

#[test]
fn literal_annotation_values_and_shadowed_import_factories_are_not_string_type_references() {
    let db = Analysis::for_project(Path::new("/declaration-refusals"));
    let file=open(&db,Path::new("/declaration-refusals/main.py"),"from typing import Literal\nexported=1\nvalue:Literal['exported']\ndef import_module(name): return name\nimport_module('exported')\n__all__=import_module('exported')\n");
    for (needle, inside) in [
        ("Literal['exported']", 12),
        ("import_module('exported')", 17),
    ] {
        assert!(
            features::definition_at(&db, &file, at(&file, needle, inside)).is_empty(),
            "{needle}"
        );
    }
}

#[test]
fn inplace_call_and_store_syntax_have_their_own_dunder_targets() {
    let db = Analysis::for_project(Path::new("/declaration-operation-modes"));
    let file=open(&db,Path::new("/declaration-operation-modes/main.py"),"class Value:\n    def __iadd__(self,other): return self\n    def __call__(self): return 1\n    def __getitem__(self,key): return 1\n    def __setitem__(self,key,value): pass\n    def __delitem__(self,key): pass\nvalue=Value()\nvalue += 1\nvalue()\nvalue[0]=1\ndel value[1]\n");
    for (needle, inside, expected) in [
        ("value += 1", 7, "__iadd__"),
        ("value()", 5, "__call__"),
        ("value[0]=1", 5, "__setitem__"),
        ("del value[1]", 9, "__delitem__"),
    ] {
        assert_eq!(
            names(
                &db,
                features::definition_at(&db, &file, at(&file, needle, inside))
            ),
            [expected],
            "{needle}"
        );
    }
}

#[test]
fn relative_import_strings_bind_name_and_package_independently_of_keyword_order() {
    let temporary = tempfile::TempDir::with_prefix("relative-declaration-import").unwrap();
    let package = temporary.path().join("pkg");
    std::fs::create_dir(&package).unwrap();
    std::fs::write(package.join("__init__.py"), "").unwrap();
    std::fs::write(package.join("child.py"), "value=1\n").unwrap();
    let db = Analysis::for_project(temporary.path());
    for call in [
        "import_module('.child','pkg')",
        "import_module(name='.child',package='pkg')",
        "import_module(package='pkg',name='.child')",
        "load('.child',package='pkg')",
        "importlib.import_module(package='pkg',name='.child')",
    ] {
        let source = format!(
            "import importlib\nfrom importlib import import_module,import_module as load\n{call}\n"
        );
        let file = open(&db, &temporary.path().join("main.py"), &source);
        let child = features::definition_at(&db, &file, at(&file, "'.child'", 3));
        assert_eq!(child.len(), 1, "{call}");
        assert_eq!(child[0].path, package.join("child.py"));
        assert!(child[0].range.is_empty());
        let target = features::definition_at(&db, &file, at(&file, "'pkg'", 2));
        assert_eq!(target.len(), 1, "{call}");
        assert_eq!(target[0].path, package.join("__init__.py"));
    }
    for call in [
        "import_module('.child')",
        "import_module('.child','pkg',name='.child')",
        "import_module(name='.child',package=unknown)",
        "fake('.child','pkg')",
    ] {
        let source = format!(
            "from importlib import import_module\ndef fake(name,package): return name\n{call}\n"
        );
        let file = open(&db, &temporary.path().join("main.py"), &source);
        assert!(
            features::definition_at(&db, &file, at(&file, "'.child'", 3)).is_empty(),
            "{call}"
        );
    }
}
