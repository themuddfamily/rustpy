//! What the editor's ▶ gutter icons, "Run/Debug context configuration"
//! (Ctrl+Shift+F10 / Ctrl+Shift+F9) and the Project view's Run/Debug launch
//! (roadmap §3.1 gutter run icons, context configurations).
//!
//! Tests come from [`rustpy_test::discover_file_in`], so node ids match the
//! Test Runner: functions, methods (`Class::method`), nested classes,
//! `unittest.TestCase` subclasses and camelCase `testFoo` methods. Test
//! markers are shown in test files (`test_*.py`, `*_test.py`, `tests.py`,
//! files under `tests/`) and for `*TestCase` subclasses in any file.

use std::path::{Path, PathBuf};

use rustpy_project::{RunConfig, RunConfigKind};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::Parse;
use rustpy_test::{discover_parsed_in, TestItem, TestKind};

/// How a configuration is launched.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RunMode {
    Run,
    Debug,
    Coverage,
    Profile,
    Concurrency,
}

impl RunMode {
    pub const ALL: [RunMode; 5] = [
        RunMode::Run,
        RunMode::Debug,
        RunMode::Coverage,
        RunMode::Profile,
        RunMode::Concurrency,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RunMode::Run => "Run",
            RunMode::Debug => "Debug",
            RunMode::Coverage => "Run with Coverage",
            RunMode::Profile => "Profile",
            RunMode::Concurrency => "Run with Concurrency Diagram",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            RunMode::Run => "▶",
            RunMode::Debug => "🐞",
            RunMode::Coverage => "☂",
            RunMode::Profile => "⏱",
            RunMode::Concurrency => "⇄",
        }
    }
}

/// What the ▶ on a line runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GutterTarget {
    /// The file as a script (`if __name__ == "__main__":` or, for scripts
    /// without other markers, the first statement).
    Script,
    /// A test function, method or class: its node id (relative to the
    /// project root) and display name.
    Test {
        id: String,
        name: String,
        kind: TestKind,
    },
    /// The first line of a `>>>` doctest block.
    Doctest,
    SelectedDoctest {
        qualified: String,
    },
}

/// A test with the lines (0-based, inclusive) its definition spans,
/// decorators included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedTest {
    pub id: String,
    pub name: String,
    pub kind: TestKind,
    /// 0-based line of the `def` / `class` keyword (where the ▶ is).
    pub line: usize,
    pub first_line: usize,
    pub last_line: usize,
}

/// Extent and `*TestCase` base of every `def` / `class` by its 1-based keyword line.
struct Definition {
    keyword_line: usize,
    first_line: usize,
    last_line: usize,
    test_case_class: bool,
}

fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

fn line_of(starts: &[usize], offset: usize) -> usize {
    starts.partition_point(|&s| s <= offset).saturating_sub(1)
}

fn definitions(text: &str, parse: &Parse) -> Vec<Definition> {
    let starts = line_starts(text);
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for node in parse.syntax_node().descendants() {
        let (keyword, test_case_class) = if let Some(f) = ast::FuncDef::cast(node.clone()) {
            (f.def_token(), false)
        } else if let Some(c) = ast::ClassDef::cast(node.clone()) {
            let is_case = c.bases().iter().any(|b| {
                let t = b.syntax().text().to_string();
                let t = t.split('[').next().unwrap_or("").trim();
                t.rsplit('.').next().unwrap_or(t).ends_with("TestCase")
            });
            (c.class_token(), is_case)
        } else {
            continue;
        };
        let Some(keyword) = keyword else { continue };
        let range = node.text_range();
        let start: usize = range.start().into();
        // The last character that is not trailing whitespace of the definition.
        let mut end = usize::from(range.end()).min(bytes.len());
        while end > start + 1 && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        out.push(Definition {
            keyword_line: line_of(&starts, keyword.text_range().start().into()),
            first_line: line_of(&starts, start),
            last_line: line_of(&starts, end.saturating_sub(1).max(start)),
            test_case_class,
        });
    }
    out
}

/// Every test of the file with its extent. Outside test files only
/// `*TestCase` subclasses (and their contents) count.
pub fn located_tests(root: &Path, path: &Path, text: &str) -> Vec<LocatedTest> {
    located_tests_in(root, path, text, &rustpy_syntax::parse(text))
}

/// [`located_tests`] with the file's parse tree (an editor's cached one).
pub fn located_tests_in(root: &Path, path: &Path, text: &str, parse: &Parse) -> Vec<LocatedTest> {
    let items = discover_parsed_in(root, path, text, parse);
    if items.is_empty() {
        return Vec::new();
    }
    let test_file = rustpy_test::is_test_file_in(root, path);
    let defs: std::collections::HashMap<usize, Definition> = definitions(text, parse)
        .into_iter()
        .map(|d| (d.keyword_line, d))
        .collect();
    let mut out = Vec::new();
    fn walk(
        item: &TestItem,
        defs: &std::collections::HashMap<usize, Definition>,
        allowed: bool,
        out: &mut Vec<LocatedTest>,
    ) {
        let line = item.line.saturating_sub(1);
        let def = defs.get(&line);
        let allowed =
            allowed || (item.kind == TestKind::Class && def.is_some_and(|d| d.test_case_class));
        if allowed {
            let (first_line, last_line) = def
                .map(|d| (d.first_line, d.last_line))
                .unwrap_or((line, line));
            out.push(LocatedTest {
                id: item.id.clone(),
                name: item.name.clone(),
                kind: item.kind,
                line,
                first_line,
                last_line,
            });
        }
        for c in &item.children {
            walk(c, defs, allowed, out);
        }
    }
    for item in &items {
        walk(item, &defs, test_file, &mut out);
    }
    out
}

/// Lines (0-based, sorted) that show a ▶ marker and what each runs.
pub fn gutter_targets(root: &Path, path: &Path, text: &str) -> Vec<(usize, GutterTarget)> {
    gutter_targets_in(root, path, text, &rustpy_syntax::parse(text))
}

/// [`gutter_targets`] with the file's parse tree (an editor's cached one,
/// so nothing is parsed again while typing).
pub fn gutter_targets_in(
    root: &Path,
    path: &Path,
    text: &str,
    parse: &Parse,
) -> Vec<(usize, GutterTarget)> {
    let mut out: Vec<(usize, GutterTarget)> = Vec::new();
    for test in rustpy_syntax::doctest::discover(text, parse) {
        if let Some(first) = test.lines.first() {
            let line = text[..usize::from(first.prompt.start())]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            out.push((
                line,
                GutterTarget::SelectedDoctest {
                    qualified: test.qualified,
                },
            ));
        }
    }
    let starts = line_starts(text);
    for guard in parse
        .syntax_node()
        .descendants()
        .filter_map(ast::IfStmt::cast)
    {
        let Some(condition) = guard.condition() else {
            continue;
        };
        let compact: String = condition
            .syntax()
            .descendants_with_tokens()
            .filter_map(|node| node.into_token())
            .filter(|token| {
                !token.kind().is_trivia() && token.kind() != rustpy_syntax::SyntaxKind::NEWLINE
            })
            .map(|token| token.text().to_string())
            .collect();
        let compact = compact.trim_start_matches('(').trim_end_matches(')');
        if matches!(
            compact,
            "__name__==\"__main__\""
                | "__name__=='__main__'"
                | "\"__main__\"==__name__"
                | "'__main__'==__name__"
        ) {
            out.push((
                line_of(&starts, guard.syntax().text_range().start().into()),
                GutterTarget::Script,
            ));
        }
    }
    for t in located_tests_in(root, path, text, parse) {
        out.push((
            t.line,
            GutterTarget::Test {
                id: t.id,
                name: t.name,
                kind: t.kind,
            },
        ));
    }
    // (`#%%` cells have their own ▶, which runs the cell in the Python Console.)
    if out.is_empty()
        && !text.trim().is_empty()
        && crate::editor::console_code::cell_marker_lines(text).is_empty()
    {
        // A script without a main guard: the first statement line.
        if let Some(first) = text
            .lines()
            .position(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        {
            out.push((first, GutterTarget::Script));
        }
    }
    out.sort_by_key(|(l, _)| *l);
    out.dedup_by_key(|(l, _)| *l);
    out
}

/// The target of the ▶ on `line0`.
pub fn target_at(root: &Path, path: &Path, text: &str, line0: usize) -> Option<GutterTarget> {
    gutter_targets(root, path, text)
        .into_iter()
        .find(|(l, _)| *l == line0)
        .map(|(_, t)| t)
}

/// The innermost test whose definition contains `line0`.
pub fn test_containing(root: &Path, path: &Path, text: &str, line0: usize) -> Option<LocatedTest> {
    located_tests(root, path, text)
        .into_iter()
        .filter(|t| t.first_line <= line0 && line0 <= t.last_line)
        .min_by_key(|t| t.last_line - t.first_line)
}

/// `test_x.TestA.test_b` for `tests/test_x.py::TestA::test_b`.
pub fn dotted_test_name(id: &str) -> String {
    let (file, rest) = id.split_once("::").unwrap_or((id, ""));
    let stem = Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_string());
    if rest.is_empty() {
        stem
    } else {
        format!("{stem}.{}", rest.replace("::", "."))
    }
}

/// A temporary pytest configuration for one test (function, method or class).
pub fn test_config(id: &str, root: &Path) -> RunConfig {
    let mut cfg = RunConfig::for_pytest(id, root);
    cfg.name = format!("pytest for {}", dotted_test_name(id));
    cfg
}

/// The doctest runner for a file (`python -m doctest -v file`).
pub fn doctest_config(path: &Path) -> RunConfig {
    let name = format!(
        "doctest {}",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    RunConfig {
        name,
        kind: RunConfigKind::PythonModule {
            module: "doctest".into(),
        },
        parameters: format!(
            "-v {}",
            crate::run_anything::quote_args(&[path.to_string_lossy().into_owned()])
        ),
        working_dir: path.parent().map(Path::to_path_buf),
        temporary: true,
        ..Default::default()
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The configuration a ▶ target runs.
pub fn config_for_target(root: &Path, path: &Path, target: &GutterTarget) -> RunConfig {
    match target {
        GutterTarget::Script => RunConfig::for_script(path),
        GutterTarget::Test { id, .. } => test_config(id, root),
        GutterTarget::Doctest => doctest_config(path),
        GutterTarget::SelectedDoctest { qualified } => {
            let mut cfg = doctest_config(path);
            cfg.name = format!("doctest {qualified}");
            cfg.parameters = format!(
                "-v {}",
                crate::run_anything::quote_args(&[format!("{}::{qualified}", path.display())])
            );
            cfg
        }
    }
}

/// What "Current File" runs for an editor file: the tests of a test file,
/// otherwise the file as a script.
pub fn file_config(root: &Path, path: &Path, text: &str) -> RunConfig {
    if rustpy_test::is_test_file_in(root, path) && !located_tests(root, path, text).is_empty() {
        return RunConfig::for_pytest(&relative(root, path), root);
    }
    RunConfig::for_script(path)
}

/// "Run/Debug context configuration" for the caret line: the innermost test
/// around the caret, else the file's tests (test files), else the script.
pub fn context_config(root: &Path, path: &Path, text: &str, caret_line0: usize) -> RunConfig {
    let starts = line_starts(text);
    let parse = rustpy_syntax::parse(text);
    for doctest in rustpy_syntax::doctest::discover(text, &parse) {
        if line_of(&starts, doctest.literal.start().into()) <= caret_line0
            && caret_line0 <= line_of(&starts, doctest.literal.end().into())
        {
            return config_for_target(
                root,
                path,
                &GutterTarget::SelectedDoctest {
                    qualified: doctest.qualified,
                },
            );
        }
    }
    match located_tests_in(root, path, text, &parse)
        .into_iter()
        .filter(|test| test.first_line <= caret_line0 && caret_line0 <= test.last_line)
        .min_by_key(|test| test.last_line - test.first_line)
    {
        Some(t) => test_config(&t.id, root),
        None => file_config(root, path, text),
    }
}

/// What Run / Debug on a Project view entry launches: pytest for a
/// directory or a test file, otherwise the script.
pub fn config_for_path(root: &Path, path: &Path) -> RunConfig {
    if path.is_dir() {
        let rel = relative(root, path);
        let mut cfg = RunConfig::for_pytest(if rel.is_empty() { "" } else { &rel }, root);
        if rel.is_empty() {
            cfg.name = "pytest in project".into();
        }
        return cfg;
    }
    file_config(
        root,
        path,
        &std::fs::read_to_string(path).unwrap_or_default(),
    )
}

/// True when Project view entries at `path` can run tests (directories and
/// test files): the menu then reads "Run 'pytest in …'".
pub fn runs_tests(root: &Path, path: &Path) -> bool {
    matches!(
        config_for_path(root, path).kind,
        RunConfigKind::Pytest { .. }
    )
}

/// Test kind of a configuration created from a context (`None` for others).
pub fn test_target(cfg: &RunConfig) -> Option<&str> {
    match &cfg.kind {
        RunConfigKind::Pytest { target, .. } | RunConfigKind::Unittest { target } => Some(target),
        _ => None,
    }
}

/// The directory to discover tests in for a configuration's target.
pub fn target_path(root: &Path, target: &str) -> PathBuf {
    let file = target.split("::").next().unwrap_or(target);
    let p = Path::new(file);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const TESTS: &str = r#"import unittest
import pytest


def helper():
    pass


def test_top():
    assert True


class TestThing:
    @pytest.mark.parametrize("x", [1, 2])
    def test_param(self, x):
        assert x

    def test_multi_line_signature(
        self,
    ):
        y = 1
        assert y

    class TestNested:
        def test_inner(self):
            pass


class Cases(unittest.TestCase):
    def testCamel(self):
        self.assertTrue(True)


if __name__ == "__main__":
    unittest.main()
"#;

    fn ids(targets: &[(usize, GutterTarget)]) -> Vec<(usize, String)> {
        targets
            .iter()
            .map(|(l, t)| {
                (
                    *l,
                    match t {
                        GutterTarget::Script => "<script>".to_string(),
                        GutterTarget::Doctest => "<doctest>".to_string(),
                        GutterTarget::SelectedDoctest { qualified } => {
                            format!("<doctest:{qualified}>")
                        }
                        GutterTarget::Test { id, .. } => id.clone(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn gutter_marks_methods_nested_classes_and_test_cases() {
        let root = Path::new("/proj");
        let path = root.join("tests/test_things.py");
        let got = ids(&gutter_targets(root, &path, TESTS));
        let f = "tests/test_things.py";
        assert_eq!(
            got,
            vec![
                (8, format!("{f}::test_top")),
                (12, format!("{f}::TestThing")),
                (14, format!("{f}::TestThing::test_param")),
                (17, format!("{f}::TestThing::test_multi_line_signature")),
                (23, format!("{f}::TestThing::TestNested")),
                (24, format!("{f}::TestThing::TestNested::test_inner")),
                (28, format!("{f}::Cases")),
                (29, format!("{f}::Cases::testCamel")),
                (33, "<script>".to_string()),
            ]
        );
        // The ▶ of a method runs the method inside its class (not `file::method`).
        let cfg = config_for_target(root, &path, &target_at(root, &path, TESTS, 14).unwrap());
        assert_eq!(
            cfg.kind,
            RunConfigKind::Pytest {
                target: format!("{f}::TestThing::test_param"),
                keywords: String::new()
            }
        );
        assert_eq!(cfg.name, "pytest for test_things.TestThing.test_param");
        assert!(cfg.temporary);
    }

    #[test]
    fn outside_test_files_only_test_cases_are_marked() {
        let root = Path::new("/proj");
        let path = root.join("app/models.py");
        let got = ids(&gutter_targets(root, &path, TESTS));
        assert_eq!(
            got,
            vec![
                (28, "app/models.py::Cases".to_string()),
                (29, "app/models.py::Cases::testCamel".to_string()),
                (33, "<script>".to_string())
            ]
        );
        // A plain script gets its first statement.
        let got = ids(&gutter_targets(
            root,
            &root.join("main.py"),
            "# comment\n\nprint('hi')\n",
        ));
        assert_eq!(got, vec![(2, "<script>".to_string())]);
        let doc = "def f():\n    \"\"\"\n    >>> f()\n    >>> 1 + 1\n    2\n    \"\"\"\n";
        assert_eq!(
            ids(&gutter_targets(root, &root.join("lib.py"), doc)),
            vec![(2, "<doctest:f>".to_string())]
        );
    }

    #[test]
    fn context_configuration_follows_the_caret() {
        let root = Path::new("/proj");
        let path = root.join("tests/test_things.py");
        let target = |line: usize| match context_config(root, &path, TESTS, line).kind {
            RunConfigKind::Pytest { target, .. } => target,
            other => panic!("{other:?}"),
        };
        let f = "tests/test_things.py";
        // Decorator line and body lines belong to the decorated method.
        assert_eq!(target(13), format!("{f}::TestThing::test_param"));
        assert_eq!(target(15), format!("{f}::TestThing::test_param"));
        // Multi-line signatures and their bodies.
        assert_eq!(
            target(19),
            format!("{f}::TestThing::test_multi_line_signature")
        );
        assert_eq!(
            target(21),
            format!("{f}::TestThing::test_multi_line_signature")
        );
        // Between methods: the class; innermost wins for nested classes.
        assert_eq!(target(16), format!("{f}::TestThing"));
        assert_eq!(
            target(25),
            format!("{f}::TestThing::TestNested::test_inner")
        );
        // Outside any test: the whole test file.
        assert_eq!(target(5), f);
        // Non-test files run as scripts.
        let script = root.join("main.py");
        assert_eq!(
            context_config(root, &script, "print(1)\n", 0).kind,
            RunConfigKind::Python {
                script: script.clone()
            }
        );
        // "Current File" runs the whole test file wherever the caret is.
        assert_eq!(
            file_config(root, &path, TESTS).kind,
            RunConfigKind::Pytest {
                target: f.to_string(),
                keywords: String::new()
            }
        );
        assert_eq!(
            file_config(root, &script, "print(1)\n").kind,
            RunConfigKind::Python { script }
        );
        // A test file without tests is a script.
        let empty = root.join("tests/test_empty.py");
        assert!(matches!(
            file_config(root, &empty, "print(1)\n").kind,
            RunConfigKind::Python { .. }
        ));
    }

    /// A scratch directory removed on drop.
    pub(crate) struct ScratchDir(pub PathBuf);

    impl ScratchDir {
        pub(crate) fn new(tag: &str) -> Self {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p =
                std::env::temp_dir().join(format!("rustpy-ui-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            ScratchDir(p.canonicalize().unwrap())
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn project_view_runs_directories_and_test_files_with_pytest() {
        let dir = ScratchDir::new("ctx");
        let root = dir.0.as_path();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/test_a.py"), "def test_a():\n    pass\n").unwrap();
        std::fs::write(root.join("tests/helpers.py"), "def helper():\n    pass\n").unwrap();
        std::fs::write(root.join("main.py"), "print(1)\n").unwrap();
        let dir_cfg = config_for_path(root, &root.join("tests"));
        assert_eq!(
            (dir_cfg.name.as_str(), &dir_cfg.kind),
            (
                "pytest in tests",
                &RunConfigKind::Pytest {
                    target: "tests".into(),
                    keywords: String::new()
                }
            )
        );
        assert_eq!(
            config_for_path(root, &root.join("tests/test_a.py")).kind,
            RunConfigKind::Pytest {
                target: "tests/test_a.py".into(),
                keywords: String::new()
            }
        );
        assert!(matches!(
            config_for_path(root, &root.join("tests/helpers.py")).kind,
            RunConfigKind::Python { .. }
        ));
        assert!(matches!(
            config_for_path(root, &root.join("main.py")).kind,
            RunConfigKind::Python { .. }
        ));
        assert_eq!(config_for_path(root, root).name, "pytest in project");
        assert!(runs_tests(root, &root.join("tests")) && !runs_tests(root, &root.join("main.py")));
    }

    #[test]
    fn doctest_configuration() {
        let cfg = doctest_config(Path::new("/proj/lib.py"));
        assert_eq!(
            cfg.kind,
            RunConfigKind::PythonModule {
                module: "doctest".into()
            }
        );
        assert_eq!(
            RunConfig::split_args(&cfg.parameters),
            vec!["-v", "/proj/lib.py"]
        );
        assert_eq!(cfg.working_dir.as_deref(), Some(Path::new("/proj")));
        // Argument identity matters; safe paths need no display quotes, while
        // whitespace and quote characters must still preserve a single target.
        for target in ["/proj/a file.py", "/proj/π's examples.py"] {
            let cfg = doctest_config(Path::new(target));
            assert_eq!(
                cfg.kind,
                RunConfigKind::PythonModule {
                    module: "doctest".into()
                }
            );
            assert_eq!(RunConfig::split_args(&cfg.parameters), vec!["-v", target]);
            assert_eq!(cfg.working_dir.as_deref(), Some(Path::new("/proj")));
        }
    }
}

#[cfg(test)]
mod doctest_gutter_ownership_tests {
    use super::*;
    #[test]
    fn only_owned_docstrings_offer_one_target_and_selected_parameters_roundtrip() {
        let root = Path::new("/proj");
        let path = root.join("a file.py");
        let source = "# >>> comment\ntext = '''>>> not_a_docstring()'''\nclass Examples:\n    def first(self):\n        \"\"\"Doc.\n        >>> 1 + 1\n        2\n        >>> 2 + 2\n        4\n        \"\"\"\n";
        let targets = gutter_targets(root, &path, source);
        let selected: Vec<_> = targets
            .iter()
            .filter(|(_, target)| matches!(target, GutterTarget::SelectedDoctest { .. }))
            .collect();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, 5);
        let cfg = config_for_target(root, &path, &selected[0].1);
        assert_eq!(
            RunConfig::split_args(&cfg.parameters),
            vec!["-v", "/proj/a file.py::Examples.first"]
        );
    }
}

/// One menu action, shared by gutter, editor and Project view.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ContextAction {
    Launch(RunMode),
    Modify,
    Create,
}
impl ContextAction {
    pub const ALL: [Self; 7] = [
        Self::Launch(RunMode::Run),
        Self::Launch(RunMode::Debug),
        Self::Launch(RunMode::Coverage),
        Self::Launch(RunMode::Profile),
        Self::Launch(RunMode::Concurrency),
        Self::Modify,
        Self::Create,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Launch(mode) => mode.label(),
            Self::Modify => "Modify Run Configuration…",
            Self::Create => "Create Run Configuration…",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ProjectOwner {
    pub root: PathBuf,
    pub session: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorRequest {
    pub owner: ProjectOwner,
    pub document: rustpy_text::DocumentId,
    pub version: u64,
    pub path: PathBuf,
    pub configuration: RunConfig,
    pub action: ContextAction,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PathStamp {
    pub modified: Option<std::time::SystemTime>,
    pub len: u64,
    pub directory: bool,
}
impl PathStamp {
    pub fn read(path: &Path) -> Option<Self> {
        #[cfg(test)]
        STAMP_READS.with(|reads| reads.set(reads.get() + 1));
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
            directory: metadata.is_dir(),
        })
    }
}
#[cfg(test)]
thread_local! {
    /// How often this thread read a path stamp (tests: no stats while painting).
    pub(crate) static STAMP_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The stamp of `path` for a context menu being shown in `ui`, read only
/// while the menu is open (never on the paint path of closed menus). The
/// first frame of the menu records it; when the path changes while the menu
/// is open, the menu closes and `None` is returned, so its actions never act
/// on a file other than the one it was opened for.
pub fn open_menu_stamp(ui: &mut egui::Ui, path: &Path) -> Option<Option<PathStamp>> {
    let key = ui.id().with(("open-menu-path-stamp", path));
    let pass = ui.ctx().cumulative_pass_nr();
    let current = PathStamp::read(path);
    let opened = ui
        .data(|data| data.get_temp::<(u64, Option<PathStamp>)>(key))
        // Shown on the previous pass too: the same open menu.
        .filter(|(seen, _)| seen + 1 >= pass)
        .map(|(_, stamp)| stamp);
    match opened {
        Some(opened) if opened != current => {
            ui.data_mut(|data| data.remove::<(u64, Option<PathStamp>)>(key));
            ui.close();
            None
        }
        opened => {
            let stamp = opened.unwrap_or(current);
            ui.data_mut(|data| data.insert_temp(key, (pass, stamp.clone())));
            Some(stamp)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRequest {
    pub owner: ProjectOwner,
    pub path: PathBuf,
    pub stamp: PathStamp,
    pub configuration: RunConfig,
    pub action: ContextAction,
}
/// Preparing a context edit respects a matching saved configuration's settings.
/// Create always makes a permanent, uniquely named configuration; Modify selects
/// the existing target when possible without replacing unrelated names.
pub fn configuration_for_edit(
    store: &rustpy_project::RunConfigStore,
    candidate: RunConfig,
    root: &Path,
    create: bool,
) -> RunConfig {
    if create {
        let mut configuration = candidate;
        configuration.name = store.unique_name(&configuration.name);
        configuration.temporary = false;
        configuration
    } else {
        store.resolve_temporary(candidate, true, root)
    }
}
pub fn owner_key(view: egui::Id) -> egui::Id {
    view.with("context-run-owner")
}
pub fn tree_owner_key() -> egui::Id {
    egui::Id::new("project-tree-context-run-owner")
}

impl EditorRequest {
    pub fn is_current(
        &self,
        owner: &ProjectOwner,
        document: rustpy_text::DocumentId,
        version: u64,
        path: Option<&Path>,
    ) -> bool {
        &self.owner == owner
            && self.document == document
            && self.version == version
            && path == Some(self.path.as_path())
    }
}
impl PathRequest {
    pub fn is_current(&self, owner: &ProjectOwner) -> bool {
        &self.owner == owner && PathStamp::read(&self.path).as_ref() == Some(&self.stamp)
    }
}
#[cfg(test)]
mod context_action_tests {
    use super::*;
    #[test]
    fn modify_reuses_selected_settings_and_create_persists_distinct_permanent_target() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let path = root.join("test_a.py");
        let candidate = test_config("test_a.py::test_one", root);
        let mut existing = candidate.clone();
        existing.name = "Configured single test".into();
        existing.temporary = false;
        existing.parameters = "--maxfail 1 -x".into();
        existing.env.insert("APP_MODE".into(), "fixture".into());
        let mut store = rustpy_project::RunConfigStore::default();
        store.upsert(existing.clone());
        assert_eq!(
            configuration_for_edit(&store, candidate.clone(), root, false),
            existing
        );
        let mut collision = RunConfig::for_script(&path);
        collision.name = candidate.name.clone();
        collision.temporary = false;
        store.upsert(collision.clone());
        let created = configuration_for_edit(&store, candidate, root, true);
        assert!(!created.temporary);
        assert_ne!(created.name, collision.name);
        store.upsert(created.clone());
        store.save(root).unwrap();
        let loaded = rustpy_project::RunConfigStore::load(root);
        assert_eq!(loaded.selected_config(), Some(&created));
        assert!(loaded.configs.contains(&existing));
        assert!(loaded.configs.contains(&collision));
    }
    #[test]
    fn captured_editor_target_rejects_changed_project_document_path_and_version() {
        let document = rustpy_text::Document::new("def test_one(): pass\n");
        let replacement = rustpy_text::Document::new("def test_one(): pass\n");
        let owner = ProjectOwner {
            root: "/project".into(),
            session: 3,
        };
        let path = owner.root.join("test_a.py");
        let request = EditorRequest {
            owner: owner.clone(),
            document: document.identity(),
            version: document.version(),
            path: path.clone(),
            configuration: test_config("test_a.py::test_one", &owner.root),
            action: ContextAction::Launch(RunMode::Coverage),
        };
        assert!(request.is_current(&owner, document.identity(), document.version(), Some(&path)));
        assert!(!request.is_current(
            &ProjectOwner {
                session: 4,
                ..owner.clone()
            },
            document.identity(),
            document.version(),
            Some(&path)
        ));
        assert!(!request.is_current(
            &owner,
            replacement.identity(),
            document.version(),
            Some(&path)
        ));
        assert!(!request.is_current(
            &owner,
            document.identity(),
            document.version() + 1,
            Some(&path)
        ));
        assert!(!request.is_current(
            &owner,
            document.identity(),
            document.version(),
            Some(Path::new("/project/other.py"))
        ));
    }
    #[test]
    fn project_request_rejects_missing_or_changed_file_and_new_owner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.py");
        std::fs::write(&path, "print(1)\n").unwrap();
        let owner = ProjectOwner {
            root: directory.path().into(),
            session: 9,
        };
        let request = PathRequest {
            owner: owner.clone(),
            path: path.clone(),
            stamp: PathStamp::read(&path).unwrap(),
            configuration: config_for_path(&owner.root, &path),
            action: ContextAction::Create,
        };
        assert!(request.is_current(&owner));
        assert!(!request.is_current(&ProjectOwner {
            session: 10,
            ..owner.clone()
        }));
        std::fs::write(&path, "print('changed contents')\n").unwrap();
        assert!(!request.is_current(&owner));
        std::fs::remove_file(&path).unwrap();
        assert!(!request.is_current(&owner));
    }
    #[test]
    fn caret_inside_owned_doctest_targets_that_symbol_and_false_main_strings_do_not_mark() {
        let root = Path::new("/project");
        let path = root.join("sample.py");
        let source =
            "def example():\n    \"\"\"Example.\n    >>> 2 + 2\n    4\n    \"\"\"\n    return 4\n";
        assert_eq!(
            RunConfig::split_args(&context_config(root, &path, source, 3).parameters),
            vec!["-v", "/project/sample.py::example"]
        );
        let source = "text = '''\nif __name__ == '__main__':\n    not_python()\n'''\n\nif __name__ == '__main__':\n    print(1)\n";
        let targets = gutter_targets(root, &path, source);
        assert_eq!(targets, vec![(5, GutterTarget::Script)]);
    }
}
