//! Conversion between notebooks and Python scripts in the "percent" format used by Jupytext,
//! VS Code and Spyder:
//!
//! ```text
//! # %% [markdown]
//! # # Title
//! # Some *text*
//!
//! # %% Optional title
//! import numpy as np
//! ```
//!
//! A cell starts at a `# %%` (or `#%%`) line; `[markdown]`/`[md]` or `[raw]` on that line selects
//! the cell type and the remaining text is a title, kept in the cell metadata as `title` (as
//! Jupytext does). Markdown and raw cells are written as comment lines. `# %%time` (a commented-out
//! cell magic) is not a marker.

use serde_json::Value;

use crate::model::{Cell, CellKind, Notebook};

pub(crate) fn to_py(nb: &Notebook) -> String {
    let mut out = String::new();
    for (i, cell) in nb.cells.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str("# %%");
        if let Some(title) = cell.metadata.get("title").and_then(Value::as_str) {
            let title = title.lines().next().unwrap_or("").trim();
            if !title.is_empty() {
                out.push(' ');
                out.push_str(title);
            }
        }
        match cell.kind {
            CellKind::Code => {}
            CellKind::Markdown => out.push_str(" [markdown]"),
            CellKind::Raw => out.push_str(" [raw]"),
        }
        out.push('\n');
        let source = cell.source.trim_end_matches(['\n', '\r']);
        if source.is_empty() {
            continue;
        }
        for line in source.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            match cell.kind {
                CellKind::Code => out.push_str(line),
                _ if line.is_empty() => out.push('#'),
                _ => {
                    out.push_str("# ");
                    out.push_str(line);
                }
            }
            out.push('\n');
        }
    }
    out
}

pub(crate) fn from_py(script: &str) -> Notebook {
    struct Pending<'a> {
        kind: CellKind,
        title: Option<String>,
        lines: Vec<&'a str>,
    }
    let mut nb = Notebook::empty();
    let mut preamble: Vec<&str> = Vec::new();
    let mut current: Option<Pending> = None;
    let script = script.strip_prefix('\u{feff}').unwrap_or(script);
    for line in script.lines() {
        if let Some((kind, title)) = parse_marker(line) {
            if let Some(done) = current.take() {
                push_cell(&mut nb, done.kind, done.title, &done.lines);
            }
            current = Some(Pending {
                kind,
                title,
                lines: Vec::new(),
            });
        } else {
            match &mut current {
                Some(pending) => pending.lines.push(line),
                None => preamble.push(line),
            }
        }
    }
    if let Some(done) = current.take() {
        push_cell(&mut nb, done.kind, done.title, &done.lines);
    }
    if preamble.iter().any(|l| !l.trim().is_empty()) {
        let source = trim_blank_lines(&preamble).join("\n");
        nb.insert_cell(0, Cell::code(source));
    }
    nb
}

fn push_cell(nb: &mut Notebook, kind: CellKind, title: Option<String>, lines: &[&str]) {
    let body = trim_blank_lines(lines);
    let source = match kind {
        CellKind::Code => body.join("\n"),
        _ => body
            .iter()
            .map(|l| uncomment(l))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    let mut cell = Cell::new(kind, source);
    if let Some(title) = title {
        cell.metadata.insert("title".into(), title.into());
    }
    let end = nb.cells.len();
    nb.insert_cell(end, cell);
}

fn trim_blank_lines<'a, 'b>(lines: &'b [&'a str]) -> &'b [&'a str] {
    let start = lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(start, |i| i + 1);
    &lines[start..end]
}

/// A markdown/raw cell line without its comment prefix.
fn uncomment(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix("# ") {
        rest
    } else if let Some(rest) = line.strip_prefix('#') {
        rest
    } else {
        line
    }
}

/// For a cell marker line, the cell type and title.
fn parse_marker(line: &str) -> Option<(CellKind, Option<String>)> {
    let rest = line
        .strip_prefix('#')?
        .trim_start_matches([' ', '\t'])
        .strip_prefix("%%")?;
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None; // `# %%time`: a commented-out cell magic
    }
    let rest = rest.trim();
    let lower = rest.to_ascii_lowercase();
    for (tag, kind) in [
        ("[markdown]", CellKind::Markdown),
        ("[md]", CellKind::Markdown),
        ("[raw]", CellKind::Raw),
    ] {
        if let Some(pos) = lower.find(tag) {
            // ASCII lowercasing keeps byte offsets, so `pos` is valid in `rest` too.
            let title = format!("{} {}", rest[..pos].trim(), rest[pos + tag.len()..].trim());
            return Some((kind, non_empty(title.trim())));
        }
    }
    Some((CellKind::Code, non_empty(rest)))
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers() {
        assert_eq!(parse_marker("# %%"), Some((CellKind::Code, None)));
        assert_eq!(parse_marker("#%%"), Some((CellKind::Code, None)));
        assert_eq!(
            parse_marker("# %% Load data  "),
            Some((CellKind::Code, Some("Load data".into())))
        );
        assert_eq!(
            parse_marker("# %% [markdown]"),
            Some((CellKind::Markdown, None))
        );
        assert_eq!(parse_marker("# %% [MD]"), Some((CellKind::Markdown, None)));
        assert_eq!(
            parse_marker("# %% Intro [markdown] tags=[\"x\"]"),
            Some((CellKind::Markdown, Some("Intro tags=[\"x\"]".into())))
        );
        assert_eq!(parse_marker("# %% [raw]"), Some((CellKind::Raw, None)));
        assert_eq!(parse_marker("# %%time"), None);
        assert_eq!(parse_marker("    # %%"), None);
        assert_eq!(parse_marker("# a comment"), None);
        assert_eq!(parse_marker("x = 1  # %%"), None);
    }

    #[test]
    fn uncomments_markdown() {
        assert_eq!(uncomment("# # Title"), "# Title");
        assert_eq!(uncomment("#"), "");
        assert_eq!(uncomment("#tight"), "tight");
        assert_eq!(uncomment("not a comment"), "not a comment");
    }
}
