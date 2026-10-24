//! Static Jinja2 macro/import structure and builtin filter/test names shared by
//! template completion, navigation and inspections. Nothing is rendered.
use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::template::{self, Dialect, Kind};

/// Jinja2 builtin tests (`x is <test>`).
pub const TESTS: &[&str] = &[
    "boolean",
    "callable",
    "defined",
    "divisibleby",
    "eq",
    "equalto",
    "escaped",
    "even",
    "false",
    "filter",
    "float",
    "ge",
    "greaterthan",
    "gt",
    "in",
    "integer",
    "iterable",
    "le",
    "lessthan",
    "lower",
    "lt",
    "mapping",
    "ne",
    "none",
    "number",
    "odd",
    "sameas",
    "sequence",
    "string",
    "test",
    "true",
    "undefined",
    "upper",
];
/// Jinja2 builtin filters (including aliases) plus the i18n/Flask additions
/// installed by the default application environments.
pub const FILTERS: &[&str] = &[
    "abs",
    "attr",
    "batch",
    "capitalize",
    "center",
    "count",
    "d",
    "default",
    "dictsort",
    "e",
    "escape",
    "filesizeformat",
    "first",
    "float",
    "forceescape",
    "format",
    "groupby",
    "indent",
    "int",
    "items",
    "join",
    "last",
    "length",
    "list",
    "lower",
    "map",
    "max",
    "min",
    "pprint",
    "random",
    "reject",
    "rejectattr",
    "replace",
    "reverse",
    "round",
    "safe",
    "select",
    "selectattr",
    "slice",
    "sort",
    "string",
    "striptags",
    "sum",
    "title",
    "tojson",
    "trim",
    "truncate",
    "unique",
    "upper",
    "urlencode",
    "urlize",
    "wordcount",
    "wordwrap",
    "xmlattr",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Macro {
    pub name: String,
    /// Parameter source text, e.g. `["title", "level=2"]`.
    pub params: Vec<String>,
    pub range: TextRange,
}
impl Macro {
    pub fn signature(&self) -> String {
        format!("macro {}({})", self.name, self.params.join(", "))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedName {
    pub name: String,
    pub local: String,
    pub range: TextRange,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    pub template: String,
    pub template_range: TextRange,
    /// `{% import 'x' as alias %}`.
    pub alias: Option<String>,
    /// `{% from 'x' import a, b as c %}`.
    pub names: Vec<ImportedName>,
    pub statement: TextRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Name(String),
    String(String),
    Symbol(char),
}
fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
}
/// Bounded statement lexer: identifiers, quoted strings and punctuation with
/// absolute byte ranges.
fn lex(source: &str, content: std::ops::Range<usize>) -> Vec<(Token, std::ops::Range<usize>)> {
    let mut out = Vec::new();
    let text = &source[content.clone()];
    let mut chars = text.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        if out.len() >= 512 {
            break;
        }
        let start = content.start + at;
        if ch.is_whitespace() {
            continue;
        }
        if rustpy_core::is_ident_start(ch) {
            let mut end = at + ch.len_utf8();
            while let Some(&(next, c)) = chars.peek() {
                if !rustpy_core::is_ident_continue(c) {
                    break;
                }
                end = next + c.len_utf8();
                chars.next();
            }
            out.push((
                Token::Name(text[at..end].to_string()),
                start..content.start + end,
            ));
        } else if matches!(ch, '\'' | '"') {
            let mut escaped = false;
            let mut end = None;
            for (next, c) in chars.by_ref() {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == ch {
                    end = Some(next);
                    break;
                }
            }
            let Some(end) = end else { break };
            out.push((
                Token::String(text[at + 1..end].to_string()),
                start + 1..content.start + end,
            ));
        } else {
            out.push((Token::Symbol(ch), start..start + ch.len_utf8()));
        }
    }
    out
}
fn statements(source: &str) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    if source.len() > 2 * 1024 * 1024 {
        return Vec::new();
    }
    template::parse(source, Dialect::Jinja)
        .regions
        .into_iter()
        .filter(|region| region.kind == Kind::Statement)
        .take(8192)
        .map(|region| (region.range, region.content))
        .collect()
}
fn trimmed(source: &str, content: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let text = &source[content.clone()];
    let left = text.len() - text.trim_start_matches(['-', '+']).trim_start().len();
    let right = text.trim_end_matches(['-', '+']).trim_end().len().max(left);
    content.start + left..content.start + right
}

/// Macro definitions in source order.
pub fn macros(source: &str) -> Vec<Macro> {
    let mut out = Vec::new();
    for (_, content) in statements(source) {
        let content = trimmed(source, content);
        let tokens = lex(source, content);
        let [(Token::Name(keyword), _), (Token::Name(name), name_range), rest @ ..] =
            tokens.as_slice()
        else {
            continue;
        };
        if keyword != "macro" {
            continue;
        }
        let mut params = Vec::new();
        if let Some((Token::Symbol('('), open)) = rest.first() {
            let close = rest
                .iter()
                .rev()
                .find(|(token, _)| *token == Token::Symbol(')'))
                .map(|(_, r)| r.start);
            if let Some(close) = close.filter(|close| *close > open.end) {
                let inner = &source[open.end..close];
                let mut depth = 0usize;
                let mut quote = None;
                let mut start = 0;
                for (at, ch) in inner.char_indices() {
                    match (quote, ch) {
                        (Some(q), c) if c == q => quote = None,
                        (Some(_), _) => {}
                        (None, '\'' | '"') => quote = Some(ch),
                        (None, '(' | '[' | '{') => depth += 1,
                        (None, ')' | ']' | '}') => depth = depth.saturating_sub(1),
                        (None, ',') if depth == 0 => {
                            params.push(inner[start..at].trim().to_string());
                            start = at + 1;
                        }
                        _ => {}
                    }
                }
                params.push(inner[start..].trim().to_string());
                params.retain(|param| !param.is_empty());
            }
        }
        out.push(Macro {
            name: name.clone(),
            params,
            range: range(name_range.start, name_range.end),
        });
    }
    out
}

/// Names a template exports to `import`/`from`: macros and `set` targets.
pub fn exports(source: &str) -> Vec<String> {
    let mut out: Vec<String> = macros(source).into_iter().map(|m| m.name).collect();
    for (_, content) in statements(source) {
        let tokens = lex(source, trimmed(source, content));
        if !matches!(tokens.first(), Some((Token::Name(keyword), _)) if keyword == "set") {
            continue;
        }
        for (token, _) in tokens.iter().skip(1) {
            match token {
                Token::Name(name) => out.push(name.clone()),
                Token::Symbol(',') => {}
                _ => break,
            }
        }
    }
    out
}

/// `import` and `from ... import` statements with literal template names.
pub fn imports(source: &str) -> Vec<Import> {
    let mut out = Vec::new();
    for (statement, content) in statements(source) {
        let tokens = lex(source, trimmed(source, content));
        let [(Token::Name(keyword), _), (Token::String(template), template_range), rest @ ..] =
            tokens.as_slice()
        else {
            continue;
        };
        let mut import = Import {
            template: template.clone(),
            template_range: range(template_range.start, template_range.end),
            alias: None,
            names: Vec::new(),
            statement: range(statement.start, statement.end),
        };
        match keyword.as_str() {
            "import" => {
                let [(Token::Name(word), _), (Token::Name(alias), _), ..] = rest else {
                    continue;
                };
                if word != "as" {
                    continue;
                }
                import.alias = Some(alias.clone());
            }
            "from" => {
                if !matches!(rest.first(), Some((Token::Name(word), _)) if word == "import") {
                    continue;
                }
                let mut index = 1;
                while let Some((Token::Name(name), name_range)) = rest.get(index) {
                    if matches!(name.as_str(), "with" | "without")
                        && matches!(rest.get(index + 1), Some((Token::Name(c), _)) if c == "context")
                    {
                        break;
                    }
                    let mut local = name.clone();
                    index += 1;
                    if matches!(rest.get(index), Some((Token::Name(word), _)) if word == "as") {
                        if let Some((Token::Name(alias), _)) = rest.get(index + 1) {
                            local = alias.clone();
                        }
                        index += 2;
                    }
                    import.names.push(ImportedName {
                        name: name.clone(),
                        local,
                        range: range(name_range.start, name_range.end),
                    });
                    if !matches!(rest.get(index), Some((Token::Symbol(','), _))) {
                        break;
                    }
                    index += 1;
                }
            }
            _ => continue,
        }
        out.push(import);
    }
    out
}

/// Where a macro reference at a caret is defined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MacroTarget {
    /// Defined in the same template.
    Local(String),
    /// Defined in another template (literal name as written).
    Imported { template: String, name: String },
}

/// The macro referenced by the identifier at `offset`: `{{ m() }}`,
/// `{{ alias.m() }}`, a `from` import name or its local alias. Definitions
/// themselves are not references.
pub fn reference_at(source: &str, offset: usize) -> Option<MacroTarget> {
    if source.len() > 2 * 1024 * 1024 || offset > source.len() || !source.is_char_boundary(offset) {
        return None;
    }
    let doc = template::parse(source, Dialect::Jinja);
    let region = doc.regions.iter().find(|region| {
        matches!(region.kind, Kind::Statement | Kind::Expression)
            && region.content.start <= offset
            && offset <= region.content.end
    })?;
    let start = source[region.content.start..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| rustpy_core::is_ident_continue(*c))
        .last()
        .map_or(offset, |(at, _)| region.content.start + at);
    let end = offset
        + source[offset..region.content.end]
            .char_indices()
            .find(|(_, c)| !rustpy_core::is_ident_continue(*c))
            .map_or(region.content.end - offset, |(at, _)| at);
    let word = &source[start..end];
    if word.is_empty() || !word.starts_with(rustpy_core::is_ident_start) {
        return None;
    }
    let imports = imports(source);
    let word_range = range(start, end);
    if let Some(import) = imports
        .iter()
        .find(|import| import.statement.contains_range(word_range))
    {
        // Inside an import statement: only the imported names navigate.
        return import
            .names
            .iter()
            .find(|name| name.range == word_range || name.local == word)
            .map(|name| MacroTarget::Imported {
                template: import.template.clone(),
                name: name.name.clone(),
            });
    }
    let before = source[region.content.start..start].trim_end();
    if let Some(object) = before.strip_suffix('.') {
        let object = object.trim_end();
        let alias_start = object
            .char_indices()
            .rev()
            .take_while(|(_, c)| rustpy_core::is_ident_continue(*c))
            .last()
            .map_or(object.len(), |(at, _)| at);
        let alias = &object[alias_start..];
        if object[..alias_start].ends_with('.') {
            return None;
        }
        return imports
            .iter()
            .rev()
            .find(|import| import.alias.as_deref() == Some(alias))
            .map(|import| MacroTarget::Imported {
                template: import.template.clone(),
                name: word.to_string(),
            });
    }
    let macros = macros(source);
    if macros.iter().any(|m| m.range == word_range) {
        return None;
    }
    if let Some((import, name)) = imports.iter().rev().find_map(|import| {
        import
            .names
            .iter()
            .find(|name| name.local == word)
            .map(|name| (import, name))
    }) {
        return Some(MacroTarget::Imported {
            template: import.template.clone(),
            name: name.name.clone(),
        });
    }
    macros
        .iter()
        .any(|m| m.name == word)
        .then(|| MacroTarget::Local(word.to_string()))
}

/// Macro definition range by name.
pub fn definition(source: &str, name: &str) -> Option<TextRange> {
    macros(source)
        .into_iter()
        .find(|m| m.name == name)
        .map(|m| m.range)
}

/// A completion candidate with a short detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub detail: String,
}
fn macro_items(source: &str) -> Vec<Item> {
    macros(source)
        .into_iter()
        .map(|m| Item {
            detail: m.signature(),
            label: m.name,
        })
        .collect()
}

/// Macros callable by bare name: local definitions and `from` imports.
pub fn scope_items(source: &str, load: &mut dyn FnMut(&str) -> Option<String>) -> Vec<Item> {
    let mut out = macro_items(source);
    for import in imports(source).into_iter().take(64) {
        if import.names.is_empty() {
            continue;
        }
        let loaded = load(&import.template).map(|text| macros(&text));
        for name in import.names {
            let detail = loaded
                .as_ref()
                .and_then(|macros| macros.iter().find(|m| m.name == name.name))
                .map_or_else(|| format!("from '{}'", import.template), |m| m.signature());
            if !out.iter().any(|item| item.label == name.local) {
                out.push(Item {
                    label: name.local,
                    detail,
                });
            }
        }
    }
    out
}

/// Macros reachable as `alias.<name>`; `None` when `alias` is not an import alias.
pub fn alias_items(
    source: &str,
    alias: &str,
    load: &mut dyn FnMut(&str) -> Option<String>,
) -> Option<Vec<Item>> {
    let import = imports(source)
        .into_iter()
        .rev()
        .find(|import| import.alias.as_deref() == Some(alias))?;
    Some(load(&import.template).map_or_else(Vec::new, |text| macro_items(&text)))
}

/// Completion inside an unfinished `{% from 'x' import a, <caret>`.
pub fn from_import_items(
    source: &str,
    caret: usize,
    load: &mut dyn FnMut(&str) -> Option<String>,
) -> Option<(usize, Vec<Item>)> {
    let before = source.get(..caret)?;
    let open = before.rfind("{%")?;
    let context = &before[open + 2..];
    if context.contains("%}") {
        return None;
    }
    let tokens = lex(before, trimmed(before, open + 2..caret));
    let [(Token::Name(keyword), _), (Token::String(template), _), (Token::Name(word), _), rest @ ..] =
        tokens.as_slice()
    else {
        return None;
    };
    if keyword != "from" || word != "import" {
        return None;
    }
    // Only at a name position: the start or after a comma.
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| rustpy_core::is_ident_continue(*c))
        .last()
        .map_or(caret, |(at, _)| at);
    let preceding: Vec<_> = rest.iter().filter(|(_, r)| r.end <= start).collect();
    if preceding
        .last()
        .is_some_and(|(token, _)| *token != Token::Symbol(','))
    {
        return None;
    }
    let items = load(template).map_or_else(Vec::new, |text| macro_items(&text));
    Some((start, items))
}

/// True when the caret's identifier follows `is` / `is not` inside a
/// template expression (`context` is the text between the delimiter and it).
pub fn test_position(context: &str) -> bool {
    let mut words = context.split_whitespace().rev();
    match words.next() {
        Some("is") => true,
        Some("not") => words.next() == Some("is"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(source: &str, needle: &str) -> usize {
        source.find(needle).unwrap() + 1
    }
    #[test]
    fn macros_record_names_and_parameters() {
        let source = "{%- macro field(name, value='', type='text') -%}<input>{% endmacro %}\n{% macro plain %}{% endmacro %}";
        let macros = macros(source);
        assert_eq!(macros.len(), 2);
        assert_eq!(macros[0].name, "field");
        assert_eq!(macros[0].params, ["name", "value=''", "type='text'"]);
        assert_eq!(
            &source[usize::from(macros[0].range.start())..usize::from(macros[0].range.end())],
            "field"
        );
        assert_eq!(
            macros[0].signature(),
            "macro field(name, value='', type='text')"
        );
        assert!(macros[1].params.is_empty());
        assert_eq!(
            exports("{% set title = 'x' %}{% macro m() %}{% endmacro %}"),
            ["m", "title"]
        );
    }
    #[test]
    fn imports_and_references_resolve_local_alias_and_from_names() {
        let source = "{% import 'forms.html' as forms %}{% from \"util.html\" import render as r, other with context %}\
{% macro local(x) %}{% endmacro %}{{ local(1) }}{{ forms.input('a') }}{{ r() }}{% call local(2) %}{% endcall %}";
        let imports = imports(source);
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].alias.as_deref(), Some("forms"));
        assert_eq!(imports[1].template, "util.html");
        assert_eq!(
            imports[1]
                .names
                .iter()
                .map(|n| (n.name.as_str(), n.local.as_str()))
                .collect::<Vec<_>>(),
            [("render", "r"), ("other", "other")]
        );
        assert_eq!(
            reference_at(source, at(source, "{{ local(") + 3),
            Some(MacroTarget::Local("local".into()))
        );
        assert_eq!(
            reference_at(source, at(source, "input(")),
            Some(MacroTarget::Imported {
                template: "forms.html".into(),
                name: "input".into()
            })
        );
        assert_eq!(
            reference_at(source, at(source, "r() }}")),
            Some(MacroTarget::Imported {
                template: "util.html".into(),
                name: "render".into()
            })
        );
        assert_eq!(
            reference_at(source, at(source, "render as")),
            Some(MacroTarget::Imported {
                template: "util.html".into(),
                name: "render".into()
            })
        );
        assert_eq!(
            reference_at(source, at(source, "call local") + 5),
            Some(MacroTarget::Local("local".into()))
        );
        // The definition itself and unknown names are not references.
        assert_eq!(reference_at(source, at(source, "macro local") + 6), None);
        assert_eq!(reference_at("{{ missing() }}", 4), None);
        let range = definition(source, "local").unwrap();
        assert_eq!(
            &source[usize::from(range.start())..usize::from(range.end())],
            "local"
        );
    }
    #[test]
    fn completion_items_cover_scope_alias_and_from_import() {
        let library =
            "{% macro input(name, value='') %}{% endmacro %}{% macro label(text) %}{% endmacro %}";
        let mut load = |name: &str| (name == "forms.html").then(|| library.to_string());
        let source = "{% import 'forms.html' as f %}{% from 'forms.html' import label %}{% macro own() %}{% endmacro %}";
        let scope = scope_items(source, &mut load);
        assert!(scope
            .iter()
            .any(|i| i.label == "own" && i.detail == "macro own()"));
        assert!(scope
            .iter()
            .any(|i| i.label == "label" && i.detail == "macro label(text)"));
        let alias = alias_items(source, "f", &mut load).unwrap();
        assert_eq!(
            alias.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["input", "label"]
        );
        assert!(alias_items(source, "user", &mut load).is_none());
        let text = "{% from 'forms.html' import label, in";
        let (start, items) = from_import_items(text, text.len(), &mut load).unwrap();
        assert_eq!(&text[start..], "in");
        assert!(items.iter().any(|i| i.label == "input"));
        let text = "{% from 'forms.html' import label as l";
        assert!(from_import_items(text, text.len(), &mut load).is_none());
    }
    #[test]
    fn test_position_requires_is_keyword() {
        assert!(test_position(" x is "));
        assert!(test_position(" loop.index is not "));
        assert!(!test_position(" x "));
        assert!(!test_position(" this "));
        assert!(TESTS.contains(&"divisibleby") && TESTS.contains(&"sameas"));
    }
}
