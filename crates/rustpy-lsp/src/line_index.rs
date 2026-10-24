//! Conversions between the byte offsets the analysis uses and LSP positions
//! (line plus a character offset in the negotiated encoding: UTF-8, UTF-16 or
//! UTF-32 code units).
//!
//! Lines end at `\n`, `\r\n` or a lone `\r`, as the protocol (and Python's
//! tokenizer) define them.

use std::sync::Arc;

use lsp_types::{Position, Range};
use rustpy_core::{TextRange, TextSize};
use rustpy_text::{ColumnUnits, TextIndex, TextPosition};

/// The unit LSP character offsets are counted in.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Encoding {
    Utf8,
    /// The protocol default.
    #[default]
    Utf16,
    Utf32,
}

impl Encoding {
    pub fn kind(self) -> lsp_types::PositionEncodingKind {
        match self {
            Encoding::Utf8 => lsp_types::PositionEncodingKind::UTF8,
            Encoding::Utf16 => lsp_types::PositionEncodingKind::UTF16,
            Encoding::Utf32 => lsp_types::PositionEncodingKind::UTF32,
        }
    }

    /// The encoding to use given the client's preferences (most preferred
    /// first): UTF-8 when offered (no conversion needed), then UTF-32, and
    /// UTF-16 otherwise.
    pub fn negotiate(offered: Option<&[lsp_types::PositionEncodingKind]>) -> Encoding {
        let Some(offered) = offered else {
            return Encoding::Utf16;
        };
        if offered.iter().any(|k| k.as_str() == "utf-8") {
            Encoding::Utf8
        } else if offered.iter().any(|k| k.as_str() == "utf-32") {
            Encoding::Utf32
        } else {
            Encoding::Utf16
        }
    }

    fn units(self) -> ColumnUnits {
        match self {
            Encoding::Utf8 => ColumnUnits::Bytes,
            Encoding::Utf16 => ColumnUnits::Utf16,
            Encoding::Utf32 => ColumnUnits::Scalars,
        }
    }

    /// Length of `s` in this encoding's units.
    pub fn len(self, s: &str) -> u32 {
        self.units().len(s) as u32
    }
}

/// Line starts of one text, with the text itself.
#[derive(Debug)]
pub struct LineIndex {
    index: TextIndex,
}

impl LineIndex {
    pub fn new(text: impl Into<Arc<str>>) -> Self {
        Self {
            index: TextIndex::new(text),
        }
    }

    pub fn text(&self) -> &str {
        self.index.text()
    }

    pub fn text_arc(&self) -> Arc<str> {
        self.index.text_arc()
    }

    pub fn len(&self) -> TextSize {
        TextSize::new(self.index.len() as u32)
    }

    #[cfg(test)]
    pub fn line_count(&self) -> usize {
        self.index.line_count()
    }

    /// Byte range of line `line` without its line break.
    pub fn line_range(&self, line: usize) -> Option<TextRange> {
        self.index.line_range(line).map(|range| {
            TextRange::new(
                TextSize::new(range.start as u32),
                TextSize::new(range.end as u32),
            )
        })
    }

    /// Zero-based line containing byte `offset`.
    pub fn line_of(&self, offset: TextSize) -> usize {
        self.index.line_of(usize::from(offset))
    }

    /// The LSP position of byte `offset` (clamped to the text and to a
    /// character boundary).
    pub fn position(&self, offset: TextSize, enc: Encoding) -> Position {
        let position = self.index.position(usize::from(offset), enc.units());
        Position::new(position.line as u32, position.column as u32)
    }

    pub fn range(&self, range: TextRange, enc: Encoding) -> Range {
        Range::new(
            self.position(range.start(), enc),
            self.position(range.end(), enc),
        )
    }

    /// The byte offset of an LSP position. Lines past the end map to the end
    /// of the text and characters past the end of a line to its end (before
    /// the line break); an offset inside a character snaps to its start.
    pub fn offset(&self, pos: Position, enc: Encoding) -> TextSize {
        TextSize::new(self.index.offset(
            TextPosition {
                line: pos.line as usize,
                column: pos.character as usize,
            },
            enc.units(),
        ) as u32)
    }

    pub fn text_range(&self, range: Range, enc: Encoding) -> TextRange {
        let start = self.offset(range.start, enc);
        let end = self.offset(range.end, enc).max(start);
        TextRange::new(start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip_in_every_encoding() {
        let text = "a = 'é'\r\nb = '😀' + x\rc\n";
        let idx = LineIndex::new(text);
        assert_eq!(idx.line_count(), 4);
        let x = TextSize::new(text.find('x').unwrap() as u32);
        assert_eq!(idx.position(x, Encoding::Utf8), Position::new(1, 13));
        assert_eq!(idx.position(x, Encoding::Utf16), Position::new(1, 11));
        assert_eq!(idx.position(x, Encoding::Utf32), Position::new(1, 10));
        for enc in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            for (off, _) in text.char_indices() {
                let off = TextSize::new(off as u32);
                let pos = idx.position(off, enc);
                let content_end = idx.line_range(idx.line_of(off)).unwrap().end();
                assert_eq!(
                    idx.offset(pos, enc),
                    off.min(content_end),
                    "{enc:?} {pos:?}"
                );
            }
        }
        // `c` starts after the lone `\r`.
        let c = TextSize::new(text.find('c').unwrap() as u32);
        assert_eq!(idx.position(c, Encoding::Utf16), Position::new(2, 0));
    }

    #[test]
    fn out_of_range_positions_clamp() {
        let idx = LineIndex::new("ab\ncd");
        assert_eq!(
            idx.offset(Position::new(0, 99), Encoding::Utf16),
            TextSize::new(2)
        );
        assert_eq!(
            idx.offset(Position::new(9, 0), Encoding::Utf16),
            TextSize::new(5)
        );
        assert_eq!(
            idx.position(TextSize::new(99), Encoding::Utf16),
            Position::new(1, 2)
        );
        // The middle of a surrogate pair snaps to the character's start.
        let idx = LineIndex::new("😀x");
        assert_eq!(
            idx.offset(Position::new(0, 1), Encoding::Utf16),
            TextSize::new(0)
        );
        assert_eq!(
            idx.offset(Position::new(0, 2), Encoding::Utf16),
            TextSize::new(4)
        );
    }

    #[test]
    fn crlf_offsets_and_ranges_stop_before_the_line_break() {
        let text = "é😀\r\n\r\nx\r\ny\rz\n";
        let idx = LineIndex::new(text);
        for (enc, first_line_units) in [
            (Encoding::Utf8, 6),
            (Encoding::Utf16, 3),
            (Encoding::Utf32, 2),
        ] {
            // Both positions within CRLF identify the same end-of-content cursor.
            for off in [6, 7] {
                assert_eq!(
                    idx.position(TextSize::new(off), enc),
                    Position::new(0, first_line_units)
                );
            }
            assert_eq!(idx.position(TextSize::new(8), enc), Position::new(1, 0));
            assert_eq!(idx.position(TextSize::new(9), enc), Position::new(1, 0));
            assert_eq!(idx.position(TextSize::new(10), enc), Position::new(2, 0));
            assert_eq!(idx.position(idx.len(), enc), Position::new(5, 0));
            // A range including CRLF must end at the start of the next line.
            assert_eq!(
                idx.range(TextRange::new(TextSize::new(0), TextSize::new(8)), enc),
                Range::new(Position::new(0, 0), Position::new(1, 0))
            );
            assert_eq!(
                idx.range(TextRange::new(TextSize::new(6), TextSize::new(7)), enc),
                Range::new(
                    Position::new(0, first_line_units),
                    Position::new(0, first_line_units)
                )
            );
            for off in 0..=text.len() + 1 {
                let boundary = rustpy_core::floor_char_boundary(text, off);
                let line = idx.line_of(TextSize::new(boundary as u32));
                let expected =
                    TextSize::new(boundary as u32).min(idx.line_range(line).unwrap().end());
                assert_eq!(
                    idx.offset(idx.position(TextSize::new(off as u32), enc), enc),
                    expected,
                    "{enc:?} byte {off}"
                );
            }
        }
    }

    #[test]
    fn invalid_columns_snap_to_unicode_boundaries_or_crlf_content_end() {
        let idx = LineIndex::new("é😀\r\nx");
        for enc in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            assert_eq!(idx.offset(Position::new(0, 99), enc), TextSize::new(6));
            assert_eq!(idx.offset(Position::new(1, 99), enc), TextSize::new(9));
            assert_eq!(idx.offset(Position::new(99, 99), enc), idx.len());
        }
        assert_eq!(
            idx.offset(Position::new(0, 1), Encoding::Utf8),
            TextSize::new(0)
        );
        for column in 3..6 {
            assert_eq!(
                idx.offset(Position::new(0, column), Encoding::Utf8),
                TextSize::new(2)
            );
        }
        assert_eq!(
            idx.offset(Position::new(0, 2), Encoding::Utf16),
            TextSize::new(2)
        );
        assert_eq!(
            idx.offset(Position::new(0, 3), Encoding::Utf16),
            TextSize::new(6)
        );
        assert_eq!(
            idx.offset(Position::new(0, 1), Encoding::Utf32),
            TextSize::new(2)
        );
        assert_eq!(
            idx.offset(Position::new(0, 2), Encoding::Utf32),
            TextSize::new(6)
        );
    }

    #[test]
    fn negotiation_prefers_utf8() {
        use lsp_types::PositionEncodingKind as K;
        assert_eq!(Encoding::negotiate(None), Encoding::Utf16);
        assert_eq!(
            Encoding::negotiate(Some(&[K::UTF16, K::UTF8])),
            Encoding::Utf8
        );
        assert_eq!(Encoding::negotiate(Some(&[K::UTF16])), Encoding::Utf16);
        assert_eq!(
            Encoding::negotiate(Some(&[K::UTF32, K::UTF16])),
            Encoding::Utf32
        );
    }
    #[test]
    fn shared_index_keeps_raw_snapshot_identity_through_document_edits() {
        use crate::docs::Doc;
        use std::str::FromStr;
        let original: Arc<str> = Arc::from("é😀\r\nx\rz\n");
        let index = LineIndex::new(original.clone());
        assert!(Arc::ptr_eq(&original, &index.text_arc()));
        assert_eq!(index.text(), "é😀\r\nx\rz\n");
        for (encoding, start, end) in [
            (Encoding::Utf8, 2, 6),
            (Encoding::Utf16, 1, 3),
            (Encoding::Utf32, 1, 2),
        ] {
            let mut doc = Doc::new(
                lsp_types::Uri::from_str("file:///owned/m.py").unwrap(),
                "/owned/m.py".into(),
                1,
                "python",
                original.to_string(),
            );
            let snapshot = doc.index.clone();
            doc.apply_changes(
                2,
                vec![
                    lsp_types::TextDocumentContentChangeEvent {
                        range: Some(Range::new(Position::new(0, start), Position::new(0, end))),
                        range_length: None,
                        text: "Q".into(),
                    },
                    lsp_types::TextDocumentContentChangeEvent {
                        range: Some(Range::new(Position::new(1, 0), Position::new(1, 1))),
                        range_length: None,
                        text: "long".into(),
                    },
                ],
                encoding,
            );
            assert_eq!(doc.version, 2);
            assert_eq!(doc.text(), "éQ\r\nlong\rz\n");
            assert_eq!(
                snapshot.text(),
                &*original,
                "old request snapshot stays immutable"
            );
            assert_eq!(
                doc.index
                    .position(doc.index.line_range(2).unwrap().start(), encoding),
                Position::new(2, 0)
            );
        }
    }
}
