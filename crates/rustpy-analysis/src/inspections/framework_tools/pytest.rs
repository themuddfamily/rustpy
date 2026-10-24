use super::*;
use std::collections::{HashMap, HashSet};
const BUILTIN_FIXTURES: &[&str] = &[
    "request",
    "tmp_path",
    "tmp_path_factory",
    "tmpdir",
    "tmpdir_factory",
    "capsys",
    "capfd",
    "capsysbinary",
    "capfdbinary",
    "caplog",
    "monkeypatch",
    "pytestconfig",
    "cache",
    "recwarn",
    "doctest_namespace",
    "record_property",
    "record_testsuite_property",
    "record_xml_attribute",
];
const MARKS: &[&str] = &[
    "parametrize",
    "usefixtures",
    "filterwarnings",
    "skip",
    "skipif",
    "xfail",
    "tryfirst",
    "trylast",
];
fn decorators(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    function: &ast::FuncDef,
) -> Vec<(String, ast::Expr)> {
    function
        .decorators()
        .take(64)
        .filter_map(|d| d.expr())
        .filter_map(|e| {
            let head = match e.clone() {
                ast::Expr::Call(c) => c.callee()?,
                e => e,
            };
            Some((identity(inf, file, &head)?, e))
        })
        .collect()
}
fn args(expr: &ast::Expr) -> Vec<ast::Expr> {
    if let ast::Expr::Call(c) = expr {
        c.arg_list()
            .map(|a| {
                a.args()
                    .filter_map(|a| match a {
                        ast::AnyArg::Positional(p) => p.expr(),
                        _ => None,
                    })
                    .take(128)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        vec![]
    }
}
fn names(expr: &ast::Expr) -> Vec<String> {
    if let Some(s) = literal(expr) {
        s.split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        match expr {
            ast::Expr::List(l) => l
                .elements()
                .map(|e| literal(&e))
                .collect::<Option<Vec<_>>>()
                .unwrap_or_default(),
            ast::Expr::Tuple(l) => l
                .elements()
                .map(|e| literal(&e))
                .collect::<Option<Vec<_>>>()
                .unwrap_or_default(),
            _ => vec![],
        }
    }
}
fn inventory(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    fixtures: &mut HashMap<String, (TextRange, bool)>,
) {
    for function in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .take(2048)
    {
        let Some(name) = function.name() else {
            continue;
        };
        for (id, expr) in decorators(inf, file, &function) {
            if matches!(id.as_str(), "pytest.fixture" | "pytest.yield_fixture") {
                let mut fixture = name.text().to_string();
                let mut autouse = false;
                if let ast::Expr::Call(c) = expr {
                    if let Some(a) = c.arg_list() {
                        for k in a.keywords() {
                            if k.name_text() == "name" {
                                if let Some(v) = k.value().and_then(|e| literal(&e)) {
                                    fixture = v;
                                }
                            }
                            if k.name_text() == "autouse" {
                                autouse = k.value().is_some_and(|e| e.syntax().text() == "True");
                            }
                        }
                    }
                }
                fixtures.insert(fixture, (name.text_range(), autouse));
            }
        }
    }
}
pub(super) fn run(inf: &Inferer<'_>, file: &Arc<FileInfo>, inputs: &Inputs, sink: &mut Sink) {
    let mut fixtures = HashMap::new();
    inventory(inf, file, &mut fixtures);
    let local: HashSet<_> = fixtures.keys().cloned().collect();
    let mut directory = file.path.parent();
    for _ in 0..16 {
        let Some(dir) = directory else {
            break;
        };
        if inputs.root.as_ref().is_some_and(|r| !dir.starts_with(r)) {
            break;
        }
        let path = dir.join("conftest.py");
        if path != file.path && path.is_file() {
            if let Some(owner) = inf.db.file_for_path(&path) {
                inventory(inf, &owner, &mut fixtures);
            }
        }
        if inputs.root.as_deref() == Some(dir) {
            break;
        }
        directory = dir.parent();
    }
    for name in &inputs.plugin_fixtures {
        fixtures
            .entry(name.clone())
            .or_insert((TextRange::empty(TextSize::new(0)), true));
    }
    for name in BUILTIN_FIXTURES {
        fixtures
            .entry((*name).into())
            .or_insert((TextRange::empty(TextSize::new(0)), true));
    }
    let mut registered: HashSet<String> = inputs
        .registered_marks
        .iter()
        .cloned()
        .chain(MARKS.iter().map(|m| (*m).into()))
        .collect();
    if let Some(root) = &inputs.root {
        for name in ["pytest.ini", "setup.cfg", "pyproject.toml"] {
            if let Some(text) = read(&root.join(name)) {
                if name.ends_with(".toml") {
                    if let Ok(value) = toml::from_str::<toml::Value>(&text) {
                        if let Some(marks) = value
                            .get("tool")
                            .and_then(|v| v.get("pytest"))
                            .and_then(|v| v.get("ini_options"))
                            .and_then(|v| v.get("markers"))
                            .and_then(|v| v.as_array())
                        {
                            for mark in marks.iter().filter_map(|v| v.as_str()) {
                                registered.insert(
                                    mark.split([':', '(']).next().unwrap_or("").trim().into(),
                                );
                            }
                        }
                    }
                } else {
                    let mut in_marks = false;
                    for line in text.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("markers") && trimmed.contains('=') {
                            in_marks = true;
                            let mark = trimmed.split_once('=').unwrap().1.trim();
                            if !mark.is_empty() {
                                registered.insert(
                                    mark.split([':', '(']).next().unwrap_or("").trim().into(),
                                );
                            }
                        } else if in_marks
                            && line.starts_with(char::is_whitespace)
                            && !trimmed.is_empty()
                        {
                            registered.insert(
                                trimmed.split([':', '(']).next().unwrap_or("").trim().into(),
                            );
                        } else if !trimmed.is_empty() {
                            in_marks = false;
                        }
                    }
                }
            }
        }
    }
    let mut used = HashSet::new();
    for function in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .take(2048)
    {
        let Some(name) = function.name() else {
            continue;
        };
        let decs = decorators(inf, file, &function);
        let fixture = decs
            .iter()
            .any(|(n, _)| matches!(n.as_str(), "pytest.fixture" | "pytest.yield_fixture"));
        let test = name.text().starts_with("test_");
        let mut parametrized = HashSet::new();
        for (id, expr) in &decs {
            if let Some(mark) = id.strip_prefix("pytest.mark.") {
                if !registered.contains(mark) {
                    warn(
                        expr.syntax(),
                        "PytestUnknownMark",
                        format!(
                            "Unknown pytest mark '{mark}'; register it in pytest configuration"
                        ),
                        vec![],
                        sink,
                    );
                }
                let values = args(expr);
                if mark == "usefixtures" {
                    for value in &values {
                        if let Some(name) = literal(value) {
                            used.insert(name.clone());
                            if inputs.fixtures_complete && !fixtures.contains_key(&name) {
                                warn(
                                    value.syntax(),
                                    "PytestUnknownFixture",
                                    format!("Unknown pytest fixture '{name}'"),
                                    vec![],
                                    sink,
                                );
                            }
                        }
                    }
                }
                if mark == "parametrize" && values.len() >= 2 {
                    let parameters = names(&values[0]);
                    parametrized.extend(parameters.iter().cloned());
                    if parameters.iter().collect::<HashSet<_>>().len() != parameters.len() {
                        warn(
                            values[0].syntax(),
                            "PytestParametrize",
                            "Duplicate parametrized argument name",
                            vec![],
                            sink,
                        );
                    }
                    let rows: Vec<_> = match &values[1] {
                        ast::Expr::List(l) => l.elements().take(256).collect(),
                        ast::Expr::Tuple(l) => l.elements().take(256).collect(),
                        _ => vec![],
                    };
                    if parameters.len() > 1 {
                        for row in rows {
                            let count = match &row {
                                ast::Expr::List(l) => Some(l.elements().count()),
                                ast::Expr::Tuple(l) => Some(l.elements().count()),
                                _ => None,
                            };
                            if count.is_some_and(|c| c != parameters.len()) {
                                warn(
                                    row.syntax(),
                                    "PytestParametrize",
                                    format!("Expected {} parametrized values", parameters.len()),
                                    vec![],
                                    sink,
                                );
                            }
                        }
                    }
                    if let Some(params) = function.params() {
                        let formal: HashSet<_> = params
                            .params()
                            .filter_map(|p| p.name())
                            .map(|n| n.text().to_string())
                            .collect();
                        for parameter in parameters {
                            if !formal.contains(&parameter) {
                                warn(
                                    values[0].syntax(),
                                    "PytestParametrize",
                                    format!(
                                        "Parametrized argument '{parameter}' is not declared by this function"
                                    ),
                                    vec![],
                                    sink,
                                );
                            }
                        }
                    }
                }
            }
        }
        if fixture || test {
            if let Some(params) = function.params() {
                for param in params.params() {
                    let Some(name) = param.name() else {
                        continue;
                    };
                    if matches!(name.text(), "self" | "cls")
                        || param.default().is_some()
                        || parametrized.contains(name.text())
                    {
                        continue;
                    }
                    used.insert(name.text().to_string());
                    if inputs.fixtures_complete && !fixtures.contains_key(name.text()) {
                        warn(
                            param.syntax(),
                            "PytestUnknownFixture",
                            format!("Unknown pytest fixture '{}'", name.text()),
                            vec![],
                            sink,
                        );
                    }
                }
            }
        }
        if fixture {
            let yields: Vec<_> = function
                .body()
                .into_iter()
                .flat_map(|b| b.syntax().descendants().collect::<Vec<_>>())
                .filter(|n| {
                    n.kind() == rustpy_syntax::SyntaxKind::YIELD_EXPR && own(n, function.syntax())
                })
                .collect();
            let mut yield_blocks = HashMap::new();
            for point in &yields {
                if let Some(block) = point
                    .ancestors()
                    .find(|n| n.kind() == rustpy_syntax::SyntaxKind::BLOCK)
                {
                    *yield_blocks.entry(block.text_range()).or_insert(0usize) += 1;
                }
            }
            if yield_blocks.values().any(|count| *count > 1) {
                warn(
                    function.syntax(),
                    "PytestYieldFixture",
                    "A yield fixture must yield exactly once on each execution path; multiple yields require separate branches",
                    vec![],
                    sink,
                );
            }
            if decs.iter().any(|(n, _)| n == "pytest.yield_fixture") {
                warn(
                    function.syntax(),
                    "PytestYieldFixture",
                    "yield_fixture is deprecated; use pytest.fixture",
                    vec![],
                    sink,
                );
            }
        }
    }
    // Discovery absence is not proof that a fixture is unused elsewhere. Only
    // private local fixtures in a complete captured inventory admit this warning.
    let dynamic_requests=file.parse.syntax_node().descendants().filter_map(ast::CallExpr::cast).any(|call|matches!(call.callee(),Some(ast::Expr::Attribute(a)) if a.attr_text()=="getfixturevalue"));
    if inputs.fixtures_complete && !dynamic_requests {
        for name in local {
            if name.starts_with('_') && !used.contains(&name) {
                if let Some((range, false)) = fixtures.get(&name) {
                    sink.push(
                        *range,
                        Severity::WeakWarning,
                        "PytestUnusedFixture",
                        format!("Private fixture '{name}' is not requested in this module and is not autouse"),
                        vec![],
                    );
                }
            }
        }
    }
    unittest(inf, file, sink);
}
fn unittest(inf: &Inferer<'_>, file: &Arc<FileInfo>, sink: &mut Sink) {
    for attribute in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::AttributeExpr::cast)
        .take(10000)
    {
        let replacement = match attribute.attr_text().as_str() {
            "assertEquals" => "assertEqual",
            "assertNotEquals" => "assertNotEqual",
            "assertAlmostEquals" => "assertAlmostEqual",
            "assertNotAlmostEquals" => "assertNotAlmostEqual",
            "assertRegexpMatches" => "assertRegex",
            "assertNotRegexpMatches" => "assertNotRegex",
            "assertRaisesRegexp" => "assertRaisesRegex",
            "failUnless" | "assert_" => "assertTrue",
            "failIf" => "assertFalse",
            "failUnlessEqual" => "assertEqual",
            "failIfEqual" => "assertNotEqual",
            _ => continue,
        };
        let Some(object) = attribute.object() else {
            continue;
        };
        let ty = inf.type_of_expr(file, &object);
        let Some(class) = ty.class_ref() else {
            continue;
        };
        let genuine = inf.mro(class).iter().any(|c| {
            inf.db.file(c.file).is_some_and(|f| {
                f.path.starts_with("<typeshed>")
                    && f.module_name
                        .as_deref()
                        .is_some_and(|m| m.starts_with("unittest"))
                    && c.name == "TestCase"
            })
        });
        let selected = inf.mro(class).iter().find_map(|owner| {
            inf.class_info(owner).and_then(|info| {
                info.members
                    .contains_key(attribute.attr_text().as_str())
                    .then(|| owner.clone())
            })
        });
        // Python 3.12+ stubs omit removed aliases. An absent alias still gets
        // migration advice when the real TestCase owns its replacement; a
        // local alias override remains the first selected definition.
        let selected = selected.or_else(|| {
            inf.mro(class).iter().find_map(|owner| {
                inf.class_info(owner).and_then(|info| {
                    info.members
                        .contains_key(replacement)
                        .then(|| owner.clone())
                })
            })
        });
        let canonical = selected.is_some_and(|owner| {
            inf.db.file(owner.file).is_some_and(|file| {
                file.path.starts_with("<typeshed>")
                    && file
                        .module_name
                        .as_deref()
                        .is_some_and(|name| name.starts_with("unittest"))
            })
        });
        if genuine && canonical && !inf.has_unknown_base(class) {
            if let Some(token) = attribute.attr() {
                warn(
                    attribute.syntax(),
                    "PyUnittestDeprecatedAlias",
                    format!("Use unittest.{replacement} instead of deprecated alias"),
                    vec![QuickFix {
                        title: format!("Replace with {replacement}"),
                        edits: vec![(token.text_range(), replacement.into())],
                    }],
                    sink,
                );
            }
        }
    }
}
