//! Request-local export authority for star imports. Explicit named imports are
//! deliberately unrestricted. Unknown __all__ never proves a name unavailable.
use super::*;
use rustpy_syntax::exports::Exports;
use std::collections::HashSet;

impl Inferer<'_> {
    /// Select the latest executed definition preceding this expression. A later
    /// import cannot authorize an earlier expression, and ordinary definitions
    /// retain their shadowing authority over imported names.
    pub(crate) fn export_import_binding(
        &self,
        file: &FileInfo,
        name: &str,
        at: TextSize,
    ) -> Option<BindingId> {
        let id = file
            .scopes
            .bindings_in(file.scopes.module_scope(), name)
            .iter()
            .rev()
            .copied()
            .find(|id| {
                let binding = file.scopes.binding(*id);
                binding.range.start() <= at
                    && !binding.value.is_some_and(|value| value.contains(at))
                    && !matches!(binding.kind, BindingKind::Global | BindingKind::Nonlocal)
                    && self.import_fallback_binding_is_live(file, *id)
            })?;
        let binding = file.scopes.binding(id);
        let root = file.parse.syntax_node();
        let node = match root.covering_element(binding.node_range) {
            rustpy_syntax::rowan::NodeOrToken::Node(node) => node,
            rustpy_syntax::rowan::NodeOrToken::Token(token) => token.parent()?,
        };
        // A retained conditional definition is possible, not a proven executed
        // import. Do not borrow an earlier import past that uncertain overwrite.
        if node.ancestors().any(|node| {
            matches!(
                node.kind(),
                rustpy_syntax::SyntaxKind::IF_STMT
                    | rustpy_syntax::SyntaxKind::TRY_STMT
                    | rustpy_syntax::SyntaxKind::FOR_STMT
                    | rustpy_syntax::SyntaxKind::WHILE_STMT
                    | rustpy_syntax::SyntaxKind::WITH_STMT
                    | rustpy_syntax::SyntaxKind::MATCH_STMT
            )
        }) {
            return None;
        }
        Some(id)
    }

    fn export_policy(&self, module: &FileInfo) -> Option<Exports> {
        if let Some(policy) = self.export_cache.borrow().get(&module.id) {
            return policy.clone();
        }
        // Resolve outside the cache borrow: callbacks may inspect other modules.
        let policy = crate::imports::evaluated_exports(self.db, module);
        self.export_cache
            .borrow_mut()
            .insert(module.id, policy.clone());
        policy
    }

    pub(crate) fn star_import_allows(&self, module: &FileInfo, name: &str) -> bool {
        match self.export_policy(module) {
            None => !name.starts_with('_'),
            Some(exports) => !exports.exact || exports.names.iter().any(|n| n == name),
        }
    }

    /// Available names imported by `from module import *`, including bounded
    /// transitive re-exports. Does not enumerate unrelated package files.
    pub(crate) fn star_members(&self, module: &Arc<FileInfo>) -> IndexMap<SmolStr, Type> {
        self.star_members_inner(module, &mut HashSet::new(), &mut 64, 0)
    }

    fn star_members_inner(
        &self,
        module: &Arc<FileInfo>,
        visited: &mut HashSet<FileId>,
        remaining: &mut usize,
        depth: usize,
    ) -> IndexMap<SmolStr, Type> {
        let mut out = IndexMap::new();
        if depth > 8 || *remaining == 0 || !visited.insert(module.id) {
            return out;
        }
        *remaining -= 1;
        let scope = module.scopes.module_scope();
        for name in module.scopes.scope(scope).bindings.keys().take(20_000) {
            if !self.star_import_allows(module, name) {
                continue;
            }
            if let Some(def) = self.resolve_in_module(module, name, 0) {
                let ty = self.type_of_binding(&def.file, def.binding);
                if !matches!(ty, Type::Never) {
                    out.insert(name.clone(), ty);
                }
            }
        }
        for star in &module.scopes.scope(scope).star_imports {
            if let Some(target) =
                self.db
                    .module_relative(module, star.level, star.module.trim_start_matches('.'))
            {
                for (name, ty) in self.star_members_inner(&target, visited, remaining, depth + 1) {
                    if out.len() >= 20_000 {
                        break;
                    }
                    if self.star_import_allows(module, &name) {
                        out.entry(name).or_insert(ty);
                    }
                }
            }
        }
        if let Some(exports) = self.export_policy(module) {
            for name in exports.names {
                if out.len() >= 20_000 {
                    break;
                }
                if out.contains_key(name.as_str()) {
                    continue;
                }
                if let Some(child) = self.exported_submodule(module, &name) {
                    out.insert(SmolStr::new(name), Type::Module(child.id));
                }
            }
        }
        visited.remove(&module.id);
        out
    }

    fn exported_submodule(&self, module: &Arc<FileInfo>, name: &str) -> Option<Arc<FileInfo>> {
        if !module.is_package_init()
            || !name.chars().enumerate().all(|(i, c)| {
                if i == 0 {
                    rustpy_core::is_ident_start(c)
                } else {
                    rustpy_core::is_ident_continue(c)
                }
            })
            || name.is_empty()
        {
            return None;
        }
        let policy = self.export_policy(module)?;
        if !policy.names.iter().any(|n| n == name) {
            return None;
        }
        self.db
            .module(&format!("{}.{name}", module.module_name.as_deref()?))
    }

    /// Star imports can load a package child named by __all__ without a source
    /// binding. Preserve its real FileInfo identity rather than invent a binding.
    pub(crate) fn star_imported_module(
        &self,
        file: &Arc<FileInfo>,
        scope: ScopeId,
        name: &str,
    ) -> Option<Arc<FileInfo>> {
        if file.scopes.resolve(scope, name).is_some() {
            return None;
        }
        let mut current = Some(scope);
        let mut visited = HashSet::new();
        while let Some(id) = current {
            for star in &file.scopes.scope(id).star_imports {
                let Some(target) =
                    self.db
                        .module_relative(file, star.level, star.module.trim_start_matches('.'))
                else {
                    continue;
                };
                if let Some(found) = self.star_module_inner(&target, name, &mut visited, 0) {
                    return Some(found);
                }
            }
            current = file.scopes.scope(id).parent;
        }
        None
    }

    fn star_module_inner(
        &self,
        module: &Arc<FileInfo>,
        name: &str,
        visited: &mut HashSet<FileId>,
        depth: usize,
    ) -> Option<Arc<FileInfo>> {
        if depth > 8
            || visited.len() >= 64
            || !self.star_import_allows(module, name)
            || !visited.insert(module.id)
        {
            return None;
        }
        if let Some(child) = self.exported_submodule(module, name) {
            return Some(child);
        }
        for star in &module
            .scopes
            .scope(module.scopes.module_scope())
            .star_imports
        {
            if let Some(target) =
                self.db
                    .module_relative(module, star.level, star.module.trim_start_matches('.'))
            {
                if let Some(child) = self.star_module_inner(&target, name, visited, depth + 1) {
                    return Some(child);
                }
            }
        }
        None
    }
}
