//! Soft-wrap geometry (roadmap §1.1 Soft wrap). The view wraps a line's
//! glyphs (characters with tabs expanded, plus inlay hint labels) every `cols`
//! display columns; this module says which visual row a caret is on and where
//! it lands on another row, so Up/Down move by visual rows.

/// How the view soft-wraps lines: `cols` display columns per visual row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoftWrap {
    pub cols: usize,
    pub tab_size: usize,
    pub original_indent: bool,
}

/// Display columns of a character as the view lays it out (a tab is `tab_size` spaces).
fn glyphs(c: char, tab_size: usize) -> usize {
    if c == '\t' {
        tab_size
    } else {
        1
    }
}

/// Display width of a line: its characters plus `hint_chars` of inlay hints.
pub fn line_width(text: &str, hint_chars: usize, tab_size: usize) -> usize {
    text.chars().map(|c| glyphs(c, tab_size)).sum::<usize>() + hint_chars
}

/// Visual rows a line of `width` display columns takes.
pub fn rows_for_width(width: usize, cols: usize) -> usize {
    width.max(1).div_ceil(cols.max(1)).max(1)
}

/// One soft-wrapped line: the display column of every caret position.
pub struct WrappedLine {
    /// `stops[c]`: display column of a caret before character `c` (`c = 0..=chars`).
    stops: Vec<usize>,
    rows: usize,
    cols: usize,
    indent: usize,
}

impl WrappedLine {
    /// `hints`: the line's inlay hints as (byte offset in `text`, label
    /// characters), sorted. A hint is drawn before the character at its offset
    /// and after a caret there.
    pub fn new(text: &str, hints: &[(usize, usize)], wrap: SoftWrap) -> Self {
        let mut stops = Vec::with_capacity(text.len() + 1);
        let mut col = 0usize;
        let mut h = 0usize;
        for (byte, c) in text.char_indices() {
            stops.push(col);
            while h < hints.len() && hints[h].0 <= byte {
                col += hints[h].1;
                h += 1;
            }
            col += glyphs(c, wrap.tab_size);
        }
        stops.push(col);
        let trailing: usize = hints[h..].iter().map(|(_, n)| n).sum();
        let indent = if wrap.original_indent {
            continuation_indent(text, wrap.cols, wrap.tab_size)
        } else {
            0
        };
        let cols = wrap.cols.saturating_sub(indent).max(1);
        Self {
            stops,
            rows: rows_for_width(col + trailing, cols),
            cols,
            indent,
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Visual row and display column (within that row) of the caret before
    /// character `col`. A caret at a wrap boundary starts the next row (where
    /// typed text goes); at the end of a full last row it ends that row.
    pub fn row_col(&self, col: usize) -> (usize, usize) {
        let d = self.stops[col.min(self.stops.len() - 1)];
        let row = (d / self.cols).min(self.rows - 1);
        (
            row,
            d - row * self.cols + if row > 0 { self.indent } else { 0 },
        )
    }

    /// The caret position (character index) for display column `x` of visual
    /// row `row`: the last one at or before `x`, but on that row. Only the last
    /// row has a caret position after its final column (elsewhere that position
    /// belongs to the next row).
    pub fn col_at(&self, row: usize, x: usize) -> usize {
        let row = row.min(self.rows - 1);
        let x = if row > 0 {
            x.saturating_sub(self.indent)
        } else {
            x
        };
        let start = row * self.cols;
        let x = if row + 1 == self.rows {
            x
        } else {
            x.min(self.cols - 1)
        };
        let target = start.saturating_add(x);
        let at_or_before = self
            .stops
            .partition_point(|d| *d <= target)
            .saturating_sub(1);
        if self.stops[at_or_before] >= start {
            return at_or_before;
        }
        // A tab or hint crosses into this row: its first caret position on the row.
        self.stops
            .partition_point(|d| *d < start)
            .min(self.stops.len() - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::{Caret, EditorState};
    use super::super::Language;
    use super::*;
    use rustpy_core::{Position, TextSize};

    const WRAP: SoftWrap = SoftWrap {
        cols: 10,
        tab_size: 4,
        original_indent: false,
    };

    #[test]
    fn rows_and_columns_of_a_wrapped_line() {
        let line = WrappedLine::new("0123456789abcdefghijKLM", &[], WRAP);
        assert_eq!(line.rows(), 3);
        assert_eq!(line.row_col(0), (0, 0));
        assert_eq!(line.row_col(9), (0, 9));
        assert_eq!(
            line.row_col(10),
            (1, 0),
            "a caret at the wrap starts the next row"
        );
        assert_eq!(line.row_col(23), (2, 3));
        assert_eq!(line.col_at(1, 4), 14);
        assert_eq!(
            line.col_at(2, 7),
            23,
            "past the end of the last row: the line end"
        );
        assert_eq!(line.col_at(0, 15), 9, "a full row keeps the caret on it");
        // A line that fills its last row exactly: its end stays on that row.
        let full = WrappedLine::new("0123456789abcdefghij", &[], WRAP);
        assert_eq!(full.rows(), 2);
        assert_eq!(full.row_col(20), (1, 10));
        assert_eq!(full.col_at(1, 10), 20);
        assert_eq!(WrappedLine::new("", &[], WRAP).rows(), 1);
    }

    #[test]
    fn tabs_and_inlay_hints_take_display_columns() {
        // "\tab" with a 4-column tab and a 7-character hint before `b`.
        let line = WrappedLine::new("\tabcdefgh", &[(2, 7)], WRAP);
        assert_eq!(line_width("\tabcdefgh", 7, 4), 19);
        assert_eq!(line.rows(), 2);
        assert_eq!(line.row_col(1), (0, 4));
        assert_eq!(
            line.row_col(2),
            (0, 5),
            "the caret before `b` is drawn before the hint"
        );
        assert_eq!(line.row_col(3), (1, 3));
        // Row 1 starts inside the hint: its first caret position is after it.
        assert_eq!(line.col_at(1, 0), 3);
        assert_eq!(line.col_at(1, 1), 3);
    }

    /// A 47-character line wrapped at 10 columns (5 rows), then two short lines.
    fn wrapped_state() -> EditorState {
        let long = format!(
            "data = [{}]",
            (0..13)
                .map(|i| format!("{i:02}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(long.chars().count(), 47);
        let mut s = EditorState::from_text(
            &format!("{long}\nsecond = 2\nthird = 3\n"),
            Language::Python,
        );
        s.soft_wrap = Some(WRAP);
        s
    }

    fn pos(s: &EditorState) -> (u32, u32) {
        let p = s.doc.offset_to_position(s.primary().head());
        (p.line, p.column)
    }

    #[test]
    fn up_and_down_move_through_the_rows_of_a_wrapped_line() {
        let mut s = wrapped_state();
        for col in [10, 20, 30, 40] {
            s.move_vertical(1, false);
            assert_eq!(pos(&s), (0, col), "Down stays on the wrapped line");
        }
        s.move_vertical(1, false);
        assert_eq!(pos(&s), (1, 0));
        s.move_vertical(-1, false);
        assert_eq!(
            pos(&s),
            (0, 40),
            "Up from the next line lands on the last row"
        );
        // The column within the row is kept; a short last row ends the line.
        s.set_carets(vec![Caret::at(
            s.doc.position_to_offset(Position::new(0, 8)),
        )]);
        for col in [18, 28, 38, 47] {
            s.move_vertical(1, false);
            assert_eq!(pos(&s), (0, col));
        }
        s.move_vertical(1, false);
        assert_eq!(pos(&s), (1, 8));
        s.move_vertical(1, false);
        assert_eq!(pos(&s), (2, 8));
        s.move_vertical(-3, false);
        assert_eq!(
            pos(&s),
            (0, 38),
            "a multi-row step (Page Up) counts visual rows"
        );
        // The QA case: Home, Down, type — the text goes into the second row of line 1.
        s.set_carets(vec![Caret::at(TextSize::new(0))]);
        s.move_vertical(1, false);
        s.insert_text("Z", &Default::default());
        assert!(
            s.doc.line_text(0).starts_with("data = [00Z"),
            "{}",
            s.doc.line_text(0)
        );
        assert_eq!(s.doc.line_text(1), "second = 2");
    }

    #[test]
    fn rows_at_the_ends_of_the_text_and_selections() {
        let mut s = wrapped_state();
        s.set_carets(vec![Caret::at(TextSize::new(5))]);
        s.move_vertical(-1, false);
        assert_eq!(pos(&s), (0, 0), "Up on the first row goes to the start");
        s.move_vertical(1, false);
        assert_eq!(pos(&s), (0, 15), "the column is kept for the next move");
        s.set_carets(vec![Caret::at(TextSize::new(0))]);
        s.move_vertical(1, true);
        assert_eq!(
            s.primary().range(),
            rustpy_core::TextRange::new(TextSize::new(0), TextSize::new(10)),
            "Shift+Down selects one visual row"
        );
        let end = s.doc.len();
        s.set_carets(vec![Caret::at(end)]);
        s.move_vertical(1, false);
        assert_eq!(s.primary().head(), end);
        // Without soft wraps Up/Down still move by lines.
        let mut plain = wrapped_state();
        plain.soft_wrap = None;
        plain.move_vertical(1, false);
        assert_eq!(pos(&plain), (1, 0));
    }

    #[test]
    fn folded_lines_are_skipped_between_wrapped_rows() {
        // A def header of 24 characters (3 rows at 10 columns) with a folded body.
        let mut s = EditorState::from_text(
            "def function_name(a, b):\n    a = 1\n    b = 2\nz = 3\n",
            Language::Python,
        );
        s.soft_wrap = Some(WRAP);
        s.toggle_fold_at_line(0);
        s.set_carets(vec![Caret::at(TextSize::new(0))]);
        s.move_vertical(1, false);
        s.move_vertical(1, false);
        assert_eq!(pos(&s), (0, 20));
        s.move_vertical(1, false);
        assert_eq!(pos(&s), (3, 0), "the folded body is skipped");
        s.move_vertical(-1, false);
        assert_eq!(pos(&s), (0, 20), "Up lands on the header's last row");
        // A fold reaching the end of the text: Down stops at the end of the header.
        let mut t = EditorState::from_text(
            "def function_name(a, b):\n    a = 1\n    b = 2",
            Language::Python,
        );
        t.soft_wrap = Some(WRAP);
        t.toggle_fold_at_line(0);
        t.set_carets(vec![Caret::at(TextSize::new(20))]);
        t.move_vertical(1, false);
        assert_eq!(pos(&t), (0, 24));
    }

    #[test]
    fn clone_caret_below_adds_a_caret_on_the_next_visual_row() {
        let mut s = wrapped_state();
        s.set_carets(vec![Caret::at(TextSize::new(3))]);
        s.clone_caret(true);
        let heads: Vec<u32> = s.carets.iter().map(|k| u32::from(k.head())).collect();
        assert_eq!(heads, [3, 13]);
    }
}

/// Continuation indentation is bounded so every visual row retains text space.
pub fn continuation_indent(text: &str, cols: usize, tab_size: usize) -> usize {
    text.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| glyphs(c, tab_size))
        .sum::<usize>()
        .min(cols / 3)
}

#[cfg(test)]
mod indentation_fixture {
    use super::*;
    #[test]
    fn continuation_indent_preserves_caret_roundtrip() {
        let line = WrappedLine::new(
            "    abcdefghij",
            &[],
            SoftWrap {
                cols: 10,
                tab_size: 4,
                original_indent: true,
            },
        );
        assert_eq!(line.rows(), 2);
        assert_eq!(line.row_col(7), (1, 3));
        assert_eq!(line.col_at(1, 3), 7);
        for column in 0..14 {
            let (row, x) = line.row_col(column);
            assert_eq!(line.col_at(row, x), column);
        }
    }
}
