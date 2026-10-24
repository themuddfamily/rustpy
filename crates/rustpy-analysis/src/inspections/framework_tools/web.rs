use super::*;
use std::collections::HashSet;
fn keyword(call: &ast::CallExpr, name: &str) -> Option<ast::Expr> {
    call.arg_list()?
        .keywords()
        .find(|k| k.name_text() == name)?
        .value()
}
fn positional(call: &ast::CallExpr) -> Vec<ast::Expr> {
    call.arg_list()
        .map(|l| {
            l.args()
                .filter_map(|a| match a {
                    ast::AnyArg::Positional(p) => p.expr(),
                    _ => None,
                })
                .take(128)
                .collect()
        })
        .unwrap_or_default()
}
fn family(id: &str, prefix: &str) -> bool {
    id == prefix || id.starts_with(&format!("{prefix}."))
}
pub(super) fn run(inf: &Inferer<'_>, file: &Arc<FileInfo>, inputs: &Inputs, sink: &mut Sink) {
    for (path, line, code, message) in &inputs.web_issues {
        if Path::new(path) != file.path || !matches!(code.as_str(), "PyRoute" | "PySqlAlchemyModel")
        {
            continue;
        }
        let source = file.parse.syntax_node().text().to_string();
        let offset = source
            .split_inclusive('\n')
            .take(line.saturating_sub(1))
            .map(str::len)
            .sum::<usize>()
            .min(source.len());
        sink.push(
            TextRange::empty(TextSize::new(offset as u32)),
            Severity::Warning,
            code,
            message.clone(),
            vec![],
        );
    }
    catalog_checks(inf, file, inputs, sink);
    model_configuration(inf, file, sink);
    if let Some(root) = &inputs.root {
        for call in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .take(4096)
        {
            let Some(head) = call.callee() else { continue };
            let Some(id) = identity(inf, file, &head) else {
                continue;
            };
            let position = match id.as_str() {
                "django.shortcuts.render" => 1,
                "django.template.loader.render_to_string" | "flask.render_template" => 0,
                _ => continue,
            };
            let Some(name) = positional(&call).get(position).and_then(literal) else {
                continue;
            };
            if !rustpy_semantic::framework_templates::safe(&name) {
                continue;
            }
            if rustpy_project::frameworks::resource(
                root,
                &inputs.template_directories,
                &name,
                "templates",
            )
            .is_none()
            {
                warn(
                    call.syntax(),
                    "UnresolvedTemplate",
                    format!("Template '{name}' does not exist"),
                    vec![QuickFix {
                        title: format!("Create template '{name}'"),
                        edits: Vec::new(),
                    }],
                    sink,
                );
            }
        }
    }

    let mut endpoints = HashSet::new();
    for function in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .take(2048)
    {
        for decorator in function.decorators().take(32).filter_map(|d| d.expr()) {
            let ast::Expr::Call(call) = decorator else {
                continue;
            };
            let Some(head) = call.callee() else {
                continue;
            };
            let Some(id) = identity(inf, file, &head) else {
                continue;
            };
            let method = id.rsplit('.').next().unwrap_or("");
            let route = (family(&id, "flask") && method == "route")
                || (family(&id, "fastapi")
                    && matches!(
                        method,
                        "get"
                            | "post"
                            | "put"
                            | "patch"
                            | "delete"
                            | "options"
                            | "head"
                            | "api_route"
                            | "websocket"
                    ));
            if route {
                let arguments = positional(&call);
                let path = arguments
                    .first()
                    .cloned()
                    .or_else(|| keyword(&call, "path"));
                if let Some(path) = path.and_then(|e| literal(&e)) {
                    if !path.starts_with('/') {
                        warn(
                            call.syntax(),
                            "PyRoute",
                            "Route paths must begin with '/'",
                            vec![],
                            sink,
                        );
                    }
                    let mut parameters = Vec::new();
                    let (open, close) = if id.starts_with("flask.") {
                        ('<', '>')
                    } else {
                        ('{', '}')
                    };
                    let mut rest = path.as_str();
                    while let Some(start) = rest.find(open) {
                        let after = &rest[start + 1..];
                        let Some(end) = after.find(close) else {
                            warn(
                                call.syntax(),
                                "PyRoute",
                                "Unclosed route parameter",
                                vec![],
                                sink,
                            );
                            break;
                        };
                        parameters.push(
                            if id.starts_with("flask.") {
                                after[..end].rsplit(':').next().unwrap_or("")
                            } else {
                                after[..end].split(':').next().unwrap_or("")
                            }
                            .to_string(),
                        );
                        rest = &after[end + 1..];
                        if parameters.len() > 128 {
                            break;
                        }
                    }
                    let formals: HashSet<_> = function
                        .params()
                        .into_iter()
                        .flat_map(|p| p.params().collect::<Vec<_>>())
                        .filter_map(|p| p.name())
                        .map(|n| n.text().to_string())
                        .collect();
                    let catchall = function.params().is_some_and(|p| {
                        p.params().any(|p| matches!(p, ast::AnyParam::StarStar(_)))
                    });
                    for parameter in parameters {
                        if !catchall && !formals.contains(&parameter) {
                            warn(
                                call.syntax(),
                                "PyRoute",
                                format!(
                                    "Route parameter '{parameter}' is not declared by the endpoint"
                                ),
                                vec![],
                                sink,
                            );
                        }
                    }
                    let methods = keyword(&call, "methods")
                        .and_then(|e| match e {
                            ast::Expr::List(list) => Some(
                                list.elements()
                                    .map(|e| literal(&e))
                                    .collect::<Option<Vec<_>>>()?
                                    .join(","),
                            ),
                            _ => None,
                        })
                        .unwrap_or_else(|| method.to_string());
                    if !endpoints.insert((format!("{}:{methods}", head.syntax().text()), path)) {
                        warn(
                            call.syntax(),
                            "PyRoute",
                            "Duplicate route path and method in this module",
                            vec![],
                            sink,
                        );
                    }
                }
            }

            if route && family(&id, "fastapi") && method != "websocket" {
                response_model(inf, file, &function, &call, sink);
            }

            if family(&id, "pydantic") && matches!(method, "validator" | "field_validator") {
                let models = rustpy_semantic::frameworks::models(file);
                if let Some(model) = models.iter().find(|model| {
                    model.family == rustpy_semantic::frameworks::Family::Pydantic
                        && model
                            .class
                            .range
                            .contains_range(function.syntax().text_range())
                }) {
                    if keyword(&call, "check_fields")
                        .is_none_or(|value| value.syntax().text() != "False")
                    {
                        for argument in positional(&call) {
                            if let Some(name) = literal(&argument) {
                                if name != "*"
                                    && !model.fields.iter().any(|field| field.name == name)
                                {
                                    warn(
                                        argument.syntax(),
                                        "PyPydanticValidator",
                                        format!(
                                            "Validator references unknown model field '{name}'"
                                        ),
                                        Vec::new(),
                                        sink,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            if (family(&id, "pydantic")
                && matches!(method, "validator" | "field_validator" | "root_validator"))
                && function
                    .params()
                    .is_some_and(|p| p.params().filter(|p| p.name().is_some()).count() < 2)
            {
                warn(
                    function.syntax(),
                    "PyPydantic",
                    "Validator must accept its class/instance and value arguments",
                    vec![],
                    sink,
                );
            }
        }
    }
    for call in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .take(4096)
    {
        let Some(callee) = call.callee() else {
            continue;
        };
        let Some(id) = identity(inf, file, &callee) else {
            continue;
        };
        let name = id.rsplit('.').next().unwrap_or("");
        if matches!(id.as_str(), "setuptools.setup" | "distutils.core.setup") {
            for key in ["name", "version", "description", "url"] {
                if let Some(value) = keyword(&call, key) {
                    if matches!(
                        value,
                        ast::Expr::Literal(_) | ast::Expr::List(_) | ast::Expr::Dict(_)
                    ) {
                        warn(
                            value.syntax(),
                            "PySetupSchema",
                            format!("setup {key} must be a string"),
                            vec![],
                            sink,
                        );
                    }
                }
            }
            for key in [
                "install_requires",
                "extras_require",
                "packages",
                "py_modules",
            ] {
                if let Some(value) = keyword(&call, key) {
                    if literal(&value).is_some() {
                        warn(
                            value.syntax(),
                            "PySetupSchema",
                            format!("setup {key} must be a collection"),
                            vec![],
                            sink,
                        );
                    }
                }
            }
            if let Some(ast::Expr::List(values)) = keyword(&call, "install_requires") {
                for value in values.elements().take(1024) {
                    if let Some(requirement) = literal(&value) {
                        if rustpy_packages::requirements_file::parse_named(&requirement).is_err() {
                            warn(
                                value.syntax(),
                                "PyRequirementSpecifier",
                                "Invalid package requirement",
                                vec![],
                                sink,
                            );
                        }
                    }
                }
            }
        }
        if family(&id, "fastapi") && name == "Depends" {
            if let Some(value) = positional(&call).first() {
                if matches!(value, ast::Expr::Call(_)) && noncallable(inf, file, value) {
                    warn(
                        value.syntax(),
                        "PyFastApiDependency",
                        "Depends expects a dependency callable, not its evaluated result",
                        vec![],
                        sink,
                    );
                }
            }
        }
        if family(&id, "pydantic") && name == "ConfigDict" {
            if let Some(arguments) = call.arg_list() {
                for argument in arguments.keywords().take(128) {
                    if let Some(value) = argument.value() {
                        configuration_value(&argument.name_text(), &value, sink);
                    }
                }
            }
        }
        if family(&id, "pydantic") && name == "Field" {
            if keyword(&call, "default_factory").is_some()
                && (keyword(&call, "default").is_some() || !positional(&call).is_empty())
            {
                warn(
                    call.syntax(),
                    "PyPydantic",
                    "Field cannot specify both default and default_factory",
                    vec![],
                    sink,
                );
            }
            if keyword(&call, "default_factory").is_some_and(|e| noncallable(inf, file, &e)) {
                warn(
                    call.syntax(),
                    "PyPydantic",
                    "default_factory expects a callable, not its result",
                    vec![],
                    sink,
                );
            }
        }
        if family(&id, "django.db.models") {
            if name == "CharField"
                && keyword(&call, "max_length").is_some_and(|e| {
                    e.syntax()
                        .text()
                        .to_string()
                        .parse::<i64>()
                        .is_ok_and(|v| v <= 0)
                })
            {
                warn(
                    call.syntax(),
                    "PyDjangoModel",
                    "CharField max_length must be positive",
                    vec![],
                    sink,
                );
            }
            if matches!(name, "ForeignKey" | "OneToOneField")
                && keyword(&call, "on_delete").is_none()
                && positional(&call).len() < 2
            {
                warn(
                    call.syntax(),
                    "PyDjangoModel",
                    "Relational field requires on_delete",
                    vec![],
                    sink,
                );
            }
            for boolean in ["null", "blank", "unique", "primary_key"] {
                if let Some(value) = keyword(&call, boolean) {
                    if literal(&value).is_some() {
                        warn(
                            value.syntax(),
                            "PyDjangoModel",
                            format!("Field option '{boolean}' requires a boolean"),
                            vec![],
                            sink,
                        );
                    }
                }
            }
        }
        if family(&id, "sqlalchemy") && matches!(name, "relationship" | "Column" | "mapped_column")
        {
            if name == "relationship"
                && positional(&call)
                    .first()
                    .is_some_and(|e| e.syntax().text().to_string().parse::<f64>().is_ok())
            {
                warn(
                    call.syntax(),
                    "PySqlAlchemyModel",
                    "relationship target must be a mapped class or class name",
                    vec![],
                    sink,
                );
            }
            if keyword(&call, "nullable").is_some_and(|e| literal(&e).is_some()) {
                warn(
                    call.syntax(),
                    "PySqlAlchemyModel",
                    "nullable requires a boolean",
                    vec![],
                    sink,
                );
            }
        }
    }
}

fn catalog_checks(inf: &Inferer<'_>, file: &Arc<FileInfo>, inputs: &Inputs, sink: &mut Sink) {
    if !inputs.django_complete {
        return;
    }
    if file
        .path
        .file_name()
        .is_some_and(|name| name == "models.py")
    {
        for app in &inputs.pending_migrations {
            sink.push(
                TextRange::empty(TextSize::new(0)),
                Severity::Warning,
                "PyDjangoMigration",
                format!("Model changes in '{app}' require migrations"),
                vec![],
            );
        }
    }
    for (path, id, message) in &inputs.model_issues {
        if Path::new(path) == file.path {
            sink.push(
                TextRange::empty(TextSize::new(0)),
                Severity::Warning,
                "PyDjangoModel",
                format!("{id}: {message}"),
                vec![],
            );
        }
    }
    for attribute in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::AttributeExpr::cast)
        .take(10000)
    {
        let Some(object) = attribute.object() else {
            continue;
        };
        if identity(inf, file, &object).as_deref() == Some("django.conf.settings")
            && !inputs
                .settings_keys
                .iter()
                .any(|key| key == &attribute.attr_text())
        {
            warn(
                attribute.syntax(),
                "PyDjangoSetting",
                format!("Unknown Django setting '{}'", attribute.attr_text()),
                vec![],
                sink,
            );
        }
    }
    for call in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .take(4096)
    {
        if call
            .callee()
            .and_then(|e| identity(inf, file, &e))
            .is_some_and(|id| {
                matches!(
                    id.as_str(),
                    "django.urls.reverse"
                        | "django.urls.reverse_lazy"
                        | "django.shortcuts.redirect"
                )
            })
        {
            if let Some(name) = positional(&call).first().and_then(literal) {
                if !name.contains('/')
                    && !["http:", "https:", "mailto:", "tel:"]
                        .iter()
                        .any(|prefix| name.starts_with(*prefix))
                    && !inputs.url_names.contains(&name)
                {
                    warn(
                        call.syntax(),
                        "PyDjangoUrl",
                        format!("Unknown URL name '{name}'"),
                        vec![],
                        sink,
                    );
                }
            }
        }
    }
}

/// FastAPI `response_model=`: the value must be a type FastAPI can turn into a
/// response field, and a Pydantic return annotation must be the same model or a
/// subclass of it. `response_model=None` disables the check, and a non-model
/// annotation (`dict`, `Any`, …) is the usual "filter through the model" case.
fn response_model(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    function: &ast::FuncDef,
    call: &ast::CallExpr,
    sink: &mut Sink,
) {
    use rustpy_semantic::types::Type;
    let Some(value) = keyword(call, "response_model") else {
        return;
    };
    if value.syntax().text() == "None" {
        return;
    }
    let inferred = match &value {
        ast::Expr::Name(_) | ast::Expr::Attribute(_) | ast::Expr::Call(_) => {
            inf.type_of_expr(file, &value)
        }
        _ => Type::Unknown,
    };
    let not_type = match &value {
        ast::Expr::String(_)
        | ast::Expr::ConcatString(_)
        | ast::Expr::FString(_)
        | ast::Expr::Literal(_)
        | ast::Expr::Dict(_)
        | ast::Expr::List(_)
        | ast::Expr::Set(_)
        | ast::Expr::Tuple(_)
        | ast::Expr::Lambda(_)
        | ast::Expr::ListComp(_)
        | ast::Expr::SetComp(_)
        | ast::Expr::DictComp(_) => true,
        _ => match &inferred {
            Type::Literal(_) | Type::Function(_) | Type::Module(_) | Type::Tuple(_) => true,
            Type::Instance(class, _) => class.file == file.id || pydantic(inf, class),
            Type::Class(class, _) => plain_class(inf, file, class),
            _ => false,
        },
    };
    if not_type {
        warn(
            value.syntax(),
            "PyFastApiResponseModel",
            "response_model must be a Pydantic model or a type",
            vec![],
            sink,
        );
        return;
    }
    let Type::Class(model, _) = inferred else {
        return;
    };
    let Some(annotation) = function.return_annotation() else {
        return;
    };
    let Type::Instance(returned, _) = inf.annotation_type(file, &annotation) else {
        return;
    };
    if returned != model
        && pydantic(inf, &model)
        && pydantic(inf, &returned)
        && !inf.mro(&returned).contains(&model)
    {
        warn(
            annotation.syntax(),
            "PyFastApiResponseModel",
            format!(
                "Return annotation '{}' conflicts with response_model '{}'",
                returned.name, model.name
            ),
            vec![],
            sink,
        );
    }
}

fn pydantic(inf: &Inferer<'_>, class: &rustpy_semantic::types::ClassRef) -> bool {
    let Some(owner) = inf.db.file(class.file) else {
        return false;
    };
    rustpy_semantic::frameworks::models_in_analysis(inf.db, &owner)
        .iter()
        .any(|model| {
            model.family == rustpy_semantic::frameworks::Family::Pydantic
                && model.class.range == class.range
        })
}

/// A class of this module that only derives from other undecorated local classes:
/// FastAPI cannot build a response field from it.
fn plain_class(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    class: &rustpy_semantic::types::ClassRef,
) -> bool {
    if class.file != file.id || inf.has_unknown_base(class) {
        return false;
    }
    let mro = inf.mro(class);
    let local: Vec<_> = mro.iter().filter(|c| c.name != "object").collect();
    if local.iter().any(|c| c.file != file.id) {
        return false;
    }
    local.iter().all(|c| {
        file.parse
            .syntax_node()
            .descendants()
            .filter_map(ast::ClassDef::cast)
            .find(|def| def.syntax().text_range() == c.range)
            .is_some_and(|def| {
                def.decorators().next().is_none()
                    && def
                        .arguments()
                        .is_none_or(|a| a.keywords().next().is_none())
                    && def.bases().iter().all(|base| {
                        matches!(base, ast::Expr::Name(_))
                            && local
                                .iter()
                                .any(|c| c.name == base.syntax().text().to_string())
                    })
            })
    })
}

fn noncallable(inf: &Inferer<'_>, file: &Arc<FileInfo>, expr: &ast::Expr) -> bool {
    use rustpy_semantic::types::Type;
    match inf.type_of_expr(file, expr) {
        Type::None | Type::Literal(_) | Type::Tuple(_) => true,
        Type::Instance(class, _) if !inf.has_unknown_base(&class) => {
            inf.mro(&class).iter().all(|owner| {
                inf.class_info(owner)
                    .is_some_and(|info| !info.members.contains_key("__call__"))
            })
        }
        _ => false,
    }
}

// Validate only declaration-backed Pydantic models; arbitrary classes named
// Config and user-defined BaseModel lookalikes remain ordinary Python.
fn model_configuration(inf: &Inferer<'_>, file: &Arc<FileInfo>, sink: &mut Sink) {
    for class in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::ClassDef::cast)
        .take(2048)
    {
        if !class.bases().iter().any(|base| {
            identity(inf, file, base)
                .is_some_and(|id| family(&id, "pydantic") && id.ends_with(".BaseModel"))
        }) {
            continue;
        }
        let Some(body) = class.body() else { continue };
        for statement in body.statements() {
            if let ast::Stmt::ClassDef(config) = statement {
                if config.name_text() != "Config" {
                    continue;
                }
                let Some(body) = config.body() else { continue };
                for statement in body.statements() {
                    let ast::Stmt::Assign(assign) = statement else {
                        continue;
                    };
                    let targets = assign.targets();
                    let Some(ast::Expr::Name(name)) = targets.first() else {
                        continue;
                    };
                    let Some(value) = assign.value() else {
                        continue;
                    };
                    configuration_value(&name.name_text(), &value, sink);
                }
            }
        }
    }
}

fn configuration_value(key: &str, value: &ast::Expr, sink: &mut Sink) {
    let invalid = if key == "extra" {
        literal(value).is_some_and(|value| !matches!(value.as_str(), "allow" | "ignore" | "forbid"))
    } else if matches!(
        key,
        "validate_assignment"
            | "arbitrary_types_allowed"
            | "orm_mode"
            | "allow_mutation"
            | "frozen"
            | "allow_population_by_field_name"
            | "populate_by_name"
            | "from_attributes"
            | "use_enum_values"
    ) {
        matches!(value, ast::Expr::String(_))
            || matches!(value, ast::Expr::Literal(_))
                && !matches!(value.text().as_str(), "True" | "False")
    } else {
        false
    };
    if invalid {
        warn(
            value.syntax(),
            "PyPydantic",
            "Invalid Pydantic configuration value",
            vec![],
            sink,
        );
    }
}
