//! Headless tests for editor commands (roadmap §1.2).

use rustpy_core::{Position, TextSize};

use super::state::{Caret, EditOptions, EditorState, Language};

fn ed(text: &str) -> EditorState {
    EditorState::from_text(text, Language::Python)
}

fn ts(n: u32) -> TextSize {
    TextSize::new(n)
}

fn opts() -> EditOptions {
    EditOptions::default()
}

fn caret_at(e: &mut EditorState, line: u32, col: u32) {
    e.move_primary_to(Position::new(line, col));
}

#[test]
fn typing_and_auto_close() {
    let mut e = ed("");
    let o = opts();
    e.insert_text("f", &o);
    e.insert_text("(", &o);
    assert_eq!(e.doc.text(), "f()");
    assert_eq!(e.primary().head(), ts(2));
    e.insert_text("x", &o);
    e.insert_text(")", &o); // types over the auto-inserted paren
    assert_eq!(e.doc.text(), "f(x)");
    assert_eq!(e.primary().head(), ts(4));
    e.insert_text("\"", &o);
    assert_eq!(e.doc.text(), "f(x)\"\"");
    e.insert_text("s", &o);
    e.insert_text("\"", &o);
    assert_eq!(e.doc.text(), "f(x)\"s\"");
    // Undo merges consecutive typing into one step for plain chars.
    let mut e = ed("");
    e.insert_text("a", &o);
    e.insert_text("b", &o);
    e.insert_text("c", &o);
    assert!(e.undo());
    assert_eq!(e.doc.text(), "");
}

#[test]
fn surround_selection_with_quotes() {
    let mut e = ed("hello world");
    e.set_carets(vec![Caret::new(ts(0), ts(5))]);
    e.insert_text("'", &opts());
    assert_eq!(e.doc.text(), "'hello' world");
    assert_eq!(
        e.primary().range(),
        rustpy_core::TextRange::new(ts(1), ts(6))
    );
}

#[test]
fn newline_auto_indent_python() {
    let o = opts();
    let mut e = ed("def f():");
    e.move_doc_end(false);
    e.newline(&o);
    assert_eq!(e.doc.text(), "def f():\n    ");
    e.insert_text("return 1", &o);
    e.newline(&o);
    assert_eq!(
        e.doc.text(),
        "def f():\n    return 1\n",
        "dedent after return"
    );
    // Between brackets.
    let mut e = ed("x = [");
    e.move_doc_end(false);
    e.insert_text("]", &o); // no auto-close partner existed, so just a `]`
    assert_eq!(e.doc.text(), "x = []");
    caret_at(&mut e, 0, 5);
    e.newline(&o);
    assert_eq!(e.doc.text(), "x = [\n    \n]");
    assert_eq!(e.primary_position(), Position::new(1, 4));
}

#[test]
fn backspace_smart_and_pairs() {
    let o = opts();
    let mut e = ed("        x");
    caret_at(&mut e, 0, 8);
    e.backspace(&o);
    assert_eq!(e.doc.text(), "    x");
    let mut e = ed("f()");
    caret_at(&mut e, 0, 2);
    e.backspace(&o);
    assert_eq!(e.doc.text(), "f");
    let mut e = ed("ab");
    e.set_carets(vec![Caret::new(ts(0), ts(2))]);
    e.backspace(&o);
    assert_eq!(e.doc.text(), "");
}

#[test]
fn tab_and_unindent() {
    let o = opts();
    let mut e = ed("a\nb\nc");
    e.set_carets(vec![Caret::new(ts(0), ts(4))]);
    e.tab(&o);
    assert_eq!(
        e.doc.text(),
        "    a\n    b\nc",
        "selection ending at column 0 excludes that line"
    );
    e.set_carets(vec![Caret::new(ts(0), ts(13))]);
    e.tab(&o);
    assert_eq!(e.doc.text(), "        a\n        b\n    c");
    e.unindent_lines(&o);
    e.unindent_lines(&o);
    assert_eq!(e.doc.text(), "a\nb\nc");
    let mut e = ed("ab");
    caret_at(&mut e, 0, 1);
    e.tab(&o);
    assert_eq!(e.doc.text(), "a   b", "tab fills to next stop");
}

#[test]
fn duplicate_delete_move_join() {
    let o = opts();
    let mut e = ed("one\ntwo\nthree");
    caret_at(&mut e, 1, 1);
    e.duplicate();
    assert_eq!(e.doc.text(), "one\ntwo\ntwo\nthree");
    assert_eq!(e.primary_position(), Position::new(2, 1));
    e.delete_lines();
    assert_eq!(e.doc.text(), "one\ntwo\nthree");
    e.move_lines(false);
    assert_eq!(e.doc.text(), "one\nthree\ntwo");
    assert_eq!(e.primary_position().line, 1, "caret follows the moved line");
    e.move_lines(false);
    assert_eq!(e.doc.text(), "three\none\ntwo");
    e.move_lines(false); // already at top: no-op
    assert_eq!(e.doc.text(), "three\none\ntwo");
    e.move_lines(true);
    assert_eq!(e.doc.text(), "one\nthree\ntwo");
    caret_at(&mut e, 0, 0);
    e.join_lines();
    assert_eq!(e.doc.text(), "one; three\ntwo");
    let mut e = ed("# a\n# b");
    e.join_lines();
    assert_eq!(e.doc.text(), "# a b");
    let _ = o;
}

#[test]
fn comment_toggle() {
    let mut e = ed("    x = 1\n    y = 2\n");
    e.set_carets(vec![Caret::new(ts(0), ts(14))]);
    e.toggle_line_comment();
    assert_eq!(e.doc.text(), "    # x = 1\n    # y = 2\n");
    e.toggle_line_comment();
    assert_eq!(e.doc.text(), "    x = 1\n    y = 2\n");
    // Single line without selection moves the caret down.
    let mut e = ed("a\nb\n");
    e.toggle_line_comment();
    assert_eq!(e.doc.text(), "# a\nb\n");
    assert_eq!(e.primary_position().line, 1);
}

#[test]
fn multi_caret_typing_and_paste() {
    let o = opts();
    let mut e = ed("a\nb\nc");
    e.set_carets(vec![Caret::at(ts(1)), Caret::at(ts(3)), Caret::at(ts(5))]);
    e.insert_text("!", &o);
    assert_eq!(e.doc.text(), "a!\nb!\nc!");
    assert_eq!(e.carets.len(), 3);
    e.paste("1\n2\n3", &o);
    assert_eq!(e.doc.text(), "a!1\nb!2\nc!3");
    e.undo();
    assert_eq!(e.doc.text(), "a!\nb!\nc!");
    assert_eq!(e.carets.len(), 3);
}

#[test]
fn word_movement_and_selection() {
    let o = opts();
    let mut e = ed("foo_bar(baz, qux)");
    e.move_word_right(false, &o);
    assert_eq!(e.primary().head(), ts(7));
    e.move_word_right(false, &o);
    assert_eq!(e.primary().head(), ts(8));
    e.move_word_right(true, &o);
    assert_eq!(e.doc.slice(e.primary().range()), "baz");
    e.move_doc_end(false);
    e.move_word_left(false, &o);
    assert_eq!(e.primary().head(), ts(16));
    let mut e = ed("fooBarBaz x");
    let mut o2 = opts();
    o2.camel_humps = true;
    e.move_word_right(false, &o2);
    assert_eq!(e.primary().head(), ts(3));
}

#[test]
fn smart_home_and_vertical_goal_column() {
    let o = opts();
    let mut e = ed("    code here\nx\n    longer line");
    caret_at(&mut e, 0, 13);
    e.move_line_start(false, &o);
    assert_eq!(e.primary_position(), Position::new(0, 4));
    e.move_line_start(false, &o);
    assert_eq!(e.primary_position(), Position::new(0, 0));
    caret_at(&mut e, 0, 10);
    e.move_vertical(1, false);
    assert_eq!(e.primary_position(), Position::new(1, 1));
    e.move_vertical(1, false);
    assert_eq!(
        e.primary_position(),
        Position::new(2, 10),
        "goal column restored"
    );
}

#[test]
fn occurrences_and_extend_selection() {
    let mut e = ed("x = x + x\nx");
    e.add_selection_for_next_occurrence();
    assert_eq!(e.doc.slice(e.primary().range()), "x");
    e.add_selection_for_next_occurrence();
    e.add_selection_for_next_occurrence();
    assert_eq!(e.carets.len(), 3);
    e.select_all_occurrences();
    assert_eq!(e.carets.len(), 4);
    e.collapse_to_primary();
    assert_eq!(e.carets.len(), 1);
    let mut e = EditorState::from_text("  hello world\n", Language::PlainText);
    caret_at(&mut e, 0, 3);
    e.extend_selection();
    assert_eq!(e.doc.slice(e.primary().range()), "hello");
    e.extend_selection();
    assert_eq!(e.doc.slice(e.primary().range()), "hello world");
    e.extend_selection();
    assert_eq!(e.doc.slice(e.primary().range()), "  hello world");
    e.shrink_selection();
    assert_eq!(e.doc.slice(e.primary().range()), "hello world");
}

#[test]
fn syntax_aware_extend_and_navigation() {
    let mut e = ed("def f(x):\n    return g(x + 1)\n\nclass C:\n    pass\n");
    caret_at(&mut e, 1, 13);
    e.extend_selection();
    assert_eq!(e.doc.slice(e.primary().range()), "x");
    e.extend_selection();
    assert_eq!(e.doc.slice(e.primary().range()), "x + 1");
    e.extend_selection();
    e.extend_selection();
    assert_eq!(e.doc.slice(e.primary().range()), "g(x + 1)");
    e.extend_selection();
    assert_eq!(e.doc.slice(e.primary().range()), "return g(x + 1)");
    e.shrink_selection();
    assert_eq!(e.doc.slice(e.primary().range()), "g(x + 1)");
    caret_at(&mut e, 0, 0);
    e.move_to_definition(true);
    assert_eq!(e.primary_position().line, 3);
    e.move_to_definition(false);
    assert_eq!(e.primary_position().line, 0);
    assert!(e.diagnostics().is_empty());
    let mut bad = ed("x = (1,\ny = 2\nif\n");
    assert!(!bad.diagnostics().is_empty());
    assert!(bad.move_to_error(true).is_some());
    assert_eq!(e.structure().len(), 2);
    caret_at(&mut e, 1, 12);
    assert_eq!(
        e.breadcrumbs()
            .iter()
            .map(|c| c.0.clone())
            .collect::<Vec<_>>(),
        vec!["f()"]
    );
    assert_eq!(e.fold_regions().len(), 2);
    e.toggle_fold_at_line(0);
    assert_eq!(e.hidden_line_ranges(), vec![(1, 1)]);
    e.reveal_offset(e.doc.position_to_offset(Position::new(1, 0)));
    assert!(e.hidden_line_ranges().is_empty());
}

#[test]
fn copy_cut_whole_line_and_toggle_case() {
    let mut e = ed("alpha\nbeta\n");
    caret_at(&mut e, 0, 2);
    assert_eq!(e.copy_text(), "alpha\n");
    let cut = e.cut();
    assert_eq!(cut, "alpha\n");
    assert_eq!(e.doc.text(), "beta\n");
    e.toggle_case();
    assert_eq!(e.doc.text(), "BETA\n");
    e.toggle_case();
    assert_eq!(e.doc.text(), "beta\n");
}

#[test]
fn find_and_replace() {
    let mut e = ed("ab AB ab");
    assert!(e.find_next("ab", false, false));
    assert_eq!(
        e.primary().range(),
        rustpy_core::TextRange::new(ts(0), ts(2))
    );
    assert!(e.find_next("ab", false, false));
    assert_eq!(
        e.primary().range(),
        rustpy_core::TextRange::new(ts(3), ts(5))
    );
    assert_eq!(e.find_all("ab", true).len(), 2);
    assert_eq!(e.replace_all("ab", "x", false), 3);
    assert_eq!(e.doc.text(), "x x x");
    assert!(!e.find_next("zzz", false, false));
}

#[test]
fn go_to_line_and_clone_caret() {
    let mut e = ed("l1\nl2\nl3");
    e.go_to_line(3, Some(2));
    assert_eq!(e.primary_position(), Position::new(2, 1));
    e.clone_caret(false);
    assert_eq!(e.carets.len(), 2);
    e.clone_caret(false);
    assert_eq!(e.carets.len(), 3);
    e.clone_caret(false); // top reached
    assert_eq!(e.carets.len(), 3);
}

// ----- smart paste (reformat on paste: indent block) ---------------------------

#[test]
fn paste_moves_blocks_to_the_caret_indentation() {
    let o = opts();
    // Shallow → deep at an indented blank line.
    let mut e = ed("def f():\n    \n");
    caret_at(&mut e, 1, 4);
    e.paste("if x:\n    y()", &o);
    assert_eq!(e.doc.text(), "def f():\n    if x:\n        y()\n");
    assert_eq!(
        e.primary_position(),
        Position::new(2, 11),
        "caret after the pasted text"
    );
    // Deep → shallow on an empty line at column 0.
    let mut e = ed("x = 1\n");
    caret_at(&mut e, 1, 0);
    e.paste("        if a:\n            b()", &o);
    assert_eq!(e.doc.text(), "x = 1\nif a:\n    b()");
    // A blank line inside a body takes the body's indentation.
    let mut e = ed("def f():\n    a = 1\n\n    b = 2\n");
    caret_at(&mut e, 2, 0);
    e.paste("for i in x:\n    pass", &o);
    assert_eq!(
        e.doc.text(),
        "def f():\n    a = 1\n    for i in x:\n        pass\n    b = 2\n"
    );
}

#[test]
fn paste_whole_lines_after_code_and_inside_strings() {
    let o = opts();
    // Whole lines (copied without a selection) go above the caret line at its indentation.
    let mut e = ed("def f():\n    a = 1\n    b = 2\n");
    caret_at(&mut e, 2, 6);
    e.paste("c = 3\nd = 4\n", &o);
    assert_eq!(
        e.doc.text(),
        "def f():\n    a = 1\n    c = 3\n    d = 4\n    b = 2\n"
    );
    assert_eq!(
        e.primary_position(),
        Position::new(4, 6),
        "caret stays on its line"
    );
    // After code only the following lines move.
    let mut e = ed("def f():\n    x = \n");
    caret_at(&mut e, 1, 8);
    e.paste("foo(\n    1,\n)", &o);
    assert_eq!(e.doc.text(), "def f():\n    x = foo(\n        1,\n    )\n");
    // Inside a string literal nothing is re-indented.
    let mut e = ed("def f():\n    \"\"\"Doc.\n    \"\"\"\n");
    caret_at(&mut e, 1, 11);
    e.paste("one\n        two", &o);
    assert_eq!(
        e.doc.text(),
        "def f():\n    \"\"\"Doc.one\n        two\n    \"\"\"\n"
    );
    // Disabled, or not Python: verbatim.
    let off = EditOptions {
        paste_indent_block: false,
        ..opts()
    };
    let mut e = ed("def f():\n    \n");
    caret_at(&mut e, 1, 4);
    e.paste("if x:\n    y()", &off);
    assert_eq!(e.doc.text(), "def f():\n    if x:\n    y()\n");
    let mut t = EditorState::from_text("    \n", Language::PlainText);
    caret_at(&mut t, 0, 4);
    t.paste("a\n    b", &o);
    assert_eq!(t.doc.text(), "    a\n    b\n");
}

#[test]
fn paste_with_tabs_and_multiple_carets() {
    let tabs = EditOptions {
        use_tabs: true,
        ..opts()
    };
    let mut e = ed("if a:\n\t\n");
    caret_at(&mut e, 1, 1);
    e.paste("while b:\n    c()", &tabs);
    assert_eq!(e.doc.text(), "if a:\n\twhile b:\n\t\tc()\n");
    // Every caret re-indents its own copy (three lines for two carets: no distribution).
    let o = opts();
    let mut e = ed("def f():\n    \ndef g():\n        \n");
    e.set_carets(vec![Caret::at(ts(13)), Caret::at(ts(31))]);
    e.paste("if x:\n    y()\n    z()", &o);
    assert_eq!(e.doc.text(), "def f():\n    if x:\n        y()\n        z()\ndef g():\n        if x:\n            y()\n            z()\n");
    // One line per caret is distributed, each re-indented for its caret.
    let mut e = ed("def f():\n    \ndef g():\n        \n");
    e.set_carets(vec![Caret::at(ts(13)), Caret::at(ts(31))]);
    e.paste("    a()\n    b()", &o);
    assert_eq!(e.doc.text(), "def f():\n    a()\ndef g():\n        b()\n");
}

#[test]
fn paste_at_column_zero_at_the_end_stays_top_level() {
    let o = opts();
    let mut e = ed("def f():\n    print(1)\n");
    e.move_doc_end(false);
    e.paste("def g():\n    pass", &o);
    assert_eq!(e.doc.text(), "def f():\n    print(1)\ndef g():\n    pass");
    // After a block header the paste goes into the block.
    let mut e = ed("def f():\n");
    e.move_doc_end(false);
    e.paste("x = 1\ny = 2", &o);
    assert_eq!(e.doc.text(), "def f():\n    x = 1\n    y = 2");
}

// ----- Enter -------------------------------------------------------------------

#[test]
fn enter_in_the_indentation_keeps_the_line_indented() {
    let o = opts();
    let src = "def f():\n    if x:\n        a()\n        b()\n";
    let mut e = ed(src);
    caret_at(&mut e, 3, 0);
    e.newline(&o);
    assert_eq!(
        e.doc.text(),
        "def f():\n    if x:\n        a()\n\n        b()\n"
    );
    let mut e = ed(src);
    caret_at(&mut e, 3, 2);
    e.newline(&o);
    assert_eq!(
        e.doc.text(),
        "def f():\n    if x:\n        a()\n\n        b()\n"
    );
    let mut e = ed("def f():\n    return 1");
    caret_at(&mut e, 1, 0);
    e.newline(&o);
    assert_eq!(e.doc.text(), "def f():\n\n    return 1");
    // Also without auto-indent.
    let plain = EditOptions {
        auto_indent: false,
        ..opts()
    };
    let mut e = ed(src);
    caret_at(&mut e, 3, 0);
    e.newline(&plain);
    assert_eq!(
        e.doc.text(),
        "def f():\n    if x:\n        a()\n\n        b()\n"
    );
}

#[test]
fn enter_aligns_with_brackets_and_multi_line_headers() {
    let o = opts();
    let mut e = ed("    x = foo(a,");
    e.move_doc_end(false);
    e.newline(&o);
    assert_eq!(
        e.doc.text(),
        "    x = foo(a,\n            ",
        "aligned under the first argument"
    );
    let mut e = ed("def f(a,\n      b):");
    e.move_doc_end(false);
    e.newline(&o);
    assert_eq!(
        e.doc.text(),
        "def f(a,\n      b):\n    ",
        "body indent after a two-line header"
    );
    let mut e = ed("if x:\n    y = call(1,\n             2)");
    e.move_doc_end(false);
    e.newline(&o);
    assert_eq!(
        e.primary_position(),
        Position::new(3, 4),
        "statement indent after a closed call"
    );
    // Hanging bracket with the closer after the caret: the closer gets its own line.
    let mut e = ed("    d = {}");
    caret_at(&mut e, 0, 9);
    e.newline(&o);
    assert_eq!(e.doc.text(), "    d = {\n        \n    }");
    // Plain text keeps the line's indentation.
    let mut t = EditorState::from_text("    note:", Language::PlainText);
    t.move_doc_end(false);
    t.newline(&o);
    assert_eq!(t.doc.text(), "    note:\n    ");
}

// ----- Tab -------------------------------------------------------------------------

#[test]
fn tab_jumps_over_auto_inserted_closers_and_indents_selections() {
    let o = opts();
    let mut e = ed("");
    for c in ["f", "(", "\"", "x"] {
        e.insert_text(c, &o);
    }
    assert_eq!(e.doc.text(), "f(\"x\")");
    e.tab(&o);
    assert_eq!(e.primary().head(), ts(5), "past the closing quote");
    e.tab(&o);
    assert_eq!(e.primary().head(), ts(6), "past the closing paren");
    e.tab(&o);
    assert_eq!(
        e.doc.text(),
        "f(\"x\")  ",
        "nothing left to jump over: indent"
    );
    // A closer typed over or left behind is forgotten.
    let mut e = ed("");
    e.insert_text("(", &o);
    e.insert_text(")", &o);
    assert!(e.auto_closers.is_empty());
    // Disabled: Tab inserts spaces.
    let off = EditOptions {
        jump_outside_brackets_with_tab: false,
        ..opts()
    };
    let mut e = ed("");
    e.insert_text("(", &off);
    e.tab(&off);
    assert_eq!(e.doc.text(), "(   )", "spaces up to the next tab stop");
    // A selection inside one line indents that line.
    let mut e = ed("ab\ncd");
    e.set_carets(vec![Caret::new(ts(0), ts(1))]);
    e.tab(&o);
    assert_eq!(e.doc.text(), "    ab\ncd");
    assert_eq!(e.doc.slice(e.primary().range()), "a");
}

// ----- folding -------------------------------------------------------------------

fn folded_lines(e: &mut EditorState) -> Vec<(usize, usize)> {
    e.hidden_line_ranges()
}

#[test]
fn folds_follow_edits_and_undo() {
    let mut e = ed("x = 1\ndef f():\n    a = 1\n    b = 2\nz = 3\n");
    e.toggle_fold_at_line(1);
    assert_eq!(folded_lines(&mut e), vec![(2, 3)]);
    // Inserting lines above moves the fold with its function.
    e.set_carets(vec![Caret::at(ts(0))]);
    e.paste("import os\nimport sys\n", &opts());
    assert_eq!(folded_lines(&mut e), vec![(4, 5)]);
    // Deleting lines above as well.
    caret_at(&mut e, 0, 0);
    e.delete_lines();
    assert_eq!(folded_lines(&mut e), vec![(3, 4)]);
    assert!(e.undo());
    assert_eq!(folded_lines(&mut e), vec![(4, 5)], "undo moves it back");
    // Editing the folded function's header keeps it folded.
    let def_line = 3;
    caret_at(&mut e, def_line as u32, 5);
    e.insert_text("g", &opts());
    assert_eq!(folded_lines(&mut e), vec![(4, 5)]);
}

#[test]
fn collapsing_moves_the_caret_out_and_arrows_skip_folds() {
    let mut e = ed("x = 1\ndef f():\n    a = 1\n    b = 2\nz = 3\n");
    caret_at(&mut e, 3, 6);
    e.collapse_region_at_caret();
    assert_eq!(folded_lines(&mut e), vec![(2, 3)]);
    assert_eq!(
        e.primary_position(),
        Position::new(1, 8),
        "caret at the end of the header"
    );
    e.reveal_offset(e.primary().head());
    assert_eq!(
        folded_lines(&mut e),
        vec![(2, 3)],
        "the view's caret reveal keeps the fold"
    );
    e.move_vertical(1, false);
    assert_eq!(e.primary_position().line, 4, "down skips the folded body");
    e.move_vertical(-1, false);
    assert_eq!(e.primary_position().line, 1, "up lands on the header");
    assert_eq!(folded_lines(&mut e), vec![(2, 3)]);
    e.expand_region_at_caret();
    assert!(folded_lines(&mut e).is_empty());
    // Toggle from inside the body, and collapse all.
    caret_at(&mut e, 2, 2);
    e.toggle_fold_at_caret();
    assert_eq!(folded_lines(&mut e), vec![(2, 3)]);
    assert_eq!(e.primary_position().line, 1);
    e.expand_all_folds();
    caret_at(&mut e, 3, 0);
    e.collapse_all_folds();
    assert_eq!(folded_lines(&mut e), vec![(2, 3)]);
    assert_eq!(e.primary_position().line, 1);
    // A fold reaching the end of the file: Down stops at the header's end.
    let mut t = ed("def f():\n    a = 1\n    b = 2");
    t.toggle_fold_at_line(0);
    caret_at(&mut t, 0, 0);
    t.move_vertical(1, false);
    assert_eq!(t.primary_position(), Position::new(0, 8));
    assert_eq!(folded_lines(&mut t), vec![(1, 2)]);
}

#[test]
fn imports_and_docstrings_collapse_by_default_once() {
    let src = "import os\nimport sys\n\ndef f():\n    \"\"\"Doc\n    more.\"\"\"\n    return 1\n";
    let mut e = ed(src);
    e.apply_default_folds(true, true);
    assert_eq!(folded_lines(&mut e), vec![(1, 1), (5, 5)]);
    e.expand_all_folds();
    e.apply_default_folds(true, true);
    assert!(
        folded_lines(&mut e).is_empty(),
        "only for a newly opened file"
    );
    let mut e = ed(src);
    e.apply_default_folds(false, true);
    assert_eq!(folded_lines(&mut e), vec![(5, 5)]);
}

#[test]
fn other_languages_fold_too() {
    let mut e = EditorState::from_text("{\n  \"a\": [\n    1\n  ]\n}\n", Language::Json);
    assert_eq!(e.fold_regions().len(), 2);
    e.toggle_fold_at_line(0);
    assert_eq!(folded_lines(&mut e), vec![(1, 3)]);
}

#[test]
fn multi_caret_delete_and_duplicate_keep_every_caret_in_place() {
    let o = opts();
    // The primary caret (first in the list) is the bottom one.
    let mut e = ed("alpha = 1\nbeta = 2\ngamma = 3\n");
    e.set_carets(vec![Caret::at(ts(19)), Caret::at(ts(10)), Caret::at(ts(0))]);
    e.delete_forward();
    e.insert_text("X", &o);
    assert_eq!(e.doc.text(), "Xlpha = 1\nXeta = 2\nXamma = 3\n");
    // Duplicate Line with two carets: each lands on its own copy.
    let mut e = ed("def foo(a, b):\n    x = a + b\n    return x\nclass Bar:\n    def method(self):\n        pass\n");
    e.set_carets(vec![Caret::at(ts(19)), Caret::at(ts(57))]);
    e.duplicate();
    e.insert_text("Q", &o);
    assert_eq!(e.doc.text(), "def foo(a, b):\n    x = a + b\n    Qx = a + b\n    return x\nclass Bar:\n    def method(self):\n    Qdef method(self):\n        pass\n");
}

#[test]
fn quotes_pair_after_string_prefixes() {
    let o = opts();
    for prefix in ["f", "r", "b", "rb", "Fr"] {
        let mut e = ed("x = ");
        e.move_doc_end(false);
        e.insert_text(prefix, &o);
        e.insert_text("\"", &o);
        assert_eq!(e.doc.text(), format!("x = {prefix}\"\""), "{prefix}");
    }
    // Not after an ordinary name.
    let mut e = ed("x = name");
    e.move_doc_end(false);
    e.insert_text("\"", &o);
    assert_eq!(e.doc.text(), "x = name\"");
}

#[test]
fn occurrences_from_the_caret_are_whole_words_in_order() {
    let o = opts();
    // From the word at the caret: whole words only.
    let mut e = ed("val = 1\ninterval = 2\nval2 = 3\nval = 4\n");
    caret_at(&mut e, 0, 1);
    e.select_all_occurrences();
    e.insert_text("X", &o);
    assert_eq!(e.doc.text(), "X = 1\ninterval = 2\nval2 = 3\nX = 4\n");
    // Alt+J wraps to the top and keeps going; Alt+Shift+J removes the newest.
    let mut e = ed("a = x\nb = x\nc = x\n");
    caret_at(&mut e, 1, 4);
    e.add_selection_for_next_occurrence(); // word at caret (line 2)
    e.add_selection_for_next_occurrence(); // line 3
    e.add_selection_for_next_occurrence(); // wraps to line 1
    assert_eq!(e.carets.len(), 3);
    e.unselect_occurrence(); // removes line 1 (added last)
    e.insert_text("Y", &o);
    assert_eq!(e.doc.text(), "a = x\nb = Y\nc = Y\n");
}

#[test]
fn join_lines_handles_blank_lines_and_continuations() {
    // A whitespace-only line inside the selection.
    let mut e = ed("a = 1\n    \nb = 2\n");
    e.set_carets(vec![Caret::new(ts(0), ts(12))]);
    e.join_lines();
    assert_eq!(e.doc.text(), "a = 1; b = 2\n");
    // A backslash continuation goes away; adjacent literals merge.
    let mut e = ed("x = 1 + \\\n    2\n");
    e.join_lines();
    assert_eq!(e.doc.text(), "x = 1 + 2\n");
    let mut e = ed("s = \"abc\" \\\n    \"def\"\n");
    e.join_lines();
    assert_eq!(e.doc.text(), "s = \"abcdef\"\n");
    // Plain join keeps the caret at the join point.
    let mut e = ed("foo(\n    a)\n");
    e.join_lines();
    assert_eq!(e.doc.text(), "foo(a)\n");
    assert_eq!(e.primary().head(), ts(4));
}

#[test]
fn shift_arrows_in_column_mode_grow_a_block() {
    let o = opts();
    let mut e = ed("alpha = 1\nbeta = 2\ngamma = 3\n");
    e.column_mode = true;
    caret_at(&mut e, 0, 0);
    e.extend_block_selection(1, 0);
    e.extend_block_selection(1, 0);
    e.extend_block_selection(0, 1);
    assert_eq!(e.carets.len(), 3);
    e.insert_text("X", &o);
    assert_eq!(e.doc.text(), "Xlpha = 1\nXeta = 2\nXamma = 3\n");
}

#[test]
fn shrink_selection_forgets_steps_after_the_caret_moved() {
    let mut e = ed("def f(a, b):\n    return a + b\n\nx = 1\n");
    caret_at(&mut e, 1, 12);
    e.extend_selection();
    e.extend_selection();
    assert!(e.primary().has_selection());
    // Ctrl+W then Ctrl+Shift+W goes back one step.
    e.shrink_selection();
    assert!(e.primary().has_selection());
    // After moving elsewhere, shrinking does not jump back to the old selection.
    caret_at(&mut e, 3, 0);
    e.shrink_selection();
    assert_eq!(e.primary().head(), e.primary().sel.anchor);
    assert_eq!(e.primary_position(), Position::new(3, 0));
}

#[test]
fn line_commands_take_collapsed_folds_along() {
    let src = "def f():\n    a = 1\n    b = 2\nx = 0\n";
    // Delete Line on a collapsed def removes its body too.
    let mut e = ed(src);
    e.toggle_fold_at_line(0);
    caret_at(&mut e, 0, 0);
    e.delete_lines();
    assert_eq!(e.doc.text(), "x = 0\n");
    // Duplicate Line duplicates the folded block.
    let mut e = ed(src);
    e.toggle_fold_at_line(0);
    caret_at(&mut e, 0, 0);
    e.duplicate();
    assert_eq!(
        e.doc.text(),
        "def f():\n    a = 1\n    b = 2\ndef f():\n    a = 1\n    b = 2\nx = 0\n"
    );
    // Moving `x = 0` up passes the collapsed def as a whole.
    let mut e = ed(src);
    e.toggle_fold_at_line(0);
    caret_at(&mut e, 3, 0);
    e.move_lines(false);
    assert_eq!(e.doc.text(), "x = 0\ndef f():\n    a = 1\n    b = 2\n");
}

#[test]
fn breadcrumbs_at_column_one_of_a_class_name_it() {
    let src = "def greet():\n    return 1\n\n\nclass Greeter:\n    def say(self):\n        pass\n";
    let mut e = ed(src);
    caret_at(&mut e, 4, 0);
    let names: Vec<String> = e.breadcrumbs().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, vec!["Greeter".to_string()]);
    caret_at(&mut e, 6, 8);
    let names: Vec<String> = e.breadcrumbs().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, vec!["Greeter".to_string(), "say()".to_string()]);
}

#[test]
fn converting_two_space_indents_to_tabs_gives_one_tab_per_level() {
    let o = EditOptions {
        indent_size: 2,
        tab_size: 4,
        ..opts()
    };
    let mut e = ed("def f(a):\n  if a:\n    return 1\n  return 2\n");
    e.convert_indents(true, &o);
    assert_eq!(
        e.doc.text(),
        "def f(a):\n\tif a:\n\t\treturn 1\n\treturn 2\n"
    );
}

#[test]
fn accepting_a_completion_completes_at_every_caret() {
    // QA: with several carets (an in-place rename), Enter in the lookup
    // completed at the primary caret only and dropped the others.
    let heads = |e: &EditorState| {
        e.carets
            .iter()
            .map(|c| u32::from(c.head()))
            .collect::<Vec<_>>()
    };
    let mut e = ed("x = va\nprint(va)\nva.x\n");
    e.set_carets(vec![Caret::at(ts(6)), Caret::at(ts(15)), Caret::at(ts(19))]);
    let (edits, carets) = e.completion_edits(ts(4), false, |_| ("value".to_string(), None));
    assert!(e.apply(
        edits,
        carets,
        rustpy_text::EditGroup::Separate,
        Some("Complete")
    ));
    assert_eq!(e.doc.text(), "x = value\nprint(value)\nvalue.x\n");
    assert_eq!(heads(&e), vec![9, 21, 28]);
    // Parentheses and the caret offset per caret; no second `(` before an existing one.
    let mut e = ed("pr\npr(1)\n");
    e.set_carets(vec![Caret::at(ts(2)), Caret::at(ts(5))]);
    let (edits, carets) = e.completion_edits(ts(0), false, |next| {
        if next == Some('(') {
            ("print".into(), None)
        } else {
            ("print()".into(), Some(6))
        }
    });
    assert!(e.apply(edits, carets, rustpy_text::EditGroup::Separate, None));
    assert_eq!(e.doc.text(), "print()\nprint(1)\n");
    assert_eq!(heads(&e), vec![6, 13]);
    // Tab replaces the rest of each identifier; a caret after other text keeps its place.
    let mut e = ed("vaxx\nvayy\nzz\n");
    e.set_carets(vec![Caret::at(ts(2)), Caret::at(ts(7)), Caret::at(ts(12))]);
    let (edits, carets) = e.completion_edits(ts(0), true, |_| ("value".to_string(), None));
    assert!(e.apply(edits, carets, rustpy_text::EditGroup::Separate, None));
    assert_eq!(e.doc.text(), "value\nvalue\nzz\n");
    assert_eq!(heads(&e), vec![5, 11, 14]);
}
