//! Client capabilities the server adapts to, and the capabilities it announces.

use lsp_types::{
    CallHierarchyServerCapability, ClientCapabilities, CodeActionKind, CodeActionOptions,
    CodeActionProviderCapability, CompletionOptions, CompletionOptionsCompletionItem,
    DeclarationCapability, FoldingRangeProviderCapability, HoverProviderCapability,
    ImplementationProviderCapability, InlayHintOptions, InlayHintServerCapabilities, MarkupKind,
    OneOf, RenameOptions, SaveOptions, SelectionRangeProviderCapability, SemanticTokensFullOptions,
    SemanticTokensOptions, SemanticTokensServerCapabilities, ServerCapabilities,
    SignatureHelpOptions, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TypeDefinitionProviderCapability,
    WorkDoneProgressOptions, WorkspaceFoldersServerCapabilities, WorkspaceServerCapabilities,
};

use crate::line_index::Encoding;

/// The client features that change what the server sends.
#[derive(Clone, Debug, Default)]
pub struct ClientCaps {
    pub hover_markdown: bool,
    pub completion_markdown: bool,
    pub snippets: bool,
    /// Completion edits may have an insert and a replace range.
    pub insert_replace: bool,
    /// `insertTextMode: asIs` is understood (multi-line snippets keep our indentation).
    pub insert_text_mode_as_is: bool,
    pub signature_markdown: bool,
    pub signature_label_offsets: bool,
    pub signature_active_parameter: bool,
    pub document_changes: bool,
    pub line_folding_only: bool,
    pub folding_collapsed_text: bool,
    pub hierarchical_symbols: bool,
    pub watched_files_dynamic: bool,
    pub diagnostic_tags: bool,
    pub diagnostic_versions: bool,
}

impl ClientCaps {
    pub fn from(c: &ClientCapabilities) -> Self {
        let td = c.text_document.as_ref();
        let ws = c.workspace.as_ref();
        let markdown = |formats: Option<&Vec<MarkupKind>>| {
            formats.is_some_and(|f| f.contains(&MarkupKind::Markdown))
        };
        let item = td
            .and_then(|t| t.completion.as_ref())
            .and_then(|c| c.completion_item.as_ref());
        let sig = td
            .and_then(|t| t.signature_help.as_ref())
            .and_then(|s| s.signature_information.as_ref());
        let folding = td.and_then(|t| t.folding_range.as_ref());
        Self {
            hover_markdown: markdown(
                td.and_then(|t| t.hover.as_ref())
                    .and_then(|h| h.content_format.as_ref()),
            ),
            completion_markdown: markdown(item.and_then(|i| i.documentation_format.as_ref())),
            snippets: item.and_then(|i| i.snippet_support).unwrap_or(false),
            insert_replace: item.and_then(|i| i.insert_replace_support).unwrap_or(false),
            insert_text_mode_as_is: item
                .and_then(|i| i.insert_text_mode_support.as_ref())
                .is_some_and(|m| m.value_set.contains(&lsp_types::InsertTextMode::AS_IS)),
            signature_markdown: markdown(sig.and_then(|s| s.documentation_format.as_ref())),
            signature_label_offsets: sig
                .and_then(|s| s.parameter_information.as_ref())
                .and_then(|p| p.label_offset_support)
                .unwrap_or(false),
            signature_active_parameter: sig
                .and_then(|s| s.active_parameter_support)
                .unwrap_or(false),
            document_changes: ws
                .and_then(|w| w.workspace_edit.as_ref())
                .and_then(|e| e.document_changes)
                .unwrap_or(false),
            line_folding_only: folding.and_then(|f| f.line_folding_only).unwrap_or(false),
            folding_collapsed_text: folding
                .and_then(|f| f.folding_range.as_ref())
                .and_then(|f| f.collapsed_text)
                .unwrap_or(false),
            hierarchical_symbols: td
                .and_then(|t| t.document_symbol.as_ref())
                .and_then(|d| d.hierarchical_document_symbol_support)
                .unwrap_or(false),
            watched_files_dynamic: ws
                .and_then(|w| w.did_change_watched_files.as_ref())
                .and_then(|d| d.dynamic_registration)
                .unwrap_or(false),
            diagnostic_tags: td
                .and_then(|t| t.publish_diagnostics.as_ref())
                .is_some_and(|p| p.tag_support.is_some()),
            diagnostic_versions: td
                .and_then(|t| t.publish_diagnostics.as_ref())
                .and_then(|p| p.version_support)
                .unwrap_or(false),
        }
    }
}

pub fn server_capabilities(enc: Encoding) -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(enc.kind()),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                will_save: None,
                will_save_wait_until: None,
                save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                    include_text: Some(false),
                })),
            },
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(true),
            trigger_characters: Some(vec![".".into()]),
            all_commit_characters: None,
            work_done_progress_options: WorkDoneProgressOptions::default(),
            completion_item: Some(CompletionOptionsCompletionItem {
                label_details_support: Some(true),
            }),
        }),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into()]),
            retrigger_characters: Some(vec![")".into()]),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        definition_provider: Some(OneOf::Left(true)),
        declaration_provider: Some(DeclarationCapability::Simple(true)),
        type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
        implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![
                CodeActionKind::QUICKFIX,
                CodeActionKind::REFACTOR_REWRITE,
                CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
            ]),
            work_done_progress_options: WorkDoneProgressOptions::default(),
            resolve_provider: None,
        })),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                work_done_progress_options: WorkDoneProgressOptions::default(),
                legend: crate::semantic_tokens::legend(),
                range: Some(true),
                full: Some(SemanticTokensFullOptions::Bool(true)),
            },
        )),
        inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
            InlayHintOptions {
                work_done_progress_options: WorkDoneProgressOptions::default(),
                resolve_provider: Some(false),
            },
        ))),
        call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: Some(OneOf::Left(true)),
            }),
            file_operations: None,
        }),
        ..Default::default()
    }
}
