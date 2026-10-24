//! Per-file call and inherited-constructor classification. Project traversal
//! and collection belong to rustpy-search.

use std::sync::Arc;

use rustpy_core::TextRange;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;

use crate::db::{Analysis, FileInfo};
use crate::edit_helpers::{node_with_range, token_at};
use crate::infer::Inferer;
use crate::types::{ClassRef, Signature, Type};
use crate::usages;

/// One call of a function.
#[derive(Clone, Debug)]
pub struct CallSite {
    pub file: Arc<FileInfo>,
    pub call: ast::CallExpr,
    /// Leading parameters bound implicitly: 1 for `obj.method(...)`,
    /// `Class.classmethod(...)` and constructor calls `Class(...)`, else 0
    /// (functions, static methods, `Class.method(obj, ...)`).
    pub implicit: usize,
    /// The callee's type is unknown (a method called on an untyped
    /// receiver): it may call another function with the same name.
    pub unresolved: bool,
}

impl CallSite {
    pub fn args(&self) -> Option<ast::ArgList> {
        self.call.arg_list()
    }

    /// 1-based line of the call.
    pub fn line(&self) -> usize {
        let at = usize::from(self.call.syntax().text_range().start()).min(self.file.text.len());
        self.file.text[..at].matches('\n').count() + 1
    }
}

fn is_this(sig: &Signature, file: &FileInfo, func: &ast::FuncDef) -> bool {
    sig.def
        .as_ref()
        .is_some_and(|d| d.file == file.id && d.range == func.syntax().text_range())
}

/// A subclass whose constructor is an inherited `__init__` / `__new__`.
pub struct InheritingClass {
    pub file: Arc<FileInfo>,
    /// The class name token.
    pub name_range: TextRange,
    pub name: String,
}

/// Shared inference context for factual call and constructor classification.
/// A project collector creates one context and retains its private caches
/// across all candidate usages, as before the ownership split.
pub struct CallQuery<'db> {
    inf: Inferer<'db>,
}

impl<'db> CallQuery<'db> {
    pub fn new(db: &'db Analysis) -> Self {
        Self {
            inf: Inferer::new(db),
        }
    }

    /// Whether this class inherits the selected constructor. Candidate loading
    /// and transitive project traversal belong to rustpy-search.
    pub fn inherited_constructor(
        &self,
        db: &Analysis,
        file: &Arc<FileInfo>,
        func: &ast::FuncDef,
        sub: &ClassRef,
    ) -> Option<InheritingClass> {
        let inf = &self.inf;
        // `Sub(...)` passes its arguments to the inherited `__init__` even when
        // `Sub` defines only `__new__`.
        let inherits = inf
            .constructor(sub)
            .as_ref()
            .and_then(|t| t.signature())
            .is_some_and(|s| is_this(s, file, func))
            || (func.name_text() == "__init__"
                && inf
                    .lookup_member(
                        &crate::types::Type::Class(sub.clone(), Vec::new()),
                        "__init__",
                    )
                    .as_ref()
                    .and_then(|t| t.signature())
                    .is_some_and(|s| is_this(s, file, func)));
        let f = db.file(sub.file)?;
        let name = node_with_range(&f.parse.syntax_node(), sub.range)
            .and_then(ast::ClassDef::cast)
            .and_then(|d| d.name())?;
        inherits.then(|| InheritingClass {
            file: f.clone(),
            name_range: name.text_range(),
            name: name.text().to_string(),
        })
    }

    /// Classify one candidate usage as a call of the selected function.
    pub fn call_site(
        &self,
        file: &Arc<FileInfo>,
        func: &ast::FuncDef,
        f: Arc<FileInfo>,
        u: &usages::Usage,
        constructor_names: &[String],
    ) -> Option<CallSite> {
        let inf = &self.inf;
        let is_method = func.is_method();
        let is_static = func.has_decorator("staticmethod");
        let is_classmethod = func.has_decorator("classmethod");
        let tok = token_at(&f.parse.syntax_node(), u.range.start())?;
        let callee = tok
            .parent()
            .filter(|p| matches!(p.kind(), SyntaxKind::NAME_EXPR | SyntaxKind::ATTRIBUTE_EXPR))?;
        if callee.kind() == SyntaxKind::ATTRIBUTE_EXPR
            && ast::AttributeExpr::cast(callee.clone())
                .and_then(|a| a.attr())
                .as_ref()
                != Some(&tok)
        {
            return None;
        }
        let call = callee.parent().and_then(ast::CallExpr::cast)?;
        let callee_expr = call
            .callee()
            .filter(|c| c.syntax().text_range() == callee.text_range())?;
        let constructor =
            constructor_names.iter().any(|n| tok.text() == n) && tok.text() != func.name_text();
        let (implicit, unresolved) = match inf.type_of_expr(&f, &callee_expr) {
            Type::Function(s) if is_this(&s, file, func) => (0, false),
            Type::BoundMethod(s, _) if is_this(&s, file, func) => (1, false),
            Type::Class(c, _) if constructor => {
                match inf.constructor(&c).as_ref().and_then(|t| t.signature()) {
                    Some(s) if is_this(s, file, func) => (1, false),
                    _ => return None,
                }
            }
            Type::Function(_) | Type::BoundMethod(..) | Type::Class(..) => return None,
            Type::Overloaded(v) if !v.iter().any(|s| is_this(s, file, func)) => return None,
            _ => {
                let via_attr = callee.kind() == SyntaxKind::ATTRIBUTE_EXPR;
                (
                    usize::from(
                        constructor || (via_attr && is_method && !is_static) || is_classmethod,
                    ),
                    true,
                )
            }
        };
        Some(CallSite {
            file: f,
            call,
            implicit,
            unresolved,
        })
    }
}
