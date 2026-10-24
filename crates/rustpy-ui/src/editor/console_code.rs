//! Code the editor sends to the Python Console (roadmap §2.11): the selection
//! or the statement at the caret (*Execute Selection in Python Console*,
//! Alt+Shift+E), `#%%` code cells (*Run Cell*) and the `runfile(…)` command
//! echoed for *Run File in Python Console*.

use std::ops::Range;

use rustpy_core::{Position, TextRange, TextSize};

use super::{syntax_support, EditorState, Language};

/// True for a `#%%` / `# %%` cell marker (at column 0, as VS Code
/// and Spyder write them; `# %% [markdown]` titles included).
pub fn is_cell_marker(line: &str) -> bool {
    line.strip_prefix('#')
        .is_some_and(|rest| rest.trim_start_matches([' ', '\t']).starts_with("%%"))
}

/// 0-based lines of the cell markers in `text`.
pub fn cell_marker_lines(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| is_cell_marker(l))
        .map(|(i, _)| i)
        .collect()
}

/// A code cell of a `.py` file split by `#%%` markers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The cell's marker line; `None` for the code above the first marker.
    pub marker: Option<usize>,
    /// The cell's code lines (0-based, marker excluded); empty for an empty cell.
    pub lines: Range<usize>,
    /// Marker line of the next cell.
    pub next: Option<usize>,
}

/// The cell containing `line` (0-based); `None` when the file has no markers.
pub fn cell_at(text: &str, line: usize) -> Option<Cell> {
    let markers = cell_marker_lines(text);
    if markers.is_empty() {
        return None;
    }
    let idx = markers.partition_point(|&m| m <= line);
    let marker = idx.checked_sub(1).map(|i| markers[i]);
    let next = markers.get(idx).copied();
    let start = marker.map_or(0, |m| m + 1);
    let end = next.unwrap_or_else(|| text.lines().count()).max(start);
    Some(Cell {
        marker,
        lines: start..end,
        next,
    })
}

/// The code of `cell` in `text`, dedented, without surrounding blank lines.
pub fn cell_code(text: &str, cell: &Cell) -> String {
    let body: Vec<&str> = text
        .lines()
        .skip(cell.lines.start)
        .take(cell.lines.len())
        .collect();
    dedent(&body.join("\n"))
}

/// What *Execute Selection in Python Console* sends, and where the caret goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fragment {
    /// Dedented code; empty for a blank or comment line (nothing to run).
    pub code: String,
    /// Where the caret moves afterwards (the line below what was sent, same
    /// column); `None` keeps it (a selection was sent, or this is the last line).
    pub next_caret: Option<Position>,
}

/// The selection of the primary caret, or else the statement on the caret
/// line: a whole compound statement (`def`, `for`, `if … else`, decorated
/// definitions) or multi-line call when the line starts or continues one,
/// otherwise the line itself. The caret then moves below what was sent, for
/// line-by-line execution.
pub fn fragment_at_caret(state: &mut EditorState) -> Fragment {
    let caret = state.primary();
    if caret.has_selection() {
        let range = caret.range();
        let line_start = state
            .doc
            .line_range(state.doc.line_of(range.start()))
            .start();
        // A selection starting in the indentation keeps the first line's indent for dedenting.
        let before = state.doc.slice(TextRange::new(line_start, range.start()));
        let start = if before.trim().is_empty() {
            line_start
        } else {
            range.start()
        };
        return Fragment {
            code: dedent(&state.doc.slice(TextRange::new(start, range.end()))),
            next_caret: None,
        };
    }
    let pos = state.doc.offset_to_position(caret.head());
    let line = pos.line as usize;
    let below = |state: &EditorState, last: usize| {
        (last + 1 < state.doc.line_count()).then(|| Position::new(last as u32 + 1, pos.column))
    };
    let trimmed = state.doc.line_text(line).trim().to_string();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Fragment {
            code: String::new(),
            next_caret: below(state, line),
        };
    }
    let (first, last) = if state.language == Language::Python && !state.is_large_file() {
        statement_lines(state, line).unwrap_or((line, line))
    } else {
        (line, line)
    };
    let range = TextRange::new(
        state.doc.line_range(first).start(),
        state.doc.line_range(last).end(),
    );
    Fragment {
        code: dedent(&state.doc.slice(range)),
        next_caret: below(state, last),
    }
}

/// First and last line of the statement covering `line`.
fn statement_lines(state: &mut EditorState, line: usize) -> Option<(usize, usize)> {
    let text = state.doc.text();
    let parse = state.syntax().clone();
    let doc = &state.doc;
    let line_of = |off: TextSize| doc.line_of(off);
    let (first, last) =
        syntax_support::statement_unit(&parse, &text, doc.line_range(line), &line_of)?.lines;
    (first <= line && line <= last).then_some((first, last))
}

/// Removes the indentation common to the code lines of `code` (so a block
/// selected inside a function runs at top level) and surrounding blank lines.
///
/// Unlike `textwrap.dedent`, lines that continue a triple-quoted string, a
/// bracketed expression or a backslash continuation, and comment-only lines,
/// do not limit the common indentation; they lose it only where they have it.
pub fn dedent(code: &str) -> String {
    let lines: Vec<&str> = code.lines().collect();
    let starts = logical_line_starts(&lines);
    let mut common: Option<&str> = None;
    for (l, starts_statement) in lines.iter().zip(&starts) {
        let t = l.trim_start();
        if !*starts_statement || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let indent = &l[..l.len() - t.len()];
        common = Some(match common {
            None => indent,
            Some(c) => {
                &c[..c
                    .bytes()
                    .zip(indent.bytes())
                    .take_while(|(a, b)| a == b)
                    .count()]
            }
        });
    }
    let common = common.unwrap_or("");
    let out: Vec<&str> = lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                ""
            } else {
                l.strip_prefix(common).unwrap_or(l)
            }
        })
        .collect();
    let first = out.iter().position(|l| !l.is_empty()).unwrap_or(out.len());
    let last = out
        .iter()
        .rposition(|l| !l.is_empty())
        .map_or(first, |i| i + 1);
    out[first..last.max(first)].join("\n")
}

/// For each line: true when it starts a new logical line (it is not inside a
/// triple-quoted string, open brackets or after a backslash continuation).
fn logical_line_starts(lines: &[&str]) -> Vec<bool> {
    let mut out = Vec::with_capacity(lines.len());
    let mut triple: Option<&[u8]> = None;
    let mut depth: i32 = 0;
    let mut backslash = false;
    for line in lines {
        out.push(triple.is_none() && depth <= 0 && !backslash);
        backslash = false;
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if let Some(q) = triple {
                if b[i] == b'\\' {
                    i += 2;
                } else if b[i..].starts_with(q) {
                    triple = None;
                    i += 3;
                } else {
                    i += 1;
                }
                continue;
            }
            match b[i] {
                b'#' => break,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b'\\' if i + 1 == b.len() => backslash = true,
                q @ (b'"' | b'\'') => {
                    let quote3: &[u8] = if q == b'"' { b"\"\"\"" } else { b"'''" };
                    if b[i..].starts_with(quote3) {
                        triple = Some(quote3);
                        i += 3;
                        continue;
                    }
                    // One-line string: skip to its closing quote.
                    i += 1;
                    while i < b.len() && b[i] != q {
                        i += if b[i] == b'\\' { 2 } else { 1 };
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    out
}

/// A Python string literal for `s` (single quotes, backslash escapes).
pub fn py_str_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// The `runfile(…)` call shown in the console (and kept in its history) for
/// *Run File in Python Console*; running it again re-runs the file.
pub fn runfile_command(path: &str, args: &[String], wdir: Option<&str>) -> String {
    let mut cmd = format!("runfile({}", py_str_literal(path));
    if !args.is_empty() {
        let list: Vec<String> = args.iter().map(|a| py_str_literal(a)).collect();
        cmd.push_str(&format!(", args=[{}]", list.join(", ")));
    }
    if let Some(w) = wdir {
        cmd.push_str(&format!(", wdir={}", py_str_literal(w)));
    }
    cmd.push(')');
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Caret;

    fn state_at(text: &str, line: u32, column: u32) -> EditorState {
        let mut s = EditorState::from_text(text, Language::Python);
        s.move_primary_to(Position::new(line, column));
        s
    }

    fn sent(code: &str, next: Option<(u32, u32)>) -> Fragment {
        Fragment {
            code: code.to_string(),
            next_caret: next.map(|(l, c)| Position::new(l, c)),
        }
    }

    #[test]
    fn cell_markers() {
        assert!(is_cell_marker("#%%"));
        assert!(is_cell_marker("# %% Load data"));
        assert!(is_cell_marker("#\t%% [markdown]"));
        assert!(!is_cell_marker("    # %% indented"));
        assert!(!is_cell_marker("# % not a cell"));
        assert!(!is_cell_marker("x = 1  # %%"));
        let text = "import os\n# %% first\na = 1\n\n#%% second\nb = 2\n# %%\n";
        assert_eq!(cell_marker_lines(text), vec![1, 4, 6]);
    }

    #[test]
    fn cells_span_from_marker_to_the_next() {
        let text = "import os\n# %% first\na = 1\n\n#%% second\nb = 2\nc = 3\n# %%\n";
        // Code above the first marker is a cell of its own.
        assert_eq!(
            cell_at(text, 0),
            Some(Cell {
                marker: None,
                lines: 0..1,
                next: Some(1)
            })
        );
        let first = cell_at(text, 2).unwrap();
        assert_eq!(
            first,
            Cell {
                marker: Some(1),
                lines: 2..4,
                next: Some(4)
            }
        );
        assert_eq!(
            cell_at(text, 1).as_ref(),
            Some(&first),
            "the marker line belongs to its cell"
        );
        assert_eq!(cell_code(text, &first), "a = 1");
        let second = cell_at(text, 6).unwrap();
        assert_eq!(
            second,
            Cell {
                marker: Some(4),
                lines: 5..7,
                next: Some(7)
            }
        );
        assert_eq!(cell_code(text, &second), "b = 2\nc = 3");
        // A marker on the last line opens an empty cell; so do consecutive markers.
        let last = cell_at(text, 7).unwrap();
        assert_eq!((last.marker, last.lines.is_empty()), (Some(7), true));
        assert!(cell_at("#%%\n#%%\nx = 1\n", 0).unwrap().lines.is_empty());
        assert_eq!(
            cell_code(
                "#%%\n#%%\nx = 1\n",
                &cell_at("#%%\n#%%\nx = 1\n", 1).unwrap()
            ),
            "x = 1"
        );
        // A marker on the first line leaves no code above it.
        assert_eq!(cell_at("#%%\nx = 1", 0).unwrap().lines, 1..2);
        // Without markers there are no cells.
        assert_eq!(cell_at("a = 1\nb = 2\n", 0), None);
        // The last cell runs to the end of the file.
        assert_eq!(
            cell_at("#%%\nx = 1\ny = 2", 2).unwrap(),
            Cell {
                marker: Some(0),
                lines: 1..3,
                next: None
            }
        );
    }

    #[test]
    fn dedent_strips_common_code_indentation() {
        assert_eq!(
            dedent("    a = 1\n    if a:\n        b = 2\n"),
            "a = 1\nif a:\n    b = 2"
        );
        assert_eq!(dedent("\n\tx = 1\n\n\ty = 2\n\n"), "x = 1\n\ny = 2");
        // Comment lines and continuation lines do not limit the indentation.
        assert_eq!(dedent("        # note\n    y = 1\n"), "    # note\ny = 1");
        assert_eq!(
            dedent("    total = f(1,\n  2)\n    print(total)"),
            "total = f(1,\n  2)\nprint(total)"
        );
        // Triple-quoted strings at column 0 inside an indented block (SQL, docstrings).
        let sql = "    query = \"\"\"\nSELECT *\n    FROM t\n\"\"\"\n    run(query)\n";
        assert_eq!(
            dedent(sql),
            "query = \"\"\"\nSELECT *\nFROM t\n\"\"\"\nrun(query)"
        );
        assert_eq!(
            dedent("    s = '''é\n'''\n    t = 'ü'"),
            "s = '''é\n'''\nt = 'ü'"
        );
        // Strings containing quotes, brackets or `#` do not confuse the scan.
        assert_eq!(dedent("    s = '(#\"'\n    t = 2"), "s = '(#\"'\nt = 2");
        assert_eq!(
            dedent("    x = 1 + \\\n2\n    y = 3"),
            "x = 1 + \\\n2\ny = 3"
        );
        assert_eq!(dedent("plain"), "plain");
        assert_eq!(dedent("\n  \n"), "");
    }

    #[test]
    fn selection_is_sent_dedented_and_the_caret_stays() {
        let text = "def f():\n    a = 1\n    b = a + 1\n    return b\n";
        let mut s = EditorState::from_text(text, Language::Python);
        // From inside line 1's indentation to the end of line 2.
        let start = s.doc.position_to_offset(Position::new(1, 2));
        let end = s.doc.line_range(2).end();
        s.set_carets(vec![Caret::new(start, end)]);
        assert_eq!(fragment_at_caret(&mut s), sent("a = 1\nb = a + 1", None));
        // A selection starting after the indentation is sent as selected.
        let start = s.doc.position_to_offset(Position::new(2, 8));
        s.set_carets(vec![Caret::new(start, s.doc.line_range(2).end())]);
        assert_eq!(fragment_at_caret(&mut s), sent("a + 1", None));
    }

    #[test]
    fn caret_line_sends_the_whole_statement() {
        let text = "import math\n\n@decorate\ndef area(r):\n    return math.pi * r ** 2\n\nvalues = [\n    1,\n    2,\n]\nif values:\n    print(1)\nelse:\n    print(2)\n# trailing comment\n";
        // A simple statement: the line; the caret moves down, keeping its column.
        assert_eq!(
            fragment_at_caret(&mut state_at(text, 0, 3)),
            sent("import math", Some((1, 3)))
        );
        // A decorated function: decorator through the end of the body.
        assert_eq!(
            fragment_at_caret(&mut state_at(text, 3, 0)),
            sent(
                "@decorate\ndef area(r):\n    return math.pi * r ** 2",
                Some((5, 0))
            )
        );
        // Inside a multi-line literal: the whole assignment.
        assert_eq!(
            fragment_at_caret(&mut state_at(text, 8, 4)),
            sent("values = [\n    1,\n    2,\n]", Some((10, 4)))
        );
        // An `else:` line sends the whole `if` statement.
        assert_eq!(
            fragment_at_caret(&mut state_at(text, 12, 0)).code,
            "if values:\n    print(1)\nelse:\n    print(2)"
        );
        // A statement inside a block is dedented.
        assert_eq!(
            fragment_at_caret(&mut state_at(text, 11, 4)),
            sent("print(1)", Some((12, 4)))
        );
        // Blank and comment lines send nothing but still move the caret down.
        assert_eq!(
            fragment_at_caret(&mut state_at(text, 1, 0)),
            sent("", Some((2, 0)))
        );
        assert_eq!(
            fragment_at_caret(&mut state_at(text, 14, 0)),
            sent("", Some((15, 0)))
        );
        // The last line has no line below it.
        assert_eq!(
            fragment_at_caret(&mut state_at("x = 1", 0, 0)),
            sent("x = 1", None)
        );
        // Moving keeps the column where the next line is long enough.
        let mut s = state_at("abcdef\nxy\n", 0, 4);
        let f = fragment_at_caret(&mut s);
        s.move_primary_to(f.next_caret.unwrap());
        assert_eq!(s.primary_position(), Position::new(1, 2));
    }

    #[test]
    fn non_python_files_send_the_caret_line() {
        let mut s = EditorState::from_text("  print('hi')\nnext\n", Language::PlainText);
        assert_eq!(fragment_at_caret(&mut s), sent("print('hi')", Some((1, 0))));
    }

    #[test]
    fn python_literals_and_runfile_commands() {
        assert_eq!(py_str_literal("plain"), "'plain'");
        assert_eq!(
            py_str_literal("it's a\\b\n\t\u{1}é"),
            "'it\\'s a\\\\b\\n\\t\\x01é'"
        );
        assert_eq!(
            runfile_command("/p/a.py", &[], Some("/p")),
            "runfile('/p/a.py', wdir='/p')"
        );
        assert_eq!(
            runfile_command("/p/a.py", &["x".into(), "two words".into()], None),
            "runfile('/p/a.py', args=['x', 'two words'])"
        );
        assert_eq!(
            runfile_command("C:\\p\\a.py", &[], None),
            "runfile('C:\\\\p\\\\a.py')"
        );
    }
}
