//! Step text ↔ step definition matching.
//!
//! `parse` / `cfparse` patterns are converted to regular expressions the way
//! the `parse` library does it (`{name}` → `(?P<name>.+?)`, `{n:d}` → signed
//! integer, `{:f}` → fixed point, ...; unknown/custom types match `.+?`), and
//! matched against the whole step text. `re` patterns must match the whole
//! text too (behave ≥ 1.2.7 anchors them; pytest-bdd uses `fullmatch`).
//! behave matches case-sensitively; pytest-bdd's `parse`/`cfparse` parsers
//! ignore case (the `parse` library default).

use std::ops::Range;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::model::{Scenario, Step, StepKind};
use crate::outline::outline_steps_expanded;
use crate::stepdefs::{BddFramework, Matcher, StepDef};

/// A compiled pattern plus `(regex group name, original field name)` pairs.
#[derive(Clone, Debug)]
pub(crate) struct Compiled {
    pub regex: Regex,
    pub names: Vec<(String, String)>,
}

/// A parameter captured from a step text by a definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepArgument {
    /// Field / group name (`None` for anonymous fields like `{:d}` or `(\d+)`).
    pub name: Option<String>,
    pub value: String,
    /// Byte range of the value within the step text.
    pub range: Range<usize>,
}

struct FormatSpec {
    fill: Option<char>,
    align: Option<char>,
    width: String,
    precision: Option<String>,
    grouping: Option<char>,
    ty: String,
}

/// `[[fill]align][sign][0][width][grouping][.precision][type]` (parse's `extract_format`).
fn extract_format(format: &str) -> FormatSpec {
    let chars: Vec<char> = format.chars().collect();
    let mut i = 0;
    let (mut fill, mut align) = (None, None);
    if !chars.is_empty() && "<>=^".contains(chars[0]) {
        align = Some(chars[0]);
        i = 1;
    } else if chars.len() > 1 && "<>=^".contains(chars[1]) {
        fill = Some(chars[0]);
        align = Some(chars[1]);
        i = 2;
    }
    if i < chars.len() && "+- ".contains(chars[i]) {
        i += 1;
    }
    if i < chars.len() && chars[i] == '0' {
        i += 1;
    }
    let mut width = String::new();
    while i < chars.len() && chars[i].is_ascii_digit() {
        width.push(chars[i]);
        i += 1;
    }
    let mut grouping = None;
    if i < chars.len() && (chars[i] == ',' || chars[i] == '_') {
        grouping = Some(chars[i]);
        i += 1;
    }
    let mut precision = None;
    if i < chars.len() && chars[i] == '.' {
        i += 1;
        let mut p = String::new();
        while i < chars.len() && chars[i].is_ascii_digit() {
            p.push(chars[i]);
            i += 1;
        }
        precision = Some(p);
    }
    FormatSpec {
        fill,
        align,
        width,
        precision,
        grouping,
        ty: chars[i..].iter().collect(),
    }
}

/// Inner regex for a parse type; the flag tells whether it is numeric
/// (numbers accept a leading sign).
fn type_regex(ty: &str, spec: &FormatSpec) -> (String, bool) {
    let s = |x: &str| x.to_string();
    match ty {
        "n" => (s(r"\d{1,3}(?:[,.]\d{3})*"), true),
        "b" => (s(r"(?:0[bB])?[01]+"), true),
        "o" => (s(r"(?:0[oO])?[0-7]+"), true),
        "x" | "X" => (s(r"(?:0[xX])?[0-9a-fA-F]+"), true),
        "%" => (s(r"\d+(?:\.\d+)?%"), true),
        "f" | "F" if spec.precision.as_deref() == Some("0") => {
            (s(r"(?:\d+|nan|NAN|inf|INF)"), true)
        }
        "f" | "F" => (s(r"(?:\d*\.\d+|nan|NAN|inf|INF)"), true),
        "e" | "E" => (
            s(r"(?:\d*\.\d+[eE][-+]?\d+|nan|NAN|[-+]?inf|[-+]?INF)"),
            true,
        ),
        "g" | "G" => (
            s(r"(?:\d+(?:\.\d+)?(?:[eE][-+]?\d+)?|nan|NAN|[-+]?inf|[-+]?INF)"),
            true,
        ),
        "d" => {
            let w = if spec.width.is_empty() {
                "+".to_string()
            } else {
                format!("{{1,{}}}", spec.width)
            };
            let g = spec.grouping.map(|c| c.to_string()).unwrap_or_default();
            (
                format!("(?:[0-9{g}]{w}|0[xX][0-9a-fA-F{g}]{w}|0[bB][01{g}]{w}|0[oO][0-7{g}]{w})"),
                true,
            )
        }
        "l" => (s("[A-Za-z]+"), false),
        "w" | "W" | "s" | "S" | "D" => (format!(r"\{ty}+"), false),
        "" => {
            let r = match (&spec.precision, spec.width.is_empty()) {
                (Some(p), false) if !p.is_empty() => format!(".{{{},{}}}?", spec.width, p),
                (Some(p), true) if !p.is_empty() => format!(".{{1,{p}}}?"),
                (_, false) => format!(".{{{},}}?", spec.width),
                _ => s(".+?"),
            };
            (r, false)
        }
        // Date/time types and types registered with `register_type` /
        // `extra_types` (their regex is not known statically).
        _ => (s(".+?"), false),
    }
}

/// Regex (inner, without the capture group) of a field format, plus the
/// padding around the group for widths/alignment.
fn field_regex(format: &str, cfparse: bool) -> (String, String, String) {
    if format.is_empty() {
        return (".+?".into(), String::new(), String::new());
    }
    let spec = extract_format(format);
    let mut ty = spec.ty.clone();
    let mut cardinality = None;
    if cfparse && ty.len() > 1 {
        if let Some(c) = ty.chars().last().filter(|c| matches!(c, '?' | '*' | '+')) {
            cardinality = Some(c);
            ty.pop();
        }
    }
    let (mut inner, numeric) = type_regex(&ty, &spec);
    let mut align = spec.align;
    if numeric {
        if align == Some('=') {
            let f = spec.fill.unwrap_or('0');
            inner = format!("{}*{inner}", regex::escape(&f.to_string()));
        }
        inner = format!("[-+ ]?{inner}");
    }
    if let Some(c) = cardinality {
        inner = match c {
            '?' => format!("(?:{inner})?"),
            '*' => format!(r"(?:(?:{inner})(?:\s*,\s*(?:{inner}))*)?"),
            _ => format!(r"(?:{inner})(?:\s*,\s*(?:{inner}))*"),
        };
    }
    if !spec.width.is_empty() && align.is_none() {
        align = Some('>');
    }
    let fill_re = regex::escape(&spec.fill.unwrap_or(' ').to_string());
    let (pre, post) = match align {
        Some('<') => (String::new(), format!("{fill_re}*")),
        Some('>') => (format!("{fill_re}*"), String::new()),
        Some('^') => (format!("{fill_re}*"), format!("{fill_re}*")),
        _ => (String::new(), String::new()),
    };
    (inner, pre, post)
}

/// Is `field` (between the braces) a parse field: `name(.attr|[key])*(:format)?`?
fn valid_field(field: &str) -> bool {
    let (name, format) = match field.split_once(':') {
        Some((n, f)) => (n, Some(f)),
        None => (field, None),
    };
    if format.is_some_and(str::is_empty) {
        return false;
    }
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let mut chars = name.chars().peekable();
    while chars.peek().is_some_and(|c| word(*c)) {
        chars.next();
    }
    loop {
        match chars.next() {
            None => return true,
            Some('.') => {
                let mut any = false;
                while chars.peek().is_some_and(|c| word(*c)) {
                    chars.next();
                    any = true;
                }
                if !any {
                    return false;
                }
            }
            Some('[') => {
                let mut any = false;
                loop {
                    match chars.next() {
                        Some(']') if any => break,
                        Some(']') | None => return false,
                        Some(_) => any = true,
                    }
                }
            }
            Some(_) => return false,
        }
    }
}

fn group_name(field: &str, used: &[(String, String)]) -> String {
    let mut g: String = field
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if g.chars().next().is_none_or(|c| c.is_ascii_digit()) {
        g.insert(0, '_');
    }
    let base = g.clone();
    let mut n = 2;
    while used.iter().any(|(gn, _)| *gn == g) {
        g = format!("{base}{n}");
        n += 1;
    }
    g
}

/// Converts a `parse` (or `cfparse`) pattern into a regex body (unanchored)
/// and the group-name mapping.
pub(crate) fn parse_pattern_to_regex(
    pattern: &str,
    cfparse: bool,
) -> (String, Vec<(String, String)>) {
    let mut out = String::new();
    let mut names: Vec<(String, String)> = Vec::new();
    let mut literal = String::new();
    let mut rest = pattern;
    let flush = |literal: &mut String, out: &mut String| {
        if !literal.is_empty() {
            out.push_str(&regex::escape(literal));
            literal.clear();
        }
    };
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("{{") {
            literal.push('{');
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix("}}") {
            literal.push('}');
            rest = r;
            continue;
        }
        if rest.starts_with('{') {
            if let Some(close) = rest.find('}') {
                let field = &rest[1..close];
                if valid_field(field) {
                    flush(&mut literal, &mut out);
                    let (name, format) = field.split_once(':').unwrap_or((field, ""));
                    let (inner, pre, post) = field_regex(format, cfparse);
                    let named = name.chars().next().is_some_and(char::is_alphabetic);
                    let group = if !named {
                        format!("({inner})")
                    } else if names.iter().any(|(_, f)| f == name) {
                        // parse uses a back-reference here; Rust regexes have none.
                        format!("(?:{inner})")
                    } else {
                        let g = group_name(name, &names);
                        let s = format!("(?P<{g}>{inner})");
                        names.push((g, name.to_string()));
                        s
                    };
                    out.push_str(&pre);
                    out.push_str(&group);
                    out.push_str(&post);
                    rest = &rest[close + 1..];
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        literal.push(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut literal, &mut out);
    (out, names)
}

/// Adapts a Python `re` pattern to the Rust regex dialect: `\Z` → `\z`,
/// named back-references `(?P=name)` → `(?:.+?)` (no back-references in Rust),
/// literal braces that are not repetitions get escaped, the `a`/`u`/`L`
/// inline flags are dropped.
pub(crate) fn python_regex_to_rust(p: &str) -> String {
    let chars: Vec<char> = p.chars().collect();
    let mut out = String::with_capacity(p.len() + 8);
    let mut i = 0;
    let mut in_class = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            let n = chars[i + 1];
            if !in_class && n == 'Z' {
                out.push_str("\\z");
            } else {
                out.push(c);
                out.push(n);
            }
            i += 2;
            continue;
        }
        if in_class {
            if c == ']' {
                in_class = false;
            } else if c == '[' {
                out.push('\\');
            }
            out.push(c);
            i += 1;
            continue;
        }
        match c {
            '[' => {
                in_class = true;
                out.push('[');
                i += 1;
                if chars.get(i) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    out.push_str("\\]");
                    i += 1;
                }
            }
            '(' if chars[i..].starts_with(&['(', '?', 'P', '=']) => {
                let close = chars[i..]
                    .iter()
                    .position(|&c| c == ')')
                    .map(|p| i + p)
                    .unwrap_or(chars.len() - 1);
                out.push_str("(?:.+?)");
                i = close + 1;
            }
            '(' if chars.get(i + 1) == Some(&'?')
                && chars.get(i + 2).is_some_and(|c| "aiLmsux".contains(*c)) =>
            {
                // Inline flags: (?aiu) or scoped (?i:...).
                let mut j = i + 2;
                let mut flags = String::new();
                while j < chars.len() && "aiLmsux-".contains(chars[j]) {
                    if !"aLu".contains(chars[j]) {
                        flags.push(chars[j]);
                    }
                    j += 1;
                }
                match chars.get(j) {
                    Some(')') if flags.is_empty() || flags == "-" => {}
                    Some(')') => out.push_str(&format!("(?{flags})")),
                    Some(':') => out.push_str(&format!(
                        "(?{}:",
                        if flags.is_empty() || flags == "-" {
                            ""
                        } else {
                            flags.as_str()
                        }
                    )),
                    _ => {
                        // Not a flag group after all; copy verbatim.
                        out.extend(&chars[i..j]);
                        i = j;
                        continue;
                    }
                }
                i = j + 1;
            }
            '{' => {
                // A repetition is `{m}`, `{m,}`, `{m,n}` or (Python only) `{,n}`;
                // anything else is a literal brace in Python.
                let close = chars[i + 1..]
                    .iter()
                    .position(|&c| c == '}')
                    .map(|p| i + 1 + p);
                let quant = close.and_then(|c| {
                    let body: String = chars[i + 1..c].iter().collect();
                    let (lo, hi) = match body.split_once(',') {
                        Some((lo, hi)) => (lo.to_string(), Some(hi.to_string())),
                        None => (body.clone(), None),
                    };
                    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
                    let ok = digits(&lo)
                        && hi.as_deref().is_none_or(digits)
                        && (!lo.is_empty() || hi.as_deref().is_some_and(|h| !h.is_empty()));
                    ok.then(|| match hi {
                        Some(h) => {
                            format!("{{{},{h}}}", if lo.is_empty() { "0" } else { lo.as_str() })
                        }
                        None => format!("{{{lo}}}"),
                    })
                    .map(|q| (q, c))
                });
                match quant {
                    Some((q, c)) if !out.is_empty() => {
                        out.push_str(&q);
                        i = c + 1;
                    }
                    _ => {
                        out.push_str("\\{");
                        i += 1;
                    }
                }
            }
            '}' => {
                out.push_str("\\}");
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

pub(crate) fn compile(
    pattern: &str,
    matcher: Matcher,
    framework: BddFramework,
) -> Option<Compiled> {
    let (source, names) = match matcher {
        Matcher::String => (format!(r"\A{}\z", regex::escape(pattern)), Vec::new()),
        Matcher::Re => (
            format!(r"\A(?:{})\z", python_regex_to_rust(pattern)),
            Vec::new(),
        ),
        Matcher::Parse | Matcher::CFParse => {
            let (body, names) = parse_pattern_to_regex(pattern, matcher == Matcher::CFParse);
            let flags = if framework == BddFramework::PytestBdd {
                "si"
            } else {
                "s"
            };
            (format!(r"(?{flags})\A(?:{body})\z"), names)
        }
    };
    let regex = Regex::new(&source).ok()?;
    let names = if matcher == Matcher::Re {
        regex
            .capture_names()
            .flatten()
            .map(|n| (n.to_string(), n.to_string()))
            .collect()
    } else {
        names
    };
    Some(Compiled { regex, names })
}

impl StepDef {
    fn compiled(&self) -> Option<&Compiled> {
        self.compiled
            .get_or_init(|| compile(&self.pattern, self.matcher, self.framework))
            .as_ref()
    }

    /// The regular expression this definition matches with (anchored), or
    /// `None` when the pattern cannot be compiled (e.g. look-arounds).
    pub fn regex(&self) -> Option<&Regex> {
        self.compiled().map(|c| &c.regex)
    }

    /// Does this definition match the whole step text?
    pub fn is_match(&self, step_text: &str) -> bool {
        self.regex().is_some_and(|r| r.is_match(step_text))
    }

    /// The parameters captured from `step_text` (in text order), or `None`
    /// when the text does not match.
    pub fn arguments(&self, step_text: &str) -> Option<Vec<StepArgument>> {
        let c = self.compiled()?;
        let caps = c.regex.captures(step_text)?;
        let mut out = Vec::new();
        for (i, name) in c.regex.capture_names().enumerate().skip(1) {
            let Some(m) = caps.get(i) else { continue };
            let field = name.map(|g| {
                c.names
                    .iter()
                    .find(|(gn, _)| gn == g)
                    .map(|(_, f)| f.clone())
                    .unwrap_or_else(|| g.to_string())
            });
            out.push(StepArgument {
                name: field,
                value: m.as_str().to_string(),
                range: m.range(),
            });
        }
        out.sort_by_key(|a| a.range.start);
        Some(out)
    }
}

/// All definitions whose pattern matches the whole `step_text`, regardless of
/// their step type, in index order.
pub fn match_step<'a>(step_text: &str, defs: &'a [StepDef]) -> Vec<&'a StepDef> {
    defs.iter().filter(|d| d.is_match(step_text)).collect()
}

/// Like [`match_step`], restricted to definitions that apply to a step of
/// type `kind` (`@given` for Given steps, `@step` for every step).
pub fn match_step_kind<'a>(
    step_text: &str,
    kind: StepKind,
    defs: &'a [StepDef],
) -> Vec<&'a StepDef> {
    defs.iter()
        .filter(|d| d.kind.applies_to(kind) && d.is_match(step_text))
        .collect()
}

/// Definitions for a parsed step (type-aware). A trailing `:` before a table
/// or doc string is optional (behave 1.2.6 strips it, 1.3 keeps it).
pub fn find_step_definitions<'a>(step: &Step, defs: &'a [StepDef]) -> Vec<&'a StepDef> {
    let found = match_step_kind(&step.text, step.kind, defs);
    if !found.is_empty() {
        return found;
    }
    match step.text.strip_suffix(':') {
        Some(t) if step.doc_string.is_some() || !step.table.is_empty() => {
            match_step_kind(t.trim_end(), step.kind, defs)
        }
        _ => found,
    }
}

/// Definitions for a step of `scenario` (for navigation): outline steps are
/// matched with their placeholders substituted by every Examples row; the
/// union of the matches is returned in index order.
pub fn definitions_for_step<'a>(
    scenario: &Scenario,
    step: &Step,
    defs: &'a [StepDef],
) -> Vec<&'a StepDef> {
    let mut found = find_step_definitions(step, defs);
    let Some(index) = scenario.steps.iter().position(|s| s.line == step.line) else {
        return found;
    };
    for ex in outline_steps_expanded(scenario) {
        if let Some(s) = ex.steps.get(index) {
            for d in find_step_definitions(s, defs) {
                if !found.iter().any(|f| std::ptr::eq(*f, d)) {
                    found.push(d);
                }
            }
        }
    }
    found.sort_by_key(|d| defs.iter().position(|x| std::ptr::eq(x, *d)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn def(kind: StepKind, pattern: &str, matcher: Matcher) -> StepDef {
        StepDef::new(kind, pattern, matcher, Path::new("steps.py"), 1)
    }

    #[test]
    fn parse_patterns() {
        let d = def(StepKind::Given, "I have {count:d} apples", Matcher::Parse);
        assert!(d.is_match("I have 5 apples"));
        assert!(d.is_match("I have -12 apples"));
        assert!(d.is_match("I have 0x1F apples"));
        assert!(!d.is_match("I have five apples"));
        assert!(!d.is_match("I have 5 apples today"));
        assert!(
            !d.is_match("i have 5 apples"),
            "behave matches case-sensitively"
        );
        assert_eq!(
            d.arguments("I have 5 apples"),
            Some(vec![StepArgument {
                name: Some("count".into()),
                value: "5".into(),
                range: 7..8
            }])
        );

        let any = def(StepKind::Given, "the user {name} logs in", Matcher::Parse);
        assert!(any.is_match("the user John Smith logs in"));
        assert!(!any.is_match("the user  logs in x"));
        let word = def(StepKind::Given, "user {name:w}", Matcher::Parse);
        assert!(word.is_match("user bob_1"));
        assert!(!word.is_match("user bob smith"));
        let float = def(StepKind::Then, "it costs {:f} dollars", Matcher::Parse);
        assert!(float.is_match("it costs 1.50 dollars"));
        assert!(float.is_match("it costs .5 dollars"));
        assert!(
            !float.is_match("it costs 1 dollars"),
            "parse's f requires a decimal point"
        );
        assert_eq!(
            float.arguments("it costs 1.50 dollars").unwrap()[0].name,
            None
        );
        let g = def(StepKind::Then, "about {x:g}", Matcher::Parse);
        assert!(g.is_match("about 1e5") && g.is_match("about 3") && !g.is_match("about x"));
        let braces = def(StepKind::Then, "literal {{x}} and (y) {v}.", Matcher::Parse);
        assert!(braces.is_match("literal {x} and (y) 1."));
        assert!(!braces.is_match("literal {x} and (y) 1!"));
        let custom = def(StepKind::Given, "a {color:Color} car", Matcher::Parse);
        assert!(custom.is_match("a dark red car"));
        let width = def(StepKind::Given, "id {:>4d}", Matcher::Parse);
        assert!(width.is_match("id   42"));
        let repeated = def(StepKind::Given, "{a} plus {a}", Matcher::Parse);
        assert!(repeated.is_match("1 plus 1"));
        let not_a_field = def(StepKind::Given, "set { x } now", Matcher::Parse);
        assert!(not_a_field.is_match("set { x } now"));
        let dotted = def(StepKind::Given, "cfg {a.b} and {c[k]}", Matcher::Parse);
        assert_eq!(
            dotted
                .arguments("cfg 1 and 2")
                .unwrap()
                .iter()
                .map(|a| a.name.clone().unwrap())
                .collect::<Vec<_>>(),
            vec!["a.b", "c[k]"]
        );
    }

    #[test]
    fn cfparse_cardinality() {
        let many = def(StepKind::Given, "the users {names:Name+}", Matcher::CFParse);
        assert!(many.is_match("the users alice, bob,carol"));
        assert!(many.is_match("the users alice"));
        let ints = def(StepKind::Given, "numbers {n:d*} end", Matcher::CFParse);
        assert!(ints.is_match("numbers 1, 2, 3 end"));
        assert!(ints.is_match("numbers  end"));
        assert!(!ints.is_match("numbers a end"));
        let opt = def(StepKind::Given, "a{x:d?}b", Matcher::CFParse);
        assert!(opt.is_match("ab") && opt.is_match("a7b"));
    }

    #[test]
    fn regex_patterns() {
        let d = def(StepKind::When, r"I eat (?P<n>\d+) cucumbers?", Matcher::Re);
        assert!(d.is_match("I eat 3 cucumbers"));
        assert!(d.is_match("I eat 1 cucumber"));
        assert!(!d.is_match("I eat 3 cucumbers now"));
        assert_eq!(
            d.arguments("I eat 3 cucumbers").unwrap(),
            vec![StepArgument {
                name: Some("n".into()),
                value: "3".into(),
                range: 6..7
            }]
        );
        let anchored = def(StepKind::When, r"^I press (\w+)$", Matcher::Re);
        assert!(anchored.is_match("I press enter"));
        let pyz = def(StepKind::When, r"done\Z", Matcher::Re);
        assert!(pyz.is_match("done"));
        let backref = def(StepKind::When, r"(?P<w>\w+) is (?P=w)", Matcher::Re);
        assert!(backref.is_match("a is a"));
        let lookahead = def(StepKind::When, r"x(?=y)", Matcher::Re);
        assert!(lookahead.regex().is_none());
        assert!(!lookahead.is_match("xy"));
        let flags = def(StepKind::When, r"(?iu)HELLO", Matcher::Re);
        assert!(flags.is_match("hello"));
        let brace = def(StepKind::When, r"a{b} c{2}", Matcher::Re);
        assert!(brace.is_match("a{b} cc"));
        let class = def(StepKind::When, r"[]a]+", Matcher::Re);
        assert!(class.is_match("a]a"));
    }

    #[test]
    fn string_and_pytest_bdd_case() {
        let exact = def(StepKind::Given, "I have a cucumber (big)", Matcher::String)
            .with_framework(BddFramework::PytestBdd);
        assert!(exact.is_match("I have a cucumber (big)"));
        assert!(!exact.is_match("I have a cucumber"));
        let p = def(StepKind::Given, "I have {n:d} cucumbers", Matcher::Parse)
            .with_framework(BddFramework::PytestBdd);
        assert!(
            p.is_match("i HAVE 3 cucumbers"),
            "pytest-bdd's parse parser ignores case"
        );
    }

    #[test]
    fn matching_by_kind() {
        let defs = vec![
            def(StepKind::Given, "a {x}", Matcher::Parse),
            def(StepKind::Step, "a b", Matcher::Parse),
            def(StepKind::Then, "a b", Matcher::Parse),
        ];
        assert_eq!(match_step("a b", &defs).len(), 3);
        let given: Vec<StepKind> = match_step_kind("a b", StepKind::Given, &defs)
            .iter()
            .map(|d| d.kind)
            .collect();
        assert_eq!(given, vec![StepKind::Given, StepKind::Step]);
        let when: Vec<StepKind> = match_step_kind("a b", StepKind::When, &defs)
            .iter()
            .map(|d| d.kind)
            .collect();
        assert_eq!(when, vec![StepKind::Step]);
        assert!(match_step("zzz", &defs).is_empty());
    }

    #[test]
    fn trailing_colon_and_outline_navigation() {
        let f = crate::parse("Feature: F\n  Scenario: s\n    Given the numbers:\n      | n |\n  Scenario Outline: o\n    When I add <a>\n    Examples:\n      | a |\n      | 1 |\n      | x |\n");
        let defs = vec![
            def(StepKind::Given, "the numbers", Matcher::Parse),
            def(StepKind::When, "I add {a:d}", Matcher::Parse),
            def(StepKind::When, "I add {a:l}", Matcher::Parse),
        ];
        assert_eq!(
            find_step_definitions(&f.scenarios[0].steps[0], &defs).len(),
            1
        );
        let o = &f.scenarios[1];
        assert!(find_step_definitions(&o.steps[0], &defs).is_empty());
        let nav: Vec<&str> = definitions_for_step(o, &o.steps[0], &defs)
            .iter()
            .map(|d| d.pattern.as_str())
            .collect();
        assert_eq!(nav, vec!["I add {a:d}", "I add {a:l}"]);
    }

    #[test]
    fn regex_sources() {
        assert_eq!(
            parse_pattern_to_regex("I have {n:d}", false).0,
            r"I have (?P<n>[-+ ]?(?:[0-9]+|0[xX][0-9a-fA-F]+|0[bB][01]+|0[oO][0-7]+))"
        );
        assert_eq!(
            parse_pattern_to_regex("{} and {}", false).0,
            "(.+?) and (.+?)"
        );
        assert_eq!(
            python_regex_to_rust(r"(?P<a>x)\Z(?P=a)"),
            r"(?P<a>x)\z(?:.+?)"
        );
    }
}
