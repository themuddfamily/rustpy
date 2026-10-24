//! Live templates (`iter`, `main`, …) and surround-with templates.
//!
//! Template syntax: `$NAME$` is a variable, `$END$` marks the
//! final caret position, `$SELECTION$` receives the surrounded text and `$$`
//! is a literal dollar sign.

use std::path::Path;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

/// Where a template may be expanded.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TemplateContext {
    /// Statement position (start of a logical line).
    Statement,
    /// Inside an expression.
    Expression,
    /// Directly inside a class body.
    ClassBody,
    /// Module level.
    Module,
    Comment,
    String,
    Html,
    JavaScript,
    Django,
    Any,
}

impl TemplateContext {
    /// Whether a template declared for `self` applies at `at`.
    pub fn allows(self, at: TemplateContext) -> bool {
        match self {
            TemplateContext::Any => true,
            TemplateContext::Statement => matches!(
                at,
                TemplateContext::Statement
                    | TemplateContext::Module
                    | TemplateContext::ClassBody
                    | TemplateContext::Any
            ),
            other => other == at || at == TemplateContext::Any,
        }
    }
}

/// A template variable declaration ("Edit variables").
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateVariable {
    pub name: String,
    /// Expression evaluated first, e.g. `date()`, `className()`, `"literal"`.
    pub expression: String,
    /// Used when the expression yields nothing.
    pub default: String,
    /// Keep the variable as a tab stop even when the expression produced a value.
    pub always_stop_at: bool,
}

impl TemplateVariable {
    pub fn new(name: &str, expression: &str, default: &str) -> Self {
        Self {
            name: name.into(),
            expression: expression.into(),
            default: default.into(),
            always_stop_at: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveTemplate {
    pub abbreviation: String,
    pub description: String,
    pub template: String,
    pub contexts: Vec<TemplateContext>,
    pub group: String,
    pub variables: Vec<TemplateVariable>,
    /// Reformat the expanded text according to code style (hint for the UI).
    pub reformat: bool,
}

impl Default for LiveTemplate {
    fn default() -> Self {
        Self {
            abbreviation: String::new(),
            description: String::new(),
            template: String::new(),
            contexts: vec![TemplateContext::Any],
            group: "user".into(),
            variables: Vec::new(),
            reformat: false,
        }
    }
}

impl LiveTemplate {
    pub fn new(
        abbreviation: &str,
        description: &str,
        template: &str,
        contexts: &[TemplateContext],
        group: &str,
    ) -> Self {
        Self {
            abbreviation: abbreviation.into(),
            description: description.into(),
            template: template.into(),
            contexts: contexts.to_vec(),
            group: group.into(),
            variables: Vec::new(),
            reformat: false,
        }
    }

    pub fn with_variables(mut self, vars: Vec<TemplateVariable>) -> Self {
        self.variables = vars;
        self
    }

    pub fn applies_in(&self, at: TemplateContext) -> bool {
        self.contexts.iter().any(|c| c.allows(at))
    }

    /// Variable names in order of first appearance (excluding END/SELECTION).
    pub fn variable_names(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for piece in parse_template(&self.template) {
            if let Piece::Var(name) = piece {
                if name != "END" && name != "SELECTION" && !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        out
    }

    /// True for surround templates (they contain `$SELECTION$`).
    pub fn is_surround(&self) -> bool {
        parse_template(&self.template)
            .iter()
            .any(|p| matches!(p, Piece::Var(n) if n == "SELECTION"))
    }
}

/// Everything the expansion needs from the editor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExpandContext {
    /// Indentation of the line where the template is inserted (prefixed to
    /// every line but the first).
    pub indent: String,
    /// Selected text for surround templates. Lines after the first should
    /// carry their original indentation; `indent` is stripped from them.
    pub selection: Option<String>,
    pub file_name: String,
    pub class_name: Option<String>,
    pub function_name: Option<String>,
    pub user: String,
    pub clipboard: Option<String>,
    /// 1-based line number of the caret.
    pub line_number: usize,
}

/// Result of expanding a template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    pub text: String,
    /// `(byte start, byte end, variable name)` for every variable occurrence,
    /// in text order.
    pub tab_stops: Vec<(usize, usize, String)>,
    /// Byte offset of `$END$`, if present.
    pub end_offset: Option<usize>,
}

impl Expansion {
    /// Byte offset where the caret should go right after expansion.
    pub fn caret(&self) -> usize {
        self.tab_stops
            .first()
            .map(|(s, _, _)| *s)
            .or(self.end_offset)
            .unwrap_or(self.text.len())
    }

    /// Distinct variable names in tab order.
    pub fn variables(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (_, _, n) in &self.tab_stops {
            if !out.contains(n) {
                out.push(n.clone());
            }
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Text(String),
    Var(String),
}

/// Splits a template into literal text and `$VAR$` pieces (`$$` → `$`).
fn parse_template(template: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut text = String::new();
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '$' {
            if chars.get(i + 1) == Some(&'$') {
                text.push('$');
                i += 2;
                continue;
            }
            // Find the closing dollar of an identifier-like name.
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j > i + 1 && chars.get(j) == Some(&'$') {
                if !text.is_empty() {
                    out.push(Piece::Text(std::mem::take(&mut text)));
                }
                out.push(Piece::Var(chars[i + 1..j].iter().collect()));
                i = j + 1;
                continue;
            }
            text.push('$');
            i += 1;
            continue;
        }
        text.push(c);
        i += 1;
    }
    if !text.is_empty() {
        out.push(Piece::Text(text));
    }
    out
}

/// Evaluates a template expression such as `date()`, `className()`,
/// `capitalize(NAME)` or `"literal"`. Returns `None` when no value is known.
pub(crate) fn eval_expression(
    expr: &str,
    ctx: &ExpandContext,
    resolved: &[(String, String)],
) -> Option<String> {
    let expr = expr.trim();
    if expr.is_empty() {
        return None;
    }
    if let Some(lit) = expr.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        return serde_json::from_str::<String>(expr)
            .ok()
            .or_else(|| Some(lit.to_string()));
    }
    if let Some(lit) = expr.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        return Some(lit.to_string());
    }
    let (name, args) = match expr.find('(') {
        Some(p) if expr.ends_with(')') => {
            (expr[..p].trim(), split_args(&expr[p + 1..expr.len() - 1]))
        }
        _ => {
            // Bare variable reference.
            return resolved
                .iter()
                .find(|(n, _)| n == expr)
                .map(|(_, v)| v.clone());
        }
    };
    let arg = |i: usize| -> Option<String> {
        args.get(i).and_then(|a| eval_expression(a, ctx, resolved))
    };
    let value = match name {
        "date" => {
            let now = chrono::Local::now();
            match arg(0) {
                Some(fmt) => now.format(&java_to_strftime(&fmt)).to_string(),
                None => now.format("%Y-%m-%d").to_string(),
            }
        }
        "time" => {
            let now = chrono::Local::now();
            match arg(0) {
                Some(fmt) => now.format(&java_to_strftime(&fmt)).to_string(),
                None => now.format("%H:%M").to_string(),
            }
        }
        "user" => ctx.user.clone(),
        "fileName" => ctx.file_name.clone(),
        "fileNameWithoutExtension" => Path::new(&ctx.file_name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "className" | "pyClassName" => ctx.class_name.clone()?,
        "methodName" | "functionName" | "pyFunctionName" => ctx.function_name.clone()?,
        "clipboard" => ctx.clipboard.clone()?,
        "lineNumber" => ctx.line_number.to_string(),
        "suggestVariableName" | "pyVariableName" => arg(0).unwrap_or_else(|| "item".into()),
        "pyIterableVariable" => arg(0).unwrap_or_else(|| "iterable".into()),
        "capitalize" => {
            let v = arg(0)?;
            let mut c = v.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
        "decapitalize" => {
            let v = arg(0)?;
            let mut c = v.chars();
            match c.next() {
                Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
        "camelCase" => {
            let v = arg(0)?;
            let mut out = String::new();
            let mut upper = false;
            for ch in v.chars() {
                if ch == '_' || ch == '-' || ch == ' ' {
                    upper = true;
                } else if upper {
                    out.extend(ch.to_uppercase());
                    upper = false;
                } else {
                    out.push(ch);
                }
            }
            out
        }
        "snakeCase" => {
            let v = arg(0)?;
            let mut out = String::new();
            for (i, ch) in v.chars().enumerate() {
                if ch.is_uppercase() {
                    if i > 0 {
                        out.push('_');
                    }
                    out.extend(ch.to_lowercase());
                } else if ch == '-' || ch == ' ' {
                    out.push('_');
                } else {
                    out.push(ch);
                }
            }
            out
        }
        "enum" => arg(0)?,
        "regularExpression" => {
            let value = arg(0)?;
            let pattern = arg(1)?;
            let replacement = arg(2)?;
            if value.len() > 1024 * 1024 || pattern.len() > 4096 {
                return None;
            }
            let regex = regex::RegexBuilder::new(&pattern)
                .size_limit(1024 * 1024)
                .build()
                .ok()?;
            let mut out = String::new();
            let mut end = 0;
            for captures in regex.captures_iter(&value) {
                let matched = captures.get(0)?;
                let replaced = bounded_capture_expansion(&captures, &replacement, 1024 * 1024)?;
                if out
                    .len()
                    .saturating_add(matched.start() - end)
                    .saturating_add(replaced.len())
                    > 1024 * 1024
                {
                    return None;
                }
                out.push_str(&value[end..matched.start()]);
                out.push_str(&replaced);
                end = matched.end();
            }
            if out.len().saturating_add(value.len() - end) > 1024 * 1024 {
                return None;
            }
            out.push_str(&value[end..]);
            out
        }
        "firstWord" => arg(0)?.split_whitespace().next().unwrap_or("").to_string(),
        "escapeString" => arg(0)?.replace('\\', "\\\\").replace('"', "\\\""),
        "lowercaseAndDash" => arg(0)?.to_lowercase().replace([' ', '_'], "-"),
        // Interactive functions (completion popups, enums, scripts) have no
        // static value: the variable stays a tab stop with its default.
        _ => return None,
    };
    Some(value)
}

/// Expand Rust-regex replacement syntax without ever allocating beyond the
/// configured output ceiling, including repeated references to a large capture.
fn bounded_capture_expansion(
    captures: &regex::Captures<'_>,
    replacement: &str,
    limit: usize,
) -> Option<String> {
    let mut out = String::new();
    let mut rest = replacement;
    while let Some(dollar) = rest.find('$') {
        let literal = &rest[..dollar];
        if out.len().saturating_add(literal.len()) > limit {
            return None;
        }
        out.push_str(literal);
        rest = &rest[dollar + 1..];
        if let Some(after) = rest.strip_prefix('$') {
            if out.len() == limit {
                return None;
            }
            out.push('$');
            rest = after;
            continue;
        }
        let parsed = if let Some(braced) = rest.strip_prefix('{') {
            braced.find('}').map(|end| (&braced[..end], end + 2))
        } else {
            let end = rest
                .bytes()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
                .count();
            (end > 0).then_some((&rest[..end], end))
        };
        if let Some((name, consumed)) = parsed {
            let capture = name
                .parse::<usize>()
                .ok()
                .and_then(|index| captures.get(index))
                .or_else(|| captures.name(name));
            if let Some(capture) = capture {
                if out.len().saturating_add(capture.as_str().len()) > limit {
                    return None;
                }
                out.push_str(capture.as_str());
            }
            rest = &rest[consumed..];
        } else {
            if out.len() == limit {
                return None;
            }
            out.push('$');
        }
    }
    if out.len().saturating_add(rest.len()) > limit {
        return None;
    }
    out.push_str(rest);
    Some(out)
}

/// Splits `a, "b, c", d` into top-level arguments.
pub(crate) fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in s.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '(' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' => {
                    depth -= 1;
                    cur.push(c);
                }
                ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            },
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.into_iter()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect()
}

/// Converts common Java `SimpleDateFormat` patterns (as in
/// `date("yyyy-MM-dd")`) to strftime.
fn java_to_strftime(fmt: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let run = chars[i..].iter().take_while(|x| **x == c).count();
        let rep = match (c, run) {
            ('y', 4) => "%Y",
            ('y', _) => "%y",
            ('M', 2) => "%m",
            ('M', 3) => "%b",
            ('M', _) if run >= 4 => "%B",
            ('M', _) => "%-m",
            ('d', 2) => "%d",
            ('d', _) => "%-d",
            ('H', _) => "%H",
            ('h', _) => "%I",
            ('m', _) => "%M",
            ('s', _) => "%S",
            ('a', _) => "%p",
            ('E', _) if run >= 4 => "%A",
            ('E', _) => "%a",
            _ => {
                out.push(c);
                i += 1;
                continue;
            }
        };
        out.push_str(rep);
        i += run;
    }
    out
}

/// Expands `template` in `ctx`.
pub fn expand(template: &LiveTemplate, ctx: &ExpandContext) -> Expansion {
    let pieces = parse_template(&template.template);
    // Resolve variable values in order of appearance.
    let mut resolved: Vec<(String, String)> = Vec::new();
    let mut stops_for: Vec<(String, bool)> = Vec::new(); // (name, is tab stop)
    for name in template.variable_names() {
        let decl = template.variables.iter().find(|v| v.name == name);
        let (value, stop) = match decl {
            Some(v) => match eval_expression(&v.expression, ctx, &resolved) {
                Some(val) => (val, v.always_stop_at),
                None => {
                    let d = eval_expression(&v.default, ctx, &resolved)
                        .unwrap_or_else(|| v.default.clone());
                    (d, true)
                }
            },
            None => (String::new(), true),
        };
        resolved.push((name.clone(), value));
        stops_for.push((name, stop));
    }
    // Selection: lines after the first lose the base indentation.
    let selection = ctx.selection.as_ref().map(|s| {
        let s = s.replace("\r\n", "\n");
        let mut lines = s.split('\n');
        let first = lines.next().unwrap_or("").to_string();
        let rest: Vec<&str> = lines
            .map(|l| l.strip_prefix(ctx.indent.as_str()).unwrap_or(l))
            .collect();
        if rest.is_empty() {
            first
        } else {
            format!("{first}\n{}", rest.join("\n"))
        }
    });

    // First pass: build the text with template-relative indentation and
    // record spans.
    let mut text = String::new();
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    let mut end_offset: Option<usize> = None;
    for piece in &pieces {
        match piece {
            Piece::Text(t) => text.push_str(t),
            Piece::Var(name) if name == "END" => end_offset = Some(text.len()),
            Piece::Var(name) if name == "SELECTION" => {
                let sel = selection.clone().unwrap_or_default();
                // Indent continuation lines of the selection like the line
                // holding $SELECTION$.
                let line_start = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
                let lead: String = text[line_start..]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                let sel = if lead.is_empty() {
                    sel
                } else {
                    sel.replace('\n', &format!("\n{lead}"))
                };
                text.push_str(&sel);
            }
            Piece::Var(name) => {
                let value = resolved
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                let start = text.len();
                text.push_str(&value);
                if stops_for.iter().any(|(n, stop)| n == name && *stop) {
                    spans.push((start, text.len(), name.clone()));
                }
            }
        }
    }
    // Second pass: prefix `ctx.indent` to every non-blank line after the
    // first, remapping recorded offsets.
    if ctx.indent.is_empty() || !text.contains('\n') {
        return Expansion {
            text,
            tab_stops: spans,
            end_offset,
        };
    }
    let mut out = String::with_capacity(text.len() + 64);
    // old byte offset → new byte offset
    let mut map: Vec<usize> = Vec::with_capacity(text.len() + 1);
    let mut at_line_start = false;
    for ch in text.chars() {
        if at_line_start {
            // Blank lines stay blank.
            if ch != '\n' {
                out.push_str(&ctx.indent);
            }
            at_line_start = false;
        }
        for _ in 0..ch.len_utf8() {
            map.push(out.len());
        }
        out.push(ch);
        if ch == '\n' {
            at_line_start = true;
        }
    }
    if at_line_start {
        // Text ends with a newline: the caret line gets the indent too.
        out.push_str(&ctx.indent);
    }
    map.push(out.len());
    let remap = |o: usize| map.get(o).copied().unwrap_or(out.len());
    let tab_stops = spans
        .into_iter()
        .map(|(s, e, n)| (remap(s), remap(e), n))
        .collect();
    Expansion {
        end_offset: end_offset.map(remap),
        text: out,
        tab_stops,
    }
}

// ---------------------------------------------------------------------------
// Bundled templates

fn stmt(abbrev: &str, desc: &str, template: &str) -> LiveTemplate {
    LiveTemplate::new(
        abbrev,
        desc,
        template,
        &[TemplateContext::Statement],
        "Python",
    )
}

fn expr(abbrev: &str, desc: &str, template: &str) -> LiveTemplate {
    LiveTemplate::new(
        abbrev,
        desc,
        template,
        &[TemplateContext::Expression, TemplateContext::Statement],
        "Python",
    )
}

fn class_body(abbrev: &str, desc: &str, template: &str) -> LiveTemplate {
    LiveTemplate::new(
        abbrev,
        desc,
        template,
        &[TemplateContext::ClassBody],
        "Python",
    )
}

fn var(name: &str, expression: &str, default: &str) -> TemplateVariable {
    TemplateVariable::new(name, expression, default)
}

fn item_iterable() -> Vec<TemplateVariable> {
    vec![
        var("ITEM", "suggestVariableName()", "item"),
        var("ITERABLE", "pyIterableVariable()", "iterable"),
    ]
}

fn item_iterable_cond() -> Vec<TemplateVariable> {
    let mut v = item_iterable();
    v.push(var("CONDITION", "", "condition"));
    v
}

fn key_value_iterable() -> Vec<TemplateVariable> {
    vec![
        var("KEY", "", "key"),
        var("VALUE", "", "value"),
        var("ITERABLE", "pyIterableVariable()", "iterable.items()"),
    ]
}

static BUILTIN: Lazy<Vec<LiveTemplate>> = Lazy::new(|| {
    vec![
        stmt(
            "main",
            "if __name__ == \"__main__\"",
            "if __name__ == \"__main__\":\n    $END$",
        ),
        stmt(
            "ifmain",
            "if __name__ == \"__main__\"",
            "if __name__ == \"__main__\":\n    $END$",
        ),
        stmt("iter", "Iterate (for ... in ...)", "for $VAR$ in $ITERABLE$:\n    $END$")
            .with_variables(vec![
                var("VAR", "suggestVariableName()", "item"),
                var("ITERABLE", "pyIterableVariable()", "iterable"),
            ]),
        stmt(
            "itere",
            "Iterate with index (enumerate)",
            "for $INDEX$, $VAR$ in enumerate($ITERABLE$):\n    $END$",
        )
        .with_variables(vec![
            var("INDEX", "", "index"),
            var("VAR", "suggestVariableName()", "item"),
            var("ITERABLE", "pyIterableVariable()", "iterable"),
        ]),
        expr("comp", "List comprehension", "[$ITEM$ for $ITEM$ in $ITERABLE$]$END$")
            .with_variables(item_iterable()),
        expr(
            "compl",
            "List comprehension with condition",
            "[$ITEM$ for $ITEM$ in $ITERABLE$ if $CONDITION$]$END$",
        )
        .with_variables(item_iterable_cond()),
        expr(
            "compd",
            "Dict comprehension",
            "{$KEY$: $VALUE$ for $KEY$, $VALUE$ in $ITERABLE$}$END$",
        )
        .with_variables(key_value_iterable()),
        expr(
            "compdi",
            "Dict comprehension with condition",
            "{$KEY$: $VALUE$ for $KEY$, $VALUE$ in $ITERABLE$ if $CONDITION$}$END$",
        )
        .with_variables({
            let mut v = key_value_iterable();
            v.push(var("CONDITION", "", "condition"));
            v
        }),
        expr("compg", "Generator expression", "($ITEM$ for $ITEM$ in $ITERABLE$)$END$")
            .with_variables(item_iterable()),
        expr(
            "compgi",
            "Generator expression with condition",
            "($ITEM$ for $ITEM$ in $ITERABLE$ if $CONDITION$)$END$",
        )
        .with_variables(item_iterable_cond()),
        expr("comps", "Set comprehension", "{$ITEM$ for $ITEM$ in $ITERABLE$}$END$")
            .with_variables(item_iterable()),
        expr(
            "compsi",
            "Set comprehension with condition",
            "{$ITEM$ for $ITEM$ in $ITERABLE$ if $CONDITION$}$END$",
        )
        .with_variables(item_iterable_cond()),
        class_body(
            "prop",
            "Property getter",
            "@property\ndef $NAME$(self):\n    return self._$NAME$$END$",
        )
        .with_variables(vec![var("NAME", "", "name")]),
        class_body(
            "props",
            "Property getter and setter",
            "@property\ndef $NAME$(self):\n    return self._$NAME$\n\n@$NAME$.setter\ndef $NAME$(self, value):\n    self._$NAME$ = value$END$",
        )
        .with_variables(vec![var("NAME", "", "name")]),
        class_body(
            "propsg",
            "Property getter and setter (documented)",
            "@property\ndef $NAME$(self):\n    \"\"\"$DOC$\"\"\"\n    return self._$NAME$\n\n@$NAME$.setter\ndef $NAME$(self, value):\n    self._$NAME$ = value$END$",
        )
        .with_variables(vec![var("NAME", "", "name"), var("DOC", "", "Doc.")]),
        class_body(
            "propsd",
            "Property getter, setter and deleter",
            "@property\ndef $NAME$(self):\n    return self._$NAME$\n\n@$NAME$.setter\ndef $NAME$(self, value):\n    self._$NAME$ = value\n\n@$NAME$.deleter\ndef $NAME$(self):\n    del self._$NAME$$END$",
        )
        .with_variables(vec![var("NAME", "", "name")]),
        stmt("super", "Call super method", "super().$METHOD$($ARGS$)$END$").with_variables(vec![
            var("METHOD", "pyFunctionName()", "__init__"),
            var("ARGS", "", ""),
        ]),
        class_body("init", "__init__ method", "def __init__(self$ARGS$):\n    $END$")
            .with_variables(vec![var("ARGS", "", "")]),
        class_body("__init__", "__init__ method", "def __init__(self$ARGS$):\n    $END$")
            .with_variables(vec![var("ARGS", "", "")]),
        stmt("def", "Function definition", "def $NAME$($PARAMS$):\n    $END$")
            .with_variables(vec![var("NAME", "", "function"), var("PARAMS", "", "")]),
        stmt(
            "class",
            "Class definition",
            "class $NAME$:\n    def __init__(self):\n        $END$",
        )
        .with_variables(vec![var("NAME", "", "ClassName")]),
        stmt(
            "dataclass",
            "Dataclass definition",
            "@dataclass\nclass $NAME$:\n    $FIELD$: $TYPE$$END$",
        )
        .with_variables(vec![
            var("NAME", "", "ClassName"),
            var("FIELD", "", "field"),
            var("TYPE", "", "int"),
        ]),
        stmt(
            "tryex",
            "try / except",
            "try:\n    $SELECTION$$END$\nexcept $EXCEPTION$:\n    $HANDLER$",
        )
        .with_variables(vec![
            var("EXCEPTION", "", "Exception"),
            var("HANDLER", "", "pass"),
        ]),
        stmt(
            "tryfin",
            "try / finally",
            "try:\n    $SELECTION$$END$\nfinally:\n    $CLEANUP$",
        )
        .with_variables(vec![var("CLEANUP", "", "pass")]),
        stmt(
            "withopen",
            "with open(...) as f",
            "with open($FILE$, $MODE$) as $VAR$:\n    $END$",
        )
        .with_variables(vec![
            var("FILE", "", "\"file.txt\""),
            var("MODE", "", "\"r\""),
            var("VAR", "", "f"),
        ]),
        expr("lambda", "Lambda expression", "lambda $ARGS$: $END$")
            .with_variables(vec![var("ARGS", "", "x")]),
        stmt("test", "pytest test function", "def test_$NAME$():\n    $END$")
            .with_variables(vec![var("NAME", "", "something")]),
        stmt("fixture", "pytest fixture", "@pytest.fixture\ndef $NAME$():\n    $END$")
            .with_variables(vec![var("NAME", "", "fixture")]),
        class_body("setup", "unittest setUp", "def setUp(self):\n    $END$"),
        class_body("teardown", "unittest tearDown", "def tearDown(self):\n    $END$"),
        stmt("ifn", "if ... is None", "if $VAR$ is None:\n    $END$")
            .with_variables(vec![var("VAR", "suggestVariableName()", "var")]),
        stmt("ifnn", "if ... is not None", "if $VAR$ is not None:\n    $END$")
            .with_variables(vec![var("VAR", "suggestVariableName()", "var")]),
        stmt("while", "while loop", "while $CONDITION$:\n    $END$")
            .with_variables(vec![var("CONDITION", "", "True")]),
    ]
});

/// The bundled Python live templates.
pub fn builtin_templates() -> Vec<LiveTemplate> {
    BUILTIN.clone()
}

/// Looks up a bundled template by abbreviation.
pub fn find(abbrev: &str) -> Option<&'static LiveTemplate> {
    BUILTIN.iter().find(|t| t.abbreviation == abbrev)
}

/// Bundled templates whose abbreviation starts with `prefix` and that apply
/// in `context`.
pub fn matching(prefix: &str, context: TemplateContext) -> Vec<&'static LiveTemplate> {
    BUILTIN
        .iter()
        .filter(|t| t.abbreviation.starts_with(prefix) && t.applies_in(context))
        .collect()
}

static SURROUND: Lazy<Vec<LiveTemplate>> = Lazy::new(|| {
    let s = |abbrev: &str, desc: &str, template: &str| {
        LiveTemplate::new(
            abbrev,
            desc,
            template,
            &[TemplateContext::Statement],
            "Surround",
        )
    };
    let e = |abbrev: &str, desc: &str, template: &str| {
        LiveTemplate::new(
            abbrev,
            desc,
            template,
            &[TemplateContext::Expression],
            "Surround",
        )
    };
    vec![
        s("if", "if", "if $CONDITION$:\n    $SELECTION$").with_variables(vec![var(
            "CONDITION",
            "",
            "condition",
        )]),
        s("while", "while", "while $CONDITION$:\n    $SELECTION$").with_variables(vec![var(
            "CONDITION",
            "",
            "condition",
        )]),
        s("for", "for", "for $VAR$ in $ITERABLE$:\n    $SELECTION$").with_variables(vec![
            var("VAR", "", "item"),
            var("ITERABLE", "", "iterable"),
        ]),
        s("with", "with", "with $EXPR$ as $VAR$:\n    $SELECTION$")
            .with_variables(vec![var("EXPR", "", "expr"), var("VAR", "", "name")]),
        s(
            "try",
            "try / except",
            "try:\n    $SELECTION$\nexcept $EXCEPTION$:\n    $END$",
        )
        .with_variables(vec![var("EXCEPTION", "", "Exception")]),
        s(
            "tryfinally",
            "try / finally",
            "try:\n    $SELECTION$\nfinally:\n    $END$",
        ),
        s("def", "def", "def $NAME$($PARAMS$):\n    $SELECTION$")
            .with_variables(vec![var("NAME", "", "function"), var("PARAMS", "", "")]),
        s("class", "class", "class $NAME$:\n    $SELECTION$").with_variables(vec![var(
            "NAME",
            "",
            "ClassName",
        )]),
        e("(", "(expr)", "($SELECTION$)$END$"),
        e("[", "[expr]", "[$SELECTION$]$END$"),
        e("{", "{expr}", "{$SELECTION$}$END$"),
        e("\"", "\"expr\"", "\"$SELECTION$\"$END$"),
        e("'", "'expr'", "'$SELECTION$'$END$"),
        e("f\"", "f\"{expr}\"", "f\"{$SELECTION$}\"$END$"),
        e("str", "str(expr)", "str($SELECTION$)$END$"),
        e("print", "print(expr)", "print($SELECTION$)$END$"),
    ]
});

/// Surround-with templates (Ctrl+Alt+T).
pub fn surround_templates() -> Vec<LiveTemplate> {
    SURROUND.clone()
}

/// Loads user templates from a JSON file (an empty list if missing/invalid).
pub fn load_user_templates(path: &Path) -> Vec<LiveTemplate> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Saves user templates as pretty JSON, creating parent directories.
pub fn save_user_templates(path: &Path, templates: &[LiveTemplate]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(templates).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ExpandContext {
        ExpandContext {
            indent: String::new(),
            selection: None,
            file_name: "module.py".into(),
            class_name: Some("Widget".into()),
            function_name: Some("render".into()),
            user: "ada".into(),
            clipboard: Some("clip".into()),
            line_number: 7,
        }
    }

    #[test]
    fn parses_template_syntax() {
        let pieces = parse_template("for $VAR$ in $$x$ $END$");
        assert_eq!(
            pieces,
            vec![
                Piece::Text("for ".into()),
                Piece::Var("VAR".into()),
                Piece::Text(" in $x$ ".into()),
                Piece::Var("END".into())
            ]
        );
    }

    #[test]
    fn expands_main_and_iter() {
        let main = find("main").unwrap();
        let e = expand(main, &ctx());
        assert_eq!(e.text, "if __name__ == \"__main__\":\n    ");
        assert_eq!(e.end_offset, Some(e.text.len()));
        assert_eq!(e.caret(), e.text.len());
        let iter = find("iter").unwrap();
        let e = expand(iter, &ctx());
        assert_eq!(e.text, "for item in iterable:\n    ");
        assert_eq!(
            e.tab_stops,
            vec![(4, 8, "VAR".into()), (12, 20, "ITERABLE".into())]
        );
        assert_eq!(e.variables(), vec!["VAR", "ITERABLE"]);
        assert_eq!(e.caret(), 4);
    }

    #[test]
    fn indents_multiline_templates_and_mirrors_variables() {
        let props = find("props").unwrap();
        let mut c = ctx();
        c.indent = "    ".into();
        let e = expand(props, &c);
        let expected = "@property\n    def name(self):\n        return self._name\n\n    @name.setter\n    def name(self, value):\n        self._name = value";
        assert_eq!(e.text, expected);
        // Every occurrence of NAME is a (mirrored) tab stop.
        let names: Vec<&str> = e
            .tab_stops
            .iter()
            .map(|(s, en, _)| &e.text[*s..*en])
            .collect();
        assert_eq!(names, vec!["name"; 5]);
        assert_eq!(e.end_offset, Some(e.text.len()));
        // Blank line inside the template stays blank (no trailing indent).
        assert!(e.text.contains("\n\n    @name.setter"));
        // Templates ending in a newline get the indent on the caret line.
        let main = find("main").unwrap();
        let e = expand(main, &c);
        assert_eq!(e.text, "if __name__ == \"__main__\":\n        ");
        assert_eq!(e.end_offset, Some(e.text.len()));
    }

    #[test]
    fn predefined_functions() {
        let no_stop = |name: &str, expression: &str, default: &str| TemplateVariable {
            name: name.into(),
            expression: expression.into(),
            default: default.into(),
            always_stop_at: false,
        };
        let t = LiveTemplate::new(
            "hdr",
            "",
            "# $FILE$ / $STEM$ / $CLASS$ / $FUNC$ / $USER$ / $CLIP$ / $LINE$ / $CAP$ / $LIT$ / $DATE$",
            &[TemplateContext::Any],
            "t",
        )
        .with_variables(vec![
            no_stop("FILE", "fileName()", ""),
            no_stop("STEM", "fileNameWithoutExtension()", ""),
            no_stop("CLASS", "className()", "NoClass"),
            no_stop("FUNC", "functionName()", ""),
            no_stop("USER", "user()", ""),
            no_stop("CLIP", "clipboard()", ""),
            no_stop("LINE", "lineNumber()", ""),
            no_stop("CAP", "capitalize(FUNC)", ""),
            TemplateVariable::new("LIT", "\"literal\"", ""),
            no_stop("DATE", "date(\"yyyy\")", ""),
        ]);
        let e = expand(&t, &ctx());
        let year = chrono::Local::now().format("%Y").to_string();
        assert_eq!(
            e.text,
            format!("# module.py / module / Widget / render / ada / clip / 7 / Render / literal / {year}")
        );
        // Only LIT (always_stop_at) is a tab stop.
        assert_eq!(e.tab_stops.len(), 1);
        assert_eq!(e.tab_stops[0].2, "LIT");
        // Default used when the expression has no value.
        let mut c = ctx();
        c.class_name = None;
        let e = expand(&t, &c);
        assert!(e.text.contains("/ NoClass /"));
        assert_eq!(e.tab_stops.iter().filter(|s| s.2 == "CLASS").count(), 1);
        // date()/time() defaults and case helpers.
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(eval_expression("date()", &ctx(), &[]), Some(today));
        assert_eq!(
            eval_expression("time()", &ctx(), &[]).map(|s| s.len()),
            Some(5)
        );
        let vars = vec![("NAME".to_string(), "my_value".to_string())];
        assert_eq!(
            eval_expression("camelCase(NAME)", &ctx(), &vars).as_deref(),
            Some("myValue")
        );
        assert_eq!(
            eval_expression("snakeCase(\"MyValue\")", &ctx(), &[]).as_deref(),
            Some("my_value")
        );
        assert_eq!(eval_expression("complete()", &ctx(), &[]), None);
    }

    #[test]
    fn surround_multiline_selection() {
        let t = surround_templates()
            .into_iter()
            .find(|t| t.abbreviation == "try")
            .unwrap();
        let mut c = ctx();
        c.indent = "    ".into();
        c.selection = Some("a = 1\n    b = 2".into());
        let e = expand(&t, &c);
        assert_eq!(
            e.text,
            "try:\n        a = 1\n        b = 2\n    except Exception:\n        "
        );
        assert_eq!(e.end_offset, Some(e.text.len()));
        assert_eq!(&e.text[e.tab_stops[0].0..e.tab_stops[0].1], "Exception");
        assert!(t.is_surround());
        assert!(!find("iter").unwrap().is_surround());

        let paren = surround_templates()
            .into_iter()
            .find(|t| t.abbreviation == "(")
            .unwrap();
        let mut c = ctx();
        c.selection = Some("a + b".into());
        let e = expand(&paren, &c);
        assert_eq!(e.text, "(a + b)");
        assert_eq!(e.end_offset, Some(7));

        let fstr = surround_templates()
            .into_iter()
            .find(|t| t.abbreviation == "f\"")
            .unwrap();
        let e = expand(&fstr, &c);
        assert_eq!(e.text, "f\"{a + b}\"");
    }

    #[test]
    fn lookup_and_contexts() {
        assert!(find("comp").is_some());
        let m = matching("comp", TemplateContext::Expression);
        assert!(m.iter().any(|t| t.abbreviation == "compd"));
        assert!(matching("prop", TemplateContext::ClassBody).len() >= 3);
        assert!(matching("prop", TemplateContext::Expression).is_empty());
        assert_eq!(matching("main", TemplateContext::Module).len(), 1);
        assert_eq!(
            find("iter").unwrap().variable_names(),
            vec!["VAR", "ITERABLE"]
        );
        let abbrevs: Vec<&str> = builtin_templates()
            .iter()
            .map(|t| t.abbreviation.as_str())
            .collect::<Vec<_>>()
            .into_iter()
            .map(|s| Box::leak(s.to_string().into_boxed_str()) as &str)
            .collect();
        for a in [
            "main", "iter", "itere", "comp", "compd", "compg", "compl", "comps", "prop", "props",
            "propsd", "super", "init", "__init__",
        ] {
            assert!(abbrevs.contains(&a), "missing builtin {a}");
        }
    }

    #[test]
    fn json_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("templates.json");
        let mut t = find("iter").unwrap().clone();
        t.abbreviation = "myiter".into();
        t.group = "user".into();
        save_user_templates(&path, &[t.clone()]).unwrap();
        let loaded = load_user_templates(&path);
        assert_eq!(loaded, vec![t]);
        assert!(load_user_templates(&dir.path().join("missing.json")).is_empty());
        // Missing fields fall back to defaults.
        let partial: LiveTemplate =
            serde_json::from_str(r#"{"abbreviation":"x","template":"$END$"}"#).unwrap();
        assert_eq!(partial.contexts, vec![TemplateContext::Any]);
        assert_eq!(partial.group, "user");
    }

    #[test]
    fn java_date_patterns() {
        assert_eq!(java_to_strftime("yyyy-MM-dd HH:mm"), "%Y-%m-%d %H:%M");
        assert_eq!(java_to_strftime("d/M/yy"), "%-d/%-m/%y");
    }
}

#[cfg(test)]
mod bounded_regex_tests {
    use super::*;
    #[test]
    fn repeated_large_capture_is_rejected_before_materializing_expanded_match() {
        let value = "x".repeat(600_000);
        let regex = regex::Regex::new("(.*)").unwrap();
        let captures = regex.captures(&value).unwrap();
        assert!(bounded_capture_expansion(&captures, "$1$1", 1024 * 1024).is_none());
        assert_eq!(
            bounded_capture_expansion(&captures, "$1", 1024 * 1024)
                .unwrap()
                .len(),
            600_000
        );
    }
    #[test]
    fn bounded_replacement_matches_rust_regex_literal_capture_and_missing_group_rules() {
        let regex = regex::Regex::new("(?P<word>π)(x)").unwrap();
        let captures = regex.captures("πx").unwrap();
        for replacement in [
            "$0/$1/${word}/$$/$missing/$1tail",
            "${2}字$",
            "a${missing}b",
            "${}x",
            "$!",
            "${word",
        ] {
            let mut expected = String::new();
            captures.expand(replacement, &mut expected);
            assert_eq!(
                bounded_capture_expansion(&captures, replacement, 100).unwrap(),
                expected,
                "{replacement}"
            );
        }
    }
}
