//! Logical selection formatting: capture statement headers/simple statements,
//! expand indentation-dependent suites, then publish only those checked edits.
use crate::{comments_preserved, format_source, semantic_fingerprint, FormatError, FormatOptions};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind, SyntaxNode, TextRange, TextSize,
};

/// Edits use original byte coordinates. Mapped ranges own the corresponding
/// replacement text for a second pass; original line numbers must not be reused.
pub struct LogicalRangeEdits {
    pub edits: Vec<(TextRange, String)>,
    pub mapped_ranges: Vec<TextRange>,
}
struct Unit {
    node: SyntaxNode,
    header: TextRange,
    whole: TextRange,
}
fn unsafe_range(message: &str) -> FormatError {
    FormatError::Unsafe(message.into())
}
fn range(start: usize, end: usize) -> Result<TextRange, FormatError> {
    let start = u32::try_from(start)
        .map_err(|_| unsafe_range("logical selection exceeds offset capacity"))?;
    let end = u32::try_from(end)
        .map_err(|_| unsafe_range("logical selection exceeds offset capacity"))?;
    Ok(TextRange::new(TextSize::new(start), TextSize::new(end)))
}
fn starts(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(at, _)| at + 1))
        .collect()
}
fn line_of(starts: &[usize], byte: usize) -> usize {
    starts
        .partition_point(|start| *start <= byte)
        .saturating_sub(1)
}
fn physical(
    source: &str,
    lines: &[usize],
    start: usize,
    end: usize,
) -> Result<TextRange, FormatError> {
    let first = line_of(lines, start);
    let last = line_of(lines, end.saturating_sub(1).max(start));
    let mut after = last + 1;
    // Separators belong to the preceding logical unit, never the next statement.
    while let Some(&begin) = lines.get(after) {
        let finish = lines.get(after + 1).copied().unwrap_or(source.len());
        if !source[begin..finish].trim().is_empty() {
            break;
        }
        after += 1;
    }
    range(
        lines[first],
        lines.get(after).copied().unwrap_or(source.len()),
    )
}
fn units(source: &str, root: &SyntaxNode) -> Result<Vec<Unit>, FormatError> {
    use SyntaxKind::*;
    let lines = starts(source);
    root.descendants()
        .filter(|node| {
            ast::Stmt::cast(node.clone()).is_some()
                || matches!(
                    node.kind(),
                    ELIF_CLAUSE | ELSE_CLAUSE | EXCEPT_CLAUSE | FINALLY_CLAUSE | CASE_CLAUSE
                )
        })
        .map(|node| {
            let start = usize::from(node.text_range().start());
            let end = usize::from(node.text_range().end());
            // A compound's header is independently owned from the body. A
            // one-line suite shares its physical line and is captured together.
            let body = node
                .children()
                .find(|child| child.kind() == BLOCK)
                .and_then(|block| {
                    block
                        .children()
                        .find_map(ast::Stmt::cast)
                        .map(|statement| statement.syntax().clone())
                })
                .or_else(|| node.children().find(|child| child.kind() == CASE_CLAUSE));
            let header_end = body
                .as_ref()
                .map(|body| {
                    let body_line = line_of(&lines, usize::from(body.text_range().start()));
                    if body_line == line_of(&lines, start) {
                        end
                    } else {
                        lines[body_line]
                    }
                })
                .unwrap_or(end);
            Ok(Unit {
                header: physical(source, &lines, start, header_end)?,
                whole: physical(source, &lines, start, end)?,
                node,
            })
        })
        .collect()
}
fn intersects(selection: TextRange, owned: TextRange, eof: TextSize) -> bool {
    if selection.is_empty() {
        owned.start() <= selection.start()
            && (selection.start() < owned.end()
                // Only the true document end is closed. Interior boundaries
                // belong to the following logical unit, never its predecessor.
                || (!owned.is_empty() && selection.start() == eof && owned.end() == eof))
    } else {
        selection.start() < owned.end() && owned.start() < selection.end()
    }
}
fn indentation(source: &str, at: TextSize) -> &str {
    let at = usize::from(at);
    let line = source[..at].rfind('\n').map_or(0, |at| at + 1);
    let prefix = &source[line..at];
    &prefix[..prefix.len() - prefix.trim_start_matches([' ', '\t']).len()]
}
/// Unlike `format_range`, deliberately expands selections to their containing
/// logical units. Unrelated statements are retained byte-for-byte. If changing
/// indentation requires a containing suite, that suite is captured explicitly.
/// The full formatter supplies candidate fragments; its unrelated output is
/// never substituted into the caller's document.
pub fn format_logical_ranges(
    source: &str,
    selections: &[TextRange],
    options: &FormatOptions,
) -> Result<LogicalRangeEdits, FormatError> {
    range(0, source.len())?;
    if selections
        .iter()
        .any(|selection| usize::from(selection.end()) > source.len())
    {
        return Err(unsafe_range("logical selection is outside the source"));
    }
    if selections.is_empty() {
        return Ok(LogicalRangeEdits {
            edits: Vec::new(),
            mapped_ranges: Vec::new(),
        });
    }
    let formatted = format_source(source, options)?;
    range(0, formatted.len())?;
    let before = rustpy_syntax::parse(source).syntax_node();
    let after = rustpy_syntax::parse(&formatted).syntax_node();
    let old = units(source, &before)?;
    let new = units(&formatted, &after)?;
    if old.len() != new.len()
        || old
            .iter()
            .zip(&new)
            .any(|(old, new)| old.node.kind() != new.node.kind())
    {
        return Err(unsafe_range(
            "formatter changed logical statement correspondence",
        ));
    }
    let mut owned = Vec::new();
    for (index, unit) in old.iter().enumerate().filter(|(_, unit)| {
        selections
            .iter()
            .any(|selection| intersects(*selection, unit.header, TextSize::of(source)))
    }) {
        let mut owner = index;
        let mut entire = false;
        // Reindent only a captured complete suite, never an isolated child that
        // would leave sibling indentation invalid. Headers whose indentation is
        // unchanged do not acquire their unrelated body statements.
        while indentation(source, old[owner].node.text_range().start())
            != indentation(&formatted, new[owner].node.text_range().start())
        {
            let ancestor = old[owner]
                .node
                .ancestors()
                .skip(1)
                .find_map(|ancestor| old.iter().position(|unit| unit.node == ancestor));
            let Some(ancestor) = ancestor else {
                return Err(unsafe_range("cannot capture indentation owner"));
            };
            owner = ancestor;
            entire = true;
        }
        owned.push(if entire {
            (old[owner].whole, new[owner].whole)
        } else {
            (unit.header, new[index].header)
        });
    }
    owned.sort_by_key(|(old, _)| (old.start(), old.end()));
    let mut merged: Vec<(TextRange, TextRange)> = Vec::new();
    for (old, new) in owned {
        if let Some((previous_old, previous_new)) = merged.last_mut() {
            if old.start() <= previous_old.end() {
                *previous_old =
                    TextRange::new(previous_old.start(), previous_old.end().max(old.end()));
                *previous_new = TextRange::new(
                    previous_new.start().min(new.start()),
                    previous_new.end().max(new.end()),
                );
                continue;
            }
            if new.start() < previous_new.end() {
                return Err(unsafe_range(
                    "selected and unselected statements share formatted ownership",
                ));
            }
        }
        merged.push((old, new));
    }
    let mut edits = Vec::new();
    let mut mapped_ranges = Vec::new();
    let (mut old_cursor, mut new_cursor) = (0usize, 0usize);
    for (old, new) in merged {
        let text = &formatted[usize::from(new.start())..usize::from(new.end())];
        let mapped_start = new_cursor + usize::from(old.start()) - old_cursor;
        let mapped_end = mapped_start + text.len();
        mapped_ranges.push(range(mapped_start, mapped_end)?);
        old_cursor = usize::from(old.end());
        new_cursor = mapped_end;
        if &source[usize::from(old.start())..usize::from(old.end())] != text {
            edits.push((old, text.to_owned()));
        }
    }
    let applied = rustpy_syntax::printer::apply_text_edits(source, &edits)
        .map_err(|error| unsafe_range(&error.to_string()))?;
    let original_meaning = semantic_fingerprint(source);
    if !comments_preserved(source, &applied)
        || original_meaning.is_none()
        || original_meaning != semantic_fingerprint(&applied)
    {
        return Err(unsafe_range(
            "logical selection changed comments, syntax or meaning",
        ));
    }
    Ok(LogicalRangeEdits {
        edits,
        mapped_ranges,
    })
}
