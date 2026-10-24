//! Endpoint parameters (path templates and FastAPI signatures) and JSON body
//! skeletons built from Pydantic models declared in the scanned sources.

use std::collections::HashMap;
use std::path::Path;

use rustpy_syntax::ast::{self, AstNode};

use super::{Endpoint, EndpointParam, ParamLocation};

/// One scanned class: base names and own `(name, annotation)` fields.
type RawClass = (Vec<String>, Vec<(String, String)>);

/// Pydantic-style models of the scanned sources: class name -> fields
/// (name, annotation text), inherited fields first.
#[derive(Default)]
pub(super) struct Models(HashMap<String, Vec<(String, String)>>);

impl Models {
    pub(super) fn collect(files: &[(&Path, &str)]) -> Self {
        // Raw classes of every file: name -> (base names, own fields).
        let mut raw: HashMap<String, RawClass> = HashMap::new();
        for (_, text) in files {
            if text.len() > 2 * 1024 * 1024
                || !(text.contains("pydantic")
                    || text.contains("sqlmodel")
                    || text.contains("BaseModel"))
            {
                continue;
            }
            let parse = rustpy_syntax::parse(text);
            for class in parse
                .syntax_node()
                .descendants()
                .filter_map(ast::ClassDef::cast)
                .take(4096)
            {
                let bases = class
                    .bases()
                    .iter()
                    .map(|b| last_segment(&b.syntax().text().to_string()).to_string())
                    .collect();
                let mut fields = Vec::new();
                if let Some(body) = class.body() {
                    for statement in body.statements().take(512) {
                        let ast::Stmt::AnnAssign(assign) = statement else {
                            continue;
                        };
                        let (Some(ast::Expr::Name(target)), Some(annotation)) =
                            (assign.target(), assign.annotation())
                        else {
                            continue;
                        };
                        let name = target.name_text();
                        let annotation = annotation.syntax().text().to_string();
                        if name == "model_config" || annotation.starts_with("ClassVar") {
                            continue;
                        }
                        fields.push((name, annotation));
                    }
                }
                raw.entry(class.name_text()).or_insert((bases, fields));
            }
        }
        let mut models: HashMap<String, Vec<(String, String)>> = HashMap::new();
        // Resolve inheritance to a fixpoint (bounded: chains are short).
        for _ in 0..8 {
            let mut changed = false;
            for (name, (bases, own)) in &raw {
                if models.contains_key(name) {
                    continue;
                }
                let roots = bases
                    .iter()
                    .any(|b| matches!(b.as_str(), "BaseModel" | "SQLModel" | "BaseSettings"));
                let parents: Vec<_> = bases.iter().filter_map(|b| models.get(b)).collect();
                if !roots && parents.is_empty() {
                    continue;
                }
                let mut fields: Vec<(String, String)> = Vec::new();
                for (field, annotation) in parents.into_iter().flatten().chain(own) {
                    fields.retain(|(f, _)| f != field);
                    fields.push((field.clone(), annotation.clone()));
                }
                models.insert(name.clone(), fields);
                changed = true;
            }
            if !changed {
                break;
            }
        }
        Self(models)
    }

    fn get(&self, annotation: &str) -> Option<&Vec<(String, String)>> {
        self.0.get(last_segment(&unwrap_optional(annotation)))
    }

    /// Pretty-printed JSON object with a placeholder per field; nested models
    /// are expanded one level and `{}` below that.
    pub(super) fn skeleton(&self, model: &str) -> Option<String> {
        let fields = self.get(model)?;
        Some(self.object(fields, 0))
    }

    fn object(&self, fields: &[(String, String)], depth: usize) -> String {
        if fields.is_empty() {
            return "{}".into();
        }
        let indent = "  ".repeat(depth + 1);
        let body = fields
            .iter()
            .take(256)
            .map(|(name, annotation)| {
                format!(
                    "{indent}{}: {}",
                    serde_json::Value::String(name.clone()),
                    self.placeholder(annotation, depth)
                )
            })
            .collect::<Vec<_>>()
            .join(",\n");
        format!("{{\n{body}\n{}}}", "  ".repeat(depth))
    }

    fn placeholder(&self, annotation: &str, depth: usize) -> String {
        let annotation = unwrap_optional(annotation);
        let head = annotation.split('[').next().unwrap_or("").trim();
        match last_segment(head) {
            "str" | "EmailStr" | "HttpUrl" | "AnyUrl" | "AnyHttpUrl" | "SecretStr" | "constr"
            | "UUID" | "UUID4" | "Path" | "bytes" => "\"string\"".into(),
            "int" | "conint" | "PositiveInt" | "NonNegativeInt" => "0".into(),
            "float" | "confloat" | "Decimal" | "condecimal" | "PositiveFloat" => "0.0".into(),
            "bool" | "StrictBool" => "false".into(),
            "datetime" => "\"2024-01-01T00:00:00\"".into(),
            "date" => "\"2024-01-01\"".into(),
            "time" => "\"00:00:00\"".into(),
            "list" | "List" | "set" | "Set" | "tuple" | "Tuple" | "Sequence" | "frozenset"
            | "conlist" => "[]".into(),
            "dict" | "Dict" | "Mapping" => "{}".into(),
            "Literal" => annotation
                .find('[')
                .and_then(|start| {
                    let inner = annotation[start + 1..].trim_end_matches(']');
                    inner
                        .split(',')
                        .next()
                        .map(|v| v.trim().replace('\'', "\""))
                })
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "null".into()),
            name => match self.0.get(name) {
                Some(fields) if depth == 0 => self.object(fields, depth + 1),
                Some(_) => "{}".into(),
                None => "null".into(),
            },
        }
    }
}

fn last_segment(text: &str) -> &str {
    text.trim().rsplit('.').next().unwrap_or("").trim()
}

/// `Optional[X]` / `X | None` / `Annotated[X, …]` -> `X`.
fn unwrap_optional(annotation: &str) -> String {
    let mut text = annotation
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();
    for _ in 0..4 {
        let stripped = [
            "Optional[",
            "typing.Optional[",
            "Annotated[",
            "typing.Annotated[",
        ]
        .iter()
        .find_map(|p| text.strip_prefix(p).and_then(|r| r.strip_suffix(']')))
        .map(|inner| top_level_first(inner).to_string());
        if let Some(inner) = stripped {
            text = inner;
            continue;
        }
        let parts: Vec<&str> = text.split('|').map(str::trim).collect();
        if parts.len() > 1 {
            if let Some(first) = parts.iter().find(|p| **p != "None") {
                text = first.to_string();
                continue;
            }
        }
        break;
    }
    text
}

/// First comma-separated element at bracket depth zero.
fn top_level_first(text: &str) -> &str {
    let mut depth = 0i32;
    for (i, c) in text.char_indices() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            ',' if depth == 0 => return text[..i].trim(),
            _ => {}
        }
    }
    text.trim()
}

/// Path parameters of a route template: FastAPI/Starlette/Pyramid `{id}` /
/// `{id:int}`, Flask/Django `<int:id>` / `<id>` and regex `(?P<id>…)` groups.
pub(super) fn path_params(path: &str) -> Vec<EndpointParam> {
    let mut out: Vec<EndpointParam> = Vec::new();
    let push = |out: &mut Vec<EndpointParam>, name: &str, annotation: &str| {
        let name = name.trim();
        if !name.is_empty()
            && name.chars().all(|c| c == '_' || c.is_alphanumeric())
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && !out.iter().any(|p| p.name == name)
        {
            out.push(EndpointParam {
                name: name.to_string(),
                location: ParamLocation::Path,
                annotation: annotation.trim().to_string(),
            });
        }
    };
    let mut rest = path;
    while let Some(start) = rest.find(['<', '{']) {
        let regex_group = rest[..start].ends_with("(?P");
        let (open, close) = if rest[start..].starts_with('<') {
            ('<', '>')
        } else {
            ('{', '}')
        };
        let after = &rest[start + open.len_utf8()..];
        let Some(end) = after.find(close) else { break };
        let inner = &after[..end];
        if regex_group {
            push(&mut out, inner, "str");
        } else if open == '<' {
            match inner.split_once(':') {
                Some((converter, name)) => push(&mut out, name, converter),
                None => push(&mut out, inner, "str"),
            }
        } else {
            match inner.split_once(':') {
                Some((name, converter)) => push(&mut out, name, converter),
                None => push(&mut out, inner, ""),
            }
        }
        rest = &after[end + 1..];
        if out.len() > 64 {
            break;
        }
    }
    out
}

const SKIPPED_ANNOTATIONS: &[&str] = &[
    "Request",
    "Response",
    "WebSocket",
    "BackgroundTasks",
    "HTTPConnection",
    "Session",
    "AsyncSession",
    "SecurityScopes",
];

/// Path/query/body parameters of a FastAPI handler signature, plus the JSON
/// body skeleton of its Pydantic body parameter(s).
pub(super) fn fastapi_signature(function: &ast::FuncDef, endpoint: &mut Endpoint, models: &Models) {
    let Some(params) = function.params() else {
        return;
    };
    let mut bodies: Vec<(String, String)> = Vec::new();
    for param in params.params().take(64) {
        let ast::AnyParam::Plain(param) = param else {
            continue;
        };
        let name = param.name_text();
        if matches!(name.as_str(), "self" | "cls") {
            continue;
        }
        let annotation_expr = param.annotation();
        let mut annotation = annotation_expr
            .as_ref()
            .map(|a| a.syntax().text().to_string())
            .unwrap_or_default();
        // `Annotated[T, Query()]` carries its marker in the metadata.
        let mut marker = param.default().and_then(|d| marker_of(&d));
        if let Some(ast::Expr::Subscript(subscript)) = &annotation_expr {
            if subscript
                .object()
                .is_some_and(|o| last_segment(&o.syntax().text().to_string()) == "Annotated")
            {
                if let Some(index) = subscript.index() {
                    // `Annotated[T, m]` indexes with a SLICE_LIST (or a tuple).
                    let mut items: Vec<ast::Expr> = match ast::Expr::cast(index.clone()) {
                        Some(ast::Expr::Tuple(t)) => t.elements().collect(),
                        Some(other) => vec![other],
                        None => index.children().filter_map(ast::Expr::cast).collect(),
                    };
                    if !items.is_empty() {
                        annotation = items.remove(0).syntax().text().to_string();
                    }
                    marker = marker.or_else(|| items.iter().find_map(marker_of));
                }
            }
        }
        if let Some(existing) = endpoint.params.iter_mut().find(|p| p.name == name) {
            if existing.location == ParamLocation::Path {
                if !annotation.is_empty() {
                    existing.annotation = annotation;
                }
                continue;
            }
        }
        let skipped_annotation =
            SKIPPED_ANNOTATIONS.contains(&last_segment(annotation.split('[').next().unwrap_or("")));
        let location = match marker.as_deref() {
            Some("Depends" | "Security" | "Header" | "Cookie" | "File" | "Form") => continue,
            _ if skipped_annotation => continue,
            Some("Path") => ParamLocation::Path,
            Some("Body") => ParamLocation::Body,
            Some("Query") => ParamLocation::Query,
            _ if models.get(&annotation).is_some() => ParamLocation::Body,
            _ => ParamLocation::Query,
        };
        if location == ParamLocation::Body {
            bodies.push((name.clone(), annotation.clone()));
        }
        endpoint.params.push(EndpointParam {
            name,
            location,
            annotation,
        });
    }
    endpoint.body = match bodies.as_slice() {
        [] => None,
        [(name, annotation)] => models.skeleton(annotation).or_else(|| {
            Some(format!(
                "{{\n  {}: null\n}}",
                serde_json::Value::String(name.clone())
            ))
        }),
        // Several body parameters are embedded by name.
        many => Some(format!(
            "{{\n{}\n}}",
            many.iter()
                .map(|(name, annotation)| {
                    let value = models
                        .get(annotation)
                        .map(|fields| models.object(fields, 1))
                        .unwrap_or_else(|| models.placeholder(annotation, 1));
                    format!("  {}: {value}", serde_json::Value::String(name.clone()))
                })
                .collect::<Vec<_>>()
                .join(",\n")
        )),
    };
}

fn marker_of(expr: &ast::Expr) -> Option<String> {
    let ast::Expr::Call(call) = expr else {
        return None;
    };
    let callee = call.callee()?.syntax().text().to_string();
    let name = last_segment(&callee);
    matches!(
        name,
        "Depends" | "Security" | "Header" | "Cookie" | "File" | "Form" | "Path" | "Body" | "Query"
    )
    .then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_templates_of_every_framework() {
        let names = |p: &str| {
            path_params(p)
                .into_iter()
                .map(|p| format!("{}:{}", p.name, p.annotation))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names("/items/<int:item_id>/<slug>"),
            ["item_id:int", "slug:str"]
        );
        assert_eq!(
            names("/users/{user_id}/{file_path:path}"),
            ["user_id:", "file_path:path"]
        );
        assert_eq!(names(r"^archive/(?P<year>[0-9]{4})/$"), ["year:str"]);
    }

    #[test]
    fn nested_model_skeleton_expands_one_level() {
        let src = "from pydantic import BaseModel\nclass Tag(BaseModel):\n    label: str\nclass Owner(BaseModel):\n    email: str\n    tag: Tag\nclass Base(BaseModel):\n    id: int\nclass Item(Base):\n    name: str\n    price: float\n    active: bool = True\n    tags: list[Tag] = []\n    owner: Optional[Owner] = None\n";
        let models = Models::collect(&[(Path::new("m.py"), src)]);
        assert_eq!(
            models.skeleton("Item").unwrap(),
            "{\n  \"id\": 0,\n  \"name\": \"string\",\n  \"price\": 0.0,\n  \"active\": false,\n  \"tags\": [],\n  \"owner\": {\n    \"email\": \"string\",\n    \"tag\": {}\n  }\n}"
        );
        assert!(models.skeleton("Missing").is_none());
    }
}
