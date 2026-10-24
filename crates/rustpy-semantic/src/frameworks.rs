//! Framework model facts from immutable Analysis files and resolved import bindings.
//! No project imports, interpreter processes or parallel source index.
use crate::{
    db::{Analysis, FileInfo},
    features::{CompletionItem, CompletionKind, Location},
    infer::Inferer,
    scope::BindingKind,
    types::{ClassRef, Type},
};
use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use std::sync::Arc;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Django,
    SqlAlchemy,
    Pydantic,
    Form,
    Admin,
}
#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub range: TextRange,
    pub scalar: String,
    pub related: Option<String>,
    pub reverse: Option<String>,
    /// `related_query_name=`: the reverse name used by lookups when it differs.
    pub query_name: Option<String>,
    /// Declaring field class name (`CharField`, `ForeignKey`, ...); empty for annotations.
    pub kind: String,
}
#[derive(Clone, Debug)]
pub struct Model {
    pub class: ClassRef,
    pub family: Family,
    pub fields: Vec<Field>,
}
fn expression(file: &FileInfo, range: TextRange) -> Option<ast::Expr> {
    file.parse
        .syntax_node()
        .descendants()
        .filter(|node| node.text_range() == range)
        .find_map(ast::Expr::cast)
}
pub fn origin(file: &FileInfo, expr: &ast::Expr) -> Option<String> {
    origin_inner(file, expr, 0)
}
fn origin_inner(file: &FileInfo, expr: &ast::Expr, depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    match expr.clone().unparen() {
        ast::Expr::Attribute(attr) => Some(format!(
            "{}.{}",
            origin_inner(file, &attr.object()?, depth + 1)?,
            attr.attr_text()
        )),
        ast::Expr::Call(call) => origin_inner(file, &call.callee()?, depth + 1),
        ast::Expr::Name(name) => {
            let ids = file.scopes.resolve(
                file.scopes.scope_at(name.syntax().text_range().start()),
                &name.name_text(),
            )?;
            if ids.len() != 1 {
                return None;
            }
            let binding = file.scopes.binding(ids[0]);
            match &binding.kind {
                BindingKind::Import(info) if info.level == 0 => Some(match &info.name {
                    Some(name) => format!("{}.{}", info.module, name),
                    None => info.module.to_string(),
                }),
                BindingKind::Assignment => {
                    origin_inner(file, &expression(file, binding.value?)?, depth + 1)
                }
                _ => None,
            }
        }
        _ => None,
    }
}
fn keyword(call: &ast::CallExpr, name: &str) -> Option<ast::Expr> {
    call.arg_list()?
        .keywords()
        .find(|keyword| keyword.name_text() == name)?
        .value()
}
fn string(expr: ast::Expr) -> Option<String> {
    if let ast::Expr::String(string) = expr.unparen() {
        Some(string.value())
    } else {
        None
    }
}
fn positional(call: &ast::CallExpr) -> Vec<ast::Expr> {
    call.arg_list()
        .map(|args| {
            args.args()
                .filter_map(|arg| match arg {
                    ast::AnyArg::Positional(arg) => arg.expr(),
                    _ => None,
                })
                .take(64)
                .collect()
        })
        .unwrap_or_default()
}
fn family(file: &FileInfo, class: &ast::ClassDef) -> Option<Family> {
    for base in class.bases() {
        let Some(origin) = origin(file, &base) else {
            continue;
        };
        if origin.starts_with("django.db.models.") && origin.ends_with("Model") {
            return Some(Family::Django);
        }
        if (origin.starts_with("sqlalchemy.") || origin.starts_with("flask_sqlalchemy."))
            && (origin.ends_with("DeclarativeBase")
                || origin.ends_with("declarative_base")
                || origin.ends_with(".Model")
                || origin.ends_with("SQLAlchemy.Model"))
        {
            return Some(Family::SqlAlchemy);
        }
        if (origin.starts_with("pydantic.") || origin.starts_with("pydantic_settings."))
            && (origin.ends_with("BaseModel") || origin.ends_with("BaseSettings"))
        {
            return Some(Family::Pydantic);
        }
        if origin == "django.contrib.admin.ModelAdmin" {
            return Some(Family::Admin);
        }
        if origin.starts_with("django.forms.")
            && (origin.ends_with("Form") || origin.ends_with("ModelForm"))
        {
            return Some(Family::Form);
        }
    }
    None
}
pub fn models(file: &Arc<FileInfo>) -> Vec<Model> {
    // Keyed by file id; an entry answers only for the exact `FileInfo` it was computed
    // from. Sized for installed frameworks (thousands of library modules).
    type Entry = (std::sync::Weak<FileInfo>, Vec<Model>);
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<rustc_hash::FxHashMap<rustpy_core::FileId, Entry>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Ok(values) = cache.try_lock() {
        if let Some((owner, models)) = values.get(&file.id) {
            if std::ptr::eq(owner.as_ptr(), Arc::as_ptr(file)) && owner.strong_count() > 0 {
                return models.clone();
            }
        }
    }
    let models = models_uncached(file);
    if let Ok(mut values) = cache.try_lock() {
        if values.len() >= 16384 {
            values.retain(|_, (owner, _)| owner.strong_count() > 0);
            if values.len() >= 16384 {
                values.clear();
            }
        }
        values.insert(file.id, (Arc::downgrade(file), models.clone()));
    }
    models
}
fn models_uncached(file: &Arc<FileInfo>) -> Vec<Model> {
    if file.text.len() > 2 * 1024 * 1024 {
        return Vec::new();
    }
    if !["django", "sqlalchemy", "pydantic", "sqlmodel"]
        .iter()
        .any(|name| file.text.contains(name))
    {
        return Vec::new();
    }
    models_with_classifier(file, |class| family(file, class))
}
thread_local! {static INHERITANCE_QUERY:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};}
/// Live dependency-aware family query. Direct facts retain their exact-FileInfo cache;
/// inherited family is recomputed against current Analysis/MRO, never cached by child alone.
pub fn models_in_analysis(db: &Analysis, file: &Arc<FileInfo>) -> Vec<Model> {
    if INHERITANCE_QUERY.with(|active| active.replace(true)) {
        return models(file);
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INHERITANCE_QUERY.with(|active| active.set(false));
        }
    }
    let _reset = Reset;
    let inf = Inferer::new(db);
    models_with_classifier(file, |class| {
        family(file, class).or_else(|| {
            let name = class.name()?;
            let reference = ClassRef {
                file: file.id,
                range: class.syntax().text_range(),
                name: name.text().into(),
            };
            for (ancestor, _) in inf
                .mro_with_args(&reference, &[])
                .into_iter()
                .skip(1)
                .take(64)
            {
                let Some(source) = db.file(ancestor.file) else {
                    continue;
                };
                if let Some(model) = models(&source).into_iter().find(|m| m.class == ancestor) {
                    return Some(model.family);
                }
            }
            None
        })
    })
}
fn models_with_classifier(
    file: &Arc<FileInfo>,
    classify: impl Fn(&ast::ClassDef) -> Option<Family>,
) -> Vec<Model> {
    if file.text.len() > 2 * 1024 * 1024 {
        return vec![];
    }
    let mut out = Vec::new();
    for class in file
        .parse
        .syntax_node()
        .descendants()
        .take(20000)
        .filter_map(ast::ClassDef::cast)
        .take(256)
    {
        let Some(family) = classify(&class) else {
            continue;
        };
        let Some(name) = class.name() else {
            continue;
        };
        let mut fields = Vec::new();
        if let Some(body) = class.body() {
            for statement in body.statements().take(2048) {
                let (target, annotation, value) = match statement {
                    ast::Stmt::Assign(assign) => {
                        (assign.targets().into_iter().next(), None, assign.value())
                    }
                    ast::Stmt::AnnAssign(assign) => {
                        (assign.target(), assign.annotation(), assign.value())
                    }
                    _ => continue,
                };
                let Some(ast::Expr::Name(target)) = target else {
                    continue;
                };
                let name = target.name_text();
                if name.starts_with('_') || name == "model_config" {
                    continue;
                }
                let mut scalar = annotation
                    .as_ref()
                    .map(|expr| expr.syntax().text().to_string())
                    .unwrap_or_default();
                let mut related = None;
                let mut reverse = None;
                let mut query_name = None;
                let mut kind = String::new();
                if let Some(ast::Expr::Call(call)) = value {
                    if let Some(head) = call.callee().and_then(|expr| origin(file, &expr)) {
                        let last = head.rsplit('.').next().unwrap_or("");
                        let valid = match family {
                            Family::Django => {
                                head.starts_with("django.db.models.")
                                    || head.starts_with("django.contrib.postgres.fields.")
                            }
                            Family::SqlAlchemy => {
                                head.starts_with("sqlalchemy.")
                                    || head.starts_with("flask_sqlalchemy.")
                            }
                            Family::Form => head.starts_with("django.forms."),
                            Family::Admin => head.starts_with("django.contrib.admin."),
                            Family::Pydantic => head.starts_with("pydantic."),
                        };
                        if !valid && scalar.is_empty() {
                            continue;
                        }
                        kind = last.to_owned();
                        if scalar.is_empty() && matches!(last, "Column" | "mapped_column") {
                            if let Some(first) = positional(&call).first() {
                                let ty = origin(file, first)
                                    .unwrap_or_else(|| first.syntax().text().to_string());
                                scalar = match ty.rsplit('.').next().unwrap_or("") {
                                    "Integer" | "BigInteger" | "SmallInteger" => "int",
                                    "String" | "Text" | "Unicode" => "str",
                                    "Float" | "Numeric" | "DECIMAL" => "float",
                                    "Boolean" => "bool",
                                    _ => "",
                                }
                                .into();
                            }
                        }
                        if scalar.is_empty() {
                            scalar = match last {
                                "CharField" | "TextField" | "EmailField" | "SlugField"
                                | "URLField" | "String" | "Text" => "str",
                                "IntegerField" | "BigIntegerField" | "AutoField" | "Integer"
                                | "BigInteger" => "int",
                                "BooleanField" | "Boolean" => "bool",
                                "FloatField" | "Float" | "DecimalField" => "float",
                                _ => "Unknown",
                            }
                            .into();
                        }
                        if matches!(
                            last,
                            "ForeignKey" | "OneToOneField" | "ManyToManyField" | "relationship"
                        ) {
                            related = positional(&call)
                                .first()
                                .cloned()
                                .map(|expr| {
                                    string(expr.clone())
                                        .unwrap_or_else(|| expr.syntax().text().to_string())
                                })
                                .or_else(|| keyword(&call, "to").and_then(string));
                            reverse = keyword(&call, "related_name")
                                .and_then(string)
                                .filter(|name| name != "+");
                            query_name = keyword(&call, "related_query_name").and_then(string);
                        }
                    }
                }
                if !scalar.is_empty() {
                    fields.push(Field {
                        name,
                        range: target.syntax().text_range(),
                        scalar,
                        related,
                        reverse,
                        query_name,
                        kind,
                    });
                }
            }
        }
        out.push(Model {
            class: ClassRef {
                file: file.id,
                range: class.syntax().text_range(),
                name: name.text().into(),
            },
            family,
            fields,
        });
    }
    out
}
mod model_cache;
pub(crate) use model_cache::ModelCache;
/// Loaded project sources (no library or stub files); empty when too many to scan.
fn project_sources(db: &Analysis) -> Vec<Arc<FileInfo>> {
    model_cache::project(db).files.clone().unwrap_or_default()
}
/// Models visible from `file`, cached per source generation (see `model_cache`).
fn known(db: &Analysis, file: &Arc<FileInfo>) -> Arc<Vec<Model>> {
    model_cache::known(db, file)
}
fn model_for_type<'a>(models: &'a [Model], ty: &Type) -> Option<&'a Model> {
    let class = match ty {
        Type::Class(class, _) | Type::Instance(class, _) | Type::SelfType(class) => class,
        _ => return None,
    };
    models.iter().find(|model| &model.class == class)
}
/// The model `class` declares directly, or the nearest model among its ancestors.
fn model_of(inf: &Inferer<'_>, file: &Arc<FileInfo>, class: &ClassRef) -> Option<Model> {
    let mut model = models(file).into_iter().find(|model| &model.class == class);
    if model.is_none() {
        for base in inf.mro(class).iter().skip(1) {
            if let Some(source) = inf.db.file(base.file) {
                model = models(&source)
                    .into_iter()
                    .find(|model| &model.class == base);
                if model.is_some() {
                    break;
                }
            }
        }
    }
    model
}
/// Members Django's model metaclass adds to every concrete model: the default manager,
/// the per-model exceptions and the implicit primary key. Used only after the regular
/// lookup failed (real Django sources declare none of them on `Model`).
pub fn model_member(inf: &Inferer<'_>, class: &ClassRef, name: &str) -> Option<Type> {
    if !matches!(
        name,
        "objects"
            | "_default_manager"
            | "_base_manager"
            | "DoesNotExist"
            | "MultipleObjectsReturned"
            | "pk"
            | "id"
    ) {
        return None;
    }
    let file = inf.db.file(class.file)?;
    if file.is_stub {
        return None;
    }
    let model = model_of(inf, &file, class)?;
    if model.family != Family::Django {
        return None;
    }
    let resolve = |module: &str, name: &str| {
        let source = inf.db.module(module)?;
        let ids = source.scopes.resolve(source.scopes.module_scope(), name)?;
        Some(inf.type_of_binding(&source, *ids.last()?))
    };
    Some(match name {
        "objects" | "_default_manager" | "_base_manager" => {
            match resolve("django.db.models.manager", "Manager") {
                Some(Type::Class(manager, _)) => Type::Instance(manager, Vec::new()),
                _ => Type::Unknown,
            }
        }
        "DoesNotExist" => resolve("django.core.exceptions", "ObjectDoesNotExist")
            .filter(|ty| matches!(ty, Type::Class(..)))
            .unwrap_or(Type::Unknown),
        "MultipleObjectsReturned" => resolve("django.core.exceptions", "MultipleObjectsReturned")
            .filter(|ty| matches!(ty, Type::Class(..)))
            .unwrap_or(Type::Unknown),
        // An explicit `primary_key=True` field replaces `id`; its type is not tracked.
        "id" if model.fields.is_empty() || !file.text.contains("primary_key") => {
            inf.instance_of("int")
        }
        _ => Type::Unknown,
    })
}
pub fn field_type(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    class: &ClassRef,
    name: &str,
) -> Option<Type> {
    let model = model_of(inf, file, class)?;
    let field = model.fields.iter().find(|field| field.name == name)?;
    if let Some(target) = &field.related {
        if target == "self" {
            return Some(Type::Instance(model.class.clone(), Vec::new()));
        }
        if let Some(related) = known(inf.db, file)
            .iter()
            .find(|model| model.class.name.as_str() == target.rsplit('.').next().unwrap_or(target))
        {
            return Some(Type::Instance(related.class.clone(), vec![]));
        }
    }
    let scalar = field
        .scalar
        .strip_prefix("Mapped[")
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(&field.scalar);
    match scalar {
        "str" | "int" | "float" | "bool" => Some(inf.instance_of(scalar)),
        _ => None,
    }
}
fn query_model<'a>(
    db: &Analysis,
    file: &Arc<FileInfo>,
    expr: &ast::Expr,
    models: &'a [Model],
    depth: usize,
) -> Option<&'a Model> {
    if depth > 12 || models.is_empty() {
        return None;
    }
    let inf = Inferer::new(db);
    match expr.clone().unparen() {
        ast::Expr::Call(call) => {
            let head = call.callee()?;
            if origin(file, &head)
                .is_some_and(|name| name.starts_with("sqlalchemy.") && name.ends_with(".select"))
            {
                if let Some(model) = positional(&call).first() {
                    return model_for_type(models, &inf.type_of_expr(file, model));
                }
            }
            if let ast::Expr::Attribute(attr) = &head {
                let method = attr.attr_text();
                if matches!(
                    method.as_str(),
                    "filter"
                        | "exclude"
                        | "all"
                        | "get"
                        | "first"
                        | "last"
                        | "order_by"
                        | "select_related"
                        | "prefetch_related"
                        | "annotate"
                        | "alias"
                        | "values"
                        | "values_list"
                        | "only"
                        | "defer"
                        | "none"
                        | "using"
                        | "reverse"
                        | "select_for_update"
                        | "distinct"
                        | "query"
                        | "where"
                        | "join"
                        | "scalars"
                        | "execute"
                ) {
                    if method == "execute" || method == "scalars" {
                        if let Some(target) = positional(&call).first() {
                            if let Some(model) = query_model(db, file, target, models, depth + 1) {
                                return Some(model);
                            }
                        }
                    }
                    if method == "query" {
                        if let Some(target) = positional(&call).first() {
                            return model_for_type(models, &inf.type_of_expr(file, target));
                        }
                    }
                    return query_model(db, file, &attr.object()?, models, depth + 1);
                }
            }
            None
        }
        ast::Expr::Attribute(attr)
            if matches!(
                attr.attr_text().as_str(),
                "objects" | "_default_manager" | "query"
            ) =>
        {
            model_for_type(models, &inf.type_of_expr(file, &attr.object()?))
        }
        ast::Expr::Name(name) => {
            let definition = inf.resolve_name(
                file,
                file.scopes.scope_at(name.syntax().text_range().start()),
                &name.name_text(),
            )?;
            if let Some(range) = definition.binding().value {
                let expr = expression(&definition.file, range)?;
                return query_model(db, &definition.file, &expr, models, depth + 1);
            }
            None
        }
        _ => None,
    }
}
fn completion(name: &str, detail: &str) -> CompletionItem {
    CompletionItem {
        label: name.into(),
        insert: name.into(),
        detail: detail.into(),
        kind: CompletionKind::Attribute,
        caret_offset: None,
        priority: 0,
    }
}
/// Django lookup/transform names; validity per field class is decided by [`lookups`].
const COMPARISON: &[&str] = &["exact", "in", "gt", "gte", "lt", "lte", "range", "isnull"];
const TEXT_LOOKUPS: &[&str] = &[
    "iexact",
    "contains",
    "icontains",
    "startswith",
    "istartswith",
    "endswith",
    "iendswith",
    "regex",
    "iregex",
];
const DATE_PARTS: &[&str] = &[
    "year",
    "iso_year",
    "month",
    "day",
    "week",
    "week_day",
    "iso_week_day",
    "quarter",
];
const TIME_PARTS: &[&str] = &["hour", "minute", "second"];
fn lookups(kind: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    match kind {
        "BooleanField" | "NullBooleanField" => out.extend(["exact", "in", "isnull"]),
        "JSONField" | "HStoreField" => {
            out.extend(["exact", "in", "isnull", "contains", "contained_by"]);
            out.extend(["has_key", "has_keys", "has_any_keys"]);
            if kind == "HStoreField" {
                out.extend(["keys", "values"]);
            }
        }
        "ArrayField" => out.extend([
            "exact",
            "in",
            "isnull",
            "contains",
            "contained_by",
            "overlap",
            "len",
        ]),
        "CharField"
        | "TextField"
        | "EmailField"
        | "SlugField"
        | "URLField"
        | "UUIDField"
        | "FilePathField"
        | "FileField"
        | "ImageField"
        | "GenericIPAddressField"
        | "IPAddressField" => {
            out.extend(COMPARISON);
            out.extend(TEXT_LOOKUPS);
        }
        "IntegerField"
        | "BigIntegerField"
        | "SmallIntegerField"
        | "PositiveIntegerField"
        | "PositiveSmallIntegerField"
        | "PositiveBigIntegerField"
        | "AutoField"
        | "BigAutoField"
        | "SmallAutoField"
        | "FloatField"
        | "DecimalField"
        | "DurationField"
        | "ForeignKey"
        | "OneToOneField"
        | "ManyToManyField"
        | "reverse" => out.extend(COMPARISON),
        "DateField" | "DateTimeField" | "TimeField" => {
            out.extend(COMPARISON);
            if kind != "TimeField" {
                out.extend(DATE_PARTS);
            }
            if kind == "DateTimeField" {
                out.extend(["date", "time"]);
            }
            if kind != "DateField" {
                out.extend(TIME_PARTS);
            }
        }
        // Unknown/custom field class: every generic lookup and transform.
        _ => {
            out.extend(COMPARISON);
            out.extend(TEXT_LOOKUPS);
            out.extend(DATE_PARTS);
            out.extend(["date", "time"]);
            out.extend(TIME_PARTS);
        }
    }
    out
}
/// Field class produced by a transform; `None` for terminal lookups.
fn transform(segment: &str) -> Option<&'static str> {
    match segment {
        "date" => Some("DateField"),
        "time" => Some("TimeField"),
        "keys" | "values" => Some("ArrayField"),
        "len" => Some("IntegerField"),
        _ if DATE_PARTS.contains(&segment) || TIME_PARTS.contains(&segment) => Some("IntegerField"),
        _ => None,
    }
}
/// How a Django ORM path is interpreted at its call site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PathMode {
    /// `filter`/`exclude`/`get`/`Q` keywords: relations, then lookups and transforms.
    Lookup,
    /// `order_by`/`values`/`only`/`defer`/`F` strings: field paths across relations.
    Field,
    /// `select_related`: single-valued relations only.
    Select,
    /// `prefetch_related`: any relation, reverse relations by accessor name.
    Prefetch,
}
/// A queryable model member: declared field or reverse relation (declared on `owner`).
struct Member<'a> {
    name: String,
    owner: &'a Model,
    field: &'a Field,
    kind: &'a str,
    target: Option<&'a Model>,
    single: bool,
}
fn related_model<'a>(models: &'a [Model], owner: &'a Model, target: &str) -> Option<&'a Model> {
    if target == "self" {
        return Some(owner);
    }
    let name = target.rsplit('.').next().unwrap_or(target);
    models
        .iter()
        .filter(|model| model.family == Family::Django && model.class.name.as_str() == name)
        .min_by_key(|model| model.class.file != owner.class.file)
}
fn members<'a>(models: &'a [Model], model: &'a Model, accessor: bool) -> Vec<Member<'a>> {
    let mut out = Vec::new();
    for field in &model.fields {
        out.push(Member {
            name: field.name.clone(),
            owner: model,
            field,
            kind: &field.kind,
            target: field
                .related
                .as_deref()
                .and_then(|target| related_model(models, model, target)),
            single: matches!(field.kind.as_str(), "ForeignKey" | "OneToOneField"),
        });
    }
    for other in models.iter().filter(|other| other.family == Family::Django) {
        for field in &other.fields {
            let Some(target) = field.related.as_deref() else {
                continue;
            };
            let short = target.rsplit('.').next().unwrap_or(target);
            if (short != model.class.name.as_str()
                && !(target == "self" && other.class == model.class))
                || related_model(models, other, target)
                    .is_none_or(|found| found.class != model.class)
            {
                continue;
            }
            let one = field.kind == "OneToOneField";
            let lower = other.class.name.to_lowercase();
            let query = field.query_name.clone().filter(|_| !accessor);
            out.push(Member {
                name: query.or_else(|| field.reverse.clone()).unwrap_or_else(|| {
                    if accessor && !one {
                        format!("{lower}_set")
                    } else {
                        lower
                    }
                }),
                owner: other,
                field,
                kind: "reverse",
                target: Some(other),
                single: one,
            });
        }
    }
    out
}
#[derive(Clone)]
enum PathState<'a> {
    /// At a model; `true` when reached through a relation (relation lookups apply).
    Model(&'a Model, bool),
    Scalar(String),
    Done,
}
fn path_step<'a>(
    models: &'a [Model],
    state: &PathState<'a>,
    mode: PathMode,
    segment: &str,
) -> Option<(PathState<'a>, Option<Member<'a>>)> {
    match state {
        PathState::Model(model, via) => {
            if let Some(member) = members(models, model, mode == PathMode::Prefetch)
                .into_iter()
                .find(|member| member.name == segment)
            {
                let next = match member.target {
                    Some(target) => PathState::Model(target, true),
                    None => PathState::Scalar(member.kind.to_owned()),
                };
                return Some((next, Some(member)));
            }
            if matches!(mode, PathMode::Lookup | PathMode::Field) {
                // `pk`, the implicit `id` and a forward relation's `<name>_id` column.
                if segment == "pk" || segment == "id" {
                    return Some((PathState::Scalar("AutoField".into()), None));
                }
                if let Some(relation) = segment.strip_suffix("_id") {
                    if model.fields.iter().any(|field| {
                        field.name == relation
                            && matches!(field.kind.as_str(), "ForeignKey" | "OneToOneField")
                    }) {
                        return Some((PathState::Scalar("ForeignKey".into()), None));
                    }
                }
            }
            (mode == PathMode::Lookup && *via && COMPARISON.contains(&segment))
                .then_some((PathState::Done, None))
        }
        PathState::Scalar(kind) if mode == PathMode::Lookup => {
            if lookups(kind).contains(&segment) {
                let next = transform(segment)
                    .map_or(PathState::Done, |kind| PathState::Scalar(kind.into()));
                return Some((next, None));
            }
            // JSON key transforms accept any key and keep JSON lookups.
            matches!(kind.as_str(), "JSONField" | "HStoreField")
                .then(|| (PathState::Scalar("JSONField".into()), None))
        }
        _ => None,
    }
}
fn path_candidates(
    models: &[Model],
    state: &PathState<'_>,
    mode: PathMode,
    base: &str,
) -> Vec<CompletionItem> {
    let mut out = Vec::new();
    let lookup = |name: &str| {
        completion(
            &format!("{base}{name}"),
            if transform(name).is_some() {
                "Django ORM transform"
            } else {
                "Django ORM lookup"
            },
        )
    };
    match state {
        PathState::Model(model, via) => {
            for member in members(models, model, mode == PathMode::Prefetch) {
                match mode {
                    PathMode::Select if member.target.is_none() || !member.single => continue,
                    PathMode::Prefetch if member.target.is_none() => continue,
                    _ => {}
                }
                let detail = if member.kind == "reverse" {
                    format!("Reverse relation from {}", member.owner.class.name)
                } else {
                    format!("Django field: {}", member.kind)
                };
                out.push(completion(&format!("{base}{}", member.name), &detail));
                if mode == PathMode::Lookup && member.target.is_none() {
                    for name in lookups(member.kind) {
                        out.push(lookup(&format!("{}__{name}", member.name)));
                    }
                }
            }
            if matches!(mode, PathMode::Lookup | PathMode::Field) {
                out.push(completion(&format!("{base}pk"), "Django primary key"));
            }
            if mode == PathMode::Lookup && *via {
                out.extend(COMPARISON.iter().map(|name| lookup(name)));
            }
        }
        PathState::Scalar(kind) if mode == PathMode::Lookup => {
            out.extend(lookups(kind).into_iter().map(lookup));
        }
        _ => {}
    }
    out
}
/// Syntactic ORM path site: keyword name / bare positional name, or positional string,
/// inside `call`; `start`/`text` cover the path (after quotes and an `order_by` `-`).
struct OrmSite {
    call: ast::CallExpr,
    string: bool,
    start: usize,
    text: String,
}
fn orm_site(file: &FileInfo, offset: TextSize) -> Option<OrmSite> {
    use rustpy_syntax::SyntaxKind;
    let root = file.parse.syntax_node();
    let at = root.token_at_offset(offset);
    let call_of = |list: Option<rustpy_syntax::SyntaxNode>| {
        list.filter(|list| list.kind() == SyntaxKind::ARG_LIST)
            .and_then(|list| list.parent())
            .and_then(ast::CallExpr::cast)
    };
    for token in [at.clone().left_biased(), at.clone().right_biased()]
        .into_iter()
        .flatten()
    {
        let Some(parent) = token.parent() else {
            continue;
        };
        if token.kind() == SyntaxKind::IDENT {
            let list = match parent.kind() {
                SyntaxKind::KEYWORD_ARG => parent.parent(),
                SyntaxKind::NAME_EXPR => parent
                    .parent()
                    .filter(|arg| arg.kind() == SyntaxKind::ARG)
                    .and_then(|arg| arg.parent()),
                _ => None,
            };
            if let Some(call) = call_of(list) {
                return Some(OrmSite {
                    call,
                    string: false,
                    start: usize::from(token.text_range().start()),
                    text: token.text().to_owned(),
                });
            }
        } else if token.kind() == SyntaxKind::STRING {
            let Some(string) = ast::StringExpr::cast(parent.clone()) else {
                continue;
            };
            let content = string.content_range();
            if !content.contains_inclusive(offset) {
                continue;
            }
            let arg = parent.parent().filter(|arg| arg.kind() == SyntaxKind::ARG);
            if let Some(call) = call_of(arg.and_then(|arg| arg.parent())) {
                let mut start = usize::from(content.start());
                let mut text = file.text[start..usize::from(content.end())].to_owned();
                if text.starts_with('-') && usize::from(offset) > start {
                    text.remove(0);
                    start += 1;
                }
                return Some(OrmSite {
                    call,
                    string: true,
                    start,
                    text,
                });
            }
        }
    }
    // Empty argument slot: `filter(|` / `filter(a=1, |`.
    let end = usize::from(offset).min(file.text.len());
    if file.text[..end].trim_end().ends_with(['(', ',']) {
        let list = at
            .left_biased()?
            .parent_ancestors()
            .find(|node| node.kind() == SyntaxKind::ARG_LIST)?;
        if list.text_range().start() < offset {
            return Some(OrmSite {
                call: call_of(Some(list))?,
                string: false,
                start: end,
                text: String::new(),
            });
        }
    }
    None
}
/// Model and interpretation for an ORM site: queryset methods directly, `Q`/`F` through
/// the enclosing queryset call. Annotate/aggregate keywords are user names, never paths.
fn orm_path<'a>(
    db: &Analysis,
    file: &Arc<FileInfo>,
    site: &OrmSite,
    models: &'a [Model],
) -> Option<(&'a Model, PathMode)> {
    let callee = site.call.callee()?;
    let identity = origin(file, &callee).unwrap_or_default();
    let enclosing = || {
        site.call
            .syntax()
            .ancestors()
            .skip(1)
            .filter_map(ast::CallExpr::cast)
            .take(32)
            .find_map(|call| match call.callee()? {
                ast::Expr::Attribute(attr) => query_model(db, file, &attr.object()?, models, 0),
                _ => None,
            })
    };
    let django = identity.starts_with("django.db.models.");
    let (model, mode) = if django && identity.ends_with(".Q") && !site.string {
        (enclosing()?, PathMode::Lookup)
    } else if django && identity.ends_with(".F") && site.string {
        (enclosing()?, PathMode::Field)
    } else {
        let ast::Expr::Attribute(attr) = callee else {
            return None;
        };
        let mode = match (attr.attr_text().as_str(), site.string) {
            ("filter" | "exclude" | "get", false) => PathMode::Lookup,
            ("order_by" | "values" | "values_list" | "only" | "defer" | "distinct", true) => {
                PathMode::Field
            }
            ("select_related", true) => PathMode::Select,
            ("prefetch_related", true) => PathMode::Prefetch,
            _ => return None,
        };
        (query_model(db, file, &attr.object()?, models, 0)?, mode)
    };
    (model.family == Family::Django).then_some((model, mode))
}
fn orm_complete(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    site: &OrmSite,
    models: &[Model],
) -> Option<crate::basic_completion::BasicCompletion> {
    let (model, mode) = orm_path(db, file, site, models)?;
    let typed = file.text.get(site.start..usize::from(offset))?;
    let mut state = PathState::Model(model, false);
    let base = match typed.rsplit_once("__") {
        Some((done, _)) => {
            for segment in done.split("__").take(32) {
                state = path_step(models, &state, mode, segment)?.0;
            }
            format!("{done}__")
        }
        None => String::new(),
    };
    let mut items = path_candidates(models, &state, mode, &base);
    items.retain(|item| crate::features::matches_prefix(&item.label, &typed.to_lowercase()));
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.dedup_by(|a, b| a.label == b.label);
    if items.is_empty() {
        return None;
    }
    let count = items.len();
    Some(crate::basic_completion::BasicCompletion {
        items,
        imports: vec![None; count],
        prefix_start: TextSize::new(site.start as u32),
        replace_segment: false,
        import_classes: false,
    })
}
/// Navigation for the `__` segment under `offset` of a Django ORM path.
fn orm_definition(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    models: &[Model],
) -> Option<Vec<Location>> {
    let site = orm_site(file, offset)?;
    let (model, mode) = orm_path(db, file, &site, models)?;
    let at = usize::from(offset);
    let mut state = PathState::Model(model, false);
    let mut start = site.start;
    // A resolved Django path never falls back to first-segment navigation.
    for segment in site.text.split("__").take(32) {
        let Some((next, member)) = path_step(models, &state, mode, segment) else {
            break;
        };
        if at <= start + segment.len() {
            let source = member.as_ref().and_then(|m| db.file(m.owner.class.file));
            return Some(
                member
                    .zip(source)
                    .map(|(member, source)| Location {
                        path: source.path.clone(),
                        range: member.field.range,
                        is_stub: source.is_stub,
                    })
                    .into_iter()
                    .collect(),
            );
        }
        state = next;
        start += segment.len() + 2;
    }
    Some(Vec::new())
}
/// An unresolved segment of a Django ORM lookup or field path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrmProblem {
    pub range: TextRange,
    pub message: String,
}
/// What a model's source says about the completeness of its declared fields.
#[derive(Clone, Default)]
struct ModelShape {
    /// Direct concrete `models.Model` subclass declaring only recognised fields.
    closed: bool,
    /// Abstract or `Meta.default_related_name`: its relations' reverse names are not literal.
    templated: bool,
    /// Last name component of each base expression.
    bases: Vec<String>,
}
fn model_shape(db: &Analysis, model: &Model) -> ModelShape {
    let Some(source) = db.file(model.class.file) else {
        return ModelShape::default();
    };
    let Some(class) = source
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::ClassDef::cast)
        .find(|class| class.syntax().text_range() == model.class.range)
    else {
        return ModelShape::default();
    };
    let bases = class.bases();
    let direct = !bases.is_empty()
        && class
            .arguments()
            .is_some_and(|args| args.keywords().next().is_none())
        && bases.iter().all(|base| {
            origin(&source, base).is_some_and(|name| {
                matches!(
                    name.as_str(),
                    "django.db.models.Model" | "django.db.models.base.Model"
                )
            })
        });
    let mut shape = ModelShape {
        closed: direct,
        templated: false,
        bases: bases
            .iter()
            .map(|base| {
                let text = base.syntax().text().to_string();
                text.rsplit('.').next().unwrap_or(&text).trim().to_owned()
            })
            .collect(),
    };
    for statement in class
        .body()
        .into_iter()
        .flat_map(|body| body.statements().take(2048).collect::<Vec<_>>())
    {
        match statement {
            ast::Stmt::ClassDef(meta) if meta.name_text() == "Meta" => {
                for item in meta
                    .body()
                    .into_iter()
                    .flat_map(|body| body.statements().take(256).collect::<Vec<_>>())
                {
                    let ast::Stmt::Assign(assign) = item else {
                        continue;
                    };
                    let names: Vec<_> = assign
                        .targets()
                        .iter()
                        .filter_map(|target| match target {
                            ast::Expr::Name(name) => Some(name.name_text()),
                            _ => None,
                        })
                        .collect();
                    let truthy = assign
                        .value()
                        .is_some_and(|value| value.syntax().text() != "False");
                    if names.iter().any(|name| name == "default_related_name")
                        || (truthy && names.iter().any(|name| name == "abstract"))
                    {
                        shape.templated = true;
                        shape.closed = false;
                    }
                }
            }
            ast::Stmt::Assign(_) | ast::Stmt::AnnAssign(_) => {
                let (target, value) = match &statement {
                    ast::Stmt::Assign(assign) => {
                        (assign.targets().into_iter().next(), assign.value())
                    }
                    ast::Stmt::AnnAssign(assign) => (assign.target(), assign.value()),
                    _ => continue,
                };
                // A call the model scan did not recognise may be a third-party field,
                // a generic relation or a manager adding annotations.
                if let (Some(ast::Expr::Name(target)), Some(ast::Expr::Call(_))) = (target, value) {
                    let name = target.name_text();
                    if !model.fields.iter().any(|field| field.name == name) {
                        shape.closed = false;
                    }
                }
            }
            _ => {}
        }
    }
    shape
}
/// Names a queryset chain introduces with `annotate`/`alias`/`values` keywords; `None`
/// when the chain may introduce names that cannot be listed (positional aggregates,
/// `**` expansion). A variable receiver also contributes every annotation in its file,
/// covering conditional reassignment.
fn chain_names(
    db: &Analysis,
    file: &Arc<FileInfo>,
    expr: &ast::Expr,
    out: &mut Vec<String>,
    depth: usize,
) -> Option<()> {
    if depth > 12 {
        return None;
    }
    let collect = |call: &ast::CallExpr, out: &mut Vec<String>| -> Option<()> {
        let ast::Expr::Attribute(attr) = call.callee()? else {
            return Some(());
        };
        let method = attr.attr_text();
        if !matches!(
            method.as_str(),
            "annotate" | "alias" | "values" | "values_list" | "extra"
        ) {
            return Some(());
        }
        for arg in call.arg_list()?.args() {
            match arg {
                ast::AnyArg::Keyword(keyword) => out.push(keyword.name_text()),
                ast::AnyArg::Positional(_)
                    if matches!(method.as_str(), "values" | "values_list") => {}
                _ => return None,
            }
        }
        Some(())
    };
    match expr.clone().unparen() {
        ast::Expr::Call(call) => {
            collect(&call, out)?;
            match call.callee()? {
                ast::Expr::Attribute(attr) => {
                    chain_names(db, file, &attr.object()?, out, depth + 1)
                }
                _ => Some(()),
            }
        }
        ast::Expr::Name(name) => {
            let inf = Inferer::new(db);
            let definition = inf.resolve_name(
                file,
                file.scopes.scope_at(name.syntax().text_range().start()),
                &name.name_text(),
            )?;
            let source = definition.file.clone();
            for call in source
                .parse
                .syntax_node()
                .descendants()
                .filter_map(ast::CallExpr::cast)
                .take(8192)
            {
                collect(&call, out)?;
            }
            if let Some(range) = definition.binding().value {
                let value = expression(&source, range)?;
                return chain_names(db, &source, &value, out, depth + 1);
            }
            Some(())
        }
        _ => Some(()),
    }
}
/// Lookups a Django field may also accept when optional contrib apps are installed.
const CONTRIB_LOOKUPS: &[&str] = &[
    "unaccent",
    "search",
    "trigram_similar",
    "trigram_word_similar",
    "trigram_strict_word_similar",
    "lower",
    "upper",
];
/// Keyword paths of `Q(...)` objects in a lookup argument, through `|`/`&`/`^`/`~`
/// combinations and nested `Q` positionals. Any other expression (subqueries,
/// helper calls) belongs to a different model and is never entered.
fn q_paths(file: &FileInfo, expr: ast::Expr, out: &mut Vec<(usize, String)>, budget: &mut usize) {
    use rustpy_syntax::SyntaxKind;
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    match expr.unparen() {
        ast::Expr::Binary(binary) => {
            if binary.op().is_some_and(|op| {
                matches!(
                    op.kind(),
                    SyntaxKind::PIPE | SyntaxKind::AMP | SyntaxKind::CARET
                )
            }) {
                for side in [binary.lhs(), binary.rhs()].into_iter().flatten() {
                    q_paths(file, side, out, budget);
                }
            }
        }
        ast::Expr::Unary(unary) => {
            if unary.op().is_some_and(|op| op.kind() == SyntaxKind::TILDE) {
                if let Some(operand) = unary.operand() {
                    q_paths(file, operand, out, budget);
                }
            }
        }
        ast::Expr::Call(call) => {
            let is_q = call
                .callee()
                .and_then(|callee| origin(file, &callee))
                .is_some_and(|identity| {
                    identity.starts_with("django.db.models.") && identity.ends_with(".Q")
                });
            let Some(args) = call.arg_list().filter(|_| is_q) else {
                return;
            };
            for keyword in args.keywords() {
                if let Some(name) = keyword.name() {
                    out.push((
                        usize::from(name.text_range().start()),
                        name.text().to_owned(),
                    ));
                }
            }
            for positional in args.positional() {
                q_paths(file, positional, out, budget);
            }
        }
        _ => {}
    }
}
/// Unknown field names and lookups in Django queryset calls whose model fields are
/// fully known: `filter/exclude/get` keywords (directly and inside `Q` objects),
/// many-valued relations in `select_related`, and literal path strings of
/// `order_by/values/values_list/only/defer/select_related/prefetch_related`.
/// Conservative: models with inherited, abstract, third-party or dynamically added
/// members, unresolved relation targets and annotated names are never reported.
pub fn orm_problems(db: &Analysis, file: &Arc<FileInfo>) -> Vec<OrmProblem> {
    let mut out = Vec::new();
    if file.text.len() > 2 * 1024 * 1024
        || ![
            "filter(",
            "exclude(",
            "get(",
            "order_by(",
            "values(",
            "values_list(",
            "only(",
            "defer(",
            "_related(",
        ]
        .iter()
        .any(|needle| file.text.contains(needle))
    {
        return out;
    }
    // Reverse relation names are only complete when every project model is visible.
    // Library sources (Django itself adds members dynamically) do not count.
    if model_cache::is_library(db, file) {
        return out;
    }
    let project = model_cache::project(db);
    if project.files.is_none() || project.dynamic {
        return out;
    }
    let custom_lookups = project.custom_lookups;
    let models = known(db, file);
    let shapes = std::cell::RefCell::new(vec![None; models.len()]);
    let shape = |model: &Model| -> ModelShape {
        let Some(index) = models.iter().position(|m| m.class == model.class) else {
            return ModelShape::default();
        };
        shapes.borrow_mut()[index]
            .get_or_insert_with(|| model_shape(db, model))
            .clone()
    };
    let targets = |field: &Field, model: &Model, owner: &Model| {
        field.related.as_deref().is_some_and(|target| {
            target.rsplit('.').next().unwrap_or(target) == model.class.name.as_str()
                || (target == "self" && owner.class == model.class)
        })
    };
    // A model is reported against only when its reverse names are literal too:
    // no templated relation targets it and no model inherits from it (the
    // multi-table parent link adds a reverse one-to-one).
    let certain = |model: &Model| -> bool {
        shape(model).closed
            && !models
                .iter()
                .filter(|other| other.family == Family::Django && other.class != model.class)
                .any(|other| {
                    let relates = other.fields.iter().filter(|f| targets(f, model, other));
                    let templated = relates.clone().any(|field| {
                        field
                            .reverse
                            .iter()
                            .chain(&field.query_name)
                            .any(|name| name.contains("%("))
                    });
                    let relates = relates.count() > 0;
                    templated
                        || (relates && shape(other).templated)
                        || shape(other)
                            .bases
                            .iter()
                            .any(|base| base == model.class.name.as_str())
                })
            && !model.fields.iter().any(|field| {
                targets(field, model, model)
                    && field
                        .reverse
                        .iter()
                        .chain(&field.query_name)
                        .any(|name| name.contains("%("))
            })
    };
    let root = file.parse.syntax_node();
    for call in root
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .take(4096)
    {
        let Some(ast::Expr::Attribute(attr)) = call.callee() else {
            continue;
        };
        let mode = match attr.attr_text().as_str() {
            "filter" | "exclude" | "get" => PathMode::Lookup,
            "order_by" | "values" | "values_list" | "only" | "defer" => PathMode::Field,
            "select_related" => PathMode::Select,
            "prefetch_related" => PathMode::Prefetch,
            _ => continue,
        };
        let Some(args) = call.arg_list() else {
            continue;
        };
        // (path start offset, path text)
        let mut paths = Vec::new();
        if mode == PathMode::Lookup {
            for keyword in args.keywords() {
                if let Some(name) = keyword.name() {
                    paths.push((
                        usize::from(name.text_range().start()),
                        name.text().to_owned(),
                    ));
                }
            }
            let mut budget = 256;
            for expr in args.positional() {
                q_paths(file, expr, &mut paths, &mut budget);
            }
        } else {
            for expr in args.positional() {
                let ast::Expr::String(string) = expr else {
                    continue;
                };
                let content = string.content_range();
                let (start, end) = (usize::from(content.start()), usize::from(content.end()));
                let Some(text) = file.text.get(start..end) else {
                    continue;
                };
                if !string.prefix().chars().all(|c| matches!(c, 'u' | 'r'))
                    || string.value() != text
                {
                    continue;
                }
                let (start, text) = match text.strip_prefix('-') {
                    Some(rest) if mode == PathMode::Field => (start + 1, rest),
                    _ => (start, text),
                };
                if !text.is_empty() && text.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric())
                {
                    paths.push((start, text.to_owned()));
                }
            }
        }
        if paths.is_empty() {
            continue;
        }
        let Some(receiver) = attr.object() else {
            continue;
        };
        let Some(model) = query_model(db, file, &receiver, &models, 0) else {
            continue;
        };
        if model.family != Family::Django || !shape(model).closed {
            continue;
        }
        let mut annotations = Vec::new();
        if chain_names(db, file, &receiver, &mut annotations, 0).is_none() {
            continue;
        }
        for (start, text) in paths {
            if text
                .split("__")
                .next()
                .is_some_and(|first| annotations.iter().any(|name| name == first))
            {
                continue;
            }
            let mut state = PathState::Model(model, false);
            let mut at = start;
            for segment in text.split("__").take(32) {
                let range = TextRange::new(
                    TextSize::new(at as u32),
                    TextSize::new((at + segment.len()) as u32),
                );
                match path_step(&models, &state, mode, segment) {
                    Some((next, member)) => {
                        if let Some(member) = &member {
                            // Relations to unresolved targets end the known path.
                            if member.field.related.is_some() && member.target.is_none() {
                                break;
                            }
                            if matches!(mode, PathMode::Select | PathMode::Prefetch)
                                && member.target.is_none()
                            {
                                out.push(OrmProblem {
                                    range,
                                    message: format!(
                                        "'{segment}' is not a relation of '{}'",
                                        member.owner.class.name
                                    ),
                                });
                                break;
                            }
                            // Django rejects many-valued relations (many-to-many and
                            // reverse foreign keys) in `select_related`.
                            let many = member.kind == "ManyToManyField"
                                || (member.kind == "reverse"
                                    && matches!(
                                        member.field.kind.as_str(),
                                        "ForeignKey" | "ManyToManyField"
                                    ));
                            if mode == PathMode::Select && member.target.is_some() && many {
                                out.push(OrmProblem {
                                    range,
                                    message: format!(
                                        "'{segment}' is a many-valued relation and cannot be used in select_related(); use prefetch_related()"
                                    ),
                                });
                                break;
                            }
                        }
                        state = next;
                    }
                    None => {
                        match &state {
                            PathState::Model(current, _) if certain(current) => {
                                out.push(OrmProblem {
                                    range,
                                    message: format!(
                                        "Cannot resolve '{segment}' into a field of '{}'",
                                        current.class.name
                                    ),
                                });
                            }
                            PathState::Scalar(kind)
                                if mode == PathMode::Lookup
                                    && !custom_lookups
                                    && !CONTRIB_LOOKUPS.contains(&segment)
                                    && lookups(kind) != lookups("") =>
                            {
                                out.push(OrmProblem {
                                    range,
                                    message: format!("Unsupported lookup '{segment}' for {kind}"),
                                });
                            }
                            _ => {}
                        }
                        break;
                    }
                }
                at += segment.len() + 2;
            }
        }
    }
    out
}
#[cfg(test)]
mod orm_path_fixtures {
    use super::*;
    use crate::module::ModuleResolver;
    const MODELS: &str = "from django.db import models\nfrom django.db.models import Q, F\nclass Author(models.Model):\n    name = models.CharField(max_length=50)\n    born = models.DateField()\nclass Tag(models.Model):\n    label = models.CharField(max_length=20)\nclass Book(models.Model):\n    title = models.CharField(max_length=90)\n    price = models.IntegerField()\n    published = models.DateTimeField()\n    meta = models.JSONField()\n    author = models.ForeignKey(Author, related_name='books', on_delete=models.CASCADE)\n    tags = models.ManyToManyField('Tag')\nclass Review(models.Model):\n    book = models.ForeignKey(Book, on_delete=models.CASCADE)\n    stars = models.IntegerField()\n";
    fn analysis(line: &str) -> (Analysis, Arc<FileInfo>, String, TextSize) {
        let source = format!("{MODELS}{}\n", line.replace('|', ""));
        let offset = TextSize::new((MODELS.len() + line.find('|').unwrap_or(0)) as u32);
        let db = Analysis::new(ModuleResolver::new(vec![], vec![], (3, 13)));
        let id = db.set_file_text(std::path::Path::new("/project/models.py"), &source);
        let file = db.file(id).unwrap();
        (db, file, source, offset)
    }
    fn labels(line: &str) -> Vec<String> {
        let (db, file, _, offset) = analysis(line);
        complete(&db, &file, offset)
            .map(|completion| {
                completion
                    .items
                    .into_iter()
                    .map(|item| item.label.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
    fn has(line: &str, present: &[&str], absent: &[&str]) {
        let labels = labels(line);
        for label in present {
            assert!(
                labels.iter().any(|l| l == label),
                "{line}: missing {label} in {labels:?}"
            );
        }
        for label in absent {
            assert!(
                !labels.iter().any(|l| l == label),
                "{line}: unexpected {label}"
            );
        }
    }
    #[test]
    fn lookup_keywords_traverse_relations_and_offer_typed_lookups() {
        has(
            "Book.objects.filter(author__na|)",
            &["author__name", "author__name__icontains"],
            &["title", "author__title", "author__born"],
        );
        has(
            "Book.objects.exclude(author__name__|)",
            &[
                "author__name__iexact",
                "author__name__iregex",
                "author__name__range",
            ],
            &["author__name__year", "author__name__has_key"],
        );
        has(
            "Book.objects.get(published__|)",
            &[
                "published__date",
                "published__iso_week_day",
                "published__hour",
            ],
            &["published__icontains", "published__overlap"],
        );
        has(
            "Book.objects.filter(published__year__|)",
            &["published__year__gte", "published__year__in"],
            &["published__year__month"],
        );
        has(
            "Book.objects.filter(author__born__|)",
            &["author__born__quarter", "author__born__week_day"],
            &["author__born__hour", "author__born__time"],
        );
        has(
            "Book.objects.filter(price__|)",
            &["price__range", "price__isnull"],
            &["price__icontains", "price__year"],
        );
        has(
            "Book.objects.filter(meta__|)",
            &["meta__has_key", "meta__contains", "meta__contained_by"],
            &["meta__year", "meta__icontains"],
        );
        has(
            "Book.objects.filter(tags__|)",
            &["tags__label", "tags__in", "tags__isnull"],
            &["tags__title"],
        );
        has(
            "Book.objects.filter(|)",
            &["title", "author", "review", "pk", "title__startswith"],
            &["review_set", "books"],
        );
        // Reverse relations: related_name, then the default query name.
        has("Author.objects.filter(books__ti|)", &["books__title"], &[]);
        has(
            "Book.objects.filter(review__|)",
            &["review__stars", "review__book"],
            &[],
        );
        has(
            "Author.objects.filter(books__review__stars__|)",
            &["books__review__stars__lte"],
            &["books__review__stars__icontains"],
        );
        // annotate/aggregate keywords are user names.
        has("Book.objects.annotate(ti|=F('price'))", &[], &["title"]);
    }
    #[test]
    fn q_and_f_use_the_enclosing_queryset_model() {
        has(
            "Book.objects.filter(Q(author__na|) | Q(price=1))",
            &["author__name", "author__name__exact"],
            &[],
        );
        has(
            "Book.objects.filter(price__gt=F('au|'))",
            &["author"],
            &["author__in"],
        );
        has(
            "Book.objects.annotate(total=F('author__|'))",
            &["author__name", "author__books"],
            &["author__name__icontains"],
        );
        has("q = Q(ti|)", &[], &["title"]);
    }
    #[test]
    fn string_paths_respect_method_relation_rules() {
        has(
            "Book.objects.order_by('-author__|')",
            &["author__name", "author__born"],
            &["author__name__icontains"],
        );
        has("Book.objects.values('ti|')", &["title"], &[]);
        has(
            "Book.objects.values_list('author__n|', flat=True)",
            &["author__name"],
            &[],
        );
        has("Book.objects.only('pri|')", &["price"], &[]);
        has("Book.objects.defer('me|')", &["meta"], &[]);
        has(
            "Book.objects.select_related('|')",
            &["author"],
            &["tags", "title", "review", "review_set"],
        );
        has(
            "Book.objects.prefetch_related('|')",
            &["author", "tags", "review_set"],
            &["title", "review"],
        );
        has(
            "Author.objects.prefetch_related('books__|')",
            &["books__tags", "books__review_set"],
            &["books__title"],
        );
        has(
            "Book.objects.filter(price=1).order_by('author__|')",
            &["author__name"],
            &[],
        );
    }
    /// `segment: message` for each ORM problem in `MODELS` followed by `extra`.
    fn problems(extra: &str) -> Vec<String> {
        let (db, file, source, _) = analysis(extra);
        orm_problems(&db, &file)
            .into_iter()
            .map(|problem| {
                let range = usize::from(problem.range.start())..usize::from(problem.range.end());
                format!("{}: {}", &source[range], problem.message)
            })
            .collect()
    }
    #[test]
    fn unknown_orm_fields_relations_and_lookups_are_reported() {
        let found = problems(
            "Book.objects.filter(titel='x')\nBook.objects.exclude(author__nam='x')\nBook.objects.get(title__foo='x')\nBook.objects.order_by('-pricee')\nBook.objects.values('author__x')\nBook.objects.select_related('title')\nBook.objects.prefetch_related('reviews')\nAuthor.objects.filter(books__review__star=1)\nqs = Book.objects.annotate(n=F('price'))\nqs.filter(n=1, bogus=2)",
        );
        assert_eq!(
            found,
            [
                "titel: Cannot resolve 'titel' into a field of 'Book'",
                "nam: Cannot resolve 'nam' into a field of 'Author'",
                "foo: Unsupported lookup 'foo' for CharField",
                "pricee: Cannot resolve 'pricee' into a field of 'Book'",
                "x: Cannot resolve 'x' into a field of 'Author'",
                "title: 'title' is not a relation of 'Book'",
                "reviews: Cannot resolve 'reviews' into a field of 'Book'",
                "star: Cannot resolve 'star' into a field of 'Review'",
                "bogus: Cannot resolve 'bogus' into a field of 'Book'",
            ]
        );
    }
    #[test]
    fn valid_orm_paths_and_annotations_are_not_reported() {
        let found = problems(
            "Book.objects.filter(pk=1, id=2, author_id=3, author_id__in=[], author__name__icontains='x', published__year__gte=2000, published__date__lt=1, review__stars=3, tags__label='a', author__in=[], meta__any__key__has_key='k', title__unaccent='x')\nBook.objects.annotate(n=F('price')).filter(n__gt=1).order_by('-n', '?', 'author_id', 'pk')\nBook.objects.alias(m=F('price')).filter(m=1).values('m', 'title', 'author__born')\nBook.objects.annotate(F('price')).filter(price__count=1)\nBook.objects.select_related('author').prefetch_related('review_set', 'tags', 'author__books').values_list('title', flat=True)\nAuthor.objects.filter(books__title='x', books__review__stars=1).only('name').defer('born')\nTag.objects.filter(book__title='x')\nd = {}\nd.get(x=1)\nBook.objects.filter(**{'bad': 1}).order_by(f'{d}')",
        );
        assert!(found.is_empty(), "{found:?}");
    }
    #[test]
    fn q_keywords_use_the_enclosing_queryset_model() {
        // `|` is the fixture cursor marker, so `&`/`^` stand in for it here.
        let found = problems(
            "Book.objects.filter(Q(titel='x') & ~Q(author__nam='y'))\nBook.objects.exclude(Q(Q(price__foo=1), pricee=2) & Q(title='ok'))\nAuthor.objects.get(Q(books__title='x'), Q(nme='z'))\nqs = Book.objects.annotate(n=F('price'))\nqs.filter(Q(n__gt=1) ^ Q(bogus=2))",
        );
        assert_eq!(
            found,
            [
                "titel: Cannot resolve 'titel' into a field of 'Book'",
                "nam: Cannot resolve 'nam' into a field of 'Author'",
                "pricee: Cannot resolve 'pricee' into a field of 'Book'",
                "foo: Unsupported lookup 'foo' for IntegerField",
                "nme: Cannot resolve 'nme' into a field of 'Author'",
                "bogus: Cannot resolve 'bogus' into a field of 'Book'",
            ]
        );
        // Q objects outside a queryset call, in subqueries or helper calls, and
        // valid Q paths are never reported against the outer model.
        let found = problems(
            "q = Q(anything=1)\nBook.objects.filter(Q(author__name='x') & ~Q(review__stars=1), Q(tags__label='a'))\nBook.objects.filter(author__in=Author.objects.filter(Q(name='x')))\nBook.objects.filter(helper(Q(zzz=1)))\nBook.objects.annotate(c=Count('tags', filter=Q(tags__label='x')))",
        );
        assert!(found.is_empty(), "{found:?}");
    }
    #[test]
    fn many_valued_relations_in_select_related_are_reported() {
        let found = problems(
            "Book.objects.select_related('tags', 'review', 'author__books', 'author')\nAuthor.objects.select_related('books')\nTag.objects.select_related('book')",
        );
        let message = |name: &str| {
            format!("{name}: '{name}' is a many-valued relation and cannot be used in select_related(); use prefetch_related()")
        };
        assert_eq!(
            found,
            [
                message("tags"),
                message("review"),
                message("books"),
                message("books"),
                message("book"),
            ]
        );
        let found = problems(
            "class Profile(models.Model):\n    author = models.OneToOneField(Author, on_delete=models.CASCADE)\nAuthor.objects.select_related('profile')\nProfile.objects.select_related('author', 'author__profile')\nReview.objects.select_related('book__author')",
        );
        assert!(found.is_empty(), "{found:?}");
    }
    #[test]
    fn models_with_unknown_members_are_never_reported() {
        let found = problems(
            "class Stamped(models.Model):\n    created = models.DateTimeField()\n    owner = models.ForeignKey(Author, related_name='%(class)s_items', on_delete=models.CASCADE)\n    class Meta:\n        abstract = True\nclass Post(Stamped):\n    body = models.TextField()\nclass Place(models.Model):\n    name = models.CharField(max_length=5)\nclass Restaurant(Place):\n    seats = models.IntegerField()\nclass Item(models.Model):\n    tags = TaggableManager()\nclass Shelf(models.Model):\n    book = models.ForeignKey('library.Missing', on_delete=models.CASCADE)\nPost.objects.filter(created__year=1, anything=2)\nAuthor.objects.filter(post_items__body='x')\nPlace.objects.filter(restaurant__seats=1)\nItem.objects.filter(tags__name='x', zzz=1)\nShelf.objects.filter(book__whatever=1)\nShelf.objects.filter(shelf_typo=1)",
        );
        assert_eq!(
            found,
            ["shelf_typo: Cannot resolve 'shelf_typo' into a field of 'Shelf'"]
        );
    }
    #[test]
    fn every_path_segment_navigates_to_its_declaration() {
        let target = |line: &str, needle: &str| {
            let (db, file, source, _) = analysis(line);
            let at = MODELS.len() + line.find(needle).unwrap() + 1;
            let found = definition(&db, &file, TextSize::new(at as u32));
            found.first().map(|location| {
                let (start, end) = (location.range.start(), location.range.end());
                let (start, end) = (usize::from(start), usize::from(end));
                format!("{}@{start}", &source[start..end])
            })
        };
        let field = |decl: &str| {
            let at = MODELS.find(decl).unwrap();
            Some(format!("{}@{at}", decl.split(' ').next().unwrap()))
        };
        let line = "Book.objects.filter(author__name__icontains='x')";
        assert_eq!(target(line, "author"), field("author = models.ForeignKey"));
        assert_eq!(target(line, "name"), field("name = models.CharField"));
        assert_eq!(target(line, "icontains"), None);
        let line = "Author.objects.filter(books__review__stars=3)";
        assert_eq!(target(line, "books"), field("author = models.ForeignKey"));
        assert_eq!(target(line, "review"), field("book = models.ForeignKey"));
        assert_eq!(target(line, "stars"), field("stars = models.IntegerField"));
        let line = "Book.objects.prefetch_related('review_set')";
        assert_eq!(
            target(line, "review_set"),
            field("book = models.ForeignKey")
        );
        let line = "Book.objects.order_by('-author__born')";
        assert_eq!(target(line, "born"), field("born = models.DateField"));
        let line = "Book.objects.filter(Q(tags__label='x'))";
        assert_eq!(target(line, "label"), field("label = models.CharField"));
        let line = "Book.objects.update(price=F('author__name'))";
        assert_eq!(target(line, "name"), field("name = models.CharField"));
    }
}
pub fn complete(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Option<crate::basic_completion::BasicCompletion> {
    let end = usize::from(offset).min(file.text.len());
    let before = &file.text[..end];
    let mut start = before
        .rfind(|c: char| !rustpy_core::is_ident_continue(c))
        .map_or(0, |at| {
            at + before[at..].chars().next().map_or(0, char::len_utf8)
        });
    let mut prefix = &file.text[start..end];
    let line = before.rsplit('\n').next().unwrap_or_default();
    let site = orm_site(file, offset);
    if site.is_none() && !line.contains(['.', '(', '\'', '"']) {
        return None;
    }
    let models = known(db, file);
    // Completion runs on the UI thread: candidate scans stop once the budget is spent.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(150);
    let expired = || std::time::Instant::now() > deadline;
    // Django lookup paths: relation traversal at every `__` segment, then field-typed lookups.
    if let Some(completion) = site
        .as_ref()
        .and_then(|site| orm_complete(db, file, offset, site, &models))
    {
        return Some(completion);
    }
    let root = file.parse.syntax_node();
    let mut items = Vec::new();
    // ORM keyword/string lookup arguments retain the model receiver, including chained queries.
    for call in root
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter(|call| call.syntax().text_range().contains_inclusive(offset))
        .take(if models.is_empty() { 0 } else { 128 })
    {
        if expired() {
            break;
        }
        let Some(ast::Expr::Attribute(attr)) = call.callee() else {
            continue;
        };
        let Some(model) = attr
            .object()
            .and_then(|expr| query_model(db, file, &expr, &models, 0))
        else {
            continue;
        };
        // Django paths are handled by `orm_complete`; other argument positions get nothing.
        if model.family == Family::Django {
            break;
        }
        if !matches!(
            attr.attr_text().as_str(),
            "filter"
                | "exclude"
                | "get"
                | "order_by"
                | "select_related"
                | "prefetch_related"
                | "values"
                | "values_list"
        ) {
            continue;
        }
        for field in &model.fields {
            items.push(completion(
                &field.name,
                &format!("{:?} field: {}", model.family, field.scalar),
            ));
        }
        for other in models.iter() {
            for field in &other.fields {
                if field.related.as_ref().is_some_and(|target| {
                    target.rsplit('.').next() == Some(model.class.name.as_str())
                }) {
                    let reverse = field.reverse.clone().unwrap_or_else(|| {
                        if matches!(
                            attr.attr_text().as_str(),
                            "filter" | "get" | "exclude" | "select_related"
                        ) {
                            other.class.name.to_lowercase()
                        } else {
                            format!("{}_set", other.class.name.to_lowercase())
                        }
                    });
                    items.push(completion(&reverse, "Reverse model relation"));
                }
            }
        }
        break;
    }
    if items.is_empty() {
        for attribute in root
            .descendants()
            .filter_map(ast::AttributeExpr::cast)
            .filter(|attribute| attribute.syntax().text_range().contains_inclusive(offset))
            .take(128)
        {
            if expired() {
                break;
            }
            let Some(object) = attribute.object() else {
                continue;
            };
            if origin(file, &object).as_deref() == Some("django.conf.settings") {
                for source in project_sources(db).into_iter().filter(|source| {
                    source
                        .module_name
                        .as_ref()
                        .is_some_and(|name| name.split('.').any(|part| part == "settings"))
                }) {
                    for binding in source
                        .scopes
                        .bindings
                        .iter()
                        .filter(|binding| {
                            binding.name.chars().any(char::is_alphabetic)
                                && binding
                                    .name
                                    .chars()
                                    .all(|c| !c.is_alphabetic() || c.is_uppercase())
                        })
                        .take(4096)
                    {
                        items.push(completion(&binding.name, "Django settings key"));
                    }
                }
            }
            if let Some(model) = query_model(db, file, &object, &models, 0) {
                for method in [
                    "filter",
                    "exclude",
                    "get",
                    "create",
                    "all",
                    "first",
                    "last",
                    "count",
                    "exists",
                    "order_by",
                    "select_related",
                    "prefetch_related",
                    "values",
                    "values_list",
                    "annotate",
                    "aggregate",
                    "distinct",
                    "update",
                    "delete",
                    "get_or_create",
                    "update_or_create",
                ] {
                    items.push(completion(
                        method,
                        &format!("{:?} query for {}", model.family, model.class.name),
                    ));
                }
            } else if let Some(model) = (!models.is_empty())
                .then(|| Inferer::new(db).type_of_expr(file, &object))
                .and_then(|ty| model_for_type(&models, &ty))
            {
                if model.family == Family::Django {
                    items.push(completion("objects", "Django model manager"));
                }
                for field in &model.fields {
                    items.push(completion(&field.name, &format!("{} field", field.scalar)));
                }
            }
            if !items.is_empty() {
                break;
            }
        }
    }
    if items.is_empty() {
        items = string_completions(db, file, offset, &models);
        if !items.is_empty() {
            if let Some(node) = root
                .descendants()
                .filter_map(ast::StringExpr::cast)
                .find(|node| node.syntax().text_range().contains_inclusive(offset))
            {
                let raw = &file.text[usize::from(node.syntax().text_range().start())..end];
                if let Some(quote) = raw.find(['\'', '"']) {
                    start = usize::from(node.syntax().text_range().start()) + quote + 1;
                    prefix = &file.text[start..end];
                }
            }
        }
    }
    if items.is_empty() {
        return None;
    }
    items.retain(|item| crate::features::matches_prefix(&item.label, &prefix.to_lowercase()));
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.dedup_by(|a, b| a.label == b.label);
    let count = items.len();
    Some(crate::basic_completion::BasicCompletion {
        items,
        imports: vec![None; count],
        prefix_start: TextSize::new(start as u32),
        replace_segment: false,
        import_classes: false,
    })
}
pub fn definition(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Location> {
    let root = file.parse.syntax_node();
    let model_list = known(db, file);
    for attribute in root
        .descendants()
        .filter_map(ast::AttributeExpr::cast)
        .filter(|attribute| attribute.syntax().text_range().contains_inclusive(offset))
        .take(128)
    {
        if attribute
            .object()
            .and_then(|object| origin(file, &object))
            .as_deref()
            == Some("django.conf.settings")
        {
            for source in project_sources(db).into_iter().filter(|source| {
                source
                    .module_name
                    .as_ref()
                    .is_some_and(|name| name.split('.').any(|part| part == "settings"))
            }) {
                if let Some(binding) = source
                    .scopes
                    .bindings
                    .iter()
                    .find(|binding| binding.name == attribute.attr_text())
                {
                    return vec![Location {
                        path: source.path.clone(),
                        range: binding.range,
                        is_stub: source.is_stub,
                    }];
                }
            }
        }
    }
    if let Some(string_node) = root
        .descendants()
        .filter_map(ast::StringExpr::cast)
        .find(|node| node.syntax().text_range().contains_inclusive(offset))
    {
        let name = string_node.value();
        if let Some(call) = string_node
            .syntax()
            .ancestors()
            .find_map(ast::CallExpr::cast)
        {
            let identity = call
                .callee()
                .and_then(|expr| origin(file, &expr))
                .unwrap_or_default();
            // Shared with `{% url %}` / Jinja `url_for` so both sides agree on
            // namespaced Django names and blueprint-qualified endpoints.
            let declarations = match identity.as_str() {
                "flask.url_for" => Some(crate::framework_names::flask_endpoints(db)),
                "django.urls.reverse" | "django.urls.reverse_lazy" => {
                    Some(crate::framework_names::django_urls(db))
                }
                _ => None,
            };
            if let Some(location) = declarations
                .and_then(|declarations| crate::framework_names::find(&declarations, &name))
            {
                return vec![location];
            }
            if identity.starts_with("pydantic.")
                && matches!(
                    identity.rsplit('.').next(),
                    Some("validator" | "field_validator")
                )
            {
                if let Some(model) = model_list.iter().find(|model| {
                    model.class.file == file.id && model.class.range.contains_inclusive(offset)
                }) {
                    if let Some(field) = model.fields.iter().find(|field| field.name == name) {
                        return vec![Location {
                            path: file.path.clone(),
                            range: field.range,
                            is_stub: file.is_stub,
                        }];
                    }
                }
            }
        }
    }
    if let Some(locations) = orm_definition(db, file, offset, &model_list) {
        return locations;
    }

    for call in root
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter(|call| call.syntax().text_range().contains_inclusive(offset))
        .take(128)
    {
        let Some(ast::Expr::Attribute(attr)) = call.callee() else {
            continue;
        };
        let Some(model) = attr
            .object()
            .and_then(|expr| query_model(db, file, &expr, &model_list, 0))
        else {
            continue;
        };
        let token = root.token_at_offset(offset).left_biased();
        let Some(token) = token else { continue };
        let text = token.text().trim_matches(['\'', '"']);
        let field = text.split("__").next().unwrap_or(text);
        if let Some(field) = model
            .fields
            .iter()
            .find(|candidate| candidate.name == field)
        {
            if let Some(source) = db.file(model.class.file) {
                return vec![Location {
                    path: source.path.clone(),
                    range: field.range,
                    is_stub: source.is_stub,
                }];
            }
        }
    }
    Vec::new()
}
pub fn call_result(inf: &Inferer<'_>, file: &Arc<FileInfo>, call: &ast::CallExpr) -> Option<Type> {
    if call
        .callee()
        .and_then(|head| origin(file, &head))
        .as_deref()
        == Some("django.contrib.auth.get_user_model")
    {
        let mut selected = "auth.User".to_owned();
        for source in project_sources(inf.db) {
            if let Some(binding) = source
                .scopes
                .bindings
                .iter()
                .find(|binding| binding.name == "AUTH_USER_MODEL")
            {
                if let Some(value) = binding
                    .value
                    .and_then(|range| expression(&source, range))
                    .and_then(string)
                {
                    selected = value;
                    break;
                }
            }
        }
        if let Some((app, name)) = selected.rsplit_once('.') {
            if let Some(source) = inf.db.module(&format!("{app}.models")) {
                if let Some(model) = models(&source)
                    .into_iter()
                    .find(|model| model.class.name == name)
                {
                    return Some(Type::Class(model.class, Vec::new()));
                }
            }
            if app == "auth" {
                if let Some(source) = inf.db.module("django.contrib.auth.models") {
                    if let Some(ids) = source.scopes.resolve(source.scopes.module_scope(), name) {
                        if let Some(id) = ids.last() {
                            return Some(inf.type_of_binding(&source, *id));
                        }
                    }
                }
            }
        }
    }

    if call
        .callee()
        .and_then(|head| origin(file, &head))
        .as_deref()
        == Some("fastapi.Depends")
    {
        if let Some(dependency) = positional(call).first() {
            match inf.type_of_expr(file, dependency) {
                Type::Function(signature) | Type::Callable(signature) => {
                    return Some(signature.ret.clone())
                }
                _ => {}
            }
        }
    }
    let ast::Expr::Attribute(attr) = call.callee()? else {
        return None;
    };
    if !matches!(
        attr.attr_text().as_str(),
        "get" | "first" | "last" | "create" | "get_or_create" | "update_or_create"
    ) {
        return None;
    }
    let models = known(inf.db, file);
    let model = query_model(inf.db, file, &attr.object()?, &models, 0)?;
    let instance = Type::Instance(model.class.clone(), Vec::new());
    match attr.attr_text().as_str() {
        "count" => return Some(inf.instance_of("int")),
        "exists" => return Some(inf.instance_of("bool")),
        "all" => {
            if let Type::Instance(class, _) = inf.instance_of("list") {
                return Some(Type::Instance(class, vec![instance]));
            }
        }
        _ => {}
    }

    if matches!(
        attr.attr_text().as_str(),
        "get_or_create" | "update_or_create"
    ) {
        Some(Type::Tuple(vec![instance, inf.instance_of("bool")]))
    } else if matches!(
        attr.attr_text().as_str(),
        "first" | "last" | "one_or_none" | "scalar_one_or_none"
    ) {
        Some(Type::Union(vec![instance, Type::None]))
    } else {
        Some(instance)
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    use crate::module::ModuleResolver;
    #[test]
    fn imported_aliases_and_shadowing_control_model_facts() {
        let db = Analysis::new(ModuleResolver::new(vec![], vec![], (3, 13)));
        let id=db.set_file_text(std::path::Path::new("/project/models.py"),"from django.db import models as m\nclass Book(m.Model):\n title=m.CharField(max_length=90)\n count=m.IntegerField()\n");
        let file = db.file(id).unwrap();
        let facts = models(&file);
        assert_eq!(facts[0].fields[0].scalar, "str");
        assert_eq!(facts[0].fields[1].scalar, "int");
        let id = db.set_file_text(
            std::path::Path::new("/project/shadow.py"),
            "class models: pass\nclass Fake(models.Model):\n name=models.CharField()\n",
        );
        assert!(models(&db.file(id).unwrap()).is_empty());
    }
}

fn string_completions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    models: &[Model],
) -> Vec<CompletionItem> {
    let Some(node) = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::StringExpr::cast)
        .find(|string| string.syntax().text_range().contains_inclusive(offset))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let parent = node.syntax().ancestors().find_map(ast::CallExpr::cast);
    if let Some(call) = parent {
        let identity = call
            .callee()
            .and_then(|expr| origin(file, &expr))
            .unwrap_or_default();
        if identity == "django.urls.reverse"
            || identity == "django.urls.reverse_lazy"
            || identity == "flask.url_for"
        {
            // Shared with `{% url %}` / Jinja `url_for` so both sides agree.
            if identity.starts_with("django.") {
                for declaration in crate::framework_names::django_urls(db) {
                    out.push(completion(&declaration.name, "Django URL name"));
                }
            } else {
                for declaration in crate::framework_names::flask_endpoints(db) {
                    out.push(completion(&declaration.name, "Flask endpoint"));
                }
            }
        }
        if identity.starts_with("pydantic.")
            && matches!(
                identity.rsplit('.').next(),
                Some("validator" | "field_validator")
            )
        {
            if let Some(model) = models.iter().find(|model| {
                model.class.file == file.id && model.class.range.contains_inclusive(offset)
            }) {
                for field in &model.fields {
                    out.push(completion(&field.name, "Pydantic validator field"));
                }
            }
        }
    }

    if let Some(class) = node
        .syntax()
        .ancestors()
        .filter_map(ast::ClassDef::cast)
        .find(|class| {
            models.iter().any(|model| {
                model.class.range == class.syntax().text_range()
                    && matches!(model.family, Family::Form | Family::Admin)
            })
        })
    {
        let line = file.text[..usize::from(offset)]
            .rsplit('\n')
            .next()
            .unwrap_or_default();
        if [
            "fields",
            "exclude",
            "list_display",
            "list_filter",
            "search_fields",
            "readonly_fields",
            "ordering",
        ]
        .iter()
        .any(|name| line.contains(name))
        {
            let mut target = None;
            for assignment in class
                .syntax()
                .descendants()
                .filter_map(ast::AssignStmt::cast)
                .take(256)
            {
                if assignment.targets().iter().any(
                    |target| matches!(target,ast::Expr::Name(name) if name.name_text()=="model"),
                ) {
                    if let Some(value) = assignment.value() {
                        target =
                            model_for_type(models, &Inferer::new(db).type_of_expr(file, &value));
                        break;
                    }
                }
            }
            if target.is_none() {
                for decorator in class.decorators() {
                    if let Some(ast::Expr::Call(call)) = decorator.expr() {
                        if call
                            .callee()
                            .and_then(|head| origin(file, &head))
                            .as_deref()
                            == Some("django.contrib.admin.register")
                        {
                            if let Some(model) = positional(&call).first() {
                                target = model_for_type(
                                    models,
                                    &Inferer::new(db).type_of_expr(file, model),
                                );
                            }
                        }
                    }
                }
            }
            if let Some(target) = target {
                for field in &target.fields {
                    out.push(completion(
                        &field.name,
                        "ModelForm / Django admin model field",
                    ));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod query_fixtures {
    use super::*;
    use crate::module::ModuleResolver;
    #[test]
    fn django_alias_receiver_drives_lookup_completion_and_get_inference() {
        let db = Analysis::new(ModuleResolver::new(vec![], vec![], (3, 13)));
        let source="from django.db import models as m\nclass Book(m.Model):\n title=m.CharField(max_length=90)\nbook=Book.objects.get(title='x')\nBook.objects.filter(title__icontains='x')\n";
        let id = db.set_file_text(std::path::Path::new("/project/models.py"), source);
        let file = db.file(id).unwrap();
        let offset = TextSize::new((source.find("title__icontains").unwrap() + 6) as u32);
        let complete = complete(&db, &file, offset).unwrap();
        assert!(complete
            .items
            .iter()
            .any(|item| item.label == "title__icontains"));
        let call = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .find(|call| {
                call.callee()
                    .is_some_and(|callee| callee.syntax().text() == "Book.objects.get")
            })
            .unwrap();
        assert!(
            matches!(call_result(&Inferer::new(&db),&file,&call),Some(Type::Instance(class,_))if class.name=="Book")
        );
    }
}

pub(crate) fn flask_names(file: &Arc<FileInfo>) -> Vec<(String, TextRange)> {
    let mut out = Vec::new();
    for function in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .take(2048)
    {
        let Some(function_name) = function.name() else {
            continue;
        };
        for decorator in function.decorators() {
            let Some(ast::Expr::Call(call)) = decorator.expr() else {
                continue;
            };
            let Some(ast::Expr::Attribute(attribute)) = call.callee() else {
                continue;
            };
            let identity =
                origin(file, &ast::Expr::Attribute(attribute.clone())).unwrap_or_default();
            if !identity.starts_with("flask.")
                || !matches!(
                    attribute.attr_text().as_str(),
                    "route" | "get" | "post" | "put" | "delete" | "patch"
                )
            {
                continue;
            }
            let mut name = keyword(&call, "endpoint")
                .and_then(string)
                .unwrap_or_else(|| function_name.text().into());
            if let Some(ast::Expr::Name(object)) = attribute.object() {
                if let Some(ids) = file.scopes.resolve(
                    file.scopes
                        .scope_at(attribute.syntax().text_range().start()),
                    &object.name_text(),
                ) {
                    if ids.len() == 1 {
                        if let Some(ast::Expr::Call(constructor)) = file
                            .scopes
                            .binding(ids[0])
                            .value
                            .and_then(|range| expression(file, range))
                        {
                            if constructor
                                .callee()
                                .and_then(|head| origin(file, &head))
                                .as_deref()
                                == Some("flask.Blueprint")
                            {
                                if let Some(prefix) =
                                    positional(&constructor).first().cloned().and_then(string)
                                {
                                    name = format!("{prefix}.{name}");
                                }
                            }
                        }
                    }
                }
            }
            out.push((name, function_name.text_range()));
        }
    }
    out
}

#[cfg(test)]
mod inherited_diagram_models {
    use super::*;
    use crate::module::ModuleResolver;
    #[test]
    fn inherited_family_uses_current_base_dependency_while_child_arc_stays_identical() {
        let dir = tempfile::TempDir::with_prefix("rustpy-diagram-models").unwrap();
        let base = dir.path().join("base.py");
        let child = dir.path().join("models.py");
        let base_text =
            "from django.db import models\nclass Base(models.Model):\n title=models.CharField()\n";
        let child_text = "from base import Base\nclass Child(Base):\n pass\n";
        std::fs::write(&base, base_text).unwrap();
        std::fs::write(&child, child_text).unwrap();
        let db = Analysis::new(ModuleResolver::new(
            vec![dir.path().to_path_buf()],
            vec![],
            (3, 13),
        ));
        db.set_file_text(&base, base_text);
        let id = db.set_file_text(&child, child_text);
        let captured = db.file(id).unwrap();
        assert!(models(&captured).is_empty());
        let live = models_in_analysis(&db, &captured);
        assert!(live
            .iter()
            .any(|m| m.class.name.as_str() == "Child" && m.family == Family::Django));
        db.set_file_text(&base, "class Base:\n pass\n");
        assert!(Arc::ptr_eq(&captured, &db.file(id).unwrap()));
        assert!(models_in_analysis(&db, &captured).is_empty());
    }
    #[test]
    fn same_file_multilevel_models_keep_declared_fields_without_duplicate_class_nodes() {
        let db = Analysis::new(ModuleResolver::new(vec![], vec![], (3, 13)));
        let id=db.set_file_text(std::path::Path::new("/project/models.py"),"from django.db import models as m\nclass Parent(m.Model):\n id=m.IntegerField()\nclass Child(Parent):\n parent=m.ForeignKey('Parent',on_delete=m.CASCADE)\nclass Leaf(Child):\n label=m.CharField()\n");
        let file = db.file(id).unwrap();
        let models = models_in_analysis(&db, &file);
        assert_eq!(models.len(), 3);
        assert_eq!(
            models
                .iter()
                .find(|m| m.class.name.as_str() == "Child")
                .unwrap()
                .fields[0]
                .related
                .as_deref(),
            Some("Parent")
        );
    }
}
#[cfg(all(test, unix))] // its only test and helper use Unix fixture paths
mod route_name_definitions {
    use super::*;
    use crate::module::ModuleResolver;
    use std::path::Path;
    fn target(files: &[(&str, &str)], path: &str, needle: &str) -> Option<String> {
        let db = Analysis::new(ModuleResolver::new(vec![], vec![], (3, 13)));
        for (file, text) in files {
            db.set_file_text(Path::new(file), text);
        }
        let file = db.file_for_path(Path::new(path)).unwrap();
        let at = file.text.find(needle).unwrap() + 2;
        definition(&db, &file, TextSize::new(at as u32))
            .first()
            .map(|location| {
                let source = db.file_for_path(&location.path).unwrap();
                let range = usize::from(location.range.start())..usize::from(location.range.end());
                format!("{}:{}", location.path.display(), &source.text[range])
            })
    }
    #[test]
    fn reverse_and_url_for_share_the_template_name_lookup() {
        let urls = "from django.urls import path\napp_name = 'blog'\nurlpatterns = [path('', view, name='index'), path('d/', view, name='detail')]\n";
        let views = "from django.urls import reverse, reverse_lazy\nreverse('blog:detail')\nreverse_lazy('index')\nreverse('shop:index')\nreverse('missing')\n";
        let files = [("/p/blog/urls.py", urls), ("/p/blog/views.py", views)];
        let found = |needle| target(&files, "/p/blog/views.py", needle);
        assert_eq!(
            found("'blog:detail'").as_deref(),
            Some("/p/blog/urls.py:'detail'")
        );
        assert_eq!(found("'index'").as_deref(), Some("/p/blog/urls.py:'index'"));
        // A namespace from an include() elsewhere still reaches the short name.
        assert_eq!(
            found("'shop:index'").as_deref(),
            Some("/p/blog/urls.py:'index'")
        );
        assert_eq!(found("'missing'"), None);

        let app = "from flask import Flask, Blueprint, url_for\napp = Flask(__name__)\nauth = Blueprint('auth', __name__)\n@auth.route('/login')\ndef login(): pass\n@app.route('/')\ndef index():\n    return url_for('auth.login') + url_for('index')\n";
        let files = [("/p/app.py", app)];
        let found = |needle| target(&files, "/p/app.py", needle);
        assert_eq!(found("'auth.login'").as_deref(), Some("/p/app.py:login"));
        assert_eq!(found("'index'").as_deref(), Some("/p/app.py:index"));
    }
}
