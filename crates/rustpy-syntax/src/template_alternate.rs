//! Bounded source regions for Mako and Chameleon; never renders or evaluates.
use std::ops::Range;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Mako,
    Chameleon,
}
#[derive(Clone, Debug)]
pub struct Region {
    pub range: Range<usize>,
    pub content: Range<usize>,
    pub comment: bool,
}
#[derive(Default)]
pub struct Document {
    pub regions: Vec<Region>,
    pub errors: Vec<(Range<usize>, String)>,
}
pub fn engine(source: &str, extension: &str, configured: &str) -> Option<Engine> {
    if extension == "mako" || configured == "Mako" || source.contains("<%") {
        Some(Engine::Mako)
    } else if matches!(extension, "pt" | "cpt" | "zpt")
        || configured == "Chameleon"
        || source.contains("tal:")
        || source.contains("metal:")
    {
        Some(Engine::Chameleon)
    } else {
        None
    }
}
pub fn parse(source: &str, engine: Engine) -> Document {
    let mut out = Document::default();
    if source.len() > 2 * 1024 * 1024 {
        return out;
    }
    let mut at = 0;
    let mut line_start = true;
    while at < source.len() && out.regions.len() < 8192 {
        let remaining = &source[at..];
        let (open, close, comment) = if remaining.starts_with("${") {
            ("${", "}", false)
        } else if engine == Engine::Mako && remaining.starts_with("<%doc>") {
            ("<%doc>", "</%doc>", true)
        } else if engine == Engine::Mako && remaining.starts_with("<%!") {
            ("<%!", "%>", false)
        } else if engine == Engine::Mako
            && remaining.starts_with("<%")
            && ![
                "<%block",
                "<%def",
                "<%inherit",
                "<%include",
                "<%namespace",
                "<%page",
                "<%text",
            ]
            .iter()
            .any(|tag| remaining.starts_with(tag))
        {
            ("<%", "%>", false)
        } else if engine == Engine::Mako && line_start && remaining.starts_with("##") {
            ("##", "\n", true)
        } else if engine == Engine::Mako
            && line_start
            && remaining.starts_with('%')
            && !remaining.starts_with("%%")
        {
            ("%", "\n", false)
        } else {
            let character = remaining.chars().next().unwrap();
            line_start = character == '\n' || (line_start && character.is_whitespace());
            at += character.len_utf8();
            continue;
        };
        let content_start = at + open.len();
        let found = if open == "${" {
            expression_end(source, content_start)
        } else {
            source[content_start..]
                .find(close)
                .map(|end| content_start + end)
        };
        let end = found.unwrap_or(source.len());
        if found.is_none() && close != "\n" {
            out.errors.push((
                at..source.len(),
                format!("Unclosed template delimiter {open}"),
            ));
        }
        let stop = (end + if found.is_some() { close.len() } else { 0 }).min(source.len());
        out.regions.push(Region {
            range: at..stop,
            content: content_start..end,
            comment,
        });
        at = stop;
        line_start = close == "\n";
    }
    if engine == Engine::Chameleon {
        for prefix in ["tal:", "metal:", "i18n:"] {
            let mut start = 0;
            while let Some(position) = source[start..].find(prefix) {
                let at = start + position;
                let tail = &source[at..];
                let Some(equal) = tail.find('=') else { break };
                if tail[..equal].contains(['\n', '>', '<']) {
                    start = at + prefix.len();
                    continue;
                }
                let value = at + equal + 1;
                let whitespace = source[value..].len() - source[value..].trim_start().len();
                let value = value + whitespace;
                let Some(quote) = source[value..]
                    .chars()
                    .next()
                    .filter(|quote| matches!(quote, '\'' | '"'))
                else {
                    start = value;
                    continue;
                };
                let content = value + 1;
                let end = source[content..].find(quote).map(|end| content + end);
                match end {
                    Some(end) => {
                        out.regions.push(Region {
                            range: at..end + 1,
                            content: content..end,
                            comment: false,
                        });
                        start = end + 1;
                    }
                    None => {
                        out.errors
                            .push((at..source.len(), "Unclosed Chameleon attribute".into()));
                        break;
                    }
                }
                if out.regions.len() >= 8192 {
                    break;
                }
            }
        }
    }
    out.regions.sort_by_key(|region| region.range.start);
    out
}
fn expression_end(source: &str, start: usize) -> Option<usize> {
    let mut depth = 0;
    let mut quote = None;
    let mut escape = false;
    for (at, character) in source[start..].char_indices() {
        if let Some(delimiter) = quote {
            if escape {
                escape = false;
            } else if character == '\\' {
                escape = true;
            } else if character == delimiter {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '{' | '[' | '(' => depth += 1,
            '}' if depth == 0 => return Some(start + at),
            '}' | ']' | ')' => depth -= 1,
            _ => {}
        }
    }
    None
}
pub fn completions(engine: Engine) -> &'static [&'static str] {
    match engine {
        Engine::Mako => &[
            "if",
            "elif",
            "else",
            "endif",
            "for",
            "endfor",
            "def",
            "block",
            "inherit",
            "namespace",
            "include",
            "page",
            "text",
            "doc",
        ],
        Engine::Chameleon => &[
            "tal:define",
            "tal:condition",
            "tal:repeat",
            "tal:content",
            "tal:replace",
            "tal:attributes",
            "tal:omit-tag",
            "tal:on-error",
            "metal:define-macro",
            "metal:use-macro",
            "metal:define-slot",
            "metal:fill-slot",
            "i18n:translate",
            "i18n:domain",
            "i18n:attributes",
        ],
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn nested_mako_expression_and_unclosed_delimiter() {
        let parsed = parse("π ${ {'a': {'b': 1}} }\n<% x=1 %>", Engine::Mako);
        assert_eq!(parsed.regions.len(), 2);
        assert!(parsed.errors.is_empty());
        assert!(!parse("${ missing", Engine::Mako).errors.is_empty());
    }
    #[test]
    fn tal_attributes_preserve_source_byte_ranges() {
        let source = "<p tal:content=\"python: 1 + 2\">π</p>";
        let parsed = parse(source, Engine::Chameleon);
        assert_eq!(&source[parsed.regions[0].content.clone()], "python: 1 + 2");
    }
}
