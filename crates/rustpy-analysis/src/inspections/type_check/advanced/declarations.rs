use super::*;
use rustpy_semantic::{
    classes::MemberKind,
    infer::SynthKind,
    types::{ParamKind, Signature},
};
use std::collections::HashMap;
pub(super) fn node(inf: &Inferer<'_>, file: &Arc<FileInfo>, node: &SyntaxNode, sink: &mut Sink) {
    if let Some(class) = ast::ClassDef::cast(node.clone()) {
        class_contract(inf, file, &class, sink);
    }
    if let Some(function) = ast::FuncDef::cast(node.clone()) {
        dunder(inf, file, &function, sink);
    }
    if matches!(node.kind(), K::BLOCK | K::MODULE) {
        overloads(inf, file, node, sink);
    }
}
fn decorated(inf: &Inferer<'_>, file: &Arc<FileInfo>, f: &ast::FuncDef, name: &str) -> bool {
    f.decorators().filter_map(|d| d.expr()).any(|e| {
        typing(
            inf,
            file,
            &e,
            file.scopes.scope_at(e.text_range().start()),
            name,
        )
    })
}
fn class_contract(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    definition: &ast::ClassDef,
    sink: &mut Sink,
) {
    let c = ClassRef {
        file: file.id,
        range: definition.text_range(),
        name: definition.name_text().into(),
    };
    let Some(info) = inf.class_info(&c) else {
        return;
    };
    let ancestors = inf.mro(&c);
    for method in definition.methods().take(256) {
        let name = method.name_text();
        if !decorated(inf, file, &method, "override") && name != "__init__" && name != "__new__" {
            if let Some(base) = ancestors.iter().skip(1).find(|base| {
                inf.class_info(base)
                    .is_some_and(|i| i.members.contains_key(name.as_str()))
            }) {
                let actual = inf.lookup_member(&Type::Instance(c.clone(), Vec::new()), &name);
                let projected = inf
                    .mro_with_args(&c, &[])
                    .into_iter()
                    .find(|(owner, _)| owner == base)
                    .map(|(_, args)| args)
                    .unwrap_or_default();
                let expected = inf.lookup_member(&Type::Instance(base.clone(), projected), &name);
                if let (Some(actual), Some(expected)) = (actual, expected) {
                    if let (Some(a), Some(e)) = (actual.signature(), expected.signature()) {
                        if incompatible_signature(inf, a, e) {
                            warn(
                                method.syntax(),
                                "PyIncompatibleOverride",
                                format!(
                                    "Method '{name}' is incompatible with the signature or return type of '{}.{name}'",
                                    base.name
                                ),
                                sink,
                            );
                        }
                    }
                }
            }
        }
        property_contract(inf, file, definition, &method, sink);
    }
    // Slot synthesis may have inserted an InstanceVar before a later class
    // assignment. Read explicit class defaults from their source owner as well.
    let class_defaults = definition
        .body()
        .into_iter()
        .flat_map(|body| body.statements())
        .flat_map(|statement| match statement {
            ast::Stmt::Assign(assignment) => assignment.targets(),
            ast::Stmt::AnnAssign(assignment) if assignment.value().is_some() => {
                assignment.target().into_iter().collect()
            }
            _ => Vec::new(),
        })
        .filter_map(|target| match target {
            ast::Expr::Name(name) => Some(name.name_text()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    if let Some(slots) = info.members.get("__slots__").and_then(|m| m.value) {
        if let Some(value) =
            rustpy_semantic::edit_helpers::node_with_range(&file.parse.syntax_node(), slots)
        {
            for string in value.descendants().filter_map(ast::StringExpr::cast) {
                let name = string.value();
                if let Some(member) = info.members.get(name.as_str()) {
                    if class_defaults.contains(&name)
                        || member.value.is_some()
                        || matches!(
                            member.kind,
                            MemberKind::Method
                                | MemberKind::Property
                                | MemberKind::ClassMethod
                                | MemberKind::StaticMethod
                        )
                    {
                        warn(
                            string.syntax(),
                            "PySlotsConflict",
                            format!("Slot '{name}' conflicts with a class variable or descriptor"),
                            sink,
                        );
                    }
                }
            }
        }
    }
    if info.members.contains_key("__eq__")
        && !info.members.contains_key("__hash__")
        && !inf.has_unknown_base(&c)
        && info.keywords.is_empty()
    {
        sink.push(
            definition.text_range(),
            Severity::WeakWarning,
            "PyHashDefinition",
            "Defining __eq__ without __hash__ makes instances unhashable",
            Vec::new(),
        );
    }
    if let Some(spec) = inf.synth_spec(&c) {
        if matches!(spec.kind, SynthKind::Dataclass | SynthKind::NamedTuple) {
            if spec.init && !spec.kw_only {
                let sig = inf.synthesized_init(&c, &spec);
                let mut seen = false;
                for parameter in &sig.params {
                    if !matches!(
                        parameter.kind,
                        ParamKind::Normal | ParamKind::PositionalOnly
                    ) {
                        continue;
                    }
                    if seen && !parameter.has_default && parameter.name != "self" {
                        warn(
                            definition.syntax(),
                            "PyDataclass",
                            format!(
                                "Non-default field '{}' follows a default field",
                                parameter.name
                            ),
                            sink,
                        );
                        break;
                    }
                    seen |= parameter.has_default;
                }
            }
            let initvars = inherited_initvars(inf, &c);
            if let Some(body) = definition.body() {
                for statement in body.statements() {
                    if let ast::Stmt::AnnAssign(field) = statement {
                        let Some(target) = field.target() else {
                            continue;
                        };
                        let Some(annotation) = field.annotation() else {
                            continue;
                        };
                        let field_name = target.syntax().text().to_string();
                        if spec.kind == SynthKind::NamedTuple && field_name.starts_with('_') {
                            warn(
                                target.syntax(),
                                "PyNamedTuple",
                                "NamedTuple field names cannot start with an underscore",
                                sink,
                            );
                        }
                        let head = match &annotation {
                            ast::Expr::Subscript(s) => s.object(),
                            _ => Some(annotation.clone()),
                        };
                        let classvar = head.as_ref().is_some_and(|h| {
                            typing(
                                inf,
                                file,
                                h,
                                file.scopes.scope_at(h.text_range().start()),
                                "ClassVar",
                            )
                        });
                        if spec.kind == SynthKind::Dataclass && !classvar {
                            if let Some(value) = field.value() {
                                if matches!(
                                    value.clone().unparen(),
                                    ast::Expr::List(_) | ast::Expr::Dict(_) | ast::Expr::Set(_)
                                ) {
                                    warn(
                                        value.syntax(),
                                        "PyDataclass",
                                        "Mutable dataclass field default; use field(default_factory=...)",
                                        sink,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            if spec.kind == SynthKind::Dataclass {
                for decorator in definition.decorators().filter_map(|d| d.expr()) {
                    if let ast::Expr::Call(call) = decorator {
                        if call.callee().is_some_and(|head|canonical(inf,file,&head,file.scopes.scope_at(head.text_range().start())).is_some_and(|(m,n)|m=="dataclasses"&&n=="dataclass"))&&call.arg_list().is_some_and(|a|a.keywords().any(|k|k.name_text()=="order"&&matches!(k.value(),Some(ast::Expr::Literal(l)) if l.kind()==Some(K::TRUE_KW)))) && ["__lt__","__le__","__gt__","__ge__"].iter().any(|name|info.members.contains_key(*name)){warn(definition.syntax(),"PyDataclass","order=True cannot be combined with explicitly defined ordering methods",sink);}
                    }
                }
                if let Some(post) = definition
                    .methods()
                    .find(|m| m.name_text() == "__post_init__")
                {
                    if let Some(params) = post.params() {
                        let values: Vec<_> = params.params().collect();
                        if !values.iter().any(|p| matches!(p, ast::AnyParam::Star(_)))
                            && values.len() != initvars + 1
                        {
                            warn(
                                post.syntax(),
                                "PyDataclass",
                                format!(
                                    "__post_init__ expects self and {initvars} InitVar argument(s)"
                                ),
                                sink,
                            );
                        }
                    }
                }
            }
        }
    }
    if info.is_enum_like
        && inf.mro(&c).iter().any(|base| {
            inf.db.file(base.file).is_some_and(|owner| {
                owner.is_stub
                    && owner.path.starts_with("<typeshed>")
                    && owner.module_name.as_deref() == Some("enum")
            })
        })
        && !info.members.contains_key("_ignore_")
    {
        let mut values = HashMap::new();
        for (name, member) in &info.members {
            if name.starts_with('_') || member.kind != MemberKind::ClassVar {
                continue;
            }
            let Some(value) = member
                .value
                .and_then(|r| {
                    rustpy_semantic::edit_helpers::node_with_range(&file.parse.syntax_node(), r)
                })
                .and_then(ast::Expr::cast)
            else {
                continue;
            };
            let primitive = match inf.type_of_expr(file, &value) {
                Type::Literal(value) => Some(format!("{value:?}")),
                _ => None,
            };
            if let Some(value) = primitive {
                if let Some(prior) = values.insert(value, name.clone()) {
                    sink.push(
                        member.range,
                        Severity::WeakWarning,
                        "PyEnum",
                        format!("Enum member '{name}' aliases the value of '{prior}'"),
                        Vec::new(),
                    );
                }
            }
        }
        if let Some(scope) = file.scopes.scope_for_node(definition.text_range()) {
            for (name, ids) in &file.scopes.scope(scope).bindings {
                if !name.starts_with('_')
                    && ids
                        .iter()
                        .filter(|id| {
                            matches!(
                                file.scopes.binding(**id).kind,
                                rustpy_semantic::scope::BindingKind::Assignment
                            )
                        })
                        .count()
                        > 1
                {
                    warn(
                        definition.syntax(),
                        "PyEnum",
                        format!("Enum member '{name}' is defined more than once"),
                        sink,
                    );
                }
            }
        }
    }
}
fn incompatible_signature(inf: &Inferer<'_>, actual: &Signature, expected: &Signature) -> bool {
    if actual.params.len() > 128 || expected.params.len() > 128 {
        return false;
    }
    if !actual.params.iter().all(|p| known(&p.ty)) || !expected.params.iter().all(|p| known(&p.ty))
    {
        return false;
    }
    let a: Vec<_> = actual
        .params
        .iter()
        .filter(|p| matches!(p.kind, ParamKind::Normal | ParamKind::PositionalOnly))
        .collect();
    let e: Vec<_> = expected
        .params
        .iter()
        .filter(|p| matches!(p.kind, ParamKind::Normal | ParamKind::PositionalOnly))
        .collect();
    let variadic = actual.params.iter().any(|p| p.kind == ParamKind::VarArgs);
    let kwargs = actual.params.iter().any(|p| p.kind == ParamKind::KwArgs);
    if !variadic && a.len() < e.len() {
        return true;
    }
    if a.iter().filter(|p| !p.has_default).count() > e.iter().filter(|p| !p.has_default).count() {
        return true;
    }
    for (index, base) in e.iter().enumerate() {
        if let Some(child) = a.get(index) {
            if (base.kind == ParamKind::Normal && child.kind == ParamKind::PositionalOnly)
                || (base.kind == ParamKind::Normal && !kwargs && base.name != child.name)
                || (base.has_default && !child.has_default)
                || !inf.is_compatible(&base.ty, &child.ty)
            {
                return true;
            }
        }
    }
    for base in expected
        .params
        .iter()
        .filter(|p| p.kind == ParamKind::KeywordOnly)
    {
        let child = actual.params.iter().find(|p| {
            p.name == base.name && matches!(p.kind, ParamKind::Normal | ParamKind::KeywordOnly)
        });
        match child {
            Some(child) => {
                if (base.has_default && !child.has_default)
                    || !inf.is_compatible(&base.ty, &child.ty)
                {
                    return true;
                }
            }
            None => {
                if !kwargs {
                    return true;
                }
            }
        }
    }
    if actual.params.iter().any(|p| {
        p.kind == ParamKind::KeywordOnly
            && !p.has_default
            && !expected.params.iter().any(|base| base.name == p.name)
    }) {
        return true;
    }
    if expected.params.iter().any(|p| p.kind == ParamKind::VarArgs) && !variadic {
        return true;
    }
    if expected.params.iter().any(|p| p.kind == ParamKind::KwArgs) && !kwargs {
        return true;
    }
    known(&actual.ret) && known(&expected.ret) && !inf.is_compatible(&actual.ret, &expected.ret)
}
fn property_contract(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    class: &ast::ClassDef,
    method: &ast::FuncDef,
    sink: &mut Sink,
) {
    for decorator in method.decorators().filter_map(|d| d.expr()) {
        if let ast::Expr::Attribute(attribute) = decorator {
            if matches!(attribute.attr_text().as_str(), "setter" | "deleter") {
                let Some(ast::Expr::Name(name)) = attribute.object() else {
                    continue;
                };
                let getter = class
                    .methods()
                    .filter(|m| {
                        m.name_text() == name.name_text()
                            && m.text_range().start() < method.text_range().start()
                    })
                    .any(|m| {
                        m.decorators()
                            .filter_map(|d| d.expr())
                            .any(|e| builtin(inf, file, &e, "property"))
                    });
                let c = ClassRef {
                    file: file.id,
                    range: class.text_range(),
                    name: class.name_text().into(),
                };
                let inherited = inf.mro(&c).iter().skip(1).any(|c| {
                    inf.class_info(c)
                        .and_then(|i| i.members.get(name.name_text().as_str()).cloned())
                        .is_some_and(|m| m.kind == MemberKind::Property)
                });
                if !getter && !inherited {
                    // A resolved ordinary descriptor may define its own setter
                    // decorator API. Its spelling is not property authority.
                    let scope = file.scopes.scope_at(name.text_range().start());
                    if inf
                        .resolve_name(file, scope, &name.name_text())
                        .is_some_and(|definition| {
                            definition.binding().range.start() < method.text_range().start()
                                && !matches!(
                                    definition.binding().kind,
                                    rustpy_semantic::scope::BindingKind::Function
                                )
                        })
                    {
                        continue;
                    }
                    warn(
                        method.syntax(),
                        "PyProperty",
                        "Property setter/deleter has no preceding getter",
                        sink,
                    );
                }
                let expected = if attribute.attr_text() == "setter" {
                    2
                } else {
                    1
                };
                if method
                    .params()
                    .is_some_and(|p| p.params().count() != expected)
                {
                    warn(
                        method.syntax(),
                        "PyProperty",
                        format!(
                            "Property {} must have {expected} parameter(s) including self",
                            attribute.attr_text()
                        ),
                        sink,
                    );
                }
            }
        }
    }
    let property = method
        .decorators()
        .filter_map(|d| d.expr())
        .any(|e| builtin(inf, file, &e, "property"));
    if property
        && method
            .decorators()
            .filter_map(|d| d.expr())
            .any(|e| builtin(inf, file, &e, "classmethod"))
    {
        warn(
            method.syntax(),
            "PyProperty",
            "@property cannot wrap a classmethod",
            sink,
        );
    }
}
fn dunder(inf: &Inferer<'_>, file: &Arc<FileInfo>, method: &ast::FuncDef, sink: &mut Sink) {
    if !method
        .syntax()
        .parent()
        .and_then(|n| n.parent())
        .is_some_and(|n| n.kind() == K::CLASS_DEF)
    {
        return;
    }
    let name = method.name_text();
    let expected = match name.as_str() {
        "__enter__" | "__aenter__" | "__len__" | "__bool__" | "__iter__" | "__next__"
        | "__await__" | "__index__" | "__hash__" => Some(1),
        "__exit__" | "__aexit__" => Some(4),
        "__getattr__" | "__getattribute__" | "__delattr__" => Some(2),
        "__setattr__" => Some(3),
        _ => None,
    };
    if let (Some(expected), Some(params)) = (expected, method.params()) {
        let args: Vec<_> = params.params().collect();
        if !args
            .iter()
            .any(|p| matches!(p, ast::AnyParam::Star(_) | ast::AnyParam::StarStar(_)))
            && args.len() != expected
        {
            warn(
                method.syntax(),
                "PyDunderSignature",
                format!("{name} must accept {expected} positional argument(s), including self"),
                sink,
            );
        }
    }
    if let Some(annotation) = method.return_annotation() {
        let actual = inf.annotation_type(file, &annotation);
        let expected = match name.as_str() {
            "__len__" | "__index__" | "__hash__" => Some(inf.instance_of("int")),
            "__bool__" => Some(inf.instance_of("bool")),
            "__str__" | "__repr__" => Some(inf.instance_of("str")),
            "__bytes__" => Some(inf.instance_of("bytes")),
            _ => None,
        };
        if let Some(expected) = expected {
            if known(&actual) && !inf.is_compatible(&actual, &expected) {
                warn(
                    annotation.syntax(),
                    "PyDunderSignature",
                    format!("{name} must return '{expected}', not '{actual}'"),
                    sink,
                );
            }
        }
    }
    if name == "__init__" {
        for node in method.syntax().descendants().filter(|n| {
            own(n, method.syntax()) && matches!(n.kind(), K::YIELD_EXPR | K::YIELD_FROM_EXPR)
        }) {
            warn(
                &node,
                "PyDunderSignature",
                "__init__ cannot be a generator",
                sink,
            );
        }
    }
}
fn overloads(inf: &Inferer<'_>, file: &Arc<FileInfo>, block: &SyntaxNode, sink: &mut Sink) {
    let mut groups: HashMap<String, Vec<ast::FuncDef>> = HashMap::new();
    for function in block.children().filter_map(ast::FuncDef::cast) {
        groups
            .entry(function.name_text())
            .or_default()
            .push(function);
    }
    for (name, functions) in groups {
        let variants: Vec<_> = functions
            .iter()
            .filter(|f| decorated(inf, file, f, "overload"))
            .collect();
        if variants.is_empty() {
            continue;
        }
        if variants.len() == 1 {
            warn(
                variants[0].syntax(),
                "PyOverloads",
                format!(
                    "Overloaded function '{name}' must have at least two overload declarations"
                ),
                sink,
            );
        }
        let implementations: Vec<_> = functions
            .iter()
            .filter(|f| !decorated(inf, file, f, "overload"))
            .collect();
        let protocol = block
            .parent()
            .and_then(ast::ClassDef::cast)
            .is_some_and(|c| {
                let class = ClassRef {
                    file: file.id,
                    range: c.text_range(),
                    name: c.name_text().into(),
                };
                inf.class_info(&class).is_some_and(|i| i.is_protocol)
            });
        let abstract_group = variants.iter().all(|function| {
            function
                .decorators()
                .filter_map(|d| d.expr())
                .any(|expression| {
                    canonical(
                        inf,
                        file,
                        &expression,
                        file.scopes.scope_at(expression.text_range().start()),
                    )
                    .is_some_and(|(module, name)| module == "abc" && name == "abstractmethod")
                })
        });
        if !file.is_stub && !protocol && !abstract_group && implementations.is_empty() {
            warn(
                variants[0].syntax(),
                "PyOverloads",
                format!("Overloaded function '{name}' has no implementation"),
                sink,
            );
        }
        if let Some(implementation) = implementations.last() {
            for variant in variants {
                if variant.text_range().start() > implementation.text_range().start() {
                    warn(
                        variant.syntax(),
                        "PyOverloads",
                        "Overload declarations must precede the implementation",
                        sink,
                    );
                }
                let actual = inf.signature(file, implementation, None);
                let expected = inf.signature(file, variant, None);
                let mut actual_parameters = (*actual).clone();
                actual_parameters.ret = Type::Unknown;
                let mut expected_parameters = (*expected).clone();
                expected_parameters.ret = Type::Unknown;
                if incompatible_signature(inf, &actual_parameters, &expected_parameters)
                    || (known(&actual.ret)
                        && known(&expected.ret)
                        && !inf.is_compatible(&expected.ret, &actual.ret))
                {
                    warn(
                        implementation.syntax(),
                        "PyOverloads",
                        format!(
                            "Implementation of '{name}' cannot accept every overload signature"
                        ),
                        sink,
                    );
                    break;
                }
            }
        }
    }
}

fn inherited_initvars(inf: &Inferer<'_>, class: &ClassRef) -> usize {
    let mut fields = HashMap::new();
    for owner in inf.mro(class).iter().rev().take(32) {
        let Some(file) = inf.db.file(owner.file) else {
            continue;
        };
        let Some(definition) =
            rustpy_semantic::edit_helpers::node_with_range(&file.parse.syntax_node(), owner.range)
                .and_then(ast::ClassDef::cast)
        else {
            continue;
        };
        if let Some(body) = definition.body() {
            for statement in body.statements().take(256) {
                if let ast::Stmt::AnnAssign(field) = statement {
                    if let (Some(target), Some(annotation)) = (field.target(), field.annotation()) {
                        let head = if let ast::Expr::Subscript(s) = annotation {
                            s.object()
                        } else {
                            None
                        };
                        let initvar = head.is_some_and(|h| {
                            canonical(inf, &file, &h, file.scopes.scope_at(h.text_range().start()))
                                .is_some_and(|(m, n)| m == "dataclasses" && n == "InitVar")
                        });
                        fields.insert(target.syntax().text().to_string(), initvar);
                    }
                }
            }
        }
    }
    fields.values().filter(|v| **v).count()
}
