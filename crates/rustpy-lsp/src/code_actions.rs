//! Code actions: what the editor's Alt+Enter popup offers that can be
//! expressed as text edits — quick fixes of the problems at the caret (Add
//! import, create from usage, PEP 8, spelling, …), intentions (invert `if`,
//! convert quotes, comprehension ↔ loop, …), Insert documentation string stub
//! and Optimize imports (`source.organizeImports`).

use std::sync::Arc;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
};
use rustpy_analysis::inspections;
use rustpy_core::TextRange;

use crate::diagnostics::{self, FileProblems};
use crate::snapshot::{DocView, HResult, Snapshot};

fn allowed(only: &Option<Vec<CodeActionKind>>, kind: &CodeActionKind) -> bool {
    match only {
        None => true,
        Some(kinds) => kinds.iter().any(|k| {
            kind.as_str() == k.as_str() || kind.as_str().starts_with(&format!("{}.", k.as_str()))
        }),
    }
}

/// The inspection results for `doc`: the ones computed for diagnostics when
/// they are current, otherwise a fresh run.
fn problems_for(snap: &Snapshot, doc: &DocView) -> HResult<Arc<FileProblems>> {
    snap.check_cancelled()?;
    if let (Some((v, p)), Some(version)) = (snap.problems.get(&doc.path), doc.version) {
        if *v == version {
            return Ok(p.clone());
        }
    }
    Ok(Arc::new(diagnostics::problems(snap, doc)?))
}

pub fn code_action(
    snap: &Snapshot,
    params: CodeActionParams,
) -> HResult<Option<CodeActionResponse>> {
    let doc = snap.doc(&params.text_document.uri)?;
    let range = doc.index.text_range(params.range, snap.enc);
    let only = &params.context.only;
    let mut actions: Vec<CodeAction> = Vec::new();
    let single = |title: String,
                  kind: CodeActionKind,
                  edits: Vec<(TextRange, String)>,
                  diags: Option<Vec<lsp_types::Diagnostic>>|
     -> HResult<CodeAction> {
        let edit = crate::handlers::workspace_edit(snap, vec![(doc.path.clone(), edits)])?;
        Ok(CodeAction {
            title,
            kind: Some(kind),
            diagnostics: diags,
            edit: Some(edit),
            ..Default::default()
        })
    };
    let disabled = &snap.config.settings.disabled_intentions;
    // Quick fixes: problems under the caret win, otherwise those on its line.
    if allowed(only, &CodeActionKind::QUICKFIX) {
        let problems = problems_for(snap, &doc)?;
        let touches =
            |r: TextRange| r.intersect(range).is_some() || r.contains_inclusive(range.start());
        let at_caret: Vec<&diagnostics::Problem> = problems
            .iter()
            .filter(|p| touches(p.diagnostic.range))
            .collect();
        let picked: Vec<&diagnostics::Problem> = if at_caret.iter().any(|p| !p.fixes.is_empty()) {
            at_caret
        } else {
            let line = doc.index.line_of(range.start());
            problems
                .iter()
                .filter(|p| doc.index.line_of(p.diagnostic.range.start()) == line)
                .collect()
        };
        let mut seen: Vec<String> = Vec::new();
        for p in picked {
            let diag = diagnostics::to_lsp(&p.diagnostic, &doc, snap);
            for (i, fix) in p.fixes.iter().enumerate() {
                if fix.edits.is_empty()
                    || seen.contains(&fix.title)
                    || disabled.contains(&fix.title)
                {
                    continue;
                }
                seen.push(fix.title.clone());
                let mut action = single(
                    fix.title.clone(),
                    CodeActionKind::QUICKFIX,
                    fix.edits.clone(),
                    Some(vec![diag.clone()]),
                )?;
                // The first import suggestion is what Alt+Enter would apply.
                action.is_preferred = (i == 0 && fix.title.starts_with("Import ")).then_some(true);
                actions.push(action);
            }
        }
    }
    snap.check_cancelled()?;
    if allowed(only, &CodeActionKind::REFACTOR_REWRITE) {
        let at = range.start();
        let format = snap.config.docstring_format(doc.text());
        if let Some((r, text)) =
            rustpy_semantic::docstrings::generate(&snap.analysis, &doc.file, at, format)
        {
            let title = "Insert documentation string stub".to_string();
            if !disabled.contains(&title) {
                actions.push(single(
                    title,
                    CodeActionKind::REFACTOR_REWRITE,
                    vec![(r, text)],
                    None,
                )?);
            }
        }
        for it in rustpy_analysis::intentions::intentions_at(&snap.analysis, &doc.file, at) {
            if it.edits.is_empty()
                || disabled.contains(&it.title)
                || actions.iter().any(|a| a.title == it.title)
            {
                continue;
            }
            actions.push(single(
                it.title,
                CodeActionKind::REFACTOR_REWRITE,
                it.edits,
                None,
            )?);
        }
    }
    if allowed(only, &CodeActionKind::SOURCE_ORGANIZE_IMPORTS) {
        let edits = rustpy_format::imports::optimize_imports(
            &doc.file,
            &inspections::unused_import_ranges(&doc.file),
        );
        if !edits.is_empty() {
            actions.push(single(
                "Optimize imports".into(),
                CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
                edits,
                None,
            )?);
        }
    }
    Ok(Some(
        actions
            .into_iter()
            .map(CodeActionOrCommand::CodeAction)
            .collect(),
    ))
}
