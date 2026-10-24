//! Protocol-neutral positions in UTF-8 text. Physical CR, CRLF and LF end
//! lines; columns stop before the separator. Scalar columns are code points,
//! not grapheme clusters. Invalid byte/unit interiors round down.

use std::ops::Range;
use std::sync::Arc;

/// Units of a zero-based column, independent of protocol numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnUnits {
    Bytes,
    Scalars,
    Utf16,
}

impl ColumnUnits {
    pub fn len(self, text: &str) -> usize {
        match self {
            Self::Bytes => text.len(),
            Self::Scalars => text.chars().count(),
            Self::Utf16 => text.encode_utf16().count(),
        }
    }

    fn width(self, character: char) -> usize {
        match self {
            Self::Bytes => character.len_utf8(),
            Self::Scalars => 1,
            Self::Utf16 => character.len_utf16(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextPosition {
    pub line: usize,
    pub column: usize,
}

impl TextPosition {
    pub const fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

fn first_line(text: &str) -> &str {
    text.split(['\r', '\n']).next().unwrap_or(text)
}

/// Zero-based column of a byte cursor on the first physical line. Oversized
/// cursors stop at content end; UTF-8 interiors snap to the character start.
pub fn line_column(text: &str, cursor: usize, units: ColumnUnits) -> usize {
    let line = first_line(text);
    let cursor = rustpy_core::floor_char_boundary(line, cursor);
    units.len(&line[..cursor])
}

/// Byte cursor for a zero-based first-line column. Invalid unit interiors
/// snap to the character start; oversized columns stop before the separator.
pub fn line_offset(text: &str, column: usize, units: ColumnUnits) -> usize {
    let line = first_line(text);
    let mut consumed = 0;
    for (byte, character) in line.char_indices() {
        let width = units.width(character);
        if column < consumed + width {
            return byte;
        }
        consumed += width;
    }
    line.len()
}

/// Position in raw multi-line text without allocating an index. Work is
/// bounded by the cursor prefix. A cursor between CR and LF remains at the
/// preceding content end; the cursor after both belongs to the next line.
pub fn text_position(text: &str, cursor: usize, units: ColumnUnits) -> TextPosition {
    let cursor = rustpy_core::floor_char_boundary(text, cursor);
    let bytes = text.as_bytes();
    let (mut line, mut start, mut offset) = (0, 0, 0);
    while offset < cursor {
        let separator = match bytes[offset] {
            b'\r' if bytes.get(offset + 1) == Some(&b'\n') => 2,
            b'\r' | b'\n' => 1,
            _ => {
                offset += 1;
                continue;
            }
        };
        if offset + separator > cursor {
            break;
        }
        offset += separator;
        line += 1;
        start = offset;
    }
    TextPosition::new(line, line_column(&text[start..], cursor - start, units))
}

/// Immutable raw UTF-8 snapshot with physical line starts. It never normalizes
/// text. Construction is linear; line lookup is logarithmic and column scans
/// are bounded by that line. Cloning shares text and line starts.
#[derive(Clone, Debug)]
pub struct TextIndex {
    text: Arc<str>,
    starts: Arc<[usize]>,
}

impl TextIndex {
    pub fn new(text: impl Into<Arc<str>>) -> Self {
        let text = text.into();
        let bytes = text.as_bytes();
        let mut starts = vec![0];
        for (offset, byte) in bytes.iter().enumerate() {
            if *byte == b'\n' || (*byte == b'\r' && bytes.get(offset + 1) != Some(&b'\n')) {
                starts.push(offset + 1);
            }
        }
        Self {
            text,
            starts: starts.into(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn text_arc(&self) -> Arc<str> {
        self.text.clone()
    }
    pub fn len(&self) -> usize {
        self.text.len()
    }
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    pub fn line_range(&self, line: usize) -> Option<Range<usize>> {
        let start = *self.starts.get(line)?;
        let mut end = self.starts.get(line + 1).copied().unwrap_or(self.len());
        let bytes = self.text.as_bytes();
        if end > start && bytes[end - 1] == b'\n' {
            end -= 1;
        }
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        Some(start..end)
    }

    pub fn line_of(&self, offset: usize) -> usize {
        self.starts
            .partition_point(|&start| start <= offset.min(self.len()))
            .saturating_sub(1)
    }

    pub fn position(&self, offset: usize, units: ColumnUnits) -> TextPosition {
        let offset = rustpy_core::floor_char_boundary(&self.text, offset);
        let line = self.line_of(offset);
        let range = self.line_range(line).expect("containing line exists");
        TextPosition::new(
            line,
            line_column(
                &self.text[range.clone()],
                offset.min(range.end) - range.start,
                units,
            ),
        )
    }

    /// Lines past EOF map to EOF; columns past content map to content end.
    pub fn offset(&self, position: TextPosition, units: ColumnUnits) -> usize {
        let Some(range) = self.line_range(position.line) else {
            return self.len();
        };
        range.start + line_offset(&self.text[range], position.column, units)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Document, Edit, EditGroup, TextRange, TextSize};

    const ALL: [ColumnUnits; 3] = [ColumnUnits::Bytes, ColumnUnits::Scalars, ColumnUnits::Utf16];

    #[test]
    fn raw_snapshot_preserves_bytes_and_physical_separator_boundaries() {
        let text: Arc<str> = Arc::from("é😀e\u{301}\r\nx\ry\n");
        let index = TextIndex::new(text.clone());
        assert!(Arc::ptr_eq(&text, &index.text_arc()));
        assert_eq!(index.text().as_bytes(), text.as_bytes());
        assert_eq!(index.line_count(), 4);
        assert_eq!(index.line_range(0), Some(0..9));
        assert_eq!(index.line_range(1), Some(11..12));
        assert_eq!(index.line_range(2), Some(13..14));
        assert_eq!(index.line_range(3), Some(15..15));
        for (units, end) in [
            (ColumnUnits::Bytes, 9),
            (ColumnUnits::Scalars, 4),
            (ColumnUnits::Utf16, 5),
        ] {
            for byte in [9, 10] {
                assert_eq!(index.position(byte, units), TextPosition::new(0, end));
                assert_eq!(text_position(&text, byte, units), TextPosition::new(0, end));
            }
            assert_eq!(index.position(11, units), TextPosition::new(1, 0));
            assert_eq!(index.position(13, units), TextPosition::new(2, 0));
            assert_eq!(index.position(usize::MAX, units), TextPosition::new(3, 0));
            assert_eq!(index.offset(TextPosition::new(0, usize::MAX), units), 9);
            assert_eq!(index.offset(TextPosition::new(99, 0), units), 15);
        }
        // Unicode line separators are content in the protocol's physical lines.
        assert_eq!(TextIndex::new("a\u{2028}b").line_count(), 1);
    }

    #[test]
    fn columns_floor_invalid_bytes_and_surrogate_interiors_without_merging_combining_marks() {
        let text = "é😀e\u{301}\r\nrest";
        for (units, positions) in [
            (ColumnUnits::Bytes, [0, 2, 6, 7, 9]),
            (ColumnUnits::Scalars, [0, 1, 2, 3, 4]),
            (ColumnUnits::Utf16, [0, 1, 3, 4, 5]),
        ] {
            for (byte, column) in [0, 2, 6, 7, 9].into_iter().zip(positions) {
                assert_eq!(line_column(text, byte, units), column);
                assert_eq!(line_offset(text, column, units), byte);
            }
            assert_eq!(line_column(text, 1, units), 0);
            assert_eq!(line_column(text, 5, units), positions[1]);
            assert_eq!(line_column(text, 8, units), positions[3]);
            assert_eq!(line_offset(text, usize::MAX, units), 9);
        }
        assert_eq!(line_offset(text, 2, ColumnUnits::Utf16), 2);
        for column in 3..6 {
            assert_eq!(line_offset(text, column, ColumnUnits::Bytes), 2);
        }
    }

    #[test]
    fn normalized_document_conversions_use_current_rope_after_edits_and_history() {
        let mut document = Document::new("é😀e\u{301}\r\nx\ry\n");
        assert_eq!(document.text(), "é😀e\u{301}\nx\ny\n");
        assert_eq!(
            document.offset_to_encoded_position(TextSize::new(6), ColumnUnits::Utf16),
            TextPosition::new(0, 3)
        );
        assert_eq!(
            document.encoded_position_to_offset(TextPosition::new(0, 2), ColumnUnits::Utf16),
            TextSize::new(2)
        );
        assert_eq!(
            document
                .encoded_position_to_offset(TextPosition::new(0, usize::MAX), ColumnUnits::Utf16),
            TextSize::new(9)
        );
        assert_eq!(
            document
                .encoded_position_to_offset(TextPosition::new(usize::MAX, 0), ColumnUnits::Utf16),
            document.len()
        );
        let snapshot = TextIndex::new(document.text());
        document
            .edit(
                vec![Edit::replace(
                    TextRange::new(TextSize::new(0), TextSize::new(6)),
                    "z\n😀",
                )],
                &[],
                &[],
                EditGroup::Separate,
                None,
            )
            .unwrap();
        assert_eq!(
            document.offset_to_encoded_position(TextSize::new(6), ColumnUnits::Utf16),
            TextPosition::new(1, 2)
        );
        assert_eq!(
            document.encoded_position_to_offset(TextPosition::new(1, 1), ColumnUnits::Utf16),
            TextSize::new(2)
        );
        assert_eq!(snapshot.text(), "é😀e\u{301}\nx\ny\n");
        document.undo();
        assert_eq!(
            document.offset_to_encoded_position(TextSize::new(6), ColumnUnits::Utf16),
            TextPosition::new(0, 3)
        );
        document.redo();
        assert_eq!(
            document.offset_to_encoded_position(TextSize::new(6), ColumnUnits::Utf16),
            TextPosition::new(1, 2)
        );
        document.reset("😀\n");
        assert_eq!(
            document.offset_to_encoded_position(TextSize::new(u32::MAX), ColumnUnits::Utf16),
            TextPosition::new(1, 0)
        );
        for units in ALL {
            for offset in 0..=u32::from(document.len()) {
                let byte = TextSize::new(offset);
                let position = document.offset_to_encoded_position(byte, units);
                assert_eq!(
                    document.encoded_position_to_offset(position, units),
                    document.clamp(byte)
                );
            }
        }
    }

    #[test]
    fn indexed_rope_conversions_cross_chunks_and_keep_old_scalar_contract() {
        let mut document = Document::new(&format!("{}😀e\u{301}\nlast", "é".repeat(5000)));
        let start = 10000;
        assert_eq!(
            document.offset_to_encoded_position(TextSize::new(start + 4), ColumnUnits::Utf16),
            TextPosition::new(0, 5002)
        );
        assert_eq!(
            document.encoded_position_to_offset(TextPosition::new(0, 5001), ColumnUnits::Utf16),
            TextSize::new(start)
        );
        assert_eq!(
            document.offset_to_position(TextSize::new(start + 4)).column,
            5001
        );
        document
            .edit(
                vec![Edit::insert(TextSize::new(start), "😀")],
                &[],
                &[],
                EditGroup::Separate,
                None,
            )
            .unwrap();
        assert_eq!(
            document.offset_to_encoded_position(TextSize::new(start + 8), ColumnUnits::Utf16),
            TextPosition::new(0, 5004)
        );
        assert_eq!(
            document.encoded_position_to_offset(TextPosition::new(0, 5003), ColumnUnits::Utf16),
            TextSize::new(start + 4)
        );
    }

    #[test]
    fn empty_and_consecutive_mixed_lines_clamp_consistently() {
        for units in ALL {
            let empty = TextIndex::new("");
            assert!(empty.is_empty());
            assert_eq!(empty.position(usize::MAX, units), TextPosition::default());
            assert_eq!(
                empty.offset(TextPosition::new(usize::MAX, usize::MAX), units),
                0
            );
            let index = TextIndex::new("\r\n\r\n\rx\n");
            for (byte, line) in [(0, 0), (1, 0), (2, 1), (3, 1), (4, 2), (5, 3), (7, 4)] {
                assert_eq!(index.position(byte, units), TextPosition::new(line, 0));
                assert_eq!(
                    text_position(index.text(), byte, units),
                    TextPosition::new(line, 0)
                );
            }
        }
    }
}
