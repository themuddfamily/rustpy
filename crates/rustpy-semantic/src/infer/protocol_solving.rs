//! TypeVar solving through structural protocol parameters.
use super::*;
use std::cell::Cell;

thread_local! {
    /// Nested protocol solves (`Iterable[T]` → `Iterator[T]` → ...).
    static PROTOCOL_SOLVE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

impl Inferer<'_> {
    /// Solves the type variables of a protocol parameter (`SupportsAbs[T]`,
    /// `Iterable[T]`, user protocols) from the argument's own members when the
    /// argument does not inherit the protocol nominally. Every protocol member
    /// must be present on the argument; otherwise nothing is bound.
    pub(super) fn unify_protocol_members(
        &self,
        param: &Type,
        arg: &Type,
        subst: &mut FxHashMap<SmolStr, Type>,
    ) {
        let Type::Instance(protocol, args) = param else {
            return;
        };
        if args.is_empty() || !self.canonical_protocol(protocol) {
            return;
        }
        let depth = PROTOCOL_SOLVE_DEPTH.with(Cell::get);
        if depth >= 8 {
            return;
        }
        let Some(names) = self.protocol_member_names(protocol) else {
            return;
        };
        let mut pairs = Vec::with_capacity(names.len());
        for name in &names {
            let (Some(expected), Some(actual)) = (
                self.lookup_member(param, name),
                self.lookup_member(arg, name),
            ) else {
                return;
            };
            pairs.push((expected, actual));
        }
        PROTOCOL_SOLVE_DEPTH.with(|cell| cell.set(depth + 1));
        for (expected, actual) in &pairs {
            match (expected, actual) {
                (
                    Type::Function(_) | Type::BoundMethod(..) | Type::Callable(_),
                    Type::Overloaded(signatures),
                ) => {
                    // Overloaded members: the first overload's return decides.
                    if let (Some(expected), Some(actual)) =
                        (callable_signature(expected), signatures.first())
                    {
                        self.unify(&expected.ret, &actual.ret, subst);
                    }
                }
                (
                    Type::Function(_) | Type::BoundMethod(..) | Type::Callable(_),
                    Type::Function(_) | Type::BoundMethod(..) | Type::Callable(_),
                ) => {
                    // Bound members: `self` is already stripped on both sides.
                    if let (Some(expected), Some(actual)) =
                        (callable_signature(expected), callable_signature(actual))
                    {
                        for (expected, actual) in expected.params.iter().zip(&actual.params) {
                            self.unify(&expected.ty, &actual.ty, subst);
                        }
                        self.unify(&expected.ret, &actual.ret, subst);
                    }
                }
                _ => self.unify(expected, actual, subst),
            }
        }
        PROTOCOL_SOLVE_DEPTH.with(|cell| cell.set(depth));
    }
}

fn callable_signature(ty: &Type) -> Option<&Arc<Signature>> {
    match ty {
        Type::Function(signature) | Type::BoundMethod(signature, _) | Type::Callable(signature) => {
            Some(signature)
        }
        _ => None,
    }
}
