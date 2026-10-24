//! Pure declared implicit-slot proofs; execution order is owned by Flow.
use super::*;
impl Walker<'_> {
    pub(in crate::flow) fn implicit_slot_terminal(
        &self,
        receiver: &ast::Expr,
        slot: &str,
        state: &State,
    ) -> bool {
        let receiver = receiver.clone().unparen();
        let receiver = match receiver {
            ast::Expr::Named(named) => named.target().map(ast::Expr::Name),
            expression => Some(expression),
        };
        let Some(receiver) = receiver else {
            return false;
        };
        let Some(mro) = self.implicit_receiver_mro(&receiver, state) else {
            return false;
        };
        let Some(owner) = mro.iter().find(|class| {
            !class.intrinsic && !class.file.scopes.bindings_in(class.scope, slot).is_empty()
        }) else {
            return false;
        };
        let ids = owner.file.scopes.bindings_in(owner.scope, slot);
        if ids.len() != 1 || self.authority.retired_members.contains(slot) {
            return false;
        }
        let binding = owner.file.scopes.binding(ids[0]);
        let Some(function) = node_at(
            &owner.file.parse.syntax_node(),
            binding.node_range,
            SyntaxKind::FUNC_DEF,
        )
        .and_then(ast::FuncDef::cast) else {
            return false;
        };
        let arity = match slot {
            "__setitem__" | "__setattr__" => 3,
            "__getitem__" | "__delitem__" | "__delattr__" | "__iadd__" | "__isub__"
            | "__imul__" | "__itruediv__" | "__ifloordiv__" | "__imod__" | "__ipow__"
            | "__imatmul__" | "__iand__" | "__ior__" | "__ixor__" | "__ilshift__"
            | "__irshift__" => 2,
            _ => 1,
        };
        let params: Vec<_> = function
            .params()
            .into_iter()
            .flat_map(|params| params.params())
            .take(5)
            .collect();
        if params.len() != arity
            || params
                .iter()
                .any(|param| !matches!(param, ast::AnyParam::Plain(_)))
            || function.is_async()
            || function.decorators().next().is_some()
        {
            return false;
        }
        self.receiver_method_contract(
            owner.file,
            match node_at(
                &owner.file.parse.syntax_node(),
                owner.file.scopes.scope(owner.scope).range,
                SyntaxKind::CLASS_DEF,
            )
            .and_then(ast::ClassDef::cast)
            {
                Some(class) => class,
                None => return false,
            },
            slot,
            true,
        )
        .is_some_and(|contract| contract.terminates(false))
    }
    // Named-expression stores have their own binding role. Resolve only the
    // actual current Walrus RHS, never the replaced earlier parameter/value.
    pub(super) fn implicit_receiver_mro<'a>(
        &'a self,
        receiver: &ast::Expr,
        state: &State,
    ) -> Option<Vec<KnownClass<'a>>> {
        if let Some((mro, _)) = self.context_mro(receiver, state) {
            return Some(mro);
        }
        let ast::Expr::Name(name) = receiver else {
            return None;
        };
        if self
            .shadowed
            .iter()
            .any(|names| names.contains(name.name_text().as_str()))
        {
            return None;
        }
        let reaches = self.reaching_name(&name.name_text(), state)?;
        if reaches.is_empty() || reaches.len() > 16 {
            return None;
        }
        let mut result: Option<Vec<KnownClass<'a>>> = None;
        for reach in reaches {
            if !reach.preds.is_empty() {
                return None;
            }
            let binding = self.file.scopes.binding(reach.binding?);
            if !matches!(binding.kind, BindingKind::Walrus) {
                return None;
            }
            let ast::Expr::Call(construction) =
                self.expression_at(self.file, binding.value?)?.unparen()
            else {
                return None;
            };
            if construction
                .arg_list()
                .is_some_and(|args| args.args().next().is_some())
            {
                return None;
            }
            let ast::Expr::Name(class_name) = construction.callee()?.unparen() else {
                return None;
            };
            if self.file.scopes.bindings.len() > 8192 {
                return None;
            }
            if self
                .file
                .scopes
                .bindings
                .iter()
                .filter(|binding| binding.name == class_name.name_text().as_str())
                .any(|binding| !matches!(binding.kind, BindingKind::Class | BindingKind::Import(_)))
            {
                return None;
            }
            let (mro, exact) = self.context_mro(&ast::Expr::Call(construction), state)?;
            if !exact {
                return None;
            }
            if result.as_ref().is_some_and(|old| {
                old.iter()
                    .map(KnownClass::identity)
                    .ne(mro.iter().map(KnownClass::identity))
            }) {
                return None;
            }
            result = Some(mro);
        }
        result
    }

    pub(in crate::flow) fn implicit_truth_terminal(
        &self,
        receiver: &ast::Expr,
        state: &State,
    ) -> bool {
        let receiver = match receiver.clone().unparen() {
            ast::Expr::Named(named) => match named.target() {
                Some(name) => ast::Expr::Name(name),
                None => return false,
            },
            expression => expression,
        };
        let Some(mro) = self.implicit_receiver_mro(&receiver, state) else {
            return false;
        };
        let slot = if mro.iter().any(|class| {
            !class.intrinsic
                && !class
                    .file
                    .scopes
                    .bindings_in(class.scope, "__bool__")
                    .is_empty()
        }) {
            "__bool__"
        } else {
            "__len__"
        };
        self.implicit_slot_terminal(&receiver, slot, state)
    }
    pub(in crate::flow) fn implicit_inplace_terminal(
        &self,
        target: &ast::Expr,
        operator: SyntaxKind,
        captured: &State,
    ) -> bool {
        let slot = match operator {
            SyntaxKind::PLUS_EQ => "__iadd__",
            SyntaxKind::MINUS_EQ => "__isub__",
            SyntaxKind::STAR_EQ => "__imul__",
            SyntaxKind::SLASH_EQ => "__itruediv__",
            SyntaxKind::SLASH_SLASH_EQ => "__ifloordiv__",
            SyntaxKind::PERCENT_EQ => "__imod__",
            SyntaxKind::STAR_STAR_EQ => "__ipow__",
            SyntaxKind::AT_EQ => "__imatmul__",
            SyntaxKind::AMP_EQ => "__iand__",
            SyntaxKind::PIPE_EQ => "__ior__",
            SyntaxKind::CARET_EQ => "__ixor__",
            SyntaxKind::SHL_EQ => "__ilshift__",
            SyntaxKind::SHR_EQ => "__irshift__",
            _ => return false,
        };
        matches!(target.clone().unparen(), ast::Expr::Name(_))
            && self.implicit_slot_terminal(target, slot, captured)
    }
}
