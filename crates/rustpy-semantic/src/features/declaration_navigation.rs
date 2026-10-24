//! Declaration-only routes and physical stub/source correspondence.
//! These queries never execute user code or manufacture replacement source.
use super::*;
use rustpy_syntax::rowan::TokenAtOffset;

pub(super) fn definition_at(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    prefer_source: bool,
) -> Vec<Location> {
    let offset = crate::clamp_offset(file, offset);
    let inf = Inferer::new(db);
    let mut locations = special(db, &inf, file, offset)
        .unwrap_or_else(|| super::definition_at_ordinary(db, file, offset));
    if prefer_source
        && !locations
            .iter()
            .any(|location| location.path == file.path && location.range.contains_inclusive(offset))
    {
        locations = locations
            .into_iter()
            .flat_map(|location| {
                if location.is_stub {
                    let source = counterpart(db, &location, true);
                    if !source.is_empty() {
                        return source;
                    }
                }
                vec![location]
            })
            .collect();
    }
    locations.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.range.start().cmp(&b.range.start()))
    });
    locations.dedup();
    locations
}
fn location(file: &FileInfo, range: TextRange) -> Location {
    Location {
        path: file.path.clone(),
        range,
        is_stub: file.is_stub,
    }
}
fn token(file: &FileInfo, offset: TextSize) -> Option<SyntaxToken> {
    match file.parse.syntax_node().token_at_offset(offset) {
        TokenAtOffset::Single(token) => Some(token),
        TokenAtOffset::Between(left, right) => {
            if right.kind().is_trivia() {
                Some(left)
            } else {
                Some(right)
            }
        }
        TokenAtOffset::None => None,
    }
}
fn special(
    db: &Analysis,
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Option<Vec<Location>> {
    let tok = token(file, offset)?;
    if tok.kind() == SyntaxKind::STRING {
        return string_targets(db, inf, file, &tok, offset);
    }
    if let Some(expression) = ast::Expr::cast(tok.parent()?) {
        if let Some(call) = expression
            .syntax()
            .parent()
            .and_then(ast::CallExpr::cast)
            .filter(|call| {
                call.callee()
                    .is_some_and(|callee| callee.syntax() == expression.syntax())
            })
        {
            // `super` itself is a class, but calling the canonical builtin
            // creates an MRO proxy, not an instance of its __init__ owner.
            if let Type::Super { after, receiver } =
                inf.type_of_expr(file, &ast::Expr::Call(call.clone()))
            {
                let first = inf.super_mro(&after, &receiver)?.0.into_iter().next()?.0;
                let source = db.file(first.file)?;
                return Some(vec![location(
                    &source,
                    crate::hierarchy::class_name_range(&source, &first),
                )]);
            }
            let callee = inf.type_of_expr(file, &expression);
            if let Type::Class(class, args) = callee {
                let target = member_locations(db, inf, &Type::Instance(class, args), "__init__");
                if !target.is_empty()
                    && !target.iter().all(|target| {
                        db.file_for_path(&target.path)
                            .and_then(|source| qualified_name(&source, &target.range))
                            .is_some_and(|path| path.first().is_some_and(|name| name == "object"))
                    })
                {
                    return Some(target);
                }
            }
        }
    }
    operator_targets(db, inf, file, &tok)
}
fn canonical_function(db: &Analysis, value: &Type, module: &str, name: &str) -> bool {
    let valid = |signature: &crate::types::Signature| {
        signature.def.as_ref().is_some_and(|definition| {
            db.file(definition.file).is_some_and(|file| {
                file.is_stub
                    && file.path.starts_with("<typeshed>")
                    && file.module_name.as_deref() == Some(module)
                    && definition.name.as_str() == name
            })
        })
    };
    match value {
        Type::Function(signature) => valid(signature),
        Type::Overloaded(signatures) => {
            !signatures.is_empty()
                && signatures.len() <= 128
                && signatures.iter().all(|signature| valid(signature))
        }
        _ => false,
    }
}

fn string_targets(
    db: &Analysis,
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    tok: &SyntaxToken,
    offset: TextSize,
) -> Option<Vec<Location>> {
    if tok.text().len() > 4096 {
        return None;
    }
    let rustpy_syntax::string_literal::Decoded::Text(text, _) =
        rustpy_syntax::string_literal::string(tok.text()).ok()?
    else {
        return None;
    };
    let node = tok.parent()?;
    if let Some(call) = node.ancestors().find_map(ast::CallExpr::cast) {
        let args: Vec<_> = call.arg_list()?.args().take(33).collect();
        if args.len() > 32 {
            return None;
        }
        let position = args.iter().position(|argument| {
            argument
                .value()
                .is_some_and(|value| value.syntax().text_range().contains_range(tok.text_range()))
        })?;
        let callee = inf.type_of_expr(file, &call.callee()?);
        if position == 1
            && ["getattr", "setattr", "delattr", "hasattr"]
                .iter()
                .any(|name| canonical_function(db, &callee, "builtins", name))
        {
            let receiver = inf.type_of_expr(file, &args.first()?.value()?);
            return Some(member_locations(db, inf, &receiver, &text));
        }
        let import_module = canonical_function(db, &callee, "importlib", "import_module");
        if import_module || canonical_function(db, &callee, "builtins", "__import__") {
            // Bind by parameter role, not lexical argument order. The maintained
            // import inference validates duplicates, star arguments and relative
            // package semantics before this query accepts any module target.
            let role = match &args[position] {
                ast::AnyArg::Keyword(keyword) => keyword.name_text(),
                ast::AnyArg::Positional(_) => {
                    let index = args[..position]
                        .iter()
                        .filter(|argument| matches!(argument, ast::AnyArg::Positional(_)))
                        .count();
                    match index {
                        0 => "name".into(),
                        1 if import_module => "package".into(),
                        _ => String::new(),
                    }
                }
                _ => String::new(),
            };
            let Type::Module(module) = inf.type_of_expr(file, &ast::Expr::Call(call)) else {
                return None;
            };
            if role == "name" {
                return db
                    .file(module)
                    .map(|module| vec![location(&module, TextRange::empty(TextSize::new(0)))]);
            }
            if import_module && role == "package" {
                return db
                    .module(&text)
                    .map(|module| vec![location(&module, TextRange::empty(TextSize::new(0)))]);
            }
        }
    }
    if node.ancestors().any(|node| {
        matches!(
            node.kind(),
            SyntaxKind::ANNOTATION | SyntaxKind::RETURN_ANNOTATION
        )
    }) {
        if let Some(subscription) = node.ancestors().find_map(ast::SubscriptExpr::cast) {
            if let Some(head) = subscription.object() {
                if inf
                    .symbol_origin(file, &head)
                    .is_some_and(|(module, name)| {
                        matches!(module.as_str(), "typing" | "typing_extensions")
                            && name == "Literal"
                            && db.module(&module).is_some_and(|source| {
                                source.is_stub && source.path.starts_with("<typeshed>")
                            })
                    })
                {
                    return None;
                }
            }
        }
        // Unescaped spelling keeps physical caret offsets exact. In escaped
        // annotations a single identifier remains unambiguous.
        let raw = tok.text();
        let quote = raw.find(['\'', '"'])?;
        let width = if raw[quote..].starts_with("\"\"\"") || raw[quote..].starts_with("'''") {
            3
        } else {
            1
        };
        let content_offset = usize::from(offset)
            .saturating_sub(usize::from(tok.text_range().start()))
            .saturating_sub(quote + width)
            .min(text.len());
        let parsed = rustpy_syntax::parse(&text);
        if !parsed.errors().is_empty() {
            return None;
        }
        let mut names: Vec<_> = parsed
            .syntax_node()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| token.kind() == SyntaxKind::IDENT)
            .take(33)
            .collect();
        if names.len() > 32 {
            return None;
        }
        if raw.contains('\\') && names.len() != 1 {
            return None;
        }
        let selected = names
            .iter()
            .position(|name| {
                name.text_range()
                    .contains_inclusive(TextSize::new(content_offset as u32))
            })
            .or_else(|| (raw.contains('\\') && names.len() == 1).then_some(0))?;
        let name = names.remove(selected);
        let parent = name.parent()?;
        let (first, attrs) = if let Some(attribute) = ast::AttributeExpr::cast(parent) {
            let mut current = ast::Expr::Attribute(attribute);
            let mut attrs = Vec::new();
            loop {
                match current {
                    ast::Expr::Attribute(attribute) => {
                        attrs.push(attribute.attr()?.text().to_string());
                        current = attribute.object()?;
                    }
                    ast::Expr::Name(name) => {
                        attrs.reverse();
                        break (name.name_text(), attrs);
                    }
                    _ => return None,
                }
            }
        } else {
            (name.text().to_string(), Vec::new())
        };
        let definition = inf.follow_imports(inf.resolve_name(
            file,
            file.scopes.scope_at(tok.text_range().start()),
            &first,
        )?);
        if attrs.is_empty() {
            if let Some(module) = inf.import_target_module(&definition) {
                return Some(vec![location(&module, TextRange::empty(TextSize::new(0)))]);
            }
            return Some(vec![location(&definition.file, definition.binding().range)]);
        }
        let mut value = inf.type_of_binding(&definition.file, definition.binding);
        for attribute in &attrs[..attrs.len() - 1] {
            value = inf.lookup_member(&value, attribute)?;
        }
        return Some(member_locations(db, inf, &value, attrs.last()?));
    }
    // __all__ is a symbol list, unlike arbitrary string-valued variables.
    if let Some(assignment) = node.ancestors().find_map(ast::AssignStmt::cast) {
        let value = assignment
            .syntax()
            .children()
            .filter_map(ast::Expr::cast)
            .last()?;
        let direct = match value {
            ast::Expr::List(list) => list.elements().any(|value| value.syntax() == &node),
            ast::Expr::Tuple(tuple) => tuple.elements().any(|value| value.syntax() == &node),
            _ => false,
        };
        if !direct {
            return None;
        }

        if assignment
            .syntax()
            .children()
            .filter_map(ast::Expr::cast)
            .next()
            .is_some_and(
                |target| matches!(target,ast::Expr::Name(name)if name.name_text()=="__all__"),
            )
        {
            if let Some(definition) = inf.resolve_name(file, file.scopes.module_scope(), &text) {
                let definition = inf.follow_imports(definition);
                return Some(vec![location(&definition.file, definition.binding().range)]);
            }
        }
    }
    None
}
fn operator_targets(
    db: &Analysis,
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    token: &SyntaxToken,
) -> Option<Vec<Location>> {
    let mut targets = Vec::new();
    let mut add = |expression: ast::Expr, name: &str| {
        for target in member_locations(db, inf, &inf.type_of_expr(file, &expression), name) {
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
    };
    if let Some(assignment) = token.parent().and_then(ast::AugAssignStmt::cast) {
        if assignment.op().as_ref() != Some(token) {
            return None;
        }
        let (inplace, direct, reflected) = match token.text() {
            "+=" => ("__iadd__", "__add__", "__radd__"),
            "-=" => ("__isub__", "__sub__", "__rsub__"),
            "*=" => ("__imul__", "__mul__", "__rmul__"),
            "/=" => ("__itruediv__", "__truediv__", "__rtruediv__"),
            "//=" => ("__ifloordiv__", "__floordiv__", "__rfloordiv__"),
            "%=" => ("__imod__", "__mod__", "__rmod__"),
            "**=" => ("__ipow__", "__pow__", "__rpow__"),
            "@=" => ("__imatmul__", "__matmul__", "__rmatmul__"),
            "|=" => ("__ior__", "__or__", "__ror__"),
            "&=" => ("__iand__", "__and__", "__rand__"),
            "^=" => ("__ixor__", "__xor__", "__rxor__"),
            "<<=" => ("__ilshift__", "__lshift__", "__rlshift__"),
            ">>=" => ("__irshift__", "__rshift__", "__rrshift__"),
            _ => return None,
        };
        let target = assignment.target()?;
        if inf
            .lookup_member(&inf.type_of_expr(file, &target), inplace)
            .is_some()
        {
            add(target, inplace);
        } else {
            add(target, direct);
            add(assignment.value()?, reflected);
        }
    } else if let Some(binary) = token.parent().and_then(ast::BinaryExpr::cast) {
        if binary.op().as_ref() != Some(token) {
            return None;
        }
        let (direct, reflected) = match token.text() {
            "+" => ("__add__", "__radd__"),
            "-" => ("__sub__", "__rsub__"),
            "*" => ("__mul__", "__rmul__"),
            "/" => ("__truediv__", "__rtruediv__"),
            "//" => ("__floordiv__", "__rfloordiv__"),
            "%" => ("__mod__", "__rmod__"),
            "**" => ("__pow__", "__rpow__"),
            "@" => ("__matmul__", "__rmatmul__"),
            "|" => ("__or__", "__ror__"),
            "&" => ("__and__", "__rand__"),
            "^" => ("__xor__", "__rxor__"),
            "<<" => ("__lshift__", "__rlshift__"),
            ">>" => ("__rshift__", "__rrshift__"),
            _ => return None,
        };
        add(binary.lhs()?, direct);
        add(binary.rhs()?, reflected);
    } else if let Some(unary) = token.parent().and_then(ast::UnaryExpr::cast) {
        let method = match token.text() {
            "+" => "__pos__",
            "-" => "__neg__",
            "~" => "__invert__",
            _ => return None,
        };
        add(unary.operand()?, method);
    } else if let Some(operation) = token.parent().and_then(ast::CompareOp::cast) {
        let comparison = operation
            .syntax()
            .parent()
            .and_then(ast::CompareExpr::cast)?;
        let index = comparison
            .ops()
            .position(|op| op.syntax() == operation.syntax())?;
        let operands: Vec<_> = comparison.operands().take(65).collect();
        if operands.len() > 64 {
            return None;
        }
        let left = operands.get(index)?.clone();
        let right = operands.get(index + 1)?.clone();
        match operation.text().as_str() {
            "in" | "not in" => add(right, "__contains__"),
            "==" => {
                add(left, "__eq__");
                add(right, "__eq__");
            }
            "!=" => {
                add(left, "__ne__");
                add(right, "__ne__");
            }
            "<" => {
                add(left, "__lt__");
                add(right, "__gt__");
            }
            "<=" => {
                add(left, "__le__");
                add(right, "__ge__");
            }
            ">" => {
                add(left, "__gt__");
                add(right, "__lt__");
            }
            ">=" => {
                add(left, "__ge__");
                add(right, "__le__");
            }
            _ => return None,
        }
    } else if let Some(subscript) = token.parent().and_then(ast::SubscriptExpr::cast) {
        if !matches!(token.kind(), SyntaxKind::L_BRACK | SyntaxKind::R_BRACK) {
            return None;
        }
        let base = subscript.object()?;
        let mode = subscript_mode(&subscript);
        if mode == 2 {
            add(base, "__delitem__");
        } else if mode == 1 {
            add(base, "__setitem__");
        } else if mode == 3 {
            add(base.clone(), "__getitem__");
            add(base, "__setitem__");
        } else if matches!(inf.type_of_expr(file, &base), Type::Class(..)) {
            add(base, "__class_getitem__");
        } else {
            add(base, "__getitem__");
        }
    } else if let Some(negation) = token.parent().and_then(ast::NotExpr::cast) {
        if token.kind() != SyntaxKind::NOT_KW {
            return None;
        }
        let operand = negation.operand()?;
        if inf
            .lookup_member(&inf.type_of_expr(file, &operand), "__bool__")
            .is_some()
        {
            add(operand, "__bool__");
        } else {
            add(operand, "__len__");
        }
    } else if matches!(token.kind(), SyntaxKind::L_PAREN | SyntaxKind::R_PAREN) {
        let call = token.parent()?.parent().and_then(ast::CallExpr::cast)?;
        let callee = call.callee()?;
        let value = inf.type_of_expr(file, &callee);
        if matches!(value, Type::Instance(..) | Type::SelfType(..)) {
            add(callee, "__call__");
        } else {
            return None;
        }
    } else {
        return None;
    }
    Some(targets)
}

/// Physical counterpart of a selected definition, preserving its lexical
/// qualification rather than matching unrelated same-spelled members.
pub(super) fn counterpart(db: &Analysis, target: &Location, to_source: bool) -> Vec<Location> {
    let Some(file) = db.file_for_path(&target.path) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    let sibling = file
        .path
        .with_extension(if to_source { "py" } else { "pyi" });
    if sibling != file.path {
        if let Some(other) = db.file_for_path(&sibling) {
            candidates.push(other);
        }
    }
    if let Some(module) = file.module_name.as_deref() {
        let other = if to_source {
            db.resolver
                .resolve_source(module)
                .and_then(|path| db.file_for_path(&path))
        } else {
            db.module(module)
        };
        if let Some(other) = other {
            if other.path != file.path && !candidates.iter().any(|file| file.path == other.path) {
                candidates.push(other);
            }
        }
    }
    let key = qualified_name(&file, &target.range);
    for other in candidates.into_iter().take(4) {
        if other.is_stub == to_source
            || other.text.len() > 1024 * 1024
            || other.scopes.bindings.len() > 4096
        {
            continue;
        }
        if target.range.is_empty() {
            return vec![location(&other, TextRange::empty(TextSize::new(0)))];
        }
        let Some(key) = &key else { continue };
        let mut found = Vec::new();
        for binding in &other.scopes.bindings {
            if qualified_name(&other, &binding.range).as_ref() == Some(key) {
                found.push(location(&other, binding.range));
                if found.len() >= 32 {
                    break;
                }
            }
        }
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}
fn qualified_name(file: &FileInfo, range: &TextRange) -> Option<Vec<String>> {
    if file.text.len() > 1024 * 1024 {
        return None;
    }
    let token = token(file, range.start())?;
    let mut path = vec![token.text().to_string()];
    for node in token.parent()?.ancestors().take(33) {
        let name = if let Some(function) = ast::FuncDef::cast(node.clone()) {
            function.name()?
        } else if let Some(class) = ast::ClassDef::cast(node) {
            class.name()?
        } else {
            continue;
        };
        if name.text_range() != token.text_range() {
            path.push(name.text().to_string());
        }
    }
    path.reverse();
    Some(path)
}

// The index and receiver expressions of a store are still ordinary reads.
fn subscript_mode(subscript: &ast::SubscriptExpr) -> u8 {
    let mut target = subscript.syntax().clone();
    for _ in 0..32 {
        let Some(parent) = target.parent() else {
            return 0;
        };
        if matches!(
            parent.kind(),
            SyntaxKind::TUPLE_EXPR
                | SyntaxKind::LIST_EXPR
                | SyntaxKind::STAR_EXPR
                | SyntaxKind::PAREN_EXPR
        ) {
            target = parent;
            continue;
        }
        if let Some(assign) = ast::AssignStmt::cast(parent.clone()) {
            let values: Vec<_> = assign
                .syntax()
                .children()
                .filter_map(ast::Expr::cast)
                .take(65)
                .collect();
            if values.len() > 64 {
                return 0;
            }
            return u8::from(
                values[..values.len().saturating_sub(1)]
                    .iter()
                    .any(|value| value.syntax() == &target),
            );
        }
        if let Some(assign) = ast::AnnAssignStmt::cast(parent.clone()) {
            return u8::from(
                assign.value().is_some()
                    && assign
                        .target()
                        .is_some_and(|value| value.syntax() == &target),
            );
        }
        if let Some(assign) = ast::AugAssignStmt::cast(parent.clone()) {
            return if assign
                .target()
                .is_some_and(|value| value.syntax() == &target)
            {
                3
            } else {
                0
            };
        }
        if parent.kind() == SyntaxKind::DEL_STMT {
            return 2;
        }
        if let Some(statement) = ast::ForStmt::cast(parent.clone()) {
            return u8::from(
                statement
                    .target()
                    .is_some_and(|value| value.syntax() == &target),
            );
        }
        if let Some(item) = ast::WithItem::cast(parent.clone()) {
            return u8::from(item.target().is_some_and(|value| value.syntax() == &target));
        }
        if let Some(clause) = ast::CompFor::cast(parent) {
            return u8::from(
                clause
                    .target()
                    .is_some_and(|value| value.syntax() == &target),
            );
        }
        return 0;
    }
    0
}
