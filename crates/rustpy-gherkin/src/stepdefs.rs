//! Step definition index: scans Python modules for behave / pytest-bdd step
//! decorators.
//!
//! * behave: `@given("pattern")` / `@when` / `@then` / `@step` (also the
//!   capitalized aliases and `@behave.given`), with `use_step_matcher("re")`
//!   (or the deprecated `step_matcher`) switching the matcher for the
//!   definitions that follow in the same module (behave resets the matcher
//!   for each step module; the default is `parse`).
//! * pytest-bdd: `@given("exact text")`, `@given(parsers.parse("..."))`,
//!   `parsers.cfparse`, `parsers.re`, `parsers.string`.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rustpy_syntax::ast::{self, AstNode, Expr, Stmt, StringLike};
use serde::{Deserialize, Serialize};

use crate::matching::Compiled;
use crate::model::StepKind;

/// How a step pattern is matched.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Matcher {
    /// `parse` library format (`I have {count:d} apples`) — behave's default.
    Parse,
    /// `parse_type.cfparse`: `parse` plus cardinality fields (`{names:Name+}`).
    CFParse,
    /// Regular expression (`I have (?P<count>\d+) apples`), matched against the whole step text.
    Re,
    /// Exact text (pytest-bdd's default for plain strings / `parsers.string`).
    String,
}

impl Matcher {
    /// The name used by `use_step_matcher(...)` / `pytest_bdd.parsers.<name>`.
    pub fn name(self) -> &'static str {
        match self {
            Matcher::Parse => "parse",
            Matcher::CFParse => "cfparse",
            Matcher::Re => "re",
            Matcher::String => "string",
        }
    }

    /// Parses a matcher name (`"parse"`, `"cfparse"`, `"re"`, behave 1.3's
    /// `"re0"`, `"string"`).
    pub fn from_name(name: &str) -> Option<Matcher> {
        match name.trim() {
            "parse" => Some(Matcher::Parse),
            "cfparse" => Some(Matcher::CFParse),
            "re" | "re0" => Some(Matcher::Re),
            "string" => Some(Matcher::String),
            _ => None,
        }
    }
}

/// Which BDD framework a step module is written for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BddFramework {
    Behave,
    PytestBdd,
}

/// One step definition (one decorator; a function with two decorators yields two).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepDef {
    pub kind: StepKind,
    /// The pattern as written (decoded Python string value).
    pub pattern: String,
    pub matcher: Matcher,
    pub framework: BddFramework,
    pub path: PathBuf,
    /// 1-based line of the decorator.
    pub line: usize,
    /// Name of the decorated function (`step_impl`, `_`, ...).
    pub function: String,
    /// Byte range of the pattern's string literal (quotes included) in the source.
    pub pattern_range: Range<usize>,
    #[serde(skip)]
    pub(crate) compiled: OnceLock<Option<Compiled>>,
}

impl PartialEq for StepDef {
    fn eq(&self, o: &Self) -> bool {
        self.kind == o.kind
            && self.pattern == o.pattern
            && self.matcher == o.matcher
            && self.framework == o.framework
            && self.path == o.path
            && self.line == o.line
            && self.function == o.function
            && self.pattern_range == o.pattern_range
    }
}

impl Eq for StepDef {}

impl StepDef {
    /// A behave step definition (for tests and hand-built indexes).
    pub fn new(
        kind: StepKind,
        pattern: impl Into<String>,
        matcher: Matcher,
        path: impl Into<PathBuf>,
        line: usize,
    ) -> StepDef {
        StepDef {
            kind,
            pattern: pattern.into(),
            matcher,
            framework: BddFramework::Behave,
            path: path.into(),
            line,
            function: String::new(),
            pattern_range: 0..0,
            compiled: OnceLock::new(),
        }
    }

    /// The same definition for another framework.
    pub fn with_framework(mut self, framework: BddFramework) -> StepDef {
        self.framework = framework;
        self.compiled = OnceLock::new();
        self
    }
}

const STEP_MATCHER_FUNCS: &[&str] = &["use_step_matcher", "step_matcher"];

fn line_starts(src: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(src.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

fn line_of(starts: &[usize], offset: usize) -> usize {
    match starts.binary_search(&offset) {
        Ok(i) => i + 1,
        Err(i) => i,
    }
}

/// Last segment of a callee: `given` for `given`, `behave.given`, `pytest_bdd.given`.
fn callee_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.name_text()),
        Expr::Attribute(a) => Some(a.attr_text()),
        _ => None,
    }
}

/// Decoded value and range of a (possibly implicitly concatenated) string literal.
fn string_value(e: &Expr) -> Option<(String, Range<usize>)> {
    let r = e.syntax().text_range();
    let range = usize::from(r.start())..usize::from(r.end());
    match e {
        Expr::String(s) if !s.is_bytes() => Some((s.value(), range)),
        Expr::ConcatString(c)
            if c.parts()
                .all(|p| matches!(&p, Expr::String(s) if !s.is_bytes())) =>
        {
            Some((StringLike::Concat(c.clone()).value(), range))
        }
        Expr::Paren(p) => p.inner().as_ref().and_then(string_value),
        _ => None,
    }
}

fn first_positional(call: &ast::CallExpr) -> Option<Expr> {
    call.arg_list()?.positional().next()
}

fn step_kind_of(name: &str) -> Option<StepKind> {
    match name {
        "given" | "Given" => Some(StepKind::Given),
        "when" | "When" => Some(StepKind::When),
        "then" | "Then" => Some(StepKind::Then),
        "step" | "Step" => Some(StepKind::Step),
        _ => None,
    }
}

fn is_pytest_bdd_module(m: &str) -> bool {
    m == "pytest_bdd" || m.starts_with("pytest_bdd.")
}

/// True when the module imports pytest-bdd anywhere at module level (also
/// inside `if` / `try` blocks).
fn uses_pytest_bdd(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::ImportFrom(i) => is_pytest_bdd_module(&i.module_path()),
        Stmt::Import(i) => i.aliases().any(|a| is_pytest_bdd_module(&a.name())),
        Stmt::If(i) => {
            uses_pytest_bdd(&block(i.body()))
                || i.elif_clauses().any(|c| uses_pytest_bdd(&block(c.body())))
                || i.else_clause()
                    .is_some_and(|e| uses_pytest_bdd(&block(e.body())))
        }
        Stmt::Try(t) => uses_pytest_bdd(&block(t.body())),
        _ => false,
    })
}

fn block(b: Option<ast::Block>) -> Vec<Stmt> {
    b.map(|b| b.statements().collect()).unwrap_or_default()
}

struct Scanner<'a> {
    path: &'a Path,
    starts: Vec<usize>,
    framework: BddFramework,
    matcher: Matcher,
    /// Set once `use_step_matcher` was called.
    matcher_changed: bool,
    out: Vec<StepDef>,
}

impl Scanner<'_> {
    fn walk(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            match s {
                Stmt::Expr(e) => {
                    if let Some(Expr::Call(c)) = e.expr() {
                        self.matcher_call(&c);
                    }
                }
                Stmt::FuncDef(f) => self.function(f),
                Stmt::If(i) => {
                    self.walk(&block(i.body()));
                    for c in i.elif_clauses() {
                        self.walk(&block(c.body()));
                    }
                    if let Some(e) = i.else_clause() {
                        self.walk(&block(e.body()));
                    }
                }
                Stmt::Try(t) => {
                    self.walk(&block(t.body()));
                    for c in t.except_clauses() {
                        self.walk(&block(c.body()));
                    }
                    if let Some(e) = t.else_clause() {
                        self.walk(&block(e.body()));
                    }
                    if let Some(f) = t.finally_clause() {
                        self.walk(&block(f.body()));
                    }
                }
                Stmt::With(w) => self.walk(&block(w.body())),
                _ => {}
            }
        }
    }

    fn matcher_call(&mut self, c: &ast::CallExpr) {
        let Some(name) = c.callee().as_ref().and_then(callee_name) else {
            return;
        };
        if !STEP_MATCHER_FUNCS.contains(&name.as_str()) {
            return;
        }
        if let Some((value, _)) = first_positional(c).as_ref().and_then(string_value) {
            if let Some(m) = Matcher::from_name(&value) {
                self.matcher = m;
                self.matcher_changed = true;
            }
        }
    }

    fn function(&mut self, f: &ast::FuncDef) {
        let function = f.name_text();
        for d in f.decorators() {
            let Some(Expr::Call(call)) = d.expr() else {
                continue;
            };
            let Some(kind) = call
                .callee()
                .as_ref()
                .and_then(callee_name)
                .as_deref()
                .and_then(step_kind_of)
            else {
                continue;
            };
            let Some(arg) = first_positional(&call) else {
                continue;
            };
            let found = match arg.clone().unparen() {
                Expr::Call(inner) => {
                    let parser = inner.callee().as_ref().and_then(callee_name);
                    let matcher = match parser.as_deref() {
                        Some("parse") => Some(Matcher::Parse),
                        Some("cfparse") => Some(Matcher::CFParse),
                        Some("re") | Some("compile") => Some(Matcher::Re),
                        Some("string") => Some(Matcher::String),
                        _ => None,
                    };
                    matcher.and_then(|m| {
                        first_positional(&inner)
                            .as_ref()
                            .and_then(string_value)
                            .map(|(p, r)| (p, r, m))
                    })
                }
                other => string_value(&other).map(|(p, r)| {
                    let m = if self.framework == BddFramework::PytestBdd {
                        Matcher::String
                    } else {
                        self.matcher
                    };
                    (p, r, m)
                }),
            };
            let Some((pattern, pattern_range, matcher)) = found else {
                continue;
            };
            let start: usize = d.syntax().text_range().start().into();
            let line = line_of(&self.starts, start);
            self.out.push(StepDef {
                kind,
                pattern,
                matcher,
                framework: self.framework,
                path: self.path.to_path_buf(),
                line,
                function: function.clone(),
                pattern_range,
                compiled: OnceLock::new(),
            });
        }
    }
}

fn scanner_for<'a>(
    py_source: &str,
    path: &'a Path,
    default_matcher: Matcher,
) -> (Scanner<'a>, Vec<Stmt>) {
    let parse = rustpy_syntax::parse(py_source);
    let stmts: Vec<Stmt> = ast::Module::cast(parse.syntax_node())
        .map(|m| m.statements().collect())
        .unwrap_or_default();
    let framework = if uses_pytest_bdd(&stmts) {
        BddFramework::PytestBdd
    } else {
        BddFramework::Behave
    };
    (
        Scanner {
            path,
            starts: line_starts(py_source),
            framework,
            matcher: default_matcher,
            matcher_changed: false,
            out: Vec::new(),
        },
        stmts,
    )
}

/// Scans a Python module for step definitions (behave's default `parse`
/// matcher applies until `use_step_matcher(...)` changes it).
pub fn scan_step_definitions(py_source: &str, path: &Path) -> Vec<StepDef> {
    scan_step_definitions_with(py_source, path, Matcher::Parse)
}

/// Like [`scan_step_definitions`] with another default matcher (behave
/// projects may call `use_step_matcher` at module level in `environment.py`,
/// see [`default_step_matcher`]).
pub fn scan_step_definitions_with(
    py_source: &str,
    path: &Path,
    default_matcher: Matcher,
) -> Vec<StepDef> {
    let (mut sc, stmts) = scanner_for(py_source, path, default_matcher);
    sc.walk(&stmts);
    sc.out
}

/// The matcher selected by module-level `use_step_matcher(...)` calls in a
/// behave `environment.py` (the default for all step modules), if any.
pub fn default_step_matcher(environment_py: &str) -> Option<Matcher> {
    let (mut sc, stmts) = scanner_for(environment_py, Path::new("environment.py"), Matcher::Parse);
    sc.walk(&stmts);
    sc.matcher_changed.then_some(sc.matcher)
}

/// Cheap pre-filter for project-wide indexing: does this Python source look
/// like it may contain step definitions?
pub fn looks_like_step_module(py_source: &str) -> bool {
    py_source.contains("behave")
        || py_source.contains("pytest_bdd")
        || [
            "@given", "@when", "@then", "@step", "@Given", "@When", "@Then", "@Step",
        ]
        .iter()
        .any(|d| py_source.contains(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEHAVE: &str = r#"from behave import given, when, then, step, use_step_matcher
import behave


@given("a fresh calculator")
def step_fresh(context):
    context.result = 0


@when(u'I add {a:d} and {b:d}')
@when("I sum {a:d} with {b:d}")
async def step_add(context, a, b):
    context.result = a + b


use_step_matcher("re")


@then(r"the result is (?P<n>\d+)")
def step_result(context, n):
    pass


@behave.step("anything " "goes")
def step_any(context):
    pass


use_step_matcher("cfparse")


@Then("names are {names:Name+}")
def step_names(context, names):
    pass


@given(f"not {x}")
def step_fstring(context):
    pass


@other("ignored")
def nope():
    pass


if True:
    @given("conditional")
    def step_cond(context):
        pass
"#;

    #[test]
    fn behave_definitions_and_matcher_switches() {
        let defs = scan_step_definitions(BEHAVE, Path::new("features/steps/calc.py"));
        let got: Vec<(StepKind, &str, Matcher, usize, &str)> = defs
            .iter()
            .map(|d| {
                (
                    d.kind,
                    d.pattern.as_str(),
                    d.matcher,
                    d.line,
                    d.function.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    StepKind::Given,
                    "a fresh calculator",
                    Matcher::Parse,
                    5,
                    "step_fresh"
                ),
                (
                    StepKind::When,
                    "I add {a:d} and {b:d}",
                    Matcher::Parse,
                    10,
                    "step_add"
                ),
                (
                    StepKind::When,
                    "I sum {a:d} with {b:d}",
                    Matcher::Parse,
                    11,
                    "step_add"
                ),
                (
                    StepKind::Then,
                    "the result is (?P<n>\\d+)",
                    Matcher::Re,
                    19,
                    "step_result"
                ),
                (StepKind::Step, "anything goes", Matcher::Re, 24, "step_any"),
                (
                    StepKind::Then,
                    "names are {names:Name+}",
                    Matcher::CFParse,
                    32,
                    "step_names"
                ),
                (
                    StepKind::Given,
                    "conditional",
                    Matcher::CFParse,
                    48,
                    "step_cond"
                ),
            ]
        );
        assert!(defs.iter().all(|d| d.framework == BddFramework::Behave));
        assert_eq!(
            &BEHAVE[defs[1].pattern_range.clone()],
            "u'I add {a:d} and {b:d}'"
        );
        assert_eq!(defs[0].path, Path::new("features/steps/calc.py"));
    }

    #[test]
    fn pytest_bdd_definitions() {
        let src = r#"import re
from pytest_bdd import given, when, then, parsers, scenarios
from pytest_bdd.parsers import parse

scenarios("features")


@given("I have a cucumber", target_fixture="cucumbers")
def _():
    return []


@given(parsers.parse("I have {count:d} cucumbers"))
def have(count):
    pass


@when(parse("I eat {n:d}"))
def eat(n):
    pass


@then(parsers.re(r"I should have (?P<left>\d+) cucumbers"))
def left(left):
    pass


@then(parsers.cfparse("names {names:Name+}", extra_types={"Name": str}))
def names(names):
    pass


@then(parsers.string("exactly this"))
def exact():
    pass
"#;
        let defs = scan_step_definitions(src, Path::new("tests/test_bdd.py"));
        let got: Vec<(StepKind, &str, Matcher)> = defs
            .iter()
            .map(|d| (d.kind, d.pattern.as_str(), d.matcher))
            .collect();
        assert_eq!(
            got,
            vec![
                (StepKind::Given, "I have a cucumber", Matcher::String),
                (
                    StepKind::Given,
                    "I have {count:d} cucumbers",
                    Matcher::Parse
                ),
                (StepKind::When, "I eat {n:d}", Matcher::Parse),
                (
                    StepKind::Then,
                    "I should have (?P<left>\\d+) cucumbers",
                    Matcher::Re
                ),
                (StepKind::Then, "names {names:Name+}", Matcher::CFParse),
                (StepKind::Then, "exactly this", Matcher::String),
            ]
        );
        assert!(defs.iter().all(|d| d.framework == BddFramework::PytestBdd));
        assert_eq!(defs[1].line, 13);
    }

    #[test]
    fn environment_default_matcher_and_overrides() {
        assert_eq!(
            default_step_matcher("from behave import use_step_matcher\nuse_step_matcher('re')\n"),
            Some(Matcher::Re)
        );
        assert_eq!(
            default_step_matcher("def before_all(context):\n    pass\n"),
            None
        );
        let defs = scan_step_definitions_with(
            "from behave import given\n@given('x (\\d+)')\ndef s(c, n): pass\n",
            Path::new("s.py"),
            Matcher::Re,
        );
        assert_eq!(defs[0].matcher, Matcher::Re);
        // Unknown matcher names keep the current matcher.
        let defs = scan_step_definitions(
            "use_step_matcher('cucumber_expressions')\n@given('x')\ndef s(c): pass\n",
            Path::new("s.py"),
        );
        assert_eq!(defs[0].matcher, Matcher::Parse);
    }

    #[test]
    fn broken_source_does_not_panic() {
        for src in [
            "",
            "@given(",
            "@given('unterminated\ndef x(:\n",
            "@given()\ndef f(): pass\n",
            "@given(123)\ndef f(): pass\n",
        ] {
            let _ = scan_step_definitions(src, Path::new("x.py"));
        }
        assert!(looks_like_step_module("from behave import given"));
        assert!(looks_like_step_module("@when('x')"));
        assert!(!looks_like_step_module("import os\n"));
    }

    #[test]
    fn matcher_names() {
        for m in [
            Matcher::Parse,
            Matcher::CFParse,
            Matcher::Re,
            Matcher::String,
        ] {
            assert_eq!(Matcher::from_name(m.name()), Some(m));
        }
        assert_eq!(Matcher::from_name("re0"), Some(Matcher::Re));
        assert_eq!(Matcher::from_name("nope"), None);
    }
}
