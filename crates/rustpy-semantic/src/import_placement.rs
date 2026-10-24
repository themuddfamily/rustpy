//! Import placement shared by completion, inspections and refactorings.

use crate::db::FileInfo;
use crate::edit_helpers::ts;
use rustpy_core::TextSize;
use rustpy_syntax::ast::{self, AstNode};

/// Offset where a new import statement should be inserted: after the last
/// top-level import (or after the module docstring / `from __future__`).
pub fn import_insert_offset(file: &FileInfo) -> TextSize {
    let root = file.parse.syntax_node();
    let Some(module) = ast::Module::cast(root) else {
        return ts(0);
    };
    let mut last_import_end: Option<TextSize> = None;
    let mut after_docstring: Option<TextSize> = None;
    for (i, stmt) in module.statements().enumerate() {
        match &stmt {
            ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_) => {
                last_import_end = Some(stmt.text_range().end())
            }
            ast::Stmt::Expr(e) if i == 0 && matches!(e.expr(), Some(ast::Expr::String(_))) => {
                after_docstring = Some(stmt.text_range().end())
            }
            _ => {
                if last_import_end.is_some() {
                    break;
                }
            }
        }
    }
    let end = last_import_end.or(after_docstring);
    match end {
        Some(e) => {
            // Start of the next line.
            let text: &str = &file.text;
            let off: usize = e.into();
            let next = text[off..]
                .find('\n')
                .map(|i| off + i + 1)
                .unwrap_or(text.len());
            ts(next)
        }
        None => ts(0),
    }
}
