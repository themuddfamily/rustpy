//! Unresolved step detection and step definition stubs ("Create step definition").

use crate::highlight::is_number;
use crate::matching::find_step_definitions;
use crate::model::{Feature, Scenario, Step, StepKind};
use crate::outline::outline_steps_expanded;
use crate::stepdefs::{Matcher, StepDef};

/// Steps of `feature` that no definition in `defs` matches (step types are
/// respected: a `When` step needs a `@when` or `@step` definition). Outline
/// steps are checked once per Examples row with the placeholders substituted
/// and reported (unsubstituted) when any row fails; background steps are
/// reported once. Results are in document order.
pub fn unresolved_steps(feature: &Feature, defs: &[StepDef]) -> Vec<Step> {
    let resolves = |s: &Step| !find_step_definitions(s, defs).is_empty();
    let mut out: Vec<Step> = Vec::new();
    let backgrounds = feature
        .background
        .iter()
        .chain(feature.rules.iter().filter_map(|r| r.background.as_ref()));
    for b in backgrounds {
        out.extend(b.steps.iter().filter(|s| !resolves(s)).cloned());
    }
    for sc in &feature.scenarios {
        let expanded = outline_steps_expanded(sc);
        for (i, step) in sc.steps.iter().enumerate() {
            let ok = if expanded.is_empty() {
                resolves(step)
            } else {
                expanded
                    .iter()
                    .all(|e| e.steps.get(i).is_some_and(resolves))
            };
            if !ok {
                out.push(step.clone());
            }
        }
    }
    out.sort_by_key(|s| s.line);
    out
}

/// A piece of step text for stub generation.
#[derive(Debug, PartialEq)]
enum Seg {
    Lit(String),
    Placeholder(String),
    Int(String),
    Float(String),
    Quoted(String),
    QuotedPlaceholder(String),
}

fn placeholder_at(text: &str, i: usize) -> Option<(String, usize)> {
    let rest = text.get(i..)?;
    let inner = rest.strip_prefix('<')?;
    let close = inner.find(['<', '>'])?;
    if !inner[close..].starts_with('>') || close == 0 {
        return None;
    }
    Some((inner[..close].to_string(), i + close + 2))
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn segments(text: &str, placeholders: bool) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    let push_lit = |lit: &mut String, out: &mut Vec<Seg>| {
        if !lit.is_empty() {
            out.push(Seg::Lit(std::mem::take(lit)));
        }
    };
    while i < text.len() {
        let rest = &text[i..];
        let c = rest.chars().next().unwrap_or(' ');
        if placeholders {
            if let Some((name, end)) = placeholder_at(text, i) {
                push_lit(&mut lit, &mut out);
                out.push(Seg::Placeholder(name));
                i = end;
                continue;
            }
        }
        if c == '"' {
            if let Some(close) = rest[1..].find('"') {
                let content = &rest[1..1 + close];
                push_lit(&mut lit, &mut out);
                match placeholder_at(content, 0)
                    .filter(|(_, end)| *end == content.len() && placeholders)
                {
                    Some((name, _)) => out.push(Seg::QuotedPlaceholder(name)),
                    None => out.push(Seg::Quoted(content.to_string())),
                }
                i += close + 2;
                continue;
            }
        }
        let prev = text[..i].chars().next_back();
        let boundary = prev.is_none_or(|p| !is_word_char(p) && p != '.');
        let digit_start =
            c.is_ascii_digit() || (c == '-' && rest[1..].starts_with(|d: char| d.is_ascii_digit()));
        if boundary && digit_start {
            let len = rest
                .char_indices()
                .skip(1)
                .find(|(_, ch)| !(ch.is_ascii_digit() || *ch == '.'))
                .map(|(p, _)| p)
                .unwrap_or(rest.len());
            let mut num = rest[..len].trim_end_matches('.').to_string();
            let next = rest[num.len()..].chars().next();
            if is_number(&num) && next.is_none_or(|n| !is_word_char(n)) {
                push_lit(&mut lit, &mut out);
                i += num.len();
                if num.contains('.') {
                    out.push(Seg::Float(std::mem::take(&mut num)));
                } else {
                    out.push(Seg::Int(std::mem::take(&mut num)));
                }
                continue;
            }
        }
        if is_word_char(c) {
            // Copy whole words so digits inside them stay literal (`item42`).
            let len = rest
                .char_indices()
                .find(|(_, ch)| !is_word_char(*ch))
                .map(|(p, _)| p)
                .unwrap_or(rest.len());
            lit.push_str(&rest[..len]);
            i += len;
            continue;
        }
        lit.push(c);
        i += c.len_utf8();
    }
    push_lit(&mut lit, &mut out);
    out
}

const PY_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield", "context",
];

fn identifier(name: &str) -> String {
    let mut s = String::new();
    for c in name.trim().chars() {
        if c.is_alphanumeric() || c == '_' {
            s.push(c);
        } else if !s.ends_with('_') {
            s.push('_');
        }
    }
    let mut s = s.trim_matches('_').to_string();
    if s.is_empty() {
        s = "param".into();
    }
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, 'p');
    }
    if PY_KEYWORDS.contains(&s.as_str()) {
        s.push('_');
    }
    s
}

fn unique(base: &str, params: &[String]) -> String {
    if !params.iter().any(|p| p == base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}{n}"))
        .find(|c| !params.iter().any(|p| p == c))
        .unwrap_or_else(|| base.to_string())
}

/// Python's `re.escape` for the characters that matter.
fn re_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.^$*+?{}[]|()".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Builds a step pattern for `text` with parameters for numbers, `"quoted"`
/// strings and `<placeholders>`. `types` gives the parse type (`'d'` / `'f'`)
/// of a placeholder when known. Returns the pattern and parameter names.
fn build_pattern(
    text: &str,
    matcher: Matcher,
    placeholders: bool,
    types: &dyn Fn(&str) -> Option<char>,
) -> (String, Vec<String>) {
    let segs = segments(text, placeholders);
    if matcher == Matcher::String {
        return (text.to_string(), Vec::new());
    }
    let re = matcher == Matcher::Re;
    let mut params: Vec<String> = Vec::new();
    // Placeholder name → parameter name (a repeated placeholder reuses its parameter).
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut pattern = String::new();
    let typed = |name: &str, ty: Option<char>, re: bool| -> String {
        match (re, ty) {
            (false, Some(t)) => format!("{{{name}:{t}}}"),
            (false, None) => format!("{{{name}}}"),
            (true, Some('d')) => format!(r"(?P<{name}>-?\d+)"),
            (true, Some(_)) => format!(r"(?P<{name}>-?\d*\.\d+)"),
            (true, None) => format!("(?P<{name}>.+)"),
        }
    };
    for seg in &segs {
        match seg {
            Seg::Lit(l) => {
                if re {
                    pattern.push_str(&re_escape(l));
                } else {
                    pattern.push_str(&l.replace('{', "{{").replace('}', "}}"));
                }
            }
            Seg::Placeholder(raw) | Seg::QuotedPlaceholder(raw) => {
                let quoted = matches!(seg, Seg::QuotedPlaceholder(_));
                let (name, repeated) = match seen.iter().find(|(r, _)| r == raw) {
                    Some((_, n)) => (n.clone(), true),
                    None => {
                        let n = unique(&identifier(raw), &params);
                        params.push(n.clone());
                        seen.push((raw.clone(), n.clone()));
                        (n, false)
                    }
                };
                let ty = types(raw);
                let field = if re && repeated {
                    // Python regexes cannot redefine a group; refer back to it.
                    format!("(?P={name})")
                } else if quoted && re && ty.is_none() {
                    format!("(?P<{name}>[^\"]*)")
                } else {
                    typed(&name, ty, re)
                };
                if quoted {
                    pattern.push('"');
                    pattern.push_str(&field);
                    pattern.push('"');
                } else {
                    pattern.push_str(&field);
                }
            }
            Seg::Int(_) | Seg::Float(_) => {
                let name = unique("number", &params);
                params.push(name.clone());
                let ty = if matches!(seg, Seg::Int(_)) { 'd' } else { 'f' };
                pattern.push_str(&typed(&name, Some(ty), re));
            }
            Seg::Quoted(_) => {
                let name = unique("text", &params);
                params.push(name.clone());
                if re {
                    pattern.push_str(&format!("\"(?P<{name}>[^\"]*)\""));
                } else {
                    pattern.push_str(&format!("\"{{{name}}}\""));
                }
            }
        }
    }
    (pattern, params)
}

/// A Python string literal for `s` (raw when `raw`, if possible).
fn py_literal(s: &str, raw: bool) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let raw_ok = raw && !s.contains(quote) && !s.contains('\n') && !s.ends_with('\\');
    if raw_ok {
        return format!("r{quote}{s}{quote}");
    }
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn decorator_kind(step: &Step) -> StepKind {
    step.kind
}

fn behave_stub(
    step: &Step,
    matcher: Matcher,
    placeholders: bool,
    types: &dyn Fn(&str) -> Option<char>,
) -> String {
    // behave has no exact-text matcher: a parse pattern without fields is exact.
    let matcher = if matcher == Matcher::String {
        Matcher::Parse
    } else {
        matcher
    };
    let (pattern, params) = build_pattern(&step.text, matcher, placeholders, types);
    let mut args = vec!["context".to_string()];
    args.extend(params);
    format!(
        "@{}({})\ndef step_impl({}):\n    raise NotImplementedError\n",
        decorator_kind(step).decorator(),
        py_literal(&pattern, matcher == Matcher::Re),
        args.join(", ")
    )
}

/// A behave step definition stub for `step`, e.g. for `Given I have 5 apples`
/// with [`Matcher::Parse`]:
///
/// ```text
/// @given('I have {number:d} apples')
/// def step_impl(context, number):
///     raise NotImplementedError
/// ```
///
/// Numbers become `{number:d}` / `{number:f}`, `"quoted"` text `"{text}"`
/// and outline `<placeholders>` `{placeholder}` (`(?P<name>...)` groups for
/// [`Matcher::Re`]). [`Matcher::String`] is treated as `Parse`.
pub fn step_definition_stub(step: &Step, matcher: Matcher) -> String {
    behave_stub(step, matcher, true, &|_| None)
}

/// Placeholder types from the Examples of `scenario`: `'d'` when every value
/// of the column is an integer, `'f'` when all are decimals.
fn column_types(scenario: &Scenario) -> impl Fn(&str) -> Option<char> + '_ {
    move |name: &str| {
        let mut values = scenario.examples.iter().flat_map(|ex| {
            let col = ex
                .header
                .as_ref()
                .and_then(|h| h.cells.iter().position(|c| c == name));
            ex.rows
                .iter()
                .filter_map(move |r| col.and_then(|c| r.cells.get(c)))
        });
        let first = values.next()?.as_str();
        let int = |v: &str| is_number(v) && !v.contains('.');
        let float = |v: &str| is_number(v) && v.contains('.');
        if int(first) && values.clone().all(|v| int(v.as_str())) {
            Some('d')
        } else if float(first) && values.all(|v| float(v.as_str())) {
            Some('f')
        } else {
            None
        }
    }
}

/// Like [`step_definition_stub`] for a step of `scenario`: placeholders are
/// only parameters in outlines, and their types come from the Examples
/// (`<count>` with values 5 and 12 becomes `{count:d}`).
pub fn step_definition_stub_for(scenario: &Scenario, step: &Step, matcher: Matcher) -> String {
    behave_stub(
        step,
        matcher,
        scenario.is_outline(),
        &column_types(scenario),
    )
}

/// A pytest-bdd step definition stub:
///
/// ```text
/// @given(parsers.parse('I have {number:d} apples'))
/// def _(number):
///     raise NotImplementedError
/// ```
///
/// [`Matcher::String`] produces an exact-text step (`@given('...')`) unless
/// the step has `<placeholders>` (then `parsers.parse` is used, since
/// pytest-bdd substitutes Examples values before matching).
pub fn pytest_bdd_step_stub(step: &Step, matcher: Matcher) -> String {
    let has_placeholders = segments(&step.text, true)
        .iter()
        .any(|s| matches!(s, Seg::Placeholder(_) | Seg::QuotedPlaceholder(_)));
    let matcher = if matcher == Matcher::String && has_placeholders {
        Matcher::Parse
    } else {
        matcher
    };
    let (pattern, params) = build_pattern(&step.text, matcher, true, &|_| None);
    let literal = py_literal(&pattern, matcher == Matcher::Re);
    let arg = match matcher {
        Matcher::String => literal,
        Matcher::Parse if params.is_empty() => literal,
        m => format!("parsers.{}({literal})", m.name()),
    };
    format!(
        "@{}({arg})\ndef _({}):\n    raise NotImplementedError\n",
        decorator_kind(step).decorator(),
        params.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;
    use crate::stepdefs::{scan_step_definitions, BddFramework};
    use pretty_assertions::assert_eq;
    use std::path::Path;

    fn step(src_step: &str) -> Step {
        let f = parse(&format!("Feature: F\n  Scenario: s\n    {src_step}\n"));
        f.scenarios[0].steps[0].clone()
    }

    #[test]
    fn behave_stubs() {
        assert_eq!(step_definition_stub(&step("Given I have 5 apples"), Matcher::Parse), "@given('I have {number:d} apples')\ndef step_impl(context, number):\n    raise NotImplementedError\n");
        assert_eq!(step_definition_stub(&step("When I pay 2.50 for \"tea\" and 3 for \"cake\""), Matcher::Parse), "@when('I pay {number:f} for \"{text}\" and {number2:d} for \"{text2}\"')\ndef step_impl(context, number, text, number2, text2):\n    raise NotImplementedError\n");
        assert_eq!(step_definition_stub(&step("Then it's {odd} item42"), Matcher::Parse), "@then(\"it's {{odd}} item42\")\ndef step_impl(context):\n    raise NotImplementedError\n");
        assert_eq!(step_definition_stub(&step("And I have 5 apples (red)"), Matcher::Re), "@given(r'I have (?P<number>-?\\d+) apples \\(red\\)')\ndef step_impl(context, number):\n    raise NotImplementedError\n");
        assert_eq!(step_definition_stub(&step("Given a <first name> named <class>"), Matcher::CFParse), "@given('a {first_name} named {class_}')\ndef step_impl(context, first_name, class_):\n    raise NotImplementedError\n");
    }

    #[test]
    fn outline_stub_infers_types() {
        let f = parse("Feature: F\n  Scenario Outline: o\n    Given I have <count> apples at <price> as \"<kind>\" and <count> pears\n    Examples:\n      | count | price | kind |\n      | 5     | 1.5   | red  |\n      | 12    | 2.25  | 7    |\n");
        let sc = &f.scenarios[0];
        assert_eq!(step_definition_stub_for(sc, &sc.steps[0], Matcher::Parse), "@given('I have {count:d} apples at {price:f} as \"{kind}\" and {count:d} pears')\ndef step_impl(context, count, price, kind):\n    raise NotImplementedError\n");
        let re_stub = step_definition_stub_for(sc, &sc.steps[0], Matcher::Re);
        assert_eq!(re_stub, "@given(r'I have (?P<count>-?\\d+) apples at (?P<price>-?\\d*\\.\\d+) as \"(?P<kind>[^\"]*)\" and (?P=count) pears')\ndef step_impl(context, count, price, kind):\n    raise NotImplementedError\n");
        let defs = scan_step_definitions(
            &format!("use_step_matcher('re')\n{re_stub}"),
            Path::new("s.py"),
        );
        assert!(defs[0].is_match("I have 5 apples at 1.5 as \"red\" and 5 pears"));
        // In a plain scenario `<b>` is literal text.
        let plain = parse("Feature: F\n  Scenario: s\n    Given the page shows <b>\n");
        assert_eq!(step_definition_stub_for(&plain.scenarios[0], &plain.scenarios[0].steps[0], Matcher::Parse), "@given('the page shows <b>')\ndef step_impl(context):\n    raise NotImplementedError\n");
    }

    #[test]
    fn pytest_bdd_stubs() {
        assert_eq!(pytest_bdd_step_stub(&step("Given I have 5 apples"), Matcher::Parse), "@given(parsers.parse('I have {number:d} apples'))\ndef _(number):\n    raise NotImplementedError\n");
        assert_eq!(
            pytest_bdd_step_stub(&step("Given I have apples"), Matcher::Parse),
            "@given('I have apples')\ndef _():\n    raise NotImplementedError\n"
        );
        assert_eq!(
            pytest_bdd_step_stub(&step("Then I see 5 apples"), Matcher::String),
            "@then('I see 5 apples')\ndef _():\n    raise NotImplementedError\n"
        );
        assert_eq!(
            pytest_bdd_step_stub(&step("When I eat <n>"), Matcher::String),
            "@when(parsers.parse('I eat {n}'))\ndef _(n):\n    raise NotImplementedError\n"
        );
        assert_eq!(pytest_bdd_step_stub(&step("When I eat 3"), Matcher::Re), "@when(parsers.re(r'I eat (?P<number>-?\\d+)'))\ndef _(number):\n    raise NotImplementedError\n");
    }

    #[test]
    fn generated_stubs_match_their_steps() {
        for (text, m) in [
            ("Given I have 5 apples", Matcher::Parse),
            ("When I pay 2.50 for \"tea\"", Matcher::Re),
            ("Then it's {odd} (x)", Matcher::CFParse),
            ("Then a.b|c [d]", Matcher::Re),
        ] {
            let st = step(text);
            let stub = step_definition_stub(&st, m);
            let defs = scan_step_definitions(
                &format!(
                    "from behave import *\nuse_step_matcher('{}')\n{stub}",
                    m.name()
                ),
                Path::new("s.py"),
            );
            assert_eq!(defs.len(), 1, "{stub}");
            assert!(defs[0].is_match(&st.text), "{stub} vs {}", st.text);
            let bdd = pytest_bdd_step_stub(&st, m);
            let defs = scan_step_definitions(
                &format!("from pytest_bdd import given, when, then, parsers\n{bdd}"),
                Path::new("t.py"),
            );
            assert_eq!(defs[0].framework, BddFramework::PytestBdd);
            assert!(defs[0].is_match(&st.text), "{bdd} vs {}", st.text);
        }
    }

    #[test]
    fn finds_unresolved_steps() {
        let f = parse("Feature: F\n  Background:\n    Given a user\n  Scenario: s\n    When I log in\n    Then I see 3 items\n    And nothing\n  Scenario Outline: o\n    When I buy <n> items\n    Examples:\n      | n |\n      | 1 |\n      | x |\n  Rule: r\n    Background:\n      Given rule setup\n    Scenario: in rule\n      Given a user\n");
        let defs = scan_step_definitions("from behave import given, when, then\n@given('a user')\ndef a(c): pass\n@when('I log in')\ndef b(c): pass\n@given('I see {n:d} items')\ndef wrong_kind(c, n): pass\n@when('I buy {n:d} items')\ndef buy(c, n): pass\n", Path::new("steps.py"));
        let unresolved = unresolved_steps(&f, &defs);
        let got: Vec<(usize, &str)> = unresolved
            .iter()
            .map(|s| (s.line, s.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (6, "I see 3 items"),
                (7, "nothing"),
                (9, "I buy <n> items"),
                (16, "rule setup")
            ]
        );
        assert!(unresolved_steps(
            &parse("Feature: F\n  Scenario: s\n    Given a user\n"),
            &defs
        )
        .is_empty());
    }
}
