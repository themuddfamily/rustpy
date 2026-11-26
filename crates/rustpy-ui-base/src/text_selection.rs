//! Mouse text selection over line/column grids, shared by the terminal (cell
//! columns) and the Run console (character columns).

/// A caret position: `line` is an absolute line number that stays the same
/// while output is appended or the view scrolls; `col` counts the cells or
/// characters before the caret.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextPos {
    pub line: usize,
    pub col: usize,
}

impl TextPos {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// Granularity of a press-and-drag selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectUnit {
    /// Press and drag: caret to caret.
    Char,
    /// Double click: whole words.
    Word,
    /// Triple click: whole lines.
    Line,
}

/// A selection grown by dragging from the unit where it started (`origin`)
/// to the unit under the pointer (`extent`); both are `[start, end)` spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSelection {
    pub unit: SelectUnit,
    origin: (TextPos, TextPos),
    extent: (TextPos, TextPos),
}

impl TextSelection {
    pub fn new(unit: SelectUnit, span: (TextPos, TextPos)) -> Self {
        Self {
            unit,
            origin: span,
            extent: span,
        }
    }

    /// Moves the pointer end of the selection to `span` (a span of the same unit).
    pub fn extend_to(&mut self, span: (TextPos, TextPos)) {
        self.extent = span;
    }

    /// The selected range `[start, end)` in reading order.
    pub fn range(&self) -> (TextPos, TextPos) {
        (
            self.origin.0.min(self.extent.0),
            self.origin.1.max(self.extent.1),
        )
    }

    pub fn is_empty(&self) -> bool {
        let (a, b) = self.range();
        a >= b
    }

    /// Selected columns `[from, to)` of `line`, which is `len` columns long;
    /// `None` when nothing on that line is selected.
    pub fn cols_on(&self, line: usize, len: usize) -> Option<(usize, usize)> {
        let (a, b) = self.range();
        if line < a.line || line > b.line {
            return None;
        }
        let from = if line == a.line { a.col.min(len) } else { 0 };
        let to = if line == b.line { b.col.min(len) } else { len };
        (from < to).then_some((from, to))
    }

    /// Whether the selection continues past the end of `line` onto the next one.
    pub fn includes_break_after(&self, line: usize) -> bool {
        let (a, b) = self.range();
        line >= a.line && line < b.line
    }
}

/// The span a press selects at `pos` for `unit`: a caret for [`SelectUnit::Char`],
/// the word around the character at `pos` (see [`word_at`]) or the whole line.
/// `chars` is the text of `pos.line`.
pub fn unit_span(unit: SelectUnit, pos: TextPos, chars: &[char]) -> (TextPos, TextPos) {
    match unit {
        SelectUnit::Char => (pos, pos),
        SelectUnit::Word => {
            let (s, e) = word_at(chars, pos.col);
            (TextPos::new(pos.line, s), TextPos::new(pos.line, e))
        }
        SelectUnit::Line => (
            TextPos::new(pos.line, 0),
            TextPos::new(pos.line, usize::MAX),
        ),
    }
}

/// Characters besides letters and digits that a double click keeps inside a
/// word, so paths, URLs and `file.py:12` select as one piece.
const WORD_PUNCT: &[char] = &[
    '_', '-', '.', '/', '\\', '~', ':', '@', '%', '+', '=', '#', '?', '&',
];

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || WORD_PUNCT.contains(&c)
}

/// The span `[start, end)` a double click on column `col` selects: the run of
/// word characters around it (without trailing `.`/`:` such as a sentence end),
/// or the run of blanks; any other character on its own. Past the end of the
/// line nothing is selected.
pub fn word_at(chars: &[char], col: usize) -> (usize, usize) {
    if col >= chars.len() {
        return (chars.len(), chars.len());
    }
    let class = |c: char| {
        if c.is_whitespace() {
            0
        } else if is_word_char(c) {
            1
        } else {
            2
        }
    };
    let k = class(chars[col]);
    if k == 2 {
        return (col, col + 1);
    }
    let mut start = col;
    while start > 0 && class(chars[start - 1]) == k {
        start -= 1;
    }
    let mut end = col + 1;
    while end < chars.len() && class(chars[end]) == k {
        end += 1;
    }
    if k == 1 {
        while end > col + 1 && matches!(chars[end - 1], '.' | ':') {
            end -= 1;
        }
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn drag_selection_orders_its_ends() {
        let mut s = TextSelection::new(SelectUnit::Char, (TextPos::new(5, 4), TextPos::new(5, 4)));
        assert!(s.is_empty(), "a press alone selects nothing");
        s.extend_to((TextPos::new(3, 2), TextPos::new(3, 2)));
        assert_eq!(s.range(), (TextPos::new(3, 2), TextPos::new(5, 4)));
        assert!(!s.is_empty());
        assert_eq!(s.cols_on(3, 10), Some((2, 10)));
        assert_eq!(s.cols_on(4, 7), Some((0, 7)));
        assert_eq!(s.cols_on(5, 10), Some((0, 4)));
        assert_eq!(s.cols_on(5, 2), Some((0, 2)), "clamped to a short line");
        assert_eq!(
            s.cols_on(4, 0),
            None,
            "an empty line has no selected columns"
        );
        assert!(
            s.includes_break_after(4) && s.includes_break_after(3) && !s.includes_break_after(5)
        );
        assert_eq!(s.cols_on(2, 10), None);
        assert_eq!(s.cols_on(6, 10), None);
    }

    #[test]
    fn word_and_line_units_grow_by_whole_units() {
        let line = chars("see /tmp/x/main.py:12: error");
        let mut s = TextSelection::new(
            SelectUnit::Word,
            unit_span(SelectUnit::Word, TextPos::new(0, 8), &line),
        );
        assert_eq!(
            s.range(),
            (TextPos::new(0, 4), TextPos::new(0, 21)),
            "path with line number, without the trailing colon"
        );
        s.extend_to(unit_span(SelectUnit::Word, TextPos::new(0, 25), &line));
        assert_eq!(s.range(), (TextPos::new(0, 4), TextPos::new(0, 28)));
        // Dragging back before the origin keeps the origin word selected.
        s.extend_to(unit_span(SelectUnit::Word, TextPos::new(0, 1), &line));
        assert_eq!(s.range(), (TextPos::new(0, 0), TextPos::new(0, 21)));

        let mut l = TextSelection::new(
            SelectUnit::Line,
            unit_span(SelectUnit::Line, TextPos::new(7, 3), &line),
        );
        l.extend_to(unit_span(SelectUnit::Line, TextPos::new(9, 0), &line));
        assert_eq!(l.range().0, TextPos::new(7, 0));
        assert_eq!(l.cols_on(9, 12), Some((0, 12)));
        assert!(
            !l.includes_break_after(9),
            "a line selection does not take the last line break"
        );
    }

    #[test]
    fn word_boundaries() {
        let line = chars("x = foo_bar(1)  done.");
        assert_eq!(word_at(&line, 5), (4, 11), "identifier");
        assert_eq!(word_at(&line, 11), (11, 12), "punctuation selects itself");
        assert_eq!(word_at(&line, 14), (14, 16), "a run of blanks");
        assert_eq!(word_at(&line, 17), (16, 20), "sentence period dropped");
        assert_eq!(
            word_at(&line, 20),
            (16, 21),
            "clicking the period itself keeps it"
        );
        assert_eq!(word_at(&line, 99), (21, 21), "past the end selects nothing");
        assert_eq!(word_at(&[], 0), (0, 0));
        assert_eq!(word_at(&chars("https://x.org/a?b=1"), 3), (0, 19), "URL");
    }
}
