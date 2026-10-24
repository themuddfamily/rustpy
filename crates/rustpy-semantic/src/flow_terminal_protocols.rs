//! Captured standard protocol contracts; never invokes inference or user bodies.
use super::*;
#[derive(Clone, Copy)]
pub(in crate::flow) enum BuiltinProtocol {
    Len,
    Iter,
    Next,
}
#[derive(Clone, Copy, Default)]
pub(in crate::flow) struct ComparisonContract {
    pub invocation_terminal: bool,
    pub truth_terminal: bool,
}
#[derive(Clone, Copy, Default)]
pub(in crate::flow) struct UnpackContract {
    pub acquisition_terminal: bool,
    pub next_terminal: bool,
}
impl Walker<'_> {
    // A bare builtin has no lexical/runtime replacement; imports must be the
    // captured bundled module. Class LOAD_NAME through unknown mappings refuses.
    fn protocol_builtin_name(&self, expression: &ast::Expr, state: &State) -> Option<String> {
        let (name, member) = match expression.clone().unparen() {
            ast::Expr::Name(name) => (name.name_text(), None),
            ast::Expr::Attribute(attribute) => {
                let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                    return None;
                };
                (name.name_text(), Some(attribute.attr_text()))
            }
            _ => return None,
        };
        if self.shadowed.iter().any(|set| set.contains(name.as_str()))
            || self.is_class && self.class_namespace_unknown
        {
            return None;
        }
        let ids = if let Some(reaches) = self.reaching_name(&name, state) {
            if reaches.is_empty()
                || reaches.len() > 16
                || reaches
                    .iter()
                    .any(|reach| !reach.preds.is_empty() || reach.binding.is_none())
            {
                return None;
            }
            Some(
                reaches
                    .iter()
                    .filter_map(|reach| reach.binding)
                    .collect::<Vec<_>>(),
            )
        } else {
            self.file
                .scopes
                .resolve(self.scope, &name)
                .map(<[BindingId]>::to_vec)
        };
        let Some(ids) = ids else {
            let mut current = Some(self.scope);
            while let Some(scope) = current {
                let info = self.file.scopes.scope(scope);
                if !info.star_imports.is_empty() {
                    return None;
                }
                current = info.parent;
            }
            return (member.is_none()
                && matches!(name.as_str(), "len" | "iter" | "next")
                && !self.authority.retired_members.contains(name.as_str()))
            .then_some(name);
        };
        if ids.is_empty() || ids.len() > 16 {
            return None;
        }
        let mut symbol = None;
        for id in ids {
            let binding = self.file.scopes.binding(id);
            let BindingKind::Import(import) = &binding.kind else {
                return None;
            };
            let target = match (member.as_deref(), import.name.as_deref()) {
                (Some(member), None) | (None, Some(member)) => member,
                _ => return None,
            };
            if import.level != 0
                || import.module != "builtins"
                || !self.authority.modules.contains("builtins")
                || !matches!(target, "len" | "iter" | "next")
                || self.authority.retired_members.contains(target)
            {
                return None;
            }
            if symbol.as_deref().is_some_and(|old| old != target) {
                return None;
            }
            symbol = Some(target.to_owned());
        }
        symbol
    }
    pub(in crate::flow) fn protocol_builtin_call(
        &self,
        call: &ast::CallExpr,
        state: &State,
    ) -> Option<(BuiltinProtocol, Vec<ast::Expr>)> {
        let name = self.protocol_builtin_name(&call.callee()?, state)?;
        let arguments: Vec<_> = call
            .arg_list()
            .into_iter()
            .flat_map(|args| args.args())
            .take(3)
            .collect();
        if arguments.is_empty()
            || arguments.len() > 2
            || arguments
                .iter()
                .any(|arg| !matches!(arg, ast::AnyArg::Positional(_)))
        {
            return None;
        }
        let kind = match name.as_str() {
            "len" if arguments.len() == 1 => BuiltinProtocol::Len,
            "iter" if arguments.len() == 1 => BuiltinProtocol::Iter,
            "next" => BuiltinProtocol::Next,
            _ => return None,
        };
        Some((
            kind,
            arguments
                .into_iter()
                .map(|argument| argument.value())
                .collect::<Option<Vec<_>>>()?,
        ))
    }
    pub(in crate::flow) fn protocol_builtin_terminal(
        &self,
        kind: BuiltinProtocol,
        receiver: &ast::Expr,
        state: &State,
        has_default: bool,
    ) -> bool {
        let slot = match kind {
            BuiltinProtocol::Len => "__len__",
            BuiltinProtocol::Iter => "__iter__",
            BuiltinProtocol::Next if !has_default => "__next__",
            BuiltinProtocol::Next => return false,
        };
        self.implicit_slot_terminal(receiver, slot, state)
    }
    fn protocol_receiver_mro<'a>(
        &'a self,
        expression: &ast::Expr,
        state: &State,
    ) -> Option<Vec<KnownClass<'a>>> {
        let receiver = match expression.clone().unparen() {
            ast::Expr::Named(named) => ast::Expr::Name(named.target()?),
            value => value,
        };
        self.implicit_receiver_mro(&receiver, state)
    }
    fn protocol_method<'a>(
        &self,
        mro: &[KnownClass<'a>],
        slot: &str,
        arity: usize,
    ) -> Option<(&'a FileInfo, BindingId, ast::FuncDef)> {
        if self.authority.retired_members.contains(slot) {
            return None;
        }
        let owner = mro.iter().find(|owner| {
            !owner.intrinsic && !owner.file.scopes.bindings_in(owner.scope, slot).is_empty()
        })?;
        let ids = owner.file.scopes.bindings_in(owner.scope, slot);
        if ids.len() != 1 {
            return None;
        }
        let binding = owner.file.scopes.binding(ids[0]);
        if !matches!(binding.kind, BindingKind::Function) {
            return None;
        }
        let function = node_at(
            &owner.file.parse.syntax_node(),
            binding.node_range,
            SyntaxKind::FUNC_DEF,
        )
        .and_then(ast::FuncDef::cast)?;
        let params: Vec<_> = function
            .params()
            .into_iter()
            .flat_map(|params| params.params())
            .take(5)
            .collect();
        if params.len() != arity
            || params
                .iter()
                .any(|parameter| !matches!(parameter, ast::AnyParam::Plain(_)))
            || function.decorators().next().is_some()
            || function.is_async()
            || function.syntax().descendants().any(|node| {
                matches!(
                    node.kind(),
                    SyntaxKind::YIELD_EXPR | SyntaxKind::YIELD_FROM_EXPR
                ) && node
                    .ancestors()
                    .find(|owner| {
                        matches!(
                            owner.kind(),
                            SyntaxKind::FUNC_DEF | SyntaxKind::LAMBDA_EXPR | SyntaxKind::CLASS_DEF
                        )
                    })
                    .is_some_and(|owner| owner.text_range() == function.text_range())
            })
        {
            return None;
        }
        Some((owner.file, ids[0], function))
    }
    fn protocol_method_never(&self, mro: &[KnownClass<'_>], slot: &str, arity: usize) -> bool {
        self.protocol_method(mro, slot, arity)
            .is_some_and(|(file, id, _)| {
                self.contract(file, id, None, &mut FxHashSet::default())
                    .is_some_and(|contract| matches!(contract, Contract::Sync))
            })
    }
    fn protocol_result_mro<'a>(
        &'a self,
        mro: &[KnownClass<'a>],
        slot: &str,
        arity: usize,
    ) -> Option<Vec<KnownClass<'a>>> {
        let (file, id, function) = self.protocol_method(mro, slot, arity)?;
        let (result_file, class) = self.annotated_receiver_class(
            file,
            file.scopes.binding(id).scope,
            function.return_annotation()?,
            0,
        )?;
        let result = self.class_mro(result_file, class, &mut FxHashSet::default(), &mut 0)?;
        if result.iter().any(|class| {
            !class.intrinsic
                && !class
                    .file
                    .scopes
                    .bindings_in(class.scope, "__new__")
                    .is_empty()
        }) {
            return None;
        }
        Some(result)
    }
    fn protocol_result_truth(&self, mro: &[KnownClass<'_>]) -> bool {
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
        self.protocol_method_never(mro, slot, 1)
    }
    pub(in crate::flow) fn protocol_comparison(
        &self,
        left: &ast::Expr,
        right: &ast::Expr,
        operator: &str,
        left_state: &State,
        right_state: &State,
    ) -> ComparisonContract {
        let empty = ComparisonContract::default();
        if matches!(operator, "is" | "is not") {
            return empty;
        }
        let (receiver, slot, state, contains) = match operator {
            "in" | "not in" => (right, "__contains__", right_state, true),
            "<" | "<=" | ">" | ">=" | "==" | "!=" => {
                // A literal right operand cannot be a strict subtype of a
                // captured user class. Other reflected dispatch remains unknown.
                if !matches!(right.clone().unparen(), ast::Expr::Literal(_)) {
                    return empty;
                }
                let slot = match operator {
                    "<" => "__lt__",
                    "<=" => "__le__",
                    ">" => "__gt__",
                    ">=" => "__ge__",
                    "==" => "__eq__",
                    _ => "__ne__",
                };
                (left, slot, left_state, false)
            }
            _ => return empty,
        };
        let Some(mro) = self.protocol_receiver_mro(receiver, state) else {
            return empty;
        };
        if self.protocol_method_never(&mro, slot, 2) {
            return ComparisonContract {
                invocation_terminal: true,
                truth_terminal: false,
            };
        }
        let truth_terminal = self
            .protocol_result_mro(&mro, slot, 2)
            .is_some_and(|result| self.protocol_result_truth(&result));
        ComparisonContract {
            invocation_terminal: contains && truth_terminal,
            truth_terminal: !contains && truth_terminal,
        }
    }
    pub(in crate::flow) fn protocol_async_acquisition(
        &self,
        receiver: &ast::Expr,
        state: &State,
    ) -> bool {
        self.protocol_receiver_mro(receiver, state)
            .is_some_and(|mro| self.protocol_method_never(&mro, "__aiter__", 1))
    }
    pub(in crate::flow) fn protocol_unpack_contract(
        &self,
        receiver: &ast::Expr,
        state: &State,
    ) -> UnpackContract {
        let Some(mro) = self.protocol_receiver_mro(receiver, state) else {
            return UnpackContract::default();
        };
        if mro.iter().any(|class| {
            !class.intrinsic
                && !class
                    .file
                    .scopes
                    .bindings_in(class.scope, "__iter__")
                    .is_empty()
        }) {
            if self.protocol_method_never(&mro, "__iter__", 1) {
                return UnpackContract {
                    acquisition_terminal: true,
                    next_terminal: false,
                };
            }
            return UnpackContract {
                acquisition_terminal: false,
                next_terminal: self
                    .protocol_result_mro(&mro, "__iter__", 1)
                    .is_some_and(|iterator| self.protocol_method_never(&iterator, "__next__", 1)),
            };
        }
        UnpackContract {
            acquisition_terminal: false,
            next_terminal: self.protocol_method_never(&mro, "__getitem__", 2),
        }
    }
    pub(in crate::flow) fn protocol_unpack_terminal(
        &self,
        contract: UnpackContract,
        pattern: &ast::Expr,
    ) -> bool {
        let required = match pattern.clone().unparen() {
            ast::Expr::Tuple(tuple) => tuple
                .elements()
                .any(|element| !matches!(element.unparen(), ast::Expr::Star(_))),
            ast::Expr::List(list) => list
                .elements()
                .any(|element| !matches!(element.unparen(), ast::Expr::Star(_))),
            _ => return false,
        };
        contract.acquisition_terminal || required && contract.next_terminal
    }
}
