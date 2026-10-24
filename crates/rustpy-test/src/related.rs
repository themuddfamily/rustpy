//! Go to Test / Go to Test Subject (Ctrl+Shift+T, roadmap §2.7): relates a
//! source module to its test modules by naming convention (`foo.py` ↔
//! `test_foo.py`, `foo_test.py`, `tests/test_foo.py`, `tests/<pkg>/test_foo.py`)
//! and a symbol to its tests (`class Foo` ↔ `class TestFoo`, `def bar` ↔
//! `def test_bar…`), plus the stub written by *Create New Test…*.

use std::path::{Component, Path, PathBuf};

use rustpy_syntax::ast::{self, AstNode, Stmt};
use rustpy_syntax::SyntaxToken;

use crate::discover::SKIP_DIRS;

/// A related file and, when the symbol at the caret has a counterpart there,
/// where that counterpart is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelatedTarget {
    pub path: PathBuf,
    /// 0-based line and character column of the counterpart's name.
    pub position: Option<(usize, usize)>,
    /// Qualified name of the counterpart (`TestFoo.test_bar`).
    pub symbol: Option<String>,
}

/// The module name a file stands for: `foo.py` → `foo`, `pkg/__init__.py` → `pkg`.
pub fn module_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem == "__init__" {
        return path.parent()?.file_name()?.to_str().map(str::to_owned);
    }
    Some(stem.to_string())
}

/// The module a test module tests: `test_foo.py`, `foo_test.py`,
/// `tests_foo.py` and `foo_tests.py` → `foo`.
pub fn subject_module_name(test: &Path) -> Option<String> {
    let stem = test.file_stem()?.to_str()?;
    stem.strip_prefix("test_")
        .or_else(|| stem.strip_suffix("_test"))
        .or_else(|| stem.strip_prefix("tests_"))
        .or_else(|| stem.strip_suffix("_tests"))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// True when `path` is a test module (`test_*.py`, `*_test.py`, or a module in
/// a `tests` directory; see [`crate::is_test_file`]).
pub fn is_test_module(root: &Path, path: &Path) -> bool {
    crate::is_test_file_in(root, path)
}

fn python_files(root: &Path) -> Vec<PathBuf> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .follow_links(false)
        .max_depth(Some(24))
        .filter_entry(|e| {
            !e.path()
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| SKIP_DIRS.contains(&n))
        })
        .build();
    let mut files: Vec<PathBuf> = walker
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|x| x == "py"))
        .collect();
    files.sort();
    files
}

fn dir_parts(root: &Path, file: &Path) -> Vec<String> {
    let dir = file.parent().unwrap_or(file);
    dir.strip_prefix(root)
        .unwrap_or(dir)
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str().map(str::to_owned),
            _ => None,
        })
        .collect()
}

fn is_test_dir(name: &str) -> bool {
    matches!(name, "tests" | "test" | "testing")
}

/// How far a test module is from its subject (lower is closer): the same
/// directory, then a `tests` tree mirroring the package path, then any
/// `tests` directory, then anywhere else.
fn distance(root: &Path, subject: &Path, test: &Path) -> usize {
    let a = dir_parts(root, subject);
    let b = dir_parts(root, test);
    if a == b {
        return 0;
    }
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let steps = (a.len() - common) + (b.len() - common);
    match b.iter().position(|c| is_test_dir(c)) {
        Some(t) => {
            let mut mirrored = b.clone();
            mirrored.remove(t);
            let src_less: Vec<String> = a
                .iter()
                .skip(usize::from(a.first().is_some_and(|f| f == "src")))
                .cloned()
                .collect();
            if mirrored == a || mirrored == src_less {
                1
            } else {
                2 + steps
            }
        }
        None => 20 + steps,
    }
}

/// A definition: qualified name parts, 0-based line and character column of the name.
type Def = (Vec<String>, usize, usize);

fn position_of(text: &str, tok: &SyntaxToken) -> (usize, usize) {
    let off = usize::from(tok.text_range().start()).min(text.len());
    let before = &text[..off];
    let line = before.matches('\n').count();
    let col = before.rsplit('\n').next().unwrap_or("").chars().count();
    (line, col)
}

fn collect_defs(stmts: Vec<Stmt>, prefix: &[String], text: &str, out: &mut Vec<Def>) {
    for stmt in stmts {
        match stmt {
            Stmt::FuncDef(f) => {
                if let Some(tok) = f.name() {
                    let (l, c) = position_of(text, &tok);
                    let mut path = prefix.to_vec();
                    path.push(tok.text().to_string());
                    out.push((path, l, c));
                }
            }
            Stmt::ClassDef(cls) => {
                if let Some(tok) = cls.name() {
                    let (l, c) = position_of(text, &tok);
                    let mut path = prefix.to_vec();
                    path.push(tok.text().to_string());
                    out.push((path.clone(), l, c));
                    if path.len() < 3 {
                        collect_defs(
                            cls.body()
                                .map(|b| b.statements().collect())
                                .unwrap_or_default(),
                            &path,
                            text,
                            out,
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

/// Classes, functions and methods defined in `text` (module level and class bodies).
fn definitions(text: &str) -> Vec<Def> {
    let parse = rustpy_syntax::parse(text);
    let mut out = Vec::new();
    if let Some(module) = ast::Module::cast(parse.syntax_node()) {
        collect_defs(module.statements().collect(), &[], text, &mut out);
    }
    out
}

fn is_class_name(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

fn found(d: &Def) -> (String, usize, usize) {
    (d.0.join("."), d.1, d.2)
}

/// The test for the source symbol `symbols` (outermost first, e.g. `["Foo", "bar"]`)
/// among a test module's definitions.
fn test_symbol(defs: &[Def], symbols: &[&str]) -> Option<(String, usize, usize)> {
    let func = symbols.last().filter(|s| !is_class_name(s)).copied();
    let class = symbols.iter().rev().find(|s| is_class_name(s)).copied();
    let test_classes: Vec<String> = class
        .map(|c| {
            vec![
                format!("Test{c}"),
                format!("{c}Test"),
                format!("{c}Tests"),
                format!("Test{c}s"),
            ]
        })
        .unwrap_or_default();
    let is_test_of = |name: &str, f: &str| {
        name == format!("test_{f}") || name.starts_with(&format!("test_{f}_"))
    };
    if let Some(f) = func {
        // `Foo.bar` → `TestFoo.test_bar…`, then any `test_bar…`.
        let in_class = defs
            .iter()
            .find(|d| d.0.len() == 2 && test_classes.contains(&d.0[0]) && is_test_of(&d.0[1], f));
        let exact = defs
            .iter()
            .find(|d| d.0.last().is_some_and(|n| *n == format!("test_{f}")));
        let prefixed = defs
            .iter()
            .find(|d| d.0.last().is_some_and(|n| is_test_of(n, f)));
        if let Some(d) = in_class.or(exact).or(prefixed) {
            return Some(found(d));
        }
    }
    defs.iter()
        .find(|d| d.0.len() == 1 && test_classes.contains(&d.0[0]))
        .map(found)
}

/// The tested symbol for a test symbol path (`["TestFoo", "test_bar_baz"]`)
/// among the subject module's definitions.
fn subject_symbol(defs: &[Def], symbols: &[&str]) -> Option<(String, usize, usize)> {
    let class = symbols.iter().find_map(|s| {
        let c = s
            .strip_prefix("Test")
            .filter(|r| is_class_name(r))
            .or_else(|| s.strip_suffix("Tests"))
            .or_else(|| s.strip_suffix("Test"))?;
        (!c.is_empty() && is_class_name(c)).then_some(c)
    });
    // `test_bar_baz` → `bar_baz`, then `bar`.
    let mut funcs: Vec<String> = Vec::new();
    if let Some(rest) = symbols.last().and_then(|s| s.strip_prefix("test_")) {
        let parts: Vec<&str> = rest.split('_').filter(|p| !p.is_empty()).collect();
        for n in (1..=parts.len()).rev() {
            funcs.push(parts[..n].join("_"));
        }
    }
    for f in &funcs {
        if let Some(c) = class {
            if let Some(d) = defs
                .iter()
                .find(|d| d.0.len() == 2 && d.0[0] == c && d.0[1] == *f)
            {
                return Some(found(d));
            }
        }
        if let Some(d) = defs
            .iter()
            .find(|d| d.0.len() == 1 && d.0[0] == *f)
            .or_else(|| defs.iter().find(|d| d.0.last() == Some(f)))
        {
            return Some(found(d));
        }
    }
    let c = class?;
    defs.iter()
        .find(|d| d.0.len() == 1 && d.0[0] == c)
        .map(found)
}

fn target(path: PathBuf, sym: Option<(String, usize, usize)>) -> RelatedTarget {
    match sym {
        Some((name, l, c)) => RelatedTarget {
            path,
            position: Some((l, c)),
            symbol: Some(name),
        },
        None => RelatedTarget {
            path,
            position: None,
            symbol: None,
        },
    }
}

/// Existing test modules for the source file `src`, closest first, each with
/// the test of `symbols` (the class/function path at the caret) when found.
pub fn test_candidates(root: &Path, src: &Path, symbols: &[&str]) -> Vec<RelatedTarget> {
    let Some(module) = module_name(src) else {
        return Vec::new();
    };
    let names = [
        format!("test_{module}.py"),
        format!("{module}_test.py"),
        format!("tests_{module}.py"),
        format!("{module}_tests.py"),
    ];
    let mut found: Vec<(usize, PathBuf)> = python_files(root)
        .into_iter()
        .filter(|p| {
            p != src
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| names.iter().any(|x| x == n))
        })
        .map(|p| (distance(root, src, &p), p))
        .collect();
    found.sort();
    found
        .into_iter()
        .map(|(_, p)| {
            let sym = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| test_symbol(&definitions(&t), symbols));
            target(p, sym)
        })
        .collect()
}

/// Existing modules tested by the test module `test`, closest first, each
/// with the subject of `symbols` (the test class/function at the caret) when found.
pub fn subject_candidates(root: &Path, test: &Path, symbols: &[&str]) -> Vec<RelatedTarget> {
    let Some(module) = subject_module_name(test) else {
        return Vec::new();
    };
    let mut found: Vec<(usize, PathBuf)> = python_files(root)
        .into_iter()
        .filter(|p| {
            p != test
                && module_name(p).as_deref() == Some(module.as_str())
                && !is_test_module(root, p)
        })
        .map(|p| (distance(root, &p, test), p))
        .collect();
    found.sort();
    found
        .into_iter()
        .map(|(_, p)| {
            let sym = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| subject_symbol(&definitions(&t), symbols));
            target(p, sym)
        })
        .collect()
}

/// Where *Create New Test…* puts the test for `src`: `tests/test_<module>.py`
/// when the project has a `tests` (or `test`) directory, else next to `src`.
pub fn new_test_path(root: &Path, src: &Path) -> PathBuf {
    let module = module_name(src).unwrap_or_else(|| "module".into());
    let name = format!("test_{module}.py");
    for dir in ["tests", "test"] {
        if root.join(dir).is_dir() {
            return root.join(dir).join(name);
        }
    }
    src.parent().unwrap_or(root).join(name)
}

/// Dotted import path of `src` relative to the project root (a leading `src`
/// directory is a source root, not a package).
pub fn import_path(root: &Path, src: &Path) -> String {
    let rel = src.strip_prefix(root).unwrap_or(src).with_extension("");
    let mut parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str().map(str::to_owned),
            _ => None,
        })
        .collect();
    if parts.len() > 1 && parts[0] == "src" {
        parts.remove(0);
    }
    if parts.len() > 1 && parts.last().is_some_and(|p| p == "__init__") {
        parts.pop();
    }
    parts.join(".")
}

/// Source of a new test module for `module` (dotted import path) that tests
/// `symbols` (outermost first; empty = the module itself), as pytest
/// functions/classes or a `unittest.TestCase`. Returns the text and the
/// 0-based line holding the placeholder assertion.
pub fn new_test_source(module: &str, symbols: &[&str], pytest: bool) -> (String, usize) {
    let short = module
        .rsplit('.')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("module");
    let class = symbols.first().filter(|s| is_class_name(s)).copied();
    let func = symbols.last().filter(|s| !is_class_name(s)).copied();
    let import = match symbols.first() {
        _ if module.is_empty() => None,
        Some(top) => Some(format!("from {module} import {top}")),
        None => Some(format!("import {module}")),
    };
    let test_name = format!("test_{}", func.unwrap_or(short));
    let mut out = String::new();
    if pytest {
        if let Some(i) = &import {
            out.push_str(&format!("{i}\n\n\n"));
        }
        match class {
            Some(c) => out.push_str(&format!("class Test{c}:\n    def {test_name}(self) -> None:\n        assert False  # add assertion here\n")),
            None => out.push_str(&format!("def {test_name}() -> None:\n    assert False  # add assertion here\n")),
        }
    } else {
        let case = format!(
            "Test{}",
            class.map(str::to_owned).unwrap_or_else(|| camel(short))
        );
        out.push_str("import unittest\n\n");
        match &import {
            Some(i) => out.push_str(&format!("{i}\n\n\n")),
            None => out.push('\n'),
        }
        out.push_str(&format!("class {case}(unittest.TestCase):\n    def {test_name}(self) -> None:\n        self.assertEqual(True, False)  # add assertion here\n\n\nif __name__ == \"__main__\":\n    unittest.main()\n"));
    }
    let line = out
        .lines()
        .position(|l| l.contains("# add assertion here"))
        .unwrap_or(0);
    (out, line)
}

fn camel(snake: &str) -> String {
    snake
        .split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let p = dir.path().join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir
    }

    #[test]
    fn names_map_both_ways() {
        assert_eq!(
            module_name(Path::new("/p/pkg/foo.py")).as_deref(),
            Some("foo")
        );
        assert_eq!(
            module_name(Path::new("/p/pkg/__init__.py")).as_deref(),
            Some("pkg")
        );
        for t in ["test_foo.py", "foo_test.py", "tests_foo.py", "foo_tests.py"] {
            assert_eq!(
                subject_module_name(Path::new(t)).as_deref(),
                Some("foo"),
                "{t}"
            );
        }
        assert_eq!(subject_module_name(Path::new("helpers.py")), None);
        assert_eq!(subject_module_name(Path::new("test_.py")), None);
    }

    #[test]
    fn finds_tests_for_a_source_file_and_symbol() {
        let dir = project(&[
            (
                "pkg/calc.py",
                "class Calculator:\n    def add(self, a, b):\n        return a + b\n\ndef parse(s):\n    return int(s)\n",
            ),
            (
                "tests/pkg/test_calc.py",
                "from pkg.calc import Calculator, parse\n\nclass TestCalculator:\n    def test_add_numbers(self):\n        assert Calculator().add(1, 2) == 3\n\ndef test_parse():\n    assert parse('1') == 1\n",
            ),
            ("tests/test_calc.py", "def test_other():\n    pass\n"),
            ("other/calc_test.py", "def test_x():\n    pass\n"),
            ("pkg/test_unrelated.py", ""),
        ]);
        let root = dir.path();
        let src = root.join("pkg/calc.py");
        let found = test_candidates(root, &src, &["Calculator", "add"]);
        let rel: Vec<String> = found
            .iter()
            .map(|t| {
                t.path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            rel,
            vec![
                "tests/pkg/test_calc.py",
                "tests/test_calc.py",
                "other/calc_test.py"
            ],
            "the mirrored tests tree first"
        );
        assert_eq!(
            found[0].symbol.as_deref(),
            Some("TestCalculator.test_add_numbers")
        );
        assert_eq!(found[0].position, Some((3, 8)));
        assert_eq!(
            test_candidates(root, &src, &["parse"])[0].symbol.as_deref(),
            Some("test_parse")
        );
        assert_eq!(
            test_candidates(root, &src, &["Calculator"])[0]
                .symbol
                .as_deref(),
            Some("TestCalculator")
        );
        assert_eq!(test_candidates(root, &src, &[])[0].symbol, None);
    }

    #[test]
    fn finds_the_subject_of_a_test() {
        let dir = project(&[
            (
                "src/shop/cart.py",
                "class Cart:\n    def total(self):\n        return 0\n\ndef make_cart():\n    return Cart()\n",
            ),
            (
                "tests/test_cart.py",
                "class TestCart:\n    def test_total_is_zero(self):\n        pass\n\ndef test_make_cart_works():\n    pass\n",
            ),
            ("tests/cart.py", ""),
        ]);
        let root = dir.path();
        let test = root.join("tests/test_cart.py");
        let found = subject_candidates(root, &test, &["TestCart", "test_total_is_zero"]);
        assert_eq!(
            found.len(),
            1,
            "modules in tests/ are not subjects: {found:?}"
        );
        assert!(found[0].path.ends_with("src/shop/cart.py"));
        assert_eq!(found[0].symbol.as_deref(), Some("Cart.total"));
        assert_eq!(found[0].position, Some((1, 8)));
        assert_eq!(
            subject_candidates(root, &test, &["test_make_cart_works"])[0]
                .symbol
                .as_deref(),
            Some("make_cart")
        );
        assert_eq!(
            subject_candidates(root, &test, &["TestCart"])[0]
                .symbol
                .as_deref(),
            Some("Cart")
        );
        assert!(is_test_module(root, &test));
        assert!(!is_test_module(root, &root.join("src/shop/cart.py")));
    }

    #[test]
    fn new_test_location_and_source() {
        let dir = project(&[("app/core.py", "def run():\n    pass\n")]);
        let root = dir.path();
        let src = root.join("app/core.py");
        assert_eq!(
            new_test_path(root, &src),
            root.join("app/test_core.py"),
            "next to the module without a tests dir"
        );
        std::fs::create_dir_all(root.join("tests")).unwrap();
        assert_eq!(new_test_path(root, &src), root.join("tests/test_core.py"));
        assert_eq!(import_path(root, &src), "app.core");
        assert_eq!(import_path(root, &root.join("src/lib/__init__.py")), "lib");
        let (text, line) = new_test_source("app.core", &["run"], true);
        assert_eq!(
            text,
            "from app.core import run\n\n\ndef test_run() -> None:\n    assert False  # add assertion here\n"
        );
        assert_eq!(line, 4);
        let (text, _) = new_test_source("app.core", &["Engine", "start"], true);
        assert!(
            text.contains("from app.core import Engine\n")
                && text.contains("class TestEngine:\n    def test_start(self)"),
            "{text}"
        );
        let (text, line) = new_test_source("app.core", &[], false);
        assert!(text.starts_with("import unittest\n\nimport app.core\n\n\nclass TestCore(unittest.TestCase):\n    def test_core(self)"), "{text}");
        assert!(text.lines().nth(line).unwrap().contains("assertEqual"));
        // Every generated module parses.
        for (m, s, p) in [("a.b", vec!["X", "y"], false), ("a", vec![], true)] {
            let (t, _) = new_test_source(m, &s, p);
            assert!(rustpy_syntax::parse(&t).errors().is_empty(), "{t}");
        }
    }
}
