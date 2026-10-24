//! Checked class-member generation plans over the actual immutable Python CST.
use rustpy_core::{TextRange, TextSize};
use rustpy_semantic::edit_helpers::{ancestor_at, indent_at, token_at, Edit};
use rustpy_semantic::types::ClassRef;
use rustpy_semantic::{Analysis, FileInfo, Inferer};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind, SyntaxNode,
};
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub annotation: Option<String>,
    pub default: Option<String>,
    pub init_only: bool,
    pub init: bool,
    pub factory: Option<String>,
}
#[derive(Clone, Debug)]
pub struct ClassTarget {
    pub name: String,
    pub range: TextRange,
    pub fields: Vec<Field>,
    pub methods: BTreeSet<String>,
    pub dataclass: bool,
    source: String,
    class: ast::ClassDef,
    pub indent: String,
}
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub fields: Vec<String>,
    pub constructor: bool,
    pub dunders: Vec<String>,
    pub property: Option<Property>,
    pub slots: bool,
    pub post_init: bool,
    pub inherited: Vec<InheritedMethod>,
}
#[derive(Clone, Debug)]
pub struct Property {
    pub field: String,
    pub getter: bool,
    pub setter: bool,
    pub deleter: bool,
}
#[derive(Clone, Debug)]
pub struct InheritedMethod {
    pub name: String,
    pub abstract_: bool,
    pub signature: String,
    pub owner: String,
    pub source_path: std::path::PathBuf,
    pub source: Arc<str>,
    definition: ast::FuncDef,
    kind: rustpy_semantic::classes::MemberKind,
    header: String,
    import: Option<(String, String)>,
}

pub const DUNDERS: &[(&str, &str)] = &[
    ("__repr__", "repr(self)"),
    ("__str__", "str(self)"),
    ("__eq__", "Equality"),
    ("__hash__", "Hash"),
    ("__lt__", "Less than"),
    ("__le__", "Less than or equal"),
    ("__gt__", "Greater than"),
    ("__ge__", "Greater than or equal"),
    ("__bool__", "Truth value"),
    ("__len__", "Length"),
    ("__iter__", "Iterator"),
    ("__next__", "Next item"),
    ("__getitem__", "Get item"),
    ("__setitem__", "Set item"),
    ("__delitem__", "Delete item"),
    ("__contains__", "Membership"),
    ("__call__", "Call"),
    ("__enter__", "Context entry"),
    ("__exit__", "Context exit"),
    ("__add__", "Addition"),
    ("__sub__", "Subtraction"),
    ("__mul__", "Multiplication"),
    ("__truediv__", "Division"),
];

impl ClassTarget {
    pub fn capture(source: &str, root: &SyntaxNode, offset: TextSize) -> Result<Self, String> {
        if source.len() > 1024 * 1024 || root.text() != source {
            return Err("Class source is unavailable or too large".into());
        }
        let class =
            ancestor_at::<ast::ClassDef>(root, offset).ok_or("Place the caret inside a class")?;
        let mut ind = crate::refactor2::class_body_indent(source, &class);
        if ind.is_empty() {
            ind = format!(
                "{}    ",
                indent_at(source, usize::from(class.syntax().text_range().start()))
            );
        }
        let mut fields = Vec::new();
        if let Some(body) = class.body() {
            for stmt in body.statements() {
                if let ast::Stmt::AnnAssign(assign) = stmt {
                    let (Some(ast::Expr::Name(name)), Some(annotation)) =
                        (assign.target(), assign.annotation())
                    else {
                        continue;
                    };
                    let annotation = annotation.syntax().text().to_string();
                    let compact = annotation.replace(' ', "");
                    if compact.starts_with("ClassVar[") || compact.starts_with("typing.ClassVar[") {
                        continue;
                    }
                    let init_only = compact.starts_with("InitVar[")
                        || compact.starts_with("dataclasses.InitVar[");
                    let ty = if init_only {
                        compact
                            .split_once('[')
                            .and_then(|(_, s)| s.strip_suffix(']'))
                            .unwrap_or(&annotation)
                            .to_string()
                    } else {
                        annotation
                    };
                    let mut default = assign
                        .value()
                        .map(|value| value.syntax().text().to_string());
                    let mut factory = None;
                    let mut init = true;
                    if let Some(ast::Expr::Call(call)) = assign.value() {
                        if call.callee().is_some_and(|callee| {
                            matches!(
                                callee.syntax().text().to_string().as_str(),
                                "field" | "dataclasses.field"
                            )
                        }) {
                            default = None;
                            if let Some(args) = call.arg_list() {
                                for arg in args.keywords() {
                                    let value =
                                        arg.value().map(|value| value.syntax().text().to_string());
                                    match arg.name_text().as_str() {
                                        "default" => default = value,
                                        "default_factory" => factory = value,
                                        "init" => init = value.as_deref() != Some("False"),
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    fields.push(Field {
                        name: name.name_text(),
                        annotation: Some(ty),
                        default,
                        init_only,
                        init,
                        factory,
                    });
                }
            }
        }
        for node in class.syntax().descendants() {
            if node
                .ancestors()
                .find(|node| node.kind() == SyntaxKind::CLASS_DEF)
                .is_none_or(|owner| owner != *class.syntax())
            {
                continue;
            }
            let targets: Vec<_> = if let Some(assign) = ast::AssignStmt::cast(node.clone()) {
                assign.targets()
            } else if let Some(assign) = ast::AnnAssignStmt::cast(node) {
                assign.target().into_iter().collect()
            } else {
                continue;
            };
            for target in targets {
                let ast::Expr::Attribute(attr) = target else {
                    continue;
                };
                let Some(ast::Expr::Name(receiver)) = attr.object() else {
                    continue;
                };
                if receiver.name_text() != "self" {
                    continue;
                }
                let Some(name) = attr.attr() else { continue };
                let name = name.text().to_string();
                if !fields.iter().any(|field| field.name == name) {
                    fields.push(Field {
                        name,
                        annotation: None,
                        default: None,
                        init_only: false,
                        init: true,
                        factory: None,
                    });
                }
            }
        }
        if fields.len() > 256 {
            return Err("This class has too many fields for the Generate picker".into());
        }
        let methods = class
            .body()
            .into_iter()
            .flat_map(|body| body.statements())
            .filter_map(|stmt| {
                if let ast::Stmt::FuncDef(method) = stmt {
                    Some(method.name_text())
                } else {
                    None
                }
            })
            .collect();
        let dataclass = class.decorators().any(|decorator| {
            decorator
                .name_text()
                .is_some_and(|name| name == "dataclass" || name == "dataclasses.dataclass")
        });
        Ok(Self {
            name: class.name_text(),
            range: class.syntax().text_range(),
            fields,
            methods,
            dataclass,
            source: source.into(),
            class,
            indent: ind,
        })
    }
    pub fn source(&self) -> &str {
        &self.source
    }
}

pub fn generate(target: &ClassTarget, request: &Request) -> Result<Vec<Edit>, String> {
    let names: HashSet<_> = request.fields.iter().collect();
    if request
        .fields
        .iter()
        .any(|name| !target.fields.iter().any(|field| &field.name == name))
    {
        return Err("A selected field is no longer available".into());
    }
    let fields: Vec<_> = target
        .fields
        .iter()
        .filter(|field| names.contains(&field.name))
        .collect();
    let ordinary: Vec<_> = fields
        .iter()
        .copied()
        .filter(|field| !field.init_only)
        .collect();
    let mut members = Vec::new();
    let mut generated = HashSet::new();
    let ind = &target.indent;
    let mut add = |name: &str, body: String, is_method: bool| -> Result<(), String> {
        if target.methods.contains(name) || !generated.insert(name.to_string()) {
            return Err(format!("'{name}' already exists or was selected twice"));
        }
        members.push((is_method, body));
        Ok(())
    };
    if request.constructor {
        let mut sentinels = std::collections::BTreeMap::new();
        for field in &fields {
            if field.factory.is_some() && field.init {
                let mut name = format!("_generated_missing_{}", field.name);
                while target.source.contains(&name) {
                    name.push('_');
                }
                add(&name, format!("{ind}{name} = object()\n"), false)?;
                sentinels.insert(field.name.clone(), name);
            }
        }
        let mut params = fields
            .iter()
            .copied()
            .filter(|field| field.init)
            .collect::<Vec<_>>();
        params.sort_by_key(|field| field.default.is_some() || field.factory.is_some());
        let params = params
            .iter()
            .map(|field| {
                format!(
                    "{}{}{}",
                    field.name,
                    field
                        .annotation
                        .as_ref()
                        .map(|ty| format!(": {ty}"))
                        .unwrap_or_default(),
                    sentinels
                        .get(&field.name)
                        .or(field.default.as_ref())
                        .map(|default| format!(" = {default}"))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>();
        if target.dataclass
            && (request.post_init || target.methods.contains("__post_init__"))
            && target
                .fields
                .iter()
                .any(|field| field.init_only && !names.contains(&field.name))
        {
            return Err(
                "Select all InitVar parameters when generating a constructor with __post_init__"
                    .into(),
            );
        }
        let mut body = ordinary
            .iter()
            .filter_map(|field| {
                let value = if let Some(factory) = &field.factory {
                    if let Some(sentinel) = sentinels.get(&field.name) {
                        format!(
                            "{factory}() if {} is self.{sentinel} else {}",
                            field.name, field.name
                        )
                    } else {
                        format!("{factory}()")
                    }
                } else if field.init {
                    field.name.clone()
                } else {
                    field.default.clone()?
                };
                Some(format!(
                    "{ind}    self.{} = {value}\n",
                    storage_name(request, &field.name)
                ))
            })
            .collect::<String>();
        if target.dataclass && (request.post_init || target.methods.contains("__post_init__")) {
            let args = target
                .fields
                .iter()
                .filter(|field| field.init_only)
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            body.push_str(&format!("{ind}    self.__post_init__({args})\n"));
        }
        let args = if params.is_empty() {
            String::new()
        } else {
            format!(", {}", params.join(", "))
        };
        add(
            "__init__",
            format!(
                "{ind}def __init__(self{args}) -> None:\n{}",
                if body.is_empty() {
                    format!("{ind}    pass\n")
                } else {
                    body
                }
            ),
            true,
        )?;
    }
    for name in &request.dunders {
        if !DUNDERS.iter().any(|(dunder, _)| name == dunder) {
            return Err(format!("Unknown dunder method: {name}"));
        }
        let values = ordinary
            .iter()
            .map(|field| format!("self.{}", field.name))
            .collect::<Vec<_>>();
        let tuple = if values.is_empty() {
            "()".into()
        } else {
            format!("({},)", values.join(", "))
        };
        let body=match name.as_str(){
            "__repr__"=>format!("{ind}def __repr__(self) -> str:\n{ind}    return f\"{{type(self).__name__}}({})\"\n",ordinary.iter().map(|field|format!("{}={{self.{}!r}}",field.name,field.name)).collect::<Vec<_>>().join(", ")),
            "__str__"=>format!("{ind}def __str__(self) -> str:\n{ind}    return repr(self)\n"),
            "__hash__"=>format!("{ind}def __hash__(self) -> int:\n{ind}    return hash({tuple})\n"),
            "__eq__"|"__lt__"|"__le__"|"__gt__"|"__ge__"=>{let operator=match name.as_str(){"__eq__"=>"==","__lt__"=>"<","__le__"=>"<=","__gt__"=>">",_=>">="};format!("{ind}def {name}(self, other: object) -> bool:\n{ind}    if type(other) is not type(self):\n{ind}        return NotImplemented\n{ind}    return {tuple} {operator} {}\n",tuple.replace("self.","other."))},
            _=>{let params=match name.as_str(){"__getitem__"|"__delitem__"=>"self, key","__setitem__"=>"self, key, value","__contains__"=>"self, item","__call__"=>"self, *args, **kwargs","__exit__"=>"self, exc_type, exc_value, traceback","__add__"|"__sub__"|"__mul__"|"__truediv__"=>"self, other",_=>"self"};format!("{ind}def {name}({params}):\n{ind}    raise NotImplementedError\n")}
        };
        add(name, body, true)?;
    }
    let mut changes = Vec::new();
    if let Some(property) = &request.property {
        if !property.getter && (property.setter || property.deleter) {
            return Err("Generate a getter together with its setter or deleter".into());
        }
        let field = target
            .fields
            .iter()
            .find(|field| field.name == property.field)
            .ok_or("Selected property field is unavailable")?;
        if field.init_only {
            return Err("InitVar is not an instance field".into());
        }
        let name = field.name.strip_prefix('_').unwrap_or(&field.name);
        if name.is_empty() {
            return Err("Property needs a name".into());
        }
        let backing = format!("_{name}");
        if property.getter {
            let ret = field
                .annotation
                .as_ref()
                .map(|ty| format!(" -> {ty}"))
                .unwrap_or_default();
            let mut body = format!(
                "{ind}@property\n{ind}def {name}(self){ret}:\n{ind}    return self.{backing}\n"
            );
            if property.setter {
                body.push_str(&format!("\n{ind}@{name}.setter\n{ind}def {name}(self, value{}) -> None:\n{ind}    self.{backing} = value\n",field.annotation.as_ref().map(|ty|format!(": {ty}")).unwrap_or_default()));
            }
            if property.deleter {
                body.push_str(&format!("\n{ind}@{name}.deleter\n{ind}def {name}(self) -> None:\n{ind}    del self.{backing}\n"));
            }
            add(name, body, true)?;
            if field.name != backing {
                // Redirect only declarations/writes belonging to this class,
                // preserving public reads through the generated property.
                for node in target.class.syntax().descendants() {
                    if node
                        .ancestors()
                        .find(|node| node.kind() == SyntaxKind::CLASS_DEF)
                        .is_none_or(|owner| owner != *target.class.syntax())
                    {
                        continue;
                    }
                    let targets: Vec<_> = if let Some(assign) = ast::AssignStmt::cast(node.clone())
                    {
                        assign.targets()
                    } else if let Some(assign) = ast::AnnAssignStmt::cast(node) {
                        assign.target().into_iter().collect()
                    } else {
                        continue;
                    };
                    for target_node in targets {
                        match target_node {
                            ast::Expr::Attribute(attr)
                                if matches!(attr.object(),Some(ast::Expr::Name(receiver))if receiver.name_text()=="self")
                                    && instance_receiver(&attr, target.class.syntax()) =>
                            {
                                if let Some(token) =
                                    attr.attr().filter(|token| token.text() == field.name)
                                {
                                    changes.push((token.text_range(), backing.clone()));
                                }
                            }
                            ast::Expr::Name(own)
                                if own.name_text() == field.name
                                    && !own
                                        .syntax()
                                        .ancestors()
                                        .take_while(|node| node != target.class.syntax())
                                        .any(|node| node.kind() == SyntaxKind::FUNC_DEF) =>
                            {
                                if let Some(token) = own.name_token() {
                                    changes.push((token.text_range(), backing.clone()));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    if request.slots {
        if target.dataclass
            && target
                .class
                .decorators()
                .filter_map(|decorator| decorator.expr())
                .filter_map(|expr| {
                    if let ast::Expr::Call(call) = expr {
                        Some(call)
                    } else {
                        None
                    }
                })
                .any(|call| {
                    call.arg_list().is_some_and(|args| {
                        args.keywords().any(|arg| {
                            arg.name_text() == "slots"
                                && arg
                                    .value()
                                    .is_some_and(|value| value.syntax().text() == "True")
                        })
                    })
                })
        {
            return Err("The dataclass decorator already generates slots".into());
        }
        if crate::refactor2::defined_names(&target.class).contains("__slots__") {
            return Err("'__slots__' already exists".into());
        }
        if ordinary
            .iter()
            .any(|field| target.methods.contains(&storage_name(request, &field.name)))
        {
            return Err("A selected slot conflicts with an existing method".into());
        }
        for stmt in target
            .class
            .body()
            .into_iter()
            .flat_map(|body| body.statements())
        {
            if let ast::Stmt::Assign(assign) = &stmt {
                if assign.targets().into_iter().any(|target|matches!(target,ast::Expr::Name(name)if ordinary.iter().any(|field|field.name==name.name_text()))){return Err("Move unannotated class defaults into the constructor before generating slots".into());}
            }
            let ast::Stmt::AnnAssign(assign) = stmt else {
                continue;
            };
            let (Some(ast::Expr::Name(name)), Some(annotation), Some(value)) =
                (assign.target(), assign.annotation(), assign.value())
            else {
                continue;
            };
            if ordinary.iter().any(|field| field.name == name.name_text()) {
                if !request.constructor {
                    return Err(
                        "Select Constructor as well to move class defaults into slotted instances"
                            .into(),
                    );
                }
                changes.push((
                    TextRange::new(
                        annotation.syntax().text_range().end(),
                        value.syntax().text_range().end(),
                    ),
                    String::new(),
                ));
            }
        }
        let slots = ordinary
            .iter()
            .map(|field| format!("{:?}", storage_name(request, &field.name)))
            .collect::<Vec<_>>();
        let tuple = if slots.is_empty() {
            "()".into()
        } else {
            format!("({},)", slots.join(", "))
        };
        add("__slots__", format!("{ind}__slots__ = {tuple}\n"), false)?;
    }
    if request.post_init {
        if !target.dataclass {
            return Err("__post_init__ requires a dataclass".into());
        }
        let args = target
            .fields
            .iter()
            .filter(|field| field.init_only)
            .map(|field| {
                format!(
                    ", {}{}",
                    field.name,
                    field
                        .annotation
                        .as_ref()
                        .map(|ty| format!(": {ty}"))
                        .unwrap_or_default()
                )
            })
            .collect::<String>();
        add(
            "__post_init__",
            format!("{ind}def __post_init__(self{args}) -> None:\n{ind}    pass\n"),
            true,
        )?;
    }
    for inherited in &request.inherited {
        add(&inherited.name, render_inherited(target, inherited)?, true)?;
    }
    let imports: BTreeSet<_> = request
        .inherited
        .iter()
        .filter_map(|method| method.import.clone())
        .collect();
    if !imports.is_empty() {
        let header = imports
            .iter()
            .map(|(module, alias)| format!("import {module} as {alias}\n"))
            .collect::<String>();
        let at = target.class.syntax().text_range().start();
        if !indent_at(&target.source, usize::from(at)).is_empty() {
            return Err("Generate imported overrides in a top-level class".into());
        }
        changes.push((TextRange::empty(at), header));
    }
    if members.is_empty() {
        return Err("Select a method or member to generate".into());
    }
    let first_body = target
        .class
        .body()
        .and_then(|body| body.statements().next());
    if first_body.as_ref().is_some_and(|stmt| {
        target.source[..usize::from(stmt.syntax().text_range().start())].rfind('\n')
            == target.source[..usize::from(target.class.syntax().text_range().start())].rfind('\n')
    }) {
        let stmt = first_body.unwrap();
        let start = usize::from(stmt.syntax().text_range().start());
        let end = target.source[start..]
            .find('\n')
            .map_or(target.source.len(), |n| start + n);
        let old = target.source[start..end].trim();
        let mut snippet = String::from("\n");
        if old != "pass" {
            snippet.push_str(ind);
            snippet.push_str(old);
            snippet.push('\n');
        }
        for (_, member) in &members {
            if snippet != "\n" {
                snippet.push('\n');
            }
            snippet.push_str(member);
        }
        if end < target.source.len() {
            snippet.pop();
        }
        changes.push((
            TextRange::new(TextSize::from(start as u32), TextSize::from(end as u32)),
            snippet,
        ));
    } else {
        changes.extend(crate::refactor2::insert_members(
            &target.source,
            &target.class,
            &members,
        ));
    }
    let output = rustpy_syntax::printer::apply_text_edits(&target.source, &changes)
        .map_err(|error| error.to_string())?;
    if !rustpy_syntax::parse(&output).errors().is_empty() {
        return Err(
            "Generation would produce invalid Python; check field defaults and annotations".into(),
        );
    }
    Ok(changes)
}

fn instance_receiver(attr: &ast::AttributeExpr, class: &SyntaxNode) -> bool {
    let Some(method) = attr.syntax().ancestors().find_map(ast::FuncDef::cast) else {
        return false;
    };
    if method
        .syntax()
        .ancestors()
        .skip(1)
        .take_while(|node| node != class)
        .any(|node| node.kind() == SyntaxKind::FUNC_DEF)
    {
        return false;
    }
    if method
        .syntax()
        .ancestors()
        .find(|node| node.kind() == SyntaxKind::CLASS_DEF)
        .as_ref()
        != Some(class)
    {
        return false;
    }
    !method.decorators().any(|decorator| {
        decorator
            .name_text()
            .is_some_and(|name| name == "staticmethod" || name == "classmethod")
    }) && method.params().is_some_and(|params| {
        params
            .plain_params()
            .next()
            .is_some_and(|param| param.name_text() == "self")
    })
}

fn storage_name(request: &Request, field: &str) -> String {
    if request
        .property
        .as_ref()
        .is_some_and(|property| property.getter && property.field == field)
    {
        format!("_{}", field.trim_start_matches('_'))
    } else {
        field.into()
    }
}

/// Resolve decorator aliases through the same semantic model used by the
/// existing abstract-method generator, while retaining the exact CST source.
pub fn resolve_class_facts(db: &Analysis, file: &Arc<FileInfo>, target: &mut ClassTarget) {
    if file.text.as_ref() != target.source() {
        return;
    }
    let class = ClassRef {
        file: file.id,
        range: target.range,
        name: target.name.clone().into(),
    };
    if let Some(info) = Inferer::new(db).class_info(&class) {
        target.dataclass = info.is_dataclass;
    }
    target.dataclass |= target.class.decorators().any(|decorator| {
        let Some(expression) = decorator.expr() else {
            return false;
        };
        let callee = match expression {
            ast::Expr::Call(call) => call.callee(),
            expression => Some(expression),
        };
        callee
            .is_some_and(|expression| symbol_is(db, file, &expression, "dataclasses", "dataclass"))
    });
    for stmt in target
        .class
        .body()
        .into_iter()
        .flat_map(|body| body.statements())
    {
        let ast::Stmt::AnnAssign(assign) = stmt else {
            continue;
        };
        let (Some(ast::Expr::Name(name)), Some(annotation)) =
            (assign.target(), assign.annotation())
        else {
            continue;
        };
        if let ast::Expr::Subscript(subscript) = &annotation {
            if subscript.object().is_some_and(|expr| {
                symbol_is(db, file, &expr, "typing", "ClassVar")
                    || symbol_is(db, file, &expr, "typing_extensions", "ClassVar")
            }) {
                target.fields.retain(|field| field.name != name.name_text());
                continue;
            }
        }
        let Some(field) = target
            .fields
            .iter_mut()
            .find(|field| field.name == name.name_text())
        else {
            continue;
        };
        if let ast::Expr::Subscript(subscript) = annotation {
            if subscript
                .object()
                .is_some_and(|expr| symbol_is(db, file, &expr, "dataclasses", "InitVar"))
            {
                field.init_only = true;
                field.annotation = subscript.index().map(|node| node.text().to_string());
            }
        }
        if let Some(ast::Expr::Call(call)) = assign.value() {
            if call
                .callee()
                .is_some_and(|callee| symbol_is(db, file, &callee, "dataclasses", "field"))
            {
                field.default = None;
                field.factory = None;
                field.init = true;
                if let Some(args) = call.arg_list() {
                    for arg in args.keywords() {
                        let value = arg.value().map(|value| value.syntax().text().to_string());
                        match arg.name_text().as_str() {
                            "default" => field.default = value,
                            "default_factory" => field.factory = value,
                            "init" => field.init = value.as_deref() != Some("False"),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

fn symbol_is(
    db: &Analysis,
    file: &Arc<FileInfo>,
    expression: &ast::Expr,
    module: &str,
    name: &str,
) -> bool {
    let inf = Inferer::new(db);
    let scope = file
        .scopes
        .scope_at(expression.syntax().text_range().start());
    let definition = match expression {
        ast::Expr::Name(expr) => inf.resolve_name(file, scope, &expr.name_text()),
        ast::Expr::Attribute(attr) => {
            let (Some(ast::Expr::Name(receiver)), Some(attribute)) = (attr.object(), attr.attr())
            else {
                return false;
            };
            inf.resolve_name(file, scope, &receiver.name_text())
                .and_then(|definition| inf.import_target_module(&definition))
                .and_then(|file| inf.resolve_in_module(&file, attribute.text(), 0))
        }
        _ => None,
    };
    let Some(definition) = definition else {
        return false;
    };
    if let rustpy_semantic::scope::BindingKind::Import(info) = &definition.binding().kind {
        if info.level == 0 && info.module.as_str() == module && info.name.as_deref() == Some(name) {
            return true;
        }
    }
    let definition = inf.follow_imports(definition);
    definition.file.module_name.as_deref() == Some(module)
        && definition.binding().name.as_str() == name
}

/// Uses the resolver's actual C3 MRO; the first declaration (including an
/// attribute which shadows a method) wins. Captured sources are revalidated by
/// the caller before applying a plan.
pub fn inherited_methods(
    db: &Analysis,
    file: &Arc<FileInfo>,
    target: &ClassTarget,
) -> Vec<InheritedMethod> {
    use rustpy_semantic::classes::MemberKind;
    let inf = Inferer::new(db);
    let class = ClassRef {
        file: file.id,
        range: target.range,
        name: target.name.clone().into(),
    };
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for (index, base) in inf.mro(&class).iter().take(32).enumerate() {
        let Some(info) = inf.class_info(base) else {
            continue;
        };
        let Some(base_file) = db.file(base.file) else {
            continue;
        };
        if base_file.text.len() > 1024 * 1024 {
            continue;
        }
        for (name, member) in &info.members {
            if !seen.insert(name.to_string()) || index == 0 || result.len() >= 256 {
                continue;
            }
            if name.starts_with("__") && !name.ends_with("__") {
                continue;
            }
            if !matches!(
                member.kind,
                MemberKind::Method
                    | MemberKind::StaticMethod
                    | MemberKind::ClassMethod
                    | MemberKind::Property
            ) {
                continue;
            }
            let Some(definition) = token_at(&base_file.parse.syntax_node(), member.range.start())
                .and_then(|token| token.parent_ancestors().find_map(ast::FuncDef::cast))
                .filter(|def| def.name_text() == name.as_str())
            else {
                continue;
            };
            let abstract_ = definition.decorators().any(|decorator| {
                decorator
                    .name_text()
                    .is_some_and(|name| name.ends_with("abstractmethod"))
            });
            let signature = format!(
                "{}{}",
                name,
                definition
                    .params()
                    .map(|params| params.syntax().text().to_string())
                    .unwrap_or_else(|| "(self)".into())
            );
            let raw_params = definition
                .params()
                .map(|params| params.syntax().text().to_string())
                .unwrap_or_else(|| "(self)".into());
            let raw_ret = definition
                .return_annotation()
                .map(|ret| format!(" -> {}", ret.syntax().text()))
                .unwrap_or_default();
            let mut header = format!("{raw_params}{raw_ret}");
            let mut import = None;
            if base_file.id != file.id {
                let Some(module) = base_file.module_name.as_ref() else {
                    continue;
                };
                let mut alias = format!(
                    "_generated_{}",
                    module
                        .bytes()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                );
                while target.source.contains(&alias) {
                    alias.push('_');
                }
                let params_start = definition
                    .params()
                    .map(|params| usize::from(params.syntax().text_range().start()))
                    .unwrap_or(0);
                let mut replacements = Vec::new();
                let expressions = definition
                    .params()
                    .into_iter()
                    .flat_map(|params| params.params())
                    .flat_map(|param| [param.annotation(), param.default()])
                    .flatten()
                    .chain(definition.return_annotation());
                for expression in expressions {
                    for name in expression
                        .syntax()
                        .descendants()
                        .filter_map(ast::NameExpr::cast)
                    {
                        let text = name.name_text();
                        let scope = base_file
                            .scopes
                            .scope_at(name.syntax().text_range().start());
                        let Some(resolved) = inf.resolve_name(&base_file, scope, &text) else {
                            continue;
                        };
                        if resolved.file.path.starts_with("<typeshed>") {
                            continue;
                        }
                        // Header coordinates have a gap between ')' and the return
                        // expression, so compute within each captured component.
                        let range = name.syntax().text_range();
                        let start = usize::from(range.start());
                        let end = usize::from(range.end());
                        let mapped = if definition.params().is_some_and(|params| {
                            params.syntax().text_range().contains_range(range)
                        }) {
                            start - params_start
                        } else {
                            let Some(ret) = definition.return_annotation() else {
                                continue;
                            };
                            raw_params.len() + 4 + start
                                - usize::from(ret.syntax().text_range().start())
                        };
                        replacements.push((
                            mapped,
                            mapped + end - start,
                            format!("{alias}.{text}"),
                        ));
                    }
                }
                replacements.sort_by_key(|(start, _, _)| *start);
                replacements.dedup_by_key(|(start, _, _)| *start);
                for (start, end, replacement) in replacements.into_iter().rev() {
                    header.replace_range(start..end, &replacement);
                    import = Some((module.clone(), alias.clone()));
                }
            }
            result.push(InheritedMethod {
                name: name.to_string(),
                abstract_,
                signature,
                owner: base.name.to_string(),
                source_path: base_file.path.clone(),
                source: base_file.text.clone(),
                definition,
                kind: member.kind,
                header,
                import,
            });
        }
    }
    result
}

fn render_inherited(target: &ClassTarget, method: &InheritedMethod) -> Result<String, String> {
    use rustpy_semantic::classes::MemberKind;
    let ind = &target.indent;
    let def = &method.definition;
    let params = def
        .params()
        .ok_or("Inherited method has no parameter list")?;
    let mut args = Vec::new();
    let mut keyword = false;
    let mut receiver = method.kind != MemberKind::StaticMethod;
    for param in params.params() {
        match param {
            ast::AnyParam::Plain(param) => {
                let name = param.name_text();
                if receiver {
                    receiver = false;
                    continue;
                }
                args.push(if keyword {
                    format!("{name}={name}")
                } else {
                    name
                });
            }
            ast::AnyParam::Star(param) => {
                let name = param
                    .name()
                    .ok_or("Inherited variadic parameter has no name")?;
                args.push(format!("*{}", name.text()));
                keyword = true;
            }
            ast::AnyParam::StarStar(param) => {
                let name = param
                    .name()
                    .ok_or("Inherited keyword parameter has no name")?;
                args.push(format!("**{}", name.text()));
            }
            ast::AnyParam::StarSep(_) => keyword = true,
            ast::AnyParam::Slash(_) => {}
        }
    }
    let decorator = match method.kind {
        MemberKind::StaticMethod => format!("{ind}@staticmethod\n"),
        MemberKind::ClassMethod => format!("{ind}@classmethod\n"),
        MemberKind::Property => format!("{ind}@property\n"),
        _ => String::new(),
    };

    let asyn = if def.is_async() { "async " } else { "" };
    let owner = if method.kind == MemberKind::StaticMethod {
        format!("super({}, {})", target.name, target.name)
    } else {
        "super()".into()
    };
    let call = if method.kind == MemberKind::Property {
        format!("{owner}.{}", method.name)
    } else {
        format!("{owner}.{}({})", method.name, args.join(", "))
    };
    let body = if method.abstract_ {
        "raise NotImplementedError".into()
    } else {
        if def.is_async()
            && def.syntax().descendants().any(|node| {
                node.kind() == SyntaxKind::YIELD_EXPR
                    && node
                        .ancestors()
                        .find(|node| node.kind() == SyntaxKind::FUNC_DEF)
                        .is_some_and(|owner| owner == *def.syntax())
            })
        {
            format!("async for item in {call}:\n{ind}        yield item")
        } else {
            format!(
                "return {}{call}",
                if def.is_async() { "await " } else { "" }
            )
        }
    };
    Ok(format!(
        "{decorator}{ind}{asyn}def {}{}:\n{ind}    {body}\n",
        method.name, method.header
    ))
}

/// Appends a main guard without replacing an existing one.
pub fn main_guard(source: &str) -> Result<Vec<Edit>, String> {
    let root = rustpy_syntax::parse(source).syntax_node();
    if root
        .descendants()
        .filter_map(ast::IfStmt::cast)
        .any(|stmt| {
            stmt.condition().is_some_and(|condition| {
                let text = condition.syntax().text().to_string();
                text.contains("__name__") && text.contains("__main__")
            })
        })
    {
        return Err("A main guard already exists".into());
    }
    let prefix = if source.is_empty() || source.ends_with("\n\n") {
        ""
    } else if source.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let body = if root
        .children()
        .filter_map(ast::FuncDef::cast)
        .any(|function| function.name_text() == "main")
    {
        "main()"
    } else {
        "pass"
    };
    let at = TextSize::from(source.len() as u32);
    Ok(vec![(
        TextRange::empty(at),
        format!("{prefix}if __name__ == \"__main__\":\n    {body}\n"),
    )])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestSubject {
    pub key: String,
    pub class: Option<String>,
    pub method: Option<String>,
    pub function: Option<String>,
}

/// Enumerates actual callable declarations, without guessing classes from
/// uppercase names or treating nested implementation helpers as module APIs.
pub fn test_subjects(source: &str) -> Vec<TestSubject> {
    let root = rustpy_syntax::parse(source).syntax_node();
    let mut out = Vec::new();
    for node in root.children() {
        if let Some(function) = ast::FuncDef::cast(node.clone()) {
            let name = function.name_text();
            out.push(TestSubject {
                key: name.clone(),
                class: None,
                method: None,
                function: Some(name),
            });
        } else if let Some(class) = ast::ClassDef::cast(node) {
            let name = class.name_text();
            if let Some(body) = class.body() {
                for stmt in body.statements() {
                    if let ast::Stmt::FuncDef(method) = stmt {
                        let method = method.name_text();
                        if method.starts_with('_') && method != "__init__" {
                            continue;
                        }
                        out.push(TestSubject {
                            key: format!("{name}.{method}"),
                            class: Some(name.clone()),
                            method: Some(method),
                            function: None,
                        });
                    }
                }
            }
        }
        if out.len() >= 256 {
            break;
        }
    }
    out
}

/// Failing test stubs follow the existing Create Test convention. Selected
/// methods are explicit declaration identities and are grouped once per class.
pub fn test_source(module: &str, subjects: &[TestSubject], pytest: bool) -> Result<String, String> {
    if subjects.is_empty() {
        return Err("Select a function or method to test".into());
    }
    let imports: BTreeSet<_> = subjects
        .iter()
        .filter_map(|subject| subject.class.as_ref().or(subject.function.as_ref()))
        .cloned()
        .collect();
    let mut source = if pytest {
        String::new()
    } else {
        String::from("import unittest\n\n")
    };
    if !module.is_empty() {
        source.push_str(&format!(
            "from {module} import {}\n\n",
            imports.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    let mut classes = std::collections::BTreeMap::<String, Vec<String>>::new();
    let mut functions = Vec::new();
    let mut seen = HashSet::new();
    for subject in subjects {
        if !seen.insert(subject.key.clone()) {
            return Err("A test subject was selected twice".into());
        }
        if let (Some(class), Some(method)) = (&subject.class, &subject.method) {
            classes
                .entry(class.clone())
                .or_default()
                .push(method.clone());
        } else if let Some(function) = &subject.function {
            functions.push(function.clone());
        } else {
            return Err("Invalid test subject".into());
        }
    }
    for (class, methods) in classes {
        source.push_str(&format!(
            "class Test{class}{}:\n",
            if pytest { "" } else { "(unittest.TestCase)" }
        ));
        for method in methods {
            let method = method.trim_matches('_');
            source.push_str(&format!(
                "    def test_{method}(self):\n        {}\n\n",
                if pytest {
                    "assert False"
                } else {
                    "self.assertEqual(True, False)"
                }
            ));
        }
    }
    if !functions.is_empty() && !pytest {
        source.push_str("class TestFunctions(unittest.TestCase):\n");
    }
    for function in functions {
        let ind = if pytest { "" } else { "    " };
        let receiver = if pytest { "" } else { "self" };
        source.push_str(&format!(
            "{ind}def test_{function}({receiver}):\n{ind}    {}\n\n",
            if pytest {
                "assert False"
            } else {
                "self.assertEqual(True, False)"
            }
        ));
    }
    if !pytest {
        source.push_str("if __name__ == \"__main__\":\n    unittest.main()\n");
    }
    if !rustpy_syntax::parse(&source).errors().is_empty() {
        return Err("Selected subjects cannot form a valid test module".into());
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(source: &str) -> ClassTarget {
        let root = rustpy_syntax::parse(source).syntax_node();
        ClassTarget::capture(
            source,
            &root,
            TextSize::from(
                source
                    .rfind("pass")
                    .unwrap_or_else(|| source.find("class ").unwrap() + 6) as u32,
            ),
        )
        .unwrap()
    }
    fn output(target: &ClassTarget, request: Request) -> String {
        let edits = generate(target, &request).unwrap();
        rustpy_syntax::printer::apply_text_edits(target.source(), &edits).unwrap()
    }
    #[test]
    fn property_preserves_local_class_receiver_and_declaration_ownership() {
        let source="def make():\n    class Record:\n        value: int = 1\n        def update(self):\n            self.value = 2\n            return self.value\n    return Record\n";
        let target = target_capture(source);
        let text = output(
            &target,
            Request {
                property: Some(Property {
                    field: "value".into(),
                    getter: true,
                    setter: true,
                    deleter: false,
                }),
                ..Default::default()
            },
        );
        assert!(text.contains("        _value: int = 1"));
        assert!(text.contains("            self._value = 2"));
        assert!(text.contains("return self.value"));
    }
    #[test]
    fn slots_refuse_plain_class_default_collisions_without_changing_source() {
        let source =
            "class Record:\n    value = 1\n    def set(self, value):\n        self.value = value\n";
        let target = target_capture(source);
        assert!(generate(
            &target,
            &Request {
                fields: vec!["value".into()],
                slots: true,
                constructor: true,
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("unannotated class defaults"));
        assert_eq!(target.source(), source);
    }
    #[test]
    fn property_keeps_method_locals_and_unrelated_static_receivers() {
        let source="class Record:\n    value: int = 1\n    def update(self):\n        value = 2\n        self.value = value\n        return value\n    @staticmethod\n    def other(self):\n        self.value = 3\n";
        let text = output(
            &target_capture(source),
            Request {
                property: Some(Property {
                    field: "value".into(),
                    getter: true,
                    setter: true,
                    deleter: false,
                }),
                ..Default::default()
            },
        );
        assert!(
            text.contains("        value = 2\n        self._value = value\n        return value")
        );
        assert!(text.contains("def other(self):\n        self.value = 3"));
    }
    #[test]
    fn slots_move_defaults_into_selected_constructor_and_refuse_decorator_duplicates() {
        let target = target("class Record:\n    value: int = 1\n    pass\n");
        let request = Request {
            fields: vec!["value".into()],
            slots: true,
            ..Default::default()
        };
        assert!(generate(&target, &request)
            .unwrap_err()
            .contains("Select Constructor"));
        let text = output(
            &target,
            Request {
                constructor: true,
                ..request
            },
        );
        assert!(text.contains("    value: int\n"));
        assert!(!text.contains("    value: int = 1\n"));
        assert!(text.contains("def __init__(self, value: int = 1)"));
        assert!(text.contains("self.value = value"));
        let decorated=target_capture("from dataclasses import dataclass\n@dataclass(slots=True)\nclass Record:\n    value: int\n");
        assert!(generate(
            &decorated,
            &Request {
                fields: vec!["value".into()],
                slots: true,
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("decorator already"));
    }
    #[test]
    fn actual_semantic_dataclass_field_initvar_classvar_aliases_keep_runtime_values() {
        use std::path::Path;
        let source="from dataclasses import dataclass, field as f, InitVar as IV\nfrom typing import ClassVar as CV\n@dataclass\nclass Record:\n    values: list = f(default_factory=list)\n    seed: IV[str]\n    shared: CV[int] = 7\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/record.py"), source);
        let file = db.file(id).unwrap();
        let mut target = target_capture(source);
        resolve_class_facts(&db, &file, &mut target);
        assert!(!target.fields.iter().any(|field| field.name == "shared"));
        assert_eq!(
            target
                .fields
                .iter()
                .find(|field| field.name == "seed")
                .unwrap()
                .annotation
                .as_deref(),
            Some("str")
        );
        let text = output(
            &target,
            Request {
                fields: vec!["values".into(), "seed".into()],
                constructor: true,
                post_init: true,
                ..Default::default()
            },
        );
        assert!(!text.contains("def __init__(self, values: list = f("));
        assert!(text.contains("list() if values is self._generated_missing_values else values"));
        assert!(text.contains("self.__post_init__(seed)"));
    }
    #[test]
    fn inherited_same_named_bases_have_distinct_module_signature_bindings() {
        use std::path::Path;
        let db = Analysis::for_project(Path::new("/proj"));
        db.set_file_text(Path::new("/proj/a.py"),"class Token:\n    pass\nclass Base:\n    def first(self, token: Token) -> Token:\n        return token\n");
        db.set_file_text(Path::new("/proj/b.py"),"class Token:\n    pass\nclass Base:\n    def second(self, token: Token) -> Token:\n        return token\n");
        let source =
            "from a import Base as A\nfrom b import Base as B\nclass Child(A, B):\n    pass\n";
        let id = db.set_file_text(Path::new("/proj/child.py"), source);
        let file = db.file(id).unwrap();
        let target = target_capture(source);
        let inherited = inherited_methods(&db, &file, &target);
        let text = output(
            &target,
            Request {
                inherited,
                ..Default::default()
            },
        );
        assert!(text.contains("import a as _generated_61"));
        assert!(text.contains("import b as _generated_62"));
        assert!(text.contains("def first(self, token: _generated_61.Token) -> _generated_61.Token"));
        assert!(
            text.contains("def second(self, token: _generated_62.Token) -> _generated_62.Token")
        );
    }
    #[test]
    fn test_picker_uses_actual_declarations_and_both_frameworks() {
        let source="class lower:\n    def chosen(self):\n        pass\n    def omitted(self):\n        pass\n\ndef Uppercase():\n    pass\n";
        let selected = test_subjects(source)
            .into_iter()
            .filter(|subject| subject.key == "lower.chosen" || subject.key == "Uppercase")
            .collect::<Vec<_>>();
        for pytest in [true, false] {
            let text = test_source("subject", &selected, pytest).unwrap();
            assert!(text.contains("from subject import Uppercase, lower"));
            assert!(text.contains("class Testlower"));
            assert!(text.contains("test_chosen"));
            assert!(!text.contains("test_omitted"));
            assert!(text.contains("test_Uppercase"));
            assert_eq!(text.contains("unittest.TestCase"), !pytest);
        }
    }
    #[test]
    fn main_guard_is_duplicate_safe_and_does_not_call_a_missing_function() {
        let source = "π = '🙂'";
        let edits = main_guard(source).unwrap();
        let text = rustpy_syntax::printer::apply_text_edits(source, &edits).unwrap();
        assert!(text.starts_with(source));
        assert!(text.ends_with("    pass\n"));
        assert!(main_guard(&text).is_err());
        let source = "def main():\n    pass\n";
        let text =
            rustpy_syntax::printer::apply_text_edits(source, &main_guard(source).unwrap()).unwrap();
        assert!(text.ends_with("    main()\n"));
    }
    #[test]
    fn dataclass_alias_and_constructor_post_init_keep_initvar_contract() {
        use std::path::Path;
        let source="from dataclasses import dataclass as dc, InitVar\n@dc\nclass Record:\n    seed: InitVar[str]\n    value: int\n    pass\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/record.py"), source);
        let file = db.file(id).unwrap();
        let mut target = target_capture(source);
        resolve_class_facts(&db, &file, &mut target);
        assert!(target.dataclass);
        let text = output(
            &target,
            Request {
                fields: vec!["seed".into(), "value".into()],
                constructor: true,
                post_init: true,
                ..Default::default()
            },
        );
        assert!(text.contains("self.__post_init__(seed)"));
        assert!(text.contains("def __post_init__(self, seed: str)"));
        assert!(!text.contains("self.seed = seed"));
        assert!(generate(
            &target,
            &Request {
                fields: vec!["value".into()],
                constructor: true,
                post_init: true,
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("Select all InitVar"));
    }
    #[test]
    fn dataclass_factory_and_init_false_are_not_field_objects_or_shared_defaults() {
        let target=target("from dataclasses import dataclass, field\n@dataclass\nclass Record:\n    values: list = field(default_factory=list)\n    cache: dict = field(default_factory=dict, init=False)\n    pass\n");
        let text = output(
            &target,
            Request {
                fields: vec!["values".into(), "cache".into()],
                constructor: true,
                ..Default::default()
            },
        );
        assert!(text.contains("values: list = _generated_missing_values"));
        assert!(!text.contains("def __init__(self, cache"));
        assert!(text.contains("self.cache = dict()"));
        assert!(text.contains("list() if values is self._generated_missing_values else values"));
    }
    #[test]
    fn constructor_orders_required_before_defaults_and_keeps_unicode_types() {
        let target = target("class Record:\n    optional: str = 'x'\n    数: int\n    pass\n");
        let out = output(
            &target,
            Request {
                fields: vec!["optional".into(), "数".into()],
                constructor: true,
                ..Default::default()
            },
        );
        assert!(out.contains("def __init__(self, 数: int, optional: str = 'x') -> None:"));
        assert!(out.contains("self.数 = 数"));
        assert!(out.contains("optional: str = 'x'"));
    }
    #[test]
    fn selected_fields_drive_exact_dunders_and_duplicate_refusal() {
        let target = target("class Record:\n    one: int\n    two: str\n    pass\n");
        let out = output(
            &target,
            Request {
                fields: vec!["two".into()],
                dunders: vec!["__repr__".into(), "__eq__".into(), "__hash__".into()],
                ..Default::default()
            },
        );
        assert!(out.contains("two={self.two!r}"));
        assert!(!out.contains("one={self.one!r}"));
        assert!(out.contains("return hash((self.two,))"));
        assert!(out.contains("return NotImplemented"));
        let current = target_capture(&out);
        assert!(generate(
            &current,
            &Request {
                dunders: vec!["__repr__".into()],
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("already exists"));
    }
    fn target_capture(source: &str) -> ClassTarget {
        let root = rustpy_syntax::parse(source).syntax_node();
        let class = root.descendants().find_map(ast::ClassDef::cast).unwrap();
        let offset = class.name().unwrap().text_range().start();
        ClassTarget::capture(source, &root, offset).unwrap()
    }
    #[test]
    fn property_redirects_only_actual_writes_and_keeps_reads_comments_and_other_class() {
        let source="class Record:\n    value: int = 1\n    def update(self, value):\n        # self.value is documentation\n        self.value = value\n        return self.value\n\nclass Other:\n    value: int = 2\n";
        let target = target_capture(source);
        let out = output(
            &target,
            Request {
                property: Some(Property {
                    field: "value".into(),
                    getter: true,
                    setter: true,
                    deleter: true,
                }),
                ..Default::default()
            },
        );
        assert!(out.contains("_value: int = 1"));
        assert!(out.contains("self._value = value"));
        assert!(out.contains("return self.value"));
        assert!(out.contains("# self.value is documentation"));
        assert!(out.contains("@value.deleter"));
        assert!(out.contains("class Other:\n    value: int = 2"));
    }
    #[test]
    fn inline_pass_and_dataclass_initvars_slots_are_valid() {
        let inline = output(
            &target("class Tiny: pass"),
            Request {
                dunders: vec!["__str__".into()],
                ..Default::default()
            },
        );
        assert!(inline.starts_with("class Tiny: \n    def __str__"));
        let target=target("from dataclasses import dataclass, InitVar\n@dataclass\nclass Record:\n    value: int\n    seed: InitVar[str]\n    pass\n");
        let out = output(
            &target,
            Request {
                fields: vec!["value".into(), "seed".into()],
                slots: true,
                post_init: true,
                ..Default::default()
            },
        );
        assert!(out.contains("__slots__ = (\"value\",)"));
        assert!(out.contains("def __post_init__(self, seed: str) -> None:"));
    }
    #[test]
    fn inherited_signature_preserves_async_keyword_only_and_mro_shadowing() {
        use std::path::Path;
        let db = Analysis::for_project(Path::new("/proj"));
        db.set_file_text(Path::new("/proj/base.py"),"from abc import ABC, abstractmethod\nclass Base(ABC):\n    async def run(self, value: int = 1, /, *args, flag: bool = False, **kwargs) -> str:\n        return 'x'\n    @abstractmethod\n    def required(self, *, key: str) -> int:\n        pass\n");
        let source = "from base import Base\nclass Child(Base):\n    pass\n";
        let id = db.set_file_text(Path::new("/proj/child.py"), source);
        let file = db.file(id).unwrap();
        let target = target_capture(source);
        let inherited = inherited_methods(&db, &file, &target);
        assert!(inherited.iter().any(|method| method.name == "run"));
        assert!(inherited
            .iter()
            .any(|method| method.name == "required" && method.abstract_));
        let out = output(
            &target,
            Request {
                inherited,
                ..Default::default()
            },
        );
        assert!(out.contains("return await super().run(value, *args, flag=flag, **kwargs)"));
        assert!(out.contains("def required(self, *, key: str) -> int:"));
        assert!(out.contains("raise NotImplementedError"));
    }
}
