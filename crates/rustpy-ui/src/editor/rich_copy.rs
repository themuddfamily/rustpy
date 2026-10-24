//! Edit › Copy as Rich Text: the selection as HTML with the editor's syntax
//! colors (pastes with highlighting into documents, slides and mail).

use rustpy_highlight::{ColorScheme, Rgb, Span};

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// HTML for `text[start..end]` given the document's highlight `spans` (absolute offsets).
pub fn to_html(
    text: &str,
    start: usize,
    end: usize,
    spans: &[Span],
    scheme: &ColorScheme,
    font: &str,
    font_size: f32,
) -> String {
    let (start, end) = (start.min(text.len()), end.min(text.len()));
    let bg = hex(scheme.editor.background);
    let fg = hex(scheme.editor.foreground);
    let mut html = format!("<pre style=\"background-color:{bg};color:{fg};font-family:'{font}',monospace;font-size:{font_size}pt;padding:8px\">");
    let mut pos = start;
    for s in spans {
        let (a, b) = (
            usize::from(s.range.start()).max(start),
            usize::from(s.range.end()).min(end),
        );
        if b <= a {
            continue;
        }
        if a > pos {
            html.push_str(&escape(&text[pos..a]));
        }
        let st = scheme.style(s.tag);
        let mut css = String::new();
        if let Some(c) = st.fg {
            css.push_str(&format!("color:{};", hex(c)));
        }
        if let Some(c) = st.bg {
            css.push_str(&format!("background-color:{};", hex(c)));
        }
        if st.bold {
            css.push_str("font-weight:bold;");
        }
        if st.italic {
            css.push_str("font-style:italic;");
        }
        let piece = escape(&text[a.max(pos)..b]);
        if css.is_empty() {
            html.push_str(&piece);
        } else {
            html.push_str(&format!("<span style=\"{css}\">{piece}</span>"));
        }
        pos = b.max(pos);
    }
    if pos < end {
        html.push_str(&escape(&text[pos..end]));
    }
    html.push_str("</pre>");
    html
}

/// Puts HTML (with a plain-text alternative) on the system clipboard.
pub fn set_clipboard_html(html: &str, plain: &str) -> Result<(), String> {
    let mut cb = crate::system_clipboard::open().map_err(|e| e.to_string())?;
    cb.set_html(html, Some(plain)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_keywords_and_escapes_markup() {
        let text = "if a < b:\n    pass\n";
        let spans = rustpy_highlight::highlight_python(text);
        let scheme = ColorScheme::dark();
        let html = to_html(text, 0, text.len(), &spans, &scheme, "Hack", 12.0);
        assert!(html.starts_with("<pre style=\"background-color:#"));
        assert!(html.contains("&lt;"), "markup is escaped: {html}");
        let body = &html[html.find('>').unwrap() + 1..html.len() - "</pre>".len()];
        assert!(
            !body
                .replace("<span", "")
                .replace("</span>", "")
                .contains('<'),
            "no raw '<' in content: {html}"
        );
        let kw = scheme
            .style(rustpy_highlight::HighlightTag::Keyword)
            .fg
            .unwrap();
        assert!(
            html.contains(&format!("color:{};", hex(kw))),
            "keywords keep their color"
        );
        assert!(html.ends_with("</pre>"));
    }

    #[test]
    fn respects_the_selection_bounds() {
        let text = "import os\nprint(os.name)\n";
        let spans = rustpy_highlight::highlight_python(text);
        let html = to_html(text, 10, 15, &spans, &ColorScheme::dark(), "Mono", 10.0);
        assert!(html.contains("print") && !html.contains("import") && !html.contains("name"));
    }
}
