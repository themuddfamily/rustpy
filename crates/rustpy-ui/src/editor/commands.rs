//! Editing commands operating on [`EditorState`] (roadmap §1.2–§1.4).
//! Pure logic: no egui types, fully unit-testable.

use rustpy_core::{Position, TextRange, TextSize};
use rustpy_text::util::{
    char_class, indent_width, is_camel_hump, leading_whitespace, make_indent, CharClass,
};
use rustpy_text::{Edit, EditGroup};

#[path = "clipboard_payload.rs"]
pub mod clipboard_payload;
#[path = "column_paste.rs"]
mod column_paste;
use clipboard_payload::ClipboardPayload;

use super::soft_wrap::{SoftWrap, WrappedLine};
use super::state::{map_offset, Caret, EditOptions, EditorState, LastOp};

fn ts(n: usize) -> TextSize {
    TextSize::new(n as u32)
}

const DEDENT_KEYWORDS: &[&str] = &["return", "pass", "break", "continue", "raise"];

/// Enter lexes the text before the caret; beyond this much text it falls back
/// to the line-based indentation rules.
const SMART_ENTER_MAX_PREFIX: usize = 1024 * 1024;

impl EditorState {
    // ===== text insertion ====================================================

    /// Types `text` at every caret, applying smart-key behaviors for single chars.
    pub fn insert_text(&mut self, text: &str, opts: &EditOptions) {
        if !text.is_empty()
            && self.edit_in_virtual_space("Typing", |state| state.insert_text(text, opts))
        {
            return;
        }
        if opts.wrap_on_typing && !text.is_empty() && !text.contains(['\n', '\r']) {
            let token = match self.begin_transaction("Typing") {
                Ok(token) => token,
                Err(_) => {
                    self.read_only_refused = true;
                    return;
                }
            };
            self.insert_text_unwrapped(text, opts);
            self.hard_wrap_typed_lines(opts);
            self.commit_transaction(token)
                .expect("typing owns its transaction");
            return;
        }
        self.insert_text_unwrapped(text, opts);
    }

    fn insert_text_unwrapped(&mut self, text: &str, opts: &EditOptions) {
        if text.is_empty() {
            return;
        }
        if text.chars().count() == 1 {
            let c = text.chars().next().unwrap();
            if self.try_smart_char(c, opts) {
                return;
            }
        }
        let group = self.group_for(LastOp::Typing);
        self.replace_each_caret(|_, _| text.to_string(), group);
    }

    fn try_smart_char(&mut self, c: char, opts: &EditOptions) -> bool {
        let pair = match c {
            '(' => Some(')'),
            '[' => Some(']'),
            '{' => Some('}'),
            '"' | '\'' => Some(c),
            _ => None,
        };
        // Surround selection with quotes/brackets.
        if let Some(close) = pair {
            if opts.surround_selection
                && self.has_selection()
                && self.carets.iter().all(Caret::has_selection)
            {
                let group = self.group_for(LastOp::Other);
                let edits: Vec<Edit> = self
                    .carets
                    .iter()
                    .map(|k| {
                        Edit::replace(
                            k.range(),
                            format!("{c}{}{close}", self.doc.slice(k.range())),
                        )
                    })
                    .collect();
                let edits = sorted(edits);
                let new_carets: Vec<Caret> = self
                    .carets
                    .iter()
                    .map(|k| {
                        let s = map_offset(&edits, k.range().start(), false) + ts(1);
                        let e = map_offset(&edits, k.range().end(), true) - ts(close.len_utf8());
                        Caret::new(s, e)
                    })
                    .collect();
                self.apply(edits, new_carets, group, Some("Surround"));
                return true;
            }
        }
        if self.has_selection() {
            return false;
        }
        if self.language.is_pythonic() {
            if c == ':' && opts.auto_indent && self.dedent_clause_on_colon(opts) {
                return true;
            }
            if c == '('
                && opts.insert_self
                && self.insert_self_parameter(opts.paste_format_options.python.self_type_hints)
            {
                return true;
            }
            if (c == '"' || c == '\'')
                && opts.close_triple_quotes
                && opts.auto_close_quotes
                && self.close_triple_quote(c)
            {
                return true;
            }
        }
        // Type over a closing bracket/quote that was auto-inserted.
        if matches!(c, ')' | ']' | '}' | '"' | '\'')
            && (opts.auto_close_brackets || opts.auto_close_quotes)
            && self
                .carets
                .iter()
                .all(|k| self.doc.char_at(k.head()) == Some(c))
        {
            let group = self.group_for(LastOp::Typing);
            let carets = self
                .carets
                .iter()
                .map(|k| Caret::at(k.head() + ts(1)))
                .collect();
            self.set_carets(carets);
            let _ = group;
            return true;
        }
        if let Some(close) = pair {
            let is_quote = close == c;
            let enabled = if is_quote {
                opts.auto_close_quotes
            } else {
                opts.auto_close_brackets
            };
            if enabled
                && self
                    .carets
                    .iter()
                    .all(|k| self.should_auto_close(k.head(), c))
            {
                let group = self.group_for(LastOp::Typing);
                let pair_text = format!("{c}{close}");
                let edits: Vec<Edit> = self
                    .carets
                    .iter()
                    .map(|k| Edit::insert(k.head(), pair_text.clone()))
                    .collect();
                let edits = sorted(edits);
                let new_carets = self
                    .carets
                    .iter()
                    .map(|k| Caret::at(map_offset(&edits, k.head(), false) + ts(c.len_utf8())))
                    .collect();
                if self.apply(edits, new_carets, group, None) {
                    // Remember the closers so Tab can jump over them.
                    let heads: Vec<TextSize> = self.carets.iter().map(Caret::head).collect();
                    self.auto_closers.extend(heads);
                }
                return true;
            }
        }
        false
    }

    fn should_auto_close(&self, at: TextSize, c: char) -> bool {
        let next = self.doc.char_at(at);
        let prev = self.doc.char_before(at);
        // Only before whitespace / closing delimiters / end of line.
        let next_ok = matches!(
            next,
            None | Some(' ' | '\t' | '\n' | ')' | ']' | '}' | ',' | ';' | ':')
        );
        if c == '"' || c == '\'' {
            // Not right after a word character or the same quote (end of a string),
            // except a string prefix such as f, r, b, rb or fr.
            let prefix_ok = self.language.is_pythonic() && {
                let before = self.doc.slice(TextRange::new(
                    self.doc.clamp(at - TextSize::new(u32::from(at).min(3))),
                    at,
                ));
                let word: String = before
                    .chars()
                    .rev()
                    .take_while(|ch| rustpy_core::is_ident_continue(*ch))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let start = at - TextSize::of(word.as_str());
                ["r", "u", "b", "f", "t", "rb", "br", "fr", "rf", "tr", "rt"]
                    .contains(&word.to_ascii_lowercase().as_str())
                    && !self
                        .doc
                        .char_before(start)
                        .is_some_and(rustpy_core::is_ident_continue)
            };
            let prev_ok =
                prefix_ok || !prev.is_some_and(|p| rustpy_core::is_ident_continue(p) || p == c);
            // Typing the third quote of a triple-quoted string: auto-close the triple.
            return next_ok && prev_ok;
        }
        next_ok
    }

    /// Replaces each caret's selection (or inserts at the caret) with `f(caret_index, selected_text)`.
    fn replace_each_caret(&mut self, f: impl Fn(usize, &str) -> String, group: EditGroup) {
        let mut edits = Vec::with_capacity(self.carets.len());
        for (i, k) in self.carets.iter().enumerate() {
            let range = if self.overwrite_mode
                && !k.has_selection()
                && self.doc.char_at(k.head()).is_some_and(|c| c != '\n')
            {
                TextRange::new(
                    super::unicode_edit::floor_offset(self, k.head()),
                    super::unicode_edit::next(self, k.head()),
                )
            } else {
                k.range()
            };
            edits.push(Edit::replace(range, f(i, &self.doc.slice(k.range()))));
        }
        let edits = sorted(edits);
        let new_carets = self
            .carets
            .iter()
            .map(|k| Caret::at(map_offset(&edits, k.range().start(), true)))
            .collect();
        self.apply(edits, new_carets, group, None);
    }

    /// Enter: new line with Python-aware auto-indentation.
    pub fn newline(&mut self, opts: &EditOptions) {
        if self.edit_in_virtual_space("Enter", |state| state.newline(opts)) {
            return;
        }
        let group = self.group_for(LastOp::Other);
        let mut edits = Vec::new();
        let mut new_heads = Vec::new();
        // Python: indentation from the tokens before the caret (brackets,
        // multi-line headers, continuations). Lexing is linear, so skip it for
        // huge files and many carets.
        let smart = opts.auto_indent
            && self.language.is_pythonic()
            && !self.is_large_file()
            && self.carets.len() <= 8;
        let full_text = if smart {
            self.doc.text()
        } else {
            String::new()
        };
        for k in &self.carets {
            let start = k.range().start();
            let line = self.doc.line_of(start);
            let line_range = self.doc.line_range(line);
            let line_text = self.doc.line_text(line);
            let before_caret =
                &line_text[..usize::from(start - line_range.start()).min(line_text.len())];
            let mut indent = if opts.auto_indent {
                indent_width(&line_text, opts.tab_size)
            } else {
                0
            };
            let trimmed = before_caret.trim_end();
            let mut insert_closing_line = false;
            let mut closer_indent = indent.saturating_sub(opts.indent_size);
            if before_caret.trim().is_empty() {
                // At the line start or inside its indentation: the line moves
                // down unchanged and keeps its own indentation.
                indent = indent_width(&line_text, opts.tab_size);
            } else if smart && usize::from(start) <= SMART_ENTER_MAX_PREFIX {
                if let Some(e) = super::python_indent::indent_after_enter_with_continuation(
                    &full_text[..usize::from(start)],
                    opts.tab_size,
                    opts.indent_size,
                    opts.continuation_indent,
                ) {
                    indent = e.indent;
                    if let Some(c) = e.closer_indent {
                        closer_indent = c;
                        insert_closing_line =
                            matches!(self.doc.char_at(k.range().end()), Some(')' | ']' | '}'));
                    }
                }
            } else if opts.auto_indent && self.language.is_pythonic() {
                if trimmed.ends_with(':') && !trimmed.trim_start().starts_with('#') {
                    indent += opts.indent_size;
                } else if trimmed.ends_with(['(', '[', '{']) {
                    indent += opts.indent_size;
                    let after = self.doc.char_at(k.range().end());
                    insert_closing_line = matches!(after, Some(')' | ']' | '}'));
                } else if DEDENT_KEYWORDS.iter().any(|kw| {
                    trimmed.trim_start() == *kw
                        || trimmed.trim_start().starts_with(&format!("{kw} "))
                }) {
                    indent = indent.saturating_sub(opts.indent_size);
                }
            }
            let indent_str = if opts.use_tabs && opts.smart_tabs == Some(true) && smart {
                let block = super::python_indent::continuation_base(
                    &full_text[..usize::from(start)],
                    opts.tab_size,
                )
                .unwrap_or(indent);
                rustpy_format::TabIndent {
                    width: opts.tab_size,
                    smart: true,
                }
                .render(indent, block)
            } else {
                make_indent(indent, opts.use_tabs, opts.tab_size)
            };
            // Remove trailing whitespace before the caret and leading whitespace after it.
            let ws_before = before_caret.len() - trimmed.len();
            let after_caret = &line_text
                [usize::from(k.range().end() - line_range.start()).min(line_text.len())..];
            let ws_after = leading_whitespace(after_caret).len();
            let range = TextRange::new(start - ts(ws_before), k.range().end() + ts(ws_after));
            let mut text = format!("\n{indent_str}");
            let head_rel = text.len();
            if insert_closing_line {
                let outer = make_indent(closer_indent, opts.use_tabs, opts.tab_size);
                text.push('\n');
                text.push_str(&outer);
            }
            edits.push(Edit::replace(range, text));
            new_heads.push((range.start(), head_rel));
        }
        let edits = sorted(edits);
        let carets = new_heads
            .into_iter()
            .map(|(s, rel)| Caret::at(map_offset(&edits, s, false) + ts(rel)))
            .collect();
        self.apply(edits, carets, group, None);
    }

    /// Shift+Enter: start a new line below the current one.
    pub fn start_new_line(&mut self, opts: &EditOptions) {
        let carets = self
            .carets
            .iter()
            .map(|k| Caret::at(self.doc.line_range(self.doc.line_of(k.head())).end()))
            .collect();
        self.set_carets(carets);
        self.newline(opts);
    }

    /// Ctrl+Alt+Enter: start a new line above the current one.
    pub fn start_new_line_before(&mut self, opts: &EditOptions) {
        let group = self.group_for(LastOp::Other);
        let mut edits = Vec::new();
        let mut heads = Vec::new();
        for k in &self.carets {
            let line = self.doc.line_of(k.head());
            let lr = self.doc.line_range(line);
            let indent = make_indent(
                indent_width(&self.doc.line_text(line), opts.tab_size),
                opts.use_tabs,
                opts.tab_size,
            );
            edits.push(Edit::insert(lr.start(), format!("{indent}\n")));
            heads.push((lr.start(), indent.len()));
        }
        let edits = sorted(edits);
        let carets = heads
            .into_iter()
            .map(|(s, rel)| Caret::at(map_offset(&edits, s, false) + ts(rel)))
            .collect();
        self.apply(edits, carets, group, None);
    }

    /// Tab: indent selected lines, jump past an auto-inserted closing
    /// bracket/quote, or insert indentation at carets.
    pub fn tab(&mut self, opts: &EditOptions) {
        if self.edit_in_virtual_space("Tab", |state| state.tab(opts)) {
            return;
        }
        // Any selection (also within one line) indents its lines.
        if self.has_selection() {
            self.indent_lines(opts);
            return;
        }
        if opts.jump_outside_brackets_with_tab
            && !self.auto_closers.is_empty()
            && self
                .carets
                .iter()
                .all(|k| self.auto_closers.contains(&k.head()))
        {
            let carets = self
                .carets
                .iter()
                .map(|k| Caret::at(self.doc.next_boundary(k.head())))
                .collect();
            self.set_carets(carets);
            self.last_op = LastOp::Other;
            return;
        }
        let group = self.group_for(LastOp::Typing);
        let doc = &self.doc;
        let edits: Vec<Edit> = self
            .carets
            .iter()
            .map(|k| {
                let line = doc.line_of(k.range().start());
                let line_text = doc.line_text(line);
                let byte_column = usize::from(k.range().start() - doc.line_range(line).start());
                let aligned = opts.smart_tabs == Some(true)
                    && self.language.is_pythonic()
                    && !self.is_large_file()
                    && self.carets.len() <= 8
                    && line_text[..byte_column]
                        .bytes()
                        .all(|b| matches!(b, b' ' | b'\t'))
                    && super::python_indent::continuation_base(
                        &doc.text()[..usize::from(k.range().start())],
                        opts.tab_size,
                    )
                    .is_some();
                let text = if opts.use_tabs && aligned {
                    let vis = indent_width(&line_text[..byte_column], opts.tab_size);
                    " ".repeat(opts.tab_size.max(1) - vis % opts.tab_size.max(1))
                } else if opts.use_tabs {
                    "\t".to_string()
                } else {
                    let line = doc.line_of(k.range().start());
                    let col = doc.offset_to_position(k.range().start()).column as usize;
                    let vis = visual_column(&doc.line_text(line), col, opts.tab_size);
                    " ".repeat(opts.indent_size - (vis % opts.indent_size))
                };
                Edit::replace(k.range(), text)
            })
            .collect();
        let edits = sorted(edits);
        let carets = self
            .carets
            .iter()
            .map(|k| Caret::at(map_offset(&edits, k.range().start(), true)))
            .collect();
        self.apply(edits, carets, group, None);
    }

    pub fn indent_lines(&mut self, opts: &EditOptions) {
        let group = self.group_for(LastOp::Other);
        let unit = make_indent(opts.indent_size, opts.use_tabs, opts.tab_size);
        let edits: Vec<Edit> = self
            .selected_lines()
            .into_iter()
            .filter(|&l| {
                !self.doc.line_text(l).trim().is_empty()
                    || self.carets.len() == 1 && !self.has_selection()
            })
            .map(|l| Edit::insert(self.doc.line_range(l).start(), unit.clone()))
            .collect();
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
        self.apply(edits, carets, group, Some("Indent"));
    }

    pub fn unindent_lines(&mut self, opts: &EditOptions) {
        let group = self.group_for(LastOp::Other);
        let mut edits = Vec::new();
        for l in self.selected_lines() {
            let text = self.doc.line_text(l);
            let ws = leading_whitespace(&text);
            if ws.is_empty() {
                continue;
            }
            let width = indent_width(&text, opts.tab_size);
            let target = width.saturating_sub(1) / opts.indent_size * opts.indent_size;
            // Remove characters until the visual width is <= target.
            let mut col = 0;
            let mut keep = 0;
            for ch in ws.chars() {
                let w = rustpy_text::util::char_width(ch, opts.tab_size, col);
                if col + w > target {
                    break;
                }
                col += w;
                keep += ch.len_utf8();
            }
            let start = self.doc.line_range(l).start();
            edits.push(Edit::delete(TextRange::new(
                start + ts(keep),
                start + ts(ws.len()),
            )));
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
        self.apply(edits, carets, group, Some("Unindent"));
    }

    // ===== deletion ==========================================================

    pub fn backspace(&mut self, opts: &EditOptions) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let excess: Vec<_> = (0..self.carets.len())
            .map(|i| self.virtual_excess(i))
            .collect();
        let group = self.group_for(LastOp::Deleting);
        let mut edits = Vec::new();
        for (i, k) in self.carets.iter().enumerate() {
            if excess[i] > 0 {
                continue;
            }
            let head = k.head();
            if head == ts(0) {
                continue;
            }
            let line = self.doc.line_of(head);
            let lr = self.doc.line_range(line);
            let line_text = self.doc.line_text(line);
            let col_bytes = usize::from(head - lr.start());
            let before = &line_text[..col_bytes.min(line_text.len())];
            // Smart backspace inside leading whitespace: go to the previous indent level.
            if opts.smart_backspace
                && !before.is_empty()
                && before.chars().all(|c| c == ' ' || c == '\t')
            {
                let width = indent_width(before, opts.tab_size);
                let target = width.saturating_sub(1) / opts.indent_size * opts.indent_size;
                let mut col = 0;
                let mut keep = 0;
                for ch in before.chars() {
                    let w = rustpy_text::util::char_width(ch, opts.tab_size, col);
                    if col + w > target {
                        break;
                    }
                    col += w;
                    keep += ch.len_utf8();
                }
                edits.push(Edit::delete(TextRange::new(lr.start() + ts(keep), head)));
                continue;
            }
            let prev = super::unicode_edit::previous(self, head);
            let prev_c = self.doc.char_before(head);
            let next_c = self.doc.char_at(head);
            // Delete auto-inserted pairs together.
            let pair = matches!(
                (prev_c, next_c),
                (Some('('), Some(')'))
                    | (Some('['), Some(']'))
                    | (Some('{'), Some('}'))
                    | (Some('"'), Some('"'))
                    | (Some('\''), Some('\''))
            );
            let end = if pair && (opts.auto_close_brackets || opts.auto_close_quotes) {
                self.doc.next_boundary(head)
            } else {
                super::unicode_edit::ceil_offset(self, head)
            };
            edits.push(Edit::delete(TextRange::new(prev, end)));
        }
        let edits = sorted(edits);
        let carets: Vec<_> = self
            .carets
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let mut caret = Caret::at(map_offset(&edits, k.head(), false));
                if excess[i] > 0 {
                    caret.goal_column = Some(self.virtual_caret(i).head.column - 1);
                }
                caret
            })
            .collect();
        let gaps: Vec<_> = excess.iter().map(|gap| gap.saturating_sub(1)).collect();
        if edits.is_empty() {
            self.set_carets(carets.clone());
            self.restore_virtual_gaps(&carets, &gaps);
            self.last_op = LastOp::Other;
        } else if self.apply(edits, carets.clone(), group, None) {
            self.restore_virtual_gaps(&carets, &gaps);
        }
    }

    pub fn delete_forward(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let excess: Vec<_> = (0..self.carets.len())
            .map(|i| self.virtual_excess(i))
            .collect();
        let group = self.group_for(LastOp::Deleting);
        // Sorted: carets are not in document order (the primary one comes first).
        let edits = sorted(
            self.carets
                .iter()
                .enumerate()
                .filter(|(i, k)| excess[*i] == 0 && k.head() < self.doc.len())
                .map(|(_, k)| {
                    Edit::delete(TextRange::new(
                        super::unicode_edit::floor_offset(self, k.head()),
                        super::unicode_edit::next(self, k.head()),
                    ))
                })
                .collect(),
        );
        let carets: Vec<_> = self
            .carets
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let mut caret = Caret::at(map_offset(&edits, k.head(), false));
                if excess[i] > 0 {
                    caret.goal_column = Some(self.virtual_caret(i).head.column);
                }
                caret
            })
            .collect();
        if !edits.is_empty() && self.apply(edits, carets.clone(), group, None) {
            self.restore_virtual_gaps(&carets, &excess);
        }
    }

    pub fn delete_selection(&mut self) {
        if self.carets.iter().all(|caret| caret.range().is_empty()) {
            let positions = (0..self.carets.len())
                .map(|i| {
                    let p = self.virtual_caret(i);
                    let head = if (p.anchor.line, p.anchor.column) < (p.head.line, p.head.column) {
                        p.anchor
                    } else {
                        p.head
                    };
                    super::state::virtual_space::VirtualCaret { anchor: head, head }
                })
                .collect();
            self.set_virtual_carets(positions);
            self.last_op = LastOp::Other;
            return;
        }
        let gaps: Vec<_> =
            self.carets
                .iter()
                .enumerate()
                .map(|(i, caret)| {
                    if !caret.range().is_empty() {
                        return 0;
                    }
                    let p = self.virtual_caret(i);
                    p.anchor.column.min(p.head.column).saturating_sub(
                        self.doc.line_text(p.head.line as usize).chars().count() as u32,
                    )
                })
                .collect();
        let group = self.group_for(LastOp::Other);
        let edits = sorted(
            self.carets
                .iter()
                .filter(|k| !k.range().is_empty())
                .map(|k| Edit::delete(k.range()))
                .collect(),
        );
        let carets: Vec<_> = self
            .carets
            .iter()
            .map(|k| Caret::at(map_offset(&edits, k.range().start(), false)))
            .collect();
        if self.apply(edits, carets.clone(), group, None) {
            self.restore_virtual_gaps(&carets, &gaps);
        }
    }

    pub fn delete_to_word_start(&mut self, opts: &EditOptions) {
        let targets: Vec<TextSize> = self
            .carets
            .iter()
            .enumerate()
            .map(|(i, k)| {
                if self.virtual_excess(i) > 0 {
                    k.head()
                } else {
                    self.word_left(k.head(), opts)
                }
            })
            .collect();
        self.delete_ranges_mode(
            self.carets
                .iter()
                .zip(targets)
                .map(|(k, t)| TextRange::new(t, super::unicode_edit::ceil_offset(self, k.head())))
                .collect(),
            true,
        );
    }

    pub fn delete_to_word_end(&mut self, opts: &EditOptions) {
        let targets: Vec<TextSize> = self
            .carets
            .iter()
            .enumerate()
            .map(|(i, k)| {
                if self.virtual_excess(i) > 0 {
                    k.head()
                } else {
                    self.word_right(k.head(), opts)
                }
            })
            .collect();
        self.delete_ranges(
            self.carets
                .iter()
                .zip(targets)
                .map(|(k, t)| TextRange::new(super::unicode_edit::floor_offset(self, k.head()), t))
                .collect(),
        );
    }

    pub fn delete_to_line_start(&mut self) {
        let ranges = self
            .carets
            .iter()
            .map(|k| {
                TextRange::new(
                    self.doc.line_range(self.doc.line_of(k.head())).start(),
                    k.head(),
                )
            })
            .collect();
        self.delete_ranges(ranges);
    }

    pub fn delete_to_line_end(&mut self) {
        let ranges = self
            .carets
            .iter()
            .map(|k| {
                TextRange::new(
                    k.head(),
                    self.doc.line_range(self.doc.line_of(k.head())).end(),
                )
            })
            .collect();
        self.delete_ranges(ranges);
    }

    fn delete_ranges(&mut self, ranges: Vec<TextRange>) {
        self.delete_ranges_mode(ranges, false);
    }
    fn delete_ranges_mode(&mut self, ranges: Vec<TextRange>, collapse_virtual: bool) {
        let gaps: Vec<_> = ranges
            .iter()
            .enumerate()
            .map(|(i, range)| {
                if !collapse_virtual && range.is_empty() {
                    self.virtual_excess(i)
                } else {
                    0
                }
            })
            .collect();
        let preserved: Vec<_> = ranges
            .iter()
            .enumerate()
            .map(|(i, range)| {
                (!collapse_virtual && range.is_empty() && self.virtual_excess(i) > 0)
                    .then(|| self.virtual_caret(i).head.column)
            })
            .collect();
        let group = self.group_for(LastOp::Other);
        let edits = sorted(
            ranges
                .into_iter()
                .filter(|r| !r.is_empty())
                .map(Edit::delete)
                .collect(),
        );
        let carets: Vec<_> = self
            .carets
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let mut caret = Caret::at(map_offset(&edits, k.head(), false));
                caret.goal_column = preserved[i];
                caret
            })
            .collect();
        if edits.is_empty() {
            if collapse_virtual {
                self.set_carets(carets.clone());
                self.restore_virtual_gaps(&carets, &gaps);
            }
        } else if self.apply(edits, carets.clone(), group, None) {
            self.restore_virtual_gaps(&carets, &gaps);
        }
    }

    /// Ctrl+Y: delete all lines touched by carets.
    pub fn delete_lines(&mut self) {
        let group = self.group_for(LastOp::Other);
        let blocks = self.selected_line_blocks_with_folds();
        let edits: Vec<Edit> = blocks
            .iter()
            .map(|&(a, b)| {
                let start = self.doc.line_range_with_newline(a).start();
                let mut end = self.doc.line_range_with_newline(b).end();
                if end == self.doc.len() && start > ts(0) && b + 1 == self.doc.line_count() {
                    // Last line: also remove the preceding newline.
                    return Edit::delete(TextRange::new(start - ts(1), end));
                }
                end = end.min(self.doc.len());
                Edit::delete(TextRange::new(start, end))
            })
            .collect();
        let cols: Vec<u32> = self
            .carets
            .iter()
            .map(|k| self.doc.offset_to_position(k.head()).column)
            .collect();
        let edits = sorted(edits);
        let carets = self
            .carets
            .iter()
            .zip(cols)
            .map(|(k, col)| {
                let off = map_offset(&edits, k.range().start(), false);
                let line = self.doc.line_of(off);
                // Recompute after edit: approximate by mapping then clamping to column in the new text.
                Caret {
                    sel: rustpy_text::Selection::caret(off),
                    goal_column: Some(col),
                }
                .with_line_hint(line)
            })
            .collect();
        self.apply(edits, carets, group, Some("Delete Line"));
        let carets = self
            .carets
            .iter()
            .map(|k| {
                let line = self.doc.line_of(k.head());
                Caret::at(self.doc.position_to_offset(rustpy_core::Position::new(
                    line as u32,
                    k.goal_column.unwrap_or(0),
                )))
            })
            .collect();
        self.set_carets(carets);
    }

    // ===== line operations ===================================================

    /// Ctrl+D: duplicate selection, or the current line when nothing is selected.
    pub fn duplicate(&mut self) {
        let group = self.group_for(LastOp::Other);
        let mut edits = Vec::new();
        // (insertion point, anchor and head relative to it in the new text)
        let mut targets = Vec::new();
        // A collapsed fold is duplicated with its header line.
        let hidden = self.hidden_line_ranges();
        for k in &self.carets {
            if k.has_selection() {
                let text = self.doc.slice(k.range());
                let at = k.range().end();
                let len = ts(text.len());
                edits.push(Edit::insert(at, text));
                // The copy follows the original: select it.
                targets.push((at, k.sel.anchor + len, k.sel.head + len));
            } else {
                let line = self.doc.line_of(k.head());
                let end_line = hidden
                    .iter()
                    .find(|(first, _)| *first == line + 1)
                    .map(|&(_, last)| last)
                    .unwrap_or(line);
                let lr = TextRange::new(
                    self.doc.line_range(line).start(),
                    self.doc.line_range(end_line).end(),
                );
                let text = self.doc.slice(lr);
                edits.push(Edit::insert(lr.end(), format!("\n{text}")));
                let col = k.head() - lr.start();
                let new_head = lr.end() + ts(1) + col;
                targets.push((lr.end(), new_head, new_head));
            }
        }
        let edits = sorted(edits);
        // Text inserted for carets earlier in the document shifts the later ones.
        let carets = targets
            .into_iter()
            .map(|(at, a, h)| {
                let shift = map_offset(&edits, at, false) - at;
                Caret::new(a + shift, h + shift)
            })
            .collect();
        self.apply(edits, carets, group, Some("Duplicate"));
    }

    /// Alt+Shift+Up/Down: move selected lines.
    pub fn move_lines(&mut self, down: bool) {
        let blocks = self.selected_line_blocks_with_folds();
        if blocks.is_empty() {
            return;
        }
        // A collapsed fold next to the block is passed as a whole.
        let hidden = self.hidden_line_ranges();
        let last_line = self.doc.line_count() - 1;
        if (!down && blocks[0].0 == 0) || (down && blocks.last().unwrap().1 >= last_line) {
            return;
        }
        let group = self.group_for(LastOp::Other);
        let mut edits = Vec::new();
        let mut shifts: Vec<(TextRange, i64)> = Vec::new();
        for &(a, b) in &blocks {
            let block =
                TextRange::new(self.doc.line_range(a).start(), self.doc.line_range(b).end());
            let block_text = self.doc.slice(block);
            if down {
                let other_end = hidden
                    .iter()
                    .find(|(first, _)| *first == b + 2)
                    .map(|&(_, last)| last)
                    .unwrap_or(b + 1);
                let other = TextRange::new(
                    self.doc.line_range(b + 1).start(),
                    self.doc.line_range(other_end).end(),
                );
                let other_text = self.doc.slice(other);
                let whole = TextRange::new(block.start(), other.end());
                edits.push(Edit::replace(whole, format!("{other_text}\n{block_text}")));
                shifts.push((block, other_text.len() as i64 + 1));
            } else {
                let other_start = hidden
                    .iter()
                    .find(|(_, last)| *last == a - 1)
                    .map(|&(first, _)| first - 1)
                    .unwrap_or(a - 1);
                let other = TextRange::new(
                    self.doc.line_range(other_start).start(),
                    self.doc.line_range(a - 1).end(),
                );
                let other_text = self.doc.slice(other);
                let whole = TextRange::new(other.start(), block.end());
                edits.push(Edit::replace(whole, format!("{block_text}\n{other_text}")));
                shifts.push((block, -(other_text.len() as i64 + 1)));
            }
        }
        let shift = |off: TextSize| -> TextSize {
            for (block, d) in &shifts {
                if block.contains_inclusive(off) {
                    return TextSize::new((u32::from(off) as i64 + d) as u32);
                }
            }
            off
        };
        let carets = self
            .carets
            .iter()
            .map(|k| Caret::new(shift(k.sel.anchor), shift(k.sel.head)))
            .collect();
        self.apply(sorted(edits), carets, group, Some("Move Line"));
    }

    /// Ctrl+Shift+J: join each selected line with the next.
    pub fn join_lines(&mut self) {
        self.join_lines_with_options(&EditOptions::default());
    }

    pub fn join_lines_with_options(&mut self, opts: &EditOptions) {
        let group = self.group_for(LastOp::Other);
        let mut edits = Vec::new();
        // Caret for each block: the first join point (absolute, pre-edit start + offset).
        let mut joins: Vec<(TextSize, TextSize, usize)> = Vec::new();
        let comment = self.language.line_comment();
        let python = self.language.is_pythonic();
        let line_count = self.doc.line_count();
        for &(a, b) in &self.selected_line_blocks() {
            let last = if a == b { a } else { b - 1 };
            let end_line = (last + 1).min(line_count.saturating_sub(1));
            if end_line <= a {
                continue;
            }
            // One edit per block: joining pairwise produced overlapping edits
            // around whitespace-only lines, and the whole batch was rejected.
            let mut joined = self.doc.line_text(a).to_string();
            let mut first_join = None;
            let mut previous_indent = indent_width(&joined, opts.tab_size);
            for l in a + 1..=end_line {
                let next = self.doc.line_text(l);
                if python {
                    let next_indent = indent_width(&next, opts.tab_size);
                    if let Some((result, point)) =
                        super::join_lines::python(&joined, &next, previous_indent, next_indent)
                    {
                        first_join.get_or_insert(point);
                        joined = result;
                    } else {
                        joined.push('\n');
                        joined.push_str(&next);
                    }
                    if !next.trim().is_empty() {
                        previous_indent = next_indent;
                    }
                    continue;
                }
                let cur = joined.trim_end().to_string();
                let mut next_trimmed = next.trim_start();
                if let Some(c) = comment {
                    if cur.trim_start().starts_with(c) && next_trimmed.starts_with(c) {
                        next_trimmed = next_trimmed[c.len()..].trim_start();
                    }
                }
                let sep = if cur.trim().is_empty()
                    || next_trimmed.is_empty()
                    || cur.ends_with(['(', '[', '{'])
                    || next_trimmed.starts_with([')', ']', '}', ','])
                {
                    ""
                } else {
                    " "
                };
                first_join.get_or_insert(cur.len());
                joined = format!("{cur}{sep}{next_trimmed}");
            }
            if first_join.is_none() {
                continue;
            }
            let range = TextRange::new(
                self.doc.line_range(a).start(),
                self.doc.line_range(end_line).end(),
            );
            joins.push((range.start(), range.end(), first_join.unwrap_or(0)));
            edits.push(Edit::replace(range, joined));
        }
        if edits.is_empty() {
            return;
        }
        let edits = sorted(edits);
        let carets = self
            .carets
            .iter()
            .map(|k| {
                let off = k.range().start();
                match joins.iter().find(|(s, e, _)| *s <= off && off <= *e) {
                    Some((s, _, j)) => Caret::at(map_offset(&edits, *s, false) + ts(*j)),
                    None => Caret::at(map_offset(&edits, off, true)),
                }
            })
            .collect();
        self.apply(edits, carets, group, Some("Join Lines"));
    }

    /// Ctrl+/: toggle line comments on selected lines (block-comment pairs
    /// around each line in languages without line comments, e.g. HTML).
    pub fn toggle_line_comment(&mut self) {
        self.toggle_line_comment_with_style(false, true);
    }

    pub fn toggle_line_comment_with_style(&mut self, first_column: bool, space: bool) {
        let Some(prefix) = self.language.line_comment() else {
            if let Some((open, close)) = self.language.block_comment() {
                self.toggle_lines_block_wrapped(open, close);
            }
            return;
        };
        let group = self.group_for(LastOp::Other);
        let lines: Vec<usize> = self.selected_lines();
        let non_blank: Vec<usize> = lines
            .iter()
            .copied()
            .filter(|&l| !self.doc.line_text(l).trim().is_empty())
            .collect();
        let targets = if non_blank.is_empty() {
            lines.clone()
        } else {
            non_blank
        };
        let all_commented = targets
            .iter()
            .all(|&l| self.doc.line_text(l).trim_start().starts_with(prefix));
        let mut edits = Vec::new();
        if all_commented {
            for &l in &targets {
                let text = self.doc.line_text(l);
                let ws = leading_whitespace(&text).len();
                let after = &text[ws + prefix.len()..];
                let extra = if after.starts_with(' ') && (space || !first_column) {
                    1
                } else {
                    0
                };
                let start = self.doc.line_range(l).start() + ts(ws);
                edits.push(Edit::delete(TextRange::new(
                    start,
                    start + ts(prefix.len() + extra),
                )));
            }
        } else {
            let min_indent = if first_column {
                0
            } else {
                targets
                    .iter()
                    .map(|&l| leading_whitespace(&self.doc.line_text(l)).len())
                    .min()
                    .unwrap_or(0)
            };
            for &l in &targets {
                edits.push(Edit::insert(
                    self.doc.line_range(l).start() + ts(min_indent),
                    format!("{prefix}{}", if space { " " } else { "" }),
                ));
            }
        }
        let edits = sorted(edits);
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

    /// Ctrl+Shift+U: toggle case of selections (or word at caret).
    pub fn toggle_case(&mut self) {
        let transform = |text: &str| {
            let all_upper = text
                .chars()
                .filter(|c| c.is_alphabetic())
                .all(|c| c.is_uppercase());
            if all_upper {
                text.to_lowercase()
            } else {
                text.to_uppercase()
            }
        };
        // A no-op must not disturb selections, paired-delimiter tracking or history.
        if !self.carets.iter().any(|caret| {
            let range = if caret.has_selection() {
                caret.range()
            } else {
                self.doc
                    .word_range_at(caret.head())
                    .unwrap_or(caret.range())
            };
            let text = self.doc.slice(range);
            transform(&text) != text
        }) {
            return;
        }
        // Capture the original carets before expanding words. Read-only commands
        // must not select text, and undo must restore the pre-command selection.
        let token = match self.begin_transaction("Toggle Case") {
            Ok(token) => token,
            Err(error) => {
                if error == rustpy_text::HistoryError::ReadOnly {
                    self.read_only_refused = true;
                }
                return;
            }
        };
        // Expand each empty caret, including mixed selections/carets. set_carets
        // coalesces overlapping words before edits are constructed.
        self.select_word_at_carets();
        let edits: Vec<Edit> = self
            .carets
            .iter()
            .filter_map(|k| {
                let t = self.doc.slice(k.range());
                let replacement = transform(&t);
                (replacement != t).then(|| Edit::replace(k.range(), replacement))
            })
            .collect();
        if edits.is_empty() {
            self.rollback_transaction(token)
                .expect("case command owns its transaction");
            return;
        }
        let group = self.group_for(LastOp::Other);
        let edits = sorted(edits);
        let carets = self
            .carets
            .iter()
            .map(|k| {
                Caret::new(
                    map_offset(&edits, k.sel.anchor, k.sel.anchor > k.sel.head),
                    map_offset(&edits, k.sel.head, k.sel.head > k.sel.anchor),
                )
            })
            .collect();
        if self.apply(edits, carets, group, Some("Toggle Case")) {
            self.commit_transaction(token)
                .expect("case command owns its transaction");
        } else {
            self.rollback_transaction(token)
                .expect("case command owns its transaction");
        }
    }

    // ===== clipboard =========================================================

    /// Text that *Copy* places on the clipboard (whole line when nothing is selected).
    pub fn copy_text(&self) -> String {
        if self.has_selection() {
            self.carets
                .iter()
                .enumerate()
                .map(|(i, _)| self.selected_caret_text(i))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            let lines = self.selected_lines();
            lines
                .iter()
                .map(|&l| format!("{}\n", self.doc.line_text(l)))
                .collect()
        }
    }

    pub fn copy_payload(&self) -> ClipboardPayload {
        ClipboardPayload {
            text: self.copy_text(),
            pieces: self.has_selection().then(|| {
                self.carets
                    .iter()
                    .enumerate()
                    .map(|(i, _)| self.selected_caret_text(i))
                    .collect()
            }),
        }
    }

    pub fn paste_payload(
        &mut self,
        text: &str,
        payload: Option<&ClipboardPayload>,
        opts: &EditOptions,
    ) {
        let normalized = rustpy_text::LineEnding::normalize_to_lf(text);
        let pieces = payload.and_then(|p| p.matching_pieces(&normalized, self.carets.len()));
        if self.column_mode
            || self
                .block
                .is_some_and(|(a, h)| self.block_carets(a, h) == self.carets)
            || pieces.is_none()
        {
            self.paste(text, opts);
            return;
        }
        self.paste_pieces(
            pieces.unwrap(),
            opts,
            self.reindents_paste(&normalized, opts),
        );
    }

    /// Cut: returns the clipboard text and deletes it.
    pub fn cut(&mut self) -> String {
        let text = self.copy_text();
        if self.has_selection() {
            self.delete_selection();
        } else {
            self.delete_lines();
        }
        self.last_op = LastOp::Other;
        text
    }

    /// Paste: distributes lines over carets when counts match.
    pub fn paste(&mut self, text: &str, opts: &EditOptions) {
        let text = rustpy_text::LineEnding::normalize_to_lf(text).into_owned();
        if self.virtual_space.allow_in_block
            && (self.column_mode
                || self
                    .block
                    .is_some_and(|(a, h)| self.block_carets(a, h) == self.carets))
        {
            self.paste_column(&text);
            return;
        }
        if !text.is_empty() && self.edit_in_virtual_space("Paste", |state| state.paste(&text, opts))
        {
            return;
        }
        let group = self.group_for(LastOp::Other);
        let lines: Vec<&str> = text.lines().collect();
        let distribute = self.carets.len() > 1 && lines.len() == self.carets.len();
        let whole_lines = text.ends_with('\n') && !self.has_selection() && !distribute;
        // Reformat on paste (Python): shift the block to the caret's indentation.
        let reindent = self.reindents_paste(&text, opts);
        let carets_before = self.carets.clone();
        if whole_lines {
            // Line-wise paste (text copied as whole lines) goes above the caret line.
            let edits: Vec<Edit> = carets_before
                .iter()
                .map(|k| {
                    let line = self.doc.line_of(k.head());
                    let t = if reindent {
                        self.reindented_line_paste(line, &text, opts)
                    } else {
                        text.clone()
                    };
                    Edit::insert(self.doc.line_range(line).start(), t)
                })
                .collect();
            let edits = sorted(edits);
            let edits = if let Some(format_options) = self.paste_reformat_options(opts) {
                super::paste_format::reformat_replacements(&self.doc.text(), edits, &format_options)
            } else {
                edits
            };
            let carets = carets_before
                .iter()
                .map(|k| Caret::at(map_offset(&edits, k.head(), true)))
                .collect();
            self.apply(edits, carets, group, Some("Paste"));
            return;
        }
        let pieces: Vec<String> = if distribute {
            lines.iter().map(|s| s.to_string()).collect()
        } else {
            vec![text.clone(); self.carets.len()]
        };
        self.paste_pieces(&pieces, opts, reindent);
    }

    fn paste_reformat_options(
        &mut self,
        opts: &EditOptions,
    ) -> Option<rustpy_format::FormatOptions> {
        let carets_before = self.carets.clone();
        // Cython's normal editor syntax cache is empty; use Python tokens only
        // for conservative literal detection, never to change its language mode.
        let cython_parse = (opts.paste_reformat
            && self.language == super::Language::Cython
            && !self.is_large_file())
        .then(|| rustpy_syntax::parse(&self.doc.text()));
        let reformat = opts.paste_reformat
            && self.language.is_pythonic()
            && !self.is_large_file()
            && !carets_before.iter().any(|caret| {
                let parse = match cython_parse.as_ref() {
                    Some(parse) => parse,
                    None => self.syntax(),
                };
                super::syntax_support::token_at(&parse.syntax_node(), caret.head()).any(|token| {
                    matches!(
                        token.kind(),
                        rustpy_syntax::SyntaxKind::STRING
                            | rustpy_syntax::SyntaxKind::FSTRING_MIDDLE
                            | rustpy_syntax::SyntaxKind::TSTRING_MIDDLE
                    )
                })
            });
        let mut format_options = opts.paste_format_options.clone();
        format_options.indent = opts.indent_size.max(1);
        format_options.max_line_length = if opts.right_margin == 0 {
            usize::MAX / 4
        } else {
            opts.right_margin.max(40)
        };
        reformat.then_some(format_options)
    }

    fn paste_pieces(&mut self, pieces: &[String], opts: &EditOptions, reindent: bool) {
        if pieces.iter().any(|piece| !piece.is_empty())
            && self
                .edit_in_virtual_space("Paste", |state| state.paste_pieces(pieces, opts, reindent))
        {
            return;
        }
        let group = self.group_for(LastOp::Other);
        let carets_before = self.carets.clone();
        let mut edits: Vec<Edit> = carets_before
            .iter()
            .zip(pieces)
            .map(|(k, p)| {
                let (range, t) = if reindent {
                    self.reindented_paste(k.range(), p, opts)
                } else {
                    (k.range(), p.clone())
                };
                Edit::replace(range, t)
            })
            .collect();
        edits = sorted(edits);
        if edits
            .windows(2)
            .any(|w| w[1].range.start() < w[0].range.end())
        {
            // Widened ranges of carets on one line collide: paste without re-indenting.
            edits = sorted(
                carets_before
                    .iter()
                    .zip(pieces)
                    .map(|(k, p)| Edit::replace(k.range(), p.clone()))
                    .collect(),
            );
        }
        if let Some(format_options) = self.paste_reformat_options(opts) {
            edits = super::paste_format::reformat_replacements(
                &self.doc.text(),
                edits,
                &format_options,
            );
        }
        let carets = carets_before
            .iter()
            .map(|k| Caret::at(map_offset(&edits, k.range().start(), true)))
            .collect();
        self.apply(edits, carets, group, Some("Paste"));
    }

    /// Retains rectangle intent for pointer selection, including clipped short lines.
    pub fn set_block_selection(&mut self, anchor: Position, head: Position) {
        self.set_carets(self.block_carets(anchor, head));
        self.block = Some((anchor, head));
        self.remember_virtual_block(anchor, head);
    }

    fn paste_column(&mut self, text: &str) {
        let (anchor, head) = self
            .block
            .filter(|(a, h)| self.block_carets(*a, *h) == self.carets)
            .unwrap_or_else(|| {
                let first = self
                    .carets
                    .iter()
                    .min_by_key(|c| c.range().start())
                    .unwrap();
                let last = self.carets.iter().max_by_key(|c| c.range().end()).unwrap();
                (
                    self.doc.offset_to_position(first.sel.anchor),
                    self.doc.offset_to_position(last.head()),
                )
            });
        let plan = column_paste::plan(&self.doc.text(), anchor, head, text);
        let group = self.group_for(LastOp::Other);
        if self.apply(
            vec![Edit::replace(
                TextRange::new(ts(plan.start), ts(plan.end)),
                plan.text,
            )],
            plan.carets
                .into_iter()
                .map(|offset| Caret::at(ts(offset)))
                .collect(),
            group,
            Some("Paste"),
        ) {
            self.block = None;
            self.scroll_to_caret = true;
        }
    }

    // ===== caret movement ====================================================

    fn move_carets(&mut self, select: bool, f: impl Fn(&Self, &Caret) -> TextSize) {
        let select = select || self.sticky_selection;
        let moved: Vec<Caret> = self
            .carets
            .iter()
            .map(|k| {
                let head = f(self, k);
                Caret {
                    sel: if select {
                        rustpy_text::Selection::new(k.sel.anchor, head)
                    } else {
                        rustpy_text::Selection::caret(head)
                    },
                    goal_column: None,
                }
            })
            .collect();
        if self.virtual_space.enabled {
            let positions = moved
                .iter()
                .enumerate()
                .map(|(i, caret)| {
                    let head = self.doc.offset_to_position(caret.head());
                    super::state::virtual_space::VirtualCaret {
                        anchor: if select {
                            self.virtual_caret(i).anchor
                        } else {
                            head
                        },
                        head,
                    }
                })
                .collect();
            self.set_virtual_carets(positions);
        } else {
            self.set_carets(moved);
        }
        self.last_op = LastOp::Other;
    }

    pub fn move_left(&mut self, select: bool) {
        let select = select || self.sticky_selection;
        if self.move_virtual_horizontal(-1, select) {
            return;
        }
        self.move_carets(select, |s, k| {
            if !select && k.has_selection() {
                k.range().start()
            } else {
                super::unicode_edit::previous(s, k.head())
            }
        });
    }

    pub fn move_right(&mut self, select: bool) {
        let select = select || self.sticky_selection;
        if self.move_virtual_horizontal(1, select) {
            return;
        }
        self.move_carets(select, |s, k| {
            if !select && k.has_selection() {
                k.range().end()
            } else {
                super::unicode_edit::next(s, k.head())
            }
        });
    }

    /// Moves carets `delta` visible lines up/down (folded lines are skipped);
    /// with soft wraps on, `delta` visual rows (the rows of a wrapped line count
    /// one by one).
    pub fn move_vertical(&mut self, delta: i64, select: bool) {
        let select = select || self.sticky_selection;
        let virtual_before: Vec<_> = (0..self.carets.len())
            .map(|i| self.virtual_caret(i))
            .collect();
        let hidden = self.hidden_line_ranges();
        let line_count = self.doc.line_count();
        let last_hidden = hidden.last().is_some_and(|(_, b)| *b + 1 >= line_count);
        let moved: Vec<Caret> = self
            .carets
            .iter()
            .map(|k| {
                let pos = self.doc.offset_to_position(k.head());
                let (target, goal) = match self.soft_wrap {
                    Some(wrap) => {
                        self.visual_row_target(k.head(), k.goal_column, delta, wrap, &hidden)
                    }
                    None => {
                        let goal = k.goal_column.unwrap_or(pos.column);
                        let line = step_visible_lines(pos.line as usize, delta, line_count, &hidden)
                            as u32;
                        (
                            (line != pos.line || delta == 0).then(|| {
                                self.doc
                                    .position_to_offset(rustpy_core::Position::new(line, goal))
                            }),
                            goal,
                        )
                    }
                };
                // No line (row) further up/down: the start or end of the text.
                let head = target.unwrap_or_else(|| {
                    if delta < 0 {
                        ts(0)
                    } else if last_hidden {
                        // The end of the file is folded away: stop at the end of this line.
                        self.doc.line_range(pos.line as usize).end()
                    } else {
                        self.doc.len()
                    }
                });
                let head = super::unicode_edit::floor_offset(self, head);
                Caret {
                    sel: if select {
                        rustpy_text::Selection::new(k.sel.anchor, head)
                    } else {
                        rustpy_text::Selection::caret(head)
                    },
                    goal_column: Some(goal),
                }
            })
            .collect();
        if self.virtual_space.enabled {
            let positions = moved
                .iter()
                .zip(&virtual_before)
                .map(|(caret, before)| {
                    let mut head = self.doc.offset_to_position(caret.head());
                    if let Some(wrap) = self.soft_wrap {
                        let length = self.doc.line_text(head.line as usize).chars().count() as u32;
                        if head.column == length {
                            let display = self
                                .wrapped_line(head.line as usize, wrap)
                                .row_col(length as usize)
                                .1 as u32;
                            head.column +=
                                caret.goal_column.unwrap_or(display).saturating_sub(display);
                        }
                    } else {
                        head.column = caret.goal_column.unwrap_or(head.column);
                    }
                    super::state::virtual_space::VirtualCaret {
                        anchor: if select { before.anchor } else { head },
                        head,
                    }
                })
                .collect();
            self.set_virtual_carets(positions);
        } else {
            self.set_carets(moved);
        }
        self.last_op = LastOp::Other;
    }

    /// The soft-wrap layout of `line` as the view draws it (tabs, inlay hints).
    pub(super) fn wrapped_line(&self, line: usize, wrap: SoftWrap) -> WrappedLine {
        let lr = self.doc.line_range(line);
        let text = self.doc.line_text(line);
        let first = self.inlay_hints.partition_point(|h| h.offset < lr.start());
        let end = self.inlay_hints.partition_point(|h| h.offset <= lr.end());
        let hints: Vec<(usize, usize)> = self.inlay_hints[first..end.max(first)]
            .iter()
            .map(|h| {
                (
                    rustpy_core::floor_char_boundary(&text, usize::from(h.offset - lr.start())),
                    h.label.chars().count(),
                )
            })
            .collect();
        WrappedLine::new(&text, &hints, wrap)
    }

    /// With soft wraps: the caret's visual row within its line and its display
    /// column within that row (`None` while soft wraps are off).
    pub fn visual_row_col(&self, head: TextSize) -> Option<(usize, usize)> {
        let wrap = self.soft_wrap?;
        let pos = self.doc.offset_to_position(head);
        Some(
            self.wrapped_line(pos.line as usize, wrap)
                .row_col(pos.column as usize),
        )
    }

    /// With soft wraps: the offset at display column `x` of visual row `row` of `line`.
    pub fn offset_at_visual_row(&self, line: usize, row: usize, x: usize) -> Option<TextSize> {
        let wrap = self.soft_wrap?;
        let col = self.wrapped_line(line, wrap).col_at(row, x);
        Some(
            self.doc
                .position_to_offset(rustpy_core::Position::new(line as u32, col as u32)),
        )
    }

    /// Where a caret at `head` lands `delta` visual rows away with soft wraps
    /// on (folded lines skipped), keeping display column `goal` within the row
    /// (default: the caret's own). Returns the target (`None` when there is no
    /// row in that direction) and the goal column.
    fn visual_row_target(
        &self,
        head: TextSize,
        goal: Option<u32>,
        delta: i64,
        wrap: SoftWrap,
        hidden: &[(usize, usize)],
    ) -> (Option<TextSize>, u32) {
        let line_count = self.doc.line_count();
        let pos = self.doc.offset_to_position(head);
        let mut line = pos.line as usize;
        let mut layout = self.wrapped_line(line, wrap);
        let (mut row, x) = layout.row_col(pos.column as usize);
        let goal = goal.map_or(x, |g| g as usize);
        let mut moved = delta == 0;
        for _ in 0..delta.unsigned_abs() {
            if delta > 0 && row + 1 < layout.rows() {
                row += 1;
            } else if delta < 0 && row > 0 {
                row -= 1;
            } else {
                let next = step_visible_lines(line, delta.signum(), line_count, hidden);
                if next == line {
                    break;
                }
                line = next;
                layout = self.wrapped_line(line, wrap);
                row = if delta > 0 { 0 } else { layout.rows() - 1 };
            }
            moved = true;
        }
        let goal32 = u32::try_from(goal).unwrap_or(u32::MAX);
        if !moved {
            return (None, goal32);
        }
        let col = layout.col_at(row, goal);
        (
            Some(
                self.doc
                    .position_to_offset(rustpy_core::Position::new(line as u32, col as u32)),
            ),
            goal32,
        )
    }

    pub fn move_word_left(&mut self, select: bool, opts: &EditOptions) {
        self.move_carets(select, |s, k| s.word_left(k.head(), opts));
    }

    pub fn move_word_right(&mut self, select: bool, opts: &EditOptions) {
        self.move_carets(select, |s, k| s.word_right(k.head(), opts));
    }

    pub fn move_line_start(&mut self, select: bool, opts: &EditOptions) {
        self.move_carets(select, |s, k| {
            let line = s.doc.line_of(k.head());
            let lr = s.doc.line_range(line);
            if !opts.smart_home {
                return lr.start();
            }
            let text = s.doc.line_text(line);
            let first_non_ws = lr.start() + ts(leading_whitespace(&text).len());
            if k.head() == first_non_ws
                || first_non_ws == lr.end() && k.head() != lr.start() && text.trim().is_empty()
            {
                lr.start()
            } else {
                first_non_ws
            }
        });
    }

    pub fn move_line_end(&mut self, select: bool) {
        self.move_carets(select, |s, k| {
            s.doc.line_range(s.doc.line_of(k.head())).end()
        });
    }

    pub fn move_doc_start(&mut self, select: bool) {
        self.move_carets(select, |_, _| ts(0));
    }

    pub fn move_doc_end(&mut self, select: bool) {
        self.move_carets(select, |s, _| s.doc.len());
    }

    pub fn page(&mut self, down: bool, select: bool, opts: &EditOptions) {
        let n = opts.page_lines.max(1) as i64;
        self.move_vertical(if down { n } else { -n }, select);
    }

    pub fn word_left(&self, from: TextSize, opts: &EditOptions) -> TextSize {
        super::unicode_edit::floor_offset(self, self.word_left_scalar(from, opts))
    }

    fn word_left_scalar(&self, from: TextSize, opts: &EditOptions) -> TextSize {
        let mut pos = from;
        let Some(first) = self.doc.char_before(pos) else {
            return pos;
        };
        if first == '\n' {
            return self.doc.prev_boundary(pos);
        }
        // Skip whitespace, then a run of the same class.
        while let Some(c) = self.doc.char_before(pos) {
            if char_class(c) != CharClass::Whitespace || c == '\n' {
                break;
            }
            pos = self.doc.prev_boundary(pos);
        }
        let Some(c0) = self.doc.char_before(pos) else {
            return pos;
        };
        if c0 == '\n' {
            return pos;
        }
        let class = char_class(c0);
        while let Some(c) = self.doc.char_before(pos) {
            if char_class(c) != class || c == '\n' {
                break;
            }
            if opts.camel_humps && class == CharClass::Word {
                if let Some(prev) = self.doc.char_before(self.doc.prev_boundary(pos)) {
                    if is_camel_hump(prev, c, self.doc.char_at(pos)) {
                        pos = self.doc.prev_boundary(pos);
                        break;
                    }
                }
            }
            pos = self.doc.prev_boundary(pos);
        }
        pos
    }

    pub fn word_right(&self, from: TextSize, opts: &EditOptions) -> TextSize {
        super::unicode_edit::ceil_offset(self, self.word_right_scalar(from, opts))
    }

    fn word_right_scalar(&self, from: TextSize, opts: &EditOptions) -> TextSize {
        let mut pos = from;
        let Some(first) = self.doc.char_at(pos) else {
            return pos;
        };
        if first == '\n' {
            return self.doc.next_boundary(pos);
        }
        while let Some(c) = self.doc.char_at(pos) {
            if char_class(c) != CharClass::Whitespace || c == '\n' {
                break;
            }
            pos = self.doc.next_boundary(pos);
        }
        let Some(c0) = self.doc.char_at(pos) else {
            return pos;
        };
        if c0 == '\n' {
            return pos;
        }
        let class = char_class(c0);
        let mut prev = c0;
        pos = self.doc.next_boundary(pos);
        while let Some(c) = self.doc.char_at(pos) {
            if char_class(c) != class || c == '\n' {
                break;
            }
            if opts.camel_humps
                && class == CharClass::Word
                && is_camel_hump(prev, c, self.doc.char_at(self.doc.next_boundary(pos)))
            {
                break;
            }
            prev = c;
            pos = self.doc.next_boundary(pos);
        }
        pos
    }

    // ===== selection =========================================================

    pub fn select_all(&mut self) {
        self.set_carets(vec![Caret::new(ts(0), self.doc.len())]);
        self.last_op = LastOp::Other;
    }

    pub fn select_word_at_carets(&mut self) {
        let carets = self
            .carets
            .iter()
            .map(|k| match self.doc.word_range_at(k.head()) {
                Some(r) if !k.has_selection() => Caret::new(
                    super::unicode_edit::floor_offset(self, r.start()),
                    super::unicode_edit::ceil_offset(self, r.end()),
                ),
                _ => *k,
            })
            .collect();
        self.set_carets(carets);
    }

    /// Ctrl+W: word → syntax node levels (expression, statement, block,
    /// definition) → whole document; plain text falls back to line levels.
    pub fn extend_selection(&mut self) {
        self.extend_selection_with_opts(&EditOptions::default());
    }

    pub fn extend_selection_with_opts(&mut self, opts: &EditOptions) {
        // Moving the caret since the last Ctrl+W starts a new history.
        if self.selection_extended.as_ref() != Some(&self.carets) {
            self.selection_history.clear();
        }
        self.extend_selection_once(opts);
        self.selection_extended = Some(self.carets.clone());
    }

    fn extend_selection_once(&mut self, opts: &EditOptions) {
        self.selection_history.push(self.carets.clone());
        if self.language == super::Language::Python {
            let parse = self.syntax().clone();
            let doc_len = self.doc.len();
            let carets = self
                .carets
                .iter()
                .map(|k| {
                    let r = k.range();
                    if !k.has_selection() {
                        if let Some(w) = self.word_range_with_opts(k.head(), opts) {
                            return Caret::new(w.start(), w.end());
                        }
                    }
                    match super::syntax_support::extend_selection(&parse, r) {
                        Some(next) => Caret::new(next.start(), next.end()),
                        None => Caret::new(ts(0), doc_len),
                    }
                })
                .collect();
            self.set_carets(carets);
            self.last_op = LastOp::Other;
            return;
        }
        let carets = self
            .carets
            .iter()
            .map(|k| {
                let r = k.range();
                if !k.has_selection() {
                    if let Some(w) = self.word_range_with_opts(k.head(), opts) {
                        return Caret::new(w.start(), w.end());
                    }
                }
                let line = self.doc.line_of(r.start());
                let lr = self.doc.line_range(line);
                let text = self.doc.line_text(line);
                let content =
                    TextRange::new(lr.start() + ts(leading_whitespace(&text).len()), lr.end());
                if r != content && content.contains_range(r) && !content.is_empty() {
                    Caret::new(content.start(), content.end())
                } else if r != lr && lr.contains_range(r) {
                    Caret::new(lr.start(), lr.end())
                } else {
                    Caret::new(ts(0), self.doc.len())
                }
            })
            .collect();
        self.set_carets(carets);
        self.last_op = LastOp::Other;
    }

    pub fn shrink_selection(&mut self) {
        // Only undoes Ctrl+W steps when the selection is still what they made.
        if self.selection_extended.as_ref() != Some(&self.carets) {
            self.selection_history.clear();
        }
        if let Some(prev) = self.selection_history.pop() {
            self.set_carets(prev);
            self.selection_extended = Some(self.carets.clone());
        } else {
            self.collapse_to_primary();
        }
    }

    /// The occurrence session if it still matches the carets (else a new one
    /// that continues from the selections as they are).
    fn occurrence_session(&self) -> (Vec<TextRange>, bool) {
        match &self.occurrences {
            Some((added, words))
                if !added.is_empty()
                    && added
                        .iter()
                        .all(|r| self.carets.iter().any(|k| k.range() == *r))
                    && added.len() == self.carets.len() =>
            {
                (added.clone(), *words)
            }
            _ => {
                let mut ranges: Vec<TextRange> = self.carets.iter().map(|k| k.range()).collect();
                // The primary caret is the newest when nothing else is known.
                let n = ranges.len().min(1);
                ranges.rotate_left(n);
                (ranges, false)
            }
        }
    }

    /// Offsets of `needle` in `text` from `from` on, wrapping around; with
    /// `words`, only matches that stand as whole identifiers.
    fn occurrence_after(text: &str, needle: &str, from: usize, words: bool) -> Vec<usize> {
        let is_word = |i: usize| {
            let before = text[..i].chars().next_back();
            let after = text[i + needle.len()..].chars().next();
            !before.is_some_and(rustpy_core::is_ident_continue)
                && !after.is_some_and(rustpy_core::is_ident_continue)
        };
        let all: Vec<usize> = text
            .match_indices(needle)
            .map(|(i, _)| i)
            .filter(|&i| !words || is_word(i))
            .collect();
        let split = all.partition_point(|&i| i < from);
        all[split..].iter().chain(&all[..split]).copied().collect()
    }

    /// Alt+J: select word at caret, or add a caret at the next occurrence of the selection.
    pub fn add_selection_for_next_occurrence(&mut self) {
        if !self.has_selection() {
            self.select_word_at_carets();
            self.occurrences = Some((self.carets.iter().map(|k| k.range()).collect(), true));
            return;
        }
        let (mut added, words) = self.occurrence_session();
        let Some(&last) = added.last() else { return };
        let needle = self.doc.slice(last);
        if needle.is_empty() {
            return;
        }
        let text = self.doc.text();
        // Search on from the occurrence added last; wrap to the top once.
        let next = Self::occurrence_after(&text, &needle, usize::from(last.end()), words)
            .into_iter()
            .map(|i| TextRange::at(ts(i), ts(needle.len())))
            .find(|r| !self.carets.iter().any(|k| k.range() == *r));
        if let Some(r) = next {
            let mut carets = self.carets.clone();
            carets.push(Caret::new(r.start(), r.end()));
            self.set_carets(carets);
            added.push(r);
            self.occurrences = Some((added, words));
        }
    }

    pub fn select_all_occurrences(&mut self) {
        let words = !self.has_selection();
        if words {
            self.select_word_at_carets();
        }
        let needle = self.doc.slice(self.primary().range());
        if needle.is_empty() {
            return;
        }
        let text = self.doc.text();
        let mut carets = vec![self.primary()];
        for i in Self::occurrence_after(&text, &needle, 0, words) {
            let r = TextRange::at(ts(i), ts(needle.len()));
            if r != self.primary().range() {
                carets.push(Caret::new(r.start(), r.end()));
            }
        }
        self.set_carets(carets);
        self.occurrences = None;
    }

    /// Carets of a block selection between two (line, column) corners;
    /// columns are clipped to each line's length.
    fn block_carets(&self, anchor: Position, head: Position) -> Vec<Caret> {
        let (l0, l1) = (anchor.line.min(head.line), anchor.line.max(head.line));
        (l0..=l1)
            .map(|line| {
                let len = self.doc.line_text(line as usize).chars().count() as u32;
                let s = self
                    .doc
                    .position_to_offset(Position::new(line, anchor.column.min(len)));
                let e = self
                    .doc
                    .position_to_offset(Position::new(line, head.column.min(len)));
                Caret::new(s, e)
            })
            .collect()
    }

    /// Shift+arrows in column selection mode: grows the block by lines and columns.
    pub fn extend_block_selection(&mut self, lines: i32, columns: i32) {
        let current = self
            .block
            .filter(|(a, h)| self.block_carets(*a, *h) == self.carets);
        let (anchor, head) = current.unwrap_or_else(|| {
            (
                self.doc.offset_to_position(self.primary().sel.anchor),
                self.doc.offset_to_position(self.primary().head()),
            )
        });
        let last_line = self.doc.line_count().saturating_sub(1) as i64;
        let mut head = Position::new(
            (head.line as i64 + lines as i64).clamp(0, last_line) as u32,
            (head.column as i64 + columns as i64).max(0) as u32,
        );
        if !self.virtual_space.allow_in_block {
            head.column = head
                .column
                .min(self.doc.line_text(head.line as usize).chars().count() as u32);
        }
        let carets = self.block_carets(anchor, head);
        if !carets.is_empty() {
            self.set_carets(carets);
            self.block = Some((anchor, head));
            self.remember_virtual_block(anchor, head);
            self.scroll_to_caret = true;
        }
    }

    /// Alt+Shift+J: removes the occurrence added last.
    pub fn unselect_occurrence(&mut self) {
        if self.carets.len() <= 1 {
            return;
        }
        let (mut added, words) = self.occurrence_session();
        let Some(last) = added.pop() else { return };
        let carets: Vec<Caret> = self
            .carets
            .iter()
            .copied()
            .filter(|k| k.range() != last)
            .collect();
        if carets.len() < self.carets.len() && !carets.is_empty() {
            self.set_carets(carets);
            self.occurrences = Some((added, words));
        }
    }

    pub fn clone_caret(&mut self, below: bool) {
        let reference = *self
            .carets
            .iter()
            .max_by_key(|k| {
                if below {
                    u32::from(k.head()) as i64
                } else {
                    -(u32::from(k.head()) as i64)
                }
            })
            .unwrap();
        let pos = self.doc.offset_to_position(reference.head());
        let (off, goal) = match self.soft_wrap {
            // Soft wraps: the next visual row.
            Some(wrap) => {
                let hidden = self.hidden_line_ranges();
                match self.visual_row_target(
                    reference.head(),
                    reference.goal_column,
                    if below { 1 } else { -1 },
                    wrap,
                    &hidden,
                ) {
                    (Some(off), goal) => (off, goal),
                    (None, _) => return,
                }
            }
            None => {
                let line = pos.line as i64 + if below { 1 } else { -1 };
                if line < 0 || line >= self.doc.line_count() as i64 {
                    return;
                }
                let goal = reference.goal_column.unwrap_or(pos.column);
                (
                    self.doc
                        .position_to_offset(rustpy_core::Position::new(line as u32, goal)),
                    goal,
                )
            }
        };
        let mut carets = self.carets.clone();
        carets.push(Caret {
            sel: rustpy_text::Selection::caret(off),
            goal_column: Some(goal),
        });
        self.set_carets(carets);
    }

    /// Alt+Up/Down: move to the previous/next function or class definition.
    pub fn move_to_definition(&mut self, next: bool) {
        let head = self.primary().head();
        let starts = super::syntax_support::definition_starts(self.syntax());
        let target = if next {
            starts.iter().copied().find(|s| *s > head)
        } else {
            starts.iter().rev().copied().find(|s| *s < head)
        };
        if let Some(t) = target {
            self.reveal_offset(t);
            self.set_carets(vec![Caret::at(t)]);
            self.last_op = LastOp::Other;
        }
    }

    /// F2 / Shift+F2: move to the next/previous syntax error.
    pub fn move_to_error(&mut self, next: bool) -> Option<String> {
        let head = self.primary().head();
        let diags = self.diagnostics();
        let found = if next {
            diags
                .iter()
                .find(|d| d.range.start() > head)
                .or_else(|| diags.first())
        } else {
            diags
                .iter()
                .rev()
                .find(|d| d.range.start() < head)
                .or_else(|| diags.last())
        }?;
        let (start, msg) = (found.range.start(), found.message.clone());
        self.reveal_offset(start);
        self.set_carets(vec![Caret::at(start)]);
        self.last_op = LastOp::Other;
        Some(msg)
    }

    /// Ctrl+Shift+M: jump to the matching bracket.
    pub fn move_to_matching_brace(&mut self) {
        let head = self.primary().head();
        if let Some((open, close)) = super::syntax_support::matching_bracket(self.syntax(), head) {
            let target = if open.contains(head) || open.start() == head {
                close.start()
            } else {
                open.start()
            };
            self.set_carets(vec![Caret::at(target)]);
            self.last_op = LastOp::Other;
        }
    }

    /// Positions the primary caret at `line` (1-based) and optional `column` (1-based).
    pub fn go_to_line(&mut self, line: usize, column: Option<usize>) {
        let line = line
            .saturating_sub(1)
            .min(self.doc.line_count().saturating_sub(1));
        let col = column.map(|c| c.saturating_sub(1)).unwrap_or(0);
        self.move_primary_to(rustpy_core::Position::new(line as u32, col as u32));
    }

    // ===== search ============================================================

    // Plain-text shorthands for `super::find` (find bar options: see `FindOptions`).

    /// Finds the next (or previous) occurrence of `needle` from the primary
    /// caret and selects it. Returns false when not found.
    pub fn find_next(&mut self, needle: &str, case_sensitive: bool, backwards: bool) -> bool {
        let opts = super::find::FindOptions {
            case_sensitive,
            ..Default::default()
        };
        matches!(self.find_with(needle, &opts, backwards, false), Ok(Some(_)))
    }

    /// Incremental search while typing: keeps (and extends) a match that starts
    /// at the current selection instead of jumping past it.
    pub fn find_incremental(&mut self, needle: &str, case_sensitive: bool) -> bool {
        let opts = super::find::FindOptions {
            case_sensitive,
            ..Default::default()
        };
        matches!(self.find_with(needle, &opts, false, true), Ok(Some(_)))
    }

    /// All match ranges of `needle` (for highlighting).
    pub fn find_all(&self, needle: &str, case_sensitive: bool) -> Vec<TextRange> {
        let opts = super::find::FindOptions {
            case_sensitive,
            ..Default::default()
        };
        self.find_matches(needle, &opts).unwrap_or_default()
    }

    /// Replaces the current selection if it is a match, then finds the next;
    /// returns whether a next match was found.
    pub fn replace_current(
        &mut self,
        needle: &str,
        replacement: &str,
        case_sensitive: bool,
    ) -> bool {
        let opts = super::find::FindOptions {
            case_sensitive,
            ..Default::default()
        };
        let _ = self.replace_current_with(needle, replacement, &opts);
        self.find_all(needle, case_sensitive)
            .contains(&self.primary().range())
    }

    pub fn replace_all(&mut self, needle: &str, replacement: &str, case_sensitive: bool) -> usize {
        let opts = super::find::FindOptions {
            case_sensitive,
            ..Default::default()
        };
        self.replace_all_with(needle, replacement, &opts, &[])
            .unwrap_or(0)
    }

    // ===== completion =========================================================

    /// The edits and carets for accepting a lookup item at every caret (also
    /// in an in-place rename). The
    /// primary caret's typed prefix starts at `prefix_start`; every other
    /// caret preceded by the same text is completed the same way, the
    /// others stay as they are. `replace_rest` (Tab) also replaces the rest
    /// of the identifier after each caret. `insert_for` gives the text to
    /// insert and the caret offset in it, from the character after the
    /// replaced range (no second `(` before an existing one).
    pub fn completion_edits(
        &self,
        prefix_start: TextSize,
        replace_rest: bool,
        insert_for: impl Fn(Option<char>) -> (String, Option<usize>),
    ) -> (Vec<Edit>, Vec<Caret>) {
        let head = self.primary().head();
        let prefix = self.doc.slice(TextRange::new(prefix_start.min(head), head));
        let len = TextSize::of(prefix.as_str());
        let ident_end = |from: TextSize| {
            let mut end = from;
            while let Some(c) = self
                .doc
                .char_at(end)
                .filter(|c| rustpy_core::is_ident_continue(*c))
            {
                end += TextSize::of(c);
            }
            end
        };
        // Per caret: the replaced range, the text and the caret offset in it.
        let mut planned: Vec<Option<(TextRange, String, Option<usize>)>> =
            Vec::with_capacity(self.carets.len());
        for (i, k) in self.carets.iter().enumerate() {
            let h = k.head();
            let start = h
                .checked_sub(len)
                .filter(|s| i == 0 || self.doc.slice(TextRange::new(*s, h)) == prefix);
            let plan = start
                .map(|s| TextRange::new(s, if replace_rest { ident_end(h) } else { h }))
                .filter(|r| {
                    !planned.iter().flatten().any(|(p, ..)| {
                        p.start() < r.end() && r.start() < p.end() || p.start() == r.start()
                    })
                })
                .map(|r| {
                    let (text, offset) = insert_for(self.doc.char_at(r.end()));
                    (r, text, offset)
                });
            planned.push(plan);
        }
        let edits = sorted(
            planned
                .iter()
                .flatten()
                .map(|(r, t, _)| Edit::replace(*r, t.clone()))
                .collect(),
        );
        let carets = self
            .carets
            .iter()
            .zip(&planned)
            .map(|(k, plan)| match plan {
                Some((r, text, offset)) => Caret::at(
                    map_offset(&edits, r.start(), false) + ts(offset.unwrap_or(text.len())),
                ),
                None => Caret::at(map_offset(&edits, k.head(), false)),
            })
            .collect();
        (edits, carets)
    }
}

impl Caret {
    fn with_line_hint(self, _line: usize) -> Self {
        self
    }
}

fn sorted(mut edits: Vec<Edit>) -> Vec<Edit> {
    edits.sort_by_key(|e| (e.range.start(), e.range.end()));
    edits
}

/// The line `delta` visible lines away from `line`; `hidden` holds the folded
/// `(first, last)` line ranges (sorted, merged). Moving into a fold lands on
/// its header (up) or on the first line after it (down).
fn step_visible_lines(
    mut line: usize,
    delta: i64,
    line_count: usize,
    hidden: &[(usize, usize)],
) -> usize {
    let last = line_count.saturating_sub(1);
    for _ in 0..delta.unsigned_abs() {
        let next = if delta > 0 {
            if line >= last {
                break;
            }
            line + 1
        } else {
            if line == 0 {
                break;
            }
            line - 1
        };
        line = match hidden.iter().find(|(a, b)| *a <= next && next <= *b) {
            Some(&(_, b)) if delta > 0 => {
                if b >= last {
                    break;
                }
                b + 1
            }
            Some(&(a, _)) => a.saturating_sub(1),
            None => next,
        };
    }
    line
}

fn visual_column(line: &str, char_col: usize, tab_size: usize) -> usize {
    let mut col = 0;
    for c in line.chars().take(char_col) {
        col += rustpy_text::util::char_width(c, tab_size, col);
    }
    col
}

#[cfg(test)]
mod incremental_find_tests {
    use super::super::state::EditorState;
    use super::super::Language;

    #[test]
    fn typing_more_characters_extends_the_current_match() {
        let mut s = EditorState::from_text("foo fool food", Language::PlainText);
        assert!(s.find_incremental("fo", false));
        assert_eq!(usize::from(s.primary().range().start()), 0);
        assert!(s.find_incremental("foo", false), "extends in place");
        assert_eq!(usize::from(s.primary().range().start()), 0);
        assert!(
            s.find_incremental("fool", false),
            "moves only when the match no longer fits"
        );
        assert_eq!(usize::from(s.primary().range().start()), 4);
        assert!(s.find_next("foo", false, false), "F3 moves on");
        assert_eq!(usize::from(s.primary().range().start()), 9);
    }
}
