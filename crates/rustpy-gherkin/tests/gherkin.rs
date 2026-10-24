//! Integration tests over fixture projects. The behave fixtures were run with
//! behave 1.3.3 (and 1.2.6); the expected scenario names, locations and step
//! definition locations below are taken from behave's `json.pretty` report
//! for these exact files. The pytest-bdd fixture passes under pytest-bdd 9.

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rustpy_gherkin::*;

fn fixture(p: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(p);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Scenario names and lines as behave reports them (outline rows expanded).
fn reported_scenarios(f: &Feature) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for s in &f.scenarios {
        let rows = outline_steps_expanded(s);
        if rows.is_empty() {
            out.push((s.name.clone(), s.line));
        } else {
            out.extend(rows.into_iter().map(|r| (r.name, r.line)));
        }
    }
    out
}

#[test]
fn calculator_outline_matches_the_behave_report() {
    let f = parse(&fixture("behave/calculator.feature"));
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    assert_eq!(
        (f.keyword.as_str(), f.name.as_str(), f.line),
        ("Feature", "Calculator", 2)
    );
    assert_eq!(f.tags, vec!["@calc"]);
    assert_eq!(f.description, "As a user I want to add numbers.");
    assert_eq!(f.background.as_ref().map(|b| b.line), Some(5));
    let expected: Vec<(String, usize)> = [
        ("Add two numbers", 8),
        ("Failing addition", 12),
        ("Exploding step", 16),
        ("Skipped scenario", 21),
        ("Undefined step", 25),
        ("Add 1 and 2 -- @1.1 Small numbers", 35),
        ("Add 2 and 2 -- @1.2 Small numbers", 36),
        ("Data table and doc string", 38),
    ]
    .iter()
    .map(|(n, l)| (n.to_string(), *l))
    .collect();
    assert_eq!(reported_scenarios(&f), expected);
    let skipped = &f.scenarios[3];
    assert_eq!(skipped.tags, vec!["@skip"]);
    let data = &f.scenarios[6];
    assert_eq!(
        data.steps[0]
            .table
            .iter()
            .map(|r| r.cells.join(","))
            .collect::<Vec<_>>(),
        vec!["value", "1", "2"]
    );
    assert_eq!(
        data.steps[3]
            .doc_string
            .as_ref()
            .map(|d| d.content.as_str()),
        Some("total: 3")
    );
    assert_eq!(data.steps[3].kind, StepKind::Then);

    let rules = parse(&fixture("behave/rules.feature"));
    assert_eq!(rules.rules.len(), 1);
    assert_eq!(rules.rules[0].name, "Positive numbers");
    assert_eq!(
        reported_scenarios(&rules),
        vec![("Positive".to_string(), 3)]
    );
    assert_eq!(rules.scenarios[0].rule, Some(0));
}

#[test]
fn step_definitions_match_like_behave() {
    let defs = scan_step_definitions(
        &fixture("behave/steps/calc_steps.py"),
        Path::new("features/steps/calc_steps.py"),
    );
    assert_eq!(defs.len(), 7);
    let f = parse(&fixture("behave/calculator.feature"));
    // `match.location` of each step in behave's report.
    let expected = [
        ("a fresh calculator", 4),
        ("I add 2 and 3", 9),
        ("I divide 1 by 0", 15),
        ("the result is 5", 20),
        ("the following numbers:", 25),
        ("I sum them", 33),
        ("the report says:", 38),
    ];
    for (text, line) in expected {
        let step = f
            .all_steps()
            .into_iter()
            .find(|s| s.text == text)
            .unwrap_or_else(|| panic!("no step {text}"));
        let found: Vec<usize> = find_step_definitions(step, &defs)
            .iter()
            .map(|d| d.line)
            .collect();
        assert_eq!(found, vec![line], "{text}");
    }
    let add = defs.iter().find(|d| d.line == 9).unwrap();
    assert_eq!(
        add.arguments("I add 2 and 3")
            .unwrap()
            .iter()
            .map(|a| (a.name.clone().unwrap(), a.value.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("a".to_string(), "2".to_string()),
            ("b".to_string(), "3".to_string())
        ]
    );
    assert_eq!(
        defs.iter()
            .filter(|d| d.matcher == Matcher::Re)
            .map(|d| d.line)
            .collect::<Vec<_>>(),
        vec![33, 38]
    );

    // behave reports exactly one undefined step for this file.
    let unresolved = unresolved_steps(&f, &defs);
    assert_eq!(
        unresolved
            .iter()
            .map(|s| (s.text.as_str(), s.line))
            .collect::<Vec<_>>(),
        vec![("I multiply 2 by 3", 26)]
    );
    assert_eq!(step_definition_stub(&unresolved[0], Matcher::Parse), "@when('I multiply {number:d} by {number2:d}')\ndef step_impl(context, number, number2):\n    raise NotImplementedError\n");

    // Outline steps resolve through every Examples row.
    let outline = &f.scenarios[5];
    assert_eq!(
        definitions_for_step(outline, &outline.steps[1], &defs)
            .iter()
            .map(|d| d.line)
            .collect::<Vec<_>>(),
        vec![20]
    );
}

#[test]
fn localized_features() {
    let defs = scan_step_definitions(
        &fixture("i18n/steps/i18n_steps.py"),
        Path::new("features/steps/i18n_steps.py"),
    );

    let de = parse(&fixture("i18n/rechner.feature"));
    assert!(de.diagnostics.is_empty(), "{:?}", de.diagnostics);
    assert_eq!(
        (de.language.as_str(), de.keyword.as_str(), de.name.as_str()),
        ("de", "Funktionalität", "Rechner")
    );
    assert_eq!(
        de.description,
        "Um Fehler zu vermeiden\nmöchte ich addieren."
    );
    assert_eq!(
        de.background.as_ref().map(|b| b.keyword.as_str()),
        Some("Grundlage")
    );
    let kinds: Vec<(&str, StepKind)> = de.scenarios[0]
        .steps
        .iter()
        .map(|s| (s.keyword.as_str(), s.kind))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("Wenn", StepKind::When),
            ("Dann", StepKind::Then),
            ("Und", StepKind::Then)
        ]
    );
    assert_eq!(
        reported_scenarios(&de),
        vec![
            ("Zwei Zahlen addieren".to_string(), 10),
            ("Addieren von 1 und 2 -- @1.1 ".to_string(), 21),
            ("Addieren von 4 und 5 -- @1.2 ".to_string(), 22)
        ]
    );
    assert!(unresolved_steps(&de, &defs).is_empty());

    let fr = parse(&fixture("i18n/calcul.feature"));
    assert!(fr.diagnostics.is_empty(), "{:?}", fr.diagnostics);
    assert_eq!(
        (fr.language.as_str(), fr.keyword.as_str()),
        ("fr", "Fonctionnalité")
    );
    let steps: Vec<(&str, &str, StepKind)> = fr
        .scenarios
        .iter()
        .flat_map(|s| s.steps.iter())
        .map(|s| (s.keyword.as_str(), s.text.as_str(), s.kind))
        .collect();
    assert_eq!(
        steps,
        vec![
            ("Quand", "j'additionne 2 et 3", StepKind::When),
            ("Alors", "le résultat est 5", StepKind::Then),
            ("Et qu'", "il n'y a pas d'erreur", StepKind::Then),
            ("Lorsqu'", "on additionne <a> et <b>", StepKind::When),
            ("Alors", "le résultat est <somme>", StepKind::Then),
        ]
    );
    assert_eq!(
        fr.scenarios[0].steps[2].display(),
        "Et qu'il n'y a pas d'erreur"
    );
    assert_eq!(
        reported_scenarios(&fr)[1],
        ("Addition de 1 et 1 -- @1.1 ".to_string(), 17)
    );
    assert!(unresolved_steps(&fr, &defs).is_empty());

    let src = fixture("i18n/calcul.feature");
    let spans: Vec<(&str, &str)> = highlight(&src)
        .into_iter()
        .map(|(r, t)| (&src[r], t))
        .collect();
    for kw in [
        "Fonctionnalité:",
        "Contexte:",
        "Scénario:",
        "Plan du scénario:",
        "Exemples:",
        "Soit",
        "Quand",
        "Et qu'",
        "Lorsqu'",
    ] {
        assert!(spans.contains(&(kw, KEYWORD)), "{kw}: {spans:?}");
    }
    assert!(spans.contains(&("<somme>", VARIABLE)));
    assert!(spans.contains(&("somme", KEY)));
}

#[test]
fn pytest_bdd_project() {
    let defs = scan_step_definitions(
        &fixture("pytest_bdd/test_cucumbers.py"),
        Path::new("tests/test_cucumbers.py"),
    );
    assert!(defs.iter().all(|d| d.framework == BddFramework::PytestBdd));
    assert_eq!(
        defs.iter().map(|d| (d.kind, d.matcher)).collect::<Vec<_>>(),
        vec![
            (StepKind::Given, Matcher::String),
            (StepKind::Given, Matcher::Parse),
            (StepKind::When, Matcher::Parse),
            (StepKind::Then, Matcher::Re),
            (StepKind::Then, Matcher::CFParse)
        ]
    );
    let f = parse(&fixture("pytest_bdd/cucumbers.feature"));
    assert!(unresolved_steps(&f, &defs).is_empty());
    assert!(match_step("I have a basket!", &defs).is_empty());
    assert_eq!(match_step("there are 12 cucumbers", &defs).len(), 1);
    let stub = pytest_bdd_step_stub(&f.scenarios[1].steps[0], Matcher::Parse);
    assert_eq!(stub, "@given(parsers.parse('there are {start} cucumbers'))\ndef _(start):\n    raise NotImplementedError\n");
}

#[test]
fn highlighting_the_behave_fixture() {
    let src = fixture("behave/calculator.feature");
    let spans = highlight(&src);
    let tagged: Vec<(&str, &str)> = spans.iter().map(|(r, t)| (&src[r.clone()], *t)).collect();
    assert_eq!(tagged[0], ("@calc", TAG));
    assert_eq!(tagged[1], ("Feature:", KEYWORD));
    for expected in [
        ("Background:", KEYWORD),
        ("Scenario Outline:", KEYWORD),
        ("<a>", VARIABLE),
        ("Examples:", KEYWORD),
        ("sum", KEY),
        ("\"\"\"", STRING),
        ("total: 3", STRING),
        ("value", KEY),
        ("@skip", TAG),
    ] {
        assert!(tagged.contains(&expected), "{expected:?}");
    }
    let mut last = 0;
    for (r, _) in &spans {
        assert!(r.start >= last && r.start < r.end);
        last = r.end;
    }
}
