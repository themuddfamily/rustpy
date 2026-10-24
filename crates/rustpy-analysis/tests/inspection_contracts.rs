//! Source→finding→profile→fix→reanalysis contracts. Fixtures are intentionally
//! unexecuted during feature implementation; no interpreter/tool is invoked.
use rustpy_analysis::inspections::{
    self,
    framework_tools::{documents, Inputs, SchemaSnapshot},
    infrastructure::{self, Policy},
    InspectionOptions, Problem,
};
use rustpy_core::Severity;
use rustpy_semantic::Analysis;
use rustpy_settings::inspection_profiles::{Profiles, Rule};
use std::{path::Path, sync::Arc};

fn python(source: &str) -> Vec<Problem> {
    let analysis = Analysis::for_project(Path::new("/inspection-contract"));
    let id = analysis.set_file_text(Path::new("/inspection-contract/source.py"), source);
    let found = inspections::inspect(
        &analysis,
        &analysis.file(id).unwrap(),
        &InspectionOptions::default(),
    );
    for problem in &found {
        let metadata = infrastructure::inspection(&problem.diagnostic.code).unwrap_or_else(|| {
            panic!(
                "Unregistered actual provider diagnostic: {}",
                problem.diagnostic.code
            )
        });
        assert!(!metadata.name.is_empty() && !metadata.description_html.is_empty());
    }
    found
}
fn finding<'a>(problems: &'a [Problem], code: &str) -> &'a Problem {
    problems
        .iter()
        .find(|problem| problem.diagnostic.code == code)
        .unwrap_or_else(|| panic!("Missing {code}: {problems:?}"))
}
fn profile(code: &str, severity: Option<&str>, enabled: bool) -> Policy {
    let mut profiles = Profiles::default();
    profiles.ide[0].inspections.insert(
        code.into(),
        Rule {
            enabled,
            severity: severity.map(str::to_owned),
            ..Default::default()
        },
    );
    Policy::from_settings(&profiles, Some(Path::new("/inspection-contract")), false).unwrap()
}
#[test]
fn shipped_source_fixes_preserve_neighbors_parse_and_remove_the_original_obligation() {
    for (code, source, expected) in [
        (
            "PyNegatedIn",
            "def f(value, values):\n    return not value in values\n",
            "value not in values",
        ),
        (
            "PyIsLiteral",
            "def f(value):\n    return value is 5\n",
            "value == 5",
        ),
        (
            "PyRaiseNotImplemented",
            "def f():\n    raise NotImplemented\n",
            "raise NotImplementedError",
        ),
        (
            "PyDefaultArgument",
            "def f(value=[]):\n    return value\n",
            "if value is None:",
        ),
        ("PyUnusedImports", "import os\nvalue = 1\n", "value = 1"),
        (
            "PEP8:E711",
            "def f(value):\n    return value == None\n",
            "value is None",
        ),
        ("PEP8:W291", "value = 1  \n", "value = 1\n"),
        (
            "PyAssertTuple",
            "assert (False, 'reason')\n",
            "assert False, 'reason'",
        ),
        (
            "PySecurityUnsafeDeserialization",
            "import yaml\nvalue = yaml.load(data)\n",
            "yaml.safe_load(data)",
        ),
    ] {
        let source = format!("{source}# retained λ\n");
        let problems = python(&source);
        let problem = finding(&problems, code);
        let metadata = infrastructure::inspection(code).unwrap();
        assert_eq!(
            problem.diagnostic.severity, metadata.default_severity,
            "{code}"
        );
        let fix = problem
            .primary_fix()
            .unwrap_or_else(|| panic!("Expected actionable source fix for {code}: {problem:?}"));
        assert!(!fix.edits.is_empty() && !fix.is_suppression());
        let fixed = rustpy_semantic::edit_helpers::apply_edits(&source, &fix.edits);
        assert!(fixed.contains(expected), "{code}: {fixed}");
        assert!(
            fixed.ends_with("# retained λ\n"),
            "{code}: unrelated source changed"
        );
        assert!(
            rustpy_syntax::parse(&fixed).errors().is_empty(),
            "{code}: fix broke Python syntax"
        );
        assert!(
            !python(&fixed)
                .iter()
                .any(|problem| problem.diagnostic.code == code),
            "{code}: fix did not discharge the original obligation: {fixed}"
        );
    }
}
#[test]
fn suppression_entries_do_not_hide_real_primary_fixes_and_malformed_batches_are_atomic() {
    let source = "value = 1  \n# retained λ\n";
    let mut problem = finding(&python(source), "PEP8:W291").clone();
    let actual = problem.primary_fix().unwrap().clone();
    let suppression = problem
        .fixes
        .iter()
        .find(|fix| fix.is_suppression())
        .unwrap()
        .clone();
    problem.fixes = vec![suppression, actual.clone()];
    let edits = infrastructure::batch_edits(&[problem.clone()], Some("PEP8:W291"), None, false);
    let fixed = rustpy_semantic::edit_helpers::apply_edits(source, &edits);
    assert_eq!(fixed, "value = 1\n# retained λ\n");
    // Reject the whole provider fix rather than depending on unstable edit ordering.
    let range = actual.edits[0].0;
    problem.fixes[1].edits.push((range, "conflict".into()));
    assert!(infrastructure::batch_edits(&[problem], None, None, false).is_empty());
    assert_eq!(source, "value = 1  \n# retained λ\n");
}
#[test]
fn new_requirement_findings_are_addressable_by_profiles_without_guessing_a_version() {
    let source = "my_pkg==1.0\nmy-pkg==2.0\ngit+https://example.invalid/project#egg=old\n";
    let path = Path::new("/inspection-contract/requirements.txt");
    for code in ["PyRequirementConflict", "PyDeprecatedRequirementSpecifier"] {
        let findings = documents::inspect(source, path);
        let problem = finding(&findings, code);
        let metadata = infrastructure::inspection(code).unwrap();
        assert_eq!(problem.diagnostic.severity, metadata.default_severity);
        assert!(
            problem.primary_fix().is_none(),
            "No source edit may invent a dependency version/build intent"
        );
        let inputs = Inputs {
            document_policy: Some(profile(code, Some("Error"), true)),
            ..Default::default()
        };
        assert_eq!(
            finding(&documents::inspect_with_inputs(source, path, &inputs), code)
                .diagnostic
                .severity,
            Severity::Error
        );
        let inputs = Inputs {
            document_policy: Some(profile(code, None, false)),
            ..Default::default()
        };
        assert!(!documents::inspect_with_inputs(source, path, &inputs)
            .iter()
            .any(|problem| problem.diagnostic.code == code));
    }
    let corrected = "my_pkg==2.0\nold @ git+https://example.invalid/project\n";
    let problems = documents::inspect(corrected, path);
    assert!(!problems.iter().any(|problem| matches!(
        problem.diagnostic.code.as_str(),
        "PyRequirementConflict" | "PyDeprecatedRequirementSpecifier"
    )));
}
#[test]
fn template_source_and_resource_findings_distinguish_text_edits_from_project_actions() {
    let directory = tempfile::TempDir::with_prefix("inspection-template-contract").unwrap();
    let inputs = Inputs {
        root: Some(directory.path().into()),
        ..Default::default()
    };
    let source = "{% include 'missing.html' %}\n{% static 'missing.css' %}\n";
    let findings = documents::inspect_with_inputs(
        source,
        &directory.path().join("templates/page.html"),
        &inputs,
    );
    for code in ["UnresolvedTemplate", "UnresolvedStaticFile"] {
        let problem = finding(&findings, code);
        assert_eq!(
            problem.diagnostic.severity,
            infrastructure::inspection(code).unwrap().default_severity
        );
    }
    let create = finding(&findings, "UnresolvedTemplate")
        .primary_fix()
        .unwrap();
    assert!(create.title.starts_with("Create template ") && create.edits.is_empty());
    assert!(
        infrastructure::batch_edits(&findings, Some("UnresolvedTemplate"), None, false).is_empty()
    );
    std::fs::create_dir_all(directory.path().join("templates")).unwrap();
    std::fs::create_dir_all(directory.path().join("static")).unwrap();
    std::fs::write(
        directory.path().join("templates/missing.html"),
        "<p>present</p>",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("static/missing.css"),
        "p { color: red; }",
    )
    .unwrap();
    let corrected = documents::inspect_with_inputs(
        source,
        &directory.path().join("templates/page.html"),
        &inputs,
    );
    assert!(!corrected.iter().any(|problem| matches!(
        problem.diagnostic.code.as_str(),
        "UnresolvedTemplate" | "UnresolvedStaticFile"
    )));
    let broken = documents::inspect("<% value = 1", Path::new("page.mako"));
    let syntax = finding(&broken, "TemplateSyntax");
    assert_eq!(
        syntax.diagnostic.severity,
        infrastructure::inspection("TemplateSyntax")
            .unwrap()
            .default_severity
    );
    assert!(syntax.primary_fix().is_none());
    assert!(
        !documents::inspect("<% value = 1 %>", Path::new("page.mako"))
            .iter()
            .any(|problem| problem.diagnostic.code == "TemplateSyntax")
    );
}
#[test]
fn nonfinite_schema_values_keep_captured_schema_authority_and_do_not_guess_finite_replacements() {
    let uri = "rustpy://schemas/inspection-finite";
    let inputs = Inputs {
        schemas: SchemaSnapshot {
            values: Arc::new(vec![(uri.into(), "{\"type\":\"object\"}".into())]),
            ..Default::default()
        },
        ..Default::default()
    };
    let source = format!("{{\"$schema\":\"{uri}\",\"value\":Infinity}}");
    let findings = documents::inspect_with_inputs(&source, Path::new("config.json5"), &inputs);
    let problem = finding(&findings, "JsonSchemaNonFinite");
    assert_eq!(
        problem.diagnostic.severity,
        infrastructure::inspection("JsonSchemaNonFinite")
            .unwrap()
            .default_severity
    );
    assert!(problem.primary_fix().is_none());
    let corrected = source.replace("Infinity", "42");
    assert!(
        !documents::inspect_with_inputs(&corrected, Path::new("config.json5"), &inputs)
            .iter()
            .any(|problem| problem.diagnostic.code == "JsonSchemaNonFinite")
    );
}
#[test]
fn pydantic_validator_contract_has_real_metadata_profiles_and_dynamic_controls() {
    let source="from pydantic import BaseModel, field_validator\nclass Model(BaseModel):\n    name: str\n    @field_validator('missing')\n    def validate_name(cls, value):\n        return value\n";
    let problems = python(source);
    let problem = finding(&problems, "PyPydanticValidator");
    assert_eq!(
        problem.diagnostic.severity,
        infrastructure::inspection("PyPydanticValidator")
            .unwrap()
            .default_severity
    );
    assert!(
        problem.primary_fix().is_none_or(|fix| fix.is_suppression()),
        "A validator field name is application intent"
    );
    for corrected in [
        source.replace("'missing'", "'name'"),
        source.replace("'missing'", "'missing', check_fields=False"),
    ] {
        assert!(!python(&corrected)
            .iter()
            .any(|problem| problem.diagnostic.code == "PyPydanticValidator"));
    }
    let mut configured = problems.clone();
    profile("PyPydanticValidator", Some("Error"), true)
        .apply(Path::new("/inspection-contract/source.py"), &mut configured);
    assert_eq!(
        finding(&configured, "PyPydanticValidator")
            .diagnostic
            .severity,
        Severity::Error
    );
}
#[test]
fn unsafe_security_rewrites_are_not_fabricated_and_suppression_is_not_a_remediation() {
    let source="import pickle\npassword='actual-production-credential'\npickle.loads(payload)\neval(payload)\n";
    let findings = python(source);
    for code in [
        "PySecurityHardcodedSecret",
        "PySecurityUnsafeDeserialization",
        "PySecurityEval",
    ] {
        let problem = finding(&findings, code);
        assert_eq!(
            problem.diagnostic.severity,
            infrastructure::inspection(code).unwrap().default_severity
        );
        assert!(problem.primary_fix().is_none());
        assert!(problem.fixes.iter().all(|fix| fix.is_suppression()));
    }
    let security = findings
        .into_iter()
        .filter(|problem| problem.diagnostic.code.starts_with("PySecurity"))
        .collect::<Vec<_>>();
    assert!(infrastructure::batch_edits(&security, None, None, false).is_empty());
}

#[test]
fn style_suggestions_without_safe_rewrites_still_ship_behavioral_and_negative_controls() {
    for(code,bad_source,good)in[
        ("PyNestedDefinition","def outer():\n    def inner():\n        return 42\n    return inner\n","def outer(value):\n    def inner():\n        return value\n    return inner\n"),
        ("PyEnumerate","def f(values):\n    for index in range(len(values)):\n        print(values[index])\n","def f(values):\n    for index, value in enumerate(values):\n        print(value)\n"),
        ("PyUnnecessaryLambda","def target(value):\n    return value\nforward = lambda value: target(value)\n","def target(value):\n    return value\nforward = lambda value: target(value + 1)\n"),
        ("PyUnnecessaryPass","def f():\n    pass\n    return 1\n","def f():\n    pass\n"),
        ("PyRedundantIf","def f(flag):\n    if flag:\n        value = 1\n    else:\n        value = 1\n    return value\n","def f(flag):\n    if flag:\n        value = 1\n    else:\n        value = 2\n    return value\n"),
        ("PySimplifyBoolean","def f(flag):\n    if flag:\n        return True\n    else:\n        return False\n","def f(flag):\n    if flag:\n        return 42\n    else:\n        return 0\n"),
        ("PyComparisonWithCallable","def f():\n    return 1\nvalue = f == 1\n","def f():\n    return 1\nvalue = f() == 1\n"),
        ("PyModernTypeHint","from typing import List\nvalue: List[int]\n","value: list[int]\n"),
        ("PyHashDefinition","class Value:\n    def __eq__(self, other):\n        return True\n","class Value:\n    def __eq__(self, other):\n        return True\n    __hash__ = None\n"),
    ]{
        let bad=python(bad_source);let problem=finding(&bad,code);
        assert!(infrastructure::inspection(code).is_some());
        assert!(!python(good).iter().any(|problem|problem.diagnostic.code==code),"{code}: negative control still reported");
        if let Some(fix)=problem.primary_fix(){
            if !fix.edits.is_empty(){
                let fixed=rustpy_semantic::edit_helpers::apply_edits(bad_source,&fix.edits);
                assert!(rustpy_syntax::parse(&fixed).errors().is_empty(),"{code}: invalid fix");
                assert!(!python(&fixed).iter().any(|problem|problem.diagnostic.code==code),"{code}: fix retained its finding");
            }
        }
    }
}

#[test]
fn remaining_core_provider_rules_have_source_identity_and_negative_controls() {
    for(code,bad,good)in[
        ("PyClassCell","value = __class__\n","class Value:\n    def kind(self):\n        return __class__\n"),
        ("PyDeprecatedHasKey","value = {}\nresult = value.has_key('name')\n","value = {}\nresult = 'name' in value\n"),
        ("PyPrintRedirect","print >> destination\n","print('value', file=destination)\n"),
        ("PyRedundantBoolean","def f(value: bool):\n    return value == True\n","def f(value: bool):\n    return value\n"),
        ("PyRedundantUnion","from typing import Union\nvalue: Union[int]\n","value: int\n"),
        ("PyTypeComparison","def f(value):\n    return type(value) == int\n","def f(value):\n    return isinstance(value, int)\n"),
        ("PyInitCall","class Other:\n    pass\nclass Value:\n    def __init__(self):\n        Other.__init__(self)\n","class Other:\n    pass\nclass Value(Other):\n    def __init__(self):\n        Other.__init__(self)\n"),
        ("PyEnum","from enum import Enum\nclass Color(Enum):\n    RED = 1\n    ALSO_RED = 1\n","from enum import Enum\nclass Color(Enum):\n    RED = 1\n    BLUE = 2\n"),
    ]{
        let bad=python(bad);let problem=finding(&bad,code);
        assert!(infrastructure::inspection(&problem.diagnostic.code).is_some());
        assert!(!python(good).iter().any(|problem|problem.diagnostic.code==code),"{code}: corrected control diagnosed");
    }
    let analysis = Analysis::for_project(Path::new("/inspection-contract"));
    let id = analysis.set_file_text(
        Path::new("/inspection-contract/legacy.py"),
        "class Legacy:\n    pass\n",
    );
    let file = analysis.file(id).unwrap();
    let legacy = inspections::inspect(
        &analysis,
        &file,
        &InspectionOptions {
            python_version: (2, 7),
            ..Default::default()
        },
    );
    assert!(
        infrastructure::inspection(&finding(&legacy, "PyOldStyleClass").diagnostic.code).is_some()
    );
    let modern = inspections::inspect(
        &analysis,
        &file,
        &InspectionOptions {
            python_version: (3, 12),
            ..Default::default()
        },
    );
    assert!(!modern
        .iter()
        .any(|problem| problem.diagnostic.code == "PyOldStyleClass"));
}
#[test]
fn django_catalog_diagnostics_require_complete_authority_and_resolve_against_corrections() {
    let source="from django.conf import settings\nfrom django.urls import reverse\nvalue = settings.UNKNOWN\nlink = reverse('missing')\n";
    let analysis = Analysis::for_project(Path::new("/inspection-contract"));
    let id = analysis.set_file_text(Path::new("/inspection-contract/models.py"), source);
    let file = analysis.file(id).unwrap();
    let inputs = Inputs {
        django_complete: true,
        settings_keys: vec!["DEBUG".into()],
        url_names: vec!["home".into()],
        pending_migrations: vec!["catalog".into()],
        ..Default::default()
    };
    let findings = inspections::inspect(
        &analysis,
        &file,
        &InspectionOptions {
            framework: Arc::new(inputs.clone()),
            ..Default::default()
        },
    );
    for code in ["PyDjangoMigration", "PyDjangoSetting", "PyDjangoUrl"] {
        let problem = finding(&findings, code);
        assert_eq!(
            problem.diagnostic.severity,
            infrastructure::inspection(code).unwrap().default_severity
        );
        assert!(
            problem.primary_fix().is_none(),
            "No migration, setting value or route is invented"
        );
    }
    let inputs = Inputs {
        django_complete: false,
        ..inputs
    };
    let unknown = inspections::inspect(
        &analysis,
        &file,
        &InspectionOptions {
            framework: Arc::new(inputs),
            ..Default::default()
        },
    );
    assert!(!unknown.iter().any(|problem| matches!(
        problem.diagnostic.code.as_str(),
        "PyDjangoMigration" | "PyDjangoSetting" | "PyDjangoUrl"
    )));
    let id = analysis.set_file_text(
        Path::new("/inspection-contract/models.py"),
        &source
            .replace("UNKNOWN", "DEBUG")
            .replace("'missing'", "'home'"),
    );
    let inputs = Inputs {
        django_complete: true,
        settings_keys: vec!["DEBUG".into()],
        url_names: vec!["home".into()],
        ..Default::default()
    };
    let corrected = inspections::inspect(
        &analysis,
        &analysis.file(id).unwrap(),
        &InspectionOptions {
            framework: Arc::new(inputs),
            ..Default::default()
        },
    );
    assert!(!corrected.iter().any(|problem| matches!(
        problem.diagnostic.code.as_str(),
        "PyDjangoMigration" | "PyDjangoSetting" | "PyDjangoUrl"
    )));
    let toml = documents::inspect("value = [", Path::new("settings.toml"));
    let problem = finding(&toml, "TomlSchema");
    assert_eq!(
        problem.diagnostic.severity,
        infrastructure::inspection("TomlSchema")
            .unwrap()
            .default_severity
    );
    assert!(problem.primary_fix().is_none());
    assert!(
        !documents::inspect("value = []\n", Path::new("settings.toml"))
            .iter()
            .any(|problem| problem.diagnostic.code == "TomlSchema")
    );
}
