//! Code insight regression tests: `super()` proxies, synthesized
//! constructors, decorators, typeshed precedence and version branches.

use std::path::Path;
use std::sync::Arc;

use rustpy_core::TextSize;
use rustpy_syntax::ast::{self, AstNode};

use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::features;
use rustpy_semantic::infer::Inferer;

fn setup(src: &str) -> (Analysis, Arc<FileInfo>) {
    let db = Analysis::for_project(Path::new("/proj"));
    let id = db.set_file_text(Path::new("/proj/m.py"), src);
    let f = db.file(id).unwrap();
    (db, f)
}

fn at(src: &str, needle: &str, extra: usize) -> TextSize {
    TextSize::new(
        (src.find(needle)
            .unwrap_or_else(|| panic!("needle {needle:?}"))
            + extra) as u32,
    )
}

/// Type of the expression on the last line.
fn infer_last(db: &Analysis, file: &Arc<FileInfo>) -> String {
    let inf = Inferer::new(db);
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let last = module.statements().last().unwrap();
    let ast::Stmt::Expr(e) = last else {
        panic!("last statement must be an expression")
    };
    inf.type_of_expr(file, &e.expr().unwrap()).to_string()
}

/// Type of the expression `needle` (its last occurrence) in `src`.
fn infer_at(src: &str, needle: &str) -> String {
    let (db, f) = setup(src);
    let inf = Inferer::new(&db);
    let start = TextSize::new(
        src.rfind(needle)
            .unwrap_or_else(|| panic!("needle {needle:?}")) as u32,
    );
    let end = start + TextSize::new(needle.len() as u32);
    let root = f.parse.syntax_node();
    let expr = root
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|e| e.syntax().text_range() == rustpy_core::TextRange::new(start, end))
        .unwrap_or_else(|| panic!("no expression {needle:?}"));
    inf.type_of_expr(&f, &expr).to_string()
}

fn messages(db: &Analysis, file: &Arc<FileInfo>) -> Vec<String> {
    features::diagnostics(db, file)
        .into_iter()
        .map(|d| d.message)
        .collect()
}

// ---------------------------------------------------------------------------
// super()
// ---------------------------------------------------------------------------

const SUPER_SRC: &str = "class Base:
    def __init__(self, name: str, size: int = 0) -> None:
        self.name = name

    def save(self, commit: bool = True) -> int:
        return 1

    def clone(self):
        return self

    @classmethod
    def create(cls, n: int):
        return cls('x')


class Mixin:
    def mixin_only(self) -> str:
        return ''


class Child(Mixin, Base):
    def __init__(self, name: str) -> None:
        super().__init__(name, 2)

    def save(self, commit: bool = True) -> int:
        n = super().save(commit)
        super(Child, self).save()
        super().missing_method()
        return n

    def other(self):
        return super().clone()

    @classmethod
    def create(cls, n: int):
        return super().create(n)
";

#[test]
fn super_resolves_parent_members_without_false_warnings() {
    let (db, f) = setup(SUPER_SRC);
    let msgs = messages(&db, &f);
    assert!(
        !msgs.iter().any(|m| m.contains("'save'")
            || m.contains("'__init__'")
            || m.contains("'clone'")
            || m.contains("'create'")),
        "{msgs:?}"
    );
    assert!(
        !msgs.iter().any(|m| m.contains("class 'super'")),
        "{msgs:?}"
    );
    // A genuinely missing name is still reported, against the next class in the MRO.
    assert!(
        msgs.contains(
            &"Unresolved attribute reference 'missing_method' for class 'Mixin'".to_string()
        ),
        "{msgs:?}"
    );
}

#[test]
fn super_types_bind_to_the_receiver() {
    assert_eq!(infer_at(SUPER_SRC, "super().save(commit)"), "int");
    assert_eq!(infer_at(SUPER_SRC, "super(Child, self).save()"), "int");
    // Inherited `return self` follows the receiver class.
    assert_eq!(infer_at(SUPER_SRC, "super().clone()"), "Child");
    // Class methods: `super()` is bound to `cls`.
    assert_eq!(infer_at(SUPER_SRC, "super().create(n)"), "Child");
    assert_eq!(infer_at(SUPER_SRC, "super()"), "super[Child]");
}

#[test]
fn super_completion_navigation_and_parameter_info() {
    let src = SUPER_SRC.replace("        super().missing_method()\n", "        super().\n");
    let (db, f) = setup(&src);
    let items = features::complete(&db, &f, at(&src, "super().\n", 8));
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"save")
            && labels.contains(&"clone")
            && labels.contains(&"mixin_only")
            && labels.contains(&"create"),
        "{labels:?}"
    );
    // Go to declaration: the parent's method.
    let locs = features::definition_at(&db, &f, at(&src, "super().save(commit)", 8));
    assert_eq!(locs.len(), 1);
    assert_eq!(
        usize::from(locs[0].range.start()),
        src.find("save(self, commit").unwrap()
    );
    // Parameter info shows the parent constructor without `self`.
    let h = features::signature_help(&db, &f, at(&src, "super().__init__(name", 17)).unwrap();
    assert_eq!(h.label, "__init__(name: str, size: int = 0) -> None");
    assert_eq!(h.active, Some(0));
}

#[test]
fn super_calls_are_usages_of_the_parent_method() {
    let (db, f) = setup(SUPER_SRC);
    // From the parent's definition…
    let usages =
        rustpy_semantic::usages::usages_in_file(&db, &f, at(SUPER_SRC, "save(self, commit", 0));
    let hits: Vec<usize> = usages
        .iter()
        .map(|u| usize::from(u.range.start()))
        .collect();
    assert!(
        hits.contains(&(SUPER_SRC.find("super().save(commit)").unwrap() + 8)),
        "{hits:?}"
    );
    assert!(
        hits.contains(&(SUPER_SRC.find("super(Child, self).save()").unwrap() + 19)),
        "{hits:?}"
    );
    // …and from the `super().save` reference itself.
    let sym = rustpy_semantic::usages::symbol_at(&db, &f, at(SUPER_SRC, "super().save(commit)", 9))
        .unwrap();
    match sym {
        rustpy_semantic::usages::Symbol::Member {
            decl: Some((_, r)), ..
        } => assert_eq!(
            usize::from(r.start()),
            SUPER_SRC.find("save(self, commit").unwrap()
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn super_outside_methods_and_static_methods_stays_plain() {
    // Static method: no receiver to bind to.
    assert_eq!(
        infer_at(
            "class A:\n    @staticmethod\n    def s():\n        return super()\n",
            "super()"
        ),
        "super"
    );
    // Module-level function: no enclosing class.
    assert_eq!(
        infer_at("def f(x):\n    return super()\n", "super()"),
        "super"
    );
    // A user-defined `super` is not the builtin.
    assert_eq!(
        infer_at(
            "def super():\n    return 1\n\nclass A:\n    def m(self):\n        return super()\n",
            "super()"
        ),
        "int"
    );
}

#[test]
fn unresolved_bases_suppress_attribute_warnings() {
    let src = "from not_installed_pkg import View\n\nclass Page(View):\n    def get(self):\n        self.render()\n        return super().get()\n\nclass Plain:\n    def get(self):\n        self.render()\n";
    let (db, f) = setup(src);
    let msgs = messages(&db, &f);
    assert_eq!(
        msgs.iter().filter(|m| m.contains("'render'")).count(),
        1,
        "{msgs:?}"
    );
    assert!(
        msgs.iter()
            .any(|m| m.contains("'render' for class 'Plain'")),
        "{msgs:?}"
    );
    assert!(!msgs.iter().any(|m| m.contains("'get'")), "{msgs:?}");
}

fn last_type(src: &str) -> String {
    let (db, f) = setup(src);
    infer_last(&db, &f)
}

// ---------------------------------------------------------------------------
// Generated constructors
// ---------------------------------------------------------------------------

/// Parameter Info label at the end of `src` (which ends inside a call).
fn param_info(src: &str) -> String {
    let (db, f) = setup(src);
    features::signature_help(&db, &f, TextSize::new(src.len() as u32))
        .map(|h| h.label)
        .unwrap_or_else(|| "<none>".into())
}

fn type_errors(src: &str) -> Vec<String> {
    let (db, f) = setup(src);
    let opts = crate::inspections::InspectionOptions {
        pep8: false,
        unused_imports: false,
        unused_variables: false,
        code_smells: false,
        naming: false,
        security: false,
        compatibility: false,
        ..Default::default()
    };
    crate::inspections::inspect(&db, &f, &opts)
        .into_iter()
        .filter(|p| p.diagnostic.code == "PyArgumentList" || p.diagnostic.code == "PyTypeChecker")
        .map(|p| format!("{}: {}", &src[p.diagnostic.range], p.diagnostic.message))
        .collect()
}

const DATACLASS_SRC: &str = "from dataclasses import dataclass, field, KW_ONLY, InitVar
from typing import ClassVar, Generic, TypeVar

T = TypeVar('T')


@dataclass
class Point:
    \"\"\"A point.\"\"\"
    x: int
    y: float = 0.0
    tags: list[str] = field(default_factory=list)
    hidden: int = field(default=0, init=False)
    count: ClassVar[int] = 0
    _: KW_ONLY
    label: str = ''
    scale: InitVar[float] = 1.0

    def norm(self) -> float:
        return 0.0


@dataclass
class Point3(Point):
    z: int = 0


@dataclass(kw_only=True)
class Options:
    verbose: bool
    level: int = 1


@dataclass
class Box(Generic[T]):
    item: T


class Plain(Point3):
    pass
";

#[test]
fn dataclass_constructors() {
    assert_eq!(param_info(&format!("{DATACLASS_SRC}Point(")), "Point(x: int, y: float = 0.0, tags: list[str] = <factory>, *, label: str = '', scale: float = 1.0) -> None");
    assert_eq!(param_info(&format!("{DATACLASS_SRC}Point3(")), "Point3(x: int, y: float = 0.0, tags: list[str] = <factory>, z: int = 0, *, label: str = '', scale: float = 1.0) -> None");
    assert_eq!(
        param_info(&format!("{DATACLASS_SRC}Options(")),
        "Options(*, verbose: bool, level: int = 1) -> None"
    );
    // A plain subclass inherits the generated __init__.
    assert_eq!(param_info(&format!("{DATACLASS_SRC}Plain(")), "Plain(x: int, y: float = 0.0, tags: list[str] = <factory>, z: int = 0, *, label: str = '', scale: float = 1.0) -> None");
    // Type arguments are solved from the generated parameters.
    assert_eq!(last_type(&format!("{DATACLASS_SRC}Box(1)\n")), "Box[int]");
    assert_eq!(last_type(&format!("{DATACLASS_SRC}Box('s').item\n")), "str");
    // The class docstring is the constructor documentation.
    let src = format!("{DATACLASS_SRC}Point(");
    let (db, f) = setup(&src);
    let h = features::signature_help(&db, &f, TextSize::new(src.len() as u32)).unwrap();
    assert_eq!(h.docstring.as_deref(), Some("A point."));
    assert_eq!(h.active, Some(0));
}

#[test]
fn dataclass_calls_are_checked_against_the_generated_init() {
    let errs = type_errors(&format!("{DATACLASS_SRC}Point()\nPoint(1, 2.0, [], 5)\nPoint(1, label='a', scale=2.0)\nPoint('s')\nPoint(1, hidden=2)\nOptions(True)\nOptions(verbose=True)\n"));
    assert!(
        errs.contains(&"(): Parameter 'x' unfilled".to_string()),
        "{errs:?}"
    );
    assert!(
        errs.contains(&"5: Unexpected argument".to_string()),
        "{errs:?}"
    );
    assert!(
        errs.contains(&"'s': Expected type 'int', got 'str' instead".to_string()),
        "{errs:?}"
    );
    assert!(
        errs.contains(&"hidden=2: Unexpected argument 'hidden'".to_string()),
        "{errs:?}"
    );
    assert!(
        errs.contains(&"True: Unexpected argument".to_string()),
        "{errs:?}"
    );
    assert_eq!(errs.len(), 6, "{errs:?}");
}

#[test]
fn dataclass_keyword_navigation_and_inlay_hints() {
    let src = format!(
        "{DATACLASS_SRC}Point(x=1, label='a')\nOptions(verbose=True)\nPoint3(10, 2.5, [], 4)\n"
    );
    let (db, f) = setup(&src);
    let locs = features::definition_at(&db, &f, at(&src, "label='a'", 1));
    assert_eq!(locs.len(), 1);
    assert_eq!(
        usize::from(locs[0].range.start()),
        src.find("label: str").unwrap()
    );
    let last_line = TextSize::new(src.rfind("Point3(").unwrap() as u32);
    let hints: Vec<String> = features::inlay_hints(&db, &f, true, false)
        .into_iter()
        .filter(|h| h.offset > last_line)
        .map(|h| h.label)
        .collect();
    assert_eq!(hints, vec!["tags:"], "{hints:?}");
}

#[test]
fn named_tuple_and_typed_dict_constructors() {
    let src = "from typing import NamedTuple, TypedDict, Required, NotRequired\n\nclass Employee(NamedTuple):\n    name: str\n    ident: int = 3\n\n    def describe(self) -> str:\n        return self.name\n\nclass Movie(TypedDict, total=False):\n    title: Required[str]\n    year: int\n\nclass Film(TypedDict):\n    title: str\n    rating: NotRequired[float]\n\nclass Sequel(Film):\n    prequel: str\n";
    assert_eq!(
        param_info(&format!("{src}Employee(")),
        "Employee(name: str, ident: int = 3) -> None"
    );
    assert_eq!(
        param_info(&format!("{src}Movie(")),
        "Movie(*, title: str, year: int = ...) -> None"
    );
    assert_eq!(
        param_info(&format!("{src}Film(")),
        "Film(*, title: str, rating: float = ...) -> None"
    );
    assert_eq!(
        param_info(&format!("{src}Sequel(")),
        "Sequel(*, title: str, rating: float = ..., prequel: str) -> None"
    );
    let (db, f) = setup(&format!("{src}Employee('Ann', 7)\n"));
    let hints: Vec<String> = features::inlay_hints(&db, &f, true, false)
        .into_iter()
        .map(|h| h.label)
        .collect();
    assert_eq!(hints, vec!["name:", "ident:"]);
    let errs = type_errors(&format!("{src}Employee()\nEmployee('a', 1, 2)\nMovie(title='x', rating=5)\nMovie(year=1)\nMovie({{'title': 'x'}})\nFilm(title='t', rating=1.0)\n"));
    assert!(
        errs.contains(&"(): Parameter 'name' unfilled".to_string()),
        "{errs:?}"
    );
    assert!(
        errs.contains(&"2: Unexpected argument".to_string()),
        "{errs:?}"
    );
    assert!(
        errs.contains(&"rating=5: Unexpected argument 'rating'".to_string()),
        "{errs:?}"
    );
    assert!(
        errs.contains(&"(year=1): Parameter 'title' unfilled".to_string()),
        "{errs:?}"
    );
    assert_eq!(errs.len(), 4, "{errs:?}");
}

#[test]
fn attrs_and_dataclass_transform_constructors() {
    // attrs and pydantic are not installed here: the decorators/bases are recognised by origin.
    let attrs_src = "import attr\nfrom attrs import define, field\n\n@attr.s\nclass Classic:\n    _x = attr.ib()\n    y = attr.ib(default=1)\n    z: int = 0\n\n@define\nclass Modern:\n    a: int\n    b: str = 'x'\n    c: list = field(factory=list)\n    d: int = field(default=0, init=False)\n";
    assert_eq!(
        param_info(&format!("{attrs_src}Classic(")),
        "Classic(x, y=1) -> None"
    );
    assert_eq!(
        param_info(&format!("{attrs_src}Modern(")),
        "Modern(a: int, b: str = 'x', c: list = <factory>) -> None"
    );
    let pyd_src = "from pydantic import BaseModel, Field\n\nclass User(BaseModel):\n    id: int\n    name: str = 'Jane'\n    tags: list[str] = Field(default_factory=list)\n    nick: str = Field(..., alias='nickname')\n    _secret: str = 'x'\n\nclass Admin(User):\n    level: int = 0\n";
    assert_eq!(
        param_info(&format!("{pyd_src}User(")),
        "User(*, id: int, name: str = 'Jane', tags: list[str] = <factory>, nickname: str) -> None"
    );
    assert_eq!(param_info(&format!("{pyd_src}Admin(")), "Admin(*, id: int, name: str = 'Jane', tags: list[str] = <factory>, nickname: str, level: int = 0) -> None");
    // PEP 681 through a decorator function and through a metaclass.
    let pep_src = "from typing import dataclass_transform\n\n@dataclass_transform(kw_only_default=True)\ndef model(cls):\n    return cls\n\n@model\nclass Cfg:\n    host: str\n    port: int = 80\n\n@dataclass_transform()\nclass ModelMeta(type):\n    pass\n\nclass ModelBase(metaclass=ModelMeta):\n    pass\n\nclass Item(ModelBase):\n    sku: str\n    qty: int = 1\n";
    assert_eq!(
        param_info(&format!("{pep_src}Cfg(")),
        "Cfg(*, host: str, port: int = 80) -> None"
    );
    assert_eq!(
        param_info(&format!("{pep_src}Item(")),
        "Item(sku: str, qty: int = 1) -> None"
    );
    // Model fields are display-only: no argument checks for attrs / transforms.
    assert!(type_errors(&format!(
        "{pyd_src}User(id=1, unknown=2)\n{attrs_src}Modern()\n"
    ))
    .is_empty());
}

#[test]
fn aliased_special_forms_in_fields() {
    let src = "from dataclasses import dataclass\nfrom typing import ClassVar as CV, TypedDict, NotRequired as Opt\n\n@dataclass\nclass Counter:\n    total: CV[int] = 0\n    step: int = 1\n\nclass Opts(TypedDict):\n    name: str\n    debug: Opt[bool]\n";
    assert_eq!(
        param_info(&format!("{src}Counter(")),
        "Counter(step: int = 1) -> None"
    );
    assert_eq!(
        param_info(&format!("{src}Opts(")),
        "Opts(*, name: str, debug: bool = ...) -> None"
    );
}

#[test]
fn explicit_init_wins_over_generated_one() {
    let src = "from dataclasses import dataclass\n\n@dataclass\nclass Custom:\n    a: int\n\n    def __init__(self, raw: str) -> None:\n        self.a = int(raw)\n\n@dataclass(init=False)\nclass NoInit:\n    a: int\n\n@dataclass\nclass Child(NoInit):\n    b: int = 0\n";
    assert_eq!(
        param_info(&format!("{src}Custom(")),
        "Custom(raw: str) -> None"
    );
    assert_eq!(param_info(&format!("{src}NoInit(")), "<none>");
    assert_eq!(
        param_info(&format!("{src}Child(")),
        "Child(a: int, b: int = 0) -> None"
    );
}

// ---------------------------------------------------------------------------
// Decorators
// ---------------------------------------------------------------------------

#[test]
fn context_manager_decorators_type_the_with_target() {
    let src = "from contextlib import contextmanager, asynccontextmanager\nfrom typing import Iterator, AsyncIterator\n\n@contextmanager\ndef opened(path: str) -> Iterator[int]:\n    yield 1\n\n@contextmanager\ndef plain():\n    yield 'text'\n\n@asynccontextmanager\nasync def session() -> AsyncIterator[bytes]:\n    yield b''\n\nclass Db:\n    @contextmanager\n    def tx(self, name: str) -> Iterator[float]:\n        yield 1.0\n\nasync def main():\n    async with session() as raw:\n        return raw\n\nwith opened('f') as num:\n    pass\nwith plain() as txt:\n    pass\nwith Db().tx('a') as amount:\n    pass\n";
    assert_eq!(last_type(&format!("{src}num\n")), "int");
    assert_eq!(last_type(&format!("{src}txt\n")), "str");
    assert_eq!(last_type(&format!("{src}amount\n")), "float");
    assert_eq!(
        last_type(&format!("{src}main()\n")),
        "Coroutine[Any, Any, bytes]"
    );
    // Calls keep the decorated function's parameters.
    assert_eq!(
        param_info(&format!("{src}opened(")),
        "opened(path: str) -> _GeneratorContextManager[int, None, None]"
    );
    assert!(type_errors(&format!("{src}opened('a', 2)\n"))
        .contains(&"2: Unexpected argument".to_string()));
}

#[test]
fn cached_functions_expose_cache_helpers() {
    let src = "import functools\n\n@functools.lru_cache(maxsize=None)\ndef fib(n: int) -> int:\n    return n\n\n@functools.cache\ndef table() -> dict[str, int]:\n    return {}\n\n";
    assert_eq!(last_type(&format!("{src}fib.cache_clear()\n")), "None");
    assert_eq!(last_type(&format!("{src}fib(3)\n")), "int");
    let full = format!("{src}table.ca\n");
    let (db, f) = setup(&full);
    let items = features::complete(&db, &f, TextSize::new(full.len() as u32 - 1));
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"cache_clear") && labels.contains(&"cache_info"),
        "{labels:?}"
    );
    assert_eq!(param_info(&format!("{src}fib(")), "fib(n: int) -> int");
}

// ---------------------------------------------------------------------------
// Typeshed precedence and version / platform branches
// ---------------------------------------------------------------------------

/// A project plus a fake interpreter whose sys.path holds an untyped stdlib
/// (`re.py`, `signal.py`) and a third-party package.
struct Interp {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    lib: std::path::PathBuf,
}

fn interpreter() -> Interp {
    let dir = tempfile::TempDir::with_prefix("rustpy-insight").unwrap();
    let root = dir.path().join("proj");
    let lib = dir.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(lib.join("requests")).unwrap();
    std::fs::write(lib.join("re.py"), "\"\"\"Support for regular expressions (RE).\"\"\"\n\ndef compile(pattern, flags=0):\n    \"Compile a regular expression pattern, returning a Pattern object.\"\n    return _compile(pattern, flags)\n\nclass RegexFlag:\n    \"\"\"Flags.\"\"\"\n").unwrap();
    std::fs::write(lib.join("signal.py"), "import _signal\nfrom _signal import *\n_IntEnum._convert_('Signals', __name__, lambda name: name.startswith('SIG'))\n").unwrap();
    std::fs::write(
        lib.join("requests/__init__.py"),
        "def get(url, params=None, **kwargs):\n    \"\"\"Sends a GET request.\"\"\"\n",
    )
    .unwrap();
    Interp {
        _dir: dir,
        root,
        lib,
    }
}

fn analysis_for(interp: &Interp, version: (u32, u32), platform: &str) -> Analysis {
    Analysis::new(
        rustpy_semantic::module::ModuleResolver::new(
            vec![interp.root.clone()],
            vec![interp.lib.clone()],
            version,
        )
        .with_platform(platform),
    )
}

fn open(db: &Analysis, interp: &Interp, src: &str) -> Arc<FileInfo> {
    let id = db.set_file_text(&interp.root.join("main.py"), src);
    db.file(id).unwrap()
}

#[test]
fn typeshed_stubs_win_over_interpreter_stdlib_sources() {
    let interp = interpreter();
    let db = analysis_for(&interp, (3, 12), "linux");
    let f = open(&db, &interp, "import re\nre.compile('a+')\n");
    assert_eq!(infer_last(&db, &f), "Pattern[str]");
    assert!(db.module("re").unwrap().path.starts_with("<typeshed>"));
    // Untyped packages use bundled third-party types, with runtime availability
    // retained. A package that provides inline types takes priority over them.
    assert!(db
        .module("requests")
        .unwrap()
        .path
        .starts_with("<typeshed>"));
    assert!(db.resolver.import_available("requests"));
    std::fs::write(interp.lib.join("requests/py.typed"), "").unwrap();
    db.invalidate_modules();
    assert_eq!(
        db.module("requests").unwrap().path,
        interp.lib.join("requests/__init__.py")
    );
    // `signal.SIGINT` exists in the stub (the source creates it at run time).
    let f = open(&db, &interp, "import signal\nsignal.SIGINT\n");
    assert!(messages(&db, &f).is_empty(), "{:?}", messages(&db, &f));
    // Windows-only names are not offered on Linux, but are on Windows…
    let members = |db: &Analysis| -> Vec<String> {
        let src = "import os\nimport signal\nos.st\nsignal.CT\n";
        let f = open(db, &interp, src);
        let mut labels: Vec<String> = features::complete(db, &f, at(src, "os.st", 5))
            .into_iter()
            .map(|i| i.label.to_string())
            .collect();
        labels.extend(
            features::complete(db, &f, at(src, "signal.CT", 9))
                .into_iter()
                .map(|i| i.label.to_string()),
        );
        labels
    };
    let linux = members(&db);
    assert!(
        linux.contains(&"stat".to_string())
            && !linux.contains(&"startfile".to_string())
            && !linux.contains(&"CTRL_C_EVENT".to_string()),
        "{linux:?}"
    );
    let win = analysis_for(&interp, (3, 12), "win32");
    let windows = members(&win);
    assert!(
        windows.contains(&"startfile".to_string()) && windows.contains(&"CTRL_C_EVENT".to_string()),
        "{windows:?}"
    );
    // …and code written for another platform is not reported.
    let src = "import os\nimport signal\nfrom os import startfile\nsignal.CTRL_C_EVENT\nos.startfile('x')\nos.not_a_function()\n";
    let f = open(&db, &interp, src);
    assert_eq!(
        messages(&db, &f),
        vec!["Cannot find reference 'not_a_function' in 'os'".to_string()]
    );
}

#[test]
fn statically_dead_user_code_is_not_checked() {
    let interp = interpreter();
    let db = analysis_for(&interp, (3, 11), "linux");
    let src = "import sys\nimport asyncio\n\nasync def main() -> None:\n    pass\n\nif sys.version_info >= (3, 12):\n    from asyncio import TaskGroup\n    asyncio.run(main(), loop_factory=None)\nelse:\n    asyncio.run(main())\nif sys.version_info < (3, 8):\n    import importlib_metadata\n    asyncio.not_there\n";
    let f = open(&db, &interp, src);
    assert!(messages(&db, &f).is_empty(), "{:?}", messages(&db, &f));
    let opts = crate::inspections::InspectionOptions {
        pep8: false,
        unused_imports: false,
        unused_variables: false,
        code_smells: false,
        naming: false,
        security: false,
        compatibility: false,
        ..Default::default()
    };
    let problems: Vec<String> = crate::inspections::inspect(&db, &f, &opts)
        .into_iter()
        .map(|p| p.diagnostic.message)
        .collect();
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn names_typeshed_leaves_out_are_not_errors() {
    let interp = interpreter();
    // The runtime sources define names the stubs do not declare.
    std::fs::write(
        interp.lib.join("socket.py"),
        "_GLOBAL_DEFAULT_TIMEOUT = object()\n",
    )
    .unwrap();
    std::fs::create_dir_all(interp.lib.join("urllib")).unwrap();
    std::fs::write(interp.lib.join("urllib/__init__.py"), "").unwrap();
    std::fs::write(
        interp.lib.join("urllib/request.py"),
        "def proxy_bypass_environment(host, proxies=None):\n    pass\n",
    )
    .unwrap();
    let db = analysis_for(&interp, (3, 12), "linux");
    let src = "import socket\nfrom urllib.request import proxy_bypass_environment\nsocket._GLOBAL_DEFAULT_TIMEOUT\nfrom urllib.request import not_defined_anywhere\n";
    let f = open(&db, &interp, src);
    assert_eq!(
        messages(&db, &f),
        vec!["Cannot find reference 'not_defined_anywhere' in 'urllib.request'".to_string()]
    );
}

#[test]
fn both_type_checking_branches_of_user_code_are_checked() {
    let interp = interpreter();
    let db = analysis_for(&interp, (3, 12), "linux");
    let src = "from typing import TYPE_CHECKING\nif TYPE_CHECKING:\n    import os\nelse:\n    undefined_name_here\nif not TYPE_CHECKING:\n    import not_a_module_xyz\n";
    let f = open(&db, &interp, src);
    let m = messages(&db, &f);
    assert!(
        m.contains(&"Unresolved reference 'undefined_name_here'".to_string())
            && m.contains(&"No module named 'not_a_module_xyz'".to_string()),
        "{m:?}"
    );
}

#[test]
fn stub_definitions_show_the_source_docstring() {
    let interp = interpreter();
    let db = analysis_for(&interp, (3, 12), "linux");
    let src = "import re\nre.compile('a')\nre.RegexFlag\n";
    let f = open(&db, &interp, src);
    let h = features::hover_at(&db, &f, at(src, "compile", 1)).unwrap();
    assert!(h.signature.starts_with("def compile("), "{}", h.signature);
    assert_eq!(
        h.docstring.as_deref(),
        Some("Compile a regular expression pattern, returning a Pattern object.")
    );
    let h = features::hover_at(&db, &f, at(src, "RegexFlag", 1)).unwrap();
    assert_eq!(h.docstring.as_deref(), Some("Flags."));
    let h = features::hover_at(&db, &f, at(src, "re.compile", 0)).unwrap();
    assert_eq!(
        h.docstring.as_deref(),
        Some("Support for regular expressions (RE).")
    );
    let src2 = "import re\nre.compile(";
    let f = open(&db, &interp, src2);
    let help = features::signature_help(&db, &f, TextSize::new(src2.len() as u32)).unwrap();
    assert_eq!(
        help.docstring.as_deref(),
        Some("Compile a regular expression pattern, returning a Pattern object.")
    );
}

#[test]
fn version_branches_follow_the_interpreter() {
    let interp = interpreter();
    let src = "import asyncio\n\nasync def main() -> None:\n    pass\n\nasyncio.run(main(), loop_factory=None)\n";
    let check = |version: (u32, u32)| -> Vec<String> {
        let db = analysis_for(&interp, version, "linux");
        let f = open(&db, &interp, src);
        let opts = crate::inspections::InspectionOptions {
            pep8: false,
            unused_imports: false,
            unused_variables: false,
            code_smells: false,
            naming: false,
            security: false,
            compatibility: false,
            ..Default::default()
        };
        crate::inspections::inspect(&db, &f, &opts)
            .into_iter()
            .filter(|p| p.diagnostic.code == "PyArgumentList")
            .map(|p| p.diagnostic.message)
            .collect()
    };
    assert!(check((3, 12)).is_empty(), "{:?}", check((3, 12)));
    assert_eq!(
        check((3, 11)),
        vec!["Unexpected argument 'loop_factory'".to_string()]
    );
    // Overloads come from the live branch only.
    let db = analysis_for(&interp, (3, 12), "linux");
    let src = "import subprocess\nsubprocess.run(";
    let f = open(&db, &interp, src);
    assert_eq!(
        features::signature_help(&db, &f, TextSize::new(src.len() as u32))
            .unwrap()
            .overloads,
        6
    );
    // Methods declared in version branches of a class body.
    let src = "import asyncio\nasyncio.Runner().run(";
    let f = open(&db, &interp, src);
    let help = features::signature_help(&db, &f, TextSize::new(src.len() as u32)).unwrap();
    assert_eq!(help.overloads, 1);
    assert!(
        help.label.starts_with("run(coro: Coroutine["),
        "{}",
        help.label
    );
    // Names re-exported only on newer versions.
    let old = analysis_for(&interp, (3, 10), "linux");
    let f = open(
        &old,
        &interp,
        "import asyncio\nasyncio.TaskGroup\nasyncio.run\n",
    );
    assert_eq!(
        messages(&old, &f),
        vec!["Cannot find reference 'TaskGroup' in 'asyncio'".to_string()]
    );
    let f = open(&db, &interp, "import asyncio\nasyncio.TaskGroup\n");
    assert!(messages(&db, &f).is_empty());
}

#[test]
fn completion_after_a_function_body_is_at_module_level() {
    // The blank lines after a body still belong to the function's node.
    let src = "def greet(name, excited=False):\n    return name\n\n\nex\n";
    let (db, f) = setup(src);
    let labels: Vec<String> = features::complete(&db, &f, at(src, "\nex\n", 3))
        .into_iter()
        .map(|i| i.label.to_string())
        .collect();
    assert!(!labels.contains(&"excited".to_string()), "{labels:?}");
    // Indented under the def it is still the function's scope.
    let src = "def greet(name, excited=False):\n    x = 1\n    ex\n";
    let (db, f) = setup(src);
    let labels: Vec<String> = features::complete(&db, &f, at(src, "    ex\n", 6))
        .into_iter()
        .map(|i| i.label.to_string())
        .collect();
    assert!(labels.contains(&"excited".to_string()), "{labels:?}");
}
