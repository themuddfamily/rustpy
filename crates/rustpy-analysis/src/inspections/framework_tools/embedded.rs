//! Lightweight embedded-language contracts with literal source ranges. Full
//! language parsers retain syntax authority; no interpreter or browser executes.
use super::{super::Problem, documents::push, position};
use rustpy_core::{TextRange, TextSize};
use std::{collections::HashSet, path::Path};
pub(super) fn inspect(source: &str, path: &Path, out: &mut Vec<Problem>) {
    let extension = path.extension().and_then(|v| v.to_str()).unwrap_or("");
    if matches!(extension, "html" | "htm")
        && !path.components().any(|p| p.as_os_str() == "templates")
    {
        html(source, out);
    }
    if extension == "css" {
        css(source, out);
    }
    if matches!(extension, "http" | "rest") {
        http(source, path, out);
    }
    if extension == "rst" {
        rst(source, out);
    }
}
fn at(start: usize, end: usize) -> TextRange {
    TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
}
fn html(source: &str, out: &mut Vec<Problem>) {
    let void = [
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];
    let optional = [
        "html", "head", "body", "p", "li", "dt", "dd", "rt", "rp", "optgroup", "option",
        "colgroup", "thead", "tbody", "tfoot", "tr", "td", "th",
    ];
    let mut stack: Vec<(String, usize)> = Vec::new();
    let mut offset = 0;
    let mut budget = 10000;
    while offset < source.len() && budget > 0 {
        budget -= 1;
        let Some(relative) = source[offset..].find('<') else {
            break;
        };
        let start = offset + relative;
        if source[start..].starts_with("<!--") {
            offset = source[start + 4..]
                .find("-->")
                .map_or(source.len(), |n| start + 4 + n + 3);
            continue;
        }
        let mut quote = None;
        let mut end = None;
        for (i, ch) in source[start + 1..].char_indices() {
            if let Some(open) = quote {
                if ch == open {
                    quote = None;
                }
            } else if matches!(ch, '\'' | '"') {
                quote = Some(ch);
            } else if ch == '>' {
                end = Some(start + 1 + i);
                break;
            }
        }
        let Some(end) = end else {
            push(
                out,
                at(start, source.len()),
                "HtmlSyntax",
                "Unclosed HTML tag",
            );
            break;
        };
        offset = end + 1;
        let content = source[start + 1..end].trim();
        if content.starts_with(['!', '?']) {
            continue;
        }
        let closing = content.starts_with('/');
        let name = content
            .trim_start_matches('/')
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != ':')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        if closing {
            while stack
                .last()
                .is_some_and(|(tag, _)| tag != &name && optional.contains(&tag.as_str()))
            {
                stack.pop();
            }
            if stack.last().is_some_and(|(tag, _)| tag == &name) {
                stack.pop();
            } else {
                push(
                    out,
                    at(start, end + 1),
                    "HtmlSyntax",
                    format!("Closing tag </{name}> has no matching open tag"),
                );
            }
        } else if !void.contains(&name.as_str()) && !content.ends_with('/') {
            if stack.len() >= 128 {
                return;
            }
            if name == "li" && stack.last().is_some_and(|(tag, _)| tag == "li") {
                stack.pop();
            }
            stack.push((name.clone(), start));
            if matches!(name.as_str(), "script" | "style" | "textarea" | "title") {
                // Raw-text content may contain arbitrary < characters.
                let close = format!("</{name}");
                if let Some(next) = source.as_bytes()[offset..]
                    .windows(close.len())
                    .position(|bytes| bytes.eq_ignore_ascii_case(close.as_bytes()))
                {
                    offset += next;
                } else {
                    offset = source.len();
                }
            }
        }
    }
    for (tag, start) in stack
        .into_iter()
        .filter(|(tag, _)| !optional.contains(&tag.as_str()))
        .take(128)
    {
        push(
            out,
            at(start, (start + 1).min(source.len())),
            "HtmlSyntax",
            format!("Unclosed HTML tag <{tag}>"),
        );
    }
}
fn css(source: &str, out: &mut Vec<Problem>) {
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    let mut iter = source.char_indices().peekable();
    while let Some((offset, ch)) = iter.next() {
        if comment {
            if ch == '*' && iter.peek().is_some_and(|(_, ch)| *ch == '/') {
                iter.next();
                comment = false;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(open) = quote {
            if ch == open {
                quote = None;
            }
            continue;
        }
        if ch == '/' && iter.peek().is_some_and(|(_, ch)| *ch == '*') {
            iter.next();
            comment = true;
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            continue;
        }
        if matches!(ch, '{' | '(' | '[') {
            if stack.len() >= 128 {
                return;
            }
            stack.push((ch, offset));
        } else if matches!(ch, '}' | ')' | ']') {
            let expected = match ch {
                '}' => '{',
                ')' => '(',
                _ => '[',
            };
            if stack.last().is_some_and(|(open, _)| *open == expected) {
                stack.pop();
            } else {
                push(
                    out,
                    at(offset, offset + 1),
                    "CssSyntax",
                    format!("Unmatched CSS delimiter '{ch}'"),
                );
            }
        }
    }
    if quote.is_some() || comment {
        push(
            out,
            at(source.len(), source.len()),
            "CssSyntax",
            "Unclosed CSS string or comment",
        );
    }
    for (ch, offset) in stack.into_iter().take(128) {
        push(
            out,
            at(offset, offset + 1),
            "CssSyntax",
            format!("Unclosed CSS delimiter '{ch}'"),
        );
    }
}
fn http(source: &str, path: &Path, out: &mut Vec<Problem>) {
    let requests = rustpy_http::parse(source);
    let mut names = HashSet::new();
    for request in requests.into_iter().take(1024) {
        let range = position(source, request.line);
        if let Some(name) = request.name {
            if !names.insert(name.clone()) {
                push(
                    out,
                    range,
                    "HttpClient",
                    format!("Duplicate request name '{name}'"),
                );
            }
        }
        if !request.url.starts_with("http://")
            && !request.url.starts_with("https://")
            && !request.url.contains("{{")
        {
            push(
                out,
                range,
                "HttpClient",
                "HTTP requests require an http or https URL",
            );
        }
        if let Some(version) = request.version {
            if !matches!(
                version.as_str(),
                "HTTP/1.0" | "HTTP/1.1" | "HTTP/2" | "HTTP/2.0" | "HTTP/3"
            ) {
                push(
                    out,
                    range,
                    "HttpClient",
                    format!("Unsupported HTTP version '{version}'"),
                );
            }
        }
        let mut headers = HashSet::new();
        for (name, value) in request.headers {
            let lower = name.to_ascii_lowercase();
            if matches!(lower.as_str(), "host" | "content-length" | "content-type")
                && !headers.insert(lower.clone())
            {
                push(out, range, "HttpClient", format!("Duplicate {name} header"));
            }
            if lower == "content-length" && !value.contains("{{") {
                if let Ok(length) = value.parse::<usize>() {
                    if let Some(body) = &request.body {
                        if !body.contains("{{")
                            && request.body_file.is_none()
                            && body.len() != length
                        {
                            push(
                                out,
                                range,
                                "HttpClient",
                                "Content-Length differs from the literal UTF-8 body length",
                            );
                        }
                    }
                }
            }
        }
        if let Some(body) = request.body_file {
            if !path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&body)
                .is_file()
            {
                push(
                    out,
                    range,
                    "HttpClient",
                    format!("Unresolved request body file '{}'", body.display()),
                );
            }
        }
    }
}
fn rst(source: &str, out: &mut Vec<Problem>) {
    let doc = rustpy_syntax::rst::parse(source);
    let mut references = HashSet::new();
    for block in &doc.blocks {
        if let rustpy_syntax::rst::Kind::Heading { title, .. } = &block.kind {
            references.insert(source[title.clone()].trim().to_string());
        }
    }
    if source
        .lines()
        .any(|line| line.trim_start().starts_with(".. include::"))
    {
        return;
    }
    for line in source.lines() {
        let text = line.trim();
        if let Some(rest) = text.strip_prefix(".. _") {
            if let Some((name, _)) = rest.split_once(':') {
                references.insert(name.trim().to_string());
            }
        }
    }
    for block in doc.blocks.into_iter().take(10000) {
        if let rustpy_syntax::rst::Kind::Directive { name, .. } = block.kind {
            let name = &source[name];
            // Extensions own their namespaces; diagnose only standard directive
            // shape errors in the maintained parser, not arbitrary plugin names.
            if name.is_empty() {
                push(
                    out,
                    at(block.range.start, block.range.end),
                    "RstInspection",
                    "Directive name is empty",
                );
            }
        }
    }
    // Only explicit local backtick references admit a closed document lookup;
    // Sphinx :ref:/roles can resolve across documents and are not guessed here.
    for (i, line) in source.lines().enumerate() {
        let mut rest = line;
        while let Some(start) = rest.find('`') {
            let after = &rest[start + 1..];
            let Some(end) = after.find("`_") else {
                break;
            };
            let name = &after[..end];
            if after[end + 2..].starts_with('_') {
                rest = &after[end + 3..];
                continue;
            }
            if !name.contains(['<', '>']) && !references.contains(name) {
                push(
                    out,
                    position(source, i + 1),
                    "RstInspection",
                    format!("Unresolved local reference '{name}'"),
                );
            }
            rest = &after[end + 2..];
        }
    }
}
