//! Structure-aware caret and code movement (roadmap §1.2): *Move Statement
//! Up/Down* (whole statements with decorators and attached comments, past the
//! neighbouring statement of the same block), *Move Element Left/Right*
//! (arguments, parameters, collection items, imported names) and *Move Caret
//! to Code Block Start/End*.

use rustpy_core::{TextRange, TextSize};
use rustpy_text::util::indent_width;
use rustpy_text::Edit;

use super::state::{Caret, EditorState, Language, LastOp};
use super::syntax_support;

fn ts(n: usize) -> TextSize {
    TextSize::new(n as u32)
}

impl EditorState {
    /// Extends a statement's first line upward over comment lines attached to
    /// it (same indentation, no blank line in between).
    fn with_attached_comments(
        &self,
        (first, last): (usize, usize),
        floor: usize,
    ) -> (usize, usize) {
        let indent = indent_width(&self.doc.line_text(first), 4);
        let mut start = first;
        while start > floor {
            let t = self.doc.line_text(start - 1);
            if !t.trim_start().starts_with('#') || indent_width(&t, 4) != indent {
                break;
            }
            start -= 1;
        }
        (start, last)
    }

    /// Ctrl+Shift+Up/Down: moves the statement at the caret (or the selected
    /// statements) past its neighbour in the same block. Blank lines between
    /// them stay where they are; at the start/end of a block nothing moves.
    /// Files without a syntax tree move lines instead.
    pub fn move_statement(&mut self, down: bool) {
        if self.language != Language::Python || self.is_large_file() {
            self.move_lines(down);
            return;
        }
        let text = self.doc.text();
        let sel = self.primary().range();
        let first_line = self.doc.line_of(sel.start());
        let mut last_line = self.doc.line_of(sel.end());
        if last_line > first_line && sel.end() == self.doc.line_range(last_line).start() {
            last_line -= 1;
        }
        let parse = self.syntax().clone();
        let doc = &self.doc;
        let line_of = |off: TextSize| doc.line_of(off);
        let Some(a) =
            syntax_support::statement_unit(&parse, &text, doc.line_range(first_line), &line_of)
        else {
            // Blank or comment line: plain line move.
            self.move_lines(down);
            return;
        };
        // A selection over several statements of one block moves them together.
        let unit = match syntax_support::statement_unit(
            &parse,
            &text,
            doc.line_range(last_line),
            &line_of,
        ) {
            Some(b) if b.parent == a.parent && b.lines.1 > a.lines.1 => {
                syntax_support::StatementUnit {
                    lines: (a.lines.0, b.lines.1),
                    prev: a.prev,
                    next: b.next,
                    parent: a.parent,
                }
            }
            _ => a,
        };
        let Some(other) = (if down { unit.next } else { unit.prev }) else {
            return;
        };
        let (upper, lower) = if down {
            (unit.lines, other)
        } else {
            (other, unit.lines)
        };
        let upper = self.with_attached_comments(upper, 0);
        let lower = self.with_attached_comments(lower, upper.1 + 1);
        let range_of = |(a, b): (usize, usize)| {
            TextRange::new(self.doc.line_range(a).start(), self.doc.line_range(b).end())
        };
        let (ur, lr) = (range_of(upper), range_of(lower));
        let (u_text, gap, l_text) = (
            self.doc.slice(ur),
            self.doc.slice(TextRange::new(ur.end(), lr.start())),
            self.doc.slice(lr),
        );
        let whole = TextRange::new(ur.start(), lr.end());
        let (ul, gl, ll) = (u_text.len() as i64, gap.len() as i64, l_text.len() as i64);
        // Offsets in the upper block move down past the lower one and vice versa.
        let shift = |off: TextSize| -> TextSize {
            let d = if off >= ur.start() && off <= ur.end() {
                ll + gl
            } else if off >= lr.start() && off <= lr.end() {
                -(ul + gl)
            } else if off > ur.end() && off < lr.start() {
                ll - ul
            } else {
                0
            };
            TextSize::new((u32::from(off) as i64 + d) as u32)
        };
        let carets = self
            .carets
            .iter()
            .map(|k| Caret {
                sel: rustpy_text::Selection::new(shift(k.sel.anchor), shift(k.sel.head)),
                goal_column: k.goal_column,
            })
            .collect();
        let group = self.group_for(LastOp::Other);
        self.apply(
            vec![Edit::replace(whole, format!("{l_text}{gap}{u_text}"))],
            carets,
            group,
            Some("Move Statement"),
        );
    }

    /// Ctrl+Alt+Shift+Left/Right: swaps the list element at the caret with
    /// its neighbour; the caret stays on the moved element.
    pub fn move_element(&mut self, right: bool) {
        if self.language != Language::Python || self.is_large_file() {
            return;
        }
        let head = self.primary().head();
        let Some((elem, other)) = syntax_support::element_swap(self.syntax(), head, right) else {
            return;
        };
        let (first, second) = if elem.start() < other.start() {
            (elem, other)
        } else {
            (other, elem)
        };
        let between = self.doc.slice(TextRange::new(first.end(), second.start()));
        let (first_text, second_text) = (self.doc.slice(first), self.doc.slice(second));
        let new_elem_start = if elem == first {
            first.start() + ts(second_text.len() + between.len())
        } else {
            first.start()
        };
        let caret = new_elem_start + (head.max(elem.start()) - elem.start()).min(elem.len());
        let group = self.group_for(LastOp::Other);
        self.apply(
            vec![Edit::replace(
                TextRange::new(first.start(), second.end()),
                format!("{second_text}{between}{first_text}"),
            )],
            vec![Caret::at(caret)],
            group,
            Some("Move Element"),
        );
    }

    /// Ctrl+[ / Ctrl+]: moves the caret to the start/end of the enclosing code
    /// block (block statements or the inside of brackets); repeating it moves
    /// to the next outer block.
    pub fn move_to_code_block(&mut self, end: bool) {
        let head = self.primary().head();
        let ranges: Vec<TextRange> = if self.language == Language::Python && !self.is_large_file() {
            syntax_support::code_block_ranges(self.syntax(), head)
        } else {
            let mut pairs: Vec<TextRange> =
                super::text_folding::bracket_pairs(&self.doc.text(), self.language)
                    .into_iter()
                    .map(|(o, c)| TextRange::new(ts(o + 1), ts(c)))
                    .filter(|r| r.contains_inclusive(head))
                    .collect();
            pairs.sort_by_key(|r| r.len());
            pairs
        };
        if let Some(target) = ranges
            .iter()
            .map(|r| if end { r.end() } else { r.start() })
            .find(|t| *t != head)
        {
            self.set_carets(vec![Caret::at(target)]);
            self.last_op = LastOp::Other;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::{EditorState, Language};
    use rustpy_core::{Position, TextSize};

    fn ed(text: &str) -> EditorState {
        EditorState::from_text(text, Language::Python)
    }

    #[test]
    fn statements_move_with_their_bodies_and_decorators() {
        let src = "def a():\n    pass\n\n\n@dec\n# about b\ndef b():\n    return 1\n";
        let mut e = ed(src);
        e.move_primary_to(Position::new(0, 4));
        e.move_statement(true);
        assert_eq!(
            e.doc.text(),
            "@dec\n# about b\ndef b():\n    return 1\n\n\ndef a():\n    pass\n",
            "blank lines stay in between"
        );
        assert_eq!(
            e.primary_position(),
            Position::new(6, 4),
            "caret follows the statement"
        );
        e.move_statement(true);
        assert_eq!(
            e.doc.text(),
            "@dec\n# about b\ndef b():\n    return 1\n\n\ndef a():\n    pass\n",
            "last statement: nothing moves"
        );
        e.move_statement(false);
        assert_eq!(e.doc.text(), src);
        assert_eq!(e.primary_position(), Position::new(0, 4));
    }

    #[test]
    fn statements_stay_inside_their_block() {
        let src = "if x:\n    a = 1\n    b = (2,\n         3)\nelse:\n    c = 4\n";
        let mut e = ed(src);
        e.move_primary_to(Position::new(1, 4));
        e.move_statement(true);
        assert_eq!(
            e.doc.text(),
            "if x:\n    b = (2,\n         3)\n    a = 1\nelse:\n    c = 4\n",
            "multi-line statements move whole"
        );
        e.move_statement(true);
        assert_eq!(
            e.doc.text(),
            "if x:\n    b = (2,\n         3)\n    a = 1\nelse:\n    c = 4\n",
            "the block ends here"
        );
        // The `else:` line belongs to the whole if statement.
        let mut f = ed("x = 0\nif x:\n    pass\nelse:\n    pass\n");
        f.move_primary_to(Position::new(3, 0));
        f.move_statement(false);
        assert_eq!(f.doc.text(), "if x:\n    pass\nelse:\n    pass\nx = 0\n");
    }

    #[test]
    fn elements_swap_with_neighbours() {
        let mut e = ed("f(a, b=2, *c)\n");
        e.move_primary_to(Position::new(0, 2));
        e.move_element(true);
        assert_eq!(e.doc.text(), "f(b=2, a, *c)\n");
        assert_eq!(
            e.primary_position(),
            Position::new(0, 7),
            "caret stays on the element"
        );
        e.move_element(true);
        assert_eq!(e.doc.text(), "f(b=2, *c, a)\n");
        e.move_element(true);
        assert_eq!(e.doc.text(), "f(b=2, *c, a)\n", "last element: no move");
        let mut l = ed("x = [1, [2, 3], {'k': 4, 'j': 5}]\n");
        l.move_primary_to(Position::new(0, 9));
        l.move_element(true);
        assert_eq!(
            l.doc.text(),
            "x = [1, [3, 2], {'k': 4, 'j': 5}]\n",
            "innermost list"
        );
        l.move_primary_to(Position::new(0, 18));
        l.move_element(false);
        assert_eq!(
            l.doc.text(),
            "x = [1, [3, 2], {'k': 4, 'j': 5}]\n",
            "first dict item: no move"
        );
        l.move_element(true);
        assert_eq!(l.doc.text(), "x = [1, [3, 2], {'j': 5, 'k': 4}]\n");
        let mut p = ed("def f(self, a, b):\n    pass\n");
        p.move_primary_to(Position::new(0, 16));
        p.move_element(false);
        assert_eq!(p.doc.text(), "def f(self, b, a):\n    pass\n");
    }

    #[test]
    fn code_block_start_and_end() {
        let src = "def f(x):\n    if x:\n        y = g(1, 2)\n        return y\n    return 0\n";
        let mut e = ed(src);
        let at = |e: &EditorState| e.primary_position();
        e.move_primary_to(Position::new(2, 15));
        e.move_to_code_block(false);
        assert_eq!(
            at(&e),
            Position::new(2, 14),
            "inside the call's brackets first"
        );
        e.move_to_code_block(false);
        assert_eq!(at(&e), Position::new(2, 8), "then the if body");
        e.move_to_code_block(false);
        assert_eq!(at(&e), Position::new(1, 4), "then the function body");
        e.move_to_code_block(true);
        assert_eq!(at(&e), Position::new(4, 12), "end of the function body");
        // Brace languages use brackets.
        let mut j = EditorState::from_text("a { b: [1, 2] }", Language::Css);
        j.set_carets(vec![super::super::state::Caret::at(TextSize::new(9))]);
        j.move_to_code_block(true);
        assert_eq!(j.primary().head(), TextSize::new(12));
        j.move_to_code_block(true);
        assert_eq!(j.primary().head(), TextSize::new(14));
    }
}
