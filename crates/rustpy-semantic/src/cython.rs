//! Cython declaration adapter. Keeps original byte ranges and resolves .pxd files
//! through the existing Analysis resolver; compilation uses the installed Cython tool.
use crate::{
    db::{Analysis, FileInfo},
    features::{CompletionItem, CompletionKind, HoverInfo, Location},
};
use rustpy_core::{Diagnostic, TextRange, TextSize};
use std::{path::Path, sync::Arc};
#[derive(Clone, Debug)]
pub struct Declaration {
    pub name: String,
    pub ty: String,
    pub range: TextRange,
    pub kind: CompletionKind,
    pub indent: usize,
}
pub fn supports(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| matches!(extension.to_str(), Some("pyx" | "pxd" | "pxi")))
}
pub fn declarations(source: &str) -> Vec<Declaration> {
    if source.len() > 2 * 1024 * 1024 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut offset = 0;
    for segment in source.split_inclusive('\n').take(20000) {
        let line = segment.trim_end_matches(['\n', '\r']);
        let indent = line.len() - line.trim_start().len();
        let body = line.trim_start();
        let marker = ["cdef ", "cpdef ", "ctypedef ", "def ", "class "]
            .into_iter()
            .find(|prefix| body.starts_with(prefix));
        if let Some(marker) = marker {
            let declaration = &body[marker.len()..];
            let declaration = declaration
                .trim_start_matches("public ")
                .trim_start_matches("readonly ")
                .trim_start_matches("extern ");
            let head = declaration
                .split(['(', ':', '='])
                .next()
                .unwrap_or("")
                .trim();
            let class = head.strip_prefix("class ");
            let name = class
                .unwrap_or(head)
                .split_whitespace()
                .last()
                .unwrap_or("")
                .trim_matches(['*', '&']);
            if name.chars().next().is_some_and(rustpy_core::is_ident_start)
                && name.chars().all(rustpy_core::is_ident_continue)
            {
                if let Some(at) = line.find(name) {
                    let kind = if class.is_some() || marker == "class " {
                        CompletionKind::Class
                    } else if declaration.contains('(') {
                        CompletionKind::Function
                    } else {
                        CompletionKind::Variable
                    };
                    let ty = head.strip_suffix(name).unwrap_or("").trim().to_owned();
                    out.push(Declaration {
                        name: name.into(),
                        ty,
                        range: TextRange::new(
                            TextSize::new((offset + at) as u32),
                            TextSize::new((offset + at + name.len()) as u32),
                        ),
                        kind,
                        indent,
                    });
                }
            }
        }
        offset += segment.len();
    }
    out
}
fn word(source: &str, offset: TextSize) -> Option<(&str, usize)> {
    let offset = usize::from(offset).min(source.len());
    let start = source[..offset]
        .rfind(|c: char| !rustpy_core::is_ident_continue(c))
        .map_or(0, |at| {
            at + source[at..].chars().next().map_or(0, char::len_utf8)
        });
    let end = source[offset..]
        .find(|c: char| !rustpy_core::is_ident_continue(c))
        .map_or(source.len(), |at| offset + at);
    (start < end).then_some((&source[start..end], start))
}
fn imports(db: &Analysis, file: &Arc<FileInfo>) -> Vec<Arc<FileInfo>> {
    let mut files = Vec::new();
    for line in file.text.lines().take(20000) {
        let line = line.trim();
        let module = if let Some(value) = line.strip_prefix("from ") {
            value.split_once(" cimport ").map(|(module, _)| module)
        } else {
            line.strip_prefix("cimport ")
                .map(|value| value.split_whitespace().next().unwrap_or(""))
        };
        let Some(module) = module else { continue };
        if !module
            .split('.')
            .filter(|part| !part.is_empty())
            .all(|part| part.chars().all(rustpy_core::is_ident_continue))
        {
            continue;
        }
        for root in db
            .resolver
            .roots
            .iter()
            .chain(db.resolver.search_paths.iter())
        {
            let path = root
                .join(module.trim_start_matches('.').replace('.', "/"))
                .with_extension("pxd");
            if db.resolver.is_excluded(&path) {
                continue;
            }
            if let Some(imported) = db.file_for_path(&path) {
                files.push(imported);
                break;
            }
        }
        if files.len() >= 64 {
            break;
        }
    }
    files
}
pub fn complete(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Option<crate::basic_completion::BasicCompletion> {
    if !supports(&file.path) {
        return None;
    }
    let end = usize::from(offset).min(file.text.len());
    let start = file.text[..end]
        .rfind(|c: char| !rustpy_core::is_ident_continue(c))
        .map_or(0, |at| {
            at + file.text[at..].chars().next().map_or(0, char::len_utf8)
        });
    let prefix = &file.text[start..end];
    let mut declarations = declarations(&file.text);
    for imported in imports(db, file) {
        declarations.extend(self::declarations(&imported.text));
    }
    let mut items = declarations
        .into_iter()
        .map(|declaration| CompletionItem {
            label: declaration.name.clone().into(),
            insert: declaration.name,
            detail: format!("Cython {}", declaration.ty),
            kind: declaration.kind,
            caret_offset: None,
            priority: 0,
        })
        .collect::<Vec<_>>();
    for keyword in [
        "cdef", "cpdef", "ctypedef", "cimport", "public", "readonly", "extern", "nogil", "gil",
        "except", "fused", "struct", "union", "enum", "bint", "int", "double", "float", "char",
        "long", "unsigned", "void", "sizeof",
    ] {
        items.push(CompletionItem {
            label: keyword.into(),
            insert: keyword.into(),
            detail: "Cython".into(),
            kind: CompletionKind::Keyword,
            caret_offset: None,
            priority: 20,
        });
    }
    items.retain(|item| crate::features::matches_prefix(&item.label, &prefix.to_lowercase()));
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.dedup_by(|a, b| a.label == b.label);
    let count = items.len();
    Some(crate::basic_completion::BasicCompletion {
        items,
        imports: vec![None; count],
        prefix_start: TextSize::new(start as u32),
        replace_segment: false,
        import_classes: false,
    })
}
pub fn definition(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Location> {
    if !supports(&file.path) {
        return Vec::new();
    }
    let Some((name, _)) = word(&file.text, offset) else {
        return Vec::new();
    };
    let mut files = vec![file.clone()];
    files.extend(imports(db, file));
    for source in files {
        if let Some(declaration) = declarations(&source.text)
            .into_iter()
            .filter(|declaration| declaration.name == name)
            .min_by_key(|declaration| {
                usize::from(declaration.range.start()).abs_diff(usize::from(offset))
            })
        {
            return vec![Location {
                path: source.path.clone(),
                range: declaration.range,
                is_stub: source
                    .path
                    .extension()
                    .is_some_and(|extension| extension == "pxd"),
            }];
        }
    }
    Vec::new()
}
pub fn hover(file: &Arc<FileInfo>, offset: TextSize) -> Option<HoverInfo> {
    if !supports(&file.path) {
        return None;
    }
    let (name, _) = word(&file.text, offset)?;
    let declaration = declarations(&file.text)
        .into_iter()
        .find(|declaration| declaration.name == name)?;
    Some(HoverInfo {
        signature: format!(
            "{}: {}",
            declaration.name,
            if declaration.ty.is_empty() {
                "Cython declaration"
            } else {
                &declaration.ty
            }
        ),
        kind: Some("Cython declaration"),
        ..Default::default()
    })
}
pub fn diagnostics(source: &str) -> Vec<Diagnostic> {
    let mut issues = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n').take(20000) {
        let text = line.trim();
        if matches!(text, "cdef" | "cpdef" | "ctypedef")
            || text.starts_with("cdef class ") && !text.ends_with(':')
        {
            issues.push(Diagnostic::error(
                TextRange::new(
                    TextSize::new(offset as u32),
                    TextSize::new((offset + line.trim_end().len()) as u32),
                ),
                "CythonSyntax",
                "Incomplete Cython declaration",
            ));
        }
        offset += line.len();
    }
    issues
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn declarations_preserve_types_and_unicode_offsets() {
        let source="# π\ncdef class Point:\n    cdef double x\ncpdef double distance(Point point):\n    return point.x\n";
        let declarations = declarations(source);
        assert_eq!(declarations.len(), 3);
        assert_eq!(declarations[1].ty, "double");
        for declaration in declarations {
            assert_eq!(
                &source
                    [usize::from(declaration.range.start())..usize::from(declaration.range.end())],
                declaration.name
            );
        }
    }
}
