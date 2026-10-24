//! Shared source-aware Markdown operations used by preview, inspections and editing.
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use rustpy_settings::markdown::Settings;
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    path::{Path, PathBuf},
};
pub fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_HEADING_ATTRIBUTES
}
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
pub fn slug(text: &str) -> String {
    text.chars()
        .filter_map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                Some(c.to_lowercase().next().unwrap_or(c))
            } else if c.is_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .collect()
}
#[derive(Clone, Debug)]
pub struct Heading {
    pub text: String,
    pub id: String,
    pub level: usize,
    pub range: Range<usize>,
}
pub fn headings(source: &str) -> Vec<Heading> {
    let mut rows = Vec::new();
    let mut active = None;
    let mut counts = BTreeMap::<String, usize>::new();
    for (event, range) in Parser::new_ext(source, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, id, .. }) => {
                active = Some(Heading {
                    text: String::new(),
                    id: id.map(|s| s.into_string()).unwrap_or_default(),
                    level: level as usize,
                    range,
                })
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(h) = &mut active {
                    h.text.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(mut h) = active.take() {
                    if h.id.is_empty() {
                        let base = slug(&h.text);
                        let count = counts.entry(base.clone()).or_default();
                        h.id = if *count == 0 {
                            base
                        } else {
                            format!("{base}-{count}")
                        };
                        *count += 1;
                    }
                    rows.push(h);
                }
            }
            _ => {}
        }
    }
    rows
}
pub fn toc(source: &str) -> String {
    headings(source)
        .iter()
        .map(|h| {
            format!(
                "{}- [{}](#{})\n",
                "  ".repeat(h.level.saturating_sub(1)),
                h.text,
                h.id
            )
        })
        .collect()
}
pub fn safe_url(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    !url.chars().any(|c| c.is_control())
        && (!url.contains(':')
            || ["https://", "http://", "mailto:", "file://"]
                .iter()
                .any(|p| url.starts_with(p)))
}
pub fn local_link(base: &Path, dest: &str) -> Option<(PathBuf, Option<String>)> {
    if let Some(local) = dest.strip_prefix("file://") {
        let (path, anchor) = local
            .split_once('#')
            .map_or((local, None), |(p, a)| (p, Some(a.into())));
        let path = percent_decode(path);
        let path =
            if cfg!(windows) && path.starts_with('/') && path.as_bytes().get(2) == Some(&b':') {
                &path[1..]
            } else {
                &path
            };
        return Some((PathBuf::from(path), anchor));
    }
    if dest.contains("://") || dest.starts_with("mailto:") {
        return None;
    }
    let (file, anchor) = dest
        .split_once('#')
        .map_or((dest, None), |(a, b)| (a, Some(b.to_string())));
    let file = percent_decode(file);
    Some((
        if file.is_empty() {
            base.to_path_buf()
        } else {
            base.parent().unwrap_or(Path::new(".")).join(file)
        },
        anchor,
    ))
}
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if i + 2 < bytes.len() && bytes[i] == b'%' {
            let high = (bytes[i + 1] as char).to_digit(16);
            let low = (bytes[i + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
#[derive(Clone, Debug)]
pub struct Finding {
    pub range: Range<usize>,
    pub message: String,
}
pub fn inspect(source: &str, file: Option<&Path>) -> Vec<Finding> {
    let ids = headings(source)
        .into_iter()
        .map(|h| h.id)
        .collect::<BTreeSet<_>>();
    let mut rows = Vec::new();
    for (event, range) in Parser::new_ext(source, options()).into_offset_iter() {
        if let Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) = event {
            if !safe_url(&dest_url) {
                rows.push(Finding {
                    range,
                    message: "Unsafe link scheme".into(),
                });
                continue;
            }
            if let Some(anchor) = dest_url.strip_prefix('#') {
                if !ids.contains(anchor) {
                    rows.push(Finding {
                        range,
                        message: format!("Unresolved header #{anchor}"),
                    });
                }
            } else if let Some(file) = file {
                if let Some((path, anchor)) = local_link(file, &dest_url) {
                    if !path.exists() {
                        rows.push(Finding {
                            range,
                            message: format!("Unresolved file {}", path.display()),
                        });
                    } else if let Some(anchor) = anchor {
                        if let Ok(target) = std::fs::read_to_string(&path) {
                            if !headings(&target).iter().any(|h| h.id == anchor) {
                                rows.push(Finding {
                                    range,
                                    message: format!(
                                        "Unresolved header #{anchor} in {}",
                                        path.display()
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    // Broken reference links are reported by the parser callback rather than
    // guessed with a regex, so code spans/fences do not produce false positives.
    let mut broken = Vec::new();
    let mut callback = |link: pulldown_cmark::BrokenLink<'_>| {
        broken.push(Finding {
            range: link.span,
            message: format!("Unresolved reference link [{}]", link.reference),
        });
        None
    };
    for _ in Parser::new_with_broken_link_callback(source, options(), Some(&mut callback)) {}
    rows.extend(broken);
    for table in tables(source) {
        let width = table.rows.first().map_or(0, Vec::len);
        if table.rows.iter().any(|r| r.len() != width) {
            rows.push(Finding {
                range: table.range.clone(),
                message: "Incorrect table column count".into(),
            });
        }
        if source.get(table.range.clone()) != Some(table.format().as_str()) {
            rows.push(Finding {
                range: table.range,
                message: "Incorrect table formatting; use Reformat Markdown or Format table".into(),
            });
        }
    }
    rows
}
pub const FENCE_LANGUAGES: &[&str] = &[
    "python",
    "javascript",
    "typescript",
    "json",
    "yaml",
    "toml",
    "html",
    "css",
    "bash",
    "sql",
    "rust",
    "mermaid",
    "plantuml",
    "text",
];
pub fn completion_prefix(source: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(source.len());
    let offset = rustpy_core::floor_char_boundary(source, offset);
    let line = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let prefix = &source[line..offset];
    if let Some(index) = prefix.rfind("](") {
        return line + index + 2..offset;
    }
    if let Some(index) = prefix.rfind("```") {
        return line + index + 3..offset;
    }
    offset..offset
}
pub fn file_url(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let mut result: String = if path.starts_with('/') {
        "file://".into()
    } else {
        "file:///".into()
    };
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            result.push(byte as char);
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}
pub fn completions(source: &str, file: Option<&Path>, prefix: &str) -> Vec<String> {
    let mut result = headings(source)
        .into_iter()
        .map(|h| format!("#{}", h.id))
        .collect::<Vec<_>>();
    if let Some(parent) = file.and_then(Path::parent) {
        if let Ok(entries) = std::fs::read_dir(parent) {
            result.extend(entries.flatten().take(1000).map(|e| {
                format!(
                    "{}{}",
                    e.file_name().to_string_lossy(),
                    if e.path().is_dir() { "/" } else { "" }
                )
            }));
        }
    }
    result.retain(|s| {
        s.to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    });
    result.sort();
    result.dedup();
    result
}
#[derive(Clone, Debug)]
pub struct Table {
    pub range: Range<usize>,
    pub rows: Vec<Vec<String>>,
}
fn pipes(line: &str) -> Vec<usize> {
    let bytes = line.as_bytes();
    let mut positions = Vec::new();
    let mut i = 0;
    let mut ticks = 0;
    let mut escaped = false;
    while i < bytes.len() {
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        match bytes[i] {
            b'\\' => {
                escaped = true;
                i += 1;
            }
            b'`' => {
                let start = i;
                while i < bytes.len() && bytes[i] == b'`' {
                    i += 1;
                }
                let run = i - start;
                if ticks == 0 {
                    ticks = run;
                } else if ticks == run {
                    ticks = 0;
                }
            }
            b'|' if ticks == 0 => {
                positions.push(i);
                i += 1;
            }
            _ => i += 1,
        }
    }
    positions
}
fn cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let boundaries = pipes(line);
    let start = if boundaries.first() == Some(&0) { 1 } else { 0 };
    let end = if boundaries.last().copied() == line.len().checked_sub(1) {
        line.len().saturating_sub(1)
    } else {
        line.len()
    };
    if start > end {
        return vec![String::new()];
    }
    let body = &line[start..end];
    let mut cells = Vec::new();
    let mut previous = 0;
    for index in pipes(body) {
        cells.push(body[previous..index].trim().into());
        previous = index + 1;
    }
    cells.push(body[previous..].trim().into());
    cells
}
fn divider(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|s| {
            let t = s.trim_matches(':');
            t.len() >= 3 && t.chars().all(|c| c == '-')
        })
}
pub fn tables(source: &str) -> Vec<Table> {
    let mut rows = Vec::new();
    let mut offset = 0;
    let lines = source
        .split_inclusive('\n')
        .map(|line| {
            let start = offset;
            offset += line.len();
            (start, line)
        })
        .collect::<Vec<_>>();
    let mut i = 0;
    let fenced = Parser::new_ext(source, options())
        .into_offset_iter()
        .filter_map(|(event, range)| {
            matches!(event, Event::Start(Tag::CodeBlock(_))).then_some(range)
        })
        .collect::<Vec<_>>();
    while i + 1 < lines.len() {
        let (start, line) = lines[i];
        if line.contains('|')
            && divider(&cells(lines[i + 1].1))
            && !fenced.iter().any(|r| r.contains(&start))
        {
            let mut contents = vec![cells(line), cells(lines[i + 1].1)];
            let mut end = lines[i + 1].0 + lines[i + 1].1.len();
            i += 2;
            while i < lines.len() && lines[i].1.contains('|') && !lines[i].1.trim().is_empty() {
                contents.push(cells(lines[i].1));
                end = lines[i].0 + lines[i].1.len();
                i += 1;
            }
            rows.push(Table {
                range: start..end,
                rows: contents,
            });
        } else {
            i += 1;
        }
    }
    rows
}
impl Table {
    pub fn format(&self) -> String {
        let width = self.rows.iter().map(Vec::len).max().unwrap_or(1);
        let widths = (0..width)
            .map(|c| {
                self.rows
                    .iter()
                    .enumerate()
                    .filter(|(r, _)| *r != 1)
                    .map(|(_, r)| r.get(c).map_or(0, |s| s.chars().count()))
                    .max()
                    .unwrap_or(0)
                    .max(3)
            })
            .collect::<Vec<_>>();
        let mut out = String::new();
        for (row, cells) in self.rows.iter().enumerate() {
            out.push('|');
            for (column, w) in widths.iter().enumerate() {
                let value = cells.get(column).map(String::as_str).unwrap_or("");
                let value = if row == 1 {
                    let left = value.starts_with(':');
                    let right = value.ends_with(':');
                    format!(
                        "{}{}{}",
                        if left { ":" } else { "" },
                        "-".repeat(
                            w.saturating_sub(usize::from(left) + usize::from(right))
                                .max(3)
                        ),
                        if right { ":" } else { "" }
                    )
                } else {
                    value.into()
                };
                out.push(' ');
                out.push_str(&value);
                out.push_str(&" ".repeat(w.saturating_sub(value.chars().count()) + 1));
                out.push('|');
            }
            out.push('\n');
        }
        out
    }
    pub fn add_row(&mut self) {
        self.rows
            .push(vec![String::new(); self.rows.first().map_or(1, Vec::len)]);
    }
    pub fn add_column(&mut self) {
        for (i, row) in self.rows.iter_mut().enumerate() {
            row.push(if i == 1 { "---".into() } else { String::new() });
        }
    }
}
pub fn format_tables(source: &str) -> Vec<(Range<usize>, String)> {
    tables(source)
        .into_iter()
        .filter_map(|t| {
            let formatted = t.format();
            (source.get(t.range.clone()) != Some(formatted.as_str()))
                .then_some((t.range, formatted))
        })
        .collect()
}
pub fn next_table_cell(source: &str, offset: usize, backward: bool) -> Option<usize> {
    let table = tables(source)
        .into_iter()
        .find(|t| t.range.start <= offset && offset <= t.range.end)?;
    let mut positions = Vec::new();
    let mut start = table.range.start;
    for (row, line) in source[table.range.clone()]
        .split_inclusive('\n')
        .enumerate()
    {
        if row != 1 {
            if !line.trim_start().starts_with('|') {
                positions.push(start + line.len() - line.trim_start().len());
            }
            for i in pipes(line) {
                if i + 1 < line.trim_end().len() {
                    let rest = &line[i + 1..];
                    positions.push(start + i + 1 + rest.len() - rest.trim_start().len());
                }
            }
        }
        start += line.len();
    }
    if backward {
        positions.into_iter().rev().find(|p| *p < offset)
    } else {
        positions.into_iter().find(|p| *p > offset)
    }
}
pub fn autolinks(text: &str) -> Vec<(Range<usize>, String)> {
    let mut links = Vec::new();
    let mut offset = 0;
    for word in text.split_inclusive(char::is_whitespace) {
        let token = word.trim_end();
        let start = token
            .find("https://")
            .or_else(|| token.find("http://"))
            .or_else(|| token.find("www."));
        if let Some(start) = start {
            if start == 0
                || token[..start]
                    .chars()
                    .last()
                    .is_some_and(|c| !c.is_alphanumeric())
            {
                let mut end = token.len();
                while end > start {
                    let c = token[..end].chars().last().unwrap();
                    let unbalanced = matches!(c, ')' | ']')
                        && token[start..end].chars().filter(|x| *x == c).count()
                            > token[start..end]
                                .chars()
                                .filter(|x| *x == if c == ')' { '(' } else { '[' })
                                .count();
                    if matches!(c, '.' | ',' | ';' | '!' | '?' | '"' | '\'') || unbalanced {
                        end -= c.len_utf8();
                    } else {
                        break;
                    }
                }
                let value = &token[start..end];
                let url = if value.starts_with("www.") {
                    format!("https://{value}")
                } else {
                    value.into()
                };
                links.push((offset + start..offset + end, url));
            }
        }
        offset += word.len();
    }
    links
}
pub fn html_document(
    source: &str,
    file: Option<&Path>,
    settings: &Settings,
    enhanced: bool,
) -> String {
    let headings = headings(source);
    let mut heading_index = 0;
    let mut diagram = None::<String>;
    let mut diagram_text = String::new();
    let mut events = Vec::new();
    let mut in_code = false;
    let mut in_link = false;
    for event in Parser::new_ext(source, options()) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let id = headings
                    .get(heading_index)
                    .map(|h| h.id.as_str())
                    .unwrap_or("");
                heading_index += 1;
                events.push(Event::Html(
                    format!("<h{} id=\"{}\">", level as usize, escape(id)).into(),
                ));
            }
            Event::End(TagEnd::Heading(level)) => {
                events.push(Event::Html(format!("</h{}>", level as usize).into()))
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if enhanced
                    && ((info.as_ref() == "mermaid" && settings.mermaid)
                        || (info.as_ref() == "plantuml" && settings.plantuml)) =>
            {
                diagram = Some(info.into_string());
                diagram_text.clear();
            }
            Event::Text(text) if diagram.is_some() => diagram_text.push_str(&text),
            Event::End(TagEnd::CodeBlock) if diagram.is_some() => {
                let kind = diagram.take().unwrap();
                let html = if kind == "mermaid" {
                    format!("<pre class=\"mermaid\">{}</pre>", escape(&diagram_text))
                } else {
                    let hex = diagram_text
                        .as_bytes()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>();
                    format!(
                        "<img alt=\"PlantUML diagram\" src=\"{}/~h{}\">",
                        escape(settings.plantuml_server.trim_end_matches('/')),
                        hex
                    )
                };
                events.push(Event::Html(html.into()));
            }
            Event::InlineMath(text) => events.push(Event::Html(
                format!(
                    "<span class=\"math\" data-display=\"false\">{}</span>",
                    escape(&text)
                )
                .into(),
            )),
            Event::DisplayMath(text) => events.push(Event::Html(
                format!(
                    "<div class=\"math\" data-display=\"true\">{}</div>",
                    escape(&text)
                )
                .into(),
            )),
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code = true;
                events.push(Event::Start(Tag::CodeBlock(kind)));
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                events.push(Event::End(TagEnd::CodeBlock));
            }
            Event::End(TagEnd::Link) => {
                in_link = false;
                events.push(Event::End(TagEnd::Link));
            }
            Event::Text(text) if !in_code && !in_link => {
                let mut cursor = 0;
                for (span, url) in autolinks(&text) {
                    if span.start > cursor {
                        events.push(Event::Text(text[cursor..span.start].to_string().into()));
                    }
                    events.push(Event::Start(Tag::Link {
                        link_type: pulldown_cmark::LinkType::Autolink,
                        dest_url: url.into(),
                        title: "".into(),
                        id: "".into(),
                    }));
                    events.push(Event::Text(text[span.clone()].to_string().into()));
                    events.push(Event::End(TagEnd::Link));
                    cursor = span.end;
                }
                if cursor < text.len() {
                    events.push(Event::Text(text[cursor..].to_string().into()));
                }
            }
            Event::Html(_) | Event::InlineHtml(_) => {} // Source HTML never injects active script into exported documents.
            Event::Start(Tag::Link {
                dest_url,
                title,
                id,
                link_type,
            }) => {
                in_link = true;
                let url = if safe_url(&dest_url) {
                    dest_url
                } else {
                    "#".into()
                };
                events.push(Event::Start(Tag::Link {
                    dest_url: url,
                    title,
                    id,
                    link_type,
                }));
            }
            Event::Start(Tag::Image {
                dest_url,
                title,
                id,
                link_type,
            }) => {
                let remote = dest_url.starts_with("http://") || dest_url.starts_with("https://");
                let url = if safe_url(&dest_url) && (!remote || settings.remote_images) {
                    dest_url
                } else {
                    "".into()
                };
                events.push(Event::Start(Tag::Image {
                    dest_url: url,
                    title,
                    id,
                    link_type,
                }));
            }
            other => events.push(other),
        }
    }
    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, events.into_iter());
    let base = file
        .and_then(Path::parent)
        .map(|path| format!("<base href=\"{}/\">", escape(&file_url(path))))
        .unwrap_or_default();
    let css = if settings.dark {
        "body{background:#1e1f22;color:#ddd}a{color:#80bfff}"
    } else {
        "body{background:white;color:#222}a{color:#075bb5}"
    };
    let custom = settings.custom_css.replace("</style", "<\\/style");
    let mut html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">{base}<style>body{{font: {}px system-ui;max-width:980px;margin:2em auto;padding:1em;line-height:1.6}}pre,code{{font-family:monospace}}pre{{overflow:auto;padding:1em;background:#8882}}table{{border-collapse:collapse}}td,th{{border:1px solid #8886;padding:.4em}}img{{max-width:100%}}{css}\n{custom}</style></head><body>{body}",
        settings.font_size.clamp(8., 48.)
    );
    if enhanced && settings.math {
        let assets = escape(settings.katex_assets.trim_end_matches('/'));
        html.push_str(&format!("<link rel=\"stylesheet\" href=\"{assets}/katex.min.css\"><script src=\"{assets}/katex.min.js\"></script><script>for(const e of document.querySelectorAll('.math')){{try{{katex.render(e.textContent,e,{{throwOnError:false,displayMode:e.dataset.display==='true',trust:false}})}}catch(err){{e.textContent=String(err)}}}}</script>"));
    }
    if enhanced && settings.mermaid {
        let script = serde_json::to_string(&settings.mermaid_script)
            .unwrap_or_default()
            .replace('<', "\\u003c");
        html.push_str(&format!("<script type=\"module\">import mermaid from {script};mermaid.initialize({{startOnLoad:true,securityLevel:'strict',theme:'{}'}});</script>",if settings.dark{"dark"}else{"default"}));
    }
    html.push_str("</body></html>");
    html
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unique_heading_ids_and_toc_agree() {
        let source = "# Hello *world*\n## Hello world\n";
        let h = headings(source);
        assert_eq!(h[0].id, "hello-world");
        assert_eq!(h[1].id, "hello-world-1");
        assert!(toc(source).contains("(#hello-world-1)"));
    }
    #[test]
    fn tables_preserve_escaped_pipes_and_alignment() {
        let source = "| A | B |\n| :--- | ---: |\n| a\\|b | `x|y` |\n";
        let t = tables(source).remove(0);
        assert_eq!(t.rows[2].len(), 2);
        let formatted = t.format();
        assert!(formatted.contains("a\\|b"));
        assert!(formatted.contains("`x|y`"));
        assert!(next_table_cell(source, 1, false).is_some());
    }
    #[test]
    fn html_keeps_gfm_and_escapes_active_source() {
        let settings = Settings::default();
        let html = html_document(
            "# A\n\n~~old~~\n\n- [x] done\n\n<script>alert(1)</script>\n\n[bad](javascript:evil)\n\n$x^2$",
            None,
            &settings,
            true,
        );
        assert!(html.contains("id=\"a\""));
        assert!(html.contains("<del>old</del>"));
        assert!(!html.contains("alert(1)"));
        assert!(!html.contains("javascript:evil"));
        assert!(html.contains("katex.render"));
    }
    #[test]
    fn unresolved_reference_and_header_are_reported_outside_fences() {
        let findings = inspect(
            "[missing][nope]\n[header](#absent)\n\n```text\n[ignored][x]\n```",
            None,
        );
        assert!(findings.iter().any(|f| f.message.contains("nope")));
        assert!(findings.iter().any(|f| f.message.contains("absent")));
        assert!(!findings.iter().any(|f| f.message.contains("ignored")));
    }
    #[test]
    fn gfm_autolinks_preserve_balanced_parentheses_and_trim_sentence_punctuation() {
        let links = autolinks("See https://example.org/a_(b). and www.example.org!");
        assert_eq!(links[0].1, "https://example.org/a_(b)");
        assert_eq!(links[1].1, "https://www.example.org");
        let html = html_document("https://example.org", None, &Settings::default(), false);
        assert!(html.contains("href=\"https://example.org\""));
    }
    #[test]
    fn completion_replaces_only_the_link_or_fence_prefix() {
        let source = "[label](doc";
        assert_eq!(&source[completion_prefix(source, source.len())], "doc");
        let source = "```py";
        assert_eq!(&source[completion_prefix(source, source.len())], "py");
    }
    #[test]
    fn file_urls_quote_fragments_spaces_and_unicode() {
        let url = file_url(Path::new("/project with space/#notes/é.png"));
        assert!(url.contains("%20"));
        assert!(url.contains("%23notes"));
        let (path, _) = local_link(Path::new("/unused.md"), &url).unwrap();
        assert_eq!(path, PathBuf::from("/project with space/#notes/é.png"));
    }
}
