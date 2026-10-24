//! Expected types are a scoped query input, never a context-free cache fact.
use super::*;
#[path = "contextual_constructor.rs"]
mod contextual_constructor;
#[path = "lambda_context.rs"]
mod lambda_context;
#[path = "expected_overloads.rs"]
mod overload_context;
use rustc_hash::FxHashSet;
use std::cell::Cell;
#[derive(Default)]
pub(super) struct ExpectedContext {
    pub(super) active: Cell<bool>,
    pub(super) overrides: RefCell<FxHashMap<(FileId, BindingId), Type>>,
    pending: RefCell<FxHashSet<(FileId, TextRange)>>,
    queries: Cell<usize>,
}
struct Pending<'a> {
    set: &'a RefCell<FxHashSet<(FileId, TextRange)>>,
    key: (FileId, TextRange),
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.set.borrow_mut().remove(&self.key);
    }
}
impl Inferer<'_> {
    /// Contextual results and derived binding/signature caches live in a fresh
    /// Inferer. Dropping that session (also on panic) restores ordinary queries.
    pub fn type_of_expr_with_expected(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
        expected: &Type,
    ) -> Type {
        if self
            .db
            .file(file.id)
            .is_none_or(|current| !Arc::ptr_eq(&current, file))
        {
            return Type::Unknown;
        }
        let session = self.flow_session();
        session.expected_context.active.set(true);
        session.expected_context.queries.set(512);
        session.expected_context.overrides.borrow_mut().extend(
            self.expected_context
                .overrides
                .borrow()
                .iter()
                .map(|(key, value)| (*key, value.clone())),
        );
        let mut exhausted = false;
        if !bounded_context_type(expected, 0, &mut 512, &mut exhausted) {
            if exhausted {
                return Type::Unknown;
            }
            // Unsupported expected metadata is not evidence about the actual
            // expression. Preserve ordinary inference in this isolated session.
            return session.type_of_expr(file, expression);
        }
        session.contextual_expression(file, expression, expected, 0)
    }
    pub(super) fn parent_expected_type(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
    ) -> Option<Type> {
        if self.expected_context.active.get() {
            return None;
        }
        let range = expression.text_range();
        for node in expression.syntax().ancestors().skip(1).take(4) {
            if let Some(assign) = ast::AnnAssignStmt::cast(node.clone()) {
                if assign.value()?.text_range() == range {
                    return Some(self.annotation_type(file, &assign.annotation()?));
                }
                return None;
            }
            if let Some(statement) = ast::ReturnStmt::cast(node.clone()) {
                if statement.value()?.text_range() != range {
                    return None;
                }
                let function = node.ancestors().skip(1).find_map(ast::FuncDef::cast)?;
                return Some(self.annotation_type(file, &function.return_annotation()?));
            }
            if let Some(call) = ast::CallExpr::cast(node) {
                // Establish that this expression is an argument BEFORE looking
                // up its callee. Callee queries must stay on the ordinary guard.
                let mut positional = 0;
                let mut target = None;
                for argument in call.arg_list()?.args().take(33) {
                    let value = argument.value()?;
                    let position = match &argument {
                        ast::AnyArg::Positional(_) => {
                            let index = positional;
                            positional += 1;
                            (Some(index), None)
                        }
                        ast::AnyArg::Keyword(keyword) => (None, Some(keyword.name_text())),
                        _ => return None,
                    };
                    if value.text_range() == range {
                        target = Some(position);
                        break;
                    }
                }
                let (position, keyword) = target?;
                let callee = self.type_of_expr(file, &call.callee()?);
                let signature = self.parent_context_call_signature(file, &call, &callee)?;
                let parameter = if let Some(position) = position {
                    parameter_at_position(&signature, position)
                } else {
                    signature.params.iter().find(|p| {
                        p.kind != ParamKind::PositionalOnly
                            && keyword.as_ref().is_some_and(|name| p.name == *name)
                    })
                };
                return parameter.map(|p| p.ty.clone());
            }
        }
        None
    }
    fn context_signature(&self, ty: &Type) -> Option<Arc<Signature>> {
        match ty {
            Type::Function(sig) | Type::Callable(sig) | Type::BoundMethod(sig, _) => {
                Some(sig.clone())
            }
            _ => None,
        }
    }
    fn contextual_expression(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
        expected: &Type,
        depth: usize,
    ) -> Type {
        let remaining = self.expected_context.queries.get();
        if depth >= 32 || remaining == 0 {
            return Type::Unknown;
        }
        self.expected_context.queries.set(remaining - 1);
        let key = (file.id, expression.text_range());
        if !self.expected_context.pending.borrow_mut().insert(key) {
            return Type::Unknown;
        }
        let _pending = Pending {
            set: &self.expected_context.pending,
            key,
        };
        let expression = expression.clone().unparen();
        match (&expression, expected) {
            (ast::Expr::List(list), Type::Instance(class, args))
                if self.builtin_class("list").as_ref() == Some(class) && args.len() == 1 =>
            {
                let values: Vec<_> = list.elements().take(257).collect();
                if values.len() > 256 || values.iter().any(|v| matches!(v, ast::Expr::Star(_))) {
                    return self.type_of_expr(file, &expression);
                }
                let element = if values.is_empty() {
                    args[0].clone()
                } else {
                    Type::union(
                        values
                            .iter()
                            .map(|v| {
                                self.widen(self.contextual_expression(file, v, &args[0], depth + 1))
                            })
                            .collect(),
                    )
                };
                Type::Instance(class.clone(), vec![element])
            }
            (ast::Expr::Set(set), Type::Instance(class, args))
                if self.builtin_class("set").as_ref() == Some(class) && args.len() == 1 =>
            {
                let values: Vec<_> = set.elements().take(257).collect();
                if values.len() > 256 || values.iter().any(|v| matches!(v, ast::Expr::Star(_))) {
                    return self.type_of_expr(file, &expression);
                }
                let element = if values.is_empty() {
                    args[0].clone()
                } else {
                    Type::union(
                        values
                            .iter()
                            .map(|v| {
                                self.widen(self.contextual_expression(file, v, &args[0], depth + 1))
                            })
                            .collect(),
                    )
                };
                Type::Instance(class.clone(), vec![element])
            }
            (ast::Expr::Dict(dict), Type::Instance(class, args))
                if self.builtin_class("dict").as_ref() == Some(class) && args.len() == 2 =>
            {
                let items: Vec<_> = dict.items().take(257).collect();
                if items.len() > 256 || dict.unpacks().next().is_some() {
                    return self.type_of_expr(file, &expression);
                }
                if items.is_empty() {
                    return Type::Instance(class.clone(), args.clone());
                }
                let mut keys = Vec::new();
                let mut values = Vec::new();
                for item in items {
                    let (Some(k), Some(v)) = (item.key(), item.value()) else {
                        return self.type_of_expr(file, &expression);
                    };
                    keys.push(self.widen(self.contextual_expression(
                        file,
                        &k,
                        &args[0],
                        depth + 1,
                    )));
                    values.push(self.widen(self.contextual_expression(
                        file,
                        &v,
                        &args[1],
                        depth + 1,
                    )));
                }
                Type::Instance(class.clone(), vec![Type::union(keys), Type::union(values)])
            }
            (ast::Expr::Tuple(tuple), Type::Tuple(expected)) => {
                let values: Vec<_> = tuple.elements().take(257).collect();
                if values.len() != expected.len() || values.len() > 256 {
                    return self.type_of_expr(file, &expression);
                }
                Type::Tuple(
                    values
                        .iter()
                        .zip(expected)
                        .map(|(v, e)| self.contextual_expression(file, v, e, depth + 1))
                        .collect(),
                )
            }
            (ast::Expr::Lambda(lambda), _) => self
                .contextual_lambda(file, lambda, expected, depth)
                .unwrap_or_else(|| self.type_of_expr(file, &expression)),
            (ast::Expr::Call(call), _) => self
                .contextual_call(file, call, expected, depth)
                .unwrap_or_else(|| self.type_of_expr(file, &expression)),
            _ => self.type_of_expr(file, &expression),
        }
    }
    fn contextual_lambda(
        &self,
        file: &Arc<FileInfo>,
        lambda: &ast::LambdaExpr,
        expected: &Type,
        depth: usize,
    ) -> Option<Type> {
        let signature = self.context_signature(expected)?;
        if signature.is_async || signature.params.len() > 32 {
            return None;
        }
        let mut parameters = lambda
            .params()
            .map(|p| self.params_from_list(file, &p, None, false))
            .unwrap_or_default();
        if parameters.len() != signature.params.len()
            || parameters.iter().zip(&signature.params).any(|(a, b)| {
                (a.kind != b.kind
                    && !(a.kind == ParamKind::Normal && b.kind == ParamKind::PositionalOnly))
                    || matches!(a.kind, ParamKind::VarArgs | ParamKind::KwArgs)
            })
        {
            return None;
        }
        let scope = file.scopes.scope_for_node(lambda.text_range())?;
        let session = self.flow_session();
        session.expected_context.active.set(true);
        session
            .expected_context
            .queries
            .set(self.expected_context.queries.get());
        session.expected_context.overrides.borrow_mut().extend(
            self.expected_context
                .overrides
                .borrow()
                .iter()
                .map(|(key, value)| (*key, value.clone())),
        );
        for (parameter, expected) in parameters.iter_mut().zip(&signature.params) {
            if !matches!(parameter.ty, Type::Unknown) {
                continue;
            }
            let ids = file.scopes.bindings_in(scope, &parameter.name);
            let [id] = ids else {
                return None;
            };
            if !matches!(file.scopes.binding(*id).kind, BindingKind::Param) {
                return None;
            }
            parameter.ty = expected.ty.clone();
            session
                .expected_context
                .overrides
                .borrow_mut()
                .insert((file.id, *id), expected.ty.clone());
        }
        let result = session.widen(session.contextual_expression(
            file,
            &lambda.body()?,
            &signature.ret,
            depth + 1,
        ));
        self.expected_context
            .queries
            .set(session.expected_context.queries.get());
        Some(Type::Function(Arc::new(Signature {
            params: parameters,
            ret: result,
            is_async: false,
            def: None,
            docstring: None,
            ret_inferred: true,
        })))
    }
    fn contextual_call(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
        expected: &Type,
        depth: usize,
    ) -> Option<Type> {
        // Selection precedes expected return solving and uses the maintained
        // ordinary argument matcher. Classes keep their normal dispatch path.
        let callee = self.type_of_expr(file, &call.callee()?);
        if let Type::Class(class, arguments) = &callee {
            return self.contextual_constructor(file, call, class, arguments, expected);
        }
        let arguments: Vec<_> = call.arg_list()?.args().take(33).collect();
        if arguments.len() > 32
            || arguments
                .iter()
                .any(|a| !matches!(a, ast::AnyArg::Positional(_) | ast::AnyArg::Keyword(_)))
        {
            return None;
        }
        let (ordinary_pos, ordinary_kw) = self.call_args(file, call);
        let signature = if let Type::Overloaded(signatures) = &callee {
            self.unique_context_overload(signatures, &ordinary_pos, &ordinary_kw)?
        } else {
            self.context_signature(&callee)?
        };
        if !bounded_context_type(&Type::Function(signature.clone()), 0, &mut 512, &mut false)
            || signature.params.len() > 32
        {
            return None;
        }
        let mut known = FxHashMap::default();
        self.solve_typevars(&signature.params, &ordinary_pos, &ordinary_kw, &mut known);
        self.contextual_return_solutions(&signature.ret, expected, &mut known);
        let mut position = 0;
        let mut pos = Vec::new();
        let mut kw = Vec::new();
        for argument in arguments {
            let parameter = match &argument {
                ast::AnyArg::Positional(_) => {
                    let p = parameter_at_position(&signature, position);
                    position += 1;
                    p
                }
                ast::AnyArg::Keyword(k) => signature
                    .params
                    .iter()
                    .find(|p| p.kind != ParamKind::PositionalOnly && p.name == k.name_text()),
                _ => None,
            };
            let value = argument.value()?;
            let actual = if let Some(parameter) = parameter {
                let expected = parameter.ty.substitute(&|key| known.get(key).cloned());
                self.contextual_expression(file, &value, &expected, depth + 1)
            } else {
                self.type_of_expr(file, &value)
            };
            match argument {
                ast::AnyArg::Positional(_) => pos.push(actual),
                ast::AnyArg::Keyword(k) => kw.push((SmolStr::new(k.name_text()), actual)),
                _ => return None,
            }
        }
        if let Type::Overloaded(signatures) = &callee {
            let selected = self.select_overload(signatures, &pos, &kw);
            if !Arc::ptr_eq(&selected, &signature) || !self.arguments_fit(&selected, &pos, &kw) {
                return None;
            }
        }
        let mut substitutions = FxHashMap::default();
        self.solve_typevars(&signature.params, &pos, &kw, &mut substitutions);
        self.contextual_return_solutions(&signature.ret, expected, &mut substitutions);
        // Common ParamSpec/tuple-pack guards and default validation are owned
        // by the shared signature-completion helper, before final substitution.
        Some(self.finish_signature_result(&signature, substitutions))
    }
    fn contextual_return_solutions(
        &self,
        returned: &Type,
        expected: &Type,
        substitutions: &mut FxHashMap<SmolStr, Type>,
    ) {
        let mut contextual = FxHashMap::default();
        self.unify(returned, expected, &mut contextual);
        let mut scalar_keys = FxHashSet::default();
        collect_scalar_context_keys(returned, &mut scalar_keys, 0, &mut 512);
        for (key, value) in contextual {
            if scalar_keys.contains(&key) {
                substitutions.entry(key).or_insert(value);
            }
        }
    }
}
fn parameter_at_position(signature: &Signature, index: usize) -> Option<&Param> {
    signature
        .params
        .iter()
        .filter(|p| matches!(p.kind, ParamKind::Normal | ParamKind::PositionalOnly))
        .nth(index)
}

fn bounded_context_type(ty: &Type, depth: usize, budget: &mut usize, exhausted: &mut bool) -> bool {
    if depth >= 32 || *budget == 0 {
        *exhausted = true;
        return false;
    }
    *budget -= 1;
    match ty {
        Type::Instance(_, args) | Type::Class(_, args) | Type::Tuple(args) | Type::Union(args) => {
            args.iter()
                .all(|t| bounded_context_type(t, depth + 1, budget, exhausted))
        }
        Type::Function(sig) | Type::Callable(sig) | Type::BoundMethod(sig, _) => {
            sig.params.len() <= 32
                && sig
                    .params
                    .iter()
                    .all(|p| bounded_context_type(&p.ty, depth + 1, budget, exhausted))
                && bounded_context_type(&sig.ret, depth + 1, budget, exhausted)
        }
        Type::DeclaredTypeVar(def) => {
            def.bound
                .as_ref()
                .is_none_or(|t| bounded_context_type(t, depth + 1, budget, exhausted))
                && def
                    .constraints
                    .iter()
                    .all(|t| bounded_context_type(t, depth + 1, budget, exhausted))
                && def
                    .default
                    .as_ref()
                    .is_none_or(|t| bounded_context_type(t, depth + 1, budget, exhausted))
        }
        Type::Unknown
        | Type::Any
        | Type::Never
        | Type::None
        | Type::Module(_)
        | Type::Literal(_)
        | Type::TypeVar(_)
        | Type::SelfType(_) => true,
        _ => false,
    }
}
fn collect_scalar_context_keys(
    ty: &Type,
    keys: &mut FxHashSet<SmolStr>,
    depth: usize,
    budget: &mut usize,
) {
    if depth >= 32 || *budget == 0 {
        return;
    }
    *budget -= 1;
    match ty {
        Type::TypeVar(key) => {
            keys.insert(key.clone());
        }
        Type::DeclaredTypeVar(def) => {
            keys.insert(def.key.clone());
        }
        Type::Instance(_, args) | Type::Class(_, args) | Type::Tuple(args) | Type::Union(args) => {
            for t in args {
                collect_scalar_context_keys(t, keys, depth + 1, budget);
            }
        }
        Type::Function(sig) | Type::Callable(sig) | Type::BoundMethod(sig, _) => {
            for p in &sig.params {
                collect_scalar_context_keys(&p.ty, keys, depth + 1, budget);
            }
            collect_scalar_context_keys(&sig.ret, keys, depth + 1, budget);
        }
        _ => {}
    }
}
