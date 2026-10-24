use super::*;
use rustpy_semantic::types::ParamKind;
pub(super) fn node(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    node: &SyntaxNode,
    version: (u32, u32),
    sink: &mut Sink,
) {
    if let Some(raise) = ast::RaiseStmt::cast(node.clone()) {
        if let Some(exception) = raise.exception() {
            let ty = inf.type_of_expr(file, &exception);
            if exception_valid(inf, &ty, false) == Some(false) {
                warn(
                    exception.syntax(),
                    "PyException",
                    format!("Exceptions must derive from BaseException, got '{ty}'"),
                    sink,
                );
            }
        }
    }
    if let Some(statement) = ast::TryStmt::cast(node.clone()) {
        let mut prior: Vec<ClassRef> = Vec::new();
        for clause in statement.except_clauses() {
            if let Some(exception) = clause.exception_type() {
                let ty = inf.type_of_expr(file, &exception);
                if exception_valid(inf, &ty, true) == Some(false) {
                    warn(
                        exception.syntax(),
                        "PyException",
                        "Except clause requires an exception class or tuple of exception classes",
                        sink,
                    );
                }
                let classes = match ty {
                    Type::Class(c, _) => vec![c],
                    Type::Tuple(values) => values
                        .into_iter()
                        .filter_map(|t| {
                            if let Type::Class(c, _) = t {
                                Some(c)
                            } else {
                                None
                            }
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                for class in &classes {
                    if !inf.has_unknown_base(class)
                        && prior.iter().any(|p| inf.mro(class).contains(p))
                    {
                        warn(
                            clause.syntax(),
                            "PyUnreachableExcept",
                            format!(
                                "Exception '{}' is already caught by an earlier handler",
                                class.name
                            ),
                            sink,
                        );
                    }
                }
                prior.extend(classes);
            }
        }
    }
    if let Some(assign) = ast::AssignStmt::cast(node.clone()) {
        if let Some(value) = assign.value() {
            for target in assign.targets() {
                annotated_store(inf, file, &target, &value, sink);
                balance(inf, file, &target, &value, sink, 0);
            }
            if let ast::Expr::Tuple(tuple) = value.clone().unparen() {
                if tuple.elements().count() == 1
                    && !tuple
                        .syntax()
                        .children_with_tokens()
                        .filter_map(|e| e.into_token())
                        .any(|t| t.kind() == K::L_PAREN)
                {
                    sink.push(
                        tuple.text_range(),
                        Severity::WeakWarning,
                        "PyTrailingCommaTuple",
                        "A trailing comma creates a one-element tuple",
                        Vec::new(),
                    );
                }
            }
            if let (Some(ast::Expr::Name(target)), ast::Expr::Binary(binary)) =
                (assign.targets().first(), value.clone().unparen())
            {
                if let (Some(ast::Expr::Name(left)), Some(right), Some(op)) =
                    (binary.lhs(), binary.rhs(), binary.op())
                {
                    let ty = inf.type_of_expr(file, &ast::Expr::Name(left.clone()));
                    let immutable = class(inf, &ty).is_some_and(|c| {
                        ["int", "float", "complex"]
                            .iter()
                            .any(|name| inf.builtin_class(name).as_ref() == Some(&c))
                    });
                    if immutable && target.name_text() == left.name_text() {
                        sink.push(
                            node.text_range(),
                            Severity::WeakWarning,
                            "PyAugmentedAssignment",
                            "Augmented assignment can be used",
                            vec![QuickFix {
                                title: "Use augmented assignment".into(),
                                edits: vec![(
                                    node.text_range(),
                                    format!(
                                        "{} {}= {}",
                                        target.name_text(),
                                        op.text(),
                                        right.syntax().text()
                                    ),
                                )],
                            }],
                        );
                    }
                }
            }
        }
    }
    if let Some(compare) = ast::CompareExpr::cast(node.clone()) {
        let operands: Vec<_> = compare.operands().collect();
        for (index, op) in compare.ops().enumerate() {
            if !matches!(op.text().as_str(), "==" | "!=") {
                continue;
            }
            let (Some(left), Some(right)) = (operands.get(index), operands.get(index + 1)) else {
                continue;
            };
            let l = inf.type_of_expr(file, left);
            let r = inf.type_of_expr(file, right);
            if matches!(
                l,
                Type::Function(_) | Type::BoundMethod(_, _) | Type::Callable(_)
            ) && matches!(right, ast::Expr::Literal(_))
            {
                warn(
                    node,
                    "PyComparisonWithCallable",
                    "Comparing a callable value without calling it",
                    sink,
                );
            }
            if inf.is_compatible(&l, &inf.instance_of("bool"))
                && class(inf, &l) == inf.builtin_class("bool")
                && matches!(r, Type::Literal(LiteralValue::Bool(_)))
            {
                warn(
                    node,
                    "PyRedundantBoolean",
                    "Comparison to a boolean literal can be simplified",
                    sink,
                );
            }
            if let ast::Expr::Call(call) = left {
                if call
                    .callee()
                    .is_some_and(|c| builtin(inf, file, &c, "type"))
                    && matches!(r, Type::Class(..))
                {
                    sink.push(
                        node.text_range(),
                        Severity::WeakWarning,
                        "PyTypeComparison",
                        "Consider isinstance() if subclasses should also be accepted",
                        Vec::new(),
                    );
                }
            }
        }
    }
    if let Some(call) = ast::CallExpr::cast(node.clone()) {
        call_contract(inf, file, &call, sink);
    }
    if let Some(function) = ast::FuncDef::cast(node.clone()) {
        if function.name_text() == "__init__" {
            wrong_init(inf, file, &function, sink);
        }
    }
    if version.0 == 2 {
        if let Some(class) = ast::ClassDef::cast(node.clone()) {
            if class.bases().is_empty() {
                warn(
                    node,
                    "PyOldStyleClass",
                    "Class has no object base and is old-style on Python 2",
                    sink,
                );
            }
        }
    }
    if let Some(finally) = ast::FinallyClause::cast(node.clone()) {
        if let Some(body) = finally.body() {
            for transfer in body.syntax().descendants().filter(|n| {
                matches!(n.kind(), K::RETURN_STMT | K::BREAK_STMT | K::CONTINUE_STMT)
                    && own(n, node)
                    && (n.kind() == K::RETURN_STMT
                        || !n
                            .ancestors()
                            .skip(1)
                            .take_while(|a| a != node)
                            .any(|a| matches!(a.kind(), K::FOR_STMT | K::WHILE_STMT)))
            }) {
                sink.push(
                    transfer.text_range(),
                    Severity::Warning,
                    "PyFinallyTransfer",
                    "Control transfer in finally may discard a pending exception or return value",
                    Vec::new(),
                );
            }
        }
    }
    if let Some(binary) = ast::BinaryExpr::cast(node.clone()) {
        if binary
            .op()
            .is_some_and(|op| matches!(op.kind(), K::SLASH | K::SLASH_SLASH | K::PERCENT))
        {
            if let Some(right) = binary.rhs() {
                let zero = matches!(
                    inf.type_of_expr(file, &right),
                    Type::Literal(LiteralValue::Int(0))
                );
                if zero {
                    warn(
                        right.syntax(),
                        "PyDivisionByZero",
                        "Division or modulo by zero",
                        sink,
                    );
                }
            }
        }
    }
}
fn exception_valid(inf: &Inferer<'_>, ty: &Type, class_only: bool) -> Option<bool> {
    if !known(ty) {
        return None;
    }
    if let Type::Tuple(values) = ty {
        if !class_only {
            return Some(false);
        }
        let mut uncertain = false;
        for ty in values {
            match exception_valid(inf, ty, true) {
                Some(false) => return Some(false),
                None => uncertain = true,
                _ => {}
            }
        }
        return if uncertain { None } else { Some(true) };
    }
    if class_only && !matches!(ty, Type::Class(..)) {
        return Some(false);
    }
    let c = class(inf, ty)?;
    if inf.has_unknown_base(&c) {
        return None;
    }
    let base = inf.builtin_class("BaseException")?;
    Some(inf.mro(&c).contains(&base))
}
fn balance(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    target: &ast::Expr,
    value: &ast::Expr,
    sink: &mut Sink,
    depth: u8,
) {
    if depth > 16 {
        return;
    }
    let elements: Vec<_> = match target.clone().unparen() {
        ast::Expr::Tuple(t) => t.elements().collect(),
        ast::Expr::List(t) => t.elements().collect(),
        _ => return,
    };
    if elements.len() > 256 {
        return;
    }
    let length = match value.clone().unparen() {
        ast::Expr::Tuple(t) => {
            if t.elements().any(|e| matches!(e, ast::Expr::Star(_))) {
                return;
            }
            Some(t.elements().count())
        }
        ast::Expr::List(t) => {
            if t.elements().any(|e| matches!(e, ast::Expr::Star(_))) {
                return;
            }
            Some(t.elements().count())
        }
        _ => match inf.type_of_expr(file, value) {
            Type::Tuple(t) => Some(t.len()),
            _ => None,
        },
    };
    if let Some(length) = length {
        let starred = elements.iter().any(|e| matches!(e, ast::Expr::Star(_)));
        let minimum = elements.len() - usize::from(starred);
        if (!starred && length != elements.len()) || (starred && length < minimum) {
            warn(
                target.syntax(),
                "PyTupleAssignmentBalance",
                format!(
                    "Unpacking target requires {}{} value(s), got {length}",
                    if starred { "at least " } else { "" },
                    minimum
                ),
                sink,
            );
            return;
        }
        let values: Vec<_> = match value.clone().unparen() {
            ast::Expr::Tuple(t) => t.elements().collect(),
            ast::Expr::List(t) => t.elements().collect(),
            _ => Vec::new(),
        };
        if !starred {
            for (target, value) in elements.iter().zip(values) {
                balance(inf, file, target, &value, sink, depth + 1);
            }
        }
    }
}
fn call_contract(inf: &Inferer<'_>, file: &Arc<FileInfo>, call: &ast::CallExpr, sink: &mut Sink) {
    let Some(callee) = call.callee() else { return };
    let Some(args) = call.arg_list() else { return };
    for (name, literal) in [("list", "[]"), ("dict", "{}")] {
        if builtin(inf, file, &callee, name) && args.args().next().is_none() {
            sink.push(
                call.text_range(),
                Severity::WeakWarning,
                "PyCollectionLiteral",
                format!("Use a {name} literal"),
                vec![QuickFix {
                    title: "Replace with literal".into(),
                    edits: vec![(call.text_range(), literal.into())],
                }],
            );
        }
    }
    // Literal conversion is safe only for a syntactic, pack-free sequence.
    let arguments: Vec<_> = args.args().take(2).collect();
    if arguments.len() == 1 {
        if let ast::AnyArg::Positional(argument) = &arguments[0] {
            if let Some(value) = argument.expr() {
                let elements = match value.unparen() {
                    ast::Expr::Tuple(value) => Some(value.elements().collect::<Vec<_>>()),
                    ast::Expr::List(value) => Some(value.elements().collect::<Vec<_>>()),
                    _ => None,
                };
                if let Some(elements) = elements.filter(|v| v.len() <= 128) {
                    let name = if builtin(inf, file, &callee, "list") {
                        Some("list")
                    } else if builtin(inf, file, &callee, "set") && !elements.is_empty() {
                        Some("set")
                    } else {
                        None
                    };
                    if let Some(name) = name {
                        if elements.iter().all(|e| e.syntax().kind() != K::STAR_EXPR) {
                            let contents = elements
                                .iter()
                                .map(|e| e.syntax().text().to_string())
                                .collect::<Vec<_>>()
                                .join(", ");
                            let replacement = if name == "list" {
                                format!("[{contents}]")
                            } else {
                                format!("{{{contents}}}")
                            };
                            sink.push(
                                call.text_range(),
                                Severity::WeakWarning,
                                "PyCollectionLiteral",
                                format!("Use a {name} literal"),
                                vec![QuickFix {
                                    title: "Replace with literal".into(),
                                    edits: vec![(call.text_range(), replacement)],
                                }],
                            );
                        }
                    }
                }
            }
        }
    }
    let value = inf.type_of_expr(file, &callee);
    let Some(signature) = value.signature() else {
        return;
    };
    let Some(def) = &signature.def else { return };
    let Some(owner) = inf.db.file(def.file) else {
        return;
    };
    let Some(function) =
        rustpy_semantic::edit_helpers::node_with_range(&owner.parse.syntax_node(), def.range)
            .and_then(ast::FuncDef::cast)
    else {
        return;
    };
    let Some(params) = function.params() else {
        return;
    };
    if args
        .args()
        .any(|a| matches!(a, ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_)))
    {
        return;
    }
    let pos = args.positional().count();
    let kw: Vec<_> = args.keywords().map(|k| k.name_text()).collect();
    for (index, param) in signature
        .params
        .iter()
        .filter(|p| matches!(p.kind, ParamKind::Normal | ParamKind::PositionalOnly))
        .enumerate()
    {
        if index < pos || kw.iter().any(|n| n == param.name.as_str()) {
            continue;
        }
        if let Some(default) = params
            .plain_params()
            .find(|p| p.name_text() == param.name.as_str())
            .and_then(|p| p.default())
        {
            if matches!(
                default.clone().unparen(),
                ast::Expr::List(_) | ast::Expr::Dict(_) | ast::Expr::Set(_)
            ) {
                sink.push(
                    call.text_range(),
                    Severity::WeakWarning,
                    "PyMutableDefaultCall",
                    format!("Call uses shared mutable default for '{}'", param.name),
                    Vec::new(),
                );
            }
        }
    }
}
fn wrong_init(inf: &Inferer<'_>, file: &Arc<FileInfo>, function: &ast::FuncDef, sink: &mut Sink) {
    let Some(owner) = function
        .syntax()
        .parent()
        .and_then(|b| b.parent())
        .and_then(ast::ClassDef::cast)
    else {
        return;
    };
    let c = ClassRef {
        file: file.id,
        range: owner.text_range(),
        name: owner.name_text().into(),
    };
    if inf.has_unknown_base(&c) {
        return;
    }
    for call in function
        .syntax()
        .descendants()
        .filter(|n| own(n, function.syntax()))
        .filter_map(ast::CallExpr::cast)
    {
        let Some(ast::Expr::Attribute(attribute)) = call.callee() else {
            continue;
        };
        if attribute.attr_text() != "__init__" {
            continue;
        }
        let Some(object) = attribute.object() else {
            continue;
        };
        if let Type::Class(base, _) = inf.type_of_expr(file, &object) {
            if !inf.mro(&c).contains(&base) {
                warn(
                    call.syntax(),
                    "PyInitCall",
                    format!("Calling __init__ of unrelated class '{}'", base.name),
                    sink,
                );
            }
        }
    }
}

fn annotated_store(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    target: &ast::Expr,
    value: &ast::Expr,
    sink: &mut Sink,
) {
    if !super::super::precise_arg(inf, file, value) {
        return;
    }
    let expected = match target.clone().unparen() {
        ast::Expr::Name(name) => {
            let scope = file.scopes.scope_at(name.text_range().start());
            file.scopes
                .bindings_in(scope, &name.name_text())
                .iter()
                .map(|id| file.scopes.binding(*id))
                .filter(|b| b.range.start() < target.text_range().start())
                .find_map(|b| b.annotation)
                .map(|range| inf.annotation_type_range(file, range))
        }
        ast::Expr::Attribute(attribute) => {
            let Some(object) = attribute.object() else {
                return;
            };
            let receiver = inf.type_of_expr(file, &object);
            let Some(class) = class(inf, &receiver) else {
                return;
            };
            let has_declared_field = inf.mro(&class).iter().any(|owner| {
                inf.class_info(owner).is_some_and(|i| {
                    i.members
                        .get(attribute.attr_text().as_str())
                        .is_some_and(|m| {
                            m.annotation.is_some()
                                && m.kind != rustpy_semantic::classes::MemberKind::Property
                        })
                })
            });
            if has_declared_field {
                inf.lookup_member(&receiver, &attribute.attr_text())
            } else {
                None
            }
        }
        _ => None,
    };
    let Some(expected) = expected else { return };
    let actual = inf.type_of_expr(file, value);
    if let Some(message) = typed_dict_error(inf, file, value, &expected) {
        warn(value.syntax(), "PyTypedDict", message, sink);
    } else if known(&actual) && known(&expected) && !inf.is_compatible(&actual, &expected) {
        warn(
            value.syntax(),
            "PyTypeChecker",
            format!("Expected type '{expected}', got '{actual}' in assignment"),
            sink,
        );
    }
}
