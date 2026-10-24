use super::*;
use rustpy_semantic::usages::{docstring_param_ranges, usages_in_file};
use std::path::Path;

fn setup(files: &[(&str, &str)]) -> (Analysis, Vec<PathBuf>) {
    let db = Analysis::for_project(Path::new("/proj"));
    let mut paths = Vec::new();
    for (name, src) in files {
        let p = PathBuf::from(format!("/proj/{name}"));
        db.set_file_text(&p, src);
        paths.push(p);
    }
    (db, paths)
}

fn apply(src: &str, edits: &[(TextRange, String)]) -> String {
    rustpy_semantic::edit_helpers::apply_edits(src, edits)
}

fn edits_for<'a>(r: &'a RenameEdits, path: &str) -> &'a [(TextRange, String)] {
    r.iter()
        .find(|(p, _)| p.ends_with(path))
        .map(|(_, e)| e.as_slice())
        .unwrap_or(&[])
}

#[test]
fn highlights_and_finds_usages() {
    let main = "def helper(x):\n    y = x + 1\n    return y\n\nclass Box:\n    def __init__(self):\n        self.size = 1\n    def grow(self):\n        self.size += 1\n\nb = Box()\nb.grow()\nprint(b.size, helper(2))\n";
    let other = "from main import helper, Box\n\nhelper(3)\nBox().size\n";
    let (db, paths) = setup(&[("main.py", main), ("other.py", other)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let at = |needle: &str| TextSize::new(main.find(needle).unwrap() as u32);
    // local variable y: definition + usage
    let u = usages_in_file(&db, &f, at("y = x"));
    assert_eq!(u.len(), 2);
    assert!(u[0].is_definition);
    // helper: 2 in main (def + call), plus import + call in other
    let u = find_usages(&db, &f, at("helper(2)"), &paths);
    assert_eq!(u.iter().filter(|u| u.path.ends_with("main.py")).count(), 2);
    assert_eq!(u.iter().filter(|u| u.path.ends_with("other.py")).count(), 2);
    // member size: declaration (self.size = 1), aug-assign, read in main, read in other
    let u = find_usages(&db, &f, at("size, helper"), &paths);
    assert_eq!(u.len(), 4, "{u:?}");
    let r = rename(&db, &f, at("helper(2)"), "assist", &paths).unwrap();
    assert_eq!(r.len(), 2);
    assert!(rename(&db, &f, at("helper(2)"), "1bad", &paths).is_err());
    assert!(rename(&db, &f, at("helper(2)"), "class", &paths).is_err());
    // Renaming onto an existing name of the same scope is refused.
    assert!(rename(&db, &f, at("helper(2)"), "Box", &paths)
        .unwrap_err()
        .contains("already defined"));
}

#[test]
fn renaming_a_parameter_updates_keywords_through_a_mixin() {
    // `self` is typed as the mixin, which has no `greet`: the keyword still
    // belongs to the subclass's method (MRO-related receiver).
    let main = "class GreeterMixin:\n    def hello(self):\n        return self.greet(name=\"x\")\n\n\nclass Host(GreeterMixin):\n    def greet(self, name):\n        return name\n\n\nHost().hello()\n";
    let (db, paths) = setup(&[("main.py", main)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let at = TextSize::new(
        (main.find("def greet(self, name)").unwrap() + "def greet(self, ".len()) as u32,
    );
    let r = rename(&db, &f, at, "who", &paths).unwrap();
    let out = apply(main, edits_for(&r, "main.py"));
    assert!(
        out.contains("self.greet(who=\"x\")") && out.contains("def greet(self, who):"),
        "{out}"
    );
}

#[test]
fn rename_through_an_aliased_import_edits_the_imported_name() {
    let main = "def helper():\n    return 1\n";
    let other = "from main import helper as h\nfrom main import helper\n\nh()\nhelper()\n";
    let (db, paths) = setup(&[("main.py", main), ("other.py", other)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let r = rename(&db, &f, TextSize::new(4), "assist", &paths).unwrap();
    assert_eq!(
        apply(other, edits_for(&r, "other.py")),
        "from main import assist as h\nfrom main import assist\n\nh()\nassist()\n"
    );
    // Starting from the import in the other file renames the definition too.
    let o = db.file_for_path(&paths[1]).unwrap();
    let at_import = TextSize::new(other.find("helper\n").unwrap() as u32);
    let r = rename(&db, &o, at_import, "assist", &paths).unwrap();
    assert_eq!(
        apply(main, edits_for(&r, "main.py")),
        "def assist():\n    return 1\n"
    );
    assert_eq!(
        apply(other, edits_for(&r, "other.py")),
        "from main import assist as h\nfrom main import assist\n\nh()\nassist()\n"
    );
}

#[test]
fn module_attribute_usages_are_found_from_the_definition() {
    let main = "def helper():\n    return 1\n";
    let other = "import main\nimport main as m\n\nmain.helper()\nm.helper()\nobj.helper()\n";
    let (db, paths) = setup(&[("main.py", main), ("other.py", other)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let r = rename(&db, &f, TextSize::new(4), "assist", &paths).unwrap();
    assert_eq!(
        apply(other, edits_for(&r, "other.py")),
        "import main\nimport main as m\n\nmain.assist()\nm.assist()\nobj.helper()\n"
    );
}

#[test]
fn renaming_a_parameter_updates_keyword_arguments_project_wide() {
    let main = "def greet(name, excited=False):\n    \"\"\"Say hi.\n\n    :param name: who\n    :param excited: loud\n    \"\"\"\n    return 'hi ' + name\n\n\ngreet('bob')\ngreet(name='ann')\n";
    let other = "from main import greet\n\n\ndef g(name):\n    return name\n\n\ngreet(name='x', excited=True)\ng(name='z')\nmystery.call(name='q')\nsorted([], key=len)\n";
    let (db, paths) = setup(&[("main.py", main), ("other.py", other)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let o = db.file_for_path(&paths[1]).unwrap();
    let expect_main = "def greet(person, excited=False):\n    \"\"\"Say hi.\n\n    :param person: who\n    :param excited: loud\n    \"\"\"\n    return 'hi ' + person\n\n\ngreet('bob')\ngreet(person='ann')\n";
    let expect_other = "from main import greet\n\n\ndef g(name):\n    return name\n\n\ngreet(person='x', excited=True)\ng(name='z')\nmystery.call(name='q')\nsorted([], key=len)\n";
    // From the definition.
    let r = rename(
        &db,
        &f,
        TextSize::new(main.find("name,").unwrap() as u32),
        "person",
        &paths,
    )
    .unwrap();
    assert_eq!(apply(main, edits_for(&r, "main.py")), expect_main);
    assert_eq!(apply(other, edits_for(&r, "other.py")), expect_other);
    // From a keyword argument in another file.
    let r = rename(
        &db,
        &o,
        TextSize::new(other.find("name='x'").unwrap() as u32),
        "person",
        &paths,
    )
    .unwrap();
    assert_eq!(apply(main, edits_for(&r, "main.py")), expect_main);
    assert_eq!(apply(other, edits_for(&r, "other.py")), expect_other);
    // The unrelated `g(name=...)` keyword resolves to `g`'s own parameter.
    let u = find_usages(
        &db,
        &o,
        TextSize::new(other.find("name='z'").unwrap() as u32),
        &paths,
    );
    assert!(u.iter().all(|u| u.path.ends_with("other.py")), "{u:?}");
    assert_eq!(u.len(), 3, "{u:?}");
    // A keyword of an unresolvable callee is not a symbol (no blanket rename).
    assert!(rename(
        &db,
        &o,
        TextSize::new(other.find("name='q'").unwrap() as u32),
        "k",
        &paths
    )
    .is_err());
    // A library parameter is not renamed through the project's calls.
    assert!(rename(
        &db,
        &o,
        TextSize::new(other.find("key=").unwrap() as u32),
        "k",
        &paths
    )
    .unwrap_err()
    .contains("library"));
}

#[test]
fn library_symbols_and_module_imports_are_not_renamed() {
    let src = "import os\nimport json as j\n\nprint(len(os.sep), j.dumps(1))\nitems = []\nitems.append(1)\n";
    let (db, paths) = setup(&[("m.py", src)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let at = |needle: &str| TextSize::new(src.find(needle).unwrap() as u32);
    assert!(rename(&db, &f, at("len("), "size", &paths)
        .unwrap_err()
        .contains("library"));
    assert!(rename(&db, &f, at("append"), "push", &paths)
        .unwrap_err()
        .contains("library"));
    assert!(rename(&db, &f, at("os.sep"), "system", &paths)
        .unwrap_err()
        .contains("Rename Module"));
    // A local alias is just a name.
    let r = rename(&db, &f, at("j.dumps"), "js", &paths).unwrap();
    assert_eq!(apply(src, edits_for(&r, "m.py")), "import os\nimport json as js\n\nprint(len(os.sep), js.dumps(1))\nitems = []\nitems.append(1)\n");
}

#[test]
fn constructor_and_method_keyword_arguments() {
    let src = "class Box:\n    def __init__(self, size):\n        self.size = size\n\n    def grow(self, by=1):\n        self.size += by\n\n\nb = Box(size=2)\nb.grow(by=3)\n\n\ndef use(x):\n    x.grow(by=4)\n";
    let (db, paths) = setup(&[("m.py", src)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let r = rename(
        &db,
        &f,
        TextSize::new(src.find("size):").unwrap() as u32),
        "capacity",
        &paths,
    )
    .unwrap();
    assert_eq!(apply(src, edits_for(&r, "m.py")), "class Box:\n    def __init__(self, capacity):\n        self.size = capacity\n\n    def grow(self, by=1):\n        self.size += by\n\n\nb = Box(capacity=2)\nb.grow(by=3)\n\n\ndef use(x):\n    x.grow(by=4)\n");
    // Untyped receivers match by method name, like method renames do.
    let u = usages_in_file(&db, &f, TextSize::new(src.find("by=1").unwrap() as u32));
    assert_eq!(u.len(), 4, "{u:?}");
}

#[test]
fn usage_search_and_rename_stop_when_cancelled() {
    let main = "def helper():\n    return 1\n";
    let other = "from main import helper\n\nhelper()\n";
    let (db, paths) = setup(&[("main.py", main), ("other.py", other)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let cancelled = ProgressReporter::new();
    cancelled.token().cancel();
    assert_eq!(
        find_usages_with_progress(&db, &f, TextSize::new(4), &paths, &cancelled),
        Err(Cancelled)
    );
    assert_eq!(
        rename_with_progress(&db, &f, TextSize::new(4), "assist", &paths, &cancelled).unwrap_err(),
        "Rename was cancelled"
    );
    let live = ProgressReporter::new();
    assert_eq!(
        rename_with_progress(&db, &f, TextSize::new(4), "assist", &paths, &live).unwrap(),
        rename(&db, &f, TextSize::new(4), "assist", &paths).unwrap()
    );
}

#[test]
fn rename_edits_refuse_changed_text() {
    let text = "x = helper()\n";
    let edits = vec![(
        TextRange::new(TextSize::new(4), TextSize::new(10)),
        "assist".to_string(),
    )];
    assert_eq!(
        apply_rename_edits(text, &edits, "helper").unwrap(),
        "x = assist()\n"
    );
    assert!(apply_rename_edits("x = 1\n", &edits, "helper").is_err());
    assert!(apply_rename_edits("é = helper()\n", &edits, "helper").is_err());
}

#[test]
fn docstring_parameter_entries() {
    let src = "def f(name, other):\n    \"\"\"Doc.\n\n    :param name: a\n    :param str name: b\n    :type name: str\n    name (str): c\n    name : str\n    names: not this\n    :param other: d\n    \"\"\"\n";
    let (db, paths) = setup(&[("d.py", src)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let func = f
        .parse
        .syntax_node()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::FUNC_DEF)
        .unwrap();
    let found: Vec<&str> = docstring_param_ranges(&f, func.text_range(), "name")
        .iter()
        .map(|r| &src[usize::from(r.start())..usize::from(r.end())])
        .collect();
    // Unheaded Google/NumPy-looking prose is not a structured parameter field.
    assert_eq!(found, vec!["name"; 3]);
    for body in [
        "Args:\n        name (str): input\n        names: other",
        "Parameters\n    ----------\n    name : str\n        input\n    names : str\n        other",
    ] {
        let source = format!("def f(name):\n    \"\"\"Summary.\n\n    {body}\n    \"\"\"\n");
        let (db, paths) = setup(&[("structured.py", &source)]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let function = file
            .parse
            .syntax_node()
            .descendants()
            .find(|node| node.kind() == SyntaxKind::FUNC_DEF)
            .unwrap();
        let ranges = docstring_param_ranges(&file, function.text_range(), "name");
        assert_eq!(ranges.len(), 1, "{source}");
        assert_eq!(
            &source[usize::from(ranges[0].start())..usize::from(ranges[0].end())],
            "name"
        );
    }
}

/// Renames the symbol at the first `needle` in `name` and returns the new text of `name`.
fn rename_at(
    files: &[(&str, &str)],
    name: &str,
    needle: &str,
    new_name: &str,
) -> Result<String, String> {
    let (db, paths) = setup(files);
    let src = files.iter().find(|(n, _)| *n == name).unwrap().1;
    let f = db
        .file_for_path(&PathBuf::from(format!("/proj/{name}")))
        .unwrap();
    let r = rename(
        &db,
        &f,
        TextSize::new(src.find(needle).unwrap() as u32),
        new_name,
        &paths,
    )?;
    Ok(apply(src, edits_for(&r, name)))
}

#[test]
fn class_attributes_are_renamed_from_their_declaration() {
    // QA: Shift+F6 on `size` in the class body renamed only the declaration.
    let src = "class Unit:\n    size: float = 1.5\n\n    def over(self):\n        return self.size\n\n\nprint(Unit().over(), Unit.size)\n";
    let expect = "class Unit:\n    zsize: float = 1.5\n\n    def over(self):\n        return self.zsize\n\n\nprint(Unit().over(), Unit.zsize)\n";
    assert_eq!(
        rename_at(&[("ca.py", src)], "ca.py", "size:", "zsize").unwrap(),
        expect
    );
    // The same from either usage.
    assert_eq!(
        rename_at(&[("ca.py", src)], "ca.py", "size\n", "zsize").unwrap(),
        expect
    );
    assert_eq!(
        rename_at(&[("ca.py", src)], "ca.py", "size)", "zsize").unwrap(),
        expect
    );
    // The declaration is found as the usages' declaration (Highlight Usages, in-place rename).
    let (db, paths) = setup(&[("ca.py", src)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let u = usages_in_file(&db, &f, TextSize::new(src.find("size:").unwrap() as u32));
    assert_eq!(u.len(), 3, "{u:?}");
    assert!(
        u[0].is_definition && !u[1].is_definition && !u[2].is_definition,
        "{u:?}"
    );
}

#[test]
fn dataclass_fields_are_renamed_with_attribute_and_constructor_keyword_uses() {
    let src = "from dataclasses import dataclass\n\n\n@dataclass\nclass Point:\n    x: float = 0.0\n    y: float = 0.0\n\n    def distance_to(self, other: \"Point\") -> float:\n        dx, dy = self.x - other.x, self.y - other.y\n        return (dx ** 2 + dy ** 2) ** 0.5\n\n\n@dataclass\nclass Point3(Point):\n    z: float = 0.0\n\n\np = Point(x=1.0, y=2.0)\nq = Point3(x=3.0, z=1.0)\nprint(p.x, q.x)\n\n\ndef make(x):\n    return Point(x=x)\n";
    let expect = "from dataclasses import dataclass\n\n\n@dataclass\nclass Point:\n    xpos: float = 0.0\n    y: float = 0.0\n\n    def distance_to(self, other: \"Point\") -> float:\n        dx, dy = self.xpos - other.xpos, self.y - other.y\n        return (dx ** 2 + dy ** 2) ** 0.5\n\n\n@dataclass\nclass Point3(Point):\n    z: float = 0.0\n\n\np = Point(xpos=1.0, y=2.0)\nq = Point3(xpos=3.0, z=1.0)\nprint(p.xpos, q.xpos)\n\n\ndef make(x):\n    return Point(xpos=x)\n";
    assert_eq!(
        rename_at(&[("pt.py", src)], "pt.py", "x: float", "xpos").unwrap(),
        expect
    );
    assert_eq!(
        rename_at(&[("pt.py", src)], "pt.py", "x - other", "xpos").unwrap(),
        expect
    );
    // From a constructor keyword (also through the subclass's generated `__init__`).
    assert_eq!(
        rename_at(&[("pt.py", src)], "pt.py", "x=1.0", "xpos").unwrap(),
        expect
    );
    assert_eq!(
        rename_at(&[("pt.py", src)], "pt.py", "x=3.0", "xpos").unwrap(),
        expect
    );
    // A class with its own `__init__` keeps its parameter names.
    let own = "class P:\n    x: float = 0.0\n\n    def __init__(self, x=0.0):\n        self.x = x\n\n\nP(x=1.0).x\n";
    assert_eq!(rename_at(&[("own.py", own)], "own.py", "x: float", "xpos").unwrap(), "class P:\n    xpos: float = 0.0\n\n    def __init__(self, x=0.0):\n        self.xpos = x\n\n\nP(x=1.0).xpos\n");
}

#[test]
fn class_variables_rename_in_the_class_body_and_related_classes() {
    let src = "class Greeter:\n    count = 0\n    double = count * 2\n\n    def __init__(self):\n        Greeter.count += 1\n        print(self.count)\n\n\nclass Loud(Greeter):\n    count = 10\n\n\nclass Other:\n    count = 5\n\n\nprint(Loud.count, Other.count, Other().count)\n";
    let expect = "class Greeter:\n    total = 0\n    double = total * 2\n\n    def __init__(self):\n        Greeter.total += 1\n        print(self.total)\n\n\nclass Loud(Greeter):\n    total = 10\n\n\nclass Other:\n    count = 5\n\n\nprint(Loud.total, Other.count, Other().count)\n";
    assert_eq!(
        rename_at(&[("g.py", src)], "g.py", "count = 0", "total").unwrap(),
        expect
    );
    assert_eq!(
        rename_at(&[("g.py", src)], "g.py", "count * 2", "total").unwrap(),
        expect
    );
    // A nested class is a member too.
    let nested = "class Outer:\n    class Meta:\n        ordering = 1\n\n    meta = Meta\n\n\nprint(Outer.Meta.ordering, Outer().Meta)\n";
    assert_eq!(rename_at(&[("n.py", nested)], "n.py", "Meta:", "Options").unwrap(), "class Outer:\n    class Options:\n        ordering = 1\n\n    meta = Options\n\n\nprint(Outer.Options.ordering, Outer().Options)\n");
}

#[test]
fn a_property_rename_includes_its_setter_decorator() {
    let src = "class Box:\n    def __init__(self):\n        self._size = 0\n\n    @property\n    def size(self):\n        return self._size\n\n    @size.setter\n    def size(self, value):\n        self._size = value\n\n\nb = Box()\nb.size = 3\n";
    let expect = "class Box:\n    def __init__(self):\n        self._size = 0\n\n    @property\n    def volume(self):\n        return self._size\n\n    @volume.setter\n    def volume(self, value):\n        self._size = value\n\n\nb = Box()\nb.volume = 3\n";
    assert_eq!(
        rename_at(&[("b.py", src)], "b.py", "size(self):", "volume").unwrap(),
        expect
    );
    assert_eq!(
        rename_at(&[("b.py", src)], "b.py", "size = 3", "volume").unwrap(),
        expect
    );
}

#[test]
fn rename_checks_refuse_existing_names() {
    // QA: in-place renaming `excited` to `name` produced `def greet(name, name=False)`.
    let src = "def greet(name, excited=False):\n    if excited:\n        return name.upper()\n    return name\n\n\nprint(greet(\"bob\", excited=True))\n";
    let (db, paths) = setup(&[("rn4.py", src)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let check = rename_check(
        &db,
        &f,
        TextSize::new(src.find("excited=F").unwrap() as u32),
    )
    .unwrap();
    assert_eq!(check.old_name, "excited");
    assert_eq!(
        check.check("name").unwrap_err(),
        "'name' is already defined in the scope of 'excited'"
    );
    assert!(check.check("loud").is_ok());
    assert!(
        check.check("excited").is_ok(),
        "keeping the name is no conflict"
    );
    assert!(check
        .check("1x")
        .unwrap_err()
        .contains("not a valid Python identifier"));
    assert!(check
        .check("")
        .unwrap_err()
        .contains("not a valid Python identifier"));
    assert!(check.check("lambda").unwrap_err().contains("keyword"));
    // The Rename dialog runs the same check.
    assert_eq!(
        rename_at(&[("rn4.py", src)], "rn4.py", "excited=F", "name").unwrap_err(),
        "'name' is already defined in the scope of 'excited'"
    );
    // A member cannot take the name of another member of its class or a base.
    let cls = "class Base:\n    def area(self):\n        return 0\n\n\nclass Unit(Base):\n    size = 1\n\n    def __init__(self):\n        self.label = ''\n\n    def over(self):\n        return self.size\n";
    let (db, paths) = setup(&[("u.py", cls)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let check = rename_check(&db, &f, TextSize::new(cls.find("size = 1").unwrap() as u32)).unwrap();
    for taken in ["over", "label", "area"] {
        assert!(
            check
                .check(taken)
                .unwrap_err()
                .contains("is already defined in class"),
            "{taken}"
        );
    }
    assert!(check.check("width").is_ok());
    // Ordinary Rename cannot silently introduce protocol dispatch semantics.
    assert!(check
        .check("__eq__")
        .unwrap_err()
        .contains("protocol dunder"));
    // Through `module.name`: the module's other names.
    let main = "def helper():\n    return 1\n\n\nclass Box:\n    pass\n";
    let other = "import main\n\nmain.helper()\n";
    let (db, paths) = setup(&[("main.py", main), ("other.py", other)]);
    let o = db.file_for_path(&paths[1]).unwrap();
    let check = rename_check(&db, &o, TextSize::new(other.find("helper").unwrap() as u32)).unwrap();
    assert_eq!(
        check.check("Box").unwrap_err(),
        "'Box' is already defined in the scope of 'helper'"
    );
    assert!(check.check("assist").is_ok());
    assert!(rename(
        &db,
        &o,
        TextSize::new(other.find("helper").unwrap() as u32),
        "Box",
        &paths
    )
    .is_err());
    // Identifier rules alone for an introduced name.
    let introduced = RenameCheck::identifier_only("value");
    assert!(introduced.check("name").is_ok());
    assert!(introduced.check("for").is_err());
}

fn plan(
    db: &Analysis,
    paths: &[PathBuf],
    file: usize,
    src: &str,
    needle: &str,
    new_name: &str,
    search_in_comments: bool,
) -> Result<RenamePlan, String> {
    let f = db.file_for_path(&paths[file]).unwrap();
    rename_plan(
        db,
        &f,
        TextSize::new(src.find(needle).unwrap() as u32),
        new_name,
        paths,
        RenameOptions { search_in_comments },
    )
}

fn texts<'a>(src: &'a str, usages: &[Usage]) -> Vec<&'a str> {
    usages
        .iter()
        .map(|u| u.line_text.trim())
        .map(|l| &src[src.find(l).unwrap()..src.find(l).unwrap() + l.len()])
        .collect()
}

#[test]
fn untyped_receivers_are_dynamic_usages_that_rename_leaves_alone() {
    let jobs = "class Job:\n    def run(self):\n        return 1\n\n\nclass Robot:\n    def run(self):\n        return 2\n\n\ndef start(job: Job, maybe: Job | None):\n    job.run()\n    maybe.run()\n\n\ndef anything(worker):\n    worker.run()\n\n\nJob().run()\nRobot().run()\n";
    let (db, paths) = setup(&[("jobs.py", jobs)]);
    let p = plan(
        &db,
        &paths,
        0,
        jobs,
        "run(self):\n        return 1",
        "execute",
        false,
    )
    .unwrap();
    // The definition, typed calls (including through `Job | None`): code.
    assert_eq!(
        texts(jobs, &p.code),
        vec!["def run(self):", "job.run()", "maybe.run()", "Job().run()"]
    );
    // `worker` is untyped: maybe a Job, maybe a Robot.
    assert_eq!(texts(jobs, &p.dynamic), vec!["worker.run()"]);
    assert!(p.dynamic[0].dynamic && !p.code.iter().any(|u| u.dynamic));
    assert!(p.needs_preview());
    // Find Usages still lists the dynamic usage.
    let f = db.file_for_path(&paths[0]).unwrap();
    let all = find_usages(
        &db,
        &f,
        TextSize::new(jobs.find("run(self):\n        return 1").unwrap() as u32),
        &paths,
    );
    assert_eq!(all.iter().filter(|u| u.dynamic).count(), 1);
    // The plain rename only touches the code usages.
    let r = rename(
        &db,
        &f,
        TextSize::new(jobs.find("run(self):\n        return 1").unwrap() as u32),
        "execute",
        &paths,
    )
    .unwrap();
    let out = apply(jobs, edits_for(&r, "jobs.py"));
    assert!(
        out.contains("worker.run()")
            && out.contains("Robot().run()")
            && out.contains("Job().execute()")
            && out.contains("maybe.execute()"),
        "{out}"
    );
    // In place only when nothing is uncertain.
    assert_eq!(
        inplace_rename_ranges(
            &db,
            &f,
            TextSize::new(jobs.find("run(self):\n        return 1").unwrap() as u32),
            &paths,
            false
        ),
        None
    );
    let robot_run = TextSize::new(jobs.find("run(self):\n        return 2").unwrap() as u32);
    assert_eq!(
        inplace_rename_ranges(&db, &f, robot_run, &paths, false),
        None,
        "worker.run() may be a Robot too"
    );
}

#[test]
fn comment_and_string_occurrences_are_found_when_asked() {
    let main = "def helper(count):\n    \"\"\"Run helper `count` times.\n\n    :param count: how often (count > 0)\n    \"\"\"\n    # the count, not the counter\n    return count * 2\n\n\n# helper is used by other.py; helpers and _helper are other names\nNAME = \"helper\"\n";
    let other = "from main import helper\n\nprint(f\"calling helper: {helper(1)}\")  # helper!\n";
    let (db, paths) = setup(&[("main.py", main), ("other.py", other)]);
    // Off: nothing beyond code.
    let p = plan(&db, &paths, 0, main, "helper(count)", "assist", false).unwrap();
    assert!(p.text.is_empty());
    assert!(!p.needs_preview());
    // On: whole words in comments and strings, project-wide for a module-level name.
    let p = plan(&db, &paths, 0, main, "helper(count)", "assist", true).unwrap();
    let found: Vec<(String, usize)> = p
        .text
        .iter()
        .map(|u| {
            (
                u.path.file_name().unwrap().to_string_lossy().into_owned(),
                u.line,
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![
            ("main.py".to_string(), 1),
            ("main.py".to_string(), 9),
            ("main.py".to_string(), 10),
            ("other.py".to_string(), 2),
            ("other.py".to_string(), 2)
        ],
        "{:?}",
        p.text
    );
    assert!(p.text.iter().all(|u| u.non_code
        && &main_or_other(main, other, u)
            [usize::from(u.range.start())..usize::from(u.range.end())]
            == "helper"));
    assert!(p.needs_preview());
    // A parameter: only inside its function, and the docstring entry
    // (already a code usage) is not listed twice.
    let p = plan(&db, &paths, 0, main, "count):", "times", true).unwrap();
    assert_eq!(
        p.text.iter().map(|u| u.line).collect::<Vec<_>>(),
        vec![1, 3, 5],
        "{:?}",
        p.text
    );
    assert!(
        p.code.iter().any(|u| u.line == 3),
        "the :param entry is code"
    );
    // In place: the three code usages, plus (when asked) the three
    // occurrences in the function's docstring and comment.
    let f = db.file_for_path(&paths[0]).unwrap();
    assert_eq!(
        inplace_rename_ranges(
            &db,
            &f,
            TextSize::new(main.find("count):").unwrap() as u32),
            &paths,
            true
        )
        .map(|r| r.len()),
        Some(6)
    );
    assert_eq!(
        inplace_rename_ranges(
            &db,
            &f,
            TextSize::new(main.find("count):").unwrap() as u32),
            &paths,
            false
        )
        .map(|r| r.len()),
        Some(3)
    );
}

fn main_or_other<'a>(main: &'a str, other: &'a str, u: &Usage) -> &'a str {
    if u.path.ends_with("main.py") {
        main
    } else {
        other
    }
}

#[test]
fn conflicts_with_existing_names_are_reported() {
    let src = "total = 0\n\n\ndef add(n):\n    count = n\n    total_ = total + count\n    return total_\n\n\ndef report(items):\n    size = len(items)\n    return size\n\n\nclass Job:\n    def run(self):\n        pass\n\n    def stop(self):\n        pass\n\n\nclass Cron(Job):\n    def pause(self):\n        pass\n";
    let (db, paths) = setup(&[("m.py", src)]);
    // A global used in a function that has a local of the new name: captured.
    let p = plan(&db, &paths, 0, src, "total = 0", "count", false).unwrap();
    assert_eq!(p.conflicts.len(), 1, "{:?}", p.conflicts);
    assert!(
        p.conflicts[0]
            .message
            .contains("would refer to the 'count' defined on line 5"),
        "{}",
        p.conflicts[0].message
    );
    assert!(
        rename(
            &db,
            &db.file_for_path(&paths[0]).unwrap(),
            TextSize::new(0),
            "count",
            &paths
        )
        .is_err(),
        "the plain rename refuses"
    );
    // A local renamed to a builtin it calls: the call would be shadowed.
    let p = plan(&db, &paths, 0, src, "size = len", "len", false).unwrap();
    assert_eq!(p.conflicts.len(), 1, "{:?}", p.conflicts);
    assert!(p.conflicts[0]
        .message
        .contains("'len' here would refer to the renamed 'size'"));
    // Merging two names of one scope.
    let p = plan(&db, &paths, 0, src, "size = len", "items", false).unwrap();
    assert!(
        p.conflicts
            .iter()
            .any(|c| c.message.contains("already defined in the scope of 'size'")),
        "{:?}",
        p.conflicts
    );
    // A method renamed to another member of its class, or of a subclass.
    let p = plan(&db, &paths, 0, src, "run(self)", "stop", false).unwrap();
    assert!(
        p.conflicts.iter().any(|c| c
            .message
            .starts_with("Class 'Job' already has a member 'stop'")),
        "{:?}",
        p.conflicts
    );
    let p = plan(&db, &paths, 0, src, "run(self)", "pause", false).unwrap();
    assert!(
        p.conflicts.iter().any(|c| c
            .message
            .starts_with("Class 'Cron' already has a member 'pause'")),
        "{:?}",
        p.conflicts
    );
    // No conflict for a fresh name.
    assert!(plan(&db, &paths, 0, src, "run(self)", "execute", false)
        .unwrap()
        .conflicts
        .is_empty());
    assert!(plan(&db, &paths, 0, src, "total = 0", "grand_total", false)
        .unwrap()
        .conflicts
        .is_empty());
    // The same name is no rename at all.
    assert!(plan(&db, &paths, 0, src, "total = 0", "total", false).is_err());
}

#[test]
fn a_module_importing_the_symbol_must_not_bind_the_new_name() {
    let main = "def helper():\n    return 1\n";
    let other = "from main import helper\n\n\ndef assist():\n    return helper()\n";
    let aliased = "from main import helper as h\n\nassist = 2\nh()\n";
    let (db, paths) = setup(&[
        ("main.py", main),
        ("other.py", other),
        ("aliased.py", aliased),
    ]);
    let p = plan(&db, &paths, 0, main, "helper", "assist", false).unwrap();
    assert_eq!(p.conflicts.len(), 1, "{:?}", p.conflicts);
    assert!(
        p.conflicts[0].path.ends_with("other.py")
            && p.conflicts[0]
                .message
                .contains("other.py, which imports 'helper'"),
        "{:?}",
        p.conflicts
    );
}

#[test]
fn renaming_from_a_module_attribute_renames_the_definition_and_its_imports() {
    let main = "def helper():\n    return 1\n\n\ndef twice():\n    return helper() + helper()\n";
    let other = "import main\n\nmain.helper()\n";
    let third = "from main import helper\n\nhelper()\n";
    let (db, paths) = setup(&[("main.py", main), ("other.py", other), ("third.py", third)]);
    let o = db.file_for_path(&paths[1]).unwrap();
    let at_attr = TextSize::new(other.find("helper").unwrap() as u32);
    let r = rename(&db, &o, at_attr, "assist", &paths).unwrap();
    assert_eq!(
        apply(main, edits_for(&r, "main.py")),
        "def assist():\n    return 1\n\n\ndef twice():\n    return assist() + assist()\n"
    );
    assert_eq!(
        apply(other, edits_for(&r, "other.py")),
        "import main\n\nmain.assist()\n"
    );
    assert_eq!(
        apply(third, edits_for(&r, "third.py")),
        "from main import assist\n\nassist()\n"
    );
    assert_eq!(symbol_at(&db, &o, at_attr).unwrap().kind(), "function");
}

#[test]
fn submodules_reached_as_attributes_are_not_renamed_as_names() {
    let main = "import pkg.sub\n\npkg.sub.run()\n";
    let (db, paths) = setup(&[
        ("pkg/__init__.py", ""),
        ("pkg/sub.py", "def run():\n    pass\n"),
        ("main.py", main),
    ]);
    let f = db.file_for_path(&paths[2]).unwrap();
    let at_sub = TextSize::new(main.rfind("sub").unwrap() as u32);
    let err = rename_plan(&db, &f, at_sub, "other", &paths, RenameOptions::default()).unwrap_err();
    assert!(err.contains("Rename Module"), "{err}");
    // A function of the submodule is renamed with its definition.
    let at_run = TextSize::new(main.find("run").unwrap() as u32);
    let r = rename(&db, &f, at_run, "go", &paths).unwrap();
    assert_eq!(
        apply(main, edits_for(&r, "main.py")),
        "import pkg.sub\n\npkg.sub.go()\n"
    );
    assert_eq!(
        apply("def run():\n    pass\n", edits_for(&r, "sub.py")),
        "def go():\n    pass\n"
    );
}

#[test]
fn members_of_unknown_objects_have_no_declaration_to_rename() {
    let src = "class Job:\n    def run(self):\n        pass\n\n\ndef anything(worker, maybe: Job | None):\n    worker.run()\n    maybe.run()\n";
    let (db, paths) = setup(&[("w.py", src)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let at = |needle: &str| {
        TextSize::new((src.find(needle).unwrap() + needle.len() - "run()".len()) as u32)
    };
    let err = rename_plan(
        &db,
        &f,
        at("worker.run()"),
        "go",
        &paths,
        RenameOptions::default(),
    )
    .unwrap_err();
    assert!(
        err.contains("Cannot find the declaration of 'run'"),
        "{err}"
    );
    assert!(check_rename_at(&db, &f, at("worker.run()")).is_err());
    assert_eq!(
        inplace_rename_ranges(&db, &f, at("worker.run()"), &paths, false),
        None
    );
    // `Job | None` is a Job: renaming from it renames `Job.run`.
    let p = rename_plan(
        &db,
        &f,
        at("maybe.run()"),
        "go",
        &paths,
        RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(texts(src, &p.code), vec!["def run(self):", "maybe.run()"]);
    assert_eq!(texts(src, &p.dynamic), vec!["worker.run()"]);
    assert_eq!(
        symbol_at(&db, &f, at("maybe.run()")).unwrap().kind(),
        "method"
    );
}

#[test]
fn inplace_rename_takes_comments_of_this_file_only() {
    let main = "def solo(count):\n    # count must be positive\n    return count\n\n\nsolo(1)\n";
    let other = "# solo is defined in main.py\n";
    let (db, paths) = setup(&[("main.py", main), ("other.py", other)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let count = TextSize::new(main.find("count").unwrap() as u32);
    // A parameter: its comment in the function is renamed with it.
    assert_eq!(
        inplace_rename_ranges(&db, &f, count, &paths, true).map(|r| r.len()),
        Some(3)
    );
    assert_eq!(
        inplace_rename_ranges(&db, &f, count, &paths, false).map(|r| r.len()),
        Some(2)
    );
    // A module-level function mentioned in another file's comment needs the dialog.
    assert_eq!(
        inplace_rename_ranges(&db, &f, TextSize::new(4), &paths, true),
        None
    );
    assert_eq!(
        inplace_rename_ranges(&db, &f, TextSize::new(4), &paths, false).map(|r| r.len()),
        Some(2)
    );
}

/// `src` without its `«»` markers, and the ranges they enclosed.
fn marked(src: &str) -> (String, Vec<TextRange>) {
    let (mut out, mut ranges, mut start) = (String::new(), Vec::new(), 0u32);
    for c in src.chars() {
        match c {
            '«' => start = out.len() as u32,
            '»' => ranges.push(TextRange::new(
                TextSize::new(start),
                TextSize::new(out.len() as u32),
            )),
            c => out.push(c),
        }
    }
    (out, ranges)
}

fn inplace_problems(src: &str, old: &str, new: &str) -> Vec<String> {
    let (text, ranges) = marked(src);
    let (db, paths) = setup(&[("m.py", &text)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    inplace_rename_conflicts(&f, &ranges, old, new)
        .into_iter()
        .map(|c| format!("{}: {}", c.line + 1, c.message))
        .collect()
}

#[test]
fn inplace_rename_problems_are_found_after_typing() {
    // `total` renamed to `count`, which the function already has.
    let p = inplace_problems(
        "def add(n):\n    count = n\n    «count» = 2\n    return count + «count»\n",
        "total",
        "count",
    );
    assert_eq!(p, vec!["2: 'count' was already defined here (line 2): 'total' and 'count' are now one variable"]);
    // A global renamed to the name of a local of a function using it.
    let p = inplace_problems(
        "«count» = 0\n\n\ndef add(n):\n    count = n\n    return «count» + count\n",
        "total",
        "count",
    );
    assert_eq!(
        p,
        vec!["6: This usage of 'total' now refers to the 'count' defined on line 5"]
    );
    // A local renamed to a builtin the function calls.
    let p = inplace_problems(
        "def report(items):\n    «len» = len(items)\n    return «len»\n",
        "size",
        "len",
    );
    assert_eq!(
        p,
        vec!["2: 'len' here now refers to the renamed 'size' instead of an outer or builtin 'len'"]
    );
    // A fresh name.
    assert!(inplace_problems("def f(a):\n    «b» = a\n    return «b» * 2\n", "x", "b").is_empty());
    // A parameter renamed to another parameter of its function.
    let p = inplace_problems("def f(a, «a»):\n    return «a»\n", "c", "a");
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0].contains("now one variable"), "{p:?}");
}

/// The text at each range.
fn words<'a>(src: &'a str, ranges: &[TextRange]) -> Vec<&'a str> {
    ranges
        .iter()
        .map(|r| &src[usize::from(r.start())..usize::from(r.end())])
        .collect()
}

/// The line (1-based) of each range.
fn lines(src: &str, ranges: &[TextRange]) -> Vec<usize> {
    ranges
        .iter()
        .map(|r| src[..usize::from(r.start())].matches('\n').count() + 1)
        .collect()
}

/// Review: with "Search in comments and strings" on (the default), the
/// in-place rename of a local put carets on same-named string literals:
/// typing the new name also changed the request key and the dict key.
#[test]
fn inplace_rename_of_a_local_leaves_string_literals_alone() {
    let src =
        "def handler(request):\n    name = request.args[\"name\"]\n    return {\"name\": name}\n";
    let (db, paths) = setup(&[("h.py", src)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let at = TextSize::new(src.find("name =").unwrap() as u32);
    let r = inplace_rename(&db, &f, at, &paths, true).unwrap();
    assert_eq!(
        lines(src, &r.ranges),
        vec![2, 3],
        "only the variable, not the keys: {r:?}"
    );
    assert_eq!(words(src, &r.ranges), vec!["name", "name"]);
    assert_eq!(r.strings_left_out, 2);
    // The dialog's preview still offers them.
    let p = rename_plan(
        &db,
        &f,
        at,
        "label",
        &paths,
        RenameOptions {
            search_in_comments: true,
        },
    )
    .unwrap();
    assert_eq!(
        lines(src, &p.text.iter().map(|u| u.range).collect::<Vec<_>>()),
        vec![2, 3]
    );
    // Its docstring and comments are renamed with a local; string
    // literals, f-string text included, are not.
    let src = "def greet(name):\n    \"\"\"Greet name.\"\"\"\n    # name may be empty\n    print(f\"name: {name}\", \"name\")\n";
    let (db, paths) = setup(&[("g.py", src)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let at = TextSize::new(src.find("name)").unwrap() as u32);
    let r = inplace_rename(&db, &f, at, &paths, true).unwrap();
    assert_eq!(lines(src, &r.ranges), vec![1, 2, 3, 4], "{r:?}");
    assert_eq!(
        usize::from(r.ranges[3].start()),
        src.find("{name}").unwrap() + 1,
        "the f-string's code, not its text"
    );
    assert_eq!(r.strings_left_out, 2);
    let r = inplace_rename(&db, &f, at, &paths, false).unwrap();
    assert_eq!((lines(src, &r.ranges), r.strings_left_out), (vec![1, 4], 0));
    // A module-level name a string may name (`__all__`): the dialog
    // previews the string.
    let src = "__all__ = [\"helper\"]\n\n\ndef helper():\n    \"\"\"The helper.\"\"\"\n\n\nhelper()  # helper\n";
    let (db, paths) = setup(&[("m.py", src)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let at = TextSize::new(src.find("helper():").unwrap() as u32);
    assert_eq!(inplace_rename(&db, &f, at, &paths, true), None);
    assert_eq!(
        inplace_rename_ranges(&db, &f, at, &paths, false).map(|r| lines(src, &r)),
        Some(vec![4, 8])
    );
    // Without such a string: in place, docstring and comment included.
    let src = "def helper():\n    \"\"\"The helper.\"\"\"\n\n\nhelper()  # helper\n";
    let (db, paths) = setup(&[("m.py", src)]);
    let f = db.file_for_path(&paths[0]).unwrap();
    let r = inplace_rename(&db, &f, TextSize::new(4), &paths, true).unwrap();
    assert_eq!(
        (lines(src, &r.ranges), r.strings_left_out),
        (vec![1, 2, 5, 5], 0)
    );
}

/// Review: a receiver typed `A | B`, where `B` has its own `run`, was a
/// certain usage of `A.run`: Rename rewrote `x.run()` without a preview,
/// and a `B` then failed with AttributeError.
#[test]
fn a_union_with_another_class_that_has_the_member_is_a_dynamic_usage() {
    let src = "class A:\n    def run(self):\n        return 1\n\n\nclass B:\n    def run(self):\n        return 2\n\n\nclass C:\n    pass\n\n\ndef go(x: A | B, y: A | None, z: A | C):\n    x.run()\n    y.run()\n    z.run()\n";
    let (db, paths) = setup(&[("u.py", src)]);
    let p = plan(
        &db,
        &paths,
        0,
        src,
        "run(self):\n        return 1",
        "execute",
        false,
    )
    .unwrap();
    // `A | None` and `A | C` (C has no `run`) can only mean A.run.
    assert_eq!(
        lines(src, &p.code.iter().map(|u| u.range).collect::<Vec<_>>()),
        vec![2, 17, 18],
        "{:?}",
        p.code
    );
    // `A | B`: maybe B.run.
    assert_eq!(
        lines(src, &p.dynamic.iter().map(|u| u.range).collect::<Vec<_>>()),
        vec![16],
        "{:?}",
        p.dynamic
    );
    assert!(p.needs_preview());
    // The plain rename leaves it alone, and in place is not offered.
    let f = db.file_for_path(&paths[0]).unwrap();
    let at = TextSize::new(src.find("run(self):\n        return 1").unwrap() as u32);
    let out = apply(
        src,
        edits_for(&rename(&db, &f, at, "execute", &paths).unwrap(), "u.py"),
    );
    assert!(
        out.contains("    x.run()\n    y.execute()\n    z.execute()\n"),
        "{out}"
    );
    assert_eq!(inplace_rename_ranges(&db, &f, at, &paths, false), None);
    // B.run, the other way round.
    let p = plan(
        &db,
        &paths,
        0,
        src,
        "run(self):\n        return 2",
        "execute",
        false,
    )
    .unwrap();
    assert_eq!(
        lines(src, &p.code.iter().map(|u| u.range).collect::<Vec<_>>()),
        vec![7]
    );
    assert_eq!(
        lines(src, &p.dynamic.iter().map(|u| u.range).collect::<Vec<_>>()),
        vec![16]
    );
}

#[test]
fn source_stub_rename_couples_only_same_qualified_declaration() {
    let source="class Box:\n    def run(self, value):\n        return value\nclass Other:\n    def run(self): pass\n";
    let stub="class Box:\n    def run(self, value: int) -> int: ...\nclass Other:\n    def run(self) -> None: ...\n";
    let (db, paths) = setup(&[("paired.py", source), ("paired.pyi", stub)]);
    let file = db.file_for_path(&paths[0]).unwrap();
    let plan = rename_plan(
        &db,
        &file,
        TextSize::new(source.find("run").unwrap() as u32),
        "execute",
        &paths,
        RenameOptions::default(),
    )
    .unwrap();
    let edits = rename_edits(plan.code.iter(), "execute");
    let result = apply(stub, edits_for(&edits, "paired.pyi"));
    assert!(result.contains("def execute(self, value: int)"));
    assert!(result.contains("class Other:\n    def run(self)"));
    assert!(inplace_rename(
        &db,
        &file,
        TextSize::new(source.find("run").unwrap() as u32),
        &paths,
        false
    )
    .is_none());
}
