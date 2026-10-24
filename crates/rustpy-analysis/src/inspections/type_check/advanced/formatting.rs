//! Literal format strings only. Dynamic strings and expanded argument packs do
//! not provide a finite arity/key contract and are left to runtime.
use super::*;
use std::collections::HashSet;
pub(super) fn node(inf: &Inferer<'_>, file: &Arc<FileInfo>, node: &SyntaxNode, sink: &mut Sink) {
    if let Some(binary) = ast::BinaryExpr::cast(node.clone()) {
        if binary.op().is_some_and(|o| o.kind() == K::PERCENT) {
            if let (Some(ast::Expr::String(pattern)), Some(value)) = (binary.lhs(), binary.rhs()) {
                if pattern.syntax().text().len() <= rustpy_core::TextSize::new(65_536) {
                    percent(inf, file, node, &pattern, &value, sink);
                }
            }
        }
    }
    if let Some(call) = ast::CallExpr::cast(node.clone()) {
        let Some(ast::Expr::Attribute(attribute)) = call.callee() else {
            return;
        };
        if !matches!(attribute.attr_text().as_str(), "format" | "format_map") {
            return;
        }
        let Some(ast::Expr::String(pattern)) = attribute.object() else {
            return;
        };
        if pattern.is_bytes() || pattern.syntax().text().len() > rustpy_core::TextSize::new(65_536)
        {
            return;
        }
        let Ok(text) = rustpy_syntax::string_literal::text(&pattern.raw_text()) else {
            return;
        };
        let Some(args) = call.arg_list() else { return };
        if args
            .args()
            .any(|a| matches!(a, ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_)))
        {
            return;
        }
        let positional: Vec<_> = args.positional().collect();
        let keywords: HashSet<_> = args.keywords().map(|k| k.name_text()).collect();
        let mapping = if attribute.attr_text() == "format_map" {
            positional
                .first()
                .and_then(|arg| match arg.clone().unparen() {
                    ast::Expr::Dict(dict) if dict.unpacks().next().is_none() => Some(
                        dict.items()
                            .filter_map(|item| item.key())
                            .filter_map(|key| match key.unparen() {
                                ast::Expr::String(s) => Some(s.value()),
                                _ => None,
                            })
                            .collect::<HashSet<_>>(),
                    ),
                    _ => None,
                })
        } else {
            None
        };
        let mut used_pos = HashSet::new();
        let mut used_names = HashSet::new();
        let mut automatic = 0usize;
        let mut numbered = false;
        let mut auto = false;
        let bytes = text.as_bytes();
        let mut i = 0usize;
        let mut fields = 0usize;
        let mut braces = 0usize;
        while i < bytes.len() {
            if fields > 256 {
                return;
            }
            match bytes[i] {
                b'{' if bytes.get(i + 1) == Some(&b'{') => {
                    i += 2;
                    continue;
                }
                b'}' if braces == 0 && bytes.get(i + 1) == Some(&b'}') => {
                    i += 2;
                    continue;
                }
                b'}' => {
                    if braces == 0 {
                        warn(
                            node,
                            "PyStringFormat",
                            "Unmatched '}' in format string",
                            sink,
                        );
                        return;
                    }
                    braces -= 1;
                    i += 1;
                }
                b'{' => {
                    braces += 1;
                    fields += 1;
                    let start = i + 1;
                    let mut end = start;
                    while end < bytes.len() && !matches!(bytes[end], b'!' | b':' | b'}' | b'{') {
                        end += 1;
                    }
                    if end == bytes.len() {
                        warn(
                            node,
                            "PyStringFormat",
                            "Unmatched '{' in format string",
                            sink,
                        );
                        return;
                    }
                    let field = &text[start..end];
                    let head = field.split(['.', '[']).next().unwrap_or("");
                    if head.is_empty() {
                        if attribute.attr_text() == "format_map" {
                            warn(
                                node,
                                "PyStringFormat",
                                "format_map() does not accept positional fields",
                                sink,
                            );
                        }
                        auto = true;
                        used_pos.insert(automatic);
                        if automatic >= positional.len() && attribute.attr_text() == "format" {
                            warn(
                                node,
                                "PyStringFormat",
                                format!(
                                    "Not enough format arguments for automatic field {automatic}"
                                ),
                                sink,
                            );
                        }
                        automatic += 1;
                    } else if let Ok(index) = head.parse::<usize>() {
                        numbered = true;
                        used_pos.insert(index);
                        if attribute.attr_text() == "format_map" {
                            warn(
                                node,
                                "PyStringFormat",
                                "format_map() does not accept positional fields",
                                sink,
                            );
                        } else if index >= positional.len() {
                            warn(
                                node,
                                "PyStringFormat",
                                format!("Format argument index {index} is out of range"),
                                sink,
                            );
                        }
                    } else {
                        used_names.insert(head.to_string());
                        let available =
                            mapping
                                .as_ref()
                                .map(|m| m.contains(head))
                                .unwrap_or_else(|| {
                                    attribute.attr_text() == "format_map" || keywords.contains(head)
                                });
                        if !available {
                            warn(
                                node,
                                "PyStringFormat",
                                format!("Missing format keyword '{head}'"),
                                sink,
                            );
                        }
                    }
                    if auto && numbered {
                        warn(
                            node,
                            "PyStringFormat",
                            "Cannot mix automatic and manual positional format numbering",
                            sink,
                        );
                        return;
                    }
                    // Continue through nested format fields; the closing braces are
                    // balanced separately, so width/precision placeholders also count.
                    i = end;
                    if bytes.get(i) == Some(&b'!') {
                        if !matches!(bytes.get(i + 1), Some(b's' | b'r' | b'a')) {
                            warn(
                                node,
                                "PyStringFormat",
                                "Invalid format conversion (expected !s, !r or !a)",
                                sink,
                            );
                        }
                        i += 1;
                    }
                }
                _ => {
                    i += 1;
                }
            }
        }
        if braces != 0 {
            warn(
                node,
                "PyStringFormat",
                "Unmatched '{' in format string",
                sink,
            );
            return;
        }
        if attribute.attr_text() == "format" {
            for index in 0..positional.len() {
                if !used_pos.contains(&index) {
                    sink.push(
                        node.text_range(),
                        Severity::WeakWarning,
                        "PyStringFormat",
                        format!("Format argument {index} is unused"),
                        Vec::new(),
                    );
                }
            }
            for name in keywords {
                if !used_names.contains(&name) {
                    sink.push(
                        node.text_range(),
                        Severity::WeakWarning,
                        "PyStringFormat",
                        format!("Format keyword '{name}' is unused"),
                        Vec::new(),
                    );
                }
            }
        }
    }
}
fn percent(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    node: &SyntaxNode,
    pattern: &ast::StringExpr,
    value: &ast::Expr,
    sink: &mut Sink,
) {
    let text = pattern.value();
    let mut specs = Vec::new();
    let mut keys = Vec::new();
    let chars: Vec<_> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' {
            i += 1;
            continue;
        }
        i += 1;
        if chars.get(i) == Some(&'%') {
            i += 1;
            continue;
        }
        let mut key = None;
        if chars.get(i) == Some(&'(') {
            i += 1;
            let start = i;
            while i < chars.len() && chars[i] != ')' {
                i += 1;
            }
            if i == chars.len() {
                warn(
                    node,
                    "PyStringFormat",
                    "Incomplete mapping key in percent format",
                    sink,
                );
                return;
            }
            key = Some(chars[start..i].iter().collect::<String>());
            i += 1;
        }
        while chars.get(i).is_some_and(|c| "#0- +".contains(*c)) {
            i += 1;
        }
        if chars.get(i) == Some(&'*') {
            specs.push('d');
            i += 1;
        } else {
            while chars.get(i).is_some_and(char::is_ascii_digit) {
                i += 1;
            }
        }
        if chars.get(i) == Some(&'.') {
            i += 1;
            if chars.get(i) == Some(&'*') {
                specs.push('d');
                i += 1;
            } else {
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
            }
        }
        while chars.get(i).is_some_and(|c| "hlL".contains(*c)) {
            i += 1;
        }
        let Some(conversion) = chars.get(i).copied() else {
            warn(
                node,
                "PyStringFormat",
                "Incomplete percent format specifier",
                sink,
            );
            return;
        };
        i += 1;
        if !"diouxXeEfFgGcrsa".contains(conversion) && !(pattern.is_bytes() && conversion == 'b') {
            warn(
                node,
                "PyStringFormat",
                format!("Unsupported format character '{conversion}'"),
                sink,
            );
            return;
        }
        if let Some(key) = key {
            keys.push((key, conversion));
        } else {
            specs.push(conversion);
        }
    }
    if !keys.is_empty() {
        let ast::Expr::Dict(dictionary) = value.clone().unparen() else {
            if closed(inf, &inf.type_of_expr(file, value))
                && slot(inf, &inf.type_of_expr(file, value), "__getitem__") == Some(false)
            {
                warn(
                    value.syntax(),
                    "PyStringFormat",
                    "Mapping format requires a mapping value",
                    sink,
                );
            }
            return;
        };
        if dictionary.unpacks().next().is_some() {
            return;
        }
        for (key, _) in keys {
            if !dictionary
                .items()
                .any(|item| matches!(item.key(),Some(ast::Expr::String(s)) if s.value()==key))
            {
                warn(
                    value.syntax(),
                    "PyStringFormat",
                    format!("Missing percent-format key '{key}'"),
                    sink,
                );
            }
        }
        return;
    }
    let values: Vec<_> = match value.clone().unparen() {
        ast::Expr::Tuple(tuple) => {
            if tuple.elements().any(|e| matches!(e, ast::Expr::Star(_))) {
                return;
            }
            tuple.elements().collect()
        }
        _ => vec![value.clone()],
    };
    if values.len() != specs.len() {
        warn(
            node,
            "PyStringFormat",
            format!(
                "Percent format expects {} argument(s), got {}",
                specs.len(),
                values.len()
            ),
            sink,
        );
        return;
    }
    for (value, conversion) in values.iter().zip(specs) {
        let ty = inf.type_of_expr(file, value);
        if !known(&ty) {
            continue;
        }
        let expected = if pattern.is_bytes() && matches!(conversion, 's' | 'b') {
            Some(inf.instance_of("bytes"))
        } else if "diouxX".contains(conversion) {
            Some(inf.instance_of("int"))
        } else if "eEfFgG".contains(conversion) {
            Some(inf.instance_of("float"))
        } else {
            None
        };
        if let Some(expected) = expected {
            if !inf.is_compatible(&ty, &expected)
                && !("diu".contains(conversion) && slot(inf, &ty, "__int__") == Some(true))
                && !("oxX".contains(conversion) && slot(inf, &ty, "__index__") == Some(true))
                && !("eEfFgG".contains(conversion) && slot(inf, &ty, "__float__") == Some(true))
            {
                warn(
                    value.syntax(),
                    "PyStringFormat",
                    format!("Format %{conversion} requires a numeric value, got '{ty}'"),
                    sink,
                );
            }
        }
    }
}
