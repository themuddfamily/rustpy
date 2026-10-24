//! Definition/signature edits and typed adapters to project-wide refactor dialogs.
use super::*;
use rustpy_refactor::generation::{self, ClassTarget, Request};
use rustpy_semantic::{
    docstrings::{self, DocstringFormat, ParsedDocstring},
    infer::Inferer,
    types::{LiteralValue, ParamKind, Type},
};
#[derive(Clone, Debug)]
pub enum RequestAction {
    ChangeSignature,
    CallParameter {
        path: std::path::PathBuf,
        offset: TextSize,
        name: String,
        keyword_only: bool,
    },
    Rename(Option<String>),
    SafeDelete,
    GenerateDocstring,
    UpdateDocstring,
    Parameter {
        name: String,
        change: ParameterChange,
    },
}
#[derive(Clone, Copy, Debug)]
pub enum ParameterChange {
    KeywordOnly,
    PositionalOnly,
    AddDefault,
    RemoveDefault,
    Rename,
}
pub fn ui_requests(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Vec<(String, RequestAction)> {
    let root = file.parse.syntax_node();
    let mut out = Vec::new();
    let Some(token) = token_at(&root, offset) else {
        return out;
    };
    if let Some(function) = token.parent_ancestors().find_map(ast::FuncDef::cast) {
        out.push(("Change signature…".into(), RequestAction::ChangeSignature));
        out.push((
            "Generate docstring".into(),
            RequestAction::GenerateDocstring,
        ));
        if function.docstring().is_some() {
            out.push((
                "Update docstring (add/remove parameters)".into(),
                RequestAction::UpdateDocstring,
            ));
            out.push((
                "Fix docstring section".into(),
                RequestAction::UpdateDocstring,
            ));
        }
        if let Some(parameter) = token.parent_ancestors().find_map(ast::Param::cast) {
            let name = parameter.name_text();
            for (title, change) in [
                ("Make parameter keyword-only", ParameterChange::KeywordOnly),
                (
                    "Make parameter positional-only",
                    ParameterChange::PositionalOnly,
                ),
                ("Rename parameter and usages…", ParameterChange::Rename),
            ] {
                out.push((
                    title.into(),
                    RequestAction::Parameter {
                        name: name.clone(),
                        change,
                    },
                ));
            }
            out.push((
                if parameter.default().is_some() {
                    "Remove default value"
                } else {
                    "Add default value"
                }
                .into(),
                RequestAction::Parameter {
                    name,
                    change: if parameter.default().is_some() {
                        ParameterChange::RemoveDefault
                    } else {
                        ParameterChange::AddDefault
                    },
                },
            ));
        }
    }
    if let Some(call) = token.parent_ancestors().find_map(ast::CallExpr::cast) {
        if let (Some(callee), Some(arguments)) = (call.callee(), call.arg_list()) {
            let inf = Inferer::new(db);
            let signatures =
                rustpy_semantic::call_help::signatures_of(&inf, &inf.type_of_expr(file, &callee));
            if let [signature] = signatures.as_slice() {
                if let Some(definition) = &signature.def {
                    if let Some(target) = db
                        .file(definition.file)
                        .filter(|target| !target.path.to_string_lossy().contains("<typeshed>"))
                    {
                        if !signature
                            .params
                            .iter()
                            .any(|p| p.kind == ParamKind::VarArgs)
                        {
                            let count = signature
                                .params
                                .iter()
                                .filter(|p| {
                                    matches!(p.kind, ParamKind::Normal | ParamKind::PositionalOnly)
                                })
                                .count();
                            if let Some(argument) = arguments
                                .positional()
                                .nth(count)
                                .filter(|argument| argument.text_range().contains_inclusive(offset))
                            {
                                let mut name = match argument.clone().unparen() {
                                    ast::Expr::Name(name) => name.name_text(),
                                    _ => format!("parameter{}", count + 1),
                                };
                                while signature.params.iter().any(|p| p.name == name) {
                                    name.push('_')
                                }
                                out.push((
                                    format!("Add parameter '{name}' to function…"),
                                    RequestAction::CallParameter {
                                        path: target.path.clone(),
                                        offset: definition.range.start(),
                                        name,
                                        keyword_only: false,
                                    },
                                ));
                            }
                        }
                        for keyword in arguments.keywords() {
                            let name = keyword.name_text();
                            if keyword.text_range().contains_inclusive(offset)
                                && !signature
                                    .params
                                    .iter()
                                    .any(|p| p.name == name || p.kind == ParamKind::KwArgs)
                            {
                                out.push((
                                    format!("Add parameter '{name}' to function…"),
                                    RequestAction::CallParameter {
                                        path: target.path.clone(),
                                        offset: definition.range.start(),
                                        name,
                                        keyword_only: true,
                                    },
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(binding) = file
        .scopes
        .bindings
        .iter()
        .find(|binding| binding.range.contains_inclusive(offset))
    {
        out.push((
            "Delete unused definition safely…".into(),
            RequestAction::SafeDelete,
        ));
        out.push((
            "Rename definition and usages…".into(),
            RequestAction::Rename(None),
        ));
        let snake = snake_case(binding.name.as_str());
        if snake != binding.name.as_str() && !matches!(binding.kind, BindingKind::Class) {
            out.push((
                format!("Rename to PEP 8 name '{snake}'"),
                RequestAction::Rename(Some(snake)),
            ));
        }
        if !binding.name.starts_with('_') {
            out.push((
                "Rename with underscore prefix".into(),
                RequestAction::Rename(Some(format!("_{}", binding.name))),
            ));
            out.push((
                "Make protected".into(),
                RequestAction::Rename(Some(format!("_{}", binding.name))),
            ));
            out.push((
                "Make private".into(),
                RequestAction::Rename(Some(format!("__{}", binding.name))),
            ));
        }
    }
    out
}
fn snake_case(name: &str) -> String {
    let chars: Vec<_> = name.chars().collect();
    let mut out = String::new();
    for (index, c) in chars.iter().copied().enumerate() {
        if c.is_uppercase()
            && index > 0
            && (chars[index - 1].is_lowercase()
                || chars.get(index + 1).is_some_and(|c| c.is_lowercase())
                    && chars[index - 1].is_uppercase())
        {
            out.push('_')
        }
        out.extend(c.to_lowercase());
    }
    out
}
fn offer(out: &mut Vec<Intention>, range: TextRange, title: &str, value: String) {
    out.push(Intention {
        title: title.into(),
        edits: vec![(range, value)],
    })
}
fn source(file: &FileInfo, expr: &ast::Expr) -> String {
    slice(&file.text, expr.text_range()).to_string()
}
fn canonical(file: &FileInfo, offset: TextSize, name: &str) -> bool {
    file.scopes
        .resolve(file.scopes.scope_at(offset), name)
        .is_none()
        && file
            .scopes
            .scopes
            .iter()
            .all(|scope| scope.star_imports.is_empty())
}
fn function(file: &FileInfo, offset: TextSize) -> Option<ast::FuncDef> {
    token_at(&file.parse.syntax_node(), offset)?
        .parent_ancestors()
        .find_map(ast::FuncDef::cast)
}
fn suite_insert(file: &FileInfo, body: &ast::Block) -> Option<(TextSize, String)> {
    let mut statements = body.statements();
    let first = statements.next()?;
    let indent = indent_at(&file.text, usize::from(first.text_range().start()));
    if indent.is_empty()
        || !file.text[line_start(&file.text, usize::from(first.text_range().start()))
            ..usize::from(first.text_range().start())]
            .trim()
            .is_empty()
    {
        return None;
    }
    let after_doc =
        matches!(&first,ast::Stmt::Expr(expr) if matches!(expr.expr(),Some(ast::Expr::String(_))));
    let offset = if after_doc {
        statements.next()?.text_range().start()
    } else {
        first.text_range().start()
    };
    Some((offset, indent))
}
fn import_type(
    file: &FileInfo,
    offset: TextSize,
    module: &str,
    name: &str,
    imports: &mut Vec<Edit>,
) -> String {
    let scope = file.scopes.scope_at(offset);
    for (index, binding) in file.scopes.bindings.iter().enumerate() {
        if matches!(&binding.kind,BindingKind::Import(info) if info.module==module&&info.name.as_deref()==Some(name)&&info.level==0)
            && binding.range.start() < offset
            && file
                .scopes
                .resolve(scope, binding.name.as_str())
                .is_some_and(|ids| ids.len() == 1 && ids[0].0 as usize == index)
        {
            return binding.name.to_string();
        }
    }
    let alias = if canonical(file, offset, name) {
        name.into()
    } else {
        let base = format!("_{name}");
        let mut name = base.clone();
        let mut index = 0;
        while file
            .scopes
            .bindings
            .iter()
            .any(|binding| binding.name == name)
            || imports
                .iter()
                .any(|(_, text)| text.contains(&format!(" as {name}\n")))
        {
            index += 1;
            name = format!("{base}{index}");
        }
        name
    };
    let suffix = if alias == name {
        String::new()
    } else {
        format!(" as {alias}")
    };
    let insertion = rustpy_semantic::import_placement::import_insert_offset(file);
    let prefix =
        if insertion > TextSize::new(0) && !file.text[..usize::from(insertion)].ends_with('\n') {
            "\n"
        } else {
            ""
        };
    let value = format!("{prefix}from {module} import {name}{suffix}\n");
    if !imports.iter().any(|(_, text)| text == &value) {
        imports.push((
            TextRange::empty(rustpy_semantic::import_placement::import_insert_offset(
                file,
            )),
            value,
        ));
    }
    alias
}
fn annotation(
    db: &Analysis,
    file: &FileInfo,
    offset: TextSize,
    ty: &Type,
    imports: &mut Vec<Edit>,
    depth: usize,
) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match ty {
        Type::Unknown => None,
        Type::Any => Some(import_type(file, offset, "typing", "Any", imports)),
        Type::Never => Some(import_type(file, offset, "typing", "NoReturn", imports)),
        Type::None => Some("None".into()),
        Type::Literal(value) => {
            let name = match value {
                LiteralValue::Int(_) => "int",
                LiteralValue::Bool(_) => "bool",
                LiteralValue::Str(_) => "str",
                LiteralValue::Bytes(_) => "bytes",
                _ => return None,
            };
            Some(if canonical(file, offset, name) {
                name.into()
            } else {
                import_type(file, offset, "builtins", name, imports)
            })
        }
        Type::Instance(class, args) | Type::Class(class, args) => {
            let class_file = db.file(class.file)?;
            let module = class_file.module_name.as_deref().unwrap_or("");
            let name = if module == "builtins" {
                if canonical(file, offset, class.name.as_str()) {
                    class.name.to_string()
                } else {
                    import_type(file, offset, "builtins", class.name.as_str(), imports)
                }
            } else if class.file == file.id {
                let ids = file
                    .scopes
                    .resolve(file.scopes.scope_at(offset), class.name.as_str())?;
                if ids.len() != 1 || file.scopes.binding(ids[0]).node_range != class.range {
                    return None;
                }
                class.name.to_string()
            } else {
                if module.is_empty() {
                    return None;
                }
                import_type(file, offset, module, class.name.as_str(), imports)
            };
            let wrap = |name: String| {
                if matches!(ty, Type::Class(..)) {
                    format!("type[{name}]")
                } else {
                    name
                }
            };
            if args.is_empty() {
                Some(wrap(name))
            } else {
                let arguments = args
                    .iter()
                    .map(|arg| annotation(db, file, offset, arg, imports, depth + 1))
                    .collect::<Option<Vec<_>>>()?;
                Some(wrap(format!("{name}[{}]", arguments.join(", "))))
            }
        }
        Type::Union(members) => {
            let members = members
                .iter()
                .map(|member| annotation(db, file, offset, member, imports, depth + 1))
                .collect::<Option<Vec<_>>>()?;
            Some(members.join(" | "))
        }
        Type::Tuple(members) => {
            let members = members
                .iter()
                .map(|member| annotation(db, file, offset, member, imports, depth + 1))
                .collect::<Option<Vec<_>>>()?;
            Some(format!(
                "tuple[{}]",
                if members.is_empty() {
                    "()".into()
                } else {
                    members.join(", ")
                }
            ))
        }
        Type::Function(signature) | Type::Callable(signature) => {
            let callable = import_type(file, offset, "typing", "Callable", imports);
            let ret = annotation(db, file, offset, &signature.ret, imports, depth + 1)?;
            let args = if signature
                .params
                .iter()
                .all(|p| matches!(p.kind, ParamKind::Normal | ParamKind::PositionalOnly))
            {
                format!(
                    "[{}]",
                    signature
                        .params
                        .iter()
                        .map(|p| annotation(db, file, offset, &p.ty, imports, depth + 1))
                        .collect::<Option<Vec<_>>>()?
                        .join(", ")
                )
            } else {
                "...".into()
            };
            Some(format!("{callable}[{args}, {ret}]"))
        }
        _ => None,
    }
}
fn type_hints(
    db: &Analysis,
    file: &Arc<FileInfo>,
    function: &ast::FuncDef,
    out: &mut Vec<Intention>,
) {
    let inf = Inferer::new(db);
    let signature = inf.signature(file, function, None);
    let Some(params) = function.params() else {
        return;
    };
    let mut edits = Vec::new();
    let mut imports = Vec::new();
    for parameter in params.params() {
        if parameter.annotation().is_some() {
            continue;
        }
        let Some(name) = parameter.name() else {
            continue;
        };
        if function.is_method() && matches!(name.text(), "self" | "cls") {
            continue;
        }
        let inferred = parameter
            .default()
            .map(|default| inf.type_of_expr(file, &default))
            .or_else(|| {
                signature
                    .params
                    .iter()
                    .find(|p| p.name == name.text())
                    .map(|p| p.ty.clone())
            });
        let hint = inferred
            .as_ref()
            .and_then(|ty| annotation(db, file, name.text_range().start(), ty, &mut imports, 0));
        if let Some(hint) = hint {
            edits.push((
                TextRange::empty(name.text_range().end()),
                format!(": {hint}"),
            ));
        }
        let mut object_edits = Vec::new();
        let object = if canonical(file, name.text_range().start(), "object") {
            String::from("object")
        } else {
            import_type(
                file,
                name.text_range().start(),
                "builtins",
                "object",
                &mut object_edits,
            )
        };
        object_edits.push((
            TextRange::empty(name.text_range().end()),
            format!(": {object}"),
        ));
        out.push(Intention {
            title: format!("Add object type hint for parameter '{}'", name.text()),
            edits: object_edits,
        });
    }
    if function.return_annotation().is_none() {
        let mut ret = signature.ret.clone();
        if function.is_async() {
            if let Type::Instance(class, args) = &ret {
                if class.name == "Coroutine" && args.len() == 3 {
                    ret = args[2].clone();
                }
            }
        }
        if let Some(hint) = annotation(
            db,
            file,
            function.text_range().start(),
            &ret,
            &mut imports,
            0,
        ) {
            edits.push((
                TextRange::empty(params.text_range().end()),
                format!(" -> {hint}"),
            ));
        }
        offer(
            out,
            TextRange::empty(params.text_range().end()),
            "Add return annotation None",
            " -> None".into(),
        );
    }
    if !edits.is_empty() {
        edits.extend(imports);
        out.push(Intention {
            title: "Add inferred parameter and return type hints".into(),
            edits,
        });
    }
}
fn parameter_actions(file: &Arc<FileInfo>, parameter: &ast::Param, out: &mut Vec<Intention>) {
    let Some(default) = parameter.default() else {
        return;
    };
    let Some(annotation) = parameter.annotation() else {
        return;
    };
    if source(file, &default) == "None" && !source(file, &annotation).contains("None") {
        offer(
            out,
            annotation.text_range(),
            "Add Optional for None default",
            format!("({}) | None", source(file, &annotation)),
        );
    }
}
fn function_actions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    function: &ast::FuncDef,
    out: &mut Vec<Intention>,
) {
    type_hints(db, file, function, out);
    let Some(body) = function.body() else { return };
    if let Some((at, indent)) = suite_insert(file, &body) {
        if function.is_method() {
            if function.name_text() == "__init__"
                && !body
                    .syntax()
                    .descendants()
                    .filter_map(ast::CallExpr::cast)
                    .any(|call| {
                        call.callee()
                            .is_some_and(|callee| source(file, &callee) == "super().__init__")
                    })
            {
                offer(
                    out,
                    TextRange::empty(at),
                    "Add super init call",
                    format!("super().__init__()\n{indent}"),
                );
            }
            if function.decorators().next().is_none() {
                for decorator in ["staticmethod", "classmethod"] {
                    if canonical(file, function.text_range().start(), decorator) {
                        let indent =
                            indent_at(&file.text, usize::from(function.text_range().start()));
                        offer(
                            out,
                            TextRange::empty(function.text_range().start()),
                            &format!("Add @{decorator}"),
                            format!("@{decorator}\n{indent}"),
                        );
                    }
                }
            }
            if function.has_decorator("classmethod")
                && function
                    .params()
                    .and_then(|p| p.plain_params().next())
                    .is_none_or(|p| !matches!(p.name_text().as_str(), "cls" | "self"))
            {
                if let Some(params) = function.params() {
                    let at = params.text_range().start() + TextSize::new(1);
                    let separator = if params.params().next().is_some() {
                        ", "
                    } else {
                        ""
                    };
                    offer(
                        out,
                        TextRange::empty(at),
                        "Add cls parameter",
                        format!("cls{separator}"),
                    );
                }
            }
            let mut imports = Vec::new();
            let override_ = import_type(
                file,
                function.text_range().start(),
                "typing",
                "override",
                &mut imports,
            );
            if !function.has_decorator("override") {
                imports.push((
                    TextRange::empty(function.text_range().start()),
                    format!(
                        "@{override_}\n{}",
                        indent_at(&file.text, usize::from(function.text_range().start()))
                    ),
                ));
                out.push(Intention {
                    title: "Add @override".into(),
                    edits: imports,
                });
            }
        }
        offer(
            out,
            TextRange::empty(at),
            "Add raise NotImplementedError",
            format!("raise NotImplementedError\n{indent}"),
        );
        if !body
            .syntax()
            .descendants()
            .any(|node| node.kind() == SyntaxKind::RETURN_STMT)
        {
            let last = body.statements().last();
            if let Some(last) = last {
                let end = stmt_lines(&file.text, last.text_range()).end();
                offer(
                    out,
                    TextRange::empty(end),
                    "Add return",
                    format!("{indent}return\n"),
                );
            }
        }
    }
    for format in [
        DocstringFormat::ReStructuredText,
        DocstringFormat::Google,
        DocstringFormat::NumPy,
    ] {
        if let Some(doc) = function.docstring() {
            if docstrings::detect_one(&doc.value()) != format {
                if let Some(intention) =
                    update_docstring(db, file, function.text_range().start(), format)
                {
                    out.push(Intention {
                        title: format!("Convert docstring to {}", format.label()),
                        ..intention
                    });
                }
            }
        }
    }
    mutable_defaults(file, function, out);
}
fn class_actions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    class: &ast::ClassDef,
    out: &mut Vec<Intention>,
) {
    if !class
        .decorators()
        .any(|decorator| decorator.name_text().as_deref() == Some("dataclass"))
    {
        let mut imports = Vec::new();
        let name = import_type(
            file,
            class.text_range().start(),
            "dataclasses",
            "dataclass",
            &mut imports,
        );
        imports.push((
            TextRange::empty(class.text_range().start()),
            format!(
                "@{name}\n{}",
                indent_at(&file.text, usize::from(class.text_range().start()))
            ),
        ));
        out.push(Intention {
            title: "Add @dataclass".into(),
            edits: imports,
        });
    }
    if let Ok(mut target) = ClassTarget::capture(
        &file.text,
        &file.parse.syntax_node(),
        class.text_range().start(),
    ) {
        generation::resolve_class_facts(db, file, &mut target);
        let fields = target
            .fields
            .iter()
            .map(|field| field.name.clone())
            .collect::<Vec<_>>();
        for (title, request) in [
            (
                "Add __init__",
                Request {
                    constructor: true,
                    fields: fields.clone(),
                    ..Default::default()
                },
            ),
            (
                "Add __repr__",
                Request {
                    dunders: vec!["__repr__".into()],
                    fields: fields.clone(),
                    ..Default::default()
                },
            ),
            (
                "Add __str__",
                Request {
                    dunders: vec!["__str__".into()],
                    fields: fields.clone(),
                    ..Default::default()
                },
            ),
            (
                "Add __slots__",
                Request {
                    slots: true,
                    fields,
                    ..Default::default()
                },
            ),
        ] {
            if let Ok(edits) = generation::generate(&target, &request) {
                if !edits.is_empty() {
                    out.push(Intention {
                        title: title.into(),
                        edits,
                    });
                }
            }
        }
    }
    if let Ok(refactoring) =
        rustpy_refactor::refactor2::implement_abstract_methods(db, file, class.text_range().start())
    {
        if !refactoring.edits.is_empty() {
            out.push(Intention {
                title: "Implement abstract methods".into(),
                edits: refactoring.edits,
            });
        }
    }
}

fn mutable_defaults(file: &Arc<FileInfo>, function: &ast::FuncDef, out: &mut Vec<Intention>) {
    let Some(params) = function.params() else {
        return;
    };
    let Some(body) = function.body() else { return };
    let Some((at, indent)) = suite_insert(file, &body) else {
        return;
    };
    let mut edits = Vec::new();
    let mut initialize = String::new();
    for parameter in params.plain_params() {
        let Some(default) = parameter.default() else {
            continue;
        };
        let mutable = matches!(
            default,
            ast::Expr::List(_) | ast::Expr::Dict(_) | ast::Expr::Set(_)
        ) || matches!(&default,ast::Expr::Call(call) if call.callee().is_some_and(|callee|matches!(source(file,&callee).as_str(),"list"|"dict"|"set")&&canonical(file,default.text_range().start(),&source(file,&callee))));
        if !mutable {
            continue;
        }
        let name = parameter.name_text();
        edits.push((default.text_range(), "None".into()));
        if let Some(annotation) = parameter.annotation() {
            if !source(file, &annotation).contains("None") {
                edits.push((
                    annotation.text_range(),
                    format!("({}) | None", source(file, &annotation)),
                ));
            }
        }
        initialize.push_str(&format!(
            "if {name} is None:\n{indent}    {name} = {}\n{indent}",
            source(file, &default)
        ));
    }
    if !edits.is_empty() {
        edits.push((TextRange::empty(at), initialize));
        out.push(Intention {
            title: "Replace mutable defaults with None and initialize".into(),
            edits,
        });
    }
}
fn typing_head(db: &Analysis, file: &FileInfo, expr: &ast::Expr) -> Option<String> {
    imported_head(db, file, expr, &["typing", "typing_extensions"])
}
fn imported_head(
    db: &Analysis,
    file: &FileInfo,
    expr: &ast::Expr,
    modules: &[&str],
) -> Option<String> {
    let (root, member) = match expr.clone().unparen() {
        ast::Expr::Name(name) => (name.name_text(), None),
        ast::Expr::Attribute(attribute) => {
            let ast::Expr::Name(name) = attribute.object()? else {
                return None;
            };
            (name.name_text(), Some(attribute.attr_text()))
        }
        _ => return None,
    };
    let ids = file
        .scopes
        .resolve(file.scopes.scope_at(expr.text_range().start()), &root)?;
    if ids.len() != 1 {
        return None;
    }
    let BindingKind::Import(info) = &file.scopes.binding(ids[0]).kind else {
        return None;
    };
    if info.level != 0 || !modules.contains(&info.module.as_str()) {
        return None;
    }
    if db.module(&info.module).is_some_and(|module| {
        db.resolver
            .roots
            .iter()
            .any(|root| module.path.starts_with(root))
    }) {
        return None;
    }
    if file
        .scopes
        .scopes
        .iter()
        .any(|scope| !scope.star_imports.is_empty())
    {
        return None;
    }
    let aliases:Vec<_>=file.scopes.bindings.iter().filter(|binding|matches!(&binding.kind,BindingKind::Import(i) if i.level==0&&i.module==info.module&&i.name.is_none())).map(|binding|binding.name.as_str()).collect();
    let mut target_budget = 4096usize;
    for (count, node) in file.parse.syntax_node().descendants().enumerate() {
        if count == 16384 {
            return None;
        }
        let targets: Vec<_> = match node.kind() {
            SyntaxKind::ASSIGN_STMT => ast::AssignStmt::cast(node).unwrap().targets(),
            SyntaxKind::ANN_ASSIGN_STMT => ast::AnnAssignStmt::cast(node)
                .unwrap()
                .target()
                .into_iter()
                .collect(),
            SyntaxKind::AUG_ASSIGN_STMT => ast::AugAssignStmt::cast(node)
                .unwrap()
                .target()
                .into_iter()
                .collect(),
            SyntaxKind::DEL_STMT => ast::DelStmt::cast(node).unwrap().targets(),
            _ => continue,
        };
        for target in targets {
            let count = target
                .syntax()
                .descendants()
                .take(target_budget.saturating_add(1))
                .count();
            if count > target_budget {
                return None;
            }
            target_budget -= count;
            if target
                .syntax()
                .descendants()
                .filter_map(ast::AttributeExpr::cast)
                .any(|attribute| {
                    attribute
                        .syntax()
                        .descendants()
                        .filter_map(ast::NameExpr::cast)
                        .any(|name| aliases.iter().any(|alias| name.name_text() == *alias))
                })
            {
                return None;
            }
        }
    }
    member.or_else(|| info.name.as_ref().map(ToString::to_string))
}
fn modernize_type(
    db: &Analysis,
    file: &FileInfo,
    expr: &ast::Expr,
    depth: usize,
) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let ast::Expr::Subscript(subscript) = expr.clone().unparen() else {
        return Some(source(file, expr));
    };
    let head = subscript.object()?;
    let name = typing_head(db, file, &head).unwrap_or_default();
    let index = subscript.index_expr()?;
    let arguments = if let ast::Expr::Tuple(tuple) = index.clone().unparen() {
        tuple.elements().collect::<Vec<_>>()
    } else {
        vec![index]
    };
    let args = arguments
        .iter()
        .map(|argument| modernize_type(db, file, argument, depth + 1))
        .collect::<Option<Vec<_>>>()?;
    let replacement = match name.as_str() {
        "List" => "list",
        "Dict" => "dict",
        "Set" => "set",
        "FrozenSet" => "frozenset",
        "Tuple" => "tuple",
        "Type" => "type",
        "Optional" if args.len() == 1 => return Some(format!("({}) | None", args[0])),
        "Union" if !args.is_empty() => {
            return Some(
                args.iter()
                    .map(|arg| format!("({arg})"))
                    .collect::<Vec<_>>()
                    .join(" | "),
            )
        }
        _ => return Some(format!("{}[{}]", source(file, &head), args.join(", "))),
    };
    if !canonical(file, expr.text_range().start(), replacement) {
        return None;
    }
    Some(format!("{replacement}[{}]", args.join(", ")))
}
fn standalone(file: &FileInfo, range: TextRange) -> bool {
    let begin = usize::from(range.start());
    let end = usize::from(range.end());
    let finish = file.text[end..]
        .find('\n')
        .map_or(file.text.len(), |n| end + n);
    file.text[line_start(&file.text, begin)..begin]
        .trim()
        .is_empty()
        && {
            let tail = file.text[end..finish].trim();
            tail.is_empty() || tail.starts_with('#')
        }
}
fn comment_annotations(file: &FileInfo, out: &mut Vec<Intention>, statement: &ast::Stmt) {
    if !standalone(file, statement.text_range()) {
        return;
    }
    let text = &file.text;
    match statement {
        ast::Stmt::Assign(assign) => {
            let targets = assign.targets();
            if targets.len() != 1 || !matches!(targets[0], ast::Expr::Name(_)) {
                return;
            }
            let end = usize::from(assign.text_range().end());
            let line_end = text[end..].find('\n').map_or(text.len(), |at| end + at);
            let rest = &text[end..line_end];
            let Some(marker) = rest.find("# type:") else {
                return;
            };
            let comment = rest[marker + 7..].trim();
            if comment.starts_with("ignore") {
                return;
            }
            let parsed = rustpy_syntax::parse_expression(comment);
            if !parsed.errors().is_empty() {
                return;
            }
            out.push(Intention {
                title: "Convert type comment to annotation".into(),
                edits: vec![
                    (
                        TextRange::empty(targets[0].text_range().end()),
                        format!(": {comment}"),
                    ),
                    (rng(end + marker, line_end), String::new()),
                ],
            });
        }
        ast::Stmt::AnnAssign(assign) => {
            let (Some(target), Some(value), Some(annotation)) =
                (assign.target(), assign.value(), assign.annotation())
            else {
                return;
            };
            if !matches!(target, ast::Expr::Name(_)) {
                return;
            }
            offer(
                out,
                assign.text_range(),
                "Convert annotation to type comment",
                format!(
                    "{} = {}  # type: {}",
                    source(file, &target),
                    source(file, &value),
                    source(file, &annotation)
                ),
            );
        }
        _ => {}
    }
}
fn function_comments(file: &FileInfo, function: &ast::FuncDef, out: &mut Vec<Intention>) {
    let Some(params) = function.params() else {
        return;
    };
    let Some(body) = function.body() else { return };
    let boundary = body.text_range().start();
    let Some(colon) = function
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == SyntaxKind::COLON && token.text_range().start() <= boundary)
        .last()
    else {
        return;
    };
    let end = usize::from(colon.text_range().end());
    let line_end = file.text[end..]
        .find('\n')
        .map_or(file.text.len(), |at| end + at);
    let mut comment_end = line_end;
    let mut comment_start = end;
    let header_suffix = &file.text[end..line_end];
    if header_suffix.trim().is_empty() {
        if let Some(first) = body.statements().next() {
            let before = &file.text[line_end..usize::from(first.text_range().start())];
            if let Some(relative) = before.find("# type:") {
                let candidate = line_end + relative;
                let finish = file.text[candidate..]
                    .find('\n')
                    .map_or(file.text.len(), |n| candidate + n);
                comment_start = candidate;
                comment_end = finish;
            }
        }
    }
    let suffix = &file.text[comment_start..comment_end];
    let plain: Vec<_> = params
        .params()
        .filter(|parameter| parameter.name().is_some())
        .collect();
    if let Some(marker) = suffix.find("# type:") {
        let comment = suffix[marker + 7..].trim();
        let Some((arguments, ret)) = comment.rsplit_once("->") else {
            return;
        };
        if !rustpy_syntax::parse_expression(ret.trim())
            .errors()
            .is_empty()
        {
            return;
        }
        let arguments = arguments
            .trim()
            .strip_prefix('(')
            .and_then(|v| v.strip_suffix(')'));
        let Some(arguments) = arguments else { return };
        let arguments = split_types(arguments);
        let Some(arguments) = arguments else { return };
        let receiver = usize::from(
            function.is_method()
                && !function.has_decorator("staticmethod")
                && plain
                    .first()
                    .is_some_and(|p| p.name().is_some_and(|n| matches!(n.text(), "self" | "cls"))),
        );
        if arguments.len() != plain.len().saturating_sub(receiver) && arguments != ["..."] {
            return;
        }
        let mut edits = vec![(rng(comment_start + marker, comment_end), String::new())];
        if arguments != ["..."] {
            for (parameter, ty) in plain.iter().skip(receiver).zip(arguments) {
                if parameter.annotation().is_some() {
                    return;
                }
                let Some(name) = parameter.name() else { return };
                let ty = ty.trim_start_matches('*');
                if !rustpy_syntax::parse_expression(ty).errors().is_empty() {
                    return;
                }
                edits.push((TextRange::empty(name.text_range().end()), format!(": {ty}")));
            }
        }
        if function.return_annotation().is_none() {
            edits.push((
                TextRange::empty(params.text_range().end()),
                format!(" -> {}", ret.trim()),
            ));
        }
        out.push(Intention {
            title: "Convert function type comment to annotations".into(),
            edits,
        });
    } else if header_suffix.trim().is_empty() {
        let mut types = Vec::new();
        let mut edits = Vec::new();
        for (index, parameter) in plain.iter().enumerate() {
            if index == 0 && function.is_method() && !function.has_decorator("staticmethod") {
                continue;
            }
            let Some(annotation) = parameter.annotation() else {
                return;
            };
            let prefix = match parameter {
                ast::AnyParam::Star(_) => "*",
                ast::AnyParam::StarStar(_) => "**",
                _ => "",
            };
            types.push(format!("{prefix}{}", source(file, &annotation)));
            let (Some(name), Some(parent)) = (parameter.name(), annotation.syntax().parent())
            else {
                return;
            };
            // ':' belongs to the parameter; ANNOTATION only owns its type.
            edits.push((
                TextRange::new(name.text_range().end(), parent.text_range().end()),
                String::new(),
            ));
        }
        let Some(ret) = function.return_annotation() else {
            return;
        };
        let ret_text = source(file, &ret);
        let Some(parent) = ret.syntax().parent() else {
            return;
        };
        // Likewise, '->' belongs to FUNC_DEF, outside RETURN_ANNOTATION.
        edits.push((
            TextRange::new(params.text_range().end(), parent.text_range().end()),
            String::new(),
        ));
        edits.push((
            TextRange::empty(colon.text_range().end()),
            format!("  # type: ({}) -> {ret_text}", types.join(", ")),
        ));
        out.push(Intention {
            title: "Convert function annotations to type comment".into(),
            edits,
        });
    }
}
fn split_types(text: &str) -> Option<Vec<&str>> {
    let (tokens, errors) = rustpy_syntax::tokenize(text);
    if !errors.is_empty() {
        return None;
    }
    let mut depth = 0;
    let mut start = 0;
    let mut result = Vec::new();
    for token in tokens {
        match token.kind {
            SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_BRACE => depth += 1,
            SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_BRACE => depth -= 1,
            SyntaxKind::COMMA if depth == 0 => {
                result.push(text[start..usize::from(token.range.start())].trim());
                start = usize::from(token.range.end());
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    if !text[start..].trim().is_empty() {
        result.push(text[start..].trim());
    }
    Some(result)
}
fn statement_actions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    statement: &ast::Stmt,
    out: &mut Vec<Intention>,
) {
    comment_annotations(file, out, statement);
    let inf = Inferer::new(db);
    match statement {
        ast::Stmt::Assign(assign) => {
            let targets = assign.targets();
            if targets.len() == 1 {
                if let (Some(ast::Expr::Name(name)), Some(value)) =
                    (targets.first(), assign.value())
                {
                    let mut imports = Vec::new();
                    if let Some(hint) = annotation(
                        db,
                        file,
                        name.text_range().start(),
                        &inf.type_of_expr(file, &value),
                        &mut imports,
                        0,
                    ) {
                        imports.push((
                            TextRange::empty(name.text_range().end()),
                            format!(": {hint}"),
                        ));
                        out.push(Intention {
                            title: "Add inferred variable type hint".into(),
                            edits: imports,
                        });
                    }
                    let mut final_edits = Vec::new();
                    let final_ = import_type(
                        file,
                        name.text_range().start(),
                        "typing",
                        "Final",
                        &mut final_edits,
                    );
                    final_edits.push((
                        TextRange::empty(name.text_range().end()),
                        format!(": {final_}"),
                    ));
                    out.push(Intention {
                        title: "Make Final".into(),
                        edits: final_edits,
                    });
                    let mut object_edits = Vec::new();
                    let object = if canonical(file, name.text_range().start(), "object") {
                        String::from("object")
                    } else {
                        import_type(
                            file,
                            name.text_range().start(),
                            "builtins",
                            "object",
                            &mut object_edits,
                        )
                    };
                    object_edits.push((
                        TextRange::empty(name.text_range().end()),
                        format!(": {object}"),
                    ));
                    out.push(Intention {
                        title: "Add object variable type hint".into(),
                        edits: object_edits,
                    });
                }
            }
        }
        ast::Stmt::Pass(pass) => offer(
            out,
            pass.text_range(),
            "Replace pass with ellipsis",
            "...".into(),
        ),
        ast::Stmt::Expr(expr) => {
            if let Some(value) = expr.expr() {
                if function(file, statement.text_range().start()).is_some()
                    && !matches!(value, ast::Expr::String(_))
                {
                    offer(
                        out,
                        expr.text_range(),
                        "Add return to expression",
                        format!("return {}", source(file, &value)),
                    );
                }
            }
        }
        ast::Stmt::Raise(raise) => {
            if let Some(exception) = raise.exception() {
                if source(file, &exception) == "NotImplemented" {
                    offer(
                        out,
                        exception.text_range(),
                        "Replace NotImplemented with NotImplementedError",
                        "NotImplementedError".into(),
                    );
                }
            }
            if raise.cause().is_none() && raise.exception().is_some() {
                if let Some(handler) = raise.syntax().ancestors().find_map(ast::ExceptClause::cast)
                {
                    if let Some(name) = handler.name() {
                        let end = raise.text_range().end();
                        offer(
                            out,
                            TextRange::empty(end),
                            "Add exception cause from handler",
                            format!(" from {}", name.text()),
                        );
                    }
                }
            }
        }
        ast::Stmt::Try(try_) => {
            for handler in try_.except_clauses() {
                if handler.exception_type().is_none() && !handler.is_star() {
                    if let Some(token) = handler.syntax().first_token() {
                        offer(
                            out,
                            TextRange::empty(token.text_range().end()),
                            "Replace bare except with Exception",
                            " Exception".into(),
                        );
                    }
                }
            }
            reorder_exceptions(db, file, try_, out);
        }
        _ => {}
    }
    remove_unreachable(file, statement, out);
}

fn call_actions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    call: &ast::CallExpr,
    out: &mut Vec<Intention>,
) {
    let Some(callee) = call.callee() else { return };
    let inf = Inferer::new(db);
    let ty = inf.type_of_expr(file, &callee);
    let signatures = rustpy_semantic::call_help::signatures_of(&inf, &ty);
    if signatures.len() != 1 {
        return;
    }
    let signature = &signatures[0];
    let Some(args) = call.arg_list() else { return };
    let arguments: Vec<_> = args.args().collect();
    if arguments.iter().any(|arg| {
        matches!(
            arg,
            ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_) | ast::AnyArg::Generator(_)
        )
    }) {
        return;
    }
    let positional: Vec<_> = signature
        .params
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p.kind, ParamKind::Normal | ParamKind::PositionalOnly))
        .collect();
    let mut used = std::collections::HashSet::new();
    let mut position = 0;
    for arg in &arguments {
        let parameter = match arg {
            ast::AnyArg::Positional(_) => {
                let p = positional.get(position).map(|(i, _)| *i);
                position += 1;
                p
            }
            ast::AnyArg::Keyword(keyword) => signature.params.iter().position(|p| {
                p.name == keyword.name_text()
                    && matches!(p.kind, ParamKind::Normal | ParamKind::KeywordOnly)
            }),
            _ => None,
        };
        let valid = parameter.is_some_and(|parameter| used.insert(parameter));
        let variadic = match arg {
            ast::AnyArg::Keyword(_) => signature.params.iter().any(|p| p.kind == ParamKind::KwArgs),
            _ => signature
                .params
                .iter()
                .any(|p| p.kind == ParamKind::VarArgs),
        };
        if !valid && (parameter.is_some() || !variadic) {
            offer(
                out,
                element_with_comma(&file.text, arg.syntax()),
                "Remove unexpected argument",
                String::new(),
            );
        }
    }
    let missing: Vec<_> = signature
        .params
        .iter()
        .enumerate()
        .filter(|(i, p)| {
            !used.contains(i)
                && !p.has_default
                && matches!(
                    p.kind,
                    ParamKind::Normal | ParamKind::PositionalOnly | ParamKind::KeywordOnly
                )
        })
        .collect();
    if !missing.is_empty() {
        let mut edits = Vec::new();
        let first_keyword = arguments
            .iter()
            .find(|a| matches!(a, ast::AnyArg::Keyword(_)))
            .map(|a| a.text_range().start());
        let close = args
            .syntax()
            .last_token()
            .filter(|token| token.kind() == SyntaxKind::R_PAREN)
            .map(|token| token.text_range().start());
        if let Some(close) = close {
            let positional_missing = missing
                .iter()
                .filter(|(_, p)| p.kind == ParamKind::PositionalOnly)
                .map(|_| "None".to_owned())
                .collect::<Vec<_>>();
            let keywords = missing
                .iter()
                .filter(|(_, p)| p.kind != ParamKind::PositionalOnly)
                .map(|(_, p)| format!("{}=None", p.name))
                .collect::<Vec<_>>();
            let mut tail = keywords;
            if let Some(at) = first_keyword {
                if !positional_missing.is_empty() {
                    edits.push((
                        TextRange::empty(at),
                        format!("{}, ", positional_missing.join(", ")),
                    ));
                }
            } else {
                tail.splice(0..0, positional_missing);
            }
            if !tail.is_empty() {
                edits.push((
                    TextRange::empty(close),
                    format!("{}{}", argument_separator(file, &args), tail.join(", ")),
                ));
            }
            if !edits.is_empty() {
                out.push(Intention {
                    title: "Add missing arguments to call".into(),
                    edits,
                });
            }
        }
    }
}
fn argument_separator(file: &FileInfo, args: &ast::ArgList) -> &'static str {
    let text = slice(&file.text, args.text_range());
    let (tokens, _) = rustpy_syntax::tokenize(text);
    let significant: Vec<_> = tokens
        .iter()
        .filter(|token| {
            !matches!(
                token.kind,
                SyntaxKind::WHITESPACE
                    | SyntaxKind::COMMENT
                    | SyntaxKind::NEWLINE
                    | SyntaxKind::NL
                    | SyntaxKind::EOF
            )
        })
        .collect();
    match significant.iter().rev().nth(1).map(|token| token.kind) {
        Some(SyntaxKind::L_PAREN) => "",
        Some(SyntaxKind::COMMA) => " ",
        _ => ", ",
    }
}
fn encoding_action(file: &Arc<FileInfo>, call: &ast::CallExpr, out: &mut Vec<Intention>) {
    let Some(callee) = call.callee() else { return };
    let Some(args) = call.arg_list() else { return };
    if args
        .args()
        .any(|arg| matches!(arg, ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_)))
    {
        return;
    }
    // Canonical builtin open text calls get an explicit encoding, never binary.
    if matches!(callee,ast::Expr::Name(ref name) if name.name_text()=="open"&&canonical(file,call.text_range().start(),"open"))
        && args.positional().count() < 4
        && !args.keywords().any(|k| k.name_text() == "encoding")
    {
        let mode = args.positional().nth(1).or_else(|| {
            args.keywords()
                .find(|k| k.name_text() == "mode")
                .and_then(|k| k.value())
        });
        let text_mode = mode.is_none_or(
            |mode| matches!(mode,ast::Expr::String(ref string) if !string.value().contains('b')),
        );
        if text_mode {
            if let Some(close) = args
                .syntax()
                .last_token()
                .filter(|token| token.kind() == SyntaxKind::R_PAREN)
            {
                offer(
                    out,
                    TextRange::empty(close.text_range().start()),
                    "Add encoding argument",
                    format!("{}encoding='utf-8'", argument_separator(file, &args)),
                );
            }
        }
    }
}

fn async_actions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    call: &ast::CallExpr,
    out: &mut Vec<Intention>,
) {
    let inf = Inferer::new(db);
    let is_async = call.callee().is_some_and(|callee| {
        let signatures =
            rustpy_semantic::call_help::signatures_of(&inf, &inf.type_of_expr(file, &callee));
        !signatures.is_empty() && signatures.iter().all(|signature| signature.is_async)
    });
    let awaited = call
        .syntax()
        .parent()
        .is_some_and(|node| node.kind() == SyntaxKind::AWAIT_EXPR);
    if !awaited
        && is_async
        && function(file, call.text_range().start()).is_some_and(|function| function.is_async())
    {
        offer(
            out,
            call.text_range(),
            "Add missing await",
            format!("await {}", call.syntax().text()),
        );
    }
}
fn remove_unreachable(file: &FileInfo, statement: &ast::Stmt, out: &mut Vec<Intention>) {
    if !standalone(file, statement.text_range()) {
        return;
    }
    let Some(parent) = statement.syntax().parent() else {
        return;
    };
    let siblings: Vec<_> = parent.children().filter_map(ast::Stmt::cast).collect();
    let Some(position) = siblings
        .iter()
        .position(|s| s.text_range() == statement.text_range())
    else {
        return;
    };
    if siblings[..position].iter().any(|s| {
        matches!(
            s,
            ast::Stmt::Return(_)
                | ast::Stmt::Raise(_)
                | ast::Stmt::Break(_)
                | ast::Stmt::Continue(_)
        )
    }) {
        if let Some(last) = siblings.last() {
            offer(
                out,
                TextRange::new(
                    stmt_lines(&file.text, statement.text_range()).start(),
                    stmt_lines(&file.text, last.text_range()).end(),
                ),
                "Remove unreachable code",
                String::new(),
            );
        }
    }
}
fn scope_declarations(file: &Arc<FileInfo>, name: &ast::NameExpr, out: &mut Vec<Intention>) {
    let Some(function) = function(file, name.text_range().start()) else {
        return;
    };
    let offset = name.text_range().start();
    let name = name.name_text();
    let Some(body) = function.body() else { return };
    let Some((at, indent)) = suite_insert(file, &body) else {
        return;
    };
    if function.params().is_some_and(|params| {
        params
            .params()
            .any(|p| p.name().is_some_and(|n| n.text() == name))
    }) {
        return;
    }
    let scope = file.scopes.scope_at(offset);
    let current = file.scopes.scope(scope);
    if current.globals.iter().any(|n| n == &name) || current.nonlocals.iter().any(|n| n == &name) {
        return;
    }
    if file
        .scopes
        .scope(file.scopes.module_scope())
        .bindings
        .contains_key(name.as_str())
    {
        offer(
            out,
            TextRange::empty(at),
            "Add global declaration",
            format!("global {name}\n{indent}"),
        );
    }
    let mut parent = current.parent;
    while let Some(id) = parent {
        let scope = file.scopes.scope(id);
        if scope.kind == rustpy_semantic::scope::ScopeKind::Function
            && scope.bindings.contains_key(name.as_str())
        {
            offer(
                out,
                TextRange::empty(at),
                "Add nonlocal declaration",
                format!("nonlocal {name}\n{indent}"),
            );
            break;
        }
        parent = scope.parent;
    }
}
fn attribute_initialization(
    file: &Arc<FileInfo>,
    attribute: &ast::AttributeExpr,
    out: &mut Vec<Intention>,
) {
    let Some(ast::Expr::Name(receiver)) = attribute.object() else {
        return;
    };
    if receiver.name_text() != "self" {
        return;
    }
    let Some(method) = function(file, attribute.text_range().start()) else {
        return;
    };
    if method.name_text() == "__init__" {
        return;
    }
    if !method.is_method()
        || method.has_decorator("staticmethod")
        || method.has_decorator("classmethod")
    {
        return;
    }
    let Some(class) = method.syntax().ancestors().find_map(ast::ClassDef::cast) else {
        return;
    };
    let Some(body) = class.body() else { return };
    let name = attribute.attr_text();
    if name.is_empty() {
        return;
    }
    let constructor = class.methods().find(|m| m.name_text() == "__init__");
    if constructor.as_ref().is_some_and(|constructor|constructor.body().is_some_and(|body|body.syntax().descendants().filter_map(ast::AttributeExpr::cast).any(|attribute|attribute.attr_text()==name && is_assignment_target(attribute.syntax()) && matches!(attribute.object(),Some(ast::Expr::Name(receiver)) if receiver.name_text()=="self")))) {return}
    let indent = indent_at(&file.text, usize::from(method.text_range().start()));
    let (at, prefix) = if let Some(constructor) = constructor {
        let Some(body) = constructor.body() else {
            return;
        };
        let Some((at, indent)) = suite_insert(file, &body) else {
            return;
        };
        (at, format!("self.{name} = None\n{indent}"))
    } else {
        let at = stmt_lines(
            &file.text,
            body.statements()
                .last()
                .map(|s| s.text_range())
                .unwrap_or(class.text_range()),
        )
        .end();
        (
            at,
            format!("\n{indent}def __init__(self):\n{indent}    self.{name} = None\n"),
        )
    };
    offer(
        out,
        TextRange::empty(at),
        "Define attribute in init",
        prefix.clone(),
    );
    if let Some(assign) = attribute
        .syntax()
        .ancestors()
        .find_map(ast::AssignStmt::cast)
    {
        if assign.targets().len() == 1 && assign.targets()[0].text_range() == attribute.text_range()
        {
            if let Some(value) = assign.value() {
                if value.syntax().descendants().all(|node| {
                    !matches!(
                        node.kind(),
                        SyntaxKind::NAME_EXPR
                            | SyntaxKind::CALL_EXPR
                            | SyntaxKind::ATTRIBUTE_EXPR
                            | SyntaxKind::LAMBDA_EXPR
                            | SyntaxKind::COMP_FOR
                    )
                }) {
                    let replacement = prefix.replacen("None", &source(file, &value), 1);
                    let mut edits = vec![(TextRange::empty(at), replacement)];
                    let only = method
                        .body()
                        .is_some_and(|body| body.statements().count() == 1);
                    edits.push((
                        stmt_lines(&file.text, assign.text_range()),
                        if only {
                            format!(
                                "{}pass\n",
                                indent_at(&file.text, usize::from(assign.text_range().start()))
                            )
                        } else {
                            String::new()
                        },
                    ));
                    out.push(Intention {
                        title: "Move assignment to init".into(),
                        edits,
                    });
                }
            }
        }
    }
}
fn reorder_exceptions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    try_: &ast::TryStmt,
    out: &mut Vec<Intention>,
) {
    let handlers: Vec<_> = try_.except_clauses().collect();
    if handlers.len() > 64
        || handlers.len() < 2
        || handlers
            .iter()
            .any(|h| h.is_star() || h.exception_type().is_none())
    {
        return;
    }
    let inf = Inferer::new(db);
    let classes = handlers
        .iter()
        .map(|h| match inf.type_of_expr(file, &h.exception_type()?) {
            Type::Class(class, _) => Some(class),
            _ => None,
        })
        .collect::<Option<Vec<_>>>();
    let Some(classes) = classes else { return };
    let mut indices = (0..handlers.len()).collect::<Vec<_>>();
    let mut remaining = std::mem::take(&mut indices);
    while !remaining.is_empty() {
        let Some(position) = remaining.iter().position(|candidate| {
            !remaining.iter().any(|other| {
                other != candidate && inf.mro(&classes[*other]).contains(&classes[*candidate])
            })
        }) else {
            return;
        };
        indices.push(remaining.remove(position));
    }
    if indices.iter().enumerate().all(|(i, index)| i == *index) {
        return;
    }
    let replacement = indices
        .iter()
        .map(|i| {
            slice(
                &file.text,
                stmt_lines(&file.text, handlers[*i].text_range()),
            )
            .to_string()
        })
        .collect::<String>();
    let range = TextRange::new(
        stmt_lines(&file.text, handlers[0].text_range()).start(),
        stmt_lines(&file.text, handlers.last().unwrap().text_range()).end(),
    );
    offer(
        out,
        range,
        "Reorder except clauses from specific to general",
        replacement,
    );
}

fn render_doc(parsed: &ParsedDocstring, format: DocstringFormat) -> String {
    let mut lines = Vec::new();
    if !parsed.summary.is_empty() {
        lines.extend(parsed.summary.lines().map(ToString::to_string));
    }
    if !parsed.description.is_empty() {
        lines.push(String::new());
        lines.extend(parsed.description.lines().map(ToString::to_string));
    }
    if !parsed.params.is_empty() {
        lines.push(String::new());
        match format {
            DocstringFormat::Google => {
                lines.push("Args:".into());
                for (name, ty, description) in &parsed.params {
                    lines.push(format!(
                        "    {name}{}: {}",
                        ty.as_ref().map(|ty| format!(" ({ty})")).unwrap_or_default(),
                        description
                    ));
                }
            }
            DocstringFormat::NumPy => {
                lines.extend(["Parameters".into(), "----------".into()]);
                for (name, ty, description) in &parsed.params {
                    lines.push(format!(
                        "{name}{}",
                        ty.as_ref().map(|ty| format!(" : {ty}")).unwrap_or_default()
                    ));
                    lines.push(format!("    {description}"));
                }
            }
            _ => {
                for (name, ty, description) in &parsed.params {
                    lines.push(format!(":param {name}: {description}"));
                    if let Some(ty) = ty {
                        lines.push(format!(":type {name}: {ty}"));
                    }
                }
            }
        }
    }
    if let Some((ty, description)) = &parsed.returns {
        lines.push(String::new());
        match format {
            DocstringFormat::Google => {
                lines.push("Returns:".into());
                lines.push(format!(
                    "    {}{}",
                    ty.as_ref().map(|ty| format!("{ty}: ")).unwrap_or_default(),
                    description
                ));
            }
            DocstringFormat::NumPy => {
                lines.extend([
                    "Returns".into(),
                    "-------".into(),
                    ty.clone().unwrap_or_else(|| "object".into()),
                    format!("    {description}"),
                ]);
            }
            _ => {
                lines.push(format!(":return: {description}"));
                if let Some(ty) = ty {
                    lines.push(format!(":rtype: {ty}"));
                }
            }
        }
    }
    if !parsed.raises.is_empty() {
        lines.push(String::new());
        match format {
            DocstringFormat::Google => {
                lines.push("Raises:".into());
                for (name, description) in &parsed.raises {
                    lines.push(format!("    {name}: {description}"));
                }
            }
            DocstringFormat::NumPy => {
                lines.extend(["Raises".into(), "------".into()]);
                for (name, description) in &parsed.raises {
                    lines.push(name.clone());
                    lines.push(format!("    {description}"));
                }
            }
            _ => {
                for (name, description) in &parsed.raises {
                    lines.push(format!(":raises {name}: {description}"));
                }
            }
        }
    }
    if !parsed.attributes.is_empty() {
        lines.extend([String::new(), "Attributes:".into()]);
        for (name, description) in &parsed.attributes {
            lines.push(format!("    {name}: {description}"));
        }
    }
    if !parsed.examples.is_empty() {
        lines.extend([String::new(), "Examples:".into()]);
        for example in &parsed.examples {
            lines.extend(example.lines().map(ToString::to_string));
        }
    }
    lines.join("\n")
}
pub fn update_docstring(
    _db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    format: DocstringFormat,
) -> Option<Intention> {
    let function = function(file, offset)?;
    let doc = function.docstring()?;
    let mut parsed = docstrings::parse(&doc.value());
    let params = function.params()?;
    let mut names = Vec::new();
    for (index, param) in params.params().enumerate() {
        let Some(name) = param.name() else { continue };
        if index == 0
            && function.is_method()
            && !function.has_decorator("staticmethod")
            && matches!(name.text(), "self" | "cls")
        {
            continue;
        }
        names.push((
            name.text().to_string(),
            param.annotation().map(|expr| source(file, &expr)),
        ));
    }
    parsed.params.retain(|(name, _, _)| {
        names
            .iter()
            .any(|(present, _)| present == name.trim_start_matches('*'))
    });
    for (name, annotation) in names {
        if !parsed
            .params
            .iter()
            .any(|(present, _, _)| present.trim_start_matches('*') == name)
        {
            parsed.params.push((name, annotation, String::new()));
        }
    }
    if let Some(annotation) = function.return_annotation() {
        if source(file, &annotation) != "None" && parsed.returns.is_none() {
            parsed.returns = Some((Some(source(file, &annotation)), String::new()));
        }
    }
    let indent = indent_at(&file.text, usize::from(doc.syntax().text_range().start()));
    let value = render_doc(&parsed, format);
    let escaped = value
        .replace('\\', "\\\\")
        .replace("\"\"\"", "\\\"\\\"\\\"");
    let mut replacement = String::from("\"\"\"");
    for (index, line) in escaped.lines().enumerate() {
        if index > 0 {
            replacement.push('\n');
            replacement.push_str(&indent);
        }
        replacement.push_str(line);
    }
    replacement.push('\n');
    replacement.push_str(&indent);
    replacement.push_str("\"\"\"");
    Some(Intention {
        title: "Update docstring".into(),
        edits: vec![(doc.syntax().text_range(), replacement)],
    })
}
fn functional_class(
    db: &Analysis,
    file: &Arc<FileInfo>,
    assign: &ast::AssignStmt,
    out: &mut Vec<Intention>,
) {
    if !standalone(file, assign.text_range()) {
        return;
    }
    let targets = assign.targets();
    if targets.len() != 1 {
        return;
    }
    let ast::Expr::Name(target) = &targets[0] else {
        return;
    };
    let Some(ast::Expr::Call(call)) = assign.value() else {
        return;
    };
    let Some(head) = call.callee() else { return };
    let Some(kind) = typing_head(db, file, &head) else {
        return;
    };
    if !matches!(kind.as_str(), "NamedTuple" | "TypedDict") {
        return;
    }
    let Some(args) = call.arg_list() else { return };
    if args
        .args()
        .any(|arg| !matches!(arg, ast::AnyArg::Positional(_) | ast::AnyArg::Keyword(_)))
    {
        return;
    }
    let positional: Vec<_> = args.positional().collect();
    if positional.len() != 2 {
        return;
    }
    let ast::Expr::String(name) = &positional[0] else {
        return;
    };
    if name.value() != target.name_text() {
        return;
    }
    let mut fields = Vec::new();
    match &positional[1] {
        ast::Expr::List(list) => {
            for pair in list.elements() {
                let ast::Expr::Tuple(tuple) = pair else {
                    return;
                };
                let pair: Vec<_> = tuple.elements().collect();
                if pair.len() != 2 {
                    return;
                }
                let ast::Expr::String(name) = &pair[0] else {
                    return;
                };
                fields.push((name.value(), source(file, &pair[1])));
            }
        }
        ast::Expr::Tuple(tuple) => {
            for pair in tuple.elements() {
                let ast::Expr::Tuple(tuple) = pair else {
                    return;
                };
                let pair: Vec<_> = tuple.elements().collect();
                if pair.len() != 2 {
                    return;
                }
                let ast::Expr::String(name) = &pair[0] else {
                    return;
                };
                fields.push((name.value(), source(file, &pair[1])));
            }
        }
        ast::Expr::Dict(dict) if kind == "TypedDict" => {
            if dict.unpacks().next().is_some() {
                return;
            }
            for item in dict.items() {
                let Some(ast::Expr::String(name)) = item.key() else {
                    return;
                };
                let Some(value) = item.value() else { return };
                fields.push((name.value(), source(file, &value)));
            }
        }
        _ => return,
    }
    let mut seen = std::collections::HashSet::new();
    if fields.iter().any(|(name, _)| {
        rustpy_refactor::rename::check_identifier(name).is_err() || !seen.insert(name.clone())
    }) {
        return;
    }
    let keywords = args
        .keywords()
        .map(|k| k.syntax().text().to_string())
        .collect::<Vec<_>>();
    if kind == "NamedTuple" && !keywords.is_empty() {
        return;
    }
    let indent = indent_at(&file.text, usize::from(assign.text_range().start()));
    let mut replacement = format!(
        "class {}({}{}):\n",
        target.name_text(),
        source(file, &head),
        if keywords.is_empty() {
            String::new()
        } else {
            format!(", {}", keywords.join(", "))
        }
    );
    for (name, ty) in fields {
        replacement.push_str(&format!("{indent}    {name}: {ty}\n"));
    }
    if seen.is_empty() {
        replacement.push_str(&format!("{indent}    pass\n"));
    }
    offer(
        out,
        assign.text_range(),
        &format!("Convert functional {kind} to class"),
        replacement.trim_end_matches('\n').into(),
    );
}
fn enum_auto(db: &Analysis, file: &Arc<FileInfo>, class: &ast::ClassDef, out: &mut Vec<Intention>) {
    let real_enum = class.bases().iter().any(|base| {
        matches!(
            imported_head(db, file, base, &["enum"]).as_deref(),
            Some("Enum" | "IntEnum" | "Flag" | "IntFlag" | "StrEnum")
        )
    });
    if !real_enum {
        return;
    }
    let Some(body) = class.body() else { return };
    let mut imports = Vec::new();
    let auto = import_type(
        file,
        class.text_range().start(),
        "enum",
        "auto",
        &mut imports,
    );
    let mut values = Vec::<(String, String)>::new();
    let mut edits = Vec::new();
    for statement in body.statements() {
        let ast::Stmt::Assign(assign) = statement else {
            continue;
        };
        let targets = assign.targets();
        if targets.len() != 1 {
            return;
        }
        let ast::Expr::Name(name) = &targets[0] else {
            return;
        };
        if name.name_text().starts_with('_') {
            continue;
        }
        let Some(value) = assign.value() else { return };
        if !matches!(
            value,
            ast::Expr::Literal(_) | ast::Expr::String(_) | ast::Expr::Unary(_) | ast::Expr::Name(_)
        ) {
            return;
        }
        let value_text = source(file, &value);
        let replacement = if let Some((name, _)) = values.iter().find(|(_, v)| v == &value_text) {
            name.clone()
        } else if matches!(value, ast::Expr::Name(_)) {
            value_text.clone()
        } else {
            format!("{auto}()")
        };
        values.push((name.name_text(), value_text));
        if replacement != source(file, &value) {
            edits.push((value.text_range(), replacement));
        }
    }
    if !edits.is_empty() {
        edits.extend(imports);
        out.push(Intention {
            title: "Convert enum values to auto".into(),
            edits,
        });
    }
}
pub fn at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Intention> {
    let root = file.parse.syntax_node();
    let mut out = Vec::new();
    let Some(token) = token_at(&root, offset) else {
        return out;
    };
    let statement = token.parent_ancestors().find_map(ast::Stmt::cast);
    if let Some(statement) = &statement {
        statement_actions(db, file, statement, &mut out);
        if let ast::Stmt::Assign(assign) = statement {
            functional_class(db, file, assign, &mut out);
        }
    }
    if let Some(function) = function(file, offset) {
        function_actions(db, file, &function, &mut out);
        function_comments(file, &function, &mut out);
    }
    if let Some(class) = token.parent_ancestors().find_map(ast::ClassDef::cast) {
        class_actions(db, file, &class, &mut out);
        enum_auto(db, file, &class, &mut out);
    }
    if let Some(parameter) = token.parent_ancestors().find_map(ast::Param::cast) {
        parameter_actions(file, &parameter, &mut out);
    }
    if let Some(call) = token.parent_ancestors().find_map(ast::CallExpr::cast) {
        encoding_action(file, &call, &mut out);
        call_actions(db, file, &call, &mut out);
        async_actions(db, file, &call, &mut out);
    }
    if let Some(attribute) = token.parent_ancestors().find_map(ast::AttributeExpr::cast) {
        attribute_initialization(file, &attribute, &mut out);
    }
    if let Some(name) = token.parent_ancestors().find_map(ast::NameExpr::cast) {
        scope_declarations(file, &name, &mut out);
    }
    if let Some(await_) = token.parent_ancestors().find_map(ast::AwaitExpr::cast) {
        if let Some(operand) = await_.expr() {
            offer(
                &mut out,
                await_.text_range(),
                "Remove await",
                source(file, &operand),
            );
        }
    }
    if let Some(function) = function(file, offset) {
        if !function.is_async() {
            if let Some(token) = function.def_token() {
                offer(
                    &mut out,
                    TextRange::empty(token.text_range().start()),
                    "Convert to async def",
                    "async ".into(),
                );
            }
        }
    }
    let annotation = token
        .parent_ancestors()
        .find(|node| {
            matches!(
                node.kind(),
                SyntaxKind::ANNOTATION | SyntaxKind::RETURN_ANNOTATION
            )
        })
        .and_then(|node| node.children().find_map(ast::Expr::cast));
    if let Some(annotation) = annotation {
        if let Some(modern) = modernize_type(db, file, &annotation, 0) {
            if modern != source(file, &annotation) {
                offer(
                    &mut out,
                    annotation.text_range(),
                    "Modernize type hint",
                    modern,
                )
            }
        }
        if token
            .parent_ancestors()
            .any(|node| node.kind() == SyntaxKind::ANN_ASSIGN_STMT)
        {
            let inner = match annotation.clone().unparen() {
                ast::Expr::Subscript(subscript)
                    if subscript.object().is_some_and(|head| {
                        typing_head(db, file, &head).as_deref() == Some("Final")
                    }) =>
                {
                    subscript.index_expr()
                }
                _ => None,
            };
            if let Some(inner) = inner {
                offer(
                    &mut out,
                    annotation.text_range(),
                    "Remove Final",
                    source(file, &inner),
                );
            } else {
                let mut imports = Vec::new();
                let final_ = import_type(
                    file,
                    annotation.text_range().start(),
                    "typing",
                    "Final",
                    &mut imports,
                );
                imports.push((
                    annotation.text_range(),
                    format!("{final_}[{}]", source(file, &annotation)),
                ));
                out.push(Intention {
                    title: "Make Final".into(),
                    edits: imports,
                });
            }
        }
    }
    // Empty compound headers need a real indented suite in recovery mode.
    let line = line_start(&file.text, usize::from(offset));
    let end = file.text[line..]
        .find('\n')
        .map_or(file.text.len(), |at| line + at);
    if file.text[line..end].trim_end().ends_with(':') && file.text[end..].trim().is_empty() {
        let indent = indent_at(&file.text, line);
        offer(
            &mut out,
            TextRange::empty(TextSize::new(file.text.len() as u32)),
            "Insert pass",
            format!(
                "{}{}    pass\n",
                if file.text.ends_with('\n') { "" } else { "\n" },
                indent
            ),
        );
    }
    // The editor applies edits right-to-left. Combine coincident insertions so
    // imports and a new decorator at offset zero retain their source order.
    for intention in &mut out {
        let mut merged: Vec<Edit> = Vec::new();
        for (range, text) in std::mem::take(&mut intention.edits) {
            if range.is_empty() {
                if let Some((_, previous)) = merged.iter_mut().find(|(at, _)| *at == range) {
                    previous.push_str(&text);
                    continue;
                }
            }
            merged.push((range, text));
        }
        intention.edits = merged;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn setup(text: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/project"));
        let id = db.set_file_text(Path::new("/project/main.py"), text);
        let file = db.file(id).unwrap();
        (db, file)
    }
    fn choices(text: &str, needle: &str) -> Vec<Intention> {
        let (db, file) = setup(text);
        crate::intentions::intentions_at(
            &db,
            &file,
            TextSize::new(text.find(needle).unwrap() as u32),
        )
    }
    fn edit(text: &str, needle: &str, title: &str) -> String {
        let action = choices(text, needle)
            .into_iter()
            .find(|action| action.title == title)
            .unwrap_or_else(|| panic!("missing intention {title}"));
        let result = apply_edits(text, &action.edits);
        assert!(
            rustpy_syntax::parse(&result).errors().is_empty(),
            "invalid edit: {result}"
        );
        result
    }
    #[test]
    fn variable_hints_and_object_stubs() {
        let text = "value = 42\n";
        assert_eq!(
            edit(text, "value", "Add inferred variable type hint"),
            "value: int = 42\n"
        );
        assert_eq!(
            edit(text, "value", "Add object variable type hint"),
            "value: object = 42\n"
        );
        let text = "def f(value):\n    pass\n";
        assert!(
            edit(text, "value", "Add object type hint for parameter 'value'")
                .contains("value: object")
        );
        assert!(edit(text, "f(", "Add return annotation None").contains("def f(value) -> None:"));
    }
    #[test]
    fn assignment_and_function_type_comments_roundtrip() {
        assert!(edit(
            "value = 1  # type: int\n",
            "value",
            "Convert type comment to annotation"
        )
        .contains("value: int = 1"));
        assert_eq!(
            edit(
                "value: int = 1\n",
                "value",
                "Convert annotation to type comment"
            ),
            "value = 1  # type: int\n"
        );
        let source = "def f(value):  # type: (int) -> str\n    return str(value)\n";
        let typed = edit(source, "f(", "Convert function type comment to annotations");
        assert!(typed.contains("def f(value: int) -> str:"));
        assert!(
            edit(&typed, "f(", "Convert function annotations to type comment")
                .contains("# type: (int) -> str")
        );
        let body = "def f(value):\n    # type: (int) -> str\n    return str(value)\n";
        assert!(
            edit(body, "f(", "Convert function type comment to annotations").contains("value: int")
        );
    }
    #[test]
    fn function_type_comment_removal_keeps_defaults_and_variadic_parameters() {
        let source =
            "def f(value: int = 1, *args: str, **kwargs: bool) -> str:\n    return str(value)\n";
        let converted = edit(source, "f(", "Convert function annotations to type comment");
        assert!(
            converted.contains("def f(value = 1, *args, **kwargs):"),
            "{converted}"
        );
        assert!(
            converted.contains("# type: (int, *str, **bool) -> str"),
            "{converted}"
        );
        let restored = edit(
            &converted,
            "f(",
            "Convert function type comment to annotations",
        );
        assert!(restored.contains("value: int = 1"), "{restored}");
        assert!(
            restored.contains("*args: str, **kwargs: bool) -> str:"),
            "{restored}"
        );
    }
    #[test]
    fn modern_typing_aliases_and_final() {
        let source = "from typing import List as L, Optional\nvalue: L[Optional[int]] = []\n";
        let modern = edit(source, "L[", "Modernize type hint");
        assert!(modern.contains("value: list[(int) | None]"));
        assert!(edit("value: int = 1\n", "int", "Make Final").contains("value: Final[int]"));
        assert!(edit(
            "from typing import Final\nvalue: Final[int] = 1\n",
            "Final[int]",
            "Remove Final"
        )
        .contains("value: int"));
        assert!(!choices(
            "class List:\n    pass\nvalue: List[int] = []\n",
            "List[int]"
        )
        .iter()
        .any(|a| a.title == "Modernize type hint"));
    }
    #[test]
    fn optional_default_and_mutable_initialization() {
        let source="def f(value: list[int] = []):\n    \"\"\"Keep documentation.\"\"\"\n    return value\n";
        let fixed = edit(
            source,
            "value:",
            "Replace mutable defaults with None and initialize",
        );
        assert!(fixed.contains("value: (list[int]) | None = None"));
        assert!(fixed.contains("\"\"\"Keep documentation.\"\"\"\n    if value is None:\n        value = []\n    return value"));
        assert!(edit(
            "def f(value: int = None):\n    pass\n",
            "value:",
            "Add Optional for None default"
        )
        .contains("value: (int) | None"));
    }
    #[test]
    fn missing_and_unexpected_call_arguments_with_trailing_comma() {
        let source = "def f(first, second, *, flag):\n    pass\nf(1,)\n";
        let fixed = edit(source, "f(1", "Add missing arguments to call");
        assert!(fixed.contains("f(1, second=None, flag=None)"));
        let source = "def f(value):\n    pass\nf(1, extra=2)\n";
        let fixed = edit(source, "extra=", "Remove unexpected argument");
        assert!(!fixed.contains("extra="));
    }
    #[test]
    fn project_dialog_requests_preserve_authority() {
        let source = "def CamelCase(value):\n    return value\nCamelCase(extra=1)\n";
        let (db, file) = setup(source);
        let requests = ui_requests(
            &db,
            &file,
            TextSize::new(source.find("CamelCase").unwrap() as u32),
        );
        assert!(requests.iter().any(
            |(_, request)| matches!(request,RequestAction::Rename(Some(name)) if name=="camel_case")
        ));
        assert!(requests
            .iter()
            .any(|(_, request)| matches!(request, RequestAction::SafeDelete)));
        let requests = ui_requests(
            &db,
            &file,
            TextSize::new(source.find("extra=").unwrap() as u32),
        );
        assert!(requests.iter().any(
            |(_, request)| matches!(request,RequestAction::CallParameter{name,..} if name=="extra")
        ));
        let requests = ui_requests(
            &db,
            &file,
            TextSize::new(source.find("value)").unwrap() as u32),
        );
        assert!(requests.iter().any(|(_, request)| matches!(
            request,
            RequestAction::Parameter {
                change: ParameterChange::KeywordOnly,
                ..
            }
        )));
    }
    #[test]
    fn docstring_update_removes_stale_and_preserves_description() {
        let source="def f(current: int) -> str:\n    \"\"\"Summary.\n\n    Args:\n        removed (str): old\n\n    Raises:\n        ValueError: invalid\n    \"\"\"\n    return str(current)\n";
        let (db, file) = setup(source);
        let action =
            update_docstring(&db, &file, TextSize::new(4), DocstringFormat::NumPy).unwrap();
        let result = apply_edits(source, &action.edits);
        assert!(!result.contains("removed"));
        assert!(result.contains("current : int"));
        assert!(result.contains("Summary."));
        assert!(result.contains("ValueError"));
        assert!(result.contains("Returns\n    -------\n    str"));
        assert!(rustpy_syntax::parse(&result).errors().is_empty());
    }
    #[test]
    fn class_generation_and_attribute_initialization() {
        let source = "class C:\n    value: int\n    def use(self):\n        return self.value\n";
        assert!(choices(source, "class C")
            .iter()
            .any(|a| a.title == "Add __init__"));
        assert!(edit(source, "self.value", "Define attribute in init")
            .contains("def __init__(self):\n        self.value = None"));
        assert!(
            edit(source, "class C", "Add @dataclass").contains("from dataclasses import dataclass")
        );
        let source = "class C:\n    def use(self):\n        self.value = 1\n";
        let fixed = edit(source, "self.value", "Move assignment to init");
        assert!(fixed.contains("self.value = 1"));
        assert!(fixed.contains("def use(self):\n        pass"));
        let local = "class C:\n    def use(self, value):\n        self.value = [value]\n";
        assert!(!choices(local, "self.value")
            .iter()
            .any(|a| a.title == "Move assignment to init"));
    }
    #[test]
    fn functional_typed_records_and_enum_aliases() {
        let source="from typing import NamedTuple\nPair = NamedTuple('Pair', [('left', int), ('right', str)])\n";
        assert!(
            edit(source, "Pair =", "Convert functional NamedTuple to class")
                .contains("class Pair(NamedTuple):\n    left: int\n    right: str")
        );
        let source="from typing import TypedDict\nOptions = TypedDict('Options', {'name': str}, total=False)\n";
        assert!(
            edit(source, "Options =", "Convert functional TypedDict to class")
                .contains("class Options(TypedDict, total=False):")
        );
        let source =
            "from enum import Enum\nclass E(Enum):\n    FIRST = 1\n    ALIAS = 1\n    SECOND = 2\n";
        let fixed = edit(source, "class E", "Convert enum values to auto");
        assert!(fixed.contains("FIRST = auto()"));
        assert!(fixed.contains("ALIAS = FIRST"));
        assert!(fixed.contains("SECOND = auto()"));
    }
    #[test]
    fn exception_repairs_and_scope_declarations() {
        let source="def f():\n    try:\n        work()\n    except ValueError as error:\n        raise RuntimeError('bad')\n";
        assert!(edit(
            source,
            "raise RuntimeError",
            "Add exception cause from handler"
        )
        .contains("raise RuntimeError('bad') from error"));
        assert!(edit(
            "def f():\n    raise NotImplemented\n",
            "NotImplemented",
            "Replace NotImplemented with NotImplementedError"
        )
        .contains("raise NotImplementedError"));
        assert!(edit(
            "value = 1\ndef f():\n    value = 2\n",
            "value = 2",
            "Add global declaration"
        )
        .contains("global value\n    value = 2"));
        assert!(edit(
            "def outer():\n    value = 1\n    def inner():\n        value = 2\n",
            "value = 2",
            "Add nonlocal declaration"
        )
        .contains("nonlocal value"));
    }
    #[test]
    fn await_encoding_and_recovery_actions() {
        let source = "async def value():\n    return 1\nasync def use():\n    value()\n";
        assert!(edit(source, "value()\n", "Add missing await").contains("await value()"));
        assert!(edit("open('file.txt')\n", "open(", "Add encoding argument")
            .contains("encoding='utf-8'"));
        assert!(
            !choices("def open(path):\n    pass\nopen('x')\n", "open('x')")
                .iter()
                .any(|a| a.title == "Add encoding argument")
        );
        assert!(choices("if True:\n", "if True")
            .iter()
            .any(|a| a.title == "Insert pass"));
    }
    #[test]
    fn return_pass_and_unreachable_edits() {
        assert!(edit(
            "def f():\n    value()\n",
            "value()",
            "Add return to expression"
        )
        .contains("return value()"));
        assert!(
            edit("def f():\n    pass\n", "pass", "Replace pass with ellipsis").contains("    ...")
        );
        let source = "def f():\n    return 1\n    value()\n    other()\n";
        let fixed = edit(source, "value()", "Remove unreachable code");
        assert!(!fixed.contains("value()"));
        assert!(!fixed.contains("other()"));
    }
    #[test]
    fn type_authority_and_shared_line_guards_preserve_user_code() {
        let source = "import typing\ntyping.List = object\nvalue: typing.List[int] = []\n";
        assert!(!choices(source, "typing.List[int]")
            .iter()
            .any(|action| action.title == "Modernize type hint"));
        let source = "value = 1; other = 2  # type: int\n";
        assert!(!choices(source, "value")
            .iter()
            .any(|action| action.title == "Convert type comment to annotation"));
        let source = "object = 1\ndef f(value):\n    pass\n";
        let typed = edit(
            source,
            "value",
            "Add object type hint for parameter 'value'",
        );
        assert!(typed.contains("from builtins import object as _object"));
        assert!(typed.contains("value: _object"));
        assert!(
            !choices("def f(value: int) -> int:\n    return value\n", "value:")
                .iter()
                .any(|action| action.title == "Make Final")
        );
    }
    #[test]
    fn decorators_and_imports_at_same_offset_keep_python_order() {
        let source = "class C:\n    value: int\n";
        let text = edit(source, "class C", "Add @dataclass");
        assert!(text.starts_with("from dataclasses import dataclass\n@dataclass\nclass C:"));
    }
}
