//! Shared by the catalog tests and `examples/i18n_extract.rs`:
//!
//! * extraction of the translatable UI vocabulary from the `rustpy-ui`
//!   sources (action texts, menus, tool window titles, settings pages and
//!   every literal given to `tr`/`tr_static`/`tr_fmt`/`tr_n`), read at run
//!   time so that the coverage report always reflects the current UI. A
//!   string without a translation is shown in English, so a missing one is
//!   reported, never a test failure (other work adds UI strings all the
//!   time);
//! * strict reading of the catalog files under `locales/` (file order kept,
//!   duplicate keys rejected).
#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::de::{Deserialize, Deserializer, Error as _, MapAccess, Visitor};
use serde_json::Value;

/// Locales meant to translate every source string (the coverage report
/// lists what they lack; anything missing shows in English).
pub const COMPLETE_LOCALES: &[&str] = &["zh-CN", "ja", "ko", "de", "fr", "es"];
/// Locales that may be partial: missing strings fall back to English.
pub const PARTIAL_LOCALES: &[&str] = &["pt-BR", "ru"];

pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `crates/rustpy-ui`, the crate whose strings are extracted.
pub fn ui_crate_dir() -> PathBuf {
    crate_dir().join("../rustpy-ui")
}

pub fn locales_dir() -> PathBuf {
    crate_dir().join("locales")
}

// ---------------------------------------------------------------------------
// Rust source tokens
// ---------------------------------------------------------------------------

/// A token of Rust source, as far as string extraction needs: comments,
/// numbers, char literals and lifetimes are dropped, string literals are
/// decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    Ident(String),
    Str(String),
    Punct(char),
}

pub fn tokenize(src: &str) -> Vec<Tok> {
    let s: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        let next = s.get(i + 1).copied();
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && next == Some('/') {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0usize;
            while i < s.len() {
                if s[i] == '/' && s.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if s[i] == '*' && s.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if let Some((text, end)) = raw_string(&s, i) {
            out.push(Tok::Str(text));
            i = end;
        } else if c == '"' || (c == 'b' && next == Some('"')) {
            let (text, end) = cooked_string(&s, if c == 'b' { i + 2 } else { i + 1 });
            out.push(Tok::Str(text));
            i = end;
        } else if c == '\'' {
            i = skip_char_or_lifetime(&s, i);
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(s[start..i].iter().collect()));
        } else if c.is_ascii_digit() {
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                i += 1;
            }
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    out
}

/// `r"…"`, `r#"…"#`, `br"…"`: the contents and the index after the literal.
fn raw_string(s: &[char], i: usize) -> Option<(String, usize)> {
    let mut j = i;
    if matches!(s.get(j), Some('b' | 'c')) {
        j += 1;
    }
    if s.get(j) != Some(&'r') {
        return None;
    }
    j += 1;
    let mut hashes = 0;
    while s.get(j) == Some(&'#') {
        hashes += 1;
        j += 1;
    }
    if s.get(j) != Some(&'"') {
        return None;
    }
    let start = j + 1;
    let mut k = start;
    while k < s.len() {
        if s[k] == '"' && (0..hashes).all(|h| s.get(k + 1 + h) == Some(&'#')) {
            return Some((s[start..k].iter().collect(), k + 1 + hashes));
        }
        k += 1;
    }
    Some((s[start..].iter().collect(), s.len()))
}

/// A `"…"` literal starting after its opening quote: decoded contents and the
/// index after the closing quote.
fn cooked_string(s: &[char], mut j: usize) -> (String, usize) {
    let mut out = String::new();
    while j < s.len() {
        match s[j] {
            '"' => return (out, j + 1),
            '\\' => {
                let e = s.get(j + 1).copied().unwrap_or('\\');
                j += 2;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '0' => out.push('\0'),
                    '\\' | '"' | '\'' => out.push(e),
                    'x' => {
                        let hex: String = s[j.min(s.len())..(j + 2).min(s.len())].iter().collect();
                        if let Ok(v) = u8::from_str_radix(&hex, 16) {
                            out.push(char::from(v));
                        }
                        j += 2;
                    }
                    'u' if s.get(j) == Some(&'{') => {
                        let close = s[j..]
                            .iter()
                            .position(|&c| c == '}')
                            .map_or(s.len(), |p| j + p);
                        let hex: String = s[j + 1..close].iter().filter(|c| **c != '_').collect();
                        if let Some(ch) =
                            u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                        {
                            out.push(ch);
                        }
                        j = close + 1;
                    }
                    '\n' | '\r' => {
                        while j < s.len() && s[j].is_whitespace() {
                            j += 1;
                        }
                    }
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            c => {
                out.push(c);
                j += 1;
            }
        }
    }
    (out, j)
}

/// Skips `'x'`, `'\n'`, `'\u{2026}'` or a lifetime/label such as `'static`.
fn skip_char_or_lifetime(s: &[char], i: usize) -> usize {
    if s.get(i + 1) == Some(&'\\') {
        let mut j = i + 3;
        while j < s.len() && s[j] != '\'' {
            j += 1;
        }
        return j + 1;
    }
    if s.get(i + 2) == Some(&'\'') {
        return i + 3;
    }
    let mut j = i + 1;
    while j < s.len() && (s[j].is_alphanumeric() || s[j] == '_') {
        j += 1;
    }
    j
}

fn is_ident(t: Option<&Tok>, name: &str) -> bool {
    matches!(t, Some(Tok::Ident(s)) if s == name)
}

fn is_punct(t: Option<&Tok>, c: char) -> bool {
    t == Some(&Tok::Punct(c))
}

/// Index of the token closing the bracket opened at `open`.
fn closing(toks: &[Tok], open: usize) -> usize {
    let mut depth = 0usize;
    for (j, t) in toks.iter().enumerate().skip(open) {
        match t {
            Tok::Punct('{' | '(' | '[') => depth += 1,
            Tok::Punct('}' | ')' | ']') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
    }
    toks.len()
}

fn first_from(toks: &[Tok], from: usize, to: usize, pred: impl Fn(usize) -> bool) -> Option<usize> {
    (from..to.min(toks.len())).find(|&i| pred(i))
}

fn string_literals(toks: &[Tok]) -> Vec<String> {
    toks.iter()
        .filter_map(|t| match t {
            Tok::Str(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// The text (second string) of every `Name => ("Id", "Text", [...])` entry of
/// the `actions! { … }` invocation.
pub fn action_texts(toks: &[Tok]) -> Vec<String> {
    let Some(start) = first_from(toks, 0, toks.len(), |i| {
        is_ident(toks.get(i), "actions")
            && is_punct(toks.get(i + 1), '!')
            && is_punct(toks.get(i + 2), '{')
    }) else {
        return Vec::new();
    };
    let end = closing(toks, start + 2);
    toks[start + 3..end]
        .windows(7)
        .filter_map(|w| match w {
            [
                Tok::Ident(_),
                Tok::Punct('='),
                Tok::Punct('>'),
                Tok::Punct('('),
                Tok::Str(_),
                Tok::Punct(','),
                Tok::Str(text),
            ] => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// String literals in the body of `fn fn_name` inside `impl type_name { … }`.
pub fn fn_strings(toks: &[Tok], type_name: &str, fn_name: &str) -> Vec<String> {
    let Some(imp) = first_from(toks, 0, toks.len(), |i| {
        is_ident(toks.get(i), "impl") && is_ident(toks.get(i + 1), type_name)
    }) else {
        return Vec::new();
    };
    let Some(open) = first_from(toks, imp, toks.len(), |i| is_punct(toks.get(i), '{')) else {
        return Vec::new();
    };
    let close = closing(toks, open);
    let mut pending = vec![fn_name.to_owned()];
    let mut visited = HashSet::new();
    let mut strings = Vec::new();
    while let Some(name) = pending.pop() {
        if visited.len() >= 64 {
            break;
        }
        if !visited.insert(name.clone()) {
            continue;
        }
        let Some(function) = first_from(toks, open, close, |i| {
            is_ident(toks.get(i), "fn") && is_ident(toks.get(i + 1), &name)
        }) else {
            continue;
        };
        let Some(body) = first_from(toks, function, close, |i| is_punct(toks.get(i), '{')) else {
            continue;
        };
        let body = &toks[body..closing(toks, body)];
        strings.extend(string_literals(body));
        // The actual UI title now translates self.stable_name(). Follow only
        // explicit same-impl self method calls, never unrelated sibling methods.
        for call in body.windows(4) {
            if let [Tok::Ident(receiver), Tok::Punct('.'), Tok::Ident(method), Tok::Punct('(')] =
                call
            {
                if receiver == "self" {
                    pending.push(method.clone());
                }
            }
        }
    }
    dedup(strings)
}

/// String literals in the initializer of `const name: … = …;`.
pub fn const_strings(toks: &[Tok], name: &str) -> Vec<String> {
    let found = (|| {
        let c = first_from(toks, 0, toks.len(), |i| {
            is_ident(toks.get(i), "const") && is_ident(toks.get(i + 1), name)
        })?;
        let eq = first_from(toks, c, toks.len(), |i| is_punct(toks.get(i), '='))?;
        let end =
            first_from(toks, eq, toks.len(), |i| is_punct(toks.get(i), ';')).unwrap_or(toks.len());
        Some(string_literals(&toks[eq..end]))
    })();
    found.unwrap_or_default()
}

/// The functions UI code translates a string literal with.
const TR_FNS: &[&str] = &["tr", "tr_static", "tr_fmt", "tr_n"];

/// The arguments of the call whose `(` is at `open`, as token ranges between
/// top-level commas.
fn call_args(toks: &[Tok], open: usize) -> Vec<Range<usize>> {
    let close = closing(toks, open).min(toks.len());
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = open + 1;
    for (j, t) in toks.iter().enumerate().take(close).skip(open + 1) {
        match t {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
            Tok::Punct(',') if depth == 0 => {
                args.push(start..j);
                start = j + 1;
            }
            _ => {}
        }
    }
    if start < close {
        args.push(start..close);
    }
    args
}

/// The text an argument spells out: a string literal (`"File"`, translated:
/// `false`) or one wrapped in a translation call (`tr("File")`,
/// `crate::i18n::tr_static("File")`, translated: `true`).
fn literal_arg(arg: &[Tok]) -> Option<(String, bool)> {
    match arg {
        [Tok::Str(s)] => Some((s.clone(), false)),
        [path @ .., Tok::Ident(f), Tok::Punct('('), Tok::Str(s), Tok::Punct(')')]
            if matches!(f.as_str(), "tr" | "tr_static")
                && path
                    .iter()
                    .all(|t| matches!(t, Tok::Ident(_) | Tok::Punct(':'))) =>
        {
            Some((s.clone(), true))
        }
        _ => {
            // Formatted messages keep their source key literal and opaque data
            // arguments separate. Their placeholders are checked by catalogs.rs.
            let function = arg.iter().position(|token| {
                is_ident(Some(token), "tr_fmt") || is_ident(Some(token), "tr_n")
            })?;
            if !arg[..function]
                .iter()
                .all(|token| matches!(token, Tok::Ident(_) | Tok::Punct(':')))
                || !is_punct(arg.get(function + 1), '(')
            {
                return None;
            }
            match arg.get(function + 2) {
                Some(Tok::Str(source)) => Some((source.clone(), true)),
                _ => None,
            }
        }
    }
}

/// The literal text of argument `n` of the call whose `(` is at `open`.
fn literal_arg_of(toks: &[Tok], open: usize, n: usize) -> Option<(String, bool)> {
    let args = call_args(toks, open);
    literal_arg(&toks[args.get(n)?.clone()])
}

/// `Type::new` at `i` (the `new`): the type name.
fn type_before(toks: &[Tok], i: usize) -> Option<&str> {
    if i >= 3 && is_punct(toks.get(i - 1), ':') && is_punct(toks.get(i - 2), ':') {
        if let Some(Tok::Ident(name)) = toks.get(i - 3) {
            return Some(name);
        }
    }
    None
}

/// A method call `.name(` at `i`.
fn is_method_call(toks: &[Tok], i: usize) -> bool {
    i > 0 && is_punct(toks.get(i - 1), '.') && is_punct(toks.get(i + 1), '(')
}

/// Strings of the main menu built by `fn menu_bar`.
///
/// `ui.menu_button("Label", …)` calls at the outermost nesting level are the
/// top-level menus, deeper ones submenus; `ui.checkbox(&mut …, "Label")` and
/// `egui::Checkbox::new(&mut …, "Label")` are menu toggles;
/// `egui::Button::new("…")` and `ui.label("…")` are other menu items, and
/// `.on_hover_text("…")` / `.on_disabled_hover_text("…")` their tooltips.
/// Each label may be wrapped in `tr(…)` / `tr_static(…)`.
#[derive(Debug, Default)]
pub struct MenuStrings {
    pub menus: Vec<String>,
    pub submenus: Vec<String>,
    pub toggles: Vec<String>,
    /// `Button::new("…")` and `ui.label("…")`.
    pub labels: Vec<String>,
    /// `on_hover_text("…")` and `on_disabled_hover_text("…")`.
    pub hover_texts: Vec<String>,
    /// Literals the menu bar shows untranslated: `Checkbox::new`,
    /// `Button::new`, `label` and tooltip texts not wrapped in `tr(…)`. (The
    /// menu bar translates the labels of `menu_button` and `checkbox` itself.)
    pub untranslated: Vec<String>,
}

pub fn menu_strings(toks: &[Tok]) -> MenuStrings {
    let Some(f) = first_from(toks, 0, toks.len(), |i| {
        is_ident(toks.get(i), "fn") && is_ident(toks.get(i + 1), "menu_bar")
    }) else {
        return MenuStrings::default();
    };
    let Some(open) = first_from(toks, f, toks.len(), |i| is_punct(toks.get(i), '{')) else {
        return MenuStrings::default();
    };
    let close = closing(toks, open);
    let mut out = MenuStrings::default();
    let mut depth = 0usize;
    let mut menus: Vec<(usize, String)> = Vec::new();
    for i in open..close {
        let name = match &toks[i] {
            Tok::Punct('{') => {
                depth += 1;
                continue;
            }
            Tok::Punct('}') => {
                depth = depth.saturating_sub(1);
                continue;
            }
            Tok::Ident(name) if is_punct(toks.get(i + 1), '(') => name.as_str(),
            _ => continue,
        };
        let method = is_method_call(toks, i);
        let (label, list) = match name {
            "menu_button" if method => match literal_arg_of(toks, i + 1, 0) {
                Some((label, _)) => {
                    menus.push((depth, label));
                    continue;
                }
                None => continue,
            },
            "checkbox" if method => match literal_arg_of(toks, i + 1, 1) {
                Some((label, _)) => {
                    out.toggles.push(label);
                    continue;
                }
                None => continue,
            },
            "new" if type_before(toks, i) == Some("Checkbox") => {
                (literal_arg_of(toks, i + 1, 1), &mut out.toggles)
            }
            "new" if type_before(toks, i) == Some("Button") => {
                (literal_arg_of(toks, i + 1, 0), &mut out.labels)
            }
            "label" | "button" | "small_button" if method => {
                (literal_arg_of(toks, i + 1, 0), &mut out.labels)
            }
            "on_hover_text" | "on_disabled_hover_text" if method => {
                (literal_arg_of(toks, i + 1, 0), &mut out.hover_texts)
            }
            _ => continue,
        };
        if let Some((label, translated)) = label {
            if !translated {
                out.untranslated.push(label.clone());
            }
            list.push(label);
        }
    }
    let top = menus.iter().map(|(d, _)| *d).min();
    let (outer, inner): (Vec<_>, Vec<_>) = menus.into_iter().partition(|(d, _)| Some(*d) == top);
    out.menus = outer.into_iter().map(|(_, l)| l).collect();
    out.submenus = inner.into_iter().map(|(_, l)| l).collect();
    out
}

/// Source literals which are deliberately data/code or language-neutral symbols
/// in the central dialog renderer. Never exempt a human sentence or field name.
const VERBATIM_DIALOG_LITERALS: &[&str] = &[
    "IGNORECASE|MULTILINE|VERBOSE", // Python regex flag expression.
    "python3",                      // Executable example, not the "Python:" field label.
    "RustPy",                       // Product name.
    "#",
    "◀",
    "▶", // Table index and paging symbols.
];

/// Ordinary literal labels in `IdeApp::dialogs`, including rich text and text
/// entry hints. Paths, user data and provider diagnostics remain opaque values;
/// translated format strings are extracted without treating their arguments as
/// catalog keys. This deliberately does not scan arbitrary code string literals.
pub fn dialog_strings(toks: &[Tok]) -> MenuStrings {
    let Some(function) = first_from(toks, 0, toks.len(), |i| {
        is_ident(toks.get(i), "fn") && is_ident(toks.get(i + 1), "dialogs")
    }) else {
        return MenuStrings::default();
    };
    let Some(open) = first_from(toks, function, toks.len(), |i| is_punct(toks.get(i), '{')) else {
        return MenuStrings::default();
    };
    let mut out = MenuStrings::default();
    for i in open..closing(toks, open) {
        let Some(Tok::Ident(name)) = toks.get(i) else {
            continue;
        };
        let argument = if is_method_call(toks, i) {
            match name.as_str() {
                "label"
                | "button"
                | "small_button"
                | "heading"
                | "hint_text"
                | "menu_button"
                | "on_hover_text"
                | "on_disabled_hover_text" => Some(0),
                "checkbox" | "selectable_label" | "colored_label" => Some(1),
                _ => None,
            }
        } else if name == "new" {
            match type_before(toks, i) {
                Some("Window" | "RichText" | "Button") => Some(0),
                Some("Checkbox") => Some(1),
                _ => None,
            }
        } else {
            None
        };
        let Some((label, translated)) = argument.and_then(|n| literal_arg_of(toks, i + 1, n))
        else {
            continue;
        };
        if VERBATIM_DIALOG_LITERALS.contains(&label.as_str()) {
            continue;
        }
        if !translated {
            out.untranslated.push(label.clone());
        }
        out.labels.push(label);
    }
    out.labels = dedup(out.labels);
    out.untranslated = dedup(out.untranslated);
    out
}

/// Submenu titles of the editor's context menu: the `ui.menu_button("…")`
/// labels inside `….context_menu(…)` calls (`editor/view.rs`).
pub fn context_menu_strings(toks: &[Tok]) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..toks.len() {
        if !(is_ident(toks.get(i), "context_menu") && is_method_call(toks, i)) {
            continue;
        }
        let end = closing(toks, i + 1);
        for j in i + 2..end {
            if is_ident(toks.get(j), "menu_button") && is_method_call(toks, j) {
                if let Some((label, _)) = literal_arg_of(toks, j + 1, 0) {
                    out.push(label);
                }
            }
        }
    }
    out
}

/// String literals the UI code translates explicitly: the first argument of
/// `tr("…")`, `tr_static("…")`, `tr_fmt("…", …)` and `tr_n("…", "…", n)` (with
/// any path prefix such as `crate::i18n::`), the key of the catalog entry.
pub fn tr_calls(toks: &[Tok]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let Tok::Ident(name) = t else { continue };
        let call = TR_FNS.contains(&name.as_str()) && is_punct(toks.get(i + 1), '(');
        // Not a method `.tr(` of something else, nor the definition `fn tr(`.
        let foreign = i > 0 && (is_punct(toks.get(i - 1), '.') || is_ident(toks.get(i - 1), "fn"));
        if call && !foreign {
            if let Some(Tok::Str(s)) = toks.get(i + 2) {
                out.push(s.clone());
            }
        }
    }
    out
}

/// Widget calls that show a label, with the index of the label argument.
const LABEL_WIDGETS: &[(&str, usize)] = &[
    ("checkbox", 1),
    ("label", 0),
    ("button", 0),
    ("small_button", 0),
    ("heading", 0),
    ("strong", 0),
    ("small", 0),
    ("weak", 0),
    ("selectable_label", 1),
    ("selectable_value", 2),
    ("radio", 1),
    ("radio_value", 2),
    ("toggle_value", 1),
    ("collapsing", 0),
    ("on_hover_text", 0),
];

/// Labels written as a list a loop renders, e.g.
///
/// ```ignore
/// for (label, value) in [("Shell integration", &mut p.shell_integration), …] {
///     ui.checkbox(value, crate::i18n::tr_static(label));
/// }
/// ```
///
/// The literals at the tuple position (or the plain array elements) whose
/// binding the loop body gives a widget as its label, or to `tr`/`tr_static`.
/// Returns the translated labels and, separately, those a widget shows as
/// they are (without a translation call).
pub fn label_list_strings(toks: &[Tok]) -> (Vec<String>, Vec<String>) {
    let (mut translated, mut untranslated) = (Vec::new(), Vec::new());
    for i in 0..toks.len() {
        if !is_ident(toks.get(i), "for") {
            continue;
        }
        let Some(in_at) = first_from(toks, i + 1, i + 40, |j| is_ident(toks.get(j), "in")) else {
            continue;
        };
        if !is_punct(toks.get(in_at + 1), '[') {
            continue;
        }
        let array_end = closing(toks, in_at + 1);
        if !is_punct(toks.get(array_end + 1), '{') {
            continue;
        }
        let body_end = closing(toks, array_end + 1);
        // Pattern bindings: per tuple position, or a single binding.
        let names_of = |range: Range<usize>| -> Vec<String> {
            toks[range]
                .iter()
                .filter_map(|t| match t {
                    Tok::Ident(n) if n != "mut" && n != "ref" => Some(n.clone()),
                    _ => None,
                })
                .collect()
        };
        let tuple = is_punct(toks.get(i + 1), '(') && closing(toks, i + 1) + 1 == in_at;
        let positions: Vec<Vec<String>> = if tuple {
            call_args(toks, i + 1).into_iter().map(names_of).collect()
        } else {
            vec![names_of(i + 1..in_at)]
        };
        // How the body uses each binding: (translated, shown as is).
        let mut uses: BTreeMap<String, (bool, bool)> = BTreeMap::new();
        for j in array_end + 2..body_end {
            let Some(Tok::Ident(f)) = toks.get(j) else {
                continue;
            };
            if !is_punct(toks.get(j + 1), '(') || is_ident(toks.get(j.wrapping_sub(1)), "fn") {
                continue;
            }
            let (index, is_tr) = if TR_FNS[..2].contains(&f.as_str()) {
                (0, true)
            } else if let Some((_, n)) = LABEL_WIDGETS.iter().find(|(w, _)| w == f) {
                (*n, false)
            } else {
                continue;
            };
            let args = call_args(toks, j + 1);
            let Some(arg) = args.get(index) else { continue };
            let arg: Vec<&Tok> = toks[arg.clone()]
                .iter()
                .filter(|t| **t != Tok::Punct('&'))
                .collect();
            if let [Tok::Ident(name)] = arg.as_slice() {
                let entry = uses.entry(name.clone()).or_default();
                if is_tr {
                    entry.0 = true;
                } else {
                    entry.1 = true;
                }
            }
        }
        if uses.is_empty() {
            continue;
        }
        for element in call_args(toks, in_at + 1) {
            let parts: Vec<Range<usize>> = if tuple {
                if !is_punct(toks.get(element.start), '(') {
                    continue;
                }
                call_args(toks, element.start)
            } else {
                vec![element]
            };
            for (names, part) in positions.iter().zip(parts) {
                let Some(&(tr, shown)) = names.iter().find_map(|n| uses.get(n)) else {
                    continue;
                };
                if let [Tok::Str(label)] = &toks[part] {
                    if shown && !tr {
                        untranslated.push(label.clone());
                    } else {
                        translated.push(label.clone());
                    }
                }
            }
        }
    }
    (translated, untranslated)
}

/// The `.rs` files under `dir`, recursively, sorted.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "rs"))
        .map(walkdir::DirEntry::into_path)
        .collect();
    files.sort();
    files
}

/// The translatable vocabulary of the IDE, by origin.
#[derive(Debug, Default)]
pub struct UiStrings {
    /// `actions.rs`: the text of every action (menus, Find Action, keymap).
    pub actions: Vec<String>,
    /// `app.rs` `fn menu_bar`: top-level menus (File, Edit, View, …).
    pub menus: Vec<String>,
    /// `app.rs` `fn menu_bar`: nested menus (Macros, Tool Windows, …).
    pub submenus: Vec<String>,
    /// `app.rs` `fn menu_bar`: check boxes inside menus.
    pub menu_toggles: Vec<String>,
    /// `app.rs` `fn menu_bar`: other menu items (`Button::new`, `label`).
    pub menu_labels: Vec<String>,
    /// `app.rs` `fn menu_bar`: tooltips of menu items.
    pub menu_hover_texts: Vec<String>,
    /// `editor/view.rs` `context_menu`: submenus of the editor context menu.
    pub context_menus: Vec<String>,
    /// `app.rs` `BottomTool::title`: tool window titles.
    pub tool_windows: Vec<String>,
    /// `settings_ui.rs` `SettingsPage::title`: settings page headers.
    pub settings_pages: Vec<String>,
    /// `settings_ui.rs` `TREE`: settings tree groups and pages.
    pub settings_tree: Vec<String>,
    /// Every `src/**/*.rs`: literals passed to `tr`, `tr_static`, `tr_fmt`
    /// and `tr_n`.
    pub tr_calls: Vec<String>,
    /// Ordinary central dialog labels/hints, including rich text.
    pub dialogs: Vec<String>,
    /// Every `src/**/*.rs`: labels of lists rendered by a loop (see
    /// [`label_list_strings`]).
    pub label_lists: Vec<String>,
    /// Label list entries a widget shows without a translation call.
    pub untranslated_label_lists: Vec<String>,
    /// Dynamic Tip-of-the-Day catalog keys.
    pub tips: Vec<String>,
    /// Ordinary dialog literals rendered without a translation call.
    pub untranslated_dialogs: Vec<String>,
    /// Menu bar literals shown without translation (see
    /// [`MenuStrings::untranslated`]); not part of [`UiStrings::groups`].
    pub untranslated: Vec<String>,
}

impl UiStrings {
    /// Extracts the strings of the workspace's `crates/rustpy-ui`.
    pub fn from_workspace() -> Result<Self, String> {
        Self::from_ui_crate(&ui_crate_dir())
    }

    pub fn from_ui_crate(dir: &Path) -> Result<Self, String> {
        let read = |path: &Path| {
            std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))
        };
        let actions = tokenize(&read(&dir.join("src/actions.rs"))?);
        let app = tokenize(&read(&dir.join("src/app.rs"))?);
        let settings = tokenize(&read(&dir.join("src/app/settings_ui.rs"))?);
        let view = tokenize(&read(&dir.join("src/editor/view.rs"))?);
        let mut tr = Vec::new();
        let (mut lists, mut untranslated_lists) = (Vec::new(), Vec::new());
        for file in rust_files(&dir.join("src")) {
            let toks = tokenize(&read(&file)?);
            tr.extend(tr_calls(&toks));
            let (translated, shown) = label_list_strings(&toks);
            lists.extend(translated);
            untranslated_lists.extend(shown);
        }
        let menu = menu_strings(&app);
        let dialogs = dialog_strings(&app);
        Ok(UiStrings {
            actions: action_texts(&actions),
            menus: menu.menus,
            submenus: menu.submenus,
            menu_toggles: menu.toggles,
            menu_labels: menu.labels,
            menu_hover_texts: menu.hover_texts,
            context_menus: context_menu_strings(&view),
            tool_windows: fn_strings(&app, "BottomTool", "title"),
            settings_pages: fn_strings(&settings, "SettingsPage", "title"),
            settings_tree: const_strings(&settings, "TREE"),
            tr_calls: dedup(tr),
            dialogs: dialogs.labels,
            label_lists: dedup(lists.iter().chain(&untranslated_lists).cloned().collect()),
            untranslated_label_lists: dedup(untranslated_lists),
            tips: const_strings(&app, "TIPS"),
            untranslated_dialogs: dialogs.untranslated,
            untranslated: menu.untranslated,
        })
    }

    pub fn groups(&self) -> Vec<(&'static str, &[String])> {
        vec![
            ("action texts (actions.rs)", &self.actions),
            ("top-level menus (app.rs menu_bar)", &self.menus),
            ("submenus (app.rs menu_bar)", &self.submenus),
            ("menu check boxes (app.rs menu_bar)", &self.menu_toggles),
            ("menu items (app.rs menu_bar)", &self.menu_labels),
            ("menu tooltips (app.rs menu_bar)", &self.menu_hover_texts),
            ("editor context menu (editor/view.rs)", &self.context_menus),
            ("tool windows (BottomTool::title)", &self.tool_windows),
            ("settings pages (SettingsPage::title)", &self.settings_pages),
            ("settings tree (settings_ui.rs TREE)", &self.settings_tree),
            ("tr() calls (src/**/*.rs)", &self.tr_calls),
            (
                "central dialog labels/hints (app.rs dialogs)",
                &self.dialogs,
            ),
            ("Tip of the Day (app.rs TIPS)", &self.tips),
            ("label lists (for … in [ … ] loops)", &self.label_lists),
        ]
    }

    /// Every extracted string once, in first-seen order.
    pub fn all(&self) -> Vec<&str> {
        let mut seen = HashSet::new();
        self.groups()
            .into_iter()
            .flat_map(|(_, strings)| strings.iter().map(String::as_str))
            .filter(|s| seen.insert(*s))
            .collect()
    }
}

/// `strings` without repetitions, in first-seen order.
fn dedup(strings: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    strings
        .into_iter()
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// Catalog files
// ---------------------------------------------------------------------------

/// A catalog entry as written in `locales/<code>.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// `"Source": "Translation"`.
    Text(String),
    /// `"{n} file": {"one": "…", "other": "…"}`: forms by CLDR category.
    Plural(BTreeMap<String, String>),
}

impl Entry {
    /// Every translated string of the entry.
    pub fn forms(&self) -> Vec<&str> {
        match self {
            Entry::Text(s) => vec![s.as_str()],
            Entry::Plural(forms) => forms.values().map(String::as_str).collect(),
        }
    }
}

/// Reads `locales/<code>.json`, keeping the file order.
pub fn read_catalog(code: &str) -> Result<Vec<(String, Entry)>, String> {
    let path = locales_dir().join(format!("{code}.json"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_catalog(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Parses a catalog, rejecting duplicate keys and values that are neither a
/// string nor an object of string plural forms.
pub fn parse_catalog(text: &str) -> Result<Vec<(String, Entry)>, String> {
    let OrderedObject(fields) = serde_json::from_str(text).map_err(|e| e.to_string())?;
    fields
        .into_iter()
        .map(|(key, value)| {
            let entry = match value {
                Value::String(s) => Entry::Text(s),
                Value::Object(forms) => Entry::Plural(
                    forms
                        .into_iter()
                        .map(|(cat, form)| match form {
                            Value::String(s) => Ok((cat, s)),
                            other => Err(format!(
                                "{key:?}: plural form {cat:?} is not a string: {other}"
                            )),
                        })
                        .collect::<Result<_, _>>()?,
                ),
                other => {
                    return Err(format!(
                        "{key:?}: expected a string or plural forms, found {other}"
                    ));
                }
            };
            Ok((key, entry))
        })
        .collect()
}

/// A JSON object in file order; duplicate keys are an error.
struct OrderedObject(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for OrderedObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = OrderedObject;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OrderedObject, A::Error> {
                let mut seen = HashSet::new();
                let mut fields = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    if !seen.insert(key.clone()) {
                        return Err(A::Error::custom(format!("duplicate key {key:?}")));
                    }
                    fields.push((key, value));
                }
                Ok(OrderedObject(fields))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

/// `{name}` placeholders of a string, sorted and deduplicated.
pub fn placeholders(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close)
                if !after[..close].is_empty()
                    && after[..close]
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_') =>
            {
                out.push(&after[..close]);
                rest = &after[close + 1..];
            }
            _ => rest = after,
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}
