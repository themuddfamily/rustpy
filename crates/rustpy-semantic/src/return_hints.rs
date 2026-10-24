//! Inferred return annotations using the same signatures as hover/completion.
use crate::{
    features::{InlayHint, InlayKind},
    infer::Inferer,
    types::Type,
    Analysis, FileInfo,
};
use rustpy_core::{CancellationToken, Cancelled};
use rustpy_syntax::ast::{AstNode, FuncDef};
use std::sync::Arc;

pub const MAX_HINT_SOURCE_BYTES: usize = 1024 * 1024;

pub fn return_hints(
    db: &Analysis,
    file: &Arc<FileInfo>,
    token: &CancellationToken,
) -> Result<Vec<InlayHint>, Cancelled> {
    token.check()?;
    if file.is_stub || file.text.len() > MAX_HINT_SOURCE_BYTES {
        return Ok(Vec::new());
    }
    let inferer = Inferer::new(db);
    let mut hints = Vec::new();
    for function in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(FuncDef::cast)
    {
        token.check()?;
        if function.return_annotation().is_some() {
            continue;
        }
        let Some(params) = function.params() else {
            continue;
        };
        if !params
            .syntax()
            .last_token()
            .is_some_and(|token| token.text() == ")")
        {
            continue;
        }
        let signature = inferer.signature(file, &function, None);
        let mut ty = &signature.ret;
        // A displayed annotation describes the awaited result, rather than the
        // Coroutine wrapper callers receive. Generator types remain generators.
        if function.is_async() {
            if let Type::Instance(class, arguments) = ty {
                if class.name == "Coroutine" && arguments.len() == 3 {
                    ty = &arguments[2];
                }
            }
        }
        if ty.is_unknown() || matches!(ty, Type::Any) {
            continue;
        }
        let label = ty.to_string();
        if label.len() > 120 || label.contains("Unknown") {
            continue;
        }
        hints.push(InlayHint {
            offset: params.syntax().text_range().end(),
            label: format!(" -> {label}"),
            kind: InlayKind::Type,
        });
    }
    token.check()?;
    hints.sort_by_key(|hint| hint.offset);
    Ok(hints)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    #[test]
    fn return_hints_use_actual_inference_and_correct_signature_offsets() {
        let db = Analysis::for_project(Path::new("/proj"));
        let text = "def π(value: int):\n    return value + 1\ndef annotated() -> str:\n    return 'ok'\ndef unknown(value):\n    return value\n";
        let file = db
            .file(db.set_file_text(Path::new("/proj/main.py"), text))
            .unwrap();
        let hints = return_hints(&db, &file, &CancellationToken::new()).unwrap();
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].label, " -> int");
        assert_eq!(&text[usize::from(hints[0].offset)..][..1], ":");
        assert_eq!(&text[..usize::from(hints[0].offset)], "def π(value: int)");
    }
    #[test]
    fn nested_returns_do_not_contaminate_parent_and_bare_return_is_none() {
        let db = Analysis::for_project(Path::new("/proj"));
        let text = "def outer():\n    def inner():\n        return 'nested'\n    return 1\ndef empty():\n    return\n";
        let file = db
            .file(db.set_file_text(Path::new("/proj/main.py"), text))
            .unwrap();
        let hints = return_hints(&db, &file, &CancellationToken::new()).unwrap();
        assert_eq!(
            hints
                .iter()
                .map(|hint| hint.label.as_str())
                .collect::<Vec<_>>(),
            [" -> int", " -> str", " -> None"]
        );
    }
    #[test]
    fn cancellation_and_large_source_publish_no_partial_hints() {
        let db = Analysis::for_project(Path::new("/proj"));
        let file = db
            .file(db.set_file_text(Path::new("/proj/main.py"), "def f():\n    return 1\n"))
            .unwrap();
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(return_hints(&db, &file, &token), Err(Cancelled));
        let text = format!(
            "#{}\ndef f():\n    return 1\n",
            "x".repeat(MAX_HINT_SOURCE_BYTES)
        );
        let large = db
            .file(db.set_file_text(Path::new("/proj/large.py"), &text))
            .unwrap();
        assert!(return_hints(&db, &large, &CancellationToken::new())
            .unwrap()
            .is_empty());
    }
    #[test]
    fn async_return_hint_describes_awaited_result_not_coroutine_wrapper() {
        let db = Analysis::for_project(Path::new("/proj"));
        let text = "async def answer():\n    return 41\n";
        let file = db
            .file(db.set_file_text(Path::new("/proj/main.py"), text))
            .unwrap();
        let hints = return_hints(&db, &file, &CancellationToken::new()).unwrap();
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].label, " -> int");
    }
}
