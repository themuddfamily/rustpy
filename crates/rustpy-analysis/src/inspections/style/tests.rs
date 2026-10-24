use super::*;
use std::path::Path;
fn inspect(source: &str, mut configure: impl FnMut(&mut InspectionOptions)) -> Vec<Problem> {
    let db = Analysis::for_project(Path::new("/project"));
    let id = db.set_file_text(Path::new("/project/main.py"), source);
    let file = db.file(id).unwrap();
    let mut options = InspectionOptions {
        pep8: false,
        unused_imports: false,
        unused_variables: false,
        type_checker: false,
        security: false,
        compatibility: false,
        code_smells: false,
        probable_bugs: false,
        ..Default::default()
    };
    configure(&mut options);
    super::super::inspect(&db, &file, &options)
}
#[test]
fn naming_ignores_actual_overrides_but_not_unrelated_methods() {
    let source="class Parent:\n    def inheritedName(self):\n        pass\nclass Child(Parent):\n    def inheritedName(self):\n        pass\n    def unrelatedName(self, BadArgument):\n        LocalValue = 1\n        return LocalValue\n";
    let problems = inspect(source, |_| {});
    assert!(problems.iter().any(|p| p.diagnostic.code == "PyPep8Naming"
        && p.diagnostic.range.start() == ts(source.find("unrelatedName").unwrap())));
    assert!(!problems.iter().any(|p| p.diagnostic.code == "PyPep8Naming"
        && p.diagnostic.range.start() == ts(source.rfind("inheritedName").unwrap())));
    let ignored = inspect(source, |options| {
        options.style.naming_ignore_patterns = vec!["*Name".into(), "Bad*".into(), "Local*".into()]
    });
    assert!(!ignored.iter().any(|p| p.diagnostic.code == "PyPep8Naming"));
}
#[test]
fn module_constants_and_private_placeholders_are_configurable() {
    let source = "retry_count = 3\nRETRY_COUNT = 3\n_ = 1\n";
    assert!(
        inspect(source, |options| options.style.check_constants = true)
            .iter()
            .any(|p| p.diagnostic.message.contains("UPPER_CASE"))
    );
    assert!(
        !inspect(source, |options| options.style.check_constants = false)
            .iter()
            .any(|p| p.diagnostic.code == "PyPep8Naming")
    );
}
#[test]
fn module_level_literal_assignments_are_not_naming_problems_by_default() {
    // Like pep8-naming: a generated module of `value_NNNN = N`
    // lines has no PEP 8 naming problems unless the opt-in rule is enabled.
    let source: String = (0..50).map(|i| format!("value_{i:04} = {i}\n")).collect();
    assert!(!inspect(&source, |_| {})
        .iter()
        .any(|p| p.diagnostic.code == "PyPep8Naming"));
}
#[test]
fn missing_empty_docstrings_respect_kind_options_and_header() {
    let source="#!/usr/bin/env python3\n# coding: utf-8\nclass Visible:\n    def method(self):\n        \"\"\" \"\"\"\n        pass\ndef function():\n    pass\n";
    let problems = inspect(source, |options| {
        options.missing_docstrings = true;
        options.style.doc_classes = false;
        options.style.doc_functions = false;
    });
    let docs: Vec<_> = problems
        .iter()
        .filter(|p| p.diagnostic.code == "PyMissingOrEmptyDocstring")
        .collect();
    assert_eq!(docs.len(), 2);
    let module = docs.iter().find(|p| p.diagnostic.range.is_empty()).unwrap();
    let fixed = rustpy_semantic::edit_helpers::apply_edits(source, &module.fixes[0].edits);
    assert!(fixed.starts_with("#!/usr/bin/env python3\n# coding: utf-8\n\"\"\""));
}
#[test]
fn docstring_sections_types_and_quotes_have_real_diagnostics() {
    let source="def f(value: int) -> str:\n    '''Summary.\n\n    :param value: input\n    :type value: str\n    :return: result\n    :rtype: int\n    Arg:\n        value: input\n    '''\n    return str(value)\n";
    let problems = inspect(source, |_| {});
    assert_eq!(
        problems
            .iter()
            .filter(|p| p.diagnostic.code == "PyDocstringType")
            .count(),
        2
    );
    assert!(
        problems
            .iter()
            .filter(|p| p.diagnostic.code == "PyDocstringStyle")
            .count()
            >= 2
    );
    let primitive_alias="Alias = int\ndef f(value: Alias):\n    \"\"\"Summary.\n\n    :param value: input\n    :type value: int\n    \"\"\"\n    return value\n";
    assert!(!inspect(primitive_alias, |_| {})
        .iter()
        .any(|p| p.diagnostic.code == "PyDocstringType"));
}
#[test]
fn source_conventions_and_optional_metrics_are_actual_consumers() {
    let source="def f(first, second):\n    total = 0\n    if first:\n        total = second\n        return total\n    return 0\n# café\n";
    let problems = inspect(source, |options| {
        let s = &mut options.style;
        s.mandatory_encoding = true;
        s.missing_shebang = true;
        s.non_ascii_comments = true;
        s.max_arguments = 1;
        s.max_returns = 1;
        s.max_statements = 2;
    });
    assert_eq!(
        problems
            .iter()
            .filter(|p| p.diagnostic.code == "PyFunctionMetrics")
            .count(),
        3
    );
    assert!(problems.iter().any(|p| p.diagnostic.code == "PyNonAscii"));
    assert!(problems
        .iter()
        .any(|p| p.diagnostic.message.contains("encoding")));
    assert!(problems
        .iter()
        .any(|p| p.diagnostic.message.contains("shebang")));
    let nested = "def outer(one):\n    def inner(a,b,c):\n        return a\n    return one\n";
    let problems = inspect(nested, |options| options.style.max_arguments = 1);
    assert_eq!(
        problems
            .iter()
            .filter(|p| p.diagnostic.code == "PyFunctionMetrics")
            .count(),
        1
    );
}
#[test]
fn duplicate_fragments_are_project_wide_and_use_unsaved_sources() {
    let db = Analysis::for_project(Path::new("/project"));
    let body="def f(value):\n    first = value + 1\n    second = first * 2\n    third = second - 3\n    fourth = third + 4\n    fifth = fourth * 5\n    return fifth\n";
    let a = db.set_file_text(Path::new("/project/a.py"), body);
    db.set_file_text(Path::new("/project/b.py"), body);
    let mut options = InspectionOptions::default();
    options.style.duplicate_code = true;
    options.style.duplicate_min_lines = 4;
    options.style.duplicate_min_tokens = 12;
    let file = db.file(a).unwrap();
    let problems = super::super::inspect(&db, &file, &options);
    assert!(problems
        .iter()
        .any(|p| p.diagnostic.code == "PyDuplicatedCode"));
    db.set_file_text(
        Path::new("/project/b.py"),
        "def different():\n    return 'changed unsaved'\n",
    );
    let problems = super::super::inspect(&db, &file, &options);
    assert!(!problems
        .iter()
        .any(|p| p.diagnostic.code == "PyDuplicatedCode"));
}
#[test]
fn style_rewrites_keep_typed_and_shadowed_custom_objects_out() {
    let source="def f(mapping: dict[str, int]):\n    for key in mapping.keys():\n        pass\n    return mapping.get('name', None)\n";
    let problems = inspect(source, |_| {});
    assert!(problems
        .iter()
        .any(|p| p.diagnostic.code == "PyDictKeysIteration"));
    assert!(problems
        .iter()
        .any(|p| p.diagnostic.code == "PyRedundantDictDefault"));
    let source="class Custom:\n    def get(self,key,default):\n        return default\ndef f(value: Custom):\n    return value.get('key', None)\n";
    assert!(!inspect(source, |_| {})
        .iter()
        .any(|p| p.diagnostic.code == "PyRedundantDictDefault"));
    let source = "def f(value):\n    if value:\n        return 1\n    else:\n        return 2\n";
    let problem = inspect(source, |_| {})
        .into_iter()
        .find(|p| p.diagnostic.code == "PyNoElseReturn")
        .unwrap();
    let result = rustpy_semantic::edit_helpers::apply_edits(source, &problem.fixes[0].edits);
    assert!(rustpy_syntax::parse(&result).errors().is_empty());
    assert!(!result.contains("else:"));
    assert!(result.contains("    return 2"));
}
#[test]
fn profile_options_override_defaults_and_use_central_registry() {
    let mut profiles = rustpy_settings::inspection_profiles::Profiles::default();
    let profile = &mut profiles.ide[0];
    let rule = profile
        .inspections
        .entry("PyFunctionMetrics".into())
        .or_default();
    rule.options.insert("max_arguments".into(), "1".into());
    let problems = inspect("def f(a,b):\n    return a\n", |options| {
        options.policy = Some(
            infrastructure::Policy::from_settings(&profiles, Some(Path::new("/project")), false)
                .unwrap(),
        )
    });
    assert!(problems
        .iter()
        .any(|p| p.diagnostic.code == "PyFunctionMetrics"));
    for id in [
        "PEP8:E101",
        "PEP8:E211",
        "PEP8:E722",
        "PEP8:W503",
        "PyDocstringType",
        "PyFunctionMetrics",
        "PyDuplicatedCode",
    ] {
        assert!(infrastructure::inspection(id).is_some(), "{id}")
    }
}
#[test]
fn source_quote_and_comma_fixes_preserve_valid_syntax() {
    let source = "value = \"text\"\nitems = [\n    1\n]\n";
    let problems = inspect(source, |options| {
        options.style.quote_style = "single".into();
        options.style.trailing_comma = true
    });
    let quote = problems
        .iter()
        .find(|p| p.diagnostic.message == "Inconsistent string quotes")
        .unwrap();
    assert!(
        rustpy_semantic::edit_helpers::apply_edits(source, &quote.fixes[0].edits)
            .contains("'text'")
    );
    assert!(problems
        .iter()
        .any(|p| p.diagnostic.message.contains("trailing comma")));
}

#[test]
fn deprecated_raise_does_not_match_commas_inside_a_valid_exception_call() {
    assert!(!inspect(
        "def f():\n    raise ValueError('first', 'second')\n",
        |_| {}
    )
    .iter()
    .any(|problem| problem.diagnostic.code == "PyDeprecatedRaise"));
}
#[test]
fn inherited_member_exemption_does_not_hide_argument_or_variable_checks_elsewhere() {
    let source="class Base:\n    def inheritedName(self):\n        pass\nclass Child(Base):\n    def unrelatedName(self, BadArgument):\n        BadLocal = 1\n        return BadLocal\n";
    let problems = inspect(source, |options| options.style.ignore_overrides = true);
    assert!(problems
        .iter()
        .any(|problem| problem.diagnostic.message.contains("Argument name")));
    assert!(problems
        .iter()
        .any(|problem| problem.diagnostic.message.contains("Variable in function")));
}
#[test]
fn explicitly_selected_full_backend_without_interpreter_is_visible() {
    let problems = inspect("value = 1\n", |options| {
        options.pep8 = true;
        options.style.pep8_backend = "external".into();
        options.style.interpreter = None;
    });
    assert!(problems
        .iter()
        .any(|problem| problem.diagnostic.code == "PyStyleBackend"));
}
#[test]
fn nominal_docstring_types_keep_alias_identity_and_report_real_disagreement() {
    let source="class First:\n    pass\nclass Second:\n    pass\nAlias = First\ndef f(value: First):\n    \"\"\"Summary.\n\n    :param value: input\n    :type value: Alias\n    \"\"\"\n    return value\ndef g(value: First):\n    \"\"\"Summary.\n\n    :param value: input\n    :type value: Second\n    \"\"\"\n    return value\n";
    let problems = inspect(source, |_| {});
    let mismatches = problems
        .iter()
        .filter(|problem| problem.diagnostic.code == "PyDocstringType")
        .collect::<Vec<_>>();
    assert_eq!(mismatches.len(), 1);
    assert!(mismatches[0].diagnostic.message.contains("Second"));
}

#[test]
fn documentation_type_markup_keeps_semantic_aliases_and_real_disagreements() {
    let source = "class Value:\n    pass\nAlias = Value\ndef good(value: Value) -> str:\n    \"\"\"Good.\n\n    :type value: :py:class:`Alias`\n    :rtype: ``str``\n    \"\"\"\n    return str(value)\ndef bad(value: Value) -> str:\n    \"\"\"Bad.\n\n    :type value: :class:`str`\n    :rtype: :class:`bytes`\n    \"\"\"\n    return str(value)\n";
    let problems = inspect(source, |_| {});
    let types: Vec<_> = problems
        .iter()
        .filter(|problem| problem.diagnostic.code == "PyDocstringType")
        .collect();
    assert_eq!(types.len(), 2);
    let bad_start = source.find("def bad").unwrap();
    assert!(types
        .iter()
        .all(|problem| usize::from(problem.diagnostic.range.start()) > bad_start));
}
#[test]
fn comprehension_and_aggregate_checks_reuse_existing_guarded_transformations() {
    let source="def f(items):\n    output = []\n    for value in items:\n        output.append(value + 1)\n    return output\n";
    assert!(inspect(source, |_| {})
        .iter()
        .any(|problem| problem.diagnostic.code == "PyLoopToComprehension"));
    let source="def f(items):\n    total = 0\n    for value in items:\n        total += value\n    return total\n";
    assert!(inspect(source, |_| {})
        .iter()
        .any(|problem| problem.diagnostic.code == "PyLoopToComprehension"));
}

#[test]
fn constructed_values_and_comprehension_targets_are_not_type_aliases() {
    let source = "class Widget:\n    pass\ndef f(items):\n    Alias = Widget\n    BadInstance = Widget()\n    return [BadElement for BadElement in items]\n";
    let problems = inspect(source, |_| {});
    let names = problems
        .iter()
        .filter(|problem| problem.diagnostic.code == "PyPep8Naming")
        .map(|problem| {
            &source[usize::from(problem.diagnostic.range.start())
                ..usize::from(problem.diagnostic.range.end())]
        })
        .collect::<Vec<_>>();
    assert!(names.contains(&"BadInstance"));
    assert!(names.contains(&"BadElement"));
    assert!(!names.contains(&"Alias"));
}
