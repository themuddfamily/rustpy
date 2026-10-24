//! Bounded import-binding authority for cross-module export expressions.
use crate::scope::{BindingId, BindingKind, ImportInfo};
use crate::{Analysis, FileInfo, Inferer};
use rustpy_core::{FileId, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::exports::{self, Exports, ResolvedExportValue};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

type Key = (FileId, String);
struct Resolver<'a> {
    db: &'a Analysis,
    infer: Inferer<'a>,
    sources: HashMap<FileId, (PathBuf, Arc<str>)>,
    active: HashSet<Key>,
    modules_active: HashSet<(FileId, BindingId)>,
    cache: HashMap<Key, Option<ResolvedExportValue>>,
    requests: usize,
    retained_bytes: usize,
}

pub(super) fn evaluate(db: &Analysis, file: &FileInfo) -> Option<Exports> {
    let mut resolver = Resolver {
        db,
        infer: Inferer::new(db),
        sources: HashMap::new(),
        active: HashSet::new(),
        modules_active: HashSet::new(),
        cache: HashMap::new(),
        requests: 128,
        retained_bytes: 0,
    };
    match resolver.binding(file, "__all__")? {
        ResolvedExportValue::Sequence { exports, .. } => Some(exports),
        ResolvedExportValue::String { .. } => Some(Exports::default()),
    }
}

impl Resolver<'_> {
    fn source(&mut self, file: &FileInfo) -> bool {
        if let Some((path, text)) = self.sources.get(&file.id) {
            return *path == file.path && Arc::ptr_eq(text, &file.text);
        }
        if self.sources.len() >= 64 {
            return false;
        }
        self.sources
            .insert(file.id, (file.path.clone(), file.text.clone()));
        true
    }

    fn binding(&mut self, file: &FileInfo, name: &str) -> Option<ResolvedExportValue> {
        if !self.source(file) {
            return None;
        }
        let key = (file.id, name.to_owned());
        if self.active.contains(&key) {
            return None;
        }
        if let Some(value) = self.cache.get(&key) {
            return value.clone();
        }
        if self.active.len() >= 8 || self.requests == 0 {
            return None;
        }
        self.requests -= 1;
        self.active.insert(key.clone());
        let mut value =
            exports::evaluate_binding_with(&file.parse.syntax_node(), name, &mut |expr| {
                self.expression(file, expr)
            });
        self.active.remove(&key);
        let size = match &value {
            Some(ResolvedExportValue::String { value, .. }) => Some(value.len()),
            Some(ResolvedExportValue::Sequence { exports, .. }) => exports
                .names
                .iter()
                .try_fold(0usize, |sum, n| sum.checked_add(n.len())),
            None => Some(0),
        };
        if let Some(size) = size.filter(|size| self.retained_bytes.saturating_add(*size) <= 4 << 20)
        {
            self.retained_bytes += size;
        } else {
            value = None;
        }
        self.cache.insert(key, value.clone());
        value
    }

    fn expression(&mut self, file: &FileInfo, expr: &ast::Expr) -> Option<ResolvedExportValue> {
        if self.requests == 0 {
            return None;
        }
        self.requests -= 1;
        match expr {
            ast::Expr::Name(name) => {
                let id = self.infer.export_import_binding(
                    file,
                    &name.name_text(),
                    expr.text_range().start(),
                )?;
                let BindingKind::Import(info) = &file.scopes.binding(id).kind else {
                    return None;
                };
                let name = info.name.as_deref()?;
                let module = self.db.module_relative(
                    file,
                    info.level,
                    info.module.trim_start_matches('.'),
                )?;
                self.binding(&module, name)
            }
            ast::Expr::Attribute(attribute) => {
                let module = self.module_expression(
                    file,
                    &attribute.object()?,
                    expr.text_range().start(),
                    0,
                )?;
                self.binding(&module, &attribute.attr_text())
            }
            _ => None,
        }
    }

    fn module_expression(
        &mut self,
        file: &FileInfo,
        expr: &ast::Expr,
        at: TextSize,
        depth: usize,
    ) -> Option<Arc<FileInfo>> {
        if depth >= 8 || self.requests == 0 {
            return None;
        }
        self.requests -= 1;
        match expr {
            ast::Expr::Name(name) => {
                let id = self
                    .infer
                    .export_import_binding(file, &name.name_text(), at)?;
                self.module_binding(file, id, depth + 1)
            }
            ast::Expr::Attribute(attribute) => {
                let parent = self.module_expression(file, &attribute.object()?, at, depth + 1)?;
                if let Some(id) = self.infer.export_import_binding(
                    &parent,
                    &attribute.attr_text(),
                    TextSize::of(&*parent.text),
                ) {
                    return self.module_binding(&parent, id, depth + 1);
                }
                let target = format!(
                    "{}.{}",
                    parent.module_name.as_deref()?,
                    attribute.attr_text()
                );
                // On-disk existence alone does not expose pkg.child.
                if !self.loaded_dotted_module(file, &target, at) {
                    return None;
                }
                self.db.module(&target)
            }
            _ => None,
        }
    }

    fn loaded_dotted_module(&self, file: &FileInfo, target: &str, at: TextSize) -> bool {
        file.scopes
            .bindings
            .iter()
            .enumerate()
            .any(|(index, binding)| {
                let BindingKind::Import(info) = &binding.kind else {
                    return false;
                };
                if binding.scope != file.scopes.module_scope()
                    || info.name.is_some()
                    || info.level != 0
                    || binding.range.start() > at
                {
                    return false;
                }
                let imported = info.module.as_str();
                (imported == target
                    || imported
                        .strip_prefix(target)
                        .is_some_and(|rest| rest.starts_with('.')))
                    && self
                        .infer
                        .export_import_binding(file, &binding.name, binding.range.end())
                        == Some(BindingId(index as u32))
            })
    }

    fn module_binding(
        &mut self,
        file: &FileInfo,
        id: BindingId,
        depth: usize,
    ) -> Option<Arc<FileInfo>> {
        if depth >= 8
            || self.requests == 0
            || !self.source(file)
            || !self.modules_active.insert((file.id, id))
        {
            return None;
        }
        self.requests -= 1;
        let result = match &file.scopes.binding(id).kind {
            BindingKind::Import(info) => self.imported_module(file, info, depth + 1),
            _ => None,
        };
        self.modules_active.remove(&(file.id, id));
        result
    }

    fn imported_module(
        &mut self,
        file: &FileInfo,
        info: &ImportInfo,
        depth: usize,
    ) -> Option<Arc<FileInfo>> {
        let module_name = info.module.trim_start_matches('.');
        let Some(name) = &info.name else {
            // `import pkg.child` binds pkg; an explicit alias binds pkg.child.
            let target = if info.alias.is_some() {
                module_name
            } else {
                module_name.split('.').next()?
            };
            return self.db.module_relative(file, info.level, target);
        };
        let parent = self.db.module_relative(file, info.level, module_name)?;
        if let Some(id) =
            self.infer
                .export_import_binding(&parent, name, TextSize::of(&*parent.text))
        {
            let same_import = parent.id == file.id
                && matches!(
                    &parent.scopes.binding(id).kind,
                    BindingKind::Import(existing) if std::ptr::eq(existing, info)
                );
            if !same_import {
                return self.module_binding(&parent, id, depth + 1);
            }
        }
        let target = if module_name.is_empty() {
            name.to_string()
        } else {
            format!("{module_name}.{name}")
        };
        self.db.module_relative(file, info.level, &target)
    }
}
