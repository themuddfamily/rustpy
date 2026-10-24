//! The [`Expansion`] result type and the private field markers the
//! generators use to mark edit points while building output text.

use std::ops::Range;

/// Opens an edit point (tab stop) in generated text. Private-use code points
/// never survive into an [`Expansion`]; abbreviations containing them are
/// rejected up front.
pub(crate) const FIELD_OPEN: char = '\u{E000}';
/// Closes the edit point opened by [`FIELD_OPEN`]; the text in between is the
/// placeholder (empty for a plain insertion point).
pub(crate) const FIELD_CLOSE: char = '\u{E001}';

/// An empty edit point, as a string.
pub(crate) const CARET: &str = "\u{E000}\u{E001}";

/// Wraps `placeholder` in field markers.
pub(crate) fn field(placeholder: &str) -> String {
    format!("{FIELD_OPEN}{placeholder}{FIELD_CLOSE}")
}

/// True when `s` contains one of the private field markers.
pub(crate) fn has_marker(s: &str) -> bool {
    s.contains([FIELD_OPEN, FIELD_CLOSE])
}

/// True when `s` is empty once field markers are ignored.
pub(crate) fn is_blank_value(s: &str) -> bool {
    s.chars().all(|c| c == FIELD_OPEN || c == FIELD_CLOSE)
}

/// The text produced by expanding an abbreviation, plus where the caret goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    /// The expanded text. Nested lines are indented with the `indent` unit
    /// passed to the expander, relative to column 0; use
    /// [`Expansion::with_base_indent`] to fit it to an indented line.
    pub text: String,
    /// Byte offset into `text` for the caret: the start of the first edit
    /// point (empty attribute value, empty element body, snippet
    /// placeholder), or the end of `text` when there is none.
    pub caret: usize,
    /// Every edit point in document order, as byte ranges into `text`. A
    /// range is empty for a plain insertion point and covers the placeholder
    /// text otherwise (e.g. `style` in `link:css`), so an editor can select it
    /// or offer "next edit point" navigation.
    pub stops: Vec<Range<usize>>,
}

impl Expansion {
    /// Builds an expansion from generated text containing field markers.
    pub(crate) fn from_marked(raw: &str) -> Expansion {
        let mut text = String::with_capacity(raw.len());
        let mut open: Vec<usize> = Vec::new();
        let mut stops: Vec<Range<usize>> = Vec::new();
        for c in raw.chars() {
            match c {
                FIELD_OPEN => open.push(text.len()),
                FIELD_CLOSE => {
                    if let Some(start) = open.pop() {
                        stops.push(start..text.len());
                    }
                }
                _ => text.push(c),
            }
        }
        // Document order; an enclosing placeholder sorts before the ones it contains.
        stops.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
        stops.dedup();
        let caret = stops.first().map_or(text.len(), |r| r.start);
        Expansion { text, caret, stops }
    }

    /// Re-bases the expansion onto a line whose indentation is `base`: every
    /// line after the first gets `base` prepended (the first line continues
    /// wherever the abbreviation started). `caret` and `stops` are shifted to
    /// match.
    pub fn with_base_indent(&self, base: &str) -> Expansion {
        if base.is_empty() || !self.text.contains('\n') {
            return self.clone();
        }
        let newlines: Vec<usize> = self.text.match_indices('\n').map(|(i, _)| i).collect();
        // Every newline strictly before `offset` pushes it right by `base`.
        let shift =
            |offset: usize| offset + base.len() * newlines.partition_point(|&nl| nl < offset);
        Expansion {
            text: self.text.replace('\n', &format!("\n{base}")),
            caret: shift(self.caret),
            stops: self
                .stops
                .iter()
                .map(|r| shift(r.start)..shift(r.end))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_become_stops() {
        let raw = format!("<a href=\"{CARET}\">{}</a>", field("text"));
        let e = Expansion::from_marked(&raw);
        assert_eq!(e.text, "<a href=\"\">text</a>");
        assert_eq!(e.caret, 9);
        assert_eq!(e.stops, vec![9..9, 11..15]);
    }

    #[test]
    fn no_markers_puts_caret_at_end() {
        let e = Expansion::from_marked("<p>x</p>");
        assert_eq!(e.caret, 8);
        assert!(e.stops.is_empty());
    }

    #[test]
    fn nested_placeholders_sort_outer_first() {
        let raw = format!("a{FIELD_OPEN}b{}c{FIELD_CLOSE}", field("x"));
        let e = Expansion::from_marked(&raw);
        assert_eq!(e.text, "abxc");
        assert_eq!(e.stops, vec![1..4, 2..3]);
    }

    #[test]
    fn base_indent_shifts_offsets() {
        let e = Expansion::from_marked(&format!("<ul>\n\t<li>{CARET}</li>\n</ul>"));
        let r = e.with_base_indent("    ");
        assert_eq!(r.text, "<ul>\n    \t<li></li>\n    </ul>");
        assert_eq!(&r.text[..r.caret], "<ul>\n    \t<li>");
        assert_eq!(r.stops, vec![r.caret..r.caret]);
        assert_eq!(e.with_base_indent(""), e);
    }
}
