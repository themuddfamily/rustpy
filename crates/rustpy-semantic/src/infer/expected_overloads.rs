//! Overload context is permitted only after ordinary arguments prove a winner.
//! Expected return values never choose or reorder an overload candidate.
use super::*;
impl Inferer<'_> {
    pub(super) fn unique_context_overload(
        &self,
        signatures: &[Arc<Signature>],
        positional: &[Type],
        keywords: &[(SmolStr, Type)],
    ) -> Option<Arc<Signature>> {
        if signatures.is_empty() || signatures.len() > 32 || positional.len() + keywords.len() > 32
        {
            return None;
        }
        let selected = self.select_overload(signatures, positional, keywords);
        // select_overload has a last-candidate fallback on invalid calls. That
        // fallback is never authoritative for a contextual expected return.
        if !self.arguments_fit(&selected, positional, keywords) {
            return None;
        }
        if signatures
            .iter()
            .filter(|signature| self.arguments_fit(signature, positional, keywords))
            .take(2)
            .count()
            != 1
        {
            return None;
        }
        Some(selected)
    }
    pub(super) fn parent_context_call_signature(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
        callee: &Type,
    ) -> Option<Arc<Signature>> {
        if let Type::Overloaded(signatures) = callee {
            let arguments: Vec<_> = call.arg_list()?.args().take(33).collect();
            if arguments.len() > 32
                || arguments.iter().any(|argument| {
                    !matches!(
                        argument,
                        ast::AnyArg::Positional(_) | ast::AnyArg::Keyword(_)
                    )
                })
            {
                return None;
            }
            // Disable automatic parent adaptation in the probe. Inferring the
            // queried argument while searching its signature must not reenter.
            let probe = Inferer::new(self.db);
            probe.expected_context.active.set(true);
            let (pos, kw) = probe.call_args(file, call);
            self.unique_context_overload(signatures, &pos, &kw)
        } else {
            self.context_signature(callee)
        }
    }
}
