//! Rectangular clipboard insertion using the editor selection model’s Unicode scalar columns.
//! Tabs remain literal scalars, so paste replaces exactly the highlighted ranges.
use rustpy_core::Position;

pub(super) struct Plan {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub carets: Vec<usize>,
}

fn edges(line: &str, left: usize, right: usize) -> (String, String) {
    let length = line.chars().count();
    let mut before: String = line.chars().take(left).collect();
    if length < left {
        before.extend(std::iter::repeat_n(' ', left - length));
    }
    let after = line.chars().skip(right).collect();
    (before, after)
}

pub(super) fn plan(source: &str, anchor: Position, head: Position, clipboard: &str) -> Plan {
    let lines: Vec<&str> = source.split('\n').collect();
    let a = anchor.line as usize;
    let h = head.line as usize;
    let first = a.min(h);
    let last = a.max(h);
    let ac = anchor.column as usize;
    let hc = head.column as usize;
    let left = ac.min(hc);
    let right = ac.max(hc);
    // A terminal newline terminates the last row; internal empty rows remain.
    let mut rows: Vec<&str> = clipboard.split('\n').collect();
    if clipboard.ends_with('\n') && rows.len() > 1 {
        rows.pop();
    }
    let count = (last - first + 1).max(rows.len());
    let end_line = first + count - 1;
    let start = lines.iter().take(first).map(|l| l.len() + 1).sum();
    let end = if end_line < lines.len() {
        start
            + lines[first..=end_line]
                .iter()
                .map(|l| l.len())
                .sum::<usize>()
            + count
            - 1
    } else {
        source.len()
    };
    let mut text = String::new();
    let mut carets = Vec::new();
    for index in 0..count {
        if index > 0 {
            text.push('\n');
        }
        let line = first + index;
        let (before, after) = edges(
            lines.get(line).copied().unwrap_or(""),
            left,
            if line <= last { right } else { left },
        );
        let row = if rows.len() == 1 {
            rows[0]
        } else {
            rows.get(index).copied().unwrap_or("")
        };
        text.push_str(&before);
        text.push_str(row);
        carets.push(start + text.len());
        text.push_str(&after);
    }
    Plan {
        start,
        end,
        text,
        carets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditOptions, EditorState, Language};

    fn paste(source: &str, a: Position, h: Position, clipboard: &str) -> EditorState {
        let mut editor = EditorState::from_text(source, Language::Python);
        editor.column_mode = true;
        editor.set_block_selection(a, h);
        editor.paste(clipboard, &EditOptions::default());
        editor
    }

    #[test]
    fn unequal_rows_extend_document_and_undo_as_one_command() {
        let mut e = paste(
            "abcd\nefgh",
            Position::new(0, 1),
            Position::new(1, 3),
            "X\nY\nZ",
        );
        assert_eq!(e.doc.text(), "aXd\neYh\n Z");
        let carets = e.carets.clone();
        assert!(e.undo());
        assert_eq!(e.doc.text(), "abcd\nefgh");
        assert!(e.redo());
        assert_eq!(e.doc.text(), "aXd\neYh\n Z");
        assert_eq!(e.carets, carets);
    }

    #[test]
    fn short_rows_blank_rows_and_reversed_rectangle() {
        let e = paste(
            "abcdef\nx\nabcdef",
            Position::new(2, 4),
            Position::new(0, 2),
            "A\n\nC",
        );
        assert_eq!(e.doc.text(), "abAef\nx \nabCef");
        let e = paste(
            "abcd\nefgh\nijkl",
            Position::new(0, 1),
            Position::new(2, 3),
            "Q\nR",
        );
        assert_eq!(e.doc.text(), "aQd\neRh\nil");
    }

    #[test]
    fn single_column_caret_preserves_unicode_and_extends_rows() {
        let e = paste(
            "éβ\nx",
            Position::new(0, 1),
            Position::new(0, 1),
            "中\n\nΩ\n",
        );
        assert_eq!(e.doc.text(), "é中β\nx\n Ω");
    }

    #[test]
    fn tabs_are_selected_and_replaced_using_the_same_scalar_columns() {
        let e = paste("abcd\n\tZ", Position::new(0, 2), Position::new(1, 0), "Q");
        assert_eq!(e.doc.text(), "Qcd\nQ");
    }

    #[test]
    fn pointer_rectangle_works_without_toggled_mode_and_single_caret_needs_no_metadata() {
        let mut e = EditorState::from_text("abc\ndef", Language::Python);
        e.set_block_selection(Position::new(0, 1), Position::new(1, 2));
        e.paste("X\nY\nZ", &EditOptions::default());
        assert_eq!(e.doc.text(), "aXc\ndYf\n Z");
        let mut e = EditorState::from_text("éβ", Language::Python);
        e.column_mode = true;
        e.move_primary_to(Position::new(0, 1));
        e.paste("A\nB", &EditOptions::default());
        assert_eq!(e.doc.text(), "éAβ\n B");
    }

    #[test]
    fn pasted_tabs_match_the_actual_highlighted_ranges() {
        let mut e = EditorState::from_text("abcd\n\tZ", Language::Python);
        e.column_mode = true;
        e.set_block_selection(Position::new(0, 2), Position::new(1, 1));
        let selected: Vec<_> = e.carets.iter().map(|c| e.doc.slice(c.range())).collect();
        assert_eq!(selected, vec!["b", "Z"]);
        e.paste("Q", &EditOptions::default());
        assert_eq!(e.doc.text(), "aQcd\n\tQ");
        assert!(e.undo());
        assert_eq!(e.doc.text(), "abcd\n\tZ");
        let selected: Vec<_> = e.carets.iter().map(|c| e.doc.slice(c.range())).collect();
        assert_eq!(selected, vec!["b", "Z"]);
    }

    #[test]
    fn read_only_retains_document_and_rectangle() {
        let mut e = EditorState::from_text("abcd", Language::Python);
        e.column_mode = true;
        e.set_block_selection(Position::new(0, 1), Position::new(0, 3));
        let carets = e.carets.clone();
        e.doc.set_read_only(true);
        e.paste("X\nY", &EditOptions::default());
        assert_eq!(e.doc.text(), "abcd");
        assert_eq!(e.carets, carets);
        assert!(e.block.is_some());
        assert!(e.take_read_only_refusal());
    }
}
