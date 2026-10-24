//! Source-authoritative Python string injections. App owns intentions/fragment UI.
use super::state::EditorState;
use rustpy_core::{TextRange, TextSize};
use rustpy_highlight::{HighlightTag, Span};
use rustpy_syntax::{
    ast::{self, AstNode},
    Parse,
};
use rustpy_text::DocumentId;

const MAX_SOURCE: usize = 1024 * 1024;
const MAX_FRAGMENT: usize = 128 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Language {
    Python,
    Regex,
    Sql,
    Html,
    Json,
    Yaml,
    Markdown,
    FileReference,
}
impl Language {
    pub fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Regex => "regex",
            Self::Sql => "sql",
            Self::Html => "html",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Markdown => "markdown",
            Self::FileReference => "file reference",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "python" | "py" => Some(Self::Python),
            "regex" | "regexp" => Some(Self::Regex),
            "sql" => Some(Self::Sql),
            "html" | "xml" => Some(Self::Html),
            "json" => Some(Self::Json),
            "yaml" | "yml" => Some(Self::Yaml),
            "markdown" | "md" => Some(Self::Markdown),
            "file" | "file reference" => Some(Self::FileReference),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Injection {
    pub literal: TextRange,
    pub content: TextRange,
    pub language: Language,
    pub verbose: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct Temporary {
    path: Option<std::path::PathBuf>,
    document: DocumentId,
    version: u64,
    injection: Injection,
}
impl Temporary {
    pub fn injection(&self) -> &Injection {
        &self.injection
    }
    pub fn new(
        state: &EditorState,
        parse: &Parse,
        caret: TextSize,
        language: Language,
    ) -> Result<Self, String> {
        let source = state.doc.text();
        let injection = target(&source, parse, caret, language)
            .ok_or("Caret must be in a complete plain Python string")?;
        Ok(Self {
            path: state.path.clone(),
            document: state.doc.identity(),
            version: state.doc.version(),
            injection,
        })
    }
    pub fn current(&self, state: &EditorState) -> bool {
        self.document == state.doc.identity()
            && self.version == state.doc.version()
            && self.path == state.path
    }
}
fn plain(source: &str, string: &ast::StringExpr, language: Language) -> Option<Injection> {
    if string.is_bytes() || source.len() > MAX_SOURCE {
        return None;
    }
    let literal = string.text_range();
    let content = string.content_range();
    let raw = source.get(usize::from(literal.start())..usize::from(literal.end()))?;
    let prefix = raw.bytes().take_while(u8::is_ascii_alphabetic).count();
    let body = raw.get(prefix..)?;
    let delimiter = if body.starts_with("\"\"\"") {
        "\"\"\""
    } else if body.starts_with("'''") {
        "'''"
    } else if body.starts_with('"') {
        "\""
    } else if body.starts_with('\'') {
        "'"
    } else {
        return None;
    };
    if body.len() < 2 * delimiter.len()
        || !body.ends_with(delimiter)
        || usize::from(content.len()) > MAX_FRAGMENT
    {
        return None;
    }
    Some(Injection {
        literal,
        content,
        language,
        verbose: false,
    })
}
pub(crate) fn target(
    source: &str,
    parse: &Parse,
    caret: TextSize,
    language: Language,
) -> Option<Injection> {
    parse
        .syntax_node()
        .descendants()
        .filter_map(ast::StringExpr::cast)
        .filter_map(|s| plain(source, &s, language))
        .find(|i| i.content.start() <= caret && caret <= i.content.end())
}
/// Comment directive applies to the immediately following statement, including
/// every literal in its parenthesized continuation. String contents and intervening code never act as directives.
pub(crate) fn discover(
    source: &str,
    parse: &Parse,
    temporary: Option<&Temporary>,
    state: &EditorState,
) -> Vec<Injection> {
    discover_in(
        source,
        parse,
        temporary.filter(|temp| temp.current(state)),
        &state.preferences,
        state.path.as_deref(),
    )
}

/// [`discover`] with the editor's preferences and path spelled out
/// (`temporary` is current), so it can run off the UI thread.
pub(crate) fn discover_in(
    source: &str,
    parse: &Parse,
    temporary: Option<&Temporary>,
    preferences: &rustpy_settings::editor_pages::Preferences,
    path: Option<&std::path::Path>,
) -> Vec<Injection> {
    if source.len() > MAX_SOURCE || !preferences.injections_enabled {
        return vec![];
    }
    let directives: Vec<_> = parse
        .syntax_node()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == rustpy_syntax::SyntaxKind::COMMENT)
        .filter_map(|token| {
            let start = usize::from(token.text_range().start());
            let line = source[..start].rfind('\n').map_or(0, |n| n + 1);
            if !source[line..start].trim().is_empty() {
                return None;
            }
            let language = Language::parse(
                token
                    .text()
                    .strip_prefix('#')?
                    .trim()
                    .strip_prefix("language=")?,
            )?;
            let end = usize::from(token.text_range().end());
            let next = end + source[end..].find('\n')? + 1;
            Some((next, language))
        })
        .collect();
    let mut result = Vec::new();
    for string in parse
        .syntax_node()
        .descendants()
        .filter_map(ast::StringExpr::cast)
    {
        if result.len() >= 256 {
            break;
        }
        let offset = usize::from(
            string
                .syntax()
                .ancestors()
                .find(|node| node.kind().is_statement())
                .map_or(string.text_range().start(), |node| {
                    node.text_range().start()
                }),
        );
        let line_start = source[..offset].rfind('\n').map_or(0, |p| p + 1);
        let directive = directives
            .iter()
            .find(|(next, _)| *next == line_start)
            .map(|(_, language)| *language);
        if let Some(language) = directive {
            if let Some(i) = plain(source, &string, language) {
                result.push(i);
                continue;
            }
        }
        let path = path.unwrap_or(std::path::Path::new("untitled.py"));
        let content = source
            .get(
                usize::from(string.content_range().start())
                    ..usize::from(string.content_range().end()),
            )
            .unwrap_or("");
        if let Some(injection) = preferences
            .injection_rules
            .iter()
            .take(128)
            .filter(|rule| {
                rule.enabled
                    && !rule.prefix.is_empty()
                    && rustpy_settings::editor_pages::matches(&rule.file_masks, path)
                    && content.starts_with(&rule.prefix)
            })
            .find_map(|rule| {
                Language::parse(&rule.language)
                    .and_then(|language| plain(source, &string, language))
            })
        {
            result.push(injection);
            continue;
        }
        // Conservative SQL convention: only first literal argument of execute/executemany.
        if let Some(call) = string.syntax().ancestors().find_map(ast::CallExpr::cast) {
            let sql = call.callee().is_some_and(|callee| match callee {
                ast::Expr::Attribute(a) => {
                    matches!(a.attr_text().as_str(), "execute" | "executemany")
                }
                _ => false,
            });
            let first = call
                .arg_list()
                .and_then(|args| args.args().next())
                .and_then(|arg| arg.value());
            if sql && first.is_some_and(|expr| expr.syntax().text_range() == string.text_range()) {
                if let Some(i) = plain(source, &string, Language::Sql) {
                    result.push(i);
                }
            }
        }
    }
    // Existing regex discovery retains re aliases, verbose flags and Check RegExp authority.
    for regex in rustpy_regex::injections(source, parse) {
        if result.len() >= 256 {
            break;
        }
        if !result.iter().any(|i| i.literal == regex.string_range) {
            if let Some(mut i) = target(source, parse, regex.value_range.start(), Language::Regex) {
                i.verbose = regex.verbose;
                result.push(i);
            }
        }
    }
    if let Some(temp) = temporary {
        result.retain(|i| i.literal != temp.injection.literal);
        result.push(temp.injection.clone());
    }
    result.sort_by_key(|i| i.literal.start());
    result.retain(|injection| {
        !preferences
            .injections_disabled
            .iter()
            .any(|name| name == injection.language.name())
    });
    result
}
/// Host raw-content offsets stay authoritative; no decoded offset is added to source.
pub(crate) fn highlights(source: &str, injection: &Injection) -> Vec<Span> {
    let Some(raw) =
        source.get(usize::from(injection.content.start())..usize::from(injection.content.end()))
    else {
        return vec![];
    };
    if raw.len() > MAX_FRAGMENT {
        return vec![];
    }
    let Some(literal) =
        source.get(usize::from(injection.literal.start())..usize::from(injection.literal.end()))
    else {
        return vec![];
    };
    let is_raw = literal
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .any(|byte| matches!(byte, b'r' | b'R'));
    if !valid_decoding(raw, is_raw) {
        return vec![];
    }
    let (decoded, map) = decoded_map(raw, is_raw);
    let spans = if injection.language == Language::Regex {
        rustpy_regex::tokenize(&decoded, injection.verbose)
            .into_iter()
            .map(|t| Span {
                range: t.range,
                tag: match t.kind {
                    rustpy_regex::RegexTokenKind::Escape => HighlightTag::Decorator,
                    rustpy_regex::RegexTokenKind::Quantifier => HighlightTag::Number,
                    rustpy_regex::RegexTokenKind::Error => HighlightTag::Error,
                    _ => HighlightTag::Builtin,
                },
            })
            .collect()
    } else {
        rustpy_highlight::highlight_for(injection.language.name(), &decoded)
    };
    spans
        .into_iter()
        .take(8192)
        .filter_map(|span| {
            let start = usize::from(span.range.start());
            let end = usize::from(span.range.end());
            if start >= end || end > map.len() {
                return None;
            }
            Some(Span {
                range: TextRange::new(
                    injection.content.start() + TextSize::new(map[start].0 as u32),
                    injection.content.start() + TextSize::new(map[end - 1].1 as u32),
                ),
                tag: span.tag,
            })
        })
        .collect()
}
/// Rust UTF8 strings cannot represent Python surrogate escapes. Refuse invalid
/// or unsupported decoding instead of replacing the host value on edited apply.
pub(super) fn valid_decoding(raw: &str, is_raw: bool) -> bool {
    if is_raw {
        return raw
            .bytes()
            .rev()
            .take_while(|b| *b == b'\\')
            .count()
            .is_multiple_of(2);
    }
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            continue;
        }
        let Some(code) = chars.next() else {
            return false;
        };
        if code == 'N' {
            return false;
        }
        let digits = match code {
            'x' => 2,
            'u' => 4,
            'U' => 8,
            _ => 0,
        };
        if digits != 0 {
            let mut scalar = 0u32;
            for _ in 0..digits {
                let Some(digit) = chars.next().and_then(|c| c.to_digit(16)) else {
                    return false;
                };
                scalar = scalar * 16 + digit;
            }
            if char::from_u32(scalar).is_none() {
                return false;
            }
        }
    }
    true
}

/// Each decoded UTF8 byte maps to the complete host spelling that produced it.
pub(super) fn decoded_map(raw: &str, is_raw: bool) -> (String, Vec<(usize, usize)>) {
    let mut decoded = String::new();
    let mut map = Vec::new();
    let mut start = 0;
    while start < raw.len() {
        let mut end = start + raw[start..].chars().next().unwrap().len_utf8();
        let value = if !is_raw && raw.as_bytes()[start] == b'\\' && end < raw.len() {
            let code = raw[end..].chars().next().unwrap();
            end += code.len_utf8();
            let digits = match code {
                'x' => 2,
                'u' => 4,
                'U' => 8,
                _ => 0,
            };
            for _ in 0..digits {
                if raw.as_bytes().get(end).is_some_and(u8::is_ascii_hexdigit) {
                    end += 1;
                } else {
                    break;
                }
            }
            if ('0'..='7').contains(&code) {
                for _ in 0..2 {
                    if raw
                        .as_bytes()
                        .get(end)
                        .is_some_and(|b| (b'0'..=b'7').contains(b))
                    {
                        end += 1;
                    } else {
                        break;
                    }
                }
            }
            if ('0'..='7').contains(&code) {
                let value = u32::from_str_radix(&raw[start + 1..end], 8).unwrap();
                char::from_u32(value).unwrap_or('\u{FFFD}').to_string()
            } else if code == '\r' && raw.as_bytes().get(end) == Some(&b'\n') {
                end += 1;
                String::new()
            } else {
                ast::decode_string_literal(&format!("\"{}\"", &raw[start..end]))
            }
        } else {
            raw[start..end].to_owned()
        };
        map.extend(std::iter::repeat_n((start, end), value.len()));
        decoded.push_str(&value);
        start = end;
    }
    (decoded, map)
}

#[derive(Clone)]
pub(crate) struct Fragment {
    path: Option<std::path::PathBuf>,
    document: DocumentId,
    version: u64,
    injection: Injection,
    original: String,
    pub text: String,
}
impl Fragment {
    pub fn injection(&self) -> &Injection {
        &self.injection
    }
    pub fn open(state: &EditorState, injection: Injection) -> Result<Self, String> {
        let source = state.doc.text();
        let parse = rustpy_syntax::parse(&source);
        let current = target(
            &source,
            &parse,
            injection.content.start(),
            injection.language,
        )
        .ok_or("Fragment must own a complete plain string")?;
        if current.literal != injection.literal || current.content != injection.content {
            return Err("Stale injected range".into());
        }
        let original = source
            .get(usize::from(injection.literal.start())..usize::from(injection.literal.end()))
            .ok_or("Stale injected range")?
            .to_owned();
        if original.len() > MAX_FRAGMENT + 8 {
            return Err("Injected fragment is too large".into());
        }
        let prefix = original.bytes().take_while(u8::is_ascii_alphabetic).count();
        let is_raw = original[..prefix].bytes().any(|b| matches!(b, b'r' | b'R'));
        let raw = source
            .get(usize::from(injection.content.start())..usize::from(injection.content.end()))
            .ok_or("Stale injected content")?;
        if !valid_decoding(raw, is_raw) {
            return Err(
                "Invalid, surrogate or named Unicode escapes cannot be edited safely".into(),
            );
        }
        let text = decoded_map(raw, is_raw).0;
        Ok(Self {
            path: state.path.clone(),
            document: state.doc.identity(),
            version: state.doc.version(),
            injection,
            text,
            original,
        })
    }
    /// A decoded line maps through escapes to its captured host byte range.
    pub fn host_line(&self, state: &EditorState, line0: usize) -> Option<usize> {
        if self.path != state.path
            || self.document != state.doc.identity()
            || self.version != state.doc.version()
        {
            return None;
        }
        let source = state.doc.text();
        let raw = source.get(
            usize::from(self.injection.content.start())..usize::from(self.injection.content.end()),
        )?;
        let prefix = self
            .original
            .bytes()
            .take_while(u8::is_ascii_alphabetic)
            .count();
        let is_raw = self.original[..prefix]
            .bytes()
            .any(|b| matches!(b, b'r' | b'R'));
        let (decoded, map) = decoded_map(raw, is_raw);
        let offset = if line0 == 0 {
            0
        } else {
            decoded.match_indices('\n').nth(line0 - 1)?.0 + 1
        };
        let host = usize::from(self.injection.content.start())
            + map.get(offset).map(|range| range.0).unwrap_or(raw.len());
        Some(source[..host].bytes().filter(|byte| *byte == b'\n').count())
    }
    /// Apply one host edit. Stale identity/version and read-only refusal never modify source.
    pub fn apply(&self, state: &mut EditorState) -> Result<bool, String> {
        let Some(edit) = self.refactoring_edit(state)? else {
            return Ok(false);
        };
        let changed = state.apply_text_edits(vec![edit], "Edit injected fragment");
        if changed {
            state.doc.mark_refactoring();
        }
        Ok(changed)
    }
    /// Prepare an encoded host edit without modifying either document.
    pub fn refactoring_edit(
        &self,
        state: &EditorState,
    ) -> Result<Option<(TextRange, String)>, String> {
        if state.doc.identity() != self.document
            || state.doc.version() != self.version
            || state.path != self.path
        {
            return Err("Injected fragment changed; reopen it".into());
        }
        if state.doc.is_read_only() {
            return Err("Injected fragment host is read-only".into());
        }
        if self.text.len() > MAX_FRAGMENT {
            return Err("Injected fragment is too large".into());
        }
        let source = state.doc.text();
        if source.get(
            usize::from(self.injection.literal.start())..usize::from(self.injection.literal.end()),
        ) != Some(self.original.as_str())
        {
            return Err("Injected fragment changed; reopen it".into());
        }
        if Fragment::open(state, self.injection.clone())?.text == self.text {
            return Ok(None);
        }
        let encoded = encode(&self.original, &self.text)?;
        Ok(Some((self.injection.literal, encoded)))
    }
    pub fn reference(&self) -> Result<std::path::PathBuf, String> {
        if self.injection.language != Language::FileReference
            || self.text.is_empty()
            || self.text.contains('\0')
        {
            return Err("Not a valid file reference".into());
        }
        Ok(std::path::PathBuf::from(&self.text))
    }
}
fn encode(original: &str, text: &str) -> Result<String, String> {
    let prefix = original.bytes().take_while(u8::is_ascii_alphabetic).count();
    let body = original.get(prefix..).ok_or("Invalid host literal")?;
    let quote = body
        .chars()
        .next()
        .filter(|c| matches!(c, '\'' | '"'))
        .ok_or("Invalid host literal")?;
    // Normalize raw/unicode prefix to an ordinary string when necessary. Escaping
    // every backslash and newline preserves edited runtime value, never emits code.
    let mut output = String::new();
    output.push(quote);
    for c in text.chars() {
        match c {
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\0' => output.push_str("\\x00"),
            c if c == quote => {
                output.push('\\');
                output.push(c);
            }
            c => output.push(c),
        }
    }
    output.push(quote);
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn state(text: &str) -> EditorState {
        EditorState::from_text(text, super::super::Language::Python)
    }
    #[test]
    fn injected_python_line_map_owns_escaped_source_and_version() {
        let mut editor = state("code = \"a = 1\\nb = a + 1\\nprint(b)\"\n");
        let parse = rustpy_syntax::parse(&editor.doc.text());
        let injection = target(
            &editor.doc.text(),
            &parse,
            TextSize::new(10),
            Language::Python,
        )
        .unwrap();
        let fragment = Fragment::open(&editor, injection).unwrap();
        assert_eq!(fragment.text, "a = 1\nb = a + 1\nprint(b)");
        assert_eq!(fragment.host_line(&editor, 0), Some(0));
        assert_eq!(fragment.host_line(&editor, 2), Some(0));
        editor.path = Some("renamed.py".into());
        assert_eq!(fragment.host_line(&editor, 1), None);
        assert_eq!(Language::parse("Python"), Some(Language::Python));
    }

    #[test]
    fn comments_all_languages_sql_regex_and_literal_authority() {
        for name in ["sql", "html", "json", "yaml", "markdown", "regex"] {
            let text = format!("# language={name}\nx = '''π🙂 value'''\n");
            let state = state(&text);
            let parse = rustpy_syntax::parse(&text);
            let found = discover(&text, &parse, None, &state);
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].language, Language::parse(name).unwrap());
            assert_eq!(
                &text[usize::from(found[0].content.start())..usize::from(found[0].content.end())],
                "π🙂 value"
            );
        }
        let text="x = '# language=json'\ny='not injected'\ncur.execute('SELECT 1')\nre.compile(r'\\d+')\nf'not {x}'\nb'bytes'";
        let state = state(text);
        let found = discover(text, &rustpy_syntax::parse(text), None, &state);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].language, Language::Sql);
        assert_eq!(found[1].language, Language::Regex);
    }
    #[test]
    fn temporary_overrides_and_expires_on_change_or_document_swap() {
        let mut state = state("x='{}'");
        let parse = rustpy_syntax::parse(&state.doc.text());
        let temp = Temporary::new(&state, &parse, TextSize::new(3), Language::Json).unwrap();
        assert_eq!(
            discover(&state.doc.text(), &parse, Some(&temp), &state).len(),
            1
        );
        state.insert_text("x", &super::super::EditOptions::default());
        assert!(!temp.current(&state));
        let other =
            super::super::state::EditorState::from_text("x='{}'", super::super::Language::Python);
        assert!(!temp.current(&other));
    }
    #[test]
    fn fragment_quotes_backslashes_newlines_unicode_apply_single_undo() {
        let text = "before=1\nx=r'''old'''\nafter=2";
        let mut state = state(text);
        let injection = target(
            text,
            &rustpy_syntax::parse(text),
            TextSize::new(16),
            Language::Sql,
        )
        .unwrap();
        let mut fragment = Fragment::open(&state, injection).unwrap();
        fragment.text = "π'\\\n🙂\"".into();
        assert!(fragment.apply(&mut state).unwrap());
        let changed = state.doc.text();
        let string = rustpy_syntax::parse(&changed)
            .syntax_node()
            .descendants()
            .find_map(ast::StringExpr::cast)
            .unwrap();
        assert_eq!(string.value(), fragment.text);
        assert!(changed.starts_with("before=1\n"));
        assert!(changed.ends_with("\nafter=2"));
        assert!(state.undo());
        assert_eq!(state.doc.text(), text);
        assert!(state.redo());
        assert_eq!(state.doc.text(), changed);
    }
    #[test]
    fn fragment_readonly_stale_and_reference_do_not_modify_host() {
        let mut state = state("x='dir/π🙂.json'");
        let i = target(
            &state.doc.text(),
            &rustpy_syntax::parse(&state.doc.text()),
            TextSize::new(4),
            Language::FileReference,
        )
        .unwrap();
        let mut fragment = Fragment::open(&state, i).unwrap();
        assert_eq!(
            fragment.reference().unwrap(),
            std::path::PathBuf::from("dir/π🙂.json")
        );
        fragment.text = "new".into();
        state.doc.set_read_only(true);
        assert!(fragment.apply(&mut state).is_err());
        assert!(!state.doc.can_undo());
        state.doc.set_read_only(false);
        state.insert_text("#", &super::super::EditOptions::default());
        let text = state.doc.text();
        assert!(fragment.apply(&mut state).is_err());
        assert_eq!(state.doc.text(), text);
    }
    #[test]
    fn all_highlights_stay_inside_literal_source_content() {
        for (name, value) in [
            ("sql", "SELECT 1"),
            ("html", "<b>π</b>"),
            ("json", "{\"x\":1}"),
            ("yaml", "key: true"),
            ("markdown", "# title"),
            ("regex", r"\d+"),
        ] {
            let text = format!("x={value:?}");
            let parse = rustpy_syntax::parse(&text);
            let i = target(
                &text,
                &parse,
                TextSize::new(3),
                Language::parse(name).unwrap(),
            )
            .unwrap();
            let spans = highlights(&text, &i);
            assert!(!spans.is_empty(), "{name}");
            assert!(spans
                .iter()
                .all(|s| i.content.start() <= s.range.start() && s.range.end() <= i.content.end()));
        }
    }
    #[test]
    fn decoded_escape_offsets_and_octal_fragment_value_are_source_authoritative() {
        let raw = r#"{\"π\":\u0031}\n"#;
        let (decoded, map) = decoded_map(raw, false);
        assert_eq!(decoded, "{\"π\":1}\n");
        assert_eq!(map.len(), decoded.len());
        let one = decoded.find('1').unwrap();
        assert_eq!(&raw[map[one].0..map[one].1], r"\u0031");
        assert_eq!(decoded_map(r"\012", false).0, "\n");
        assert_eq!(decoded_map(r"\u0031", true).0, r"\u0031");
        let state = state(r"x='\012'");
        let i = target(
            &state.doc.text(),
            &rustpy_syntax::parse(&state.doc.text()),
            TextSize::new(3),
            Language::Json,
        )
        .unwrap();
        assert_eq!(Fragment::open(&state, i).unwrap().text, "\n");
    }
    #[test]
    fn directives_use_real_comments_and_cover_parenthesized_statement_only() {
        let text = "# language=json\nx=(\n    '{}'\n)\ny='not injected'\n";
        let editor = state(text);
        let found = discover(text, &rustpy_syntax::parse(text), None, &editor);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].language, Language::Json);
        let text = "x='''\n# language=json\nnot a directive\n'''\ny='not injected'";
        let editor = state(text);
        assert!(discover(text, &rustpy_syntax::parse(text), None, &editor).is_empty());
    }
    #[test]
    fn malformed_and_non_scalar_escapes_refuse_fragment_edit_without_lossy_replacement() {
        for raw in [
            r"\x1",
            r"\uD800",
            r"\U00110000",
            r"\N{LATIN CAPITAL LETTER A}",
        ] {
            assert!(!valid_decoding(raw, false));
            let source = format!("x='{raw}'");
            let editor = state(&source);
            if let Some(i) = target(
                &source,
                &rustpy_syntax::parse(&source),
                TextSize::new(3),
                Language::Json,
            ) {
                assert!(Fragment::open(&editor, i.clone()).is_err());
                assert!(highlights(&source, &i).is_empty());
            }
            assert_eq!(editor.doc.text(), source);
            assert!(!editor.doc.can_undo());
        }
        assert!(valid_decoding(r"\u03c0\x41\012", false));
        assert!(valid_decoding(r"\uD800", true));
    }
    #[test]
    fn captured_fragment_and_temporary_path_cannot_follow_renamed_host() {
        let mut editor = state("x='{}'");
        editor.path = Some(std::path::PathBuf::from("before.py"));
        let parse = rustpy_syntax::parse(&editor.doc.text());
        let temp = Temporary::new(&editor, &parse, TextSize::new(3), Language::Json).unwrap();
        let mut fragment = Fragment::open(&editor, temp.injection().clone()).unwrap();
        fragment.text = "[]".into();
        editor.path = Some(std::path::PathBuf::from("after.py"));
        assert!(!temp.current(&editor));
        assert!(fragment.apply(&mut editor).is_err());
        assert_eq!(editor.doc.text(), "x='{}'");
        assert!(!editor.doc.can_undo());
        assert_eq!(fragment.injection().content, temp.injection().content);
    }
}
