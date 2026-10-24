//! Python-specific CST style and guarded class-member arrangement.
use crate::*;
use rustpy_semantic::{edit_helpers::Edit, FileInfo};
use rustpy_settings::python_style::{Arrangement, Wrap};
use rustpy_syntax::ast::{self, AstNode};

pub(super) fn space(previous: &Tok, current: &Tok, options: &FormatOptions, default: bool) -> bool {
    let style = &options.python;
    let p = previous.kind;
    let c = current.kind;
    if previous.is_comment || current.is_comment {
        return default;
    }
    let selected = if c == L_PAREN {
        Some(if current.parent == PARAM_LIST {
            "before_declaration"
        } else if current.parent == ARG_LIST {
            "before_call"
        } else {
            return default;
        })
    } else if c == L_BRACK && current.parent == SUBSCRIPT_EXPR {
        Some("before_subscript")
    } else if OPEN.contains(&p) || CLOSE.contains(&c) {
        if p == L_PAREN && c == R_PAREN && previous.parent == ARG_LIST {
            Some("empty_call")
        } else if p == L_PAREN || c == R_PAREN {
            Some("within_parentheses")
        } else if p == L_BRACK || c == R_BRACK {
            Some("within_brackets")
        } else {
            Some("within_braces")
        }
    } else if p == COMMA {
        Some("after_comma")
    } else if c == COLON || p == COLON {
        let token = if c == COLON { current } else { previous };
        Some(match (token.parent, c == COLON) {
            (SLICE_EXPR, true) => "before_slice_colon",
            (SLICE_EXPR, false) => "after_slice_colon",
            // The parser consumes ':' before opening ANNOTATION, so the
            // delimiter belongs to the declaration, not its type expression.
            (
                ANNOTATION | ANN_ASSIGN_STMT | PARAM | STAR_PARAM | STAR_STAR_PARAM | TYPE_PARAM,
                true,
            ) => "before_annotation_colon",
            (
                ANNOTATION | ANN_ASSIGN_STMT | PARAM | STAR_PARAM | STAR_STAR_PARAM | TYPE_PARAM,
                false,
            ) => "after_annotation_colon",
            (LAMBDA_EXPR, true) => "before_lambda_colon",
            (LAMBDA_EXPR, false) => "after_lambda_colon",
            (DICT_ITEM, true) => "before_dict_colon",
            (DICT_ITEM, false) => "after_dict_colon",
            _ => return default,
        })
    } else if previous.prefix && matches!(p, STAR | STAR_STAR) {
        Some("after_unpack")
    } else {
        let op = if is_binary_op(c) {
            current
        } else if is_binary_op(p) {
            previous
        } else {
            return default;
        };
        if op.prefix {
            return default;
        }
        Some(match op.kind {
            EQ if op.parent == KEYWORD_ARG => "keyword_equals",
            EQ if op.parent == PARAM => "default_equals",
            EQ => "assignment",
            ARROW => "arrow",
            EQ_EQ | NOT_EQ => "equality",
            LT | GT | LT_EQ | GT_EQ => "relational",
            PLUS | MINUS => "additive",
            STAR | SLASH | SLASH_SLASH | PERCENT | AT => "multiplicative",
            SHL | SHR => "shift",
            STAR_STAR => "power",
            PIPE if op.annotation => "type_union",
            PIPE | AMP | CARET => "bitwise",
            COLON_EQ => "walrus",
            _ => "assignment",
        })
    };
    selected.map_or(default, |key| style.space(key, default))
}
pub(super) fn string_quote(parent: Option<&SyntaxNode>, options: &FormatOptions) -> QuoteStyle {
    let mut preference = "inherit";
    if let Some(parent) = parent {
        if parent.ancestors().any(|node| {
            matches!(
                node.kind(),
                ANNOTATION | RETURN_ANNOTATION | TYPE_ALIAS_STMT
            )
        }) {
            preference = &options.python.type_hint_quotes
        }
        if let Some(expr) = parent.ancestors().find_map(ast::ExprStmt::cast) {
            if let Some(container) = expr.syntax().parent().filter(|container| {
                container.kind() == MODULE
                    || container.kind() == BLOCK
                        && container
                            .parent()
                            .is_some_and(|parent| matches!(parent.kind(), CLASS_DEF | FUNC_DEF))
            }) {
                let first = container.children().find_map(ast::Stmt::cast);
                if first.is_some_and(|first| first.text_range() == expr.text_range()) {
                    preference = &options.python.docstring_quotes
                }
            }
        }
    }
    match preference {
        "single" => QuoteStyle::Single,
        "double" => QuoteStyle::Double,
        "preserve" => QuoteStyle::Preserve,
        _ => options.quote,
    }
}
pub(super) fn wrap(open: &Tok, options: &FormatOptions) -> Wrap {
    options.python.wrap(match open.parent {
        ARG_LIST => "calls",
        PARAM_LIST => "parameters",
        LIST_EXPR => "lists",
        TUPLE_EXPR => "tuples",
        SET_EXPR => "sets",
        DICT_EXPR => "dicts",
        IMPORT_FROM_STMT => "imports",
        _ => "binary",
    })
}
/// Final CST-only alignment and explicitly requested continuations. Every edit
/// is checked by the shared semantic fingerprint, including type comments.
pub(super) fn finish(
    source: &str,
    mut output: String,
    options: &FormatOptions,
) -> Result<String, FormatError> {
    if options.python == rustpy_settings::python_style::PythonStyle::default() {
        return Ok(output);
    }
    if options.python.indents.keep_empty {
        let old = source.lines().collect::<Vec<_>>();
        let old_literals = literal_owned_lines(source);
        let output_literals = literal_owned_lines(&output);
        let mut lines = output
            .split_inclusive('\n')
            .map(String::from)
            .collect::<Vec<_>>();
        // Reuse indentation only where the unchanged neighboring logical lines
        // identify a matching empty line; never create whitespace inside strings.
        for index in 1..lines.len().saturating_sub(1) {
            if lines[index].trim().is_empty() && !output_literals[index] {
                if let Some((at, _)) = old.windows(3).enumerate().find(|(at, window)| {
                    !old_literals[at + 1]
                        && window[0].trim() == lines[index - 1].trim()
                        && window[1].trim().is_empty()
                        && window[2].trim() == lines[index + 1].trim()
                }) {
                    lines[index] = format!("{}\n", old[at + 1]);
                }
            }
        }
        output = lines.concat();
    }
    output = wrap_with_headers(&output, options)?;
    output = wrap_expressions(&output, options)?;
    output = wrap_from_imports(&output, options)?;
    if options.python.dict_alignment != "none" {
        output = align_dicts(&output, options)?
    }
    if options.python.explicit_continuation {
        output = explicit_continuations(&output, options)?
    }
    if semantic_fingerprint(source) != semantic_fingerprint(&output) {
        return Err(FormatError::Unsafe(
            "Python style would change the program".into(),
        ));
    }
    preserve_regions(source, output, options)
}
/// Physical blank lines in string payloads are source content, not indentation
/// trivia. Token ownership prevents equal neighboring text in another literal
/// (or a code block) from donating different whitespace to that payload.
fn literal_owned_lines(source: &str) -> Vec<bool> {
    let starts = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(at, _)| at + 1))
        .collect::<Vec<_>>();
    let mut owned = vec![false; starts.len()];
    let (tokens, _) = rustpy_syntax::tokenize(source);
    for token in tokens.iter().filter(|token| {
        matches!(
            token.kind,
            STRING | FSTRING_START | FSTRING_MIDDLE | TSTRING_START | TSTRING_MIDDLE
        )
    }) {
        if !source[token.range].contains('\n') {
            continue;
        }
        let first = starts
            .partition_point(|at| *at <= usize::from(token.range.start()))
            .saturating_sub(1);
        let last = starts
            .partition_point(|at| *at < usize::from(token.range.end()))
            .saturating_sub(1);
        owned[first..=last].fill(true);
    }
    owned
}

fn align_dicts(source: &str, options: &FormatOptions) -> Result<String, FormatError> {
    let root = rustpy_syntax::parse(source).syntax_node();
    let mut edits = Vec::new();
    for dictionary in root
        .descendants()
        .filter_map(ast::DictExpr::cast)
        .take(10000)
    {
        if !source[usize::from(dictionary.text_range().start())
            ..usize::from(dictionary.text_range().end())]
            .contains('\n')
        {
            continue;
        }
        let mut entries = Vec::new();
        for item in dictionary
            .syntax()
            .children()
            .filter(|node| node.kind() == DICT_ITEM)
        {
            let Some(colon) = item
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| token.kind() == COLON)
            else {
                continue;
            };
            let mut values = item.children().filter_map(ast::Expr::cast);
            let Some(key) = values.next() else { continue };
            let Some(value) = values.next() else { continue };
            let start = usize::from(key.text_range().start());
            let end = usize::from(key.text_range().end());
            if source[start..end].contains('\n') {
                continue;
            }
            let line = source[..start].rfind('\n').map_or(0, |at| at + 1);
            entries.push((
                colon.text_range(),
                key.text_range().end(),
                value.text_range().start(),
                visual_width(&source[line..end], options),
            ));
        }
        let width = entries
            .iter()
            .map(|(_, _, _, column)| *column)
            .max()
            .unwrap_or(0);
        if entries.len() < 2 || width > options.max_line_length {
            continue;
        }
        for (colon, key_end, value_start, column) in entries {
            let (range, pad) = if options.python.dict_alignment == "keys" {
                (TextRange::new(key_end, colon.start()), width - column)
            } else {
                (TextRange::new(colon.end(), value_start), width - column + 1)
            };
            if source[usize::from(range.start())..usize::from(range.end())]
                .chars()
                .all(|character| matches!(character, ' ' | '\t'))
            {
                edits.push((range, " ".repeat(pad)))
            }
        }
    }
    apply_checked(source, edits)
}

/// Width uses the same Unicode table as the Black engine; tab stops describe
/// indentation columns rather than counting a tab as one scalar character.
fn visual_width(text: &str, options: &FormatOptions) -> usize {
    let tab = options.tabs.map_or(4, |tabs| tabs.width).clamp(1, 256);
    let mut column = 0;
    for character in text.chars() {
        if character == '\t' {
            column += tab - column % tab;
        } else {
            let mut buffer = [0; 4];
            column += crate::black::str_width(character.encode_utf8(&mut buffer));
        }
    }
    column
}
fn multiline_literal(source: &str, tokens: &[Token]) -> bool {
    tokens.iter().any(|token| {
        matches!(
            token.kind,
            STRING | FSTRING_START | FSTRING_MIDDLE | TSTRING_START | TSTRING_MIDDLE
        ) && source[token.range].contains('\n')
    })
}
fn explicit_continuations(source: &str, options: &FormatOptions) -> Result<String, FormatError> {
    let root = rustpy_syntax::parse(source).syntax_node();
    let mut edits = Vec::new();
    let mut covered = 0;
    for node in root.descendants().take(100000) {
        if node.kind() != PAREN_EXPR || usize::from(node.text_range().start()) < covered {
            continue;
        }
        let Some(group) = ast::ParenExpr::cast(node.clone()) else {
            continue;
        };
        let Some(inner_node) = group.inner() else {
            continue;
        };
        // These forms depend on their grouping grammar, unlike an ordinary
        // binary/call expression containing a nested comma.
        if matches!(
            inner_node.syntax().kind(),
            TUPLE_EXPR | GENERATOR_EXPR | NAMED_EXPR | YIELD_EXPR
        ) {
            continue;
        }
        // Removing grouping inside a larger expression can change precedence,
        // call/subscript grammar or conditional ownership. Keep such grouping;
        // the preference applies only to a complete statement expression.
        if !node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                ASSIGN_STMT | ANN_ASSIGN_STMT | AUG_ASSIGN_STMT | RETURN_STMT | EXPR_STMT
            )
        }) {
            continue;
        }
        let text = node.text().to_string();
        if !text.contains('\n') {
            continue;
        }
        let (tokens, _) = rustpy_syntax::tokenize(&text);
        if tokens.iter().any(|token| token.kind == COMMENT) || multiline_literal(&text, &tokens) {
            continue;
        }
        let Some(first) = node.first_token().filter(|token| token.kind() == L_PAREN) else {
            continue;
        };
        let Some(last) = node.last_token().filter(|token| token.kind() == R_PAREN) else {
            continue;
        };
        let inner =
            &source[usize::from(first.text_range().end())..usize::from(last.text_range().start())];
        let start = usize::from(node.text_range().start());
        let line = source[..start].rfind('\n').map_or(0, |index| index + 1);
        let base = source[line..start]
            .chars()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .collect::<String>();
        let pad =
            " ".repeat(visual_width(&base, options) + options.python.indents.continuation.min(256));
        let mut replacement = String::new();
        for (index, line) in inner.trim().lines().enumerate() {
            if index > 0 {
                replacement.push_str(" \\\n");
                replacement.push_str(&pad);
            }
            replacement.push_str(line.trim());
        }
        covered = usize::from(node.text_range().end());
        edits.push((node.text_range(), replacement));
    }
    apply_checked(source, edits)
}

/// Context-manager items come from the CST, so parentheses and commas inside
/// calls/targets do not get confused with separators between managers.
fn wrap_with_headers(source: &str, options: &FormatOptions) -> Result<String, FormatError> {
    let rule = options.python.wrap("with");
    if rule.mode == "inherit" && !options.python.explicit_continuation {
        return Ok(source.into());
    }
    let root = rustpy_syntax::parse(source).syntax_node();
    let mut edits = Vec::new();
    for statement in root
        .descendants()
        .filter_map(ast::WithStmt::cast)
        .take(10000)
    {
        let Some(colon) = statement
            .syntax()
            .children_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| token.kind() == COLON)
        else {
            continue;
        };
        let start = usize::from(statement.text_range().start());
        let end = usize::from(colon.text_range().start());
        let header = &source[start..end];
        let (tokens, _) = rustpy_syntax::tokenize(header);
        if tokens.iter().any(|token| token.kind == COMMENT) || multiline_literal(header, &tokens) {
            continue;
        }
        let prefix = if statement.is_async() {
            "async with"
        } else {
            "with"
        };
        let items = statement
            .items()
            .map(|item| {
                item.syntax()
                    .text()
                    .to_string()
                    .lines()
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            continue;
        }
        let line = source[..start].rfind('\n').map_or(0, |index| index + 1);
        let base = visual_width(&source[line..start], options);
        let one = format!("{prefix} {}", items.join(", "));
        let multiline = rule.mode != "never"
            && (rule.mode == "always"
                || base + visual_width(&one, options) > options.max_line_length
                || (options.python.explicit_continuation && header.contains('\n')));
        if rule.mode == "inherit" && !header.contains('\n') {
            continue;
        }
        let replacement = if !multiline {
            one
        } else {
            let column = if rule.align {
                base + prefix.len() + 2
            } else if options.python.indents.in_parentheses {
                base + options.python.indents.continuation.min(256)
            } else {
                base
            };
            let pad = " ".repeat(column.min(4096));
            if options.python.explicit_continuation {
                if items.len() < 2 {
                    continue;
                }
                format!("{prefix} {}", items.join(&format!(", \\\n{pad}")))
            } else {
                let close = if options.python.indents.hanging_closing {
                    pad.clone()
                } else {
                    " ".repeat(base.min(4096))
                };
                format!(
                    "{prefix} ({}{}{})",
                    if rule.newline_after_open {
                        format!("\n{pad}")
                    } else {
                        String::new()
                    },
                    items.join(&format!(",\n{pad}")),
                    if rule.closing_on_newline {
                        format!("\n{close}")
                    } else {
                        String::new()
                    }
                )
            }
        };
        edits.push((
            TextRange::new(statement.text_range().start(), colon.text_range().start()),
            replacement,
        ));
    }
    apply_checked(source, edits)
}
fn apply_checked(source: &str, mut edits: Vec<Edit>) -> Result<String, FormatError> {
    edits.sort_by_key(|(range, _)| range.start());
    if edits
        .windows(2)
        .any(|pair| pair[0].0.end() > pair[1].0.start())
    {
        return Ok(source.into());
    }
    let mut output = source.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        output.replace_range(
            usize::from(range.start())..usize::from(range.end()),
            &replacement,
        )
    }
    if semantic_fingerprint(source) != semantic_fingerprint(&output) {
        return Err(FormatError::Unsafe(
            "style edit changed syntax/meaning".into(),
        ));
    }
    Ok(output)
}

/// Rearrange only independent class definitions with inert headers. Class
/// execution order, decorators, duplicate names and computed defaults remain
/// authoritative: such regions are left in their original order.
pub fn arrange(file: &FileInfo, options: &Arrangement) -> Vec<Edit> {
    arrange_inner(file, options, &Default::default())
}

/// Database-aware arrangement recognizes canonical abc.abstractmethod and its
/// imported aliases; unknown or visibly retired decorators remain barriers.
pub fn arrange_with_db(
    db: &rustpy_semantic::Analysis,
    file: &FileInfo,
    options: &Arrangement,
) -> Vec<Edit> {
    let mut abstract_decorators = std::collections::HashSet::new();
    let root = file.parse.syntax_node();
    let retired = file
        .scopes
        .scopes
        .iter()
        .any(|scope| !scope.star_imports.is_empty())
        || root
            .descendants()
            .filter_map(ast::AttributeExpr::cast)
            .any(|attribute| {
                attribute.attr_text() == "abstractmethod"
                    && rustpy_semantic::edit_helpers::is_assignment_target(attribute.syntax())
            });
    if !retired {
        if let Some(captured) = db
            .file(file.id)
            .filter(|captured| captured.text == file.text)
        {
            let infer = rustpy_semantic::infer::Inferer::new(db);
            for decorator in root
                .descendants()
                .filter_map(ast::Decorator::cast)
                .take(10000)
            {
                let Some(expression) = decorator.expr() else {
                    continue;
                };
                let name = match &expression {
                    ast::Expr::Name(name) => Some(name.name_text()),
                    ast::Expr::Attribute(attribute) => match attribute.object() {
                        Some(ast::Expr::Name(name)) => Some(name.name_text()),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(name) = name else { continue };
                let scope = file.scopes.scope_at(expression.text_range().start());
                let Some(bindings) = file.scopes.resolve(scope, &name) else {
                    continue;
                };
                if bindings.len() != 1
                    || !matches!(
                        file.scopes.binding(bindings[0]).kind,
                        rustpy_semantic::scope::BindingKind::Import(_)
                    )
                {
                    continue;
                }
                let rustpy_semantic::types::Type::Function(signature) =
                    infer.type_of_expr(&captured, &expression)
                else {
                    continue;
                };
                let Some(definition) = signature.def.as_ref() else {
                    continue;
                };
                let Some(module) = db.file(definition.file) else {
                    continue;
                };
                if definition.name == "abstractmethod"
                    && module.path == std::path::Path::new("<typeshed>/abc.pyi")
                {
                    abstract_decorators.insert(decorator.text_range());
                }
            }
        }
    }
    arrange_inner(file, options, &abstract_decorators)
}
fn arrange_inner(
    file: &FileInfo,
    options: &Arrangement,
    abstract_decorators: &std::collections::HashSet<TextRange>,
) -> Vec<Edit> {
    if !options.enabled {
        return Vec::new();
    }
    let root = file.parse.syntax_node();
    let mut edits = Vec::new();
    for class in root
        .descendants()
        .filter_map(ast::ClassDef::cast)
        .take(10000)
    {
        let Some(body) = class.body() else { continue };
        let statements = body.statements().collect::<Vec<_>>();
        let mut start = 0;
        while start < statements.len() {
            if !safe_member(file, &statements[start], abstract_decorators) {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < statements.len() && safe_member(file, &statements[end], abstract_decorators)
            {
                end += 1
            }
            let group = &statements[start..end];
            let mut seen = std::collections::HashSet::new();
            if group
                .iter()
                .any(|statement| !seen.insert(member_name(statement)))
            {
                start = end;
                continue;
            }
            let a = usize::from(group[0].text_range().start());
            let a = file.text[..a].rfind('\n').map_or(0, |at| at + 1);
            let b = usize::from(group.last().unwrap().text_range().end());
            let b = file.text[b..]
                .find('\n')
                .map_or(file.text.len(), |at| b + at + 1);
            let original = &file.text[a..b];
            if original.contains('#') {
                start = end;
                continue;
            }
            let mut ranges = group
                .iter()
                .map(|statement| {
                    let a = usize::from(statement.text_range().start());
                    let a = file.text[..a].rfind('\n').map_or(0, |at| at + 1);
                    (statement, a)
                })
                .collect::<Vec<_>>();
            let mut pieces = Vec::new();
            for index in 0..ranges.len() {
                let (statement, a) = ranges[index];
                let b = ranges.get(index + 1).map_or(b, |(_, a)| *a);
                pieces.push((
                    category(statement, abstract_decorators),
                    member_name(statement),
                    file.text[a..b].to_string(),
                ));
            }
            pieces.sort_by_key(|(kind, name, _)| {
                (
                    options
                        .order
                        .iter()
                        .position(|rule| rule == kind)
                        .unwrap_or(options.order.len()),
                    name.clone(),
                )
            });
            let replacement = pieces
                .into_iter()
                .map(|(_, _, text)| text)
                .collect::<String>();
            if replacement != original {
                edits.push((
                    TextRange::new(TextSize::new(a as u32), TextSize::new(b as u32)),
                    replacement,
                ))
            }
            ranges.clear();
            start = end;
        }
    }
    edits.sort_by_key(|(range, _)| range.start());
    if edits
        .windows(2)
        .any(|pair| pair[0].0.end() > pair[1].0.start())
    {
        return Vec::new();
    }
    edits
}
fn safe_member(
    file: &FileInfo,
    statement: &ast::Stmt,
    abstract_decorators: &std::collections::HashSet<TextRange>,
) -> bool {
    match statement {
        ast::Stmt::FuncDef(function) => {
            for decorator in function.decorators() {
                if abstract_decorators.contains(&decorator.text_range()) {
                    continue;
                }
                let Some(ast::Expr::Name(name)) = decorator.expr() else {
                    return false;
                };
                let name = name.name_text();
                if !matches!(name.as_str(), "staticmethod" | "classmethod") {
                    return false;
                }
                let scope = file.scopes.scope_at(decorator.text_range().start());
                if file.scopes.resolve(scope, &name).is_some()
                    || file
                        .scopes
                        .scopes
                        .iter()
                        .any(|scope| !scope.star_imports.is_empty())
                {
                    return false;
                }
            }
            let body_start = function
                .body()
                .map(|body| body.text_range().start())
                .unwrap_or(function.text_range().end());
            for token in function
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| token.text_range().start() < body_start)
            {
                if token
                    .parent()
                    .is_some_and(|parent| parent.ancestors().any(|node| node.kind() == DECORATOR))
                {
                    continue;
                }
                if token.kind() == L_PAREN
                    && token
                        .parent()
                        .is_some_and(|parent| parent.kind() == ARG_LIST)
                {
                    return false;
                }
                if token.kind() == IDENT
                    && !matches!(
                        token.parent().map(|node| node.kind()),
                        Some(PARAM | FUNC_DEF | STAR_PARAM | STAR_STAR_PARAM)
                    )
                {
                    return false;
                }
            }
            true
        }
        ast::Stmt::Assign(assignment) => {
            assignment.targets().len() == 1
                && assignment
                    .targets()
                    .iter()
                    .all(|target| matches!(target, ast::Expr::Name(_)))
                && assignment.value().is_some_and(|value| {
                    matches!(value, ast::Expr::Literal(_) | ast::Expr::String(_))
                })
        }
        _ => false,
    }
}
fn member_name(statement: &ast::Stmt) -> String {
    match statement {
        ast::Stmt::FuncDef(function) => function.name_text().to_string(),
        ast::Stmt::Assign(assignment) => assignment
            .targets()
            .first()
            .map(|target| target.syntax().text().to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}
fn category(
    statement: &ast::Stmt,
    abstract_decorators: &std::collections::HashSet<TextRange>,
) -> String {
    let name = member_name(statement);
    if matches!(statement,ast::Stmt::FuncDef(function) if function.decorators().any(|decorator|abstract_decorators.contains(&decorator.text_range())))
    {
        return "abstract".into();
    }
    if name.starts_with("__")&&name.ends_with("__"){"dunder"}else if matches!(statement,ast::Stmt::Assign(_)){"class_variables"}else if matches!(statement,ast::Stmt::FuncDef(function) if function.decorators().any(|decorator|decorator.syntax().text().to_string().contains("staticmethod"))){"static"}else if name.starts_with("__"){"private"}else if name.starts_with('_'){"protected"}else{"public"}.into()
}

fn wrap_expressions(source: &str, options: &FormatOptions) -> Result<String, FormatError> {
    let root = rustpy_syntax::parse(source).syntax_node();
    let mut edits = Vec::new();
    let mut covered = 0;
    for node in root.descendants().take(100000) {
        let start = usize::from(node.text_range().start());
        let end = usize::from(node.text_range().end());
        if start < covered {
            continue;
        }
        let key = if ast::Expr::cast(node.clone()).is_some()
            && node
                .parent()
                .is_some_and(|parent| matches!(parent.kind(), IF_STMT | ELIF_CLAUSE | WHILE_STMT))
        {
            "conditions"
        } else {
            match node.kind() {
                BINARY_EXPR | BOOL_OP_EXPR | COMPARE_EXPR => "binary",
                LIST_COMP | SET_COMP | DICT_COMP | GENERATOR_EXPR => "comprehensions",
                ATTRIBUTE_EXPR if node.descendants().any(|node| node.kind() == CALL_EXPR) => {
                    "chains"
                }
                LAMBDA_EXPR => "lambdas",
                _ => continue,
            }
        };
        let rule = options.python.wrap(key);
        if rule.mode == "inherit" {
            continue;
        }
        let mut text = &source[start..end];
        let mut edit_range = node.text_range();
        if key != "comprehensions" {
            if node.kind() == PAREN_EXPR {
                if let Some(inner) = text
                    .trim()
                    .strip_prefix('(')
                    .and_then(|text| text.strip_suffix(')'))
                {
                    text = inner;
                }
            } else if let Some(parent) = node.parent().filter(|parent| parent.kind() == PAREN_EXPR)
            {
                edit_range = parent.text_range();
            }
        }
        let (tokens, _) = rustpy_syntax::tokenize(text);
        if tokens.iter().any(|token| token.kind == COMMENT)
            || multiline_literal(text, &tokens)
            || text.contains("\\\n")
        {
            continue;
        }
        let layout_start = usize::from(edit_range.start());
        let line_start = source[..layout_start].rfind('\n').map_or(0, |at| at + 1);
        let column = visual_width(&source[line_start..layout_start], options);
        let width = text
            .lines()
            .enumerate()
            .map(|(index, line)| visual_width(line, options) + if index == 0 { column } else { 0 })
            .max()
            .unwrap_or(0);
        if rule.mode == "never" {
            if text.contains('\n') {
                let mut replacement = text.lines().map(str::trim).collect::<Vec<_>>().join(" ");
                if node.kind() == PAREN_EXPR || edit_range != node.text_range() {
                    replacement = format!("({replacement})");
                }
                edits.push((edit_range, replacement));
                covered = end;
            }
            continue;
        }
        if rule.mode != "always" && width <= options.max_line_length {
            continue;
        }
        let mut depth = 0usize;
        let mut points = Vec::new();
        let text_start = text.as_ptr() as usize - source.as_ptr() as usize;
        let mut previous_parent = None;
        let mut previous_kind = None;
        for token in &tokens {
            if matches!(
                token.kind,
                WHITESPACE | NL | NEWLINE | INDENT | DEDENT | EOF
            ) {
                continue;
            }
            let absolute = TextSize::new((text_start + usize::from(token.range.start())) as u32);
            let syntax = root.token_at_offset(absolute).right_biased();
            let owner = syntax.as_ref().and_then(|token| token.parent());
            let parent = owner.as_ref().map(|node| node.kind());
            if matches!(token.kind, L_PAREN | L_BRACK | L_BRACE) {
                depth += 1;
                previous_parent = parent;
                previous_kind = Some(token.kind);
                continue;
            }
            if matches!(token.kind, R_PAREN | R_BRACK | R_BRACE) {
                depth = depth.saturating_sub(1);
                previous_parent = parent;
                previous_kind = Some(token.kind);
                continue;
            }
            let at = usize::from(token.range.start());
            let split = match key {
                "comprehensions" => {
                    depth == 1
                        && ((parent == Some(COMP_FOR) && token.kind == ASYNC_KW)
                            || (parent == Some(COMP_FOR)
                                && token.kind == FOR_KW
                                && previous_kind != Some(ASYNC_KW))
                            || (parent == Some(COMP_IF) && token.kind == IF_KW))
                }
                "chains" => depth == 0 && token.kind == DOT,
                "lambdas" => depth == 0 && token.kind == COLON,
                _ => {
                    depth == 0
                        && ((parent == Some(BINARY_EXPR)
                            && matches!(
                                token.kind,
                                PLUS | MINUS
                                    | STAR
                                    | SLASH
                                    | SLASH_SLASH
                                    | PERCENT
                                    | AT
                                    | PIPE
                                    | AMP
                                    | CARET
                                    | SHL
                                    | SHR
                                    | STAR_STAR
                            ))
                            || (parent == Some(BOOL_OP_EXPR)
                                && matches!(token.kind, AND_KW | OR_KW))
                            || (parent == Some(COMPARE_OP) && previous_parent != Some(COMPARE_OP)))
                }
            };
            if split && at > 0 {
                points.push(if key == "lambdas" {
                    usize::from(token.range.end())
                } else if options.python.space("operator_at_line_end", false)
                    && matches!(key, "binary" | "conditions")
                {
                    if parent == Some(COMPARE_OP) {
                        owner
                            .as_ref()
                            .map(|owner| usize::from(owner.text_range().end()) - text_start)
                            .unwrap_or(usize::from(token.range.end()))
                    } else {
                        usize::from(token.range.end())
                    }
                } else {
                    at
                });
            }
            previous_parent = parent;
            previous_kind = Some(token.kind);
        }
        if points.is_empty() {
            continue;
        }
        let leading = source[line_start..layout_start]
            .chars()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .collect::<String>();
        let base = visual_width(&leading, options);
        let continuation = if !options.python.indents.in_parentheses {
            base
        } else if rule.align {
            column + 1
        } else {
            base.saturating_add(options.python.indents.continuation.min(256))
        };
        let pad = " ".repeat(continuation.min(4096));
        let mut parts = Vec::new();
        let mut at = 0;
        for point in points {
            if point > at {
                parts.push(text[at..point].trim().to_string());
                at = point
            }
        }
        if at < text.len() {
            parts.push(text[at..].trim().into())
        }
        let body = parts.join(&format!("\n{pad}"));
        let grouped = if key == "comprehensions" {
            let mut body = body;
            if rule.newline_after_open && body.starts_with(['[', '{', '(']) {
                body.insert_str(1, &format!("\n{pad}"));
            }
            if rule.closing_on_newline && body.ends_with([']', '}', ')']) {
                let end = body.len() - 1;
                body.insert_str(end, &format!("\n{}", " ".repeat(base)));
            }
            body
        } else {
            format!(
                "({}{body}{})",
                if rule.newline_after_open {
                    format!("\n{pad}")
                } else {
                    String::new()
                },
                if rule.closing_on_newline {
                    format!("\n{}", " ".repeat(base))
                } else {
                    String::new()
                }
            )
        };
        edits.push((edit_range, grouped));
        covered = usize::from(edit_range.end());
    }
    apply_checked(source, edits)
}

fn wrap_from_imports(source: &str, options: &FormatOptions) -> Result<String, FormatError> {
    let rule = options.python.wrap("imports");
    if rule.mode == "inherit" {
        return Ok(source.into());
    }
    let root = rustpy_syntax::parse(source).syntax_node();
    let mut edits = Vec::new();
    for statement in root
        .descendants()
        .filter_map(ast::ImportFromStmt::cast)
        .take(10000)
    {
        let text = &source[usize::from(statement.text_range().start())
            ..usize::from(statement.text_range().end())];
        if text.contains('#') || statement.is_star() {
            continue;
        }
        let names = statement
            .aliases()
            .map(|alias| alias.syntax().text().to_string())
            .collect::<Vec<_>>();
        if names.is_empty() {
            continue;
        }
        let head = format!("from {} import ", statement.module_path());
        let one = format!("{head}{}", names.join(", "));
        let line_start = source[..usize::from(statement.text_range().start())]
            .rfind('\n')
            .map_or(0, |at| at + 1);
        let pad = &source[line_start..usize::from(statement.text_range().start())];
        let wrap = rule.mode == "always"
            || rule.mode != "never"
                && visual_width(&one, options) + visual_width(pad, options)
                    > options.max_line_length;
        let replacement = if wrap {
            let indent = if rule.align {
                " ".repeat(
                    visual_width(pad, options)
                        .saturating_add(visual_width(&head, options))
                        .saturating_add(1)
                        .min(4096),
                )
            } else {
                format!(
                    "{pad}{}",
                    " ".repeat(options.python.indents.continuation.min(256))
                )
            };
            format!(
                "{head}({}{}{}{})",
                if rule.newline_after_open {
                    format!("\n{indent}")
                } else {
                    String::new()
                },
                names.join(&format!(",\n{indent}")),
                if options.python.imports.trailing_comma {
                    ","
                } else {
                    ""
                },
                if rule.closing_on_newline {
                    format!("\n{pad}")
                } else {
                    String::new()
                }
            )
        } else {
            one
        };
        edits.push((statement.text_range(), replacement));
    }
    apply_checked(source, edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_settings::python_style::{Imports, PythonStyle};
    fn configured() -> FormatOptions {
        let mut options = FormatOptions::pep8();
        options.python.spaces.insert("assignment".into(), false);
        options
    }
    #[test]
    fn spaces_use_cst_roles_not_string_replacements() {
        let mut options = configured();
        for (key, value) in [
            ("keyword_equals", true),
            ("default_equals", true),
            ("before_call", true),
            ("before_annotation_colon", true),
            ("after_annotation_colon", false),
            ("power", true),
        ] {
            options.python.spaces.insert(key.into(), value);
        }
        let source = "def f(x: int=1):\n    value = g(x=2)\n    return value**2\n";
        let output = format_source(source, &options).unwrap();
        assert!(output.contains("x :int = 1"));
        assert!(output.contains("value=g (x = 2)"));
        assert!(output.contains("value ** 2"));
        assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&output));
    }
    #[test]
    fn annotation_colons_follow_declaration_owners_without_changing_other_colons() {
        let mut options = FormatOptions::pep8();
        options
            .python
            .spaces
            .insert("before_annotation_colon".into(), true);
        options
            .python
            .spaces
            .insert("after_annotation_colon".into(), false);
        let source = "value: int = 1\ndef f[T: int](x: int, *args: str, **kwargs: bool):\n    return {x: value}\n";
        let output = format_source(source, &options).unwrap();
        for annotation in [
            "value :int",
            "T :int",
            "x :int",
            "args :str",
            "kwargs :bool",
        ] {
            assert!(output.contains(annotation), "{annotation}: {output}");
        }
        assert!(output.contains("{x: value}"), "{output}");
        assert!(output.contains("kwargs :bool):\n"), "{output}");
        assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&output));
        assert_eq!(format_source(&output, &options).unwrap(), output);
    }
    #[test]
    fn wrapping_call_and_declaration_options_are_independent_and_idempotent() {
        let mut options = FormatOptions::pep8();
        options.python.wrapping.insert(
            "calls".into(),
            Wrap {
                mode: "always".into(),
                align: true,
                newline_after_open: true,
                closing_on_newline: false,
            },
        );
        options.python.wrapping.insert(
            "parameters".into(),
            Wrap {
                mode: "never".into(),
                ..Default::default()
            },
        );
        let source = "def f(first, second):\n    return g(first, second)\n";
        let output = format_source(source, &options).unwrap();
        assert!(output.contains("def f(first, second):"));
        assert!(output.contains("g(\n"));
        assert_eq!(format_source(&output, &options).unwrap(), output);
        assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&output));
    }
    #[test]
    fn string_docstring_and_forward_annotation_quotes_are_independent() {
        let mut options = FormatOptions::pep8();
        options.quote = QuoteStyle::Single;
        options.python.docstring_quotes = "double".into();
        options.python.type_hint_quotes = "double".into();
        let source = "def f(value: 'Thing'):\n    '''Summary.'''\n    return \"value\"\n";
        let output = format_source(source, &options).unwrap();
        assert!(output.contains("\"Thing\""));
        assert!(output.contains("\"\"\"Summary.\"\"\""));
        assert!(output.contains("'value'"));
        assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&output));
    }
    #[test]
    fn configurable_blank_lines_comments_and_dict_alignment_preserve_values() {
        let mut options = FormatOptions::pep8();
        options.python.blank_lines.after_imports = 3;
        options.python.spaces_before_comment = 1;
        options.python.space_after_comment = false;
        options.python.dict_alignment = "values".into();
        options.python.wrapping.insert(
            "dicts".into(),
            Wrap {
                mode: "always".into(),
                ..Default::default()
            },
        );
        let source = "import os\nvalue = {'a': 1, 'long': 2} #note\n";
        let output = format_source(source, &options).unwrap();
        assert!(output.contains("import os\n\n\n\n"));
        assert!(output.contains(" #note"));
        assert!(output.contains("'a':    1"));
        assert_eq!(format_source(&output, &options).unwrap(), output);
        assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&output));
    }
    #[test]
    fn finite_expression_and_from_import_wrapping_are_real_transformations() {
        let mut options = FormatOptions::pep8();
        for key in [
            "conditions",
            "imports",
            "chains",
            "comprehensions",
            "lambdas",
            "with",
        ] {
            options.python.wrapping.insert(
                key.into(),
                Wrap {
                    mode: "always".into(),
                    ..Default::default()
                },
            );
        }
        let source="from os import path, sep\nif first and second:\n    values = [x for x in items if x]\n    callback = lambda x: x + 1\n    with first as a, second as b:\n        result = a.make().finish()\n";
        let output = format_source(source, &options).unwrap();
        assert!(output.contains("from os import (\n"));
        assert!(output.contains("if (\n"));
        assert!(output.contains("with (\n"));
        assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&output));
    }
    fn parsed_file(source: &str) -> std::sync::Arc<FileInfo> {
        let db = rustpy_semantic::Analysis::for_project(std::path::Path::new("/project"));
        let id = db.set_file_text(std::path::Path::new("/project/pkg/module.py"), source);
        db.file(id).unwrap()
    }
    #[test]
    fn import_preferences_join_group_wrap_and_preserve_requested_order() {
        let source = "from os import sep\nfrom os import path\nimport sys\n";
        let file = parsed_file(source);
        let mut options = Imports {
            join_from: true,
            parentheses: true,
            trailing_comma: false,
            ..Default::default()
        };
        let edits = crate::imports::optimize_imports_with_style(&file, &[], &options);
        let output = apply_unchecked(source, edits);
        assert!(output.contains("path"));
        assert_eq!(output.matches("from os import").count(), 1);
        assert!(output.contains("import (\n"));
        options = Imports {
            preserve_order: true,
            ..Default::default()
        };
        let output = apply_unchecked(
            source,
            crate::imports::optimize_imports_with_style(&file, &[], &options),
        );
        assert!(output.find("sep").unwrap() < output.find("path").unwrap());
    }
    fn apply_unchecked(source: &str, mut edits: Vec<Edit>) -> String {
        edits.sort_by_key(|(range, _)| range.start());
        let mut output = source.to_string();
        for (range, replacement) in edits.into_iter().rev() {
            output.replace_range(
                usize::from(range.start())..usize::from(range.end()),
                &replacement,
            )
        }
        assert!(rustpy_syntax::parse(&output).errors().is_empty());
        output
    }
    #[test]
    fn arrangement_moves_only_independent_members_and_preserves_effectful_headers() {
        let source="class Example:\n    def zebra(self):\n        return 1\n    CONSTANT = 2\n    def alpha(self):\n        return 3\n";
        let file = parsed_file(source);
        let options = Arrangement {
            enabled: true,
            ..Default::default()
        };
        let output = apply_unchecked(source, arrange(&file, &options));
        assert!(output.find("CONSTANT").unwrap() < output.find("def alpha").unwrap());
        assert!(output.find("def alpha").unwrap() < output.find("def zebra").unwrap());
        let source="class Example:\n    def zebra(self, value=side_effect()):\n        pass\n    def alpha(self):\n        pass\n";
        assert!(arrange(&parsed_file(source), &options).is_empty());
    }
    #[test]
    fn default_python_model_keeps_existing_black_contract() {
        let options = FormatOptions::black();
        assert_eq!(options.python, PythonStyle::default());
        assert_eq!(
            format_source("value={'a':1}\n", &options).unwrap(),
            "value = {\"a\": 1}\n"
        )
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    #[test]
    fn canonical_abstract_alias_arranges_but_visible_retirement_is_a_barrier() {
        let db = rustpy_semantic::Analysis::for_project(std::path::Path::new("/project"));
        let source="from abc import abstractmethod as marker\nclass Example:\n    def public(self):\n        return 1\n    @marker\n    def required(self):\n        pass\n";
        let id = db.set_file_text(std::path::Path::new("/project/main.py"), source);
        let file = db.file(id).unwrap();
        let options = Arrangement {
            enabled: true,
            order: vec!["abstract".into(), "public".into()],
        };
        let edits = arrange_with_db(&db, &file, &options);
        assert!(!edits.is_empty());
        let mut output = source.to_string();
        for (range, replacement) in edits.into_iter().rev() {
            output.replace_range(
                usize::from(range.start())..usize::from(range.end()),
                &replacement,
            )
        }
        assert!(output.find("def required").unwrap() < output.find("def public").unwrap());
        let source=source.replace("from abc import abstractmethod as marker","import abc\nabc.abstractmethod = replacement\nfrom abc import abstractmethod as marker");
        let id = db.set_file_text(std::path::Path::new("/project/main.py"), &source);
        assert!(arrange_with_db(&db, &db.file(id).unwrap(), &options).is_empty());
    }
    #[test]
    fn python_alignment_and_wrapping_never_rewrite_formatter_off_regions() {
        let mut options = FormatOptions::pep8();
        options.formatter_tags = Some(FormatterTags {
            off: "KEEP".into(),
            on: "RESUME".into(),
        });
        options.python.dict_alignment = "values".into();
        options.python.wrapping.insert(
            "dicts".into(),
            Wrap {
                mode: "always".into(),
                ..Default::default()
            },
        );
        let source = "# KEEP\nkept={'a':1,'long':2}\n# RESUME\nchanged={'a':1,'long':2}\n";
        let output = format_source(source, &options).unwrap();
        assert!(output.contains("kept={'a':1,'long':2}"));
        assert_ne!(output, source);
        assert_eq!(semantic_fingerprint(source), semantic_fingerprint(&output));
    }
}

fn preserve_regions(
    source: &str,
    output: String,
    options: &FormatOptions,
) -> Result<String, FormatError> {
    let count = source.split_inclusive('\n').count();
    let mut blocked = vec![false; count];
    let starts = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(index, _)| index + 1))
        .collect::<Vec<_>>();
    let (tokens, _) = rustpy_syntax::tokenize(source);
    let (mut black, mut custom) = (false, false);
    let mut from = 0;
    for token in tokens.iter().filter(|token| token.kind == COMMENT) {
        let text = &source[token.range];
        let line = starts
            .partition_point(|start| *start <= usize::from(token.range.start()))
            .saturating_sub(1);
        let off = crate::is_fmt_off(text)
            || options
                .formatter_tags
                .as_ref()
                .is_some_and(|tags| tags.is_off(text));
        if off && !black && !custom {
            from = line
        }
        if crate::is_fmt_off(text) {
            black = true
        }
        if options
            .formatter_tags
            .as_ref()
            .is_some_and(|tags| tags.is_off(text))
        {
            custom = true
        }
        if black || custom {
            for value in blocked.iter_mut().take(line + 1).skip(from) {
                *value = true
            }
        }
        if crate::is_fmt_on(text) {
            black = false
        }
        if options
            .formatter_tags
            .as_ref()
            .is_some_and(|tags| tags.is_on(text))
        {
            custom = false
        }
        if crate::is_fmt_skip(text) {
            if let Some(value) = blocked.get_mut(line) {
                *value = true
            }
        }
    }
    if black || custom {
        for value in blocked.iter_mut().skip(from) {
            *value = true
        }
    }
    if !blocked.iter().any(|value| *value) {
        return Ok(output);
    }
    let allowed = blocked
        .iter()
        .enumerate()
        .filter(|(_, blocked)| !**blocked)
        .map(|(index, _)| (index, 1))
        .collect::<Vec<_>>();
    crate::style_policy::changed_lines(source, &output, &allowed)
}
