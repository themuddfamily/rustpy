//! Pure selected-overload proof from captured bindings and simple argument contracts.
//! No inference, callbacks, live module queries or argument execution.
use super::*;

#[derive(Clone, PartialEq, Eq)]
enum Primitive {
    Int,
    Str,
    Bool,
    Float,
    Bytes,
    None,
    Object,
    IntValue(i64),
    BoolValue(bool),
    TextValue(String),
    BytesValue(Vec<u8>),
    Nominal(rustpy_core::FileId, rustpy_core::TextRange),
}
struct SelectedGroup<'a> {
    file: &'a FileInfo,
    id: BindingId,
    instance: Option<bool>,
}

impl Walker<'_> {
    pub(in super::super) fn selected_overload_modes(
        &self,
        call: &ast::CallExpr,
        state: &State,
    ) -> [bool; 2] {
        let Some(contract) = self.selected_overload_contract(call, state) else {
            return [false; 2];
        };
        [contract.terminates(false), contract.terminates(true)]
    }

    fn selected_overload_contract(&self, call: &ast::CallExpr, state: &State) -> Option<Contract> {
        let mut arguments: Vec<(Option<String>, Primitive)> = Vec::new();
        for argument in call.arg_list()?.args().take(17) {
            if arguments.len() >= 16 {
                return None;
            }
            let (keyword, expression) = match argument {
                ast::AnyArg::Positional(argument) => (None, argument.expr()?),
                ast::AnyArg::Keyword(argument) => (Some(argument.name_text()), argument.value()?),
                _ => return None,
            };
            // Deliberately decline callbacks/walrus/attribute/index operands:
            // their earlier evaluation may change a later argument's contract.
            if keyword.is_none() && arguments.iter().any(|(keyword, _)| keyword.is_some()) {
                return None;
            }
            let kind = self.selected_argument_kind(
                self.file,
                self.scope,
                &expression,
                Some(state),
                &mut FxHashSet::default(),
                0,
            )?;
            arguments.push((keyword, kind));
        }
        let callee = call.callee()?.unparen();
        let (name, member) = match callee {
            ast::Expr::Name(name) => (name, None),
            ast::Expr::Attribute(attribute) => {
                let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                    return None;
                };
                (name, Some(attribute.attr_text()))
            }
            _ => return None,
        };
        if self
            .shadowed
            .iter()
            .any(|names| names.contains(name.name_text().as_str()))
        {
            return None;
        }
        let ids = if let Some(reaches) = self.reaching_name(&name.name_text(), state) {
            if reaches.is_empty()
                || reaches
                    .iter()
                    .any(|reach| reach.binding.is_none() || !reach.preds.is_empty())
            {
                return None;
            }
            reaches
                .into_iter()
                .filter_map(|reach| reach.binding)
                .collect::<Vec<_>>()
        } else {
            self.file
                .scopes
                .resolve(self.scope, &name.name_text())?
                .to_vec()
        };
        if ids.is_empty() || ids.len() > 32 {
            return None;
        }
        let mut answer = None;
        for id in ids {
            let group =
                self.selected_group(self.file, id, member.as_deref(), &mut FxHashSet::default())?;
            let contract = self.select_group_contract(group, &arguments)?;
            if answer.is_some_and(|previous| {
                !matches!(
                    (previous, contract),
                    (Contract::Sync, Contract::Sync) | (Contract::Async, Contract::Async)
                )
            }) {
                return None;
            }
            answer = Some(contract);
        }
        answer
    }

    fn selected_group<'a>(
        &'a self,
        file: &'a FileInfo,
        id: BindingId,
        member: Option<&str>,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<SelectedGroup<'a>> {
        if seen.len() >= 16 || !seen.insert((file.id, id)) {
            return None;
        }
        let binding = file.scopes.binding(id);
        let root = file.parse.syntax_node();
        let node = match root.covering_element(binding.range) {
            rustpy_syntax::rowan::NodeOrToken::Node(node) => node,
            rustpy_syntax::rowan::NodeOrToken::Token(token) => token.parent()?,
        };
        if node
            .ancestors()
            .take_while(|node| node.text_range() != file.scopes.scope(binding.scope).range)
            .any(|node| {
                matches!(
                    ast::Stmt::cast(node),
                    Some(
                        ast::Stmt::If(_)
                            | ast::Stmt::For(_)
                            | ast::Stmt::While(_)
                            | ast::Stmt::Try(_)
                            | ast::Stmt::Match(_)
                            | ast::Stmt::With(_)
                    )
                )
            })
        {
            return None;
        }

        if !file.scopes.scope(binding.scope).star_imports.is_empty() {
            return None;
        }
        if let Some(member) = member {
            if self.authority.retired_members.contains(member) {
                return None;
            }
        }
        if let BindingKind::Import(import) = &binding.kind {
            let (name, remaining) = match (import.name.as_deref(), member) {
                (Some(name), member) => (name, member),
                (None, Some(name)) => (name, None),
                _ => return None,
            };
            if self.authority.retired_members.contains(name) {
                return None;
            }
            let target = self.authority.project_imports.get(&(file.id, id))?;
            let ids = target.scopes.resolve(target.scopes.module_scope(), name)?;
            if ids.is_empty() || ids.len() > 32 {
                return None;
            }
            // Complete function group is validated by select_group_contract;
            // a non-function target must be an unambiguous captured value.
            if ids.len() > 1
                && ids
                    .iter()
                    .any(|id| !matches!(target.scopes.binding(*id).kind, BindingKind::Function))
            {
                return None;
            }
            return self.selected_group(target, ids[0], remaining, seen);
        }
        if let Some(member) = member {
            let (class_file, class, instance) = if let Some((file, class, _)) =
                self.context_instance_class(file, id, &mut FxHashSet::default())
            {
                (file, class, true)
            } else {
                let (file, class) =
                    self.class_from_binding(file, id, None, &mut FxHashSet::default())?;
                (file, class, false)
            };
            let mro = self.class_mro(class_file, class, &mut FxHashSet::default(), &mut 0)?;
            if instance
                && mro.iter().any(|class| {
                    !class.intrinsic
                        && !class
                            .file
                            .scopes
                            .bindings_in(class.scope, "__new__")
                            .is_empty()
                })
            {
                return None;
            }
            for class in mro {
                if class.intrinsic {
                    continue;
                }
                let ids = class.file.scopes.bindings_in(class.scope, member);
                if !ids.is_empty() {
                    return Some(SelectedGroup {
                        file: class.file,
                        id: ids[0],
                        instance: Some(instance),
                    });
                }
            }
            return None;
        }
        if matches!(binding.kind, BindingKind::Assignment) {
            let expression = self.expression_at(file, binding.value?)?;
            let (name, member) = match expression {
                ast::Expr::Name(name) => (name, None),
                ast::Expr::Attribute(attribute) => {
                    let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                        return None;
                    };
                    (name, Some(attribute.attr_text()))
                }
                _ => return None,
            };
            let ids = self.selected_alias_ids(file, binding.scope, &name, binding.range.start())?;
            if ids.is_empty() || ids.len() > 32 {
                return None;
            }
            let mut result = None;
            for id in ids {
                let group = self.selected_group(file, id, member.as_deref(), seen)?;
                if result.as_ref().is_some_and(|previous: &SelectedGroup<'_>| {
                    previous.file.id != group.file.id
                        || previous.file.scopes.binding(previous.id).name
                            != group.file.scopes.binding(group.id).name
                        || previous.instance != group.instance
                }) {
                    return None;
                }
                result = Some(group);
            }
            return result;
        }
        if !matches!(binding.kind, BindingKind::Function)
            || file.scopes.scope(binding.scope).kind == crate::scope::ScopeKind::Class
        {
            return None;
        }
        Some(SelectedGroup {
            file,
            id,
            instance: None,
        })
    }

    fn selected_alias_ids(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        name: &ast::NameExpr,
        before: rustpy_core::TextSize,
    ) -> Option<Vec<BindingId>> {
        if file.id == self.file.id {
            if let Some(reaches) = self.info.refs.get(&name.name_token()?.text_range().start()) {
                if reaches.is_empty()
                    || reaches
                        .iter()
                        .any(|reach| reach.binding.is_none() || !reach.preds.is_empty())
                {
                    return None;
                }
                return Some(reaches.iter().filter_map(|reach| reach.binding).collect());
            }
        }
        let ids = file.scopes.resolve(scope, &name.name_text())?;
        if ids.is_empty()
            || ids.len() > 32
            || ids
                .iter()
                .any(|id| file.scopes.binding(*id).range.start() >= before)
        {
            return None;
        }
        Some(ids.to_vec())
    }

    fn selected_argument_kind(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        expression: &ast::Expr,
        state: Option<&State>,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
        depth: usize,
    ) -> Option<Primitive> {
        if depth >= 16 {
            return None;
        }
        match expression.clone().unparen() {
            ast::Expr::Literal(literal) => match literal.kind()? {
                SyntaxKind::INT_NUMBER => {
                    selected_integer(&literal.syntax().text().to_string()).map(Primitive::IntValue)
                }
                SyntaxKind::FLOAT_NUMBER => Some(Primitive::Float),
                SyntaxKind::TRUE_KW => Some(Primitive::BoolValue(true)),
                SyntaxKind::FALSE_KW => Some(Primitive::BoolValue(false)),
                SyntaxKind::NONE_KW => Some(Primitive::None),
                _ => None,
            },
            ast::Expr::String(string) => {
                let raw = string.syntax().text().to_string();
                if raw.len() > 4096 {
                    return None;
                }
                match rustpy_syntax::string_literal::string(&raw).ok()? {
                    rustpy_syntax::string_literal::Decoded::Text(value, _) => {
                        Some(Primitive::TextValue(value))
                    }
                    rustpy_syntax::string_literal::Decoded::Bytes(value) => {
                        Some(Primitive::BytesValue(value))
                    }
                    _ => None,
                }
            }
            ast::Expr::Unary(unary) => {
                let operand = unary.operand()?;
                if !matches!(
                    operand.clone().unparen(),
                    ast::Expr::Literal(_) | ast::Expr::Unary(_)
                ) {
                    return None;
                }
                let kind =
                    self.selected_argument_kind(file, scope, &operand, state, seen, depth + 1)?;
                match (unary.op()?.text(), kind) {
                    ("+", Primitive::IntValue(value)) => Some(Primitive::IntValue(value)),
                    ("-", Primitive::IntValue(value)) => {
                        value.checked_neg().map(Primitive::IntValue)
                    }
                    ("~", Primitive::IntValue(value)) => Some(Primitive::IntValue(!value)),
                    ("+" | "-", Primitive::Int | Primitive::Bool) => Some(Primitive::Int),
                    ("+" | "-", Primitive::Float) => Some(Primitive::Float),
                    ("~", Primitive::Int | Primitive::Bool) => Some(Primitive::Int),
                    _ => None,
                }
            }
            ast::Expr::Name(name) => {
                let ids = if file.id == self.file.id {
                    if let Some(state) = state {
                        if let Some(reaches) = self.reaching_name(&name.name_text(), state) {
                            if reaches.is_empty()
                                || reaches
                                    .iter()
                                    .any(|reach| reach.binding.is_none() || !reach.preds.is_empty())
                            {
                                return None;
                            }
                            reaches
                                .into_iter()
                                .filter_map(|reach| reach.binding)
                                .collect::<Vec<_>>()
                        } else {
                            file.scopes.resolve(scope, &name.name_text())?.to_vec()
                        }
                    } else {
                        self.value_binding_ids(file, scope, &name, expression.text_range().start())?
                    }
                } else {
                    file.scopes.resolve(scope, &name.name_text())?.to_vec()
                };
                if ids.is_empty() || ids.len() > 16 {
                    return None;
                }
                let mut answer = None;
                for id in ids {
                    if !seen.insert((file.id, id)) {
                        return None;
                    }
                    let binding = file.scopes.binding(id);
                    let kind = match binding.kind {
                        BindingKind::Param => self
                            .selected_annotation(
                                file,
                                file.scopes.scope(binding.scope).parent?,
                                &self.expression_at(file, binding.annotation?)?,
                            )
                            .filter(|kind| *kind != Primitive::Object)?,
                        BindingKind::Assignment => self.selected_argument_kind(
                            file,
                            binding.scope,
                            &self.expression_at(file, binding.value?)?,
                            None,
                            seen,
                            depth + 1,
                        )?,
                        _ => return None,
                    };
                    seen.remove(&(file.id, id));
                    if answer.as_ref().is_some_and(|previous| previous != &kind) {
                        return None;
                    }
                    answer = Some(kind);
                }
                answer
            }
            _ => None,
        }
    }

    fn selected_primitive_annotation(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        expression: &ast::Expr,
    ) -> Option<Primitive> {
        if matches!(expression.clone().unparen(), ast::Expr::Literal(literal) if literal.kind() == Some(SyntaxKind::NONE_KW))
        {
            return Some(Primitive::None);
        }
        let ast::Expr::Name(name) = expression.clone().unparen() else {
            return None;
        };
        if self
            .authority
            .retired_members
            .contains(name.name_text().as_str())
        {
            return None;
        }
        if file.scopes.resolve(scope, &name.name_text()).is_some() {
            return None;
        }
        let mut current = Some(scope);
        while let Some(id) = current {
            let scope = file.scopes.scope(id);
            if !scope.star_imports.is_empty() {
                return None;
            }
            current = scope.parent;
        }
        match name.name_text().as_str() {
            "int" => Some(Primitive::Int),
            "str" => Some(Primitive::Str),
            "bool" => Some(Primitive::Bool),
            "float" => Some(Primitive::Float),
            "bytes" => Some(Primitive::Bytes),
            "object" => Some(Primitive::Object),
            _ => None,
        }
    }

    // Nominal identities are actual captured file/range identities, never names.
    fn selected_annotation(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        expression: &ast::Expr,
    ) -> Option<Primitive> {
        if let Some(primitive) = self.selected_primitive_annotation(file, scope, expression) {
            return Some(primitive);
        }
        if let ast::Expr::Subscript(subscript) = expression.clone().unparen() {
            let object = subscript.object()?;
            if !self.canonical_type_symbol(file, scope, &object, &["Literal"]) {
                return None;
            }
            let index = subscript.index_expr()?;
            // A single literal is enough for precise branch selection; tuples,
            // unions and computed values remain unknown, not a mismatch.
            let value = self.selected_argument_kind(
                file,
                scope,
                &index,
                None,
                &mut FxHashSet::default(),
                0,
            )?;
            return matches!(
                value,
                Primitive::IntValue(_)
                    | Primitive::BoolValue(_)
                    | Primitive::TextValue(_)
                    | Primitive::BytesValue(_)
                    | Primitive::None
            )
            .then_some(value);
        }
        let (name, member) = match expression.clone().unparen() {
            ast::Expr::Name(name) => (name, None),
            ast::Expr::Attribute(attribute) => {
                let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                    return None;
                };
                (name, Some(attribute.attr_text()))
            }
            _ => return None,
        };
        if file.id == self.file.id
            && self
                .shadowed
                .iter()
                .any(|names| names.contains(name.name_text().as_str()))
        {
            return None;
        }
        let ids = self.value_binding_ids(file, scope, &name, expression.text_range().start())?;
        if ids.len() != 1 {
            return None;
        }
        let (target, class) =
            self.class_from_binding(file, ids[0], member.as_deref(), &mut FxHashSet::default())?;
        // Validate guarded nominal C3 even for exact identity: custom metaclass,
        // factories, writes and opaque bases cannot establish assignability.
        self.class_mro(target, class.clone(), &mut FxHashSet::default(), &mut 0)?;
        Some(Primitive::Nominal(target.id, class.text_range()))
    }

    fn selected_type_matches(&self, actual: &Primitive, expected: &Primitive) -> Option<bool> {
        if actual == expected {
            return Some(true);
        }
        if matches!(expected, Primitive::Object) {
            return Some(true);
        }
        if let (Primitive::Nominal(file, range), Primitive::Nominal(wanted_file, wanted_range)) =
            (actual, expected)
        {
            let source = if *file == self.file.id {
                self.file
            } else {
                self.authority
                    .project_imports
                    .values()
                    .find(|source| source.id == *file)?
                    .as_ref()
            };
            let class = node_at(&source.parse.syntax_node(), *range, SyntaxKind::CLASS_DEF)
                .and_then(ast::ClassDef::cast)?;
            let mro = self.class_mro(source, class, &mut FxHashSet::default(), &mut 0)?;
            return Some(mro.iter().any(|class| {
                !class.intrinsic
                    && class.file.id == *wanted_file
                    && class.file.scopes.scope_for_node(*wanted_range) == Some(class.scope)
            }));
        }
        // Nominal-vs-primitive cannot be disproved without validated intrinsic
        // base projection; never skip an earlier possibly compatible overload.
        if matches!(actual, Primitive::Nominal(..)) || matches!(expected, Primitive::Nominal(..)) {
            return None;
        }
        // A widened declaration is not evidence for a particular literal.
        if matches!(
            expected,
            Primitive::IntValue(_)
                | Primitive::BoolValue(_)
                | Primitive::TextValue(_)
                | Primitive::BytesValue(_)
        ) {
            return match actual {
                Primitive::IntValue(_)
                | Primitive::BoolValue(_)
                | Primitive::TextValue(_)
                | Primitive::BytesValue(_)
                | Primitive::None => Some(false),
                _ => None,
            };
        }
        let actual = match actual {
            Primitive::IntValue(_) => Primitive::Int,
            Primitive::BoolValue(_) => Primitive::Bool,
            Primitive::TextValue(_) => Primitive::Str,
            Primitive::BytesValue(_) => Primitive::Bytes,
            _ => actual.clone(),
        };
        Some(
            actual == *expected
                || matches!(
                    (actual, expected),
                    (Primitive::Bool, Primitive::Int | Primitive::Float)
                        | (Primitive::Int, Primitive::Float)
                ),
        )
    }

    fn select_group_contract(
        &self,
        group: SelectedGroup<'_>,
        arguments: &[(Option<String>, Primitive)],
    ) -> Option<Contract> {
        let binding = group.file.scopes.binding(group.id);
        let ids = group.file.scopes.bindings_in(binding.scope, &binding.name);
        if ids.len() < 2 || ids.len() > 32 {
            return None;
        }
        let mut functions = Vec::new();
        for id in ids {
            let binding = group.file.scopes.binding(*id);
            if !matches!(binding.kind, BindingKind::Function) {
                return None;
            }
            let function = node_at(
                &group.file.parse.syntax_node(),
                binding.node_range,
                SyntaxKind::FUNC_DEF,
            )
            .and_then(ast::FuncDef::cast)?;
            let node = function.syntax();
            if node
                .ancestors()
                .skip(1)
                .take_while(|node| {
                    node.text_range() != group.file.scopes.scope(binding.scope).range
                })
                .any(|node| {
                    matches!(
                        ast::Stmt::cast(node),
                        Some(
                            ast::Stmt::If(_)
                                | ast::Stmt::For(_)
                                | ast::Stmt::While(_)
                                | ast::Stmt::Try(_)
                                | ast::Stmt::Match(_)
                                | ast::Stmt::With(_)
                        )
                    )
                })
            {
                return None;
            }
            if node.descendants().any(|node| {
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
            }) {
                return None;
            }
            functions.push(function);
        }
        let (overloaded, descriptor) =
            self.overload_metadata(group.file, &functions[0], binding.scope)?;
        if !overloaded {
            return None;
        }
        let skip_receiver = match (group.instance, descriptor.as_deref()) {
            (None, None) => false,
            (Some(true), None) => true,
            (Some(_), Some("staticmethod")) => false,
            (Some(_), Some("classmethod")) => true,
            _ => return None,
        };
        let asynchronous = functions[0].is_async();
        let mut declarations = 0;
        let mut implementation = false;
        let mut chosen = None;
        for (index, function) in functions.iter().enumerate() {
            let (overloaded, current_descriptor) =
                self.overload_metadata(group.file, function, binding.scope)?;
            if current_descriptor != descriptor
                || function.is_async() != asynchronous
                || function.syntax().parent()?.text_range()
                    != functions[0].syntax().parent()?.text_range()
            {
                return None;
            }
            if !overloaded {
                if implementation || index + 1 != functions.len() {
                    return None;
                }
                implementation = true;
                continue;
            }
            if implementation {
                return None;
            }
            declarations += 1;
            if chosen.is_none() {
                let matches = self.selected_arguments_match(
                    group.file,
                    binding.scope,
                    function,
                    arguments,
                    skip_receiver,
                )?;
                if matches {
                    let annotation = function.return_annotation()?.unparen();
                    let contract = if self.canonical_type_symbol(
                        group.file,
                        binding.scope,
                        &annotation,
                        &["Never", "NoReturn"],
                    ) {
                        Some(if function.is_async() {
                            Contract::Async
                        } else {
                            Contract::Sync
                        })
                    } else if !function.is_async() {
                        self.awaitable_return_contract(group.file, binding.scope, &annotation)
                    } else {
                        None
                    };
                    chosen = Some(contract);
                }
            }
        }
        if declarations < 2 || (!group.file.is_stub && !implementation) {
            return None;
        }
        chosen.flatten()
    }

    fn selected_arguments_match(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        function: &ast::FuncDef,
        arguments: &[(Option<String>, Primitive)],
        skip_receiver: bool,
    ) -> Option<bool> {
        let mut parameters: Vec<(ast::AnyParam, bool, bool, u8)> = Vec::new();
        let mut keyword_only = false;
        let mut skip = skip_receiver;
        let declared: Vec<_> = function.params()?.params().take(20).collect();
        if declared.len() >= 20 {
            return None;
        }
        for parameter in declared {
            match &parameter {
                ast::AnyParam::Plain(_) => {
                    if skip {
                        skip = false;
                        continue;
                    }
                    parameters.push((parameter, false, keyword_only, 0));
                }
                ast::AnyParam::Slash(_) => {
                    for (_, positional_only, _, kind) in &mut parameters {
                        if *kind == 0 {
                            *positional_only = true;
                        }
                    }
                }
                ast::AnyParam::StarSep(_) => keyword_only = true,
                ast::AnyParam::Star(_) => {
                    if skip || parameters.iter().any(|p| p.3 == 1) {
                        return None;
                    }
                    parameters.push((parameter, false, true, 1));
                    keyword_only = true;
                }
                ast::AnyParam::StarStar(_) => {
                    if skip || parameters.iter().any(|p| p.3 == 2) {
                        return None;
                    }
                    parameters.push((parameter, false, true, 2));
                }
            }
        }
        if skip {
            return None;
        }
        let mut assigned: Vec<Vec<Primitive>> = vec![Vec::new(); parameters.len()];
        let mut seen_keywords = FxHashSet::default();
        let mut positional = 0;
        for (keyword, kind) in arguments {
            let index = if let Some(keyword) = keyword {
                if !seen_keywords.insert(keyword) {
                    return Some(false);
                }
                parameters
                    .iter()
                    .position(|(parameter, positional_only, _, kind)| {
                        *kind == 0
                            && !*positional_only
                            && parameter
                                .name()
                                .is_some_and(|name| name.text() == keyword.as_str())
                    })
                    .or_else(|| parameters.iter().position(|p| p.3 == 2))
            } else {
                while positional < parameters.len()
                    && parameters[positional].3 == 0
                    && parameters[positional].2
                {
                    positional += 1;
                }
                if positional < parameters.len() && parameters[positional].3 == 0 {
                    let index = positional;
                    positional += 1;
                    Some(index)
                } else {
                    parameters.iter().position(|p| p.3 == 1)
                }
            };
            let Some(index) = index else {
                return Some(false);
            };
            if parameters[index].3 == 0 && !assigned[index].is_empty() {
                return Some(false);
            }
            assigned[index].push(kind.clone());
        }
        for ((parameter, _, _, kind), actuals) in parameters.iter().zip(assigned) {
            if actuals.is_empty() {
                if *kind == 0 && parameter.default().is_none() {
                    return Some(false);
                }
                continue;
            }
            let expected = self.selected_annotation(file, scope, &parameter.annotation()?)?;
            for actual in actuals {
                if !self.selected_type_matches(&actual, &expected)? {
                    return Some(false);
                }
            }
        }
        Some(true)
    }
}

fn selected_integer(text: &str) -> Option<i64> {
    let text = text.replace('_', "");
    let (radix, digits) = if text.starts_with("0x") || text.starts_with("0X") {
        (16, &text[2..])
    } else if text.starts_with("0o") || text.starts_with("0O") {
        (8, &text[2..])
    } else if text.starts_with("0b") || text.starts_with("0B") {
        (2, &text[2..])
    } else {
        (10, text.as_str())
    };
    i64::from_str_radix(digits, radix).ok()
}
