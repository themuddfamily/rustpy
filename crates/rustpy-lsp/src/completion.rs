//! Code completion: the IDE's lookup (`features::complete`, filtered and
//! ranked like the editor's popup), names from other modules with their
//! import (the editor's second Ctrl+Space), live and postfix templates as
//! snippets, and documentation on `completionItem/resolve` (the Quick
//! Documentation text of the completed name).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionItemLabelDetails, CompletionList,
    CompletionParams, CompletionResponse, CompletionTextEdit, Documentation, InsertReplaceEdit,
    InsertTextFormat, InsertTextMode, MarkupContent, MarkupKind, TextEdit,
};
use rustpy_core::{TextRange, TextSize};
use rustpy_semantic::features::{self, CompletionKind};
use serde::{Deserialize, Serialize};

use crate::snapshot::{DocView, HResult, LspError, Snapshot};

/// Below this prefix length names from other modules are not offered yet;
/// the list is marked incomplete so the client asks again as the user types.
const AUTO_IMPORT_MIN_PREFIX: usize = 3;
const MAX_ITEMS: usize = 400;

/// What `completionItem/resolve` needs to find the item's documentation.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct ResolveData {
    uri: lsp_types::Uri,
    version: Option<i32>,
    text_hash: u64,
    /// Byte offsets of the typed prefix in the document.
    start: u32,
    end: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    import: Option<(String, String)>,
}

fn kind(k: CompletionKind) -> CompletionItemKind {
    match k {
        CompletionKind::Keyword => CompletionItemKind::KEYWORD,
        CompletionKind::Variable | CompletionKind::Parameter => CompletionItemKind::VARIABLE,
        CompletionKind::Function => CompletionItemKind::FUNCTION,
        CompletionKind::Method => CompletionItemKind::METHOD,
        CompletionKind::Class => CompletionItemKind::CLASS,
        CompletionKind::Module => CompletionItemKind::MODULE,
        CompletionKind::Attribute => CompletionItemKind::FIELD,
        CompletionKind::Property => CompletionItemKind::PROPERTY,
        CompletionKind::Constant => CompletionItemKind::CONSTANT,
        CompletionKind::Template | CompletionKind::Postfix => CompletionItemKind::SNIPPET,
    }
}

/// Escapes text for an LSP snippet.
pub fn snippet_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '$' | '}') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Start of the identifier being typed before `offset`.
fn prefix_start(text: &str, offset: usize) -> usize {
    let before = &text[..offset];
    before
        .rfind(|c: char| !rustpy_core::is_ident_continue(c))
        .map(|i| {
            i + before[i..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1)
        })
        .unwrap_or(0)
}

pub fn completion(
    snap: &Snapshot,
    params: CompletionParams,
) -> HResult<Option<CompletionResponse>> {
    let pos = params.text_document_position;
    if let Some(path) = snap
        .path_of(&pos.text_document.uri)
        .filter(|path| path.file_name().is_some_and(|name| name == ".editorconfig"))
    {
        snap.check_cancelled()?;
        let Some(doc) = snap.docs.get(&path) else {
            return Ok(None);
        };
        let text = doc.text();
        let offset = usize::from(doc.index.offset(pos.position, snap.enc));
        let result =
            rustpy_settings::editorconfig::document::complete(text, offset).map(|completion| {
                let range = doc.index.range(
                    TextRange::new(
                        TextSize::new(completion.range.start as u32),
                        TextSize::new(completion.range.end as u32),
                    ),
                    snap.enc,
                );
                CompletionResponse::Array(
                    completion
                        .items
                        .into_iter()
                        .map(|item| CompletionItem {
                            label: item.label,
                            kind: Some(CompletionItemKind::PROPERTY),
                            detail: Some(item.detail),
                            text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                                range,
                                item.insert,
                            ))),
                            ..Default::default()
                        })
                        .collect(),
                )
            });
        return Ok(result);
    }
    let doc = snap.doc(&pos.text_document.uri)?;
    let offset = usize::from(doc.offset(pos.position, snap.enc));
    let text = doc.text();
    let context = rustpy_semantic::basic_completion::complete(
        &snap.analysis,
        &doc.file,
        TextSize::new(offset as u32),
    );
    let start = context.as_ref().map_or_else(
        || prefix_start(text, offset),
        |context| usize::from(context.prefix_start),
    );
    let segment = context
        .as_ref()
        .is_some_and(|context| context.replace_segment);
    let class_imports = context
        .as_ref()
        .is_some_and(|context| context.import_classes);
    let supplied_imports: std::collections::HashMap<_, _> = context
        .as_ref()
        .map(|context| {
            context
                .items
                .iter()
                .zip(&context.imports)
                .map(|(item, import)| ((item.label.to_string(), item.kind), import.clone()))
                .collect()
        })
        .unwrap_or_default();
    let prefix = &text[start..offset];
    let replace = doc.index.range(
        TextRange::new(TextSize::new(start as u32), TextSize::new(offset as u32)),
        snap.enc,
    );
    // Enter inserts before the rest of the name, Tab replaces it (as in the editor).
    let ident_end = rustpy_semantic::basic_completion::replacement_end(text, offset, segment);
    let replace_whole = doc.index.range(
        TextRange::new(TextSize::new(start as u32), TextSize::new(ident_end as u32)),
        snap.enc,
    );
    let text_edit = |new_text: String| {
        if snap.caps.insert_replace && ident_end > offset {
            CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
                new_text,
                insert: replace,
                replace: replace_whole,
            })
        } else {
            CompletionTextEdit::Edit(TextEdit::new(replace, new_text))
        }
    };
    let caret = TextSize::new(offset as u32);
    let in_code = features::completion_context(text, caret).is_code();
    let mut items = context.map_or_else(
        || features::complete(&snap.analysis, &doc.file, caret),
        |context| context.items,
    );
    snap.check_cancelled()?;
    let cs = snap.config.completion_settings();
    if !prefix.is_empty() {
        items = rustpy_semantic::completion2::filter_items(items, prefix, &cs);
    }
    let next_char = text[offset..].chars().next();
    let insert_parens = snap.config.settings.completion.insert_parentheses;
    let text_hash = hash(text);
    let data = |import: Option<(String, String)>| {
        serde_json::to_value(ResolveData {
            uri: doc.uri.clone(),
            version: doc.version,
            text_hash,
            start: start as u32,
            end: offset as u32,
            import,
        })
        .ok()
    };
    let mut out: Vec<CompletionItem> = Vec::new();
    for item in &items {
        let import = supplied_imports
            .get(&(item.label.to_string(), item.kind))
            .cloned()
            .flatten();
        if import.is_some() && !snap.config.auto_import_completions {
            continue;
        }
        let additional_text_edits = import.as_ref().and_then(|(module, name)| {
            let (range, text) =
                rustpy_semantic::completion2::import_edit_for(&doc.file, module, name);
            (!text.is_empty()).then(|| vec![TextEdit::new(doc.index.range(range, snap.enc), text)])
        });
        let (insert, caret_off) = features::insert_text_for(item, insert_parens, next_char);
        let (new_text, format) = insert_text(snap, &item.label, &insert, caret_off);
        out.push(CompletionItem {
            label: item.label.to_string(),
            kind: Some(kind(item.kind)),
            detail: (!item.detail.is_empty()).then(|| item.detail.clone()),
            filter_text: Some(item.label.to_string()),
            text_edit: Some(text_edit(new_text)),
            insert_text_format: Some(format),
            additional_text_edits,
            data: data(import),
            ..Default::default()
        });
    }
    // Live and postfix templates (the editor offers them in code with a typed prefix).
    if in_code && !prefix.is_empty() && snap.config.live_templates && snap.caps.snippets {
        out.extend(template_items(snap, &doc, start, offset, prefix));
    }
    let mut incomplete = false;
    // Names from other modules, inserted with their import.
    if in_code
        && snap.config.auto_import_completions
        && !prefix.is_empty()
        && !after_dot(text, start)
        && !in_import_statement(text, start)
    {
        if prefix.chars().count() < AUTO_IMPORT_MIN_PREFIX {
            incomplete = true;
        } else {
            snap.check_cancelled()?;
            let imports = import_items(snap, &doc, caret, prefix, class_imports);
            incomplete |= imports.len() >= 200;
            for (item, module, name) in imports {
                if out.iter().any(|o| o.label == item.label.as_str()) {
                    continue;
                }
                let (insert, caret_off) =
                    features::insert_text_for(&item, insert_parens, next_char);
                let (new_text, format) = insert_text(snap, &item.label, &insert, caret_off);
                // An empty name imports the module itself (`import json`).
                let (irange, itext) =
                    rustpy_semantic::completion2::import_edit_for(&doc.file, &module, &name);
                let extra = (!itext.is_empty())
                    .then(|| vec![TextEdit::new(doc.index.range(irange, snap.enc), itext)]);
                let description = if name.is_empty() {
                    "import".to_string()
                } else {
                    module.clone()
                };
                out.push(CompletionItem {
                    label: item.label.to_string(),
                    label_details: Some(CompletionItemLabelDetails {
                        detail: None,
                        description: Some(description),
                    }),
                    kind: Some(kind(item.kind)),
                    detail: Some(if name.is_empty() {
                        format!("import {module}")
                    } else {
                        format!("from {module} import {name}")
                    }),
                    filter_text: Some(item.label.to_string()),
                    text_edit: Some(text_edit(new_text)),
                    insert_text_format: Some(format),
                    additional_text_edits: extra,
                    data: data(Some((module, name))),
                    ..Default::default()
                });
            }
        }
    }
    if out.len() > MAX_ITEMS {
        out.truncate(MAX_ITEMS);
        incomplete = true;
    }
    for (i, item) in out.iter_mut().enumerate() {
        item.sort_text = Some(format!("{i:05}"));
    }
    Ok(Some(CompletionResponse::List(CompletionList {
        is_incomplete: incomplete,
        items: out,
    })))
}

/// The text to insert: with snippet support the caret lands inside `()`.
fn insert_text(
    snap: &Snapshot,
    label: &str,
    insert: &str,
    caret: Option<usize>,
) -> (String, InsertTextFormat) {
    match caret {
        Some(c) if c < insert.len() && insert.is_char_boundary(c) => {
            if snap.caps.snippets {
                (
                    format!(
                        "{}$0{}",
                        snippet_escape(&insert[..c]),
                        snippet_escape(&insert[c..])
                    ),
                    InsertTextFormat::SNIPPET,
                )
            } else {
                (label.to_string(), InsertTextFormat::PLAIN_TEXT)
            }
        }
        _ => (insert.to_string(), InsertTextFormat::PLAIN_TEXT),
    }
}

fn after_dot(text: &str, start: usize) -> bool {
    text[..start].trim_end_matches([' ', '\t']).ends_with('.')
}

fn in_import_statement(text: &str, start: usize) -> bool {
    let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = text[line_start..start].trim_start();
    line.starts_with("import ") || line.starts_with("from ")
}

/// Second-press completion: project names from the symbol index, then the
/// stdlib names and modules the editor offers.
fn import_items(
    snap: &Snapshot,
    doc: &DocView,
    caret: TextSize,
    prefix: &str,
    classes_only: bool,
) -> Vec<(features::CompletionItem, String, String)> {
    let file = &doc.file;
    let lower = prefix.to_lowercase();
    let scope = file.scopes.scope_at(caret);
    let visible = |name: &str| {
        file.scopes.resolve(scope, name).is_some_and(|ids| {
            ids.iter()
                .any(|id| file.scopes.binding(*id).is_definition())
        }) || snap.analysis.builtins().is_some_and(|b| {
            !b.scopes
                .bindings_in(b.scopes.module_scope(), name)
                .is_empty()
        })
    };
    let mut out: Vec<(features::CompletionItem, String, String)> = Vec::new();
    for (name, module, k) in snap.index.top_level().iter() {
        if name.starts_with('_')
            || file.module_name.as_deref() == Some(module.as_str())
            || !features::matches_prefix(name, &lower)
            || visible(name)
        {
            continue;
        }
        let ck = match k {
            crate::syntax::OutlineKind::Class => CompletionKind::Class,
            crate::syntax::OutlineKind::Function => CompletionKind::Function,
            crate::syntax::OutlineKind::Constant => CompletionKind::Constant,
            _ => CompletionKind::Variable,
        };
        if classes_only && ck != CompletionKind::Class {
            continue;
        }
        let insert = if ck == CompletionKind::Function {
            format!("{name}()")
        } else {
            name.clone()
        };
        let caret_offset = (ck == CompletionKind::Function).then_some(name.len() + 1);
        out.push((
            features::CompletionItem {
                label: name.as_str().into(),
                kind: ck,
                detail: format!("from {module}"),
                insert,
                caret_offset,
                priority: 20,
            },
            module.clone(),
            name.clone(),
        ));
    }
    let bundled = if classes_only {
        rustpy_semantic::completion2::class_name_complete_with(
            &snap.analysis,
            file,
            caret,
            &[],
            &[],
        )
    } else {
        rustpy_semantic::completion2::second_press_complete(&snap.analysis, file, caret, &[])
    };
    for ic in bundled {
        if out
            .iter()
            .any(|(i, m, n)| i.label == ic.item.label && (m, n) == (&ic.module, &ic.name))
        {
            continue;
        }
        out.push((
            ic.item,
            ic.module,
            if ic.is_module { String::new() } else { ic.name },
        ));
    }
    out.sort_by(|a, b| {
        let a_exact = a.0.label.to_lowercase().starts_with(&lower);
        let b_exact = b.0.label.to_lowercase().starts_with(&lower);
        b_exact
            .cmp(&a_exact)
            .then(a.0.priority.cmp(&b.0.priority))
            .then_with(|| a.0.label.to_lowercase().cmp(&b.0.label.to_lowercase()))
            .then_with(|| a.1.cmp(&b.1))
    });
    out.truncate(200);
    out
}

/// The editor's live templates (builtin plus the user's `live_templates.json`).
fn live_templates() -> Vec<rustpy_templates::LiveTemplate> {
    let mut all = rustpy_templates::builtin_templates();
    let user = rustpy_templates::load_user_templates(
        &rustpy_settings::Settings::config_dir().join("live_templates.json"),
    );
    for t in user {
        all.retain(|b| b.abbreviation != t.abbreviation);
        all.push(t);
    }
    all
}

fn template_items(
    snap: &Snapshot,
    doc: &DocView,
    start: usize,
    offset: usize,
    prefix: &str,
) -> Vec<CompletionItem> {
    let text = doc.text();
    let line = doc.index.line_of(TextSize::new(start as u32));
    let line_range = doc.index.line_range(line).unwrap_or_default();
    let line_text = &text[usize::from(line_range.start())..usize::from(line_range.end())];
    let line_indent: String = line_text
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let as_is = snap.caps.insert_text_mode_as_is;
    // Without `asIs` the client indents continuation lines itself.
    let indent = if as_is {
        line_indent.clone()
    } else {
        String::new()
    };
    let replace_range = |s: usize| {
        doc.index.range(
            TextRange::new(TextSize::new(s as u32), TextSize::new(offset as u32)),
            snap.enc,
        )
    };
    let mut out = Vec::new();
    if start > 0 && text.as_bytes()[start - 1] == b'.' {
        // Postfix templates: `expr.if` → `if expr:`.
        let dot = start - 1;
        if let Some((expr_start, _, _)) = rustpy_templates::expression_before_dot(text, dot) {
            // The edit must stay on the caret's line.
            if expr_start >= usize::from(line_range.start()) {
                for t in rustpy_templates::matching_postfix(prefix) {
                    let Some(r) = rustpy_templates::apply_postfix(text, dot, t.key, &indent) else {
                        continue;
                    };
                    let caret = r.caret.min(r.text.len());
                    let snippet = format!(
                        "{}$0{}",
                        snippet_escape(&r.text[..caret]),
                        snippet_escape(&r.text[caret..])
                    );
                    out.push(CompletionItem {
                        label: t.key.to_string(),
                        kind: Some(CompletionItemKind::SNIPPET),
                        detail: Some(t.description.to_string()),
                        filter_text: Some(
                            text[usize::from(r.edit_range.start())..start].to_string() + t.key,
                        ),
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                            replace_range(usize::from(r.edit_range.start())),
                            snippet,
                        ))),
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        insert_text_mode: as_is.then_some(InsertTextMode::AS_IS),
                        ..Default::default()
                    });
                }
            }
        }
        return out;
    }
    let head = text[usize::from(line_range.start())..start].trim();
    let ctx = if head.is_empty() {
        rustpy_templates::TemplateContext::Statement
    } else {
        rustpy_templates::TemplateContext::Expression
    };
    let crumbs = crate::syntax::breadcrumbs(&doc.file.parse, TextSize::new(start as u32));
    let class_name = crumbs
        .iter()
        .rev()
        .find(|(c, _)| !c.ends_with(')'))
        .map(|(c, _)| c.clone());
    let function_name = crumbs
        .iter()
        .rev()
        .find(|(c, _)| c.ends_with(')'))
        .map(|(c, _)| c.trim_end_matches("()").to_string());
    let ectx = rustpy_templates::ExpandContext {
        indent,
        selection: None,
        file_name: doc
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        class_name,
        function_name,
        user: std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".into()),
        clipboard: None,
        line_number: line + 1,
    };
    for t in live_templates()
        .iter()
        .filter(|t| t.abbreviation.starts_with(prefix) && t.applies_in(ctx) && !t.is_surround())
    {
        let exp = rustpy_templates::expand(t, &ectx);
        out.push(CompletionItem {
            label: t.abbreviation.clone(),
            kind: Some(CompletionItemKind::SNIPPET),
            detail: Some(t.description.clone()),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("```python\n{}\n```", exp.text),
            })),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                replace_range(start),
                expansion_snippet(&exp),
            ))),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            insert_text_mode: as_is.then_some(InsertTextMode::AS_IS),
            ..Default::default()
        });
    }
    out
}

/// A template expansion as a snippet: every variable becomes a placeholder
/// (occurrences of the same variable share its number), `$END$` becomes `$0`.
pub fn expansion_snippet(exp: &rustpy_templates::Expansion) -> String {
    let names = exp.variables();
    let mut out = String::new();
    let mut at = 0usize;
    let mut end_done = false;
    for (s, e, name) in &exp.tab_stops {
        let (s, e) = ((*s).min(exp.text.len()), (*e).min(exp.text.len()));
        if s < at {
            continue;
        }
        if let Some(end) = exp
            .end_offset
            .filter(|end| !end_done && *end >= at && *end <= s)
        {
            out.push_str(&snippet_escape(&exp.text[at..end]));
            out.push_str("$0");
            at = end;
            end_done = true;
        }
        out.push_str(&snippet_escape(&exp.text[at..s]));
        let n = names.iter().position(|v| v == name).unwrap_or(0) + 1;
        out.push_str(&format!("${{{n}:{}}}", snippet_escape(&exp.text[s..e])));
        at = e;
    }
    match exp.end_offset.filter(|end| !end_done && *end >= at) {
        Some(end) => {
            let end = end.min(exp.text.len());
            out.push_str(&snippet_escape(&exp.text[at..end]));
            out.push_str("$0");
            out.push_str(&snippet_escape(&exp.text[end..]));
        }
        None => out.push_str(&snippet_escape(&exp.text[at..])),
    }
    out
}

static SCRATCH: AtomicU64 = AtomicU64::new(0);

/// Documentation of a completion item: Quick Documentation for the name as
/// if it were inserted.
pub fn resolve(snap: &Snapshot, mut item: CompletionItem) -> HResult<CompletionItem> {
    let Some(data) = item
        .data
        .clone()
        .and_then(|d| serde_json::from_value::<ResolveData>(d).ok())
    else {
        return Ok(item);
    };
    let doc = snap.doc(&data.uri)?;
    if data.version != doc.version || data.text_hash != hash(doc.text()) {
        return Err(LspError::content_modified());
    }
    if item.documentation.is_some() {
        return Ok(item);
    }
    if matches!(
        item.kind,
        Some(CompletionItemKind::KEYWORD | CompletionItemKind::SNIPPET)
    ) {
        return Ok(item);
    }
    let text = doc.text();
    let (start, end) = (data.start as usize, data.end as usize);
    if end > text.len()
        || start > end
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return Err(LspError::content_modified());
    }
    let name = item
        .filter_text
        .clone()
        .unwrap_or_else(|| item.label.clone());
    let (scratch, at) = match &data.import {
        Some((module, imported)) if imported.is_empty() => (
            format!("import {module}\n{module}\n"),
            format!("import {module}\n").len(),
        ),
        Some((module, imported)) => (
            format!("from {module} import {imported}\n{imported}\n"),
            format!("from {module} import {imported}\n").len(),
        ),
        None => (format!("{}{}{}", &text[..start], name, &text[end..]), start),
    };
    if let Some(info) = hover_in_scratch(snap, &doc.path, &scratch, TextSize::new(at as u32)) {
        let markdown = snap.caps.completion_markdown;
        let value = crate::handlers::hover_text(&info, markdown);
        if item.detail.is_none() {
            item.detail = Some(info.signature.clone());
        }
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: if markdown {
                MarkupKind::Markdown
            } else {
                MarkupKind::PlainText
            },
            value,
        }));
    }
    Ok(item)
}

fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Hover at `offset` of `text` analysed as a sibling of `path` (so relative
/// imports and the project's modules resolve the same way).
fn hover_in_scratch(
    snap: &Snapshot,
    path: &Path,
    text: &str,
    offset: TextSize,
) -> Option<rustpy_semantic::features::HoverInfo> {
    let dir = path
        .parent()
        .filter(|_| !crate::paths::is_virtual_stub(path))
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
    let scratch: PathBuf = dir.join(format!(
        "__rustpy_completion_{}_{n}__.py",
        std::process::id()
    ));
    snap.analysis.set_file_text(&scratch, text);
    let info = snap
        .analysis
        .file_for_path(&scratch)
        .and_then(|f| features::hover_at(&snap.analysis, &f, offset));
    snap.analysis.close_file(&scratch);
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_become_snippets() {
        let exp = rustpy_templates::Expansion {
            text: "for item in items:\n    pass".into(),
            tab_stops: vec![(4, 8, "VAR".into()), (12, 17, "ITER".into())],
            end_offset: Some(23),
        };
        assert_eq!(
            expansion_snippet(&exp),
            "for ${1:item} in ${2:items}:\n    $0pass"
        );
        let mirrored = rustpy_templates::Expansion {
            text: "x = x".into(),
            tab_stops: vec![(0, 1, "A".into()), (4, 5, "A".into())],
            end_offset: None,
        };
        assert_eq!(expansion_snippet(&mirrored), "${1:x} = ${1:x}");
        assert_eq!(snippet_escape("a$b}c\\"), "a\\$b\\}c\\\\");
    }
}
