//! Import block rendering, removal and grouping.
//! The caller supplies removable binding ranges after its suppression policy.

use rustpy_core::TextRange;
use rustpy_semantic::{
    edit_helpers::{ts, Edit},
    FileInfo,
};
use rustpy_syntax::ast::{self, AstNode};

/// Standard library top-level module names (from typeshed) for import grouping.
fn is_stdlib(module: &str) -> bool {
    rustpy_semantic::typeshed::is_stdlib(module.split('.').next().unwrap_or(module))
}

/// An import and the comments attached to its aliases/statement.
struct RenderedImport {
    module: String,
    lines: Vec<String>,
    orphans: Vec<String>,
}

fn binding_range(alias: &ast::ImportAlias) -> TextRange {
    alias
        .alias()
        .or_else(|| alias.name_token())
        .map(|t| t.text_range())
        .unwrap_or_default()
}

fn alias_text(alias: &ast::ImportAlias) -> String {
    let name = alias.name();
    match alias.alias() {
        Some(as_name) => format!("{name} as {}", as_name.text()),
        None => name,
    }
}

/// Include an inline comment even if the parser leaves it outside the statement.
fn statement_end(text: &str, stmt: &ast::Stmt) -> usize {
    let start = usize::from(stmt.text_range().start());
    let end = usize::from(stmt.text_range().end());
    let end = start + text[start..end].trim_end().len();
    let line_end = text[end..]
        .find('\n')
        .map(|n| end + n)
        .unwrap_or(text.len());
    let suffix = text[end..line_end].trim();
    if suffix.is_empty() || suffix.starts_with('#') {
        text[..line_end].trim_end_matches('\r').len()
    } else {
        end
    }
}

/// Comment tokens are safe to recognize by `#` here: import statements contain
/// identifiers and punctuation, never string literals.
fn comments(text: &str, start: usize, end: usize) -> Vec<(usize, String)> {
    let mut offset = start;
    let mut out = Vec::new();
    for line in text[start..end].split_inclusive('\n') {
        if let Some(at) = line.find('#') {
            out.push((
                offset + at,
                line[at..].trim_end_matches(['\r', '\n']).to_string(),
            ));
        }
        offset += line.len();
    }
    out
}

fn isort_skip(comment: &str) -> bool {
    comment
        .replace(' ', "")
        .to_ascii_lowercase()
        .contains("isort:skip")
}

fn line_directive(comment: &str) -> bool {
    comment.contains("noqa")
        || comment.contains("type: ignore")
        || comment.contains("pylint:")
        || comment.contains("pyright:")
        || comment.contains("noinspection")
}

type AliasRecord = (String, Vec<String>, Vec<String>);

fn sort_alias_records(
    records: &mut Vec<AliasRecord>,
    header: &[String],
    mut disabled: [bool; 2],
    case_insensitive: bool,
    allow_sort: bool,
) {
    for note in header {
        apply_sort_directive(note, &mut disabled);
    }
    let mut pending: Vec<AliasRecord> = Vec::new();
    let mut sorted = Vec::new();
    let flush = |pending: &mut Vec<AliasRecord>, sorted: &mut Vec<AliasRecord>, enabled: bool| {
        if enabled && allow_sort {
            pending.sort_by_key(|record| {
                if case_insensitive {
                    record.0.to_lowercase()
                } else {
                    record.0.clone()
                }
            });
            pending.dedup();
        }
        sorted.append(pending);
    };
    for record in std::mem::take(records) {
        if record
            .1
            .iter()
            .chain(&record.2)
            .any(|note| scope_directive(note))
        {
            flush(&mut pending, &mut sorted, !disabled.iter().any(|off| *off));
            for note in record.1.iter().chain(&record.2) {
                apply_sort_directive(note, &mut disabled);
            }
            sorted.push(record);
        } else {
            pending.push(record);
        }
    }
    flush(&mut pending, &mut sorted, !disabled.iter().any(|off| *off));
    *records = sorted;
}

fn render_import(
    text: &str,
    stmt: &ast::Stmt,
    end: usize,
    unused: &std::collections::HashSet<TextRange>,
    disabled: [bool; 2],
    options: &rustpy_settings::python_style::Imports,
) -> RenderedImport {
    let sort_enabled = !disabled.iter().any(|off| *off)
        && options.sort
        && !options.preserve_order
        && !options.remove_unused_only;
    let start = usize::from(stmt.text_range().start());
    let notes = comments(text, start, end);
    let (module, aliases, star) = match stmt {
        ast::Stmt::Import(i) => (
            i.aliases().next().map(|a| a.name()).unwrap_or_default(),
            i.aliases().collect::<Vec<_>>(),
            false,
        ),
        ast::Stmt::ImportFrom(f) => (
            f.module_path(),
            f.aliases().collect::<Vec<_>>(),
            f.is_star(),
        ),
        _ => unreachable!(),
    };
    if star {
        let suffix = notes
            .iter()
            .map(|(_, c)| format!("  {c}"))
            .collect::<String>();
        return RenderedImport {
            lines: vec![format!("from {module} import *{suffix}")],
            module,
            orphans: Vec::new(),
        };
    }
    let kept: Vec<_> = aliases
        .iter()
        .filter(|a| !unused.contains(&binding_range(a)))
        .collect();
    if options.remove_unused_only && kept.len() == aliases.len() {
        return RenderedImport {
            module,
            lines: vec![text[start..end].to_string()],
            orphans: Vec::new(),
        };
    }
    if kept.is_empty() {
        return RenderedImport {
            module,
            lines: Vec::new(),
            orphans: notes.into_iter().map(|(_, c)| c).collect(),
        };
    }
    if matches!(stmt, ast::Stmt::Import(_)) {
        // A line directive covers every binding in a plain multi-import; keep
        // it on each resulting statement when that import is split.
        let suffix = notes
            .iter()
            .map(|(_, c)| format!("  {c}"))
            .collect::<String>();
        return RenderedImport {
            module,
            lines: if options.remove_unused_only {
                vec![format!(
                    "import {}{suffix}",
                    kept.iter()
                        .map(|alias| alias_text(alias))
                        .collect::<Vec<_>>()
                        .join(", ")
                )]
            } else {
                kept.iter()
                    .map(|a| format!("import {}{suffix}", alias_text(a)))
                    .collect()
            },
            orphans: Vec::new(),
        };
    }
    if notes.is_empty() {
        let mut names: Vec<_> = kept.iter().map(|a| alias_text(a)).collect();
        if sort_enabled {
            names.sort_by_key(|name| {
                if options.case_insensitive {
                    name.to_lowercase()
                } else {
                    name.clone()
                }
            });
        }
        if !options.remove_unused_only {
            names.dedup();
        }
        let line = format!("from {module} import {}", names.join(", "));
        let line = if line.len() > 100 || options.parentheses {
            let indent = if options.align {
                " ".repeat(module.len() + 14)
            } else {
                "    ".into()
            };
            format!(
                "from {module} import (\n{indent}{}{}\n)",
                names.join(&format!(",\n{indent}")),
                if options.trailing_comma { "," } else { "" }
            )
        } else {
            line
        };
        return RenderedImport {
            module,
            lines: vec![line],
            orphans: Vec::new(),
        };
    }
    let raw = &text[start..end];
    if !raw.contains('\n') {
        let mut names: Vec<_> = kept.iter().map(|a| alias_text(a)).collect();
        if sort_enabled {
            names.sort_by_key(|name| {
                if options.case_insensitive {
                    name.to_lowercase()
                } else {
                    name.clone()
                }
            });
        }
        if !options.remove_unused_only {
            names.dedup();
        }
        let suffix = notes
            .iter()
            .map(|(_, c)| format!("  {c}"))
            .collect::<String>();
        return RenderedImport {
            lines: vec![format!("from {module} import {}{suffix}", names.join(", "))],
            module,
            orphans: Vec::new(),
        };
    }
    // Preserve multiline alias comments while sorting alias records. Inline
    // notes follow the last alias on that physical line; standalone notes
    // precede the next alias. Notes from deleted bindings become standalone.
    let mut records: Vec<AliasRecord> = aliases
        .iter()
        .map(|a| (alias_text(a), Vec::new(), Vec::new()))
        .collect();
    let mut header = Vec::new();
    let mut closing_before = Vec::new();
    let mut closing_after = Vec::new();
    for (offset, note) in notes {
        let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let same_line = aliases.iter().enumerate().rev().find(|(_, a)| {
            let alias_end = usize::from(a.text_range().end());
            alias_end <= offset && alias_end > line_start
        });
        if let Some((i, _)) = same_line {
            if line_directive(&note) && !scope_directive(&note) {
                for (j, alias) in aliases.iter().enumerate() {
                    let alias_end = usize::from(alias.text_range().end());
                    if alias_end <= offset && alias_end > line_start {
                        records[j].2.push(note.clone());
                    }
                }
            } else {
                records[i].2.push(note);
            }
        } else if text[line_start..offset].trim().starts_with(')') {
            closing_after.push(note);
        } else if text[line_start..offset].contains("import") {
            header.push(note);
        } else if let Some((i, _)) = aliases
            .iter()
            .enumerate()
            .find(|(_, a)| usize::from(a.text_range().start()) > offset)
        {
            records[i].1.push(note);
        } else {
            closing_before.push(note);
        }
    }
    let mut orphan = Vec::new();
    let mut orphan_directives = Vec::new();
    let mut names = Vec::new();
    for (alias, mut record) in aliases.iter().zip(records) {
        if unused.contains(&binding_range(alias)) {
            if record
                .1
                .iter()
                .chain(&record.2)
                .any(|note| scope_directive(note))
            {
                record.0.clear();
                record.1.append(&mut record.2);
                names.push(record);
                continue;
            }
            for note in record.1.into_iter().chain(record.2) {
                if line_directive(&note) {
                    orphan_directives.push(note);
                } else {
                    orphan.push(note);
                }
            }
        } else {
            names.push(record);
        }
    }
    sort_alias_records(
        &mut names,
        &header,
        disabled,
        options.case_insensitive,
        options.sort && !options.preserve_order && !options.remove_unused_only,
    );
    let mut line = format!("from {module} import (");
    for c in header {
        line.push_str(&format!("  {c}"));
    }
    line.push('\n');
    for c in orphan {
        line.push_str(&format!("    {c}\n"));
    }
    for (name, before, after) in names {
        for c in before {
            line.push_str(&format!("    {c}\n"));
        }
        if name.is_empty() {
            continue;
        }
        line.push_str(&format!("    {name},"));
        for c in after {
            line.push_str(&format!("  {c}"));
        }
        line.push('\n');
    }
    for c in closing_before {
        line.push_str(&format!("    {c}\n"));
    }
    line.push(')');
    for c in closing_after {
        line.push_str(&format!("  {c}"));
    }
    RenderedImport {
        module,
        lines: vec![line],
        orphans: orphan_directives,
    }
}

fn import_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .unwrap_or("")
}

fn scope_directive(comment: &str) -> bool {
    let compact = comment.replace(' ', "");
    [
        "isort:off",
        "isort:on",
        "fmt:off",
        "fmt:on",
        "pylint:disable=",
        "pylint:enable=",
    ]
    .iter()
    .any(|directive| compact.contains(directive))
}

fn apply_sort_directive(comment: &str, disabled: &mut [bool; 2]) {
    let compact = comment.replace(' ', "");
    if compact.contains("isort:off") {
        disabled[0] = true;
    }
    if compact.contains("isort:on") {
        disabled[0] = false;
    }
    if compact.contains("fmt:off") {
        disabled[1] = true;
    }
    if compact.contains("fmt:on") {
        disabled[1] = false;
    }
}

/// A header next-statement directive belongs to the first import only when
/// its next significant token is that import (not a preceding docstring).
fn header_next_directives(
    module: &ast::Module,
    text: &str,
    first_start: rustpy_core::TextSize,
) -> (Vec<String>, Vec<Edit>) {
    use rustpy_syntax::SyntaxKind;
    let mut notes = Vec::new();
    let mut edits = Vec::new();
    for token in module
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        if token.text_range().end() > first_start {
            break;
        }
        if token.kind() != SyntaxKind::COMMENT
            || !(token.text().contains("noinspection")
                || token
                    .text()
                    .replace(' ', "")
                    .contains("pylint:disable-next"))
        {
            continue;
        }
        let start = usize::from(token.text_range().start());
        let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        if !text[line_start..start].trim().is_empty() {
            continue;
        }
        let first_byte = usize::from(first_start);
        let first_line = text[..first_byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
        if token.text().contains("noinspection") && start - line_start > first_byte - first_line {
            continue;
        }
        let mut next = token.next_token();
        while next.as_ref().is_some_and(|token| {
            matches!(
                token.kind(),
                SyntaxKind::WHITESPACE
                    | SyntaxKind::COMMENT
                    | SyntaxKind::NL
                    | SyntaxKind::NEWLINE
                    | SyntaxKind::INDENT
                    | SyntaxKind::DEDENT
            )
        }) {
            next = next.and_then(|token| token.next_token());
        }
        if next.is_some_and(|token| token.text_range().start() == first_start) {
            notes.push(token.text().to_string());
            // Leave newlines and preceding header lines in place, including
            // a PEP 263 cookie on the second physical line.
            edits.push((token.text_range(), String::new()));
        }
    }
    (notes, edits)
}

fn render_groups(
    groups: &mut [Vec<(String, String)>; 4],
    unsorted: &mut Vec<String>,
    sort_enabled: bool,
    options: &rustpy_settings::python_style::Imports,
) -> String {
    if !sort_enabled {
        return std::mem::take(unsorted).join("\n");
    }
    let mut rendered = Vec::new();
    for group in groups {
        group.sort_by(|a, b| {
            let order = if options.import_before_from || options.modules_first {
                import_line(&a.1)
                    .starts_with("import ")
                    .cmp(&import_line(&b.1).starts_with("import "))
                    .reverse()
            } else {
                std::cmp::Ordering::Equal
            };
            order
                .then_with(|| {
                    if options.case_insensitive {
                        a.0.to_lowercase().cmp(&b.0.to_lowercase())
                    } else {
                        a.0.cmp(&b.0)
                    }
                })
                .then(a.1.cmp(&b.1))
        });
        group.dedup_by(|a, b| a.1 == b.1);
        if !group.is_empty() {
            rendered.push(
                std::mem::take(group)
                    .into_iter()
                    .map(|(_, line)| line)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    }
    rendered.join(if options.group { "\n\n" } else { "\n" })
}

/// Edits that remove unused imports and sort the leading import block
/// (future, stdlib, third-party, project), each group alphabetically.
pub fn optimize_imports(file: &FileInfo, removable_ranges: &[TextRange]) -> Vec<Edit> {
    optimize_imports_with_style(file, removable_ranges, &Default::default())
}

pub fn optimize_imports_with_style(
    file: &FileInfo,
    removable_ranges: &[TextRange],
    options: &rustpy_settings::python_style::Imports,
) -> Vec<Edit> {
    let root = file.parse.syntax_node();
    let Some(module) = ast::Module::cast(root) else {
        return Vec::new();
    };
    let text: &str = &file.text;
    if module
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| {
            token.kind() == rustpy_syntax::SyntaxKind::COMMENT
                && token
                    .text()
                    .replace(' ', "")
                    .to_ascii_lowercase()
                    .contains("isort:skip_file")
        })
    {
        return Vec::new();
    }
    if !file.parse.errors().is_empty() {
        return Vec::new();
    }
    // Leading contiguous import statements (skipping the docstring).
    let mut block: Vec<ast::Stmt> = Vec::new();
    for (i, stmt) in module.statements().enumerate() {
        match &stmt {
            ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_) => block.push(stmt),
            ast::Stmt::Expr(e) if i == 0 && matches!(e.expr(), Some(ast::Expr::String(_))) => {}
            _ => break,
        }
    }
    if block.is_empty() {
        return Vec::new();
    }
    // Unused names.
    let unused: std::collections::HashSet<TextRange> = removable_ranges.iter().copied().collect();
    let mut groups: [Vec<(String, String)>; 4] = Default::default();
    let mut unsorted = Vec::new();
    let mut segments = Vec::new();
    let mut disabled = [false; 2];
    let first_start = block.first().unwrap().text_range().start();
    // Region directives stay outside the replacement; inspect real comment
    // tokens so a docstring mentioning `isort: off` does not disable sorting.
    for token in module
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        if token.text_range().end() > first_start {
            break;
        }
        if token.kind() == rustpy_syntax::SyntaxKind::COMMENT {
            apply_sort_directive(token.text(), &mut disabled);
        }
    }
    let mut sort_enabled = !disabled.iter().any(|off| *off)
        && options.sort
        && !options.preserve_order
        && !options.remove_unused_only;
    let (header_notes, mut header_edits) = header_next_directives(&module, text, first_start);
    let mut first_survives = false;
    let project_roots: Vec<String> = file
        .module_name
        .iter()
        .map(|m| m.split('.').next().unwrap_or("").to_string())
        .collect();
    let mut previous_end = usize::from(block.first().unwrap().text_range().start());
    let mut orphan_comments = Vec::new();
    for stmt in &block {
        let stmt_start = usize::from(stmt.text_range().start());
        let mut prefix: Vec<_> = if stmt.text_range().start() == first_start {
            header_notes.clone()
        } else {
            comments(text, previous_end, stmt_start)
                .into_iter()
                .map(|(_, c)| c)
                .collect()
        };
        if prefix.iter().any(|comment| scope_directive(comment)) {
            let segment = render_groups(&mut groups, &mut unsorted, sort_enabled, options);
            if !segment.is_empty() {
                segments.push(segment);
            }
            for comment in &prefix {
                apply_sort_directive(comment, &mut disabled);
            }
            sort_enabled = !disabled.iter().any(|off| *off)
                && options.sort
                && !options.preserve_order
                && !options.remove_unused_only;
            segments.push(prefix.join("\n"));
            prefix.clear();
        }
        let end = statement_end(text, stmt);
        previous_end = end;
        let inline_controls: Vec<_> = comments(text, stmt_start, end)
            .into_iter()
            .map(|(_, note)| note)
            .filter(|note| scope_directive(note))
            .collect();
        if !inline_controls.is_empty() {
            let segment = render_groups(&mut groups, &mut unsorted, sort_enabled, options);
            if !segment.is_empty() {
                segments.push(segment);
            }
        }
        if comments(text, stmt_start, end)
            .iter()
            .any(|(_, note)| isort_skip(note))
        {
            let segment = render_groups(&mut groups, &mut unsorted, sort_enabled, options);
            if !segment.is_empty() {
                segments.push(segment);
            }
            if !prefix.is_empty() {
                segments.push(prefix.join("\n"));
            }
            segments.push(text[stmt_start..end].to_string());
            if stmt.text_range().start() == first_start {
                first_survives = true;
            }
            continue;
        }
        let rendered = render_import(text, stmt, end, &unused, disabled, options);
        let RenderedImport {
            module,
            mut lines,
            mut orphans,
        } = rendered;
        if stmt.text_range().start() == first_start {
            first_survives = !lines.is_empty();
        }
        if !inline_controls.is_empty() {
            orphans.retain(|note| !scope_directive(note));
        }
        if lines.is_empty() {
            orphan_comments.extend(prefix);
            orphan_comments.extend(orphans);
            if !inline_controls.is_empty() {
                segments.push(inline_controls.join("\n"));
                for note in &inline_controls {
                    apply_sort_directive(note, &mut disabled);
                }
                sort_enabled = !disabled.iter().any(|off| *off)
                    && options.sort
                    && !options.preserve_order
                    && !options.remove_unused_only;
            }
            continue;
        }
        orphan_comments.extend(orphans);
        if !prefix.is_empty() {
            let prefix = prefix.join("\n");
            if prefix.contains("disable-next") || prefix.contains("noinspection") {
                for line in &mut lines {
                    *line = format!("{prefix}\n{line}");
                }
            } else {
                lines[0] = format!("{prefix}\n{}", lines[0]);
            }
        }
        if !inline_controls.is_empty() {
            for note in &inline_controls {
                if !lines.iter().any(|line| line.contains(note)) {
                    lines.push(note.clone());
                }
                apply_sort_directive(note, &mut disabled);
            }
            segments.push(lines.join("\n"));
            sort_enabled = !disabled.iter().any(|off| *off)
                && options.sort
                && !options.preserve_order
                && !options.remove_unused_only;
            continue;
        }
        for l in lines {
            // A plain multi-import is split into separate statements. Classify each
            // resulting module independently rather than using its first sibling.
            let line_module = import_line(&l)
                .strip_prefix("import ")
                .and_then(|body| body.split_whitespace().next())
                .unwrap_or(&module);
            let top = line_module
                .trim_start_matches('.')
                .split('.')
                .next()
                .unwrap_or("")
                .to_string();
            if !sort_enabled {
                unsorted.push(l);
                continue;
            }
            let group = if line_module == "__future__" {
                &mut groups[0]
            } else if !options.group {
                &mut groups[1]
            } else if line_module.starts_with('.')
                || project_roots.iter().any(|r| r == &top)
                || rustpy_semantic::typeshed::stub_for_module(&top).is_none()
                    && file
                        .path
                        .parent()
                        .map(|d| d.join(format!("{top}.py")).exists() || d.join(&top).is_dir())
                        .unwrap_or(false)
            {
                &mut groups[3]
            } else if is_stdlib(line_module) {
                &mut groups[1]
            } else {
                &mut groups[2]
            };
            group.push((line_module.to_string(), l));
        }
    }
    let segment = render_groups(&mut groups, &mut unsorted, sort_enabled, options);
    if !segment.is_empty() {
        segments.push(segment);
    }
    let mut new_block = segments.join("\n");
    let control_end = statement_end(text, block.last().unwrap());
    let controlled = module
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| {
            token.kind() == rustpy_syntax::SyntaxKind::COMMENT
                && usize::from(token.text_range().start()) <= control_end
                && (scope_directive(token.text()) || isort_skip(token.text()))
        });
    if options.join_from && !options.remove_unused_only && !controlled {
        new_block = join_from_lines(&new_block, options);
    }
    if !options.remove_unused_only && !controlled {
        new_block = preferred_import_lines(&new_block, file, options);
    }
    if first_survives && !header_notes.is_empty() {
        let prefix = format!("{}\n", header_notes.join("\n"));
        if let Some(rest) = new_block.strip_prefix(&prefix) {
            // The original first import still leads the block: its header
            // annotation can stay at its exact original position.
            new_block = rest.to_string();
            header_edits.clear();
        }
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    if newline != "\n" {
        new_block = new_block.replace('\n', newline);
    }
    let start: usize = block.first().unwrap().text_range().start().into();
    let end = statement_end(text, block.last().unwrap());
    // Keep the original trailing newline handling: replace statement range only.
    let old = &text[start..end];
    let old_trimmed = old.trim_end();
    let end = start + old_trimmed.len();
    let mut edits = header_edits;
    if old_trimmed != new_block {
        edits.push((TextRange::new(ts(start), ts(end)), new_block));
    }
    if !orphan_comments.is_empty() {
        // A comment whose binding was removed has no surviving attachment.
        // Keep it after the code: standalone next-statement/block directives
        // must not start suppressing a different import or the first body line.
        let separator = if text.ends_with('\n') { "\n" } else { "\n\n" };
        let footer = format!("{separator}{}\n", orphan_comments.join("\n")).replace('\n', newline);
        edits.push((TextRange::empty(ts(text.len())), footer));
    }
    edits
}

fn join_from_lines(source: &str, options: &rustpy_settings::python_style::Imports) -> String {
    let parse = rustpy_syntax::parse(source);
    if !parse.errors().is_empty() {
        return source.into();
    }
    let Some(module) = ast::Module::cast(parse.syntax_node()) else {
        return source.into();
    };
    // Join only adjacent, comment-free statements. A comment elsewhere in the
    // block must neither disappear nor prevent unrelated modules being joined.
    let mut groups = Vec::<(String, Vec<String>, usize, usize, usize)>::new();
    let mut adjacent = false;
    for statement in module.statements() {
        let start = usize::from(statement.text_range().start());
        let end = statement_end(source, &statement);
        if let ast::Stmt::ImportFrom(import) = &statement {
            if !import.is_star() && comments(source, start, end).is_empty() {
                let module = import.module_path();
                let names = import
                    .aliases()
                    .map(|alias| alias_text(&alias))
                    .collect::<Vec<_>>();
                if adjacent {
                    if let Some((previous, existing, _, previous_end, count)) = groups.last_mut() {
                        let gap = &source[*previous_end..start];
                        if *previous == module && gap.trim().is_empty() && !gap.contains("\n\n") {
                            existing.extend(names);
                            *previous_end = end;
                            *count += 1;
                            continue;
                        }
                    }
                }
                groups.push((module, names, start, end, 1));
                adjacent = true;
                continue;
            }
        }
        adjacent = false;
    }
    let mut output = source.to_string();
    for (module, mut names, start, end, count) in groups.into_iter().rev() {
        if count < 2 {
            continue;
        }
        if options.sort && !options.preserve_order {
            names.sort_by_key(|name| {
                if options.case_insensitive {
                    name.to_lowercase()
                } else {
                    name.clone()
                }
            });
        }
        let mut seen = std::collections::HashSet::new();
        names.retain(|name| seen.insert(name.clone()));
        output.replace_range(
            start..end,
            &format!("from {module} import {}", names.join(", ")),
        );
    }
    output
}

fn preferred_import_lines(
    source: &str,
    file: &FileInfo,
    options: &rustpy_settings::python_style::Imports,
) -> String {
    let package = file
        .module_name
        .as_deref()
        .and_then(|module| module.rsplit_once('.').map(|(package, _)| package));
    source
        .lines()
        .map(|line| {
            if line.contains('#') {
                return line.to_string();
            }
            let mut line = line.to_string();
            if options.force_from {
                if let Some(body) = line.strip_prefix("import ") {
                    if let Some((module, alias)) = body.split_once(" as ") {
                        if let Some((parent, name)) = module.rsplit_once('.') {
                            line = format!("from {parent} import {name} as {alias}")
                        }
                    }
                }
            }
            if let Some((module, names)) = line
                .strip_prefix("from ")
                .and_then(|line| line.split_once(" import "))
            {
                if let Some(package) = package {
                    if options.prefer_relative || options.relative_style == "relative" {
                        if module == package {
                            line = format!("from . import {names}")
                        } else if let Some(rest) = module.strip_prefix(&format!("{package}.")) {
                            line = format!("from .{rest} import {names}")
                        }
                    } else if options.relative_style == "absolute" && module.starts_with('.') {
                        let level = module.chars().take_while(|c| *c == '.').count();
                        let parts = package.split('.').collect::<Vec<_>>();
                        if level <= parts.len() {
                            let prefix = parts[..parts.len() - level + 1].join(".");
                            let suffix = module.trim_start_matches('.');
                            line = format!(
                                "from {prefix}{} import {names}",
                                if suffix.is_empty() {
                                    String::new()
                                } else {
                                    format!(".{suffix}")
                                }
                            )
                        }
                    }
                }
            }
            if options.parentheses {
                if let Some((module, names)) = line
                    .strip_prefix("from ")
                    .and_then(|line| line.split_once(" import "))
                {
                    if !names.contains('*') && !names.contains('(') {
                        let indent = if options.align {
                            " ".repeat(module.len() + 14)
                        } else {
                            "    ".into()
                        };
                        let names = names.split(',').map(str::trim).collect::<Vec<_>>();
                        return format!(
                            "from {module} import (\n{indent}{}{}\n)",
                            names.join(&format!(",\n{indent}")),
                            if options.trailing_comma { "," } else { "" }
                        );
                    }
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_semantic::edit_helpers::apply_edits;
    use rustpy_semantic::{imports::unused_import_candidates, Analysis};
    use std::path::Path;

    #[test]
    fn explicit_isort_skip_is_an_immutable_barrier_even_with_joining() {
        let src = "from pkg import Zed, Alpha  # isort: skip\nfrom pkg import Beta\nimport os\nif ready:\n    import optional\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/main.py"), src);
        let file = db.file(id).unwrap();
        let options = rustpy_settings::python_style::Imports {
            join_from: true,
            ..Default::default()
        };
        let ranges = unused_import_candidates(&file)
            .iter()
            .map(|candidate| candidate.range)
            .collect::<Vec<_>>();
        let output = apply_edits(src, &optimize_imports_with_style(&file, &ranges, &options));
        assert!(output.contains("from pkg import Zed, Alpha  # isort: skip"));
        assert!(output.contains("if ready:\n    import optional\n"));
        let skipped = "# isort: skip_file\nimport sys\nimport os\n";
        let id = db.set_file_text(Path::new("/proj/main.py"), skipped);
        let file = db.file(id).unwrap();
        assert!(optimize_imports(&file, &[]).is_empty());
    }

    #[test]
    fn split_plain_imports_group_each_module_and_join_respects_future() {
        let src = "from __future__ import annotations\nimport requests, sys, os\nfrom math import sqrt\nfrom math import ceil\nprint(requests, sys, os, sqrt, ceil)\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/main.py"), src);
        let file = db.file(id).unwrap();
        let options = rustpy_settings::python_style::Imports {
            join_from: true,
            ..Default::default()
        };
        let output = apply_edits(src, &optimize_imports_with_style(&file, &[], &options));
        assert_eq!(output, "from __future__ import annotations\n\nimport os\nimport sys\nfrom math import ceil, sqrt\n\nimport requests\nprint(requests, sys, os, sqrt, ceil)\n");
        let id = db.set_file_text(Path::new("/proj/main.py"), &output);
        assert!(optimize_imports_with_style(&db.file(id).unwrap(), &[], &options).is_empty());
    }

    #[test]
    fn join_keeps_annotated_statements_and_comments_elsewhere() {
        let options = rustpy_settings::python_style::Imports::default();
        let source = "from math import ceil\nfrom math import sqrt\nimport sys  # preserve this explanation\nfrom pkg import Apple  # noqa: F401\nfrom pkg import Zebra";
        assert_eq!(join_from_lines(source, &options), "from math import ceil, sqrt\nimport sys  # preserve this explanation\nfrom pkg import Apple  # noqa: F401\nfrom pkg import Zebra");
    }

    #[test]
    fn style_preserves_commented_alias_order_when_sorting_disabled() {
        let src = "from pkg import (\n    Zebra,  # last alphabetically\n    Apple,  # first alphabetically\n)\nprint(Zebra, Apple)\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/main.py"), src);
        let file = db.file(id).unwrap();
        let options = rustpy_settings::python_style::Imports {
            sort: false,
            ..Default::default()
        };
        assert!(optimize_imports_with_style(&file, &[], &options).is_empty());
    }

    #[test]
    fn optimizer_groups_imports_and_preserves_export_aliases() {
        let src = "from __future__ import annotations\nimport requests as requests\nfrom typing import List as List\nimport os as os\nimport main as main\nx = 1\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/main.py"), src);
        let file = db.file(id).unwrap();
        assert!(unused_import_candidates(&file).is_empty());
        let edits = optimize_imports(&file, &[]);
        assert_eq!(apply_edits(src, &edits), "from __future__ import annotations\n\nimport os as os\nfrom typing import List as List\n\nimport requests as requests\n\nimport main as main\nx = 1\n");
    }

    #[test]
    fn optimizer_removes_only_caller_selected_bindings() {
        let src = "import os  # noqa: F401\nimport json\nx = 1\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let candidates = unused_import_candidates(&file);
        let json = candidates.iter().find(|b| b.name == "json").unwrap();
        let edits = optimize_imports(&file, &[json.range]);
        assert_eq!(apply_edits(src, &edits), "import os  # noqa: F401\nx = 1\n");
        assert_eq!(
            apply_edits(src, &optimize_imports(&file, &[])),
            "import json\nimport os  # noqa: F401\nx = 1\n"
        );
    }

    fn optimize(src: &str, removed: &[&str]) -> String {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let ranges: Vec<_> = unused_import_candidates(&file)
            .into_iter()
            .filter(|b| removed.contains(&b.name.as_str()))
            .map(|b| b.range)
            .collect();
        apply_edits(src, &optimize_imports(&file, &ranges))
    }

    #[test]
    fn optimizer_preserves_split_binding_directives_and_annotated_stars() {
        let src = "import sys, os  # noqa: F401  μ\nfrom math import *  # type: ignore[import-not-found]\nprint(os)\n";
        let expected = "import os  # noqa: F401  μ\nimport sys  # noqa: F401  μ\nfrom math import *  # type: ignore[import-not-found]\nprint(os)\n";
        assert_eq!(optimize(src, &[]), expected);
        assert_eq!(optimize(expected, &[]), expected);
        assert_eq!(
            optimize(src, &["sys"]),
            expected.replace("import sys  # noqa: F401  μ\n", "")
        );
    }

    #[test]
    fn optimizer_preserves_multiline_alias_header_and_closing_comments() {
        let src = "from pkg import (  # noqa: F401\n    Zebra,  # zebra μ\n    # apple explanation\n    Apple,  # type: ignore[attr-defined]\n    Gone,  # removed explanation\n    # end explanation\n)  # statement explanation\nprint(Apple, Zebra)\n";
        let expected = "from pkg import (  # noqa: F401\n    # removed explanation\n    # apple explanation\n    Apple,  # type: ignore[attr-defined]\n    Zebra,  # zebra μ\n    # end explanation\n)  # statement explanation\nprint(Apple, Zebra)\n";
        assert_eq!(optimize(src, &["Gone"]), expected);
        assert_eq!(optimize(expected, &[]), expected);
    }

    #[test]
    fn optimizer_keeps_line_suppressions_on_each_multiline_binding() {
        let src =
            "from pkg import (\n    Zebra, Apple, Gone,  # noqa: F401\n)\nprint(Zebra, Apple)\n";
        let expected = "from pkg import (\n    Apple,  # noqa: F401\n    Zebra,  # noqa: F401\n)\nprint(Zebra, Apple)\n\n# noqa: F401\n";
        assert_eq!(optimize(src, &["Gone"]), expected);
        assert_eq!(optimize(expected, &[]), expected);
        let src = "import json\n# pylint: disable-next=unused-import\nimport sys, os\nx = 1\n";
        let expected = "import json\n# pylint: disable-next=unused-import\nimport os\n# pylint: disable-next=unused-import\nimport sys\nx = 1\n";
        assert_eq!(optimize(src, &[]), expected);
        assert_eq!(optimize(expected, &[]), expected);
    }

    #[test]
    fn optimizer_preserves_scope_directive_boundaries_without_disabling_other_runs() {
        let src = "import sys\nimport os\n# isort: off\nimport zeta\nimport alpha\nimport unused\n# isort: on\nimport urllib\nimport json\n# pylint: disable=unused-import\nimport math\n# pylint: enable=unused-import\nimport decimal\nx = 1\n";
        let expected = "import os\nimport sys\n# isort: off\nimport zeta\nimport alpha\n# isort: on\nimport json\nimport urllib\n# pylint: disable=unused-import\nimport math\n# pylint: enable=unused-import\nimport decimal\nx = 1\n";
        assert_eq!(optimize(src, &["unused"]), expected);
        assert_eq!(optimize(expected, &[]), expected);
        let src = "# isort: off\nimport sys\nimport os\n# fmt: off\nimport urllib\n# isort: on\nimport json\n# fmt: on\nimport math\nimport decimal\nx = 1\n";
        let expected = "# isort: off\nimport sys\nimport os\n# fmt: off\nimport urllib\n# isort: on\nimport json\n# fmt: on\nimport decimal\nimport math\nx = 1\n";
        assert_eq!(optimize(src, &[]), expected);
        assert_eq!(optimize(expected, &[]), expected);
        let src = "\"\"\"# isort: off\"\"\"\nimport sys\nimport os\nx = 1\n";
        assert_eq!(
            optimize(src, &[]),
            src.replace("import sys\nimport os", "import os\nimport sys")
        );
    }

    #[test]
    fn optimizer_retains_standalone_and_deleted_line_comments_with_crlf() {
        let src = "#!/usr/bin/python\r\n# coding: utf-8\r\n\"\"\"μ doc\"\"\"\r\nimport requests  # deleted μ\r\n# os explanation\r\nimport os  # noqa: F401\r\nimport json\r\nprint(os, json)\r\n";
        let expected = "#!/usr/bin/python\r\n# coding: utf-8\r\n\"\"\"μ doc\"\"\"\r\nimport json\r\n# os explanation\r\nimport os  # noqa: F401\r\nprint(os, json)\r\n\r\n# deleted μ\r\n";
        assert_eq!(optimize(src, &["requests"]), expected);
        assert_eq!(optimize(expected, &[]), expected);
    }

    #[test]
    fn deleted_statement_directives_do_not_target_the_next_statement() {
        let src = "import os\n# noinspection PyUnusedImports\nimport json  # pylint: disable=unused-import\nprint(os)\n";
        let expected = "import os\n# pylint: disable=unused-import\nprint(os)\n\n# noinspection PyUnusedImports\n";
        assert_eq!(optimize(src, &["json"]), expected);
        assert_eq!(optimize(expected, &[]), expected);
    }

    #[test]
    fn header_next_statement_directives_follow_their_import_only() {
        for directive in [
            "# noinspection PyUnusedImports",
            "# pylint: disable-next=unused-import",
        ] {
            let src = format!("{directive}\nimport zeta\nimport alpha\nprint(alpha, zeta)\n");
            let expected =
                format!("\nimport alpha\n{directive}\nimport zeta\nprint(alpha, zeta)\n");
            assert_eq!(optimize(&src, &[]), expected);
            assert_eq!(optimize(&expected, &[]), expected);
            let src = format!("{directive}\nimport json\nprint('body')\n");
            let expected = format!("\n\nprint('body')\n\n{directive}\n");
            assert_eq!(optimize(&src, &["json"]), expected);
            assert_eq!(optimize(&expected, &[]), expected);
        }
        let src = "    # noinspection PyUnusedImports\nimport zeta\nimport alpha\nx = 1\n";
        let expected = src.replace("import zeta\nimport alpha", "import alpha\nimport zeta");
        assert_eq!(
            optimize(src, &[]),
            expected,
            "a deeper header comment did not originally suppress an import"
        );
        assert_eq!(optimize(&expected, &[]), expected);
        let src = "# noinspection PyUnusedImports\n# coding: utf-8\nimport zeta\nimport alpha\nprint('μ')\n";
        let expected = "\n# coding: utf-8\nimport alpha\n# noinspection PyUnusedImports\nimport zeta\nprint('μ')\n";
        assert_eq!(optimize(src, &[]), expected);
        assert_eq!(optimize(expected, &[]), expected);
        let src = "#!/usr/bin/python\n# coding: utf-8\n# noinspection PyUnusedImports\n\"\"\"μ doc\"\"\"\nfrom __future__ import annotations\nimport sys\nimport os\nx = 1\n";
        let expected = src.replace(
            "from __future__ import annotations\nimport sys\nimport os",
            "from __future__ import annotations\n\nimport os\nimport sys",
        );
        assert_eq!(
            optimize(src, &[]),
            expected,
            "the header annotation belongs to the docstring"
        );
        let src = "# noinspection PyUnusedImports\nimport sys, os\nx = 1\n";
        let expected = "# noinspection PyUnusedImports\nimport os\n# noinspection PyUnusedImports\nimport sys\nx = 1\n";
        // Each split statement keeps the annotation, even when the first one
        // remains first and its original header line can stay outside the edit.
        assert_eq!(optimize(src, &[]), expected);
        assert_eq!(optimize(expected, &[]), expected);
    }

    #[test]
    fn inline_block_controls_bound_sorting_and_survive_binding_removal() {
        let src = "import sys\nimport os\nimport zeta  # pylint: disable=unused-import\nimport beta\nimport alpha\n# pylint: enable=unused-import\nimport urllib\nimport json\nx = 1\n";
        let expected = "import os\nimport sys\nimport zeta  # pylint: disable=unused-import\nimport alpha\nimport beta\n# pylint: enable=unused-import\nimport json\nimport urllib\nx = 1\n";
        assert_eq!(optimize(src, &[]), expected);
        assert_eq!(optimize(expected, &[]), expected);
        let expected_removed = expected.replace(
            "import zeta  # pylint: disable=unused-import",
            "# pylint: disable=unused-import",
        );
        assert_eq!(optimize(src, &["zeta"]), expected_removed);
        assert_eq!(optimize(&expected_removed, &[]), expected_removed);
    }

    #[test]
    fn multiline_alias_block_controls_keep_their_scope_boundaries() {
        let src = "from pkg import (\n    Zeta,  # pylint: disable=unused-import\n    Gamma,\n    Beta,\n    Alpha,  # pylint: enable=unused-import\n    Delta,\n)\nx = 1\n";
        let expected = "from pkg import (\n    Zeta,  # pylint: disable=unused-import\n    Beta,\n    Gamma,\n    Alpha,  # pylint: enable=unused-import\n    Delta,\n)\nx = 1\n";
        assert_eq!(optimize(src, &[]), expected);
        assert_eq!(optimize(expected, &[]), expected);
        let removed = expected.replace(
            "    Zeta,  # pylint: disable=unused-import",
            "    # pylint: disable=unused-import",
        );
        assert_eq!(optimize(src, &["Zeta"]), removed);
        assert_eq!(optimize(&removed, &[]), removed);
    }

    #[test]
    fn outer_sort_regions_preserve_from_alias_order_and_still_remove_unused() {
        for directive in ["# isort: off", "# fmt: off"] {
            let src = format!("{directive}\nfrom pkg import Zeta, Gone, Alpha\nx = 1\n");
            assert_eq!(optimize(&src, &[]), src);
            let expected = src.replace("Zeta, Gone, Alpha", "Zeta, Alpha");
            assert_eq!(optimize(&src, &["Gone"]), expected);
            assert_eq!(optimize(&expected, &[]), expected);
            let src = format!("{directive}\nfrom pkg import (\n    Zeta,  # zeta explanation\n    Gone,\n    Alpha,  # alpha explanation\n)\nx = 1\n");
            assert_eq!(optimize(&src, &[]), src);
            let expected = src.replace("    Gone,\n", "");
            assert_eq!(optimize(&src, &["Gone"]), expected);
            assert_eq!(optimize(&expected, &[]), expected);
        }
        let src = "# fmt: off\nfrom pkg import (  # isort: on\n    Zeta,  # zeta explanation\n    Alpha,  # alpha explanation\n)\n# fmt: on\nfrom other import Zeta, Alpha\nx = 1\n";
        let expected = src.replace(
            "from other import Zeta, Alpha",
            "from other import Alpha, Zeta",
        );
        assert_eq!(
            optimize(src, &[]),
            expected,
            "isort on does not cancel an outer fmt off"
        );
        assert_eq!(optimize(&expected, &[]), expected);
    }

    #[test]
    fn optimizer_deduplicates_multiline_imports_and_keeps_stars() {
        let src = "\"\"\"doc\"\"\"\nfrom typing import (\n    List as List, Dict as Dict, List as List,\n)\nfrom os import *\nimport sys as sys\nimport sys as sys\nx = 1\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        assert_eq!(apply_edits(src, &optimize_imports(&file, &[])), "\"\"\"doc\"\"\"\nimport sys as sys\nfrom os import *\nfrom typing import Dict as Dict, List as List\nx = 1\n");
    }

    #[test]
    fn optimizer_ranges_preserve_unicode_docstrings_and_trailing_text() {
        let src = "\"\"\"μ\"\"\"\nimport π\nfrom decimal import Decimal as δ\nδ('1')\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let removable: Vec<_> = unused_import_candidates(&file)
            .iter()
            .map(|b| b.range)
            .collect();
        let edits = optimize_imports(&file, &removable);
        assert_eq!(
            apply_edits(src, &edits),
            "\"\"\"μ\"\"\"\nfrom decimal import Decimal as δ\nδ('1')\n"
        );
    }
}
