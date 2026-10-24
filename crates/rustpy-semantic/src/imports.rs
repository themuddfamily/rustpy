//! Import usage facts shared by inspections and import formatting.

use rustpy_core::TextRange;

use crate::db::{Analysis, FileInfo};

mod export_resolver;
use crate::scope::BindingKind;

/// One unused binding candidate, before inspection suppression policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnusedImport {
    pub range: TextRange,
    pub node_range: TextRange,
    pub name: String,
    pub module: String,
    pub is_from: bool,
}

/// Known names exported by the shared bounded __all__ evaluator.
pub fn dunder_all(file: &FileInfo) -> Vec<String> {
    rustpy_syntax::exports::evaluate(&file.parse.syntax_node())
        .map(|exports| exports.names)
        .unwrap_or_default()
}

/// Module-level import bindings with no reads or export use.
/// Suppression comments are inspection policy and are deliberately not applied here.
pub fn unused_import_candidates(file: &FileInfo) -> Vec<UnusedImport> {
    unused_import_candidates_with_policy(
        file,
        rustpy_syntax::exports::evaluate(&file.parse.syntax_node()),
    )
}

/// Current, bounded cross-module export authority; never executes source code.
pub fn evaluated_exports(
    db: &Analysis,
    file: &FileInfo,
) -> Option<rustpy_syntax::exports::Exports> {
    export_resolver::evaluate(db, file)
}

/// Known exported names using current imported module snapshots.
pub fn dunder_all_with(db: &Analysis, file: &FileInfo) -> Vec<String> {
    evaluated_exports(db, file)
        .map(|exports| exports.names)
        .unwrap_or_default()
}

/// Db-aware facts for inspections and immutable candidate import optimization.
pub fn unused_import_candidates_with(db: &Analysis, file: &FileInfo) -> Vec<UnusedImport> {
    unused_import_candidates_with_policy(file, evaluated_exports(db, file))
}

fn unused_import_candidates_with_policy(
    file: &FileInfo,
    policy: Option<rustpy_syntax::exports::Exports>,
) -> Vec<UnusedImport> {
    // An unknown export list might use any import. Never optimize away a
    // potentially exported binding on the strength of incomplete authority.
    if policy.as_ref().is_some_and(|exports| !exports.exact) {
        return Vec::new();
    }
    let exported = policy.map(|exports| exports.names).unwrap_or_default();
    let module_scope = file.scopes.module_scope();
    // Names that are read anywhere through a module-level import binding. A
    // reference resolves to one binding only, but `import xml.dom` and
    // `import xml.sax` both bind `xml` (and try/except imports rebind a name),
    // so every import of a used name counts as used.
    let mut used_names: std::collections::HashSet<&str> = file
        .scopes
        .references
        .iter()
        .filter_map(|r| r.resolved)
        .map(|id| file.scopes.binding(id))
        .filter(|b| b.scope == module_scope && matches!(b.kind, BindingKind::Import(_)))
        .map(|b| b.name.as_str())
        .collect();
    // Forward references in string annotations (`x: "Decimal"`).
    for tok in file
        .parse
        .syntax_node()
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        if tok.kind() == rustpy_syntax::SyntaxKind::STRING
            && tok.parent_ancestors().any(|a| {
                matches!(
                    a.kind(),
                    rustpy_syntax::SyntaxKind::ANNOTATION
                        | rustpy_syntax::SyntaxKind::RETURN_ANNOTATION
                )
            })
        {
            let t = tok.text();
            for word in t
                .split(|c: char| !rustpy_core::is_ident_continue(c))
                .filter(|w| !w.is_empty())
            {
                if let Some(b) = file
                    .scopes
                    .bindings
                    .iter()
                    .find(|b| b.name.as_str() == word && b.scope == module_scope)
                {
                    used_names.insert(b.name.as_str());
                }
            }
        }
    }
    // Type comments and structured docstring type fields are maintained type
    // consumers too. Optimizing imports must not erase their dependencies.
    let mut external_types = Vec::new();
    use rustpy_syntax::ast::AstNode;
    for function in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(rustpy_syntax::ast::FuncDef::cast)
    {
        if let Some(doc) = function.docstring() {
            let parsed = crate::docstrings::parse(&doc.value());
            external_types.extend(parsed.params.into_iter().filter_map(|(_, ty, _)| ty));
            if let Some((Some(ty), _)) = parsed.returns {
                external_types.push(ty);
            }
        }
    }
    for token in file
        .parse
        .syntax_node()
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        if token.kind() != rustpy_syntax::SyntaxKind::COMMENT {
            continue;
        }
        if let Some(value) = token
            .text()
            .trim_start_matches('#')
            .trim_start()
            .strip_prefix("type:")
        {
            let value = value.trim();
            if value != "ignore" && !value.starts_with("ignore[") {
                external_types.push(value.to_owned());
            }
        }
    }
    let external_names: std::collections::HashSet<String> = external_types
        .iter()
        .flat_map(|ty| ty.split(|c: char| !rustpy_core::is_ident_continue(c)))
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect();
    for binding in &file.scopes.bindings {
        if binding.scope == module_scope && external_names.contains(binding.name.as_str()) {
            used_names.insert(binding.name.as_str());
        }
    }
    let mut out = Vec::new();
    for (i, b) in file.scopes.bindings.iter().enumerate() {
        let BindingKind::Import(info) = &b.kind else {
            continue;
        };
        if b.scope != module_scope || info.module == "__future__" {
            continue;
        }
        // Re-export conventions: `import x as x`, `from m import y as y`.
        if info.alias.as_deref() == Some(b.name.as_str()) {
            continue;
        }
        if exported.iter().any(|e| e == b.name.as_str()) {
            continue;
        }
        let id = crate::scope::BindingId(i as u32);
        if used_names.contains(b.name.as_str())
            || file
                .scopes
                .references
                .iter()
                .any(|r| r.resolved == Some(id))
        {
            continue;
        }
        out.push(UnusedImport {
            range: b.range,
            node_range: b.node_range,
            name: b.name.to_string(),
            module: info.module.to_string(),
            is_from: info.name.is_some(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Analysis;
    use std::path::Path;

    #[test]
    fn factual_candidates_do_not_apply_inspection_suppressions() {
        let src = "import os  # noqa: F401\n# noinspection PyUnresolvedReferences\nfrom decimal import Decimal\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let candidates = unused_import_candidates(&file);
        assert_eq!(
            candidates
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>(),
            vec!["os", "Decimal"]
        );
        assert_eq!(candidates[0].module, "os");
        assert!(!candidates[0].is_from);
        assert_eq!(candidates[1].module, "decimal");
        assert!(candidates[1].is_from);
        for candidate in candidates {
            assert_eq!(
                &src[usize::from(candidate.range.start())..usize::from(candidate.range.end())],
                candidate.name
            );
        }
    }

    #[test]
    fn factual_candidates_preserve_exports_and_all_read_forms() {
        let src = "from __future__ import annotations\nimport os as os\nimport hidden as renamed\nfrom typing import List as List\nimport json\nimport xml.dom\nimport xml.sax\nfrom decimal import Decimal\nimport sys\nimport unused\n__all__ = ['json']\nx: 'Decimal'\nxml.dom\nsys.argv\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        assert_eq!(dunder_all(&file), vec!["json"]);
        assert_eq!(
            unused_import_candidates(&file)
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>(),
            vec!["unused"]
        );
    }

    #[test]
    fn factual_candidates_leave_function_imports_to_their_scope() {
        let src = "import unused\ndef f():\n    import os\n    return 1\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        assert_eq!(
            unused_import_candidates(&file)
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>(),
            vec!["unused"]
        );
    }
}
