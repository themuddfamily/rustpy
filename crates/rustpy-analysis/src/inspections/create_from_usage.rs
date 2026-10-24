//! "Create from usage" quick fixes for unresolved references: create
//! function / class / parameter / local variable, and `self.x` attributes
//! or methods for unresolved attributes inside a class.

use std::sync::Arc;

use rustpy_core::{Diagnostic, TextRange};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};

use super::{
    body_insert_point, enclosing_stmt, indent_at, line_end_incl, line_start, token_at, ts, QuickFix,
};
use rustpy_semantic::db::FileInfo;
use rustpy_semantic::scope::ScopeKind;

/// Quick fixes for one `PyUnresolvedReferences` diagnostic.
pub(super) fn fixes_for(file: &Arc<FileInfo>, d: &Diagnostic) -> Vec<QuickFix> {
    let text: &str = &file.text;
    let root = file.parse.syntax_node();
    let Some(tok) = token_at(&root, d.range) else {
        return Vec::new();
    };
    let Some(parent) = tok.parent() else {
        return Vec::new();
    };
    if let Some(name) = d
        .message
        .strip_prefix("Unresolved reference '")
        .and_then(|m| m.strip_suffix('\''))
    {
        if parent.kind() == SyntaxKind::NAME_EXPR && is_identifier(name) {
            return name_fixes(file, text, &parent, name);
        }
        return Vec::new();
    }
    if let Some(rest) = d.message.strip_prefix("Unresolved attribute reference '") {
        let name = rest.split('\'').next().unwrap_or("");
        if parent.kind() == SyntaxKind::ATTRIBUTE_EXPR && is_identifier(name) {
            return attribute_fixes(text, &parent, name);
        }
    }
    Vec::new()
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(rustpy_core::is_ident_start)
        && chars.all(rustpy_core::is_ident_continue)
}

fn name_fixes(
    file: &Arc<FileInfo>,
    text: &str,
    name_node: &SyntaxNode,
    name: &str,
) -> Vec<QuickFix> {
    let mut fixes = Vec::new();
    let call = name_node
        .parent()
        .and_then(ast::CallExpr::cast)
        .filter(|c| c.callee().is_some_and(|e| e.syntax() == name_node));
    if let Some(call) = call {
        let params = param_names_from_call(&call);
        let is_class = name.chars().next().is_some_and(|c| c.is_uppercase());
        let src = if is_class {
            class_source(name, &params)
        } else {
            function_source(name, &params)
        };
        if let Some(edit) = top_level_insertion(text, name_node, &src) {
            let title = if is_class {
                format!("Create class '{name}'")
            } else {
                format!("Create function '{name}'")
            };
            fixes.push(QuickFix {
                title,
                edits: vec![edit],
            });
        }
        return fixes;
    }
    // Not a call: a parameter (inside a function) and a variable.
    let scope = super::scope_of_node(file, name_node);
    let mut cur = Some(scope);
    let mut func_scope = None;
    while let Some(s) = cur {
        let sc = file.scopes.scope(s);
        match sc.kind {
            ScopeKind::Function => {
                func_scope = Some(sc.range);
                break;
            }
            ScopeKind::Comprehension | ScopeKind::Lambda | ScopeKind::TypeParams => cur = sc.parent,
            _ => break,
        }
    }
    if let Some(range) = func_scope {
        if let Some(func) =
            rustpy_semantic::edit_helpers::node_with_range(&file.parse.syntax_node(), range)
                .and_then(ast::FuncDef::cast)
        {
            if let Some(edit) = create_parameter(&func, name) {
                fixes.push(QuickFix {
                    title: format!("Create parameter '{name}'"),
                    edits: vec![edit],
                });
            }
        }
        if let Some(edit) = create_variable(text, name_node, name) {
            fixes.push(QuickFix {
                title: format!("Create local variable '{name}'"),
                edits: vec![edit],
            });
        }
    } else if let Some(edit) = create_variable(text, name_node, name) {
        fixes.push(QuickFix {
            title: format!("Create variable '{name}'"),
            edits: vec![edit],
        });
    }
    fixes
}

/// Parameter names derived from the call arguments (`f(x, y=1, "s")` → `x, y, arg3`).
fn param_names_from_call(call: &ast::CallExpr) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(args) = call.arg_list() else {
        return out;
    };
    for (i, a) in args.args().enumerate() {
        let base = match &a {
            ast::AnyArg::Positional(p) => match p.expr().map(|e| e.unparen()) {
                Some(ast::Expr::Name(n)) => n.name_text(),
                Some(ast::Expr::Attribute(at)) => at.attr_text(),
                Some(ast::Expr::Call(c)) => match c.callee() {
                    Some(ast::Expr::Name(n)) => n.name_text().to_lowercase(),
                    Some(ast::Expr::Attribute(at)) => at.attr_text(),
                    _ => String::new(),
                },
                _ => String::new(),
            },
            ast::AnyArg::Keyword(k) => k.name_text(),
            ast::AnyArg::Star(_) => "*args".to_string(),
            ast::AnyArg::StarStar(_) => "**kwargs".to_string(),
            ast::AnyArg::Generator(_) => String::new(),
        };
        let mut name = if base.is_empty()
            || !is_identifier(base.trim_start_matches('*'))
            || matches!(base.as_str(), "self" | "cls")
        {
            format!("arg{}", i + 1)
        } else {
            base
        };
        if out.contains(&name) {
            name = format!("{name}{}", i + 1);
        }
        out.push(name);
    }
    out
}

fn function_source(name: &str, params: &[String]) -> String {
    format!("def {name}({}):\n    pass\n", params.join(", "))
}

fn class_source(name: &str, params: &[String]) -> String {
    let fields: Vec<&String> = params.iter().filter(|p| !p.starts_with('*')).collect();
    if fields.is_empty() {
        return format!("class {name}:\n    pass\n");
    }
    let mut s = format!(
        "class {name}:\n    def __init__(self, {}):\n",
        params.join(", ")
    );
    for f in fields {
        s.push_str(&format!("        self.{f} = {f}\n"));
    }
    s
}

/// Inserts a top-level definition near the top-level statement containing
/// `node`: after it when that statement is a `def`/`class`, before it
/// otherwise (so module-level code sees the definition), separated by two
/// blank lines.
fn top_level_insertion(text: &str, node: &SyntaxNode, src: &str) -> Option<(TextRange, String)> {
    let top = node.ancestors().find(|n| {
        n.parent().is_some_and(|p| p.kind() == SyntaxKind::MODULE) && ast::Stmt::can_cast(n.kind())
    })?;
    let after = matches!(top.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF);
    let at = if after {
        let end: usize = top.text_range().end().into();
        line_end_incl(
            text,
            end.saturating_sub(1)
                .max(usize::from(top.text_range().start())),
        )
    } else {
        line_start(text, usize::from(top.text_range().start()))
    };
    Some((TextRange::empty(ts(at)), with_blank_lines(text, at, src)))
}

/// `src` padded so that two blank lines separate it from non-blank neighbours.
pub(super) fn with_blank_lines(text: &str, at: usize, src: &str) -> String {
    let before = &text[..at];
    let after = &text[at..];
    let mut out = String::new();
    if !before.trim().is_empty() {
        if !before.ends_with('\n') {
            out.push('\n');
        }
        let trailing_nl = before.chars().rev().take_while(|c| *c == '\n').count();
        let blank = trailing_nl.saturating_sub(1);
        for _ in blank..2 {
            out.push('\n');
        }
    }
    out.push_str(src);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if !after.trim().is_empty() {
        let leading_nl = after.chars().take_while(|c| *c == '\n').count();
        for _ in leading_nl..2 {
            out.push('\n');
        }
    }
    out
}

/// Appends parameter `name` to `func`'s parameter list.
pub(super) fn create_parameter(func: &ast::FuncDef, name: &str) -> Option<(TextRange, String)> {
    let params = func.params()?;
    let plain: Vec<ast::Param> = params.plain_params().collect();
    if let Some(last) = plain.last() {
        let kw_only = params
            .syntax()
            .children()
            .take_while(|c| c.text_range().start() < last.text_range().start())
            .any(|c| {
                matches!(
                    c.kind(),
                    SyntaxKind::STAR_PARAM | SyntaxKind::STAR_SEPARATOR
                )
            });
        let needs_default = last.default().is_some() && !kw_only;
        let at = last.text_range().end();
        return Some((
            TextRange::empty(at),
            format!(", {name}{}", if needs_default { "=None" } else { "" }),
        ));
    }
    let open = params
        .syntax()
        .first_token()
        .filter(|t| t.kind() == SyntaxKind::L_PAREN)?;
    let has_star = params.params().any(|p| {
        matches!(
            p,
            ast::AnyParam::Star(_) | ast::AnyParam::StarStar(_) | ast::AnyParam::StarSep(_)
        )
    });
    Some((
        TextRange::empty(open.text_range().end()),
        if has_star {
            format!("{name}, ")
        } else {
            name.to_string()
        },
    ))
}

/// `name = None` before the statement containing the usage.
fn create_variable(text: &str, name_node: &SyntaxNode, name: &str) -> Option<(TextRange, String)> {
    let stmt = enclosing_stmt(name_node)?;
    let start: usize = stmt.text_range().start().into();
    let ls = line_start(text, start);
    if !text[ls..start].trim().is_empty() {
        return None;
    }
    let indent = indent_at(text, start);
    Some((TextRange::empty(ts(ls)), format!("{indent}{name} = None\n")))
}

fn attribute_fixes(text: &str, attr_node: &SyntaxNode, name: &str) -> Vec<QuickFix> {
    let mut fixes = Vec::new();
    let Some(attr) = ast::AttributeExpr::cast(attr_node.clone()) else {
        return fixes;
    };
    let is_self = matches!(attr.object().map(|o| o.unparen()), Some(ast::Expr::Name(n)) if n.name_text() == "self");
    if !is_self {
        return fixes;
    }
    let Some(class_node) = attr_node
        .ancestors()
        .find(|n| n.kind() == SyntaxKind::CLASS_DEF)
    else {
        return fixes;
    };
    let Some(class) = ast::ClassDef::cast(class_node.clone()) else {
        return fixes;
    };
    let method = attr_node
        .ancestors()
        .take_while(|n| n != &class_node)
        .filter_map(ast::FuncDef::cast)
        .find(|f| f.is_method());
    let call = attr_node
        .parent()
        .and_then(ast::CallExpr::cast)
        .filter(|c| c.callee().is_some_and(|e| e.syntax() == attr_node));
    if let Some(call) = call {
        let params = param_names_from_call(&call);
        let sig = if params.is_empty() {
            "self".to_string()
        } else {
            format!("self, {}", params.join(", "))
        };
        let (at, indent) = match &method {
            Some(m) => {
                let end: usize = m.text_range().end().into();
                (
                    line_end_incl(text, end.saturating_sub(1)),
                    indent_at(text, usize::from(m.text_range().start())),
                )
            }
            None => {
                let end: usize = class_node.text_range().end().into();
                let Some(body) = class.body() else {
                    return fixes;
                };
                let Some((_, indent)) = body_insert_point(text, &body) else {
                    return fixes;
                };
                (line_end_incl(text, end.saturating_sub(1)), indent)
            }
        };
        let lead = if text[..at].ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let src = format!("{lead}{indent}def {name}({sig}):\n{indent}    pass\n");
        fixes.push(QuickFix {
            title: format!("Create method '{name}'"),
            edits: vec![(TextRange::empty(ts(at)), src)],
        });
        return fixes;
    }
    // Attribute: define it in __init__.
    let Some(body) = class.body() else {
        return fixes;
    };
    match class.methods().find(|m| m.name_text() == "__init__") {
        Some(init) => {
            let Some(ib) = init.body() else { return fixes };
            let Some((_, indent)) = body_insert_point(text, &ib) else {
                return fixes;
            };
            let Some(last) = ib.statements().last() else {
                return fixes;
            };
            let edit = match &last {
                ast::Stmt::Pass(_) => (last.text_range(), format!("self.{name} = None")),
                _ => {
                    let end: usize = last.text_range().end().into();
                    let at = line_end_incl(text, end.saturating_sub(1));
                    let lead = if text[..at].ends_with('\n') { "" } else { "\n" };
                    (
                        TextRange::empty(ts(at)),
                        format!("{lead}{indent}self.{name} = None\n"),
                    )
                }
            };
            fixes.push(QuickFix {
                title: format!("Create attribute 'self.{name}' in __init__"),
                edits: vec![edit],
            });
        }
        None => {
            let Some((at, indent)) = body_insert_point(text, &body) else {
                return fixes;
            };
            let src = format!("{indent}def __init__(self):\n{indent}    self.{name} = None\n\n");
            fixes.push(QuickFix {
                title: format!("Create attribute 'self.{name}' in __init__"),
                edits: vec![(TextRange::empty(ts(at)), src)],
            });
        }
    }
    fixes
}
