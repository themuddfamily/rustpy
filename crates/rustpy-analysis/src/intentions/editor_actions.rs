//! Editor-level context actions. Text transformations remain atomic intentions;
//! dialogs and project-wide refactorings are typed requests owned by the IDE.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refactoring {
    Variable,
    Constant,
    Field,
    Parameter,
    Method,
    Inline,
}
#[derive(Clone, Debug)]
pub enum EditorRequest {
    Regex(String),
    Copy(String),
    OpenUrl(String),
    Refactor(Refactoring),
    ConvertPackage,
    ConvertModule,
    CreateTest,
    NavigateTest,
    Color { range: TextRange, rgb: [u8; 3] },
}
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    None,
    Tuple(Vec<Value>),
}
impl Value {
    fn render(&self) -> String {
        match self {
            Self::Int(n) => n.to_string(),
            Self::Float(n) => {
                let mut s = n.to_string();
                if !s.contains(['.', 'e', 'E']) {
                    s.push_str(".0")
                }
                s
            }
            Self::Bool(b) => if *b { "True" } else { "False" }.into(),
            Self::None => "None".into(),
            Self::String(s) => quote(s),
            Self::Tuple(v) => format!(
                "({}{})",
                v.iter().map(Self::render).collect::<Vec<_>>().join(", "),
                if v.len() == 1 { "," } else { "" }
            ),
        }
    }
}
fn quote(text: &str) -> String {
    let mut out = String::from("'");
    for c in text.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}
fn constant(expr: ast::Expr, budget: &mut usize, depth: usize) -> Option<Value> {
    if depth > 32 || *budget == 0 {
        return None;
    }
    *budget -= 1;
    match expr.unparen() {
        ast::Expr::String(string) => {
            if string.is_bytes() || u32::from(string.text_range().len()) > 65536 {
                return None;
            }
            let value = string.value();
            (value.len() <= 65536).then_some(Value::String(value))
        }
        ast::Expr::Literal(literal) => {
            let text = literal.syntax().text().to_string().replace('_', "");
            Some(match text.as_str() {
                "True" => Value::Bool(true),
                "False" => Value::Bool(false),
                "None" => Value::None,
                _ => {
                    let integer = if text.starts_with("0x") || text.starts_with("0X") {
                        i64::from_str_radix(&text[2..], 16)
                    } else if text.starts_with("0o") || text.starts_with("0O") {
                        i64::from_str_radix(&text[2..], 8)
                    } else if text.starts_with("0b") || text.starts_with("0B") {
                        i64::from_str_radix(&text[2..], 2)
                    } else {
                        text.parse::<i64>()
                    };
                    if let Ok(n) = integer {
                        Value::Int(n)
                    } else {
                        if text.starts_with('0')
                            && text.get(1..2).is_some_and(|prefix| {
                                matches!(prefix, "x" | "X" | "o" | "O" | "b" | "B")
                            })
                            || !text.contains(['.', 'e', 'E'])
                        {
                            return None;
                        }
                        let n = text.parse::<f64>().ok()?;
                        if !n.is_finite() {
                            return None;
                        }
                        Value::Float(n)
                    }
                }
            })
        }
        ast::Expr::Tuple(tuple) => {
            let mut values = Vec::new();
            for expr in tuple.elements() {
                if values.len() == 64 {
                    return None;
                }
                values.push(constant(expr, budget, depth + 1)?)
            }
            Some(Value::Tuple(values))
        }
        ast::Expr::Unary(unary) => {
            let value = constant(unary.operand()?, budget, depth + 1)?;
            let op = unary.syntax().first_token()?.text().to_string();
            match (value, op.as_str()) {
                (Value::Int(n), "-") => n.checked_neg().map(Value::Int),
                (Value::Int(n), "+") => Some(Value::Int(n)),
                (Value::Int(n), "~") => Some(Value::Int(!n)),
                (Value::Float(n), "-") => Some(Value::Float(-n)),
                (Value::Float(n), "+") => Some(Value::Float(n)),
                _ => None,
            }
        }
        ast::Expr::Binary(binary) => {
            let lhs = constant(binary.lhs()?, budget, depth + 1)?;
            let rhs = constant(binary.rhs()?, budget, depth + 1)?;
            let op = binary.op()?.text().to_string();
            match (lhs, rhs) {
                (Value::Int(a), Value::Int(b)) => match op.as_str() {
                    "+" => a.checked_add(b),
                    "-" => a.checked_sub(b),
                    "*" => a.checked_mul(b),
                    "//" if b != 0 => {
                        let q = a.checked_div(b)?;
                        let r = a.checked_rem(b)?;
                        if r != 0 && (r > 0) != (b > 0) {
                            q.checked_sub(1)
                        } else {
                            Some(q)
                        }
                    }
                    "%" if b != 0 => {
                        let r = a.checked_rem(b)?;
                        if r != 0 && (r > 0) != (b > 0) {
                            r.checked_add(b)
                        } else {
                            Some(r)
                        }
                    }
                    "**" if (0..=63).contains(&b) => a.checked_pow(b as u32),
                    "&" => Some(a & b),
                    "|" => Some(a | b),
                    "^" => Some(a ^ b),
                    "<<" if (0..=62).contains(&b) => a.checked_mul(1i64.checked_shl(b as u32)?),
                    ">>" if (0..=63).contains(&b) => Some(a >> b),
                    _ => return None,
                }
                .map(Value::Int),
                (Value::String(a), Value::String(b))
                    if op == "+" && a.len().checked_add(b.len())? <= 65536 =>
                {
                    Some(Value::String(a + &b))
                }
                (Value::String(a), Value::Int(n)) | (Value::Int(n), Value::String(a))
                    if op == "*" =>
                {
                    let n = n.max(0) as usize;
                    if n > 65536 || a.len().checked_mul(n)? > 65536 {
                        return None;
                    }
                    Some(Value::String(a.repeat(n)))
                }
                (a, b) => {
                    let num = |v: Value| match v {
                        Value::Int(n) => Some(n as f64),
                        Value::Float(n) => Some(n),
                        _ => None,
                    };
                    let a = num(a)?;
                    let b = num(b)?;
                    let n = match op.as_str() {
                        "+" => a + b,
                        "-" => a - b,
                        "*" => a * b,
                        "/" if b != 0.0 => a / b,
                        _ => return None,
                    };
                    n.is_finite().then_some(Value::Float(n))
                }
            }
        }
        _ => None,
    }
}
fn evaluated(file: &FileInfo, offset: TextSize) -> Option<(ast::Expr, Value)> {
    let token = token_at(&file.parse.syntax_node(), offset)?;
    let mut result = None;
    for expression in token.parent_ancestors().filter_map(ast::Expr::cast) {
        if let Some(value) = constant(expression.clone(), &mut 256, 0) {
            result = Some((expression, value))
        } else {
            break;
        }
    }
    result
}
fn comma_actions(file: &FileInfo, offset: TextSize) -> Vec<Intention> {
    let root = file.parse.syntax_node();
    let Some(token) = token_at(&root, offset) else {
        return vec![];
    };
    let mut out = Vec::new();
    for parent in token.parent_ancestors() {
        if !matches!(
            parent.kind(),
            SyntaxKind::ARG_LIST
                | SyntaxKind::PARAM_LIST
                | SyntaxKind::TUPLE_EXPR
                | SyntaxKind::LIST_EXPR
                | SyntaxKind::SET_EXPR
                | SyntaxKind::DICT_EXPR
                | SyntaxKind::IMPORT_STMT
                | SyntaxKind::IMPORT_FROM_STMT
        ) {
            continue;
        }
        let nodes: Vec<_> = parent
            .children()
            .filter(|node| {
                ast::Expr::can_cast(node.kind())
                    || ast::AnyArg::can_cast(node.kind())
                    || ast::AnyParam::can_cast(node.kind())
                    || node.kind() == SyntaxKind::DICT_ITEM
                    || node.kind() == SyntaxKind::IMPORT_ALIAS
            })
            .take(257)
            .collect();
        if nodes.len() > 256 {
            return vec![];
        }
        for pair in nodes.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            let gap = TextRange::new(a.text_range().end(), b.text_range().start());
            let separator = slice(&file.text, gap);
            if !separator.contains(',') || separator.contains('#') {
                continue;
            }
            let flip = gap.contains_inclusive(offset);
            let left = b.text_range().contains_inclusive(offset);
            let right = a.text_range().contains_inclusive(offset);
            if !flip && !left && !right {
                continue;
            }
            let range = TextRange::new(a.text_range().start(), b.text_range().end());
            let replacement = format!("{}{}{}", b.text(), separator, a.text());
            let edits = vec![(range, replacement)];
            let candidate = apply_edits(&file.text, &edits);
            if !rustpy_syntax::parse(&candidate).errors().is_empty() {
                continue;
            }
            if flip {
                out.push(Intention {
                    title: "Flip ','".into(),
                    edits: edits.clone(),
                })
            }
            if left {
                out.push(Intention {
                    title: "Move element left".into(),
                    edits: edits.clone(),
                })
            }
            if right {
                out.push(Intention {
                    title: "Move element right".into(),
                    edits,
                })
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}
pub fn at(_db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Intention> {
    let mut out = comma_actions(file, offset);
    if let Some((expression, value)) = evaluated(file, offset) {
        let result = value.render();
        if expression.syntax().text() != result.as_str() {
            out.push(Intention {
                title: "Compute constant value".into(),
                edits: vec![(expression.text_range(), result)],
            })
        }
    }
    out
}
pub fn requests(file: &Arc<FileInfo>, offset: TextSize) -> Vec<(String, EditorRequest)> {
    let root = file.parse.syntax_node();
    let Some(token) = token_at(&root, offset) else {
        return vec![];
    };
    let mut out = Vec::new();
    if token
        .parent_ancestors()
        .any(|node| ast::Expr::can_cast(node.kind()))
    {
        for (title, kind) in [
            ("Introduce variable from expression", Refactoring::Variable),
            ("Extract variable…", Refactoring::Variable),
            ("Extract constant…", Refactoring::Constant),
            ("Extract field…", Refactoring::Field),
            ("Extract parameter…", Refactoring::Parameter),
            ("Extract method…", Refactoring::Method),
            ("Inline local variable", Refactoring::Inline),
        ] {
            let function = token.parent_ancestors().find_map(ast::FuncDef::cast);
            if kind == Refactoring::Field
                && !function
                    .as_ref()
                    .is_some_and(|function| function.is_method())
            {
                continue;
            }
            if kind == Refactoring::Parameter && function.is_none() {
                continue;
            }
            if kind == Refactoring::Inline
                && !token
                    .parent_ancestors()
                    .any(|node| node.kind() == SyntaxKind::NAME_EXPR)
            {
                continue;
            }
            out.push((title.into(), EditorRequest::Refactor(kind)))
        }
    }
    if let Some(string) = token.parent_ancestors().find_map(ast::StringExpr::cast) {
        if !string.is_bytes() {
            out.push(("Check RegExp".into(), EditorRequest::Regex(string.value())))
        }
    }
    if let Some((_, value)) = evaluated(file, offset) {
        let text = if let Value::String(text) = value {
            text
        } else {
            value.render()
        };
        out.push(("Copy value to clipboard".into(), EditorRequest::Copy(text)))
    }
    if file
        .path
        .extension()
        .is_some_and(|extension| extension == "py")
        && !file.path.starts_with("<typeshed>")
    {
        out.push(("Create test".into(), EditorRequest::CreateTest));
        out.push(("Navigate to test".into(), EditorRequest::NavigateTest));
        out.push((
            if file
                .path
                .file_name()
                .is_some_and(|name| name == "__init__.py")
            {
                "Convert to module"
            } else {
                "Convert to package"
            }
            .into(),
            if file
                .path
                .file_name()
                .is_some_and(|name| name == "__init__.py")
            {
                EditorRequest::ConvertModule
            } else {
                EditorRequest::ConvertPackage
            },
        ));
    }
    out.extend(text_requests(&file.text, offset));
    out
}
/// Non-Python text (CSS, comments, requirements) uses the same caret-owned menu.
pub fn text_requests(text: &str, offset: TextSize) -> Vec<(String, EditorRequest)> {
    let offset = usize::from(offset).min(text.len());
    if !text.is_char_boundary(offset) {
        return vec![];
    }
    let begin = line_start(text, offset);
    let end = text[offset..].find('\n').map_or(text.len(), |n| offset + n);
    if end - begin > 65536 {
        return Vec::new();
    }
    let line = &text[begin..end];
    let local = offset - begin;
    let mut out = Vec::new();
    let re = regex::Regex::new(r#"https?://[^\s<>\"']+"#).unwrap();
    for url in re.find_iter(line) {
        let text = url
            .as_str()
            .trim_end_matches([')', ']', '}', ',', '.', ';']);
        if (url.start()..url.start() + text.len()).contains(&local) {
            let url = text;
            out.push((
                "Open URL in browser".into(),
                EditorRequest::OpenUrl(url.into()),
            ))
        }
    }
    let re = regex::Regex::new(r"(?i)\bPEP[- ]?(\d{1,4})\b").unwrap();
    for capture in re.captures_iter(line) {
        let matched = capture.get(0).unwrap();
        if (matched.start()..matched.end()).contains(&local) {
            let number = capture[1].parse::<u16>().unwrap();
            out.push((
                format!("Go to PEP {number}"),
                EditorRequest::OpenUrl(format!("https://peps.python.org/pep-{number:04}/")),
            ))
        }
    }
    let re = regex::Regex::new(r"#[0-9a-fA-F]{3}(?:[0-9a-fA-F]{3})?\b").unwrap();
    for color in re.find_iter(line) {
        if (color.start()..color.end()).contains(&local) {
            let digits = &color.as_str()[1..];
            let mut rgb = [0; 3];
            for (i, channel) in rgb.iter_mut().enumerate() {
                *channel = if digits.len() == 3 {
                    u8::from_str_radix(&digits[i..i + 1], 16).unwrap() * 17
                } else {
                    u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16).unwrap()
                }
            }
            out.push((
                "Show color picker".into(),
                EditorRequest::Color {
                    range: rng(begin + color.start(), begin + color.end()),
                    rgb,
                },
            ))
        }
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
    fn transform(source: &str, needle: &str, title: &str) -> String {
        let (db, file) = setup(source);
        let action = at(
            &db,
            &file,
            TextSize::new(source.find(needle).unwrap() as u32),
        )
        .into_iter()
        .find(|a| a.title == title)
        .unwrap();
        apply_edits(source, &action.edits)
    }
    #[test]
    fn comma_movement_preserves_separators_and_comments() {
        assert_eq!(
            transform("f(first, second)\n", "first", "Move element right"),
            "f(second, first)\n"
        );
        assert_eq!(
            transform("values = [1, 2, 3]\n", "2", "Move element left"),
            "values = [2, 1, 3]\n"
        );
        assert_eq!(transform("f(a, b)\n", ",", "Flip ','"), "f(b, a)\n");
        let (db, file) = setup("f(a, # keep\n b)\n");
        assert!(at(&db, &file, TextSize::new(3)).is_empty());
    }
    #[test]
    fn constants_are_python_values_with_bounded_safe_evaluation() {
        assert_eq!(
            transform("value = (2 + 3) * 4\n", "2", "Compute constant value"),
            "value = 20\n"
        );
        assert_eq!(
            transform("value = -7 // 3\n", "7", "Compute constant value"),
            "value = -3\n"
        );
        assert_eq!(
            transform("value = -7 % 3\n", "7", "Compute constant value"),
            "value = 2\n"
        );
        let (db, file) = setup("value = dangerous() + 1\n");
        assert!(!at(&db, &file, TextSize::new(10))
            .iter()
            .any(|a| a.title == "Compute constant value"));
        let (db, file) = setup("value = 'x' * 10000000\n");
        assert!(!at(&db, &file, TextSize::new(12))
            .iter()
            .any(|a| a.title == "Compute constant value"));
    }
    #[test]
    fn typed_requests_use_actual_string_value_and_refactoring_bridges() {
        let (_, file) = setup("value = 'a\\nb'\n");
        let requests = requests(&file, TextSize::new(12));
        assert!(requests
            .iter()
            .any(|(_, r)| matches!(r,EditorRequest::Copy(value)if value=="a\nb")));
        assert!(requests
            .iter()
            .any(|(_, r)| matches!(r,EditorRequest::Regex(value)if value=="a\nb")));
        assert!(requests
            .iter()
            .any(|(_, r)| matches!(r, EditorRequest::Refactor(Refactoring::Variable))));
    }
    #[test]
    fn url_pep_and_color_use_actual_caret_range() {
        let text = "# PEP 484 https://example.org/a?q=1 and #a0F\n";
        let at =
            |needle: &str| text_requests(text, TextSize::new(text.find(needle).unwrap() as u32));
        assert!(at("484").iter().any(|(_,r)|matches!(r,EditorRequest::OpenUrl(url)if url=="https://peps.python.org/pep-0484/")));
        assert!(at("example").iter().any(
            |(_, r)| matches!(r,EditorRequest::OpenUrl(url)if url=="https://example.org/a?q=1")
        ));
        assert!(at("#a0F").iter().any(|(_,r)|matches!(r,EditorRequest::Color{range,rgb}if slice(text,*range)=="#a0F"&&*rgb==[170,0,255])));
        assert!(at(" and ").is_empty());
        assert!(at(" https").is_empty());
        assert!(at("\n").is_empty());
        let punctuation = "https://example.org/path). next";
        let end = punctuation.find(')').unwrap();
        assert!(text_requests(punctuation, TextSize::new(end as u32)).is_empty());
    }
    #[test]
    fn oversized_integer_and_bytes_are_never_retyped_as_float_or_text() {
        for source in [
            "value = 999999999999999999999 + 1\n",
            "value = b'a' + b'b'\n",
        ] {
            let (db, file) = setup(source);
            assert!(!at(&db, &file, TextSize::new(10))
                .iter()
                .any(|action| action.title == "Compute constant value"));
        }
    }
}
