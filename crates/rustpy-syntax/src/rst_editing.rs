//! Source-preserving reStructuredText editor support, independent of Python ASTs.
use std::ops::Range;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub range: Range<usize>,
    pub names: Vec<String>,
    pub suffix: &'static str,
}
pub const DIRECTIVES: &[&str] = &[
    "admonition",
    "attention",
    "caution",
    "code",
    "code-block",
    "compound",
    "container",
    "contents",
    "csv-table",
    "danger",
    "date",
    "default-role",
    "deprecated",
    "epigraph",
    "error",
    "figure",
    "footer",
    "header",
    "highlights",
    "hint",
    "image",
    "important",
    "include",
    "index",
    "line-block",
    "list-table",
    "literalinclude",
    "math",
    "meta",
    "note",
    "only",
    "parsed-literal",
    "pull-quote",
    "raw",
    "replace",
    "role",
    "rubric",
    "sectionauthor",
    "sectnum",
    "section-numbering",
    "seealso",
    "sidebar",
    "table",
    "target-notes",
    "tip",
    "title",
    "toctree",
    "topic",
    "unicode",
    "versionadded",
    "versionchanged",
    "warning",
];
pub const ROLES: &[&str] = &[
    "abbreviation",
    "acronym",
    "any",
    "attr",
    "class",
    "code",
    "command",
    "const",
    "data",
    "dfn",
    "doc",
    "download",
    "emphasis",
    "envvar",
    "exc",
    "file",
    "func",
    "guilabel",
    "kbd",
    "literal",
    "math",
    "menuselection",
    "meth",
    "mod",
    "numref",
    "obj",
    "option",
    "pep",
    "program",
    "ref",
    "rfc",
    "samp",
    "strong",
    "subscript",
    "superscript",
    "term",
    "title-reference",
];
pub fn complete(
    source: &str,
    caret: usize,
    directives: &[String],
    roles: &[String],
    references: &[String],
) -> Option<Completion> {
    if source.len() > 2 * 1024 * 1024 || caret > source.len() || !source.is_char_boundary(caret) {
        return None;
    }
    let start = source[..caret].rfind('\n').map_or(0, |i| i + 1);
    let before = &source[start..caret];
    if before.len() > 16384 {
        return None;
    }
    let trimmed = before.trim_start();
    let indent = before.len() - trimmed.len();
    if let Some(completion) = complete_option(source, start, caret, indent, trimmed) {
        return Some(completion);
    }
    let (range, prefix, values, suffix) = if let Some(prefix) = trimmed.strip_prefix(".. ") {
        if !prefix
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ':'))
            || prefix.contains("::")
        {
            return None;
        }
        let suffix = if source[caret..].starts_with("::") {
            ""
        } else {
            ":: "
        };
        (
            start + indent + 3..caret,
            prefix,
            DIRECTIVES
                .iter()
                .map(|s| s.to_string())
                .chain(directives.iter().cloned())
                .collect::<Vec<_>>(),
            suffix,
        )
    } else if let Some(open) = before
        .rfind(":ref:`")
        .or_else(|| before.rfind(":doc:`"))
        .or_else(|| before.rfind(":any:`"))
    {
        let offset = open + before[open..].find('`')? + 1;
        let prefix = &before[offset..];
        if prefix.contains('`') {
            return None;
        }
        (
            start + offset..caret,
            prefix,
            references.to_vec(),
            if source[caret..].starts_with('`') {
                ""
            } else {
                "`"
            },
        )
    } else {
        let token_start = before
            .rfind(|c: char| c.is_whitespace() || matches!(c, '`' | '(' | ')'))
            .map_or(0, |i| i + before[i..].chars().next().unwrap().len_utf8());
        let token = &before[token_start..];
        let prefix = token.strip_prefix(':')?;
        if !prefix
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ':' | '_' | '-'))
        {
            return None;
        }
        (
            start + token_start + 1..caret,
            prefix,
            ROLES
                .iter()
                .map(|s| s.to_string())
                .chain(roles.iter().cloned())
                .collect::<Vec<_>>(),
            if source[caret..].starts_with(':') {
                ""
            } else {
                ":`"
            },
        )
    };
    let mut names: Vec<_> = values
        .into_iter()
        .filter(|s| s.starts_with(prefix) && s.len() <= 256)
        .collect();
    names.sort();
    names.dedup();
    names.truncate(256);
    Some(Completion {
        range,
        names,
        suffix,
    })
}
fn inline(source: &str) -> String {
    let mut result = String::new();
    let mut rest = source;
    let mut steps = 0;
    while let Some(start) = rest.find('`') {
        steps += 1;
        if steps > 4096 {
            break;
        }
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else {
            result.push_str(&rest[start..]);
            return result;
        };
        let value = &after[..end];
        if let Some((label, url)) = value
            .rsplit_once(" <")
            .filter(|(_, url)| url.ends_with('>'))
        {
            result.push_str(&format!("[{label}]({})", &url[..url.len() - 1]));
        } else {
            result.push('`');
            result.push_str(value);
            result.push('`');
        }
        rest = after[end + 1..]
            .strip_prefix('_')
            .unwrap_or(&after[end + 1..]);
    }
    result.push_str(rest);
    result
}
/// Local, non-executing split preview. Full Docutils/Sphinx rendering is exposed
/// separately as a real cancellable Run task, including extension transforms.
pub fn preview_markdown(source: &str) -> String {
    if source.len() > 2 * 1024 * 1024 {
        return "Preview is limited to 2 MiB documents.".into();
    }
    let document = crate::rst::parse(source);
    let mut output = String::new();
    let mut seen = 0;
    for block in document.blocks.into_iter().take(10000) {
        if block.range.start < seen {
            continue;
        }
        seen = block.range.end;
        match block.kind {
            crate::rst::Kind::Heading {
                title, adornment, ..
            } => {
                let level = match adornment {
                    '=' => 1,
                    '-' => 2,
                    '~' => 3,
                    _ => 4,
                };
                output.push_str(&"#".repeat(level));
                output.push(' ');
                output.push_str(source[title].trim());
                output.push_str("\n\n");
            }
            crate::rst::Kind::Literal => {
                output.push_str("```\n");
                for line in source[block.range].lines() {
                    output.push_str(line.strip_prefix("    ").unwrap_or(line));
                    output.push('\n');
                }
                output.push_str("```\n\n");
            }
            crate::rst::Kind::Directive { name, arguments } => {
                let directive = &source[name];
                let arguments = source[arguments].trim();
                let text = &source[block.range];
                let body = text
                    .lines()
                    .skip(1)
                    .map(|line| line.strip_prefix("   ").unwrap_or(line))
                    .collect::<Vec<_>>()
                    .join("\n");
                match directive {
                    "code" | "code-block" | "sourcecode" => {
                        output.push_str(&format!("```{arguments}\n{body}\n```\n\n"))
                    }
                    "image" | "figure" => output.push_str(&format!(
                        "![{}]({arguments})\n\n{}\n",
                        if directive == "figure" {
                            body.as_str()
                        } else {
                            "image"
                        },
                        body
                    )),
                    _ => {
                        output.push_str(&format!("> **{directive}** {}\n", inline(arguments)));
                        for line in body.lines() {
                            output.push_str("> ");
                            output.push_str(&inline(line));
                            output.push('\n');
                        }
                        output.push('\n');
                    }
                }
            }
            crate::rst::Kind::ExplicitMarkup => {}
            _ => {
                output.push_str(&inline(&source[block.range]));
                output.push_str("\n\n");
            }
        }
    }
    output
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directive_role_and_reference_replacements_are_precise() {
        let input = "  .. custom:";
        let c = complete(input, input.len(), &["custom:panel".into()], &[], &[]).unwrap();
        assert_eq!(&input[c.range], "custom:");
        assert_eq!(c.names, ["custom:panel"]);
        let input = ":py:fu";
        let c = complete(input, input.len(), &[], &["py:func".into()], &[]).unwrap();
        assert_eq!(c.names, ["py:func"]);
        assert_eq!(&input[c.range], "py:fu");
        let input = ":ref:`some";
        assert_eq!(
            complete(input, input.len(), &[], &[], &["some-target".into()])
                .unwrap()
                .names,
            ["some-target"]
        );
        assert!(complete("prose", 5, &[], &[], &[]).is_none());
    }
    #[test]
    fn existing_delimiters_are_not_duplicated() {
        let c = complete(".. note::", 7, &[], &[], &[]).unwrap();
        assert_eq!(c.suffix, "");
        assert_eq!(&".. note::"[c.range], "note");
    }
    #[test]
    fn preview_is_rst_aware() {
        let preview = preview_markdown(
            "Title\n=====\n\n.. note:: Read this\n\n   Body.\n\n.. code-block:: python\n\n   print(1)\n",
        );
        assert!(preview.contains("# Title"));
        assert!(preview.contains("> **note**"));
        assert!(preview.contains("```python"));
        assert!(preview.contains("print(1)"));
    }
}

/// Builtin directive option names follow the Docutils reference. Sphinx-specific
/// directives are included without executing project extension code.
/// https://docutils.sourceforge.io/docs/ref/rst/directives.html
fn options(name: &str) -> Vec<&'static str> {
    let mut names = match name {
        "image" | "figure" => vec![
            "alt", "height", "width", "scale", "align", "target", "loading",
        ],
        "table" | "list-table" | "csv-table" => vec!["widths", "width", "align"],
        "code" | "code-block" | "sourcecode" => vec!["number-lines"],
        "contents" => vec!["depth", "local", "backlinks"],
        "sectnum" | "section-numbering" => vec!["depth", "prefix", "suffix", "start"],
        "include" | "literalinclude" => vec![
            "encoding",
            "literal",
            "code",
            "start-line",
            "end-line",
            "start-after",
            "end-before",
            "number-lines",
            "tab-width",
        ],
        "toctree" => vec![
            "maxdepth",
            "caption",
            "glob",
            "hidden",
            "includehidden",
            "titlesonly",
            "numbered",
            "reversed",
            "name",
        ],
        _ => Vec::new(),
    };
    match name {
        "figure" => names.extend(["figwidth", "figclass"]),
        "list-table" | "csv-table" => names.extend(["header-rows", "stub-columns"]),
        _ => (),
    }
    if name == "csv-table" {
        names.extend([
            "header",
            "file",
            "url",
            "encoding",
            "delim",
            "quote",
            "escape",
            "keepspace",
        ]);
    }
    if DIRECTIVES.contains(&name) {
        names.extend(["class", "name"]);
    }
    names.sort_unstable();
    names.dedup();
    names
}
fn complete_option(
    source: &str,
    start: usize,
    caret: usize,
    indent: usize,
    trimmed: &str,
) -> Option<Completion> {
    if indent == 0 {
        return None;
    }
    let prefix = trimmed.strip_prefix(':')?;
    if !prefix
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return None;
    }
    let mut used = Vec::new();
    let mut directive = None;
    for line in source[..start].lines().rev().take(256) {
        let content = line.trim_start();
        if content.is_empty() {
            return None;
        } // A blank separates directive content from options.
        let depth = line.len() - content.len();
        if depth < indent {
            directive = content
                .strip_prefix(".. ")?
                .split_once("::")
                .map(|(name, _)| name.trim().to_ascii_lowercase());
            break;
        }
        let option = content.strip_prefix(':')?.split_once(':')?.0;
        used.push(option.to_owned());
    }
    let names = options(directive.as_deref()?)
        .into_iter()
        .filter(|name| name.starts_with(prefix) && !used.iter().any(|used| used == name))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if names.is_empty() {
        return None;
    }
    Some(Completion {
        range: start + indent + 1..caret,
        names,
        suffix: if source[caret..].starts_with(':') {
            ""
        } else {
            ": "
        },
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceEdit {
    pub range: Range<usize>,
    pub replacement: String,
}
/// Changes the current heading, or promotes the current text line to a heading.
pub fn section_edit(
    source: &str,
    caret: usize,
    adornment: char,
    overline: bool,
) -> Result<SourceEdit, String> {
    if source.len() > 2 * 1024 * 1024 || caret > source.len() || !source.is_char_boundary(caret) {
        return Err("Invalid or oversized reStructuredText source".into());
    }
    if !matches!(adornment, '=' | '-' | '~' | '^' | '"' | '+' | '#') {
        return Err("Unsupported section adornment".into());
    }
    let heading = crate::rst::parse(source)
        .blocks
        .into_iter()
        .find_map(|block| {
            if block.range.start <= caret && caret < block.range.end {
                if let crate::rst::Kind::Heading { title, .. } = block.kind {
                    return Some((block.range, title));
                }
            }
            None
        });
    let (range, title, indent) = if let Some((range, title)) = heading {
        let line_start = source[..title.start].rfind('\n').map_or(0, |i| i + 1);
        let indent = source[line_start..title.start].to_owned();
        (range, source[title].trim().to_owned(), indent)
    } else {
        let start = source[..caret].rfind('\n').map_or(0, |i| i + 1);
        let end = source[caret..]
            .find('\n')
            .map_or(source.len(), |i| caret + i + 1);
        let line = &source[start..end];
        let indent = line[..line.len() - line.trim_start().len()].to_owned();
        (start..end, line.trim().to_owned(), indent)
    };
    if title.is_empty() || title.contains(['\n', '\r']) {
        return Err("Choose a nonempty title line".into());
    }
    let border = adornment
        .to_string()
        .repeat(unicode_width::UnicodeWidthStr::width(title.as_str()).max(3));
    let mut replacement = String::new();
    if overline {
        replacement.push_str(&indent);
        replacement.push_str(&border);
        replacement.push('\n');
    }
    replacement.push_str(&indent);
    replacement.push_str(&title);
    replacement.push('\n');
    replacement.push_str(&indent);
    replacement.push_str(&border);
    if range.end < source.len() || source[range.clone()].ends_with('\n') {
        replacement.push('\n');
    }
    Ok(SourceEdit { range, replacement })
}
/// A source-preserving helper for selected tab-separated rows (including empty cells).
pub fn grid_table(text: &str, header: bool) -> Result<String, String> {
    if text.len() > 256 * 1024 {
        return Err("Table input exceeds 256 KiB".into());
    }
    let rows = text
        .trim_end_matches(['\r', '\n'])
        .lines()
        .map(|line| {
            line.trim_end_matches('\r')
                .split('\t')
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if rows.is_empty()
        || rows.len() > 200
        || columns == 0
        || columns > 30
        || rows.iter().any(|row| row.len() != columns)
    {
        return Err("Select 1–200 tab-separated rows with the same 1–30 column count".into());
    }
    let widths = (0..columns)
        .map(|column| {
            rows.iter()
                .map(|row| unicode_width::UnicodeWidthStr::width(row[column].as_str()))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect::<Vec<_>>();
    if widths.iter().sum::<usize>() > 4096 {
        return Err("Table is too wide".into());
    }
    let border = |fill: char| {
        format!(
            "+{}+\n",
            widths
                .iter()
                .map(|width| fill.to_string().repeat(width + 2))
                .collect::<Vec<_>>()
                .join("+")
        )
    };
    let mut result = border('-');
    for (index, row) in rows.iter().enumerate() {
        result.push('|');
        for (cell, width) in row.iter().zip(&widths) {
            result.push(' ');
            result.push_str(cell);
            result.push_str(
                &" ".repeat(width - unicode_width::UnicodeWidthStr::width(cell.as_str()) + 1),
            );
            result.push('|');
        }
        result.push('\n');
        result.push_str(&border(if header && index == 0 { '=' } else { '-' }));
    }
    Ok(result)
}
#[cfg(test)]
mod helper_tests {
    use super::*;
    #[test]
    fn options_belong_to_directive_not_body_roles_and_preserve_existing_delimiters() {
        let input = ".. image:: picture.png\n   :alt: image\n   :wi";
        let c = complete(input, input.len(), &[], &[], &[]).unwrap();
        assert_eq!(c.names, ["width"]);
        assert_eq!(&input[c.range], "wi");
        assert_eq!(c.suffix, ": ");
        let input = ".. image:: picture.png\n   :width:";
        let c = complete(input, input.len() - 1, &[], &[], &[]).unwrap();
        assert_eq!(c.suffix, "");
        let input = ".. note::\n\n   :py:fu";
        let c = complete(input, input.len(), &[], &["py:func".into()], &[]).unwrap();
        assert_eq!(c.names, ["py:func"]);
    }
    #[test]
    fn heading_replacement_retains_surroundings_and_unicode_display_width() {
        let text = "before\n\n界e\u{301}\n\nend\n";
        let edit = section_edit(text, text.find('界').unwrap(), '=', true).unwrap();
        let mut changed = text.to_owned();
        changed.replace_range(edit.range, &edit.replacement);
        assert_eq!(changed, "before\n\n===\n界e\u{301}\n===\n\nend\n");
    }
    #[test]
    fn grid_table_keeps_unicode_and_empty_cells_and_refuses_ragged_rows() {
        let table = grid_table("Name\tValue\n界\t\n", true).unwrap();
        assert!(table.contains("| 界   |       |"));
        assert!(table.contains("+======+=======+"));
        assert!(grid_table("a\tb\nc", false).is_err());
    }
}
