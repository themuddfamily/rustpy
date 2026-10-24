//! Target-version gates and migration diagnostics. The parser always accepts
//! the current grammar; selecting an older interpreter must not change recovery.
use super::{QuickFix, Sink};
use rustpy_core::{Severity, TextRange};
use rustpy_semantic::db::FileInfo;
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind as K, SyntaxNode,
};

fn require(
    node: &SyntaxNode,
    version: (u32, u32),
    needed: (u32, u32),
    what: &str,
    sink: &mut Sink,
) {
    if version < needed {
        sink.push(
            node.text_range(),
            Severity::Warning,
            "PyCompatibility",
            format!(
                "Python version {}.{} does not support {what} (requires {}.{})",
                version.0, version.1, needed.0, needed.1
            ),
            Vec::new(),
        );
    }
}
pub(super) fn run(file: &FileInfo, version: (u32, u32), sink: &mut Sink) {
    let root = file.parse.syntax_node();
    for node in root.descendants() {
        match node.kind() {
            K::FSTRING_EXPR => fstring(&node, version, sink),
            K::TYPE_PARAM | K::TYPE_VAR_TUPLE_PARAM | K::PARAM_SPEC_PARAM => {
                if node.children().any(|n| n.kind() == K::DEFAULT_VALUE) {
                    require(
                        &node,
                        version,
                        (3, 13),
                        "type parameter defaults (PEP 696)",
                        sink,
                    );
                }
            }
            K::FUNC_DEF => {
                if let Some(f) = ast::FuncDef::cast(node.clone()) {
                    if f.is_async() {
                        require(&node, version, (3, 5), "async functions", sink);
                    }
                }
                generator_returns(&node, version, sink);
                python2_dunder(&node, version, sink);
            }
            K::AWAIT_EXPR => require(&node, version, (3, 5), "await expressions", sink),
            K::FOR_STMT | K::WITH_STMT | K::COMP_FOR => {
                if node
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .any(|t| t.kind() == K::ASYNC_KW)
                {
                    require(
                        &node,
                        version,
                        if node.kind() == K::COMP_FOR {
                            (3, 6)
                        } else {
                            (3, 5)
                        },
                        "asynchronous iteration/context managers",
                        sink,
                    );
                }
            }
            K::YIELD_FROM_EXPR => require(&node, version, (3, 3), "yield from", sink),
            K::ANN_ASSIGN_STMT => require(&node, version, (3, 6), "variable annotations", sink),
            K::IMPORT_FROM_STMT => relative_import(file, &node, sink),
            K::CONCAT_STRING_EXPR if version.0 >= 3 => string_mix(&node, sink),
            K::CALL_EXPR if version.0 >= 3 => python2_call(file, &node, sink),
            K::ASSIGN_STMT if version.0 >= 3 => metaclass(&node, sink),
            K::EXCEPT_CLAUSE => {
                if let Some(c) = ast::ExceptClause::cast(node.clone()) {
                    if let Some(ast::Expr::Tuple(types)) = c.exception_type() {
                        if !types
                            .syntax()
                            .children_with_tokens()
                            .filter_map(|e| e.into_token())
                            .any(|t| t.kind() == K::L_PAREN)
                        {
                            require(
                                &node,
                                version,
                                (3, 14),
                                "unparenthesized multiple exception types",
                                sink,
                            );
                        }
                    }
                }
            }
            K::RAISE_STMT if version.0 >= 3 => {
                if let Some(stmt) = ast::RaiseStmt::cast(node.clone()) {
                    if matches!(stmt.exception(), Some(ast::Expr::String(_))) {
                        migration(
                            &node,
                            "String exceptions are not supported; raise an exception instance",
                            None,
                            sink,
                        );
                    }
                }
            }
            _ => {}
        }
    }
    for token in root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        if token.kind() == K::INT_NUMBER
            && token.text().len() > 1
            && token.text().starts_with('0')
            && token.text().chars().all(|c| c.is_ascii_digit())
            && token.text().chars().any(|c| c != '0')
            && version.0 >= 3
        {
            let replacement = token
                .text()
                .chars()
                .all(|c| ('0'..='7').contains(&c))
                .then(|| format!("0o{}", &token.text()[1..]));
            let fixes = replacement
                .map(|value| QuickFix {
                    title: "Use an explicit octal prefix".into(),
                    edits: vec![(token.text_range(), value)],
                })
                .into_iter()
                .collect();
            sink.push(
                token.text_range(),
                Severity::Error,
                "PyCompatibility",
                "Leading-zero decimal integers are not permitted; use the 0o prefix for octal",
                fixes,
            );
        }
        if token.kind() == K::TSTRING_START && version < (3, 14) {
            sink.push(
                token.text_range(),
                Severity::Warning,
                "PyCompatibility",
                format!(
                    "Python version {}.{} does not support template strings (requires 3.14)",
                    version.0, version.1
                ),
                Vec::new(),
            );
        }
        if token.kind() == K::STRING {
            let raw = token.text();
            let prefix = raw
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect::<String>()
                .to_ascii_lowercase();
            if prefix.contains('b') && !raw.is_ascii() {
                sink.push(
                    token.text_range(),
                    Severity::Error,
                    "PySyntaxError",
                    "Bytes literals may only contain ASCII source characters",
                    Vec::new(),
                );
            }
            if version.0 == 2
                && !prefix.contains('u')
                && !raw.is_ascii()
                && !has_encoding(&file.text)
            {
                sink.push(
                    token.text_range(),
                    Severity::Warning,
                    "PyCompatibility",
                    "Non-ASCII Python 2 source requires an encoding declaration",
                    Vec::new(),
                );
            }
        }
        if version.0 >= 3 && token.kind() == K::IDENT && token.text() == "__debug__" {
            // Parameter and definition names do not pass through assignment ASTs.
            if token.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    K::PARAM | K::STAR_PARAM | K::STAR_STAR_PARAM | K::FUNC_DEF | K::CLASS_DEF
                )
            }) {
                sink.push(
                    token.text_range(),
                    Severity::Error,
                    "PySyntaxError",
                    "Cannot bind __debug__",
                    Vec::new(),
                );
            }
        }
    }
    if version.0 >= 3 {
        legacy_lines(file, sink);
    }
}
fn has_encoding(text: &str) -> bool {
    text.lines().take(2).any(|line| {
        line.trim_start().starts_with('#') && (line.contains("coding:") || line.contains("coding="))
    })
}
fn migration(
    node: &SyntaxNode,
    message: &str,
    replacement: Option<(TextRange, String)>,
    sink: &mut Sink,
) {
    let fixes = replacement
        .map(|edit| QuickFix {
            title: "Convert Python 2 usage".into(),
            edits: vec![edit],
        })
        .into_iter()
        .collect();
    sink.push(
        node.text_range(),
        Severity::Warning,
        "PyCompatibility",
        message,
        fixes,
    );
}
fn python2_call(file: &FileInfo, node: &SyntaxNode, sink: &mut Sink) {
    let Some(call) = ast::CallExpr::cast(node.clone()) else {
        return;
    };
    let Some(callee) = call.callee() else { return };
    match callee {
        ast::Expr::Name(n)
            if file
                .scopes
                .resolve(file.scopes.scope_at(n.text_range().start()), &n.name_text())
                .is_none() =>
        {
            let replacement = match n.name_text().as_str() {
                "xrange" => "range",
                "unicode" => "str",
                "raw_input" => "input",
                "basestring" => "str",
                "long" => "int",
                "file" => "open",
                _ => return,
            };
            migration(
                node,
                &format!(
                    "Python 2 builtin '{}' is not available; use {replacement}",
                    n.name_text()
                ),
                Some((n.text_range(), replacement.into())),
                sink,
            );
        }
        ast::Expr::Attribute(a) => {
            let replacement = match a.attr_text().as_str() {
                "iteritems" => Some("items"),
                "iterkeys" => Some("keys"),
                "itervalues" => Some("values"),
                "has_key" => None,
                "sort" => {
                    if call
                        .arg_list()
                        .is_some_and(|args| args.keywords().any(|k| k.name_text() == "cmp"))
                    {
                        migration(
                            node,
                            "The cmp= argument to sort() was removed; use key= with functools.cmp_to_key",
                            None,
                            sink,
                        );
                    }
                    return;
                }
                _ => return,
            };
            let edit = replacement.and_then(|r| a.attr().map(|t| (t.text_range(), r.to_string())));
            migration(
                node,
                if a.attr_text() == "has_key" {
                    "Python 2 has_key() is obsolete; use 'key in mapping'"
                } else {
                    "Python 2 iterator method is obsolete; use the Python 3 dictionary view method"
                },
                edit,
                sink,
            );
        }
        _ => {}
    }
}
fn python2_dunder(node: &SyntaxNode, version: (u32, u32), sink: &mut Sink) {
    if version.0 < 3 {
        return;
    }
    let Some(f) = ast::FuncDef::cast(node.clone()) else {
        return;
    };
    let hint = match f.name_text().as_str() {
        "__nonzero__" => "__bool__",
        "__cmp__" => "rich comparison methods",
        "__div__" => "__truediv__",
        "__idiv__" => "__itruediv__",
        "__rdiv__" => "__rtruediv__",
        _ => return,
    };
    migration(
        node,
        &format!(
            "Python 2 special method '{}' is not used by Python 3; use {hint}",
            f.name_text()
        ),
        None,
        sink,
    );
}
fn metaclass(node: &SyntaxNode, sink: &mut Sink) {
    let Some(s) = ast::AssignStmt::cast(node.clone()) else {
        return;
    };
    if s.targets()
        .iter()
        .any(|e| matches!(e,ast::Expr::Name(n) if n.name_text()=="__metaclass__"))
        && node
            .ancestors()
            .skip(1)
            .find(|a| matches!(a.kind(), K::CLASS_DEF | K::FUNC_DEF | K::MODULE))
            .is_some_and(|n| n.kind() == K::CLASS_DEF)
    {
        migration(
            node,
            "Python 2 __metaclass__ assignment has no effect; use class C(metaclass=...)",
            None,
            sink,
        );
    }
}
fn fstring(node: &SyntaxNode, version: (u32, u32), sink: &mut Sink) {
    let Some(start) = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| matches!(t.kind(), K::FSTRING_START | K::TSTRING_START))
    else {
        return;
    };
    if start.kind() == K::TSTRING_START {
        return;
    }
    require(node, version, (3, 6), "formatted string literals", sink);
    let quote = start
        .text()
        .chars()
        .find(|c| matches!(c, '\'' | '"'))
        .unwrap_or('"');
    for replacement in node
        .children()
        .filter(|n| n.kind() == K::FSTRING_REPLACEMENT)
    {
        if replacement
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == K::EQ)
        {
            require(
                &replacement,
                version,
                (3, 8),
                "self-documenting f-string expressions",
                sink,
            );
        }
        if version < (3, 12) {
            let modern = replacement
                .descendants_with_tokens()
                .filter_map(|e| e.into_token())
                .any(|t| {
                    t.kind() == K::COMMENT
                        || (matches!(t.kind(), K::STRING | K::FSTRING_START)
                            && t.text().chars().find(|c| matches!(c, '\'' | '"')) == Some(quote))
                        || (t.kind() == K::STRING && t.text().contains('\\'))
                });
            if modern {
                require(
                    &replacement,
                    version,
                    (3, 12),
                    "f-string quote reuse, comments and backslashes (PEP 701)",
                    sink,
                );
            }
        }
    }
}
fn string_mix(node: &SyntaxNode, sink: &mut Sink) {
    let mut bytes = false;
    let mut text = false;
    for child in node.children() {
        match ast::Expr::cast(child) {
            Some(ast::Expr::String(s)) => {
                if s.is_bytes() {
                    bytes = true
                } else {
                    text = true
                }
            }
            Some(ast::Expr::FString(_)) => text = true,
            _ => {}
        }
    }
    if bytes && text {
        sink.push(
            node.text_range(),
            Severity::Error,
            "PySyntaxError",
            "Cannot mix bytes and nonbytes literals",
            Vec::new(),
        );
    }
}
fn relative_import(file: &FileInfo, node: &SyntaxNode, sink: &mut Sink) {
    let Some(import) = ast::ImportFromStmt::cast(node.clone()) else {
        return;
    };
    if import.level() == 0 {
        return;
    }
    let Some(module) = &file.module_name else {
        return;
    };
    let init = file
        .path
        .file_name()
        .is_some_and(|n| n == "__init__.py" || n == "__init__.pyi");
    let package_depth = module.split('.').count().saturating_sub(usize::from(!init));
    if import.level() > package_depth {
        sink.push(
            node.text_range(),
            Severity::Error,
            "PyRelativeImport",
            if package_depth == 0 {
                "Relative import in a non-package module"
            } else {
                "Relative import goes beyond the top-level package"
            },
            Vec::new(),
        );
    }
}
/// Legacy exception syntax is recovered as tokens rather than a modern AST.
/// Inspect only physical statement headers, never comments or string contents.
fn legacy_lines(file: &FileInfo, sink: &mut Sink) {
    let root = file.parse.syntax_node();
    let tokens: Vec<_> = root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .collect();
    for (i, token) in tokens.iter().enumerate() {
        if !matches!(token.kind(), K::EXCEPT_KW | K::RAISE_KW) {
            continue;
        }
        let mut depth = 0;
        for next in tokens.iter().skip(i + 1) {
            if next.kind() == K::NEWLINE || (next.kind() == K::COLON && depth == 0) {
                break;
            }
            match next.kind() {
                K::L_PAREN | K::L_BRACK | K::L_BRACE => depth += 1,
                K::R_PAREN | K::R_BRACK | K::R_BRACE => depth -= 1,
                K::COMMA if depth == 0 => {
                    // Python 3.14 allows unparenthesized exception lists; comma+as
                    // and no bound name are distinguished by the parsed clause.
                    if token.kind() == K::RAISE_KW {
                        sink.push(
                            TextRange::new(token.text_range().start(), next.text_range().end()),
                            Severity::Error,
                            "PyCompatibility",
                            "Python 2 'raise E, value' syntax is not supported; use raise E(value)",
                            Vec::new(),
                        );
                    }
                    break;
                }
                _ => {}
            }
        }
    }
    // Binding metadata gives aliases and user-defined replacements precedence.
    let callees: std::collections::HashSet<_> = root
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter_map(|c| c.callee())
        .map(|e| e.text_range())
        .collect();
    for reference in &file.scopes.references {
        if !matches!(
            reference.name.as_str(),
            "xrange" | "unicode" | "raw_input" | "basestring" | "long" | "file"
        ) || reference.resolved.is_some()
        {
            continue;
        }
        let in_call = callees.contains(&reference.range);
        if !in_call
            && file
                .scopes
                .resolve(reference.scope, &reference.name)
                .is_none()
        {
            sink.push(
                reference.range,
                Severity::Warning,
                "PyCompatibility",
                format!(
                    "Python 2 builtin '{}' is not available in Python 3",
                    reference.name
                ),
                Vec::new(),
            );
        }
    }
}

fn generator_returns(node: &SyntaxNode, version: (u32, u32), sink: &mut Sink) {
    let Some(function) = ast::FuncDef::cast(node.clone()) else {
        return;
    };
    let own = |n: &SyntaxNode| {
        n.ancestors()
            .skip(1)
            .find(|a| matches!(a.kind(), K::FUNC_DEF | K::CLASS_DEF | K::LAMBDA_EXPR))
            .is_some_and(|a| a == *node)
    };
    if !node
        .descendants()
        .any(|n| matches!(n.kind(), K::YIELD_EXPR | K::YIELD_FROM_EXPR) && own(&n))
    {
        return;
    }
    for n in node.descendants().filter(|n| own(n)) {
        if n.kind() == K::YIELD_FROM_EXPR && function.is_async() {
            sink.push(
                n.text_range(),
                Severity::Error,
                "PySyntaxError",
                "'yield from' inside async function",
                Vec::new(),
            );
        }
        if let Some(ret) = ast::ReturnStmt::cast(n.clone()) {
            if ret.value().is_some() {
                if function.is_async() {
                    sink.push(
                        n.text_range(),
                        Severity::Error,
                        "PySyntaxError",
                        "'return' with value in async generator",
                        Vec::new(),
                    );
                } else {
                    require(
                        &n,
                        version,
                        (3, 3),
                        "return with a value in a generator",
                        sink,
                    );
                }
            }
        }
    }
}
