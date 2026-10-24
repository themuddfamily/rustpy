//! Context-specific Python lookup providers. Suggestions never execute source.
use crate::{
    db::{Analysis, FileInfo},
    features::{CompletionItem, CompletionKind},
    infer::Inferer,
    scope::{BindingKind, ScopeKind},
    types::{LiteralValue, Type},
};
use rustpy_core::TextSize;
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind,
};
use std::sync::Arc;

pub struct BasicCompletion {
    pub items: Vec<CompletionItem>,
    pub imports: Vec<Option<(String, String)>>,
    pub prefix_start: TextSize,
    pub replace_segment: bool,
    pub import_classes: bool,
}
fn item(name: &str, kind: CompletionKind, detail: &str) -> CompletionItem {
    CompletionItem {
        label: name.into(),
        kind,
        detail: detail.into(),
        insert: name.into(),
        caret_offset: None,
        priority: 0,
    }
}
fn batch(
    mut items: Vec<CompletionItem>,
    imports: Vec<Option<(String, String)>>,
    start: usize,
    prefix: &str,
) -> BasicCompletion {
    let mut pairs: Vec<_> = items
        .drain(..)
        .zip(imports)
        .filter(|(item, _)| crate::features::matches_prefix(&item.label, &prefix.to_lowercase()))
        .collect();
    pairs.sort_by(|(a, _), (b, _)| a.priority.cmp(&b.priority).then(a.label.cmp(&b.label)));
    pairs.dedup_by(|(a, _), (b, _)| a.label == b.label && a.kind == b.kind);
    let (items, imports) = pairs.into_iter().unzip();
    BasicCompletion {
        items,
        imports,
        prefix_start: TextSize::new(start as u32),
        replace_segment: false,
        import_classes: false,
    }
}
fn plain(items: Vec<CompletionItem>, start: usize, prefix: &str) -> BasicCompletion {
    let imports = vec![None; items.len()];
    batch(items, imports, start, prefix)
}
fn prefix(text: &str, end: usize) -> (usize, &str) {
    let before = &text[..end];
    let start = before
        .rfind(|c: char| !rustpy_core::is_ident_continue(c))
        .map_or(0, |at| {
            at + before[at..].chars().next().map_or(0, char::len_utf8)
        });
    (start, &before[start..])
}
/// Only name leaves of tuple/list deletion targets retire the binding.
/// Attribute and subscript deletion mutate an object without deleting its base.
fn deletes_annotation_name(target: ast::Expr, expected: &str) -> bool {
    let mut pending = vec![target];
    while let Some(target) = pending.pop() {
        match target.unparen() {
            ast::Expr::Name(name) if name.name_text() == expected => return true,
            ast::Expr::Tuple(tuple) => pending.extend(tuple.elements()),
            ast::Expr::List(list) => pending.extend(list.elements()),
            _ => {}
        }
    }
    false
}

/// Imported annotation forms are instances in typing's stub, rather than
/// ordinary classes. Keep their resolved canonical class identity; accepting
/// every imported value would leak functions/constants into annotation lookup.
fn imported_annotation_form(
    db: &Analysis,
    file: &FileInfo,
    binding: &crate::scope::Binding,
    offset: TextSize,
    ty: &Type,
) -> bool {
    if !matches!(binding.kind, BindingKind::Import(_)) {
        return false;
    }
    let Type::Instance(class, _) = ty else {
        return false;
    };
    if !matches!(class.name.as_str(), "_SpecialForm" | "_SpecialGenericAlias") {
        return false;
    }
    let canonical = ["typing", "typing_extensions"].into_iter().any(|name| {
        matches!(db.resolver.resolve(name), Some(crate::module::ModuleSource::Stub(module)) if module == name)
            && db.module(name).is_some_and(|module| module.id == class.file)
    });
    canonical
        && !file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::DelStmt::cast)
            .any(|deleted| {
                deleted.text_range().start() >= binding.range.end()
                    && deleted.text_range().start() < offset
                    && file.scopes.scope_at(deleted.text_range().start()) == binding.scope
                    && deleted
                        .targets()
                        .into_iter()
                        .any(|target| deletes_annotation_name(target, binding.name.as_str()))
            })
}

fn scope_types(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<CompletionItem> {
    let inf = Inferer::new(db);
    let mut scope = Some(file.scopes.scope_at(offset));
    let mut names = std::collections::HashSet::new();
    let mut out = Vec::new();
    while let Some(id) = scope {
        let current = file.scopes.scope(id);
        for (name, ids) in &current.bindings {
            if !names.insert(name.to_string()) {
                continue;
            }
            let Some(id) = ids.last() else {
                continue;
            };
            let binding = file.scopes.binding(*id);
            let ty = inf.type_of_binding(file, *id);
            if matches!(
                ty,
                Type::Class(..)
                    | Type::TypeVar(_)
                    | Type::DeclaredTypeVar(_)
                    | Type::ParamSpec(_)
                    | Type::TypeVarTuple(_)
            ) || binding.kind == BindingKind::TypeAlias
                || imported_annotation_form(db, file, binding, offset, &ty)
            {
                out.push(item(name, CompletionKind::Class, &ty.to_string()));
            }
        }
        scope = current.parent;
    }
    for name in [
        "int",
        "str",
        "bytes",
        "bool",
        "float",
        "complex",
        "list",
        "dict",
        "tuple",
        "set",
        "frozenset",
        "object",
        "type",
        "None",
    ] {
        if names.insert(name.into()) {
            out.push(item(name, CompletionKind::Class, "builtin type"));
        }
    }
    out
}
fn typing(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    start: usize,
    typed: &str,
) -> BasicCompletion {
    let mut items = scope_types(db, file, offset);
    let mut imports = vec![None; items.len()];
    let scope = file.scopes.scope_at(offset);
    for module in ["typing", "typing_extensions"] {
        let Some(source) = db.module(module) else {
            continue;
        };
        for name in [
            "Annotated",
            "Any",
            "Callable",
            "ClassVar",
            "Final",
            "Generic",
            "Iterable",
            "Iterator",
            "Literal",
            "Mapping",
            "MutableMapping",
            "NamedTuple",
            "Never",
            "NoReturn",
            "NotRequired",
            "Optional",
            "ParamSpec",
            "Protocol",
            "Required",
            "Self",
            "Sequence",
            "TypeAlias",
            "TypeGuard",
            "TypeIs",
            "TypedDict",
            "TypeVar",
            "TypeVarTuple",
            "Union",
            "Unpack",
        ] {
            if items.iter().any(|item| item.label == name)
                || file.scopes.resolve(scope, name).is_some()
                || source
                    .scopes
                    .bindings_in(source.scopes.module_scope(), name)
                    .is_empty()
            {
                continue;
            }
            items.push(item(name, CompletionKind::Class, &format!("from {module}")));
            imports.push(Some((module.into(), name.into())));
        }
    }
    let mut completion = batch(items, imports, start, typed);
    completion.import_classes = true;
    completion
}
fn literal_strings(ty: &Type, out: &mut Vec<String>, depth: usize) {
    if depth > 16 || out.len() > 256 {
        return;
    }
    match ty {
        Type::Literal(LiteralValue::Str(value)) => out.push(value.to_string()),
        Type::Union(values) => {
            for value in values.iter().take(257) {
                literal_strings(value, out, depth + 1)
            }
        }
        _ => {}
    }
}
fn dict_keys(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    object: &ast::Expr,
    offset: TextSize,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Type::Instance(class, args) = inf.type_of_expr(file, object) {
        if let Some(fields) = inf.typed_dict_fields(&class, &args) {
            out.extend(fields.into_iter().map(|(name, _, _, _)| name.to_string()));
        }
    }
    let ast::Expr::Name(name) = object.clone().unparen() else {
        return out;
    };
    let Some(ids) = file
        .scopes
        .resolve(file.scopes.scope_at(offset), &name.name_text())
    else {
        return out;
    };
    for id in ids.iter().rev().take(16) {
        let binding = file.scopes.binding(*id);
        if binding.range.start() > offset {
            continue;
        }
        let Some(range) = binding.value else {
            continue;
        };
        let Some(value) = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::Expr::cast)
            .find(|expr| expr.text_range() == range)
        else {
            continue;
        };
        let value = match value.unparen() {
            ast::Expr::Call(call)
                if call
                    .callee()
                    .and_then(|expr| crate::frameworks::origin(file, &expr))
                    .is_some_and(|origin| {
                        origin == "pandas.DataFrame" || origin == "pandas.core.frame.DataFrame"
                    }) =>
            {
                call.arg_list().and_then(|args| {
                    args.args().find_map(|arg| match arg {
                        ast::AnyArg::Positional(arg) => arg.expr(),
                        _ => None,
                    })
                })
            }
            value => Some(value),
        };
        if let Some(ast::Expr::Dict(dict)) = value {
            for entry in dict.items().take(256) {
                if let Some(key) = entry.key() {
                    if let Type::Literal(LiteralValue::Str(key)) = inf.type_of_expr(file, &key) {
                        out.push(key.to_string());
                    }
                }
            }
        }
        break;
    }
    out.sort();
    out.dedup();
    out
}
fn string_context(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Option<BasicCompletion> {
    let end = usize::from(offset);
    let root = file.parse.syntax_node();
    let token = root.token_at_offset(offset).left_biased()?;
    if token.kind() != SyntaxKind::STRING {
        return None;
    }
    let raw = &file.text[usize::from(token.text_range().start())..end];
    let quote = raw.find(['\'', '"'])?;
    let delimiter = raw.as_bytes()[quote] as char;
    if raw[..quote].contains(['b', 'B', 'f', 'F']) {
        return None;
    }
    let typed = &raw[quote + 1..];
    if typed.contains(delimiter) || typed.contains(['\n', '\r', '\\']) {
        return None;
    }
    let start = usize::from(token.text_range().start()) + quote + 1;
    let inf = Inferer::new(db);
    let mut names = Vec::new();
    let mut exclusive = false;
    if let Some(subscript) = token.parent_ancestors().find_map(ast::SubscriptExpr::cast) {
        if subscript
            .object()
            .is_some_and(|object| object.text_range().end() < token.text_range().start())
        {
            exclusive = true;
            if let Some(object) = subscript.object() {
                names.extend(dict_keys(&inf, file, &object, offset));
            }
        }
    }
    if let Some(assign) = token.parent_ancestors().find_map(ast::AssignStmt::cast) {
        if assign.targets().iter().any(|target|matches!(target.clone().unparen(),ast::Expr::Name(name) if name.name_text()=="__all__")){
        exclusive=true;for (name,ids) in &file.scopes.scope(file.scopes.module_scope()).bindings{if ids.iter().any(|id|file.scopes.binding(*id).is_definition()){names.push(name.to_string());}}
        let used:Vec<_>=assign.value().into_iter().flat_map(|value|value.syntax().descendants().filter_map(ast::StringExpr::cast).map(|string|string.value()).collect::<Vec<_>>()).collect();names.retain(|name|!used.contains(name)||name==typed);
    }
    }
    if let Some((expected, _)) = crate::completion2::expected_type(db, file, offset) {
        let before = names.len();
        literal_strings(&expected, &mut names, 0);
        exclusive |= names.len() > before;
    }
    if !exclusive {
        return None;
    }
    names.sort();
    names.dedup();
    let mut completion = plain(
        names
            .into_iter()
            .filter(|name| name.starts_with(typed))
            .take(256)
            .map(|name| item(&name, CompletionKind::Constant, "string value"))
            .collect(),
        start,
        typed,
    );
    completion.replace_segment = true;
    Some(completion)
}
pub(crate) fn fixture_expression(
    db: &Analysis,
    file: &Arc<FileInfo>,
    expression: ast::Expr,
) -> bool {
    let offset = expression.text_range().start();
    let expression = match expression.unparen() {
        ast::Expr::Call(call) => match call.callee() {
            Some(callee) => callee,
            None => return false,
        },
        other => other,
    };
    let (root, member) = match expression.unparen() {
        ast::Expr::Name(name) => (name.name_text(), None),
        ast::Expr::Attribute(attribute) => {
            let Some(ast::Expr::Name(root)) = attribute.object().map(ast::Expr::unparen) else {
                return false;
            };
            (root.name_text(), Some(attribute.attr_text()))
        }
        _ => return false,
    };
    let Some(ids) = file.scopes.resolve(file.scopes.scope_at(offset), &root) else {
        return false;
    };
    let Some(id) = ids
        .iter()
        .rev()
        .find(|id| file.scopes.binding(**id).range.start() <= offset)
    else {
        return false;
    };
    let BindingKind::Import(import) = &file.scopes.binding(*id).kind else {
        return false;
    };
    let valid = if let Some(member) = member {
        member == "fixture"
            && import.name.is_none()
            && matches!(import.module.as_str(), "pytest" | "_pytest.fixtures")
    } else {
        import.name.as_deref() == Some("fixture")
            && matches!(import.module.as_str(), "pytest" | "_pytest.fixtures")
    };
    valid
        && import.level == 0
        && db.module(&import.module).is_none_or(|module| {
            module.path.starts_with("<typeshed>")
                || !db
                    .resolver
                    .roots
                    .iter()
                    .any(|root| module.path.starts_with(root))
        })
}
pub(crate) fn fixture_decorator(
    db: &Analysis,
    file: &Arc<FileInfo>,
    function: &ast::FuncDef,
) -> bool {
    function
        .decorators()
        .take(16)
        .filter_map(|decorator| decorator.expr())
        .any(|expression| fixture_expression(db, file, expression))
}
fn pytest(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    start: usize,
    typed: &str,
) -> Option<BasicCompletion> {
    let function = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .find(|function| {
            function
                .params()
                .is_some_and(|params| params.text_range().contains_inclusive(offset))
        })?;
    if !function.name_text().starts_with("test_") && !fixture_decorator(db, file, &function) {
        return None;
    }
    let used: Vec<_> = function
        .params()?
        .params()
        .filter_map(|param| param.name())
        .filter(|name| !name.text_range().contains_inclusive(offset))
        .map(|name| name.text().to_owned())
        .collect();
    let mut names: Vec<String> = [
        "tmp_path",
        "tmp_path_factory",
        "monkeypatch",
        "capsys",
        "capsysbinary",
        "capfd",
        "capfdbinary",
        "caplog",
        "pytestconfig",
        "request",
        "recwarn",
        "cache",
        "doctest_namespace",
        "record_property",
        "record_testsuite_property",
        "record_xml_attribute",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    names.extend(
        crate::pytest_fixtures::available(db, file, Some(offset))
            .into_iter()
            .map(|fixture| fixture.name),
    );
    names.sort();
    names.dedup();
    Some(plain(
        names
            .into_iter()
            .filter(|name| !used.contains(name))
            .map(|name| item(&name, CompletionKind::Parameter, "pytest fixture"))
            .collect(),
        start,
        typed,
    ))
}
/// Exclusive context providers, including their exact replacement prefix.
pub fn complete(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Option<BasicCompletion> {
    if let Some(completion) = crate::cython::complete(db, file, offset) {
        return Some(completion);
    }
    if let Some(completion) = crate::frameworks::complete(db, file, offset) {
        return Some(completion);
    }
    let offset = crate::clamp_offset(file, offset);
    let end = usize::from(offset);
    let (start, typed) = prefix(&file.text, end);
    let before = &file.text[..start];
    let line = before.rsplit('\n').next().unwrap_or("").trim_start();
    if let Some(strings) = string_context(db, file, offset) {
        return Some(strings);
    }
    let comment = file
        .parse
        .syntax_node()
        .token_at_offset(offset)
        .left_biased()
        .filter(|token| token.kind() == SyntaxKind::COMMENT);
    if let Some(comment) = comment {
        let line = &file.text[usize::from(comment.text_range().start())..start];
        if line.contains("noqa") {
            let codes = [
                "E101", "E111", "E112", "E113", "E114", "E115", "E116", "E117", "E201", "E202",
                "E203", "E211", "E221", "E222", "E225", "E226", "E231", "E251", "E261", "E262",
                "E265", "E266", "E271", "E272", "E301", "E302", "E303", "E305", "E401", "E402",
                "E501", "E701", "E702", "E703", "E711", "E712", "E713", "E714", "E721", "E722",
                "E731", "E741", "F401", "F403", "F405", "F541", "F601", "F602", "F621", "F622",
                "F631", "F632", "F634", "F701", "F702", "F704", "F706", "F707", "F722", "F811",
                "F821", "F822", "F823", "F831", "F841", "F901", "W191", "W291", "W292", "W293",
                "W391", "W605",
            ];
            return Some(plain(
                codes
                    .into_iter()
                    .map(|code| item(code, CompletionKind::Constant, "inspection suppression"))
                    .collect(),
                start,
                typed,
            ));
        }
        if line.contains("type: ignore[") {
            let code_start = file.text[..end]
                .rfind(['[', ','])
                .map_or(start, |at| at + 1);
            let code_start = code_start + file.text[code_start..end].len()
                - file.text[code_start..end].trim_start().len();
            let codes = [
                "arg-type",
                "assignment",
                "attr-defined",
                "call-arg",
                "call-overload",
                "dict-item",
                "index",
                "list-item",
                "misc",
                "name-defined",
                "operator",
                "override",
                "return-value",
                "type-arg",
                "type-var",
                "union-attr",
                "valid-type",
                "var-annotated",
            ];
            let mut completion = plain(
                codes
                    .into_iter()
                    .map(|name| item(name, CompletionKind::Constant, "type-checker suppression"))
                    .collect(),
                code_start,
                &file.text[code_start..end],
            );
            completion.replace_segment = true;
            return Some(completion);
        }
        if line.contains("type:") {
            return Some(typing(db, file, offset, start, typed));
        }
        return None;
    }
    if line.starts_with("from __future__ import") {
        let names = [
            "absolute_import",
            "division",
            "generators",
            "nested_scopes",
            "print_function",
            "unicode_literals",
            "with_statement",
            "barry_as_FLUFL",
            "generator_stop",
            "annotations",
        ];
        return Some(plain(
            names
                .into_iter()
                .map(|name| item(name, CompletionKind::Keyword, "future feature"))
                .collect(),
            start,
            typed,
        ));
    }
    let root = file.parse.syntax_node();
    let annotation = root
        .token_at_offset(offset)
        .left_biased()
        .is_some_and(|token| {
            token.parent_ancestors().any(|node| {
                matches!(
                    node.kind(),
                    SyntaxKind::ANNOTATION | SyntaxKind::RETURN_ANNOTATION
                )
            })
        });
    if (annotation || line.trim_end().ends_with("->")) && !line.trim_end().ends_with('.') {
        return Some(typing(db, file, offset, start, typed));
    }
    if let Some(fixtures) = pytest(db, file, offset, start, typed) {
        return Some(fixtures);
    }
    let words: Vec<_> = line.split_whitespace().collect();
    if words.first() == Some(&"for") && words.len() > 1 && !words.contains(&"in") {
        return Some(plain(
            vec![item("in", CompletionKind::Keyword, "iteration")],
            start,
            typed,
        ));
    }
    if words.first() == Some(&"async") && words.len() == 1 {
        return Some(plain(
            ["def", "for", "with"]
                .into_iter()
                .map(|name| item(name, CompletionKind::Keyword, "async statement"))
                .collect(),
            start,
            typed,
        ));
    }
    if words.len() > 1
        && !words.contains(&"as")
        && matches!(words.first().copied(), Some("with" | "except" | "import"))
        && !line.ends_with('.')
        && (typed.is_empty() || "as".starts_with(typed))
    {
        return Some(plain(
            vec![item("as", CompletionKind::Keyword, "bind an alias")],
            start,
            typed,
        ));
    }
    if matches!(words.first().copied(), Some("except" | "raise")) {
        let inf = Inferer::new(db);
        let base = inf.builtin_class("BaseException");
        let mut items = Vec::new();
        let mut files = vec![file.clone()];
        if let Some(builtins) = db.builtins() {
            files.push(builtins);
        }
        for source in files {
            for (name, ids) in &source.scopes.scope(source.scopes.module_scope()).bindings {
                let Some(id) = ids.last() else {
                    continue;
                };
                if let Type::Class(class, _) = inf.type_of_binding(&source, *id) {
                    if base
                        .as_ref()
                        .is_some_and(|base| inf.mro(&class).contains(base))
                    {
                        let mut candidate = item(name, CompletionKind::Class, "exception");
                        if words.first() == Some(&"raise") {
                            candidate.insert = format!("{name}()");
                            candidate.caret_offset = Some(name.len() + 1);
                        }
                        items.push(candidate);
                    }
                }
            }
        }
        return Some(plain(items, start, typed));
    }
    if line.starts_with('@') {
        let inf = Inferer::new(db);
        let mut items = Vec::new();
        let mut imports = Vec::new();
        for binding in &file.scopes.bindings {
            if binding.scope != file.scopes.module_scope() {
                continue;
            }
            let ids = file.scopes.bindings_in(binding.scope, &binding.name);
            if let Some(id) = ids.last() {
                if matches!(
                    inf.type_of_binding(file, *id),
                    Type::Class(..) | Type::Function(_) | Type::Callable(_) | Type::Overloaded(_)
                ) {
                    items.push(item(&binding.name, CompletionKind::Function, "decorator"));
                    imports.push(None);
                }
            }
        }
        for name in ["staticmethod", "classmethod", "property"] {
            items.push(item(name, CompletionKind::Function, "builtin decorator"));
            imports.push(None);
        }
        for (module, name) in [
            ("dataclasses", "dataclass"),
            ("typing", "overload"),
            ("typing", "final"),
            ("typing", "runtime_checkable"),
            ("abc", "abstractmethod"),
        ] {
            if file
                .scopes
                .resolve(file.scopes.scope_at(offset), name)
                .is_none()
            {
                items.push(item(
                    name,
                    CompletionKind::Function,
                    &format!("from {module}"),
                ));
                imports.push(Some((module.into(), name.into())));
            }
        }
        return Some(batch(items, imports, start, typed));
    }
    let scope = file.scopes.scope(file.scopes.scope_at(offset));
    if line.starts_with("def")
        && scope
            .parent
            .is_some_and(|parent| file.scopes.scope(parent).kind == ScopeKind::Class)
        || line.starts_with("def") && scope.kind == ScopeKind::Class
    {
        let mut items = crate::completion_ctx::override_completions(db, file, offset);
        for (name, args) in [
            ("__init__", "self"),
            ("__new__", "cls"),
            ("__repr__", "self"),
            ("__str__", "self"),
            ("__bytes__", "self"),
            ("__format__", "self, format_spec"),
            ("__bool__", "self"),
            ("__len__", "self"),
            ("__iter__", "self"),
            ("__next__", "self"),
            ("__aiter__", "self"),
            ("__anext__", "self"),
            ("__getitem__", "self, key"),
            ("__setitem__", "self, key, value"),
            ("__delitem__", "self, key"),
            ("__contains__", "self, item"),
            ("__call__", "self, *args, **kwargs"),
            ("__enter__", "self"),
            ("__exit__", "self, exc_type, exc_value, traceback"),
            ("__aenter__", "self"),
            ("__aexit__", "self, exc_type, exc_value, traceback"),
            ("__eq__", "self, other"),
            ("__hash__", "self"),
            ("__getattr__", "self, name"),
            ("__getattribute__", "self, name"),
            ("__setattr__", "self, name, value"),
            ("__delattr__", "self, name"),
            ("__get__", "self, instance, owner"),
            ("__set__", "self, instance, value"),
            ("__delete__", "self, instance"),
        ] {
            if !items.iter().any(|item| item.label == name) {
                let mut value = item(name, CompletionKind::Method, "special method");
                value.insert = format!("{name}({args}):");
                items.push(value);
            }
        }
        return Some(plain(items, start, typed));
    }
    None
}
/// Control-only keywords belong to their actual syntax/scope, not every name lookup.
pub fn keyword_allowed(db: &Analysis, file: &FileInfo, offset: TextSize, keyword: &str) -> bool {
    let root = file.parse.syntax_node();
    let token = root.token_at_offset(offset).left_biased();
    let ancestors: Vec<_> = token
        .into_iter()
        .flat_map(|token| token.parent_ancestors())
        .collect();
    let function = ancestors
        .iter()
        .find_map(|node| ast::FuncDef::cast(node.clone()));
    let before = &file.text[..usize::from(offset)];
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let line = &before[line_start..];
    let indent = line.len() - line.trim_start().len();
    let previous = before[..line_start]
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .find(|line| line.len() - line.trim_start().len() <= indent)
        .map(str::trim_start)
        .unwrap_or("");
    let starts = |keywords: &[&str]| {
        keywords.iter().any(|word| {
            previous.starts_with(&format!("{word} ")) || previous.starts_with(&format!("{word}:"))
        })
    };
    match keyword {
        "else" => starts(&["if", "elif", "for", "while", "try", "except"]),
        "elif" => starts(&["if", "elif"]),
        "except" | "finally" => starts(&["try", "except"]),
        "as" => ["import ", "from ", "with ", "async with ", "except "]
            .iter()
            .any(|prefix| line.trim_start().starts_with(prefix)),
        "in" => line.split_whitespace().count() > 1,
        "match" => db.resolver.python_version >= (3, 10),
        "type" => db.resolver.python_version >= (3, 12),
        "return" | "yield" => {
            function.is_some()
                && ancestors
                    .iter()
                    .find(|node| {
                        matches!(node.kind(), SyntaxKind::CLASS_DEF | SyntaxKind::FUNC_DEF)
                    })
                    .is_some_and(|node| node.kind() == SyntaxKind::FUNC_DEF)
        }
        "await" => function.is_some_and(|function| function.is_async()),
        "break" | "continue" => ancestors
            .iter()
            .take_while(|node| node.kind() != SyntaxKind::FUNC_DEF)
            .any(|node| matches!(node.kind(), SyntaxKind::FOR_STMT | SyntaxKind::WHILE_STMT)),
        "case" => ancestors
            .iter()
            .any(|node| node.kind() == SyntaxKind::MATCH_STMT),
        "nonlocal" => {
            ancestors
                .iter()
                .filter(|node| node.kind() == SyntaxKind::FUNC_DEF)
                .count()
                > 1
        }
        _ => true,
    }
}

/// End of the identifier or structured string/package segment replaced by Tab.
/// All consumers use the same byte range; LSP converts it through its UTF index.
pub fn replacement_end(text: &str, offset: usize, segment: bool) -> usize {
    offset
        + text
            .get(offset..)
            .unwrap_or("")
            .find(|character: char| {
                if segment {
                    character.is_whitespace()
                        || matches!(
                            character,
                            '/' | '\\'
                                | '\''
                                | '"'
                                | ','
                                | ';'
                                | ']'
                                | '}'
                                | '='
                                | '>'
                                | '<'
                                | '!'
                                | '~'
                                | '#'
                                | '('
                                | ')'
                        )
                } else {
                    !rustpy_core::is_ident_continue(character)
                }
            })
            .unwrap_or(text.len().saturating_sub(offset))
}

#[cfg(test)]
mod imported_annotation_tests {
    use super::*;

    fn candidates(source: &str) -> BasicCompletion {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.py");
        let offset = source.find("$0").unwrap();
        let source = source.replacen("$0", "", 1);
        let db = Analysis::for_project(root.path());
        let id = db.set_file_text(&path, &source);
        let file = db.file(id).unwrap();
        complete(&db, &file, TextSize::new(offset as u32)).unwrap()
    }

    #[test]
    fn canonical_imported_forms_and_aliases_keep_existing_binding_without_autoimport() {
        for (source, expected) in [
            (
                "from typing import Optional\nvalue = None # type: Opt$0",
                "Optional",
            ),
            (
                "from typing import Optional as Maybe\nvalue: May$0",
                "Maybe",
            ),
            (
                "from typing import Literal as Choice\nvalue = None # type: Cho$0",
                "Choice",
            ),
        ] {
            let completion = candidates(source);
            let index = completion
                .items
                .iter()
                .position(|item| item.label == expected)
                .expect("canonical imported annotation form remains in the existing lookup");
            assert_eq!(completion.items[index].insert, expected);
            assert!(
                completion.imports[index].is_none(),
                "already imported form must not add another import"
            );
        }
    }

    #[test]
    fn rebound_forms_and_unrelated_or_lookalike_values_are_not_annotation_candidates() {
        for (source, rejected) in [
            (
                "from typing import Optional as Maybe\nMaybe = 1\nvalue: May$0",
                "Maybe",
            ),
            (
                "from typing import Optional\nOptional = 1\nvalue = None # type: Opt$0",
                "Optional",
            ),
            (
                "from typing import TYPE_CHECKING as Flag\nvalue: Fla$0",
                "Flag",
            ),
            (
                "from typing import cast as Convert\nvalue: Con$0",
                "Convert",
            ),
            (
                "class _SpecialForm: pass\nlookalike = _SpecialForm()\nvalue: loo$0",
                "lookalike",
            ),
            (
                "from typing import Optional as Maybe\ndel Maybe\nvalue: May$0",
                "Maybe",
            ),
        ] {
            let completion = candidates(source);
            assert!(
                !completion.items.iter().any(|item| item.label == rejected),
                "{source}"
            );
        }
    }

    #[test]
    fn structured_deletion_retires_alias_but_object_mutation_keeps_the_base_name() {
        for target in [
            "(Maybe,)",
            "[Maybe]",
            "(other, (Maybe,))",
            "[other, ([Maybe],)]",
        ] {
            let source =
                format!("from typing import Optional as Maybe\ndel {target}\nvalue: May$0");
            let completion = candidates(&source);
            assert!(
                !completion.items.iter().any(|item| item.label == "Maybe"),
                "{source}"
            );
        }
        for target in [
            "Maybe.value",
            "Maybe[0]",
            "(Maybe.value, other)",
            "[Maybe[0], (other,)]",
        ] {
            let source =
                format!("from typing import Optional as Maybe\ndel {target}\nvalue: May$0");
            let completion = candidates(&source);
            let index = completion
                .items
                .iter()
                .position(|item| item.label == "Maybe")
                .unwrap_or_else(|| panic!("object mutation must preserve imported base: {source}"));
            assert!(completion.imports[index].is_none());
            assert_eq!(completion.items[index].insert, "Maybe");
        }
    }
}
