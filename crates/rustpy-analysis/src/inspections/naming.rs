//! PEP 8 naming conventions (`PyPep8Naming`) and missing docstrings
//! (`PyMissingOrEmptyDocstring`).

use std::sync::Arc;

use rustpy_core::{Severity, TextRange};
use rustpy_semantic::{
    infer::Inferer,
    types::{ClassRef, Type},
};
use rustpy_settings::style_inspections::StyleInspections;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;

use super::{body_insert_point, ts, QuickFix, Sink};
use rustpy_semantic::db::FileInfo;
use rustpy_semantic::scope::{BindingId, BindingKind, ScopeKind};

const NAMING: &str = "PyPep8Naming";
const DOCSTRING: &str = "PyMissingOrEmptyDocstring";

fn has_upper(s: &str) -> bool {
    s.chars().any(|c| c.is_uppercase())
}

fn is_dunder(s: &str) -> bool {
    s.starts_with("__") && s.ends_with("__")
}

/// Methods of classes with explicit bases may override inherited names
/// (`setUp`, `assertEqual`); PEP 8 allows keeping those.
fn may_override(inf: &Inferer<'_>, file: &FileInfo, f: &ast::FuncDef) -> bool {
    if !f.is_method() {
        return false;
    }
    let Some(class) = f.syntax().ancestors().find_map(ast::ClassDef::cast) else {
        return false;
    };
    let class = ClassRef {
        file: file.id,
        range: class.text_range(),
        name: class.name_text().into(),
    };
    inf.mro(&class).iter().skip(1).any(|base| {
        inf.class_info(base)
            .is_some_and(|info| info.members.contains_key(f.name_text().as_str()))
    })
}

pub(super) fn run(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    config: &StyleInspections,
    sink: &mut Sink,
) {
    let root = file.parse.syntax_node();
    for node in root.descendants() {
        match node.kind() {
            SyntaxKind::FUNC_DEF => {
                let Some(f) = ast::FuncDef::cast(node.clone()) else {
                    continue;
                };
                let Some(name_tok) = f.name() else { continue };
                let name = name_tok.text();
                if has_upper(name)
                    && !(config.ignore_overrides && may_override(inf, file, &f))
                    && !is_dunder(name)
                    && !super::style::ignored_name(config, name)
                {
                    sink.push(
                        name_tok.text_range(),
                        Severity::WeakWarning,
                        NAMING,
                        "Function name should be lowercase",
                        Vec::new(),
                    );
                }
                if let Some(params) = f.params() {
                    for p in params.params() {
                        if let Some(t) = p.name() {
                            if has_upper(t.text()) && !super::style::ignored_name(config, t.text())
                            {
                                sink.push(
                                    t.text_range(),
                                    Severity::WeakWarning,
                                    NAMING,
                                    "Argument name should be lowercase",
                                    Vec::new(),
                                );
                            }
                        }
                    }
                }
            }
            SyntaxKind::CLASS_DEF => {
                let Some(c) = ast::ClassDef::cast(node.clone()) else {
                    continue;
                };
                let Some(name_tok) = c.name() else { continue };
                let name = name_tok.text();
                let core = name.trim_matches('_');
                let bad =
                    core.chars().next().is_some_and(|ch| ch.is_lowercase()) || core.contains('_');
                if bad && !core.is_empty() && !super::style::ignored_name(config, name) {
                    sink.push(
                        name_tok.text_range(),
                        Severity::WeakWarning,
                        NAMING,
                        "Class names should use CapWords convention",
                        Vec::new(),
                    );
                }
            }
            _ => {}
        }
    }
    if config.check_constants {
        for (name, ids) in &file.scopes.scope(file.scopes.module_scope()).bindings {
            if ids.len() != 1
                || !name.chars().any(|c| c.is_lowercase())
                || super::style::ignored_name(config, name)
                || name.starts_with('_')
            {
                continue;
            }
            let binding = file.scopes.binding(ids[0]);
            if binding.kind != BindingKind::Assignment {
                continue;
            }
            let Some(range) = binding.value else { continue };
            let Some(value) = rustpy_semantic::edit_helpers::node_with_range(&root, range)
                .and_then(ast::Expr::cast)
            else {
                continue;
            };
            if matches!(value, ast::Expr::Literal(_) | ast::Expr::String(_))
                && !file
                    .scopes
                    .references
                    .iter()
                    .any(|reference| reference.name == *name && reference.is_write)
            {
                sink.push(
                    binding.range,
                    Severity::WeakWarning,
                    NAMING,
                    "Module constant name should use UPPER_CASE",
                    Vec::new(),
                );
            }
        }
    }
    // Variables in functions.
    for (index, b) in file.scopes.bindings.iter().enumerate() {
        if !matches!(
            b.kind,
            BindingKind::Assignment
                | BindingKind::AugAssignment
                | BindingKind::Annotation
                | BindingKind::For
                | BindingKind::With
                | BindingKind::Except
                | BindingKind::Walrus
                | BindingKind::MatchCapture
                | BindingKind::CompTarget
        ) {
            continue;
        }
        if !matches!(
            file.scopes.scope(b.scope).kind,
            ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Comprehension
        ) || !has_upper(&b.name)
            || is_dunder(&b.name)
            || super::style::ignored_name(config, &b.name)
        {
            continue;
        }
        // Type aliases retain class-style spelling; an instance constructed by
        // an uppercase callee is still an ordinary variable.
        if matches!(
            inf.type_of_binding(file, BindingId(index as u32)),
            Type::Class(..) | Type::TypeVar(_) | Type::DeclaredTypeVar(_)
        ) {
            continue;
        }
        sink.push(
            b.range,
            Severity::WeakWarning,
            NAMING,
            "Variable in function should be lowercase",
            Vec::new(),
        );
    }
}

/// Missing docstrings for public modules, classes and functions.
pub(super) fn docstrings(file: &Arc<FileInfo>, config: &StyleInspections, sink: &mut Sink) {
    let text: &str = &file.text;
    let root = file.parse.syntax_node();
    let Some(module) = ast::Module::cast(root.clone()) else {
        return;
    };
    let public_module = !file
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.starts_with('_') && s != "__init__");
    if config.doc_modules
        && (public_module || config.doc_private)
        && module
            .docstring()
            .is_none_or(|doc| doc.value().trim().is_empty())
        && module.statements().next().is_some()
    {
        let insertion = module.statements().next().map_or(0, |statement| {
            super::line_start(text, usize::from(statement.text_range().start()))
        });
        let fix = QuickFix {
            title: "Insert docstring stub".into(),
            edits: vec![(
                TextRange::empty(ts(insertion)),
                "\"\"\"TODO: describe this module.\"\"\"\n".into(),
            )],
        };
        sink.push(
            TextRange::empty(ts(0)),
            Severity::WeakWarning,
            DOCSTRING,
            "Missing docstring",
            vec![fix],
        );
    }
    for node in root.descendants() {
        let (name_tok, body, what) = match node.kind() {
            SyntaxKind::FUNC_DEF => {
                let Some(f) = ast::FuncDef::cast(node.clone()) else {
                    continue;
                };
                let Some(t) = f.name() else { continue };
                if (f.is_method() && !config.doc_methods)
                    || (!f.is_method() && !config.doc_functions)
                {
                    continue;
                }
                if f.docstring()
                    .is_some_and(|doc| !doc.value().trim().is_empty())
                    || f.has_decorator("overload")
                    || f.has_decorator("typing.overload")
                {
                    continue;
                }
                if (!config.doc_private && t.text().starts_with('_'))
                    || t.text().starts_with("test")
                {
                    continue;
                }
                // Nested functions are not public API.
                if !config.doc_private
                    && node
                        .ancestors()
                        .skip(1)
                        .any(|a| a.kind() == SyntaxKind::FUNC_DEF)
                {
                    continue;
                }
                (t, f.body(), "function")
            }
            SyntaxKind::CLASS_DEF => {
                let Some(c) = ast::ClassDef::cast(node.clone()) else {
                    continue;
                };
                let Some(t) = c.name() else { continue };
                if !config.doc_classes
                    || c.docstring()
                        .is_some_and(|doc| !doc.value().trim().is_empty())
                    || (!config.doc_private && t.text().starts_with('_'))
                {
                    continue;
                }
                (t, c.body(), "class")
            }
            _ => continue,
        };
        let mut fixes = Vec::new();
        if let Some((at, indent)) = body.as_ref().and_then(|b| body_insert_point(text, b)) {
            let stub = format!(
                "{indent}\"\"\"TODO: describe {} {}.\"\"\"\n",
                what,
                name_tok.text()
            );
            // The stub must come first: insert at the body start (before any statement).
            let body_start: usize = body
                .as_ref()
                .and_then(|b| b.statements().next())
                .map(|s| s.text_range().start().into())
                .unwrap_or(at);
            let at = super::line_start(text, body_start.min(at.max(body_start)));
            fixes.push(QuickFix {
                title: "Insert docstring stub".into(),
                edits: vec![(TextRange::empty(ts(at)), stub)],
            });
        }
        sink.push(
            name_tok.text_range(),
            Severity::WeakWarning,
            DOCSTRING,
            "Missing docstring",
            fixes,
        );
    }
}
