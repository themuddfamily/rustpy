//! Python smart keys while typing (roadmap §1.2 Smart Keys): dedent
//! `else:` / `elif` / `except` / `finally` / `case` when their colon is typed,
//! insert `self` / `cls` when the `(` of a method is typed, and close a typed
//! `"""` with `"""`.

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::SyntaxKind;
use rustpy_text::util::{indent_width, leading_whitespace, make_indent};
use rustpy_text::Edit;

use super::state::{map_offset, Caret, EditOptions, EditorState, Language, LastOp};

fn ts(n: usize) -> TextSize {
    TextSize::new(n as u32)
}

impl EditorState {
    /// Typing `:` that completes a clause keyword line re-indents the line to
    /// its statement (`if`, `try`, `for`, …) in the same edit.
    pub(super) fn dedent_clause_on_colon(&mut self, opts: &EditOptions) -> bool {
        let mut edits = Vec::new();
        let mut any = false;
        let heads: Vec<TextSize> = self.carets.iter().map(|k| k.head()).collect();
        // A line inside a multi-line string is text, not a clause.
        let in_string: Vec<bool> = heads
            .iter()
            .map(|&h| {
                self.offset_in_string(self.doc.line_range(self.doc.line_of(h)).start())
                    || self.offset_in_string(h)
            })
            .collect();
        for (k, in_string) in self.carets.iter().zip(in_string) {
            let head = k.head();
            let line = self.doc.line_of(head);
            let lr = self.doc.line_range(line);
            let text = self.doc.line_text(line);
            let col = usize::from(head - lr.start()).min(text.len());
            edits.push(Edit::insert(head, ":"));
            if in_string || !text[col..].trim().is_empty() {
                continue;
            }
            let typed = format!("{}:", &text[..col]);
            if !super::python_indent::is_clause_line(&typed) {
                continue;
            }
            let above = self.doc.slice(TextRange::new(TextSize::new(0), lr.start()));
            if let Some(target) =
                super::python_indent::clause_indent(&typed, &above, opts.tab_size, opts.indent_size)
            {
                let ws = leading_whitespace(&text).len();
                if ws < col {
                    edits.push(Edit::replace(
                        TextRange::new(lr.start(), lr.start() + ts(ws)),
                        make_indent(target, opts.use_tabs, opts.tab_size),
                    ));
                    any = true;
                }
            }
        }
        if !any {
            return false;
        }
        edits.sort_by_key(|e| (e.range.start(), e.range.end()));
        // Its own undo step, so Ctrl+Z restores the typed indentation.
        let group = self.group_for(LastOp::Other);
        let carets = self
            .carets
            .iter()
            .map(|k| Caret::at(map_offset(&edits, k.head(), true)))
            .collect();
        self.apply(edits, carets, group, Some("Indent"));
        true
    }

    /// `def name(` inside a class body: types `(self)` (`(cls)` for class
    /// methods, nothing for static methods) with the caret after the name.
    pub(super) fn insert_self_parameter(&mut self, type_hint: bool) -> bool {
        if self.carets.len() != 1 {
            return false;
        }
        let head = self.primary().head();
        let line = self.doc.line_of(head);
        let lr = self.doc.line_range(line);
        let text = self.doc.line_text(line);
        let col = usize::from(head - lr.start()).min(text.len());
        let (before, after) = text.split_at(col);
        if !after.trim().is_empty() && !after.trim_start().starts_with(')') {
            return false;
        }
        let header = before.trim_start();
        let header = header
            .strip_prefix("async")
            .filter(|r| r.starts_with([' ', '\t']))
            .map(str::trim_start)
            .unwrap_or(header);
        let Some(name) = header
            .strip_prefix("def")
            .filter(|r| r.starts_with([' ', '\t']))
            .map(str::trim)
        else {
            return false;
        };
        if name.is_empty()
            || !name.chars().next().is_some_and(rustpy_core::is_ident_start)
            || !name.chars().all(rustpy_core::is_ident_continue)
        {
            return false;
        }
        let Some(param) = self.method_receiver(line) else {
            return false;
        };
        let param = if type_hint {
            let indent = before
                .chars()
                .take_while(|character| character.is_whitespace())
                .count();
            let class = (0..line).rev().take(4096).find_map(|index| {
                let text = self.doc.line_text(index);
                let prefix = text
                    .chars()
                    .take_while(|character| character.is_whitespace())
                    .count();
                if prefix >= indent {
                    return None;
                }
                let header = text.trim_start().strip_prefix("class ")?;
                let name = header
                    .chars()
                    .take_while(|character| rustpy_core::is_ident_continue(*character))
                    .collect::<String>();
                (!name.is_empty()).then_some(name)
            });
            class.map_or_else(
                || param.to_string(),
                |name| {
                    if param == "cls" {
                        format!("cls: \"type[{name}]\"")
                    } else {
                        format!("self: \"{name}\"")
                    }
                },
            )
        } else {
            param.into()
        };
        let close = if after.trim_start().starts_with(')') {
            ""
        } else {
            ")"
        };
        let group = self.group_for(LastOp::Typing);
        let caret = head + ts(1 + param.len());
        self.apply(
            vec![Edit::insert(head, format!("({param}{close}"))],
            vec![Caret::at(caret)],
            group,
            None,
        );
        if !close.is_empty() {
            self.auto_closers.push(caret);
        }
        true
    }

    /// `Some("self")` / `Some("cls")` when the `def` on `line` is a method of
    /// a class; `None` for functions and static methods.
    fn method_receiver(&self, line: usize) -> Option<&'static str> {
        let def_text = self.doc.line_text(line);
        let def_indent = indent_width(&def_text, 4);
        let mut receiver = "self";
        let mut decorators = true;
        for l in (0..line).rev() {
            let t = self.doc.line_text(l);
            let stripped = t.trim();
            if stripped.is_empty() || stripped.starts_with('#') {
                continue;
            }
            let ind = indent_width(&t, 4);
            if decorators && ind == def_indent && stripped.starts_with('@') {
                let name = stripped
                    .trim_start_matches('@')
                    .split(['(', ' '])
                    .next()
                    .unwrap_or("");
                match name.rsplit('.').next().unwrap_or(name) {
                    "staticmethod" => return None,
                    "classmethod" => receiver = "cls",
                    _ => {}
                }
                continue;
            }
            decorators = false;
            if ind < def_indent {
                return (stripped.starts_with("class ") || stripped.starts_with("class\t"))
                    .then_some(receiver);
            }
        }
        None
    }

    /// The third quote of `"""` (typed after an empty `""` string) inserts
    /// the closing `"""` with the caret in between.
    pub(super) fn close_triple_quote(&mut self, c: char) -> bool {
        if self.carets.len() != 1 {
            return false;
        }
        let head = self.primary().head();
        let line = self.doc.line_of(head);
        let lr = self.doc.line_range(line);
        let text = self.doc.line_text(line);
        let col = usize::from(head - lr.start()).min(text.len());
        let (before, after) = text.split_at(col);
        let pair = format!("{c}{c}");
        if !before.ends_with(&pair)
            || !matches!(
                after.chars().next(),
                None | Some(' ' | '\t' | ')' | ']' | '}' | ',' | ';' | ':')
            )
        {
            return false;
        }
        // The two quotes must be an empty string literal: not the end of
        // `"abc"` or `""""`, nor the closing quotes of a docstring that is
        // still open, so ask the whole document's tokens, not the line's.
        let quotes_at = head - ts(pair.len());
        let root = self.syntax().syntax_node();
        let Some(tok) = super::syntax_support::token_at(&root, quotes_at).right_biased() else {
            return false;
        };
        if tok.kind() != SyntaxKind::STRING
            || tok.text_range().end() != head
            || tok
                .text()
                .trim_start_matches(|ch: char| ch.is_ascii_alphabetic())
                != pair
        {
            return false;
        }
        let group = self.group_for(LastOp::Typing);
        let triple: String = std::iter::repeat_n(c, 3).collect();
        self.apply(
            vec![Edit::insert(head, format!("{c}{triple}"))],
            vec![Caret::at(head + ts(c.len_utf8()))],
            group,
            None,
        );
        true
    }

    /// Right after Enter inside an empty docstring that is the first statement
    /// of a function (`"""|"""`, or a lone `"""` line), returns whether a
    /// documentation stub should replace it. A lone `"""` gets its closing
    /// quotes first so the file parses again.
    pub fn prepare_docstring_stub(&mut self) -> bool {
        if self.language != Language::Python
            || self.carets.len() != 1
            || self.has_selection()
            || self.is_large_file()
        {
            return false;
        }
        let head = self.primary().head();
        let line = self.doc.line_of(head);
        if line == 0 {
            return false;
        }
        let prev = self.doc.line_text(line - 1);
        let quotes = prev.trim();
        if quotes != "\"\"\"" && quotes != "'''" {
            return false;
        }
        let cur = self.doc.line_text(line);
        let col = usize::from(head - self.doc.line_range(line).start());
        let lead = leading_whitespace(&cur).len();
        let closed = cur.trim() == quotes && col == lead;
        let open = cur.trim().is_empty();
        if !closed && !open {
            return false;
        }
        let quote_at = self.doc.line_range(line - 1).start() + ts(leading_whitespace(&prev).len());
        // The quotes must open the string (not close an existing docstring),
        // and the string must be empty or still unterminated.
        {
            let root = self.syntax().syntax_node();
            let Some(tok) = super::syntax_support::token_at(&root, quote_at).right_biased() else {
                return false;
            };
            if !matches!(tok.kind(), SyntaxKind::STRING | SyntaxKind::ERROR_TOKEN)
                || tok.text_range().start() != quote_at
            {
                return false;
            }
            let body = tok.text();
            let terminated = body.len() >= 2 * quotes.len() && body.ends_with(quotes);
            if terminated
                && !body[quotes.len()..body.len() - quotes.len()]
                    .trim()
                    .is_empty()
            {
                return false;
            }
        }
        if !self.starts_function_body(quote_at) {
            return false;
        }
        if open {
            let group = self.group_for(LastOp::Other);
            self.apply(
                vec![Edit::insert(head, quotes.to_string())],
                vec![Caret::at(head)],
                group,
                None,
            );
        }
        true
    }

    /// Whether the statement starting at `off` is the first one of a function body.
    fn starts_function_body(&mut self, off: TextSize) -> bool {
        let root = self.syntax().syntax_node();
        let Some(tok) = super::syntax_support::token_at(&root, off).right_biased() else {
            return false;
        };
        let Some(stmt) = tok.parent_ancestors().find(|n| n.kind().is_statement()) else {
            return false;
        };
        let Some(block) = stmt.parent().filter(|b| b.kind() == SyntaxKind::BLOCK) else {
            return false;
        };
        block
            .parent()
            .is_some_and(|f| f.kind() == SyntaxKind::FUNC_DEF)
            && block
                .children()
                .find(|c| c.kind().is_statement())
                .is_some_and(|first| first == stmt)
    }

    /// Moves the caret from the opening quotes of a generated docstring stub
    /// to the end of its (indented, empty) summary line.
    pub fn caret_to_docstring_summary(&mut self) {
        let line = self.doc.line_of(self.primary().head());
        if line + 1 < self.doc.line_count()
            && self.doc.line_text(line + 1).trim().is_empty()
            && self.doc.line_text(line).trim().ends_with(['"', '\''])
        {
            self.set_carets(vec![Caret::at(self.doc.line_range(line + 1).end())]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::{EditOptions, EditorState, Language};
    use rustpy_core::Position;

    fn ed(text: &str) -> EditorState {
        EditorState::from_text(text, Language::Python)
    }

    fn type_str(e: &mut EditorState, s: &str, o: &EditOptions) {
        for c in s.chars() {
            if c == '\n' {
                e.newline(o);
            } else {
                e.insert_text(&c.to_string(), o);
            }
        }
    }

    #[test]
    fn typing_else_dedents_to_its_if() {
        let o = EditOptions::default();
        let mut e = ed("if a:\n    x = 1\n");
        e.move_doc_end(false);
        e.insert_text("    ", &o);
        type_str(&mut e, "else:", &o);
        assert_eq!(e.doc.text(), "if a:\n    x = 1\nelse:");
        assert_eq!(e.primary_position(), Position::new(2, 5));
        // Enter + except inside a try body.
        let mut t = ed("try:\n    x = 1");
        t.move_doc_end(false);
        type_str(&mut t, "\nexcept ValueError:", &o);
        assert_eq!(t.doc.text(), "try:\n    x = 1\nexcept ValueError:");
        assert!(t.undo());
        assert_eq!(
            t.doc.text(),
            "try:\n    x = 1\n    except ValueError",
            "undo takes back the colon and the dedent first"
        );
        // A colon elsewhere does nothing special.
        let mut d = ed("if a:\n    d = {'else'");
        d.move_doc_end(false);
        d.insert_text(":", &o);
        assert_eq!(d.doc.text(), "if a:\n    d = {'else':");
    }

    #[test]
    fn method_paren_inserts_self_or_cls() {
        let o = EditOptions::default();
        let mut e = ed("class C:\n    def m");
        e.move_doc_end(false);
        e.insert_text("(", &o);
        assert_eq!(e.doc.text(), "class C:\n    def m(self)");
        assert_eq!(
            e.primary_position(),
            Position::new(1, 14),
            "caret after self"
        );
        e.insert_text(",", &o);
        e.insert_text(" ", &o);
        e.insert_text("x", &o);
        e.insert_text(")", &o);
        assert_eq!(
            e.doc.text(),
            "class C:\n    def m(self, x)",
            "the inserted ) is typed over"
        );
        let mut c = ed("class C:\n    @classmethod\n    def make");
        c.move_doc_end(false);
        c.insert_text("(", &o);
        assert_eq!(
            c.doc.text(),
            "class C:\n    @classmethod\n    def make(cls)"
        );
        let mut s = ed("class C:\n    @staticmethod\n    def f");
        s.move_doc_end(false);
        s.insert_text("(", &o);
        assert_eq!(s.doc.text(), "class C:\n    @staticmethod\n    def f()");
        let mut f = ed("def top");
        f.move_doc_end(false);
        f.insert_text("(", &o);
        assert_eq!(f.doc.text(), "def top()", "plain functions get no self");
        let mut n = ed("class C:\n    def m(self):\n        def inner");
        n.move_doc_end(false);
        n.insert_text("(", &o);
        assert_eq!(
            n.doc.text(),
            "class C:\n    def m(self):\n        def inner()",
            "nested functions are not methods"
        );
        let off = EditOptions {
            insert_self: false,
            ..EditOptions::default()
        };
        let mut x = ed("class C:\n    def m");
        x.move_doc_end(false);
        x.insert_text("(", &off);
        assert_eq!(x.doc.text(), "class C:\n    def m()");
    }

    #[test]
    fn triple_quotes_close() {
        let o = EditOptions::default();
        let mut e = ed("def f():\n    ");
        e.move_doc_end(false);
        type_str(&mut e, "\"\"\"", &o);
        assert_eq!(e.doc.text(), "def f():\n    \"\"\"\"\"\"");
        assert_eq!(e.primary_position(), Position::new(1, 7));
        type_str(&mut e, "Doc.", &o);
        type_str(&mut e, "\"\"\"", &o);
        assert_eq!(
            e.doc.text(),
            "def f():\n    \"\"\"Doc.\"\"\"",
            "closing quotes are typed over"
        );
        // Single quotes too; a closed string is not reopened.
        let mut s = ed("x = ");
        s.move_doc_end(false);
        type_str(&mut s, "'''", &o);
        assert_eq!(s.doc.text(), "x = ''''''");
        let mut n = ed("x = \"a\"\"");
        n.move_doc_end(false);
        n.insert_text("\"", &o);
        assert_eq!(
            n.doc.text(),
            "x = \"a\"\"\"",
            "not after a non-empty string"
        );
        let off = EditOptions {
            close_triple_quotes: false,
            ..EditOptions::default()
        };
        let mut p = ed("");
        type_str(&mut p, "\"\"\"", &off);
        assert_eq!(p.doc.text(), "\"\"\"");
    }

    #[test]
    fn existing_docstrings_get_no_stub_and_closing_quotes_are_not_doubled() {
        let o = EditOptions::default();
        // Enter after the closing quotes of a multi-line docstring.
        let src = "def f(a):\n    \"\"\"Summary.\n\n    More.\n    \"\"\"\n    return a\n";
        let mut e = ed(src);
        e.move_primary_to(Position::new(4, 7));
        e.newline(&o);
        assert!(
            !e.prepare_docstring_stub(),
            "closing quotes of an existing docstring"
        );
        // Enter after opening quotes whose summary is on the next line.
        let src = "def f(a):\n    \"\"\"\n    Summary.\n    \"\"\"\n    return a\n";
        let mut e = ed(src);
        e.move_primary_to(Position::new(1, 7));
        e.newline(&o);
        assert!(!e.prepare_docstring_stub(), "a docstring with text");
        // Typing the closing quotes of an open docstring closes it.
        let mut e = ed("def f():\n    \"\"\"Summary.\n\n    \n    return 1\n");
        e.move_primary_to(Position::new(3, 4));
        type_str(&mut e, "\"\"\"", &o);
        assert_eq!(e.doc.line_text(3), "    \"\"\"");
        // `else:` typed inside a multi-line string stays where it is.
        let mut e = ed("if x:\n    s = \"\"\"\n    else\n\"\"\"\n");
        e.move_primary_to(Position::new(2, 8));
        e.insert_text(":", &o);
        assert_eq!(e.doc.line_text(2), "    else:");
    }

    #[test]
    fn enter_in_an_empty_docstring_asks_for_a_stub() {
        let o = EditOptions::default();
        let mut e = ed("def f(a):\n    ");
        e.move_doc_end(false);
        type_str(&mut e, "\"\"\"\n", &o);
        assert_eq!(e.doc.text(), "def f(a):\n    \"\"\"\n    \"\"\"");
        assert_eq!(e.primary_position(), Position::new(2, 4));
        assert!(e.prepare_docstring_stub());
        // A lone `"""` (quotes not auto-closed) is closed first.
        let mut l = ed("def f(a):\n    \"\"\"\n    ");
        l.move_doc_end(false);
        assert!(l.prepare_docstring_stub());
        assert_eq!(l.doc.text(), "def f(a):\n    \"\"\"\n    \"\"\"");
        // Only for the first statement of a function.
        let mut c = ed("class C:\n    \"\"\"\n    \"\"\"");
        c.move_primary_to(Position::new(2, 4));
        assert!(!c.prepare_docstring_stub());
        let mut s = ed("def f():\n    x = 1\n    \"\"\"\n    \"\"\"");
        s.move_primary_to(Position::new(3, 4));
        assert!(!s.prepare_docstring_stub());
        let mut n = ed("def f():\n    x = \"\"\"\n    \"\"\"");
        n.move_primary_to(Position::new(2, 4));
        assert!(
            !n.prepare_docstring_stub(),
            "a string assignment is no docstring"
        );
    }
    #[test]
    fn code_generation_self_hints_are_quoted_and_cls_hints_keep_class_semantics() {
        let mut options = EditOptions::default();
        options.paste_format_options.python.self_type_hints = true;
        for (source, expected) in [
            (
                "class C:\n    def method",
                "class C:\n    def method(self: \"C\")",
            ),
            (
                "class C:\n    @classmethod\n    def make",
                "class C:\n    @classmethod\n    def make(cls: \"type[C]\")",
            ),
        ] {
            let mut editor = ed(source);
            editor.move_doc_end(false);
            editor.insert_text("(", &options);
            assert_eq!(editor.doc.text(), expected);
            let completed = format!("{}:\n        pass\n", editor.doc.text());
            assert!(rustpy_syntax::parse(&completed).errors().is_empty());
        }
    }
    #[test]
    fn line_comment_generation_preferences_round_trip_original_indentation() {
        let source = "    first()\n    second()\n";
        let mut editor = ed(source);
        editor.select_all();
        editor.toggle_line_comment_with_style(true, false);
        assert_eq!(editor.doc.text(), "#    first()\n#    second()\n");
        editor.select_all();
        editor.toggle_line_comment_with_style(true, false);
        assert_eq!(editor.doc.text(), source);
        editor.select_all();
        editor.toggle_line_comment_with_style(false, true);
        assert_eq!(editor.doc.text(), "    # first()\n    # second()\n");
    }
}
