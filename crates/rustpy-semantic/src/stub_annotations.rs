//! Type information supplied by PEP 484 comments and structured docstrings.
//! Explicit Python annotations retain priority; callers use these as fallbacks.
use crate::{
    db::FileInfo,
    infer::Inferer,
    scope::{Binding, ScopeId},
    types::Type,
};
use rustpy_syntax::ast::{self, AstNode};
use std::sync::Arc;

fn type_comment(line: &str) -> Option<&str> {
    if !line.contains("type:") {
        return None;
    }
    let parsed = rustpy_syntax::parse(line);
    let token = parsed
        .syntax_node()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == rustpy_syntax::SyntaxKind::COMMENT)?;
    let comment = line.get(usize::from(token.text_range().start()) + 1..)?;
    let value = comment.trim_start().strip_prefix("type:")?.trim();
    if value.is_empty() || value == "ignore" || value.starts_with("ignore[") {
        None
    } else {
        Some(value.split(" #").next()?.trim())
    }
}
fn split_types(value: &str) -> Option<Vec<&str>> {
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;
    let mut out = Vec::new();
    for (at, ch) in value.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' | '[' | '{' => {
                depth += 1;
                if depth > 32 {
                    return None;
                }
            }
            ')' | ']' | '}' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                out.push(value[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return None;
    }
    out.push(value[start..].trim());
    (out.len() <= 256).then_some(out)
}
impl Inferer<'_> {
    /// Resolve a bounded documentation/type-comment annotation in an explicit
    /// captured lexical scope. This never executes the annotation expression.
    pub fn annotation_text_in_scope(
        &self,
        file: &Arc<FileInfo>,
        scope: ScopeId,
        text: &str,
    ) -> Option<Type> {
        self.stub_text_type(file, scope, text)
    }

    pub fn documentation_type_in_scope(
        &self,
        file: &Arc<FileInfo>,
        scope: ScopeId,
        text: &str,
    ) -> Option<Type> {
        let text = crate::docstrings::type_information::normalize(text)?;
        self.stub_text_type(file, scope, &text)
    }

    fn stub_text_type(&self, file: &Arc<FileInfo>, scope: ScopeId, text: &str) -> Option<Type> {
        if text.len() > 16384 || text.is_empty() || text == "..." {
            return None;
        }
        let text = text.trim().trim_end_matches(", optional").trim();
        let parsed = rustpy_syntax::parse(&format!("__stub_type = {text}\n"));
        if !parsed.errors().is_empty() {
            return None;
        }
        let value = parsed
            .syntax_node()
            .descendants()
            .find_map(ast::AssignStmt::cast)?
            .value()?;
        let ty = self.stub_expression_type(file, scope, value, 0)?;
        (!ty.is_unknown()).then_some(ty)
    }
    fn stub_expression_type(
        &self,
        file: &Arc<FileInfo>,
        scope: ScopeId,
        expression: ast::Expr,
        depth: usize,
    ) -> Option<Type> {
        if depth > 32 {
            return None;
        }
        let expression = expression.unparen();
        let elements: Vec<_> = match &expression {
            ast::Expr::Tuple(tuple) => tuple.elements().take(257).collect(),
            ast::Expr::List(list) => list.elements().take(257).collect(),
            _ => return Some(self.annotation_type_in_scope(file, &expression, scope)),
        };
        if elements.len() > 256 {
            return None;
        }
        let types = elements
            .into_iter()
            .map(|element| self.stub_expression_type(file, scope, element, depth + 1))
            .collect::<Option<Vec<_>>>()?;
        Some(Type::Tuple(types))
    }
    fn function_comment(&self, file: &FileInfo, function: &ast::FuncDef) -> Option<String> {
        let range = function.text_range();
        let start = usize::from(range.start());
        let text = file.text.get(start..usize::from(range.end()))?;
        // Only this function's header and leading body comments participate.
        // An inline body ends the search on its header line, even though that
        // physical line does not end in a colon.
        let first_statement = function
            .body()?
            .statements()
            .next()
            .map(|statement| usize::from(statement.syntax().text_range().start()));
        let mut offset = start;
        for line in text.split_inclusive('\n').take(64) {
            let line_end = offset + line.len();
            if let Some(comment) = type_comment(line) {
                if comment.starts_with('(') && comment.contains("->") {
                    return Some(comment.to_owned());
                }
            }
            if first_statement.is_some_and(|at| at < line_end) {
                break;
            }
            offset = line_end;
        }
        None
    }
    pub(crate) fn stub_parameter_type(
        &self,
        file: &Arc<FileInfo>,
        function: &ast::FuncDef,
        name: &str,
    ) -> Option<Type> {
        let scope = self.scope_of(file, function.syntax());
        let params = function.params()?;
        let named: Vec<_> = params
            .params()
            .filter_map(|p| p.name().map(|n| n.text().to_owned()))
            .collect();
        let index = named.iter().position(|n| n == name)?;
        if let Some(comment) = self.function_comment(file, function) {
            let (arguments, _) = comment.rsplit_once("->")?;
            let arguments = arguments.trim().strip_prefix('(')?.strip_suffix(')')?;
            let values = split_types(arguments)?;
            // PEP 484 method comments may omit the implicit self/cls parameter.
            let omitted = values.len() + 1 == named.len() && function.is_method();
            if !(omitted && index == 0) {
                let value = values.get(index - usize::from(omitted))?;
                if let Some(ty) = self.stub_text_type(file, scope, value.trim_start_matches('*')) {
                    return Some(ty);
                }
            }
        }
        // Per-parameter comments in a multiline parameter list.
        if let Some(parameter) = params
            .params()
            .find(|p| p.name().is_some_and(|n| n.text() == name))
        {
            let at = usize::from(parameter.name()?.text_range().start());
            if let Some(comment) = file.text.get(at..)?.lines().next().and_then(type_comment) {
                if let Some(ty) = self.stub_text_type(file, scope, comment) {
                    return Some(ty);
                }
            }
        }
        if !self.db.analyze_doc_comments() {
            return None;
        }
        let doc = crate::docstrings::parse(&function.docstring()?.value());
        let (_, ty, _) = doc
            .params
            .iter()
            .find(|(n, _, _)| n.trim_start_matches('*') == name)?;
        self.documentation_type_in_scope(file, scope, ty.as_deref()?)
    }
    pub(crate) fn stub_return_type(
        &self,
        file: &Arc<FileInfo>,
        function: &ast::FuncDef,
    ) -> Option<Type> {
        let scope = self.scope_of(file, function.syntax());
        if let Some(comment) = self.function_comment(file, function) {
            let (_, result) = comment.rsplit_once("->")?;
            if let Some(ty) = self.stub_text_type(file, scope, result.trim()) {
                return Some(ty);
            }
        }
        if !self.db.analyze_doc_comments() {
            return None;
        }
        let value = function.docstring()?.value();
        let doc = crate::docstrings::parse(&value);
        let ty = self.documentation_type_in_scope(file, scope, doc.returns?.0.as_deref()?)?;
        if crate::docstrings::type_information::result_is_yield(&value) {
            self.documented_generator_type(function, ty)
        } else {
            Some(ty)
        }
    }
    fn stub_target_type(
        &self,
        target: ast::Expr,
        ty: Type,
        binding: &Binding,
        depth: usize,
    ) -> Option<Type> {
        if depth > 32 {
            return None;
        }
        let target = target.unparen();
        if let ast::Expr::Name(name) = &target {
            return (name.text_range() == binding.range).then_some(ty);
        }
        let elements: Vec<_> = match target {
            ast::Expr::Tuple(tuple) => tuple.elements().take(257).collect(),
            ast::Expr::List(list) => list.elements().take(257).collect(),
            _ => return None,
        };
        if elements.len() > 256 {
            return None;
        }
        let Type::Tuple(values) = ty else {
            return None;
        };
        let star = elements
            .iter()
            .position(|element| matches!(element, ast::Expr::Star(_)));
        if star.is_none() && elements.len() != values.len() {
            return None;
        }
        if star.is_some() && values.len() + 1 < elements.len() {
            return None;
        }
        for (index, element) in elements.iter().enumerate() {
            if !element.text_range().contains_range(binding.range) {
                continue;
            }
            let (target, value) = if let ast::Expr::Star(starred) = element {
                let end = values.len() - (elements.len() - index - 1);
                let item = Type::union(values[index..end].to_vec());
                (
                    starred.expr()?,
                    Type::Instance(self.builtin_class("list")?, vec![item]),
                )
            } else {
                let source = if star.is_some_and(|position| index > position) {
                    values.len() - (elements.len() - index)
                } else {
                    index
                };
                (element.clone(), values.get(source)?.clone())
            };
            return self.stub_target_type(target, value, binding, depth + 1);
        }
        None
    }
    pub(crate) fn stub_binding_type(
        &self,
        file: &Arc<FileInfo>,
        binding: &Binding,
    ) -> Option<Type> {
        // Ordinary assignments pay only for their final physical line; search
        // the AST only when that line actually carries a type comment.
        let end = usize::from(binding.node_range.end());
        let line_start = file.text.get(..end)?.rfind('\n').map_or(0, |at| at + 1);
        let line = file.text.get(line_start..)?.lines().next()?;
        let comment = type_comment(line)?;
        if comment.starts_with('(') && comment.contains("->") {
            return None;
        }
        let root = file.parse.syntax_node();
        let statement = root.descendants().find_map(|node| {
            (node.text_range() == binding.node_range)
                .then(|| ast::AssignStmt::cast(node))
                .flatten()
        })?;
        let ty = self.stub_text_type(file, binding.scope, comment)?;
        statement
            .targets()
            .into_iter()
            .find(|target| target.text_range().contains_range(binding.range))
            .and_then(|target| self.stub_target_type(target, ty, binding, 0))
    }
}
