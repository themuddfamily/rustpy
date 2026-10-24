//! Framework/tool consumers through public analysis and document APIs.
//! Test execution is intentionally deferred to the central final feature gate.
use rustpy_analysis::inspections::{
    self,
    framework_tools::{documents, Inputs},
    InspectionOptions, Problem,
};
use rustpy_semantic::db::Analysis;
use std::{path::Path, sync::Arc};
fn inspect(source: &str, inputs: Inputs) -> Vec<Problem> {
    let db = Analysis::for_project(Path::new("/framework-inspections"));
    let id = db.set_file_text(Path::new("/framework-inspections/test_m.py"), source);
    inspections::inspect(
        &db,
        &db.file(id).unwrap(),
        &InspectionOptions {
            framework: Arc::new(inputs),
            ..Default::default()
        },
    )
}
fn count(problems: &[Problem], code: &str) -> usize {
    problems
        .iter()
        .filter(|p| p.diagnostic.code == code)
        .count()
}
#[test]
fn interpreter_state_is_explicit_and_missing_inventory_is_not_empty_inventory() {
    let p = inspect("pass\n", Inputs::default());
    assert_eq!(count(&p, "PyInterpreter"), 0);
    let p = inspect(
        "pass\n",
        Inputs {
            interpreter_checked: true,
            ..Default::default()
        },
    );
    assert_eq!(count(&p, "PyInterpreter"), 1, "{p:?}");
    let p = inspect(
        "pass\n",
        Inputs {
            interpreter_checked: true,
            interpreter: Some("/missing/python".into()),
            ..Default::default()
        },
    );
    assert!(
        p.iter()
            .any(|p| p.diagnostic.message.contains("Invalid Python")),
        "{p:?}"
    );
}
#[test]
fn pytest_fixture_mark_parametrize_and_yield_obligations_have_alias_authority() {
    let p = inspect(
        "import pytest as pt\n@pt.fixture\ndef resource():\n yield 1\n yield 2\n@pt.mark.unknown\n@pt.mark.parametrize('a,b',[(1,)])\ndef test_values(a,b,missing,resource):pass\n",
        Inputs {
            fixtures_complete: true,
            ..Default::default()
        },
    );
    for code in [
        "PytestUnknownMark",
        "PytestParametrize",
        "PytestUnknownFixture",
        "PytestYieldFixture",
    ] {
        assert!(count(&p, code) > 0, "{code}: {p:?}");
    }
    let p = inspect(
        "import pytest as pt\n@pt.fixture\ndef resource():yield 1\n@pt.mark.custom\n@pt.mark.parametrize('a,b',[(1,2)])\ndef test_values(a,b,resource,tmp_path,plugin_value):pass\n",
        Inputs {
            fixtures_complete: true,
            plugin_fixtures: vec!["plugin_value".into()],
            registered_marks: vec!["custom".into()],
            ..Default::default()
        },
    );
    for code in [
        "PytestUnknownMark",
        "PytestParametrize",
        "PytestUnknownFixture",
        "PytestYieldFixture",
    ] {
        assert_eq!(count(&p, code), 0, "{code}: {p:?}");
    }
}
#[test]
fn pytest_dynamic_plugins_and_fake_decorators_are_conservative() {
    let p = inspect("def test_value(plugin_fixture):pass\n", Inputs::default());
    assert_eq!(count(&p, "PytestUnknownFixture"), 0);
    let p = inspect(
        "class Fake:\n def fixture(self,f):return f\npytest=Fake()\n@pytest.fixture\ndef resource():\n yield 1\n yield 2\n",
        Inputs::default(),
    );
    assert_eq!(count(&p, "PytestYieldFixture"), 0, "{p:?}");
    let p = inspect(
        "import pytest\n@pytest.fixture\ndef resource():\n if flag:yield 1\n else:yield 2\n",
        Inputs::default(),
    );
    assert_eq!(count(&p, "PytestYieldFixture"), 0, "{p:?}");
}
#[test]
fn unittest_deprecated_alias_fix_requires_real_testcase_ancestry() {
    let p = inspect(
        "import unittest\nclass Example(unittest.TestCase):\n def test_value(self):self.assertEquals(1,1)\nclass Fake:\n def assertEquals(self,a,b):pass\nFake().assertEquals(1,1)\n",
        Inputs::default(),
    );
    assert_eq!(count(&p, "PyUnittestDeprecatedAlias"), 1, "{p:?}");
    assert!(p.iter().any(|p| {
        p.diagnostic.code == "PyUnittestDeprecatedAlias"
            && p.fixes
                .iter()
                .any(|f| f.edits.iter().any(|(_, v)| v == "assertEqual"))
    }));
}
#[test]
fn requirement_versions_packages_and_install_fix_use_shared_pep508_parser() {
    let inputs = Inputs {
        installed: Some(vec![("requests".into(), "1.0".into())]),
        ..Default::default()
    };
    let p = documents::inspect_with_inputs(
        "requests>=2\nmissing-package==1.0\ninvalid>=banana\n",
        Path::new("requirements.txt"),
        &inputs,
    );
    assert_eq!(count(&p, "PyPackageRequirements"), 2, "{p:?}");
    assert!(count(&p, "PyRequirementSpecifier") > 0, "{p:?}");
    assert!(p
        .iter()
        .flat_map(|p| &p.fixes)
        .any(|f| inspections::install_package_target(&f.title) == Some("requests>=2")));
    let p = documents::inspect_with_inputs(
        "requests>=2\n",
        Path::new("requirements.txt"),
        &Inputs::default(),
    );
    assert_eq!(count(&p, "PyPackageRequirements"), 0);
}
#[test]
fn package_context_and_advisories_are_project_and_version_bound() {
    let temp = tempfile::TempDir::with_prefix("framework-tool-project").unwrap();
    std::fs::write(temp.path().join("requirements.txt"), "requests>=2\n").unwrap();
    let p = inspect(
        "import requests\nimport yaml\n",
        Inputs {
            root: Some(temp.path().into()),
            installed: Some(vec![("requests".into(), "1.0".into())]),
            vulnerabilities: vec![
                ("requests".into(), "1.0".into(), "OSV-TEST".into()),
                ("requests".into(), "2.0".into(), "STALE".into()),
            ],
            ..Default::default()
        },
    );
    assert_eq!(count(&p, "PyPackageRequirements"), 1, "{p:?}");
    assert_eq!(count(&p, "PyUnlistedDependency"), 1, "{p:?}");
    assert_eq!(count(&p, "PyVulnerablePackage"), 1, "{p:?}");
}
#[test]
fn pyproject_schema_and_setup_requirements_reject_invalid_shapes() {
    let p = documents::inspect(
        "[project]\nname=42\nversion='1.0'\ndependencies=['requests>=banana']\n[build-system]\nrequires='setuptools'\n",
        Path::new("pyproject.toml"),
    );
    assert!(count(&p, "PyProjectSchema") >= 2, "{p:?}");
    assert_eq!(count(&p, "PyRequirementSpecifier"), 1, "{p:?}");
    let p = documents::inspect(
        "[project]\nname='sample'\nversion='1.0'\ndependencies=['requests>=2']\n[build-system]\nrequires=['setuptools>=64']\n",
        Path::new("pyproject.toml"),
    );
    assert!(p.is_empty(), "{p:?}");
    let p = inspect(
        "from setuptools import setup\nsetup(name=42,install_requires=['requests>=banana'])\n",
        Inputs::default(),
    );
    assert_eq!(count(&p, "PySetupSchema"), 1, "{p:?}");
    assert_eq!(count(&p, "PyRequirementSpecifier"), 1, "{p:?}");
}
#[test]
fn json_schema_validates_required_type_enum_and_local_refs() {
    let schema = serde_json::json!({"type":"object","required":["name"],"properties":{"name":{"type":"string"},"mode":{"$ref":"#/$defs/mode"}},"additionalProperties":false,"$defs":{"mode":{"enum":["safe","fast"]}}});
    let p = documents::inspect_json_schema(r#"{"name":1,"mode":"bad","extra":true}"#, &schema);
    assert_eq!(count(&p, "JsonSchema"), 3, "{p:?}");
    assert!(documents::inspect_json_schema(r#"{"name":"ok","mode":"safe"}"#, &schema).is_empty());
}
#[test]
fn github_actions_schema_uses_yaml_tree_not_comment_substrings() {
    let p = documents::inspect(
        "on: push\njobs:\n  test:\n    steps:\n      - run: echo ok\n        uses: actions/checkout@v4\n",
        Path::new(".github/workflows/test.yml"),
    );
    assert_eq!(count(&p, "YamlSchema"), 2, "{p:?}");
    let p = documents::inspect(
        "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - run: echo ok\n",
        Path::new(".github/workflows/test.yml"),
    );
    assert!(p.is_empty(), "{p:?}");
}
#[test]
fn docker_env_ignore_and_editorconfig_checks_have_safe_fixes() {
    let p = documents::inspect(
        "FROM ubuntu\nFROM ubuntu:24.04 AS base\nFROM base\n",
        Path::new("Dockerfile"),
    );
    assert_eq!(count(&p, "Dockerfile"), 1, "{p:?}");
    let p = documents::inspect("TOKEN=value\nTOKEN=other\n", Path::new(".env"));
    assert_eq!(count(&p, "EnvFile"), 1);
    assert_eq!(p[0].fixes[0].edits[0].1, "");
    let p = documents::inspect("target/\ntarget/\n", Path::new(".gitignore"));
    assert_eq!(count(&p, "GitIgnorePattern"), 1);
    let p = documents::inspect(
        "root=true\n[*]\nindent_style=other\nend_of_line=crlf\n",
        Path::new(".editorconfig"),
    );
    assert_eq!(count(&p, "EditorConfig"), 1, "{p:?}");
}
#[test]
fn markdown_links_headers_and_todo_are_document_consumers() {
    let p = documents::inspect(
        "# Start\n### Jump\n[missing](missing-file.md)\n<!-- TODO finish -->\n",
        Path::new("/framework-inspections/README.md"),
    );
    assert_eq!(count(&p, "Markdown"), 2, "{p:?}");
    assert_eq!(count(&p, "TodoComment"), 1);
    let p = inspect("# FIXME implement\nvalue=1\n", Inputs::default());
    assert_eq!(count(&p, "TodoComment"), 1, "{p:?}");
}
#[test]
fn framework_constructors_are_canonical_and_dynamic_calls_are_not_guessed() {
    let p = inspect(
        "from fastapi import Depends\nfrom pydantic import Field\nfrom django.db.models import ForeignKey,CharField\nfrom sqlalchemy.orm import relationship\ndef make_dependency()->int:return 1\nDepends(make_dependency())\nField(1,default_factory=list)\nForeignKey('User')\nCharField(max_length=-1)\nrelationship(42)\n",
        Inputs::default(),
    );
    for code in [
        "PyFastApiDependency",
        "PyPydantic",
        "PyDjangoModel",
        "PySqlAlchemyModel",
    ] {
        assert!(count(&p, code) > 0, "{code}: {p:?}");
    }
    let p = inspect(
        "def Depends(value):return value\nDepends(make_dependency())\n",
        Inputs::default(),
    );
    assert_eq!(count(&p, "PyFastApiDependency"), 0, "{p:?}");
}
#[test]
fn django_template_catalog_respects_load_scopes_filters_and_url_names() {
    let inputs = Inputs {
        django_complete: true,
        django_template_engine: true,
        template_tags: vec!["load".into(), "url".into()],
        template_filters: vec!["lower".into()],
        url_names: vec!["home".into()],
        template_libraries: vec![("custom".into(), vec!["greet".into()], vec!["pretty".into()])],
        ..Default::default()
    };
    let p = documents::inspect_with_inputs(
        "{% greet %}{{ value|pretty }}{% url 'missing' %}",
        Path::new("templates/page.html"),
        &inputs,
    );
    assert_eq!(count(&p, "DjangoTemplate"), 3, "{p:?}");
    let p = documents::inspect_with_inputs(
        "{% load custom %}{% greet %}{{ value|pretty|lower }}{% url 'home' %}{{ 'text|fake' }}",
        Path::new("templates/page.html"),
        &inputs,
    );
    assert_eq!(count(&p, "DjangoTemplate"), 0, "{p:?}");
    assert!(documents::inspect_with_inputs(
        "{% unknown_tag %}",
        Path::new("templates/page.html"),
        &Inputs::default()
    )
    .is_empty());
}
#[test]
fn copyright_uses_the_existing_header_planner_and_supplies_a_real_edit() {
    let p = inspect(
        "value=1\n",
        Inputs {
            copyright_template: Some("Copyright (c) $today.year Example".into()),
            copyright_year: "2026".into(),
            ..Default::default()
        },
    );
    assert_eq!(count(&p, "Copyright"), 1, "{p:?}");
    assert!(p
        .iter()
        .filter(|p| p.diagnostic.code == "Copyright")
        .flat_map(|p| &p.fixes)
        .any(|f| f
            .edits
            .iter()
            .any(|(_, text)| text.contains("2026 Example"))));
}
#[test]
fn embedded_html_css_http_and_rst_have_real_source_contracts() {
    let p = documents::inspect(
        "<html><body><div><span>bad</div></body></html>",
        Path::new("page.html"),
    );
    assert!(count(&p, "HtmlSyntax") > 0, "{p:?}");
    let p = documents::inspect(
        "<!doctype html><html><body><input value='>'><script>if(a<b){x=1;}</script><div>ok</div></body></html>",
        Path::new("page.html"),
    );
    assert_eq!(count(&p, "HtmlSyntax"), 0, "{p:?}");
    let p = documents::inspect("a { color: red;\n", Path::new("style.css"));
    assert_eq!(count(&p, "CssSyntax"), 1, "{p:?}");
    let p = documents::inspect(
        "a::before {content:'}';color:rgb(1,2,3)} /* [ */",
        Path::new("style.css"),
    );
    assert_eq!(count(&p, "CssSyntax"), 0, "{p:?}");
    let p = documents::inspect(
        "GET ftp://example.test/file HTTP/9\nHost: first\nHost: second\n",
        Path::new("client.http"),
    );
    assert!(count(&p, "HttpClient") >= 2, "{p:?}");
    let p = documents::inspect(
        "GET https://{{host}}/data HTTP/1.1\nAccept: application/json\n",
        Path::new("client.http"),
    );
    assert_eq!(count(&p, "HttpClient"), 0, "{p:?}");
    let p = documents::inspect(
        "Overview\n========\n\n`Overview`_ and `missing`_\n",
        Path::new("page.rst"),
    );
    assert_eq!(count(&p, "RstInspection"), 1, "{p:?}");
}
#[test]
fn full_json_schema_handles_conditionals_unevaluated_and_numeric_equality() {
    let schema = serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"kind":{"enum":[1]},"value":{}},"if":{"properties":{"kind":{"const":1}}},"then":{"properties":{"value":{"type":"string"}}},"unevaluatedProperties":false});
    assert!(documents::inspect_json_schema(r#"{"kind":1.0,"value":"ok"}"#, &schema).is_empty());
    let p = documents::inspect_json_schema(r#"{"kind":1,"value":42,"extra":1}"#, &schema);
    assert!(count(&p, "JsonSchema") >= 2, "{p:?}");
}
#[test]
fn captured_schema_resources_resolve_without_any_network_or_filesystem_retrieval() {
    let schema = serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema","properties":{"value":{"$ref":"https://json.schemastore.org/captured-test.json"}}});
    let inputs = Inputs {
        schemas: inspections::framework_tools::SchemaSnapshot {
            generation: 1,
            values: Arc::new(vec![
                (
                    "https://json.schemastore.org/main-test.json".into(),
                    schema.to_string(),
                ),
                (
                    "https://json.schemastore.org/captured-test.json".into(),
                    serde_json::json!({"type":"integer"}).to_string(),
                ),
            ]),
        },
        ..Default::default()
    };
    let p = documents::inspect_with_inputs(
        r#"{"$schema":"https://json.schemastore.org/main-test.json","value":"wrong"}"#,
        Path::new("settings.json"),
        &inputs,
    );
    assert_eq!(count(&p, "JsonSchema"), 1, "{p:?}");
}
#[test]
fn explicit_namespace_roles_and_unicode_path_policy_do_not_reject_ordinary_packages() {
    let inputs = Inputs {
        namespace_roots: vec!["/project/pkg".into()],
        warn_non_ascii_paths: true,
        ..Default::default()
    };
    let p = documents::inspect_with_inputs("", Path::new("/project/pkg/__init__.py"), &inputs);
    assert_eq!(count(&p, "NamespacePackage"), 1);
    let p =
        documents::inspect_with_inputs("", Path::new("/project/pkg/child/__init__.py"), &inputs);
    assert_eq!(count(&p, "NamespacePackage"), 0);
    let p = documents::inspect_with_inputs("", Path::new("/project/π.py"), &inputs);
    assert_eq!(count(&p, "NonAsciiPath"), 1);
    assert!(documents::inspect("", Path::new("/project/π.py")).is_empty());
}
#[test]
fn regex_variable_width_and_redundant_escapes_are_distinct_from_valid_assertions() {
    assert!(rustpy_regex::check("(?<=a+)x", false)
        .iter()
        .any(|issue| issue.message.contains("fixed-width")));
    assert!(rustpy_regex::check("(?<=ab|cd)x", false).is_empty());
    assert!(rustpy_regex::check("(?<=(?=a+)b)x", false).is_empty());
    assert_eq!(
        rustpy_regex::inspection_checks::redundant_escapes(r"\_", false).len(),
        1
    );
    assert!(rustpy_regex::inspection_checks::redundant_escapes(r"[\-]\#", true).is_empty());
}
#[test]
fn requirement_markers_use_selected_interpreter_and_optional_groups_are_not_mandatory() {
    let environment = [
        ("implementation_name", "cpython"),
        ("implementation_version", "3.12.0"),
        ("os_name", "posix"),
        ("platform_machine", "x86_64"),
        ("platform_python_implementation", "CPython"),
        ("platform_release", "6.0"),
        ("platform_system", "Linux"),
        ("platform_version", "6.0"),
        ("python_full_version", "3.12.0"),
        ("python_version", "3.12"),
        ("sys_platform", "linux"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    let inputs = Inputs {
        installed: Some(vec![]),
        marker_environment: environment,
        ..Default::default()
    };
    let p = documents::inspect_with_inputs(
        "old-only; python_version < '3.8'\nnew-only; python_version >= '3.8'\n",
        Path::new("requirements.txt"),
        &inputs,
    );
    assert_eq!(count(&p, "PyPackageRequirements"), 1, "{p:?}");
    assert!(p.iter().any(|p| p.diagnostic.message.contains("new-only")));
    let p =
        documents::inspect_with_inputs("uninstalled<2\n", Path::new("constraints.txt"), &inputs);
    assert_eq!(count(&p, "PyPackageRequirements"), 0, "{p:?}");
    let temp = tempfile::TempDir::with_prefix("framework-optional").unwrap();
    std::fs::write(temp.path().join("pyproject.toml"),"[project]\nname='example'\nversion='1.0'\n[project.optional-dependencies]\ntest=['pytest>=7']\n").unwrap();
    let p = inspect(
        "import pytest\n",
        Inputs {
            root: Some(temp.path().into()),
            installed: Some(vec![]),
            ..Default::default()
        },
    );
    assert_eq!(count(&p, "PyPackageRequirements"), 0, "{p:?}");
    assert_eq!(count(&p, "PyUnlistedDependency"), 0, "{p:?}");
    let p = inspect(
        "import pytest\n",
        Inputs {
            root: Some(temp.path().into()),
            installed: Some(vec![]),
            active_extras: vec!["test".into()],
            ..Default::default()
        },
    );
    assert_eq!(count(&p, "PyPackageRequirements"), 1, "{p:?}");
}
#[test]
fn unittest_local_overrides_and_dynamic_fixture_requests_do_not_borrow_authority() {
    let p = inspect(
        "import unittest\nclass Example(unittest.TestCase):\n def assertEquals(self,a,b):pass\n def test_value(self):self.assertEquals(1,1)\n",
        Inputs::default(),
    );
    assert_eq!(count(&p, "PyUnittestDeprecatedAlias"), 0, "{p:?}");
    let p = inspect(
        "import pytest\n@pytest.fixture\ndef _resource():return 1\ndef test_dynamic(request):request.getfixturevalue(name)\n",
        Inputs {
            fixtures_complete: true,
            ..Default::default()
        },
    );
    assert_eq!(count(&p, "PytestUnusedFixture"), 0, "{p:?}");
}

#[test]
fn pydantic_configuration_uses_canonical_heads_and_preserves_unknown_values() {
    let problems = inspect(
        "from pydantic import BaseModel, ConfigDict\nclass Model(BaseModel):\n    class Config:\n        extra = 'discard'\n        validate_assignment = 'yes'\nsettings = ConfigDict(extra='invalid', frozen=False)\n",
        Inputs::default(),
    );
    assert_eq!(count(&problems, "PyPydantic"), 3, "{problems:?}");
    let problems = inspect(
        "class BaseModel: pass\nclass Model(BaseModel):\n    class Config:\n        extra = 'discard'\ndef ConfigDict(**kwargs):return kwargs\nsettings=ConfigDict(extra='invalid')\n",
        Inputs::default(),
    );
    assert_eq!(count(&problems, "PyPydantic"), 0, "{problems:?}");
}

#[test]
fn captured_registered_web_diagnostics_are_file_and_kind_scoped() {
    let p = inspect(
        "def endpoint():\n    return {'name': 42}\n",
        Inputs {
            web_issues: vec![
                (
                    "/framework-inspections/test_m.py".into(),
                    2,
                    "PyRoute".into(),
                    "Registered response model rejects the literal return".into(),
                ),
                (
                    "/framework-inspections/test_m.py".into(),
                    1,
                    "PySqlAlchemyModel".into(),
                    "Column default does not match declared SQL type".into(),
                ),
                (
                    "/another/file.py".into(),
                    1,
                    "PyRoute".into(),
                    "Other module".into(),
                ),
                (
                    "/framework-inspections/test_m.py".into(),
                    1,
                    "syntax".into(),
                    "Unadmitted tool code".into(),
                ),
            ],
            ..Default::default()
        },
    );
    assert_eq!(count(&p, "PyRoute"), 1, "{p:?}");
    assert_eq!(count(&p, "PySqlAlchemyModel"), 1, "{p:?}");
    let route = p.iter().find(|p| p.diagnostic.code == "PyRoute").unwrap();
    assert_eq!(usize::from(route.diagnostic.range.start()), 16);
    assert!(p
        .iter()
        .all(|p| p.diagnostic.message != "Unadmitted tool code"));
}
#[test]
fn fastapi_response_model_must_be_a_type_compatible_with_the_return_annotation() {
    let header = "from fastapi import FastAPI\nfrom pydantic import BaseModel\napp = FastAPI()\nclass Item(BaseModel):\n    name: str\nclass Detailed(Item):\n    extra: int\nclass User(BaseModel):\n    email: str\nclass Plain:\n    pass\ndef helper():\n    return 1\n";
    // Route identities need the framework classes from an installed library.
    let tmp = tempfile::TempDir::with_prefix("fastapi-response-model").unwrap();
    let base = tmp.path().canonicalize().unwrap();
    for (path, text) in [
        (
            "site/fastapi/__init__.py",
            "class FastAPI:\n    def get(self, path, **kwargs): ...\n    def post(self, path, **kwargs): ...\n",
        ),
        ("site/pydantic/__init__.py", "class BaseModel: ...\n"),
    ] {
        std::fs::create_dir_all(base.join(path).parent().unwrap()).unwrap();
        std::fs::write(base.join(path), text).unwrap();
    }
    let problems = |body: &str| {
        let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
            vec![base.join("proj")],
            vec![base.join("site")],
            (3, 12),
        ));
        let id = db.set_file_text(&base.join("proj/main.py"), &format!("{header}{body}"));
        let p = inspections::inspect(&db, &db.file(id).unwrap(), &InspectionOptions::default());
        count(&p, "PyFastApiResponseModel")
    };
    // Not a type: literals, plain classes, functions and model instances.
    assert_eq!(
        problems("@app.get('/a', response_model='Item')\ndef a():\n    pass\n"),
        1
    );
    assert_eq!(
        problems("@app.get('/a', response_model=Plain)\ndef a():\n    pass\n"),
        1
    );
    assert_eq!(
        problems("@app.get('/a', response_model=helper)\ndef a():\n    pass\n"),
        1
    );
    assert_eq!(
        problems("@app.get('/a', response_model=Item(name='x'))\ndef a():\n    pass\n"),
        1
    );
    // A different, unrelated model annotation conflicts.
    assert_eq!(
        problems("@app.get('/a', response_model=Item)\ndef a() -> User:\n    pass\n"),
        1
    );
    // Valid: same model, subclass, non-model annotation, None, generics.
    for ok in [
        "@app.get('/a', response_model=Item)\ndef a() -> Item:\n    pass\n",
        "@app.post('/a', response_model=Item)\nasync def a() -> Detailed:\n    pass\n",
        "@app.get('/a', response_model=Item)\ndef a() -> dict:\n    pass\n",
        "@app.get('/a', response_model=None)\ndef a() -> User:\n    pass\n",
        "@app.get('/a', response_model=list[Item])\ndef a():\n    pass\n",
        "@app.get('/a', response_model=int)\ndef a():\n    pass\n",
    ] {
        assert_eq!(problems(ok), 0, "{ok}");
    }
    // Look-alike decorators of non-FastAPI objects are left alone.
    assert_eq!(
        problems("class Router:\n    def get(self, path, response_model=None):\n        return lambda f: f\nr = Router()\n@r.get('/a', response_model='x')\ndef a():\n    pass\n"),
        0
    );
}
/// The installed packages' real layout: FastAPI re-exported from a large
/// documented `applications` module that delegates to `APIRouter.api_route`,
/// pydantic's lazy `__init__` re-exporting a metaclass-built `BaseModel`, all
/// inside the project's `.venv`.
#[test]
fn fastapi_response_model_resolves_real_package_layout_inside_project_venv() {
    let tmp = tempfile::TempDir::with_prefix("fastapi-real-layout").unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let project = base.join("proj");
    write_real_web_layout(&project.join(".venv"));
    assert_real_fastapi_routes(
        &project,
        &project.join(".venv/lib/python3.12/site-packages"),
    );
}
/// A project `.venv` that is a symlink to an environment elsewhere: library
/// files are keyed by canonical path while the interpreter reports search
/// paths through the link, so module names (and with them the FastAPI and
/// Flask route identities) must match either spelling.
#[cfg(unix)]
#[test]
fn web_routes_resolve_real_package_layout_in_symlinked_project_venv() {
    let tmp = tempfile::TempDir::with_prefix("fastapi-symlinked-venv").unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let project = base.join("proj");
    write_real_web_layout(&base.join("venv"));
    std::fs::create_dir_all(&project).unwrap();
    std::os::unix::fs::symlink("../venv", project.join(".venv")).unwrap();
    let site = project.join(".venv/lib/python3.12/site-packages");
    let resolver = rustpy_semantic::module::ModuleResolver::new(
        vec![project.clone()],
        vec![site.clone()],
        (3, 12),
    );
    let canonical = base.join("venv/lib/python3.12/site-packages");
    assert_eq!(
        resolver
            .module_name_of(&canonical.join("fastapi/applications.py"))
            .as_deref(),
        Some("fastapi.applications")
    );
    assert_eq!(
        resolver
            .module_name_of(&canonical.join("flask/sansio/scaffold.py"))
            .as_deref(),
        Some("flask.sansio.scaffold")
    );
    assert_real_fastapi_routes(&project, &site);
    let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
        vec![project.clone()],
        vec![site.clone()],
        (3, 12),
    ));
    let flask_routes = |body: &str| {
        let id = db.set_file_text(
            &project.join("app.py"),
            &format!("from flask import Flask\napp = Flask(__name__)\n{body}"),
        );
        let p = inspections::inspect(&db, &db.file(id).unwrap(), &InspectionOptions::default());
        p.into_iter()
            .filter(|p| p.diagnostic.code == "PyRoute")
            .map(|p| p.diagnostic.message)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        flask_routes("@app.route(\"bad\")\ndef index():\n    return ''\n"),
        ["Route paths must begin with '/'"]
    );
    assert!(flask_routes("@app.route(\"/good\")\ndef index():\n    return ''\n").is_empty());
}
/// Writes FastAPI, Starlette, pydantic and Flask shaped like the installed
/// packages into the virtual environment `venv`.
fn write_real_web_layout(venv: &Path) {
    let site = venv.join("lib/python3.12/site-packages");
    // Documented parameters make the module far larger than a naive bounded walk.
    let documented = (0..80)
        .map(|n| {
            let params = (0..30)
                .map(|p| format!("        p{p}: Annotated[Optional[str], Doc(\"\"\"Parameter {p}.\"\"\")] = None,\n"))
                .collect::<String>();
            format!("    def helper{n}(\n        self,\n{params}    ) -> None:\n        self.extra = p0\n")
        })
        .collect::<String>();
    let verbs = ["get", "post", "put", "delete"]
        .iter()
        .map(|verb| {
            format!("    def {verb}(\n        self,\n        path: Annotated[str, Doc(\"\"\"The URL path.\"\"\")],\n        *,\n        response_model: Annotated[Any, Doc(\"\"\"The response model.\"\"\")] = Default(None),\n    ) -> Callable[[DecoratedCallable], DecoratedCallable]:\n        return self.router.{verb}(path, response_model=response_model)\n")
        })
        .collect::<String>();
    let router_verbs = ["get", "post", "put", "delete"]
        .iter()
        .map(|verb| {
            format!("    def {verb}(self, path: str, *, response_model: Any = Default(None)) -> Callable[[DecoratedCallable], DecoratedCallable]:\n        return self.api_route(path, response_model=response_model, methods=['{}'])\n", verb.to_uppercase())
        })
        .collect::<String>();
    for (path, text) in [
        ("pyvenv.cfg".to_string(), "home = /usr/bin\n".to_string()),
        (
            "fastapi/__init__.py".into(),
            "from .applications import FastAPI as FastAPI\nfrom .routing import APIRouter as APIRouter\n".into(),
        ),
        (
            "fastapi/datastructures.py".into(),
            "class DefaultPlaceholder:\n    def __init__(self, value): self.value = value\ndef Default(value): return DefaultPlaceholder(value)\n".into(),
        ),
        (
            "fastapi/routing.py".into(),
            format!("from typing import Any, Callable, TypeVar\nfrom fastapi.datastructures import Default\nDecoratedCallable = TypeVar('DecoratedCallable', bound=Callable[..., Any])\nclass APIRouter:\n    def api_route(self, path: str, *, response_model: Any = Default(None), methods=None) -> Callable[[DecoratedCallable], DecoratedCallable]:\n        def decorator(func: DecoratedCallable) -> DecoratedCallable:\n            return func\n        return decorator\n{router_verbs}"),
        ),
        (
            "fastapi/applications.py".into(),
            format!("from typing import Any, Callable, Optional, TypeVar\nfrom typing_extensions import Annotated, Doc\nfrom starlette.applications import Starlette\nfrom fastapi.datastructures import Default\nfrom fastapi.routing import APIRouter\nDecoratedCallable = TypeVar('DecoratedCallable', bound=Callable[..., Any])\nclass FastAPI(Starlette):\n    def __init__(self, *, debug: Annotated[bool, Doc(\"\"\"Debug.\"\"\")] = False) -> None:\n        self.router: APIRouter = APIRouter()\n{documented}{verbs}"),
        ),
        (
            "starlette/applications.py".into(),
            "class Starlette:\n    def __init__(self, debug: bool = False) -> None: ...\n".into(),
        ),
        ("starlette/__init__.py".into(), String::new()),
        (
            "pydantic/__init__.py".into(),
            "import typing\nfrom typing import TYPE_CHECKING\nif TYPE_CHECKING:\n    from .main import *\n__all__ = ('BaseModel',)\n_dynamic_imports = {'BaseModel': (__spec__.parent, '.main')}\ndef __getattr__(attr_name: str) -> object:\n    from importlib import import_module\n    package, module_name = _dynamic_imports[attr_name]\n    return getattr(import_module(module_name, package=package), attr_name)\n".into(),
        ),
        (
            "pydantic/main.py".into(),
            "import typing\nfrom ._internal import _model_construction\n__all__ = 'BaseModel', 'create_model'\nclass BaseModel(metaclass=_model_construction.ModelMetaclass):\n    model_config: typing.ClassVar[dict] = {}\n    def __init__(self, /, **data: typing.Any) -> None: ...\n".into(),
        ),
        ("pydantic/_internal/__init__.py".into(), String::new()),
        (
            "pydantic/_internal/_model_construction.py".into(),
            "from abc import ABCMeta\nclass ModelMetaclass(ABCMeta):\n    def __new__(mcs, cls_name, bases, namespace, **kwargs): return super().__new__(mcs, cls_name, bases, namespace)\n".into(),
        ),
        (
            "flask/__init__.py".into(),
            "from .app import Flask as Flask\nfrom .blueprints import Blueprint as Blueprint\n".into(),
        ),
        (
            "flask/app.py".into(),
            "import typing as t\nfrom .sansio.app import App\nclass Flask(App):\n    def __init__(self, import_name: str, static_url_path: t.Optional[str] = None) -> None:\n        super().__init__(import_name)\n".into(),
        ),
        (
            "flask/blueprints.py".into(),
            "from .sansio.blueprints import Blueprint as SansioBlueprint\nclass Blueprint(SansioBlueprint):\n    pass\n".into(),
        ),
        ("flask/sansio/__init__.py".into(), String::new()),
        (
            "flask/sansio/app.py".into(),
            "from .scaffold import Scaffold\nclass App(Scaffold):\n    def __init__(self, import_name: str) -> None:\n        super().__init__(import_name)\n".into(),
        ),
        (
            "flask/sansio/blueprints.py".into(),
            "from .scaffold import Scaffold\nclass Blueprint(Scaffold):\n    pass\n".into(),
        ),
        (
            "flask/sansio/scaffold.py".into(),
            "import typing as t\nT_route = t.TypeVar('T_route', bound=t.Callable[..., t.Any])\nF = t.TypeVar('F', bound=t.Callable[..., t.Any])\ndef setupmethod(f: F) -> F:\n    return f\nclass Scaffold:\n    def __init__(self, import_name: str) -> None:\n        self.import_name = import_name\n    @setupmethod\n    def route(self, rule: str, **options: t.Any) -> t.Callable[[T_route], T_route]:\n        def decorator(f: T_route) -> T_route:\n            return f\n        return decorator\n".into(),
        ),
    ] {
        let target = if path == "pyvenv.cfg" {
            venv.join(&path)
        } else {
            site.join(&path)
        };
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, text).unwrap();
    }
}
/// FastAPI `response_model` conflicts through the real package layout.
fn assert_real_fastapi_routes(project: &Path, site: &Path) {
    let header = "from fastapi import APIRouter, FastAPI\nfrom pydantic import BaseModel\napi = FastAPI()\nrouter = APIRouter()\nclass Item(BaseModel):\n    name: str\nclass Other(BaseModel):\n    x: int\n";
    let problems = |body: &str| {
        let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
            vec![project.to_path_buf()],
            vec![site.to_path_buf()],
            (3, 12),
        ));
        let id = db.set_file_text(&project.join("main.py"), &format!("{header}{body}"));
        let p = inspections::inspect(&db, &db.file(id).unwrap(), &InspectionOptions::default());
        p.into_iter()
            .filter(|p| p.diagnostic.code == "PyFastApiResponseModel")
            .map(|p| p.diagnostic.message)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        problems("@api.get('/x', response_model=Other)\ndef f() -> Item:\n    pass\n"),
        ["Return annotation 'Item' conflicts with response_model 'Other'"]
    );
    assert_eq!(
        problems("@router.post('/x', response_model=Other)\nasync def f() -> Item:\n    pass\n"),
        ["Return annotation 'Item' conflicts with response_model 'Other'"]
    );
    assert!(
        problems("@api.get('/x', response_model=Item)\ndef f() -> Item:\n    pass\n").is_empty()
    );
    assert_eq!(
        problems("@api.get('/x', response_model='Item')\ndef f():\n    pass\n"),
        ["response_model must be a Pydantic model or a type"]
    );
}
#[test]
fn django_orm_lookups_report_unknown_fields_on_fully_declared_models() {
    let models = "from django.db import models\nclass Author(models.Model):\n    name = models.CharField(max_length=50)\nclass Book(models.Model):\n    title = models.CharField(max_length=90)\n    author = models.ForeignKey(Author, on_delete=models.CASCADE)\n";
    let p = inspect(
        &format!("{models}Book.objects.filter(titel='x', author__name__icontains='y', author_id=1).order_by('-pk', 'author__nme')\nBook.objects.annotate(n=models.F('title')).filter(n='x')\n"),
        Inputs::default(),
    );
    let found: Vec<_> = p
        .iter()
        .filter(|p| p.diagnostic.code == "PyDjangoOrmLookup")
        .map(|p| p.diagnostic.message.as_str())
        .collect();
    assert_eq!(
        found,
        [
            "Cannot resolve 'titel' into a field of 'Book'",
            "Cannot resolve 'nme' into a field of 'Author'",
        ],
        "{p:?}"
    );
    assert!(
        rustpy_analysis::inspections::infrastructure::inspection("PyDjangoOrmLookup").is_some()
    );
}
#[test]
fn django_orm_lookups_check_q_keywords_and_select_related_cardinality() {
    let models = "from django.db import models\nfrom django.db.models import Q\nclass Author(models.Model):\n    name = models.CharField(max_length=50)\nclass Tag(models.Model):\n    label = models.CharField(max_length=20)\nclass Book(models.Model):\n    title = models.CharField(max_length=90)\n    author = models.ForeignKey(Author, on_delete=models.CASCADE)\n    tags = models.ManyToManyField(Tag)\n";
    let p = inspect(
        &format!("{models}Book.objects.filter(Q(titel='x') | ~Q(author__name='y'))\nBook.objects.select_related('author', 'tags')\nAuthor.objects.select_related('book')\nAuthor.objects.prefetch_related('book_set')\n"),
        Inputs::default(),
    );
    let found: Vec<_> = p
        .iter()
        .filter(|p| p.diagnostic.code == "PyDjangoOrmLookup")
        .map(|p| p.diagnostic.message.as_str())
        .collect();
    assert_eq!(
        found,
        [
            "Cannot resolve 'titel' into a field of 'Book'",
            "'tags' is a many-valued relation and cannot be used in select_related(); use prefetch_related()",
            "'book' is a many-valued relation and cannot be used in select_related(); use prefetch_related()",
        ],
        "{p:?}"
    );
}
