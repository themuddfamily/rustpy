//! Configured TODO colors projected only onto maintained lexical comments.
use rustpy_core::{TextRange, TextSize};
use rustpy_highlight::{HighlightTag, Span};
use rustpy_settings::todo::TodoSettings;
use rustpy_text::{Document, DocumentId};

#[derive(Clone, Debug)]
pub(in crate::editor) struct Paint {
    pub range: TextRange,
    pub color: [u8; 3],
}

#[derive(Debug)]
pub(super) struct Cache {
    settings: TodoSettings,
    patterns: Option<crate::todo_patterns::Patterns>,
    key: Option<(DocumentId, u64, Vec<TextRange>)>,
    paint: Vec<Paint>,
    #[cfg(test)]
    pub builds: usize,
}
impl Default for Cache {
    fn default() -> Self {
        let settings = TodoSettings::default();
        Self {
            patterns: crate::todo_patterns::Patterns::compile(&settings).ok(),
            settings,
            key: None,
            paint: Vec::new(),
            #[cfg(test)]
            builds: 0,
        }
    }
}
impl Cache {
    pub fn configure(&mut self, settings: &TodoSettings) {
        if self.settings != *settings {
            self.settings = settings.clone();
            self.patterns = crate::todo_patterns::Patterns::compile(settings).ok();
            self.key = None;
            self.paint.clear();
        }
    }
    pub fn paint(&mut self, doc: &Document, spans: &[Span]) -> &[Paint] {
        let comments: Vec<_> = spans
            .iter()
            .filter(|span| {
                (self.settings.comments
                    && matches!(span.tag, HighlightTag::Comment | HighlightTag::TodoComment))
                    || (self.settings.strings
                        && matches!(
                            span.tag,
                            HighlightTag::String | HighlightTag::Docstring | HighlightTag::FString
                        ))
            })
            .map(|span| span.range)
            .collect();
        let key = (doc.identity(), doc.version(), comments);
        if self.key.as_ref() == Some(&key) {
            return &self.paint;
        }
        self.paint.clear();
        if let Some(patterns) = &self.patterns {
            // Large documents are skipped before flattening; same limit as scanner.
            if u32::from(doc.len()) <= 1024 * 1024 {
                let source = doc.text();
                for item in patterns.scan(&source, &key.2) {
                    let first = key
                        .2
                        .partition_point(|range| range.end() <= item.range.start());
                    for comment in key
                        .2
                        .iter()
                        .skip(first)
                        .take_while(|range| range.start() < item.range.end())
                    {
                        let start = comment.start().max(item.range.start());
                        let end = comment.end().min(item.range.end());
                        if start < end {
                            self.paint.push(Paint {
                                range: TextRange::new(start, end),
                                color: item.color,
                            });
                        }
                    }
                }
            }
        }
        self.paint.sort_by_key(|paint| paint.range.start());
        self.key = Some(key);
        #[cfg(test)]
        {
            self.builds += 1;
        }
        &self.paint
    }
}

/// Split a source segment at configured color boundaries. Later source markers
/// own their suffix; configuration priority already resolves same-offset ties.
pub(in crate::editor) fn segments(
    paint: &[Paint],
    range: TextRange,
) -> Vec<(TextRange, Option<[u8; 3]>)> {
    let relevant: Vec<_> = paint
        .iter()
        .filter(|paint| paint.range.start() < range.end() && paint.range.end() > range.start())
        .collect();
    let mut boundaries = vec![range.start(), range.end()];
    for paint in &relevant {
        boundaries.push(paint.range.start().max(range.start()));
        boundaries.push(paint.range.end().min(range.end()));
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
        .windows(2)
        .map(|pair| {
            let color = relevant
                .iter()
                .rev()
                .find(|paint| paint.range.start() <= pair[0] && paint.range.end() > pair[0])
                .map(|paint| paint.color);
            (TextRange::new(pair[0], pair[1]), color)
        })
        .collect()
}

pub(in crate::editor) fn source_range(start: usize, a: usize, b: usize) -> TextRange {
    TextRange::new(
        TextSize::new((start + a) as u32),
        TextSize::new((start + b) as u32),
    )
}
