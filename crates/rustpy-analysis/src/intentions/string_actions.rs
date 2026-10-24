//! Value-preserving literal edits and explicit formatting/path conversions.
use super::*;
use rustpy_semantic::{
    infer::Inferer,
    types::{LiteralValue, Type},
};
use rustpy_syntax::string_literal::{self, Decoded};
fn quoted(value: &str, ascii: bool) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || ascii && !c.is_ascii() => {
                let n = c as u32;
                out.push_str(&if n <= 255 {
                    format!("\\x{n:02x}")
                } else if n <= 65535 {
                    format!("\\u{n:04x}")
                } else {
                    format!("\\U{n:08x}")
                });
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn literal_value(expr: &ast::Expr) -> Option<Decoded> {
    match expr.clone().unparen() {
        ast::Expr::String(string) => string_literal::string(&string.raw_text()).ok(),
        ast::Expr::ConcatString(concat) => {
            let mut text = String::new();
            let mut bytes = Vec::new();
            let mut kind = None;
            for (index, part) in concat.parts().enumerate() {
                if index == 256 {
                    return None;
                }
                match literal_value(&part)? {
                    Decoded::Text(value, _) if kind != Some(false) => {
                        kind = Some(true);
                        text.push_str(&value);
                    }
                    Decoded::Bytes(value) if kind != Some(true) => {
                        kind = Some(false);
                        bytes.extend(value);
                    }
                    _ => return None,
                }
                if text.len().saturating_add(bytes.len()) > 1024 * 1024 {
                    return None;
                }
            }
            match kind {
                Some(true) => Some(Decoded::Text(text, None)),
                Some(false) => Some(Decoded::Bytes(bytes)),
                None => None,
            }
        }
        _ => None,
    }
}
fn text_value(expr: &ast::Expr) -> Option<String> {
    match literal_value(expr)? {
        Decoded::Text(value, _) => Some(value),
        _ => None,
    }
}
fn byte_literal(bytes: &[u8]) -> String {
    format!(
        "b\"{}\"",
        bytes
            .iter()
            .map(|b| match b {
                b'"' => "\\\"".into(),
                b'\\' => "\\\\".into(),
                32..=126 => (*b as char).to_string(),
                _ => format!("\\x{b:02x}"),
            })
            .collect::<String>()
    )
}
fn canonical_namespace(db: &Analysis, file: &FileInfo, root: &str, module: &str) -> bool {
    if file
        .scopes
        .scopes
        .iter()
        .any(|scope| !scope.star_imports.is_empty())
    {
        return false;
    }
    if db.module(module).is_some_and(|source| {
        db.resolver
            .roots
            .iter()
            .any(|path| source.path.starts_with(path))
    }) {
        return false;
    }
    let mut roots = vec![root.to_string()];
    for binding in file.scopes.bindings.iter().take(1025) {
        if let BindingKind::Import(info) = &binding.kind {
            if info.level == 0
                && (info.module == module
                    || module
                        .strip_prefix(info.module.as_str())
                        .is_some_and(|rest| rest.starts_with('.')))
                && info.name.is_none()
            {
                roots.push(binding.name.to_string());
            }
        }
    }
    if file.scopes.bindings.len() > 1024 {
        return false;
    }
    for _ in 0..16 {
        let before = roots.len();
        for binding in &file.scopes.bindings {
            if !matches!(binding.kind, BindingKind::Assignment) {
                continue;
            }
            if let Some(range) = binding.value {
                let value = slice(&file.text, range).trim();
                if roots.iter().any(|root| {
                    value == root.as_str()
                        || value
                            .strip_prefix(root.as_str())
                            .is_some_and(|suffix| suffix.starts_with('.'))
                }) && !roots.iter().any(|root| root == binding.name.as_str())
                {
                    roots.push(binding.name.to_string());
                }
            }
        }
        if roots.len() == before {
            break;
        }
    }
    for (count, node) in file.parse.syntax_node().descendants().enumerate() {
        if count == 16384 {
            return false;
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
            for (count, node) in target.syntax().descendants().enumerate() {
                if count == 4096 {
                    return false;
                }
                if let Some(attribute) = ast::AttributeExpr::cast(node) {
                    if attribute
                        .syntax()
                        .descendants()
                        .filter_map(ast::NameExpr::cast)
                        .any(|name| roots.iter().any(|root| name.name_text() == root.as_str()))
                    {
                        return false;
                    }
                }
            }
        }
    }
    true
}
fn action(out: &mut Vec<Intention>, range: TextRange, title: &str, value: String) {
    out.push(Intention {
        title: title.into(),
        edits: vec![(range, value)],
    })
}
fn builtin(file: &FileInfo, offset: TextSize, name: &str) -> bool {
    if file
        .scopes
        .scopes
        .iter()
        .any(|scope| !scope.star_imports.is_empty())
    {
        return false;
    }
    file.scopes
        .resolve(file.scopes.scope_at(offset), name)
        .is_none()
}
fn literal_actions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    out: &mut Vec<Intention>,
) {
    let root = file.parse.syntax_node();
    let Some(s) = string_at(&root, offset) else {
        return;
    };
    let raw = s.raw_text();
    let Some((prefix, _, _, _)) = string_parts(&raw) else {
        return;
    };
    let Ok(decoded) = string_literal::string(&raw) else {
        return;
    };
    if let Decoded::Text(value, _) = &decoded {
        if string_parts(&raw).is_some_and(|parts| parts.3) {
            action(
                out,
                s.text_range(),
                "Convert triple-quoted string to single",
                quoted(value, false),
            );
        }
        if prefix.to_ascii_lowercase().contains('r') {
            action(
                out,
                s.text_range(),
                "Convert to ordinary string",
                quoted(value, false),
            )
        } else if let Some(q) = ['"', '\''].into_iter().find(|q| !value.contains(*q)) {
            if !value.contains(['\n', '\r'])
                && value
                    .chars()
                    .rev()
                    .take_while(|c| *c == '\\')
                    .count()
                    .is_multiple_of(2)
            {
                action(
                    out,
                    s.text_range(),
                    "Convert to raw string",
                    format!("r{q}{value}{q}"),
                );
                if regex_argument(db, file, &s) {
                    action(
                        out,
                        s.text_range(),
                        "Add r prefix for regex",
                        format!("r{q}{value}{q}"),
                    );
                }
            }
        }
        let escaped = quoted(value, true);
        if escaped != raw {
            action(out, s.text_range(), "Escape string characters", escaped)
        }
        let unescaped = quoted(value, false);
        if unescaped != raw {
            action(out, s.text_range(), "Unescape string characters", unescaped)
        }
        let bytes = value
            .as_bytes()
            .iter()
            .map(|b| match b {
                b'"' => "\\\"".into(),
                b'\\' => "\\\\".into(),
                32..=126 => (*b as char).to_string(),
                _ => format!("\\x{b:02x}"),
            })
            .collect::<String>();
        action(
            out,
            s.text_range(),
            "Convert to UTF-8 bytes literal",
            format!("b\"{bytes}\""),
        );
    } else if let Decoded::Bytes(bytes) = &decoded {
        if let Ok(value) = std::str::from_utf8(bytes) {
            action(
                out,
                s.text_range(),
                "Convert bytes to str literal",
                quoted(value, false),
            );
        }
    }
    // Retain the established quote and split UI titles only when their edited
    // literal(s) decode to exactly the same value; escapes may cross boundaries.
    for intention in [
        convert_quotes(file, offset),
        convert_triple_quoted(file, offset),
        split_string(file, offset),
    ]
    .into_iter()
    .flatten()
    {
        let Some((_, replacement)) = intention.edits.first() else {
            continue;
        };
        let parsed = rustpy_syntax::parse_expression(replacement);
        let expr = parsed.syntax_node().descendants().find_map(ast::Expr::cast);
        let equal = if let Some(expr) = expr {
            match (&decoded, literal_value(&expr)) {
                (Decoded::Text(a, _), Some(Decoded::Text(b, _))) => a == &b,
                (Decoded::Bytes(a), Some(Decoded::Bytes(b))) => a == &b,
                _ => false,
            }
        } else {
            false
        };
        if equal {
            out.push(intention)
        }
    }
}
fn regex_argument(db: &Analysis, file: &FileInfo, string: &ast::StringExpr) -> bool {
    let Some(call) = string.syntax().ancestors().find_map(ast::CallExpr::cast) else {
        return false;
    };
    let (root, member) = match call.callee() {
        Some(ast::Expr::Attribute(attribute)) => {
            let Some(ast::Expr::Name(root)) = attribute.object() else {
                return false;
            };
            (root.name_text(), Some(attribute.attr_text()))
        }
        Some(ast::Expr::Name(root)) => (root.name_text(), None),
        _ => return false,
    };
    let Some(ids) = file
        .scopes
        .resolve(file.scopes.scope_at(call.text_range().start()), &root)
    else {
        return false;
    };
    if ids.len() != 1 {
        return false;
    }
    let BindingKind::Import(info) = &file.scopes.binding(ids[0]).kind else {
        return false;
    };
    let method = member.as_deref().or(info.name.as_deref());
    if info.module != "re"
        || info.level != 0
        || !matches!(
            method,
            Some(
                "compile"
                    | "match"
                    | "search"
                    | "fullmatch"
                    | "findall"
                    | "finditer"
                    | "split"
                    | "sub"
                    | "subn"
            )
        )
        || !canonical_namespace(db, file, &root, "re")
    {
        return false;
    }
    call.arg_list().is_some_and(|args| {
        args.positional()
            .next()
            .is_some_and(|expr| expr.text_range() == string.text_range())
            || args.keywords().any(|arg| {
                arg.name_text() == "pattern"
                    && arg
                        .value()
                        .is_some_and(|expr| expr.text_range() == string.text_range())
            })
    })
}
fn format_fstring(
    file: &Arc<FileInfo>,
    f: &ast::FStringExpr,
    source: &str,
    authority: TextSize,
    out: &mut Vec<Intention>,
) {
    if f.is_template() {
        return;
    }
    let raw = f.syntax().text().to_string();
    let Some((prefix, _, _, _)) = string_parts(&raw) else {
        return;
    };
    let quote_at = raw.find(['\'', '"']).unwrap();
    let quote = raw.as_bytes()[quote_at] as char;
    let width = if raw[quote_at..].starts_with(&quote.to_string().repeat(3)) {
        3
    } else {
        1
    };
    let content_start = quote_at + width;
    let content_end = raw.len() - width;
    let content = &raw[content_start..content_end];
    let other = if quote == '"' { '\'' } else { '"' };
    let quote_replacement = if !content.contains(other) {
        Some((
            if other == '"' {
                "Convert to double quotes"
            } else {
                "Convert to single quotes"
            },
            format!(
                "{prefix}{}{content}{}",
                other.to_string().repeat(width),
                other.to_string().repeat(width)
            ),
        ))
    } else {
        None
    };
    if let Some((title, replacement)) = quote_replacement {
        if rustpy_syntax::parse_expression(&replacement)
            .errors()
            .is_empty()
        {
            action(out, f.text_range(), title, replacement)
        }
    }
    if width == 1 && !content.ends_with(quote) {
        let replacement = format!(
            "{prefix}{}{content}{}",
            quote.to_string().repeat(3),
            quote.to_string().repeat(3)
        );
        if rustpy_syntax::parse_expression(&replacement)
            .errors()
            .is_empty()
        {
            action(
                out,
                f.text_range(),
                "Convert to triple-quoted string",
                replacement,
            )
        }
    } else if width == 3 && !content.contains(['\n', '\r', quote]) {
        let replacement = format!("{prefix}{quote}{content}{quote}");
        if rustpy_syntax::parse_expression(&replacement)
            .errors()
            .is_empty()
        {
            action(
                out,
                f.text_range(),
                "Convert triple-quoted string to single",
                replacement,
            )
        }
    }
    let replacements: Vec<_> = f.replacements().take(33).collect();
    if replacements.is_empty() || replacements.len() > 32 {
        return;
    }
    let mut cursor = content_start;
    let mut pattern = String::new();
    let mut percent = String::new();
    let mut args = Vec::new();
    for field in replacements {
        let Some(expr) = field.expr() else { return };
        let at = f.text_range().start();
        let start = usize::from(field.text_range().start() - at);
        let end = usize::from(field.text_range().end() - at);
        let literal = &raw[cursor..start];
        let literal_raw = format!(
            "{}{}{}{}",
            if prefix.to_ascii_lowercase().contains('r') {
                "r"
            } else {
                ""
            },
            quote.to_string().repeat(width),
            literal.replace("{{", "{").replace("}}", "}"),
            quote.to_string().repeat(width)
        );
        let Ok(Decoded::Text(mut literal, _)) = string_literal::string(&literal_raw) else {
            return;
        };
        let expr_text = slice(source, expr.text_range()).trim();
        let suffix = &raw[usize::from(expr.text_range().end() - at)..end - 1];
        let mut suffix = suffix.trim_start();
        let debug = suffix.starts_with('=');
        if debug {
            let after = suffix[1..].trim_start();
            let end = usize::from(expr.text_range().end() - at)
                + (raw[usize::from(expr.text_range().end() - at)..end - 1].len() - after.len());
            literal.push_str(&raw[start + 1..end]);
            suffix = after;
        }
        pattern.push_str(&escape_braces(&literal));
        percent.push_str(&literal.replace('%', "%%"));
        let conversion = if suffix.starts_with('!') {
            let Some(c) = suffix.chars().nth(1) else {
                return;
            };
            suffix = &suffix[2..];
            Some(c)
        } else if debug && suffix.is_empty() {
            Some('r')
        } else {
            None
        };
        let spec = if let Some(s) = suffix.strip_prefix(':') {
            s
        } else if suffix.is_empty() {
            ""
        } else {
            return;
        };
        if spec.contains(['{', '}']) {
            return;
        }
        let expr = match conversion {
            None => format!("({expr_text})"),
            Some(c) => {
                let function = match c {
                    'r' => "repr",
                    's' => "str",
                    'a' => "ascii",
                    _ => return,
                };
                if !builtin(file, authority, function) {
                    return;
                }
                format!("{function}(({expr_text}))")
            }
        };
        if !builtin(file, authority, "format") {
            return;
        }
        args.push(format!("format({expr}, {})", quoted(spec, false)));
        pattern.push_str("{}");
        percent.push_str("%s");
        cursor = end;
    }
    let tail = &raw[cursor..content_end];
    let tail_raw = format!(
        "{}{}{}{}",
        if prefix.to_ascii_lowercase().contains('r') {
            "r"
        } else {
            ""
        },
        quote.to_string().repeat(width),
        tail.replace("{{", "{").replace("}}", "}"),
        quote.to_string().repeat(width)
    );
    let Ok(Decoded::Text(tail, _)) = string_literal::string(&tail_raw) else {
        return;
    };
    pattern.push_str(&escape_braces(&tail));
    percent.push_str(&tail.replace('%', "%%"));
    action(
        out,
        f.text_range(),
        "Convert f-string to str.format",
        format!("{}.format({})", quoted(&pattern, false), args.join(", ")),
    );
    action(
        out,
        f.text_range(),
        "Convert f-string to percent formatting",
        format!("{} % ({},)", quoted(&percent, false), args.join(", ")),
    );
}
fn concatenate(expr: ast::Expr, parts: &mut Vec<ast::Expr>) -> bool {
    if parts.len() > 32 {
        return false;
    }
    if let ast::Expr::Binary(binary) = expr.clone().unparen() {
        if binary.op().is_some_and(|op| op.kind() == SyntaxKind::PLUS) {
            let (Some(left), Some(right)) = (binary.lhs(), binary.rhs()) else {
                return false;
            };
            return concatenate(left, parts) && concatenate(right, parts);
        }
    }
    parts.push(expr);
    true
}
fn string_type(inf: &Inferer<'_>, file: &Arc<FileInfo>, expr: &ast::Expr) -> bool {
    match inf.type_of_expr(file, expr) {
        Type::Literal(LiteralValue::Str(_)) => true,
        Type::Instance(class, _) => inf
            .builtin_class("str")
            .is_some_and(|builtin| builtin == class),
        _ => false,
    }
}
fn concatenate_actions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    expr: ast::Expr,
    out: &mut Vec<Intention>,
) {
    let mut parts = Vec::new();
    if !concatenate(expr.clone(), &mut parts) || parts.len() < 2 {
        return;
    }
    let inf = Inferer::new(db);
    if !parts.iter().all(|part| string_type(&inf, file, part)) {
        return;
    }
    if let Some(values) = parts.iter().map(text_value).collect::<Option<Vec<_>>>() {
        action(
            out,
            expr.text_range(),
            "Join string literals",
            quoted(&values.concat(), false),
        );
    }
    let mut content = String::new();
    for part in &parts {
        if let Some(value) = text_value(part) {
            content.push_str(&escape_braces(&value))
        } else {
            let text = slice(&file.text, part.text_range());
            if text.contains(['"', '\\', '\n']) {
                return;
            }
            content.push_str(&format!("{{{text}}}"));
        }
    }
    // Encode literal text without escaping field braces, then add the f prefix.
    action(
        out,
        expr.text_range(),
        "Replace concatenation with f-string",
        format!("f{}", quoted(&content, false)),
    );
    action(
        out,
        expr.text_range(),
        "Replace concatenation with join",
        format!(
            "\"\".join(({}))",
            parts
                .iter()
                .map(|part| slice(&file.text, part.text_range()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
}
pub fn at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Intention> {
    let mut out = Vec::new();
    literal_actions(db, file, offset, &mut out);
    let root = file.parse.syntax_node();
    let Some(token) = token_at(&root, offset) else {
        return out;
    };
    if let Some(concat) = token
        .parent_ancestors()
        .find_map(ast::ConcatStringExpr::cast)
    {
        let replacement = match literal_value(&ast::Expr::ConcatString(concat.clone())) {
            Some(Decoded::Text(value, _)) => Some(quoted(&value, false)),
            Some(Decoded::Bytes(value)) => Some(byte_literal(&value)),
            _ => None,
        };
        if let Some(value) = replacement {
            action(&mut out, concat.text_range(), "Join string literals", value)
        }
    }
    if let Some(f) = token.parent_ancestors().find_map(ast::FStringExpr::cast) {
        format_fstring(file, &f, &file.text, f.text_range().start(), &mut out)
    }
    if let Some(binary) = token
        .parent_ancestors()
        .filter_map(ast::BinaryExpr::cast)
        .filter(|binary| binary.op().is_some_and(|op| op.kind() == SyntaxKind::PLUS))
        .last()
    {
        concatenate_actions(db, file, ast::Expr::Binary(binary), &mut out)
    }
    if let Some(call) = token.parent_ancestors().find_map(ast::CallExpr::cast) {
        if let Some(ast::Expr::Name(name)) = call.callee() {
            if name.name_text() == "str" && builtin(file, call.text_range().start(), "str") {
                if let Some(args) = call.arg_list() {
                    let args: Vec<_> = args.args().collect();
                    if args.is_empty() {
                        action(
                            &mut out,
                            call.text_range(),
                            "Convert str call to f-string",
                            "f\"\"".into(),
                        );
                    }
                    if args.len() == 1 {
                        if let ast::AnyArg::Positional(arg) = &args[0] {
                            if let Some(expr) = arg.expr() {
                                let text = slice(&file.text, expr.text_range());
                                if !text.contains(['"', '\\', '\n']) {
                                    action(
                                        &mut out,
                                        call.text_range(),
                                        "Convert str call to f-string",
                                        format!("f\"{{{text}!s}}\""),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        pathlib(db, file, &call, &mut out);
    }
    if let Some(intention) = convert_to_fstring(file, offset) {
        if let Some((range, value)) = intention.edits.first() {
            let parsed = rustpy_syntax::parse_expression(value);
            if let Some(f) = parsed
                .syntax_node()
                .descendants()
                .find_map(ast::FStringExpr::cast)
            {
                let mut choices = Vec::new();
                format_fstring(file, &f, value, range.start(), &mut choices);
                if let Some(mut choice) = choices
                    .into_iter()
                    .find(|choice| choice.title == "Convert f-string to str.format")
                {
                    choice.title = "Use str.format".into();
                    choice.edits[0].0 = *range;
                    out.push(choice);
                }
            }
        }
    }
    let line = line_start(&file.text, usize::from(offset));
    let end = file.text[line..]
        .find('\n')
        .map_or(file.text.len(), |n| line + n);
    let suppressed = root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| {
            token.kind() == SyntaxKind::COMMENT
                && usize::from(token.text_range().start()) >= line
                && usize::from(token.text_range().start()) <= end
                && token.text().contains("noqa")
        });
    if !suppressed {
        action(
            &mut out,
            TextRange::empty(TextSize::new(end as u32)),
            "Insert # noqa",
            "  # noqa".into(),
        );
    }
    out
}
fn pathlib(db: &Analysis, file: &Arc<FileInfo>, call: &ast::CallExpr, out: &mut Vec<Intention>) {
    let (root, mut method, qualified) = match call.callee() {
        Some(ast::Expr::Attribute(method)) => match method.object() {
            Some(ast::Expr::Attribute(path)) if path.attr_text() == "path" => {
                let Some(ast::Expr::Name(root)) = path.object() else {
                    return;
                };
                (root.name_text(), method.attr_text(), true)
            }
            Some(ast::Expr::Name(root)) => (root.name_text(), method.attr_text(), false),
            _ => return,
        },
        Some(ast::Expr::Name(root)) => (root.name_text(), String::new(), false),
        _ => return,
    };
    let scope = file.scopes.scope_at(call.text_range().start());
    let Some(ids) = file.scopes.resolve(scope, &root) else {
        return;
    };
    if ids.len() != 1 {
        return;
    }
    let BindingKind::Import(info) = &file.scopes.binding(ids[0]).kind else {
        return;
    };
    if info.level != 0 || file.scopes.binding(ids[0]).range.start() > call.text_range().start() {
        return;
    }
    let valid = if qualified {
        info.module == "os" && info.name.is_none()
    } else if method.is_empty() {
        if info.module == "os.path" {
            if let Some(name) = &info.name {
                method = name.to_string();
                true
            } else {
                false
            }
        } else {
            false
        }
    } else {
        info.module == "os.path" && info.name.is_none()
            || info.module == "os" && info.name.as_deref() == Some("path")
    };
    if !valid
        || !canonical_namespace(
            db,
            file,
            &root,
            if info.module == "os.path" {
                "os.path"
            } else {
                "os"
            },
        )
    {
        return;
    }
    let mut path_name = None;
    for binding in &file.scopes.bindings {
        if matches!(&binding.kind,BindingKind::Import(info) if info.module=="pathlib"&&info.name.as_deref()==Some("Path")&&info.level==0)
            && file
                .scopes
                .resolve(scope, binding.name.as_str())
                .is_some_and(|ids| {
                    ids.len() == 1 && file.scopes.binding(ids[0]).range == binding.range
                })
        {
            path_name = Some(binding.name.to_string());
            break;
        }
    }
    let needs_import = path_name.is_none();
    let path_name = if let Some(name) = path_name {
        name
    } else if builtin(file, call.text_range().start(), "Path") {
        "Path".into()
    } else {
        return;
    };
    let Some(args) = call.arg_list() else { return };
    let mut expressions = Vec::new();
    for (count, arg) in args.args().enumerate() {
        if count == 32 {
            return;
        }
        let ast::AnyArg::Positional(arg) = arg else {
            return;
        };
        let Some(expr) = arg.expr() else { return };
        expressions.push(slice(&file.text, expr.text_range()).to_string());
    }
    if expressions.is_empty() {
        return;
    }
    let replacement = match method.as_str() {
        "join" => format!(
            "{path_name}({}).joinpath({})",
            expressions[0],
            expressions[1..].join(", ")
        ),
        "basename" if expressions.len() == 1 => format!("{path_name}({}).name", expressions[0]),
        "dirname" if expressions.len() == 1 => format!("{path_name}({}).parent", expressions[0]),
        _ => return,
    };
    let mut edits = vec![(call.text_range(), replacement)];
    if needs_import {
        edits.push((
            TextRange::empty(rustpy_semantic::import_placement::import_insert_offset(
                file,
            )),
            "from pathlib import Path\n".into(),
        ));
    }
    out.push(Intention {
        title: "Convert to pathlib".into(),
        edits,
    });
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
    fn intention(source: &str, needle: &str, title: &str) -> Option<Intention> {
        let (db, file) = setup(source);
        crate::intentions::intentions_at(
            &db,
            &file,
            TextSize::new(source.find(needle).unwrap() as u32),
        )
        .into_iter()
        .find(|i| i.title == title)
    }
    fn converted(source: &str, needle: &str, title: &str) -> String {
        let intention =
            intention(source, needle, title).unwrap_or_else(|| panic!("{title} unavailable"));
        let edited = apply_edits(source, &intention.edits);
        assert!(
            rustpy_syntax::parse(&edited).errors().is_empty(),
            "invalid edit: {edited}"
        );
        edited
    }
    fn decoded_assignment(source: &str) -> Decoded {
        let parsed = rustpy_syntax::parse(source);
        let assignment = parsed
            .syntax_node()
            .descendants()
            .find_map(ast::AssignStmt::cast)
            .unwrap();
        literal_value(&assignment.value().unwrap()).unwrap()
    }
    #[test]
    fn raw_ordinary_escape_and_unicode_edits_keep_exact_values() {
        let source = "value = 'π\\nC:\\\\tmp'\n";
        for title in [
            "Escape string characters",
            "Unescape string characters",
            "Convert to double quotes",
        ] {
            let edited = converted(source, "π", title);
            assert_eq!(decoded_assignment(&edited), decoded_assignment(source));
        }
        let source = "value = 'C:\\\\tmp'\n";
        let raw = converted(source, "tmp", "Convert to raw string");
        assert!(raw.contains("r\"C:\\tmp\""));
        assert_eq!(decoded_assignment(source), decoded_assignment(&raw));
        let ordinary = converted(&raw, "tmp", "Convert to ordinary string");
        assert_eq!(decoded_assignment(source), decoded_assignment(&ordinary));
        assert!(intention("value='end\\\\'\n", "end", "Convert to raw string").is_none());
    }
    #[test]
    fn triple_and_quote_changes_preserve_multiline_content() {
        let source = "value = '''first\nsecond \"quoted\" line'''\n";
        let edited = converted(source, "first", "Convert triple-quoted string to single");
        assert_eq!(decoded_assignment(source), decoded_assignment(&edited));
        assert!(edited.contains("\\n"));
        let source = "value = 'hello'\n";
        let edited = converted(source, "hello", "Convert to triple-quoted string");
        assert_eq!(decoded_assignment(source), decoded_assignment(&edited));
        let edited = converted(
            "value=f'hello {name}'\n",
            "hello",
            "Convert to double quotes",
        );
        assert!(edited.contains("f\"hello {name}\""));
    }
    #[test]
    fn joining_and_splitting_do_not_merge_escape_boundaries() {
        let source = "value = '\\1' '23'\n";
        let edited = converted(source, "23", "Join string literals");
        assert_eq!(decoded_assignment(source), decoded_assignment(&edited));
        let source = "value = 'hello world'\n";
        let (db, file) = setup(source);
        let caret = TextSize::new(source.find(" world").unwrap() as u32);
        let split = at(&db, &file, caret)
            .into_iter()
            .find(|i| i.title == "Split string at caret")
            .unwrap();
        let edited = apply_edits(source, &split.edits);
        assert_eq!(decoded_assignment(source), decoded_assignment(&edited));
        let source = "value='\\u1234'\n";
        let (db, file) = setup(source);
        let offset = TextSize::new(source.find("1234").unwrap() as u32 + 1);
        assert!(!at(&db, &file, offset)
            .iter()
            .any(|i| i.title == "Split string at caret"));
        let source = "value=b'one' b'two'\n";
        let edited = converted(source, "two", "Join string literals");
        assert_eq!(decoded_assignment(source), decoded_assignment(&edited));
    }
    #[test]
    fn bytes_and_text_use_explicit_utf8_and_refuse_invalid_utf8() {
        let source = "value='π'\n";
        let bytes = converted(source, "π", "Convert to UTF-8 bytes literal");
        assert_eq!(
            decoded_assignment(&bytes),
            Decoded::Bytes("π".as_bytes().to_vec())
        );
        let string = converted(&bytes, "xcf", "Convert bytes to str literal");
        assert_eq!(decoded_assignment(&string), decoded_assignment(source));
        assert!(intention("value=b'\\xff'\n", "xff", "Convert bytes to str literal").is_none());
    }
    #[test]
    fn concatenation_and_str_routes_require_known_string_or_canonical_builtin() {
        let source = "name: str = 'π'\nvalue = 'hello ' + name + '!'\n";
        assert!(
            converted(source, "hello", "Replace concatenation with f-string")
                .contains("f\"hello {name}!\"")
        );
        assert!(
            converted(source, "hello", "Replace concatenation with join")
                .contains("\"\".join(('hello ', name, '!'))")
        );
        assert!(intention(
            "value='hello'+unknown\n",
            "hello",
            "Replace concatenation with f-string"
        )
        .is_none());
        assert!(
            converted("value=str(number)\n", "str", "Convert str call to f-string")
                .contains("f\"{number!s}\"")
        );
        assert!(intention(
            "def str(x): return 1\nvalue=str(number)\n",
            "str(number)",
            "Convert str call to f-string"
        )
        .is_none());
    }
    #[test]
    fn fstring_back_conversions_preserve_conversion_order_and_specs() {
        let source = "value=f'hello {first()!r:>10} {second()}'\n";
        let formatted = converted(source, "hello", "Convert f-string to str.format");
        assert!(formatted
            .contains(".format(format(repr((first())), \">10\"), format((second()), \"\"))"));
        let percent = converted(source, "hello", "Convert f-string to percent formatting");
        assert!(percent.contains("% (format(repr((first())), \">10\"), format((second()), \"\"),)"));
        assert!(intention(
            "format=lambda x,s:x\nvalue=f'{name}'\n",
            "{name}",
            "Convert f-string to str.format"
        )
        .is_none());
        assert!(intention(
            "value=f'{name:{width}}'\n",
            "{name",
            "Convert f-string to str.format"
        )
        .is_none());
    }
    #[test]
    fn percent_and_format_forward_routes_and_use_format_are_wired() {
        assert!(
            converted("value='hello %s' % name\n", "hello", "Convert to f-string")
                .contains("f'hello {name!s}'")
        );
        assert!(converted(
            "value='hello {}'.format(name)\n",
            "hello",
            "Convert to f-string"
        )
        .contains("f'hello {name}'"));
        let edited = converted("value='hello %s' % name\n", "hello", "Use str.format");
        assert!(edited.contains(".format("));
        assert!(!edited.contains(" % name"));
    }
    #[test]
    fn regex_raw_prefix_uses_actual_import_and_visible_namespace_authority() {
        let source = "import re\npattern=re.compile('\\d+')\n";
        let edited = converted(source, "d+", "Add r prefix for regex");
        assert!(edited.contains("r\"\\d+\""));
        assert!(intention(
            "class re:\n    compile=print\nvalue=re.compile('\\d+')\n",
            "d+",
            "Add r prefix for regex"
        )
        .is_none());
        assert!(intention(
            "import re\nre.compile=print\nvalue=re.compile('\\d+')\n",
            "d+",
            "Add r prefix for regex"
        )
        .is_none());
    }
    #[test]
    fn pathlib_and_noqa_apply_actual_intention_edits_without_duplicate_imports() {
        let edited = converted(
            "import os\nvalue=os.path.join('a',name)\n",
            "join",
            "Convert to pathlib",
        );
        assert!(edited.contains("from pathlib import Path\n"));
        assert!(edited.contains("Path('a').joinpath(name)"));
        assert!(intention(
            "import os\nos.path.join=print\nvalue=os.path.join('a',name)\n",
            "value=",
            "Convert to pathlib"
        )
        .is_none());
        let edited = converted("value = 'π'\n", "π", "Insert # noqa");
        assert_eq!(edited, "value = 'π'  # noqa\n");
        assert!(intention(&edited, "π", "Insert # noqa").is_none());
    }
    #[test]
    fn existing_path_import_and_regex_function_aliases_are_reused() {
        let source="from pathlib import Path as FilePath\nfrom os.path import join as combine\nvalue=combine('a',name)\n";
        let edited = converted(source, "combine('a'", "Convert to pathlib");
        assert!(edited.contains("FilePath('a').joinpath(name)"));
        assert_eq!(edited.matches("from pathlib import").count(), 1);
        let source = "from re import compile as regex\nvalue=regex(pattern='\\d+')\n";
        assert!(converted(source, "d+", "Add r prefix for regex").contains("r\"\\d+\""));
        assert!(intention(
            "import re\nfrom re import compile as regex\nre.compile=print\nvalue=regex('\\d+')\n",
            "d+",
            "Add r prefix for regex"
        )
        .is_none());
        assert!(converted("value='# noqa'\n", "noqa", "Insert # noqa").ends_with("  # noqa\n"));
    }
    #[test]
    fn debug_formatting_keeps_whitespace_and_does_not_invent_repr_for_specs() {
        let edited = converted(
            "value=f'{name = :>10}'\n",
            "{name",
            "Convert f-string to str.format",
        );
        assert!(edited.contains("name = {}"));
        assert!(edited.contains("format((name), \">10\")"));
        assert!(!edited.contains("repr("));
        let edited = converted(
            "value=f'{name = }'\n",
            "{name",
            "Convert f-string to str.format",
        );
        assert!(edited.contains("name = {}"));
        assert!(edited.contains("repr((name))"));
    }
}
