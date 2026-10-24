//! Source-preserving template links shared by navigation and inspections.
use rustpy_core::{TextRange, TextSize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug)]
pub struct Link {
    pub kind: String,
    pub name: String,
    pub range: TextRange,
}
pub fn links(source: &str) -> Vec<Link> {
    if source.len() > 2 * 1024 * 1024 {
        return Vec::new();
    }
    let doc = rustpy_syntax::template::parse(source, rustpy_syntax::template::Dialect::Jinja);
    let mut out = Vec::new();
    for region in doc
        .regions
        .iter()
        .filter(|region| region.kind == rustpy_syntax::template::Kind::Statement)
        .take(8192)
    {
        let body = &source[region.content.clone()];
        let kind = body.split_whitespace().next().unwrap_or("");
        if ![
            "extends", "include", "import", "from", "static", "block", "endblock", "macro",
        ]
        .contains(&kind)
        {
            continue;
        }
        let offset = region.content.start;
        if matches!(kind, "block" | "endblock" | "macro") {
            if let Some(name) = body.split_whitespace().nth(1) {
                let name = name.split('(').next().unwrap_or(name);
                if let Some(start) = body.find(name) {
                    out.push(Link {
                        kind: kind.into(),
                        name: name.into(),
                        range: TextRange::new(
                            TextSize::new((offset + start) as u32),
                            TextSize::new((offset + start + name.len()) as u32),
                        ),
                    });
                }
            }
        } else {
            let Some(start) = body.find(['\'', '"']) else {
                continue;
            };
            let delimiter = body.as_bytes()[start] as char;
            let Some(end) = body[start + 1..].find(delimiter) else {
                continue;
            };
            let name = &body[start + 1..start + 1 + end];
            if safe(name) {
                out.push(Link {
                    kind: kind.into(),
                    name: name.into(),
                    range: TextRange::new(
                        TextSize::new((offset + start + 1) as u32),
                        TextSize::new((offset + start + 1 + end) as u32),
                    ),
                });
            }
        }
    }
    out
}
/// `{% url 'name' %}` and `url_for('endpoint')` arguments, kept apart from
/// [`links`] because they name routes rather than template/static files.
pub fn url_links(source: &str) -> Vec<Link> {
    if source.len() > 2 * 1024 * 1024 {
        return Vec::new();
    }
    let doc = rustpy_syntax::template::parse(source, rustpy_syntax::template::Dialect::Jinja);
    let mut out = Vec::new();
    for region in doc
        .regions
        .iter()
        .filter(|region| {
            matches!(
                region.kind,
                rustpy_syntax::template::Kind::Statement
                    | rustpy_syntax::template::Kind::Expression
            )
        })
        .take(8192)
    {
        let body = &source[region.content.clone()];
        let offset = region.content.start;
        let quoted = |at: usize| -> Option<(usize, usize)> {
            let delimiter = *body.as_bytes().get(at)?;
            if !matches!(delimiter, b'\'' | b'"') {
                return None;
            }
            let end = body[at + 1..].find(delimiter as char)?;
            Some((at + 1, at + 1 + end))
        };
        let mut push = |kind: &str, (start, end): (usize, usize)| {
            if start < end {
                out.push(Link {
                    kind: kind.into(),
                    name: body[start..end].into(),
                    range: TextRange::new(
                        TextSize::new((offset + start) as u32),
                        TextSize::new((offset + end) as u32),
                    ),
                });
            }
        };
        if region.kind == rustpy_syntax::template::Kind::Statement {
            let trimmed = body.trim_start_matches(['-', '+']).trim_start();
            if let Some(rest) = trimmed
                .strip_prefix("url")
                .filter(|rest| rest.starts_with(char::is_whitespace))
            {
                let at = body.len() - rest.trim_start().len();
                if let Some(range) = quoted(at) {
                    push("url", range);
                }
            }
        }
        let mut from = 0;
        while let Some(found) = body[from..].find("url_for") {
            let at = from + found;
            from = at + "url_for".len();
            if body[..at]
                .chars()
                .next_back()
                .is_some_and(rustpy_core::is_ident_continue)
            {
                continue;
            }
            let rest = &body[from..];
            let Some(rest) = rest.trim_start().strip_prefix('(') else {
                continue;
            };
            let quote = body.len() - rest.trim_start().len();
            if let Some(range) = quoted(quote) {
                push("url_for", range);
            }
        }
    }
    out
}
/// What a template string or `{% load %}` argument at the caret names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// `{% url '…' %}`: a Django URL pattern name.
    UrlName,
    /// `{% load … %}`: a registered Django template library (the word after
    /// `from`, or every word of a `{% load %}` without `from`).
    Library,
    /// `{% load name from library %}`: a tag or filter of the library named
    /// after `from` (see [`load_from_library`]).
    LoadedName,
    /// `{% extends/include/import/from '…' %}`: a template path.
    TemplateName,
    /// `url_for('…')`: a Flask endpoint.
    Endpoint,
}
/// The argument slot at byte `offset` and the offset its typed prefix starts at.
pub fn slot(source: &str, offset: usize) -> Option<(Slot, usize)> {
    let before = source.get(..offset)?;
    let (open, statement) = match (before.rfind("{{"), before.rfind("{%")) {
        (Some(a), Some(b)) => (a.max(b), b > a),
        (Some(a), None) => (a, false),
        (None, Some(b)) => (b, true),
        _ => return None,
    };
    let body_start = open + 2;
    let context = &before[body_start..];
    if context.contains("%}") || context.contains("}}") {
        return None;
    }
    let trimmed = context.trim_start_matches(['-', '+']).trim_start();
    let tag = if statement {
        trimmed.split_whitespace().next().unwrap_or("")
    } else {
        ""
    };
    let mut quote: Option<(char, usize)> = None;
    let mut first = None;
    for (at, ch) in context.char_indices() {
        match quote {
            Some((open, _)) if ch == open => quote = None,
            Some(_) => {}
            None if matches!(ch, '\'' | '"') => {
                quote = Some((ch, at));
                first.get_or_insert(at);
            }
            None => {}
        }
    }
    if let Some((_, at)) = quote {
        let start = body_start + at + 1;
        let head = context[..at].trim_end();
        if let Some(callee) = head.strip_suffix('(').map(str::trim_end) {
            if callee.ends_with("url_for")
                && !callee[..callee.len() - "url_for".len()]
                    .chars()
                    .next_back()
                    .is_some_and(rustpy_core::is_ident_continue)
            {
                return Some((Slot::Endpoint, start));
            }
        }
        if statement && first == Some(at) {
            return match tag {
                "url" => Some((Slot::UrlName, start)),
                "extends" | "include" | "import" | "from" => Some((Slot::TemplateName, start)),
                _ => None,
            };
        }
        return None;
    }
    if tag == "load" && trimmed["load".len()..].starts_with(char::is_whitespace) {
        let start = before
            .rfind(char::is_whitespace)
            .map_or(offset, |at| at + 1);
        let (earlier, later) = load_words(source, offset)?;
        let slot = if earlier.contains(&"from") || !later.contains(&"from") {
            Slot::Library
        } else {
            Slot::LoadedName
        };
        return Some((slot, start));
    }
    None
}
/// Words of the `{% load %}` tag around `offset`: complete words before the
/// caret's word (after `load`) and the words after it, up to the tag's end.
fn load_words(source: &str, offset: usize) -> Option<(Vec<&str>, Vec<&str>)> {
    let before = source.get(..offset)?;
    let open = before.rfind("{%")?;
    let head = before[open + 2..]
        .trim_start_matches(['-', '+'])
        .trim_start();
    let head = head.strip_prefix("load")?;
    let word = head
        .rfind(char::is_whitespace)
        .map_or(head.len(), |at| at + 1);
    let earlier = head[..word].split_whitespace().collect();
    let after = &source[offset..];
    let end = [after.find("%}"), after.find("{%"), after.find("{{")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(after.len());
    let after = after[..end].trim_end_matches(['-', '+']);
    // The rest of the caret's word is not a separate word.
    let after = after.trim_start_matches(|ch: char| !ch.is_whitespace());
    Some((earlier, after.split_whitespace().collect()))
}
/// The library named after `from` in the `{% load … from library %}` tag at
/// `offset`, when the caret is on an imported name before `from`.
pub fn load_from_library(source: &str, offset: usize) -> Option<String> {
    let (earlier, later) = load_words(source, offset)?;
    if earlier.contains(&"from") {
        return None;
    }
    let at = later.iter().position(|word| *word == "from")?;
    later.get(at + 1).map(|library| (*library).to_owned())
}
pub fn safe(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\0', '\\', ':'])
        && Path::new(name)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}
pub fn target<'a>(
    link: &Link,
    paths: &'a [PathBuf],
    directories: &[PathBuf],
) -> Option<&'a PathBuf> {
    if !safe(&link.name) {
        return None;
    }
    paths
        .iter()
        .filter(|path| {
            directories
                .iter()
                .any(|directory| path.starts_with(directory))
        })
        .find(|path| path.ends_with(Path::new(&link.name)))
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn links_keep_unicode_ranges_and_refuse_traversal() {
        let source = "π {% extends 'base.html' %}{% block content %}{% include '../secret' %}";
        let links = links(source);
        assert_eq!(links.len(), 2);
        assert_eq!(
            &source[usize::from(links[0].range.start())..usize::from(links[0].range.end())],
            "base.html"
        );
    }
}
#[cfg(test)]
mod catalog_slots {
    use super::*;
    #[test]
    fn url_links_cover_django_url_tags_and_jinja_url_for_calls() {
        let source = "{% url 'blog:detail' pk=1 %} {{ url_for( \"auth.login\", next=1) }} {% set x = my_url_for('no') %}";
        let links = url_links(source);
        let names: Vec<_> = links
            .iter()
            .map(|link| (link.kind.as_str(), link.name.as_str()))
            .collect();
        assert_eq!(names, [("url", "blog:detail"), ("url_for", "auth.login")]);
        for link in &links {
            assert_eq!(
                &source[usize::from(link.range.start())..usize::from(link.range.end())],
                link.name
            );
        }
        // File links stay unchanged so missing-template inspections ignore URLs.
        assert!(links_kinds(source).is_empty());
    }
    fn links_kinds(source: &str) -> Vec<String> {
        links(source).into_iter().map(|link| link.kind).collect()
    }
    #[test]
    fn slots_classify_url_load_template_and_endpoint_arguments() {
        let cases = [
            ("{% url 'bl", Some((Slot::UrlName, 8))),
            ("{% url 'a' 'b", None),
            ("{% load hum", Some((Slot::Library, 8))),
            ("{% load static i18", Some((Slot::Library, 15))),
            ("{% load", None),
            ("{% load a b from hum", Some((Slot::Library, 17))),
            ("{% load a from ", Some((Slot::Library, 15))),
            ("{% extends \"lay", Some((Slot::TemplateName, 12))),
            ("{%- include 'parts/", Some((Slot::TemplateName, 13))),
            ("{% from 'mac", Some((Slot::TemplateName, 9))),
            ("{{ url_for('ind", Some((Slot::Endpoint, 12))),
            ("{% set u = url_for(\"a", Some((Slot::Endpoint, 20))),
            ("{{ my_url_for('x", None),
            ("{% url 'done' %} plain '", None),
            ("{{ title", None),
        ];
        for (source, expected) in cases {
            assert_eq!(slot(source, source.len()), expected, "{source}");
        }
    }
    #[test]
    fn load_names_before_from_belong_to_the_named_library() {
        let at = |source: &str| source.find('|').unwrap();
        let cases = [
            // Names before `from` are that library's tags/filters.
            (
                "{% load a sh| from custom %}",
                Some((Slot::LoadedName, 10)),
                Some("custom"),
            ),
            (
                "{% load |show_tree from custom %}",
                Some((Slot::LoadedName, 8)),
                Some("custom"),
            ),
            // The library is not typed yet: still an imported name, library unknown.
            ("{% load sh| from %}", Some((Slot::LoadedName, 8)), None),
            // After `from`, and in a `{% load %}` without `from`: libraries.
            ("{% load a from cu| %}", Some((Slot::Library, 15)), None),
            ("{% load hum| static %}", Some((Slot::Library, 8)), None),
            (
                "{% load hum|%}{% load x from y %}",
                Some((Slot::Library, 8)),
                None,
            ),
        ];
        for (marked, slot_expected, library) in cases {
            let offset = at(marked);
            let source = marked.replace('|', "");
            assert_eq!(slot(&source, offset), slot_expected, "{marked}");
            assert_eq!(
                load_from_library(&source, offset).as_deref(),
                library,
                "{marked}"
            );
        }
    }
}
