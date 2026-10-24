//! Static test discovery from the syntax tree.

use std::path::{Path, PathBuf};

use rustpy_syntax::ast::{self, AstNode, Expr, Stmt};
use rustpy_syntax::SyntaxToken;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TestKind {
    Module,
    Class,
    Function,
}

/// A discovered test (module, class or function/method).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestItem {
    /// pytest node id (`tests/test_x.py::TestA::test_b`), relative to the project root.
    pub id: String,
    pub name: String,
    pub kind: TestKind,
    pub path: PathBuf,
    /// 1-based line of the `def`/`class` keyword (1 for modules).
    pub line: usize,
    pub children: Vec<TestItem>,
}

impl TestItem {
    /// Depth-first list of this item and all descendants.
    pub fn walk(&self) -> Vec<&TestItem> {
        let mut out = vec![self];
        for c in &self.children {
            out.extend(c.walk());
        }
        out
    }

    /// Number of leaf tests (functions/methods) under this item.
    pub fn test_count(&self) -> usize {
        if self.kind == TestKind::Function {
            1
        } else {
            self.children.iter().map(TestItem::test_count).sum()
        }
    }
}

pub(crate) const SKIP_DIRS: &[&str] = &[
    "__pycache__",
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "venv",
    ".venv",
    "env",
    ".env",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    "build",
    "dist",
    "target",
    ".idea",
    ".rustpy",
    "site-packages",
    ".eggs",
];

/// Whether a path (relative to the project root) looks like a test module:
/// `test_*.py`, `*_test.py`, `tests.py`, or any `.py` file under a
/// `tests`/`test`/`testing` directory (excluding `conftest.py`/`__init__.py`).
pub fn is_test_file(rel: &Path) -> bool {
    let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !name.ends_with(".py") || name == "conftest.py" || name == "__init__.py" {
        return false;
    }
    if name.starts_with("test_") || name.ends_with("_test.py") || name == "tests.py" {
        return true;
    }
    rel.components()
        .rev()
        .skip(1)
        .any(|c| matches!(c.as_os_str().to_str(), Some("tests" | "test" | "testing")))
}

/// Discovers tests in a single file. The given `path` is used verbatim as the
/// node-id prefix and as [`TestItem::path`]; use [`discover_file_in`] to get
/// ids relative to a project root.
pub fn discover_file(path: &Path, text: &str) -> Vec<TestItem> {
    let prefix = path.to_string_lossy().replace('\\', "/");
    discover_with_prefix(&prefix, path, text)
}

/// Discovers tests in `path` with node ids relative to `root`.
pub fn discover_file_in(root: &Path, path: &Path, text: &str) -> Vec<TestItem> {
    discover_parsed_in(root, path, text, &rustpy_syntax::parse(text))
}

/// [`discover_file_in`] for a file that is already parsed (`parse` is the
/// parse of `text`), e.g. an open editor's syntax tree.
pub fn discover_parsed_in(
    root: &Path,
    path: &Path,
    text: &str,
    parse: &rustpy_syntax::Parse,
) -> Vec<TestItem> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let prefix = rel.to_string_lossy().replace('\\', "/");
    discover_tree_with_config(
        &prefix,
        path,
        text,
        parse,
        &crate::PytestConfiguration::load(root),
    )
}

fn discover_with_prefix(prefix: &str, path: &Path, text: &str) -> Vec<TestItem> {
    discover_tree(prefix, path, text, &rustpy_syntax::parse(text))
}

fn discover_tree(
    prefix: &str,
    path: &Path,
    text: &str,
    parse: &rustpy_syntax::Parse,
) -> Vec<TestItem> {
    discover_tree_with_config(
        prefix,
        path,
        text,
        parse,
        &crate::PytestConfiguration::default(),
    )
}
fn discover_tree_with_config(
    prefix: &str,
    path: &Path,
    text: &str,
    parse: &rustpy_syntax::Parse,
    config: &crate::PytestConfiguration,
) -> Vec<TestItem> {
    let Some(module) = ast::Module::cast(parse.syntax_node()) else {
        return Vec::new();
    };
    // Line starts, for the line of each `def` / `class` (counting newlines
    // from the start for every test made discovery quadratic in file size).
    let lines: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let mut out = Vec::new();
    for stmt in module.statements() {
        collect_stmt(&stmt, prefix, path, &lines, &mut out, config);
    }
    out
}

fn block_stmts(block: Option<ast::Block>) -> Vec<Stmt> {
    block
        .into_iter()
        .flat_map(|b| b.statements().collect::<Vec<_>>())
        .collect()
}

fn collect_stmt(
    stmt: &Stmt,
    prefix: &str,
    path: &Path,
    lines: &[usize],
    out: &mut Vec<TestItem>,
    config: &crate::PytestConfiguration,
) {
    match stmt {
        Stmt::FuncDef(f) => {
            let name = f.name_text();
            if config.matches("python_functions", &name, &["test*"]) {
                out.push(TestItem {
                    id: format!("{prefix}::{name}"),
                    name,
                    kind: TestKind::Function,
                    path: path.to_path_buf(),
                    line: line_of_token(lines, f.def_token()),
                    children: Vec::new(),
                });
            }
        }
        Stmt::ClassDef(c) => {
            if let Some(item) = collect_class(c, prefix, path, lines, config) {
                out.push(item);
            }
        }
        // Tests guarded by conditions (`if sys.platform == ...:`) are still shown.
        Stmt::If(i) => {
            for s in block_stmts(i.body()) {
                collect_stmt(&s, prefix, path, lines, out, config);
            }
            for clause in i.elif_clauses() {
                for s in block_stmts(clause.body()) {
                    collect_stmt(&s, prefix, path, lines, out, config);
                }
            }
            if let Some(e) = i.else_clause() {
                for s in block_stmts(e.body()) {
                    collect_stmt(&s, prefix, path, lines, out, config);
                }
            }
        }
        _ => {}
    }
}

fn collect_class(
    c: &ast::ClassDef,
    prefix: &str,
    path: &Path,
    lines: &[usize],
    config: &crate::PytestConfiguration,
) -> Option<TestItem> {
    if !is_test_class(c, config) {
        return None;
    }
    let name = c.name_text();
    let id = format!("{prefix}::{name}");
    let mut children = Vec::new();
    for stmt in block_stmts(c.body()) {
        match &stmt {
            Stmt::FuncDef(f) => {
                let mname = f.name_text();
                if config.matches("python_functions", &mname, &["test*"])
                    || (mname.starts_with("test")
                        && c.bases().iter().any(|base| {
                            base_name(base).is_some_and(|name| name.ends_with("TestCase"))
                        }))
                {
                    children.push(TestItem {
                        id: format!("{id}::{mname}"),
                        name: mname,
                        kind: TestKind::Function,
                        path: path.to_path_buf(),
                        line: line_of_token(lines, f.def_token()),
                        children: Vec::new(),
                    });
                }
            }
            Stmt::ClassDef(inner) => {
                if let Some(item) = collect_class(inner, &id, path, lines, config) {
                    children.push(item);
                }
            }
            _ => {}
        }
    }
    Some(TestItem {
        id,
        name,
        kind: TestKind::Class,
        path: path.to_path_buf(),
        line: line_of_token(lines, c.class_token()),
        children,
    })
}

/// `Test*` classes (pytest) or subclasses of `*TestCase` (unittest).
fn is_test_class(c: &ast::ClassDef, config: &crate::PytestConfiguration) -> bool {
    if config.matches("python_classes", &c.name_text(), &["Test*"]) {
        return true;
    }
    c.bases()
        .iter()
        .any(|b| base_name(b).is_some_and(|n| n.ends_with("TestCase")))
}

fn base_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.name_text()),
        Expr::Attribute(a) => Some(a.attr_text()),
        Expr::Subscript(s) => s
            .syntax()
            .children()
            .find_map(Expr::cast)
            .as_ref()
            .and_then(base_name),
        Expr::Paren(p) => p.inner().as_ref().and_then(base_name),
        _ => None,
    }
}

/// 1-based line of a token, from the file's line starts.
fn line_of_token(lines: &[usize], tok: Option<SyntaxToken>) -> usize {
    let off: usize = tok.map(|t| t.text_range().start().into()).unwrap_or(0);
    lines.partition_point(|&start| start <= off).max(1)
}

/// Walks the project (honoring `.gitignore`, skipping virtualenvs and caches)
/// and discovers tests in every test file. Returns one [`TestKind::Module`]
/// item per file that contains tests, sorted by path, with ids relative to `root`.
pub fn discover_project(root: &Path) -> Vec<TestItem> {
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
        .filter(|p| crate::is_test_file_in(root, p))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let children = discover_file_in(root, &file, &text);
        if children.is_empty() {
            continue;
        }
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| rel.clone());
        out.push(TestItem {
            id: rel,
            name,
            kind: TestKind::Module,
            path: file.clone(),
            line: 1,
            children,
        });
    }
    out
}

/// Converts a node id (`pkg/test_x.py::TestA::test_b`) into a unittest dotted
/// name (`pkg.test_x.TestA.test_b`). Paths may be absolute (under `root`) or
/// relative to it; a bare dotted name is returned unchanged.
pub fn node_id_to_unittest_name(root: &Path, id: &str) -> String {
    let (file_part, rest) = match id.split_once("::") {
        Some((f, r)) => (f, Some(r)),
        None => (id, None),
    };
    let file_part = file_part.replace('\\', "/");
    let looks_like_path = file_part.ends_with(".py") || file_part.contains('/');
    let module = if looks_like_path {
        let p = Path::new(&file_part);
        let rel = p
            .strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        let mut m = rel.strip_suffix(".py").unwrap_or(&rel).to_string();
        if let Some(stripped) = m.strip_suffix("/__init__") {
            m = stripped.to_string();
        }
        m.trim_matches('/').replace('/', ".")
    } else {
        file_part.clone()
    };
    match rest {
        Some(r) if !r.is_empty() => format!("{module}.{}", r.replace("::", ".")),
        _ => module,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"import unittest
import pytest


def helper():
    pass


def test_top():
    assert True


async def test_async():
    assert True


class Helper:
    def test_not_collected(self):
        pass


class TestThing:
    def setup_method(self):
        pass

    def test_a(self):
        assert 1

    @pytest.mark.parametrize("x", [1, 2])
    def test_param(self, x):
        assert x

    class TestNested:
        def test_inner(self):
            assert True


class MyCase(unittest.TestCase):
    def setUp(self):
        pass

    def testCamel(self):
        self.assertTrue(True)

    def test_snake(self):
        pass


class AsyncCase(unittest.IsolatedAsyncioTestCase):
    async def test_io(self):
        pass
"#;

    #[test]
    fn discovers_functions_classes_and_unittest_cases() {
        let items = discover_file(Path::new("tests/test_sample.py"), SAMPLE);
        let ids: Vec<&str> = items
            .iter()
            .flat_map(|i| i.walk())
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "tests/test_sample.py::test_top",
                "tests/test_sample.py::test_async",
                "tests/test_sample.py::TestThing",
                "tests/test_sample.py::TestThing::test_a",
                "tests/test_sample.py::TestThing::test_param",
                "tests/test_sample.py::TestThing::TestNested",
                "tests/test_sample.py::TestThing::TestNested::test_inner",
                "tests/test_sample.py::MyCase",
                "tests/test_sample.py::MyCase::testCamel",
                "tests/test_sample.py::MyCase::test_snake",
                "tests/test_sample.py::AsyncCase",
                "tests/test_sample.py::AsyncCase::test_io",
            ]
        );
        assert_eq!(items[0].line, 9);
        assert_eq!(items[0].kind, TestKind::Function);
        assert_eq!(items[2].kind, TestKind::Class);
        assert_eq!(items[2].line, 22);
        // Decorated method: line points at `def`, not the decorator.
        assert_eq!(items[2].children[1].line, 30);
        assert_eq!(items[2].children[1].name, "test_param");
        assert_eq!(items[2].test_count(), 3);
        assert_eq!(items[2].path, Path::new("tests/test_sample.py"));
    }

    #[test]
    fn discover_file_in_uses_relative_ids() {
        let root = Path::new("/proj");
        let items = discover_file_in(
            root,
            Path::new("/proj/tests/test_a.py"),
            "def test_x():\n    pass\n",
        );
        assert_eq!(items[0].id, "tests/test_a.py::test_x");
        assert_eq!(items[0].path, Path::new("/proj/tests/test_a.py"));
    }

    #[test]
    fn discovery_on_an_existing_parse_tree() {
        let root = Path::new("/proj");
        let path = Path::new("/proj/test_crlf.py");
        // CRLF line endings; the last test is on the last line of the file.
        let text = "import os\r\n\r\nclass TestA:\r\n    def test_one(self):\r\n        pass\r\n\r\ndef test_two(): pass";
        let items = discover_parsed_in(root, path, text, &rustpy_syntax::parse(text));
        assert_eq!(items, discover_file_in(root, path, text));
        let lines: Vec<(&str, usize)> = items
            .iter()
            .flat_map(|i| i.walk())
            .map(|i| (i.id.as_str(), i.line))
            .collect();
        assert_eq!(
            lines,
            vec![
                ("test_crlf.py::TestA", 3),
                ("test_crlf.py::TestA::test_one", 4),
                ("test_crlf.py::test_two", 7)
            ]
        );
    }

    #[test]
    fn conditional_tests_are_found() {
        let items = discover_file(
            Path::new("t.py"),
            "import sys\nif sys.platform == 'linux':\n    def test_linux():\n        pass\nelse:\n    def test_other():\n        pass\n",
        );
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["t.py::test_linux", "t.py::test_other"]);
        assert_eq!(items[1].line, 6);
    }

    #[test]
    fn test_file_names() {
        assert!(is_test_file(Path::new("test_x.py")));
        assert!(is_test_file(Path::new("pkg/x_test.py")));
        assert!(is_test_file(Path::new("tests/helpers.py")));
        assert!(is_test_file(Path::new("tests/unit/foo.py")));
        assert!(!is_test_file(Path::new("src/testing_tools.py")));
        assert!(!is_test_file(Path::new("tests/conftest.py")));
        assert!(!is_test_file(Path::new("tests/__init__.py")));
        assert!(!is_test_file(Path::new("main.py")));
    }

    #[test]
    fn project_discovery_walks_test_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::create_dir_all(root.join(".venv/lib")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("tests/test_a.py"), "def test_a():\n    pass\n").unwrap();
        std::fs::write(root.join("tests/helpers.py"), "def helper():\n    pass\n").unwrap();
        std::fs::write(
            root.join("foo_test.py"),
            "class TestFoo:\n    def test_b(self):\n        pass\n",
        )
        .unwrap();
        std::fs::write(root.join("src/bar.py"), "def test_hidden():\n    pass\n").unwrap();
        std::fs::write(
            root.join(".venv/lib/test_venv.py"),
            "def test_venv():\n    pass\n",
        )
        .unwrap();
        let items = discover_project(root);
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["foo_test.py", "tests/test_a.py"]);
        assert_eq!(items[0].kind, TestKind::Module);
        assert_eq!(items[0].children[0].id, "foo_test.py::TestFoo");
        assert_eq!(
            items[0].children[0].children[0].id,
            "foo_test.py::TestFoo::test_b"
        );
        assert_eq!(items[1].path, root.join("tests/test_a.py"));
        assert_eq!(items.iter().map(TestItem::test_count).sum::<usize>(), 2);
    }

    #[test]
    fn unittest_names() {
        let root = Path::new("/proj");
        assert_eq!(
            node_id_to_unittest_name(root, "tests/test_x.py::TestA::test_b"),
            "tests.test_x.TestA.test_b"
        );
        assert_eq!(
            node_id_to_unittest_name(root, "/proj/tests/test_x.py"),
            "tests.test_x"
        );
        assert_eq!(
            node_id_to_unittest_name(root, "pkg/__init__.py::TestP"),
            "pkg.TestP"
        );
        assert_eq!(
            node_id_to_unittest_name(root, "pkg.mod.Case.test"),
            "pkg.mod.Case.test"
        );
        assert_eq!(
            node_id_to_unittest_name(root, "tests/test_x.py::Outer::Inner::test_c"),
            "tests.test_x.Outer.Inner.test_c"
        );
    }
}
