//! Request handlers. Each runs on a worker thread with a [`Snapshot`] and
//! answers with the IDE engine's results converted to LSP types.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    DocumentFormattingParams, DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams,
    DocumentRangeFormattingParams, DocumentSymbolParams, DocumentSymbolResponse, Documentation,
    FoldingRange, FoldingRangeKind, FoldingRangeParams, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, InlayHint, InlayHintKind,
    InlayHintLabel, InlayHintParams, Location, MarkupContent, MarkupKind, OneOf,
    OptionalVersionedTextDocumentIdentifier, ParameterInformation, ParameterLabel,
    PrepareRenameResponse, ReferenceParams, RenameParams, SelectionRange, SelectionRangeParams,
    SemanticTokens, SemanticTokensParams, SemanticTokensRangeParams, SemanticTokensRangeResult,
    SemanticTokensResult, SignatureHelp, SignatureHelpParams, SignatureInformation,
    SymbolInformation, SymbolKind, TextDocumentEdit, TextDocumentPositionParams, TextEdit, Uri,
    WorkspaceEdit, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use rustpy_core::{TextRange, TextSize};
use rustpy_refactor::rename;
use rustpy_semantic::features::{self, HoverInfo};
use rustpy_semantic::{hierarchy, usages};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;

use crate::semantic_tokens;
use crate::snapshot::{DocView, HResult, LspError, Snapshot};
use crate::syntax;

// ----- hover, signature help ------------------------------------------------

/// Quick Documentation as the IDE shows it: the signature, where the symbol
/// comes from, then the rendered docstring.
pub fn hover_text(info: &HoverInfo, markdown: bool) -> String {
    let doc = info
        .docstring
        .as_deref()
        .map(dedent_docstring)
        .filter(|d| !d.is_empty());
    if !markdown {
        let mut out = info.signature.clone();
        if let Some(o) = &info.origin {
            out.push('\n');
            out.push_str(o);
        }
        if let Some(d) = doc {
            out.push_str("\n\n");
            out.push_str(&d);
        }
        return out;
    }
    let mut out = format!("```python\n{}\n```", info.signature);
    if let Some(o) = info.origin.as_ref().filter(|o| !o.is_empty()) {
        out.push_str(&format!(
            "\n*{}*",
            o.replace('*', "\\*").replace('_', "\\_")
        ));
    }
    if let Some(d) = doc {
        out.push_str("\n\n---\n\n");
        out.push_str(&rustpy_semantic::docstrings::render_markdown(
            &d,
            rustpy_semantic::docstrings::DocstringFormat::Plain,
        ));
    }
    out
}

/// Dedents a docstring and trims blank edges (as the Quick Documentation popup does).
pub fn dedent_docstring(doc: &str) -> String {
    let lines: Vec<&str> = doc.lines().collect();
    let indent = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out: Vec<String> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if i == 0 {
            out.push(l.trim().to_string());
        } else {
            out.push(
                l.get(indent.min(l.len())..)
                    .unwrap_or("")
                    .trim_end()
                    .to_string(),
            );
        }
    }
    let joined = out.join("\n");
    let trimmed = joined.trim();
    if trimmed.len() > 4000 {
        format!(
            "{}…",
            &trimmed[..rustpy_core::floor_char_boundary(trimmed, 4000)]
        )
    } else {
        trimmed.to_string()
    }
}

pub fn hover(snap: &Snapshot, params: HoverParams) -> HResult<Option<Hover>> {
    let pos = params.text_document_position_params;
    let doc = snap.doc(&pos.text_document.uri)?;
    let offset = doc.offset(pos.position, snap.enc);
    let Some(info) = features::hover_at(&snap.analysis, &doc.file, offset) else {
        return Ok(None);
    };
    let range =
        features::ident_at(&doc.file, offset).map(|t| doc.index.range(t.text_range(), snap.enc));
    let markdown = snap.caps.hover_markdown;
    let value = hover_text(&info, markdown);
    Ok(Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: if markdown {
                MarkupKind::Markdown
            } else {
                MarkupKind::PlainText
            },
            value,
        }),
        range,
    }))
}

pub fn signature_help(
    snap: &Snapshot,
    params: SignatureHelpParams,
) -> HResult<Option<SignatureHelp>> {
    let pos = params.text_document_position_params;
    let doc = snap.doc(&pos.text_document.uri)?;
    let offset = doc.offset(pos.position, snap.enc);
    let Some(h) = features::signature_help(&snap.analysis, &doc.file, offset) else {
        return Ok(None);
    };
    let params: Vec<ParameterInformation> = h
        .params
        .iter()
        .filter(|(s, e)| {
            *s <= *e
                && *e <= h.label.len()
                && h.label.is_char_boundary(*s)
                && h.label.is_char_boundary(*e)
        })
        .map(|&(s, e)| {
            let label = if snap.caps.signature_label_offsets {
                // Offsets count UTF-16 code units of the label.
                let a = h.label[..s].encode_utf16().count() as u32;
                let b = a + h.label[s..e].encode_utf16().count() as u32;
                ParameterLabel::LabelOffsets([a, b])
            } else {
                ParameterLabel::Simple(h.label[s..e].to_string())
            };
            ParameterInformation {
                label,
                documentation: None,
            }
        })
        .collect();
    let mut label = h.label.clone();
    if h.overloads > 1 {
        // The IDE shows the overload count in the Parameter Info popup.
        label.push_str(&format!("  (+{} overloads)", h.overloads - 1));
    }
    let documentation = h
        .docstring
        .as_ref()
        .filter(|d| !d.trim().is_empty())
        .map(|d| {
            if snap.caps.signature_markdown {
                Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: rustpy_semantic::docstrings::render_markdown(
                        d,
                        rustpy_semantic::docstrings::DocstringFormat::Plain,
                    ),
                })
            } else {
                Documentation::String(d.clone())
            }
        });
    let active = h.active.map(|a| a as u32);
    let sig = SignatureInformation {
        label,
        documentation,
        parameters: Some(params),
        active_parameter: if snap.caps.signature_active_parameter {
            active
        } else {
            None
        },
    };
    Ok(Some(SignatureHelp {
        signatures: vec![sig],
        active_signature: Some(0),
        active_parameter: active,
    }))
}

// ----- navigation ---------------------------------------------------------------

fn goto(snap: &Snapshot, locs: Vec<features::Location>) -> Option<GotoDefinitionResponse> {
    let out = snap.locations(locs.into_iter().map(|l| (l.path, l.range)));
    (!out.is_empty()).then_some(GotoDefinitionResponse::Array(out))
}

/// Go to Declaration (Ctrl+B); `textDocument/declaration` answers the same.
pub fn definition(
    snap: &Snapshot,
    params: GotoDefinitionParams,
) -> HResult<Option<GotoDefinitionResponse>> {
    let pos = params.text_document_position_params;
    let doc = snap.doc(&pos.text_document.uri)?;
    let offset = doc.offset(pos.position, snap.enc);
    Ok(goto(
        snap,
        features::definition_at(&snap.analysis, &doc.file, offset),
    ))
}

/// Go to Type Declaration (Ctrl+Shift+B).
pub fn type_definition(
    snap: &Snapshot,
    params: GotoDefinitionParams,
) -> HResult<Option<GotoDefinitionResponse>> {
    let pos = params.text_document_position_params;
    let doc = snap.doc(&pos.text_document.uri)?;
    let offset = doc.offset(pos.position, snap.enc);
    Ok(goto(
        snap,
        features::type_definition_at(&snap.analysis, &doc.file, offset),
    ))
}

/// Go to Implementation(s) (Ctrl+Alt+B): subclasses, or overriding methods.
pub fn implementation(
    snap: &Snapshot,
    params: GotoDefinitionParams,
) -> HResult<Option<GotoDefinitionResponse>> {
    let pos = params.text_document_position_params;
    let doc = snap.doc(&pos.text_document.uri)?;
    let offset = doc.offset(pos.position, snap.enc);
    let files = snap.project_files();
    snap.check_cancelled()?;
    Ok(goto(
        snap,
        hierarchy::implementations(&snap.analysis, &doc.file, offset, &files),
    ))
}

/// Find Usages (Alt+F7).
pub fn references(snap: &Snapshot, params: ReferenceParams) -> HResult<Option<Vec<Location>>> {
    let pos = params.text_document_position;
    let doc = snap.doc(&pos.text_document.uri)?;
    let offset = doc.offset(pos.position, snap.enc);
    let files = snap.project_files();
    snap.check_cancelled()?;
    let found = rustpy_search::usages::find_usages(&snap.analysis, &doc.file, offset, &files);
    snap.check_cancelled()?;
    let include_decl = params.context.include_declaration;
    // Without declarations, imports still count: Find Usages lists them as usages.
    let is_import = |u: &usages::Usage| {
        snap.file_for_path(&u.path).is_some_and(|f| {
            let root = f.parse.syntax_node();
            root.token_at_offset(u.range.start())
                .right_biased()
                .is_some_and(|t| {
                    t.parent_ancestors().any(|n| {
                        matches!(
                            n.kind(),
                            SyntaxKind::IMPORT_STMT | SyntaxKind::IMPORT_FROM_STMT
                        )
                    })
                })
        })
    };
    let out = snap.locations(
        found
            .into_iter()
            .filter(|u| include_decl || !u.is_definition || is_import(u))
            .map(|u| (u.path, u.range)),
    );
    Ok(Some(out))
}

/// Highlight Usages in File: writes and reads of the symbol at the caret.
pub fn document_highlight(
    snap: &Snapshot,
    params: DocumentHighlightParams,
) -> HResult<Option<Vec<DocumentHighlight>>> {
    let pos = params.text_document_position_params;
    let doc = snap.doc(&pos.text_document.uri)?;
    let offset = doc.offset(pos.position, snap.enc);
    let found = usages::usages_in_file(&snap.analysis, &doc.file, offset);
    let out: Vec<DocumentHighlight> = found
        .into_iter()
        .filter(|u| u.path == doc.path)
        .map(|u| DocumentHighlight {
            range: doc.index.range(u.range, snap.enc),
            kind: Some(if u.is_write {
                DocumentHighlightKind::WRITE
            } else {
                DocumentHighlightKind::READ
            }),
        })
        .collect();
    Ok((!out.is_empty()).then_some(out))
}

// ----- rename ---------------------------------------------------------------------

pub fn prepare_rename(
    snap: &Snapshot,
    params: TextDocumentPositionParams,
) -> HResult<Option<PrepareRenameResponse>> {
    let doc = snap.doc(&params.text_document.uri)?;
    let offset = doc.offset(params.position, snap.enc);
    let Some(tok) = features::ident_at(&doc.file, offset).filter(|t| t.kind() == SyntaxKind::IDENT)
    else {
        return Ok(None);
    };
    rename::check_rename_at(&snap.analysis, &doc.file, tok.text_range().start())
        .map_err(LspError::failed)?;
    Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: doc.index.range(tok.text_range(), snap.enc),
        placeholder: tok.text().to_string(),
    }))
}

/// Rename (Shift+F6) with the IDE's rename engine: every usage across the
/// project, keyword arguments and docstring parameter entries included.
pub fn rename(snap: &Snapshot, params: RenameParams) -> HResult<Option<WorkspaceEdit>> {
    let pos = params.text_document_position;
    let doc = snap.doc(&pos.text_document.uri)?;
    let offset = doc.offset(pos.position, snap.enc);
    // A position just after the name still renames it.
    let offset = features::ident_at(&doc.file, offset)
        .map(|t| t.text_range().start())
        .unwrap_or(offset);
    let files = snap.project_files();
    snap.check_cancelled()?;
    let edits = rename::rename(&snap.analysis, &doc.file, offset, &params.new_name, &files)
        .map_err(LspError::failed)?;
    snap.check_cancelled()?;
    Ok(Some(workspace_edit(snap, edits)?))
}

/// Edits of several files as a workspace edit (versioned for open documents
/// when the client supports `documentChanges`).
// `WorkspaceEdit::changes` is keyed by `Uri` (whose parsed form caches data inside).
#[allow(clippy::mutable_key_type)]
pub fn workspace_edit(
    snap: &Snapshot,
    edits: Vec<(PathBuf, Vec<(TextRange, String)>)>,
) -> HResult<WorkspaceEdit> {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    let mut doc_changes: Vec<TextDocumentEdit> = Vec::new();
    for (path, file_edits) in edits {
        if file_edits.is_empty() {
            continue;
        }
        let uri = snap
            .uri_for(&path)
            .ok_or_else(|| LspError::failed(format!("cannot edit {}", path.display())))?;
        let index = snap
            .line_index(&path)
            .ok_or_else(|| LspError::failed(format!("cannot read {}", path.display())))?;
        let mut text_edits: Vec<TextEdit> = file_edits
            .into_iter()
            .map(|(r, t)| TextEdit::new(index.range(r, snap.enc), t))
            .collect();
        text_edits.sort_by_key(|e| (e.range.start.line, e.range.start.character));
        if snap.caps.document_changes {
            doc_changes.push(TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier {
                    uri,
                    version: snap.version_of(&path),
                },
                edits: text_edits.into_iter().map(OneOf::Left).collect(),
            });
        } else {
            changes.entry(uri).or_default().extend(text_edits);
        }
    }
    Ok(if snap.caps.document_changes {
        WorkspaceEdit {
            changes: None,
            document_changes: Some(lsp_types::DocumentChanges::Edits(doc_changes)),
            change_annotations: None,
        }
    } else {
        WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }
    })
}

// ----- formatting ---------------------------------------------------------------

pub fn formatting(
    snap: &Snapshot,
    params: DocumentFormattingParams,
) -> HResult<Option<Vec<TextEdit>>> {
    let doc = snap.doc(&params.text_document.uri)?;
    Ok(Some(crate::formatting::format(
        snap,
        &doc.path,
        &doc.index,
        None,
        &params.options,
    )?))
}

pub fn range_formatting(
    snap: &Snapshot,
    params: DocumentRangeFormattingParams,
) -> HResult<Option<Vec<TextEdit>>> {
    let doc = snap.doc(&params.text_document.uri)?;
    let range = doc.index.text_range(params.range, snap.enc);
    Ok(Some(crate::formatting::format(
        snap,
        &doc.path,
        &doc.index,
        Some(range),
        &params.options,
    )?))
}

// ----- symbols ------------------------------------------------------------------

pub fn document_symbol(
    snap: &Snapshot,
    params: DocumentSymbolParams,
) -> HResult<Option<DocumentSymbolResponse>> {
    let doc = snap.doc(&params.text_document.uri)?;
    let items = syntax::outline(&doc.file.parse);
    if snap.caps.hierarchical_symbols {
        return Ok(Some(DocumentSymbolResponse::Nested(
            crate::symbols::document_symbols(&items, &doc.index, snap.enc),
        )));
    }
    let mut flat = Vec::new();
    crate::symbols::flat_symbols(&items, &doc.index, snap.enc, &doc.uri, None, &mut flat);
    Ok(Some(DocumentSymbolResponse::Flat(flat)))
}

/// Updates the symbol index for the current project files (see [`crate::symbols::SymbolIndex::refresh`]).
pub fn refresh_index(snap: &Snapshot, include_open: bool) -> Option<bool> {
    let files = if include_open {
        snap.project_files()
    } else {
        snap.files.get()
    };
    let open = |p: &Path| {
        include_open
            .then(|| {
                snap.docs
                    .get(p)
                    .filter(|d| d.is_python)
                    .map(|d| d.index.text_arc())
            })
            .flatten()
    };
    let module_of = |p: &Path| snap.analysis.resolver.module_name_of(p);
    snap.index
        .refresh(&files, &open, &module_of, &|| snap.cancel.is_cancelled())
}

/// Go to Symbol: fuzzy matching over the project's definitions.
#[allow(deprecated)]
pub fn workspace_symbol(
    snap: &Snapshot,
    params: WorkspaceSymbolParams,
) -> HResult<Option<WorkspaceSymbolResponse>> {
    if refresh_index(snap, true).is_none() {
        return Err(LspError::cancelled());
    }
    let query = params.query.trim();
    let hits = snap.index.search(query, 256);
    let mut uris: HashMap<PathBuf, Option<Uri>> = HashMap::new();
    let mut out = Vec::new();
    for (path, s) in hits {
        let Some(uri) = uris
            .entry(path.clone())
            .or_insert_with(|| snap.uri_for(&path))
            .clone()
        else {
            continue;
        };
        out.push(SymbolInformation {
            name: s.name.clone(),
            kind: crate::symbols::symbol_kind(s.kind),
            tags: None,
            deprecated: None,
            location: Location::new(uri, s.range),
            container_name: s
                .container
                .clone()
                .or_else(|| snap.analysis.resolver.module_name_of(&path)),
        });
    }
    Ok(Some(WorkspaceSymbolResponse::Flat(out)))
}

// ----- folding, selection ranges --------------------------------------------------

pub fn folding_range(
    snap: &Snapshot,
    params: FoldingRangeParams,
) -> HResult<Option<Vec<FoldingRange>>> {
    let doc = snap.doc(&params.text_document.uri)?;
    let index = &doc.index;
    let regions = syntax::fold_regions(&doc.file.parse, &|off| index.line_of(off));
    let text = doc.text();
    let mut out = Vec::new();
    for r in regions {
        let kind = match r.kind {
            syntax::FoldKind::Imports => Some(FoldingRangeKind::Imports),
            syntax::FoldKind::Docstring => Some(FoldingRangeKind::Comment),
            syntax::FoldKind::Region => Some(FoldingRangeKind::Region),
            _ => None,
        };
        let mut end_line = r.last_line as u32;
        let (mut start_character, mut end_character) = (None, None);
        if snap.caps.line_folding_only {
            // Whole lines fold: keep a closing `)` / `]` visible on its own line.
            let line_end = index
                .line_range(r.last_line)
                .map(|l| l.end())
                .unwrap_or(index.len());
            let rest = &text[usize::from(r.range.end()).min(text.len())
                ..usize::from(line_end)
                    .max(usize::from(r.range.end()))
                    .min(text.len())];
            if !rest.trim().is_empty() {
                end_line = end_line.saturating_sub(1);
            }
        } else {
            start_character = Some(index.position(r.range.start(), snap.enc).character);
            end_character = Some(index.position(r.range.end(), snap.enc).character);
        }
        if end_line <= r.first_line as u32 {
            continue;
        }
        let collapsed_text = snap
            .caps
            .folding_collapsed_text
            .then(|| r.placeholder.clone());
        out.push(FoldingRange {
            start_line: r.first_line as u32,
            start_character,
            end_line,
            end_character,
            kind,
            collapsed_text,
        });
    }
    Ok(Some(out))
}

/// Extend Selection (Ctrl+W) steps at each position, innermost first.
pub fn selection_range(
    snap: &Snapshot,
    params: SelectionRangeParams,
) -> HResult<Option<Vec<SelectionRange>>> {
    let doc = snap.doc(&params.text_document.uri)?;
    let mut out = Vec::new();
    for pos in params.positions {
        let offset = doc.offset(pos, snap.enc);
        let chain = syntax::selection_chain(&doc.file.parse, offset);
        let mut node: Option<SelectionRange> = None;
        for r in chain.iter().rev() {
            node = Some(SelectionRange {
                range: doc.index.range(*r, snap.enc),
                parent: node.map(Box::new),
            });
        }
        out.push(node.unwrap_or(SelectionRange {
            range: lsp_types::Range::new(pos, pos),
            parent: None,
        }));
    }
    Ok(Some(out))
}

// ----- semantic tokens, inlay hints ------------------------------------------------

fn semantic_toks(snap: &Snapshot, doc: &DocView) -> Vec<semantic_tokens::Tok> {
    let builtins = snap.analysis.builtins();
    let classes: std::collections::HashSet<String> = builtins
        .as_ref()
        .map(|b| {
            let ms = b.scopes.module_scope();
            b.scopes
                .scope(ms)
                .bindings
                .iter()
                .filter(|(_, ids)| {
                    ids.iter().any(|id| {
                        b.scopes.binding(*id).kind == rustpy_semantic::scope::BindingKind::Class
                    })
                })
                .map(|(n, _)| n.to_string())
                .collect()
        })
        .unwrap_or_default();
    semantic_tokens::classify(&doc.file, &|name| classes.contains(name))
}

pub fn semantic_tokens_full(
    snap: &Snapshot,
    params: SemanticTokensParams,
) -> HResult<Option<SemanticTokensResult>> {
    let doc = snap.doc(&params.text_document.uri)?;
    let toks = semantic_toks(snap, &doc);
    let data = semantic_tokens::encode(&toks, &doc.index, snap.enc);
    Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data,
    })))
}

pub fn semantic_tokens_range(
    snap: &Snapshot,
    params: SemanticTokensRangeParams,
) -> HResult<Option<SemanticTokensRangeResult>> {
    let doc = snap.doc(&params.text_document.uri)?;
    let range = doc.index.text_range(params.range, snap.enc);
    let toks: Vec<semantic_tokens::Tok> = semantic_toks(snap, &doc)
        .into_iter()
        .filter(|t| t.range.end() > range.start() && t.range.start() < range.end())
        .collect();
    let data = semantic_tokens::encode(&toks, &doc.index, snap.enc);
    Ok(Some(SemanticTokensRangeResult::Tokens(SemanticTokens {
        result_id: None,
        data,
    })))
}

/// Parameter-name and inferred-type hints (Settings › Editor › Inlay Hints).
pub fn inlay_hints(snap: &Snapshot, params: InlayHintParams) -> HResult<Option<Vec<InlayHint>>> {
    let doc = snap.doc(&params.text_document.uri)?;
    let editor = &snap.config.settings.editor;
    let (ph, th) = (editor.inlay_parameter_hints, editor.inlay_type_hints);
    if !(ph || th) || !editor.show_inlay_hints {
        return Ok(Some(Vec::new()));
    }
    let range = doc.index.text_range(params.range, snap.enc);
    let hints = features::inlay_hints(&snap.analysis, &doc.file, ph, th);
    let out = hints
        .into_iter()
        .filter(|h| h.offset >= range.start() && h.offset <= range.end())
        .map(|h| {
            let is_param = h.kind == features::InlayKind::Parameter;
            InlayHint {
                position: doc.index.position(h.offset, snap.enc),
                label: InlayHintLabel::String(h.label),
                kind: Some(if is_param {
                    InlayHintKind::PARAMETER
                } else {
                    InlayHintKind::TYPE
                }),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: Some(is_param),
                data: None,
            }
        })
        .collect();
    Ok(Some(out))
}

// ----- call hierarchy ---------------------------------------------------------------

/// The function (or class) definition whose name is at `name_range`, or
/// that encloses `offset`.
fn def_node(
    file: &rustpy_semantic::FileInfo,
    offset: TextSize,
) -> Option<rustpy_syntax::SyntaxNode> {
    let root = file.parse.syntax_node();
    let tok = root
        .token_at_offset(offset)
        .right_biased()
        .or_else(|| root.token_at_offset(offset).left_biased())?;
    tok.parent_ancestors()
        .find(|n| n.kind() == SyntaxKind::FUNC_DEF)
}

fn call_item(
    snap: &Snapshot,
    path: &Path,
    file: &rustpy_semantic::FileInfo,
    node: &rustpy_syntax::SyntaxNode,
) -> Option<CallHierarchyItem> {
    let f = ast::FuncDef::cast(node.clone())?;
    let name = f.name()?;
    let index = snap.line_index(path)?;
    let in_class = node
        .parent()
        .and_then(|b| b.parent())
        .is_some_and(|c| c.kind() == SyntaxKind::CLASS_DEF);
    let class = node
        .ancestors()
        .skip(1)
        .find_map(ast::ClassDef::cast)
        .map(|c| c.name_text());
    let range = TextRange::new(syntax::trimmed_start(node), syntax::trimmed_end(node));
    Some(CallHierarchyItem {
        name: name.text().to_string(),
        kind: if in_class {
            SymbolKind::METHOD
        } else {
            SymbolKind::FUNCTION
        },
        tags: None,
        detail: class.or_else(|| file.module_name.clone()),
        uri: snap.uri_for(path)?,
        range: index.range(range, snap.enc),
        selection_range: index.range(name.text_range(), snap.enc),
        data: None,
    })
}

/// A module-level caller: the module itself.
fn module_item(
    snap: &Snapshot,
    path: &Path,
    file: &rustpy_semantic::FileInfo,
) -> Option<CallHierarchyItem> {
    let index = snap.line_index(path)?;
    let whole = index.range(TextRange::new(TextSize::new(0), index.len()), snap.enc);
    let name = file.module_name.clone().unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    Some(CallHierarchyItem {
        name,
        kind: SymbolKind::MODULE,
        tags: None,
        detail: None,
        uri: snap.uri_for(path)?,
        range: whole,
        selection_range: lsp_types::Range::new(whole.start, whole.start),
        data: None,
    })
}

pub fn prepare_call_hierarchy(
    snap: &Snapshot,
    params: CallHierarchyPrepareParams,
) -> HResult<Option<Vec<CallHierarchyItem>>> {
    let pos = params.text_document_position_params;
    let doc = snap.doc(&pos.text_document.uri)?;
    let offset = doc.offset(pos.position, snap.enc);
    // A call or reference: the function it resolves to.
    for loc in features::definition_at(&snap.analysis, &doc.file, offset) {
        let Some(file) = snap.file_for_path(&loc.path) else {
            continue;
        };
        if let Some(node) = def_node(&file, loc.range.start()).filter(|n| {
            ast::FuncDef::cast(n.clone())
                .and_then(|f| f.name())
                .is_some_and(|t| t.text_range() == loc.range)
        }) {
            return Ok(call_item(snap, &loc.path, &file, &node).map(|i| vec![i]));
        }
    }
    // Otherwise the function around the caret.
    Ok(def_node(&doc.file, offset)
        .and_then(|n| call_item(snap, &doc.path, &doc.file, &n))
        .map(|i| vec![i]))
}

pub fn incoming_calls(
    snap: &Snapshot,
    params: CallHierarchyIncomingCallsParams,
) -> HResult<Option<Vec<CallHierarchyIncomingCall>>> {
    let doc = snap.doc(&params.item.uri)?;
    let offset = doc.offset(params.item.selection_range.start, snap.enc);
    let files = snap.project_files();
    snap.check_cancelled()?;
    let usages = rustpy_search::usages::find_usages(&snap.analysis, &doc.file, offset, &files);
    snap.check_cancelled()?;
    // Group the call sites by the function (or module) containing them.
    let mut groups: Vec<(CallHierarchyItem, Vec<lsp_types::Range>)> = Vec::new();
    for c in usages {
        let Some(file) = snap.file_for_path(&c.path) else {
            continue;
        };
        let Some(index) = snap.line_index(&c.path) else {
            continue;
        };
        // Hierarchy nodes navigate to caller declarations. LSP also needs the
        // original call-site ranges, so classify usages before aggregating.
        if rustpy_semantic::hierarchy::caller_in_file(&file, &c).is_none() {
            continue;
        }
        let caller = def_node(&file, c.range.start());
        let item = match caller {
            Some(n) => call_item(snap, &c.path, &file, &n),
            None => module_item(snap, &c.path, &file),
        };
        let Some(item) = item else { continue };
        let range = index.range(c.range, snap.enc);
        match groups
            .iter_mut()
            .find(|(i, _)| i.uri == item.uri && i.selection_range == item.selection_range)
        {
            Some((_, ranges)) => ranges.push(range),
            None => groups.push((item, vec![range])),
        }
    }
    Ok(Some(
        groups
            .into_iter()
            .map(|(from, from_ranges)| CallHierarchyIncomingCall { from, from_ranges })
            .collect(),
    ))
}

pub fn outgoing_calls(
    snap: &Snapshot,
    params: CallHierarchyOutgoingCallsParams,
) -> HResult<Option<Vec<CallHierarchyOutgoingCall>>> {
    let doc = snap.doc(&params.item.uri)?;
    let offset = doc.offset(params.item.selection_range.start, snap.enc);
    let Some(func) = def_node(&doc.file, offset) else {
        return Ok(None);
    };
    let mut groups: Vec<(CallHierarchyItem, Vec<lsp_types::Range>)> = Vec::new();
    for call in func
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CALL_EXPR)
    {
        snap.check_cancelled()?;
        let Some(callee) = ast::CallExpr::cast(call.clone()).and_then(|c| c.callee()) else {
            continue;
        };
        let name_range = match &callee {
            ast::Expr::Name(n) => n.syntax().text_range(),
            ast::Expr::Attribute(a) => match a.attr() {
                Some(t) => t.text_range(),
                None => continue,
            },
            _ => continue,
        };
        // Calls of nested functions belong to them.
        if call
            .ancestors()
            .find(|n| n.kind() == SyntaxKind::FUNC_DEF)
            .as_ref()
            != Some(&func)
        {
            continue;
        }
        for loc in features::definition_at(&snap.analysis, &doc.file, name_range.start()) {
            let Some(file) = snap.file_for_path(&loc.path) else {
                continue;
            };
            let Some(node) = def_node(&file, loc.range.start()).filter(|n| {
                ast::FuncDef::cast(n.clone())
                    .and_then(|f| f.name())
                    .is_some_and(|t| t.text_range() == loc.range)
            }) else {
                continue;
            };
            let Some(item) = call_item(snap, &loc.path, &file, &node) else {
                continue;
            };
            let range = doc.index.range(name_range, snap.enc);
            match groups
                .iter_mut()
                .find(|(i, _)| i.uri == item.uri && i.selection_range == item.selection_range)
            {
                Some((_, ranges)) => ranges.push(range),
                None => groups.push((item, vec![range])),
            }
            break;
        }
    }
    Ok(Some(
        groups
            .into_iter()
            .map(|(to, from_ranges)| CallHierarchyOutgoingCall { to, from_ranges })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_text_matches_quick_documentation() {
        let info = HoverInfo {
            signature: "def greet(name: str) -> str".into(),
            docstring: Some("Say hello.\n\n    :param name: who\n    ".into()),
            origin: Some("my_mod".into()),
            kind: Some("function"),
        };
        let md = hover_text(&info, true);
        assert!(
            md.starts_with("```python\ndef greet(name: str) -> str\n```\n*my\\_mod*"),
            "{md}"
        );
        assert!(md.contains("Say hello."));
        assert!(md.contains("`name`"), "{md}");
        let plain = hover_text(&info, false);
        assert!(
            plain.starts_with("def greet(name: str) -> str\nmy_mod\n\nSay hello."),
            "{plain}"
        );
    }
}
