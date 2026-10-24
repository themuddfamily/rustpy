//! Tests for the second inspection batch: create-from-usage fixes, call and
//! type checks, compatibility, code smells, security and naming.

use std::path::Path;
use std::sync::Arc;

use super::{inspect, Edit, InspectionOptions, Problem};
use rustpy_semantic::db::{Analysis, FileInfo};

fn setup(src: &str) -> (Analysis, Arc<FileInfo>) {
    let db = Analysis::for_project(Path::new("/proj"));
    let id = db.set_file_text(Path::new("/proj/m.py"), src);
    let f = db.file(id).unwrap();
    (db, f)
}

fn apply(text: &str, edits: &[Edit]) -> String {
    let mut edits = edits.to_vec();
    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start()));
    let mut out = text.to_string();
    for (r, rep) in edits {
        out.replace_range(usize::from(r.start())..usize::from(r.end()), &rep);
    }
    out
}

fn run(src: &str) -> Vec<Problem> {
    let (db, f) = setup(src);
    inspect(&db, &f, &InspectionOptions::default())
}

fn run_with(src: &str, opts: InspectionOptions) -> Vec<Problem> {
    let (db, f) = setup(src);
    inspect(&db, &f, &opts)
}

fn dump(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(|p| {
            format!(
                "{} {:?} {} [{}]",
                p.diagnostic.code,
                p.diagnostic.range,
                p.diagnostic.message,
                p.fixes
                    .iter()
                    .map(|f| f.title.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn messages(problems: &[Problem], code: &str) -> Vec<String> {
    problems
        .iter()
        .filter(|p| p.diagnostic.code == code)
        .map(|p| p.diagnostic.message.clone())
        .collect()
}

fn has(problems: &[Problem], code: &str, msg_part: &str) -> bool {
    problems
        .iter()
        .any(|p| p.diagnostic.code == code && p.diagnostic.message.contains(msg_part))
}

/// Applies the first fix whose title starts with `title` on a problem with `code`.
fn fixed(src: &str, problems: &[Problem], code: &str, title: &str) -> String {
    let fix = problems
        .iter()
        .filter(|p| p.diagnostic.code == code)
        .flat_map(|p| p.fixes.iter())
        .find(|f| f.title.starts_with(title))
        .unwrap_or_else(|| panic!("no fix '{title}' for {code} in:\n{}", dump(problems)));
    apply(src, &fix.edits)
}

// ---------------------------------------------------------------------------
// Create from usage

#[test]
fn create_function_parameter_and_local() {
    let src = "def main():\n    result = compute(1, x)\n    return result\n";
    let p = run(src);
    let f = fixed(
        src,
        &p,
        "PyUnresolvedReferences",
        "Create function 'compute'",
    );
    assert_eq!(f, "def main():\n    result = compute(1, x)\n    return result\n\n\ndef compute(arg1, x):\n    pass\n", "{f}");
    let f = fixed(src, &p, "PyUnresolvedReferences", "Create parameter 'x'");
    assert!(f.starts_with("def main(x):\n"), "{f}");
    let f = fixed(
        src,
        &p,
        "PyUnresolvedReferences",
        "Create local variable 'x'",
    );
    assert_eq!(
        f, "def main():\n    x = None\n    result = compute(1, x)\n    return result\n",
        "{f}"
    );
}

#[test]
fn create_class_before_module_level_usage() {
    let src = "import os\n\np = Point(1, y=2)\n";
    let p = run(src);
    let f = fixed(src, &p, "PyUnresolvedReferences", "Create class 'Point'");
    assert_eq!(f, "import os\n\n\nclass Point:\n    def __init__(self, arg1, y):\n        self.arg1 = arg1\n        self.y = y\n\n\np = Point(1, y=2)\n", "{f}");
    let src2 = "def f(a=1):\n    return q\n";
    let p2 = run(src2);
    let f = fixed(src2, &p2, "PyUnresolvedReferences", "Create parameter 'q'");
    assert!(f.starts_with("def f(a=1, q=None):"), "{f}");
    let src3 = "value = unknown_name\n";
    let p3 = run(src3);
    let f = fixed(
        src3,
        &p3,
        "PyUnresolvedReferences",
        "Create variable 'unknown_name'",
    );
    assert_eq!(f, "unknown_name = None\nvalue = unknown_name\n");
}

#[test]
fn create_attribute_and_method_on_self() {
    let src = "class A:\n    def __init__(self):\n        self.x = 1\n\n    def m(self):\n        return self.y + self.helper(2)\n";
    let p = run(src);
    let f = fixed(
        src,
        &p,
        "PyUnresolvedReferences",
        "Create attribute 'self.y' in __init__",
    );
    assert_eq!(f, "class A:\n    def __init__(self):\n        self.x = 1\n        self.y = None\n\n    def m(self):\n        return self.y + self.helper(2)\n", "{f}");
    let f = fixed(src, &p, "PyUnresolvedReferences", "Create method 'helper'");
    assert_eq!(f, "class A:\n    def __init__(self):\n        self.x = 1\n\n    def m(self):\n        return self.y + self.helper(2)\n\n    def helper(self, arg1):\n        pass\n", "{f}");
    // Without __init__, one is created at the top of the class body.
    let src2 = "class B:\n    def m(self):\n        return self.z\n";
    let p2 = run(src2);
    let f = fixed(
        src2,
        &p2,
        "PyUnresolvedReferences",
        "Create attribute 'self.z' in __init__",
    );
    assert_eq!(f, "class B:\n    def __init__(self):\n        self.z = None\n\n    def m(self):\n        return self.z\n", "{f}");
}

// ---------------------------------------------------------------------------
// Call arguments and types

#[test]
fn call_argument_errors() {
    let src = "def f(a, b, c=1):\n    return a\n\nf(1)\nf(1, 2, 3, 4)\nf(1, 2, d=3)\nf(1, 2, a=5)\nf(*args)\nf(**kw)\n";
    let p = run(src);
    let m = messages(&p, "PyArgumentList");
    assert!(
        m.contains(&"Parameter 'b' unfilled".to_string()),
        "{}",
        dump(&p)
    );
    assert!(
        m.contains(&"Unexpected argument".to_string()),
        "{}",
        dump(&p)
    );
    assert!(
        m.contains(&"Unexpected argument 'd'".to_string()),
        "{}",
        dump(&p)
    );
    assert!(
        m.contains(&"Multiple values for parameter 'a'".to_string()),
        "{}",
        dump(&p)
    );
    assert_eq!(m.len(), 4, "{}", dump(&p));
    let src2 = "class P:\n    def __init__(self, a, b):\n        self.a = a\n\nP(1)\nP(1, 2)\n\ndef k(x, /, y, *, z):\n    pass\n\nk(x=1, y=2, z=3)\nk(1, 2)\n";
    let p2 = run(src2);
    let m2 = messages(&p2, "PyArgumentList");
    assert!(
        m2.contains(&"Parameter 'b' unfilled".to_string()),
        "{}",
        dump(&p2)
    );
    assert!(
        m2.iter()
            .any(|m| m.contains("Positional-only parameter 'x'")),
        "{}",
        dump(&p2)
    );
    assert!(
        m2.contains(&"Parameter 'z' unfilled".to_string()),
        "{}",
        dump(&p2)
    );
}

#[test]
fn type_mismatches_and_no_false_positives() {
    let src = "def g(n: int, s: str) -> int:\n    return 'x'\n\ng('a', 2)\nz: int = 'q'\n";
    let p = run(src);
    let m = messages(&p, "PyTypeChecker");
    assert_eq!(
        m.iter()
            .filter(|m| *m == "Expected type 'int', got 'str' instead")
            .count(),
        3,
        "{}",
        dump(&p)
    );
    assert!(
        m.contains(&"Expected type 'str', got 'int' instead".to_string()),
        "{}",
        dump(&p)
    );
    let ok = "from typing import Optional\n\nclass Base:\n    pass\n\nclass Sub(Base):\n    pass\n\ndef h(x: float, y: Optional[int], z: object, b: Base, c: bytes, it: list[int]) -> Optional[int]:\n    return None\n\nh(1, None, 'a', Sub(), bytearray(), [1])\nh(True, 2, 3, Base(), b'x', [])\n\ndef r() -> int:\n    return 1\n";
    let p2 = run(ok);
    assert!(messages(&p2, "PyTypeChecker").is_empty(), "{}", dump(&p2));
    let bad = "def q(x: int) -> str:\n    return\n\nq(None)\n";
    let p3 = run(bad);
    let m3 = messages(&p3, "PyTypeChecker");
    assert!(
        m3.contains(&"Expected type 'str', got 'None' instead".to_string()),
        "{}",
        dump(&p3)
    );
    assert!(
        m3.contains(&"Expected type 'int', got 'None' instead".to_string()),
        "{}",
        dump(&p3)
    );
}

#[test]
fn none_attribute_access() {
    let src = "x = None\nprint(x.foo)\ny = None\ny = 1\nprint(y.real)\n";
    let p = run(src);
    assert!(
        has(
            &p,
            "PyUnresolvedReferences",
            "Cannot find reference 'foo' in 'None'"
        ),
        "{}",
        dump(&p)
    );
    assert!(!has(&p, "PyUnresolvedReferences", "'real'"), "{}", dump(&p));
}

// ---------------------------------------------------------------------------
// Compatibility

#[test]
fn version_compatibility() {
    let src = "match x:\n    case 1:\n        pass\nif (n := 1):\n    pass\ndef f(a, /):\n    pass\ntype A = int\ndef g[T](x: T):\n    pass\ntry:\n    pass\nexcept* ValueError:\n    pass\n";
    let p = run_with(
        src,
        InspectionOptions {
            python_version: (3, 7),
            ..Default::default()
        },
    );
    let m = messages(&p, "PyCompatibility");
    for what in [
        "'match' statement",
        "assignment expressions",
        "positional-only",
        "'type' alias",
        "type parameter",
        "'except*'",
    ] {
        assert!(
            m.iter().any(|x| x.contains(what)),
            "missing {what}:\n{}",
            dump(&p)
        );
    }
    assert!(
        m.iter()
            .all(|x| x.starts_with("Python version 3.7 does not support")),
        "{m:?}"
    );
    let p = run(src);
    assert!(messages(&p, "PyCompatibility").is_empty(), "{}", dump(&p));
}

#[test]
fn python2_syntax() {
    let src = "print \"hello\"  # greet\nif a <> b:\n    pass\nx = `y`\n";
    let p = run(src);
    let f = fixed(src, &p, "PyCompatibility", "Convert to print() call");
    assert!(f.starts_with("print(\"hello\")  # greet\n"), "{f}");
    let f = fixed(src, &p, "syntax", "Replace with '!='");
    assert!(f.contains("if a != b:"), "{f}");
    assert!(has(&p, "PyCompatibility", "backquotes"), "{}", dump(&p));
}

// ---------------------------------------------------------------------------
// Code smells

#[test]
fn redeclaration_and_unreachable() {
    let src = "def f():\n    pass\n\ndef f():\n    pass\n\ndef g():\n    return 1\n    x = 2\n";
    let p = run(src);
    assert!(
        has(
            &p,
            "PyRedeclaration",
            "Redeclared 'f' defined above without usage"
        ),
        "{}",
        dump(&p)
    );
    assert!(has(&p, "PyUnreachableCode", "unreachable"), "{}", dump(&p));
    let used = "def f():\n    pass\n\nf()\n\ndef f():\n    pass\n\nclass A:\n    @property\n    def v(self):\n        return 1\n\n    @v.setter\n    def v(self, x):\n        pass\n";
    let p2 = run(used);
    assert!(messages(&p2, "PyRedeclaration").is_empty(), "{}", dump(&p2));
}

#[test]
fn global_and_nonlocal() {
    let src = "global x\n\ndef f():\n    global undefined_thing\n    nonlocal y\n\nnonlocal z\n\ndef outer():\n    v = 1\n    def inner():\n        nonlocal v\n        v = 2\n    return inner\n";
    let p = run(src);
    let f = fixed(
        src,
        &p,
        "PyGlobalAtModuleLevel",
        "Remove 'global' statement",
    );
    assert!(f.starts_with("\ndef f():"), "{f}");
    assert!(
        has(&p, "PyGlobalUndefined", "'undefined_thing' is undefined"),
        "{}",
        dump(&p)
    );
    assert!(
        has(&p, "PyNonlocalDeclaration", "No binding for nonlocal 'y'"),
        "{}",
        dump(&p)
    );
    assert!(
        has(&p, "PyNonlocalDeclaration", "not allowed at module level"),
        "{}",
        dump(&p)
    );
    assert_eq!(
        messages(&p, "PyNonlocalDeclaration").len(),
        2,
        "{}",
        dump(&p)
    );
}

#[test]
fn shadowing_builtins() {
    let src = "def f(list):\n    id = 1\n    return list, id\n";
    let p = run(src);
    let m = messages(&p, "PyShadowingBuiltins");
    assert!(
        m.contains(&"Shadows built-in name 'list'".to_string())
            && m.contains(&"Shadows built-in name 'id'".to_string()),
        "{}",
        dump(&p)
    );
}

#[test]
fn simplifiable_expressions() {
    let src = "def f(a, b, c, x):\n    if not a in b:\n        pass\n    if a < b and b < c:\n        pass\n    if (x):\n        return (x)\n    if len(x) == 0:\n        pass\n    if x is 'a':\n        pass\n    return list([i for i in b]), set(i for i in b)\n";
    let p = run(src);
    assert!(
        fixed(src, &p, "PyNegatedIn", "Replace with").contains("if a not in b:"),
        "{}",
        dump(&p)
    );
    assert!(
        fixed(src, &p, "PyChainedComparisons", "Replace with").contains("if a < b < c:"),
        "{}",
        dump(&p)
    );
    let parens = fixed(src, &p, "PyRedundantParentheses", "Remove redundant");
    assert!(
        parens.contains("if x:\n") || parens.contains("return x\n"),
        "{parens}"
    );
    assert_eq!(
        messages(&p, "PyRedundantParentheses").len(),
        2,
        "{}",
        dump(&p)
    );
    assert!(
        fixed(src, &p, "PyLenZeroCheck", "Replace with 'not x'").contains("if not x:"),
        "{}",
        dump(&p)
    );
    assert!(
        fixed(src, &p, "PyIsLiteral", "Replace with '=='").contains("if x == 'a':"),
        "{}",
        dump(&p)
    );
    let f = fixed(
        src,
        &p,
        "PyComprehensionInCollectionCall",
        "Replace with comprehension",
    );
    assert!(
        f.contains("return [i for i in b], set(i for i in b)")
            || f.contains("list([i for i in b]), {i for i in b}"),
        "{f}"
    );
    assert_eq!(
        messages(&p, "PyComprehensionInCollectionCall").len(),
        2,
        "{}",
        dump(&p)
    );
}

#[test]
fn mutable_default_and_bare_except() {
    let src = "def f(x=[]):\n    return x\n\ntry:\n    pass\nexcept:\n    pass\n";
    let p = run(src);
    let f = fixed(src, &p, "PyDefaultArgument", "Replace with None");
    assert!(
        f.starts_with("def f(x=None):\n    if x is None:\n        x = []\n    return x\n"),
        "{f}"
    );
    let f = fixed(
        src,
        &p,
        "PyBroadException",
        "Replace with 'except Exception:'",
    );
    assert!(f.contains("except Exception:\n"), "{f}");
}

#[test]
fn assert_tuple_and_raise_not_implemented() {
    let src = "def f(x):\n    assert (x, 'msg')\n    raise NotImplemented\n";
    let p = run(src);
    assert!(
        fixed(src, &p, "PyAssertTuple", "Remove parentheses").contains("assert x, 'msg'"),
        "{}",
        dump(&p)
    );
    assert!(
        fixed(src, &p, "PyRaiseNotImplemented", "Replace with")
            .contains("raise NotImplementedError"),
        "{}",
        dump(&p)
    );
}

#[test]
fn method_checks() {
    let src = "class A:\n    def m(self, v):\n        return v * 2\n\n    def n():\n        pass\n\n    def o(this):\n        return this\n\n    def __init__(self):\n        return 1\n\n    def uses(self):\n        return self.m(1)\n";
    let p = run(src);
    let f = fixed(src, &p, "PyMethodMayBeStatic", "Make method static");
    assert!(
        f.starts_with("class A:\n    @staticmethod\n    def m(v):\n        return v * 2\n"),
        "{f}"
    );
    assert_eq!(messages(&p, "PyMethodMayBeStatic").len(), 1, "{}", dump(&p));
    assert!(
        has(
            &p,
            "PyMethodParameters",
            "Method must have a first parameter"
        ),
        "{}",
        dump(&p)
    );
    assert!(
        fixed(src, &p, "PyMethodParameters", "Add 'self' parameter").contains("def n(self):"),
        "{}",
        dump(&p)
    );
    assert!(
        has(
            &p,
            "PyMethodParameters",
            "Usually first parameter of a method is named 'self'"
        ),
        "{}",
        dump(&p)
    );
    assert!(
        has(
            &p,
            "PyReturnFromInit",
            "Cannot return a value from __init__"
        ),
        "{}",
        dump(&p)
    );
}

#[test]
fn missing_super_init() {
    let src = "class B:\n    def __init__(self):\n        self.a = 1\n\n\nclass C(B):\n    def __init__(self):\n        self.b = 2\n\n\nclass D(B):\n    def __init__(self):\n        super().__init__()\n        self.c = 3\n";
    let p = run(src);
    assert_eq!(
        messages(&p, "PyMissingConstructor").len(),
        1,
        "{}",
        dump(&p)
    );
    let f = fixed(
        src,
        &p,
        "PyMissingConstructor",
        "Add 'super().__init__()' call",
    );
    assert!(f.contains("class C(B):\n    def __init__(self):\n        super().__init__()\n        self.b = 2\n"), "{f}");
}

#[test]
fn duplicate_keys_and_items() {
    let src = "d = {'a': 1, 'a': 2, 1: 3, 1_0: 4, 10: 5}\ns = {1, 1, 'x'}\n";
    let p = run(src);
    assert_eq!(messages(&p, "PyDictDuplicateKeys").len(), 2, "{}", dump(&p));
    assert_eq!(messages(&p, "PySetDuplicateItems").len(), 1, "{}", dump(&p));
}

#[test]
fn function_in_condition_and_statement_effect() {
    let src = "def f():\n    pass\n\n\nif f:\n    pass\n\n\ndef g(x):\n    x == 1\n    x.y\n    x.z()\n    '''doc-ish'''\n    ...\n";
    let p = run(src);
    assert!(
        has(&p, "PyFunctionInCondition", "Function 'f' is always true"),
        "{}",
        dump(&p)
    );
    assert_eq!(messages(&p, "PyStatementEffect").len(), 2, "{}", dump(&p));
    let f = fixed(src, &p, "PyStatementEffect", "Remove statement");
    assert!(!f.contains("x == 1"), "{f}");
}

#[test]
fn misplaced_control_flow() {
    let src = "yield 1\nreturn 2\nawait x\nbreak\nfor i in x:\n    pass\nelse:\n    continue\nwhile True:\n    break\n\ndef f():\n    await g()\n\nasync def h():\n    [await g() for _ in y]\n    return (yield)\n\nclass K:\n    return 3\n";
    let p = run(src);
    assert!(
        has(&p, "PyYieldOutsideFunction", "'yield' outside function"),
        "{}",
        dump(&p)
    );
    assert_eq!(
        messages(&p, "PyYieldOutsideFunction").len(),
        1,
        "{}",
        dump(&p)
    );
    assert_eq!(
        messages(&p, "PyReturnOutsideFunction").len(),
        2,
        "{}",
        dump(&p)
    );
    let awaits = messages(&p, "PyAwaitOutsideAsync");
    assert_eq!(awaits.len(), 2, "{}", dump(&p));
    assert!(
        awaits.contains(&"'await' outside async function".to_string())
            && awaits.contains(&"'await' outside function".to_string()),
        "{awaits:?}"
    );
    let loops = messages(&p, "PyLoopControlOutsideLoop");
    assert_eq!(loops.len(), 2, "{}", dump(&p));
    assert!(
        loops.contains(&"'break' outside loop".to_string())
            && loops.contains(&"'continue' outside loop".to_string()),
        "{loops:?}"
    );
}

#[test]
fn parameter_and_argument_lists() {
    let src = "def f(a=1, b):\n    pass\n\ndef g(a, a):\n    pass\n\ndef h(a, *):\n    pass\n\ndef ok(a, *, b):\n    pass\n\nf(a=1, 2)\nf(**k, *a)\nf(a=1, a=2)\nf(*a, 1)\nf(**k, b=1)\n";
    let p = run(src);
    let pm = messages(&p, "PyParameterList");
    assert!(
        pm.contains(&"Non-default parameter follows default parameter".to_string()),
        "{}",
        dump(&p)
    );
    assert!(
        pm.contains(&"Duplicate parameter 'a'".to_string()),
        "{}",
        dump(&p)
    );
    assert!(
        pm.contains(&"Named arguments must follow bare '*'".to_string()),
        "{}",
        dump(&p)
    );
    assert_eq!(pm.len(), 3, "{}", dump(&p));
    let am = messages(&p, "PyArgumentList");
    assert!(
        am.contains(&"Positional argument after keyword argument".to_string()),
        "{}",
        dump(&p)
    );
    assert!(
        am.contains(&"Iterable argument unpacking follows keyword argument unpacking".to_string()),
        "{}",
        dump(&p)
    );
    assert!(
        am.contains(&"Keyword argument repeated: 'a'".to_string()),
        "{}",
        dump(&p)
    );
    assert_eq!(
        am.iter()
            .filter(|m| !m.contains("unfilled") && !m.starts_with("Unexpected"))
            .count(),
        3,
        "{}",
        dump(&p)
    );
}

#[test]
fn future_import_and_dunder_all() {
    let src = "\"\"\"doc\"\"\"\nfrom __future__ import annotations\nimport os\nfrom __future__ import division\n__all__ = ['f', 'missing']\n\n\ndef f():\n    pass\n";
    let p = run(src);
    assert_eq!(messages(&p, "PyFromFutureImport").len(), 1, "{}", dump(&p));
    assert!(
        has(
            &p,
            "PyUnresolvedReferences",
            "Unresolved reference 'missing' in __all__"
        ),
        "{}",
        dump(&p)
    );
    assert!(
        !has(&p, "PyUnresolvedReferences", "'f' in __all__"),
        "{}",
        dump(&p)
    );
}

// ---------------------------------------------------------------------------
// Security and naming

#[test]
fn security_checks() {
    let src = "import os\nimport pickle\nimport subprocess\nimport tempfile\nimport yaml\n\npassword = 'hunter2'\ntoken_header = 'X-Token'\nempty_secret = ''\neval(cmd)\neval('1 + 1')\nsubprocess.run(cmd, shell=True)\nsubprocess.run(['ls'])\npickle.loads(data)\nyaml.load(s)\nyaml.load(s, Loader=yaml.SafeLoader)\ntempfile.mktemp()\nos.system('ls')\nconnect(password='abc')\n";
    let p = run(src);
    for code in [
        "PySecurityEval",
        "PySecurityShellInjection",
        "PySecurityUnsafeDeserialization",
        "PySecurityTempfile",
        "PySecurityOsSystem",
        "PySecurityHardcodedSecret",
    ] {
        assert!(
            p.iter().any(|x| x.diagnostic.code == code),
            "missing {code}:\n{}",
            dump(&p)
        );
    }
    assert_eq!(messages(&p, "PySecurityEval").len(), 1, "{}", dump(&p));
    assert_eq!(
        messages(&p, "PySecurityShellInjection").len(),
        1,
        "{}",
        dump(&p)
    );
    assert_eq!(
        messages(&p, "PySecurityUnsafeDeserialization").len(),
        2,
        "{}",
        dump(&p)
    );
    assert_eq!(
        messages(&p, "PySecurityHardcodedSecret").len(),
        2,
        "{}",
        dump(&p)
    );
    let off = run_with(
        src,
        InspectionOptions {
            security: false,
            ..Default::default()
        },
    );
    assert!(!off
        .iter()
        .any(|x| x.diagnostic.code.starts_with("PySecurity")));
}

#[test]
fn naming_and_docstrings() {
    let src = "from collections import namedtuple\n\ndef badName(Arg):\n    MyVar = 1\n    Point = namedtuple('Point', 'x y')\n    return MyVar, Point\n\n\nclass my_class:\n    def setUp(self):\n        pass\n\n\nclass Base:\n    def setUp(self):\n        pass\n\n\nclass Derived(Base):\n    def setUp(self):\n        pass\n";
    let p = run(src);
    let m = messages(&p, "PyPep8Naming");
    assert!(
        m.contains(&"Function name should be lowercase".to_string()),
        "{}",
        dump(&p)
    );
    assert!(
        m.contains(&"Argument name should be lowercase".to_string()),
        "{}",
        dump(&p)
    );
    assert!(
        m.contains(&"Variable in function should be lowercase".to_string()),
        "{}",
        dump(&p)
    );
    assert!(
        m.contains(&"Class names should use CapWords convention".to_string()),
        "{}",
        dump(&p)
    );
    // Both base declarations are flagged; Derived.setUp has a real inherited
    // member and retains that spelling. The imported namedtuple makes Point an
    // actual type alias, not an unresolved uppercase-valued variable.
    assert_eq!(
        m.iter()
            .filter(|x| *x == "Function name should be lowercase")
            .count(),
        3,
        "{}",
        dump(&p)
    );
    assert_eq!(
        m.iter()
            .filter(|x| *x == "Variable in function should be lowercase")
            .count(),
        1,
        "{}",
        dump(&p)
    );
    let inherited = src.rfind("setUp").unwrap();
    assert!(!p
        .iter()
        .any(|problem| problem.diagnostic.code == "PyPep8Naming"
            && usize::from(problem.diagnostic.range.start()) == inherited));
    let point = src.find("Point =").unwrap();
    assert!(!p
        .iter()
        .any(|problem| problem.diagnostic.code == "PyPep8Naming"
            && usize::from(problem.diagnostic.range.start()) == point));
    let src2 = "import os\n\n\ndef public():\n    return os\n\n\ndef _private():\n    pass\n\n\nclass Thing:\n    \"\"\"Documented.\"\"\"\n";
    let p2 = run_with(
        src2,
        InspectionOptions {
            missing_docstrings: true,
            naming: false,
            pep8: false,
            ..Default::default()
        },
    );
    let m2 = messages(&p2, "PyMissingOrEmptyDocstring");
    assert_eq!(m2.len(), 2, "{}", dump(&p2));
    let f = fixed(
        src2,
        &p2,
        "PyMissingOrEmptyDocstring",
        "Insert docstring stub",
    );
    assert!(
        f.starts_with("\"\"\"TODO: describe this module.\"\"\"\nimport os\n")
            || f.contains(
                "def public():\n    \"\"\"TODO: describe function public.\"\"\"\n    return os\n"
            ),
        "{f}"
    );
    let none = run(src2);
    assert!(messages(&none, "PyMissingOrEmptyDocstring").is_empty());
}

/// Noise check over the system stdlib (dev tool): prints problem counts per
/// inspection code. Run with `cargo test -p rustpy-semantic stdlib_noise -- --ignored --nocapture`.
#[test]
#[ignore]
fn stdlib_noise() {
    let root = Path::new("/usr/lib/python3.12");
    if !root.is_dir() {
        return;
    }
    // A fake project root so that stdlib imports resolve to typeshed stubs
    // (as they do for real projects) rather than to the scanned sources.
    let db = Analysis::for_project(Path::new("/noise-proj"));
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut samples: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut files = 0usize;
    for entry in std::fs::read_dir(root).unwrap().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // Modules that typeshed describes with stubs would shadow those stubs when
        // loaded as project files (`_collections_abc`, `typing`, `abc`, …).
        if path.extension().and_then(|e| e.to_str()) != Some("py")
            || name.starts_with('_')
            || matches!(
                name.as_str(),
                "typing.py" | "abc.py" | "types.py" | "enum.py" | "functools.py" | "builtins.py"
            )
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let id = db.set_file_text(
            &Path::new("/noise-proj").join(path.file_name().unwrap()),
            &text,
        );
        let f = db.file(id).unwrap();
        files += 1;
        for p in inspect(
            &db,
            &f,
            &InspectionOptions {
                pep8: false,
                unused_imports: false,
                unused_variables: false,
                ..Default::default()
            },
        ) {
            let code = p.diagnostic.code.to_string();
            if code == "PyUnresolvedReferences" || code == "syntax" {
                continue;
            }
            *counts.entry(code.clone()).or_default() += 1;
            let limit = if std::env::var("NOISE_CODE").is_ok_and(|c| c == code) {
                40
            } else {
                3
            };
            let s = samples.entry(code).or_default();
            if s.len() < limit {
                let line = text[..usize::from(p.diagnostic.range.start())]
                    .matches('\n')
                    .count()
                    + 1;
                let src_line = text.lines().nth(line - 1).unwrap_or("").trim();
                s.push(format!(
                    "{}:{line}: {}  ::  {src_line}",
                    path.file_name().unwrap().to_string_lossy(),
                    p.diagnostic.message
                ));
            }
        }
    }
    println!("{files} files");
    for (code, n) in &counts {
        println!("{code}: {n}");
        for s in &samples[code] {
            println!("    {s}");
        }
    }
}

#[test]
fn options_disable_groups() {
    let src = "def f(x=[]):\n    MyVar = 1\n    return eval(x), MyVar\n";
    let p = run_with(
        src,
        InspectionOptions {
            code_smells: false,
            naming: false,
            security: false,
            type_checker: false,
            compatibility: false,
            ..Default::default()
        },
    );
    assert!(
        !p.iter().any(|x| matches!(
            x.diagnostic.code.as_str(),
            "PyDefaultArgument" | "PyPep8Naming" | "PySecurityEval"
        )),
        "{}",
        dump(&p)
    );
}

#[test]
fn non_ascii_statement_ends_do_not_panic() {
    // `end - 1` landed inside the last multi-byte character of the statement.
    for src in [
        "café\n",
        "x = 1\nnaïve\n",
        "def f():\n    résumé\n",
        "class C:\n    pass\nç = C()\nç.attr_ü\n",
    ] {
        let problems = run(src);
        let _ = dump(&problems);
    }
    assert!(
        has(&run("café\n"), "PyStatementEffect", ""),
        "{}",
        dump(&run("café\n"))
    );
}

#[test]
fn imports_binding_the_same_name_are_all_used() {
    // `import xml.dom` and `import xml.sax` both bind `xml`.
    let src = "import xml.dom\nimport xml.sax\n\nxml.dom.minidom\nxml.sax.parse\n";
    assert!(
        messages(&run(src), "PyUnusedImports").is_empty(),
        "{}",
        dump(&run(src))
    );
    let src = "try:\n    import json\nexcept ImportError:\n    import simplejson as json\n\njson.dumps({})\n";
    assert!(
        messages(&run(src), "PyUnusedImports").is_empty(),
        "{}",
        dump(&run(src))
    );
    // A forward reference in a string annotation uses the import.
    let src =
        "from decimal import Decimal\n\ndef f(x: \"Decimal\") -> \"Decimal\":\n    return x\n";
    assert!(
        messages(&run(src), "PyUnusedImports").is_empty(),
        "{}",
        dump(&run(src))
    );
    // A genuinely unused import is still reported.
    assert_eq!(
        messages(
            &run("import os\nimport sys\nsys.exit()\n"),
            "PyUnusedImports"
        )
        .len(),
        1
    );
}

/// Review (code insight 2): narrowing a name by a guard that raises gave
/// "Expected type 'Sized', got 'str'" for `len(s)` after `isinstance(s, str)`.
#[test]
fn narrowed_names_are_not_reported_as_mismatches() {
    let src = "def h(s):\n    if not isinstance(s, str):\n        raise TypeError('x')\n    return len(s)\n\n\ndef k(ctx):\n    while ctx.parent is not None:\n        ctx = ctx.parent\n    return ctx\n\n\ndef total(items: list[int]) -> int:\n    out = 0\n    for i in items:\n        out += i\n    return out\n";
    let p = run(src);
    assert!(messages(&p, "PyTypeChecker").is_empty(), "{}", dump(&p));
}
