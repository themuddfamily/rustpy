//! Source-aware whitespace markers; painting and settings remain in the editor view.
use rustpy_core::TextSize;
use rustpy_text::util::char_width;
use rustpy_text::{Document, LineEnding};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Leading,
    Inner,
    Trailing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Space,
    Tab,
    LineEnding(LineEnding),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marker {
    /// Absolute document byte offset; always a UTF-8 boundary.
    pub offset: TextSize,
    /// Source scalar index, before tab expansion/inlay hints/soft wrapping.
    pub char_index: usize,
    pub column: usize,
    pub width: usize,
    /// Physical line endings have no whitespace category.
    pub category: Option<Category>,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub leading: bool,
    pub inner: bool,
    pub trailing: bool,
    pub line_endings: bool,
    pub tab_size: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            leading: true,
            inner: true,
            trailing: true,
            line_endings: false,
            tab_size: 4,
        }
    }
}
impl Options {
    fn shows(self, category: Category) -> bool {
        match category {
            Category::Leading => self.leading,
            Category::Inner => self.inner,
            Category::Trailing => self.trailing,
        }
    }
}

/// Classifies spaces/tabs around the first/last non-space/tab character.
/// Whitespace-only lines belong to Leading, so each marker appears once.
/// Newline markers describe the document's serialization format: normalized
/// CRLF occupies one internal LF, while an unterminated last line has no marker.
pub fn line_markers(doc: &Document, line: usize, opts: Options) -> Vec<Marker> {
    if line >= doc.line_count() {
        return Vec::new();
    }
    let range = doc.line_range(line);
    let text = doc.line_text(line);
    let first = text.find(|c| c != ' ' && c != '\t').unwrap_or(text.len());
    let last = text
        .rfind(|c| c != ' ' && c != '\t')
        .map(|i| i + text[i..].chars().next().unwrap().len_utf8())
        .unwrap_or(text.len());
    let mut markers = Vec::new();
    let mut column = 0;
    for (char_index, (byte, ch)) in text.char_indices().enumerate() {
        let width = char_width(ch, opts.tab_size.max(1), column);
        let kind = match ch {
            ' ' => Some(Kind::Space),
            '\t' => Some(Kind::Tab),
            _ => None,
        };
        if let Some(kind) = kind {
            let category = if byte < first {
                Category::Leading
            } else if byte >= last {
                Category::Trailing
            } else {
                Category::Inner
            };
            if opts.shows(category) {
                markers.push(Marker {
                    offset: range.start() + TextSize::new(byte as u32),
                    char_index,
                    column,
                    width,
                    category: Some(category),
                    kind,
                });
            }
        }
        column += width;
    }
    if opts.line_endings && doc.line_range_with_newline(line).end() > range.end() {
        markers.push(Marker {
            offset: range.end(),
            char_index: text.chars().count(),
            column,
            width: 0,
            category: None,
            kind: Kind::LineEnding(doc.line_ending()),
        });
    }
    markers
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_boundaries_and_all_three_categories() {
        let doc = Document::new(" \tα界 x \t");
        let markers = line_markers(&doc, 0, Options::default());
        assert_eq!(
            markers
                .iter()
                .map(|m| (
                    usize::from(m.offset),
                    m.char_index,
                    m.column,
                    m.width,
                    m.category.unwrap()
                ))
                .collect::<Vec<_>>(),
            vec![
                (0, 0, 0, 1, Category::Leading),
                (1, 1, 1, 3, Category::Leading),
                (7, 4, 7, 1, Category::Inner),
                (9, 6, 9, 1, Category::Trailing),
                (10, 7, 10, 2, Category::Trailing)
            ]
        );
        for marker in markers {
            assert!(doc.text().is_char_boundary(usize::from(marker.offset)));
        }
    }
    #[test]
    fn category_filters_do_not_affect_widths_or_offsets() {
        let doc = Document::new("\tα x \t");
        let markers = line_markers(
            &doc,
            0,
            Options {
                leading: false,
                inner: false,
                ..Default::default()
            },
        );
        assert_eq!(
            markers
                .iter()
                .map(|m| (m.column, m.kind))
                .collect::<Vec<_>>(),
            vec![(7, Kind::Space), (8, Kind::Tab)]
        );
    }
    #[test]
    fn blank_lines_have_one_category_and_zero_tab_size_is_safe() {
        let doc = Document::new(" \t\n\n");
        let markers = line_markers(
            &doc,
            0,
            Options {
                tab_size: 0,
                line_endings: true,
                ..Default::default()
            },
        );
        assert_eq!(markers.len(), 3);
        assert_eq!(markers[1].width, 1);
        assert!(markers[..2]
            .iter()
            .all(|m| m.category == Some(Category::Leading)));
        let hidden = line_markers(
            &doc,
            0,
            Options {
                leading: false,
                line_endings: true,
                ..Default::default()
            },
        );
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].kind, Kind::LineEnding(LineEnding::Lf));
    }
    #[test]
    fn real_physical_endings_and_normalized_crlf() {
        for (input, ending) in [
            ("α\r\nβ", LineEnding::CrLf),
            ("α\nβ", LineEnding::Lf),
            ("α\rβ", LineEnding::Cr),
        ] {
            let doc = Document::new(input);
            let opts = Options {
                line_endings: true,
                ..Default::default()
            };
            let marks = line_markers(&doc, 0, opts);
            assert_eq!(marks.len(), 1);
            assert_eq!(marks[0].offset, TextSize::new(2));
            assert_eq!(marks[0].column, 1);
            assert_eq!(marks[0].kind, Kind::LineEnding(ending));
            assert!(line_markers(&doc, 1, opts).is_empty());
            assert_eq!(doc.to_bytes().unwrap(), input.as_bytes());
        }
        let doc = Document::new("x\n");
        let opts = Options {
            line_endings: true,
            ..Default::default()
        };
        assert_eq!(line_markers(&doc, 0, opts).len(), 1);
        assert!(line_markers(&doc, 1, opts).is_empty());
        assert!(line_markers(&doc, 2, opts).is_empty());
    }
    #[test]
    fn line_offsets_are_absolute_and_line_ending_is_independent() {
        let doc = Document::new("first\n\t界 \n");
        let marks = line_markers(
            &doc,
            1,
            Options {
                leading: false,
                inner: false,
                trailing: false,
                line_endings: true,
                ..Default::default()
            },
        );
        assert_eq!(marks.len(), 1);
        assert_eq!(usize::from(marks[0].offset), 11);
        assert_eq!(marks[0].char_index, 3);
        assert_eq!(marks[0].column, 7);
    }
}
