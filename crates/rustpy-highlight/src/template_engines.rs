//! Template Data Language coloring with source-preserving embedded regions.
use crate::{HighlightTag, Span};
use rustpy_core::{TextRange, TextSize};
pub fn highlight(source: &str, extension: &str, engine: &str, data: &str) -> Vec<Span> {
    let mut spans = match data {
        "Plain text" => Vec::new(),
        "CSS" => crate::highlight_css(source),
        "JavaScript" => crate::highlight_javascript(source),
        _ => crate::highlight_html(source),
    };
    let overlay = if let Some(engine) =
        rustpy_syntax::template_alternate::engine(source, extension, engine)
    {
        let document = rustpy_syntax::template_alternate::parse(source, engine);
        let mut overlay = Vec::new();
        for region in document.regions {
            let start = TextSize::new(region.range.start as u32);
            let end = TextSize::new(region.range.end as u32);
            spans.retain(|span| span.range.end() <= start || span.range.start() >= end);
            if region.comment {
                overlay.push(Span {
                    range: TextRange::new(start, end),
                    tag: HighlightTag::Comment,
                });
            } else {
                overlay.push(Span {
                    range: TextRange::new(start, TextSize::new(region.content.start as u32)),
                    tag: HighlightTag::Keyword,
                });
                for mut span in crate::highlight_python(&source[region.content.clone()]) {
                    let offset = TextSize::new(region.content.start as u32);
                    span.range =
                        TextRange::new(span.range.start() + offset, span.range.end() + offset);
                    overlay.push(span);
                }
                if region.content.end < region.range.end {
                    overlay.push(Span {
                        range: TextRange::new(TextSize::new(region.content.end as u32), end),
                        tag: HighlightTag::Keyword,
                    });
                }
            }
        }
        overlay
    } else {
        let regions = rustpy_syntax::template::parse(
            source,
            if engine == "Django" {
                rustpy_syntax::template::Dialect::Django
            } else {
                rustpy_syntax::template::Dialect::Jinja
            },
        );
        let mut overlay = Vec::new();
        let original = crate::highlight_jinja(source);
        for region in regions
            .regions
            .iter()
            .filter(|region| region.kind != rustpy_syntax::template::Kind::Text)
        {
            let start = TextSize::new(region.range.start as u32);
            let end = TextSize::new(region.range.end as u32);
            spans.retain(|span| span.range.end() <= start || span.range.start() >= end);
            overlay.extend(
                original
                    .iter()
                    .filter(|span| span.range.start() >= start && span.range.end() <= end)
                    .cloned(),
            );
        }
        overlay
    };
    spans.extend(overlay);
    crate::common::finish(source, spans)
}
