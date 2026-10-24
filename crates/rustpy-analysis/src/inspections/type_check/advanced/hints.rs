use super::*;
use rustpy_semantic::scope::BindingKind;
/// Shared authority for one recursive annotation validation walk. Scope, source
/// anchor, depth and the shared traversal budget remain explicit per step.
#[derive(Clone, Copy)]
struct ValidationContext<'a, 'db> {
    inf: &'a Inferer<'db>,
    file: &'a Arc<FileInfo>,
    version: (u32, u32),
}
pub(super) fn node(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    node: &SyntaxNode,
    version: (u32, u32),
    sink: &mut Sink,
) {
    let context = ValidationContext { inf, file, version };
    if matches!(node.kind(), K::ANNOTATION | K::RETURN_ANNOTATION) {
        if let Some(expression) = node.children().find_map(ast::Expr::cast) {
            let scope = file.scopes.scope_at(expression.text_range().start());
            unbound_typevars(inf, file, &expression, sink);
            check(&context, &expression, scope, None, &mut 512, 0, sink);
        }
    }
    if let Some(assignment) = ast::AnnAssignStmt::cast(node.clone()) {
        if let Some(annotation) = assignment.annotation() {
            let scope = file.scopes.scope_at(annotation.text_range().start());
            if typing(inf, file, &annotation, scope, "TypeAlias") {
                if let Some(value) = assignment.value() {
                    check(&context, &value, scope, None, &mut 512, 0, sink);
                }
            }
        }
    }
    if let Some(function) = ast::FuncDef::cast(node.clone()) {
        function_hints(inf, file, &function, sink);
    }
    if let Some(alias) = ast::TypeAliasStmt::cast(node.clone()) {
        if let Some(value) = alias.value() {
            let scope = file.scopes.scope_at(value.text_range().start());
            check(&context, &value, scope, None, &mut 512, 0, sink);
        }
        let mut names = std::collections::HashSet::new();
        let mut default = false;
        for param in node
            .children()
            .filter(|n| n.kind() == K::TYPE_PARAM_LIST)
            .flat_map(|n| n.children().collect::<Vec<_>>())
        {
            if let Some(name) = param
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| t.kind() == K::IDENT)
            {
                if !names.insert(name.text().to_string()) {
                    warn(&param, "PyTypeHints", "Duplicate type parameter", sink);
                }
            }
            let has_default = param.children().any(|n| n.kind() == K::DEFAULT_VALUE);
            if default && !has_default {
                warn(
                    &param,
                    "PyTypeHints",
                    "A non-default type parameter follows a default type parameter",
                    sink,
                );
            }
            default |= has_default;
        }
    }
}
fn issue(expr: &ast::Expr, anchor: Option<TextRange>, message: impl Into<String>, sink: &mut Sink) {
    sink.push(
        anchor.unwrap_or(expr.text_range()),
        Severity::Warning,
        "PyTypeHints",
        message,
        Vec::new(),
    );
}
fn check(
    context: &ValidationContext<'_, '_>,
    expr: &ast::Expr,
    scope: ScopeId,
    anchor: Option<TextRange>,
    budget: &mut usize,
    depth: u8,
    sink: &mut Sink,
) {
    let ValidationContext { inf, file, version } = *context;
    if *budget == 0 || depth > 32 {
        return;
    }
    *budget -= 1;
    let expr = expr.clone().unparen();
    if let ast::Expr::String(string) = &expr {
        if string.is_bytes() {
            issue(
                &expr,
                anchor,
                "A bytes literal is not a type annotation",
                sink,
            );
            return;
        }
        if string.syntax().text().len() > rustpy_core::TextSize::new(65_536) {
            return;
        }
        let Ok(text) = rustpy_syntax::string_literal::text(&string.raw_text()) else {
            return;
        };
        let parse = rustpy_syntax::parse_expression(&text);
        if !parse.errors().is_empty() {
            issue(
                &expr,
                anchor,
                "Invalid syntax in forward type annotation",
                sink,
            );
            return;
        }
        if let Some(inner) = parse.syntax_node().children().find_map(ast::Expr::cast) {
            check(
                context,
                &inner,
                scope,
                Some(anchor.unwrap_or(expr.text_range())),
                budget,
                depth + 1,
                sink,
            );
        }
        return;
    }
    if let ast::Expr::Subscript(subscript) = &expr {
        let Some(head) = subscript.object() else {
            return;
        };
        let Some(index) = subscript.index() else {
            return;
        };
        let args: Vec<_> = if let Some(ast::Expr::Tuple(tuple)) = ast::Expr::cast(index.clone()) {
            tuple.elements().collect()
        } else {
            index
                .children()
                .filter_map(ast::Expr::cast)
                .collect::<Vec<_>>()
        };
        let args = if args.is_empty() {
            ast::Expr::cast(index).into_iter().collect::<Vec<_>>()
        } else {
            args
        };
        let origin = canonical(inf, file, &head, scope);
        let name = origin
            .as_ref()
            .filter(|(m, _)| {
                matches!(
                    m.as_str(),
                    "typing" | "typing_extensions" | "builtins" | "collections.abc"
                )
            })
            .map(|(_, n)| n.as_str());
        let arity = match name {
            Some(
                "Optional" | "List" | "Set" | "FrozenSet" | "Sequence" | "MutableSequence"
                | "Iterable" | "Iterator" | "Awaitable" | "AsyncIterator" | "ClassVar" | "Final"
                | "Required" | "NotRequired" | "ReadOnly" | "TypeGuard" | "TypeIs" | "Type"
                | "type" | "list" | "set" | "frozenset",
            ) => Some(1),
            Some("Dict" | "Mapping" | "MutableMapping" | "dict") => Some(2),
            Some("Callable") => Some(2),
            _ => None,
        };
        if arity.is_some_and(|arity| args.len() != arity) {
            issue(
                &expr,
                anchor,
                format!(
                    "Type '{}' expects {} argument(s), got {}",
                    name.unwrap_or(""),
                    arity.unwrap_or(0),
                    args.len()
                ),
                sink,
            );
            return;
        }
        if name == Some("Annotated") && args.len() < 2 {
            issue(
                &expr,
                anchor,
                "Annotated requires a type and at least one metadata value",
                sink,
            );
            return;
        }
        if name == Some("Union") && args.len() == 1 {
            sink.push(
                anchor.unwrap_or(expr.text_range()),
                Severity::WeakWarning,
                "PyRedundantUnion",
                "Union with one argument can be replaced by that argument",
                Vec::new(),
            );
        }
        if name == Some("Literal") {
            return;
        }
        if matches!(name, Some("Generic" | "Protocol")) {
            for argument in &args {
                let ty = inf.annotation_type(file, argument);
                if known(&ty)
                    && !matches!(
                        ty,
                        Type::TypeVar(_)
                            | Type::DeclaredTypeVar(_)
                            | Type::ParamSpec(_)
                            | Type::TypeVarTuple(_)
                            | Type::Unpack(_)
                    )
                {
                    issue(
                        argument,
                        anchor,
                        "Generic/Protocol parameters must be type variables",
                        sink,
                    );
                }
            }
            return;
        }
        if name == Some("Unpack") {
            if args.len() != 1 {
                issue(
                    &expr,
                    anchor,
                    "Unpack requires exactly one type argument",
                    sink,
                );
            }
            return;
        }
        if matches!(name, Some("tuple" | "Tuple"))
            && args
                .iter()
                .any(|a| matches!(a,ast::Expr::Literal(l) if l.kind()==Some(K::ELLIPSIS)))
            && !(args.len() == 2
                && matches!(&args[1],ast::Expr::Literal(l) if l.kind()==Some(K::ELLIPSIS)))
        {
            issue(
                &expr,
                anchor,
                "Tuple ellipsis is only valid as tuple[T, ...]",
                sink,
            );
            return;
        }
        if name == Some("Annotated") {
            if let Some(arg) = args.first() {
                check(context, arg, scope, anchor, budget, depth + 1, sink);
            }
            return;
        }
        if name == Some("Callable") {
            if let Some(first) = args.first() {
                match first.clone().unparen() {
                    ast::Expr::List(list) => {
                        for p in list.elements() {
                            check(context, &p, scope, anchor, budget, depth + 1, sink);
                        }
                    }
                    ast::Expr::Literal(l) if l.kind() == Some(K::ELLIPSIS) => {}
                    ast::Expr::Name(_) | ast::Expr::Subscript(_) => {
                        let first_type = inf.annotation_type(file, first);
                        if !matches!(
                            first_type,
                            Type::ParamSpec(_)
                                | Type::Concatenate(_, _)
                                | Type::ParameterList(_)
                                | Type::Unknown
                                | Type::Any
                        ) {
                            issue(
                                first,
                                anchor,
                                "Callable's first argument must be a parameter list, ParamSpec or ellipsis",
                                sink,
                            );
                        }
                    }
                    _ => issue(
                        first,
                        anchor,
                        "Callable's first argument must be a parameter list, ParamSpec or ellipsis",
                        sink,
                    ),
                }
            }
            if let Some(ret) = args.get(1) {
                check(context, ret, scope, anchor, budget, depth + 1, sink);
            }
            return;
        }
        if name == Some("Concatenate") {
            if args.len() < 2 {
                issue(
                    &expr,
                    anchor,
                    "Concatenate requires leading types and a final ParamSpec",
                    sink,
                );
                return;
            }
            if let Some(last) = args.last() {
                let value = inf.annotation_type(file, last);
                if !matches!(value, Type::ParamSpec(_) | Type::Unknown | Type::Any)
                    && !matches!(last,ast::Expr::Literal(l) if l.kind()==Some(K::ELLIPSIS))
                {
                    issue(
                        last,
                        anchor,
                        "The last Concatenate argument must be a ParamSpec or ellipsis",
                        sink,
                    );
                }
            }
        }
        if let Some((module, name)) = &origin {
            if version >= (3, 9)
                && module == "typing"
                && matches!(
                    name.as_str(),
                    "List" | "Dict" | "Tuple" | "Set" | "FrozenSet" | "Type"
                )
            {
                let replacement = match name.as_str() {
                    "List" => "list",
                    "Dict" => "dict",
                    "Tuple" => "tuple",
                    "Set" => "set",
                    "FrozenSet" => "frozenset",
                    _ => "type",
                };
                sink.push(
                    anchor.unwrap_or(head.text_range()),
                    Severity::WeakWarning,
                    "PyModernTypeHint",
                    format!("Use built-in '{replacement}' instead of typing.{name} on Python 3.9+"),
                    if anchor.is_none() {
                        vec![QuickFix {
                            title: format!("Replace with {replacement}"),
                            edits: vec![(head.text_range(), replacement.into())],
                        }]
                    } else {
                        Vec::new()
                    },
                );
            }
        }
        if origin.is_none() {
            let ty = if anchor.is_none() {
                inf.type_of_expr(file, &head)
            } else {
                Type::Unknown
            };
            if let Type::Class(class, _) = ty {
                if let Some(info) = inf.class_info(&class) {
                    if !info.type_params.is_empty()
                        && generic_count_invalid(inf, &class, args.len())
                    {
                        issue(
                            &expr,
                            anchor,
                            format!(
                                "Generic '{}' expects {} type arguments, got {}",
                                class.name,
                                info.type_params.len(),
                                args.len()
                            ),
                            sink,
                        );
                    }
                }
            }
        }
        for arg in args {
            if matches!(arg,ast::Expr::Literal(ref l) if l.kind()==Some(K::ELLIPSIS)) {
                continue;
            }
            if matches!(&arg,ast::Expr::Tuple(t) if t.elements().next().is_none()) {
                continue;
            }
            check(context, &arg, scope, anchor, budget, depth + 1, sink);
        }
        return;
    }
    if let Some((module, name)) = canonical(inf, file, &expr, scope) {
        if matches!(module.as_str(), "typing" | "typing_extensions") {
            if name == "Optional" || name == "Annotated" {
                issue(
                    &expr,
                    anchor,
                    format!("{name} requires type arguments"),
                    sink,
                );
            }
            if name == "Self" {
                let class = if let Some(anchor_range) = anchor {
                    file.parse
                        .syntax_node()
                        .covering_element(anchor_range)
                        .parent()
                        .and_then(|n| n.ancestors().find(|a| a.kind() == K::CLASS_DEF))
                } else {
                    expr.syntax().ancestors().find(|n| n.kind() == K::CLASS_DEF)
                };
                if class.is_none() {
                    issue(&expr, anchor, "Self cannot be used outside a class", sink);
                } else if expr
                    .syntax()
                    .ancestors()
                    .find_map(ast::FuncDef::cast)
                    .is_some_and(|f| {
                        f.decorators().any(|d| {
                            d.name_text().is_some_and(|n| {
                                n == "staticmethod" || n == "builtins.staticmethod"
                            })
                        })
                    })
                {
                    issue(
                        &expr,
                        anchor,
                        "Self cannot be used in a static method",
                        sink,
                    );
                }
            }
        }
    }
    match &expr {
        ast::Expr::Call(_)
        | ast::Expr::Dict(_)
        | ast::Expr::Set(_)
        | ast::Expr::List(_)
        | ast::Expr::FString(_) => issue(
            &expr,
            anchor,
            "Expression is not valid in a type annotation",
            sink,
        ),
        ast::Expr::Literal(l) if !matches!(l.kind(), Some(K::NONE_KW | K::ELLIPSIS)) => issue(
            &expr,
            anchor,
            "A literal value is not a type; use Literal[...]",
            sink,
        ),
        ast::Expr::Name(n) => {
            if let Some(definition) = inf.resolve_name(file, scope, &n.name_text()) {
                let binding = definition.binding();
                if matches!(
                    binding.kind,
                    BindingKind::Assignment | BindingKind::Walrus | BindingKind::Param
                ) {
                    let value = inf.type_of_binding(&definition.file, definition.binding);
                    let class_union = matches!(&value,Type::Union(arms) if arms.iter().all(|arm|matches!(arm,Type::Class(..))));
                    if !class_union
                        && known(&value)
                        && !matches!(
                            value,
                            Type::Class(..)
                                | Type::DeclaredTypeVar(_)
                                | Type::TypeVar(_)
                                | Type::ParamSpec(_)
                                | Type::TypeVarTuple(_)
                        )
                    {
                        issue(
                            &expr,
                            anchor,
                            "A variable value is not allowed in a type expression",
                            sink,
                        );
                    }
                }
            }
        }
        ast::Expr::Binary(binary) => {
            if let (Some(left), Some(right)) = (binary.lhs(), binary.rhs()) {
                check(context, &left, scope, anchor, budget, depth + 1, sink);
                check(context, &right, scope, anchor, budget, depth + 1, sink);
            }
        }
        _ => {}
    }
}
fn function_hints(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    function: &ast::FuncDef,
    sink: &mut Sink,
) {
    let Some(params) = function.params() else {
        return;
    };
    let mut occurrences: std::collections::HashMap<String, (usize, TextRange)> =
        std::collections::HashMap::new();
    for annotation in params
        .params()
        .filter_map(|p| p.annotation())
        .chain(function.return_annotation())
    {
        for name in annotation
            .syntax()
            .descendants()
            .filter_map(ast::NameExpr::cast)
        {
            let scope = file.scopes.scope_at(name.text_range().start());
            let Some(def) = inf.resolve_name(file, scope, &name.name_text()) else {
                continue;
            };
            let ty = inf.type_of_binding(&def.file, def.binding);
            if matches!(ty, Type::DeclaredTypeVar(_) | Type::TypeVar(_)) {
                let entry = occurrences
                    .entry(name.name_text())
                    .or_insert((0, name.text_range()));
                entry.0 += 1;
            }
        }
    }
    let in_generic_class = function
        .syntax()
        .ancestors()
        .filter_map(ast::ClassDef::cast)
        .any(|c| {
            c.type_params().is_some()
                || c.bases()
                    .iter()
                    .any(|b| b.syntax().text().to_string().contains("Generic["))
        });
    if !in_generic_class {
        for (name, (count, range)) in occurrences {
            if count == 1 {
                sink.push(
                    range,
                    Severity::WeakWarning,
                    "PyTypeVarMisuse",
                    format!("Type variable '{name}' appears only once in the function signature"),
                    Vec::new(),
                );
            }
        }
    }
    for param in params.params() {
        let Some(annotation) = param.annotation() else {
            continue;
        };
        if let Some(default) = param.default() {
            if matches!(default.clone().unparen(),ast::Expr::Literal(l) if l.kind()==Some(K::NONE_KW))
            {
                let ty = inf.annotation_type(file, &annotation);
                if known(&ty) && !inf.is_compatible(&Type::None, &ty) {
                    warn(
                        annotation.syntax(),
                        "PyTypeHints",
                        "Default None requires an explicit Optional or '| None' annotation",
                        sink,
                    );
                }
            }
        }
        if let ast::Expr::Attribute(attr) = annotation.clone().unparen() {
            if matches!(attr.attr_text().as_str(), "args" | "kwargs")
                && attr
                    .object()
                    .is_some_and(|base| matches!(inf.type_of_expr(file, &base), Type::ParamSpec(_)))
            {
                let valid = matches!(
                    (&param, attr.attr_text().as_str()),
                    (ast::AnyParam::Star(_), "args") | (ast::AnyParam::StarStar(_), "kwargs")
                );
                if !valid {
                    warn(
                        annotation.syntax(),
                        "PyTypeHints",
                        "ParamSpec.args/kwargs are only valid on matching *args/**kwargs parameters",
                        sink,
                    );
                }
            }
        }
    }
}

fn generic_count_invalid(inf: &Inferer<'_>, class: &ClassRef, count: usize) -> bool {
    let Some(info) = inf.class_info(class) else {
        return false;
    };
    let Some(file) = inf.db.file(class.file) else {
        return false;
    };
    let definition =
        rustpy_semantic::edit_helpers::node_with_range(&file.parse.syntax_node(), class.range)
            .and_then(ast::ClassDef::cast);
    if let Some(params) = definition.and_then(|d| d.type_params()) {
        let params: Vec<_> = params.syntax().children().collect();
        let minimum = params
            .iter()
            .filter(|p| {
                p.kind() != K::TYPE_VAR_TUPLE_PARAM
                    && !p.children().any(|n| n.kind() == K::DEFAULT_VALUE)
            })
            .count();
        if count < minimum {
            return true;
        }
        if params.iter().any(|p| p.kind() == K::TYPE_VAR_TUPLE_PARAM) {
            return false;
        }
        return count > params.len();
    }
    count != info.type_params.len()
}

fn typevar_key(value: Type) -> Option<String> {
    match value {
        Type::DeclaredTypeVar(v) => Some(v.key.to_string()),
        Type::TypeVar(name) => Some(name.to_string()),
        _ => None,
    }
}
fn unbound_typevars(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    annotation: &ast::Expr,
    sink: &mut Sink,
) {
    for name in annotation
        .syntax()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .take(256)
    {
        let scope = file.scopes.scope_at(name.text_range().start());
        let Some(def) = inf.resolve_name(file, scope, &name.name_text()) else {
            continue;
        };
        if def.binding().kind == BindingKind::TypeParam {
            continue;
        }
        let Some(key) = typevar_key(inf.type_of_binding(&def.file, def.binding)) else {
            continue;
        };
        let matches = |expression: ast::Expr| {
            expression
                .syntax()
                .descendants()
                .filter_map(ast::NameExpr::cast)
                .take(256)
                .any(|candidate| {
                    inf.resolve_name(
                        file,
                        file.scopes.scope_at(candidate.text_range().start()),
                        &candidate.name_text(),
                    )
                    .and_then(|d| typevar_key(inf.type_of_binding(&d.file, d.binding)))
                    .as_deref()
                        == Some(key.as_str())
                })
        };
        let function = annotation.syntax().ancestors().find_map(ast::FuncDef::cast);
        let bound_by_function = function.is_some_and(|function| {
            function
                .params()
                .into_iter()
                .flat_map(|p| {
                    p.params()
                        .filter_map(|p| p.annotation())
                        .collect::<Vec<_>>()
                })
                .chain(function.return_annotation())
                .any(&matches)
        });
        let bound_by_class = annotation
            .syntax()
            .ancestors()
            .filter_map(ast::ClassDef::cast)
            .any(|class| class.bases().into_iter().any(&matches));
        if !bound_by_function && !bound_by_class {
            warn(
                name.syntax(),
                "PyTypeVarMisuse",
                format!(
                    "Type variable '{}' is unbound in this scope",
                    name.name_text()
                ),
                sink,
            );
        }
    }
}
