//! Comment toggling for every language and the Edit › Lines commands
//! (roadmap §1.2): block comments, line comments in languages that only have
//! block comments (HTML, CSS, Markdown, templates), sort / reverse / remove
//! duplicate lines, strip trailing spaces and indent conversion.

use rustpy_core::{Position, TextRange, TextSize};
use rustpy_text::util::{indent_width, leading_whitespace, make_indent};
use rustpy_text::Edit;

use super::state::{map_offset, Caret, EditOptions, EditorState, LastOp};

fn ts(n: usize) -> TextSize {
    TextSize::new(n as u32)
}

/// Whether `content` (a trimmed line) is wrapped in `open` … `close`.
fn is_wrapped(content: &str, open: &str, close: &str) -> bool {
    content.len() >= open.len() + close.len()
        && content.starts_with(open)
        && content.ends_with(close)
}

/// Delete ranges (relative to `content`) that unwrap `open … close`,
/// together with one space just inside each delimiter.
fn unwrap_ranges(content: &str, open: &str, close: &str) -> [(usize, usize); 2] {
    let inner = &content[open.len()..content.len() - close.len()];
    let sp1 = usize::from(inner.starts_with(' '));
    let sp2 = usize::from(inner.len() > sp1 && inner.ends_with(' '));
    [
        (0, open.len() + sp1),
        (content.len() - close.len() - sp2, content.len()),
    ]
}

impl EditorState {
    /// *Comment with Line Comment* in a language that only has block comments:
    /// wraps every selected line in `open … close` (HTML `<!-- … -->`), or
    /// unwraps them when all are wrapped.
    pub(super) fn toggle_lines_block_wrapped(&mut self, open: &str, close: &str) {
        let lines = self.selected_lines();
        let non_blank: Vec<usize> = lines
            .iter()
            .copied()
            .filter(|&l| !self.doc.line_text(l).trim().is_empty())
            .collect();
        let mut edits = Vec::new();
        if non_blank.is_empty() {
            // Blank line: an empty comment with the caret inside it.
            if self.carets.len() == 1 && !self.has_selection() {
                let at = self.primary().head();
                let group = self.group_for(LastOp::Other);
                self.apply(
                    vec![Edit::insert(at, format!("{open}  {close}"))],
                    vec![Caret::at(at + ts(open.len() + 1))],
                    group,
                    Some("Comment"),
                );
            }
            return;
        }
        let content_of = |s: &Self, l: usize| -> (TextSize, String) {
            let text = s.doc.line_text(l);
            let ws = leading_whitespace(&text).len();
            (
                s.doc.line_range(l).start() + ts(ws),
                text[ws..].trim_end().to_string(),
            )
        };
        let all_wrapped = non_blank
            .iter()
            .all(|&l| is_wrapped(&content_of(self, l).1, open, close));
        for &l in &non_blank {
            let (start, content) = content_of(self, l);
            if all_wrapped {
                for (a, b) in unwrap_ranges(&content, open, close) {
                    edits.push(Edit::delete(TextRange::new(start + ts(a), start + ts(b))));
                }
            } else {
                edits.push(Edit::insert(start, format!("{open} ")));
                edits.push(Edit::insert(start + ts(content.len()), format!(" {close}")));
            }
        }
        let group = self.group_for(LastOp::Other);
        edits.sort_by_key(|e| (e.range.start(), e.range.end()));
        let carets = self
            .carets
            .iter()
            .map(|k| {
                Caret::new(
                    map_offset(&edits, k.sel.anchor, true),
                    map_offset(&edits, k.sel.head, true),
                )
            })
            .collect();
        self.apply(edits, carets, group, Some("Comment"));
        self.caret_down_after_single_line_comment(lines.len());
    }

    /// The caret moves to the next line after commenting a single line without selection.
    pub(super) fn caret_down_after_single_line_comment(&mut self, line_count: usize) {
        if line_count == 1 && self.carets.len() == 1 && !self.has_selection() {
            let k = self.primary();
            let line = self.doc.line_of(k.head());
            if line + 1 < self.doc.line_count() {
                let col = self.doc.offset_to_position(k.head()).column;
                self.set_carets(vec![Caret::at(
                    self.doc
                        .position_to_offset(Position::new(line as u32 + 1, col)),
                )]);
            }
        }
    }

    /// Ctrl+Shift+/: wraps each selection (or the caret line) in the
    /// language's block comment, or removes the block comment around it.
    /// Languages without block comments (Python) fall back to line comments.
    pub fn toggle_block_comment(&mut self) {
        let Some((open, close)) = self.language.block_comment() else {
            self.toggle_line_comment();
            return;
        };
        let mut edits: Vec<Edit> = Vec::new();
        // Selection each caret gets afterwards (pre-edit coordinates of the commented text).
        let mut targets: Vec<(TextRange, bool)> = Vec::new();
        for k in &self.carets {
            let (range, from_selection) = if k.has_selection() {
                (k.range(), true)
            } else {
                let line = self.doc.line_of(k.head());
                let text = self.doc.line_text(line);
                let ws = leading_whitespace(&text).len();
                let start = self.doc.line_range(line).start() + ts(ws);
                (
                    TextRange::new(start, start + ts(text[ws..].trim_end().len())),
                    false,
                )
            };
            if targets.iter().any(|(r, _)| r.intersect(range).is_some()) {
                continue;
            }
            targets.push((range, from_selection));
        }
        targets.sort_by_key(|(r, _)| r.start());
        let mut new_sels: Vec<(TextSize, TextSize, bool)> = Vec::new();
        for (range, from_selection) in &targets {
            let text = self.doc.slice(*range);
            let before = TextRange::new(
                range
                    .start()
                    .checked_sub(ts(open.len()))
                    .unwrap_or_default(),
                range.start(),
            );
            let after = TextRange::new(
                range.end(),
                (range.end() + ts(close.len())).min(self.doc.len()),
            );
            if is_wrapped(&text, open, close) {
                for (a, b) in unwrap_ranges(&text, open, close) {
                    edits.push(Edit::delete(TextRange::new(
                        range.start() + ts(a),
                        range.start() + ts(b),
                    )));
                }
                new_sels.push((range.start(), range.end(), *from_selection));
            } else if *from_selection
                && self.doc.slice(before) == open
                && self.doc.slice(after) == close
            {
                // The selection is exactly the inside of a block comment.
                edits.push(Edit::delete(before));
                edits.push(Edit::delete(after));
                new_sels.push((range.start(), range.end(), true));
            } else {
                edits.push(Edit::insert(range.start(), open.to_string()));
                edits.push(Edit::insert(range.end(), close.to_string()));
                new_sels.push((range.start(), range.end(), *from_selection));
            }
        }
        if edits.is_empty() {
            return;
        }
        edits.sort_by_key(|e| (e.range.start(), e.range.end()));
        let carets: Vec<Caret> = new_sels
            .iter()
            .map(|&(s, e, sel)| {
                let (s, e) = (map_offset(&edits, s, true), map_offset(&edits, e, false));
                if sel {
                    Caret::new(s, e.max(s))
                } else {
                    Caret::at(e.max(s))
                }
            })
            .collect();
        let group = self.group_for(LastOp::Other);
        self.apply(edits, carets, group, Some("Block Comment"));
    }

    /// Line ranges a line command works on: the selected lines, or the whole
    /// document (without its final empty line) when nothing is selected.
    fn line_command_blocks(&self) -> Vec<(usize, usize)> {
        if self.has_selection() {
            return self.selected_line_blocks();
        }
        let mut last = self.doc.line_count().saturating_sub(1);
        if last > 0 && self.doc.line_text(last).is_empty() {
            last -= 1;
        }
        vec![(0, last)]
    }

    /// Replaces the lines of every block with `f(lines)`. Selections end up on
    /// the processed blocks; a plain caret keeps its line and column.
    fn transform_line_blocks(&mut self, label: &str, f: impl Fn(Vec<String>) -> Vec<String>) {
        let blocks = self.line_command_blocks();
        let had_selection = self.has_selection();
        let mut edits = Vec::new();
        let mut block_ranges = Vec::new();
        for &(a, b) in &blocks {
            let range =
                TextRange::new(self.doc.line_range(a).start(), self.doc.line_range(b).end());
            block_ranges.push(range);
            let lines: Vec<String> = (a..=b).map(|l| self.doc.line_text(l)).collect();
            let new_lines = f(lines.clone());
            if new_lines != lines {
                edits.push(Edit::replace(range, new_lines.join("\n")));
            }
        }
        if edits.is_empty() {
            return;
        }
        let positions: Vec<Position> = self
            .carets
            .iter()
            .map(|k| self.doc.offset_to_position(k.head()))
            .collect();
        let mut carets: Vec<Caret> = if had_selection {
            block_ranges
                .iter()
                .map(|r| {
                    let start = map_offset(&edits, r.start(), false);
                    let end = map_offset(&edits, r.end(), true);
                    // The primary caret comes first: when adjacent selections
                    // coalesce into one line block, keep its direction.
                    let backwards = self
                        .carets
                        .iter()
                        .find(|k| r.start() <= k.range().start() && k.range().start() <= r.end())
                        .is_some_and(|k| k.sel.anchor > k.sel.head);
                    if backwards {
                        Caret::new(end, start)
                    } else {
                        Caret::new(start, end)
                    }
                })
                .collect()
        } else {
            self.carets
                .iter()
                .map(|k| Caret::at(map_offset(&edits, k.head(), false)))
                .collect()
        };
        if had_selection {
            let primary_start = self.primary().range().start();
            if let Some(index) = block_ranges
                .iter()
                .position(|range| range.start() <= primary_start && primary_start <= range.end())
            {
                carets.swap(0, index);
            }
        }
        let token = match self.begin_transaction(label) {
            Ok(token) => token,
            Err(error) => {
                if error == rustpy_text::HistoryError::ReadOnly {
                    self.read_only_refused = true;
                }
                return;
            }
        };
        let group = self.group_for(LastOp::Other);
        if self.apply(edits, carets, group, Some(label)) {
            if !had_selection {
                let restored = positions
                    .into_iter()
                    .map(|p| Caret::at(self.doc.position_to_offset(p)))
                    .collect();
                self.set_carets(restored);
            }
            // Include the restored logical positions in the redo snapshot.
            self.commit_transaction(token)
                .expect("line command owns its transaction");
        } else {
            self.rollback_transaction(token)
                .expect("line command owns its transaction");
        }
    }

    /// Edit › Sort Lines (selected lines, or the whole file).
    pub fn sort_lines(&mut self) {
        self.transform_line_blocks("Sort Lines", |mut lines| {
            lines.sort();
            lines
        });
    }

    /// Edit › Reverse Lines (selected lines, or the whole file).
    pub fn reverse_lines(&mut self) {
        self.transform_line_blocks("Reverse Lines", |mut lines| {
            lines.reverse();
            lines
        });
    }

    /// Edit › Remove Duplicate Lines: keeps the first occurrence of every
    /// line, including empty lines. Whitespace-distinct lines remain distinct.
    pub fn remove_duplicate_lines(&mut self) {
        self.transform_line_blocks("Remove Duplicate Lines", |lines| {
            let mut seen = std::collections::HashSet::new();
            lines
                .into_iter()
                .filter(|l| seen.insert(l.clone()))
                .collect()
        });
    }

    /// Edit › Strip Trailing Spaces (selected lines, or the whole file).
    pub fn trim_trailing_whitespace(&mut self) {
        let mut edits = Vec::new();
        for (a, b) in self.line_command_blocks() {
            for l in a..=b {
                let text = self.doc.line_text(l);
                let trimmed = text.trim_end_matches([' ', '\t']).len();
                if trimmed < text.len() {
                    let start = self.doc.line_range(l).start();
                    edits.push(Edit::delete(TextRange::new(
                        start + ts(trimmed),
                        start + ts(text.len()),
                    )));
                }
            }
        }
        if edits.is_empty() {
            return;
        }
        let carets = self
            .carets
            .iter()
            .map(|k| {
                Caret::new(
                    map_offset(&edits, k.sel.anchor, false),
                    map_offset(&edits, k.sel.head, false),
                )
            })
            .collect();
        let group = self.group_for(LastOp::Other);
        self.apply(edits, carets, group, Some("Strip Trailing Spaces"));
    }

    /// Edit › Convert Indents › To Tabs / To Spaces (selected lines, or the whole file).
    pub fn convert_indents(&mut self, to_tabs: bool, opts: &EditOptions) {
        let mut edits = Vec::new();
        for (a, b) in self.line_command_blocks() {
            for l in a..=b {
                let text = self.doc.line_text(l);
                let ws = leading_whitespace(&text);
                if ws.is_empty() {
                    continue;
                }
                // Spaces to tabs: one tab per indent level (a file indented by 2
                // spaces gets one tab per 2 spaces, not a mix of tabs and spaces).
                let unit = if to_tabs && !ws.contains('\t') {
                    opts.indent_size.max(1)
                } else {
                    opts.tab_size
                };
                let indent = make_indent(indent_width(&text, opts.tab_size), to_tabs, unit);
                if indent != ws {
                    let start = self.doc.line_range(l).start();
                    edits.push(Edit::replace(
                        TextRange::new(start, start + ts(ws.len())),
                        indent,
                    ));
                }
            }
        }
        if edits.is_empty() {
            return;
        }
        // Carets at a line start stay there; carets inside the indent go to its end.
        let map =
            |off: TextSize| map_offset(&edits, off, !edits.iter().any(|e| e.range.start() == off));
        let carets = self
            .carets
            .iter()
            .map(|k| Caret::new(map(k.sel.anchor), map(k.sel.head)))
            .collect();
        let group = self.group_for(LastOp::Other);
        self.apply(
            edits,
            carets,
            group,
            Some(if to_tabs {
                "Convert Indents to Tabs"
            } else {
                "Convert Indents to Spaces"
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::{Caret, EditOptions, EditorState, Language};
    use rustpy_core::{Position, TextSize};

    fn ts(n: u32) -> TextSize {
        TextSize::new(n)
    }

    #[test]
    fn line_comments_in_more_languages() {
        for (lang, expected) in [
            (Language::Dockerfile, "# RUN x\n"),
            (Language::Requirements, "# requests\n"),
            (Language::Http, "# GET /\n"),
            (Language::Sql, "-- SELECT 1\n"),
        ] {
            let src = expected.split_once(' ').unwrap().1;
            let mut e = EditorState::from_text(src, lang);
            e.toggle_line_comment();
            assert_eq!(e.doc.text(), expected, "{lang:?}");
            e.set_carets(vec![Caret::at(ts(0))]);
            e.toggle_line_comment();
            assert_eq!(e.doc.text(), src, "{lang:?} uncomment");
        }
    }

    #[test]
    fn html_line_comment_wraps_each_line() {
        let mut e = EditorState::from_text("<div>\n  <p>x</p>\n</div>\n", Language::Html);
        e.set_carets(vec![Caret::new(ts(0), ts(17))]);
        e.toggle_line_comment();
        assert_eq!(
            e.doc.text(),
            "<!-- <div> -->\n  <!-- <p>x</p> -->\n</div>\n"
        );
        e.toggle_line_comment();
        assert_eq!(e.doc.text(), "<div>\n  <p>x</p>\n</div>\n");
        // Templates use `{# … #}`; a single line moves the caret down.
        let mut t = EditorState::from_text("{{ x }}\nnext\n", Language::Template);
        t.toggle_line_comment();
        assert_eq!(t.doc.text(), "{# {{ x }} #}\nnext\n");
        assert_eq!(t.primary_position().line, 1);
    }

    #[test]
    fn block_comment_wraps_and_unwraps() {
        let mut e = EditorState::from_text("a { color: red; }\n", Language::Css);
        e.set_carets(vec![Caret::new(ts(4), ts(15))]);
        e.toggle_block_comment();
        assert_eq!(e.doc.text(), "a { /*color: red;*/ }\n");
        assert_eq!(e.doc.slice(e.primary().range()), "color: red;");
        e.toggle_block_comment();
        assert_eq!(e.doc.text(), "a { color: red; }\n");
        // Without a selection the caret line is commented.
        let mut h = EditorState::from_text("  <b>x</b>\n", Language::Html);
        h.toggle_block_comment();
        assert_eq!(h.doc.text(), "  <!--<b>x</b>-->\n");
        h.toggle_block_comment();
        assert_eq!(h.doc.text(), "  <b>x</b>\n");
        // Python has no block comments: falls back to line comments.
        let mut p = EditorState::from_text("x = 1\n", Language::Python);
        p.toggle_block_comment();
        assert_eq!(p.doc.text(), "# x = 1\n");
    }

    #[test]
    fn sort_reverse_and_dedupe_lines() {
        let mut e = EditorState::from_text("pear\napple\nfig\napple\n", Language::PlainText);
        e.move_primary_to(Position::new(1, 2));
        e.sort_lines();
        assert_eq!(
            e.doc.text(),
            "apple\napple\nfig\npear\n",
            "whole file without a selection; final newline kept"
        );
        assert_eq!(
            e.primary_position(),
            Position::new(1, 2),
            "caret keeps line and column"
        );
        e.remove_duplicate_lines();
        assert_eq!(e.doc.text(), "apple\nfig\npear\n");
        e.set_carets(vec![Caret::new(ts(0), ts(10))]);
        e.reverse_lines();
        assert_eq!(
            e.doc.text(),
            "fig\napple\npear\n",
            "only the selected lines"
        );
        assert_eq!(e.doc.slice(e.primary().range()), "fig\napple");
        assert!(e.undo());
        assert_eq!(e.doc.text(), "apple\nfig\npear\n");
    }

    #[test]
    fn strip_trailing_spaces_and_convert_indents() {
        let mut e = EditorState::from_text("a  \n\tb\t\n    c\n", Language::Python);
        e.trim_trailing_whitespace();
        assert_eq!(e.doc.text(), "a\n\tb\n    c\n");
        let o = EditOptions::default();
        e.convert_indents(false, &o);
        assert_eq!(e.doc.text(), "a\n    b\n    c\n");
        e.convert_indents(true, &o);
        assert_eq!(e.doc.text(), "a\n\tb\n\tc\n");
    }
}
