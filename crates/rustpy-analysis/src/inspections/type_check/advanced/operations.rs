use super::*;
/// Type expressions have dedicated checks, but evaluated annotation metadata
/// retains ordinary runtime obligations. Never infer roles from source spelling.
fn runtime_position(inf: &Inferer<'_>, file: &Arc<FileInfo>, node: &SyntaxNode) -> bool {
    let ancestors = node.ancestors().take(65).collect::<Vec<_>>();
    let Some(boundary) = ancestors.iter().position(|ancestor| {
        matches!(
            ancestor.kind(),
            K::ANNOTATION | K::RETURN_ANNOTATION | K::TYPE_ALIAS_STMT
        )
    }) else {
        return true;
    };
    if ancestors[boundary].kind() == K::TYPE_ALIAS_STMT {
        return false;
    }
    let postponed = ast::Module::cast(file.parse.syntax_node()).is_some_and(|module| {
        module.statements().any(|statement| {
            matches!(statement,
            ast::Stmt::ImportFrom(import) if import.module_path() == "__future__"
                && import.aliases().any(|alias| alias.name() == "annotations"))
        })
    });
    if postponed {
        return false;
    }
    // Local variable annotations are not evaluated, unlike class/module fields
    // and function parameter/return annotations without postponement.
    if let Some(assignment) = ancestors[boundary]
        .ancestors()
        .find_map(ast::AnnAssignStmt::cast)
    {
        if assignment
            .syntax()
            .ancestors()
            .skip(1)
            .find(|owner| matches!(owner.kind(), K::FUNC_DEF | K::CLASS_DEF))
            .is_some_and(|owner| owner.kind() == K::FUNC_DEF)
        {
            return false;
        }
    }
    let target = node.text_range();
    for expression in ancestors[..boundary]
        .iter()
        .rev()
        .filter_map(|node| ast::Expr::cast(node.clone()))
    {
        match expression {
            ast::Expr::String(_) => return false, // forward annotation payload
            ast::Expr::Call(_) => return true,
            ast::Expr::Binary(binary) if binary.op().is_some_and(|op| op.kind() != K::PIPE) => {
                return true;
            }
            ast::Expr::Subscript(subscript) => {
                let Some(head) = subscript.object() else {
                    return false;
                };
                let scope = file.scopes.scope_at(head.text_range().start());
                let origin = canonical(inf, file, &head, scope);
                let Some((module, name)) = origin.filter(|(module, _)| {
                    matches!(
                        module.as_str(),
                        "typing" | "typing_extensions" | "builtins" | "collections.abc"
                    )
                }) else {
                    return true; // user subscriptions retain runtime argument semantics
                };
                if matches!(module.as_str(), "typing" | "typing_extensions")
                    && matches!(name.as_str(), "Annotated" | "Literal")
                {
                    let Some(index) = subscript.index() else {
                        return false;
                    };
                    let args = if let Some(ast::Expr::Tuple(tuple)) = ast::Expr::cast(index.clone())
                    {
                        tuple.elements().collect::<Vec<_>>()
                    } else if let Some(expr) = ast::Expr::cast(index.clone()) {
                        vec![expr]
                    } else {
                        index.children().filter_map(ast::Expr::cast).collect()
                    };
                    if args.iter().enumerate().any(|(index, argument)| {
                        (name == "Literal" || index > 0)
                            && argument.text_range().contains_range(target)
                    }) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}
pub(super) fn node(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    node: &SyntaxNode,
    version: (u32, u32),
    sink: &mut Sink,
) {
    if !runtime_position(inf, file, node) {
        return;
    }
    protocol_results(inf, file, node, version, sink);
    match node.kind() {
        K::CALL_EXPR => {
            if let Some(call) = ast::CallExpr::cast(node.clone()) {
                call_check(inf, file, &call, sink);
            }
        }
        K::AWAIT_EXPR => {
            if let Some(expression) = node.children().find_map(ast::Expr::cast) {
                let ty = inf.type_of_expr(file, &expression);
                if closed(inf, &ty) && slot(inf, &ty, "__await__") == Some(false) {
                    warn(
                        node,
                        "PyTypeChecker",
                        format!("Object of type '{ty}' is not awaitable"),
                        sink,
                    );
                }
            }
        }
        K::FOR_STMT | K::COMP_FOR => {
            let (value, asynchronous) = if let Some(f) = ast::ForStmt::cast(node.clone()) {
                (f.iter(), f.is_async())
            } else if let Some(f) = ast::CompFor::cast(node.clone()) {
                (f.iter(), f.is_async())
            } else {
                return;
            };
            if let Some(value) = value {
                let ty = inf.type_of_expr(file, &value);
                let invalid = if asynchronous {
                    slot(inf, &ty, "__aiter__") == Some(false)
                } else {
                    slot(inf, &ty, "__iter__") == Some(false)
                        && slot(inf, &ty, "__getitem__") == Some(false)
                };
                if closed(inf, &ty) && invalid {
                    warn(
                        value.syntax(),
                        "PyTypeChecker",
                        format!(
                            "Object of type '{ty}' is not {}iterable",
                            if asynchronous { "async " } else { "" }
                        ),
                        sink,
                    );
                }
            }
        }
        K::WITH_STMT => {
            if let Some(statement) = ast::WithStmt::cast(node.clone()) {
                for item in statement.items() {
                    if let Some(value) = item.context_expr() {
                        let ty = inf.type_of_expr(file, &value);
                        let (enter, exit) = if statement.is_async() {
                            ("__aenter__", "__aexit__")
                        } else {
                            ("__enter__", "__exit__")
                        };
                        if closed(inf, &ty)
                            && (slot(inf, &ty, enter) == Some(false)
                                || slot(inf, &ty, exit) == Some(false))
                        {
                            warn(
                                value.syntax(),
                                "PyTypeChecker",
                                format!(
                                    "Object of type '{ty}' does not implement {enter} and {exit}"
                                ),
                                sink,
                            );
                        }
                    }
                }
            }
        }
        K::SUBSCRIPT_EXPR => {
            if let Some(subscript) = ast::SubscriptExpr::cast(node.clone()) {
                subscript_check(inf, file, &subscript, sink);
            }
        }
        K::UNARY_EXPR => {
            if let Some(unary) = ast::UnaryExpr::cast(node.clone()) {
                if let (Some(op), Some(value)) = (unary.op(), unary.operand()) {
                    let method = match op.kind() {
                        K::PLUS => "__pos__",
                        K::MINUS => "__neg__",
                        K::TILDE => "__invert__",
                        _ => return,
                    };
                    let ty = inf.type_of_expr(file, &value);
                    if closed(inf, &ty) && slot(inf, &ty, method) == Some(false) {
                        warn(
                            node,
                            "PyTypeChecker",
                            format!("Unary operator '{}' is not supported by '{ty}'", op.text()),
                            sink,
                        );
                    }
                }
            }
        }
        K::BINARY_EXPR => {
            if let Some(binary) = ast::BinaryExpr::cast(node.clone()) {
                if let (Some(left), Some(op), Some(right)) =
                    (binary.lhs(), binary.op(), binary.rhs())
                {
                    let methods = match op.text() {
                        "+" => ("__add__", "__radd__"),
                        "-" => ("__sub__", "__rsub__"),
                        "*" => ("__mul__", "__rmul__"),
                        "/" => ("__truediv__", "__rtruediv__"),
                        "//" => ("__floordiv__", "__rfloordiv__"),
                        "%" => ("__mod__", "__rmod__"),
                        "**" => ("__pow__", "__rpow__"),
                        "&" => ("__and__", "__rand__"),
                        "|" => ("__or__", "__ror__"),
                        "^" => ("__xor__", "__rxor__"),
                        "<<" => ("__lshift__", "__rlshift__"),
                        ">>" => ("__rshift__", "__rrshift__"),
                        "@" => ("__matmul__", "__rmatmul__"),
                        _ => return,
                    };
                    let l = inf.type_of_expr(file, &left);
                    let r = inf.type_of_expr(file, &right);
                    if closed(inf, &l)
                        && closed(inf, &r)
                        && accepts(inf, &l, methods.0, &r) == Some(false)
                        && accepts(inf, &r, methods.1, &l) == Some(false)
                    {
                        warn(
                            node,
                            "PyTypeChecker",
                            format!(
                                "Operator '{}' is not supported between '{l}' and '{r}'",
                                op.text()
                            ),
                            sink,
                        );
                    }
                }
            }
        }
        K::COMPARE_EXPR => {
            if let Some(compare) = ast::CompareExpr::cast(node.clone()) {
                let values: Vec<_> = compare.operands().collect();
                for (index, op) in compare.ops().enumerate() {
                    let (Some(left), Some(right)) = (values.get(index), values.get(index + 1))
                    else {
                        break;
                    };
                    let l = inf.type_of_expr(file, left);
                    let r = inf.type_of_expr(file, right);
                    if !closed(inf, &l) || !closed(inf, &r) {
                        continue;
                    }
                    let invalid = match op.text().as_str() {
                        "in" | "not in" => {
                            slot(inf, &r, "__contains__") == Some(false)
                                && slot(inf, &r, "__iter__") == Some(false)
                                && slot(inf, &r, "__getitem__") == Some(false)
                        }
                        "<" => {
                            accepts(inf, &l, "__lt__", &r) == Some(false)
                                && accepts(inf, &r, "__gt__", &l) == Some(false)
                        }
                        ">" => {
                            accepts(inf, &l, "__gt__", &r) == Some(false)
                                && accepts(inf, &r, "__lt__", &l) == Some(false)
                        }
                        "<=" => {
                            accepts(inf, &l, "__le__", &r) == Some(false)
                                && accepts(inf, &r, "__ge__", &l) == Some(false)
                        }
                        ">=" => {
                            accepts(inf, &l, "__ge__", &r) == Some(false)
                                && accepts(inf, &r, "__le__", &l) == Some(false)
                        }
                        _ => false,
                    };
                    if invalid {
                        warn(
                            node,
                            "PyTypeChecker",
                            format!(
                                "Comparison '{}' is not supported between '{l}' and '{r}'",
                                op.text()
                            ),
                            sink,
                        );
                    }
                }
            }
        }
        K::ATTRIBUTE_EXPR => {
            if let Some(attribute) = ast::AttributeExpr::cast(node.clone()) {
                if let Some(object) = attribute.object() {
                    let ty = inf.type_of_expr(file, &object);
                    if matches!(&ty,Type::Union(arms) if arms.contains(&Type::None))
                        && inf
                            .lookup_member(&Type::None, &attribute.attr_text())
                            .is_none()
                    {
                        warn(
                            node,
                            "PyTypeChecker",
                            format!(
                                "Possible None value has no attribute '{}'",
                                attribute.attr_text()
                            ),
                            sink,
                        );
                    }
                }
            }
        }
        _ => {}
    }
}
fn accepts(inf: &Inferer<'_>, receiver: &Type, name: &str, arg: &Type) -> Option<bool> {
    if slot(inf, receiver, name) == Some(false) {
        return Some(false);
    }
    let c = class(inf, receiver)?;
    let value = inf.lookup_member(&Type::Class(c, Vec::new()), name)?;
    let signatures: Vec<_> = match &value {
        Type::Function(s) | Type::Callable(s) | Type::BoundMethod(s, _) => vec![s],
        Type::Overloaded(s) if s.len() <= 32 => s.iter().collect(),
        _ => return None,
    };
    let mut uncertain = false;
    for signature in signatures {
        let params: Vec<_> = signature
            .params
            .iter()
            .filter(|p| {
                matches!(
                    p.kind,
                    rustpy_semantic::types::ParamKind::Normal
                        | rustpy_semantic::types::ParamKind::PositionalOnly
                )
            })
            .collect();
        let Some(expected) = params.last() else {
            uncertain = true;
            continue;
        };
        if !known(&expected.ty) {
            uncertain = true;
            continue;
        }
        if inf.is_compatible(arg, &expected.ty) {
            return Some(true);
        }
    }
    if uncertain {
        None
    } else {
        Some(false)
    }
}
fn subscript_check(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    subscript: &ast::SubscriptExpr,
    sink: &mut Sink,
) {
    let Some(object) = subscript.object() else {
        return;
    };
    let scope = file.scopes.scope_at(object.text_range().start());
    if canonical(inf, file, &object, scope).is_some_and(|(module, name)| {
        matches!(module.as_str(), "typing" | "typing_extensions")
            && matches!(name.as_str(), "Generic" | "Protocol")
    }) {
        return;
    }
    let ty = inf.type_of_expr(file, &object);
    if matches!(ty, Type::Class(_, _)) {
        return;
    }
    let method = target_method(subscript.syntax()).unwrap_or("__getitem__");
    if closed(inf, &ty) && slot(inf, &ty, method) == Some(false) {
        warn(
            subscript.syntax(),
            "PyTypeChecker",
            if method == "__getitem__" {
                format!("Object of type '{ty}' is not subscriptable")
            } else {
                format!("Object of type '{ty}' does not support {method}")
            },
            sink,
        );
        return;
    }
    let Some(index) = subscript.index_expr() else {
        return;
    };
    let index_ty = inf.type_of_expr(file, &index);
    if method == "__getitem__"
        && closed(inf, &ty)
        && known(&index_ty)
        && accepts(inf, &ty, method, &index_ty) == Some(false)
    {
        warn(
            index.syntax(),
            "PyTypeChecker",
            format!("Index type '{index_ty}' is incompatible with __getitem__"),
            sink,
        );
    }
    if let Some(c) = class(inf, &ty) {
        if let Type::Instance(_, args) = &ty {
            if let Some(fields) = inf.typed_dict_fields(&c, args) {
                if let Some(key) = inf.typed_dict_key(file, &index) {
                    if !fields.iter().any(|(name, _, _, _)| name == &key) {
                        warn(
                            index.syntax(),
                            "PyTypedDict",
                            format!("TypedDict '{cname}' has no key '{key}'", cname = c.name),
                            sink,
                        );
                    }
                }
            }
        }
        let sequence = ["list", "tuple", "str", "bytes", "bytearray"]
            .iter()
            .any(|name| inf.builtin_class(name).as_ref() == Some(&c));
        if sequence
            && !matches!(index, ast::Expr::Slice(_))
            && known(&index_ty)
            && !inf.is_compatible(&index_ty, &inf.instance_of("int"))
            && slot(inf, &index_ty, "__index__") != Some(true)
        {
            warn(
                index.syntax(),
                "PyTypeChecker",
                format!("Sequence index must be an integer or slice, got '{index_ty}'"),
                sink,
            );
        }
    }
}
fn call_check(inf: &Inferer<'_>, file: &Arc<FileInfo>, call: &ast::CallExpr, sink: &mut Sink) {
    let Some(callee) = call.callee() else { return };
    let ty = inf.type_of_expr(file, &callee);
    if closed(inf, &ty)
        && !matches!(ty, Type::Class(..))
        && slot(inf, &ty, "__call__") == Some(false)
        // Special-form factories (`TypedDict("TD", {...})`) produce classes.
        && !matches!(
            inf.type_of_expr(file, &ast::Expr::Call(call.clone())),
            Type::Class(..)
        )
    {
        warn(
            callee.syntax(),
            "PyTypeChecker",
            format!("Object of type '{ty}' is not callable"),
            sink,
        );
    }
    if let Type::Class(class, _) = &ty {
        if protocol(inf, class) {
            warn(
                call.syntax(),
                "PyTypeChecker",
                "Protocols cannot be instantiated",
                sink,
            );
        }
    }
    let Some(arguments) = call.arg_list() else {
        return;
    };
    let arguments: Vec<_> = arguments.positional().collect();
    if builtin(inf, file, &callee, "len") {
        if let Some(arg) = arguments.first() {
            let ty = inf.type_of_expr(file, arg);
            if closed(inf, &ty) && slot(inf, &ty, "__len__") == Some(false) {
                warn(
                    arg.syntax(),
                    "PyTypeChecker",
                    format!("Object of type '{ty}' has no length"),
                    sink,
                );
            }
        }
    }
    if builtin(inf, file, &callee, "isinstance") || builtin(inf, file, &callee, "issubclass") {
        if let Some(arg) = arguments.get(1) {
            for part in arg
                .syntax()
                .descendants()
                .filter_map(ast::SubscriptExpr::cast)
            {
                warn(
                    part.syntax(),
                    "PyTypeChecker",
                    "Parameterized generics cannot be used in isinstance() or issubclass()",
                    sink,
                );
            }
        }
    }
    // TypedDict construction and literal arguments need schema obligations,
    // rather than nominal dict-versus-TypedDict compatibility alone.
    if let Type::Class(c, args) = &ty {
        if let Some(fields) = inf.typed_dict_fields(c, args) {
            let kws: Vec<_> = call
                .arg_list()
                .into_iter()
                .flat_map(|a| a.keywords().collect::<Vec<_>>())
                .collect();
            let has_star = call.arg_list().is_some_and(|a| {
                a.args()
                    .any(|a| matches!(a, ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_)))
            });
            if !has_star && arguments.is_empty() {
                for (name, _, required, _) in &fields {
                    if *required && !kws.iter().any(|k| k.name_text() == name.as_str()) {
                        warn(
                            call.syntax(),
                            "PyTypedDict",
                            format!("Missing required TypedDict key '{name}'"),
                            sink,
                        );
                    }
                }
                for keyword in kws {
                    if !fields
                        .iter()
                        .any(|(name, _, _, _)| name.as_str() == keyword.name_text())
                    {
                        warn(
                            keyword.syntax(),
                            "PyTypedDict",
                            format!("Unknown TypedDict key '{}'", keyword.name_text()),
                            sink,
                        );
                    }
                }
            }
        }
    }
}

fn protocol_results(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    node: &SyntaxNode,
    version: (u32, u32),
    sink: &mut Sink,
) {
    if let Some(statement) = ast::WithStmt::cast(node.clone()) {
        if statement.is_async() {
            for item in statement.items() {
                if let Some(value) = item.context_expr() {
                    let ty = inf.type_of_expr(file, &value);
                    for method in ["__aenter__", "__aexit__"] {
                        if let Some(ret) = slot_return(inf, &ty, method) {
                            if closed(inf, &ret) && slot(inf, &ret, "__await__") == Some(false) {
                                warn(
                                    value.syntax(),
                                    "PyTypeChecker",
                                    format!("{method} must return an awaitable, got '{ret}'"),
                                    sink,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    let iterator = if let Some(statement) = ast::ForStmt::cast(node.clone()) {
        if statement.is_async() {
            statement.iter()
        } else {
            None
        }
    } else if let Some(clause) = ast::CompFor::cast(node.clone()) {
        if clause.is_async() {
            clause.iter()
        } else {
            None
        }
    } else {
        None
    };
    if let Some(value) = iterator {
        let receiver = inf.type_of_expr(file, &value);
        if let Some(iterator) = slot_return(inf, &receiver, "__aiter__") {
            if version >= (3, 7)
                && closed(inf, &iterator)
                && slot(inf, &iterator, "__anext__") == Some(false)
            {
                warn(
                    value.syntax(),
                    "PyTypeChecker",
                    "__aiter__ must return an async iterator",
                    sink,
                );
            }
            if let Some(ret) = slot_return(inf, &iterator, "__anext__") {
                if closed(inf, &ret) && slot(inf, &ret, "__await__") == Some(false) {
                    warn(
                        value.syntax(),
                        "PyTypeChecker",
                        format!("__anext__ must return an awaitable, got '{ret}'"),
                        sink,
                    );
                }
            }
        }
    }
    if node.kind() == K::AWAIT_EXPR {
        if let Some(value) = node.children().find_map(ast::Expr::cast) {
            let receiver = inf.type_of_expr(file, &value);
            if let Some(ret) = slot_return(inf, &receiver, "__await__") {
                if closed(inf, &ret) && slot(inf, &ret, "__next__") == Some(false) {
                    warn(
                        value.syntax(),
                        "PyTypeChecker",
                        "__await__ must return an iterator",
                        sink,
                    );
                }
            }
        }
    }
}

fn target_method(node: &SyntaxNode) -> Option<&'static str> {
    let mut cursor = node.clone();
    while let Some(parent) = cursor.parent() {
        match parent.kind() {
            K::PAREN_EXPR | K::TUPLE_EXPR | K::LIST_EXPR | K::STAR_EXPR => {
                cursor = parent;
                continue;
            }
            K::ASSIGN_STMT => {
                return ast::AssignStmt::cast(parent)
                    .filter(|s| {
                        s.targets()
                            .iter()
                            .any(|t| t.text_range().contains_range(node.text_range()))
                    })
                    .map(|_| "__setitem__");
            }
            K::ANN_ASSIGN_STMT => {
                return ast::AnnAssignStmt::cast(parent)
                    .and_then(|s| s.target())
                    .filter(|t| t.text_range().contains_range(node.text_range()))
                    .map(|_| "__setitem__");
            }
            K::DEL_STMT => return Some("__delitem__"),
            K::FOR_STMT => {
                return ast::ForStmt::cast(parent)
                    .and_then(|s| s.target())
                    .filter(|t| t.text_range().contains_range(node.text_range()))
                    .map(|_| "__setitem__");
            }
            K::COMP_FOR => {
                return ast::CompFor::cast(parent)
                    .and_then(|s| s.target())
                    .filter(|t| t.text_range().contains_range(node.text_range()))
                    .map(|_| "__setitem__");
            }
            K::WITH_ITEM => {
                return ast::WithItem::cast(parent)
                    .and_then(|s| s.target())
                    .filter(|t| t.text_range().contains_range(node.text_range()))
                    .map(|_| "__setitem__");
            }
            _ => return None,
        }
    }
    None
}
